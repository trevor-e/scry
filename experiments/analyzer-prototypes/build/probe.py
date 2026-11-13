#!/usr/bin/env python3
"""Read-only, deliberately narrow Cargo build-script input/watch comparison."""
from __future__ import annotations

import argparse
import ast
import json
import os
from pathlib import Path
import re
import subprocess
import tomllib

from tree_sitter import Language, Parser
import tree_sitter_rust

PARSER = Parser(Language(tree_sitter_rust.language()))
EXCLUDED = {".git", "target", "node_modules", "vendor", "vendored", "third_party"}
CARGO_ENV = {"OUT_DIR", "TARGET", "HOST", "NUM_JOBS", "OPT_LEVEL", "DEBUG", "PROFILE", "RUSTC", "RUSTDOC", "RUSTC_LINKER", "CARGO", "CARGO_MANIFEST_DIR", "CARGO_MANIFEST_LINKS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_MAKEFLAGS"}
SAFE_ROOTS = {"std", "core", "alloc", "env", "fs", "path", "process", "io", "Path", "PathBuf", "File", "String", "Vec", "Option", "Result", "OsStr", "OsString", "BTreeMap", "HashMap", "LazyLock", "Command"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("repos", nargs="+", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    result = {"check": "missing-build-rerun", "version": 1, "repositories": [scan_repo(p.resolve()) for p in args.repos]}
    result["summary"] = {key: sum(r["counts"][key] for r in result["repositories"]) for key in ("manifests", "build_scripts", "inputs", "candidates", "unknown", "covered", "excluded")}
    result["limits"] = [
        "Syntax-only: no execution of corpus build scripts or external helpers; unresolved helper effects cause abstention.",
        "Only manifest-selected build scripts, direct std env/fs/File reads, simple lexical path construction, and println! rerun directives.",
        "Local function bodies are followed from main by bare function name; arbitrary call/trait/macro expansion is unsupported.",
        "Conditional execution is not modeled: observed watches may over-suppress, so absence of findings is not proof of coverage.",
        "Subprocess inputs (git/compilers), filesystem existence probes, build dependency implementation reads, and package include/exclude glob expansion are not inferred.",
    ]
    encoded = json.dumps(result, indent=2) + "\n"
    if args.output:
        args.output.write_text(encoded)
    else:
        print(encoded, end="")


def scan_repo(root):
    tracked = git(root, "ls-files", "-z").split("\0")
    paths = sorted(p for p in tracked if p and not EXCLUDED.intersection(Path(p).parts))
    scripts, manifest_errors = [], []
    for name in paths:
        if Path(name).name != "Cargo.toml":
            continue
        try:
            manifest = tomllib.loads((root / name).read_text())
        except (OSError, tomllib.TOMLDecodeError) as error:
            manifest_errors.append({"path": name, "error": str(error)})
            continue
        package = manifest.get("package")
        if not package or package.get("build") is False:
            continue
        build = package.get("build", "build.rs")
        if build is True:
            build = "build.rs"
        script = Path(name).parent / build
        if str(script) in paths:
            scripts.append(scan_script(root, script, manifest))
    counts = {"manifests": sum(Path(p).name == "Cargo.toml" for p in paths), "build_scripts": len(scripts)}
    counts.update({key: sum(len(s[key]) for s in scripts) for key in ("inputs", "candidates", "unknown", "covered", "excluded")})
    return {"repo": root.name, "root": str(root), "head": git(root, "rev-parse", "HEAD").strip(), "counts": counts, "scripts": scripts, "manifest_errors": manifest_errors}


def scan_script(root, path, manifest):
    src = (root / path).read_bytes()
    tree = PARSER.parse(src)
    functions = {text(n.child_by_field_name("name"), src): n for n in walk(tree.root_node) if n.type == "function_item"}
    reachable, pending = {}, ["main"]
    while pending:
        name = pending.pop()
        if name in reachable or name not in functions:
            continue
        reachable[name] = functions[name]
        for node in walk(functions[name]):
            if node.type == "call_expression":
                called = text(node.child_by_field_name("function"), src)
                if called in functions and called not in reachable:
                    pending.append(called)
    nodes = {n.id: n for f in reachable.values() for n in walk(f)}
    ordered = sorted(nodes.values(), key=lambda n: (n.start_byte, n.end_byte))
    # Constants are visible at module and function scope; let bindings stay lexical.
    declarations = [n for n in walk(tree.root_node) if n.type in ("let_declaration", "const_item", "static_item")]
    package_dir = root / path.parent
    if manifest.get("package", {}).get("build") not in (None, True, False):
        # A custom build.rs path can live below its package root.
        depth = len(Path(manifest["package"]["build"]).parts) - 1
        for _ in range(depth):
            package_dir = package_dir.parent
    resolver = Resolver(src, declarations, package_dir)
    watches, automatic, inputs, helper_calls = [], set(), [], []
    for node in ordered:
        if node.type == "macro_invocation":
            macro = text(node.child_by_field_name("macro"), src)
            body = next((c for c in node.named_children if c.type == "token_tree"), None)
            strings = [c for c in body.named_children if c.type in ("string_literal", "raw_string_literal")] if body else []
            if macro in ("env", "option_env") and strings:
                value = literal(strings[0], src)
                if value is not None:
                    automatic.add(value)
            if macro in ("println", "print") and strings:
                fmt = literal(strings[0], src)
                if fmt:
                    for directive in fmt.splitlines():
                        match = re.fullmatch(r"cargo::?(rerun-if-changed|rerun-if-env-changed)=(.*)", directive)
                        if match:
                            kind = "file" if match[1] == "rerun-if-changed" else "env"
                            value = match[2]
                            symbol = None
                            resolved = value
                            if "{" in value:
                                resolved, symbol = resolve_format(value, body, strings[0], node, resolver)
                            watches.append({"kind": kind, "value": resolved, "symbol": symbol, "line": node.start_point.row + 1, "expression": text(node, src)})
            elif macro not in ("env", "option_env", "format", "assert", "assert_eq", "assert_ne", "debug_assert", "debug_assert_eq", "panic", "unreachable", "todo", "vec", "cfg", "eprintln", "eprint", "write", "writeln", "matches", "concat", "include_str", "include_bytes"):
                helper_calls.append({"line": node.start_point.row + 1, "call": macro + "!"})
        if node.type == "mod_item" and node.child_by_field_name("body") is None:
            helper_calls.append({"line": node.start_point.row + 1, "call": text(node, src)})
        if node.type != "call_expression":
            continue
        function = node.child_by_field_name("function")
        name = text(function, src).lstrip(":")
        args = node.child_by_field_name("arguments").named_children
        kind = None
        if re.fullmatch(r"(?:std::)?env::var(?:_os)?", name):
            kind = "env"
        elif re.fullmatch(r"(?:std::)?fs::(?:read|read_to_string|read_dir)|(?:(?:std::)?fs::)?File::open", name):
            kind = "file"
        if kind and args:
            value = resolver.resolve(args[0], node)
            inputs.append({"kind": kind, "value": value, "symbol": resolver.symbol(args[0], node), "line": node.start_point.row + 1, "expression": text(node, src), "function": enclosing_function(node, src)})
        elif function and function.type in ("identifier", "scoped_identifier"):
            root_name = name.split("::")[0]
            if name not in functions and root_name not in SAFE_ROOTS and name not in {"Some", "Ok", "Err", "drop"}:
                helper_calls.append({"line": node.start_point.row + 1, "call": name})
    # Compile-time env! is tracked even in unused items: rustc sees it while compiling.
    for node in walk(tree.root_node):
        if node.type == "macro_invocation" and text(node.child_by_field_name("macro"), src) in ("env", "option_env"):
            for child in walk(node):
                if child.type in ("string_literal", "raw_string_literal"):
                    value = literal(child, src)
                    if value is not None:
                        automatic.add(value)
        if node.type == "mod_item" and node.child_by_field_name("body") is None:
            helper_calls.append({"line": node.start_point.row + 1, "call": text(node, src)})
    result = {"path": str(path), "package": manifest["package"].get("name"), "context": "example/test" if {"examples", "tests", "bench", "benches", "fixtures", "templates"}.intersection(path.parts) else "production", "parse_error": tree.root_node.has_error, "inputs": inputs, "watches": watches, "automatic_env": sorted(automatic), "helper_calls": unique(helper_calls), "candidates": [], "unknown": [], "covered": [], "excluded": []}
    for entry in inputs:
        value = entry["value"]
        covered_by = [w for w in watches if covers(w, entry, package_dir)]
        if value and entry["kind"] == "env" and (value in CARGO_ENV or value.startswith(("CARGO_CFG_", "CARGO_FEATURE_", "CARGO_PKG_", "DEP_"))):
            status, reason = "excluded", "Cargo-managed environment variable"
        elif value and entry["kind"] == "env" and value in automatic:
            status, reason = "covered", "env!/option_env! automatically triggers rebuilds"
        elif value and entry["kind"] == "file" and value.startswith("$OUT_DIR"):
            status, reason = "excluded", "generated output under OUT_DIR"
        elif covered_by:
            status, reason = "covered", "explicit matching watch"
        elif entry["kind"] == "file" and not watches and not helper_calls and value and is_inside(package_dir / value, package_dir) and not (manifest["package"].get("include") or manifest["package"].get("exclude")):
            status, reason = "covered", "Cargo default package-file scan (no explicit rerun directives)"
        elif tree.root_node.has_error:
            status, reason = "unknown", "parse error"
        elif helper_calls:
            status, reason = "unknown", "unresolved helper effects may emit rerun directives"
        elif value is None:
            status, reason = "unknown", "dynamic input value"
        elif any(w["kind"] == entry["kind"] and w["value"] is None for w in watches):
            status, reason = "unknown", "unresolved dynamic watch"
        elif entry["kind"] == "file" and not watches and (manifest["package"].get("include") or manifest["package"].get("exclude")):
            status, reason = "unknown", "Cargo package include/exclude rules not expanded"
        else:
            status, reason = "candidates", "input has no observed matching rerun declaration"
        result[status].append({**entry, "reason": reason, "watch_lines": [w["line"] for w in covered_by]})
    return result


def resolve_format(value, body, string, context, resolver):
    # Only a whole interpolated value, including .display(), is modeled.
    match = re.fullmatch(r"\{([A-Za-z_][A-Za-z_0-9]*)?\}", value)
    if not match:
        return None, None
    if match[1]:
        return resolver.resolve_name(match[1], context), resolver.name_symbol(match[1], context)
    rest = resolver.src[string.end_byte:body.end_byte - 1].strip().lstrip(b",").strip()
    if not rest:
        return None, None
    # Parsed nodes have separate byte offsets, so resolve identifier text in original context.
    return resolver.resolve_text(rest.decode(), context), resolver.symbol_text(rest.decode(), context)


class Resolver:
    def __init__(self, src, declarations, package_dir):
        self.src = src
        self.declarations = declarations
        self.package_dir = package_dir

    def resolve(self, node, context, depth=0):
        if node is None or depth > 12:
            return None
        if node.type in ("string_literal", "raw_string_literal"):
            return literal(node, self.src)
        if node.type == "identifier":
            return self.resolve_name(text(node, self.src), context, depth + 1)
        if node.type in ("reference_expression", "parenthesized_expression", "try_expression"):
            return self.resolve(node.named_children[-1], context, depth + 1)
        if node.type == "call_expression":
            fun = node.child_by_field_name("function")
            args = node.child_by_field_name("arguments").named_children
            name = text(fun, self.src)
            if name in ("Path::new", "PathBuf::from", "std::path::Path::new", "std::path::PathBuf::from") and args:
                return self.resolve(args[0], context, depth + 1)
            if re.fullmatch(r"(?:std::)?env::var(?:_os)?", name) and args:
                key = self.resolve(args[0], context, depth + 1)
                if key == "CARGO_MANIFEST_DIR":
                    return str(self.package_dir)
                if key == "OUT_DIR":
                    return "$OUT_DIR"
            if fun.type == "field_expression":
                method = text(fun.child_by_field_name("field"), self.src)
                receiver = self.resolve(fun.child_by_field_name("value"), context, depth + 1)
                if method in ("unwrap", "expect", "as_path", "as_ref", "to_path_buf", "display", "to_str", "to_string_lossy", "clone"):
                    return receiver
                if method == "join" and args:
                    arg = self.resolve(args[0], context, depth + 1)
                    if receiver is not None and arg is not None:
                        return os.path.normpath(os.path.join(receiver, arg))
        return None

    def binding(self, name, context):
        ancestors = set()
        node = context
        while node:
            ancestors.add(node.id)
            node = node.parent
        matches = []
        for decl in self.declarations:
            id_node = decl.child_by_field_name("pattern") or decl.child_by_field_name("name")
            if text(id_node, self.src) != name:
                continue
            if decl.type == "let_declaration" and decl.start_byte >= context.start_byte:
                continue
            if decl.parent.id not in ancestors:
                continue
            matches.append(decl)
        return max(matches, key=lambda n: n.start_byte) if matches else None

    def resolve_name(self, name, context, depth=0):
        decl = self.binding(name, context)
        if decl is None:
            return None
        # A mutated binding cannot safely be resolved from its initial value.
        scope = decl.parent
        for node in walk(scope):
            if decl.end_byte < node.start_byte < context.start_byte and node.type in ("assignment_expression", "compound_assignment_expr"):
                if text(node.child_by_field_name("left"), self.src) == name:
                    return None
        return self.resolve(decl.child_by_field_name("value"), decl, depth + 1)

    def symbol(self, node, context):
        return self.symbol_text(text(node, self.src), context)

    def symbol_text(self, expr, context):
        expr = expr.strip().lstrip("&").strip()
        expr = re.sub(r"\.(?:display|as_path|as_ref|to_str|to_string_lossy)\(\)$", "", expr)
        if re.fullmatch(r"[A-Za-z_]\w*", expr):
            return self.name_symbol(expr, context)
        return None

    def name_symbol(self, name, context):
        decl = self.binding(name, context)
        if decl:
            return f"{name}@{decl.start_byte}"
        # Loop/match binding comparison stays within its nearest lexical block.
        block = context
        while block and block.type != "block":
            block = block.parent
        return f"{name}@block:{block.start_byte}" if block else None

    def resolve_text(self, expr, context):
        expr = expr.strip().lstrip("&").strip()
        expr = re.sub(r"\.(?:display|as_path|as_ref|to_str|to_string_lossy)\(\)$", "", expr)
        if re.fullmatch(r"[A-Za-z_]\w*", expr):
            return self.resolve_name(expr, context)
        if expr.startswith('"'):
            try:
                return ast.literal_eval(expr)
            except (SyntaxError, ValueError):
                return None
        return None


def covers(watch, entry, package_dir):
    if watch["kind"] != entry["kind"]:
        return False
    if watch["symbol"] and watch["symbol"] == entry["symbol"]:
        return True
    a, b = watch["value"], entry["value"]
    if a is None or b is None:
        return False
    if entry["kind"] == "env":
        return a == b
    watched, read = Path(os.path.normpath(package_dir / a)), Path(os.path.normpath(package_dir / b))
    return watched == read or (watched.is_dir() and is_inside(read, watched))


def is_inside(path, root):
    return Path(os.path.abspath(path)).is_relative_to(Path(os.path.abspath(root)))


def literal(node, src):
    value = text(node, src)
    if node.type == "raw_string_literal":
        match = re.fullmatch(r'r(\#*)"(.*)"\1', value, re.S)
        return match[2] if match else None
    try:
        return ast.literal_eval(value)
    except (SyntaxError, ValueError):
        return None



def enclosing_function(node, src):
    while node and node.type != "function_item":
        node = node.parent
    return text(node.child_by_field_name("name"), src) if node else None


def text(node, src):
    return src[node.start_byte:node.end_byte].decode("utf-8", errors="replace") if node else ""


def walk(node):
    yield node
    for child in node.named_children:
        yield from walk(child)


def unique(items):
    seen = set()
    result = []
    for item in items:
        key = json.dumps(item, sort_keys=True)
        if key not in seen:
            seen.add(key)
            result.append(item)
    return result


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args], text=True)


if __name__ == "__main__":
    main()
