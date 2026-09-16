//! Which test units name a Source file's symbols.
//!
//! "A test file imports it" is the wrong test for CLI-style suites: kanspec's tests drive the
//! binary and import nothing, yet dozens of test functions name the symbols of
//! `src/cmd/status.rs`. This pass indexes every test unit (a function or item-level macro call
//! in a Test file, one inside an inline `#[cfg(test)]` region) by the identifier tokens in its body,
//! collects the top-level symbols each Source file defines, and counts the units naming at
//! least one of them. No string scan: a symbol has to appear as an identifier token, so a
//! subcommand name inside a string literal is never a mention.

use crate::config::Tests as Cfg;
use crate::discover::{FileKind, SourceFile};
use crate::lang::Language;
use crate::metrics::unit_nodes;
use crate::regions::{self, TestRegion};
use rayon::prelude::*;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use tree_sitter::Node;

/// A top-level symbol of a Source file: a function or method (metrics unit), a type, or a const.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Symbol {
    pub name: String,
    /// `fn`, `method`, `struct`, `enum`, `union`, `trait`, `type`, `const`, `static`, `class`,
    /// `interface`.
    pub kind: &'static str,
    pub start_line: usize,
    pub end_line: usize,
    /// Rust: carries a visibility modifier (`pub(crate)` counts: a bin crate's API is
    /// crate-visible). Python: a module-level def or class whose name and file do not start
    /// with `_`. TS/JS: an exported function, class or arrow/function const. Everything else
    /// (methods, types, interfaces, plain consts) is matchable but not public.
    pub public: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FileMentions {
    /// Test units naming at least one symbol of the file (`test_file_units + inline_units`).
    pub test_units: usize,
    /// …of which units in Test files…
    pub test_file_units: usize,
    /// …and units inside inline `#[cfg(test)]` regions (any Source file's).
    pub inline_units: usize,
    /// Symbols indexed for the file (names of at least `[tests].min_name_len` characters).
    pub symbols: usize,
    pub public_symbols: usize,
    /// Public symbols named by no test unit, in line order.
    pub unmentioned: Vec<Symbol>,
}

#[derive(Debug, Default, Serialize)]
pub struct MentionIndex {
    /// Per Source file, keyed by relative path (ordered, so `--json` is stable across runs).
    pub files: BTreeMap<String, FileMentions>,
    /// Units indexed from Test files (a Test file with no unit is one unit).
    pub test_file_units: usize,
    /// Units indexed from inline test regions of Source files (a region with no unit is one unit).
    pub inline_units: usize,
}

/// A Source file's side of the index, computed on the metrics pass's tree so the file is not
/// parsed twice: its symbols and the identifier sets of its inline test units.
#[derive(Default)]
pub struct SourceSide<'a> {
    path: &'a str,
    symbols: Vec<Symbol>,
    inline_units: Vec<HashSet<&'a str>>,
}

/// The Source side for one parsed file (`root` is `None` when the parse failed: no symbols).
pub fn source_side<'a>(root: Option<Node>, file: &'a SourceFile, test_regions: &[TestRegion], cfg: &Cfg) -> SourceSide<'a> {
    let mut side = SourceSide { path: &file.path, ..Default::default() };
    let Some(root) = root else { return side };
    let src = file.content.as_bytes();
    let units = outermost_units(root, file.lang);
    side.symbols = symbols(root, &units, file, src, test_regions, cfg.min_name_len);
    if cfg.count_inline_tests_as_refs && !test_regions.is_empty() {
        let idents = identifiers(root, file.lang, src, cfg.min_name_len);
        let all = unit_ranges(root, &units, file.lang);
        for r in test_regions {
            // Anchored on the unit's own start: a `#[cfg(test)] #[test] fn` region begins at the
            // fn, below the attributes its range was widened to.
            let inside: Vec<(usize, usize)> = all.iter().filter(|(at, _)| *at >= r.start_byte && *at < r.end_byte).map(|(_, range)| *range).collect();
            // A region with no unit (`#[cfg(test)] use …`) is one unit.
            let ranges = if inside.is_empty() { vec![(r.start_byte, r.end_byte)] } else { inside };
            side.inline_units.extend(ranges.into_iter().map(|r| idents_in(&idents, r)));
        }
    }
    side
}

