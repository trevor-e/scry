//! Repeated literals across files: the same message text hand-written in several files, and
//! configuration literals (time formats, env names, paths, URLs, MIME types, numbers in a config
//! role) duplicated with no shared constant.
//!
//! Every LLM session writes the next-step hint and the error phrasing it needs without knowing
//! which module owns the constant, so one timestamp format ends up in four files and one hint in
//! ten. Prose literals in a message role are masked (holes, `%s`, digits), lowercased and grouped
//! by exact text across files; config literals are classed by regex and grouped by text (numbers
//! by folded role name); near-duplicate prose pairs are information only.

use crate::config::Strings as Cfg;
use crate::discover::SourceFile;
use crate::lang::Language;
use crate::regions::{self, TestRegion};
use rayon::prelude::*;
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use tree_sitter::Node;

// ---------- literals ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    /// A message-role string: masked text is the family key.
    Prose,
    /// A config literal: class + text (numbers: class + value + folded role) is the key.
    Config,
}

/// One kept literal of a Source file.
#[derive(Debug, Clone, Serialize)]
pub struct Lit {
    pub line: usize,
    /// The literal as written between its delimiters (holes and escapes as in the source).
    pub text: String,
    /// Prose: masked, lowercased text. Config: the text (numbers: the canonical value).
    pub key: String,
    pub class: Class,
    /// `strftime`, `env_name`, `path`, `url`, `mime` or `number`.
    pub config_class: Option<String>,
    /// Folded role name (`fetchmaxagesecs`) for a number; the role it sits in for a string.
    pub role: Option<String>,
    /// The const / static / top-level `const` / module-level ALL_CAPS name this literal is the
    /// value of, when it is.
    pub named_const: Option<String>,
    /// Whitespace-separated words of the masked text.
    pub words: usize,
}

/// A file's kept literals, collected on the metrics pass's tree (`scan`) or one parse (`scry strings`).
#[derive(Debug, Clone)]
pub struct FileSide {
    pub path: String,
    pub literals: Vec<Lit>,
    /// Literals that passed the prose rule whatever their role: the share's denominator.
    pub prose_shaped: usize,
}

fn text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    node.utf8_text(src).unwrap_or("")
}

fn is_comment(kind: &str) -> bool {
    matches!(kind, "comment" | "line_comment" | "block_comment")
}

/// Named function definitions: the message-role search stops there. Closures, lambdas and
/// arrow functions are not boundaries (`unwrap_or_else(|| "kanspec doctor".into())` inside a
/// `format!` argument is still that argument).
fn is_function(kind: &str, lang: Language) -> bool {
    match lang {
        Language::Rust => kind == "function_item",
        Language::Python => kind == "function_definition",
        _ => matches!(kind, "function_declaration" | "generator_function_declaration" | "function_expression" | "function" | "generator_function" | "method_definition"),
    }
}

/// `console.log`, `self.logger.info`, `log::warn`, `expect(a).toBe` -> their name pieces.
fn segments(callee: &str) -> Vec<&str> {
    callee.split(|c: char| c == ':' || c == '.' || c == '(' || c == ')' || c == '[' || c == ']' || c == '<' || c == '>' || c.is_whitespace()).filter(|s| !s.is_empty()).collect()
}

/// The literal's text between its delimiters: Rust `b"…"` / `r#"…"#` prefixes and quotes,
/// Python `f"…"` / `'''…'''` start and end tokens, TS quotes and backticks.
fn inner_text<'a>(node: Node, src: &'a [u8], lang: Language) -> &'a str {
    let t = text(node, src);
    match lang {
        Language::Python => {
            let mut c = node.walk();
            let mut start = node.start_byte();
            let mut end = node.end_byte();
            for ch in node.children(&mut c) {
                match ch.kind() {
                    "string_start" => start = ch.end_byte(),
                    "string_end" => end = ch.start_byte(),
                    _ => {}
                }
            }
            std::str::from_utf8(&src[start..end]).unwrap_or("")
        }
        Language::Rust => {
            let open = t.find(['"', '\'']).unwrap_or(0);
            let quote = t.as_bytes().get(open).copied().unwrap_or(b'"') as char;
            let body = &t[open + 1..];
            let close = body.rfind(quote).unwrap_or(body.len());
            &body[..close]
        }
        _ => {
            // Char-safe: a MISSING closing quote leaves the text ending mid-literal, possibly on
            // a multi-byte char (`foo("héllö` parses as a string node).
            let Some(d) = t.chars().next() else { return t };
            let body = t.strip_prefix(d).unwrap_or(t);
            body.strip_suffix(d).unwrap_or(body)
        }
    }
}

/// A call's callee, memoised per call node: its text when short (a gettext callee is a bare
/// `_` / `t` / `i18n.t`, `require`, `import`) and its name pieces, read off the tree: the field /
/// member / attribute names down the callee, the innermost name's segments, and before them
/// the pieces of the receiver call's own callee (`expect(a).toBe` -> [expect, toBe], the
/// method last). In a builder chain the k-th callee spans the whole chain, so the receiver's
/// pieces are carried as a set and the text is never taken: a literal deep in the chain costs
/// its distinct names, not the chain's length.
#[derive(Clone)]
struct Callee<'s> {
    short: &'s str,
    pieces: Rc<Vec<&'s str>>,
}
type Callees<'s> = HashMap<usize, Callee<'s>>;

