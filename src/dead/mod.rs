//! Dead surface: exported symbols nothing outside their file uses, symbols only tests keep
//! alive, and (Rust) struct fields nothing reads and enum variants nothing constructs.
//!
//! LLM sessions write `pub fn` / `export` by default and never narrow visibility, so rustc's
//! `dead_code` lint (silent on anything public behind an all-`pub mod` lib.rs) says nothing.
//! One extra walk over the parsed trees builds a [`SymbolIndex`]: definitions and identifier
//! references by bare name, per file, split into production and test context. Every finding
//! here is a category on that index, and other passes can read it.

use crate::config::{Dead as Cfg, DeadMode};
use crate::discover::{FileKind, SourceFile};
use crate::lang::Language;
use crate::regions::{self, TestRegion};
use globset::{Glob, GlobSet, GlobSetBuilder};
use rayon::prelude::*;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use tree_sitter::Node;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Rust `pub`, TS `export`, Python without a leading `_`.
    Public,
    /// Rust `pub(crate)`, `pub(super)`, `pub(in …)`.
    Restricted,
    Private,
}

/// One definition in the index: an item of any visibility, in production or test context.
#[derive(Debug, Clone, Serialize)]
pub struct Def {
    /// The bare name references are keyed by.
    pub name: String,
    /// `Git::is_ignored` for a method, the name otherwise.
    pub qualified: String,
    /// How a reason names it: `pub fn Git::is_ignored`, `export function f`, `def f`.
    pub display: String,
    /// `fn`, `method`, `struct`, `enum`, `union`, `trait`, `type`, `const`, `static`, `mod`,
    /// `class`, `interface`, `var`.
    pub kind: &'static str,
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,
    pub visibility: Visibility,
    /// Defined in test context (a Test file, a `#[cfg(test)]` region, a `#[test]` fn, a
    /// `describe` block, a `test_` function).
    pub in_test: bool,
    /// Why it can never be a candidate: `trait impl`, `#[test]`, `@pytest.fixture`, …
    pub exempt: Option<String>,
}

impl Def {
    pub fn lines(&self) -> usize {
        self.end_line - self.start_line + 1
    }
}

/// References to one name from one file, by the context they sit in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct RefCount {
    pub prod: u32,
    pub test: u32,
}

impl RefCount {
    fn add(&mut self, other: RefCount) {
        self.prod += other.prod;
        self.test += other.test;
    }
    pub fn total(&self) -> u32 {
        self.prod + self.test
    }
}

/// A struct or enum with the attributes that decide whether its members are checked.
#[derive(Debug, Clone)]
pub struct TypeDef {
    pub name: String,
    pub is_enum: bool,
    #[allow(dead_code)] // read by later passes over the index
    pub line: usize,
    pub derives: Vec<String>,
    /// Attribute names other than `derive` (`repr`, `non_exhaustive`, `serde`).
    pub attrs: Vec<String>,
    pub in_test: bool,
}

/// A struct field or enum variant, by the index of its type in `FileIndex::types`.
#[derive(Debug, Clone)]
pub struct MemberDef {
    pub type_idx: usize,
    pub name: String,
    pub line: usize,
}

/// The Rust shape side of a file: field reads/writes and variant constructions/matches by
/// bare name, wildcard imports and trait impls.
#[derive(Debug, Clone, Default)]
pub struct Shapes {
    pub types: Vec<TypeDef>,
    pub fields: Vec<MemberDef>,
    pub variants: Vec<MemberDef>,
    pub field_reads: HashMap<String, u32>,
    /// Lines of the write sites.
    pub field_writes: HashMap<String, Vec<usize>>,
    pub variant_ctors: HashMap<String, u32>,
    /// Lines of the pattern-context mentions.
    pub variant_matches: HashMap<String, Vec<usize>>,
    /// Last segments of `use …::*` paths, at any scope.
    pub wildcard_uses: HashSet<String>,
    /// `(type, trait)` of every `impl Trait for Type`, names without generics.
    pub trait_impls: Vec<(String, String)>,
}

/// One file's side of the index.
#[derive(Debug, Clone)]
pub struct FileIndex {
    pub path: String,
    pub lang: Language,
    /// The whole file is test context (a Test file, `extra_test_globs`, benches).
    pub test_context: bool,
    pub lines: usize,
    pub inline_test_lines: usize,
    pub defs: Vec<Def>,
    /// Identifier references by bare name (a definition's own name node never counts).
    pub refs: HashMap<String, RefCount>,
    /// Words of doc comments and docstrings; counted, never a use.
    pub doc_mentions: HashMap<String, u32>,
    pub shapes: Shapes,
}

/// Definitions and references of every indexed file, with name lookups.
#[derive(Debug, Default)]
pub struct SymbolIndex {
    /// Sorted by path.
    pub files: Vec<FileIndex>,
    by_path: HashMap<String, usize>,
    /// name -> (file index, def index)
    by_name: HashMap<String, Vec<(usize, usize)>>,
    /// name -> (file index, refs)
    refs: HashMap<String, Vec<(usize, RefCount)>>,
    docs: HashMap<String, u32>,
}

impl SymbolIndex {
    pub fn build(mut files: Vec<FileIndex>) -> Self {
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let mut idx = SymbolIndex::default();
        for (fi, f) in files.iter().enumerate() {
            idx.by_path.insert(f.path.clone(), fi);
            for (di, d) in f.defs.iter().enumerate() {
                idx.by_name.entry(d.name.clone()).or_default().push((fi, di));
            }
            for (name, rc) in &f.refs {
                idx.refs.entry(name.clone()).or_default().push((fi, *rc));
            }
            for (w, n) in &f.doc_mentions {
                *idx.docs.entry(w.clone()).or_default() += n;
            }
        }
        idx.files = files;
        idx
    }

    #[allow(dead_code)] // lookups for the passes that reuse the index
    pub fn file(&self, path: &str) -> Option<&FileIndex> {
        self.by_path.get(path).map(|&i| &self.files[i])
    }

    /// Every definition of `name`, any file, any visibility, any context.
    #[allow(dead_code)]
    pub fn definitions(&self, name: &str) -> impl Iterator<Item = &Def> {
        self.by_name.get(name).into_iter().flatten().map(|&(fi, di)| &self.files[fi].defs[di])
    }

    /// Definitions sharing the bare name.
    pub fn samename(&self, name: &str) -> usize {
        self.by_name.get(name).map_or(0, Vec::len)
    }

    /// References to `name` from `path` alone.
    pub fn refs_in(&self, name: &str, path: &str) -> RefCount {
        let Some(&fi) = self.by_path.get(path) else { return RefCount::default() };
        self.files[fi].refs.get(name).copied().unwrap_or_default()
    }

    /// `(path, refs)` for every file referencing `name`.
    pub fn refs_by_file(&self, name: &str) -> impl Iterator<Item = (&str, RefCount)> {
        self.refs.get(name).into_iter().flatten().map(|&(fi, rc)| (self.files[fi].path.as_str(), rc))
    }

    /// References to `name` from every file but `path`.
    pub fn external_refs(&self, name: &str, path: &str) -> RefCount {
        let mut out = RefCount::default();
        for (p, rc) in self.refs_by_file(name) {
            if p != path {
                out.add(rc);
            }
        }
        out
    }

    pub fn doc_mentions(&self, name: &str) -> u32 {
        self.docs.get(name).copied().unwrap_or(0)
    }
}

// ---------- the walk ----------

/// Config-derived matchers, built once and shared by every file walk.
pub struct Walker {
    cfg: Cfg,
    test_globs: GlobSet,
}

fn globset(globs: &[String]) -> GlobSet {
    let mut b = GlobSetBuilder::new();
    for g in globs {
        match Glob::new(g) {
            Ok(g) => {
                b.add(g);
            }
            Err(e) => eprintln!("warning: [dead] bad glob {g:?}: {e}"),
        }
    }
    b.build().unwrap_or_else(|_| GlobSet::empty())
}

impl Walker {
    pub fn new(cfg: &Cfg) -> Self {
        let mut globs = cfg.test_only.extra_test_globs.clone();
        if !cfg.test_only.treat_benches_as_prod {
            globs.extend(cfg.test_only.bench_globs.iter().cloned());
        }
        Walker { cfg: cfg.clone(), test_globs: globset(&globs) }
    }

    /// Is the whole file test context?
    pub fn test_context(&self, file: &SourceFile) -> bool {
        file.kind == FileKind::Test || self.test_globs.is_match(&file.path)
    }

    /// One file's side of the index on an already-parsed tree (`None` when the parse failed:
    /// no definitions, no references). `regions` are the file's inline test regions.
    pub fn file_index(&self, root: Option<Node>, file: &SourceFile, regions: &[TestRegion]) -> FileIndex {
        let mut out = FileIndex {
            path: file.path.clone(),
            lang: file.lang,
            test_context: self.test_context(file),
            lines: file.lines,
            inline_test_lines: regions::inline_lines(regions),
            defs: Vec::new(),
            refs: HashMap::new(),
            doc_mentions: HashMap::new(),
            shapes: Shapes::default(),
        };
        let Some(root) = root else { return out };
        let mut st = State {
            src: file.content.as_bytes(), path: &file.path, out: &mut out, skip: HashSet::new(), cfg: &self.cfg, lang: file.lang, regions,
            refs: HashMap::new(), docs: HashMap::new(), field_reads: HashMap::new(), field_writes: HashMap::new(), variant_ctors: HashMap::new(), variant_matches: HashMap::new(),
        };
        let ctx = Ctx { in_test: st.out.test_context, ..Ctx::default() };
        st.walk(root, ctx);
        st.finish();
        out
    }

    /// Parse and index one file on its own (Test files in `scan`, everything in `scry dead`).
    pub fn parse_index(&self, file: &SourceFile) -> FileIndex {
        let src = file.content.as_bytes();
        let tree = file.lang.parser().parse(src, None);
        let root = tree.as_ref().map(|t| t.root_node());
        let regions = match (root, file.lang) {
            (Some(root), Language::Rust) => regions::test_regions(root, src),
            _ => Vec::new(),
        };
        self.file_index(root, file, &regions)
    }
}

/// Index every Source and Test file, parsing each once: `scry dead`.
pub fn index_all(files: &[SourceFile], cfg: &Cfg) -> SymbolIndex {
    let w = Walker::new(cfg);
    let idx: Vec<FileIndex> = files.par_iter().filter(|f| indexed(f)).map(|f| w.parse_index(f)).collect();
    SymbolIndex::build(idx)
}

/// Index Test files on trees `scan` parsed once for the mentions pass and this one (a Test
/// file is whole-file test context, so no regions are needed).
pub fn index_tests(tests: &[(&SourceFile, Option<tree_sitter::Tree>)], cfg: &Cfg) -> Vec<FileIndex> {
    let w = Walker::new(cfg);
    tests.par_iter().map(|(f, t)| w.file_index(t.as_ref().map(|t| t.root_node()), f, &[])).collect()
}

/// Which discovered files the index is built from.
pub fn indexed(f: &SourceFile) -> bool {
    matches!(f.kind, FileKind::Source | FileKind::Test)
}

