//! Declared but unconsumed: dependencies a manifest declares that no file in its scope
//! imports, feature flags nothing checks that gate only such dependencies, and config knobs a
//! `Deserialize` struct accepts that no code reads.
//!
//! An agent declares the stack a design doc lists and never builds the renderer; a cleanup
//! pass deletes the only `#[cfg(feature = …)]` and leaves the feature, its optional dep and
//! the config section behind. Every finding here is a zero-reference test on a manifest or a
//! struct field, enriched with the commit that declared it or removed its last consumer. Cargo
//! only in this version: package.json and pyproject.toml manifests are found and counted
//! (`Totals::other_manifests`), never parsed; `parse_cargo` is the one manifest reader and the
//! place a package.json / pyproject.toml reader plugs in.

use crate::config::Declared as Cfg;
use crate::discover::{FileKind, SourceFile};
use crate::lang::Language;
use crate::regions::{self, TestRegion};
use globset::{Glob, GlobSet, GlobSetBuilder};
use rayon::prelude::*;
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::process::Command;
use tree_sitter::Node;

// ---------- per-file collection ----------

/// References from one file, by the context they sit in.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct RefCount {
    pub prod: u32,
    pub test: u32,
}

impl RefCount {
    fn bump(&mut self, test: bool) {
        if test { self.test += 1 } else { self.prod += 1 }
    }
    fn add(&mut self, o: RefCount) {
        self.prod += o.prod;
        self.test += o.test;
    }
    pub fn total(&self) -> u32 {
        self.prod + self.test
    }
}

/// One field of a `Deserialize`-derived struct.
#[derive(Debug, Clone)]
pub struct FieldDecl {
    /// The Rust identifier: what reads are keyed by.
    pub name: String,
    /// The serialized key (`#[serde(rename = "…")]`), what docs and the section path use.
    pub key: String,
    pub line: usize,
    /// The bare type name when the type is a plain identifier or `Option<T>` / `Box<T>` /
    /// `Vec<T>` / a map's value: the roll-up link to another candidate struct.
    pub ty: Option<String>,
    /// `#[serde(skip)]` (or `skip_deserializing`): never a knob.
    pub skip: bool,
}

/// A struct that derives `Deserialize`.
#[derive(Debug, Clone)]
pub struct StructDecl {
    pub name: String,
    pub start_line: usize,
    pub end_line: usize,
    pub fields: Vec<FieldDecl>,
    pub deny_unknown_fields: bool,
    pub in_test: bool,
}

/// A file's side of the pass, collected on the metrics pass's tree (`scan`) or one parse
/// (`scry declared`). Rust only; other grammars leave everything empty.
#[derive(Debug, Clone, Default)]
pub struct FileSide {
    pub path: String,
    pub rust: bool,
    /// A Test file: every reference is test context.
    pub test_file: bool,
    /// Crate name (leftmost path segment) -> references, split by context.
    pub crate_refs: HashMap<String, RefCount>,
    /// `feature = "x"` triples inside any token tree (`#[cfg]`, `cfg!`, `quote!`), plus
    /// `cargo:rustc-cfg=feature="x"` literals in a build script.
    pub feature_refs: HashMap<String, u32>,
    /// `CARGO_FEATURE_<X>` suffixes read by a build script (uppercased, `-` -> `_`).
    pub build_feature_env: HashSet<String>,
    /// `#[cfg(test)] mod x;` names: the resolved file is test context.
    pub cfg_test_mods: Vec<String>,
    /// `#![allow(warnings)]` / `#![allow(unused)]`: (line, attribute text).
    pub lint_silenced: Option<(usize, String)>,
    pub structs: Vec<StructDecl>,
    /// Type names that are the target of a deserialise call (`toml::from_str::<T>`, `let x: T =
    /// serde_json::from_str(…)`), with the format crate's name.
    pub consumed_types: HashMap<String, String>,
    /// Field name -> reads (`.field`, `Struct { field, .. }`, `v["field"]`), excluding `impl
    /// Default` / `Serialize` / `Deserialize` bodies; test context counted apart.
    pub field_reads: HashMap<String, RefCount>,
    /// String literals that look like a config file name (`scry.toml`), in order.
    pub config_literals: Vec<String>,
}

fn text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    node.utf8_text(src).unwrap_or("")
}

fn line(node: Node) -> usize {
    node.start_position().row + 1
}

fn never() -> Regex {
    Regex::new("$^").unwrap()
}

fn regex(pattern: &str, what: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|e| {
        eprintln!("warning: [declared] bad {what} regex {pattern:?}: {e}");
        never()
    })
}

fn globset(globs: &[String]) -> GlobSet {
    let mut b = GlobSetBuilder::new();
    for g in globs {
        match Glob::new(g) {
            Ok(g) => {
                b.add(g);
            }
            Err(e) => eprintln!("warning: [declared] bad glob {g:?}: {e}"),
        }
    }
    b.build().unwrap_or_else(|_| GlobSet::empty())
}

/// Config-derived matchers, built once and shared by every file walk.
pub struct Walker {
    rust: bool,
    config_file: Regex,
    config_struct: Regex,
    config_literal: Regex,
    serialize_fn: Regex,
}

#[derive(Clone, Copy, Default)]
struct Ctx {
    in_tt: bool,
    /// Inside an `impl` block (any).
    in_impl: bool,
    /// Inside `impl Default` / `impl Serialize` / `impl Deserialize`, or a method named like
    /// the write side (`serialize_fn_regex`): no field read counts.
    excluded_impl: bool,
    /// This node is the `function:` of a `call_expression`: `.method()` is not a field read.
    call_fn: bool,
}

struct State<'a> {
    src: &'a [u8],
    out: &'a mut FileSide,
    regions: &'a [TestRegion],
    build_script: bool,
    config_literal: &'a Regex,
    serialize_fn: &'a Regex,
}

impl State<'_> {
    fn in_test(&self, n: Node) -> bool {
        self.out.test_file || regions::contains(self.regions, n.start_byte())
    }

    fn crate_ref(&mut self, name: &str, test: bool) {
        if !name.is_empty() {
            self.out.crate_refs.entry(name.to_string()).or_default().bump(test);
        }
    }

    fn field_read(&mut self, name: &str, test: bool) {
        self.out.field_reads.entry(name.to_string()).or_default().bump(test);
    }

    /// Leftmost identifier of every leaf path of a `use` tree.
    fn use_roots(&mut self, arg: Node, test: bool) {
        let mut stack = vec![arg];
        while let Some(n) = stack.pop() {
            match n.kind() {
                "identifier" => {
                    let t = text(n, self.src);
                    self.crate_ref(t, test);
                }
                "scoped_identifier" | "scoped_use_list" => match n.child_by_field_name("path") {
                    Some(p) => stack.push(p),
                    // `use ::log::debug;`: the name after the leading `::` is the crate.
                    None => {
                        if let Some(x) = n.child_by_field_name("name").or_else(|| n.child_by_field_name("list")) {
                            stack.push(x);
                        }
                    }
                },
                "use_as_clause" => {
                    if let Some(p) = n.child_by_field_name("path") {
                        stack.push(p);
                    }
                }
                "use_list" | "use_wildcard" => {
                    let mut c = n.walk();
                    for ch in n.named_children(&mut c) {
                        stack.push(ch);
                    }
                }
                _ => {}
            }
        }
    }

    fn string_literal(&mut self, n: Node) {
        let t = text(n, self.src);
        let inner = t.trim_start_matches(['r', '#', 'b']).trim_start_matches('"').trim_end_matches(['"', '#']);
        if self.build_script {
            if let Some(rest) = inner.strip_prefix("CARGO_FEATURE_") {
                let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                if !name.is_empty() {
                    self.out.build_feature_env.insert(name);
                }
            }
            if inner.contains("rustc-cfg=")
                && let Some(i) = inner.find("feature")
            {
                let after = inner[i + 7..].trim_start().trim_start_matches('=').trim_start().trim_start_matches(['\\', '"']);
                let name: String = after.chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')).collect();
                if !name.is_empty() {
                    *self.out.feature_refs.entry(name).or_default() += 1;
                }
            }
        }
        if !inner.contains('/') && self.config_literal.is_match(inner) {
            self.out.config_literals.push(inner.to_string());
        }
    }

    /// `#[derive(…)]` names and `#[serde(…)]` args of the attribute items right above `item`,
    /// and the line the first of them starts on.
    fn attrs_above(&self, item: Node) -> (Vec<String>, Vec<String>, usize) {
        let (mut derives, mut serde, mut first) = (Vec::new(), Vec::new(), line(item));
        let mut p = item.prev_named_sibling();
        while let Some(a) = p {
            if a.kind() != "attribute_item" {
                break;
            }
            first = line(a);
            if let Some(attr) = a.named_child(0).filter(|n| n.kind() == "attribute") {
                let name = attr.named_child(0).map(|n| text(n, self.src)).unwrap_or("");
                let name = name.rsplit("::").next().unwrap_or(name);
                if let Some(tt) = attr.child_by_field_name("arguments") {
                    let mut c = tt.walk();
                    let idents: Vec<String> = tt.children(&mut c).filter(|n| n.kind() == "identifier").map(|n| text(n, self.src).to_string()).collect();
                    match name {
                        "derive" => derives.extend(idents),
                        "serde" => serde.push(text(tt, self.src).trim_matches(|c| c == '(' || c == ')').to_string()),
                        _ => {}
                    }
                }
            }
            p = a.prev_named_sibling();
        }
        (derives, serde, first)
    }

    fn struct_decl(&mut self, n: Node) {
        let (derives, serde, start_line) = self.attrs_above(n);
        if !derives.iter().any(|d| d == "Deserialize") {
            return;
        }
        let Some(name) = n.child_by_field_name("name") else { return };
        let Some(body) = n.child_by_field_name("body").filter(|b| b.kind() == "field_declaration_list") else { return };
        let mut fields = Vec::new();
        let mut pending: Vec<String> = Vec::new();
        let mut c = body.walk();
        for ch in body.named_children(&mut c) {
            match ch.kind() {
                "attribute_item" => {
                    if let Some(attr) = ch.named_child(0).filter(|a| a.kind() == "attribute")
                        && attr.named_child(0).is_some_and(|i| text(i, self.src) == "serde")
                        && let Some(tt) = attr.child_by_field_name("arguments")
                    {
                        pending.push(text(tt, self.src).trim_matches(|c| c == '(' || c == ')').to_string());
                    }
                }
                "field_declaration" => {
                    let Some(fname) = ch.child_by_field_name("name") else { continue };
                    let fname = text(fname, self.src).to_string();
                    let args = pending.join(",");
                    let skip = args.split(',').any(|a| matches!(a.trim(), "skip" | "skip_deserializing"));
                    let key = args.split(',').find_map(|a| a.trim().strip_prefix("rename").map(|r| r.trim().trim_start_matches('=').trim().trim_matches('"').to_string())).filter(|k| !k.is_empty()).unwrap_or_else(|| fname.clone());
                    let ty = ch.child_by_field_name("type").and_then(|t| self.type_link(t));
                    fields.push(FieldDecl { name: fname, key, line: line(ch), ty, skip });
                    pending.clear();
                }
                _ => {}
            }
        }
        let deny = serde.iter().any(|s| s.split(',').any(|a| a.trim() == "deny_unknown_fields"));
        self.out.structs.push(StructDecl { name: text(name, self.src).to_string(), start_line, end_line: n.end_position().row + 1, fields, deny_unknown_fields: deny, in_test: self.in_test(n) });
    }

    /// The candidate-struct name a field type links to, if any.
    fn type_link(&self, t: Node) -> Option<String> {
        match t.kind() {
            "type_identifier" => Some(text(t, self.src).to_string()),
            "generic_type" => {
                let outer = t.child_by_field_name("type").map(|n| text(n, self.src)).unwrap_or("");
                let outer = outer.rsplit("::").next().unwrap_or(outer);
                if !matches!(outer, "Option" | "Box" | "Vec" | "Arc" | "Rc" | "BTreeMap" | "HashMap" | "IndexMap") {
                    return None;
                }
                let args = t.child_by_field_name("type_arguments")?;
                let mut c = args.walk();
                let last = args.named_children(&mut c).filter(|n| n.kind() == "type_identifier").last()?;
                Some(text(last, self.src).to_string())
            }
            _ => None,
        }
    }

    fn walk(&mut self, root: Node) {
        let mut stack: Vec<(Node, Ctx)> = vec![(root, Ctx::default())];
        while let Some((n, ctx)) = stack.pop() {
            let kind = n.kind();
            let mut child_ctx = ctx;
            child_ctx.call_fn = false;
            match kind {
                "line_comment" | "block_comment" => continue,
                "use_declaration" => {
                    let test = self.in_test(n);
                    if let Some(a) = n.child_by_field_name("argument") {
                        self.use_roots(a, test);
                    }
                    continue;
                }
                "extern_crate_declaration" => {
                    let test = self.in_test(n);
                    if let Some(name) = n.child_by_field_name("name") {
                        let t = text(name, self.src);
                        self.crate_ref(t, test);
                    }
                    continue;
                }
                "scoped_identifier" | "scoped_type_identifier" => {
                    let test = self.in_test(n);
                    match n.child_by_field_name("path") {
                        Some(p) if p.kind() == "identifier" => {
                            let t = text(p, self.src);
                            self.crate_ref(t, test);
                        }
                        Some(_) => {}
                        None => {
                            if let Some(name) = n.child_by_field_name("name") {
                                let t = text(name, self.src);
                                self.crate_ref(t, test);
                            }
                        }
                    }
                }
                "identifier" if ctx.in_tt => {
                    let t = text(n, self.src);
                    let next = n.next_sibling();
                    let prev = n.prev_sibling();
                    // `a::b` starts a path; `::a::b` too (a leading `::` follows no path piece).
                    let leading = prev.is_none_or(|p| p.kind() != "::" || p.prev_sibling().is_none_or(|pp| !matches!(pp.kind(), "identifier" | ">" | "self" | "crate" | "super" | "metavariable")));
                    if next.is_some_and(|s| s.kind() == "::") && leading {
                        let test = self.in_test(n);
                        self.crate_ref(t, test);
                    }
                    // `x.name` inside a macro body (`format!("{}", cfg.plan.x)`) is a field read;
                    // `x.name(…)` is a call.
                    if !ctx.excluded_impl && prev.is_some_and(|p| p.kind() == ".") && next.is_none_or(|s| s.kind() != "token_tree") {
                        let test = self.in_test(n);
                        self.field_read(t, test);
                    }
                    if t == "feature"
                        && next.is_some_and(|s| s.kind() == "=")
                        && let Some(lit) = next.and_then(|s| s.next_sibling()).filter(|s| s.kind() == "string_literal")
                    {
                        let name = text(lit, self.src).trim_matches('"').to_string();
                        if !name.is_empty() {
                            *self.out.feature_refs.entry(name).or_default() += 1;
                        }
                    }
                    continue;
                }
                "token_tree" => child_ctx.in_tt = true,
                "inner_attribute_item" => {
                    if self.out.lint_silenced.is_none()
                        && let Some(attr) = n.named_child(0).filter(|a| a.kind() == "attribute")
                        && attr.named_child(0).is_some_and(|i| text(i, self.src) == "allow")
                        && let Some(tt) = attr.child_by_field_name("arguments")
                    {
                        let mut c = tt.walk();
                        if tt.children(&mut c).any(|i| i.kind() == "identifier" && matches!(text(i, self.src), "warnings" | "unused")) {
                            self.out.lint_silenced = Some((line(n), text(n, self.src).to_string()));
                        }
                    }
                }
                "attribute_item" => {
                    // `#[cfg(test)] mod x;`: the file `x` resolves to is test context.
                    if let Some(attr) = n.named_child(0).filter(|a| a.kind() == "attribute")
                        && attr.named_child(0).is_some_and(|i| text(i, self.src) == "cfg")
                        && attr.child_by_field_name("arguments").is_some_and(|tt| text(tt, self.src).trim().trim_start_matches('(').trim_end_matches(')').trim() == "test")
                        && let Some(item) = n.next_named_sibling().filter(|i| i.kind() == "mod_item" && i.child_by_field_name("body").is_none())
                        && let Some(name) = item.child_by_field_name("name")
                    {
                        self.out.cfg_test_mods.push(text(name, self.src).to_string());
                    }
                }
                "struct_item" => self.struct_decl(n),
                "function_item" if ctx.in_impl => {
                    if n.child_by_field_name("name").is_some_and(|f| self.serialize_fn.is_match(text(f, self.src))) {
                        child_ctx.excluded_impl = true;
                    }
                }
                "impl_item" => {
                    child_ctx.in_impl = true;
                    if let Some(tr) = n.child_by_field_name("trait") {
                        let t = text(tr, self.src);
                        let t = t.split('<').next().unwrap_or(t);
                        let t = t.rsplit("::").next().unwrap_or(t);
                        if matches!(t, "Default" | "Serialize" | "Deserialize") {
                            child_ctx.excluded_impl = true;
                        }
                    }
                }
                "field_expression" if !ctx.excluded_impl && !ctx.call_fn => {
                    if let Some(f) = n.child_by_field_name("field").filter(|f| f.kind() == "field_identifier") {
                        let test = self.in_test(n);
                        let t = text(f, self.src);
                        self.field_read(t, test);
                    }
                }
                "field_pattern" if !ctx.excluded_impl => {
                    if let Some(f) = n.child_by_field_name("name") {
                        let test = self.in_test(n);
                        let t = text(f, self.src);
                        self.field_read(t, test);
                    }
                }
                "index_expression" if !ctx.excluded_impl => {
                    if let Some(lit) = n.named_child(1).filter(|l| l.kind() == "string_literal") {
                        let test = self.in_test(n);
                        let t = text(lit, self.src).trim_matches('"').to_string();
                        self.field_read(&t, test);
                    }
                }
                "let_declaration" => {
                    if let Some(ty) = n.child_by_field_name("type").filter(|t| t.kind() == "type_identifier")
                        && let Some(v) = n.child_by_field_name("value")
                    {
                        let vt = text(v, self.src);
                        if let Some(fmt) = deserialise_format(vt) {
                            self.out.consumed_types.entry(text(ty, self.src).to_string()).or_insert(fmt);
                        }
                    }
                }
                "generic_function" => {
                    if let Some(f) = n.child_by_field_name("function")
                        && let Some(fmt) = deserialise_format(text(f, self.src))
                        && let Some(args) = n.child_by_field_name("type_arguments")
                    {
                        let mut c = args.walk();
                        for a in args.named_children(&mut c).filter(|a| a.kind() == "type_identifier") {
                            self.out.consumed_types.entry(text(a, self.src).to_string()).or_insert(fmt.clone());
                        }
                    }
                }
                "string_literal" | "raw_string_literal" => {
                    self.string_literal(n);
                    continue;
                }
                _ => {}
            }
            let fn_child = if kind == "call_expression" { n.child_by_field_name("function").map(|c| c.id()) } else { None };
            let mut c = n.walk();
            let children: Vec<Node> = n.children(&mut c).collect();
            for ch in children.into_iter().rev() {
                let mut cc = child_ctx;
                cc.call_fn = fn_child == Some(ch.id());
                stack.push((ch, cc));
            }
        }
    }
}

