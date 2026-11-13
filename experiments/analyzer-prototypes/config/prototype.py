#!/usr/bin/env python3
"""Narrow, syntax-only experiment: literal environment default/parser drift.

Only reads tracked Rust/TypeScript files. Findings are review candidates, not
proof that two sites execute in the same process or implement the same contract.
"""
import argparse
import collections
import json
import pathlib
import re
import subprocess

from tree_sitter import Language, Parser
import tree_sitter_rust
import tree_sitter_typescript


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("roots", nargs="+", type=pathlib.Path)
    ap.add_argument("--output", type=pathlib.Path)
    args = ap.parse_args()
    result = {"prototype": "literal-env-default-drift-v1", "repositories": [scan_repo(p.resolve()) for p in args.roots]}
    encoded = json.dumps(result, indent=2, ensure_ascii=False) + "\n"
    if args.output:
        args.output.write_text(encoded)
    else:
        print(encoded, end="")


def scan_repo(root):
    tracked = git(root, "ls-files", "-z").split("\0")
    tracked_set = set(tracked)
    head = git(root, "rev-parse", "HEAD", optional=True).strip()
    counts = collections.Counter()
    sites, unsupported = [], []
    parsers = {
        "rust": Parser(Language(tree_sitter_rust.language())),
        "typescript": Parser(Language(tree_sitter_typescript.language_typescript())),
        "tsx": Parser(Language(tree_sitter_typescript.language_tsx())),
    }
    for rel in sorted(tracked_set):
        path = pathlib.PurePosixPath(rel)
        if path.suffix not in {".rs", ".ts", ".tsx", ".mts", ".cts"}:
            continue
        counts["tracked_language_files"] += 1
        if any(p in {"node_modules", "vendor", "vendored", "target", "dist", "generated", ".yarn"} for p in path.parts) or rel.endswith(".d.ts"):
            counts["excluded_files"] += 1
            continue
        absolute = root / rel
        if absolute.is_symlink() or not absolute.is_file():
            counts["excluded_files"] += 1
            continue
        data = absolute.read_bytes()
        if re.search(rb"(?i)(?:@generated|code generated|automatically generated|auto-generated)", data[:800]):
            counts["excluded_generated_files"] += 1
            continue
        lang = "rust" if path.suffix == ".rs" else "tsx" if path.suffix == ".tsx" else "typescript"
        counts["scanned_files"] += 1
        counts[lang + "_files"] += 1
        tree = parsers[lang].parse(data)
        if tree.root_node.has_error:
            counts["files_with_parse_errors"] += 1
        package = manifest_scope(rel, tracked_set, lang)
        file_sites, file_unsupported, env_count = collect_file(rel, data, tree, lang, package)
        sites.extend(file_sites)
        unsupported.extend(file_unsupported)
        counts["recognized_literal_env_reads"] += env_count
    candidates, cross_scope = compare_sites(sites)
    counts["literal_default_sites"] = len(sites)
    counts["within_scope_candidates"] = len(candidates)
    counts["cross_scope_differences"] = len(cross_scope)
    counts["unsupported_cases"] = len(unsupported)
    return {
        "name": root.name, "root": str(root), "head": head or None,
        "scope": "tracked .rs/.ts/.tsx/.mts/.cts; tests/examples/tooling retained in separate scopes; nearest manifest + runtime role + provider",
        "limitations": [
            "No type checker, runtime execution, environment configuration, or deployment/process graph.",
            "Only direct literal-key std::env::{var,var_os}, process.env, import.meta.env, Deno.env.get calls.",
            "Rust aliases, local shadowing, generic config getters and inter-statement dataflow are unresolved.",
            "Dynamic defaults, default constants, nested alternatives and multi-key precedence chains abstain.",
            "Tests/examples have separate scope; separate binaries/scripts may still share or isolate environment by design.",
            "Numeric defaults normalize only plain finite decimal literals; parser descriptions are syntactic.",
        ],
        "counts": dict(sorted(counts.items())),
        "sites": sites, "candidates": candidates,
        "cross_scope_differences": cross_scope,
        "unsupported": unsupported,
    }


