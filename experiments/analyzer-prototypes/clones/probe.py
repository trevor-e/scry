#!/usr/bin/env python3
"""Find surviving pre-change clone bodies after fix-labelled commits; read-only."""
import argparse
from collections import defaultdict
from dataclasses import dataclass
from difflib import SequenceMatcher
import hashlib
import json
from pathlib import Path
import re
import subprocess

import tree_sitter as ts
import tree_sitter_rust
import tree_sitter_typescript


def main():
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument('repos', type=Path, nargs='+')
    cli.add_argument('--commits', type=int, default=200)
    cli.add_argument('--similarity', type=float, default=0.85)
    args = cli.parse_args()
    print(json.dumps([scan(p.resolve(), args.commits, args.similarity) for p in args.repos], indent=2))


def scan(repo, commit_limit, similarity):
    parsers = {
        '.rs': ts.Parser(ts.Language(tree_sitter_rust.language())),
        '.ts': ts.Parser(ts.Language(tree_sitter_typescript.language_typescript())),
        '.tsx': ts.Parser(ts.Language(tree_sitter_typescript.language_tsx())),
    }
    files = [p for p in git(repo, 'ls-files', '-z').decode().split('\0') if eligible(p)]
    head_units, parse_errors = [], []
    for path in files:
        try:
            source = (repo / path).read_bytes()
        except OSError:
            continue
        found, errors = units(path, source, parsers)
        head_units.extend(found)
        if errors:
            parse_errors.append(path)
    shingles = defaultdict(set)
    for index, unit in enumerate(head_units):
        if 45 <= len(unit.tokens) <= 1200:
            for key in windows(unit.tokens, 12):
                shingles[key].add(index)
    history = git(repo, 'log', '--no-merges', f'-{commit_limit}', '--format=%H%x09%s').decode().splitlines()
    fixes = [line.split('\t', 1) for line in history if re.search(r'\b(fix(?:es|ed)?|bug|regression)\b', line.split('\t', 1)[-1], re.I)]
    findings, seen, skipped_parents = [], set(), 0
    compared_functions = 0
    truncated_candidates = 0
    for commit, subject in fixes:
        parent = git(repo, 'rev-parse', '--verify', commit + '^', optional=True).strip().decode()
        if not parent:
            skipped_parents += 1
            continue
        paths = git(repo, 'diff', '--name-only', '--diff-filter=M', '-z', parent, commit).decode().split('\0')
        for path in filter(eligible, paths):
            old_source = git(repo, 'show', f'{parent}:{path}', optional=True)
            new_source = git(repo, 'show', f'{commit}:{path}', optional=True)
            if not old_source or not new_source:
                continue
            before, _ = units(path, old_source, parsers)
            after, _ = units(path, new_source, parsers)
            after_by_key = {u.key: u for u in after}
            for old in before:
                new = after_by_key.get(old.key)
                if new is None or old.tokens == new.tokens or not 45 <= len(old.tokens) <= 1200:
                    continue
                matcher = SequenceMatcher(None, old.tokens, new.tokens, autojunk=False)
                if matcher.ratio() < 0.65:
                    continue
                edits = matcher.get_opcodes()
                compared_functions += 1
                votes = defaultdict(int)
                old_keys = set(windows(old.tokens, 12))
                for key in old_keys:
                    for index in shingles.get(key, ()):
                        votes[index] += 1
                candidates = sorted(votes, key=lambda i: (-votes[i], head_units[i].path, head_units[i].line))
                matched = []
                for index in candidates:
                    other = head_units[index]
                    if (other.path, other.key) == (old.path, old.key):
                        continue
                    if Path(other.path).suffix != Path(old.path).suffix:
                        continue
                    if votes[index] / max(1, len(old_keys)) < 0.4:
                        continue
                    ratio = SequenceMatcher(None, old.tokens, other.tokens, autojunk=False).ratio()
                    if ratio < similarity:
                        continue
                    evidence = retained_edits(old, new, other, edits)
                    if not evidence:
                        continue
                    key = (commit, old.path, old.key, other.path, other.key)
                    if key in seen:
                        continue
                    seen.add(key)
                    matched.append(dict(check='missed_clone_fix', commit=commit, subject=subject,
                        changed=old.location(), changed_at_fix=new.location(), survivor=other.location(),
                        similarity=round(ratio, 4), normalized_exact=old.tokens == other.tokens,
                        edits=evidence[:4], status='candidate'))
                matched.sort(key=lambda x: (-x['similarity'], x['survivor']['path'], x['survivor']['line']))
                truncated_candidates += max(0, len(matched) - 10)
                findings.extend(matched[:10])
    findings.sort(key=lambda f: (-f['similarity'], f['commit'], f['survivor']['path'], f['survivor']['line']))
    return dict(repo=repo.name, root=str(repo), revision=git(repo, 'rev-parse', 'HEAD').decode().strip(),
        source_files=len(files), function_bodies=len(head_units), history_commits=len(history),
        fix_labelled_commits=len(fixes), changed_functions_compared=compared_functions,
        missing_history_parents=skipped_parents, parse_error_files=parse_errors,
        candidates_omitted_by_per_function_cap=truncated_candidates, findings=findings,
        limitations=['Fix labels select history; do not prove defect fixes.',
            'Identifier normalization preserves calls, properties, types, literals and operators but loses local variable identity.',
            'Nested closures are part of their enclosing function; tests/examples/generated/vendor paths excluded.',
            'No semantic equivalence or proof of missing fix; surviving old fragments need manual inspection.',
            'Current HEAD survivors only; deletions, renames, very small/large functions and older history may be missed.'])