#[derive(Clone, Copy, Default)]
struct Ctx<'s> {
    in_test: bool,
    in_pattern: bool,
    in_tt: bool,
    /// Rust `use_declaration`, TS `import_statement`.
    in_import: bool,
    /// Python docstring.
    in_doc: bool,
    /// The `left:` of an assignment (direct child only).
    assign_left: bool,
    /// TS: the `declaration:` of an `export_statement` (direct child only).
    exported: bool,
    trait_impl: bool,
    /// The impl / trait / class the item belongs to.
    owner: Option<&'s str>,
    /// The struct or enum being declared (index into `Shapes::types`).
    type_idx: Option<usize>,
    /// The parent node (`Node::parent` re-walks from the root: never call it per node).
    parent: Option<Node<'s>>,
}

struct State<'a> {
    src: &'a [u8],
    path: &'a str,
    out: &'a mut FileIndex,
    /// Start bytes of name nodes that are declarations, never references.
    skip: HashSet<usize>,
    cfg: &'a Cfg,
    lang: Language,
    regions: &'a [TestRegion],
    // Borrowed from the source during the walk; owned copies go into `out` at the end, so a
    // name is allocated once per file, not once per mention.
    refs: HashMap<&'a str, RefCount>,
    docs: HashMap<&'a str, u32>,
    field_reads: HashMap<&'a str, u32>,
    field_writes: HashMap<&'a str, Vec<usize>>,
    variant_ctors: HashMap<&'a str, u32>,
    variant_matches: HashMap<&'a str, Vec<usize>>,
}

fn text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    node.utf8_text(src).unwrap_or("")
}

fn is_ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$') && !s.chars().all(|c| c.is_ascii_digit())
}

fn line(node: Node) -> usize {
    node.start_position().row + 1
}

/// The bare name of a type node: `Store<'c>` -> `Store`, `fmt::Result` -> `Result`, `&mut T` -> `T`.
fn type_name<'a>(node: Node, src: &'a [u8]) -> Option<&'a str> {
    let mut n = node;
    loop {
        match n.kind() {
            "type_identifier" | "identifier" | "primitive_type" => return Some(text(n, src)),
            "generic_type" | "reference_type" | "pointer_type" | "dynamic_type" | "abstract_type" => n = n.child_by_field_name("type").or_else(|| n.named_child(0))?,
            "scoped_type_identifier" | "scoped_identifier" => n = n.child_by_field_name("name")?,
            _ => return None,
        }
    }
}

/// The whole string and its dot- or colon-separated segments that are identifier-shaped
/// (`'app.apps.AppConfig'`, `'pkg.mod:func'`).
fn string_segments<'a>(s: &'a str, mut f: impl FnMut(&'a str)) {
    for seg in s.split(['.', ':']) {
        let seg = seg.trim();
        if is_ident(seg) {
            f(seg);
        }
    }
}

/// `{name}` / `{name:` captures: Rust 2021 inline format arguments (`{{` is an escape).
fn format_captures<'a>(s: &'a str, mut f: impl FnMut(&'a str)) {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'{' {
            i += 1;
            continue;
        }
        if i + 1 < b.len() && b[i + 1] == b'{' {
            i += 2;
            continue;
        }
        let start = i + 1;
        let mut j = start;
        while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
            j += 1;
        }
        if j > start && j < b.len() && (b[j] == b'}' || b[j] == b':') && is_ident(&s[start..j]) {
            f(&s[start..j]);
        }
        i = j.max(i + 1);
    }
}

const RUST_PATTERNS: &[&str] = &[
    "tuple_struct_pattern", "struct_pattern", "or_pattern", "field_pattern", "slice_pattern", "tuple_pattern", "reference_pattern",
    "ref_pattern", "mut_pattern", "captured_pattern", "range_pattern", "generic_pattern", "remaining_field_pattern",
];

impl<'a> State<'a> {
    fn finish(&mut self) {
        let own = |m: &HashMap<&str, u32>| m.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        let own_v = |m: &HashMap<&str, Vec<usize>>| m.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        self.out.refs = self.refs.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        self.out.doc_mentions = own(&self.docs);
        self.out.shapes.field_reads = own(&self.field_reads);
        self.out.shapes.field_writes = own_v(&self.field_writes);
        self.out.shapes.variant_ctors = own(&self.variant_ctors);
        self.out.shapes.variant_matches = own_v(&self.variant_matches);
    }

    fn add_ref(&mut self, name: &'a str, in_test: bool) {
        let e = self.refs.entry(name).or_default();
        if in_test { e.test += 1 } else { e.prod += 1 }
    }

