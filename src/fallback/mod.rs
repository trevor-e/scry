//! Parse-default fallbacks (P28): a literal default applied to the result of a fallible
//! transform.
//!
//! `fields.next().unwrap_or("")` and `t.parse().ok().unwrap_or(0)` are how a malformed input
//! line turns into author "" at timestamp 0 without a word: the failure is swallowed at the
//! one site that could have reported it. The measure counts, per unit, the fallback sites
//! whose receiver chain holds a syntactically fallible transform (`parse`, `split`, `next`,
//! `get`…) and whose default is a literal or an empty constructor; an `unwrap_or` on a struct
//! field has no call in its chain and never counts. Two such sites in one unit make a reason on
//! the hotspot; nothing here enters the score.

use crate::config::Fallback as Cfg;
use crate::lang::Language;
use crate::metrics::{self, FunctionMetrics};
use regex::Regex;
use serde::Serialize;
use tree_sitter::Node;

/// Characters of a default or binding kept in a site.
const TEXT_LEN: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SiteKind {
    /// A literal default on a receiver chain holding a fallible transform.
    ParseDefault,
    /// Any other value-swallowing default (`unwrap_or(other)`, `.ok()?`, `d.get(k, 0)`).
    Fallback,
}

/// One fallback site of a unit.
#[derive(Debug, Clone, Serialize)]
pub struct Site {
    pub line: usize,
    pub kind: SiteKind,
    /// The default as written (`""`, `0`, `Default::default()`; `?` for `.ok()?`), cut short.
    pub default: String,
    /// The fallible transform in the receiver chain that made it a parse default (`next`, `parse`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transform: Option<String>,
    /// The name the value lands in: a `let` pattern (the matching element of a tuple pattern),
    /// a declarator, an assignment target, a struct field or an object key; none for a value
    /// returned or passed on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding: Option<String>,
}

/// What a grammar-specific check found at a node: the default (literal text when it is one),
/// the default's raw text, and where the receiver chain starts.
struct Found<'t> {
    literal: Option<String>,
    raw: String,
    chain: Node<'t>,
}

fn text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    node.utf8_text(src).unwrap_or("")
}

fn cut(s: &str) -> String {
    if s.chars().count() <= TEXT_LEN { s.to_string() } else { format!("{}…", s.chars().take(TEXT_LEN - 1).collect::<String>()) }
}

/// The unit's name without its owner (`Repo.discover` → `discover`).
pub fn bare_name(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

fn named_args(args: Node) -> Vec<Node> {
    let mut c = args.walk();
    args.named_children(&mut c).filter(|a| a.kind() != "comment").collect()
}

/// The method or function a callee names: `x.parse` → `parse`, `t.parse::<i64>` → `parse`,
/// `std::str::from_utf8` → `from_utf8`, `JSON.parse` → `parse`.
fn callee_name<'a>(f: Node, src: &'a [u8]) -> Option<&'a str> {
    match f.kind() {
        "identifier" => Some(text(f, src)),
        "field_expression" => f.child_by_field_name("field").map(|x| text(x, src)),
        "generic_function" => f.child_by_field_name("function").and_then(|g| match g.kind() {
            "field_expression" => g.child_by_field_name("field").map(|x| text(x, src)),
            "scoped_identifier" => g.child_by_field_name("name").map(|x| text(x, src)),
            "identifier" => Some(text(g, src)),
            _ => None,
        }),
        "scoped_identifier" => f.child_by_field_name("name").map(|x| text(x, src)),
        "attribute" => f.child_by_field_name("attribute").map(|x| text(x, src)),
        "member_expression" => f.child_by_field_name("property").map(|x| text(x, src)),
        _ => None,
    }
}

