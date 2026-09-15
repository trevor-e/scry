//! Re-implemented helpers: the same small utility defined in several files, and helper bodies
//! inlined where the helper should have been called.
//!
//! Each LLM session writes the utility it needs without grepping for it, so `plural`, `io_err`
//! and `render` end up byte-identical in two or three files, each from a different session, and
//! a 12-token helper body is re-derived in place instead of called. Names group top-level
//! definitions across files; a helper-specific normalizer (only bound names anonymized; callee,
//! field, macro and path names and every literal kept) makes bodies comparable by 5-gram Jaccard
//! and searchable as exact token sequences in every other Source file's stream.

use crate::config::Helpers as Cfg;
use crate::dead::{self, SymbolIndex, Visibility};
use crate::deps::DepGraph;
use crate::discover::{FileKind, SourceFile};
use crate::lang::Language;
use crate::regions::{self, TestRegion};
use globset::{Glob, GlobSet, GlobSetBuilder};
use rayon::prelude::*;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::process::Command;
use tree_sitter::Node;

// ---------- tokens ----------

/// What a normalized token is, for the candidate filters (`min_distinct_kinds`, one keyword or
/// operator, one kept name or literal).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Class {
    /// A bound name (parameter, `let` / `const` / `for` / closure binding), anonymized.
    Id,
    /// A kept name: callee, field, method, macro, path segment, type, free variable.
    Name,
    Num,
    Str,
    /// Keywords and punctuation.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Tok {
    hash: u64,
    class: Class,
}

/// One token of a file's stream, with where it came from.
#[derive(Debug, Clone, Copy)]
struct STok {
    tok: Tok,
    line: u32,
}

fn hash_str(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn id_tok() -> Tok {
    Tok { hash: hash_str("\u{0}ID"), class: Class::Id }
}

fn text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    node.utf8_text(src).unwrap_or("")
}

const RUST_KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
    "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while", "dyn", "async", "await",
];

/// Node kinds emitted as one literal token (the node's whole text), never descended.
fn literal_class(kind: &str, lang: Language) -> Option<Class> {
    match lang {
        Language::Rust => match kind {
            "string_literal" | "raw_string_literal" | "char_literal" => Some(Class::Str),
            "integer_literal" | "float_literal" => Some(Class::Num),
            _ => None,
        },
        Language::Python => match kind {
            "string" | "concatenated_string" => Some(Class::Str),
            "integer" | "float" => Some(Class::Num),
            _ => None,
        },
        _ => match kind {
            "string" | "template_string" => Some(Class::Str),
            "number" => Some(Class::Num),
            _ => None,
        },
    }
}

fn is_comment(kind: &str) -> bool {
    matches!(kind, "comment" | "line_comment" | "block_comment")
}

/// Function-like nodes: the scope a bound-name set is collected for.
fn is_unit(kind: &str, lang: Language) -> bool {
    match lang {
        Language::Rust => matches!(kind, "function_item" | "closure_expression"),
        Language::Python => matches!(kind, "function_definition" | "lambda"),
        _ => matches!(kind, "function_declaration" | "generator_function_declaration" | "arrow_function" | "function_expression" | "function" | "generator_function" | "method_definition"),
    }
}

/// Identifier-like leaves that can be a bound name.
fn is_bindable(kind: &str) -> bool {
    matches!(kind, "identifier" | "shorthand_field_identifier" | "shorthand_property_identifier" | "shorthand_property_identifier_pattern")
}

/// Identifier-like leaves that are kept names when not bound.
fn is_name(kind: &str) -> bool {
    kind.contains("identifier") || matches!(kind, "primitive_type" | "predefined_type" | "metavariable")
}

/// The names bound inside one function-like node: parameters, `let` / `const` / `var` patterns,
/// closure and lambda parameters, `for` targets, match-arm and `except … as` bindings, nested
/// closures included. Type positions and path segments inside patterns are not bindings.
fn bound_names<'a>(unit: Node<'a>, src: &'a [u8], lang: Language) -> HashSet<&'a str> {
    let mut out = HashSet::new();
    // (node, the node lies in a binding position)
    let mut stack: Vec<(Node<'a>, bool)> = vec![(unit, false)];
    while let Some((node, in_pat)) = stack.pop() {
        let kind = node.kind();
        if node.child_count() == 0 {
            if in_pat && is_bindable(kind) {
                out.insert(text(node, src));
            }
            continue;
        }
        if in_pat && (kind.starts_with("scoped_") || kind == "type_annotation" || kind.ends_with("_type")) {
            continue;
        }
        let mut c = node.walk();
        for (i, ch) in node.children(&mut c).enumerate() {
            let field = node.field_name_for_child(i as u32);
            let child_pat = in_pat
                || match lang {
                    Language::Rust => match kind {
                        "parameter" | "let_declaration" | "let_condition" | "for_expression" | "match_arm" => field == Some("pattern"),
                        "closure_parameters" => true,
                        "tuple_struct_pattern" | "struct_pattern" => false,
                        _ => false,
                    },
                    Language::Python => match kind {
                        "parameters" | "lambda_parameters" => true,
                        "typed_parameter" | "default_parameter" | "typed_default_parameter" | "list_splat_pattern" | "dictionary_splat_pattern" => field != Some("type") && field != Some("value"),
                        "assignment" | "augmented_assignment" | "for_statement" | "for_in_clause" => field == Some("left"),
                        "named_expression" => field == Some("name"),
                        "as_pattern" => field == Some("alias"),
                        _ => false,
                    },
                    _ => match kind {
                        "formal_parameters" => true,
                        "required_parameter" | "optional_parameter" | "rest_pattern" => field != Some("type") && field != Some("value"),
                        "variable_declarator" => field == Some("name"),
                        "for_in_statement" => field == Some("left"),
                        "catch_clause" => field == Some("parameter"),
                        "arrow_function" => field == Some("parameter"),
                        _ => false,
                    },
                };
            // Inside a Rust pattern the `type` of `Some(x)` / `Point { x }` and the field
            // names of `Point { x: px }` are not bindings; the `pattern` of a field is.
            let child_pat = child_pat
                && !(in_pat && lang == Language::Rust && (field == Some("type") || (kind == "field_pattern" && field == Some("name") && ch.kind() != "shorthand_field_identifier")));
            stack.push((ch, child_pat));
        }
    }
    out
}

/// `{name}` / `{name:` captures of bound names inside a kept literal become `{ID}`, so a format
/// string keeps its text but not the parameter names it interpolates.
fn anonymize_captures(s: &str, bound: &HashSet<&str>) -> String {
    if bound.is_empty() || !s.contains('{') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'{' {
            let start = i + 1;
            let mut j = start;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            if j > start && j < b.len() && (b[j] == b'}' || b[j] == b':') && bound.contains(&s[start..j]) {
                out.push_str("{ID");
                i = j;
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap_or('?');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Emit the helper-normalized tokens of `node`'s subtree with `bound` as the anonymized names.
/// Comments are dropped, literals are one token each, everything else is a leaf. Nested
/// function-like nodes share the enclosing set (it was collected over them).
fn emit<'a>(node: Node<'a>, src: &'a [u8], lang: Language, bound: &HashSet<&str>, skip: &[TestRegion], out: &mut Vec<STok>) {
    let mut stack: Vec<Node<'a>> = vec![node];
    while let Some(n) = stack.pop() {
        let kind = n.kind();
        if is_comment(kind) || (!skip.is_empty() && regions::contains(skip, n.start_byte())) {
            continue;
        }
        let push = |out: &mut Vec<STok>, tok: Tok| {
            out.push(STok { tok, line: n.start_position().row as u32 + 1 });
        };
        if let Some(class) = literal_class(kind, lang) {
            let t = anonymize_captures(text(n, src), bound);
            push(out, Tok { hash: hash_str(&t), class });
            continue;
        }
        if n.child_count() == 0 {
            let t = text(n, src);
            if t.trim().is_empty() {
                continue;
            }
            let tok = if is_bindable(kind) && bound.contains(t) {
                id_tok()
            } else if is_name(kind) {
                // A keyword lexed as an identifier inside a macro `token_tree`.
                let class = if lang == Language::Rust && RUST_KEYWORDS.contains(&t) { Class::Other } else { Class::Name };
                Tok { hash: hash_str(t), class }
            } else {
                Tok { hash: hash_str(t), class: Class::Other }
            };
            push(out, tok);
            continue;
        }
        let mut c = n.walk();
        let children: Vec<Node> = n.children(&mut c).collect();
        for ch in children.into_iter().rev() {
            stack.push(ch);
        }
    }
}

/// A whole file's stream: every outermost function-like node emitted with its own bound-name
/// set, everything else with none. Inline test regions emit one sentinel each, so no sequence
/// can lie in or span one.
fn stream<'a>(root: Node<'a>, src: &'a [u8], lang: Language, regions: &[TestRegion], path: &str) -> Vec<STok> {
    let mut out = Vec::new();
    let empty = HashSet::new();
    let mut stack: Vec<Node<'a>> = vec![root];
    let mut last_region: Option<usize> = None;
    while let Some(n) = stack.pop() {
        let kind = n.kind();
        if is_comment(kind) {
            continue;
        }
        if let Some(i) = regions::index_of(regions, n.start_byte()) {
            if last_region != Some(i) {
                last_region = Some(i);
                let r = &regions[i];
                out.push(STok { tok: Tok { hash: hash_str(&format!("\u{0}region:{path}:{i}")), class: Class::Other }, line: r.start_line as u32 });
            }
            continue;
        }
        if is_unit(kind, lang) {
            let bound = bound_names(n, src, lang);
            emit(n, src, lang, &bound, regions, &mut out);
            continue;
        }
        if literal_class(kind, lang).is_some() || n.child_count() == 0 {
            emit(n, src, lang, &empty, &[], &mut out);
            continue;
        }
        let mut c = n.walk();
        let children: Vec<Node> = n.children(&mut c).collect();
        for ch in children.into_iter().rev() {
            stack.push(ch);
        }
    }
    out
}

// ---------- definitions ----------

/// One top-level helper: a free function, const or static (or an inherent method by knob).
#[derive(Debug, Clone)]
pub struct HelperDef {
    pub name: String,
    /// `rust`, `python`, `typescript`: a family never crosses a language.
    pub lang: &'static str,
    /// `Type::name` for an inherent method, the name otherwise.
    pub qualified: String,
    /// `fn` or `const`.
    pub kind: &'static str,
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,
    pub visibility: Visibility,
    pub in_test: bool,
    pub params: usize,
    pub param_names: Vec<String>,
    /// Return-type text, whitespace-normalized; empty when none.
    pub ret: String,
    /// The first parameter's type text (the sibling-type fan annotation).
    first_param_type: String,
    /// Helper-normalized body tokens with outer braces, a leading `return` and a trailing `;`
    /// stripped; for a const the raw value words when `compare_consts_raw`.
    body: Vec<Tok>,
    /// The body is one `match` / `switch` whose arms all yield a string literal.
    match_over_strings: bool,
    /// `fn plural(`: what `git log -S` looks for.
    needle: String,
}

impl HelperDef {
    /// `plural(n, noun) -> String`.
    pub fn signature(&self) -> String {
        let ret = if self.ret.is_empty() { String::new() } else { format!(" -> {}", self.ret) };
        format!("{}({}){ret}", self.qualified, self.param_names.join(", "))
    }
}

/// One file's side of the pass: its helper definitions and its normalized token stream.
pub struct FileSide {
    pub path: String,
    pub kind: FileKind,
    pub defs: Vec<HelperDef>,
    stream: Vec<STok>,
}

fn norm_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first identifier-like leaf under a parameter node: its name.
fn first_ident(node: Node, src: &[u8]) -> Option<String> {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.child_count() == 0 {
            if is_bindable(n.kind()) || n.kind() == "self" {
                return Some(text(n, src).to_string());
            }
            continue;
        }
        if n.kind().ends_with("_type") || n.kind() == "type_annotation" || n.kind().starts_with("scoped_") {
            continue;
        }
        let mut c = n.walk();
        let children: Vec<Node> = n.children(&mut c).collect();
        for ch in children.into_iter().rev() {
            stack.push(ch);
        }
    }
    None
}

/// Parameter names and count of a `parameters` / `formal_parameters` node.
fn params_of(params: Option<Node>, src: &[u8]) -> (usize, Vec<String>) {
    let Some(p) = params else { return (0, Vec::new()) };
    let mut c = p.walk();
    let named: Vec<Node> = p.named_children(&mut c).filter(|n| !is_comment(n.kind())).collect();
    let names = named.iter().map(|n| first_ident(*n, src).unwrap_or_else(|| "_".to_string())).collect();
    (named.len(), names)
}

/// The type text of the first parameter (`&Ticket` for `t: &Ticket`), empty when untyped.
fn first_param_type(params: Option<Node>, src: &[u8]) -> String {
    let Some(p) = params else { return String::new() };
    let mut c = p.walk();
    let Some(first) = p.named_children(&mut c).find(|n| !is_comment(n.kind())) else { return String::new() };
    let ty = first.child_by_field_name("type").or_else(|| {
        let mut c2 = first.walk();
        first.named_children(&mut c2).find(|n| n.kind() == "type_annotation").and_then(|a| a.named_child(0))
    });
    ty.map_or(String::new(), |t| norm_ws(text(t, src)))
}

/// Body tokens with the outer braces, a leading `return` and a trailing `;` stripped, so a
/// tail-expression body and a `return x;` body match.
fn strip_body(mut toks: Vec<Tok>, lang: Language) -> Vec<Tok> {
    let brace = |t: &Tok, s: &str| t.class == Class::Other && t.hash == hash_str(s);
    if lang != Language::Python && toks.len() >= 2 && brace(&toks[0], "{") && brace(&toks[toks.len() - 1], "}") {
        toks.remove(0);
        toks.pop();
    }
    if toks.first().is_some_and(|t| brace(t, "return")) {
        toks.remove(0);
    }
    if toks.last().is_some_and(|t| brace(t, ";")) {
        toks.pop();
    }
    toks
}

/// Is `body` a single match / switch whose arms all yield a string literal?
fn match_over_strings(body: Node, lang: Language) -> bool {
    let mut n = body;
    // Down through a block holding exactly one named child.
    loop {
        if matches!(n.kind(), "block" | "statement_block" | "expression_statement") && n.named_child_count() == 1 {
            n = n.named_child(0).unwrap();
            continue;
        }
        break;
    }
    let (arm_kind, holder) = match (lang, n.kind()) {
        (Language::Rust, "match_expression") => ("match_arm", n.child_by_field_name("body")),
        (Language::Python, "match_statement") => ("case_clause", n.child_by_field_name("body").or(Some(n))),
        (_, "switch_statement") => ("switch_case", n.child_by_field_name("body")),
        _ => return false,
    };
    let Some(holder) = holder else { return false };
    let mut c = holder.walk();
    let arms: Vec<Node> = holder.named_children(&mut c).filter(|a| a.kind() == arm_kind).collect();
    if arms.is_empty() {
        return false;
    }
    arms.iter().all(|a| {
        // The arm's value: its `value` field, else the string in its (single-statement) body.
        let mut v = a.child_by_field_name("value").or_else(|| a.child_by_field_name("consequence")).or_else(|| a.child_by_field_name("body"));
        if v.is_none() {
            let mut c2 = a.walk();
            v = a.named_children(&mut c2).last();
        }
        let mut stack: Vec<Node> = v.into_iter().collect();
        let mut found = false;
        while let Some(x) = stack.pop() {
            if literal_class(x.kind(), lang) == Some(Class::Str) {
                found = true;
                break;
            }
            if x.named_child_count() > 2 || matches!(x.kind(), "if_expression" | "if_statement" | "call_expression" | "call") {
                break;
            }
            let mut c3 = x.walk();
            stack.extend(x.named_children(&mut c3));
        }
        found
    })
}

/// Does a preceding outer attribute gate the item on a platform or feature (or `cfg(test)`)?
fn cfg_gated(item: Node, src: &[u8], gates: &[String], gate_test: bool) -> bool {
    let mut prev = item.prev_named_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "attribute_item" => {
                let t = text(p, src);
                if t.contains("cfg(") && ((gate_test && t.contains("test")) || gates.iter().any(|g| t.contains(g.as_str()))) {
                    return true;
                }
            }
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        prev = p.prev_named_sibling();
    }
    false
}