    fn doc_words(&mut self, s: &'a str) {
        for w in s.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|w| !w.is_empty()) {
            *self.docs.entry(w).or_default() += 1;
        }
    }

    /// Symbols: whole string, path segments and format captures. Shapes: only the format
    /// captures of a string inside a macro body (`index.db` in a file name is not a field read).
    fn string_leaf(&mut self, s: &'a str, ctx: Ctx) {
        let mut names: Vec<&'a str> = Vec::new();
        if self.cfg.symbols.string_refs {
            string_segments(s, |n| names.push(n));
            format_captures(s, |n| names.push(n));
            for n in &names {
                self.add_ref(n, ctx.in_test);
            }
        }
        if self.cfg.shapes.string_refs && self.lang == Language::Rust && ctx.in_tt {
            names.clear();
            format_captures(s, |n| names.push(n));
            for n in names {
                *self.field_reads.entry(n).or_default() += 1;
                *self.variant_ctors.entry(n).or_default() += 1;
            }
        }
    }

    /// Outer attributes above `item`, looking past comments: `(name, derive identifiers)`.
    fn attrs_of(&self, item: Node) -> Vec<(String, Vec<String>)> {
        let mut out = Vec::new();
        let mut prev = item.prev_named_sibling();
        while let Some(p) = prev {
            match p.kind() {
                "attribute_item" => {
                    if let Some(a) = p.named_child(0).filter(|a| a.kind() == "attribute")
                        && let Some(n) = a.named_child(0)
                    {
                        let name = text(n, self.src).to_string();
                        let mut derives = Vec::new();
                        if name == "derive" && let Some(tt) = a.child_by_field_name("arguments") {
                            let mut c = tt.walk();
                            derives.extend(tt.named_children(&mut c).filter(|d| d.kind() == "identifier").map(|d| text(d, self.src).to_string()));
                        }
                        out.push((name, derives));
                    }
                }
                "line_comment" | "block_comment" => {}
                _ => break,
            }
            prev = p.prev_named_sibling();
        }
        out
    }

    fn skipped_attr(&self, attrs: &[(String, Vec<String>)]) -> Option<String> {
        attrs.iter().find(|(a, _)| {
            let last = a.rsplit("::").next().unwrap_or(a);
            self.cfg.symbols.skip_attrs.iter().any(|s| s == a || s == last)
        }).map(|(a, _)| format!("#[{a}]"))
    }

    /// Record a definition: `(kind, keyword)` is how it is classed and printed, `(vis, vis_text)`
    /// its visibility and the modifier's spelling.
    fn push_def(&mut self, name_node: Node, item: Node, (kind, keyword): (&'static str, &str), (vis, vis_text): (Visibility, &str), ctx: Ctx, exempt: Option<String>) {
        let name = text(name_node, self.src).to_string();
        self.skip.insert(name_node.start_byte());
        let qualified = match ctx.owner {
            Some(o) if kind == "method" => format!("{o}::{name}"),
            Some(o) if self.lang == Language::Python && kind == "method" => format!("{o}.{name}"),
            _ => name.clone(),
        };
        let display = if vis_text.is_empty() { format!("{keyword} {qualified}") } else { format!("{vis_text} {keyword} {qualified}") };
        self.out.defs.push(Def {
            name, qualified, display, kind, file: self.path.to_string(),
            start_line: line(item), end_line: item.end_position().row + 1, visibility: vis, in_test: ctx.in_test, exempt,
        });
    }

    /// Cursor pre-order walk: one cursor for the whole tree (a cursor per node is the cost),
    /// iterative, with the enclosing items' base contexts on an explicit stack.
    fn walk(&mut self, root: Node<'a>, initial: Ctx<'a>) {
        let mut cursor = root.walk();
        let mut parents: Vec<(Node<'a>, Ctx<'a>)> = Vec::new();
        loop {
            let node = cursor.node();
            let mut ctx = match parents.last() {
                Some(&(p, base)) => self.child_ctx(p, base, cursor.field_name()),
                None => initial,
            };
            if !ctx.in_test && self.lang == Language::Rust && regions::contains(self.regions, node.start_byte()) {
                ctx.in_test = true;
            }
            let base = match self.lang {
                Language::Rust => self.rust_node(node, ctx),
                Language::Python => self.python_node(node, ctx),
                _ => self.ts_node(node, ctx),
            };
            if cursor.goto_first_child() {
                parents.push((node, base));
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return;
                }
                parents.pop();
            }
        }
    }

    /// The context a child inherits: sticky flags from `base`, plus what the parent's kind and
    /// the child's field add.
    fn child_ctx(&self, parent: Node<'a>, base: Ctx<'a>, field: Option<&str>) -> Ctx<'a> {
        let mut ctx = Ctx { assign_left: false, exported: false, parent: Some(parent), ..base };
        match self.lang {
            Language::Rust => match parent.kind() {
                k if RUST_PATTERNS.contains(&k) => ctx.in_pattern = true,
                "match_pattern" => ctx.in_pattern = field != Some("condition"),
                "let_declaration" | "let_condition" | "for_expression" | "parameter" => {
                    if field == Some("pattern") {
                        ctx.in_pattern = true;
                    }
                }
                "closure_parameters" => ctx.in_pattern = true,
                "assignment_expression" | "compound_assignment_expr" => ctx.assign_left = field == Some("left"),
                "token_tree" => ctx.in_tt = true,
                "use_declaration" => ctx.in_import = true,
                _ => {}
            },
            Language::Python => {}
            _ => match parent.kind() {
                "export_statement" => ctx.exported = field == Some("declaration"),
                "import_statement" => ctx.in_import = true,
                _ => {}
            },
        }
        ctx
    }

    // ----- Rust -----

    fn rust_node(&mut self, node: Node<'a>, ctx: Ctx<'a>) -> Ctx<'a> {
        let src = self.src;
        let mut base = ctx;
        let kind = node.kind();
        if node.child_count() == 0 {
            match kind {
                "identifier" | "type_identifier" | "field_identifier" | "shorthand_field_identifier" => {
                    if !self.skip.contains(&node.start_byte()) {
                        let t = text(node, src);
                        self.add_ref(t, ctx.in_test);
                        if ctx.in_tt {
                            *self.field_reads.entry(t).or_default() += 1;
                            *self.variant_ctors.entry(t).or_default() += 1;
                        }
                    }
                }
                "string_content" => self.string_leaf(text(node, src), ctx),
                "doc_comment" => self.doc_words(text(node, src)),
                _ => {}
            }
            return base;
        }
        match kind {
            "function_item" | "function_signature_item" => {
                let attrs = self.attrs_of(node);
                let exempt = if ctx.trait_impl { Some("trait impl".to_string()) } else { self.skipped_attr(&attrs) };
                let (vis, vis_text) = visibility(node, src);
                let kind = if ctx.owner.is_some() { "method" } else { "fn" };
                if let Some(n) = node.child_by_field_name("name") {
                    self.push_def(n, node, (kind, "fn"), (vis, vis_text), ctx, exempt);
                }
            }
            "struct_item" | "enum_item" | "union_item" | "trait_item" | "type_item" | "const_item" | "static_item" | "mod_item" => {
                let attrs = self.attrs_of(node);
                let exempt = self.skipped_attr(&attrs);
                let (vis, vis_text) = visibility(node, src);
                let (k, kw) = match kind {
                    "struct_item" => ("struct", "struct"),
                    "enum_item" => ("enum", "enum"),
                    "union_item" => ("union", "union"),
                    "trait_item" => ("trait", "trait"),
                    "type_item" => ("type", "type"),
                    "const_item" => ("const", "const"),
                    "static_item" => ("static", "static"),
                    _ => ("mod", "mod"),
                };
                if let Some(n) = node.child_by_field_name("name") {
                    self.push_def(n, node, (k, kw), (vis, vis_text), ctx, exempt);
                    if matches!(kind, "struct_item" | "enum_item") {
                        let mut derives = Vec::new();
                        let mut names = Vec::new();
                        for (a, d) in attrs {
                            if a == "derive" { derives.extend(d) } else { names.push(a) }
                        }
                        self.out.shapes.types.push(TypeDef { name: text(n, src).to_string(), is_enum: kind == "enum_item", line: line(node), derives, attrs: names, in_test: ctx.in_test });
                        base.type_idx = Some(self.out.shapes.types.len() - 1);
                    } else {
                        base.type_idx = None;
                    }
                }
                if kind == "trait_item" {
                    base.owner = node.child_by_field_name("name").map(|n| text(n, src));
                    base.trait_impl = false;
                } else if kind == "mod_item" {
                    base.owner = None;
                }
            }
            "impl_item" => {
                let ty = node.child_by_field_name("type").and_then(|t| type_name(t, src));
                let tr = node.child_by_field_name("trait").and_then(|t| type_name(t, src));
                if let (Some(ty), Some(tr)) = (ty, tr) {
                    self.out.shapes.trait_impls.push((ty.to_string(), tr.to_string()));
                }
                base.owner = ty;
                base.trait_impl = tr.is_some();
            }
            "field_declaration" => {
                if let Some(n) = node.child_by_field_name("name") {
                    self.skip.insert(n.start_byte());
                    if let Some(ti) = ctx.type_idx && !self.out.shapes.types[ti].is_enum {
                        self.out.shapes.fields.push(MemberDef { type_idx: ti, name: text(n, src).to_string(), line: line(node) });
                    }
                }
            }
            "enum_variant" => {
                if let Some(n) = node.child_by_field_name("name") {
                    self.skip.insert(n.start_byte());
                    if let Some(ti) = ctx.type_idx {
                        self.out.shapes.variants.push(MemberDef { type_idx: ti, name: text(n, src).to_string(), line: line(node) });
                    }
                }
            }
            "field_expression" => {
                if let Some(f) = node.child_by_field_name("field").filter(|f| f.kind() == "field_identifier") {
                    let name = text(f, src);
                    if ctx.assign_left {
                        self.field_writes.entry(name).or_default().push(line(node));
                    } else {
                        *self.field_reads.entry(name).or_default() += 1;
                    }
                }
            }
            "field_pattern" => {
                if let Some(n) = node.child_by_field_name("name") {
                    *self.field_reads.entry(text(n, src)).or_default() += 1;
                }
            }
            "field_initializer" => {
                if let Some(f) = node.child_by_field_name("field") {
                    self.field_writes.entry(text(f, src)).or_default().push(line(node));
                }
            }
            "shorthand_field_initializer" => {
                if let Some(f) = node.named_child(0) {
                    self.field_writes.entry(text(f, src)).or_default().push(line(node));
                }
            }
            "scoped_identifier" if !ctx.in_import => {
                if let Some(n) = node.child_by_field_name("name") {
                    let name = text(n, src);
                    if ctx.in_pattern {
                        self.variant_matches.entry(name).or_default().push(line(node));
                    } else {
                        *self.variant_ctors.entry(name).or_default() += 1;
                    }
                }
            }
            "scoped_type_identifier" if !ctx.in_import => {
                if let Some(n) = node.child_by_field_name("name") {
                    let name = text(n, src);
                    if ctx.in_pattern {
                        self.variant_matches.entry(name).or_default().push(line(node));
                    } else if ctx.parent.is_some_and(|p| p.kind() == "struct_expression") {
                        *self.variant_ctors.entry(name).or_default() += 1;
                    }
                }
            }
            "use_wildcard" => {
                if let Some(p) = node.named_child(0) && let Some(last) = type_name(p, src) {
                    self.out.shapes.wildcard_uses.insert(last.to_string());
                }
            }
            _ => {}
        }
        base
    }

    // ----- TypeScript / TSX / JavaScript -----

    fn ts_node(&mut self, node: Node<'a>, ctx: Ctx<'a>) -> Ctx<'a> {
        let src = self.src;
        let mut base = ctx;
        let kind = node.kind();
        if node.child_count() == 0 {
            match kind {
                "identifier" | "type_identifier" | "property_identifier" | "shorthand_property_identifier" | "shorthand_property_identifier_pattern" => {
                    if !ctx.in_import && !self.skip.contains(&node.start_byte()) {
                        self.add_ref(text(node, src), ctx.in_test);
                    }
                }
                "string_fragment" => self.string_leaf(text(node, src), ctx),
                "comment" => self.doc_words(text(node, src)),
                _ => {}
            }
            return base;
        }
        let top = ctx.parent.is_some_and(|p| p.kind() == "program") || ctx.exported;
        let vis = |exported: bool| if exported { (Visibility::Public, "export") } else { (Visibility::Private, "") };
        let framework = |name: &str| self.cfg.symbols.ts_framework_exports.iter().any(|f| f == name).then(|| "framework export".to_string());
        match kind {
            "function_declaration" | "generator_function_declaration" => {
                if let Some(n) = node.child_by_field_name("name") {
                    let (v, vt) = vis(ctx.exported);
                    let v = if top { v } else { Visibility::Private };
                    let ex = framework(text(n, src));
                    self.push_def(n, node, ("fn", "function"), (v, vt), ctx, ex);
                }
            }
            "class_declaration" | "abstract_class_declaration" | "interface_declaration" | "type_alias_declaration" | "enum_declaration" => {
                if let Some(n) = node.child_by_field_name("name") {
                    let (k, kw) = match kind {
                        "interface_declaration" => ("interface", "interface"),
                        "type_alias_declaration" => ("type", "type"),
                        "enum_declaration" => ("enum", "enum"),
                        _ => ("class", "class"),
                    };
                    let (v, vt) = vis(ctx.exported);
                    let v = if top { v } else { Visibility::Private };
                    let ex = framework(text(n, src));
                    self.push_def(n, node, (k, kw), (v, vt), ctx, ex);
                }
                if k_is_class(kind) {
                    base.owner = node.child_by_field_name("name").map(|n| text(n, src));
                }
            }
            "lexical_declaration" | "variable_declaration" => {
                let mut c = node.walk();
                let decls: Vec<Node> = node.named_children(&mut c).filter(|d| d.kind() == "variable_declarator").collect();
                for d in decls {
                    let Some(n) = d.child_by_field_name("name").filter(|n| n.kind() == "identifier") else { continue };
                    let is_fn = d.child_by_field_name("value").is_some_and(|v| matches!(v.kind(), "arrow_function" | "function_expression" | "function" | "generator_function"));
                    let (v, vt) = vis(ctx.exported);
                    let v = if top { v } else { Visibility::Private };
                    let ex = framework(text(n, src));
                    self.push_def(n, d, if is_fn { ("fn", "function") } else { ("const", "const") }, (v, vt), ctx, ex);
                }
            }
            "method_definition" => {
                if let Some(n) = node.child_by_field_name("name").filter(|n| n.kind() == "property_identifier") {
                    self.push_def(n, node, ("method", "method"), (Visibility::Private, ""), ctx, None);
                }
            }
            "call_expression" => {
                if let Some(f) = node.child_by_field_name("function").filter(|f| f.kind() == "identifier")
                    && self.cfg.symbols.ts_test_calls.iter().any(|t| t == text(f, src))
                {
                    base.in_test = true;
                }
            }
            _ => {}
        }
        base
    }

    // ----- Python -----

    fn python_node(&mut self, node: Node<'a>, ctx: Ctx<'a>) -> Ctx<'a> {
        let src = self.src;
        let mut base = ctx;
        let kind = node.kind();
        if node.child_count() == 0 {
            match kind {
                "identifier" => {
                    if !self.skip.contains(&node.start_byte()) {
                        self.add_ref(text(node, src), ctx.in_test);
                    }
                }
                "string_content" => {
                    if ctx.in_doc { self.doc_words(text(node, src)) } else { self.string_leaf(text(node, src), ctx) }
                }
                "comment" => self.doc_words(text(node, src)),
                _ => {}
            }
            return base;
        }
        let module_level = |n: Node| {
            let mut p = n.parent();
            if p.is_some_and(|p| p.kind() == "decorated_definition") {
                p = p.and_then(|p| p.parent());
            }
            p.is_some_and(|p| p.kind() == "module")
        };
        let public = |name: &str, top: bool| if top && !name.starts_with('_') { (Visibility::Public, "") } else { (Visibility::Private, "") };
        match kind {
            "function_definition" | "class_definition" => {
                if let Some(n) = node.child_by_field_name("name") {
                    let name = text(n, src);
                    let is_method = kind == "function_definition" && ctx.owner.is_some() && node.parent().and_then(|b| b.parent().map(|p| p.kind() == "class_definition" || p.kind() == "decorated_definition")).unwrap_or(false);
                    let top = module_level(node) || is_method;
                    let (v, vt) = public(name, top);
                    let decorators = node.parent().filter(|p| p.kind() == "decorated_definition").map(|p| {
                        let mut c = p.walk();
                        p.named_children(&mut c).filter(|d| d.kind() == "decorator").map(|d| text(d, src).trim_start_matches('@').to_string()).collect::<Vec<_>>()
                    }).unwrap_or_default();
                    let exempt = decorators.iter().find(|d| {
                        let d = d.split('(').next().unwrap_or(d);
                        self.cfg.symbols.skip_decorators.iter().any(|s| match s.strip_suffix('*') { Some(prefix) => d.starts_with(prefix), None => s == d })
                    }).map(|d| format!("@{d}"));
                    let is_test = (kind == "function_definition" && name.starts_with("test_")) || decorators.iter().any(|d| d.contains("pytest"));
                    let ctx_here = Ctx { in_test: ctx.in_test || is_test, ..ctx };
                    let (k, kw) = match (kind, is_method) {
                        ("class_definition", _) => ("class", "class"),
                        (_, true) => ("method", "def"),
                        _ => ("fn", "def"),
                    };
                    self.push_def(n, node, (k, kw), (v, vt), ctx_here, exempt);
                    base.in_test = ctx_here.in_test;
                    base.owner = if kind == "class_definition" { Some(name) } else { None };
                }
            }
            "assignment" => {
                if let Some(l) = node.child_by_field_name("left").filter(|l| l.kind() == "identifier")
                    && ctx.parent.is_some_and(|p| p.kind() == "expression_statement" && p.parent().is_some_and(|m| m.kind() == "module"))
                {
                    let name = text(l, src);
                    let (v, vt) = public(name, true);
                    let is_const = name.chars().any(|c| c.is_ascii_uppercase()) && !name.chars().any(|c| c.is_ascii_lowercase());
                    self.push_def(l, node, if is_const { ("const", "const") } else { ("var", "var") }, (v, vt), ctx, None);
                }
            }
            "string" => {
                // A docstring: the first statement of a module, class or function body.
                if let Some(p) = ctx.parent.filter(|p| p.kind() == "expression_statement")
                    && let Some(b) = p.parent().filter(|b| matches!(b.kind(), "block" | "module"))
                    && b.named_child(0).is_some_and(|f| f.id() == p.id())
                {
                    base.in_doc = true;
                }
            }
            _ => {}
        }
        base
    }
}