/// The format crate of a deserialise call in `expr` text (`toml::from_str(…)` -> `toml`).
fn deserialise_format(expr: &str) -> Option<String> {
    for call in ["from_str", "from_slice", "from_reader", "from_value", "try_into", "try_from"] {
        if let Some(i) = expr.find(call)
            && (expr.len() == i + call.len() || expr[i + call.len()..].starts_with(['(', ':']))
        {
            let before = expr[..i].trim_end_matches("::");
            let start = before.rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).map_or(0, |p| p + 1);
            let krate = &before[start..];
            return Some(if krate.is_empty() { "a config file".to_string() } else { krate.to_string() });
        }
    }
    None
}

impl Walker {
    pub fn new(cfg: &Cfg) -> Self {
        Walker {
            rust: cfg.languages.iter().any(|l| l == "rust"),
            config_file: regex(&cfg.config_file_regex, "config_file"),
            config_struct: regex(&cfg.config_struct_regex, "config_struct"),
            config_literal: Regex::new(r"^[\w.-]+\.(toml|ya?ml|json|json5|ini|cfg|conf)$").unwrap(),
            serialize_fn: regex(&cfg.serialize_fn_regex, "serialize_fn"),
        }
    }

    /// One file's side on an already-parsed tree (`None` when the parse failed).
    pub fn file_side(&self, root: Option<Node>, file: &SourceFile, regions: &[TestRegion]) -> FileSide {
        let mut out = FileSide { path: file.path.clone(), rust: file.lang == Language::Rust && self.rust, test_file: file.kind == FileKind::Test, ..FileSide::default() };
        let Some(root) = root else { return out };
        if !out.rust {
            return out;
        }
        let build_script = file.path == "build.rs" || file.path.ends_with("/build.rs");
        let mut st = State { src: file.content.as_bytes(), out: &mut out, regions, build_script, config_literal: &self.config_literal, serialize_fn: &self.serialize_fn };
        st.walk(root);
        out
    }

    /// Parse and collect one file on its own.
    pub fn parse_side(&self, file: &SourceFile) -> FileSide {
        if file.lang != Language::Rust || !self.rust {
            return FileSide { path: file.path.clone(), test_file: file.kind == FileKind::Test, ..FileSide::default() };
        }
        let src = file.content.as_bytes();
        let tree = file.lang.parser().parse(src, None);
        let root = tree.as_ref().map(|t| t.root_node());
        let regions = match root {
            Some(root) if file.kind != FileKind::Test => regions::test_regions(root, src),
            _ => Vec::new(),
        };
        self.file_side(root, file, &regions)
    }

    fn config_file(&self, path: &str) -> bool {
        self.config_file.is_match(path)
    }

    fn config_struct(&self, name: &str) -> bool {
        self.config_struct.is_match(name)
    }
}

/// Which discovered files the pass reads.
pub fn indexed(f: &SourceFile) -> bool {
    matches!(f.kind, FileKind::Source | FileKind::Test)
}

/// Collect every Source and Test file, one parse each: `scry declared`.
pub fn index_all(files: &[SourceFile], cfg: &Cfg) -> Vec<FileSide> {
    let w = Walker::new(cfg);
    files.par_iter().filter(|f| indexed(f)).map(|f| w.parse_side(f)).collect()
}

/// Test files on trees `scan` parsed once (whole-file test context: no regions).
pub fn index_tests(tests: &[(&SourceFile, Option<tree_sitter::Tree>)], cfg: &Cfg) -> Vec<FileSide> {
    let w = Walker::new(cfg);
    tests.par_iter().map(|(f, t)| w.file_side(t.as_ref().map(|t| t.root_node()), f, &[])).collect()
}

// ---------- manifests ----------

/// One declared dependency.
#[derive(Debug, Clone, Serialize)]
pub struct Dep {
    /// The table key (`pulldown-cmark`)…
    pub name: String,
    /// …and the in-code name (`pulldown_cmark`); never the `package` value.
    pub ident: String,
    pub line: usize,
    /// `dependencies`, `dev-dependencies`, `build-dependencies`, `target.'cfg(unix)'.dependencies`.
    pub section: String,
    pub optional: bool,
    pub dev: bool,
    /// `x = { workspace = true }`, resolved from the workspace table.
    pub workspace: bool,
    /// Registry name when renamed (`package = "memmap2"`).
    pub package: Option<String>,
}

#[derive(Debug, Clone)]
struct FeatureDecl {
    name: String,
    line: usize,
    elements: Vec<String>,
    /// The `#` comment block right above the line: (first line, last line, text).
    comment: Option<(usize, usize, String)>,
}

#[derive(Debug, Clone)]
struct Manifest {
    /// Repo-relative path (`crates/index/Cargo.toml`).
    path: String,
    /// Its directory (`""` at the root).
    dir: String,
    deps: Vec<Dep>,
    features: Vec<FeatureDecl>,
    /// Feature names `required-features` / docs.rs metadata / cargo-all-features consume.
    manifest_consumers: HashSet<String>,
    /// Features other manifests enable on this crate through a path dep: (their manifest, feature).
    enabled_by: Vec<(String, String)>,
    has_bin: bool,
    /// Crate root files, repo-relative (`src/lib.rs`, `src/main.rs`, `[[bin]] path`).
    root_files: Vec<String>,
    /// `[workspace.dependencies]` of this manifest.
    workspace_deps: HashMap<String, toml::Value>,
    /// (dep dir repo-relative, features) for every path dep with a `features` list.
    path_dep_features: Vec<(String, Vec<String>)>,
}

fn join_dir(dir: &str, file: &str) -> String {
    if dir.is_empty() { file.to_string() } else { format!("{dir}/{file}") }
}

fn norm_path(p: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

/// Header text normalised for comparison with a key path: quotes and spaces dropped.
fn norm_header(h: &str) -> String {
    h.chars().filter(|c| !matches!(c, '"' | '\'' | ' ' | '\t')).collect()
}

/// Lines of `text` where a key is declared, by (normalised table header, key): a `key = …`
/// line under the table, or a `[table.key]` / `[[table.key]]` header.
fn key_lines(text: &str) -> HashMap<(String, String), usize> {
    let mut out = HashMap::new();
    let mut header = String::new();
    for (i, raw) in text.lines().enumerate() {
        let l = raw.trim();
        if l.starts_with('[') {
            let h = l.trim_start_matches('[').split(']').next().unwrap_or("").trim();
            let h = norm_header(h);
            if let Some((parent, key)) = h.rsplit_once('.') {
                out.entry((parent.to_string(), key.to_string())).or_insert(i + 1);
            }
            header = h;
            continue;
        }
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let raw: String = l.chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '"' | '\'')).collect();
        let key = raw.trim_matches(|c| c == '"' || c == '\'').to_string();
        if key.is_empty() {
            continue;
        }
        let rest = l[raw.len()..].trim_start();
        if rest.starts_with('=') || rest.starts_with('.') {
            out.entry((header.clone(), key)).or_insert(i + 1);
        }
    }
    out
}

/// The `#` comment block right above line `at` (1-based).
fn comment_above(text: &str, at: usize) -> Option<(usize, usize, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut first = at.checked_sub(1)?; // 0-based index of the line above
    let mut body = Vec::new();
    while first > 0 && lines.get(first - 1).is_some_and(|l| l.trim_start().starts_with('#')) {
        first -= 1;
        body.push(lines[first].trim_start().trim_start_matches('#').trim().to_string());
    }
    if body.is_empty() {
        return None;
    }
    body.reverse();
    Some((first + 1, at - 1, body.join(" ")))
}

fn strings_of(v: Option<&toml::Value>) -> Vec<String> {
    v.and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect()).unwrap_or_default()
}

fn all_strings(v: &toml::Value, out: &mut Vec<String>) {
    match v {
        toml::Value::String(s) => out.push(s.clone()),
        toml::Value::Array(a) => a.iter().for_each(|v| all_strings(v, out)),
        toml::Value::Table(t) => t.values().for_each(|v| all_strings(v, out)),
        _ => {}
    }
}