/// The first fallible transform met walking the receiver chain down from `start`: the value of
/// a field access, the function of a call, the operand of `?` / parentheses / `&` / `await`,
/// the object of a member or subscript. Arguments are never entered: `a.unwrap_or(b.next())`
/// has `next` in an argument, not in the chain.
fn transform_in_chain(start: Node, lang: Language, src: &[u8], cfg: &Cfg) -> Option<String> {
    let names: &[String] = match lang {
        Language::Rust => &cfg.parse_calls,
        Language::Python => &cfg.parse_calls_python,
        _ => &cfg.parse_calls_ts,
    };
    let mut cur = Some(start);
    while let Some(n) = cur {
        let kind = n.kind();
        if matches!(kind, "call_expression" | "call") {
            let f = n.child_by_field_name("function");
            if let Some(name) = f.and_then(|f| callee_name(f, src))
                && names.iter().any(|p| p == name)
            {
                return Some(name.to_string());
            }
            cur = f;
            continue;
        }
        cur = match (lang, kind) {
            (Language::Rust, "field_expression") => n.child_by_field_name("value"),
            (Language::Rust, "generic_function") => n.child_by_field_name("function"),
            (Language::Rust, "try_expression" | "parenthesized_expression" | "await_expression" | "reference_expression" | "unary_expression") => n.named_child(0),
            (Language::Python, "attribute") => n.child_by_field_name("object"),
            (Language::Python, "subscript") => n.child_by_field_name("value"),
            (Language::Python, "parenthesized_expression" | "await") => n.named_child(0),
            (Language::Rust | Language::Python, _) => None,
            (_, "member_expression" | "subscript_expression") => n.child_by_field_name("object"),
            (_, "parenthesized_expression" | "non_null_expression" | "as_expression" | "satisfies_expression" | "await_expression") => n.named_child(0),
            _ => None,
        };
    }
    None
}

/// The literal a Rust default is, as text: a literal token (behind `&` / parentheses / a
/// minus), `()`, `[]`, `None`, an `empty_constructors` call, or (for `unwrap_or_else`) a
/// closure returning one or the bare constructor path.
fn rust_literal(arg: Node, src: &[u8], cfg: &Cfg) -> Option<String> {
    let mut n = arg;
    loop {
        match n.kind() {
            "integer_literal" | "float_literal" | "string_literal" | "raw_string_literal" | "char_literal" | "boolean_literal" | "unit_expression" => {
                return Some(text(n, src).to_string());
            }
            "reference_expression" | "parenthesized_expression" => n = n.named_child(0)?,
            "unary_expression" => {
                let inner = n.named_child(0)?;
                return matches!(inner.kind(), "integer_literal" | "float_literal").then(|| text(n, src).to_string());
            }
            "array_expression" => return (n.named_child_count() == 0).then(|| "[]".to_string()),
            "identifier" => return (text(n, src) == "None").then(|| "None".to_string()),
            "scoped_identifier" => {
                let t = text(n, src);
                return cfg.empty_constructors.iter().any(|c| c == t).then(|| format!("{t}()"));
            }
            "call_expression" => {
                let f = n.child_by_field_name("function")?;
                let t = text(f, src);
                let empty = n.child_by_field_name("arguments").is_some_and(|a| a.named_child_count() == 0);
                return (empty && cfg.empty_constructors.iter().any(|c| c == t)).then(|| format!("{t}()"));
            }
            "closure_expression" => {
                let body = n.child_by_field_name("body")?;
                n = if body.kind() == "block" {
                    let inner = named_args(body);
                    if inner.len() != 1 {
                        return None;
                    }
                    inner[0]
                } else {
                    body
                };
            }
            _ => return None,
        }
    }
}

fn python_literal(arg: Node, src: &[u8]) -> Option<String> {
    let mut n = arg;
    loop {
        match n.kind() {
            "dictionary" | "list" | "tuple" | "set" | "none" | "string" | "concatenated_string" | "integer" | "float" | "true" | "false" => {
                return Some(text(n, src).to_string());
            }
            "parenthesized_expression" => n = n.named_child(0)?,
            "unary_operator" => {
                let inner = n.named_child(0)?;
                return matches!(inner.kind(), "integer" | "float").then(|| text(n, src).to_string());
            }
            _ => return None,
        }
    }
}

fn ts_literal(arg: Node, src: &[u8]) -> Option<String> {
    let mut n = arg;
    loop {
        match n.kind() {
            "object" | "array" | "string" | "template_string" | "number" | "null" | "undefined" | "true" | "false" => {
                return Some(text(n, src).to_string());
            }
            "parenthesized_expression" | "as_expression" | "satisfies_expression" => n = n.named_child(0)?,
            "unary_expression" => {
                let inner = n.named_child(0)?;
                return (inner.kind() == "number").then(|| text(n, src).to_string());
            }
            _ => return None,
        }
    }
}