fn k_is_class(kind: &str) -> bool {
    matches!(kind, "class_declaration" | "abstract_class_declaration")
}

/// Rust: the item's visibility and its text (`pub`, `pub(crate)`).
fn visibility<'a>(node: Node, src: &'a [u8]) -> (Visibility, &'a str) {
    let mut c = node.walk();
    match node.children(&mut c).find(|n| n.kind() == "visibility_modifier") {
        None => (Visibility::Private, ""),
        Some(v) => {
            let t = text(v, src);
            (if t == "pub" { Visibility::Public } else { Visibility::Restricted }, t)
        }
    }
}

// ---------- manifests and mode ----------

#[derive(Debug, Clone)]
struct Manifest {
    /// Repo-relative directory holding it (`""` at the root).
    dir: String,
    mode: DeadMode,
    /// What decided the mode, for the `scry dead` header.
    why: String,
    api_roots: GlobSet,
    entrypoints: GlobSet,
}

/// Resolve `exports`-style values of a package.json: strings anywhere under the key, `./dist/x.js`
/// remapped to `src/x.ts`; wildcards stay globs.
fn package_targets(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) => {
            let s = s.trim_start_matches("./");
            let s = s.strip_prefix("dist/").map_or(s.to_string(), |r| format!("src/{r}"));
            let stem = s.strip_suffix(".js").or_else(|| s.strip_suffix(".mjs")).or_else(|| s.strip_suffix(".cjs")).or_else(|| s.strip_suffix(".d.ts"));
            match stem {
                Some(st) => out.extend([format!("{st}.ts"), format!("{st}.tsx"), format!("{st}.mts"), format!("{st}.js")]),
                None => out.push(s),
            }
        }
        serde_json::Value::Object(m) => m.values().for_each(|v| package_targets(v, out)),
        serde_json::Value::Array(a) => a.iter().for_each(|v| package_targets(v, out)),
        _ => {}
    }
}

/// The manifest of `dir` (repo-relative), read from disk, or None when it has none.
fn read_manifest(root: &Path, dir: &str, lang: Language, cfg: &Cfg) -> Option<Manifest> {
    let sym = &cfg.symbols;
    let disk = if dir.is_empty() { root.to_path_buf() } else { root.join(dir) };
    let mut api = sym.api_roots.clone();
    let mut entry = sym.entrypoints.clone();
    let (mode, why) = match lang {
        Language::Rust => {
            let t: toml::Table = toml::from_str(&std::fs::read_to_string(disk.join("Cargo.toml")).ok()?).ok()?;
            let bins = t.get("bin").and_then(|b| b.as_array()).cloned().unwrap_or_default();
            for b in &bins {
                if let Some(p) = b.get("path").and_then(|p| p.as_str()) {
                    entry.push(p.trim_start_matches("./").to_string());
                }
            }
            if let Some(p) = t.get("lib").and_then(|l| l.get("path")).and_then(|p| p.as_str()) {
                api.push(p.trim_start_matches("./").to_string());
            }
            let has_bin = !bins.is_empty() || disk.join("src/main.rs").is_file() || disk.join("src/bin").is_dir();
            let has_lib = t.contains_key("lib") || disk.join("src/lib.rs").is_file();
            if has_bin {
                (DeadMode::Application, format!("{} has {}", join_dir(dir, "Cargo.toml"), if bins.is_empty() { "src/main.rs" } else { "[[bin]]" }))
            } else if has_lib {
                (DeadMode::Library, format!("{} has [lib] and no [[bin]]", join_dir(dir, "Cargo.toml")))
            } else {
                (DeadMode::Application, format!("{} names no target", join_dir(dir, "Cargo.toml")))
            }
        }
        Language::Python => {
            let t: toml::Table = toml::from_str(&std::fs::read_to_string(disk.join("pyproject.toml")).ok()?).ok()?;
            let scripts = t.get("project").and_then(|p| p.get("scripts")).is_some();
            if scripts {
                (DeadMode::Application, format!("{} has [project.scripts]", join_dir(dir, "pyproject.toml")))
            } else {
                (DeadMode::Library, format!("{} has no [project.scripts]", join_dir(dir, "pyproject.toml")))
            }
        }
        _ => {
            let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(disk.join("package.json")).ok()?).ok()?;
            let mut targets = Vec::new();
            for k in ["exports", "main", "module", "types", "bin"] {
                if let Some(x) = v.get(k) {
                    package_targets(x, &mut targets);
                }
            }
            api.extend(targets);
            if v.get("exports").is_some() {
                (DeadMode::Library, format!("{} has exports", join_dir(dir, "package.json")))
            } else {
                (DeadMode::Application, format!("{} has no exports", join_dir(dir, "package.json")))
            }
        }
    };
    Some(Manifest { dir: dir.to_string(), mode, why, api_roots: globset(&api), entrypoints: globset(&entry) })
}

fn join_dir(dir: &str, file: &str) -> String {
    if dir.is_empty() { file.to_string() } else { format!("{dir}/{file}") }
}

/// Per-file mode and root resolution, manifests read once per directory.
struct Modes {
    cfg_mode: DeadMode,
    read: bool,
    manifests: HashMap<(String, &'static str), Option<Manifest>>,
    default_api: GlobSet,
    default_entry: GlobSet,
}

impl Modes {
    fn new(root: &Path, index: &SymbolIndex, cfg: &Cfg) -> Self {
        let mut m = Modes { cfg_mode: cfg.symbols.mode, read: cfg.symbols.read_manifests, manifests: HashMap::new(), default_api: globset(&cfg.symbols.api_roots), default_entry: globset(&cfg.symbols.entrypoints) };
        if !m.read {
            return m;
        }
        // Every ancestor directory of every file, once per language family.
        let mut dirs: HashSet<(String, &'static str)> = HashSet::new();
        for f in &index.files {
            let fam = family(f.lang);
            let mut d = f.path.rsplit_once('/').map_or("", |(d, _)| d).to_string();
            loop {
                dirs.insert((d.clone(), fam));
                match d.rfind('/') {
                    Some(i) => d.truncate(i),
                    None if d.is_empty() => break,
                    None => d.clear(),
                }
            }
        }
        for (d, fam) in dirs {
            let lang = match fam { "rust" => Language::Rust, "python" => Language::Python, _ => Language::TypeScript };
            let man = read_manifest(root, &d, lang, cfg);
            m.manifests.insert((d, fam), man);
        }
        m
    }

    /// The nearest manifest above `path` for its language family.
    fn manifest(&self, path: &str, lang: Language) -> Option<&Manifest> {
        let fam = family(lang);
        let mut d = path.rsplit_once('/').map_or("", |(d, _)| d).to_string();
        loop {
            if let Some(Some(m)) = self.manifests.get(&(d.clone(), fam)) {
                return Some(m);
            }
            match d.rfind('/') {
                Some(i) => d.truncate(i),
                None if d.is_empty() => return None,
                None => d.clear(),
            }
        }
    }

    /// `(mode, api root?, entrypoint?)` for one file.
    fn resolve(&self, path: &str, lang: Language) -> (DeadMode, bool, bool) {
        let man = self.manifest(path, lang);
        let mode = match self.cfg_mode {
            DeadMode::Auto => man.map_or(DeadMode::Application, |m| m.mode),
            m => m,
        };
        let rel = man.and_then(|m| if m.dir.is_empty() { Some(path) } else { path.strip_prefix(&format!("{}/", m.dir)) }).unwrap_or(path);
        let (api, entry) = match man {
            Some(m) => (&m.api_roots, &m.entrypoints),
            None => (&self.default_api, &self.default_entry),
        };
        let is_api = api.is_match(path) || api.is_match(rel) || self.default_api.is_match(path);
        let is_entry = entry.is_match(path) || entry.is_match(rel) || self.default_entry.is_match(path);
        (mode, is_api, is_entry)
    }

    /// `manifest dir -> (mode, why)` for the header, plus the configured override.
    fn summary(&self) -> Vec<String> {
        let mut rows: Vec<String> = self.manifests.values().flatten().map(|m| format!("{}: {} ({})", if m.dir.is_empty() { "." } else { &m.dir }, mode_name(m.mode), m.why)).collect();
        rows.sort();
        rows.dedup();
        rows
    }
}

fn family(lang: Language) -> &'static str {
    match lang {
        Language::Rust => "rust",
        Language::Python => "python",
        _ => "typescript",
    }
}

fn mode_name(m: DeadMode) -> &'static str {
    match m {
        DeadMode::Auto => "auto",
        DeadMode::Library => "library",
        DeadMode::Application => "application",
    }
}

fn kind_class(kind: &str) -> &'static str {
    match kind {
        "fn" | "method" => "fn",
        "struct" | "enum" | "union" | "trait" | "type" | "class" | "interface" => "type",
        "const" | "static" | "var" => "const",
        "mod" => "mod",
        _ => "other",
    }
}

// ---------- categories ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// No reference anywhere, in any context.
    Dead,
    /// No production reference; test references at or above the floor.
    TestOnly,
    /// No production reference outside its file; some inside it.
    Overexported,
    /// Would be dead or test-only, but another definition shares the name.
    Ambiguous,
    Reachable,
}