fn callee_node(call: Node<'_>) -> Option<Node<'_>> {
    call.child_by_field_name("function").or_else(|| call.child_by_field_name("constructor"))
}

/// A callee's own names, innermost first, and the receiver call it bottoms out at, if any.
fn own_pieces<'s, 't>(callee: Node<'t>, src: &'s [u8]) -> (Vec<&'s str>, Option<Node<'t>>) {
    let mut names: Vec<&'s str> = Vec::new();
    let mut cur = callee;
    let (pieces, recv) = loop {
        match cur.kind() {
            "field_expression" | "member_expression" | "attribute" => {
                let (name, obj) = match cur.kind() {
                    "field_expression" => ("field", "value"),
                    "member_expression" => ("property", "object"),
                    _ => ("attribute", "object"),
                };
                if let Some(f) = cur.child_by_field_name(name) {
                    names.push(text(f, src));
                }
                match cur.child_by_field_name(obj) {
                    Some(o) => cur = o,
                    None => break (Vec::new(), None),
                }
            }
            "generic_function" | "parenthesized_expression" | "non_null_expression" => match cur.child_by_field_name("function").or_else(|| cur.named_child(0)) {
                Some(o) => cur = o,
                None => break (Vec::new(), None),
            },
            "call_expression" | "call" | "new_expression" => break (Vec::new(), Some(cur)),
            _ => break (if cur.end_byte() - cur.start_byte() <= 200 { segments(text(cur, src)) } else { Vec::new() }, None),
        }
    };
    names.reverse();
    let mut out = pieces;
    out.extend(names);
    (out, recv)
}

/// The memoised callee of `call`, building the receiver chain bottom-up without recursion.
fn callee_facts<'s, 't>(callees: &mut Callees<'s>, call: Node<'t>, src: &'s [u8]) -> Callee<'s> {
    // (call, its callee, the callee's own pieces, the receiver call), outermost first.
    type Link<'s, 't> = (Node<'t>, Option<Node<'t>>, Vec<&'s str>, Option<Node<'t>>);
    let mut chain: Vec<Link<'s, 't>> = Vec::new();
    let mut cur = Some(call);
    while let Some(c) = cur {
        if callees.contains_key(&c.id()) {
            break;
        }
        let callee = callee_node(c);
        let (own, recv) = callee.map_or((Vec::new(), None), |n| own_pieces(n, src));
        chain.push((c, callee, own, recv));
        cur = recv;
    }
    for (c, callee, own, recv) in chain.into_iter().rev() {
        let mut pieces: Vec<&'s str> = recv.and_then(|r| callees.get(&r.id())).map(|r| r.pieces.as_ref().clone()).unwrap_or_default();
        for p in own {
            if !pieces.contains(&p) {
                pieces.push(p);
            }
        }
        let short = callee.filter(|n| n.end_byte() - n.start_byte() <= 64).map(|n| text(n, src)).unwrap_or("");
        callees.insert(c.id(), Callee { short, pieces: Rc::new(pieces) });
    }
    callees[&call.id()].clone()
}

/// Interpolation holes (`{..}`, `${..}`), `%s`-style specifiers and digit runs become one
/// placeholder; the result is lowercased with whitespace runs collapsed and trimmed.
pub fn mask(text: &str) -> String {
    let cs: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c == '{' {
            if cs.get(i + 1) == Some(&'{') {
                out.push('{');
                i += 2;
                continue;
            }
            if let Some(j) = cs[i + 1..].iter().position(|&x| x == '}' || x == '{' || x == '\n') && cs[i + 1 + j] == '}' {
                out.push('#');
                i += j + 2;
                continue;
            }
            out.push(c);
            i += 1;
            continue;
        }
        if c == '}' && cs.get(i + 1) == Some(&'}') {
            out.push('}');
            i += 2;
            continue;
        }
        if c == '$' && cs.get(i + 1) == Some(&'{') && let Some(j) = cs[i + 2..].iter().position(|&x| x == '}') {
            out.push('#');
            i += j + 3;
            continue;
        }
        if c == '%' {
            if cs.get(i + 1) == Some(&'%') {
                out.push('%');
                i += 2;
                continue;
            }
            let mut j = i + 1;
            while j < cs.len() && matches!(cs[j], '-' | '+' | ' ' | '#' | '0'..='9' | '.') {
                j += 1;
            }
            if j < cs.len() && cs[j].is_ascii_alphabetic() {
                out.push('#');
                i = j + 1;
                continue;
            }
            out.push('%');
            i += 1;
            continue;
        }
        if c.is_ascii_digit() {
            while i < cs.len() && cs[i].is_ascii_digit() {
                i += 1;
            }
            out.push('#');
            continue;
        }
        out.push(c);
        i += 1;
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// `fetch_max_age_secs` / `FetchMaxAge` -> `fetchmaxagesecs` / `fetchmaxage`.
fn fold(role: &str) -> String {
    role.chars().filter(|c| *c != '_' && *c != '-').flat_map(|c| c.to_lowercase()).collect()
}

fn is_all_caps(name: &str) -> bool {
    name.chars().any(|c| c.is_ascii_uppercase()) && !name.chars().any(|c| c.is_ascii_lowercase())
}

/// A numeric literal's canonical key and value: `300u64` -> ("300", 300), `1_000` -> ("1000",
/// 1000), `0x100` -> ("256", 256), `1.5` -> ("1.5", 1.5, float).
fn number_value(t: &str) -> Option<(String, f64, bool)> {
    let mut s: String = t.chars().filter(|c| *c != '_').collect::<String>().to_ascii_lowercase();
    let neg = s.starts_with('-');
    if neg {
        s.remove(0);
    }
    let sign = if neg { -1.0 } else { 1.0 };
    if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0o")).or_else(|| s.strip_prefix("0b")) {
        let radix = match &s[..2] { "0x" => 16, "0o" => 8, _ => 2 };
        let digits = rest.split(['u', 'i']).next().unwrap_or("");
        let v = i128::from_str_radix(digits, radix).ok()?;
        let v = if neg { -v } else { v };
        return Some((v.to_string(), v as f64, false));
    }
    for suf in ["u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize", "f32", "f64", "n"] {
        if let Some(b) = s.strip_suffix(suf) && !b.is_empty() && !b.ends_with('e') {
            s = b.to_string();
            break;
        }
    }
    if let Ok(v) = s.parse::<i128>() {
        let v = if neg { -v } else { v };
        return Some((v.to_string(), v as f64, false));
    }
    let v: f64 = s.parse().ok()?;
    Some((format!("{}{s}", if neg { "-" } else { "" }), sign * v, true))
}

/// What the ancestor walk learns about one literal.
#[derive(Default)]
struct Facts {
    excluded: bool,
    message: bool,
}

/// `fix!("…")` parsed as tokens inside another macro's `token_tree`: the tree's previous
/// siblings are `!` and the macro's name.
fn nested_macro_name<'a>(tt: Node, src: &'a [u8]) -> Option<&'a str> {
    let bang = tt.prev_sibling()?;
    if bang.kind() != "!" {
        return None;
    }
    let id = bang.prev_sibling()?;
    (id.kind() == "identifier").then(|| text(id, src))
}

/// The regex-backed literal collector; one per scan, shared across files.
pub struct Walker {
    cfg: Cfg,
    nonprose: Option<Regex>,
    ignore: Vec<Regex>,
    /// `(class, pattern)` in `config_classes` order, `number` left out.
    classes: Vec<(String, Regex)>,
    number_class: bool,
    macros: HashSet<String>,
    calls: HashSet<String>,
    gettext: HashSet<String>,
}