/// A Rust site: a `methods_rust` call (`x.unwrap_or(0)`, `x.unwrap_or_else(Vec::new)`,
/// `x.or_default()`), or `.ok()?`. A bare `.ok()` anywhere else is not a site, so
/// `x.parse().ok().unwrap_or(0)` is one site at the `unwrap_or`.
fn rust_site<'t>(node: Node<'t>, src: &[u8], cfg: &Cfg) -> Option<Found<'t>> {
    match node.kind() {
        "call_expression" => {
            let f = node.child_by_field_name("function")?;
            if f.kind() != "field_expression" {
                return None;
            }
            let method = text(f.child_by_field_name("field")?, src);
            if !cfg.methods_rust.iter().any(|m| m == method) {
                return None;
            }
            let chain = f.child_by_field_name("value")?;
            let arg = node.child_by_field_name("arguments").and_then(|a| named_args(a).first().copied());
            let (literal, raw) = match (method, arg) {
                (_, Some(a)) => (rust_literal(a, src, cfg), text(a, src).to_string()),
                // `unwrap_or_default` / `or_default`: the default is the type's.
                (_, None) => (Some("Default::default()".to_string()), "Default::default()".to_string()),
            };
            Some(Found { literal, raw, chain })
        }
        "try_expression" => {
            let inner = node.named_child(0).filter(|i| i.kind() == "call_expression")?;
            let f = inner.child_by_field_name("function").filter(|f| f.kind() == "field_expression")?;
            (callee_name(f, src) == Some("ok")).then(|| Found { literal: None, raw: "?".to_string(), chain: f.child_by_field_name("value").unwrap_or(f) })
        }
        _ => None,
    }
}

/// A Python site: `d.get(k, lit)` / `d.pop(k, lit)` / `d.setdefault(k, lit)`, `getattr(o, k,
/// lit)`, or `x or lit` (never inside `python_skip_or_in` units: a constructor defaulting an
/// optional argument). The default must be a literal.
fn python_site<'t>(node: Node<'t>, bare: &str, src: &[u8], cfg: &Cfg) -> Option<Found<'t>> {
    match node.kind() {
        "call" => {
            let f = node.child_by_field_name("function")?;
            let args = named_args(node.child_by_field_name("arguments")?);
            let name = callee_name(f, src)?;
            if !cfg.python_default_getters.iter().any(|g| g == name) {
                return None;
            }
            let (chain, default) = match f.kind() {
                "attribute" if args.len() == 2 => (f.child_by_field_name("object")?, args[1]),
                "identifier" if args.len() == 3 => (args[0], args[2]),
                _ => return None,
            };
            let literal = python_literal(default, src)?;
            Some(Found { raw: literal.clone(), literal: Some(literal), chain })
        }
        "boolean_operator" => {
            let op = node.child_by_field_name("operator").map(|o| text(o, src));
            if op != Some("or") || cfg.python_skip_or_in.iter().any(|u| u == bare) {
                return None;
            }
            let literal = python_literal(node.child_by_field_name("right")?, src)?;
            Some(Found { raw: literal.clone(), literal: Some(literal), chain: node.child_by_field_name("left")? })
        }
        _ => None,
    }
}

/// A TS site: `a ?? lit` (`||` only with `ts_count_or`), and with `count_optional_chain` every
/// `?.` member, call or index (a default of `undefined`, never a parse default).
fn ts_site<'t>(node: Node<'t>, src: &[u8], cfg: &Cfg) -> Option<Found<'t>> {
    match node.kind() {
        "binary_expression" => {
            let op = text(node.child_by_field_name("operator")?, src);
            if !(cfg.ts_operators.iter().any(|o| o == op) || (cfg.ts_count_or && op == "||")) {
                return None;
            }
            let literal = ts_literal(node.child_by_field_name("right")?, src)?;
            Some(Found { raw: literal.clone(), literal: Some(literal), chain: node.child_by_field_name("left")? })
        }
        "member_expression" | "call_expression" | "subscript_expression" if cfg.count_optional_chain && node.child_by_field_name("optional_chain").is_some() => {
            let chain = node.child_by_field_name("object").or_else(|| node.child_by_field_name("function"))?;
            Some(Found { literal: None, raw: "?.".to_string(), chain })
        }
        _ => None,
    }
}