/// One candidate symbol with its counters.
#[derive(Debug, Clone, Serialize)]
pub struct SymbolReport {
    pub name: String,
    pub display: String,
    pub kind: &'static str,
    pub visibility: Visibility,
    pub start_line: usize,
    pub end_line: usize,
    pub category: Category,
    pub external_prod_refs: u32,
    pub external_test_refs: u32,
    pub own_prod_refs: u32,
    pub own_test_refs: u32,
    pub doc_mentions: u32,
    pub samename: usize,
    /// Files with a test reference (for test-only symbols).
    pub test_files: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeKind {
    Field,
    Variant,
}

/// A never-read field or never-constructed variant.
#[derive(Debug, Clone, Serialize)]
pub struct Shape {
    pub kind: ShapeKind,
    pub owner: String,
    pub name: String,
    pub line: usize,
    /// Field: the write sites; variant: the match sites (`file line`).
    pub sites: Vec<String>,
    /// `never read`, `never constructed`, `never constructed or matched`.
    pub label: &'static str,
    pub reason: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FileDead {
    pub pub_items: usize,
    pub dead_count: usize,
    pub test_only_count: usize,
    pub overexported_count: usize,
    pub ambiguous_count: usize,
    /// `overexported_count / pub_items`.
    pub own_file_only_share: f64,
    /// Lines spanned by candidates with no production reference outside the file (dead,
    /// test-only, in-file-only, ambiguous), as a union of line ranges.
    pub dead_lines: usize,
    /// `dead_lines / (lines - inline_test_lines)`.
    pub dead_ratio: f64,
    pub symbols: Vec<SymbolReport>,
    pub shapes: Vec<Shape>,
    /// The reason lines the report prints for the file, in order.
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    pub files_indexed: usize,
    pub pub_items: usize,
    pub dead: usize,
    pub test_only: usize,
    pub overexported: usize,
    pub ambiguous: usize,
    /// Exported items in production code that were not checked: plain `pub` / `export` in
    /// library mode, api roots and entrypoints, unchecked languages, under `min_lines`.
    pub exempt_items: usize,
    /// `(dead + test_only + overexported + ambiguous) / pub_items`.
    pub zero_external_share: f64,
    pub shapes: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DeadReport {
    pub totals: Totals,
    /// `dir: mode (why)` per manifest read.
    pub modes: Vec<String>,
    /// Languages present but not checked, and why.
    pub notes: Vec<String>,
    /// Per Source file with at least one candidate, keyed by path.
    pub files: BTreeMap<String, FileDead>,
}

fn range(s: usize, e: usize) -> String {
    if s == e { format!("{s}") } else { format!("{s}-{e}") }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn ordinal(p: f64) -> String {
    let n = (p * 100.0).round() as usize;
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// Union of line ranges.
fn union_lines(mut ranges: Vec<(usize, usize)>) -> usize {
    ranges.sort_unstable();
    let mut covered = 0;
    let mut to = 0;
    for (s, e) in ranges {
        let s = s.max(to + 1);
        if e >= s {
            covered += e - s + 1;
            to = e;
        }
    }
    covered
}

/// Categorise the index: `scan` and `scry dead`.
pub fn analyze(index: &SymbolIndex, root: &Path, cfg: &Cfg) -> DeadReport {
    let sym = &cfg.symbols;
    let modes = Modes::new(root, index, cfg);
    let mut report = DeadReport { modes: modes.summary(), ..DeadReport::default() };
    report.totals.files_indexed = index.files.len();
    let files_of_lang = |lang: Language| index.files.iter().filter(|f| family(f.lang) == family(lang)).count();
    let mut ts_unchecked = 0usize;
    let mut py_unchecked = 0usize;
    let mut exempt = 0usize;

    let mut per_file: Vec<(usize, FileDead)> = Vec::new();
    for (fi, f) in index.files.iter().enumerate() {
        if f.test_context || !sym.enabled {
            continue;
        }
        let fam = family(f.lang);
        let lang_on = sym.languages.iter().any(|l| l == fam) && (fam != "typescript" || sym.mode == DeadMode::Application);
        let (mode, is_api, is_entry) = modes.resolve(&f.path, f.lang);
        let mut fd = FileDead::default();
        for d in &f.defs {
            if d.in_test || d.visibility == Visibility::Private {
                continue;
            }
            exempt += 1;
            if d.exempt.is_some() {
                continue;
            }
            if !lang_on {
                if fam == "typescript" { ts_unchecked += 1 } else if fam == "python" { py_unchecked += 1 }
                continue;
            }
            let checked_vis = match mode {
                DeadMode::Library => d.visibility == Visibility::Restricted,
                _ => true,
            };
            if !checked_vis || is_entry || (mode == DeadMode::Library && is_api) {
                continue;
            }
            if !sym.kinds.iter().any(|k| k == kind_class(d.kind)) || d.lines() < sym.min_lines {
                continue;
            }
            if sym.skip_names.iter().any(|s| s == &d.name) || sym.skip_name_prefixes.iter().any(|p| d.name.starts_with(p.as_str())) {
                continue;
            }
            exempt -= 1;
            let own = index.refs_in(&d.name, &f.path);
            let ext = index.external_refs(&d.name, &f.path);
            let samename = index.samename(&d.name);
            let test_refs = (own.test + ext.test) as usize;
            let category = if ext.prod > 0 {
                Category::Reachable
            } else if own.prod > 0 {
                Category::Overexported
            } else if test_refs == 0 {
                if samename == 1 { Category::Dead } else { Category::Ambiguous }
            } else if cfg.test_only.enabled && test_refs >= cfg.test_only.min_external_test_refs {
                if samename == 1 { Category::TestOnly } else { Category::Ambiguous }
            } else {
                Category::Reachable
            };
            let mut test_files: Vec<String> = index.refs_by_file(&d.name).filter(|(_, rc)| rc.test > 0).map(|(p, _)| p.to_string()).collect();
            test_files.sort();
            fd.symbols.push(SymbolReport {
                name: d.name.clone(), display: d.display.clone(), kind: d.kind, visibility: d.visibility, start_line: d.start_line, end_line: d.end_line,
                category, external_prod_refs: ext.prod, external_test_refs: ext.test, own_prod_refs: own.prod, own_test_refs: own.test,
                doc_mentions: index.doc_mentions(&d.name), samename, test_files,
            });
        }
        if fd.symbols.is_empty() && !(cfg.shapes.enabled && f.lang == Language::Rust) {
            continue;
        }
        fd.pub_items = fd.symbols.len();
        fd.dead_count = fd.symbols.iter().filter(|s| s.category == Category::Dead).count();
        fd.test_only_count = fd.symbols.iter().filter(|s| s.category == Category::TestOnly).count();
        fd.overexported_count = fd.symbols.iter().filter(|s| s.category == Category::Overexported).count();
        fd.ambiguous_count = fd.symbols.iter().filter(|s| s.category == Category::Ambiguous).count();
        fd.own_file_only_share = if fd.pub_items == 0 { 0.0 } else { fd.overexported_count as f64 / fd.pub_items as f64 };
        fd.dead_lines = union_lines(fd.symbols.iter().filter(|s| s.external_prod_refs == 0).map(|s| (s.start_line, s.end_line)).collect());
        fd.dead_ratio = fd.dead_lines as f64 / f.lines.saturating_sub(f.inline_test_lines).max(1) as f64;
        // Per-symbol reasons: dead before test-only, longest first, then ambiguous.
        let mut listed: Vec<&SymbolReport> = fd.symbols.iter().filter(|s| matches!(s.category, Category::Dead | Category::TestOnly | Category::Ambiguous)).collect();
        listed.sort_by_key(|s| (s.category != Category::Dead, s.category != Category::TestOnly, std::cmp::Reverse(s.end_line - s.start_line), s.start_line));
        let n_files = files_of_lang(f.lang);
        let mut reasons: Vec<String> = Vec::new();
        let all_test_only = fd.pub_items >= 2 && fd.test_only_count == fd.pub_items;
        if all_test_only {
            let lo = fd.symbols.iter().map(|s| s.start_line).min().unwrap_or(0);
            let hi = fd.symbols.iter().map(|s| s.end_line).max().unwrap_or(0);
            let mut kept: Vec<&str> = fd.symbols.iter().flat_map(|s| s.test_files.iter().map(String::as_str)).collect();
            kept.sort_unstable();
            kept.dedup();
            reasons.push(format!("all {} exported symbols in {} are test-only (lines {lo}-{hi}), kept alive by {}", fd.pub_items, basename(&f.path), name_files(&kept, &f.path, f.lang)));
        }
        let mut shown = 0;
        let mut test_only_shown = 0;
        let mut more = 0;
        for s in &listed {
            if s.category == Category::TestOnly && (all_test_only || test_only_shown >= cfg.test_only.max_reported_per_file) {
                if !all_test_only { more += 1 }
                continue;
            }
            if shown >= sym.max_reported_per_file {
                more += 1;
                continue;
            }
            shown += 1;
            let loc = format!("({} {})", basename(&f.path), range(s.start_line, s.end_line));
            let is_fn = kind_class(s.kind) == "fn";
            reasons.push(match s.category {
                Category::Dead => {
                    let docs = if s.doc_mentions > 0 { format!(" ({} doc mentions)", s.doc_mentions) } else { String::new() };
                    format!("{} {loc} is {} nowhere in {n_files} files{docs}", s.display, if is_fn { "called" } else { "referenced" })
                }
                Category::TestOnly => {
                    test_only_shown += 1;
                    let files: Vec<&str> = s.test_files.iter().map(String::as_str).collect();
                    let uses = s.own_test_refs + s.external_test_refs;
                    let inline_only = files.iter().all(|p| *p == f.path);
                    let advice = match (f.lang, inline_only) {
                        (Language::Rust, true) => "move under #[cfg(test)] or delete",
                        (Language::Rust, false) => "move into the test crate or delete",
                        _ => "move next to the tests or delete",
                    };
                    format!("{} {loc} has no production {}: {uses} uses, all in {}; {advice}", s.display, if is_fn { "caller" } else { "use" }, name_files(&files, &f.path, f.lang))
                }
                _ => format!("{} {loc} shares its name with {} other definitions; reachability not assessed", s.display, s.samename.saturating_sub(1)),
            });
        }
        if more > 0 {
            reasons.push(format!("(+{more} more dead or test-only symbols in dead_symbols)"));
        }
        fd.reasons = reasons;
        per_file.push((fi, fd));
    }

    // The in-file-only line: gated on count and share, ranked against every checked file.
    let shares: Vec<f64> = per_file.iter().map(|(_, fd)| fd.own_file_only_share).collect();
    let pct = crate::report::percentiles(&shares);
    for (i, (fi, fd)) in per_file.iter_mut().enumerate() {
        let f = &index.files[*fi];
        if fd.pub_items >= sym.overexported_min_items && fd.own_file_only_share >= sym.overexported_min_share && family(f.lang) != "python" {
            let mut over: Vec<&SymbolReport> = fd.symbols.iter().filter(|s| s.category == Category::Overexported).collect();
            over.sort_by_key(|s| s.start_line);
            let named: Vec<String> = over.iter().take(sym.max_reported_per_file).map(|s| format!("{} ({})", s.name, if s.start_line == s.end_line { format!("line {}", s.start_line) } else { format!("lines {}-{}", s.start_line, s.end_line) })).collect();
            let more = match over.len().saturating_sub(named.len()) { 0 => String::new(), n => format!(" and {n} more") };
            fd.reasons.push(format!(
                "{} of {} pub items are referenced only inside this file ({:.0}%, {} percentile): {}{more}",
                fd.overexported_count, fd.pub_items, fd.own_file_only_share * 100.0, ordinal(pct[i]), named.join(", ")
            ));
        }
    }

    if cfg.shapes.enabled && cfg.shapes.languages.iter().any(|l| l == "rust") {
        shapes(index, cfg, &mut per_file);
    }

    for (fi, fd) in per_file {
        let t = &mut report.totals;
        t.pub_items += fd.pub_items;
        t.dead += fd.dead_count;
        t.test_only += fd.test_only_count;
        t.overexported += fd.overexported_count;
        t.ambiguous += fd.ambiguous_count;
        t.shapes += fd.shapes.len();
        if fd.pub_items > 0 || !fd.shapes.is_empty() {
            report.files.insert(index.files[fi].path.clone(), fd);
        }
    }
    let t = &mut report.totals;
    t.exempt_items = exempt;
    t.zero_external_share = if t.pub_items == 0 { 0.0 } else { (t.dead + t.test_only + t.overexported + t.ambiguous) as f64 / t.pub_items as f64 };
    if ts_unchecked > 0 {
        report.notes.push(format!("typescript: {ts_unchecked} exported symbols not checked; run knip, or set [dead.symbols] mode = \"application\" and add \"typescript\" to languages"));
    }
    if py_unchecked > 0 {
        report.notes.push(format!("python: {py_unchecked} public definitions not checked; add \"python\" to [dead.symbols] languages"));
    }
    report
}

/// `tests/git_real.rs, tests/lock.rs`; the file's own inline tests are named as such.
fn name_files(files: &[&str], own: &str, lang: Language) -> String {
    let mut out: Vec<String> = Vec::new();
    for f in files {
        if *f == own {
            out.push(if lang == Language::Rust { "its own #[cfg(test)] tests".to_string() } else { "its own tests".to_string() });
        } else {
            out.push(f.to_string());
        }
    }
    out.join(", ")
}

/// Never-read fields and never-constructed variants (Rust), appended as reasons on the
/// defining file. Members are merged by bare name across types, so a same-named member
/// elsewhere can only hide a hit.
fn shapes(index: &SymbolIndex, cfg: &Cfg, per_file: &mut [(usize, FileDead)]) {
    let sh = &cfg.shapes;
    let mut reads: HashMap<&str, u32> = HashMap::new();
    let mut writes: HashMap<&str, Vec<(usize, usize)>> = HashMap::new(); // (file idx, line)
    let mut ctors: HashMap<&str, u32> = HashMap::new();
    let mut matches: HashMap<&str, Vec<(usize, usize)>> = HashMap::new();
    let mut impls: HashSet<(&str, &str)> = HashSet::new();
    let mut wildcards: HashMap<&str, Vec<usize>> = HashMap::new(); // enum name -> files importing its variants bare
    for (fi, f) in index.files.iter().enumerate() {
        let s = &f.shapes;
        for (n, c) in &s.field_reads { *reads.entry(n).or_default() += c }
        for (n, ls) in &s.field_writes { writes.entry(n).or_default().extend(ls.iter().map(|l| (fi, *l))) }
        for (n, c) in &s.variant_ctors { *ctors.entry(n).or_default() += c }
        for (n, ls) in &s.variant_matches { matches.entry(n).or_default().extend(ls.iter().map(|l| (fi, *l))) }
        for (t, tr) in &s.trait_impls { impls.insert((t, tr)); }
        for w in &s.wildcard_uses { wildcards.entry(w).or_default().push(fi) }
    }
    let site = |(fi, l): &(usize, usize)| format!("{} {l}", basename(&index.files[*fi].path));
    for (fi, fd) in per_file.iter_mut() {
        let f = &index.files[*fi];
        if f.lang != Language::Rust {
            continue;
        }
        let candidate = |t: &TypeDef| {
            !t.in_test
                && t.derives.iter().all(|d| sh.allow_derives.contains(d) || sh.serializing_derives.contains(d))
                && !t.attrs.iter().any(|a| sh.skip_attrs.contains(a))
                && (!sh.require_type_reachable || index.external_refs(&t.name, &f.path).prod > 0)
        };
        let mut out: Vec<Shape> = Vec::new();
        for m in &f.shapes.fields {
            let t = &f.shapes.types[m.type_idx];
            if !candidate(t) || m.name.starts_with('_') || t.derives.iter().any(|d| sh.serializing_derives.contains(d)) {
                continue;
            }
            if reads.get(m.name.as_str()).copied().unwrap_or(0) > 0 {
                continue;
            }
            let mut sites: Vec<(usize, usize)> = writes.get(m.name.as_str()).cloned().unwrap_or_default();
            sites.sort_unstable();
            let sites: Vec<String> = sites.iter().map(site).collect();
            let reason = if sites.is_empty() {
                format!("field {}.{} ({} {}) is never written or read", t.name, m.name, basename(&f.path), m.line)
            } else {
                let listed: Vec<&str> = sites.iter().take(3).map(String::as_str).collect();
                let ell = if sites.len() > 3 { ", ..." } else { "" };
                format!("field {}.{} ({} {}) is written at {} site{} ({}{ell}) and never read", t.name, m.name, basename(&f.path), m.line, sites.len(), if sites.len() == 1 { "" } else { "s" }, listed.join(", "))
            };
            out.push(Shape { kind: ShapeKind::Field, owner: t.name.clone(), name: m.name.clone(), line: m.line, sites, label: "never read", reason });
        }
        for m in &f.shapes.variants {
            let t = &f.shapes.types[m.type_idx];
            if !candidate(t) || t.derives.iter().any(|d| d == "Default") || sh.exempt_impl_traits.iter().any(|tr| impls.contains(&(t.name.as_str(), tr.as_str()))) {
                continue;
            }
            let mut n = ctors.get(m.name.as_str()).copied().unwrap_or(0);
            // A wildcard import of the enum makes every bare mention of the variant a construction.
            if let Some(files) = wildcards.get(t.name.as_str()) {
                n += files.iter().map(|&wf| index.files[wf].refs.get(&m.name).map_or(0, RefCount::total)).sum::<u32>();
            }
            if n > 0 {
                continue;
            }
            let mut sites: Vec<(usize, usize)> = matches.get(m.name.as_str()).cloned().unwrap_or_default();
            sites.sort_unstable();
            let sites: Vec<String> = sites.iter().map(site).collect();
            let non_exhaustive = t.attrs.iter().any(|a| a == "non_exhaustive");
            let loc = format!("({} {})", basename(&f.path), m.line);
            if sites.is_empty() {
                if non_exhaustive {
                    continue;
                }
                let reason = format!("variant {}::{} {loc} is never constructed or matched", t.name, m.name);
                out.push(Shape { kind: ShapeKind::Variant, owner: t.name.clone(), name: m.name.clone(), line: m.line, sites, label: "never constructed or matched", reason });
            } else {
                if !sh.report_handled_never_produced || (non_exhaustive && sh.non_exhaustive_hides_handled) {
                    continue;
                }
                let listed: Vec<&str> = sites.iter().take(3).map(String::as_str).collect();
                let ell = if sites.len() > 3 { ", ..." } else { "" };
                let reason = format!("variant {}::{} {loc} is matched at {}{ell} but never constructed: a state nothing produces", t.name, m.name, listed.join(", "));
                out.push(Shape { kind: ShapeKind::Variant, owner: t.name.clone(), name: m.name.clone(), line: m.line, sites, label: "never constructed", reason });
            }
        }
        out.sort_by_key(|s| s.line);
        for s in out.iter().take(sh.max_reported_per_file) {
            fd.reasons.push(s.reason.clone());
        }
        if out.len() > sh.max_reported_per_file {
            fd.reasons.push(format!("(+{} more dead shapes in dead_shapes)", out.len() - sh.max_reported_per_file));
        }
        fd.shapes = out;
    }
}

/// The `scry dead` text output.
pub fn render(r: &DeadReport, top: usize) -> String {
    use std::fmt::Write;
    let mut o = String::new();
    for m in &r.modes {
        let _ = writeln!(o, "mode  {m}");
    }
    let _ = writeln!(o, "{}", totals_line(r));
    for n in &r.notes {
        let _ = writeln!(o, "note  {n}");
    }
    let _ = writeln!(o, "\ndead and test-only symbols (longest first):");
    let mut rows: Vec<(&String, &SymbolReport)> = r.files.iter().flat_map(|(p, fd)| fd.symbols.iter().filter(|s| matches!(s.category, Category::Dead | Category::TestOnly | Category::Ambiguous)).map(move |s| (p, s))).collect();
    rows.sort_by_key(|(p, s)| (s.category != Category::Dead, s.category != Category::TestOnly, std::cmp::Reverse(s.end_line - s.start_line), (*p).clone(), s.start_line));
    if rows.is_empty() {
        let _ = writeln!(o, "  none");
    }
    for (p, s) in rows.iter().take(top) {
        let cat = match s.category { Category::Dead => "DEAD", Category::TestOnly => "TESTONLY", _ => "AMBIGUOUS" };
        let _ = writeln!(o, "  {cat:<9} {p}:{}-{}  {}  ext {}/{} own {}/{} (prod/test), {} doc mentions", s.start_line, s.end_line, s.display, s.external_prod_refs, s.external_test_refs, s.own_prod_refs, s.own_test_refs, s.doc_mentions);
    }
    let _ = writeln!(o, "\ndead shapes:");
    let shapes: Vec<&Shape> = r.files.values().flat_map(|fd| fd.shapes.iter()).collect();
    if shapes.is_empty() {
        let _ = writeln!(o, "  none");
    }
    for s in shapes.iter().take(top) {
        let _ = writeln!(o, "  {}", s.reason);
    }
    let _ = writeln!(o, "\nfiles by dead lines (dead / test-only / in-file-only of pub items):");
    let _ = writeln!(o, "  {:>5} {:>5}  {:>14}  path", "lines", "ratio", "d/t/i of items");
    for (p, fd) in top_files(r, top) {
        let _ = writeln!(o, "  {:>5} {:>4.0}%  {:>14}  {p}", fd.dead_lines, fd.dead_ratio * 100.0, format!("{}/{}/{} of {}", fd.dead_count, fd.test_only_count, fd.overexported_count, fd.pub_items));
    }
    o
}

/// `754 pub items in 81 files: 3 dead, 8 test-only, 154 referenced only in-file (23% with no external production use); 4 dead shapes`.
pub fn totals_line(r: &DeadReport) -> String {
    let t = &r.totals;
    let amb = if t.ambiguous > 0 { format!(", {} ambiguous", t.ambiguous) } else { String::new() };
    let ex = if t.exempt_items > 0 { format!(" ({} more exempt: library API, entrypoints, unchecked languages)", t.exempt_items) } else { String::new() };
    format!(
        "{} pub items checked in {} files{ex}: {} dead, {} test-only, {} referenced only in-file{amb} ({:.0}% with no external production use); {} dead shape{}",
        t.pub_items, t.files_indexed, t.dead, t.test_only, t.overexported, t.zero_external_share * 100.0, t.shapes, if t.shapes == 1 { "" } else { "s" }
    )
}

/// Files with dead lines, most first (then by path).
pub fn top_files(r: &DeadReport, top: usize) -> Vec<(&String, &FileDead)> {
    let mut rows: Vec<(&String, &FileDead)> = r.files.iter().filter(|(_, fd)| fd.dead_lines > 0).collect();
    rows.sort_by(|a, b| b.1.dead_lines.cmp(&a.1.dead_lines).then_with(|| a.0.cmp(b.0)));
    rows.truncate(top);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::percentiles;

    fn file(path: &str, kind: FileKind, content: &str) -> SourceFile {
        let lang = Language::from_path(std::path::Path::new(path)).unwrap();
        SourceFile { path: path.into(), lang, kind, lines: content.lines().count(), bytes: content.len(), content: content.into() }
    }

    fn src(path: &str, content: &str) -> SourceFile {
        file(path, FileKind::Source, content)
    }

    /// No manifest on disk: `auto` resolves to application.
    fn run(files: &[SourceFile], cfg: &Cfg) -> DeadReport {
        analyze(&index_all(files, cfg), Path::new("/nonexistent/scry-dead-tests"), cfg)
    }

    fn cat(r: &DeadReport, path: &str, name: &str) -> Option<Category> {
        r.files.get(path).and_then(|f| f.symbols.iter().find(|s| s.name == name)).map(|s| s.category)
    }

    fn sym<'a>(r: &'a DeadReport, path: &str, name: &str) -> &'a SymbolReport {
        r.files[path].symbols.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("{path} {name}: {r:?}"))
    }

    #[test]
    fn own_name_is_not_a_ref_and_the_index_covers_source_and_test_files_only() {
        let files = [
            src("src/a.rs", "pub fn lonely_fn() {}\npub fn used_fn() {}\n"),
            file("tests/t.rs", FileKind::Test, "#[test] fn t() { used_fn(); }\n"),
            file("fixtures/d.rs", FileKind::Data, "fn x() { lonely_fn(); }\n"),
        ];
        let idx = index_all(&files, &Cfg::default());
        assert_eq!(idx.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), vec!["src/a.rs", "tests/t.rs"]);
        assert_eq!(idx.refs_in("lonely_fn", "src/a.rs"), RefCount::default());
        assert_eq!(idx.external_refs("used_fn", "src/a.rs"), RefCount { prod: 0, test: 1 });
        assert_eq!(idx.samename("used_fn"), 1);
        assert!(idx.file("tests/t.rs").unwrap().test_context);
        assert_eq!(idx.definitions("lonely_fn").map(|d| d.display.as_str()).collect::<Vec<_>>(), vec!["pub fn lonely_fn"]);
        let mut cfg = Cfg::default();
        cfg.symbols.min_lines = 1;
        let r = run(&files, &cfg);
        assert_eq!(cat(&r, "src/a.rs", "lonely_fn"), Some(Category::Dead));
        // One test ref is under the floor of 2: neither dead nor test-only.
        assert_eq!(cat(&r, "src/a.rs", "used_fn"), Some(Category::Reachable));
        assert_eq!(r.files["src/a.rs"].reasons, vec!["pub fn lonely_fn (a.rs 1) is called nowhere in 2 files"]);
        assert_eq!((r.totals.pub_items, r.totals.dead, r.totals.exempt_items), (2, 1, 0));
    }