/// Parse a Source file for its side: `scry mentions` and tests, where no metrics pass shares a tree.
fn parse_source<'a>(file: &'a SourceFile, cfg: &Cfg) -> SourceSide<'a> {
    let src = file.content.as_bytes();
    let tree = file.lang.parser().parse(src, None);
    let root = tree.as_ref().map(|t| t.root_node());
    let test_regions = match (root, file.lang) {
        (Some(root), Language::Rust) => regions::test_regions(root, src),
        _ => Vec::new(),
    };
    source_side(root, file, &test_regions, cfg)
}

/// The test units of a Test file: its outermost metrics units and top-level macro calls, or
/// the whole file when it has neither.
fn test_units<'a>(file: &'a SourceFile, cfg: &Cfg) -> Vec<HashSet<&'a str>> {
    let tree = file.lang.parser().parse(file.content.as_bytes(), None);
    test_units_on(tree.as_ref().map(|t| t.root_node()), file, cfg)
}

/// `test_units` on an already-parsed tree (`None`: a failed parse has no units).
fn test_units_on<'a>(root: Option<Node>, file: &'a SourceFile, cfg: &Cfg) -> Vec<HashSet<&'a str>> {
    let src = file.content.as_bytes();
    let Some(root) = root else { return Vec::new() };
    let idents = identifiers(root, file.lang, src, cfg.min_name_len);
    let units = outermost_units(root, file.lang);
    let mut ranges: Vec<(usize, usize)> = unit_ranges(root, &units, file.lang).into_iter().map(|(_, range)| range).collect();
    if ranges.is_empty() {
        ranges.push((root.start_byte(), root.end_byte()));
    }
    ranges.into_iter().map(|r| idents_in(&idents, r)).collect()
}

/// The test units as `(own start byte, byte range)`: each outermost unit, its range widened to
/// the decorators or outer attributes above it (`@pytest.mark.parametrize("cls", [Widget])`,
/// `#[case(Foo::Bar)]` name symbols the test is about), plus every Rust macro call outside any
/// unit (`rgtest!(name, …)` next to a helper fn is a test of its own), in document order.
fn unit_ranges(root: Node<'_>, units: &[Node<'_>], lang: Language) -> Vec<(usize, (usize, usize))> {
    let mut out: Vec<(usize, (usize, usize))> = units.iter().map(|n| (n.start_byte(), (attributed_start(*n), n.end_byte()))).collect();
    if lang == Language::Rust {
        out.extend(macro_calls(root).into_iter().filter(|m| !units.iter().any(|u| u.start_byte() <= m.start_byte() && m.start_byte() < u.end_byte())).map(|m| (m.start_byte(), (m.start_byte(), m.end_byte()))));
        out.sort_unstable();
    }
    out
}

/// Where a unit starts once its decorators and outer attributes are counted as its own: the
/// Python `decorated_definition` around it, or the first `attribute_item` sibling above it.
fn attributed_start(n: Node) -> usize {
    if let Some(p) = n.parent().filter(|p| p.kind() == "decorated_definition") {
        return p.start_byte();
    }
    let mut start = n.start_byte();
    let mut prev = n.prev_named_sibling();
    while let Some(a) = prev.filter(|a| a.kind() == "attribute_item") {
        start = a.start_byte();
        prev = a.prev_named_sibling();
    }
    start
}

/// Rust `macro_invocation` nodes at item level (under the root, a module or an impl body, with
/// or without the `expression_statement` a trailing `;` wraps them in), never inside a function body.
fn macro_calls(root: Node<'_>) -> Vec<Node<'_>> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "macro_invocation" => out.push(node),
            "source_file" | "mod_item" | "declaration_list" | "impl_item" | "trait_item" | "expression_statement" => {
                let mut cursor = node.walk();
                let children: Vec<Node> = node.children(&mut cursor).collect();
                for child in children.into_iter().rev() {
                    stack.push(child);
                }
            }
            _ => {}
        }
    }
    out
}

/// Index the Source sides (from `metrics::analyze_all_with`) against every Test file in `files`.
pub fn index<'a>(files: &'a [SourceFile], sides: &[SourceSide<'a>], cfg: &Cfg) -> MentionIndex {
    let test_file_units: Vec<Vec<HashSet<&str>>> =
        files.par_iter().filter(|f| f.kind == FileKind::Test).map(|f| test_units(f, cfg)).collect();
    index_units(sides, test_file_units)
}