def collect_file(rel, data, tree, lang, package):
    text = data.decode("utf-8", errors="replace")
    nodes = list(walk(tree.root_node))
    reads = []
    unsupported = []
    for node in nodes:
        access = env_read(node, data, lang, text)
        if access and access[1] is None:
            unsupported.append(location(rel, node) | {"key": None, "reason": "dynamic or unsupported environment key"})
        elif access:
            reads.append((node, access))
    sites, seen = [], set()
    for env_node, access in reads:
        provider, key = access
        found = find_default(env_node, data, lang)
        if found is None:
            unsupported.append(location(rel, env_node) | {"key": key, "reason": "no supported literal default in the same expression"})
            continue
        fallback_node, literal_node, operator = found
        outer = expression_outer(fallback_node, data)
        linked = [(n, a) for n, a in reads if outer.start_byte <= n.start_byte and n.end_byte <= outer.end_byte]
        if len(linked) != 1:
            unsupported.append(location(rel, env_node) | {"key": key, "reason": "multiple env reads / precedence in default expression", "expression": snippet(outer, data)})
            continue
        if lang != "rust" and fallback_node.child_by_field_name("left").type == "binary_expression":
            unsupported.append(location(rel, env_node) | {"key": key, "reason": "nested boolean/default precedence", "expression": snippet(outer, data)})
            continue
        value = literal_value(literal_node, data, lang)
        if value is None:
            unsupported.append(location(rel, env_node) | {"key": key, "reason": "nonliteral or unsupported default", "expression": snippet(fallback_node, data)})
            continue
        if contains_error(outer):
            unsupported.append(location(rel, env_node) | {"key": key, "reason": "parse error in enclosing expression"})
            continue
        parsing = parser_signature(outer, data, lang)
        if parsing == "unknown-transform":
            unsupported.append(location(rel, env_node) | {"key": key, "reason": "unrecognized transform before/after default", "expression": snippet(outer, data)})
            continue
        identity = (env_node.start_byte, fallback_node.start_byte)
        if identity in seen:
            continue
        seen.add(identity)
        context, role = context_scope(rel, env_node, data, lang)
        site = location(rel, env_node) | {
            "provider": provider, "key": key, "manifest": package,
            "context": context, "role": role,
            "scope": f"{package}|{context}|{role}|{provider}",
            "default": value, "normalized_default": normalize_default(value, parsing),
            "fallback": operator, "parsing": parsing,
            "expression": snippet(outer, data),
        }
        sites.append(site)
    return sites, unsupported, len(reads)


def compare_sites(sites):
    scoped = collections.defaultdict(list)
    all_keys = collections.defaultdict(list)
    for site in sites:
        scoped[(site["scope"], site["key"])].append(site)
        all_keys[(site["provider"], site["key"])].append(site)
    candidates = []
    for (scope, key), group in sorted(scoped.items()):
        changes = differences(group)
        if changes:
            candidates.append({"id": f"{scope}|{key}", "key": key, "scope": scope, "differences": changes,
                               "classification": "review-candidate", "sites": group})
    cross_scope = []
    for (provider, key), group in sorted(all_keys.items()):
        changes = differences(group)
        if changes and len({s["scope"] for s in group}) > 1:
            cross_scope.append({"key": key, "provider": provider, "differences": changes,
                                "classification": "cross-scope-observation-only", "sites": group})
    return candidates, cross_scope


def env_read(node, data, lang, source):
    if lang == "rust" and node.type == "call_expression":
        function = node.child_by_field_name("function")
        name = compact(function, data)
        allowed = {"std::env::var", "std::env::var_os", "::std::env::var", "::std::env::var_os"}
        if re.search(r"use\s+std::(?:env\b|\{[^;]*\benv\b)", source):
            allowed |= {"env::var", "env::var_os"}
        for fn in ("var", "var_os"):
            if re.search(r"use\s+std::env::(?:" + fn + r"\s*;|\{[^;]*\b" + fn + r"\b)", source):
                allowed.add(fn)
        if name not in allowed:
            return None
        args = arguments(node)
        key = string_value(args[0], data) if len(args) == 1 else None
        return "os-env", key
    if lang != "rust":
        if node.type in {"member_expression", "subscript_expression"}:
            obj = compact(node.child_by_field_name("object"), data)
            if obj not in {"process.env", "import.meta.env"}:
                return None
            key_node = node.child_by_field_name("property") or node.child_by_field_name("index")
            key = compact(key_node, data) if node.type == "member_expression" else string_value(key_node, data)
            return "os-env" if obj == "process.env" else "vite-env", key
        if node.type == "call_expression" and compact(node.child_by_field_name("function"), data) == "Deno.env.get":
            args = arguments(node)
            key = string_value(args[0], data) if len(args) == 1 else None
            return "deno-env", key
    return None