    #[test]
    fn token_tree_refs_follow_their_context_and_doc_mentions_never_reach() {
        let a = "\
/// Calls `helper_one` (mentioned twice: helper_one).
pub fn helper_one() {
}
pub fn helper_two() {
}
pub fn helper_three() {
}
fn caller() { m!(helper_two); }
#[cfg(test)]
mod tests {
    #[test] fn t() { assert!(m!(helper_three)); assert!(m!(helper_three)); }
}
";
        let r = run(&[src("src/a.rs", a)], &Cfg::default());
        let one = sym(&r, "src/a.rs", "helper_one");
        assert_eq!((one.category, one.doc_mentions), (Category::Dead, 2));
        assert_eq!(cat(&r, "src/a.rs", "helper_two"), Some(Category::Overexported));
        let three = sym(&r, "src/a.rs", "helper_three");
        assert_eq!((three.category, three.own_test_refs, three.own_prod_refs), (Category::TestOnly, 2, 0));
        let reasons = &r.files["src/a.rs"].reasons;
        assert_eq!(reasons[0], "pub fn helper_one (a.rs 2-3) is called nowhere in 1 files (2 doc mentions)");
        assert_eq!(reasons[1], "pub fn helper_three (a.rs 6-7) has no production caller: 2 uses, all in its own #[cfg(test)] tests; move under #[cfg(test)] or delete");
    }