fn parse_cargo(rel: &str, text: &str) -> Option<Manifest> {
    let t: toml::Table = toml::from_str(text).ok()?;
    let dir = rel.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
    let lines = key_lines(text);
    let mut deps = Vec::new();
    let add = |section: &str, table: &toml::Table, dev: bool, deps: &mut Vec<Dep>| {
        for (k, v) in table {
            let opt = v.get("optional").and_then(|o| o.as_bool()).unwrap_or(false);
            let ws = v.get("workspace").and_then(|o| o.as_bool()).unwrap_or(false);
            let package = v.get("package").and_then(|p| p.as_str()).map(String::from);
            let line = lines.get(&(norm_header(section), k.clone())).copied().unwrap_or(0);
            deps.push(Dep { name: k.clone(), ident: k.replace('-', "_"), line, section: section.to_string(), optional: opt, dev, workspace: ws, package });
        }
    };
    for (sec, dev) in [("dependencies", false), ("build-dependencies", false), ("dev-dependencies", true)] {
        if let Some(tb) = t.get(sec).and_then(|v| v.as_table()) {
            add(sec, tb, dev, &mut deps);
        }
    }
    if let Some(targets) = t.get("target").and_then(|v| v.as_table()) {
        for (cfg, tv) in targets {
            for (sec, dev) in [("dependencies", false), ("build-dependencies", false), ("dev-dependencies", true)] {
                if let Some(tb) = tv.get(sec).and_then(|v| v.as_table()) {
                    add(&format!("target.'{cfg}'.{sec}"), tb, dev, &mut deps);
                }
            }
        }
    }
    let mut path_dep_features = Vec::new();
    for sec in ["dependencies", "build-dependencies", "dev-dependencies"] {
        if let Some(tb) = t.get(sec).and_then(|v| v.as_table()) {
            for v in tb.values() {
                if let Some(p) = v.get("path").and_then(|p| p.as_str()) {
                    let feats = strings_of(v.get("features"));
                    if !feats.is_empty() {
                        path_dep_features.push((norm_path(&join_dir(&dir, p)), feats));
                    }
                }
            }
        }
    }
    let mut features = Vec::new();
    if let Some(ft) = t.get("features").and_then(|v| v.as_table()) {
        for (name, v) in ft {
            let line = lines.get(&("features".to_string(), name.clone())).copied().unwrap_or(0);
            features.push(FeatureDecl { name: name.clone(), line, elements: strings_of(Some(v)), comment: if line > 0 { comment_above(text, line) } else { None } });
        }
    }
    let mut consumers = HashSet::new();
    let mut bins = Vec::new();
    for target in ["bin", "example", "test", "bench"] {
        for tt in t.get(target).and_then(|v| v.as_array()).into_iter().flatten() {
            consumers.extend(strings_of(tt.get("required-features")));
            if target == "bin" {
                bins.push(tt.get("path").and_then(|p| p.as_str()).map(|p| join_dir(&dir, p.trim_start_matches("./"))));
            }
        }
    }
    if let Some(meta) = t.get("package").and_then(|p| p.get("metadata")) {
        for key in ["docs.rs", "cargo-all-features"] {
            if let Some(m) = meta.get(key) {
                let mut s = Vec::new();
                all_strings(m, &mut s);
                consumers.extend(s);
            }
        }
    }
    let workspace_deps = t.get("workspace").and_then(|w| w.get("dependencies")).and_then(|d| d.as_table()).cloned().unwrap_or_default().into_iter().collect();
    let mut root_files: Vec<String> = bins.iter().flatten().cloned().collect();
    root_files.push(join_dir(&dir, t.get("lib").and_then(|l| l.get("path")).and_then(|p| p.as_str()).unwrap_or("src/lib.rs").trim_start_matches("./")));
    root_files.push(join_dir(&dir, "src/main.rs"));
    Some(Manifest {
        path: rel.to_string(), dir, deps, features, manifest_consumers: consumers, enabled_by: Vec::new(),
        has_bin: !bins.is_empty(), root_files, workspace_deps, path_dep_features,
    })
}

/// Files under `root` matching `globs`, repo-relative with `/` separators; `.git` and the
/// discover pass's vendor directories skipped, gitignore respected, hidden files included
/// (`.github/workflows`).
fn find_files(root: &Path, globs: &GlobSet, skip_dirs: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let skip: HashSet<String> = skip_dirs.iter().cloned().chain([".git".to_string()]).collect();
    let walk = ignore::WalkBuilder::new(root).hidden(false).git_ignore(true).filter_entry(move |e| !(e.file_type().is_some_and(|t| t.is_dir()) && e.file_name().to_str().is_some_and(|n| skip.contains(n)))).build();
    for entry in walk.flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else { continue };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if globs.is_match(&rel) {
            out.push(rel);
        }
    }
    out.sort();
    out
}

// ---------- output ----------

/// A declared dependency no file in its manifest's scope references.
#[derive(Debug, Clone, Serialize)]
pub struct Orphan {
    pub manifest: String,
    pub name: String,
    pub ident: String,
    pub line: usize,
    pub section: String,
    /// `optional = true`: printed through the feature gating it (`dead_features`), never as
    /// an orphan line of its own.
    pub optional: bool,
    /// Features whose elements name it (`dep:x` or the implicit `x`).
    pub gated_by: Vec<String>,
    /// The commit that declared the manifest line: (short hash, date).
    pub birth: Option<(String, String)>,
    /// Days from the birth commit to the newest commit, and commits since it.
    pub age_days: Option<u64>,
    pub commits_since: Option<usize>,
    /// `(last commit that still used `ident::`, commit that removed it)` when the scope's
    /// history ever imported it.
    pub ever_imported: Option<(String, String, String)>,
    /// First `doc_globs` file naming the dep: (file, line).
    pub doc_mention: Option<(String, usize)>,
    /// `#![allow(warnings)]` on a crate root: (file, line).
    pub lint_silenced: Option<(String, usize)>,
    pub line_text: String,
}