def find_default(env_node, data, lang):
    cur = env_node
    while cur.parent is not None:
        parent = cur.parent
        if parent.type in {"variable_declarator", "let_declaration", "expression_statement", "return_expression", "return_statement", "block", "statement_block", "pair", "field_initializer", "arrow_function", "closure_expression"}:
            return None
        if lang == "rust" and parent.type == "call_expression":
            fn = parent.child_by_field_name("function")
            if fn is not None and fn.type == "field_expression":
                receiver = fn.child_by_field_name("value")
                name = compact(fn.child_by_field_name("field"), data)
                args = arguments(parent)
                if receiver is not None and receiver.start_byte <= env_node.start_byte < receiver.end_byte:
                    if name in {"unwrap_or", "map_or"} and args:
                        return parent, args[0], name
                    if name == "unwrap_or_else" and args and args[0].type == "closure_expression":
                        return parent, args[0].child_by_field_name("body"), name
        if lang != "rust" and parent.type == "binary_expression":
            left = parent.child_by_field_name("left")
            right = parent.child_by_field_name("right")
            if left is not None and right is not None and left.start_byte <= env_node.start_byte < left.end_byte:
                operator = data[left.end_byte:right.start_byte].decode().strip()
                if operator in {"??", "||"}:
                    return parent, right, operator
        cur = parent
    return None


def expression_outer(node, data):
    cur = node
    wrappers = {"parenthesized_expression", "binary_expression", "unary_expression", "await_expression", "as_expression", "satisfies_expression", "non_null_expression", "field_expression", "generic_function", "call_expression", "try_expression", "member_expression", "subscript_expression"}
    while cur.parent is not None and cur.parent.type in wrappers:
        cur = cur.parent
    if cur.parent is not None and cur.parent.type == "arguments":
        call = cur.parent.parent
        if call is not None and call.type == "call_expression" and compact(call.child_by_field_name("function"), data) in {"Number", "parseInt", "Number.parseInt", "parseFloat", "Number.parseFloat", "Boolean", "JSON.parse"}:
            return call
    return cur


def literal_value(node, data, lang):
    if node is None:
        return None
    if node.type in {"parenthesized_expression", "block"} and len(node.named_children) == 1:
        return literal_value(node.named_children[0], data, lang)
    value = string_value(node, data)
    if value is not None:
        return {"kind": "string", "value": value}
    if node.type in {"integer_literal", "float_literal", "number"}:
        raw = compact(node, data).replace("_", "")
        raw = re.sub(r"(?:u|i)(?:8|16|32|64|128|size)$|f(?:32|64)$", "", raw)
        try:
            parsed = float(raw) if any(c in raw for c in ".eE") else int(raw, 0) if raw.startswith(("0x", "0b", "0o")) else int(raw)
            return {"kind": "number", "value": parsed}
        except ValueError:
            return None
    if node.type in {"boolean_literal", "true", "false"}:
        return {"kind": "boolean", "value": compact(node, data) == "true"}
    if node.type == "unary_expression":
        raw = compact(node, data)
        if re.fullmatch(r"-\d+(?:\.\d+)?", raw):
            return {"kind": "number", "value": float(raw) if "." in raw else int(raw)}
    if lang == "rust" and node.type == "call_expression":
        function = node.child_by_field_name("function")
        args = arguments(node)
        if function is not None and function.type == "field_expression" and not args:
            if compact(function.child_by_field_name("field"), data) in {"to_owned", "to_string", "into"}:
                return literal_value(function.child_by_field_name("value"), data, lang)
        if compact(function, data) in {"String::from", "OsString::from"} and len(args) == 1:
            return literal_value(args[0], data, lang)
    return None


def parser_signature(node, data, lang):
    parsers = []
    for child in walk(node):
        if child.type != "call_expression":
            continue
        function = child.child_by_field_name("function")
        name = compact(function, data)
        if lang == "rust":
            match = re.search(r"\.parse(?:::<([^>]+)>)?$", name)
            if match:
                parsers.append("rust-parse:" + (match.group(1) or "inferred"))
            elif function is not None and function.type == "field_expression":
                method = compact(function.child_by_field_name("field"), data)
                if method not in {"unwrap_or", "unwrap_or_else", "map_or", "ok", "and_then", "to_string", "to_owned", "into", "to_str", "to_string_lossy"}:
                    return "unknown-transform"
        elif name in {"Number", "parseInt", "Number.parseInt", "parseFloat", "Number.parseFloat", "Boolean", "JSON.parse"}:
            if "parseInt" in name:
                args = arguments(child)
                radix = compact(args[1], data) if len(args) > 1 else "auto"
                parsers.append("parseInt:" + radix)
            else:
                parsers.append(name.replace("Number.parseFloat", "parseFloat"))
        elif name != "Deno.env.get":
            return "unknown-transform"
    return "+".join(sorted(set(parsers))) or "raw"