    #[test]
    fn test_context_is_test_files_extra_globs_cfg_test_and_test_fns() {
        let files = [
            src("src/a.rs", "pub fn seam_a() {}\npub fn seam_b() {}\npub fn seam_c() {}\npub fn seam_d() {}\npub fn live_fn() {}\n#[test]\nfn bare() { seam_c(); seam_c(); }\n"),
            file("tests/t.rs", FileKind::Test, "fn t() { seam_a(); seam_a(); }\n"),
            src("src/testutil.rs", "pub fn util() { seam_b(); seam_b(); }\n"),
            src("benches/b.rs", "fn main() { seam_d(); seam_d(); }\n"),
            src("src/c.rs", "fn go() { live_fn(); }\n"),
        ];
        let mut cfg = Cfg::default();
        cfg.symbols.min_lines = 1;
        let r = run(&files, &cfg);
        for n in ["seam_a", "seam_b", "seam_c", "seam_d"] {
            assert_eq!(cat(&r, "src/a.rs", n), Some(Category::TestOnly), "{n}");
        }
        // testutil.rs is test context: its own pub fn is never a candidate.
        assert!(!r.files.contains_key("src/testutil.rs"));
        assert_eq!(sym(&r, "src/a.rs", "seam_a").test_files, vec!["tests/t.rs"]);
        let reasons = &r.files["src/a.rs"].reasons;
        assert_eq!(reasons[0], "pub fn seam_a (a.rs 1) has no production caller: 2 uses, all in tests/t.rs; move into the test crate or delete", "{reasons:?}");
        assert_eq!(reasons[2], "pub fn seam_c (a.rs 3) has no production caller: 2 uses, all in its own #[cfg(test)] tests; move under #[cfg(test)] or delete");
        assert_eq!(reasons.len(), 4);
        // Every exported symbol test-only: one rollup line, no per-symbol lines.
        let two = [src("src/a.rs", "pub fn seam_a() {}\npub fn seam_b() {}\n"), file("tests/t.rs", FileKind::Test, "fn t() { seam_a(); seam_a(); seam_b(); seam_b(); }\n")];
        let r = run(&two, &cfg);
        assert_eq!(r.files["src/a.rs"].reasons, vec!["all 2 exported symbols in a.rs are test-only (lines 1-2), kept alive by tests/t.rs"]);
        // Benches as production: seam_d is used.
        cfg.test_only.treat_benches_as_prod = true;
        let r = run(&files, &cfg);
        assert_eq!(cat(&r, "src/a.rs", "seam_d"), Some(Category::Reachable));
    }

