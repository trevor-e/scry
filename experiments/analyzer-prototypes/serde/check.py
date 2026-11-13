#!/usr/bin/env python3
"""Conservative JSON-shape shadowing checks for derived Serde untagged enums."""
from __future__ import annotations

import argparse
import dataclasses
import json
import re
import subprocess
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import TypeAlias, assert_never

import tree_sitter
import tree_sitter_rust


@dataclass(frozen=True)
class Atom:
    kind: str


@dataclass(frozen=True)
class Integer:
    minimum: int
    maximum: int


@dataclass(frozen=True)
class Nullable:
    item: Shape


@dataclass(frozen=True)
class Sequence:
    item: Shape


@dataclass(frozen=True)
class TupleShape:
    items: tuple[Shape, ...]


@dataclass(frozen=True)
class Alternatives:
    items: tuple[Shape, ...]


@dataclass(frozen=True)
class Field:
    name: str
    shape: Shape
    optional: bool


@dataclass(frozen=True)
class Object:
    fields: tuple[Field, ...]
    unknown: Shape | None


Shape: TypeAlias = Atom | Integer | Nullable | Sequence | TupleShape | Alternatives | Object


@dataclass
class Source:
    path: str
    data: bytes
    root: tree_sitter.Node


class Unsupported(Exception):
    pass


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("repositories", nargs="+", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--fixtures", action="store_true", help="Allow non-Git fixture directories")
    args = parser.parse_args()
    result = {
        "schema_version": 1,
        "analyzer": "serde-shadowed-untagged-json-variants",
        "scope": "Tracked Rust source; JSON data model only; syntax-restricted acceptance inclusion",
        "repositories": [scan(root.resolve(), args.fixtures) for root in args.repositories],
    }
    encoded = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.output:
        args.output.write_text(encoded)
    else:
        print(encoded, end="")


def scan(root: Path, fixtures: bool) -> dict:
    parser = tree_sitter.Parser(tree_sitter.Language(tree_sitter_rust.language()))
    if fixtures:
        paths = sorted(str(p.relative_to(root)) for p in root.rglob("*.rs"))
        head = None
    else:
        paths = subprocess.check_output(
            ["git", "-C", str(root), "ls-files", "-z", "--", "*.rs"]
        ).decode().split("\0")
        head = subprocess.check_output(["git", "-C", str(root), "rev-parse", "HEAD"]).decode().strip()
    findings, abstentions, inventories = [], [], []
    excluded = Counter()
    stats = Counter()
    for path in sorted(filter(None, paths)):
        parts = set(Path(path).parts)
        if parts & {"vendor", "vendored", "node_modules", "target", "generated", "third_party", "fixtures", "test-data", "testdata"}:
            excluded["dependency_generated_or_fixture_path"] += 1
            continue
        file = root / path
        if file.is_symlink() or not file.is_file():
            excluded["symlink_or_missing"] += 1
            continue
        data = file.read_bytes()
        if re.search(rb"(?i)(@generated|automatically generated|generated (?:by|file)|do not edit)", data[:1500]):
            excluded["generated_header"] += 1
            continue
        stats["rust_files_scanned"] += 1
        # No candidate can occur without this Serde marker; do not parse unrelated files.
        if b"untagged" not in data:
            continue
        tree = parser.parse(data)
        if tree.root_node.has_error:
            abstentions.append({"file": path, "line": 1, "reason": "parse_errors_in_candidate_file"})
            continue
        src = Source(path, data, tree.root_node)
        index = type_index(src)
        for node in walk(src.root):
            if node.type != "enum_item" or "untagged" not in attribute_text(node, src):
                continue
            attrs = attribute_text(node, src)
            name = text(node.child_by_field_name("name"), src)
            loc = {"file": path, "line": node.start_point.row + 1, "enum": name,
                   "context": context(node, path, src)}
            stats["untagged_enum_declarations"] += 1
            inventory = {**loc, "variants": []}
            inventories.append(inventory)
            try:
                ensure_derived(node, src)
                options = serde_options(node, src)
                ensure_options(options, {"untagged", "rename", "rename_all", "rename_all_fields", "deny_unknown_fields", "expecting"})
                if node.child_by_field_name("type_parameters"):
                    raise Unsupported("generic_enum")
                # Container cfg only controls whether the entire finding is present.
                if "cfg_attr" in attrs and re.search(r"cfg_attr\([^\n]*(?:serde|Deserialize)", attrs):
                    raise Unsupported("conditional_deserialization_attributes")
            except Unsupported as exc:
                abstentions.append({**loc, "reason": str(exc)})
                continue
            stats["derived_untagged_enums"] += 1
            earlier = []
            body = node.child_by_field_name("body")
            for variant in body.named_children:
                if variant.type != "enum_variant":
                    continue
                vname = text(variant.child_by_field_name("name"), src)
                vloc = {**loc, "variant": vname, "line": variant.start_point.row + 1}
                try:
                    vopts = serde_options(variant, src)
                    ensure_options(vopts, {"rename", "rename_all"})
                    if re.search(r"#\[(?:cfg|cfg_attr)\b", attribute_text(variant, src)):
                        raise Unsupported("conditional_variant")
                    field_options = {k: v for k, v in options.items() if k == "deny_unknown_fields"}
                    if "rename_all_fields" in options:
                        field_options["rename_all"] = options["rename_all_fields"]
                    if "rename_all" in vopts:
                        field_options["rename_all"] = vopts["rename_all"]
                    shape = fields_shape(variant.child_by_field_name("body"), src, index,
                                         field_options, set())
                    inventory["variants"].append({"name": vname, "line": vloc["line"], "modeled": True,
                                                   "shape": dataclasses.asdict(shape)})
                    stats["modeled_variants"] += 1
                    for previous, pline, pshape in earlier:
                        stats["variant_pairs_compared"] += 1
                        if contains(pshape, shape):
                            findings.append({**vloc, "earlier_variant": previous,
                                             "earlier_line": pline,
                                             "reason": "Every value in the supported JSON shape of this variant is accepted by the earlier variant",
                                             "witness_json": witness(shape),
                                             "earlier_shape": dataclasses.asdict(pshape),
                                             "later_shape": dataclasses.asdict(shape),
                                             "classification": "review_candidate_not_a_confirmed_defect"})
                            break
                    earlier.append((vname, vloc["line"], shape))
                except Unsupported as exc:
                    inventory["variants"].append({"name": vname, "line": vloc["line"], "modeled": False,
                                                   "reason": str(exc)})
                    abstentions.append({**vloc, "reason": str(exc)})
    stats["candidates"] = len(findings)
    return {"name": root.name, "root": str(root), "head": head, "counts": dict(stats),
            "excluded": dict(excluded), "findings": findings, "abstentions": abstentions,
            "enum_inventory": inventories}


def type_index(src: Source) -> dict[str, list[tree_sitter.Node]]:
    index: dict[str, list[tree_sitter.Node]] = {}
    for node in walk(src.root):
        if node.type in {"struct_item", "enum_item", "type_item"}:
            name = text(node.child_by_field_name("name"), src)
            index.setdefault(name, []).append(node)
    return index


def fields_shape(body, src, index, options, active) -> Shape:
    if body is None:
        return Atom("null")
    if body.type == "ordered_field_declaration_list":
        if any(n.type == "attribute_item" for n in body.named_children):
            raise Unsupported("tuple_field_attributes")
        types = [n for i, n in enumerate(body.children) if body.field_name_for_child(i) == "type"]
        shapes = tuple(resolve_type(n, src, index, active) for n in types)
        if len(shapes) == 1:
            return shapes[0]
        return TupleShape(shapes)
    if body.type != "field_declaration_list":
        raise Unsupported("unsupported_field_body")
    fields = []
    for field in body.named_children:
        if field.type != "field_declaration":
            continue
        attrs = attribute_text(field, src)
        if re.search(r"#\[(?:cfg|cfg_attr)\b", attrs):
            raise Unsupported("conditional_field")
        opts = serde_options(field, src)
        ensure_options(opts, {"rename", "default", "skip_serializing", "skip_serializing_if"})
        name = text(field.child_by_field_name("name"), src).removeprefix("r#")
        if "rename" in opts:
            name = opts["rename"]
        elif "rename_all" in options:
            name = rename_field(name, options["rename_all"])
        ty = field.child_by_field_name("type")
        shape = resolve_type(ty, src, index, active)
        optional = "default" in opts or "default" in options or is_option(ty, src, index, set())
        fields.append(Field(name, shape, optional))
    if len({f.name for f in fields}) != len(fields):
        raise Unsupported("duplicate_wire_field_names")
    return Object(tuple(fields), None if "deny_unknown_fields" in options else Atom("any"))


def resolve_type(node, src, index, active) -> Shape:
    raw = re.sub(r"\s+", "", text(node, src))
    # Only builtin names without a local replacement are accepted.
    if raw in index:
        definitions = index[raw]
        if len(definitions) != 1:
            raise Unsupported(f"ambiguous_local_type:{raw}")
        definition = definitions[0]
        if lexical_scope(definition) != lexical_scope(node):
            raise Unsupported(f"type_outside_lexical_scope:{raw}")
        if raw in active:
            raise Unsupported(f"recursive_type:{raw}")
        if definition.child_by_field_name("type_parameters"):
            raise Unsupported(f"generic_local_type:{raw}")
        active = active | {raw}
        if definition.type == "type_item":
            return resolve_type(definition.child_by_field_name("type"), src, index, active)
        if definition.type != "struct_item":
            raise Unsupported(f"non_struct_local_type:{raw}")
        ensure_derived(definition, src)
        opts = serde_options(definition, src)
        ensure_options(opts, {"rename", "rename_all", "deny_unknown_fields", "default", "expecting"})
        body = definition.child_by_field_name("body")
        shape = fields_shape(body, src, index, opts, active)
        if isinstance(shape, Object):
            # Derived named structs also accept sequences, unlike inline untagged
            # struct variants. Missing Option fields in sequences still need default.
            fields = [n for n in body.named_children if n.type == "field_declaration"]
            lengths = [len(fields)]
            for position in range(len(fields) - 1, -1, -1):
                if "default" not in opts and "default" not in serde_options(fields[position], src):
                    break
                lengths.append(position)
            sequences = tuple(TupleShape(tuple(f.shape for f in shape.fields[:length])) for length in lengths)
            return Alternatives((shape, *sequences))
        return shape
    if raw == "String":
        ensure_standard_name("String", node, src)
    if raw in {"String", "std::string::String", "alloc::string::String"}:
        return Atom("string")
    if raw == "bool":
        return Atom("bool")
    if raw == "()":
        return Atom("null")
    if raw in {"serde_json::Value", "::serde_json::Value"}:
        return Atom("any")
    # Qualified serde_json Value imports are safe only for this deliberately narrow form.
    if raw == "Value" and re.search(rb"use\s+serde_json::Value\s*;", src.data):
        ensure_standard_name("Value", node, src, {"serde_json"})
        return Atom("any")
    if re.fullmatch(r"[iu](8|16|32|64)", raw):
        bits = int(raw[1:])
        return Integer(-(2 ** (bits - 1)) if raw[0] == "i" else 0,
                       2 ** (bits - (raw[0] == "i")) - 1)
    if raw in {"f32", "f64"}:
        return Atom("number")
    if node.type == "generic_type":
        base = text(node.child_by_field_name("type"), src)
        args = node.child_by_field_name("type_arguments").named_children
        if base in index:
            raise Unsupported(f"shadowed_generic_type:{base}")
        if "::" not in base:
            ensure_standard_name(base, node, src)
        if base in {"Option", "std::option::Option", "core::option::Option"} and len(args) == 1:
            return Nullable(resolve_type(args[0], src, index, active))
        if base in {"Vec", "std::vec::Vec", "alloc::vec::Vec"} and len(args) == 1:
            return Sequence(resolve_type(args[0], src, index, active))
        if base in {"Box", "std::boxed::Box", "alloc::boxed::Box"} and len(args) == 1:
            return resolve_type(args[0], src, index, active)
        if base in {"HashMap", "BTreeMap", "std::collections::HashMap", "std::collections::BTreeMap"} and len(args) == 2:
            if resolve_type(args[0], src, index, active) != Atom("string"):
                raise Unsupported("non_string_map_key")
            return Object((), resolve_type(args[1], src, index, active))
    raise Unsupported(f"unresolved_type:{raw}")


def contains(earlier: Shape, later: Shape) -> bool:
    if earlier == Atom("any") or earlier == later:
        return True
    if isinstance(later, Alternatives):
        return all(contains(earlier, item) for item in later.items)
    if isinstance(earlier, Alternatives):
        return any(contains(item, later) for item in earlier.items)
    if isinstance(earlier, Nullable):
        if isinstance(later, Nullable):
            return contains(earlier.item, later.item)
        return later == Atom("null") or contains(earlier.item, later)
    if isinstance(later, Nullable):
        return contains(earlier, Atom("null")) and contains(earlier, later.item)
    match earlier, later:
        case Atom("number"), Integer():
            return True
        case Integer(a, b), Integer(c, d):
            return a <= c and b >= d
        case Sequence(a), Sequence(b):
            return contains(a, b)
        case Sequence(a), TupleShape(items):
            return all(contains(a, item) for item in items)
        case TupleShape(a), TupleShape(b):
            return len(a) == len(b) and all(contains(x, y) for x, y in zip(a, b))
        case Object(afields, aunknown), Object(bfields, bunknown):
            amap = {f.name: f for f in afields}
            bmap = {f.name: f for f in bfields}
            for name, field in amap.items():
                other = bmap.get(name)
                if other is None:
                    if not field.optional or (bunknown is not None and not contains(field.shape, bunknown)):
                        return False
                elif (not field.optional and other.optional) or not contains(field.shape, other.shape):
                    return False
            for name, field in bmap.items():
                if name not in amap and (aunknown is None or not contains(aunknown, field.shape)):
                    return False
            if bunknown is not None and (aunknown is None or not contains(aunknown, bunknown)):
                return False
            return True
        case _:
            return False


def witness(shape: Shape):
    match shape:
        case Atom("any" | "null"):
            return None
        case Atom("bool"):
            return False
        case Atom("string"):
            return "example"
        case Atom("number"):
            return 1.5
        case Integer(a, b):
            return max(a, min(b, 0))
        case Nullable():
            return None
        case Sequence():
            return []
        case TupleShape(items):
            return [witness(item) for item in items]
        case Alternatives(items):
            return witness(items[0])
        case Object(fields, _):
            return {f.name: witness(f.shape) for f in fields if not f.optional}
        case Atom(kind):
            raise AssertionError(kind)
        case _:
            assert_never(shape)


def is_option(node, src, index, active) -> bool:
    raw = re.sub(r"\s+", "", text(node, src))
    if raw in index and len(index[raw]) == 1 and raw not in active:
        definition = index[raw][0]
        if definition.type == "type_item":
            return is_option(definition.child_by_field_name("type"), src, index, active | {raw})
    if node.type == "generic_type":
        base = text(node.child_by_field_name("type"), src)
        return base in {"Option", "std::option::Option", "core::option::Option"} and base not in index
    return False


def ensure_derived(node, src) -> None:
    attrs = attribute_text(node, src)
    if not re.search(r"#\[derive\([^\]]*\bDeserialize\b", attrs):
        raise Unsupported("no_unconditional_derive_deserialize")
    if re.search(r"#\[cfg_attr\([^\n]*(?:serde|Deserialize)", attrs):
        raise Unsupported("conditional_deserialization_attributes")


def serde_options(node, src) -> dict[str, str | bool]:
    options = {}
    for attr in attributes(node):
        raw = text(attr, src)
        if not re.match(r"#\[\s*serde\s*\(", raw):
            continue
        body = raw[raw.index("(") + 1:raw.rfind(")")]
        for item in split_options(body):
            match = re.fullmatch(r'([a-z_]+)\s*(?:=\s*("(?:\\.|[^"\\])*"))?', item.strip(), re.S)
            if not match:
                raise Unsupported(f"complex_serde_attribute:{item.strip()}")
            key, value = match.groups()
            if key in options:
                raise Unsupported(f"duplicate_serde_attribute:{key}")
            options[key] = json.loads(value) if value else True
    return options


def split_options(raw):
    return re.split(r',(?=(?:[^"\\]*"(?:\\.|[^"\\])*"[^"\\]*)*[^"\\]*$)', raw.rstrip().rstrip(","))


def ensure_options(options, allowed) -> None:
    unsupported = sorted(set(options) - allowed)
    if unsupported:
        raise Unsupported("unsupported_serde_attributes:" + ",".join(unsupported))
    if "default" in options and options["default"] is not True:
        raise Unsupported("custom_default_function")


def ensure_standard_name(name, node, src, roots=None):
    roots = roots or {"std", "core", "alloc"}
    for declaration in walk(src.root):
        if declaration.type != "use_declaration" or lexical_scope(declaration) != lexical_scope(node):
            continue
        raw = text(declaration, src)
        if not re.search(r"\b" + re.escape(name) + r"\b|\*", raw):
            continue
        match = re.match(r"(?:pub\s+)?use\s+([a-zA-Z_][\w]*)", raw)
        if match is None or match[1] not in roots:
            raise Unsupported(f"unresolved_import_for_builtin:{name}")


def rename_field(name, style):
    if style in {"lowercase", "snake_case"}:
        return name
    if style in {"UPPERCASE", "SCREAMING_SNAKE_CASE"}:
        return name.upper()
    if style == "kebab-case":
        return name.replace("_", "-")
    if style == "SCREAMING-KEBAB-CASE":
        return name.upper().replace("_", "-")
    words = name.split("_")
    if style == "PascalCase":
        return "".join(w[:1].upper() + w[1:] for w in words)
    if style == "camelCase":
        return words[0] + "".join(w[:1].upper() + w[1:] for w in words[1:])
    raise Unsupported(f"unknown_rename_all:{style}")


def context(node, path, src):
    if set(Path(path).parts) & {"tests", "test", "benches"}:
        return "test"
    if set(Path(path).parts) & {"examples", "example"}:
        return "example"
    parent = node
    while parent is not None:
        if re.search(r"#\[(?:cfg\(test\)|test\]|tokio::test)", attribute_text(parent, src)):
            return "test"
        parent = parent.parent
    return "production"


def lexical_scope(node):
    parent = node.parent
    while parent is not None:
        if parent.type in {"mod_item", "function_item", "closure_expression"}:
            return parent.start_byte
        parent = parent.parent
    return -1


def attribute_text(node, src):
    return "\n".join(text(a, src) for a in attributes(node))


def attributes(node):
    result = []
    previous = node.prev_named_sibling
    while previous is not None and previous.type in {"attribute_item", "line_comment", "block_comment"}:
        if previous.type == "attribute_item":
            result.append(previous)
        previous = previous.prev_named_sibling
    return list(reversed(result))


def walk(node):
    yield node
    for child in node.named_children:
        yield from walk(child)


def text(node, src):
    return src.data[node.start_byte:node.end_byte].decode() if node else ""


if __name__ == "__main__":
    main()