/// `index` on Test files `scan` already parsed (the dead pass walks the same trees).
pub fn index_parsed<'a>(sides: &[SourceSide<'a>], tests: &[(&'a SourceFile, Option<tree_sitter::Tree>)], cfg: &Cfg) -> MentionIndex {
    let test_file_units: Vec<Vec<HashSet<&str>>> =
        tests.par_iter().map(|(f, t)| test_units_on(t.as_ref().map(|t| t.root_node()), f, cfg)).collect();
    index_units(sides, test_file_units)
}

fn index_units<'a>(sides: &[SourceSide<'a>], test_file_units: Vec<Vec<HashSet<&'a str>>>) -> MentionIndex {

    // Symbol name -> the Source files defining it. A name shared by two files marks both.
    let mut defined: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, side) in sides.iter().enumerate() {
        for s in &side.symbols {
            let owners = defined.entry(s.name.as_str()).or_default();
            if owners.last() != Some(&i) {
                owners.push(i);
            }
        }
    }

    let mut out = MentionIndex::default();
    let mut counts: Vec<(usize, usize)> = vec![(0, 0); sides.len()]; // (test-file units, inline units)
    let mut mentioned: HashSet<&str> = HashSet::new();
    let mut hit: Vec<usize> = Vec::new();
    let from_tests = test_file_units.iter().flatten().map(|u| (false, u));
    let from_inline = sides.iter().flat_map(|s| s.inline_units.iter()).map(|u| (true, u));
    for (inline, idents) in from_tests.chain(from_inline) {
        if inline { out.inline_units += 1 } else { out.test_file_units += 1 }
        hit.clear();
        for id in idents {
            if let Some(owners) = defined.get(id) {
                mentioned.insert(id);
                hit.extend(owners);
            }
        }
        hit.sort_unstable();
        hit.dedup();
        for &f in &hit {
            if inline { counts[f].1 += 1 } else { counts[f].0 += 1 }
        }
    }

    for (i, side) in sides.iter().enumerate() {
        let (test_file_units, inline_units) = counts[i];
        let mut unmentioned: Vec<Symbol> =
            side.symbols.iter().filter(|s| s.public && !mentioned.contains(s.name.as_str())).cloned().collect();
        unmentioned.sort_by_key(|s| (s.start_line, s.name.clone()));
        out.files.insert(side.path.to_string(), FileMentions {
            test_units: test_file_units + inline_units,
            test_file_units,
            inline_units,
            symbols: side.symbols.len(),
            public_symbols: side.symbols.iter().filter(|s| s.public).count(),
            unmentioned,
        });
    }
    out
}

/// Parse the Source files too: for `scry mentions`, where no metrics pass shares its trees.
pub fn index_all(files: &[SourceFile], cfg: &Cfg) -> MentionIndex {
    let sides: Vec<SourceSide> =
        files.par_iter().filter(|f| f.kind == FileKind::Source).map(|f| parse_source(f, cfg)).collect();
    index(files, &sides, cfg)
}

/// Node kinds whose text is an identifier token, per grammar. Type positions count: a Rust test
/// that only builds `Config { .. }` names `Config` as a `type_identifier`.
fn ident_kinds(lang: Language) -> &'static [&'static str] {
    match lang {
        Language::Rust => &["identifier", "field_identifier", "shorthand_field_identifier", "type_identifier"],
        Language::Python => &["identifier"],
        _ => &["identifier", "property_identifier", "shorthand_property_identifier", "shorthand_property_identifier_pattern", "type_identifier"],
    }
}

/// Every identifier token of the tree in document order with its start byte, names shorter than
/// `min_len` left out (they can never match a symbol).
fn identifiers<'a>(root: Node, lang: Language, src: &'a [u8], min_len: usize) -> Vec<(usize, &'a str)> {
    let kinds = ident_kinds(lang);
    let mut out = Vec::new();
    // Cursor pre-order walk: no allocation per node, and iterative, so depth never matters.
    let mut cursor = root.walk();
    'next: loop {
        let node = cursor.node();
        if node.child_count() == 0
            && kinds.contains(&node.kind())
            && let Ok(t) = node.utf8_text(src)
            && t.chars().count() >= min_len
        {
            out.push((node.start_byte(), t));
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                continue 'next;
            }
            if !cursor.goto_parent() {
                break 'next;
            }
        }
    }
    out
}