/// The name a site's value lands in, found on the way up to the unit: a `let` pattern (the
/// element at the site's position when a tuple is bound to a tuple pattern), a declarator, an
/// assignment target, a struct field initializer, an object key or a keyword argument.
fn binding_of(site: Node, unit: Node, lang: Language, src: &[u8]) -> Option<String> {
    let mut prev = site;
    let mut cur = site.parent();
    // Position of `prev` inside a tuple / expression list, for the tuple-pattern `let`.
    let mut tuple_idx: Option<usize> = None;
    while let Some(p) = cur {
        if p.id() == unit.id() {
            return None;
        }
        let field = |f: &str| p.child_by_field_name(f).map(|n| cut(text(n, src)));
        // An assignment names the value only when the site sits in its right-hand side:
        // `*m.entry(k).or_default() += 1` holds the site in its target.
        let from_right = p.child_by_field_name("right").is_some_and(|r| r.id() == prev.id());
        let target = |f: &str| if from_right { field(f) } else { None };
        let found = match (lang, p.kind()) {
            (Language::Rust, "let_declaration") => {
                let pat = p.child_by_field_name("pattern")?;
                match (pat.kind(), tuple_idx) {
                    ("tuple_pattern", Some(i)) if prev.kind() == "tuple_expression" => {
                        let elems = named_args(pat);
                        return elems.get(i).map(|e| cut(text(*e, src)));
                    }
                    _ => Some(cut(text(pat, src))),
                }
            }
            (Language::Rust, "field_initializer") => field("field"),
            (Language::Rust, "assignment_expression" | "compound_assignment_expr") => target("left"),
            (Language::Python, "assignment" | "augmented_assignment") => target("left"),
            (Language::Python, "keyword_argument") => field("name"),
            (Language::Python, "pair") => field("key"),
            (Language::TypeScript | Language::JavaScript, "variable_declarator") => field("name"),
            (Language::TypeScript | Language::JavaScript, "assignment_expression") => target("left"),
            (Language::TypeScript | Language::JavaScript, "pair") => field("key"),
            _ => None,
        };
        if found.is_some() {
            return found;
        }
        tuple_idx = (p.kind() == "tuple_expression").then(|| named_args(p).iter().position(|c| c.id() == prev.id())).flatten();
        prev = p;
        cur = p.parent();
    }
    None
}

/// Every fallback site of one unit node, in document order; nested units are left to their own
/// measurement, closures and inline callbacks belong to the unit.
pub fn unit_sites(unit: Node, name: &str, lang: Language, src: &[u8], cfg: &Cfg) -> Vec<Site> {
    let bare = bare_name(name);
    let mut out = Vec::new();
    let mut stack: Vec<Node> = Vec::new();
    let mut c = unit.walk();
    for ch in unit.children(&mut c).collect::<Vec<_>>().into_iter().rev() {
        stack.push(ch);
    }
    while let Some(node) = stack.pop() {
        if metrics::is_nested_unit(node, lang) {
            continue;
        }
        let found = match lang {
            Language::Rust => rust_site(node, src, cfg),
            Language::Python => python_site(node, bare, src, cfg),
            _ => ts_site(node, src, cfg),
        };
        if let Some(f) = found {
            let transform = transform_in_chain(f.chain, lang, src, cfg);
            let kind = if f.literal.is_some() && transform.is_some() { SiteKind::ParseDefault } else { SiteKind::Fallback };
            out.push(Site {
                line: node.start_position().row + 1,
                kind,
                default: cut(&f.literal.unwrap_or(f.raw)),
                transform,
                binding: binding_of(node, unit, lang, src),
            });
        }
        let mut c = node.walk();
        for ch in node.children(&mut c).collect::<Vec<_>>().into_iter().rev() {
            stack.push(ch);
        }
    }
    out
}

/// The compiled `exempt_fn_patterns`; a pattern that does not compile is dropped.
pub struct Exempt(Vec<Regex>);

impl Exempt {
    pub fn new(cfg: &Cfg) -> Self {
        Exempt(cfg.exempt_fn_patterns.iter().filter_map(|p| Regex::new(p).ok()).collect())
    }

    fn matches(&self, name: &str) -> bool {
        self.0.iter().any(|r| r.is_match(name))
    }
}

/// `a, b and c`.
fn join_and(parts: &[String]) -> String {
    match parts.len() {
        0 => String::new(),
        1 => parts[0].clone(),
        n => format!("{} and {}", parts[..n - 1].join(", "), parts[n - 1]),
    }
}