impl Walker {
    pub fn new(cfg: &Cfg) -> Self {
        let compile = |what: &str, p: &str| match Regex::new(p) {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!("warning: [strings] {what} {p:?} is not a valid regex ({e}); ignored");
                None
            }
        };
        let nonprose = if cfg.nonprose_regex.is_empty() { None } else { compile("nonprose_regex", &cfg.nonprose_regex) };
        let ignore = cfg.ignore_patterns.iter().filter_map(|p| compile("ignore_patterns entry", p)).collect();
        let mut classes = Vec::new();
        for c in &cfg.config_classes {
            if c == "number" {
                continue;
            }
            match cfg.config_patterns.get(c) {
                Some(p) => {
                    if let Some(r) = compile(&format!("config_patterns.{c}"), p) {
                        classes.push((c.clone(), r));
                    }
                }
                None => eprintln!("warning: [strings] config_classes names `{c}` but config_patterns has no pattern for it"),
            }
        }
        Self {
            cfg: cfg.clone(),
            nonprose,
            ignore,
            classes,
            number_class: cfg.config_classes.iter().any(|c| c == "number"),
            macros: cfg.message_macros.iter().cloned().collect(),
            calls: cfg.message_calls.iter().cloned().collect(),
            gettext: cfg.gettext_calls.iter().cloned().collect(),
        }
    }

    /// The literals of one file, on an already-parsed tree (`None` when parsing failed).
    pub fn file_side(&self, root: Option<Node>, file: &SourceFile, regions: &[TestRegion]) -> FileSide {
        let mut side = FileSide { path: file.path.clone(), literals: Vec::new(), prose_shaped: 0 };
        let Some(root) = root else { return side };
        let src = file.content.as_bytes();
        let lang = file.lang;
        let mut callees: Callees = HashMap::new();
        // One cursor, pre-order, with the ancestors of the current node on a stack: the role
        // and exclusion walks read them instead of `Node::parent()`, which starts from the
        // root each call and made a literal deep in a builder chain cost its depth squared.
        let mut cursor = root.walk();
        let mut anc: Vec<Node> = Vec::new();
        loop {
            let n = cursor.node();
            let kind = n.kind();
            // Subtrees nothing inside can rescue: skipped whole.
            let skip = is_comment(kind)
                || match lang {
                    Language::Rust => kind == "use_declaration" || (self.cfg.exclude_attributes && matches!(kind, "attribute_item" | "inner_attribute_item")) || regions::contains(regions, n.start_byte()),
                    Language::Python => matches!(kind, "import_statement" | "import_from_statement" | "future_import_statement") || (self.cfg.exclude_assert_calls && kind == "assert_statement"),
                    _ => kind == "import_statement" || (self.cfg.exclude_jsx_attributes && kind == "jsx_attribute") || (self.cfg.exclude_attributes && kind == "decorator"),
                };
            let (is_string, is_number) = match lang {
                Language::Rust => (kind == "string_literal" || (self.cfg.include_raw && kind == "raw_string_literal"), matches!(kind, "integer_literal" | "float_literal")),
                Language::Python => (matches!(kind, "string" | "concatenated_string"), matches!(kind, "integer" | "float")),
                _ => (matches!(kind, "string" | "template_string"), kind == "number"),
            };
            if !skip && is_string {
                let (lit, prose_shaped) = self.string_lit(n, &anc, src, lang, &mut callees);
                side.prose_shaped += usize::from(prose_shaped);
                side.literals.extend(lit);
            } else if !skip && is_number && let Some(l) = self.number_lit(n, &anc, src, lang, &mut callees) {
                side.literals.push(l);
            }
            if !skip && !is_string && !is_number && cursor.goto_first_child() {
                anc.push(n);
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    side.literals.sort_by_key(|l| l.line);
                    return side;
                }
                anc.pop();
            }
        }
    }

    /// One parse per file, for `scry strings`.
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

    /// Exclusions and the message role, from the ancestors of `lit` (`anc`: root first).
    fn facts<'s>(&self, lit: Node, anc: &[Node], src: &'s [u8], lang: Language, callees: &mut Callees<'s>) -> Facts {
        let mut f = Facts::default();
        let cfg = &self.cfg;
        let is_assert = |s: &str| s.starts_with("assert") || s.starts_with("debug_assert");
        // Python docstring: the first statement of a module, class or function body.
        let up = |i: usize| anc.len().checked_sub(i).and_then(|j| anc.get(j)).copied();
        if lang == Language::Python && cfg.exclude_docstrings && let Some(es) = up(1) && es.kind() == "expression_statement" && let Some(body) = up(2) {
            let container = body.kind() == "module" || (body.kind() == "block" && up(3).is_some_and(|p| matches!(p.kind(), "function_definition" | "class_definition")));
            if container {
                let mut c = body.walk();
                let first = body.named_children(&mut c).find(|ch| !is_comment(ch.kind()));
                if first == Some(es) {
                    f.excluded = true;
                    return f;
                }
            }
        }
        let mut in_function = false;
        let mut child = lit;
        let parent = up(1).unwrap_or(lit);
        for &p in anc.iter().rev() {
            let k = p.kind();
            match lang {
                Language::Rust => match k {
                    "use_declaration" => f.excluded = true,
                    "attribute_item" | "inner_attribute_item" if cfg.exclude_attributes => f.excluded = true,
                    "macro_invocation" => {
                        let name = p.child_by_field_name("macro").map(|m| text(m, src)).unwrap_or("");
                        self.macro_role(name, &mut f, in_function, is_assert);
                    }
                    "token_tree" => {
                        if let Some(name) = nested_macro_name(p, src) {
                            self.macro_role(name, &mut f, in_function, is_assert);
                        }
                    }
                    "return_expression" if !in_function => f.message = true,
                    "call_expression" => {
                        let c = callee_facts(callees, p, src);
                        let (callee, segs) = (c.short, c.pieces.as_slice());
                        if child.kind() == "arguments" && child == parent && self.gettext.contains(callee) {
                            f.excluded = true;
                        }
                        if !in_function && (segs.last() == Some(&"Err") || segs.iter().any(|s| self.calls.contains(*s))) {
                            f.message = true;
                        }
                    }
                    _ => {}
                },
                Language::Python => match k {
                    "import_statement" | "import_from_statement" | "future_import_statement" => f.excluded = true,
                    "assert_statement" if cfg.exclude_assert_calls => f.excluded = true,
                    "raise_statement" if !in_function && self.calls.contains("raise") => f.message = true,
                    "return_statement" if !in_function => f.message = true,
                    "call" => {
                        let c = callee_facts(callees, p, src);
                        let (callee, segs) = (c.short, c.pieces.as_slice());
                        if cfg.exclude_assert_calls && segs.last().is_some_and(|s| is_assert(s)) {
                            f.excluded = true;
                        }
                        if child.kind() == "argument_list" && child == parent && self.gettext.contains(callee) {
                            f.excluded = true;
                        }
                        if !in_function && segs.iter().any(|s| self.calls.contains(*s)) {
                            f.message = true;
                        }
                    }
                    _ => {}
                },
                _ => match k {
                    "import_statement" => f.excluded = true,
                    "export_statement" if child == lit && p.child_by_field_name("source") == Some(lit) => f.excluded = true,
                    // `declare module '../..' {`: an ambient module name is a module specifier.
                    "module" | "internal_module" if child == lit && p.child_by_field_name("name") == Some(lit) => f.excluded = true,
                    "jsx_attribute" if cfg.exclude_jsx_attributes => f.excluded = true,
                    "decorator" if cfg.exclude_attributes => f.excluded = true,
                    "throw_statement" if !in_function && self.calls.contains("throw") => f.message = true,
                    "return_statement" if !in_function => f.message = true,
                    "call_expression" | "new_expression" => {
                        let c = callee_facts(callees, p, src);
                        let (callee, segs) = (c.short, c.pieces.as_slice());
                        if child.kind() == "arguments" && child == parent {
                            if matches!(callee, "require" | "import") || self.gettext.contains(callee) {
                                f.excluded = true;
                            }
                            if cfg.exclude_assert_calls && segs.iter().any(|s| s.starts_with("expect") || is_assert(s)) {
                                f.excluded = true;
                            }
                        }
                        if !in_function && segs.iter().any(|s| self.calls.contains(*s)) {
                            f.message = true;
                        }
                    }
                    _ => {}
                },
            }
            if is_function(k, lang) {
                in_function = true;
            }
            child = p;
        }
        f
    }

    fn macro_role(&self, name: &str, f: &mut Facts, in_function: bool, is_assert: impl Fn(&str) -> bool) {
        let segs: Vec<&str> = name.split("::").collect();
        let last = segs.last().copied().unwrap_or("");
        if self.cfg.exclude_assert_calls && is_assert(last) {
            f.excluded = true;
        }
        if !in_function && (self.macros.contains(last) || segs.first().is_some_and(|s| self.calls.contains(*s))) {
            f.message = true;
        }
    }

    /// The config role a value sits in: `(role name, named const)`. `skip_calls` lets a number
    /// look through `Duration::from_secs(300)` to the field it initialises; a builder call
    /// (`.timeout(300)`) is the role itself.
    fn role_of(v: Node, anc: &[Node], src: &[u8], lang: Language, skip_calls: bool) -> (Option<String>, Option<String>) {
        let mut v = v;
        let mut i = anc.len();
        loop {
            if i == 0 {
                return (None, None);
            }
            i -= 1;
            let p = anc[i];
            let k = p.kind();
            let is_value = |field: &str| p.child_by_field_name(field) == Some(v);
            let name = |field: &str| p.child_by_field_name(field).map(|n| text(n, src).to_string());
            match (lang, k) {
                (Language::Rust, "const_item" | "static_item") if is_value("value") => return (name("name"), name("name")),
                (Language::Rust, "field_initializer") if is_value("value") => return (name("field"), None),
                (Language::Python, "keyword_argument" | "default_parameter" | "typed_default_parameter") if is_value("value") => return (name("name"), None),
                (Language::Python, "assignment") if is_value("right") => {
                    // Only a module-level assignment is a config role; a local is not.
                    let module_level = i >= 2 && anc[i - 2].kind() == "module";
                    if !module_level {
                        return (None, None);
                    }
                    let left = p.child_by_field_name("left").filter(|l| l.kind() == "identifier").map(|l| text(l, src).to_string());
                    let named = left.clone().filter(|l| is_all_caps(l));
                    return (left, named);
                }
                (Language::Rust | Language::Python, _) if matches!(k, "arguments" | "argument_list") => {}
                (Language::Rust | Language::Python, _) => return (None, None),
                (_, "pair") if is_value("value") => return (name("key").map(|n| n.trim_matches(['"', '\'']).to_string()), None),
                (_, "variable_declarator") if is_value("value") => {
                    // Only a top-level `const` is a config role; a local or a `let` is not.
                    let decl = (i >= 1).then(|| anc[i - 1]);
                    let is_const = decl.and_then(|d| d.child_by_field_name("kind")).is_some_and(|kk| text(kk, src) == "const");
                    let top = i >= 2 && matches!(anc[i - 2].kind(), "program" | "export_statement");
                    if !(is_const && top) {
                        return (None, None);
                    }
                    let n = name("name");
                    return (n.clone(), n);
                }
                (_, "arguments") => {}
                _ => return (None, None),
            }
            // `v` is inside the arguments of a call: a builder method names the role; a plain
            // call is looked through for numbers only.
            if i == 0 {
                return (None, None);
            }
            let call = anc[i - 1];
            let callee = call.child_by_field_name("function").or_else(|| call.child_by_field_name("constructor"));
            match callee.map(|c| c.kind()) {
                Some("field_expression") => return (callee.and_then(|c| c.child_by_field_name("field")).map(|n| text(n, src).to_string()), None),
                Some("member_expression") => return (callee.and_then(|c| c.child_by_field_name("property")).map(|n| text(n, src).to_string()), None),
                Some("attribute") => return (callee.and_then(|c| c.child_by_field_name("attribute")).map(|n| text(n, src).to_string()), None),
                _ if skip_calls => {
                    v = call;
                    i -= 1;
                }
                _ => return (None, None),
            }
        }
    }

    fn ignored(&self, raw: &str, masked: &str) -> bool {
        self.ignore.iter().any(|r| r.is_match(raw) || r.is_match(masked))
    }

    /// The literal, if kept, and whether it was prose-shaped (length, words, whitespace, not
    /// `nonprose_regex`) whatever its role.
    fn string_lit<'s>(&self, n: Node, anc: &[Node], src: &'s [u8], lang: Language, callees: &mut Callees<'s>) -> (Option<Lit>, bool) {
        // Python's adjacent-string concatenation is one literal.
        let raw: String = if n.kind() == "concatenated_string" {
            let mut c = n.walk();
            n.children(&mut c).filter(|ch| ch.kind() == "string").map(|ch| inner_text(ch, src, lang)).collect()
        } else {
            inner_text(n, src, lang).to_string()
        };
        let raw = raw.as_str();
        if raw.trim().is_empty() {
            return (None, false);
        }
        let f = self.facts(n, anc, src, lang, callees);
        if f.excluded {
            return (None, false);
        }
        let masked = mask(raw);
        if self.ignored(raw, &masked) {
            return (None, false);
        }
        let line = n.start_position().row + 1;
        let words = masked.split_whitespace().count();
        let cfg = &self.cfg;
        let shaped = masked.chars().count() >= cfg.min_len
            && words >= cfg.exact_min_words
            && masked.contains(' ')
            && !self.nonprose.as_ref().is_some_and(|r| r.is_match(raw));
        let prose = shaped && (!cfg.require_message_role || f.message);
        let (role, named_const) = Self::role_of(n, anc, src, lang, false);
        if prose {
            return (Some(Lit { line, text: raw.to_string(), key: masked, class: Class::Prose, config_class: None, role: role.map(|r| fold(&r)), named_const, words }), true);
        }
        let Some(class) = self.classes.iter().find(|(_, r)| r.is_match(raw)).map(|(c, _)| c.clone()) else { return (None, shaped) };
        (Some(Lit { line, text: raw.to_string(), key: raw.to_string(), class: Class::Config, config_class: Some(class), role: role.map(|r| fold(&r)), named_const, words }), shaped)
    }

    fn number_lit<'s>(&self, n: Node, anc: &[Node], src: &'s [u8], lang: Language, callees: &mut Callees<'s>) -> Option<Lit> {
        if !self.number_class {
            return None;
        }
        let mut v = n;
        let mut anc = anc;
        let mut t = text(n, src).to_string();
        if let Some(p) = anc.last() && matches!(p.kind(), "unary_expression" | "unary_operator") && text(*p, src).starts_with('-') {
            v = *p;
            anc = &anc[..anc.len() - 1];
            t = format!("-{t}");
        }
        let (key, value, is_float) = number_value(&t)?;
        if !is_float && value.abs() < self.cfg.number_min as f64 {
            return None;
        }
        if value.fract() == 0.0 && self.cfg.ignore_numbers.iter().any(|i| *i as f64 == value) {
            return None;
        }
        if self.ignored(&t, &key) {
            return None;
        }
        let (role, named_const) = Self::role_of(v, anc, src, lang, true);
        let role = role?;
        if self.facts(v, anc, src, lang, callees).excluded {
            return None;
        }
        Some(Lit { line: n.start_position().row + 1, text: t, key, class: Class::Config, config_class: Some("number".into()), role: Some(fold(&role)), named_const, words: 1 })
    }
}