/// The identifier set of one byte range.
fn idents_in<'a>(idents: &[(usize, &'a str)], (start, end): (usize, usize)) -> HashSet<&'a str> {
    let lo = idents.partition_point(|(b, _)| *b < start);
    let hi = idents.partition_point(|(b, _)| *b < end);
    idents[lo..hi].iter().map(|(_, t)| *t).collect()
}

/// Metrics units that no other unit encloses: a nested helper counts toward its enclosing
/// test, not as a test of its own.
fn outermost_units(root: Node<'_>, lang: Language) -> Vec<Node<'_>> {
    let mut out: Vec<Node> = Vec::new();
    let mut outer_end = 0;
    for n in unit_nodes(root, lang) {
        if n.start_byte() < outer_end {
            continue;
        }
        outer_end = n.end_byte();
        out.push(n);
    }
    out
}

fn text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    node.utf8_text(src).unwrap_or("")
}

fn is_ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

/// Top-level symbols of a Source file: the outermost metrics units (functions and methods) plus
/// types and consts from a small per-grammar walk. Symbols inside inline test regions, Rust
/// trait-impl methods (named via the trait) and names shorter than `min_len` are left out.
fn symbols(root: Node, units: &[Node], file: &SourceFile, src: &[u8], test_regions: &[TestRegion], min_len: usize) -> Vec<Symbol> {
    let lang = file.lang;
    let basename_private = lang == Language::Python && file.path.rsplit('/').next().is_some_and(|b| b.starts_with('_'));
    let mut out: Vec<Symbol> = Vec::new();
    let mut push = |name: &str, kind: &'static str, (start_line, end_line): (usize, usize), public: bool| {
        push_symbol(&mut out, name, kind, (start_line, end_line), public && !basename_private, min_len)
    };
    fn span(n: Node) -> (usize, usize) {
        (n.start_position().row + 1, n.end_position().row + 1)
    }
    for &n in units {
        if regions::contains(test_regions, n.start_byte()) || (lang == Language::Rust && in_trait_impl(n)) {
            continue;
        }
        let Some(name) = unit_name(n, src) else { continue };
        let name: &str = name;
        let (kind, public): (&'static str, bool) = match lang {
            Language::Rust => (if is_method(n) { "method" } else { "fn" }, has_visibility(n)),
            Language::Python => (if is_method(n) { "method" } else { "fn" }, module_level(n) && !name.starts_with('_')),
            _ => (if is_method(n) { "method" } else { "fn" }, exported(n)),
        };
        push(name, kind, span(n), public);
    }
    // Types and consts: descend only through containers, never into a function body.
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if regions::contains(test_regions, node.start_byte()) {
            continue;
        }
        let kind = node.kind();
        let mut descend = false;
        match lang {
            Language::Rust => match kind {
                "source_file" | "mod_item" | "declaration_list" => descend = true,
                "struct_item" | "enum_item" | "union_item" | "trait_item" | "type_item" | "const_item" | "static_item" => {
                    let k = match kind {
                        "struct_item" => "struct",
                        "enum_item" => "enum",
                        "union_item" => "union",
                        "trait_item" => "trait",
                        "type_item" => "type",
                        "const_item" => "const",
                        _ => "static",
                    };
                    if let Some(n) = node.child_by_field_name("name") {
                        push(text(n, src), k, span(node), has_visibility(node));
                    }
                }
                _ => {}
            },
            Language::Python => match kind {
                "module" | "decorated_definition" => descend = true,
                "class_definition" => {
                    if let Some(n) = node.child_by_field_name("name") {
                        let name = text(n, src);
                        push(name, "class", span(node), module_level(node) && !name.starts_with('_'));
                    }
                }
                // `LIMIT = 3` at module level: the UPPER_CASE convention is what makes it a const.
                "expression_statement" => {
                    if let Some(a) = node.named_child(0).filter(|a| a.kind() == "assignment")
                        && let Some(l) = a.child_by_field_name("left").filter(|l| l.kind() == "identifier")
                    {
                        let name = text(l, src);
                        if name.chars().any(|c| c.is_ascii_uppercase()) && !name.chars().any(|c| c.is_ascii_lowercase()) {
                            push(name, "const", span(node), !name.starts_with('_'));
                        }
                    }
                }
                _ => {}
            },
            _ => match kind {
                "program" | "export_statement" => descend = true,
                "class_declaration" | "abstract_class_declaration" | "interface_declaration" | "type_alias_declaration" | "enum_declaration" => {
                    if let Some(n) = node.child_by_field_name("name") {
                        let k = match kind {
                            "interface_declaration" => "interface",
                            "type_alias_declaration" => "type",
                            "enum_declaration" => "enum",
                            _ => "class",
                        };
                        push(text(n, src), k, span(node), k == "class" && exported(node));
                    }
                }
                "lexical_declaration" | "variable_declaration" => {
                    let mut c = node.walk();
                    for d in node.named_children(&mut c).filter(|d| d.kind() == "variable_declarator") {
                        let is_fn = d.child_by_field_name("value").is_some_and(|v| matches!(v.kind(), "arrow_function" | "function_expression" | "function" | "generator_function"));
                        if !is_fn && let Some(n) = d.child_by_field_name("name").filter(|n| n.kind() == "identifier") {
                            push(text(n, src), "const", span(d), false);
                        }
                    }
                }
                _ => {}
            },
        }
        if descend {
            let mut cursor = node.walk();
            let children: Vec<Node> = node.children(&mut cursor).collect();
            for child in children.into_iter().rev() {
                stack.push(child);
            }
        }
    }
    out
}