/// The reason for one unit: `parse swallows 5 parse failures with literal defaults (lines
/// 157, 160, 161, 162, 163): a malformed line becomes header "", hash "", author "", ts 0 and
/// subject ""`, listing every parse-default line and the distinct binding / default pairs in
/// line order. `None` under `min_sites` or when the bare unit name matches an exempt pattern.
pub fn unit_reason(u: &FunctionMetrics, cfg: &Cfg, exempt: &Exempt) -> Option<String> {
    if u.parse_defaults < cfg.min_sites || u.parse_defaults == 0 || exempt.matches(bare_name(&u.name)) {
        return None;
    }
    let sites: Vec<&Site> = u.fallback_sites.iter().filter(|s| s.kind == SiteKind::ParseDefault).collect();
    let lines: Vec<String> = sites.iter().map(|s| s.line.to_string()).collect();
    let mut becomes: Vec<String> = Vec::new();
    for s in &sites {
        let t = match &s.binding {
            Some(b) => format!("{b} {}", s.default),
            None => s.default.clone(),
        };
        if !becomes.contains(&t) {
            becomes.push(t);
        }
    }
    let n = sites.len();
    Some(format!(
        "{} swallows {n} parse failure{} with literal defaults (lines {}): a malformed line becomes {}",
        u.name,
        if n == 1 { "" } else { "s" },
        lines.join(", "),
        join_and(&becomes)
    ))
}