def normalize_default(value, parsing):
    if value["kind"] == "string" and (parsing.startswith("rust-parse:") or parsing in {"Number", "parseFloat", "parseInt:10", "parseInt:auto"}):
        raw = value["value"]
        if re.fullmatch(r"-?(?:0|[1-9]\d*)(?:\.\d+)?", raw):
            return {"kind": "number", "value": float(raw) if "." in raw else int(raw)}
    return value


def context_scope(rel, node, data, lang):
    path = pathlib.PurePosixPath(rel)
    lower = rel.lower()
    if any(p in {"benches", "benchmarks"} for p in path.parts) or re.search(r"\.(?:perf|bench)\.", lower):
        return "benchmark", "benchmark"
    if any(p in {"test", "tests", "__tests__", "fixtures", "testdata", "test-fixtures", "benches"} for p in path.parts) or re.search(r"(?:\.test\.|\.spec\.|_test\.)", lower):
        return "test", "test"
    if any(p in {"example", "examples", "playground", "playgrounds"} for p in path.parts):
        return "example", str(path.parent)
    if lang == "rust":
        cur = node
        while cur is not None:
            if cur.type in {"function_item", "mod_item"}:
                prev = cur.prev_named_sibling
                while prev is not None and prev.type in {"attribute_item", "line_comment", "block_comment"}:
                    if prev.type == "attribute_item" and re.search(r"cfg\s*\(\s*test\s*\)|#\[\s*(?:tokio::)?test\b", text_of(prev, data)):
                        return "test", "test"
                    prev = prev.prev_named_sibling
            cur = cur.parent
        if path.name == "build.rs":
            return "tooling", "build-script"
        if "bin" in path.parts:
            index = path.parts.index("bin")
            return "production", "binary:" + path.parts[index + 1]
    if any(p in {"scripts", "tasks", "tools"} for p in path.parts) or ".config." in path.name:
        return "tooling", str(path.parent)
    return "production", "runtime"


def differences(group):
    if len(group) < 2:
        return []
    out = []
    for field, label in (("normalized_default", "literal-default"), ("parsing", "parser"), ("fallback", "fallback-semantics")):
        variants = {json.dumps(s[field], sort_keys=True) for s in group}
        if len(variants) > 1:
            if field == "fallback" and all(s[field] in {"unwrap_or", "unwrap_or_else"} for s in group):
                continue
            out.append(label)
    return out


def manifest_scope(rel, tracked, lang):
    manifest = "Cargo.toml" if lang == "rust" else "package.json"
    path = pathlib.PurePosixPath(rel).parent
    for parent in (path, *path.parents):
        candidate = str(parent / manifest)
        if candidate in tracked:
            return candidate
    return "<repository>"


def string_value(node, data):
    if node is None:
        return None
    raw = text_of(node, data)
    if node.type == "raw_string_literal":
        match = re.fullmatch(r'r(#+)?"(.*)"\1', raw, re.S) if raw.startswith('r#') else re.fullmatch(r'r"(.*)"', raw, re.S)
        return match.group(2) if match and raw.startswith('r#') else match.group(1) if match else None
    if node.type not in {"string_literal", "string"} or len(raw) < 2:
        return None
    if "\\" in raw:
        try:
            return json.loads(raw) if raw.startswith('"') else None
        except ValueError:
            return None
    return raw[1:-1]


def arguments(node):
    args = node.child_by_field_name("arguments")
    return [n for n in args.named_children if n.type not in {"comment", "line_comment", "block_comment"}] if args else []


def walk(node):
    yield node
    for child in node.named_children:
        yield from walk(child)


def contains_error(node):
    return node.has_error


def text_of(node, data):
    return data[node.start_byte:node.end_byte].decode("utf-8", errors="replace") if node else ""


def compact(node, data):
    return re.sub(r"\s+", "", text_of(node, data))


def snippet(node, data):
    return text_of(node, data)[:1000]


def location(rel, node):
    return {"file": rel, "line": node.start_point.row + 1, "end_line": node.end_point.row + 1}


def git(root, *args, optional=False):
    result = subprocess.run(["git", "-C", str(root), *args], check=not optional, capture_output=True, text=True)
    return result.stdout if result.returncode == 0 else ""


if __name__ == "__main__":
    main()