/// The unit's own name, or the name it is bound to (`const f = () => …`, `{ f() {} }`,
/// `x.f = function …`). Call-site names (`app.post('/x')`) and anonymous units have none.
fn unit_name<'a>(node: Node, src: &'a [u8]) -> Option<&'a str> {
    if let Some(n) = node.child_by_field_name("name") {
        return Some(text(n, src));
    }
    let p = node.parent()?;
    match p.kind() {
        "variable_declarator" | "public_field_definition" => p.child_by_field_name("name").map(|n| text(n, src)),
        "pair" => p.child_by_field_name("key").map(|n| text(n, src)),
        "assignment_expression" => p.child_by_field_name("left").map(|n| text(n, src).rsplit('.').next().unwrap_or("")),
        _ => None,
    }
}

fn push_symbol(out: &mut Vec<Symbol>, name: &str, kind: &'static str, (start_line, end_line): (usize, usize), public: bool, min_len: usize) {
    if is_ident(name) && name.chars().count() >= min_len {
        out.push(Symbol { name: name.to_string(), kind, start_line, end_line, public });
    }
}

fn has_visibility(node: Node) -> bool {
    let mut c = node.walk();
    node.children(&mut c).any(|n| n.kind() == "visibility_modifier")
}

/// Inside `impl Trait for Type { … }`: the method is named through the trait, not the file.
fn in_trait_impl(node: Node) -> bool {
    let mut cur = node.parent();
    while let Some(p) = cur {
        if p.kind() == "impl_item" {
            return p.child_by_field_name("trait").is_some();
        }
        cur = p.parent();
    }
    false
}

fn is_method(node: Node) -> bool {
    if node.kind() == "method_definition" {
        return true;
    }
    let owner = node.parent().and_then(|p| match p.kind() {
        "declaration_list" | "block" | "class_body" => p.parent(),
        "public_field_definition" => Some(p),
        _ => None,
    });
    owner.is_some_and(|o| matches!(o.kind(), "impl_item" | "trait_item" | "class_definition" | "class_declaration" | "abstract_class_declaration" | "class" | "public_field_definition"))
}

/// Python: directly under the module, decorators allowed.
fn module_level(node: Node) -> bool {
    let mut p = node.parent();
    if p.is_some_and(|p| p.kind() == "decorated_definition") {
        p = p.and_then(|p| p.parent());
    }
    p.is_some_and(|p| p.kind() == "module")
}