/// One parse per Source file: the sides `analyze` takes, for `scry strings`.
pub fn index_all(files: &[SourceFile], cfg: &Cfg) -> Vec<FileSide> {
    let w = Walker::new(cfg);
    files.par_iter().filter(|f| f.kind == crate::discover::FileKind::Source).map(|f| w.parse_side(f)).collect()
}

// ---------- families ----------

/// `(file index, literal index)` into the sides.
type Ref = (usize, usize);
/// Family key `(class, key, role)` -> its sites.
type Groups = HashMap<(Class, String, Option<String>), Vec<Ref>>;
/// A distinct masked text's sorted word set and its sites.
type TextEntry<'a> = (Vec<&'a str>, Vec<Ref>);
/// The families a file has a site in, with that file's first line in each.
type FamRefs<'a> = Vec<(&'a Family, usize)>;

#[derive(Debug, Clone, Serialize)]
pub struct Site {
    pub file: String,
    pub line: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct NamedConst {
    pub name: String,
    pub file: String,
    pub line: usize,
}

/// One masked text (prose) or one config literal spelled in several files.
#[derive(Debug, Clone, Serialize)]
pub struct Family {
    pub class: Class,
    pub config_class: Option<String>,
    /// The first site's text as written.
    pub text: String,
    pub key: String,
    /// Numbers: the folded role name every site shares.
    pub role: Option<String>,
    pub files: usize,
    pub occurrences: usize,
    /// Sorted by file, then line.
    pub sites: Vec<Site>,
    /// The first occurrence that is the value of a named constant.
    pub named_const: Option<NamedConst>,
    /// The section line.
    pub line: String,
}

/// Two distinct prose texts with word-set Jaccard in `[near_jaccard, 1)`: information only.
#[derive(Debug, Clone, Serialize)]
pub struct NearPair {
    pub a: Site,
    pub text_a: String,
    pub b: Site,
    pub text_b: String,
    pub jaccard: f64,
    pub line: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FileStrings {
    /// Literal sites in exact prose or config families (plus near pairs unless `near_info_only`).
    pub family_literals: usize,
    pub prose_family_literals: usize,
    pub config_family_literals: usize,
    /// `(family text, line)` of every such site.
    pub literals: Vec<(String, usize)>,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    pub files: usize,
    /// Kept literals (prose in a message role, config-class strings, numbers in a config role).
    pub literals: usize,
    /// Prose-shaped literals in a message role (the ones kept).
    pub prose_literals: usize,
    /// Prose-shaped literals whatever their role.
    pub prose_shaped: usize,
    /// Prose literals in an exact family…
    pub prose_family_literals: usize,
    /// …over `prose_shaped`.
    pub prose_family_share: f64,
    pub prose_families: usize,
    pub config_literals: usize,
    pub config_family_literals: usize,
    pub config_families: usize,
    pub near_pairs: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct StringsReport {
    pub totals: Totals,
    pub notes: Vec<String>,
    /// Exact prose families: files desc, occurrences desc, text.
    pub families: Vec<Family>,
    /// Config-literal families, same order.
    pub config: Vec<Family>,
    /// Near-duplicate prose pairs: Jaccard desc, then texts.
    pub near: Vec<NearPair>,
    pub files: BTreeMap<String, FileStrings>,
}

fn class_label(class: &str) -> &str {
    match class {
        "strftime" => "timestamp pattern",
        "env_name" => "env name",
        "path" => "path",
        "url" => "URL",
        "mime" => "MIME type",
        "number" => "number",
        other => other,
    }
}

/// Display form of a literal: newlines escaped, cut to `max` chars with `…`.
fn display(text: &str, max: usize) -> String {
    let t = text.replace('\n', "\\n");
    if t.chars().count() <= max {
        return t;
    }
    let cut: String = t.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

fn join_and(items: &[String]) -> String {
    match items.len() {
        0 => String::new(),
        1 => items[0].clone(),
        n => format!("{} and {}", items[..n - 1].join(", "), items[n - 1]),
    }
}

/// `timestamp pattern "%Y-%m-%dT%H:%MZ"` / `number 300 as fetchmaxagesecs` / `"kanspec show {id}"`.
fn family_head(f: &Family, max: usize) -> String {
    match (&f.config_class, &f.role) {
        (Some(c), Some(role)) if c == "number" => format!("number {} as {role}", f.text),
        (Some(c), _) => format!("{} \"{}\"", class_label(c), display(&f.text, max)),
        (None, _) => format!("\"{}\"", display(&f.text, max)),
    }
}

fn sites_text(sites: &[Site], listed: usize) -> String {
    let shown: Vec<String> = sites.iter().take(listed).map(|s| format!("{}:{}", s.file, s.line)).collect();
    let more = sites.len().saturating_sub(listed);
    if more == 0 { shown.join(", ") } else { format!("{}, +{more} more", shown.join(", ")) }
}

fn build_families(groups: Groups, sides: &[FileSide], min_files: usize, cfg: &Cfg) -> Vec<Family> {
    let mut out = Vec::new();
    for ((class, key, role), mut refs) in groups {
        refs.sort_by(|a, b| sides[a.0].path.cmp(&sides[b.0].path).then(sides[a.0].literals[a.1].line.cmp(&sides[b.0].literals[b.1].line)));
        let files = refs.iter().map(|(fi, _)| fi).collect::<HashSet<_>>().len();
        if files < min_files {
            continue;
        }
        let lit = |r: &Ref| &sides[r.0].literals[r.1];
        let sites: Vec<Site> = refs.iter().map(|r| Site { file: sides[r.0].path.clone(), line: lit(r).line }).collect();
        let named_const = refs.iter().find_map(|r| lit(r).named_const.as_ref().map(|n| NamedConst { name: n.clone(), file: sides[r.0].path.clone(), line: lit(r).line }));
        let first = lit(&refs[0]);
        let mut f = Family { class, config_class: first.config_class.clone(), text: first.text.clone(), key, role, files, occurrences: refs.len(), sites, named_const, line: String::new() };
        let konst = match &f.named_const {
            Some(n) => format!("; const {} at {}:{}", n.name, n.file, n.line),
            None => String::new(),
        };
        f.line = format!("{:>2} files {:>3}x  {}  {}{konst}", f.files, f.occurrences, family_head(&f, cfg.display_text_len), sites_text(&f.sites, cfg.max_sites_listed));
        out.push(f);
    }
    out.sort_by(|a, b| b.files.cmp(&a.files).then(b.occurrences.cmp(&a.occurrences)).then(a.text.cmp(&b.text)).then(a.role.cmp(&b.role)));
    out
}

/// Near-duplicate prose pairs: distinct masked texts with `>= min_words` words, bucketed by
/// words with document frequency `<= rare_word_df`, compared pairwise, cross-file only.
fn near_pairs(sides: &[FileSide], cfg: &Cfg) -> Vec<NearPair> {
    // Distinct masked texts -> (word set, sites).
    let mut texts: HashMap<&str, TextEntry> = HashMap::new();
    for (fi, s) in sides.iter().enumerate() {
        for (li, l) in s.literals.iter().enumerate() {
            if l.class == Class::Prose && l.words >= cfg.min_words {
                let e = texts.entry(l.key.as_str()).or_insert_with(|| {
                    let mut w: Vec<&str> = l.key.split_whitespace().collect();
                    w.sort_unstable();
                    w.dedup();
                    (w, Vec::new())
                });
                e.1.push((fi, li));
            }
        }
    }
    let mut entries: Vec<(&str, TextEntry)> = texts.into_iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut df: HashMap<&str, usize> = HashMap::new();
    for (_, (words, _)) in &entries {
        for w in words {
            *df.entry(w).or_default() += 1;
        }
    }
    let mut buckets: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, (_, (words, _))) in entries.iter().enumerate() {
        for w in words {
            if df[w] <= cfg.rare_word_df {
                buckets.entry(w).or_default().push(i);
            }
        }
    }
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut out = Vec::new();
    for ids in buckets.values() {
        for (x, &i) in ids.iter().enumerate() {
            for &j in &ids[x + 1..] {
                let (i, j) = if i < j { (i, j) } else { (j, i) };
                if !seen.insert((i, j)) {
                    continue;
                }
                let (wa, wb) = (&entries[i].1.0, &entries[j].1.0);
                let inter = wa.iter().filter(|w| wb.binary_search(w).is_ok()).count();
                let union = wa.len() + wb.len() - inter;
                let jac = if union == 0 { 0.0 } else { inter as f64 / union as f64 };
                if jac < cfg.near_jaccard || jac >= 1.0 {
                    continue;
                }
                // Cross-file: some site of one lies in a file no site of the other does.
                let files_a: HashSet<usize> = entries[i].1.1.iter().map(|r| r.0).collect();
                let files_b: HashSet<usize> = entries[j].1.1.iter().map(|r| r.0).collect();
                if files_a == files_b && files_a.len() == 1 {
                    continue;
                }
                let site = |r: &Ref| Site { file: sides[r.0].path.clone(), line: sides[r.0].literals[r.1].line };
                let lit = |r: &Ref| &sides[r.0].literals[r.1];
                let (ra, rb) = (&entries[i].1.1[0], &entries[j].1.1[0]);
                let (a, b) = (site(ra), site(rb));
                let line = format!("{jac:.2}  {}:{} \"{}\"  ~  {}:{} \"{}\"", a.file, a.line, display(&lit(ra).text, cfg.display_text_len), b.file, b.line, display(&lit(rb).text, cfg.display_text_len));
                out.push(NearPair { text_a: lit(ra).text.clone(), text_b: lit(rb).text.clone(), a, b, jaccard: jac, line });
            }
        }
    }
    out.sort_by(|x, y| y.jaccard.partial_cmp(&x.jaccard).unwrap().then(x.text_a.cmp(&y.text_a)).then(x.text_b.cmp(&y.text_b)));
    out
}

pub fn analyze(sides: &[FileSide], cfg: &Cfg) -> StringsReport {
    let mut prose: Groups = HashMap::new();
    let mut config: Groups = HashMap::new();
    let mut totals = Totals { files: sides.len(), prose_shaped: sides.iter().map(|s| s.prose_shaped).sum(), ..Totals::default() };
    for (fi, s) in sides.iter().enumerate() {
        for (li, l) in s.literals.iter().enumerate() {
            totals.literals += 1;
            match l.class {
                Class::Prose => {
                    totals.prose_literals += 1;
                    prose.entry((Class::Prose, l.key.clone(), None)).or_default().push((fi, li));
                }
                Class::Config => {
                    totals.config_literals += 1;
                    let class = l.config_class.clone().unwrap_or_default();
                    // Numbers group by value and folded role; string classes by text alone.
                    let role = if class == "number" { l.role.clone() } else { None };
                    config.entry((Class::Config, format!("{class}\u{0}{}", l.key), role)).or_default().push((fi, li));
                }
            }
        }
    }
    let families = build_families(prose, sides, cfg.min_files, cfg);
    let config = build_families(config, sides, cfg.config_min_files, cfg);
    let near = near_pairs(sides, cfg);
    totals.prose_families = families.len();
    totals.config_families = config.len();
    totals.near_pairs = near.len();
    totals.prose_family_literals = families.iter().map(|f| f.occurrences).sum();
    totals.config_family_literals = config.iter().map(|f| f.occurrences).sum();
    totals.prose_family_share = if totals.prose_shaped == 0 { 0.0 } else { totals.prose_family_literals as f64 / totals.prose_shaped as f64 };
    // Other ways the repo spells a class (`repo also formats time 4 other ways`): distinct
    // texts of that class across every kept literal, minus the family's own.
    let mut class_texts: HashMap<&str, HashSet<&str>> = HashMap::new();
    for l in sides.iter().flat_map(|s| &s.literals) {
        if let Some(c) = &l.config_class && c != "number" {
            class_texts.entry(c.as_str()).or_default().insert(l.key.as_str());
        }
    }

    let mut files: BTreeMap<String, FileStrings> = BTreeMap::new();
    let mut per_file: HashMap<&str, (FamRefs, FamRefs)> = HashMap::new();
    for f in &families {
        for s in &f.sites {
            let e = per_file.entry(s.file.as_str()).or_default();
            if !e.0.iter().any(|(g, _)| std::ptr::eq(*g, f)) {
                e.0.push((f, s.line));
            }
            files.entry(s.file.clone()).or_default().literals.push((f.text.clone(), s.line));
        }
    }
    for f in &config {
        for s in &f.sites {
            let e = per_file.entry(s.file.as_str()).or_default();
            if !e.1.iter().any(|(g, _)| std::ptr::eq(*g, f)) {
                e.1.push((f, s.line));
            }
            files.entry(s.file.clone()).or_default().literals.push((f.text.clone(), s.line));
        }
    }
    if !cfg.near_info_only {
        for p in &near {
            for s in [&p.a, &p.b] {
                files.entry(s.file.clone()).or_default().literals.push((if std::ptr::eq(s, &p.a) { p.text_a.clone() } else { p.text_b.clone() }, s.line));
            }
        }
    }
    for (path, fs) in files.iter_mut() {
        fs.literals.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        fs.family_literals = fs.literals.len();
        let (prose_fams, config_fams) = per_file.get(path.as_str()).cloned().unwrap_or_default();
        fs.prose_family_literals = prose_fams.iter().map(|(f, _)| f.sites.iter().filter(|s| &s.file == path).count()).sum();
        fs.config_family_literals = config_fams.iter().map(|(f, _)| f.sites.iter().filter(|s| &s.file == path).count()).sum();
        let mut reasons = Vec::new();
        if let Some((best, line)) = prose_fams.first() {
            reasons.push(format!(
                "{} of its strings recur elsewhere: \"{}\" (line {line}) is spelled in {} files ({}x) - centralise the hint and message text",
                fs.prose_family_literals, display(&best.text, cfg.display_text_len), best.files, best.occurrences
            ));
        }
        for (f, line) in &config_fams {
            let others: Vec<String> = f.sites.iter().filter(|s| &s.file != path).map(|s| format!("{}:{}", s.file, s.line)).collect();
            let tail = match &f.named_const {
                Some(n) => format!(" - use {} from {}:{}", n.name, n.file, n.line),
                None => " with no shared constant".to_string(),
            };
            let also = match f.config_class.as_deref() {
                Some("strftime") => match class_texts.get("strftime").map_or(0, |t| t.len().saturating_sub(1)) {
                    0 => String::new(),
                    1 => "; repo also formats time 1 other way".to_string(),
                    n => format!("; repo also formats time {n} other ways"),
                },
                _ => String::new(),
            };
            reasons.push(format!("{} (line {line}) is duplicated in {}{tail}{also}", family_head(f, cfg.display_text_len), join_and(&others)));
        }
        let total = reasons.len();
        if total > cfg.max_reported_per_file {
            reasons.truncate(cfg.max_reported_per_file);
            reasons.push(format!("(+{} more repeated-literal families in repeated_literals)", total - cfg.max_reported_per_file));
        }
        fs.reasons = reasons;
    }
    let notes = if cfg.near_info_only && !near.is_empty() { vec![format!("{} near-duplicate pairs (word-set Jaccard >= {}) are information only; they do not count toward family_literals", near.len(), cfg.near_jaccard)] } else { Vec::new() };
    StringsReport { totals, notes, families, config, near, files }
}

/// `760 message literals of 1256 prose, 192 in 52 exact families (15.3% of prose); 88 config literals, 31 in 12 families; 4 near-duplicate pairs`.
pub fn totals_line(r: &StringsReport) -> String {
    let t = &r.totals;
    format!(
        "{} message literals of {} prose, {} in {} exact families ({:.1}% of prose); {} config literals, {} in {} families; {} near-duplicate pairs",
        t.prose_literals, t.prose_shaped, t.prose_family_literals, t.prose_families, t.prose_family_share * 100.0, t.config_literals, t.config_family_literals, t.config_families, t.near_pairs
    )
}

pub fn render(r: &StringsReport, top: usize) -> String {
    use std::fmt::Write;
    let mut o = String::new();
    let _ = writeln!(o, "{} literals kept in {} files; {}", r.totals.literals, r.totals.files, totals_line(r));
    for n in &r.notes {
        let _ = writeln!(o, "note  {n}");
    }
    let _ = writeln!(o, "\nexact message families (files, occurrences):");
    if r.families.is_empty() {
        let _ = writeln!(o, "  none");
    }
    for f in r.families.iter().take(top) {
        let _ = writeln!(o, "  {}", f.line);
    }
    let _ = writeln!(o, "\nconfig literal families:");
    if r.config.is_empty() {
        let _ = writeln!(o, "  none");
    }
    for f in r.config.iter().take(top) {
        let _ = writeln!(o, "  {}", f.line);
    }
    let _ = writeln!(o, "\nnear-duplicate message pairs (information only):");
    if r.near.is_empty() {
        let _ = writeln!(o, "  none");
    }
    for p in r.near.iter().take(top) {
        let _ = writeln!(o, "  {}", p.line);
    }
    let _ = writeln!(o, "\nfiles (literals in families):");
    let mut rows: Vec<(&String, &FileStrings)> = r.files.iter().collect();
    rows.sort_by(|a, b| b.1.family_literals.cmp(&a.1.family_literals).then_with(|| a.0.cmp(b.0)));
    if rows.is_empty() {
        let _ = writeln!(o, "  none");
    }
    for (p, fs) in rows.iter().take(top) {
        let _ = writeln!(o, "  {:>5}  {p}", fs.family_literals);
        for r in &fs.reasons {
            let _ = writeln!(o, "         - {r}");
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::FileKind;
    use std::path::Path;

    fn src(path: &str, content: &str) -> SourceFile {
        let lang = Language::from_path(Path::new(path)).unwrap();
        SourceFile { path: path.into(), lang, kind: FileKind::Source, lines: content.lines().count(), bytes: content.len(), content: content.into() }
    }

    fn lits(path: &str, content: &str, cfg: &Cfg) -> Vec<Lit> {
        Walker::new(cfg).parse_side(&src(path, content)).literals
    }

    fn run(files: &[SourceFile], cfg: &Cfg) -> StringsReport {
        analyze(&index_all(files, cfg), cfg)
    }

    fn loose() -> Cfg {
        Cfg { min_len: 10, min_files: 2, ..Cfg::default() }
    }

    #[test]
    fn masking_replaces_holes_specifiers_and_digits_and_lowercases() {
        assert_eq!(mask("Kanspec show {id}"), "kanspec show #");
        assert_eq!(mask("hello {} world {x:?} {{lit}}"), "hello # world # {lit}");
        assert_eq!(mask("run ${cmd} at 12:30 with %s and %-5.2f and 100%%"), "run # at #:# with # and # and #%");
        assert_eq!(mask("  spaced   out\ttext  "), "spaced out text");
    }

    #[test]
    fn literals_are_collected_per_grammar_with_holes_masked() {
        let cfg = loose();
        let rs = "fn f(id: &str) {\n    println!(\"next: kanspec show {id} now\");\n    let x = r#\"raw text goes here\"#;\n    vec![fix!(\"nested macro text here {id}\")];\n    eprintln!(\"{}\", x);\n}\n";
        let l = lits("a.rs", rs, &Cfg { message_macros: vec!["println".into(), "fix".into()], ..cfg.clone() });
        assert_eq!(l.iter().map(|l| (l.text.as_str(), l.key.as_str())).collect::<Vec<_>>(), vec![("next: kanspec show {id} now", "next: kanspec show # now"), ("nested macro text here {id}", "nested macro text here #")]);
        // The raw string is not in a message role; a call inside a message macro still is.
        let l = lits("a.rs", "fn f() { format!(\"{}\", helper(\"argument text here\")); }\n", &cfg);
        assert_eq!(l.len(), 1);
        let py = "def f(a):\n    raise ValueError(f\"bad value {a} here\")\n    print(\"hello world there\")\n    logging.info(\"logged %s things\", a)\n    x = \"not a message here\"\n    warnings.warn(\n        \"'BaseCommand' is deprecated. Use\"\n        \" 'Command' instead.\",\n        DeprecationWarning,\n    )\n";
        let l = lits("b.py", py, &cfg);
        assert_eq!(l.iter().map(|l| l.key.as_str()).collect::<Vec<_>>(), vec!["bad value # here", "hello world there", "logged # things", "'basecommand' is deprecated. use 'command' instead."]);
        assert_eq!(l[3].text, "'BaseCommand' is deprecated. Use 'Command' instead.");
        let ts = "function f(a: string) {\n  console.log(`hello ${a} world`);\n  throw new Error('thrown message here');\n  const q = 'quiet text here';\n  return 'returned value here';\n}\n";
        let l = lits("c.ts", ts, &cfg);
        assert_eq!(l.iter().map(|l| l.key.as_str()).collect::<Vec<_>>(), vec!["hello # world", "thrown message here", "returned value here"]);
        // Rust return and Err values are message roles too.
        let l = lits("a.rs", "fn f() -> Result<(), String> { if x { return Err(\"plain return error\".into()); } Err(anyhow!(\"anyhow message here\")) }\n", &cfg);
        assert_eq!(l.len(), 2);
        // Without the role requirement every prose literal counts.
        let l = lits("a.rs", "fn f() { let x = \"quiet prose text\"; }\n", &Cfg { require_message_role: false, ..cfg.clone() });
        assert_eq!(l.len(), 1);
    }

    #[test]
    fn excluded_positions_never_yield_a_literal() {
        let cfg = Cfg { require_message_role: false, ..loose() };
        let rs = "use a::b;\n#[serde(rename = \"attribute string here\")]\nstruct S;\nfn f() {\n    assert!(x, \"assert message here\");\n    debug_assert_eq!(a, b, \"debug assert here\");\n    _(\"gettext id text here\");\n}\n#[cfg(test)]\nmod tests { fn t() { println!(\"inline test text here\"); } }\n";
        assert!(lits("a.rs", rs, &cfg).is_empty(), "{:?}", lits("a.rs", rs, &cfg));
        let py = "\"\"\"module docstring here\"\"\"\nfrom x import y\ndef f():\n    \"\"\"function docstring here\"\"\"\n    assert a, \"assert message here\"\n    self.assertEqual(a, \"unittest message here\")\n    gettext(\"gettext id here\")\nclass C:\n    'class docstring here'\n    def g(self):\n        return \"kept return text\"\n";
        let l = lits("b.py", py, &cfg);
        assert_eq!(l.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), vec!["kept return text"]);
        let ts = "import { x } from 'some/module here';\nexport { y } from 'other/module here';\nconst m = require('required module here');\nexpect(a).toBe('expected value here');\nassert.equal(a, 'assert value here');\nconst el = <div title=\"jsx attribute text\">kept</div>;\n@Component({ text: 'decorator text here' })\nclass C {}\ndeclare module 'ambient/module here' { interface X {} }\nfunction f() { return 'kept ts text here'; }\n";
        let l = lits("c.tsx", ts, &cfg);
        assert_eq!(l.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), vec!["kept ts text here"]);
        // Knobs turn the exclusions off.
        let on = Cfg { exclude_assert_calls: false, exclude_docstrings: false, exclude_attributes: false, exclude_jsx_attributes: false, ..cfg.clone() };
        assert_eq!(lits("b.py", py, &on).len(), 6);
        assert!(lits("c.tsx", ts, &on).iter().any(|l| l.text == "jsx attribute text"));
        assert!(!lits("c.tsx", ts, &on).iter().any(|l| l.text.contains("ambient")));
        // An unterminated TS string ending on a multi-byte char (a MISSING quote) is no panic.
        let broken = lits("d.ts", "foo(\"héllö wörld text hére\n)\n", &on);
        assert_eq!(broken.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), vec!["héllö wörld text hére"]);
        // A literal deep in a builder chain costs one ancestor walk, not the chain's text at
        // every level: 300 `.arg(Arg::new("...").help("..."))` calls stay well under a second.
        let mut chain = String::from("fn cli() -> Cmd {\n    Cmd::new(\"x\")\n");
        for i in 0..300 {
            chain.push_str(&format!("        .arg(Arg::new(\"flag{i}\").help(\"help text for flag number {i}\"))\n"));
        }
        chain.push_str("}\n");
        let started = std::time::Instant::now();
        let l = lits("e.rs", &chain, &Cfg { require_message_role: false, ..loose() });
        assert_eq!(l.len(), 300);
        assert!(started.elapsed().as_secs_f64() < 5.0, "{:?}", started.elapsed());
        assert!(lits("a.rs", rs, &on).iter().any(|l| l.text == "attribute string here"));
        // Test regions stay excluded either way; an ignore pattern drops what it matches.
        assert!(!lits("a.rs", rs, &on).iter().any(|l| l.text.contains("inline test")));
        let ign = Cfg { ignore_patterns: vec!["^kept".into()], ..cfg.clone() };
        assert!(lits("b.py", py, &ign).is_empty());
    }

    #[test]
    fn prose_rule_needs_length_words_whitespace_and_a_message_role() {
        let cfg = Cfg { min_len: 12, ..loose() };
        let rs = "fn f() {\n    println!(\"short text\");\n    println!(\"single-word-long-enough\");\n    println!(\"UPPER_CASE_CONST_TEXT\");\n    println!(\"content-type\");\n    println!(\"long enough prose text\");\n    println!(\"error {code} for {id}\");\n    helper(\"long enough but no role\");\n}\n";
        let prose = |cfg: &Cfg| lits("a.rs", rs, cfg).into_iter().filter(|l| l.class == Class::Prose).map(|l| l.text).collect::<Vec<_>>();
        assert_eq!(prose(&cfg), vec!["long enough prose text", "error {code} for {id}"]);
        // The ALL_CAPS one is not prose: it went to the env_name config class.
        assert_eq!(lits("a.rs", rs, &cfg).iter().filter(|l| l.class == Class::Config).map(|l| l.text.as_str()).collect::<Vec<_>>(), vec!["UPPER_CASE_CONST_TEXT"]);
        assert!(prose(&Cfg { min_len: 25, ..cfg.clone() }).is_empty());
        assert!(prose(&Cfg { exact_min_words: 5, ..cfg.clone() }).is_empty());
    }

    #[test]
    fn exact_families_need_min_files_and_sort_by_files_then_occurrences() {
        let cfg = loose();
        let hint = |extra: &str| format!("fn f(id: &str) {{\n    println!(\"kanspec show {{id}}\");\n{extra}}}\n");
        let files = [
            src("src/a.rs", &hint("    println!(\"kanspec show {}\", id);\n    println!(\"run kanspec doctor\");\n")),
            src("src/b.rs", &hint("    println!(\"run kanspec doctor\");\n")),
            src("src/c.rs", &hint("    eprintln!(\"Kanspec Show {other}\");\n    println!(\"only in this file text\");\n")),
            src("src/d.rs", "fn g() { println!(\"only in the d file text\"); println!(\"only in the d file text\"); }\n"),
        ];
        let r = run(&files, &cfg);
        assert_eq!(r.families.iter().map(|f| (f.text.as_str(), f.files, f.occurrences)).collect::<Vec<_>>(), vec![("kanspec show {id}", 3, 5), ("run kanspec doctor", 2, 2)]);
        assert_eq!(r.families[0].sites.iter().map(|s| format!("{}:{}", s.file, s.line)).collect::<Vec<_>>(), vec!["src/a.rs:2", "src/a.rs:3", "src/b.rs:2", "src/c.rs:2", "src/c.rs:3"]);
        assert_eq!(r.families[0].line, " 3 files   5x  \"kanspec show {id}\"  src/a.rs:2, src/a.rs:3, src/b.rs:2, src/c.rs:2, src/c.rs:3");
        assert_eq!(r.files["src/a.rs"].reasons, vec!["3 of its strings recur elsewhere: \"kanspec show {id}\" (line 2) is spelled in 3 files (5x) - centralise the hint and message text"]);
        assert_eq!(r.files["src/a.rs"].family_literals, 3);
        assert_eq!(r.files.get("src/d.rs").map_or(0, |f| f.family_literals), 0);
        assert_eq!((r.totals.prose_literals, r.totals.prose_shaped), (10, 10));
        assert_eq!((r.totals.prose_family_literals, r.totals.prose_families), (7, 2));
        assert!((r.totals.prose_family_share - 0.7).abs() < 1e-9);
        // A prose-shaped literal outside a message role widens the share's denominator only.
        let mut more = files.to_vec();
        more.push(src("src/e.rs", "fn h() { helper(\"prose shaped but no role\"); }\n"));
        let r5 = run(&more, &cfg);
        assert_eq!((r5.totals.prose_literals, r5.totals.prose_shaped), (10, 11));
        assert!((r5.totals.prose_family_share - 7.0 / 11.0).abs() < 1e-9);
        // A higher floor keeps only the three-file family.
        let r3 = run(&files, &Cfg { min_files: 3, ..cfg.clone() });
        assert_eq!(r3.families.len(), 1);
        assert_eq!(r3.files["src/b.rs"].family_literals, 1);
    }

    #[test]
    fn config_classes_match_by_text_and_numbers_by_folded_role() {
        let cfg = loose();
        let files = [
            src("src/a.rs", "const TS_FMT: &str = \"%Y-%m-%dT%H:%MZ\";\nconst OTHER: &str = \"%Y-%m-%d\";\nfn f() { cmd.env_remove(\"GIT_WORK_TREE\"); let c = Cfg { fetch_max_age_secs: 300, other: 240 }; let d = Duration::from_secs(300); }\n"),
            src("src/b.rs", "fn g() { let t = now.format(\"%Y-%m-%dT%H:%MZ\"); let e = std::env::var(\"GIT_WORK_TREE\"); let c = Cfg { fetch_max_age_secs: 300, cache_age_secs: 240 }; }\n"),
            src("src/c.py", "def h(fetch_max_age_secs=300, detail_max=240, url='https://example.com/api', mime='application/json'):\n    p = './cache/index.db'\n    q = 'kanspec show {id}'\n"),
            src("src/d.ts", "const cfg = { fetchMaxAgeSecs: 300, url: 'https://example.com/api', p: './cache/index.db', mime: 'application/json', small: 10, ignored: 100, f: 0.5 };\nconst other = { f: 0.5 };\n"),
        ];
        let r = run(&files, &cfg);
        let heads: Vec<String> = r.config.iter().map(|f| format!("{} {}f {}x", family_head(f, 80), f.files, f.occurrences)).collect();
        assert_eq!(heads, vec![
            "number 300 as fetchmaxagesecs 4f 4x",
            "timestamp pattern \"%Y-%m-%dT%H:%MZ\" 2f 2x",
            "path \"./cache/index.db\" 2f 2x",
            "env name \"GIT_WORK_TREE\" 2f 2x",
            "MIME type \"application/json\" 2f 2x",
            "URL \"https://example.com/api\" 2f 2x",
        ], "{heads:?}");
        // 240 sits in two files under different roles: no family. 0.5 twice in one file: no family.
        assert!(!heads.iter().any(|h| h.contains("240") || h.contains("0.5")));
        // A local variable is not a config role: Python function-local assignments and TS
        // `let` / function-local declarators never form a number family.
        let locals = [
            src("src/l1.py", "def f():\n    timeout = 300\n    return timeout\n"), src("src/l2.py", "def g():\n    timeout = 300\n    return timeout\n"),
            src("src/l1.ts", "function f() { let retries = 300; const inner = 300; return retries + inner }\n"), src("src/l2.ts", "function g() { let retries = 300; const inner = 300; return retries + inner }\n"),
            src("src/l3.py", "TIMEOUT = 300\n"), src("src/l4.py", "TIMEOUT = 300\n"),
        ];
        let heads: Vec<String> = run(&locals, &cfg).config.iter().map(|f| family_head(f, 80)).collect();
        assert_eq!(heads, vec!["number 300 as timeout"], "{heads:?}");
        let ts = r.config.iter().find(|f| f.config_class.as_deref() == Some("strftime")).unwrap();
        assert_eq!(ts.named_const.as_ref().map(|n| (n.name.as_str(), n.file.as_str(), n.line)), Some(("TS_FMT", "src/a.rs", 1)));
        assert_eq!(ts.line, " 2 files   2x  timestamp pattern \"%Y-%m-%dT%H:%MZ\"  src/a.rs:1, src/b.rs:1; const TS_FMT at src/a.rs:1");
        let b = &r.files["src/b.rs"].reasons;
        assert_eq!(b[0], "number 300 as fetchmaxagesecs (line 1) is duplicated in src/a.rs:3, src/c.py:1 and src/d.ts:1 with no shared constant");
        assert_eq!(b[1], "timestamp pattern \"%Y-%m-%dT%H:%MZ\" (line 1) is duplicated in src/a.rs:1 - use TS_FMT from src/a.rs:1; repo also formats time 1 other way");
        assert_eq!(b[2], "env name \"GIT_WORK_TREE\" (line 1) is duplicated in src/a.rs:3 with no shared constant");
        assert_eq!(r.files["src/b.rs"].family_literals, 3);
        // The per-file cap, then the remainder line.
        let capped = run(&files, &Cfg { max_reported_per_file: 2, ..cfg.clone() });
        assert_eq!(capped.files["src/d.ts"].reasons.len(), 3);
        assert_eq!(capped.files["src/d.ts"].reasons[2], "(+2 more repeated-literal families in repeated_literals)");
        // ignore_numbers and the class list are knobs.
        let no_num = run(&files, &Cfg { config_classes: vec!["strftime".into(), "env_name".into()], ..cfg.clone() });
        assert_eq!(no_num.config.len(), 2);
        let ign = run(&files, &Cfg { ignore_numbers: vec![300], ..cfg.clone() });
        assert!(!ign.config.iter().any(|f| f.text == "300"));
    }

    #[test]
    fn near_pairs_are_cross_file_within_the_jaccard_band_and_info_only() {
        let cfg = loose();
        let files = [
            src("src/a.rs", "fn f() { println!(\"knowledge check: branch touched {} spec {} scope {}\"); println!(\"totally unrelated message text here\"); }\n"),
            src("src/b.rs", "fn g() { println!(\"knowledge check: branch touched {} spec {} scope {} globs\"); println!(\"knowledge check branch touched spec scope\"); }\n"),
        ];
        let r = run(&files, &cfg);
        assert_eq!(r.near.iter().map(|p| (p.a.file.as_str(), p.a.line, p.b.file.as_str(), format!("{:.2}", p.jaccard))).collect::<Vec<_>>(), vec![("src/a.rs", 1, "src/b.rs", "0.88".into())]);
        assert_eq!(r.near[0].line, "0.88  src/a.rs:1 \"knowledge check: branch touched {} spec {} scope {}\"  ~  src/b.rs:1 \"knowledge check: branch touched {} spec {} scope {} globs\"");
        assert_eq!(r.totals.near_pairs, 1);
        assert!(r.families.is_empty());
        assert_eq!(r.files.get("src/a.rs").map_or(0, |f| f.family_literals), 0);
        assert!(r.notes[0].starts_with("1 near-duplicate pairs"));
        // Counted once the info-only switch is off; gone above the band or the word floor.
        let counted = run(&files, &Cfg { near_info_only: false, ..cfg.clone() });
        assert_eq!(counted.files["src/a.rs"].family_literals, 1);
        assert!(run(&files, &Cfg { near_jaccard: 0.9, ..cfg.clone() }).near.is_empty());
        assert!(run(&files, &Cfg { min_words: 11, ..cfg.clone() }).near.is_empty());
    }
}