fn is_all_caps(name: &str) -> bool {
    name.len() > 1 && name.chars().any(|c| c.is_ascii_uppercase()) && !name.chars().any(|c| c.is_ascii_lowercase())
}

/// The parts of one definition handed to `push_def`.
struct Item<'a> {
    name: Node<'a>,
    item: Node<'a>,
    body: Option<Node<'a>>,
    params: Option<Node<'a>>,
    ret: Option<Node<'a>>,
    kind: &'static str,
    /// `fn`, `const`, `def`, `function`: what the git needle starts with.
    keyword: &'a str,
    owner: Option<&'a str>,
    visibility: Visibility,
    in_test: bool,
}

/// Config-derived matchers, built once per pass.
pub struct Walker {
    cfg: Cfg,
    siblings: GlobSet,
}

impl Walker {
    pub fn new(cfg: &Cfg) -> Self {
        let mut b = GlobSetBuilder::new();
        for g in &cfg.sibling_dir_globs {
            match Glob::new(g) {
                Ok(g) => {
                    b.add(g);
                }
                Err(e) => eprintln!("warning: [helpers] bad glob {g:?}: {e}"),
            }
        }
        Walker { cfg: cfg.clone(), siblings: b.build().unwrap_or_else(|_| GlobSet::empty()) }
    }

    /// One file's side on an already-parsed tree (`None` when the parse failed: no
    /// definitions, an empty stream). `regions` are the file's inline test regions.
    pub fn file_side(&self, root: Option<Node>, file: &SourceFile, regions: &[TestRegion]) -> FileSide {
        let mut side = FileSide { path: file.path.clone(), kind: file.kind, defs: Vec::new(), stream: Vec::new() };
        let Some(root) = root else { return side };
        let src = file.content.as_bytes();
        let test_file = file.kind == FileKind::Test;
        let skip: &[TestRegion] = if self.cfg.include_test_helpers { &[] } else { regions };
        side.stream = stream(root, src, file.lang, skip, &file.path);
        let mut defs = Vec::new();
        match file.lang {
            Language::Rust => self.rust_defs(root, src, file, regions, &mut defs),
            Language::Python => self.python_defs(root, src, file, &mut defs),
            _ => self.ts_defs(root, src, file, &mut defs),
        }
        for d in &mut defs {
            d.in_test = d.in_test || test_file;
        }
        defs.sort_by_key(|d| d.start_line);
        side.defs = defs.into_iter().filter(|d| self.cfg.include_test_helpers || !d.in_test).collect();
        side
    }

    /// Parse and index one file on its own.
    #[cfg(test)]
    pub fn parse_side(&self, file: &SourceFile) -> FileSide {
        let src = file.content.as_bytes();
        let tree = file.lang.parser().parse(src, None);
        let root = tree.as_ref().map(|t| t.root_node());
        let regions = match (root, file.lang) {
            (Some(root), Language::Rust) => regions::test_regions(root, src),
            _ => Vec::new(),
        };
        self.file_side(root, file, &regions)
    }

    fn push_def(&self, out: &mut Vec<HelperDef>, it: Item, src: &[u8], file: &SourceFile) {
        let Item { name, item, body, params, ret, kind, keyword, owner, visibility, in_test } = it;
        let nm = text(name, src).to_string();
        let qualified = owner.map_or_else(|| nm.clone(), |o| format!("{o}::{nm}"));
        let (n, names) = params_of(params, src);
        let body_toks = match body {
            None => Vec::new(),
            Some(b) if kind == "const" && self.cfg.compare_consts_raw => text(b, src).split_whitespace().map(|w| Tok { hash: hash_str(w), class: Class::Other }).collect(),
            Some(b) => {
                let bound = bound_names(item, src, file.lang);
                let mut toks = Vec::new();
                emit(b, src, file.lang, &bound, &[], &mut toks);
                strip_body(toks.into_iter().map(|t| t.tok).collect(), file.lang)
            }
        };
        let after = src.get(name.end_byte()..name.end_byte() + 1).and_then(|b| std::str::from_utf8(b).ok()).unwrap_or("");
        let after = if matches!(after, "(" | "<" | ":" | " " | "=") { after } else { "" };
        out.push(HelperDef {
            needle: format!("{keyword} {nm}{after}"),
            name: nm,
            lang: lang_family(file.lang),
            qualified,
            kind,
            file: file.path.clone(),
            start_line: item.start_position().row + 1,
            end_line: item.end_position().row + 1,
            visibility,
            in_test,
            params: n,
            param_names: names,
            ret: ret.map_or(String::new(), |r| norm_ws(text(r, src))),
            first_param_type: first_param_type(params, src),
            match_over_strings: kind == "fn" && body.is_some_and(|b| match_over_strings(b, file.lang)),
            body: body_toks,
        });
    }