/// The reason lines of a file's production units: one per unit at or over `min_sites` (most
/// sites first, then line order), at most `max_reported_per_file`, then a count of the rest.
pub fn file_reasons(units: &[&FunctionMetrics], cfg: &Cfg, exempt: &Exempt) -> Vec<String> {
    let mut hits: Vec<(&FunctionMetrics, String)> = units.iter().filter_map(|u| unit_reason(u, cfg, exempt).map(|r| (*u, r))).collect();
    hits.sort_by(|a, b| b.0.parse_defaults.cmp(&a.0.parse_defaults).then(a.0.start_line.cmp(&b.0.start_line)));
    let more = hits.len().saturating_sub(cfg.max_reported_per_file);
    let mut out: Vec<String> = hits.into_iter().take(cfg.max_reported_per_file).map(|(_, r)| r).collect();
    if more > 0 {
        out.push(format!("+{more} more function(s) swallow {}+ parse failures with literal defaults", cfg.min_sites));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Metrics as MetricsCfg, Naming as NamingCfg, Tests as TestsCfg};
    use crate::discover::{FileKind, SourceFile};

    fn file(path: &str, lang: Language, content: &str) -> SourceFile {
        SourceFile { path: path.into(), lang, kind: FileKind::Source, lines: content.lines().count(), bytes: content.len(), content: content.into() }
    }

    fn units(path: &str, lang: Language, src: &str, cfg: &Cfg) -> Vec<FunctionMetrics> {
        let (_, fs) = metrics::analyze_file(&file(path, lang, src), &MetricsCfg::default(), &TestsCfg::default(), &NamingCfg::default(), cfg);
        fs
    }

    /// (line, kind, default, transform, binding)
    type Row<'a> = (usize, SiteKind, &'a str, Option<&'a str>, Option<&'a str>);

    fn rows(u: &FunctionMetrics) -> Vec<Row<'_>> {
        u.fallback_sites.iter().map(|s| (s.line, s.kind, s.default.as_str(), s.transform.as_deref(), s.binding.as_deref())).collect()
    }

    #[test]
    fn rust_parse_defaults_need_a_transform_in_the_chain_and_a_literal_default() {
        let cfg = Cfg::default();
        let src = "\
fn parse(header: &str) -> (u64, String) {
    let mut fields = header.split('\\x02');
    let (hash, author, ts, subject) = (
        fields.next().unwrap_or(\"\"),
        fields.next().unwrap_or(\"\"),
        fields.next().and_then(|t| t.parse::<i64>().ok()).unwrap_or(0),
        fields.next().unwrap_or(\"\"),
    );
    let n: u8 = s.parse().ok().unwrap_or(0);
    let d = lines.next().unwrap_or_default();
    let v = opt.unwrap_or_else(Vec::new);
    let w = words.next().unwrap_or_else(|| String::new());
    let z = self.x.unwrap_or(0);
    let g = m.get(&k).copied().unwrap_or(&0);
    let o = a.unwrap_or(b.next().unwrap_or(\"\"));
    let q = s.strip_prefix(\"x\").unwrap_or(s);
    let r = s.parse::<u8>().ok()?;
    let e = it.next().unwrap_or(());
    *m.entry(k).or_default() += 1;
    (0, String::new())
}
";
        let f = &units("a.rs", Language::Rust, src, &cfg)[0];
        use SiteKind::*;
        let expect = vec![
            (4, ParseDefault, "\"\"", Some("next"), Some("hash")),
            (5, ParseDefault, "\"\"", Some("next"), Some("author")),
            (6, ParseDefault, "0", Some("next"), Some("ts")),
            (7, ParseDefault, "\"\"", Some("next"), Some("subject")),
            // `.ok()` under an `unwrap_or` is not a site of its own: one site, at the outer call.
            (9, ParseDefault, "0", Some("parse"), Some("n")),
            (10, ParseDefault, "Default::default()", Some("next"), Some("d")),
            // A constructor path or a closure returning one is a literal default; no transform.
            (11, Fallback, "Vec::new()", None, Some("v")),
            (12, ParseDefault, "String::new()", Some("next"), Some("w")),
            // A struct field has no call in its chain.
            (13, Fallback, "0", None, Some("z")),
            (14, ParseDefault, "0", Some("get"), Some("g")),
            // The outer default is not a literal; the inner one is a site of its own.
            (15, Fallback, "b.next().unwrap_or(\"\")", None, Some("o")),
            (15, ParseDefault, "\"\"", Some("next"), Some("o")),
            (16, Fallback, "s", Some("strip_prefix"), Some("q")),
            // `.ok()?` is a fallback with no default; the transform is noted whatever the kind.
            (17, Fallback, "?", Some("parse"), Some("r")),
            (18, ParseDefault, "()", Some("next"), Some("e")),
            // A site inside an assignment's target has no binding.
            (19, Fallback, "Default::default()", None, None),
        ];
        assert_eq!(rows(f), expect, "{:#?}", f.fallback_sites);
        assert_eq!((f.parse_defaults, f.fallbacks), (10, 16));
        let r = unit_reason(f, &cfg, &Exempt::new(&cfg)).unwrap();
        assert_eq!(r, "parse swallows 10 parse failures with literal defaults (lines 4, 5, 6, 7, 9, 10, 12, 14, 15, 18): a malformed line becomes hash \"\", author \"\", ts 0, subject \"\", n 0, d Default::default(), w String::new(), g 0, o \"\" and e ()");
    }

    #[test]
    fn reason_needs_min_sites_and_skips_exempt_names_and_test_units() {
        let mut cfg = Cfg::default();
        let src = "\
fn one(s: &str) -> u8 { s.parse().unwrap_or(0) }
fn from_env(s: &str) -> (u8, u8) { (s.parse().unwrap_or(0), s.parse().unwrap_or(1)) }
impl D { fn default_two(s: &str) -> (u8, u8) { (s.parse().unwrap_or(0), s.parse().unwrap_or(1)) } }
fn two(s: &str) -> (u8, u8) { (s.parse().unwrap_or(0), s.parse().unwrap_or(1)) }
#[cfg(test)]
mod tests {
    fn t(s: &str) -> (u8, u8) { (s.parse().unwrap_or(0), s.parse().unwrap_or(1)) }
}
";
        let fs = units("a.rs", Language::Rust, src, &cfg);
        let exempt = Exempt::new(&cfg);
        let by: Vec<(&str, usize, bool)> = fs.iter().map(|f| (f.name.as_str(), f.parse_defaults, unit_reason(f, &cfg, &exempt).is_some())).collect();
        assert_eq!(by, vec![("one", 1, false), ("from_env", 2, false), ("D.default_two", 2, false), ("two", 2, true), ("t", 0, false)], "{by:?}");
        assert_eq!(unit_reason(&fs[3], &cfg, &exempt).unwrap(), "two swallows 2 parse failures with literal defaults (lines 4, 4): a malformed line becomes 0 and 1");
        cfg.min_sites = 1;
        assert_eq!(unit_reason(&fs[0], &cfg, &exempt).unwrap(), "one swallows 1 parse failure with literal defaults (lines 1): a malformed line becomes 0");
        // Per-file lines: most sites first, capped, the rest counted.
        cfg.max_reported_per_file = 1;
        let prod: Vec<&FunctionMetrics> = fs.iter().filter(|f| !f.in_test).collect();
        let lines = file_reasons(&prod, &cfg, &exempt);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].starts_with("two swallows 2"), "{lines:?}");
        assert_eq!(lines[1], "+1 more function(s) swallow 1+ parse failures with literal defaults");
    }

    #[test]
    fn python_getters_getattr_and_or_with_literals_and_no_or_in_init() {
        let cfg = Cfg::default();
        let src = "\
def f(line, d, o):
    a = line.split(':')[1] or ''
    b = d.get('k', 0)
    c = getattr(o, 'k', None)
    e = int(line.partition('=')[2] or 0)
    g = d.get('k', other)
    h = line.strip() or None
    i = d.get('k')
    return dict(x=line.split(',')[0] or '')
class K:
    def __init__(self, x=None):
        self.x = x or []
        self.y = str(x).split('.')[0] or ''
";
        let fs = units("a.py", Language::Python, src, &cfg);
        use SiteKind::*;
        let expect = vec![
            (2, ParseDefault, "''", Some("split"), Some("a")),
            (3, Fallback, "0", None, Some("b")),
            (4, Fallback, "None", None, Some("c")),
            (5, ParseDefault, "0", Some("partition"), Some("e")),
            (7, Fallback, "None", None, Some("h")),
            (9, ParseDefault, "''", Some("split"), Some("x")),
        ];
        assert_eq!(rows(&fs[0]), expect, "{:#?}", fs[0].fallback_sites);
        assert_eq!(fs[0].parse_defaults, 3);
        // `or` never counts in `__init__`, and the name is exempt from the reason anyway.
        assert_eq!(fs[1].name, "K.__init__");
        assert!(fs[1].fallback_sites.is_empty(), "{:?}", fs[1].fallback_sites);
    }

    #[test]
    fn ts_nullish_with_literal_and_the_or_and_optional_chain_knobs() {
        let mut cfg = Cfg::default();
        let src = "\
function f(line: string, obj: any) {
  const a = line.split(':')[1] ?? '';
  const b = parseInt(line, 10) ?? 0;
  const c = obj.x ?? null;
  const d = line.match(/x/)?.[1] ?? '';
  const e = obj.y || '';
  const g = obj?.z;
  const h = line.split(':')[0] ?? other;
  return { k: JSON.parse(line) ?? {} };
}
";
        let f = &units("a.ts", Language::TypeScript, src, &cfg)[0];
        use SiteKind::*;
        let expect = vec![
            (2, ParseDefault, "''", Some("split"), Some("a")),
            (3, ParseDefault, "0", Some("parseInt"), Some("b")),
            (4, Fallback, "null", None, Some("c")),
            (5, ParseDefault, "''", Some("match"), Some("d")),
            (9, ParseDefault, "{}", Some("parse"), Some("k")),
        ];
        assert_eq!(rows(f), expect, "{:#?}", f.fallback_sites);
        cfg.ts_count_or = true;
        cfg.count_optional_chain = true;
        let f = &units("a.ts", Language::TypeScript, src, &cfg)[0];
        let extra: Vec<(usize, SiteKind, &str)> = f.fallback_sites.iter().filter(|s| (5..=7).contains(&s.line)).map(|s| (s.line, s.kind, s.default.as_str())).collect();
        assert_eq!(extra, vec![(5, ParseDefault, "''"), (5, Fallback, "?."), (6, Fallback, "''"), (7, Fallback, "?.")], "{extra:?}");
    }

    #[test]
    fn nested_units_keep_their_own_sites_and_arguments_are_not_the_chain() {
        let cfg = Cfg::default();
        let src = "\
fn outer(s: &str) -> u8 {
    fn inner(t: &str) -> u8 { t.parse().unwrap_or(0) }
    let c = |t: &str| t.parse().unwrap_or(0);
    let x = a.unwrap_or(s.next().len());
    inner(s) + c(s)
}
";
        let fs = units("a.rs", Language::Rust, src, &cfg);
        let by: Vec<(&str, usize, usize)> = fs.iter().map(|f| (f.name.as_str(), f.parse_defaults, f.fallbacks)).collect();
        // The closure's site folds into `outer`; `inner` keeps its own; `next` in an argument
        // is not the receiver chain.
        assert_eq!(by, vec![("outer", 1, 2), ("inner", 1, 1)], "{by:?}");
    }
}