def retained_edits(old, new, other, edits):
    result = []
    for tag, a, b, c, d in edits:
        if tag == 'equal':
            continue
        removed, added = old.tokens[a:b], new.tokens[c:d]
        anchor_start, anchor_end = max(0, a - 8), min(len(old.tokens), b + 8)
        anchor = old.tokens[anchor_start:anchor_end]
        if not anchor or not contains(other.tokens, anchor):
            continue
        replacement = old.tokens[max(0, a - 5):a] + added + old.tokens[b:min(len(old.tokens), b + 5)]
        if replacement and contains(other.tokens, replacement):
            continue
        position = locate(other.tokens, anchor)
        result.append(dict(operation=tag, old_line=old.lines[min(a, len(old.lines)-1)],
            new_line=new.lines[min(c, len(new.lines)-1)],
            survivor_line=other.lines[position + min(a - anchor_start, len(anchor)-1)],
            removed_tokens=list(removed), added_tokens=list(added),
            old_excerpt=old.excerpt(a, b), new_excerpt=new.excerpt(c, d)))
    return result


@dataclass
class Unit:
    path: str
    key: str
    line: int
    end_line: int
    tokens: tuple
    lines: tuple
    source: bytes

    def location(self):
        return dict(path=self.path, symbol=self.key, line=self.line, end_line=self.end_line)

    def excerpt(self, begin, end):
        lines = self.source.decode(errors='replace').splitlines()
        first = self.lines[min(begin, len(self.lines)-1)]
        last = self.lines[min(max(begin, end-1), len(self.lines)-1)]
        return '\n'.join(lines[max(0, first-2):min(len(lines), last+1)])[:1800]


def units(path, source, parsers):
    tree = parsers[Path(path).suffix].parse(source)
    found, keys = [], defaultdict(int)

    def visit(node, parents=(), excluded=False):
        if node.type in {'mod_item', 'class_declaration', 'impl_item'}:
            name = node.child_by_field_name('name') or node.child_by_field_name('type')
            label = source[name.start_byte:name.end_byte].decode() if name else ''
            parents = parents + (label,)
            excluded = excluded or label in {'test', 'tests'}
        previous = node.prev_named_sibling
        while previous is not None and previous.type in {'attribute_item', 'line_comment', 'block_comment'}:
            if previous.type == 'attribute_item' and re.search(rb'#\[(?:.*::)?test\b|cfg\s*\(\s*test\s*\)', source[previous.start_byte:previous.end_byte]):
                excluded = True
            previous = previous.prev_named_sibling
        is_fn = node.type in {'function_item', 'function_declaration', 'method_definition', 'function_expression', 'arrow_function'}
        if is_fn:
            body = node.child_by_field_name('body')
            name = node.child_by_field_name('name')
            if name is None and node.parent is not None and node.parent.type in {'variable_declarator', 'pair'}:
                name = node.parent.child_by_field_name('name') or node.parent.child_by_field_name('key')
            if not excluded and body is not None and not body.has_error and name is not None:
                key = '::'.join(parents + (source[name.start_byte:name.end_byte].decode(),))
                keys[key] += 1
                key += '#' + str(keys[key])
                tokens, lines = tokenize(body, source)
                if tokens:
                    found.append(Unit(path, key, node.start_point.row+1, node.end_point.row+1, tuple(tokens), tuple(lines), source))
            return
        for child in node.named_children:
            visit(child, parents, excluded)

    visit(tree.root_node)
    return found, tree.root_node.has_error


def tokenize(root, source):
    tokens, lines = [], []
    def visit(node):
        if 'comment' in node.type:
            return
        if not node.children:
            value = source[node.start_byte:node.end_byte].decode(errors='replace')
            if node.type in {'identifier', 'shorthand_property_identifier'}:
                parent = node.parent
                is_call = parent is not None and parent.type == 'call_expression' and parent.child_by_field_name('function') == node
                if not is_call and parent.type not in {'scoped_identifier', 'macro_invocation'}:
                    value = '$id'
            tokens.append(value)
            lines.append(node.start_point.row+1)
        else:
            for child in node.children:
                visit(child)
    visit(root)
    return tokens, lines


def eligible(path):
    p = Path(path)
    excluded = {'tests', 'test', '__tests__', '__fixtures__', 'fixtures', 'examples', 'playground', 'node_modules', 'target', 'vendor', 'vendored', 'generated', 'snapshots', '.git'}
    return (p.suffix in {'.rs', '.ts', '.tsx'} and not set(p.parts) & excluded
        and not re.search(r'(?:\.test|\.spec|\.generated|\.d)\.[^.]+$', p.name))


def windows(tokens, size):
    for i in range(len(tokens)-size+1):
        yield hashlib.blake2b('\0'.join(tokens[i:i+size]).encode(), digest_size=12).digest()


def contains(haystack, needle):
    return locate(haystack, needle) >= 0


def locate(haystack, needle):
    for i in range(len(haystack)-len(needle)+1):
        if haystack[i:i+len(needle)] == needle:
            return i
    return -1


def git(repo, *args, optional=False):
    proc = subprocess.run(['git', '-C', str(repo), *args], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if proc.returncode and not optional:
        raise RuntimeError(proc.stderr.decode(errors='replace'))
    return proc.stdout if proc.returncode == 0 else b''


if __name__ == '__main__':
    main()