/// A `[dependencies]` entry only test code references (`check_placement`).
#[derive(Debug, Clone, Serialize)]
pub struct Misplaced {
    pub manifest: String,
    pub name: String,
    pub line: usize,
    pub test_refs: u32,
    pub files: Vec<String>,
    pub line_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureState {
    Alive,
    Noop,
    Dead,
}

/// One `[features]` entry with its verdict.
#[derive(Debug, Clone, Serialize)]
pub struct Feature {
    pub manifest: String,
    pub name: String,
    pub line: usize,
    pub elements: Vec<String>,
    /// `cfg(feature = "x")` sites, build-script reads and manifest consumers.
    pub consumers: usize,
    /// `feature_ci_globs` files naming it.
    pub ci_files: Vec<String>,
    pub state: FeatureState,
    /// Optional deps this feature gates directly: (dep, manifest line).
    pub gates: Vec<(String, usize)>,
    /// Dead siblings it enables.
    pub dead_siblings: Vec<String>,
    /// The commit that deleted the last `cfg(feature = "x")`: (short hash, date).
    pub removed_in: Option<(String, String)>,
    pub enabled_by: Vec<String>,
    pub comment: Option<(usize, usize, String)>,
    pub line_text: String,
}

/// A knob (or a whole section) a config struct accepts that no code reads.
#[derive(Debug, Clone, Serialize)]
pub struct UnreadKnob {
    /// `[ci.homerunner]`, `[report].foo`, `Config.foo`.
    pub section: String,
    pub struct_name: String,
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,
    /// Every knob the line covers (the parent field and its descendants for a roll-up).
    pub knobs: Vec<String>,
    /// The unread fields the line reports.
    pub unread: Vec<String>,
    pub source: String,
    pub doc: Option<(String, usize, usize)>,
    /// Read by tests only (never a finding, information).
    pub test_only: bool,
    /// A whole section: the unread parent field and every knob under it.
    pub rollup: bool,
    pub line_text: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FileDeclared {
    /// `(section.knob, line)` of every unread knob declared in this file.
    pub unread_knobs: Vec<(String, usize)>,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    pub manifests: usize,
    pub other_manifests: usize,
    /// Every declared dependency, dev-dependencies included.
    pub deps: usize,
    pub checked_deps: usize,
    pub orphans: usize,
    pub misplaced: usize,
    /// `[features]` entries other than `default`.
    pub features: usize,
    pub dead_features: usize,
    pub noop_features: usize,
    pub config_structs: usize,
    pub knobs: usize,
    pub unread_knobs: usize,
    pub test_only_knobs: usize,
    pub git_lookups_capped: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DeclaredReport {
    pub totals: Totals,
    pub notes: Vec<String>,
    pub orphans: Vec<Orphan>,
    pub misplaced: Vec<Misplaced>,
    pub features: Vec<Feature>,
    pub dead_features: Vec<Feature>,
    pub noop_features: Vec<Feature>,
    pub unread_knobs: Vec<UnreadKnob>,
    pub files: BTreeMap<String, FileDeclared>,
}

// ---------- git ----------

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(root).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_available(root: &Path) -> bool {
    git(root, &["rev-parse", "--show-prefix"]).is_some()
}

/// `(short hash, date, commit time)` of the oldest commit whose diff changed the count of
/// `needle` under `paths`.
fn birth_of(root: &Path, needle: &str, paths: &[String]) -> Option<(String, String, i64)> {
    let mut args = vec!["log", "--format=%h%x09%ad%x09%ct", "--date=short", "-S", needle, "--"];
    args.extend(paths.iter().map(String::as_str));
    let out = git(root, &args)?;
    let last = out.lines().last()?;
    let mut it = last.split('\t');
    Some((it.next()?.to_string(), it.next()?.to_string(), it.next()?.parse().ok()?))
}

/// Newest commit whose diff changed the count of `needle` under `paths`: (short hash, date).
fn newest_of(root: &Path, needle: &str, paths: &[String]) -> Option<(String, String)> {
    let mut args = vec!["log", "--format=%h%x09%ad", "--date=short", "-S", needle, "--"];
    args.extend(paths.iter().map(String::as_str));
    let out = git(root, &args)?;
    let first = out.lines().next()?;
    let (h, d) = first.split_once('\t')?;
    Some((h.to_string(), d.to_string()))
}

fn commits_since(root: &Path, hash: &str) -> Option<usize> {
    git(root, &["rev-list", "--count", &format!("{hash}..HEAD")])?.parse().ok()
}

// ---------- analysis ----------

/// `(repo-relative path, text)` of the doc / CI files read for a pass.
type Docs = Vec<(String, String)>;

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn join_and(items: &[String]) -> String {
    match items.len() {
        0 => String::new(),
        1 => items[0].clone(),
        n => format!("{} and {}", items[..n - 1].join(", "), items[n - 1]),
    }
}

fn join_or(items: &[String]) -> String {
    match items.len() {
        0 => String::new(),
        1 => items[0].clone(),
        n => format!("{} or {}", items[..n - 1].join(", "), items[n - 1]),
    }
}

/// `name` as a whole word in `text` (`-` and `_` are word characters).
fn word_at(text: &str, name: &str) -> Option<usize> {
    let b = text.as_bytes();
    let mut from = 0;
    while let Some(i) = text[from..].find(name) {
        let at = from + i;
        let end = at + name.len();
        let wc = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'-';
        if (at == 0 || !wc(b[at - 1])) && (end >= b.len() || !wc(b[end])) {
            return Some(at);
        }
        from = end;
    }
    None
}

/// First (file, line) among `docs` whose text names `key` or `ident` as a word.
fn doc_mention(docs: &[(String, String)], key: &str, ident: &str) -> Option<(String, usize)> {
    for (path, text) in docs {
        for (i, l) in text.lines().enumerate() {
            if word_at(l, key).is_some() || (ident != key && word_at(l, ident).is_some()) {
                return Some((path.clone(), i + 1));
            }
        }
    }
    None
}

/// Fenced code blocks of a doc naming `[section]` or one of `knobs` as a key: the file and
/// the line range from the header (or first knob) to the last knob line in that fence.
fn doc_fence(docs: &[(String, String)], section: &str, knobs: &[String]) -> Option<(String, usize, usize)> {
    let header = section.trim_start_matches('[').trim_end_matches(']');
    let mut fallback = None;
    for (path, text) in docs {
        let mut in_fence = false;
        let (mut first, mut last, mut has_header) = (0usize, 0usize, false);
        for (i, l) in text.lines().enumerate() {
            let t = l.trim();
            if t.starts_with("```") || t.starts_with("~~~") {
                if in_fence && first > 0 {
                    if has_header {
                        return Some((path.clone(), first, last));
                    }
                    fallback.get_or_insert((path.clone(), first, last));
                }
                in_fence = !in_fence;
                first = 0;
                last = 0;
                has_header = false;
                continue;
            }
            if !in_fence {
                continue;
            }
            let is_header = !header.is_empty() && (t == format!("[{header}]") || t == format!("[[{header}]]"));
            let key = t.split(['=', ':']).next().unwrap_or("").trim().trim_matches('"');
            let is_knob = !key.is_empty() && t.len() > key.len() && knobs.iter().any(|k| k == key);
            if is_header {
                first = i + 1;
                last = i + 1;
                has_header = true;
            } else if is_knob {
                if first == 0 {
                    first = i + 1;
                }
                last = i + 1;
            }
        }
    }
    fallback
}

/// `src/ or tests/`: the distinct first components of the in-scope files under the manifest
/// dir (root-level files by name).
fn scope_dirs(manifest_dir: &str, paths: &[&str]) -> Vec<String> {
    let mut set = BTreeSet::new();
    for p in paths {
        let rel = if manifest_dir.is_empty() { *p } else { p.strip_prefix(manifest_dir).map_or(*p, |r| r.trim_start_matches('/')) };
        match rel.split_once('/') {
            Some((d, _)) => set.insert(format!("{d}/")),
            None => set.insert(rel.to_string()),
        };
    }
    set.into_iter().collect()
}

fn age_text(days: u64) -> String {
    plural(days as usize, "day", "days")
}

/// Resolve `#[cfg(test)] mod x;` of `decl` to the file it names, among `paths`.
fn resolve_mod(decl: &str, name: &str, paths: &HashSet<&str>) -> Option<String> {
    let dir = decl.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let stem = decl.rsplit('/').next().unwrap_or(decl).trim_end_matches(".rs");
    let mut candidates = vec![join_dir(dir, &format!("{name}.rs")), join_dir(dir, &format!("{name}/mod.rs"))];
    if !matches!(stem, "mod" | "lib" | "main") {
        candidates.push(join_dir(dir, &format!("{stem}/{name}.rs")));
        candidates.push(join_dir(dir, &format!("{stem}/{name}/mod.rs")));
    }
    candidates.into_iter().find(|c| paths.contains(c.as_str()))
}

/// The pass. `git` runs the history enrichment (birth commits, last consumers) when the root
/// is a git checkout; `skip_dirs` are the discover pass's vendor directories.
pub fn analyze(sides: &[FileSide], root: &Path, use_git: bool, cfg: &Cfg, skip_dirs: &[String]) -> DeclaredReport {
    let mut report = DeclaredReport::default();
    let walker = Walker::new(cfg);
    let use_git = use_git && git_available(root);
    let side_effect = globset(&cfg.side_effect_deps);

    // Manifests and the auxiliary files (docs, CI, public docs), one walk.
    let manifest_globs = globset(&cfg.manifest_globs);
    let doc_globs = globset(&cfg.doc_globs);
    let ci_globs = globset(&cfg.feature_ci_globs);
    let public_doc_globs = globset(&cfg.public_doc_globs);
    let mut union: Vec<String> = cfg.manifest_globs.clone();
    union.extend(cfg.doc_globs.iter().cloned());
    union.extend(cfg.feature_ci_globs.iter().cloned());
    union.extend(cfg.public_doc_globs.iter().cloned());
    let aux = find_files(root, &globset(&union), skip_dirs);
    let read = |rel: &str| -> Option<String> {
        let p = root.join(rel);
        let meta = std::fs::metadata(&p).ok()?;
        if meta.len() > 4 << 20 {
            return None;
        }
        std::fs::read_to_string(p).ok()
    };
    let mut manifests: Vec<Manifest> = Vec::new();
    for rel in aux.iter().filter(|r| manifest_globs.is_match(r)) {
        if !rel.ends_with("Cargo.toml") {
            report.totals.other_manifests += 1;
            continue;
        }
        match read(rel).and_then(|t| parse_cargo(rel, &t)) {
            Some(m) => manifests.push(m),
            None => report.notes.push(format!("{rel} did not parse as TOML; skipped")),
        }
    }
    // Docs, CI files and public docs are read on first use: a repo with no manifest never
    // reads them.
    let lazy = |globs: &GlobSet| -> Vec<String> { aux.iter().filter(|r| !manifest_globs.is_match(r) && globs.is_match(r)).cloned().collect() };
    let (doc_paths, ci_paths, public_paths) = (lazy(&doc_globs), lazy(&ci_globs), lazy(&public_doc_globs));
    let load = |paths: &[String]| -> Docs { paths.iter().filter_map(|p| read(p).map(|t| (p.clone(), t))).collect() };
    let (docs_cell, ci_cell, public_cell): (std::cell::OnceCell<Docs>, std::cell::OnceCell<Docs>, std::cell::OnceCell<Docs>) = Default::default();
    let docs = || docs_cell.get_or_init(|| load(&doc_paths));
    let ci = || ci_cell.get_or_init(|| load(&ci_paths));
    let public_docs = || public_cell.get_or_init(|| load(&public_paths));
    report.totals.manifests = manifests.len();
    if report.totals.other_manifests > 0 {
        report.notes.push(format!("{} package.json / pyproject.toml {} found; only Cargo.toml is parsed in this version", report.totals.other_manifests, if report.totals.other_manifests == 1 { "manifest" } else { "manifests" }));
    }
    if manifests.is_empty() {
        return report;
    }
    manifests.sort_by(|a, b| a.path.cmp(&b.path));

    // Workspace deps: a member's `x = { workspace = true }` inherits `optional` / `package`
    // from the nearest ancestor manifest that declares `x` under [workspace.dependencies].
    let ws: Vec<(String, HashMap<String, toml::Value>)> = manifests.iter().filter(|m| !m.workspace_deps.is_empty()).map(|m| (m.dir.clone(), m.workspace_deps.clone())).collect();
    for m in &mut manifests {
        for d in m.deps.iter_mut().filter(|d| d.workspace) {
            let owner = ws.iter().filter(|(dir, t)| (dir.is_empty() || m.dir == *dir || m.dir.starts_with(&format!("{dir}/"))) && t.contains_key(&d.name)).max_by_key(|(dir, _)| dir.len());
            if let Some((_, t)) = owner {
                let v = &t[&d.name];
                if d.package.is_none() {
                    d.package = v.get("package").and_then(|p| p.as_str()).map(String::from);
                }
            }
        }
    }
    // Features other manifests enable on a path dep, by that dep's dir.
    let by_dir: HashMap<String, usize> = manifests.iter().enumerate().map(|(i, m)| (m.dir.clone(), i)).collect();
    let mut enabling: Vec<(usize, String, String)> = Vec::new();
    for m in &manifests {
        for (dir, feats) in &m.path_dep_features {
            if let Some(&i) = by_dir.get(dir) {
                enabling.extend(feats.iter().map(|f| (i, m.path.clone(), f.clone())));
            }
        }
    }
    for (i, from, f) in enabling {
        manifests[i].enabled_by.push((from, f));
    }

    // Scope: every Rust side's nearest manifest.
    let all_paths: HashSet<&str> = sides.iter().map(|s| s.path.as_str()).collect();
    let mut propagated_test: HashSet<String> = HashSet::new();
    for s in sides.iter().filter(|s| s.rust) {
        for name in &s.cfg_test_mods {
            if let Some(p) = resolve_mod(&s.path, name, &all_paths) {
                propagated_test.insert(p);
            }
        }
    }
    let nearest = |path: &str| -> Option<usize> {
        manifests.iter().enumerate().filter(|(_, m)| m.dir.is_empty() || path.starts_with(&format!("{}/", m.dir))).max_by_key(|(_, m)| m.dir.len()).map(|(i, _)| i)
    };
    let mut scope: Vec<Vec<usize>> = vec![Vec::new(); manifests.len()];
    for (si, s) in sides.iter().enumerate() {
        if s.rust
            && let Some(mi) = nearest(&s.path)
        {
            scope[mi].push(si);
        }
    }
    let is_test_side = |s: &FileSide| s.test_file || propagated_test.contains(&s.path);

    // ---- P64: orphans ----
    let mut all_orphans: Vec<Orphan> = Vec::new();
    let mut orphan_idents: Vec<HashSet<String>> = vec![HashSet::new(); manifests.len()];
    let mut side_effect_skipped: Vec<String> = Vec::new();
    let mut dev_skipped = 0usize;
    let mut lint_silenced_crates: Vec<String> = Vec::new();
    let scope_paths: Vec<Vec<&str>> = scope.iter().map(|ids| ids.iter().map(|&i| sides[i].path.as_str()).collect()).collect();
    let scope_text: Vec<String> = manifests.iter().enumerate().map(|(mi, m)| {
        let dirs: Vec<String> = scope_dirs(&m.dir, &scope_paths[mi]).into_iter().filter(|d| d.ends_with('/')).map(|d| join_dir(&m.dir, &d)).collect();
        if dirs.is_empty() { format!("{}/", if m.dir.is_empty() { "." } else { m.dir.as_str() }) } else { join_or(&dirs) }
    }).collect();
    let git_paths: Vec<Vec<String>> = manifests.iter().enumerate().map(|(mi, m)| {
        let mut v: Vec<String> = scope_dirs(&m.dir, &scope_paths[mi]).into_iter().map(|d| join_dir(&m.dir, d.trim_end_matches('/'))).collect();
        if v.is_empty() {
            v.push(if m.dir.is_empty() { ".".to_string() } else { m.dir.clone() });
        }
        v
    }).collect();
    for (mi, m) in manifests.iter().enumerate() {
        let silenced = scope[mi].iter().map(|&i| &sides[i]).find(|s| m.root_files.contains(&s.path) && s.lint_silenced.is_some()).and_then(|s| s.lint_silenced.as_ref().map(|(l, _)| (s.path.clone(), *l)));
        if let Some((f, l)) = &silenced {
            lint_silenced_crates.push(format!("{f}:{l}"));
            if cfg.skip_lint_silenced_crates {
                continue;
            }
        }
        report.totals.deps += m.deps.len();
        for d in &m.deps {
            if d.dev && !cfg.check_dev_dependencies {
                dev_skipped += 1;
                continue;
            }
            if side_effect.is_match(&d.name) || d.package.as_ref().is_some_and(|p| side_effect.is_match(p)) {
                side_effect_skipped.push(d.name.clone());
                continue;
            }
            report.totals.checked_deps += 1;
            let mut refs = RefCount::default();
            let mut test_files: Vec<String> = Vec::new();
            for &si in &scope[mi] {
                let s = &sides[si];
                if let Some(rc) = s.crate_refs.get(&d.ident) {
                    if is_test_side(s) {
                        refs.test += rc.total();
                        test_files.push(s.path.clone());
                    } else {
                        refs.add(*rc);
                        if rc.test > 0 && rc.prod == 0 {
                            test_files.push(format!("a #[cfg(test)] module in {}", s.path));
                        }
                    }
                }
            }
            if refs.total() == 0 {
                orphan_idents[mi].insert(d.ident.clone());
                let gated_by: Vec<String> = m.features.iter().filter(|f| f.elements.iter().any(|e| e == &format!("dep:{}", d.name) || e == &d.name)).map(|f| f.name.clone()).collect();
                all_orphans.push(Orphan {
                    manifest: m.path.clone(), name: d.name.clone(), ident: d.ident.clone(), line: d.line, section: d.section.clone(), optional: d.optional, gated_by,
                    birth: None, age_days: None, commits_since: None, ever_imported: None, doc_mention: doc_mention(docs(), &d.name, &d.ident), lint_silenced: silenced.clone(), line_text: String::new(),
                });
            } else if cfg.check_placement && !d.dev && refs.prod == 0 && refs.test > 0 && d.section == "dependencies" {
                let files = test_files;
                let refs_text = if refs.test == 1 { "reference is".to_string() } else { format!("{} references are", refs.test) };
                let line_text = format!("{} is declared in [dependencies] ({}:{}) but its only {refs_text} in {} - move it to [dev-dependencies]", d.name, m.path, d.line, join_and(&files));
                report.misplaced.push(Misplaced { manifest: m.path.clone(), name: d.name.clone(), line: d.line, test_refs: refs.test, files, line_text });
            }
        }
    }

    // ---- P65: features ----
    let mut all_features: Vec<Feature> = Vec::new();
    for (mi, m) in manifests.iter().enumerate() {
        if cfg.skip_lint_silenced_crates && scope[mi].iter().any(|&i| m.root_files.contains(&sides[i].path) && sides[i].lint_silenced.is_some()) {
            continue;
        }
        let names: HashSet<&str> = m.features.iter().map(|f| f.name.as_str()).collect();
        let optional: HashMap<&str, &Dep> = m.deps.iter().filter(|d| d.optional).map(|d| (d.name.as_str(), d)).collect();
        let mut decls: Vec<FeatureDecl> = m.features.iter().filter(|f| f.name != "default").cloned().collect();
        // Optional deps no feature names get cargo's implicit `x = ["dep:x"]`.
        for d in m.deps.iter().filter(|d| d.optional) {
            let named = m.features.iter().any(|f| f.elements.iter().any(|e| e == &d.name || e == &format!("dep:{}", d.name)));
            if !named && !names.contains(d.name.as_str()) {
                decls.push(FeatureDecl { name: d.name.clone(), line: d.line, elements: vec![format!("dep:{}", d.name)], comment: None });
            }
        }
        let mut consumers: HashMap<&str, usize> = HashMap::new();
        for f in &decls {
            let env = f.name.to_uppercase().replace('-', "_");
            let n: usize = scope[mi].iter().map(|&i| sides[i].feature_refs.get(&f.name).copied().unwrap_or(0) as usize + usize::from(sides[i].build_feature_env.contains(&env))).sum::<usize>()
                + usize::from(m.manifest_consumers.contains(&f.name));
            consumers.insert(f.name.as_str(), n);
        }
        let decl_names: HashSet<&str> = decls.iter().map(|f| f.name.as_str()).collect();
        // Element classes: `x/f` forwards; `dep:x` or the bare name of an optional dep no
        // feature is named after is a dep; a declared name is a sibling; anything else is
        // unknown and keeps the feature alive.
        let is_dep_elem = |e: &str| e.starts_with("dep:") || (!decl_names.contains(e) && optional.contains_key(e));
        let dep_orphaned = |e: &str| orphan_idents[mi].contains(&e.strip_prefix("dep:").unwrap_or(e).replace('-', "_"));
        let is_unknown = |e: &str| !e.contains('/') && !is_dep_elem(e) && !decl_names.contains(e) && !names.contains(e);
        // alive: forwards, consumed, unknown element, or enables an alive sibling (fixpoint).
        let mut alive: HashSet<&str> = decls.iter().filter(|f| consumers[f.name.as_str()] > 0 || f.elements.iter().any(|e| e.contains('/') || is_unknown(e))).map(|f| f.name.as_str()).collect();
        loop {
            let more: Vec<&str> = decls.iter().filter(|f| !alive.contains(f.name.as_str()) && f.elements.iter().any(|e| alive.contains(e.as_str()))).map(|f| f.name.as_str()).collect();
            if more.is_empty() {
                break;
            }
            alive.extend(more);
        }
        let noop: HashSet<&str> = decls.iter().filter(|f| f.elements.is_empty() && consumers[f.name.as_str()] == 0).map(|f| f.name.as_str()).collect();
        // dead: every element is an orphaned dep or a dead sibling (fixpoint from the top).
        let mut dead: HashSet<&str> = decls.iter().filter(|f| !alive.contains(f.name.as_str()) && !noop.contains(f.name.as_str())).map(|f| f.name.as_str()).collect();
        loop {
            let drop: Vec<&str> = decls.iter().filter(|f| dead.contains(f.name.as_str()) && !f.elements.iter().all(|e| if is_dep_elem(e) { dep_orphaned(e) } else { dead.contains(e.as_str()) })).map(|f| f.name.as_str()).collect();
            if drop.is_empty() {
                break;
            }
            for d in drop {
                dead.remove(d);
            }
        }
        for f in &decls {
            let state = if alive.contains(f.name.as_str()) { FeatureState::Alive } else if noop.contains(f.name.as_str()) { FeatureState::Noop } else if dead.contains(f.name.as_str()) { FeatureState::Dead } else { FeatureState::Alive };
            let gates: Vec<(String, usize)> = f.elements.iter().filter(|e| is_dep_elem(e)).filter_map(|e| { let n = e.strip_prefix("dep:").unwrap_or(e); m.deps.iter().find(|x| x.name == n).map(|x| (x.name.clone(), x.line)) }).collect();
            let dead_siblings: Vec<String> = f.elements.iter().filter(|e| dead.contains(e.as_str()) && !e.starts_with("dep:")).cloned().collect();
            let ci_files: Vec<String> = ci().iter().filter(|(_, t)| word_at(t, &f.name).is_some()).map(|(p, _)| p.clone()).collect();
            all_features.push(Feature {
                manifest: m.path.clone(), name: f.name.clone(), line: f.line, elements: f.elements.clone(), consumers: consumers[f.name.as_str()], ci_files, state, gates, dead_siblings,
                removed_in: None, enabled_by: m.enabled_by.iter().filter(|(_, x)| x == &f.name).map(|(p, _)| p.clone()).collect(), comment: f.comment.clone(), line_text: String::new(),
            });
        }
    }
    report.totals.features = all_features.len();

    // ---- git enrichment: orphans (non-optional: the optional ones print through their
    // feature) and dead features, capped ----
    let mut work: Vec<(usize, bool)> = Vec::new(); // (index, is_feature)
    work.extend(all_orphans.iter().enumerate().filter(|(_, o)| !o.optional).map(|(i, _)| (i, false)));
    work.extend(all_features.iter().enumerate().filter(|(_, f)| f.state == FeatureState::Dead).map(|(i, _)| (i, true)));
    let mut too_young: HashSet<(usize, bool)> = HashSet::new();
    if use_git {
        report.totals.git_lookups_capped = work.len() > cfg.max_git_lookups;
        work.truncate(cfg.max_git_lookups);
        let head_ct: i64 = git(root, &["log", "-1", "--format=%ct"]).and_then(|s| s.parse().ok()).unwrap_or(0);
        let mindex: HashMap<&str, usize> = manifests.iter().enumerate().map(|(i, m)| (m.path.as_str(), i)).collect();
        type Enrich = (Option<(String, String, i64)>, Option<usize>, Option<(String, String, String)>, Option<(String, String)>);
        let results: Vec<((usize, bool), Enrich)> = work.par_iter().map(|&(i, is_feature)| {
            let (manifest, needle_manifest, ident, dirs) = if is_feature {
                let f = &all_features[i];
                (f.manifest.clone(), format!("{} = [", f.name), None, git_paths[mindex[f.manifest.as_str()]].clone())
            } else {
                let o = &all_orphans[i];
                (o.manifest.clone(), o.name.clone(), Some(o.ident.clone()), git_paths[mindex[o.manifest.as_str()]].clone())
            };
            let birth = birth_of(root, &needle_manifest, std::slice::from_ref(&manifest));
            let since = birth.as_ref().and_then(|(h, _, _)| commits_since(root, h));
            let ever = ident.and_then(|id| {
                let (removed, date) = newest_of(root, &format!("{id}::"), &dirs)?;
                let parent = format!("{removed}^");
                let mut args: Vec<&str> = vec!["log", "-1", "--format=%h", &parent, "--"];
                args.extend(dirs.iter().map(String::as_str));
                let last_used = git(root, &args).filter(|s| !s.is_empty()).unwrap_or_else(|| removed.clone());
                Some((last_used, removed, date))
            });
            let killed = if is_feature { newest_of(root, &format!("feature = \"{}\"", all_features[i].name), &dirs) } else { None };
            ((i, is_feature), (birth, since, ever, killed))
        }).collect();
        for ((i, is_feature), (birth, since, ever, killed)) in results {
            if since.is_some_and(|n| n < cfg.min_age_commits) {
                too_young.insert((i, is_feature));
            }
            if is_feature {
                all_features[i].removed_in = killed;
            } else {
                let o = &mut all_orphans[i];
                o.age_days = birth.as_ref().map(|(_, _, ct)| ((head_ct - ct).max(0) as f64 / 86400.0).round() as u64);
                o.birth = birth.map(|(h, d, _)| (h, d));
                o.commits_since = since;
                o.ever_imported = ever;
            }
        }
    }

    // ---- lines ----
    let scope_by_manifest: HashMap<&str, &str> = manifests.iter().enumerate().map(|(i, m)| (m.path.as_str(), scope_text[i].as_str())).collect();
    let scope_of = |manifest: &str| -> &str { scope_by_manifest.get(manifest).copied().unwrap_or("src/") };
    for o in all_orphans.iter_mut() {
        let since = match (&o.birth, o.age_days, o.commits_since) {
            (Some((h, _)), Some(days), Some(n)) => format!(" since {h} ({}, {})", age_text(days), plural(n, "commit", "commits")),
            (Some((h, _)), _, _) => format!(" since {h}"),
            _ => String::new(),
        };
        let under = if o.section == "dependencies" { String::new() } else { format!(" under [{}]", o.section) };
        let imported = match &o.ever_imported {
            Some((last, removed, date)) => format!("last used in {last}, removed in {removed} ({date})"),
            None => format!("never imported by any {} file", scope_of(&o.manifest)),
        };
        let doc = o.doc_mention.as_ref().map(|(f, l)| format!("; mentioned in {f}:{l}")).unwrap_or_default();
        let lint = o.lint_silenced.as_ref().map(|(f, l)| format!("; the crate silences warnings (#![allow] at {f}:{l})")).unwrap_or_default();
        o.line_text = format!("{} declared in {}:{}{under}{since} and {imported}{doc}{lint} - wire it or drop it", o.name, o.manifest, o.line);
    }
    for f in all_features.iter_mut() {
        let manifest = f.manifest.clone();
        let gates = if f.gates.is_empty() {
            if f.dead_siblings.is_empty() { "gates no dependency".to_string() } else { format!("enables only the dead {} {}", if f.dead_siblings.len() == 1 { "feature" } else { "features" }, join_and(&f.dead_siblings)) }
        } else {
            format!("gates optional {} {}", if f.gates.len() == 1 { "dep" } else { "deps" }, join_and(&f.gates.iter().map(|(d, l)| format!("{d} ({manifest}:{l})")).collect::<Vec<_>>()))
        };
        let consumed = match &f.removed_in {
            Some((h, d)) => format!("its last consumer was removed in {h} ({d})"),
            None => "never consumed".to_string(),
        };
        let ci_note = if f.ci_files.is_empty() { String::new() } else { format!("; consumed only by CI: {}", join_and(&f.ci_files)) };
        let comment = f.comment.as_ref().map(|(a, b, t)| {
            let last = t.rsplit(". ").next().unwrap_or(t).trim_end_matches('.').trim();
            let last: String = last.chars().take(120).collect();
            format!("; the comment at {manifest}:{} says '{last}'", if a == b { a.to_string() } else { format!("{a}-{b}") })
        }).unwrap_or_default();
        let enabled = if f.enabled_by.is_empty() { String::new() } else { format!("; enabled by {}", join_and(&f.enabled_by)) };
        f.line_text = match f.state {
            FeatureState::Dead => format!("feature {} ({manifest}:{}) {gates} and nothing in {} checks cfg(feature = \"{}\"); {consumed}{ci_note}{comment}{enabled}", f.name, f.line, scope_of(&manifest), f.name),
            FeatureState::Noop => format!("feature {} ({manifest}:{}) = [] has no consumer: a no-op{ci_note}{comment}", f.name, f.line),
            FeatureState::Alive => format!("feature {} ({manifest}:{}) alive: {} {}{}", f.name, f.line, plural(f.consumers, "consumer", "consumers"), if f.elements.iter().any(|e| e.contains('/')) { "(forwards)" } else { "" }, enabled),
        };
    }
    // Dead features print when they gate an orphaned dep (bare ones behind the knob) and
    // are old enough; noop ones behind theirs.
    let dead_features: Vec<Feature> = all_features.iter().enumerate().filter(|(i, f)| f.state == FeatureState::Dead && !too_young.contains(&(*i, true)) && (!f.gates.is_empty() || cfg.report_bare_dead_features)).map(|(_, f)| f.clone()).collect();
    let bare_suppressed = all_features.iter().filter(|f| f.state == FeatureState::Dead && f.gates.is_empty()).count();
    report.totals.dead_features = all_features.iter().filter(|f| f.state == FeatureState::Dead).count();
    report.totals.noop_features = all_features.iter().filter(|f| f.state == FeatureState::Noop).count();
    let noop: Vec<Feature> = if cfg.report_noop_features { all_features.iter().filter(|f| f.state == FeatureState::Noop).cloned().collect() } else { Vec::new() };
    // Orphans: the optional ones gated by a printed dead feature are that line's business.
    let printed_features: HashSet<(String, String)> = dead_features.iter().flat_map(|f| f.gates.iter().map(move |(d, _)| (f.manifest.clone(), d.clone()))).collect();
    let mut orphans: Vec<Orphan> = all_orphans.iter().enumerate().filter(|(i, _)| !too_young.contains(&(*i, false))).map(|(_, o)| o.clone()).collect();
    report.totals.orphans = orphans.len();
    let young = all_orphans.len() - orphans.len();
    orphans.sort_by(|a, b| b.commits_since.unwrap_or(0).cmp(&a.commits_since.unwrap_or(0)).then_with(|| a.manifest.cmp(&b.manifest)).then_with(|| a.line.cmp(&b.line)));
    let mut notes = Vec::new();
    if dev_skipped > 0 {
        notes.push(format!("{} not checked (check_dev_dependencies)", plural(dev_skipped, "dev-dependency", "dev-dependencies")));
    }
    if !side_effect_skipped.is_empty() {
        side_effect_skipped.sort();
        side_effect_skipped.dedup();
        notes.push(format!("{} on the side-effect list not checked: {}", plural(side_effect_skipped.len(), "dep", "deps"), side_effect_skipped.join(", ")));
    }
    if !lint_silenced_crates.is_empty() {
        notes.push(format!("{} silence{} warnings with #![allow]: {}{}", plural(lint_silenced_crates.len(), "crate root", "crate roots"), if lint_silenced_crates.len() == 1 { "s" } else { "" }, lint_silenced_crates.join(", "), if cfg.skip_lint_silenced_crates { " (skipped)" } else { "" }));
    }
    if young > 0 {
        notes.push(format!("{} younger than min_age_commits ({}) not reported", plural(young, "orphan", "orphans"), cfg.min_age_commits));
    }
    if report.totals.noop_features > 0 && !cfg.report_noop_features {
        notes.push(format!("{} (`= []`, no consumer) suppressed; report_noop_features lists them", plural(report.totals.noop_features, "no-op feature", "no-op features")));
    }
    if bare_suppressed > 0 && !cfg.report_bare_dead_features {
        notes.push(format!("{} gating no dependency suppressed; report_bare_dead_features lists them", plural(bare_suppressed, "dead feature", "dead features")));
    }
    if report.totals.git_lookups_capped {
        notes.push(format!("git enrichment capped at {} lookups (max_git_lookups); later findings carry no history", cfg.max_git_lookups));
    }
    let optional_printed: HashSet<(String, String)> = orphans.iter().filter(|o| o.optional && printed_features.contains(&(o.manifest.clone(), o.name.clone()))).map(|o| (o.manifest.clone(), o.name.clone())).collect();
    let optional_unprinted = orphans.iter().filter(|o| o.optional && !optional_printed.contains(&(o.manifest.clone(), o.name.clone()))).count();
    if optional_unprinted > 0 {
        notes.push(format!("{} optional and gated by a live or unreported feature: not an orphan line", plural(optional_unprinted, "orphaned dep is", "orphaned deps are")));
    }

    report.notes.extend(notes);
    // ---- P66: config knobs ----
    knobs(sides, &manifests, &scope, &walker, cfg, public_docs(), &is_test_side, &mut report);

    report.orphans = orphans;
    report.features = all_features;
    report.dead_features = dead_features;
    report.noop_features = noop;
    report
}

/// The struct the deserialise call targets, else the file / name rule.
#[allow(clippy::too_many_arguments)]
fn knobs(sides: &[FileSide], manifests: &[Manifest], scope: &[Vec<usize>], walker: &Walker, cfg: &Cfg, public_docs: &[(String, String)], is_test_side: &dyn Fn(&FileSide) -> bool, report: &mut DeclaredReport) {
    // Reads repo-wide, by field name; test context apart.
    let mut reads: HashMap<&str, RefCount> = HashMap::new();
    let mut consumed: HashMap<&str, &str> = HashMap::new();
    for s in sides.iter().filter(|s| s.rust) {
        let test = is_test_side(s);
        for (name, rc) in &s.field_reads {
            let e = reads.entry(name.as_str()).or_default();
            if test { e.test += rc.total() } else { e.add(*rc) }
        }
        for (t, fmt) in &s.consumed_types {
            consumed.entry(t.as_str()).or_insert(fmt.as_str());
        }
    }
    let config_target = |name: &str| consumed.get(name).is_some_and(|fmt| cfg.config_formats.iter().any(|f| f == fmt));
    {
    }
    let mut skipped_lib_crates = 0usize;
    let mut suppressed = 0usize;
    // Candidates: (side index, struct index), per crate scope.
    struct Cand {
        si: usize,
        st: usize,
        parent: Option<(usize, usize)>, // (candidate index, field index)
    }
    let mut cands: Vec<Cand> = Vec::new();
    let mut cand_by_name: HashMap<&str, usize> = HashMap::new();
    for (mi, m) in manifests.iter().enumerate() {
        if cfg.require_bin_target && !cfg.config_is_public_api && !m.has_bin && !scope[mi].iter().any(|&i| sides[i].path == join_dir(&m.dir, "src/main.rs") || sides[i].path.starts_with(&join_dir(&m.dir, "src/bin/"))) {
            let n = scope[mi].iter().flat_map(|&i| sides[i].structs.iter()).filter(|st| !st.in_test).count();
            if n > 0 {
                skipped_lib_crates += 1;
            }
            continue;
        }
        for &si in &scope[mi] {
            let s = &sides[si];
            if is_test_side(s) {
                continue;
            }
            for (st, decl) in s.structs.iter().enumerate() {
                if decl.in_test {
                    continue;
                }
                if config_target(&decl.name) || walker.config_file(&s.path) || walker.config_struct(&decl.name) {
                    cand_by_name.entry(decl.name.as_str()).or_insert(cands.len());
                    cands.push(Cand { si, st, parent: None });
                }
            }
        }
    }
    // Field types reached from a candidate are candidates too (a `Deserialize` struct a config
    // struct holds is config), then the parent links.
    let mut added = true;
    while added {
        added = false;
        for ci in 0..cands.len() {
            let decl = &sides[cands[ci].si].structs[cands[ci].st];
            for f in &decl.fields {
                let Some(t) = &f.ty else { continue };
                if cand_by_name.contains_key(t.as_str()) {
                    continue;
                }
                let found = sides.iter().enumerate().filter(|(_, s)| s.rust && !is_test_side(s)).find_map(|(si, s)| s.structs.iter().position(|d| &d.name == t && !d.in_test).map(|st| (si, st)));
                if let Some((si, st)) = found {
                    cand_by_name.insert(sides[si].structs[st].name.as_str(), cands.len());
                    cands.push(Cand { si, st, parent: None });
                    added = true;
                }
            }
        }
    }
    for ci in 0..cands.len() {
        let decl = &sides[cands[ci].si].structs[cands[ci].st];
        for (fi, f) in decl.fields.iter().enumerate() {
            if let Some(t) = &f.ty
                && let Some(&child) = cand_by_name.get(t.as_str())
                && child != ci
                && cands[child].parent.is_none()
            {
                cands[child].parent = Some((ci, fi));
            }
        }
    }
    let decl_of = |ci: usize| -> &StructDecl { &sides[cands[ci].si].structs[cands[ci].st] };
    // Section path from the root: field keys down to this struct.
    let path_of = |ci: usize| -> (usize, Vec<String>) {
        let (mut cur, mut keys, mut seen) = (ci, Vec::new(), HashSet::new());
        while let Some((p, fi)) = cands[cur].parent {
            if !seen.insert(cur) {
                break;
            }
            keys.push(decl_of(p).fields[fi].key.clone());
            cur = p;
        }
        keys.reverse();
        (cur, keys)
    };
    // Short names another candidate also declares are skipped.
    let mut name_count: HashMap<&str, usize> = HashMap::new();
    for ci in 0..cands.len() {
        for f in &decl_of(ci).fields {
            *name_count.entry(f.name.as_str()).or_default() += 1;
        }
    }
    let collides = |f: &FieldDecl| f.name.len() < cfg.min_field_name_len && name_count.get(f.name.as_str()).copied().unwrap_or(0) > 1;
    let descendants = |ci: usize| -> Vec<String> {
        let (mut out, mut stack, mut seen) = (Vec::new(), vec![ci], HashSet::new());
        while let Some(c) = stack.pop() {
            if !seen.insert(c) {
                continue;
            }
            for f in decl_of(c).fields.iter().filter(|f| !f.skip) {
                out.push(f.key.clone());
                if let Some(&child) = f.ty.as_ref().and_then(|t| cand_by_name.get(t.as_str())) {
                    stack.push(child);
                }
            }
        }
        out
    };
    let source_of = |root: usize| -> String {
        let decl = decl_of(root);
        let s = &sides[cands[root].si];
        if let Some(l) = s.config_literals.first() {
            return l.clone();
        }
        if let Some(fmt) = consumed.get(decl.name.as_str()) {
            return match *fmt {
                "toml" => "TOML".to_string(),
                "serde_json" | "json" => "JSON".to_string(),
                "serde_yaml" | "serde_yml" | "serde_yaml_ng" | "yaml" => "YAML".to_string(),
                other => other.to_string(),
            };
        }
        "a config file".to_string()
    };
    report.totals.config_structs = cands.len();
    report.totals.knobs = cands.iter().enumerate().map(|(ci, _)| decl_of(ci).fields.iter().filter(|f| !f.skip).count()).sum();
    // Parents before children, so a roll-up covers its descendants.
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by_key(|&ci| (path_of(ci).1.len(), sides[cands[ci].si].path.clone(), decl_of(ci).start_line));
    let mut covered: HashSet<usize> = HashSet::new();
    let mut findings: Vec<UnreadKnob> = Vec::new();
    for ci in order {
        if covered.contains(&ci) {
            continue;
        }
        let decl = decl_of(ci);
        let file = &sides[cands[ci].si].path;
        let (root, keys) = path_of(ci);
        let source = source_of(root);
        let sibling_read = decl.fields.iter().any(|f| !f.skip && reads.get(f.name.as_str()).is_some_and(|r| r.prod > 0));
        let mut unread_here: Vec<&FieldDecl> = Vec::new();
        for f in decl.fields.iter().filter(|f| !f.skip && !collides(f)) {
            let r = reads.get(f.name.as_str()).copied().unwrap_or_default();
            if r.prod > 0 {
                continue;
            }
            let child = f.ty.as_ref().and_then(|t| cand_by_name.get(t.as_str())).copied().filter(|&c| c != ci);
            if r.test > 0 {
                let section = if keys.is_empty() { format!("{}.{}", decl.name, f.key) } else { format!("[{}].{}", keys.join("."), f.key) };
                findings.push(UnreadKnob { section, struct_name: decl.name.clone(), file: file.clone(), start_line: f.line, end_line: f.line, knobs: vec![f.key.clone()], unread: vec![f.key.clone()], source: source.clone(), doc: None, test_only: true, rollup: false, line_text: format!("{}.{} ({file}:{}) is read only by tests", decl.name, f.name, f.line) });
                if let Some(c) = child {
                    covered.insert(c);
                }
                continue;
            }
            match child {
                Some(c) => {
                    // Roll-up: the section and every knob under it.
                    covered.insert(c);
                    let cdecl = decl_of(c);
                    let mut all_keys = vec![f.key.clone()];
                    all_keys.extend(descendants(c));
                    let mut path = keys.clone();
                    path.push(f.key.clone());
                    let section = format!("[{}]", path.join("."));
                    let doc = doc_fence(public_docs, &section, &all_keys);
                    if cfg.require_sibling_read_or_doc && !sibling_read && doc.is_none() {
                        suppressed += 1;
                        continue;
                    }
                    let cfile = &sides[cands[c].si].path;
                    let (rfile, start, end) = if cfile == file { (file.clone(), f.line.min(cdecl.start_line), f.line.max(cdecl.end_line)) } else { (cfile.clone(), cdecl.start_line, cdecl.end_line) };
                    let listed: Vec<String> = all_keys.iter().take(cfg.max_knobs_listed).cloned().collect();
                    let more = if all_keys.len() > listed.len() { format!(", +{} more", all_keys.len() - listed.len()) } else { String::new() };
                    let deny = if cdecl.deny_unknown_fields || decl.deny_unknown_fields { " under deny_unknown_fields" } else { "" };
                    let doc_text = doc.as_ref().map(|(p, a, b)| format!(" and documented at {p}:{}", if a == b { a.to_string() } else { format!("{a}-{b}") })).unwrap_or_default();
                    let line_text = format!("{section} ({rfile}:{start}-{end}, {}: {}{more}) is deserialised from {source}{deny}{doc_text}, but no code reads any of them - the program accepts the section and ignores it", plural(all_keys.len(), "knob", "knobs"), listed.join(", "));
                    findings.push(UnreadKnob { section, struct_name: cdecl.name.clone(), file: rfile, start_line: start, end_line: end, knobs: all_keys, unread: vec![f.key.clone()], source: source.clone(), doc, test_only: false, rollup: true, line_text });
                }
                None => unread_here.push(f),
            }
        }
        if !unread_here.is_empty() {
            let section = if keys.is_empty() { decl.name.clone() } else { format!("[{}]", keys.join(".")) };
            let names: Vec<String> = unread_here.iter().map(|f| f.key.clone()).collect();
            let doc = doc_fence(public_docs, if keys.is_empty() { "" } else { &section }, &names);
            if cfg.require_sibling_read_or_doc && !sibling_read && doc.is_none() {
                suppressed += 1;
                continue;
            }
            let n = decl.fields.iter().filter(|f| !f.skip).count();
            let all = unread_here.len() == n;
            let deny = if decl.deny_unknown_fields { " under deny_unknown_fields" } else { "" };
            let doc_text = doc.as_ref().map(|(p, a, b)| format!(" and documented at {p}:{}", if a == b { a.to_string() } else { format!("{a}-{b}") })).unwrap_or_default();
            let listed: Vec<String> = unread_here.iter().take(cfg.max_knobs_listed).map(|f| format!("{} (line {})", f.key, f.line)).collect();
            let more = if unread_here.len() > listed.len() { format!(", +{} more", unread_here.len() - listed.len()) } else { String::new() };
            let what = if all { "any of them".to_string() } else { format!("{}{more}", listed.join(", ")) };
            let head = if all { format!("{section} ({file}:{}-{}, {}: {}{more})", decl.start_line, decl.end_line, plural(n, "knob", "knobs"), names.iter().take(cfg.max_knobs_listed).cloned().collect::<Vec<_>>().join(", ")) } else { format!("{section} ({file}:{}-{}, {})", decl.start_line, decl.end_line, plural(n, "knob", "knobs")) };
            let line_text = format!("{head} is deserialised from {source}{deny}{doc_text}, but no code reads {what} - the program accepts {} and ignores it", if all { "the section" } else { "it" });
            findings.push(UnreadKnob { section, struct_name: decl.name.clone(), file: file.clone(), start_line: decl.start_line, end_line: decl.end_line, knobs: decl.fields.iter().filter(|f| !f.skip).map(|f| f.key.clone()).collect(), unread: names, source, doc, test_only: false, rollup: false, line_text });
        }
    }
    report.totals.unread_knobs = findings.iter().filter(|k| !k.test_only).map(|k| if k.rollup { k.knobs.len() } else { k.unread.len() }).sum();
    report.totals.test_only_knobs = findings.iter().filter(|k| k.test_only).count();
    if skipped_lib_crates > 0 {
        report.notes.push(format!("{} with Deserialize structs but no bin target skipped (require_bin_target)", plural(skipped_lib_crates, "crate", "crates")));
    }
    if suppressed > 0 {
        report.notes.push(format!("{} with no read sibling and no doc mention not reported (require_sibling_read_or_doc)", plural(suppressed, "unread knob line", "unread knob lines")));
    }
    for k in &findings {
        let e = report.files.entry(k.file.clone()).or_default();
        e.reasons.push(k.line_text.clone());
        if k.test_only {
            e.unread_knobs.push((k.section.clone(), k.start_line));
        } else {
            for u in &k.unread {
                e.unread_knobs.push((format!("{}.{u}", k.section.trim_start_matches('[').trim_end_matches(']')), k.start_line));
            }
        }
    }
    report.unread_knobs = findings;
}

// ---------- render ----------

pub fn totals_line(r: &DeclaredReport) -> String {
    let t = &r.totals;
    let pct = if t.deps == 0 { 0.0 } else { 100.0 * t.orphans as f64 / t.deps as f64 };
    let names: Vec<&str> = r.orphans.iter().map(|o| o.name.as_str()).collect();
    let orphan_names = if names.is_empty() { String::new() } else { format!(": {}", names.join(", ")) };
    format!(
        "{} of {} declared deps orphaned ({pct:.1}%{orphan_names}) in {}; {} of {} features dead, {} no-op; {} of {} config knobs in {} unread{}",
        t.orphans, t.deps, plural(t.manifests, "manifest", "manifests"), t.dead_features, t.features, t.noop_features, t.unread_knobs, t.knobs, plural(t.config_structs, "struct", "structs"),
        if t.test_only_knobs > 0 { format!(", {} read only by tests", t.test_only_knobs) } else { String::new() }
    )
}

/// The DECLARED section body as `scan` prints it (after the heading).
pub fn render_section(r: &DeclaredReport, top: usize) -> String {
    use std::fmt::Write;
    let mut o = String::new();
    if r.totals.manifests == 0 && r.totals.config_structs == 0 {
        let _ = writeln!(o, "  none");
    } else {
        let _ = writeln!(o, "  {}", totals_line(r));
    }
    for n in &r.notes {
        let _ = writeln!(o, "  {n}");
    }
    let printed_orphans: Vec<&Orphan> = r.orphans.iter().filter(|x| !x.optional).collect();
    if !printed_orphans.is_empty() {
        let _ = writeln!(o, "  ORPHANED DEPS  (declared, referenced by no file in the manifest's scope)");
        for x in printed_orphans.iter().take(top) {
            let _ = writeln!(o, "  {}", x.line_text);
        }
    }
    if !r.misplaced.is_empty() {
        let _ = writeln!(o, "  MISPLACED  (only test code references them)");
        for x in r.misplaced.iter().take(top) {
            let _ = writeln!(o, "  {}", x.line_text);
        }
    }
    if !r.dead_features.is_empty() || !r.noop_features.is_empty() {
        let _ = writeln!(o, "  DEAD FEATURES  (no cfg consumer; gating only orphaned deps)");
        for x in r.dead_features.iter().take(top) {
            let _ = writeln!(o, "  {}", x.line_text);
        }
        for x in r.noop_features.iter().take(top) {
            let _ = writeln!(o, "  {}", x.line_text);
        }
    }
    if !r.unread_knobs.is_empty() {
        let _ = writeln!(o, "  UNREAD KNOBS  (accepted by a Deserialize struct, read by no code)");
        for x in r.unread_knobs.iter().take(top) {
            let _ = writeln!(o, "  {}", x.line_text);
        }
    }
    if r.totals.manifests + r.totals.config_structs > 0 && printed_orphans.is_empty() && r.misplaced.is_empty() && r.dead_features.is_empty() && r.noop_features.is_empty() && r.unread_knobs.is_empty() {
        let _ = writeln!(o, "  none");
    }
    o
}

/// `scry declared`: the section, then the files with unread knobs and their reasons.
pub fn render(r: &DeclaredReport, top: usize) -> String {
    use std::fmt::Write;
    let mut o = String::new();
    let _ = writeln!(o, "DECLARED  (dependencies no file imports; feature flags nothing checks; config knobs no code reads)");
    o.push_str(&render_section(r, top));
    if !r.files.is_empty() {
        let _ = writeln!(o, "\nfiles with unread config knobs:");
        for (p, f) in r.files.iter().take(top) {
            let _ = writeln!(o, "  {p} ({}):", plural(f.unread_knobs.len(), "knob", "knobs"));
            for reason in &f.reasons {
                let _ = writeln!(o, "    - {reason}");
            }
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(path: &str, content: &str) -> SourceFile {
        SourceFile { path: path.into(), lang: Language::from_path(Path::new(path)).unwrap(), kind: crate::discover::classify(path, content, content.lines().count(), &crate::config::Discover::default()), lines: content.lines().count(), bytes: content.len(), content: content.into() }
    }

    fn side(path: &str, content: &str) -> FileSide {
        Walker::new(&Cfg::default()).parse_side(&src(path, content))
    }

    /// A temp repo root with the given files, no git.
    fn repo(files: &[(&str, &str)]) -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("scry-declared-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
        for (p, c) in files {
            let path = dir.join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, c).unwrap();
        }
        dir
    }

    fn run(root: &Path, cfg: &Cfg) -> DeclaredReport {
        let files = crate::discover::walk(root, &crate::config::Discover::default()).unwrap();
        let sides = index_all(&files, cfg);
        analyze(&sides, root, false, cfg, &[])
    }

    #[test]
    fn crate_references_come_from_every_path_form() {
        let s = side("src/a.rs", r#"
use foo::bar::{a, b as c, d::*};
use {x::y, z};
use ::log::debug;
extern crate serde;
#[derive(clap::Parser)]
struct P;
fn f() -> regex::Regex { let v = anyhow::anyhow!("{}", other::X); lazy_static! { static ref R: rx::Re = rx::Re::new(); } a::b::c(); v }
macro_rules! d { ($($t:tt)*) => (::log2::debug!($($t)*);) }
#[cfg(test)]
mod tests { fn t() { tempfile::tempdir(); } }
"#);
        for name in ["foo", "x", "z", "log", "log2", "serde", "clap", "regex", "anyhow", "other", "rx", "a"] {
            assert!(s.crate_refs.get(name).is_some_and(|r| r.prod > 0), "{name}: {:?}", s.crate_refs);
        }
        // `b` in `a::b::c` is not a crate; `bar` inside the use path is not either.
        assert!(!s.crate_refs.contains_key("b") && !s.crate_refs.contains_key("bar"), "{:?}", s.crate_refs);
        let t = s.crate_refs["tempfile"];
        assert_eq!((t.prod, t.test), (0, 1));
    }

    #[test]
    fn feature_consumers_cfg_test_mods_and_lint_silence_are_collected() {
        let s = side("src/lib.rs", r#"
#![allow(warnings)]
#[cfg(feature = "alpha")]
fn a() {}
#[cfg(all(unix, not(feature = "beta")))]
fn b() { if cfg!(feature = "gamma") {} }
#[cfg(test)]
mod testutil;
#[cfg(test)]
mod tests { }
"#);
        assert_eq!(s.feature_refs.get("alpha"), Some(&1));
        assert_eq!(s.feature_refs.get("beta"), Some(&1));
        assert_eq!(s.feature_refs.get("gamma"), Some(&1));
        assert_eq!(s.cfg_test_mods, vec!["testutil"]);
        assert_eq!(s.lint_silenced.as_ref().map(|(l, _)| *l), Some(2));
        let b = side("build.rs", r#"fn main() { if std::env::var("CARGO_FEATURE_USE_JEMALLOC").is_ok() { println!("cargo:rustc-cfg=feature=\"fast\""); } }"#);
        assert!(b.build_feature_env.contains("USE_JEMALLOC"));
        assert_eq!(b.feature_refs.get("fast"), Some(&1));
    }

    #[test]
    fn config_structs_fields_and_reads_are_collected_with_the_exclusions() {
        let s = side("src/config.rs", r#"
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config { pub ci: CiCfg, #[serde(rename = "lvl")] pub level: u8, #[serde(skip)] pub hidden: u8, pub names: Vec<Name>, pub port: u16 }
impl Config { pub fn render(&self) -> String { format!("port = {}", self.port) } fn other(&self) -> u8 { self.hidden } }
#[derive(Deserialize)]
pub struct CiCfg { pub provider: String, pub homerunner: Option<HomerunnerCfg> }
impl Default for Config { fn default() -> Self { Self { ci: CiCfg::default(), level: self.level, hidden: 0 } } }
impl Serialize for Config { fn serialize(&self) { self.level; self.ci.provider } }
pub fn load(text: &str) -> Config { let cfg: Config = toml::from_str(text).unwrap(); let Config { level, .. } = &cfg; cfg.ci.provider.len(); cfg.validate(); cfg }
pub fn show(c: &Config) { println!("{} {}", c.level, c.names.len()); }
#[cfg(test)]
mod tests { fn t() { let c = Config::default(); c.ci.homerunner; } }
"#);
        let names: Vec<(&str, &str, bool, Option<&str>)> = s.structs[0].fields.iter().map(|f| (f.name.as_str(), f.key.as_str(), f.skip, f.ty.as_deref())).collect();
        assert_eq!(names, vec![("ci", "ci", false, Some("CiCfg")), ("level", "lvl", false, None), ("hidden", "hidden", true, None), ("names", "names", false, Some("Name")), ("port", "port", false, None)]);
        assert!(s.structs[0].deny_unknown_fields && !s.structs[1].deny_unknown_fields);
        assert_eq!(s.structs[1].fields[1].ty.as_deref(), Some("HomerunnerCfg"));
        assert_eq!(s.consumed_types.get("Config").map(String::as_str), Some("toml"));
        // Reads: `level` twice (the pattern and the `println!` token tree; the Default and Serialize
        // bodies are excluded), `provider` once in production, `homerunner` only in the test module,
        // `validate` and `len` never (method calls, in and out of a macro body).
        assert_eq!((s.field_reads["level"].prod, s.field_reads["level"].test), (2, 0));
        assert_eq!(s.field_reads["provider"].prod, 1);
        assert_eq!(s.field_reads["names"].prod, 1);
        assert!(!s.field_reads.contains_key("len"));
        // `render` is the write side: its read of `port` is not a read; `other` is an ordinary method.
        assert!(!s.field_reads.contains_key("port") && s.field_reads["hidden"].prod == 1);
        assert_eq!((s.field_reads["homerunner"].prod, s.field_reads["homerunner"].test), (0, 1));
        assert!(!s.field_reads.contains_key("validate"));
        assert!(s.field_reads.contains_key("ci"));
    }

    #[test]
    fn manifest_parse_keys_lines_sections_and_features() {
        let text = r#"[package]
name = "k"

[[bin]]
name = "k"
path = "src/main.rs"
required-features = ["cli"]

[features]
default = []
# v0.2. Moves into default when ci.rs lands.
ci-homerunner = ["dep:rusqlite"]
cli = []

[dependencies]
anyhow = "1"
pulldown-cmark = { version = "0.13", default-features = false }
rusqlite = { version = "0.40", optional = true }
memmap = { package = "memmap2", version = "0.9" }

[dependencies.clap]
version = "4"

[target.'cfg(unix)'.dependencies]
libc = "0.2"

[dev-dependencies]
tempfile = "3"
"#;
        let m = parse_cargo("Cargo.toml", text).unwrap();
        let dep = |n: &str| m.deps.iter().find(|d| d.name == n).unwrap();
        assert_eq!((dep("pulldown-cmark").ident.as_str(), dep("pulldown-cmark").line), ("pulldown_cmark", 17));
        assert!(dep("rusqlite").optional);
        assert_eq!(dep("memmap").package.as_deref(), Some("memmap2"));
        assert_eq!(dep("clap").line, 21);
        assert_eq!((dep("libc").section.as_str(), dep("libc").line), ("target.'cfg(unix)'.dependencies", 25));
        assert!(dep("tempfile").dev);
        assert!(m.has_bin);
        let f = m.features.iter().find(|f| f.name == "ci-homerunner").unwrap();
        assert_eq!((f.line, f.elements.clone()), (12, vec!["dep:rusqlite".to_string()]));
        assert_eq!(f.comment.as_ref().map(|(a, b, _)| (*a, *b)), Some((11, 11)));
        assert!(m.manifest_consumers.contains("cli"));
    }

    #[test]
    fn orphans_dead_features_and_the_optional_dedupe() {
        let root = repo(&[
            ("Cargo.toml", "[package]\nname = \"k\"\n\n[features]\ndefault = []\n# Moves into default when ci.rs lands.\nci-homerunner = [\"dep:rusqlite\"]\nnoop = []\nfwd = [\"serde/derive\"]\n\n[dependencies]\nanyhow = \"1\"\nserde = \"1\"\npulldown-cmark = \"0.13\"\nrusqlite = { version = \"0.40\", optional = true }\nopenssl = \"0.10\"\n\n[dev-dependencies]\ntempfile = \"3\"\n"),
            ("src/main.rs", "use anyhow::Result;\nfn main() -> Result<()> { serde::x(); Ok(()) }\n"),
            ("DESIGN.md", "# design\n\nCrates: **pulldown-cmark** for markdown rendering.\n"),
        ]);
        let r = run(&root, &Cfg::default());
        assert_eq!((r.totals.manifests, r.totals.deps, r.totals.checked_deps, r.totals.orphans), (1, 6, 4, 2));
        let names: Vec<&str> = r.orphans.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec!["pulldown-cmark", "rusqlite"]);
        assert_eq!(r.orphans[0].line_text, "pulldown-cmark declared in Cargo.toml:14 and never imported by any src/ file; mentioned in DESIGN.md:3 - wire it or drop it");
        assert!(r.orphans[1].optional && r.orphans[1].gated_by == vec!["ci-homerunner"]);
        assert_eq!((r.totals.features, r.totals.dead_features, r.totals.noop_features), (3, 1, 1));
        assert_eq!(r.dead_features.len(), 1);
        assert_eq!(r.dead_features[0].line_text, "feature ci-homerunner (Cargo.toml:7) gates optional dep rusqlite (Cargo.toml:15) and nothing in src/ checks cfg(feature = \"ci-homerunner\"); never consumed; the comment at Cargo.toml:6 says 'Moves into default when ci.rs lands'");
        assert!(r.noop_features.is_empty() && r.notes.iter().any(|n| n.contains("1 no-op feature")), "{:?}", r.notes);
        assert!(r.notes.iter().any(|n| n == "1 dev-dependency not checked (check_dev_dependencies)"), "{:?}", r.notes);
        assert!(r.notes.iter().any(|n| n == "1 dep on the side-effect list not checked: openssl"), "{:?}", r.notes);
        // The text: rusqlite prints once, through the feature line, never as an orphan line.
        let text = render(&r, 20);
        assert_eq!(text.matches("rusqlite").count(), 2, "{text}"); // the totals line and the feature line
        assert!(text.contains("  ORPHANED DEPS") && text.contains("\n  pulldown-cmark declared") && !text.contains("\n  rusqlite declared"), "{text}");
        // Knobs: noop features listed, bare dead features, dev deps checked.
        let all = Cfg { report_noop_features: true, check_dev_dependencies: true, ..Cfg::default() };
        let r = run(&root, &all);
        assert_eq!(r.noop_features.len(), 1);
        assert!(r.orphans.iter().any(|o| o.name == "tempfile"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn feature_liveness_is_transitive_and_consumers_are_counted() {
        let root = repo(&[
            ("Cargo.toml", "[package]\nname = \"fd\"\n\n[[bin]]\nname = \"fd\"\npath = \"src/main.rs\"\n\n[features]\nuse-jemalloc = [\"tikv-jemallocator\"]\ncompletions = [\"clap_complete\"]\nbase = [\"use-jemalloc\"]\ndefault = [\"use-jemalloc\", \"completions\"]\nbare = [\"dep:redb\"]\nwrap = [\"bare\"]\n\n[dependencies]\nclap_complete = { version = \"4\", optional = true }\nredb = { version = \"4\", optional = true }\n\n[target.'cfg(unix)'.dependencies]\ntikv-jemallocator = { version = \"0.7\", optional = true }\n"),
            ("src/main.rs", "#[cfg(feature = \"use-jemalloc\")]\nuse tikv_jemallocator::Jemalloc;\nfn main() { clap_complete::generate(); }\n"),
            ("Makefile", "all:\n\tcargo build --features completions\n"),
        ]);
        let r = run(&root, &Cfg::default());
        let state = |n: &str| r.features.iter().find(|f| f.name == n).map(|f| (f.state, f.consumers, f.ci_files.clone())).unwrap();
        assert_eq!(state("use-jemalloc"), (FeatureState::Alive, 1, vec![]));
        // No cfg names `completions`, but its dep is imported: a feature gating a used dep is not dead.
        assert_eq!(state("completions"), (FeatureState::Alive, 0, vec!["Makefile".to_string()]));
        assert_eq!(state("base").0, FeatureState::Alive);
        assert_eq!(state("bare").0, FeatureState::Dead);
        assert_eq!(state("wrap").0, FeatureState::Dead);
        // `bare` gates the orphan redb and prints; `wrap` gates no dep and is suppressed by default.
        assert_eq!(r.dead_features.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), vec!["bare"]);
        assert!(r.notes.iter().any(|n| n.starts_with("1 dead feature gating no dependency suppressed")), "{:?}", r.notes);
        let r = run(&root, &Cfg { report_bare_dead_features: true, ..Cfg::default() });
        assert_eq!(r.dead_features.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), vec!["bare", "wrap"]);
        assert!(r.dead_features[1].line_text.starts_with("feature wrap (Cargo.toml:14) enables only the dead feature bare and nothing in src/ checks"), "{}", r.dead_features[1].line_text);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scope_is_the_nearest_manifest_and_lint_silence_is_a_note() {
        let root = repo(&[
            ("Cargo.toml", "[package]\nname = \"rg\"\n[[bin]]\nname = \"rg\"\npath = \"crates/core/main.rs\"\n[dependencies]\ngrep = { path = \"crates/grep\", features = [\"pcre2\"] }\n"),
            ("crates/core/main.rs", "fn main() { grep::run(); }\n"),
            ("crates/grep/Cargo.toml", "[package]\nname = \"grep\"\n[features]\npcre2 = [\"dep:grep-pcre2\"]\n[dependencies]\nfst = \"0.4\"\ngrep-pcre2 = { version = \"1\", optional = true }\n"),
            ("crates/grep/src/lib.rs", "#![allow(warnings)]\npub fn run() {}\n"),
        ]);
        let r = run(&root, &Cfg::default());
        assert_eq!(r.orphans.iter().map(|o| (o.manifest.as_str(), o.name.as_str())).collect::<Vec<_>>(), vec![("crates/grep/Cargo.toml", "fst"), ("crates/grep/Cargo.toml", "grep-pcre2")]);
        assert_eq!(r.orphans[0].line_text, "fst declared in crates/grep/Cargo.toml:6 and never imported by any crates/grep/src/ file; the crate silences warnings (#![allow] at crates/grep/src/lib.rs:1) - wire it or drop it");
        // The root's `features = ["pcre2"]` on the path dep is printed, never a consumer.
        let f = r.features.iter().find(|f| f.name == "pcre2").unwrap();
        assert_eq!((f.state, f.enabled_by.clone()), (FeatureState::Dead, vec!["Cargo.toml".to_string()]));
        assert!(f.line_text.ends_with("; never consumed; enabled by Cargo.toml"), "{}", f.line_text);
        let r = run(&root, &Cfg { skip_lint_silenced_crates: true, ..Cfg::default() });
        assert!(r.orphans.is_empty() && r.notes.iter().any(|n| n.ends_with("(skipped)")), "{:?}", r.notes);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn workspace_deps_resolve_to_the_member_and_placement_is_a_knob() {
        let root = repo(&[
            ("Cargo.toml", "[workspace]\nmembers = [\"a\"]\n[workspace.dependencies]\nmemmap = { package = \"memmap2\", version = \"0.9\" }\ntempfile = \"3\"\n"),
            ("a/Cargo.toml", "[package]\nname = \"a\"\n[dependencies]\nmemmap = { workspace = true }\ntempfile.workspace = true\n"),
            ("a/src/lib.rs", "pub fn f() -> memmap::Mmap { todo!() }\n#[cfg(test)]\nmod tests { fn t() { tempfile::tempdir(); } }\n"),
        ]);
        let r = run(&root, &Cfg::default());
        assert!(r.orphans.is_empty(), "{:?}", r.orphans);
        assert!(r.misplaced.is_empty());
        let r = run(&root, &Cfg { check_placement: true, ..Cfg::default() });
        assert_eq!(r.misplaced.len(), 1);
        assert_eq!(r.misplaced[0].line_text, "tempfile is declared in [dependencies] (a/Cargo.toml:5) but its only reference is in a #[cfg(test)] module in a/src/lib.rs - move it to [dev-dependencies]");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn unread_knobs_roll_up_to_the_section_with_docs_and_the_gates() {
        let config = r#"
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config { pub ci: CiCfg, pub level: u8, pub unused: u8 }
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CiCfg { pub provider: String, pub homerunner: HomerunnerCfg }
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HomerunnerCfg { pub bin: String, pub db: String, pub api: String }
impl Default for HomerunnerCfg { fn default() -> Self { Self { bin: String::new(), db: String::new(), api: String::new() } } }
pub fn load(t: &str) -> Config { let c: Config = toml::from_str(t).unwrap(); c }
"#;
        let main = "fn main() { let c = crate::config::load(\"\"); c.ci.provider; c.level; }\n#[cfg(test)]\nmod tests { fn t() { let c = crate::config::load(\"\"); c.unused; } }\n";
        let root = repo(&[
            ("Cargo.toml", "[package]\nname = \"k\"\n[[bin]]\nname = \"k\"\npath = \"src/main.rs\"\n"),
            ("src/config.rs", config),
            ("src/main.rs", main),
            ("docs/config.md", "# config\n\n```toml\n[ci]\nprovider = \"auto\"\n\n[ci.homerunner]\nbin = \"x\"\ndb = \"y\"\n```\n"),
        ]);
        let r = run(&root, &Cfg::default());
        assert_eq!((r.totals.config_structs, r.totals.knobs, r.totals.unread_knobs, r.totals.test_only_knobs), (3, 8, 4, 1));
        let lines: Vec<&str> = r.unread_knobs.iter().map(|k| k.line_text.as_str()).collect();
        assert_eq!(lines, vec![
            "Config.unused (src/config.rs:4) is read only by tests",
            "[ci.homerunner] (src/config.rs:7-10, 4 knobs: homerunner, bin, db, api) is deserialised from TOML under deny_unknown_fields and documented at docs/config.md:7-9, but no code reads any of them - the program accepts the section and ignores it",
        ]);
        assert_eq!(r.files["src/config.rs"].unread_knobs, vec![("Config.unused".to_string(), 4), ("ci.homerunner.homerunner".to_string(), 7)]);
        // A source literal in the config file names the source; with no read sibling and no doc the line is suppressed.
        let root2 = repo(&[
            ("Cargo.toml", "[package]\nname = \"k\"\n[[bin]]\nname = \"k\"\npath = \"src/main.rs\"\n"),
            ("src/config.rs", "pub const FILE: &str = \"k.toml\";\n#[derive(Deserialize)]\npub struct Config { pub a_knob: u8, pub b_knob: u8 }\n"),
            ("src/main.rs", "fn main() {}\n"),
        ]);
        let r = run(&root2, &Cfg::default());
        assert!(r.unread_knobs.is_empty() && r.notes.iter().any(|n| n.contains("require_sibling_read_or_doc")), "{:?} {:?}", r.unread_knobs, r.notes);
        let r = run(&root2, &Cfg { require_sibling_read_or_doc: false, ..Cfg::default() });
        assert_eq!(r.unread_knobs[0].line_text, "Config (src/config.rs:2-3, 2 knobs: a_knob, b_knob) is deserialised from k.toml, but no code reads any of them - the program accepts the section and ignores it");
        // A lib-only crate is skipped unless the knobs are public API.
        let root3 = repo(&[
            ("Cargo.toml", "[package]\nname = \"k\"\n"),
            ("src/lib.rs", "#[derive(Deserialize)]\npub struct Options { pub knob_a: u8, pub knob_b: u8 }\npub fn f(o: &Options) -> u8 { o.knob_a }\n"),
        ]);
        let r = run(&root3, &Cfg::default());
        assert!(r.unread_knobs.is_empty() && r.notes.iter().any(|n| n.contains("require_bin_target")), "{:?}", r.notes);
        let r = run(&root3, &Cfg { config_is_public_api: true, ..Cfg::default() });
        assert_eq!(r.unread_knobs[0].line_text, "Options (src/lib.rs:1-2, 2 knobs) is deserialised from a config file, but no code reads knob_b (line 2) - the program accepts it and ignores it");
        for d in [root, root2, root3] {
            std::fs::remove_dir_all(&d).unwrap();
        }
    }

    #[test]
    fn short_field_names_skip_only_on_collision() {
        let root = repo(&[
            ("Cargo.toml", "[package]\nname = \"k\"\n[[bin]]\nname = \"k\"\npath = \"src/main.rs\"\n"),
            ("src/config.rs", "#[derive(Deserialize)]\npub struct Config { pub k: u8, pub id: u8, pub used: u8 }\n#[derive(Deserialize)]\npub struct Other { pub id: u8, pub read_it: u8 }\n"),
            ("src/main.rs", "fn main() { let c = load(); c.used; let o = other(); o.read_it; }\n"),
        ]);
        let r = run(&root, &Cfg { config_struct_regex: "(Config|Other)$".into(), ..Cfg::default() });
        // `id` is declared by both candidates: skipped; `k` is short but unique: reported.
        assert_eq!(r.unread_knobs.iter().map(|k| k.unread.clone()).collect::<Vec<_>>(), vec![vec!["k".to_string()]]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn git_enrichment_reads_birth_age_commits_and_the_killing_commit() {
        let root = repo(&[("Cargo.toml", "[package]\nname = \"k\"\n[[bin]]\nname = \"k\"\npath = \"src/main.rs\"\n[features]\nci = [\"dep:rusqlite\"]\n[dependencies]\nold = \"1\"\nrusqlite = { version = \"1\", optional = true }\n"), ("src/main.rs", "#[cfg(feature = \"ci\")]\nfn c() {}\nfn main() { old::x(); }\n")]);
        let git = |args: &[&str]| {
            let out = Command::new("git").arg("-C").arg(&root).args(args).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["add", "."]);
        git(&["commit", "-qm", "root"]);
        let birth = git(&["rev-parse", "--short", "HEAD"]);
        for i in 0..6 {
            std::fs::write(root.join("src/main.rs"), format!("fn main() {{ let _ = {i}; }}\n")).unwrap();
            git(&["add", "."]);
            git(&["commit", "-qm", &format!("drop {i}")]);
        }
        let killed = git(&["log", "--format=%h", "-1"]);
        let first_drop = git(&["log", "--format=%h", "-S", "old::", "--", "src"]);
        let files = crate::discover::walk(&root, &crate::config::Discover::default()).unwrap();
        let sides = index_all(&files, &Cfg::default());
        let r = analyze(&sides, &root, true, &Cfg::default(), &[]);
        let o = r.orphans.iter().find(|o| o.name == "old").unwrap();
        assert_eq!((o.birth.as_ref().map(|(h, _)| h.as_str()), o.commits_since, o.age_days), (Some(birth.as_str()), Some(6), Some(0)));
        let (last, removed, _) = o.ever_imported.clone().unwrap();
        assert_eq!((removed.as_str(), last.as_str()), (first_drop.lines().next().unwrap(), birth.as_str()));
        assert!(o.line_text.starts_with(&format!("old declared in Cargo.toml:9 since {birth} (0 days, 6 commits) and last used in {birth}, removed in {removed} (")), "{}", o.line_text);
        let f = &r.dead_features[0];
        // The first drop commit deleted the only `cfg(feature = "ci")`; the killing commit is that one, not HEAD.
        assert_eq!(f.removed_in.as_ref().map(|(h, _)| h.as_str()), Some(first_drop.lines().next().unwrap()));
        assert_ne!(f.removed_in.as_ref().unwrap().0, killed);
        // Younger than min_age_commits: skipped.
        let r = analyze(&sides, &root, true, &Cfg { min_age_commits: 10, ..Cfg::default() }, &[]);
        assert!(r.orphans.iter().all(|o| o.optional) && r.dead_features.is_empty() && r.notes.iter().any(|n| n.contains("min_age_commits")), "{:?} {:?}", r.orphans, r.notes);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn render_says_none_without_manifests_and_lists_sections() {
        let r = DeclaredReport::default();
        assert_eq!(render_section(&r, 5), "  none\n");
        let root = repo(&[("Cargo.toml", "[package]\nname = \"k\"\n[dependencies]\nanyhow = \"1\"\n"), ("src/main.rs", "fn main() { anyhow::bail!() }\n")]);
        let r = run(&root, &Cfg::default());
        assert_eq!(render_section(&r, 5), "  0 of 1 declared deps orphaned (0.0%) in 1 manifest; 0 of 0 features dead, 0 no-op; 0 of 0 config knobs in 0 structs unread\n  none\n");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