    fn rust_defs(&self, root: Node, src: &[u8], file: &SourceFile, regions: &[TestRegion], out: &mut Vec<HelperDef>) {
        // (container, inherent impl type)
        let mut stack: Vec<(Node, Option<&str>)> = vec![(root, None)];
        while let Some((container, owner)) = stack.pop() {
            let mut c = container.walk();
            let items: Vec<Node> = container.named_children(&mut c).collect();
            for n in items {
                match n.kind() {
                    "mod_item" => {
                        if !cfg_gated(n, src, &self.cfg.cfg_gate_attrs, !self.cfg.include_test_helpers) && let Some(b) = n.child_by_field_name("body") {
                            stack.push((b, None));
                        }
                    }
                    "impl_item" => {
                        if self.cfg.include_inherent_methods && n.child_by_field_name("trait").is_none() && let Some(b) = n.child_by_field_name("body") {
                            let ty = n.child_by_field_name("type").map(|t| text(t, src));
                            stack.push((b, ty));
                        }
                    }
                    "function_item" | "const_item" | "static_item" => {
                        if cfg_gated(n, src, &self.cfg.cfg_gate_attrs, !self.cfg.include_test_helpers) {
                            continue;
                        }
                        let Some(name) = n.child_by_field_name("name") else { continue };
                        let (vis, _) = rust_visibility(n, src);
                        let in_test = regions::contains(regions, n.start_byte());
                        if n.kind() == "function_item" {
                            self.push_def(out, Item { name, item: n, body: n.child_by_field_name("body"), params: n.child_by_field_name("parameters"), ret: n.child_by_field_name("return_type"), kind: "fn", keyword: "fn", owner, visibility: vis, in_test }, src, file);
                        } else {
                            let kw = if n.kind() == "const_item" { "const" } else { "static" };
                            self.push_def(out, Item { name, item: n, body: n.child_by_field_name("value"), params: None, ret: None, kind: "const", keyword: kw, owner, visibility: vis, in_test }, src, file);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn python_defs(&self, root: Node, src: &[u8], file: &SourceFile, out: &mut Vec<HelperDef>) {
        let mut c = root.walk();
        let items: Vec<Node> = root.named_children(&mut c).collect();
        for n in items {
            let def = match n.kind() {
                "decorated_definition" => {
                    let mut c2 = n.walk();
                    if n.named_children(&mut c2).any(|d| d.kind() == "decorator" && text(d, src).contains("overload")) {
                        continue;
                    }
                    n.child_by_field_name("definition")
                }
                "function_definition" => Some(n),
                "expression_statement" => {
                    if let Some(a) = n.named_child(0).filter(|a| a.kind() == "assignment")
                        && let Some(l) = a.child_by_field_name("left").filter(|l| l.kind() == "identifier")
                        && is_all_caps(text(l, src))
                    {
                        let vis = if text(l, src).starts_with('_') { Visibility::Private } else { Visibility::Public };
                        self.push_def(out, Item { name: l, item: a, body: a.child_by_field_name("right"), params: None, ret: None, kind: "const", keyword: "", owner: None, visibility: vis, in_test: false }, src, file);
                    }
                    None
                }
                _ => None,
            };
            let Some(d) = def.filter(|d| d.kind() == "function_definition") else { continue };
            let Some(name) = d.child_by_field_name("name") else { continue };
            let nm = text(name, src);
            if matches!(nm, "__getattr__" | "__dir__") {
                continue;
            }
            let vis = if nm.starts_with('_') { Visibility::Private } else { Visibility::Public };
            let in_test = nm.starts_with("test_");
            self.push_def(out, Item { name, item: d, body: d.child_by_field_name("body"), params: d.child_by_field_name("parameters"), ret: d.child_by_field_name("return_type"), kind: "fn", keyword: "def", owner: None, visibility: vis, in_test }, src, file);
        }
    }

    fn ts_defs(&self, root: Node, src: &[u8], file: &SourceFile, out: &mut Vec<HelperDef>) {
        let mut c = root.walk();
        let items: Vec<Node> = root.named_children(&mut c).collect();
        for n in items {
            let (decl, exported) = if n.kind() == "export_statement" {
                match n.child_by_field_name("declaration") {
                    Some(d) => (d, true),
                    None => continue,
                }
            } else {
                (n, false)
            };
            let vis = if exported { Visibility::Public } else { Visibility::Private };
            match decl.kind() {
                "function_declaration" | "generator_function_declaration" => {
                    if let Some(name) = decl.child_by_field_name("name") {
                        self.push_def(out, Item { name, item: decl, body: decl.child_by_field_name("body"), params: decl.child_by_field_name("parameters"), ret: decl.child_by_field_name("return_type"), kind: "fn", keyword: "function", owner: None, visibility: vis, in_test: false }, src, file);
                    }
                }
                "lexical_declaration" | "variable_declaration" => {
                    let kw = decl.child(0).map_or("const", |k| text(k, src));
                    let mut c2 = decl.walk();
                    let decls: Vec<Node> = decl.named_children(&mut c2).filter(|d| d.kind() == "variable_declarator").collect();
                    for d in decls {
                        let Some(name) = d.child_by_field_name("name").filter(|n| n.kind() == "identifier") else { continue };
                        let Some(value) = d.child_by_field_name("value") else { continue };
                        if matches!(value.kind(), "arrow_function" | "function_expression" | "function" | "generator_function") {
                            self.push_def(out, Item { name, item: d, body: value.child_by_field_name("body"), params: value.child_by_field_name("parameters").or_else(|| value.child_by_field_name("parameter")), ret: value.child_by_field_name("return_type"), kind: "fn", keyword: kw, owner: None, visibility: vis, in_test: false }, src, file);
                        } else if is_all_caps(text(name, src)) {
                            self.push_def(out, Item { name, item: d, body: Some(value), params: None, ret: None, kind: "const", keyword: kw, owner: None, visibility: vis, in_test: false }, src, file);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn lang_family(lang: Language) -> &'static str {
    match lang {
        Language::Rust => "rust",
        Language::Python => "python",
        _ => "typescript",
    }
}

fn rust_visibility<'a>(node: Node, src: &'a [u8]) -> (Visibility, &'a str) {
    let mut c = node.walk();
    match node.children(&mut c).find(|n| n.kind() == "visibility_modifier") {
        None => (Visibility::Private, ""),
        Some(v) => {
            let t = text(v, src);
            (if t == "pub" { Visibility::Public } else { Visibility::Restricted }, t)
        }
    }
}

/// Parse and index every Source and Test file once for this pass and the symbol index:
/// `scry helpers`.
pub fn index_all(files: &[SourceFile], cfg: &Cfg, dead_cfg: &crate::config::Dead) -> (Vec<FileSide>, SymbolIndex) {
    let w = Walker::new(cfg);
    let dw = dead::Walker::new(dead_cfg);
    let both: Vec<(FileSide, dead::FileIndex)> = files
        .par_iter()
        .filter(|f| dead::indexed(f))
        .map(|f| {
            let src = f.content.as_bytes();
            let tree = f.lang.parser().parse(src, None);
            let root = tree.as_ref().map(|t| t.root_node());
            let regions = match (root, f.lang) {
                (Some(root), Language::Rust) => regions::test_regions(root, src),
                _ => Vec::new(),
            };
            (w.file_side(root, f, &regions), dw.file_index(root, f, &regions))
        })
        .collect();
    let (sides, idx): (Vec<_>, Vec<_>) = both.into_iter().unzip();
    (sides, SymbolIndex::build(idx))
}

// ---------- analysis ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FamilyClass {
    /// Same signature, bodies under `min_body_jaccard` (off unless `report_divergent`).
    Divergent,
    /// Parameter count or return-type text differ (section info only).
    DifferentContract,
    /// Same signature, 5-gram Jaccard at or above `min_body_jaccard`.
    Similar,
    /// Same signature, identical normalized bodies.
    Verbatim,
}

impl FamilyClass {
    fn label(self) -> &'static str {
        match self {
            FamilyClass::Verbatim => "verbatim",
            FamilyClass::Similar => "similar",
            FamilyClass::DifferentContract => "different contract",
            FamilyClass::Divergent => "divergent",
        }
    }
}

/// One definition in a family.
#[derive(Debug, Clone, Serialize)]
pub struct HelperCopy {
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,
    pub signature: String,
    pub visibility: Visibility,
    /// Relation to the family's first copy: `verbatim`, `similar 0.71`, `different contract`.
    pub relation: String,
    pub commit: Option<String>,
    /// `session_…` from the introducing commit's `Claude-Session` trailer.
    pub session: Option<String>,
}

/// A pair of copies in different files.
#[derive(Debug, Clone, Serialize)]
pub struct PairClass {
    pub a: usize,
    pub b: usize,
    pub class: FamilyClass,
    pub jaccard: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Family {
    pub name: String,
    pub kind: &'static str,
    pub class: FamilyClass,
    pub best_jaccard: f64,
    pub files: usize,
    pub copies: Vec<HelperCopy>,
    pub pairs: Vec<PairClass>,
    /// Distinct introducing commits found; 0 when attribution did not run.
    pub commits: usize,
    pub sessions: usize,
    /// `same match over 3 types`.
    pub annotation: Option<String>,
    /// `(occurrences, files)` when the family's body is also an inlined idiom.
    pub inlined: Option<(usize, usize)>,
    /// The section line.
    pub line: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub file: String,
    pub line: usize,
    /// The helper is not private and the hit file is in its crate / package or imports its file.
    pub reachable: bool,
}

/// A helper whose body is inlined elsewhere.
#[derive(Debug, Clone, Serialize)]
pub struct Inlined {
    pub name: String,
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,
    pub visibility: Visibility,
    pub tokens: usize,
    /// Other definitions with the same name and the same body (`src/store.rs:254`).
    pub also_defined: Vec<String>,
    pub hits: Vec<Hit>,
    pub files: usize,
    pub line: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FileHelpers {
    /// This file's definitions in a reported verbatim / similar family.
    pub helper_copies: usize,
    /// Inline copies of some helper's body in this file.
    pub inlined_idioms: usize,
    /// The families' names.
    pub families: Vec<String>,
    /// `(helper name, hit line)`.
    pub idioms: Vec<(String, usize)>,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    pub definitions: usize,
    pub files: usize,
    /// Names defined in at least `min_files` files.
    pub multi_file_names: usize,
    pub suppressed_twins: usize,
    pub verbatim: usize,
    pub similar: usize,
    pub different_contract: usize,
    pub divergent: usize,
    /// Reported families.
    pub families: usize,
    /// Definitions whose body is `min_tokens..=max_helper_tokens` tokens and passes the kind filters.
    pub small_helpers: usize,
    /// Of those, definitions inlined at least `min_occurrences` times in `min_files` files.
    pub inlined_helpers: usize,
    /// `inlined_helpers / small_helpers`.
    pub inlined_share: f64,
    pub git_lookups: usize,
    pub git_lookups_capped: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct HelpersReport {
    pub totals: Totals,
    /// `attribution: …` and other notes.
    pub notes: Vec<String>,
    /// Sorted by files x Jaccard, then path.
    pub families: Vec<Family>,
    /// Sorted by occurrences, then path.
    pub inlined: Vec<Inlined>,
    pub files: BTreeMap<String, FileHelpers>,
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The crate or package a file belongs to: the path up to and including its first `src`
/// component, else its first directory (`crates/ignore/src`, `src`, `pkg`).
fn package_of(path: &str) -> &str {
    let parts: Vec<&str> = path.split('/').collect();
    if let Some(i) = parts.iter().position(|p| *p == "src") {
        let end: usize = parts[..=i].iter().map(|p| p.len() + 1).sum();
        return &path[..end.saturating_sub(1)];
    }
    parts.first().copied().unwrap_or("")
}

/// 5-gram Jaccard of two token sequences; sequences too short for a 5-gram compare whole.
fn jaccard(a: &[Tok], b: &[Tok]) -> f64 {
    if a.len() < 5 || b.len() < 5 {
        return if a == b { 1.0 } else { 0.0 };
    }
    let grams = |s: &[Tok]| s.windows(5).map(|w| w.to_vec()).collect::<HashSet<Vec<Tok>>>();
    let (ga, gb) = (grams(a), grams(b));
    let inter = ga.intersection(&gb).count();
    let union = ga.union(&gb).count();
    if union == 0 { 1.0 } else { inter as f64 / union as f64 }
}

fn range(s: usize, e: usize) -> String {
    if s == e { format!("{s}") } else { format!("{s}-{e}") }
}

fn join_and(items: &[String]) -> String {
    match items.len() {
        0 => String::new(),
        1 => items[0].clone(),
        n => format!("{} and {}", items[..n - 1].join(", "), items[n - 1]),
    }
}

/// `(commit hash, session id)`.
type Attribution = (String, Option<String>);

/// `git log -S'fn plural(' -- file`, oldest commit, and its `Claude-Session` trailer.
fn attribute(root: &Path, def: &HelperDef) -> Option<Attribution> {
    let out = Command::new("git").arg("-C").arg(root).args(["log", "--format=%H", "--reverse", "-S"]).arg(&def.needle).arg("--").arg(&def.file).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let hash = String::from_utf8_lossy(&out.stdout).lines().next()?.trim().to_string();
    if hash.is_empty() {
        return None;
    }
    let body = Command::new("git").arg("-C").arg(root).args(["log", "-1", "--format=%B"]).arg(&hash).output().ok()?;
    let body = String::from_utf8_lossy(&body.stdout);
    let session = body.lines().find_map(|l| l.strip_prefix("Claude-Session:")).map(|v| v.trim().rsplit('/').next().unwrap_or("").to_string()).filter(|s| !s.is_empty());
    Some((hash, session))
}

/// Match `seq` at `stream[i..]`; with `wildcard` one `ID` slot of `seq` may cover one
/// expression-shaped token run of the stream (`n == 1` matching `e.rules == 1` or
/// `open.len() == 1`): brackets balanced, no `,` `;` `{` `}` `=>` at depth 0, no longer than
/// the helper itself (a node test would miss runs inside a macro's `token_tree`). Returns the
/// exclusive end index of the hit.
fn match_at(seq: &[Tok], stream: &[STok], i: usize, wildcard: bool) -> Option<usize> {
    let exact = |j0: usize, p0: usize| -> Option<usize> {
        let n = seq.len() - j0;
        if p0 + n > stream.len() {
            return None;
        }
        (0..n).all(|k| stream[p0 + k].tok == seq[j0 + k]).then_some(p0 + n)
    };
    if let Some(end) = exact(0, i) {
        return Some(end);
    }
    if !wildcard {
        return None;
    }
    let h = |s: &str| hash_str(s);
    let (open, close, stop) = ([h("("), h("["), h("{")], [h(")"), h("]"), h("}")], [h(","), h(";"), h("=>")]);
    // The tokens before the wildcard slot match one for one, so slot j sits at stream i + j.
    for (j, t) in seq.iter().enumerate() {
        let p = i + j;
        if p >= stream.len() {
            return None;
        }
        if t.class == Class::Id {
            let mut depth = 0i32;
            let mut q = p;
            while q < stream.len() && q - p < seq.len() {
                let tk = stream[q].tok;
                if tk.class == Class::Other {
                    if open.contains(&tk.hash) {
                        depth += 1;
                    } else if close.contains(&tk.hash) {
                        depth -= 1;
                        if depth < 0 {
                            break;
                        }
                    } else if depth == 0 && stop.contains(&tk.hash) {
                        break;
                    }
                }
                q += 1;
                if depth == 0 && q > p + 1 && let Some(end) = exact(j + 1, q) {
                    return Some(end);
                }
            }
        }
        if stream[p].tok != *t {
            return None;
        }
    }
    None
}

/// Group, classify, suppress, attribute and search: `scan` and `scry helpers`. `git` is the
/// repository root when history is available (attribution needs it).
pub fn analyze(sides: &[FileSide], index: &SymbolIndex, deps: &DepGraph, git: Option<&Path>, cfg: &Cfg) -> HelpersReport {
    let w = Walker::new(cfg);
    let mut report = HelpersReport::default();
    let defs: Vec<&HelperDef> = sides.iter().flat_map(|s| s.defs.iter()).collect();
    report.totals.definitions = defs.len();
    report.totals.files = sides.iter().filter(|s| !s.defs.is_empty()).count();

    // ----- P07: name families -----
    let mut by_name: BTreeMap<(&str, &str), Vec<&HelperDef>> = BTreeMap::new();
    for d in &defs {
        if d.name.len() < cfg.min_name_len || cfg.ignore_names.iter().any(|n| n == &d.name) {
            continue;
        }
        by_name.entry((d.qualified.as_str(), d.lang)).or_default().push(d);
    }
    let mut families: Vec<Family> = Vec::new();
    for ((name, _), copies) in by_name {
        let mut files: Vec<&str> = copies.iter().map(|d| d.file.as_str()).collect();
        files.sort_unstable();
        files.dedup();
        if files.len() < cfg.min_files {
            continue;
        }
        report.totals.multi_file_names += 1;
        // Twin suppression.
        let mut suppressed = false;
        for (i, a) in files.iter().enumerate() {
            if w.siblings.is_match(a) {
                suppressed = true;
            }
            for b in &files[i + 1..] {
                if cfg.suppress_path_suffix_twins && basename(a) == basename(b) {
                    suppressed = true;
                }
                if cfg.suppress_import_linked {
                    let linked = |from: &str, to: &str| deps.edge(from, to).is_some_and(|e| e.glob || e.names.iter().any(|n| n == name || n.rsplit("::").next() == Some(name)));
                    if linked(a, b) || linked(b, a) {
                        suppressed = true;
                    }
                }
            }
        }
        if suppressed {
            report.totals.suppressed_twins += 1;
            continue;
        }
        let mut copies: Vec<&HelperDef> = copies;
        copies.sort_by(|a, b| a.file.cmp(&b.file).then(a.start_line.cmp(&b.start_line)));
        let mut pairs: Vec<PairClass> = Vec::new();
        let mut best = 0.0f64;
        for i in 0..copies.len() {
            for j in i + 1..copies.len() {
                if copies[i].file == copies[j].file {
                    continue;
                }
                let sig = copies[i].params == copies[j].params && copies[i].ret == copies[j].ret;
                let jac = jaccard(&copies[i].body, &copies[j].body);
                let identical = copies[i].body == copies[j].body;
                let class = if !sig {
                    FamilyClass::DifferentContract
                } else if identical {
                    FamilyClass::Verbatim
                } else if jac >= cfg.min_body_jaccard {
                    FamilyClass::Similar
                } else {
                    FamilyClass::Divergent
                };
                best = best.max(jac);
                pairs.push(PairClass { a: i, b: j, class, jaccard: jac });
            }
        }
        let class = pairs.iter().map(|p| p.class).max().unwrap_or(FamilyClass::Divergent);
        match class {
            FamilyClass::Verbatim => report.totals.verbatim += 1,
            FamilyClass::Similar => report.totals.similar += 1,
            FamilyClass::DifferentContract => report.totals.different_contract += 1,
            FamilyClass::Divergent => report.totals.divergent += 1,
        }
        if class == FamilyClass::Divergent && !cfg.report_divergent {
            continue;
        }
        let annotation = (matches!(class, FamilyClass::Verbatim | FamilyClass::Similar) && copies.iter().all(|c| c.match_over_strings)).then(|| {
            let mut types: Vec<&str> = copies.iter().map(|c| c.first_param_type.as_str()).collect();
            types.sort_unstable();
            types.dedup();
            (types.len() > 1).then(|| format!("same match over {} types", types.len()))
        }).flatten();
        let relation = |i: usize| -> String {
            if i == 0 {
                return String::new();
            }
            let p = pairs.iter().find(|p| p.a == 0 && p.b == i);
            match p.map(|p| (p.class, p.jaccard)) {
                Some((FamilyClass::Verbatim, _)) => "verbatim".into(),
                Some((FamilyClass::Similar, j)) => format!("similar {j:.2}"),
                Some((FamilyClass::DifferentContract, _)) => "different contract".into(),
                Some((FamilyClass::Divergent, j)) => format!("divergent {j:.2}"),
                None => "same file".into(),
            }
        };
        let out: Vec<HelperCopy> = copies.iter().enumerate().map(|(i, d)| HelperCopy {
            file: d.file.clone(), start_line: d.start_line, end_line: d.end_line, signature: d.signature(), visibility: d.visibility, relation: relation(i), commit: None, session: None,
        }).collect();
        families.push(Family { name: name.to_string(), kind: copies[0].kind, class, best_jaccard: best, files: files.len(), copies: out, pairs, commits: 0, sessions: 0, annotation, inlined: None, line: String::new() });
    }
    families.sort_by(|x, y| {
        let kx = x.files as f64 * x.best_jaccard;
        let ky = y.files as f64 * y.best_jaccard;
        ky.partial_cmp(&kx).unwrap().then_with(|| x.copies[0].file.cmp(&y.copies[0].file)).then_with(|| x.name.cmp(&y.name))
    });
    report.totals.families = families.len();

    // Attribution: verbatim / similar copies first, capped.
    let git = git.filter(|_| cfg.attribute_commits).filter(|r| Command::new("git").arg("-C").arg(r).args(["rev-parse", "--show-prefix"]).output().is_ok_and(|o| o.status.success()));
    if let Some(root) = git {
        let def_of = |c: &HelperCopy| defs.iter().find(|d| d.file == c.file && d.start_line == c.start_line).copied();
        let mut order: Vec<(usize, usize)> = Vec::new();
        for pass in [true, false] {
            for (fi, f) in families.iter().enumerate() {
                if matches!(f.class, FamilyClass::Verbatim | FamilyClass::Similar) == pass {
                    order.extend((0..f.copies.len()).map(|ci| (fi, ci)));
                }
            }
        }
        report.totals.git_lookups_capped = order.len() > cfg.max_git_lookups;
        order.truncate(cfg.max_git_lookups);
        report.totals.git_lookups = order.len();
        let found: Vec<((usize, usize), Option<Attribution>)> =
            order.par_iter().map(|&(fi, ci)| ((fi, ci), def_of(&families[fi].copies[ci]).and_then(|d| attribute(root, d)))).collect();
        for ((fi, ci), r) in found {
            if let Some((hash, session)) = r {
                families[fi].copies[ci].commit = Some(hash);
                families[fi].copies[ci].session = session;
            }
        }
        for f in families.iter_mut() {
            let mut commits: Vec<&str> = f.copies.iter().filter_map(|c| c.commit.as_deref()).collect();
            commits.sort_unstable();
            commits.dedup();
            let mut sessions: Vec<&str> = f.copies.iter().filter_map(|c| c.session.as_deref()).collect();
            sessions.sort_unstable();
            sessions.dedup();
            f.commits = commits.len();
            f.sessions = sessions.len();
        }
        report.notes.push(match (report.totals.git_lookups, report.totals.git_lookups_capped) {
            (n, true) => format!("attribution: {n} git lookups (capped at {}; later copies unattributed)", cfg.max_git_lookups),
            (n, false) => format!("attribution: {n} git lookups"),
        });
    } else {
        report.notes.push(if cfg.attribute_commits { "attribution: off (no git history)".to_string() } else { "attribution: off ([helpers].attribute_commits = false)".to_string() });
    }

    // ----- P08: inlined idioms -----
    let id = id_tok();
    let candidates: Vec<&HelperDef> = defs
        .iter()
        .copied()
        .filter(|d| {
            d.kind == "fn"
                && !cfg.inline_ignore_names.iter().any(|n| n == &d.name)
                && (cfg.min_tokens..=cfg.max_helper_tokens).contains(&d.body.len())
                && d.body.iter().collect::<HashSet<_>>().len() >= cfg.min_distinct_kinds
                && d.body.iter().any(|t| t.class == Class::Other)
                && d.body.iter().any(|t| matches!(t.class, Class::Name | Class::Str | Class::Num))
        })
        .collect();
    report.totals.small_helpers = candidates.len();
    // Helpers with one name and one body are one idiom.
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut group_of: HashMap<(&str, &[Tok]), usize> = HashMap::new();
    for (i, d) in candidates.iter().enumerate() {
        match group_of.get(&(d.name.as_str(), d.body.as_slice())) {
            Some(&g) => groups[g].push(i),
            None => {
                group_of.insert((d.name.as_str(), d.body.as_slice()), groups.len());
                groups.push(vec![i]);
            }
        }
    }
    // Helpers indexed by their first tokens (one, with the wildcard on: an `ID` slot among the
    // first four would break a longer key), looked up with a fixed-size key: no allocation
    // per stream position.
    let key_len = if cfg.allow_one_wildcard { 1 } else { 4.min(cfg.min_tokens).max(1) };
    let key_of = |toks: &[Tok]| -> [u64; 4] {
        let mut k = [0u64; 4];
        for (i, t) in toks.iter().take(key_len).enumerate() {
            k[i] = t.hash;
        }
        k
    };
    let mut prefix: HashMap<[u64; 4], Vec<usize>> = HashMap::new();
    let mut any: Vec<usize> = Vec::new();
    for (g, members) in groups.iter().enumerate() {
        let body = &candidates[members[0]].body;
        if cfg.allow_one_wildcard && body[0] == id {
            any.push(g);
        } else {
            prefix.entry(key_of(body)).or_default().push(g);
        }
    }
    // Spans of every definition of a name (any file): a hit inside one is the definition.
    let mut def_spans: HashMap<&str, Vec<(&str, usize, usize)>> = HashMap::new();
    for g in &groups {
        let name = candidates[g[0]].name.as_str();
        def_spans.entry(name).or_insert_with(|| index.definitions(name).map(|d| (d.file.as_str(), d.start_line, d.end_line)).collect());
    }
    let searched: Vec<&FileSide> = sides.iter().filter(|s| s.kind == FileKind::Source).collect();
    let hits_per_file: Vec<Vec<(usize, usize, usize)>> = searched
        .par_iter()
        .map(|s| {
            let mut out: Vec<(usize, usize, usize)> = Vec::new(); // (group, start token, end token)
            let st = &s.stream;
            let mut key = [0u64; 4];
            for i in 0..st.len() {
                if i + key_len > st.len() {
                    break;
                }
                for (k, t) in st[i..i + key_len].iter().enumerate() {
                    key[k] = t.tok.hash;
                }
                let cands = prefix.get(&key).into_iter().flatten().chain(any.iter());
                for &g in cands {
                    let body = &candidates[groups[g][0]].body;
                    if let Some(end) = match_at(body, st, i, cfg.allow_one_wildcard) {
                        let (l0, l1) = (st[i].line as usize, st[end - 1].line as usize);
                        let name = candidates[groups[g][0]].name.as_str();
                        let inside_def = def_spans.get(name).into_iter().flatten().any(|(f, a, z)| *f == s.path && *a <= l0 && l1 <= *z);
                        if !inside_def {
                            out.push((g, i, end));
                        }
                    }
                }
            }
            out
        })
        .collect();
    let mut hits_by_group: Vec<Vec<Hit>> = vec![Vec::new(); groups.len()];
    for (si, hits) in hits_per_file.iter().enumerate() {
        let s = searched[si];
        for &(g, i, _) in hits {
            let d = candidates[groups[g][0]];
            let reachable = d.visibility != Visibility::Private && (package_of(&s.path) == package_of(&d.file) || deps.connected(&s.path, &d.file));
            if cfg.require_reachable && !reachable {
                continue;
            }
            hits_by_group[g].push(Hit { file: s.path.clone(), line: s.stream[i].line as usize, reachable });
        }
    }
    let mut inlined: Vec<Inlined> = Vec::new();
    for (g, members) in groups.iter().enumerate() {
        let mut hits = std::mem::take(&mut hits_by_group[g]);
        hits.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
        let mut files: Vec<&str> = hits.iter().map(|h| h.file.as_str()).collect();
        files.sort_unstable();
        files.dedup();
        if hits.len() < cfg.min_occurrences || files.len() < cfg.min_files {
            continue;
        }
        report.totals.inlined_helpers += members.len();
        let d = candidates[members[0]];
        let also: Vec<String> = members[1..].iter().map(|&m| format!("{}:{}", candidates[m].file, candidates[m].start_line)).collect();
        inlined.push(Inlined {
            name: d.name.clone(), file: d.file.clone(), start_line: d.start_line, end_line: d.end_line, visibility: d.visibility, tokens: d.body.len(),
            also_defined: also, files: files.len(), hits, line: String::new(),
        });
    }
    inlined.sort_by(|x, y| y.hits.len().cmp(&x.hits.len()).then_with(|| x.file.cmp(&y.file)).then_with(|| x.start_line.cmp(&y.start_line)));
    report.totals.inlined_share = if report.totals.small_helpers == 0 { 0.0 } else { report.totals.inlined_helpers as f64 / report.totals.small_helpers as f64 };
    for f in families.iter_mut() {
        if let Some(i) = inlined.iter().find(|i| i.name == f.name && f.copies.iter().any(|c| c.file == i.file && c.start_line == i.start_line)) {
            f.inlined = Some((i.hits.len(), i.files));
        }
    }

    // ----- lines and per-file reasons -----
    let mut per_file: BTreeMap<String, FileHelpers> = BTreeMap::new();
    for f in families.iter_mut() {
        let attribution = match (f.commits, f.sessions) {
            (0, _) => String::new(),
            (c, 0) => format!(" from {c} commit{}", if c == 1 { "" } else { "s" }),
            (c, s) => format!(" from {c} commit{} / {s} session{}", if c == 1 { "" } else { "s" }, if s == 1 { "" } else { "s" }),
        };
        let note = f.annotation.as_ref().map_or(String::new(), |a| format!(" ({a})"));
        let locs: Vec<String> = f.copies.iter().map(|c| {
            let rel = if c.relation.is_empty() { String::new() } else { format!(" ({})", c.relation) };
            format!("{}:{}{rel}", c.file, range(c.start_line, c.end_line))
        }).collect();
        let attributed = if attribution.is_empty() { String::new() } else { format!(";{attribution}") };
        f.line = match f.class {
            FamilyClass::DifferentContract | FamilyClass::Divergent => {
                let sigs: Vec<String> = f.copies.iter().map(|c| format!("{}:{} {}", c.file, c.start_line, c.signature)).collect();
                format!("{}: {} (Jaccard {:.1}){attributed}", f.name, sigs.join(", "), f.best_jaccard)
            }
            _ => format!("{}  {} copies in {} files, {} (Jaccard {:.2}){note}: {}{attributed}", f.name, f.copies.len(), f.files, f.class.label(), f.best_jaccard, locs.join(", ")),
        };
        if !matches!(f.class, FamilyClass::Verbatim | FamilyClass::Similar) {
            continue;
        }
        let inl = f.inlined.map_or(String::new(), |(n, m)| format!("; its body is also inlined {n} time{} in {m} file{} instead of called", if n == 1 { "" } else { "s" }, if m == 1 { "" } else { "s" }));
        for (i, c) in f.copies.iter().enumerate() {
            let has_partner = f.pairs.iter().any(|p| (p.a == i || p.b == i) && matches!(p.class, FamilyClass::Verbatim | FamilyClass::Similar));
            if !has_partner {
                continue;
            }
            let others: Vec<String> = f.copies.iter().enumerate().filter(|(j, _)| *j != i && f.copies[*j].file != c.file).map(|(j, o)| {
                let (lo, hi) = (i.min(j), i.max(j));
                let p = f.pairs.iter().find(|p| p.a == lo && p.b == hi);
                let loc = format!("{}:{}", o.file, o.start_line);
                match p.map(|p| (p.class, p.jaccard)) {
                    Some((FamilyClass::Verbatim, _)) => format!("verbatim in {loc}"),
                    Some((FamilyClass::Similar, j)) => format!("similarly (Jaccard {j:.2}) in {loc}"),
                    Some((FamilyClass::DifferentContract, _)) => {
                        let d = defs.iter().find(|d| d.file == o.file && d.start_line == o.start_line).copied();
                        let sig = d.map_or(o.signature.clone(), |d| {
                            let me = defs.iter().find(|x| x.file == c.file && x.start_line == c.start_line).copied();
                            if me.is_some_and(|m| m.params == d.params) { d.signature() } else { format!("{}({})", d.qualified, d.param_names.join(", ")) }
                        });
                        format!("as {sig} in {loc}")
                    }
                    Some((FamilyClass::Divergent, j)) => format!("differently (Jaccard {j:.2}) in {loc}"),
                    None => format!("in {loc}"),
                }
            }).collect();
            let n = f.copies.len();
            let reason = format!("defines {} (lines {}), also defined {} - {n} copies{note}{attribution}{inl}; hoist one", f.name, range(c.start_line, c.end_line), join_and(&others));
            let e = per_file.entry(c.file.clone()).or_default();
            e.helper_copies += 1;
            e.families.push(f.name.clone());
            e.reasons.push(reason);
        }
    }
    for i in inlined.iter_mut() {
        let locs: Vec<String> = i.hits.iter().map(|h| format!("{}:{}", h.file, h.line)).collect();
        let also = if i.also_defined.is_empty() { String::new() } else { format!(", also defined at {}", i.also_defined.join(", ")) };
        let n = i.hits.len();
        let head = format!("body of {} ({}:{}{also}, {} tokens) is inlined {n} time{} in {} file{}", i.name, i.file, range(i.start_line, i.end_line), i.tokens, if n == 1 { "" } else { "s" }, i.files, if i.files == 1 { "" } else { "s" });
        let private = i.visibility == Visibility::Private;
        i.line = if private { format!("{head}: {} - {} is private; hoist and call", locs.join(", "), i.name) } else { format!("{head} instead of called: {}", locs.join(", ")) };
        let mut by_file: BTreeMap<&str, bool> = BTreeMap::new();
        for h in &i.hits {
            let e = by_file.entry(h.file.as_str()).or_insert(true);
            *e = *e && h.reachable;
        }
        for (file, reachable) in by_file {
            let reason = if reachable {
                format!("body of {} ({}:{}) is inlined {n} time{} in {} file{} instead of called: {}", i.name, i.file, range(i.start_line, i.end_line), if n == 1 { "" } else { "s" }, i.files, if i.files == 1 { "" } else { "s" }, locs.join(", "))
            } else {
                format!("body of {} ({}:{}) is inlined {n} time{} in {} file{}: {} - {} is {}; hoist and call", i.name, i.file, range(i.start_line, i.end_line), if n == 1 { "" } else { "s" }, i.files, if i.files == 1 { "" } else { "s" }, locs.join(", "), i.name, if private { "private" } else { "not importable here" })
            };
            let e = per_file.entry(file.to_string()).or_default();
            let mine = i.hits.iter().filter(|h| h.file == file).count();
            e.inlined_idioms += mine;
            e.idioms.extend(i.hits.iter().filter(|h| h.file == file).map(|h| (i.name.clone(), h.line)));
            e.reasons.push(reason);
        }
    }
    for fh in per_file.values_mut() {
        if fh.reasons.len() > cfg.max_reported_per_file {
            let more = fh.reasons.len() - cfg.max_reported_per_file;
            fh.reasons.truncate(cfg.max_reported_per_file);
            fh.reasons.push(format!("(+{more} more helper findings in helper_copies / inlined_idioms)"));
        }
    }
    report.families = families;
    report.inlined = inlined;
    report.files = per_file;
    report
}

// ---------- output ----------

/// `12 top-level helpers defined in 2+ files (3 twins suppressed): 3 verbatim, 2 similar, 4 different contract; 8 of 205 small helpers inlined elsewhere (3.9%)`.
pub fn totals_line(r: &HelpersReport) -> String {
    let t = &r.totals;
    let div = if t.divergent > 0 { format!(", {} divergent", t.divergent) } else { String::new() };
    let sup = if t.suppressed_twins > 0 { format!(" ({} twins suppressed)", t.suppressed_twins) } else { String::new() };
    format!(
        "{} names defined in 2+ files{sup}: {} verbatim, {} similar, {} different contract{div}; {} of {} small helpers inlined elsewhere ({:.1}%)",
        t.multi_file_names, t.verbatim, t.similar, t.different_contract, t.inlined_helpers, t.small_helpers, t.inlined_share * 100.0
    )
}

/// The `scry helpers` text output.
pub fn render(r: &HelpersReport, top: usize) -> String {
    use std::fmt::Write;
    let mut o = String::new();
    let _ = writeln!(o, "{} top-level helpers in {} files; {}", r.totals.definitions, r.totals.files, totals_line(r));
    for n in &r.notes {
        let _ = writeln!(o, "note  {n}");
    }
    let _ = writeln!(o, "\nfamilies (files x Jaccard):");
    if r.families.is_empty() {
        let _ = writeln!(o, "  none");
    }
    for f in r.families.iter().take(top) {
        let _ = writeln!(o, "  {}", f.line);
        for c in &f.copies {
            let attr = match (&c.commit, &c.session) {
                (Some(h), Some(s)) => format!("  {}  {s}", &h[..h.len().min(10)]),
                (Some(h), None) => format!("  {}", &h[..h.len().min(10)]),
                _ => String::new(),
            };
            let _ = writeln!(o, "    {}:{}  {}{attr}", c.file, range(c.start_line, c.end_line), c.signature);
        }
    }
    let _ = writeln!(o, "\ninlined helpers (occurrences first):");
    if r.inlined.is_empty() {
        let _ = writeln!(o, "  none");
    }
    for i in r.inlined.iter().take(top) {
        let _ = writeln!(o, "  {}", i.line);
    }
    let _ = writeln!(o, "\nfiles (copies in reported families / inlined idioms):");
    let mut rows: Vec<(&String, &FileHelpers)> = r.files.iter().collect();
    rows.sort_by(|a, b| (b.1.helper_copies + b.1.inlined_idioms).cmp(&(a.1.helper_copies + a.1.inlined_idioms)).then_with(|| a.0.cmp(b.0)));
    if rows.is_empty() {
        let _ = writeln!(o, "  none");
    }
    for (p, fh) in rows.iter().take(top) {
        let _ = writeln!(o, "  {:>6} {:>7}  {p}", fh.helper_copies, fh.inlined_idioms);
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Dead as DeadCfg;

    fn file(path: &str, kind: FileKind, content: &str) -> SourceFile {
        let lang = Language::from_path(Path::new(path)).unwrap();
        SourceFile { path: path.into(), lang, kind, lines: content.lines().count(), bytes: content.len(), content: content.into() }
    }

    fn src(path: &str, content: &str) -> SourceFile {
        file(path, FileKind::Source, content)
    }

    fn run(files: &[SourceFile], cfg: &Cfg) -> HelpersReport {
        let (sides, index) = index_all(files, cfg, &DeadCfg::default());
        let deps = crate::deps::build(files, &crate::config::Deps::default());
        analyze(&sides, &index, &deps, None, cfg)
    }

    fn toks(path: &str, content: &str, name: &str) -> Vec<Tok> {
        let f = src(path, content);
        let side = Walker::new(&Cfg::default()).parse_side(&f);
        side.defs.iter().find(|d| d.name == name).unwrap_or_else(|| panic!("{name} in {:?}", side.defs.iter().map(|d| &d.name).collect::<Vec<_>>())).body.clone()
    }

    #[test]
    fn normalizer_anonymizes_bound_names_and_keeps_callees_fields_macros_and_literals() {
        let a = "fn io_err(p: &Path, what: &str, e: std::io::Error) -> KsError {\n    KsError::internal(anyhow::anyhow!(\"cannot {what} {}: {e}\", p.display()))\n}\n";
        let b = "fn io_err(q: &Path, w: &str, err: std::io::Error) -> KsError {\n    return KsError::internal(anyhow::anyhow!(\"cannot {w} {}: {err}\", q.display()));\n}\n";
        let ta = toks("src/a.rs", a, "io_err");
        let tb = toks("src/b.rs", b, "io_err");
        assert_eq!(ta, tb, "bound names and format captures are anonymized; `return` and `;` stripped");
        assert_eq!(ta.len(), 18);
        assert_eq!(ta.iter().filter(|t| t.class == Class::Id).count(), 1);
        assert!(ta.iter().any(|t| t.class == Class::Str) && ta.iter().any(|t| t.class == Class::Name));
        // A different format string or callee is a different body; a renamed let binding is not.
        let c = "fn io_err(p: &Path, what: &str, e: std::io::Error) -> KsError {\n    KsError::internal(anyhow::anyhow!(\"cannot open {}: {e}\", p.display()))\n}\n";
        assert_ne!(ta, toks("src/c.rs", c, "io_err"));
        let d = "fn f(x: &[u8]) -> Vec<u8> { let y = x.iter().map(|v| v + 1).collect(); y }\n";
        let e = "fn f(items: &[u8]) -> Vec<u8> { let out = items.iter().map(|it| it + 1).collect(); out }\n";
        assert_eq!(toks("src/d.rs", d, "f"), toks("src/e.rs", e, "f"));
        let g = "fn f(x: &[u8]) -> Vec<u8> { let y = x.iter().map(|v| v + 2).collect(); y }\n";
        assert_ne!(toks("src/d.rs", d, "f"), toks("src/g.rs", g, "f"), "literals are kept verbatim");
        // Keyword-spelled identifiers inside a macro token tree are keywords.
        let m = "fn m(n: u8) -> u8 { m!(if n == 1 { 1 } else { 2 }) }\n";
        let tm = toks("src/m.rs", m, "m");
        assert_eq!(tm.iter().filter(|t| t.class == Class::Other && t.hash == hash_str("if")).count(), 1);
        // Python and TypeScript: parameters and locals anonymized, attribute names kept.
        let py = "def f(a, b=1):\n    x = a.strip()\n    return x.lower()\n";
        let py2 = "def f(s, n=1):\n    y = s.strip()\n    return y.lower()\n";
        assert_eq!(toks("p/a.py", py, "f"), toks("p/b.py", py2, "f"));
        let ts = "export const toArray = (children: Child): Child[] => Array.isArray(children) ? children : [children]\n";
        let ts2 = "export function toArray(x: Child): Child[] { return Array.isArray(x) ? x : [x] }\n";
        assert_eq!(toks("s/a.ts", ts, "toArray"), toks("s/b.ts", ts2, "toArray"));
    }

    #[test]
    fn units_are_top_level_free_functions_consts_and_statics_not_methods_or_gated_items() {
        let a = "\
pub fn free_one() {}
pub const LIMIT: usize = 3;
static NAME: &str = \"x\";
struct S;
impl S { fn method_one(&self) {} }
impl Clone for S { fn clone(&self) -> S { S } }
trait T { fn trait_fn(); }
#[cfg(unix)]
fn gated_fn() {}
#[cfg(feature = \"x\")]
mod feat { pub fn in_feature() {} }
mod plain { pub fn in_mod() {} }
fn outer() { fn inner() {} }
#[cfg(test)]
mod tests { fn test_helper() {} }
";
        let side = Walker::new(&Cfg::default()).parse_side(&src("src/a.rs", a));
        let names: Vec<&str> = side.defs.iter().map(|d| d.qualified.as_str()).collect();
        assert_eq!(names, vec!["free_one", "LIMIT", "NAME", "in_mod", "outer"], "{names:?}");
        assert_eq!(side.defs.iter().find(|d| d.name == "LIMIT").map(|d| (d.kind, d.visibility)), Some(("const", Visibility::Public)));
        let cfg = Cfg { include_inherent_methods: true, include_test_helpers: true, ..Cfg::default() };
        let side = Walker::new(&cfg).parse_side(&src("src/a.rs", a));
        let names: Vec<&str> = side.defs.iter().map(|d| d.qualified.as_str()).collect();
        assert!(names.contains(&"S::method_one") && names.contains(&"test_helper") && !names.contains(&"S::clone"), "{names:?}");
        let py = "def f(a):\n    pass\n@overload\ndef g(a): ...\ndef __getattr__(n):\n    pass\nMAX = 3\nlower = 1\nclass C:\n    def m(self):\n        pass\n";
        let side = Walker::new(&Cfg::default()).parse_side(&src("p/a.py", py));
        assert_eq!(side.defs.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), vec!["f", "MAX"]);
        let ts = "export function a() {}\nconst b = (x: number) => x\nexport const KEY = 1\nconst lower = 2\nclass C { m() {} }\nfunction outer() { function inner() {} }\n";
        let side = Walker::new(&Cfg::default()).parse_side(&src("s/a.ts", ts));
        let defs: Vec<(&str, Visibility)> = side.defs.iter().map(|d| (d.name.as_str(), d.visibility)).collect();
        assert_eq!(defs, vec![("a", Visibility::Public), ("b", Visibility::Private), ("KEY", Visibility::Public), ("outer", Visibility::Private)]);
    }

    const PLURAL: &str = "fn plural(n: usize) -> &'static str {\n    if n == 1 {\n        \"\"\n    } else {\n        \"s\"\n    }\n}\n";

    #[test]
    fn families_classify_pairs_and_the_reason_names_every_copy() {
        let flow = format!("pub fn flow_it() {{}}\n{PLURAL}");
        let status = format!("pub fn status_it() {{}}\n\n{PLURAL}");
        let proposal = "pub(crate) fn plural(n: usize, noun: &str) -> String {\n    format!(\"{n} {noun}{}\", if n == 1 { \"\" } else { \"s\" })\n}\n";
        let cache = "pub fn render(state: &GitState) -> Result<String> {\n    let mut s = serde_json::to_string_pretty(state).map_err(KsError::internal)?;\n    s.push('\\n');\n    Ok(s)\n}\n";
        let setup = "fn render(root: &Map<String, Value>) -> Result<String> {\n    let mut s = serde_json::to_string_pretty(root).map_err(KsError::internal)?;\n    s.push('\\n');\n    Ok(s)\n}\n";
        let scan = "fn repo_relative(ctx: &Ctx, p: &Path) -> Option<String> {\n    p.strip_prefix(&ctx.root).ok().map(|r| r.display().to_string())\n}\n";
        let quirk = "fn repo_relative(ctx: &Ctx, p: &Path) -> String {\n    match p.strip_prefix(&ctx.root) { Ok(r) => r.display().to_string(), Err(_) => p.display().to_string() }\n}\n";
        let files = [
            src("src/cmd/flow.rs", &flow), src("src/cmd/status.rs", &status), src("src/cmd/proposal.rs", proposal),
            src("src/cache.rs", cache), src("src/setup.rs", setup), src("src/scan.rs", scan), src("src/cmd/quirk.rs", quirk),
            src("src/short.rs", "fn go() {}\n"), src("src/other.rs", "fn go() {}\n"),
        ];
        let r = run(&files, &Cfg::default());
        let names: Vec<(&str, FamilyClass, usize)> = r.families.iter().map(|f| (f.name.as_str(), f.class, f.files)).collect();
        assert_eq!(names, vec![("plural", FamilyClass::Verbatim, 3), ("render", FamilyClass::Verbatim, 2), ("repo_relative", FamilyClass::DifferentContract, 2)], "{names:?}");
        assert_eq!((r.totals.verbatim, r.totals.similar, r.totals.different_contract, r.totals.multi_file_names, r.totals.suppressed_twins), (2, 0, 1, 3, 0));
        let plural = &r.families[0];
        assert_eq!(plural.copies.iter().map(|c| c.relation.as_str()).collect::<Vec<_>>(), vec!["", "different contract", "verbatim"]);
        assert_eq!(plural.pairs.iter().map(|p| p.class).collect::<Vec<_>>(), vec![FamilyClass::DifferentContract, FamilyClass::Verbatim, FamilyClass::DifferentContract]);
        assert_eq!(r.files["src/cmd/flow.rs"].reasons, vec!["defines plural (lines 2-8), also defined as plural(n, noun) in src/cmd/proposal.rs:1 and verbatim in src/cmd/status.rs:3 - 3 copies; hoist one"]);
        assert_eq!(r.files["src/cmd/status.rs"].reasons, vec!["defines plural (lines 3-9), also defined verbatim in src/cmd/flow.rs:2 and as plural(n, noun) in src/cmd/proposal.rs:1 - 3 copies; hoist one"]);
        // The different-contract copy has no verbatim partner: section info, no reason.
        assert!(!r.files.contains_key("src/cmd/proposal.rs"));
        assert!(!r.files.contains_key("src/scan.rs"));
        assert!(r.families[2].line.starts_with("repo_relative: src/cmd/quirk.rs:1 repo_relative(ctx, p) -> String, src/scan.rs:1 repo_relative(ctx, p) -> Option<String> (Jaccard 0."), "{}", r.families[2].line);
        assert_eq!(r.families[0].line, "plural  3 copies in 3 files, verbatim (Jaccard 1.00): src/cmd/flow.rs:2-8, src/cmd/proposal.rs:1-3 (different contract), src/cmd/status.rs:3-9 (verbatim)");
        assert_eq!(r.files["src/cache.rs"].helper_copies, 1);
        // A same-named function in another language never joins a family.
        let mixed = [src("src/cache.rs", cache), src("src/setup.rs", setup), src("assets/app.js", "function render() { return 1 }\n")];
        assert_eq!(run(&mixed, &Cfg::default()).families[0].copies.len(), 2);
        assert_eq!(r.notes, vec!["attribution: off (no git history)"]);
        // `go` is under min_name_len; raising min_files hides everything.
        let mut cfg = Cfg { min_files: 4, ..Cfg::default() };
        assert!(run(&files, &cfg).families.is_empty());
        // Ignored names never form families.
        cfg.min_files = 2;
        cfg.ignore_names.push("plural".into());
        assert!(run(&files, &cfg).families.iter().all(|f| f.name != "plural"));
        // The same-signature-low-Jaccard class only with report_divergent.
        let files = [src("src/a.rs", "fn same_sig(x: u8) -> u8 { x + 1 }\n"), src("src/b.rs", "fn same_sig(y: u8) -> u8 { if y > 3 { y * 2 } else { 0 } }\n")];
        assert!(run(&files, &Cfg::default()).families.is_empty());
        let cfg = Cfg { report_divergent: true, ..Cfg::default() };
        let r = run(&files, &cfg);
        assert_eq!((r.families.len(), r.families[0].class, r.totals.divergent), (1, FamilyClass::Divergent, 1));
    }

    #[test]
    fn similar_families_and_the_match_over_types_annotation() {
        // Bare variants (a `use Enum::*` style match): the bodies are verbatim, the first
        // parameter types differ.
        let a = "pub fn status_word(s: Ticket) -> &'static str {\n    match s { Open => \"open\", Done => \"done\", Gone => \"gone\" }\n}\n";
        let b = "pub fn status_word(s: Decision) -> &'static str {\n    match s { Open => \"open\", Done => \"done\", Gone => \"gone\" }\n}\n";
        let r = run(&[src("src/a.rs", a), src("src/b.rs", b)], &Cfg::default());
        assert_eq!(r.families.len(), 1);
        let f = &r.families[0];
        assert_eq!((f.class, f.annotation.as_deref()), (FamilyClass::Verbatim, Some("same match over 2 types")));
        assert_eq!(r.files["src/a.rs"].reasons, vec!["defines status_word (lines 1-3), also defined verbatim in src/b.rs:1 - 2 copies (same match over 2 types); hoist one"]);
        // Scoped variants of different enums are not verbatim, and here not similar either:
        // every 5-gram touches a type segment. No annotation without a verbatim / similar pair.
        let c = "pub fn status_word(s: Ticket) -> &'static str {\n    match s { Ticket::Open => \"open\", Ticket::Done => \"done\", Ticket::Gone => \"gone\" }\n}\n";
        let d = c.replace("Ticket", "Decision");
        assert!(run(&[src("src/a.rs", c), src("src/b.rs", &d)], &Cfg::default()).families.is_empty());
        // Similar: one arm differs in a long body.
        let e = "fn scaffold(t: &Ticket) -> String {\n    let mut s = String::new();\n    s.push_str(&t.title);\n    s.push_str(\"\\n\");\n    s.push_str(&t.body);\n    s.push_str(\"\\n\");\n    s.push_str(&t.footer);\n    s\n}\n";
        let g = e.replace("t.footer", "t.trailer");
        let r = run(&[src("src/a.rs", e), src("src/b.rs", &g)], &Cfg::default());
        let f = &r.families[0];
        assert!(f.class == FamilyClass::Similar && f.best_jaccard >= 0.5 && f.best_jaccard < 1.0, "{f:?}");
        assert!(r.files["src/a.rs"].reasons[0].starts_with("defines scaffold (lines 1-9), also defined similarly (Jaccard 0."), "{:?}", r.files["src/a.rs"].reasons);
        assert!(r.files["src/a.rs"].reasons[0].ends_with(" - 2 copies; hoist one"));
        let strict = Cfg { min_body_jaccard: 0.99, ..Cfg::default() };
        assert!(run(&[src("src/a.rs", e), src("src/b.rs", &g)], &strict).families.is_empty());
    }

    #[test]
    fn twins_are_suppressed_by_basename_import_link_sibling_glob_and_cfg_gates() {
        let body = "pub fn twin_fn(x: u8) -> u8 { x + 1 }\n";
        // Same basename in different directories.
        let r = run(&[src("src/jsx/base.rs", body), src("src/jsx/dom/base.rs", body)], &Cfg::default());
        assert!(r.families.is_empty() && r.totals.suppressed_twins == 1, "{r:?}");
        let mut cfg = Cfg { suppress_path_suffix_twins: false, ..Cfg::default() };
        assert_eq!(run(&[src("src/jsx/base.rs", body), src("src/jsx/dom/base.rs", body)], &cfg).families.len(), 1);
        // A re-export wrapper: one file imports the name from the other.
        let wrapper = "use crate::util::twin_fn;\npub fn twin_fn(x: u8) -> u8 { x + 1 }\n";
        let root = src("src/main.rs", "mod util;\nmod wrap;\nfn main() {}\n");
        let r = run(&[src("src/util.rs", body), src("src/wrap.rs", wrapper), root.clone()], &Cfg::default());
        assert!(r.families.is_empty() && r.totals.suppressed_twins == 1, "{r:?}");
        // An unrelated import between the files is not a link.
        let other = "use crate::util::Other;\npub fn twin_fn(x: u8) -> u8 { x + 1 }\n";
        assert_eq!(run(&[src("src/util.rs", body), src("src/wrap.rs", other), root.clone()], &Cfg::default()).families.len(), 1);
        cfg.suppress_import_linked = false;
        assert_eq!(run(&[src("src/util.rs", body), src("src/wrap.rs", wrapper), root], &cfg).families.len(), 1);
        // Sibling directory globs.
        let r = run(&[src("src/adapter/bun/ssg.ts", "export function twin_fn(x: number) { return x + 1 }\n"), src("src/helper/x.ts", "export function twin_fn(x: number) { return x + 1 }\n")], &Cfg::default());
        assert!(r.families.is_empty() && r.totals.suppressed_twins == 1, "{r:?}");
        // cfg-gated twins never join.
        let gated = "#[cfg(windows)]\npub fn twin_fn(x: u8) -> u8 { x + 1 }\n";
        assert!(run(&[src("src/a.rs", body), src("src/b.rs", gated)], &Cfg::default()).families.is_empty());
        let gated_mod = "#[cfg(target_os = \"linux\")]\nmod imp { pub fn twin_fn(x: u8) -> u8 { x + 1 } }\n";
        assert!(run(&[src("src/a.rs", body), src("src/b.rs", gated_mod)], &Cfg::default()).families.is_empty());
        // Consts compare on raw text: two one-literal consts are not verbatim.
        let r = run(&[src("src/a.rs", "pub const TEMPLATE: &str = \"a\";\n"), src("src/b.rs", "pub const TEMPLATE: &str = \"b\";\n")], &Cfg::default());
        assert!(r.families.is_empty(), "{:?}", r.families);
        let r = run(&[src("src/a.rs", "pub const TEMPLATE: &str = \"a\";\n"), src("src/b.rs", "pub const TEMPLATE: &str = \"a\";\n")], &Cfg::default());
        assert_eq!(r.families[0].class, FamilyClass::Verbatim);
    }

    const IO_ERR: &str = "fn io_err(p: &Path, what: &str, e: std::io::Error) -> KsError {\n    KsError::internal(anyhow::anyhow!(\"cannot {what} {}: {e}\", p.display()))\n}\n";

    #[test]
    fn inlined_bodies_are_found_outside_definitions_and_the_reason_says_who_can_call() {
        let init = format!("pub fn init_it() {{}}\n{IO_ERR}");
        let store = format!("{IO_ERR}\nfn parent_of(p: &Path, what: &str, e: std::io::Error) -> Result<()> {{\n    Err(KsError::internal(anyhow::anyhow!(\"cannot {{what}} {{}}: {{e}}\", p.display())))\n}}\n");
        let lock = "fn acquire(path: &Path, what: &str) -> Result<()> {\n    std::fs::create_dir_all(path).map_err(|e| {\n        KsError::internal(anyhow::anyhow!(\"cannot {what} {}: {e}\", path.display()))\n    })?;\n    Ok(())\n}\n";
        let files = [src("src/cmd/init.rs", &init), src("src/store.rs", &store), src("src/lock.rs", lock)];
        let r = run(&files, &Cfg::default());
        assert_eq!(r.inlined.len(), 1, "{:?}", r.inlined);
        let i = &r.inlined[0];
        assert_eq!((i.name.as_str(), i.file.as_str(), i.tokens, i.files), ("io_err", "src/cmd/init.rs", 18, 2));
        assert_eq!(i.hits.iter().map(|h| (h.file.as_str(), h.line, h.reachable)).collect::<Vec<_>>(), vec![("src/lock.rs", 3, false), ("src/store.rs", 6, false)]);
        assert_eq!(i.also_defined, vec!["src/store.rs:1"]);
        assert_eq!(i.line, "body of io_err (src/cmd/init.rs:2-4, also defined at src/store.rs:1, 18 tokens) is inlined 2 times in 2 files: src/lock.rs:3, src/store.rs:6 - io_err is private; hoist and call");
        assert_eq!(r.files["src/lock.rs"].reasons, vec!["body of io_err (src/cmd/init.rs:2-4) is inlined 2 times in 2 files: src/lock.rs:3, src/store.rs:6 - io_err is private; hoist and call"]);
        assert_eq!((r.files["src/lock.rs"].inlined_idioms, r.files["src/store.rs"].inlined_idioms), (1, 1));
        assert_eq!((r.totals.small_helpers, r.totals.inlined_helpers), (3, 2));
        assert!((r.totals.inlined_share - 2.0 / 3.0).abs() < 1e-9);
        // A pub helper in the same crate: "instead of called".
        let pub_init = init.replace("fn io_err", "pub fn io_err");
        let r = run(&[src("src/cmd/init.rs", &pub_init), src("src/store.rs", &store), src("src/lock.rs", lock)], &Cfg::default());
        assert_eq!(r.files["src/lock.rs"].reasons, vec!["body of io_err (src/cmd/init.rs:2-4) is inlined 2 times in 2 files instead of called: src/lock.rs:3, src/store.rs:6"]);
        // Floors: occurrences, files, tokens, kinds; require_reachable drops private helpers' hits.
        let cfg = Cfg { min_occurrences: 3, ..Cfg::default() };
        assert!(run(&files, &cfg).inlined.is_empty());
        let cfg = Cfg { min_tokens: 19, ..Cfg::default() };
        assert!(run(&files, &cfg).inlined.is_empty());
        let cfg = Cfg { require_reachable: true, ..Cfg::default() };
        assert!(run(&files, &cfg).inlined.is_empty());
        let cfg = Cfg { inline_ignore_names: vec!["io_err".into()], ..Cfg::default() };
        assert!(run(&files, &cfg).inlined.is_empty());
        // Both hits in one file: under min_files.
        let two = format!("{lock}fn again(path: &Path, what: &str, e: u8) -> Result<()> {{\n    Err(KsError::internal(anyhow::anyhow!(\"cannot {{what}} {{}}: {{e}}\", path.display())))\n}}\n");
        let r = run(&[src("src/cmd/init.rs", &init), src("src/lock.rs", &two)], &Cfg::default());
        assert!(r.inlined.is_empty(), "{:?}", r.inlined);
    }

    #[test]
    fn plural_needs_the_wildcard_and_a_lower_floor_and_test_regions_are_skipped() {
        let flow = format!("pub fn flow_it() {{}}\n{PLURAL}");
        let done = "fn done(triaged: usize) -> String {\n    format!(\"{} step{}\", triaged, if triaged == 1 { \"\" } else { \"s\" })\n}\n";
        let init = "fn init(ahead: usize) -> String {\n    format!(\"{} commit{}\", ahead, if ahead == 1 { \"\" } else { \"s\" })\n}\n";
        let triage = "fn triage(open: &[u8], e: &E) -> String {\n    format!(\"{}{}\", if open.len() == 1 { \"\" } else { \"s\" }, if e.rules == 1 { \"\" } else { \"s\" })\n}\n#[cfg(test)]\nmod tests {\n    fn t(n: usize) -> &'static str { if n == 1 { \"\" } else { \"s\" } }\n}\n";
        let files = [src("src/cmd/flow.rs", &flow), src("src/cmd/done.rs", done), src("src/cmd/init.rs", init), src("src/triage.rs", triage)];
        // 11 tokens: under the default floor.
        let r = run(&files, &Cfg::default());
        assert!(r.inlined.is_empty(), "{:?}", r.inlined);
        let mut cfg = Cfg { min_tokens: 8, ..Cfg::default() };
        let r = run(&files, &cfg);
        assert_eq!(r.inlined.len(), 1);
        assert_eq!(r.inlined[0].hits.iter().map(|h| format!("{}:{}", h.file, h.line)).collect::<Vec<_>>(), vec!["src/cmd/done.rs:2", "src/cmd/init.rs:2"]);
        cfg.allow_one_wildcard = true;
        let r = run(&files, &cfg);
        assert_eq!(r.inlined[0].hits.iter().map(|h| format!("{}:{}", h.file, h.line)).collect::<Vec<_>>(), vec!["src/cmd/done.rs:2", "src/cmd/init.rs:2", "src/triage.rs:2", "src/triage.rs:2"], "{:?}", r.inlined);
        assert_eq!(r.inlined[0].files, 3);
        assert_eq!(r.files["src/triage.rs"].inlined_idioms, 2);
    }

    #[test]
    fn attribution_reads_the_introducing_commit_and_its_session_trailer() {
        let dir = std::env::temp_dir().join(format!("scry-helpers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git").arg("-C").arg(&dir).args(args).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").output().unwrap();
            assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        git(&["init", "-q"]);
        std::fs::write(dir.join("src/a.rs"), PLURAL).unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "a\n\nClaude-Session: https://claude.ai/code/session_AAA"]);
        std::fs::write(dir.join("src/b.rs"), format!("pub fn other() {{}}\n{PLURAL}")).unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "b\n\nClaude-Session: https://claude.ai/code/session_BBB"]);
        std::fs::write(dir.join("src/a.rs"), format!("pub(crate) {PLURAL}")).unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "visibility"]);
        let files = [src("src/a.rs", &format!("pub(crate) {PLURAL}")), src("src/b.rs", &format!("pub fn other() {{}}\n{PLURAL}"))];
        let cfg = Cfg::default();
        let (sides, index) = index_all(&files, &cfg, &DeadCfg::default());
        let deps = crate::deps::build(&files, &crate::config::Deps::default());
        let r = analyze(&sides, &index, &deps, Some(&dir), &cfg);
        let f = &r.families[0];
        assert_eq!((f.commits, f.sessions), (2, 2), "{f:?}");
        assert_eq!(f.copies.iter().map(|c| c.session.as_deref()).collect::<Vec<_>>(), vec![Some("session_AAA"), Some("session_BBB")]);
        assert!(r.files["src/a.rs"].reasons[0].ends_with(" - 2 copies from 2 commits / 2 sessions; hoist one"), "{:?}", r.files["src/a.rs"].reasons);
        assert_eq!(r.notes, vec!["attribution: 2 git lookups"]);
        let capped = Cfg { max_git_lookups: 1, ..Cfg::default() };
        let r = analyze(&sides, &index, &deps, Some(&dir), &capped);
        assert_eq!((r.families[0].commits, r.totals.git_lookups_capped), (1, true));
        let off = Cfg { attribute_commits: false, ..Cfg::default() };
        let r = analyze(&sides, &index, &deps, Some(&dir), &off);
        assert_eq!(r.families[0].commits, 0);
        assert!(r.files["src/a.rs"].reasons[0].ends_with(" - 2 copies; hoist one"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