/// TS/JS: the declaration (or the lexical declaration binding an arrow) sits in an `export_statement`.
fn exported(node: Node) -> bool {
    let mut p = node.parent();
    if p.is_some_and(|p| p.kind() == "variable_declarator") {
        p = p.and_then(|p| p.parent()).and_then(|p| p.parent());
    }
    p.is_some_and(|p| p.kind() == "export_statement")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, kind: FileKind, content: &str) -> SourceFile {
        let lang = Language::from_path(std::path::Path::new(path)).unwrap();
        SourceFile { path: path.into(), lang, kind, lines: content.lines().count(), bytes: content.len(), content: content.into() }
    }

    fn names(syms: &[Symbol]) -> Vec<(&str, &str, bool)> {
        syms.iter().map(|s| (s.name.as_str(), s.kind, s.public)).collect()
    }

    #[test]
    fn rust_symbols_skip_trait_impls_regions_and_short_names() {
        let src = "\
pub struct Config { pub a: u8 }
pub(crate) enum Shape { X }
pub trait Render { fn m(&self); }
type Alias = u8;
pub const LIMIT_MAX: usize = 3;
static COUNTER: u8 = 0;
impl Config { pub fn build_it(&self) -> u8 { fn inner_helper() {} 1 } fn private_one(&self) {} }
impl Render for Config { fn render_all(&self) {} }
pub fn free_function() -> Config { Config { a: 1 } }
pub fn run() {}
mod inner_mod { pub fn in_mod_fn() {} }
#[cfg(test)]
mod tests {
    pub fn test_helper_fn() {}
    #[test] fn t() { assert_eq!(free_function().build_it(), 1); }
}
";
        let f = file("src/a.rs", FileKind::Source, src);
        let p = parse_source(&f, &Cfg::default());
        assert_eq!(names(&p.symbols), vec![
            ("build_it", "method", true), ("private_one", "method", false), ("free_function", "fn", true), ("in_mod_fn", "fn", true),
            ("Config", "struct", true), ("Shape", "enum", true), ("Render", "trait", true), ("Alias", "type", false), ("LIMIT_MAX", "const", true), ("COUNTER", "static", false),
        ], "{:?}", p.symbols);
        // Two inline units: the region's helper (a unit, not a symbol) and the `#[test]` fn.
        assert_eq!(p.inline_units.len(), 2);
        assert!(p.inline_units[1].contains("free_function") && p.inline_units[1].contains("build_it"), "{:?}", p.inline_units[1]);
        let off = Cfg { count_inline_tests_as_refs: false, ..Cfg::default() };
        assert!(parse_source(&f, &off).inline_units.is_empty());
    }

    #[test]
    fn python_and_ts_public_rules() {
        let py = "\
LIMIT_MAX = 3
logger_x = None
class Widget:
    def render_all(self): pass
    def _hidden_one(self): pass
def free_function(): pass
def _private_fn(): pass
@decorated
def decorated_fn():
    def nested_helper(): pass
";
        let w = file("pkg/w.py", FileKind::Source, py);
        let p = parse_source(&w, &Cfg::default());
        assert_eq!(names(&p.symbols), vec![
            ("render_all", "method", false), ("_hidden_one", "method", false), ("free_function", "fn", true), ("_private_fn", "fn", false), ("decorated_fn", "fn", true),
            ("LIMIT_MAX", "const", true), ("Widget", "class", true),
        ], "{:?}", p.symbols);
        let private = file("pkg/_w.py", FileKind::Source, py);
        let p = parse_source(&private, &Cfg::default());
        assert!(p.symbols.iter().all(|s| !s.public), "{:?}", p.symbols);
        let ts = "\
export function free_function() {}
function local_function() {}
export class Widget { render_all() {} }
export const arrow_fn = () => 1
export const plain_const = 3
export interface Iface_x { a: number }
export type Alias_x = string
export enum Enum_x { A }
export default function dflt_fn() {}
app.post('/x', (req, res) => {})
";
        let w = file("src/w.ts", FileKind::Source, ts);
        let p = parse_source(&w, &Cfg::default());
        assert_eq!(names(&p.symbols), vec![
            ("free_function", "fn", true), ("local_function", "fn", false), ("render_all", "method", false), ("arrow_fn", "fn", true), ("dflt_fn", "fn", true),
            ("Widget", "class", true), ("plain_const", "const", false), ("Iface_x", "interface", false), ("Alias_x", "type", false), ("Enum_x", "enum", false),
        ], "{:?}", p.symbols);
    }

    #[test]
    fn units_are_counted_per_file_and_strings_never_count() {
        let files = vec![
            file("src/status.rs", FileKind::Source, "pub fn render_status() {}\npub fn print_status_line() {}\npub struct StatusRow;\n"),
            file("src/other.rs", FileKind::Source, "pub fn other_thing() {}\n#[cfg(test)]\nmod tests {\n    #[test] fn t() { let _ = StatusRow; }\n}\n"),
            file("src/quiet.rs", FileKind::Source, "pub fn quiet_thing() {}\npub fn quiet_other() {}\npub fn quiet_third() {}\n"),
            // Two test fns name status symbols (one only through a type), one names only a string.
            file("tests/cli.rs", FileKind::Test, "#[test]\nfn a() { render_status(); }\n#[test]\nfn b() { let r: StatusRow = todo!(); }\n#[test]\nfn c() { run(\"render_status quiet_thing\"); }\n"),
            // No unit at all: the whole file is one unit.
            file("tests/macros.rs", FileKind::Test, "rgtest!(x, |d| { print_status_line() });\n"),
            // A helper fn beside macro-made tests: each macro call is a unit of its own.
            file("tests/mixed.rs", FileKind::Test, "fn helper_x() {}\nrgtest!(a, |x| { let _ = StatusRow; });\nrgtest!(b, |x| { helper_x(); });\n"),
        ];
        let idx = index_all(&files, &Cfg::default());
        assert_eq!((idx.test_file_units, idx.inline_units), (7, 1));
        let s = &idx.files["src/status.rs"];
        assert_eq!((s.test_units, s.test_file_units, s.inline_units, s.symbols, s.public_symbols), (5, 4, 1, 3, 3), "{s:?}");
        assert!(s.unmentioned.is_empty(), "{:?}", s.unmentioned);
        let q = &idx.files["src/quiet.rs"];
        assert_eq!((q.test_units, q.public_symbols), (0, 3));
        assert_eq!(q.unmentioned.iter().map(|s| (s.name.as_str(), s.start_line)).collect::<Vec<_>>(), vec![("quiet_thing", 1), ("quiet_other", 2), ("quiet_third", 3)]);
        assert_eq!(idx.files["src/other.rs"].test_units, 0);
        assert!(!idx.files.contains_key("tests/cli.rs"));
        // A longer minimum drops the short symbol names and the mentions with them.
        let strict = Cfg { min_name_len: 14, ..Cfg::default() };
        let idx = index_all(&files, &strict);
        let s = &idx.files["src/status.rs"];
        assert_eq!((s.test_units, s.symbols), (1, 1), "{s:?}"); // only print_status_line (17) is left, named by the macro file
    }

    #[test]
    fn macro_calls_in_an_inline_region_and_attributes_above_a_test_are_units() {
        let src = "\
pub fn target_fn() {}
pub struct Target_x;
#[cfg(test)]
mod tests {
    fn helper_y() {}
    t!(one, target_fn());
    #[rstest]
    #[case(Target_x)]
    fn cased() {}
}
";
        let f = file("src/a.rs", FileKind::Source, src);
        let p = parse_source(&f, &Cfg::default());
        let sets: Vec<Vec<&str>> = p.inline_units.iter().map(|u| { let mut v: Vec<&str> = u.iter().copied().collect(); v.sort(); v }).collect();
        assert_eq!(sets, vec![vec!["helper_y"], vec!["target_fn"], vec!["Target_x", "cased", "rstest"]], "{sets:?}");
    }

    #[test]
    fn python_decorators_belong_to_the_test() {
        let files = vec![
            file("src/mod_x.py", FileKind::Source, "class Widget_x:\n    pass\ndef other_fn(): pass\n"),
            file("tests/test_mod_x.py", FileKind::Test, "import pytest\n@pytest.mark.parametrize(\"cls\", [Widget_x])\ndef test_a(cls):\n    pass\n"),
        ];
        let idx = index_all(&files, &Cfg::default());
        assert_eq!(idx.test_file_units, 1);
        let m = &idx.files["src/mod_x.py"];
        assert_eq!((m.test_units, m.public_symbols), (1, 2), "{m:?}");
        assert_eq!(m.unmentioned.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), vec!["other_fn"]);
    }

    #[test]
    fn nested_units_fold_into_the_outer_test() {
        let files = vec![
            file("src/m.py", FileKind::Source, "def target_fn(): pass\n"),
            file("tests/test_m.py", FileKind::Test, "def test_a():\n    def helper_x():\n        target_fn()\n    helper_x()\ndef test_b():\n    pass\n"),
        ];
        let idx = index_all(&files, &Cfg::default());
        assert_eq!(idx.test_file_units, 2);
        assert_eq!(idx.files["src/m.py"].test_units, 1);
    }
}