    #[test]
    fn exempt_items_still_contribute_refs() {
        let a = "\
pub trait Render { fn render(&self); }
pub struct Widget_x;
impl Render for Widget_x { fn render(&self) { hidden_fn(); } }
#[test]
pub fn test_it() {
}
#[tokio::test]
pub async fn async_it() {
}
pub fn hidden_fn() {
}
pub fn _private_ish() {
}
pub fn one_liner() {}
pub mod inner_mod {
}
";
        let files = [src("src/a.rs", a), src("src/main.rs", "pub fn main_only() {\n}\nfn main() { main_only(); Widget_x; }\n")];
        let r = run(&files, &Cfg::default());
        let names: Vec<&str> = r.files["src/a.rs"].symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["hidden_fn"], "{names:?}"); // Render and Widget_x are one-liners
        // The trait-impl method's body references hidden_fn in production.
        assert_eq!(cat(&r, "src/a.rs", "hidden_fn"), Some(Category::Overexported));
        assert!(!r.files.contains_key("src/main.rs"));
        // Render, Widget_x, async_it, _private_ish, one_liner, inner_mod, main_only (test_it is in a #[test] region).
        assert_eq!(r.totals.exempt_items, 7, "{:?}", r.totals);
        let mut one = Cfg::default();
        one.symbols.min_lines = 1;
        let r = run(&files, &one);
        assert_eq!(cat(&r, "src/a.rs", "Widget_x"), Some(Category::Reachable));
        assert_eq!(cat(&r, "src/a.rs", "Render"), Some(Category::Overexported));
        let idx = index_all(&files, &Cfg::default());
        let defs: Vec<(&str, Option<&str>)> = idx.definitions("render").map(|d| (d.display.as_str(), d.exempt.as_deref())).collect();
        assert_eq!(defs, vec![("fn Render::render", None), ("fn Widget_x::render", Some("trait impl"))]);
        assert_eq!(idx.definitions("async_it").next().unwrap().exempt.as_deref(), Some("#[tokio::test]"));
        let mut cfg = Cfg::default();
        cfg.symbols.kinds = vec!["mod".into()];
        let r = run(&files, &cfg);
        assert_eq!(r.files["src/a.rs"].symbols.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), vec!["inner_mod"]);
    }

    #[test]
    fn mode_auto_reads_the_manifest_and_library_checks_only_restricted_visibility() {
        let dir = std::env::temp_dir().join(format!("scry-dead-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n[lib]\npath = \"src/lib.rs\"\n").unwrap();
        let a = "pub fn api_fn() {\n}\npub(crate) fn crate_fn() {\n}\n";
        let files = [src("src/a.rs", a), src("src/lib.rs", "pub mod a;\npub fn root_fn() {\n}\n")];
        let cfg = Cfg::default();
        let r = analyze(&index_all(&files, &cfg), &dir, &cfg);
        assert_eq!(r.modes, vec![".: library (Cargo.toml has [lib] and no [[bin]])"]);
        assert_eq!(r.files["src/a.rs"].symbols.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), vec!["crate_fn"]);
        assert_eq!(cat(&r, "src/a.rs", "crate_fn"), Some(Category::Dead));
        assert!(!r.files.contains_key("src/lib.rs"), "api roots are exempt in library mode");
        // A [[bin]] makes it an application: plain pub is checked, lib.rs too, main.rs never.
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n[lib]\npath = \"src/lib.rs\"\n[[bin]]\nname = \"x\"\npath = \"src/main.rs\"\n").unwrap();
        let r = analyze(&index_all(&files, &cfg), &dir, &cfg);
        assert_eq!(r.modes, vec![".: application (Cargo.toml has [[bin]])"]);
        assert_eq!(cat(&r, "src/a.rs", "api_fn"), Some(Category::Dead));
        assert_eq!(cat(&r, "src/lib.rs", "root_fn"), Some(Category::Dead));
        // The knob overrides the manifest.
        let mut lib = Cfg::default();
        lib.symbols.mode = DeadMode::Library;
        let r = analyze(&index_all(&files, &lib), &dir, &lib);
        assert_eq!(cat(&r, "src/a.rs", "api_fn"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn duplicated_names_merge_refs_and_never_create_a_hit() {
        let files = [
            src("src/a.rs", "pub fn parse_it() {\n}\n"),
            src("src/b.rs", "pub fn parse_it() {\n}\nfn go() { parse_it(); }\n"),
            src("src/c.rs", "pub fn both_dead() {\n}\n"),
            src("src/d.rs", "pub fn both_dead() {\n}\n"),
        ];
        let r = run(&files, &Cfg::default());
        // a.rs's parse_it is reachable through b.rs's call (the merge hides it).
        assert_eq!(cat(&r, "src/a.rs", "parse_it"), Some(Category::Reachable));
        assert_eq!(cat(&r, "src/b.rs", "parse_it"), Some(Category::Overexported));
        assert_eq!(cat(&r, "src/c.rs", "both_dead"), Some(Category::Ambiguous));
        assert_eq!(r.files["src/c.rs"].reasons, vec!["pub fn both_dead (c.rs 1-2) shares its name with 1 other definitions; reachability not assessed"]);
        assert_eq!((r.totals.dead, r.totals.ambiguous), (0, 2));
    }

    #[test]
    fn strings_reference_symbols_by_segment_and_format_capture() {
        let a = "\
pub const BIN_ENV: &str =
    \"KANSPEC_BIN\";
pub const KEY_ONE: &str =
    \"k\";
pub fn install_it() {
}
pub fn by_path() {
}
pub fn by_whole() {
}
pub fn by_colon() {
}
fn go() {
    let _ = format!(\"{BIN_ENV} {{not_a_ref}} {KEY_ONE:?}\");
    call(\"hooks.install_it\");
    call(\"by_whole\");
    call(\"pkg.mod:by_colon\");
    call(\"by path with spaces\");
}
";
        let r = run(&[src("src/a.rs", a)], &Cfg::default());
        for n in ["BIN_ENV", "KEY_ONE", "install_it", "by_whole", "by_colon"] {
            assert_eq!(cat(&r, "src/a.rs", n), Some(Category::Overexported), "{n}");
        }
        assert_eq!(cat(&r, "src/a.rs", "by_path"), Some(Category::Dead));
        let mut off = Cfg::default();
        off.symbols.string_refs = false;
        let r = run(&[src("src/a.rs", a)], &off);
        assert_eq!(cat(&r, "src/a.rs", "BIN_ENV"), Some(Category::Dead));
        assert_eq!(cat(&r, "src/a.rs", "install_it"), Some(Category::Dead));
    }

    #[test]
    fn typescript_and_python_are_off_by_default_and_python_never_gets_the_in_file_line() {
        let files = [
            src("src/a.ts", "import { x } from './x';\nexport function dead_ts() {\n}\nexport function used_ts() {\n}\nfunction go() { used_ts(); }\ndescribe('s', () => { it('t', () => { dead_ts(); dead_ts(); }); });\n"),
            src("pkg/m.py", "def dead_py():\n    pass\ndef a_py():\n    pass\ndef b_py():\n    pass\ndef c_py():\n    pass\ndef d_py():\n    pass\ndef go():\n    a_py(); b_py(); c_py(); d_py()\ndef test_x():\n    dead_py(); dead_py()\n"),
        ];
        let r = run(&files, &Cfg::default());
        assert!(r.files.is_empty(), "{:?}", r.files);
        assert_eq!(r.notes.len(), 2, "{:?}", r.notes);
        assert!(r.notes[0].starts_with("typescript: 2 exported symbols not checked; run knip"), "{:?}", r.notes);
        // Application mode with the languages on: TS test blocks and Python test_ fns are test context.
        let mut cfg = Cfg::default();
        cfg.symbols.mode = DeadMode::Application;
        cfg.symbols.languages = vec!["rust".into(), "typescript".into(), "python".into()];
        let r = run(&files, &cfg);
        assert_eq!(cat(&r, "src/a.ts", "dead_ts"), Some(Category::TestOnly));
        assert_eq!(cat(&r, "src/a.ts", "used_ts"), Some(Category::Overexported));
        assert_eq!(cat(&r, "pkg/m.py", "dead_py"), Some(Category::TestOnly));
        assert_eq!(cat(&r, "pkg/m.py", "a_py"), Some(Category::Overexported));
        assert!(r.files["pkg/m.py"].reasons.iter().all(|x| !x.contains("referenced only inside")), "{:?}", r.files["pkg/m.py"].reasons);
        assert!(r.notes.is_empty());
        // Library mode (the auto default for a package.json with exports) prints the knip line.
        cfg.symbols.mode = DeadMode::Library;
        let r = run(&files, &cfg);
        assert!(cat(&r, "src/a.ts", "dead_ts").is_none());
        assert!(r.notes[0].contains("run knip"));
    }

    #[test]
    fn strict_test_only_needs_no_own_prod_ref_and_the_floor_counts_own_and_external_tests() {
        let a = "\
pub fn plan_it() {
}
pub fn once_only() {
}
pub fn split_refs() {
}
fn go() { plan_it(); }
#[cfg(test)]
mod tests {
    #[test] fn t() { plan_it(); plan_it(); once_only(); split_refs(); }
}
";
        let files = [src("src/a.rs", a), file("tests/t.rs", FileKind::Test, "fn t() { split_refs(); }\n")];
        let r = run(&files, &Cfg::default());
        let p = sym(&r, "src/a.rs", "plan_it");
        assert_eq!((p.category, p.own_prod_refs, p.own_test_refs), (Category::Overexported, 1, 2));
        assert_eq!(cat(&r, "src/a.rs", "once_only"), Some(Category::Reachable));
        assert_eq!(cat(&r, "src/a.rs", "split_refs"), Some(Category::TestOnly));
        let mut cfg = Cfg::default();
        cfg.test_only.min_external_test_refs = 1;
        let r = run(&files, &cfg);
        assert_eq!(cat(&r, "src/a.rs", "once_only"), Some(Category::TestOnly));
        cfg.test_only.enabled = false;
        let r = run(&files, &cfg);
        assert_eq!(cat(&r, "src/a.rs", "split_refs"), Some(Category::Reachable));
    }

    #[test]
    fn reasons_are_capped_dead_first_longest_first_and_the_in_file_line_is_per_file() {
        let mut a = String::new();
        for (i, n) in ["short_dead", "long_dead", "mid_dead"].iter().enumerate() {
            a.push_str(&format!("pub fn {n}() {{\n{}}}\n", "    //\n".repeat(i * 2 + 1)));
        }
        a.push_str("pub fn only_here_a() {\n}\npub fn only_here_b() {\n}\npub fn only_here_c() {\n}\nfn go() { only_here_a(); only_here_b(); only_here_c(); }\n");
        let files = [src("src/a.rs", &a), src("src/b.rs", "pub fn used_b() {\n}\n"), src("src/c.rs", "fn go() { used_b(); }\n")];
        let mut cfg = Cfg::default();
        cfg.symbols.max_reported_per_file = 2;
        let r = run(&files, &cfg);
        let reasons = &r.files["src/a.rs"].reasons;
        assert_eq!(reasons[0], "pub fn mid_dead (a.rs 9-15) is called nowhere in 3 files", "{reasons:?}");
        assert_eq!(reasons[1], "pub fn long_dead (a.rs 4-8) is called nowhere in 3 files", "{reasons:?}");
        assert_eq!(reasons[2], "(+1 more dead or test-only symbols in dead_symbols)");
        assert_eq!(reasons[3], "3 of 6 pub items are referenced only inside this file (50%, 100th percentile): only_here_a (lines 16-17), only_here_b (lines 18-19) and 1 more");
        assert_eq!(reasons.len(), 4);
        assert_eq!(r.files["src/a.rs"].own_file_only_share, 0.5);
        assert_eq!(r.files["src/a.rs"].dead_lines, 15 + 6);
        assert!(r.files["src/a.rs"].dead_ratio > 0.9);
        assert!(r.files["src/b.rs"].reasons.is_empty());
        cfg.symbols.overexported_min_items = 7;
        let r = run(&files, &cfg);
        assert!(r.files["src/a.rs"].reasons.iter().all(|x| !x.contains("referenced only inside")));
        assert_eq!(percentiles(&[0.5, 0.0]), vec![1.0, 0.0]);
    }

    #[test]
    fn shapes_never_read_fields_and_never_constructed_variants() {
        let a = "\
#[derive(Debug, Clone)]
pub struct Ticket {
    pub mtime: u8,
    pub read_me: u8,
    pub _hidden: u8,
    pub in_macro: u8,
    pub captured: u8,
}
#[derive(Clone, Copy, Debug)]
pub enum Color {
    Red,
    Blue,
    Green,
    Yellow,
    Ghost,
}
#[derive(Debug, Serialize)]
pub struct Json { pub never: u8 }
#[derive(Debug, Serialize)]
pub enum Code { Handled, Raised }
#[derive(Debug, Deserialize)]
pub enum Wire { Never }
#[repr(u8)]
pub enum Repr { Never }
#[non_exhaustive]
pub enum Ext { Unmentioned, Handled }
pub enum Conv { Never }
impl From<u8> for Conv { fn from(_: u8) -> Self { Conv::Never } }
pub fn paint(c: Color) -> u8 {
    match c { Color::Red => 1, Color::Blue => 2, Color::Green | Color::Yellow => 3, Color::Ghost => 4 }
}
impl Color { fn mk() -> Self { Self::Green } }
pub fn go(t: &mut Ticket, c: Code, e: Ext) {
    t.mtime = 1;
    t.mtime += 1;
    let Ticket { read_me, .. } = *t;
    let _ = t.read_me;
    m!(t.in_macro);
    let captured = 1; let _ = format!(\"{captured}\");
    let _ = Ticket { mtime: 1, read_me: 2, _hidden: 3, in_macro: 4, captured: 5 };
    let _ = Json { never: 1 };
    let _ = Code::Raised;
    match c { Code::Handled => {}, Code::Raised => {} }
    match e { Ext::Handled => {}, _ => {} }
}
";
        let b = "use crate::a::Color::*;\nfn f() -> Color { Yellow }\npub fn touch(_: &Ticket, _: &Color, _: &Json, _: &Code, _: &Wire, _: &Repr, _: &Ext, _: &Conv) { let _ = Wire::Never; }\n";
        let files = [src("src/a.rs", a), src("src/b.rs", b)];
        let r = run(&files, &Cfg::default());
        let shapes: Vec<String> = r.files["src/a.rs"].shapes.iter().map(|s| s.reason.clone()).collect();
        // Red is constructed nowhere either; Green through `Self::Green`, Yellow bare under the
        // wildcard import in b.rs. `Handled` merges Code's and Ext's match sites (same bare name).
        assert_eq!(shapes, vec![
            "field Ticket.mtime (a.rs 3) is written at 3 sites (a.rs 34, a.rs 35, a.rs 40) and never read",
            "variant Color::Red (a.rs 11) is matched at a.rs 30 but never constructed: a state nothing produces",
            "variant Color::Blue (a.rs 12) is matched at a.rs 30 but never constructed: a state nothing produces",
            "variant Color::Ghost (a.rs 15) is matched at a.rs 30 but never constructed: a state nothing produces",
            "variant Code::Handled (a.rs 20) is matched at a.rs 43, a.rs 44 but never constructed: a state nothing produces",
            "variant Ext::Handled (a.rs 26) is matched at a.rs 43, a.rs 44 but never constructed: a state nothing produces",
        ], "{shapes:?}");
        assert_eq!(r.totals.shapes, 6);
        assert_eq!(r.files["src/a.rs"].reasons.last().unwrap(), "(+1 more dead shapes in dead_shapes)");
        assert!(r.files["src/a.rs"].reasons.iter().any(|x| x.starts_with("field Ticket.mtime")));
        // Knobs: handled-never-produced off, non_exhaustive hiding handled, type reachability.
        let mut cfg = Cfg::default();
        cfg.shapes.report_handled_never_produced = false;
        let r = run(&files, &cfg);
        assert_eq!(r.files["src/a.rs"].shapes.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), vec!["mtime"]);
        let mut cfg = Cfg::default();
        cfg.shapes.non_exhaustive_hides_handled = true;
        let r = run(&files, &cfg);
        assert!(r.files["src/a.rs"].shapes.iter().all(|s| s.owner != "Ext"));
        let r = run(&files[..1], &Cfg::default());
        assert!(r.files.get("src/a.rs").is_none_or(|f| f.shapes.is_empty()), "a type nothing outside its file references has no shape findings");
        let mut cfg = Cfg::default();
        cfg.shapes.require_type_reachable = false;
        let r = run(&files[..1], &cfg);
        // Without b.rs the wildcard import is gone: Yellow is only matched.
        assert!(r.files["src/a.rs"].shapes.iter().any(|s| s.name == "Yellow"), "{:?}", r.files["src/a.rs"].shapes);
        cfg.shapes.enabled = false;
        assert!(run(&files, &cfg).files["src/a.rs"].shapes.is_empty());
    }

    #[test]
    fn same_named_members_merge_and_only_hide() {
        let a = "#[derive(Debug)]\npub struct A { pub shared: u8 }\n#[derive(Debug)]\npub struct B { pub shared: u8 }\nfn go(b: &B) { let _ = b.shared; let _ = A { shared: 1 }; }\n";
        let b = "pub fn t(_: &A, _: &B) {}\n";
        let r = run(&[src("src/a.rs", a), src("src/b.rs", b)], &Cfg::default());
        assert!(r.files.get("src/a.rs").is_none_or(|f| f.shapes.is_empty()));
    }
}
