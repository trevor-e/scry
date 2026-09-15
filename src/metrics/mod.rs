//! Per-function complexity from tree-sitter.
//!
//! Cognitive complexity follows the SonarSource definition: control-flow
//! breaks cost 1, each level of nesting they sit in costs 1 more, `else`/`elif`
//! cost 1 flat, and a run of the same boolean operator costs 1 for the run.
//! Cyclomatic complexity is the classic 1 + decision points and is kept
//! because it is what most people have an intuition for, but nesting-aware
//! cognitive complexity is the one that predicts "hard to change safely".

use crate::config::{Fallback as FallbackCfg, Metrics as Cfg, Naming as NamingCfg, Tests as TestsCfg};
use crate::discover::SourceFile;
use crate::lang::Language;
use crate::regions::{self, TestRegion};
use rayon::prelude::*;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use tree_sitter::Node;

#[derive(Debug, Clone, Serialize)]
pub struct FunctionMetrics {
    pub file: String,
    pub name: String,
    pub start_line: usize,
    pub end_line: usize,
    pub lines: usize,
    pub params: usize,
    pub cyclomatic: u32,
    pub cognitive: u32,
    pub max_nesting: u32,
    /// Lies inside an inline test region (`#[cfg(test)]` mod/item, `#[test]` fn): kept in the
    /// list, left out of the file totals and of the worst-function ranking.
    pub in_test: bool,
    /// Labelled phases of the body (see `comments`), filled by the report for the units it
    /// prints; empty (and left out of JSON) everywhere else.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub phases: Vec<crate::comments::Phase>,
    /// Names the unit binds (`let` / assignment / `for` / `with`; parameters, closure and
    /// nested-unit parameters excluded; see `naming`)…
    pub bindings: usize,
    /// …the short ones (`[naming].short_name_max_len` characters, outside the allow list) with
    /// their use pattern, in declaration order; left out of JSON when empty…
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub short_bindings: Vec<crate::naming::ShortBinding>,
    /// …and how many of those have a use gap of `[naming].short_name_min_gap`+ lines (0 when
    /// the unit binds too few other names).
    pub long_short_bindings: usize,
    /// Distinct names bound by `let` / assignment / `for` / `with` / walrus / declarator inside
    /// the unit: a destructuring pattern counts as one, a rebinding not at all; closure and
    /// lambda bodies fold in, nested named units and every parameter do not.
    pub locals: usize,
    /// Brain method: `lines >= [metrics].brain_min_lines`, `cognitive >= brain_min_cognitive`
    /// and `locals >= brain_min_locals` all hold. A label on the cognitive reason, never a score.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub brain: bool,
    /// The (up to 3) locals of a brain method with the longest live span, longest first: the
    /// declaration-to-last-use span the `naming` pass measured for a short binding, else the
    /// first-to-last mention of the name in the unit. Empty (and left out of JSON) otherwise.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub longest_locals: Vec<LocalSpan>,
    /// Parse-default sites (see `fallback`): a literal default (`""`, `0`, `Default::default()`)
    /// applied to a receiver chain holding a fallible transform (`parse`, `split`, `next`…)…
    pub parse_defaults: usize,
    /// …among every value-swallowing default of the unit (`unwrap_or*`, `.ok()?`, Python
    /// `d.get(k, lit)` / `x or lit`, TS `?? lit`)…
    pub fallbacks: usize,
    /// …listed with line, kind, default, transform and the name the value lands in; left out
    /// of JSON when empty. Units in an inline test region are not measured.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fallback_sites: Vec<crate::fallback::Site>,
}

/// One local of a brain method and the lines its name spans.
#[derive(Debug, Clone, Serialize)]
pub struct LocalSpan {
    pub name: String,
    pub first_line: usize,
    pub last_line: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileMetrics {
    pub path: String,
    pub functions: usize,
    pub total_cognitive: u32,
    pub max_cognitive: u32,
    pub max_nesting: u32,
    /// Functions above the "hard to follow" threshold.
    pub complex_functions: usize,
    pub parse_errors: bool,
    /// Lines spanned by inline test regions (Rust `#[cfg(test)]` mods and items, `#[test]` fns).
    pub inline_test_lines: usize,
    pub test_regions: Vec<TestRegion>,
    /// Bindings of the production units, the short ones among them, and the short ones with a
    /// far use gap (see `naming`)…
    pub bindings: usize,
    pub short_bindings: usize,
    pub long_short_bindings: usize,
    /// …and `short_bindings / bindings` (0 with no binding), when `[naming].emit_short_binding_share`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short_binding_share: Option<f64>,
}

/// Node-kind tables per grammar. One walker, three tables.
struct Profile {
    /// Reported as a function. Their subtree is measured; nested units are
    /// measured on their own *and* count toward the enclosing unit.
    units: &'static [&'static str],
    /// Anonymous callables: nesting +1, no increment, not reported.
    nest_only: &'static [&'static str],
    /// +1, and their children sit one nesting level deeper.
    branch_nest: &'static [&'static str],
    /// +1 flat (else, elif, comprehension `if`).
    branch_flat: &'static [&'static str],
    /// Cyclomatic-only decision points (switch cases, match arms).
    cases: &'static [&'static str],
    /// Branches whose cyclomatic cost is carried by their cases, not by themselves.
    switch_like: &'static [&'static str],
    /// The node kind carrying boolean operators, and the operator spellings.
    bool_expr: &'static str,
    bool_ops: &'static [&'static str],
    /// `else if` is spelled as an `if` nested directly in an else clause.
    else_clause: &'static str,
    if_kind: &'static str,
    params: &'static [&'static str],
    class_like: &'static [&'static str],
    /// The name (or whole pattern) a node binds as a local, when it is a binding form.
    binding: for<'a> fn(Node<'_>, &'a [u8]) -> Option<&'a str>,
    /// Node kinds that mention a name (for the live span of a brain method's locals).
    ident: &'static [&'static str],
}

const PYTHON: Profile = Profile {
    units: &["function_definition"],
    nest_only: &["lambda"],
    branch_nest: &[
        "if_statement", "for_statement", "while_statement", "conditional_expression",
        "except_clause", "match_statement", "for_in_clause",
    ],
    branch_flat: &["elif_clause", "else_clause", "if_clause"],
    cases: &["case_clause"],
    switch_like: &["match_statement"],
    bool_expr: "boolean_operator",
    bool_ops: &["and", "or"],
    else_clause: "",
    if_kind: "if_statement",
    params: &["parameters", "lambda_parameters"],
    class_like: &["class_definition"],
    binding: python_binding,
    ident: &["identifier"],
};

const JS: Profile = Profile {
    units: &[
        "function_declaration", "generator_function_declaration", "method_definition",
        "arrow_function", "function_expression", "generator_function",
    ],
    nest_only: &[],
    branch_nest: &[
        "if_statement", "for_statement", "for_in_statement", "while_statement", "do_statement",
        "switch_statement", "catch_clause", "ternary_expression",
    ],
    branch_flat: &["else_clause"],
    cases: &["switch_case"],
    switch_like: &["switch_statement"],
    bool_expr: "binary_expression",
    bool_ops: &["&&", "||", "??"],
    else_clause: "else_clause",
    if_kind: "if_statement",
    params: &["formal_parameters"],
    class_like: &["class_declaration", "class"],
    binding: js_binding,
    ident: &["identifier", "shorthand_property_identifier", "shorthand_property_identifier_pattern"],
};

const RUST: Profile = Profile {
    units: &["function_item"],
    nest_only: &["closure_expression"],
    branch_nest: &[
        "if_expression", "match_expression", "for_expression", "while_expression",
        "loop_expression",
    ],
    branch_flat: &["else_clause"],
    cases: &["match_arm"],
    switch_like: &["match_expression"],
    bool_expr: "binary_expression",
    bool_ops: &["&&", "||"],
    else_clause: "else_clause",
    if_kind: "if_expression",
    params: &["parameters", "closure_parameters"],
    class_like: &["impl_item", "trait_item"],
    binding: rust_binding,
    ident: &["identifier", "shorthand_field_identifier"],
};

/// Rust: `let` and `for` patterns. `let _ = …` has no pattern field and binds nothing; a
/// `mut` is a sibling of the pattern, so `let mut x` is `x`. Match arms and `if let` / `while
/// let` conditions are not counted.
fn rust_binding<'a>(node: Node<'_>, src: &'a [u8]) -> Option<&'a str> {
    match node.kind() {
        "let_declaration" | "for_expression" => node.child_by_field_name("pattern").map(|p| text(p, src)),
        _ => None,
    }
}

/// Python: the plain assignment of a statement (`x = …`, `x: T = …`, `a, b = …`; never
/// `self.x` / `xs[i]`, never `+=`), `for` targets, `with … as x`, walrus.
fn python_binding<'a>(node: Node<'_>, src: &'a [u8]) -> Option<&'a str> {
    let target = |n: Node<'_>| matches!(n.kind(), "identifier" | "pattern_list" | "tuple_pattern" | "list_pattern");
    match node.kind() {
        "assignment" if node.parent().is_some_and(|p| p.kind() == "expression_statement") => {
            node.child_by_field_name("left").filter(|l| target(*l)).map(|l| text(l, src))
        }
        "for_statement" => node.child_by_field_name("left").filter(|l| target(*l)).map(|l| text(l, src)),
        "with_item" => node
            .child_by_field_name("value")
            .filter(|v| v.kind() == "as_pattern")
            .and_then(|v| v.child_by_field_name("alias"))
            .map(|a| a.named_child(0).unwrap_or(a))
            .map(|a| text(a, src)),
        "named_expression" => node.child_by_field_name("name").map(|n| text(n, src)),
        _ => None,
    }
}

/// JS/TS: every `variable_declarator` (`let` / `const` / `var`, a `for (let i…)` initializer
/// included) and the `for … of/in` target. A plain `x = …` rebinds and is not a declaration.
fn js_binding<'a>(node: Node<'_>, src: &'a [u8]) -> Option<&'a str> {
    match node.kind() {
        "variable_declarator" => node.child_by_field_name("name").map(|n| text(n, src)),
        "for_in_statement" => node
            .child_by_field_name("left")
            .filter(|l| matches!(l.kind(), "identifier" | "array_pattern" | "object_pattern"))
            .map(|l| text(l, src)),
        _ => None,
    }
}

fn profile(lang: Language) -> &'static Profile {
    match lang {
        Language::Python => &PYTHON,
        Language::Rust => &RUST,
        _ => &JS,
    }
}

/// Arrow functions and function expressions are only reported as units when
/// they are bound to a name; inline callbacks are nesting, not functions.
fn is_bound_callable(node: Node) -> bool {
    node.parent().is_some_and(|p| {
        matches!(
            p.kind(),
            "variable_declarator" | "pair" | "assignment_expression" | "public_field_definition"
                | "export_statement"
        )
    })
}

pub fn analyze_all(files: &[SourceFile], cfg: &Cfg, tests: &TestsCfg, naming: &NamingCfg, fallback: &FallbackCfg) -> (Vec<FileMetrics>, Vec<FunctionMetrics>) {
    let (file_metrics, funcs, _) = analyze_all_with(files, cfg, tests, naming, fallback, |_, _, _, _, _| ());
    (file_metrics, funcs)
}

/// `analyze_all` plus one caller-supplied pass over each file's parsed tree (`None` when the
/// parse failed) with its test regions and its units (the metrics and their nodes, in the same
/// order): the mentions pass collects symbols and inline test units there and the comments
/// pass reads the over-threshold bodies, so no Source file is parsed twice. The extras come
/// back in file order.
pub fn analyze_all_with<'a, T: Send>(
    files: &'a [SourceFile],
    cfg: &Cfg,
    tests: &TestsCfg,
    naming: &NamingCfg,
    fallback: &FallbackCfg,
    extra: impl Fn(Option<Node>, &'a SourceFile, &[TestRegion], &[FunctionMetrics], &[Node]) -> T + Sync,
) -> (Vec<FileMetrics>, Vec<FunctionMetrics>, Vec<T>) {
    let per_file: Vec<(FileMetrics, Vec<FunctionMetrics>, T)> =
        files.par_iter().map(|f| analyze_file_with(f, cfg, tests, naming, fallback, &extra)).collect();
    let mut file_metrics = Vec::with_capacity(per_file.len());
    let mut funcs = Vec::new();
    let mut extras = Vec::with_capacity(per_file.len());
    for (fm, fs, x) in per_file {
        file_metrics.push(fm);
        funcs.extend(fs);
        extras.push(x);
    }
    (file_metrics, funcs, extras)
}

#[cfg(test)]
pub fn analyze_file(file: &SourceFile, cfg: &Cfg, tests: &TestsCfg, naming: &NamingCfg, fallback: &FallbackCfg) -> (FileMetrics, Vec<FunctionMetrics>) {
    let (fm, fs, ()) = analyze_file_with(file, cfg, tests, naming, fallback, |_, _, _, _, _| ());
    (fm, fs)
}

fn analyze_file_with<'a, T>(
    file: &'a SourceFile,
    cfg: &Cfg,
    tests: &TestsCfg,
    naming: &NamingCfg,
    fallback: &FallbackCfg,
    extra: impl Fn(Option<Node>, &'a SourceFile, &[TestRegion], &[FunctionMetrics], &[Node]) -> T,
) -> (FileMetrics, Vec<FunctionMetrics>, T) {
    let mut parser = file.lang.parser();
    let src = file.content.as_bytes();
    let mut funcs = Vec::new();
    let mut parse_errors = false;
    // Regions are always detected on a Rust file, so they are listed whatever the knob says;
    // the knob gates their effects (unit tags, line counts).
    let mut test_regions = Vec::new();
    let tree = parser.parse(src, None);
    let extra_out;
    if let Some(tree) = &tree {
        let root = tree.root_node();
        parse_errors = root.has_error();
        if file.lang == Language::Rust {
            test_regions = regions::test_regions(root, src);
        }
        let tagged = if tests.inline_modules { test_regions.as_slice() } else { &[] };
        let nodes = collect_units(root, file, tagged, cfg, naming, fallback, &mut funcs);
        extra_out = extra(Some(root), file, &test_regions, &funcs, &nodes);
    } else {
        extra_out = extra(None, file, &test_regions, &funcs, &[]);
    }
    // File totals describe the production code only; tagged units stay in the list.
    let source = || funcs.iter().filter(|f| !f.in_test);
    let mut fm = FileMetrics {
        path: file.path.clone(),
        functions: source().count(),
        total_cognitive: source().map(|f| f.cognitive).sum(),
        max_cognitive: source().map(|f| f.cognitive).max().unwrap_or(0),
        max_nesting: source().map(|f| f.max_nesting).max().unwrap_or(0),
        complex_functions: source().filter(|f| f.cognitive > cfg.cognitive_hard).count(),
        parse_errors,
        inline_test_lines: if tests.inline_modules { regions::inline_lines(&test_regions) } else { 0 },
        test_regions,
        bindings: source().map(|f| f.bindings).sum(),
        short_bindings: source().map(|f| f.short_bindings.len()).sum(),
        long_short_bindings: source().map(|f| f.long_short_bindings).sum(),
        short_binding_share: None,
    };
    if naming.emit_short_binding_share {
        fm.short_binding_share = Some(if fm.bindings == 0 { 0.0 } else { fm.short_bindings as f64 / fm.bindings as f64 });
    }
    (fm, funcs, extra_out)
}

/// Every node reported as a unit, in document order: the grammar's unit kinds, with arrow
/// functions and function expressions only when bound to a name or enclosed by no unit. The
/// mentions pass uses the same rule to split a Test file into test units.
pub fn unit_nodes(root: Node<'_>, lang: Language) -> Vec<Node<'_>> {
    let prof = profile(lang);
    let mut out = Vec::new();
    // Explicit stack: source files can nest expressions thousands deep.
    // (node, inside a reported unit already)
    let mut stack: Vec<(Node, bool)> = vec![(root, false)];
    while let Some((node, in_unit)) = stack.pop() {
        let kind = node.kind();
        let is_unit = prof.units.contains(&kind)
            && (!matches!(kind, "arrow_function" | "function_expression")
                // Inline callbacks nest inside their enclosing unit; but a callback
                // with no enclosing unit (route handlers, forwardRef components,
                // describe blocks) is the only place its complexity can be reported.
                || is_bound_callable(node)
                || !in_unit);
        if is_unit {
            out.push(node);
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push((child, in_unit || is_unit));
        }
    }
    out
}

/// Measures every unit into `out` and returns the unit nodes in the same order.
fn collect_units<'t>(root: Node<'t>, file: &SourceFile, test_regions: &[TestRegion], cfg: &Cfg, naming: &NamingCfg, fallback: &FallbackCfg, out: &mut Vec<FunctionMetrics>) -> Vec<Node<'t>> {
    let (lang, src) = (file.lang, file.content.as_bytes());
    let prof = profile(lang);
    let nodes = unit_nodes(root, lang);
    for node in &nodes {
        let (mut m, locals) = measure(*node, prof, src, &file.path);
        m.in_test = regions::contains(test_regions, node.start_byte());
        let b = crate::naming::unit_bindings(*node, lang, src, naming);
        m.bindings = b.bindings;
        m.short_bindings = b.short;
        m.long_short_bindings = b.long_short_bindings;
        m.brain = m.lines >= cfg.brain_min_lines && m.cognitive >= cfg.brain_min_cognitive && m.locals >= cfg.brain_min_locals;
        if m.brain {
            m.longest_locals = longest_locals(*node, prof, lang, src, &locals, &m.short_bindings);
        }
        // Fallback sites on the same node (see `fallback`); test units are never measured.
        if !m.in_test {
            m.fallback_sites = crate::fallback::unit_sites(*node, &m.name, lang, src, fallback);
            m.fallbacks = m.fallback_sites.len();
            m.parse_defaults = m.fallback_sites.iter().filter(|s| s.kind == crate::fallback::SiteKind::ParseDefault).count();
        }
        out.push(m);
    }
    nodes
}

/// How many of a brain method's locals the reason names.
const LONGEST_LOCALS_NAMED: usize = 3;

/// The locals of a brain method with the longest live span: for a name the `naming` pass
/// measured, its declaration-to-last-use span; for the rest, the first to the last mention of
/// the name inside the unit (nested named units left out). A pattern binding has no single
/// name to follow and is never listed.
fn longest_locals(unit: Node, prof: &Profile, lang: Language, src: &[u8], locals: &HashSet<&str>, short: &[crate::naming::ShortBinding]) -> Vec<LocalSpan> {
    let mut span: HashMap<&str, (usize, usize)> = HashMap::new();
    let mut stack: Vec<Node> = vec![unit];
    while let Some(node) = stack.pop() {
        if node.id() != unit.id() && is_nested_unit(node, lang) {
            continue;
        }
        if prof.ident.contains(&node.kind()) {
            let t = text(node, src);
            if locals.contains(t) {
                let line = node.start_position().row + 1;
                let e = span.entry(t).or_insert((line, line));
                e.0 = e.0.min(line);
                e.1 = e.1.max(line);
            }
        }
        let mut c = node.walk();
        for child in node.children(&mut c) {
            stack.push(child);
        }
    }
    let mut out: Vec<LocalSpan> = span
        .into_iter()
        .map(|(name, (first, last))| {
            // The measured binding wins: it stops at the last real use, not at a homonym.
            let measured = short.iter().filter(|s| s.name == name).max_by_key(|s| s.span);
            let (first_line, last_line) = measured.map_or((first, last), |s| (s.decl_line, s.last_line));
            LocalSpan { name: name.to_string(), first_line, last_line }
        })
        .collect();
    out.sort_by(|a, b| (b.last_line - b.first_line).cmp(&(a.last_line - a.first_line)).then(a.first_line.cmp(&b.first_line)).then(a.name.cmp(&b.name)));
    out.truncate(LONGEST_LOCALS_NAMED);
    out
}

/// Cognitive increments of `nodes` walked with nesting re-based to 0: what a phase would cost
/// as a helper of its own (see `comments`).
pub fn cognitive_rebased<'t>(nodes: impl IntoIterator<Item = Node<'t>>, lang: Language, src: &[u8]) -> u32 {
    let prof = profile(lang);
    let mut acc = Acc::default();
    for n in nodes {
        walk(n, prof, src, 0, &mut acc);
    }
    acc.cognitive
}

/// A node that `unit_nodes` reports as a unit of its own when it sits inside another unit: the
/// grammar's unit kinds, arrow functions and function expressions only when bound to a name.
pub(crate) fn is_nested_unit(node: Node, lang: Language) -> bool {
    let kind = node.kind();
    profile(lang).units.contains(&kind) && (!matches!(kind, "arrow_function" | "function_expression") || is_bound_callable(node))
}

/// A unit kind or an anonymous callable of `lang`: a scope of its own.
pub fn is_callable_kind(lang: Language, kind: &str) -> bool {
    let prof = profile(lang);
    prof.units.contains(&kind) || prof.nest_only.contains(&kind)
}

fn measure<'a>(node: Node, prof: &Profile, src: &'a [u8], path: &str) -> (FunctionMetrics, HashSet<&'a str>) {
    let mut acc = Acc::default();
    // Children of the unit start at nesting 0; the unit itself is not a nesting level.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, prof, src, 0, &mut acc);
    }
    let start_line = node.start_position().row + 1;
    let end_line = node.end_position().row + 1;
    let m = FunctionMetrics {
        file: path.to_string(),
        name: unit_name(node, prof, src),
        start_line,
        end_line,
        lines: end_line - start_line + 1,
        params: count_params(node, prof, src),
        cyclomatic: 1 + acc.cyclomatic,
        cognitive: acc.cognitive,
        max_nesting: acc.max_nesting,
        in_test: false,
        phases: Vec::new(),
        bindings: 0,
        short_bindings: Vec::new(),
        long_short_bindings: 0,
        locals: acc.locals.len(),
        brain: false,
        longest_locals: Vec::new(),
        parse_defaults: 0,
        fallbacks: 0,
        fallback_sites: Vec::new(),
    };
    (m, acc.locals)
}

#[derive(Default)]
struct Acc<'a> {
    cognitive: u32,
    cyclomatic: u32,
    max_nesting: u32,
    /// Distinct local binding names (or whole patterns) met outside nested named units.
    locals: HashSet<&'a str>,
}

fn walk<'a>(root: Node, prof: &Profile, src: &'a [u8], nesting: u32, acc: &mut Acc<'a>) {
    // (node, nesting, inside a nested named unit: its locals are its own)
    let mut stack: Vec<(Node, u32, bool)> = vec![(root, nesting, false)];
    while let Some((node, nesting, in_nested)) = stack.pop() {
        let kind = node.kind();
        let mut child_nesting = nesting;
        let mut child_nested = in_nested;

        if prof.units.contains(&kind) || prof.nest_only.contains(&kind) {
            // Nested callable: its body sits one level deeper, no increment of its own.
            child_nesting = nesting + 1;
            // A named nested unit keeps its locals; a closure's fold into the enclosing unit.
            child_nested = in_nested || (prof.units.contains(&kind) && (!matches!(kind, "arrow_function" | "function_expression") || is_bound_callable(node)));
        } else if kind == prof.if_kind && node.parent().is_some_and(|p| p.kind() == prof.else_clause) {
            // `else if`: flat +1, children keep the parent's nesting.
            acc.cognitive += 1;
            acc.cyclomatic += 1;
        } else if prof.branch_nest.contains(&kind) {
            acc.cognitive += 1 + nesting;
            // A switch/match is one decision per case; the head itself adds none.
            if !prof.switch_like.contains(&kind) {
                acc.cyclomatic += 1;
            }
            child_nesting = nesting + 1;
            acc.max_nesting = acc.max_nesting.max(child_nesting);
        } else if prof.branch_flat.contains(&kind) {
            // An else clause that directly holds an `if` is the `else if` above.
            let holds_if = !prof.else_clause.is_empty()
                && kind == prof.else_clause
                && node.named_child(0).is_some_and(|c| c.kind() == prof.if_kind);
            // Python hangs `else` off try/for/while too; only the `if` form is a branch.
            let else_of_if = kind != "else_clause" || node.parent().is_some_and(|p| p.kind() == prof.if_kind);
            if !holds_if && else_of_if {
                acc.cognitive += 1;
                if kind != "else_clause" {
                    acc.cyclomatic += 1;
                }
            }
        } else if prof.cases.contains(&kind) {
            acc.cyclomatic += 1;
        } else if kind == prof.bool_expr {
            if let Some(op) = operator_of(node, prof, src) {
                acc.cyclomatic += 1;
                let same_as_parent = node
                    .parent()
                    .filter(|p| p.kind() == prof.bool_expr)
                    .and_then(|p| operator_of(p, prof, src))
                    .is_some_and(|pop| pop == op);
                if !same_as_parent {
                    acc.cognitive += 1;
                }
            }
        }

        if !in_nested && let Some(name) = (prof.binding)(node, src) && name != "_" {
            acc.locals.insert(name);
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push((child, child_nesting, child_nested));
        }
    }
}

fn operator_of<'a>(node: Node, prof: &Profile, src: &'a [u8]) -> Option<&'a str> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if !child.is_named() {
            let text = child.utf8_text(src).ok()?;
            if prof.bool_ops.contains(&text) {
                return Some(text);
            }
        }
    }
    // Python spells `and`/`or` as named keyword tokens under the `operator` field.
    node.child_by_field_name("operator")
        .and_then(|c| c.utf8_text(src).ok())
        .filter(|t| prof.bool_ops.contains(t))
}

fn text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    node.utf8_text(src).unwrap_or("?")
}

/// The name `FunctionMetrics.name` would carry for a unit node of `lang` (`park`,
/// `Store<'c>.transact`, `app.post('/x')`), for callers naming units on their own tree.
pub fn unit_name_of(node: Node, lang: Language, src: &[u8]) -> String {
    unit_name(node, profile(lang), src)
}

fn unit_name(node: Node, prof: &Profile, src: &[u8]) -> String {
    let own = node.child_by_field_name("name").map(|n| text(n, src).to_string());
    let name = own.or_else(|| {
        let p = node.parent()?;
        match p.kind() {
            "variable_declarator" | "public_field_definition" => {
                p.child_by_field_name("name").map(|n| text(n, src).to_string())
            }
            "pair" => p.child_by_field_name("key").map(|n| text(n, src).to_string()),
            "assignment_expression" => p.child_by_field_name("left").map(|n| text(n, src).to_string()),
            "export_statement" => Some("default".to_string()),
            _ => call_site_name(node, src),
        }
    });
    let name = name.unwrap_or_else(|| "<anonymous>".to_string());
    // Qualify methods with their class / impl target.
    let mut cur = node.parent();
    while let Some(p) = cur {
        if prof.class_like.contains(&p.kind()) {
            let owner = p
                .child_by_field_name("name")
                .or_else(|| p.child_by_field_name("type"))
                .map(|n| text(n, src).to_string());
            if let Some(owner) = owner {
                return format!("{owner}.{name}");
            }
            break;
        }
        if prof.units.contains(&p.kind()) {
            break;
        }
        cur = p.parent();
    }
    name
}

/// `app.post('/x', (req, res) => …)` → `app.post('/x')`; `const C = memo(() => …)` → `C`.
fn call_site_name(node: Node, src: &[u8]) -> Option<String> {
    let args = node.parent().filter(|p| p.kind() == "arguments")?;
    let call = args.parent().filter(|p| p.kind() == "call_expression")?;
    let mut owner = call.parent();
    while let Some(p) = owner {
        match p.kind() {
            "variable_declarator" => return p.child_by_field_name("name").map(|n| text(n, src).to_string()),
            "call_expression" | "arguments" | "member_expression" | "parenthesized_expression"
            | "as_expression" | "satisfies_expression" => owner = p.parent(),
            _ => break,
        }
    }
    let callee: String = text(call.child_by_field_name("function")?, src).chars().take(40).collect();
    let first_arg = args.named_child(0).filter(|a| a.kind() == "string").map(|a| text(a, src));
    Some(match first_arg {
        Some(a) => format!("{callee}({a})"),
        None => callee,
    })
}

fn count_params(node: Node, prof: &Profile, src: &[u8]) -> usize {
    let Some(params) = node
        .child_by_field_name("parameters")
        .or_else(|| {
            let mut c = node.walk();
            node.children(&mut c).find(|n| prof.params.contains(&n.kind()))
        })
    else {
        return 0;
    };
    let mut cursor = params.walk();
    params
        .named_children(&mut cursor)
        .filter(|n| !matches!(n.kind(), "comment" | "self_parameter" | "keyword_separator" | "positional_separator"))
        .filter(|n| !matches!(text(*n, src), "self" | "cls"))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::FileKind;

    #[test]
    fn python_with_try_else_and_separators_are_not_branches() {
        let src = "\
def f(path):
    with open(path) as fh:
        data = fh.read()
    try:
        x = int(data)
    except ValueError:   # +1
        x = 0
    else:
        x += 1
    for i in data:
        pass
    else:
        pass
    return x
def kw(a, *, b, **kw):
    return a
";
        let (_, fs) = analyze_file(&file("w.py", Language::Python, src), &Cfg::default(), &TestsCfg::default(), &NamingCfg::default(), &FallbackCfg::default());
        assert_eq!(fs[0].cognitive, 2, "{:?}", fs[0]); // except + for
        assert_eq!(fs[0].max_nesting, 1);
        assert_eq!(fs[1].params, 3);
    }

    #[test]
    fn unbound_top_level_callbacks_are_units() {
        let src = "\
app.post('/x', async (req, res) => {
  if (req.body.a) { if (req.body.b) { return; } }   // +1 +2
  items.forEach(x => { if (x) {} });               // +2 (nested callback)
});
export const Comp = React.forwardRef((props, ref) => { if (props.a) {} });
describe('suite', () => { it('works', () => { if (1) {} }); });
function sw(x: number) { switch (x) { case 1: return 1; case 2: return 2; default: return 0; } }
";
        let (_, fs) = analyze_file(&file("r.ts", Language::TypeScript, src), &Cfg::default(), &TestsCfg::default(), &NamingCfg::default(), &FallbackCfg::default());
        let names: Vec<&str> = fs.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["app.post('/x')", "Comp", "describe('suite')", "sw"], "{fs:?}");
        assert_eq!(fs[0].cognitive, 5, "{:?}", fs[0]);
        assert_eq!(fs[3].cyclomatic, 3, "{:?}", fs[3]); // 1 + two cases, not the switch head
    }

    #[test]
    fn deep_expression_does_not_overflow() {
        let expr = std::iter::repeat_n("a", 60_000).collect::<Vec<_>>().join(" + ");
        let src = format!("export const s = {expr};\nfunction f() {{ return {expr}; }}\n");
        let (fm, fs) = analyze_file(&file("deep.ts", Language::TypeScript, &src), &Cfg::default(), &TestsCfg::default(), &NamingCfg::default(), &FallbackCfg::default());
        assert!(!fm.parse_errors);
        assert_eq!(fs.len(), 1);
    }

    fn file(path: &str, lang: Language, content: &str) -> SourceFile {
        SourceFile {
            path: path.into(),
            lang,
            kind: FileKind::Source,
            lines: content.lines().count(),
            bytes: content.len(),
            content: content.into(),
        }
    }

    #[test]
    fn python_cognitive_matches_sonar_rules() {
        let src = "\
def f(a, b):
    if a and b or not a:   # +1 if, +1 and-run, +1 or
        for i in a:        # +2 (nested)
            while b:       # +3
                pass
    elif a:                # +1
        pass
    else:                  # +1
        pass
    return 1 if a else 2   # +1
";
        let (_, fs) = analyze_file(&file("f.py", Language::Python, src), &Cfg::default(), &TestsCfg::default(), &NamingCfg::default(), &FallbackCfg::default());
        assert_eq!(fs.len(), 1);
        let f = &fs[0];
        assert_eq!(f.name, "f");
        assert_eq!(f.params, 2);
        assert_eq!(f.cognitive, 11, "{f:?}");
        assert_eq!(f.max_nesting, 3);
    }

    #[test]
    fn ts_else_if_is_flat_and_methods_are_qualified() {
        let src = "\
class K {
  m(a: number, b?: string) {
    if (a) { } else if (b) { } else { }   // +1, +1, +1
    const cb = () => { if (a) {} };       // nested callable: if is +2
    items.forEach(x => { if (x) {} });    // inline callback: if is +2
    return a ? 1 : 2;                     // +1
  }
}
export const g = (x: number) => x && x;   // +1
";
        let (_, fs) = analyze_file(&file("k.ts", Language::TypeScript, src), &Cfg::default(), &TestsCfg::default(), &NamingCfg::default(), &FallbackCfg::default());
        let names: Vec<&str> = fs.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["K.m", "cb", "g"], "{fs:?}");
        assert_eq!(fs[0].cognitive, 8, "{:?}", fs[0]);
        assert_eq!(fs[0].params, 2);
        assert_eq!(fs[1].cognitive, 1);
        assert_eq!(fs[2].cognitive, 1);
    }

    #[test]
    fn rust_inline_test_units_are_tagged_and_left_out_of_file_totals() {
        let src = "\
fn f(a: u8) -> u8 { if a > 1 { 1 } else { 0 } }
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deep(a: u8) { if a > 1 { if a > 2 { if a > 3 { if a > 4 { if a > 5 { if a > 6 {} } } } } } }
}
";
        let f = file("t.rs", Language::Rust, src);
        let (fm, fs) = analyze_file(&f, &Cfg { cognitive_hard: 5, ..Cfg::default() }, &TestsCfg::default(), &NamingCfg::default(), &FallbackCfg::default());
        assert_eq!(fs.len(), 2, "{fs:?}");
        assert_eq!((fs[0].name.as_str(), fs[0].in_test), ("f", false));
        assert_eq!((fs[1].name.as_str(), fs[1].in_test), ("deep", true));
        assert!(fs[1].cognitive > 5);
        assert_eq!((fm.functions, fm.total_cognitive, fm.max_cognitive, fm.complex_functions), (1, 2, 2, 0), "{fm:?}");
        assert_eq!(fm.inline_test_lines, 5);
        assert_eq!(fm.test_regions.len(), 1);
        // The knob turns the effects off: every unit is source and every line counts, but the
        // region is still reported.
        let off = TestsCfg { inline_modules: false, ..TestsCfg::default() };
        let (fm, fs) = analyze_file(&f, &Cfg { cognitive_hard: 5, ..Cfg::default() }, &off, &NamingCfg::default(), &FallbackCfg::default());
        assert!(fs.iter().all(|f| !f.in_test));
        assert_eq!((fm.functions, fm.complex_functions, fm.inline_test_lines), (2, 1, 0));
        assert_eq!(fm.test_regions.len(), 1);
    }

    #[test]
    fn locals_are_distinct_binding_names_with_patterns_as_one_and_closures_folded() {
        let src = "\
fn f(a: u8, b: u8) {
    let _ = a;
    let mut x = 1;
    let (p, q) = (1, 2);
    let (p, q) = (3, 4);
    let Some(z) = Some(1) else { return };
    for (k, v) in m { }
    for t in xs { }
    if let Some(w) = o { }
    while let Some(w2) = it.next() { }
    let c = |y| { let inner = y; inner };
    xs.iter().map(|e| { let mapped = e; mapped });
    fn nested(n0: u8) { let n = 1; let n2 = 2; }
    match a { Some(m) => {}, _ => {} }
    x = 2;
    let x = 3;
}
";
        let (_, fs) = analyze_file(&file("l.rs", Language::Rust, src), &Cfg::default(), &TestsCfg::default(), &NamingCfg::default(), &FallbackCfg::default());
        let by: Vec<(&str, usize)> = fs.iter().map(|f| (f.name.as_str(), f.locals)).collect();
        // x, (p, q), Some(z), (k, v), t, c, inner, mapped: parameters, `_`, the `if let` /
        // `while let` / match-arm names, the rebinding of x and the nested fn's names do not count.
        assert_eq!(by, vec![("f", 8), ("nested", 2)], "{by:?}");
        assert!(fs.iter().all(|f| !f.brain && f.longest_locals.is_empty()));
    }

    #[test]
    fn python_locals_count_assignment_for_with_and_walrus_once_per_name() {
        let src = "\
def f(a):
    x = 1
    x: int = 2
    a, b = 1, 2
    self.y = 5
    xs[0] = 6
    for i, j in xs: pass
    with open(p) as fh: pass
    if (n := 3): pass
    x += 1
    l = lambda y: (z := y)
    def g(): w = 1
    e = a = 3
";
        let (_, fs) = analyze_file(&file("l.py", Language::Python, src), &Cfg::default(), &TestsCfg::default(), &NamingCfg::default(), &FallbackCfg::default());
        let by: Vec<(&str, usize)> = fs.iter().map(|f| (f.name.as_str(), f.locals)).collect();
        // x, `a, b`, `i, j`, fh, n, l, z (lambda body folds in), e; `self.y`, `xs[0]`, `+=`, the
        // chained inner `a = 3` and the nested def's w do not count.
        assert_eq!(by, vec![("f", 8), ("g", 1)], "{by:?}");
    }

    #[test]
    fn ts_locals_count_declarators_and_for_targets_and_keep_bound_arrows_apart() {
        let src = "\
function f(a: number) {
  let x = 1, y = 2;
  const { p, q } = o;
  var [r, s] = t;
  for (let i = 0; i < 3; i++) {}
  for (const k of xs) {}
  for (var m in o) {}
  const cb = () => { let inner = 1; };
  items.map(z => { let w = z; });
  function g() { let n = 1; }
  x = 3;
  try {} catch (e) {}
}
";
        let (_, fs) = analyze_file(&file("l.ts", Language::TypeScript, src), &Cfg::default(), &TestsCfg::default(), &NamingCfg::default(), &FallbackCfg::default());
        let by: Vec<(&str, usize)> = fs.iter().map(|f| (f.name.as_str(), f.locals)).collect();
        // x, y, {p, q}, [r, s], i, k, m, cb, w (inline callback folds in); the bound arrow's and
        // the nested function's bodies, the parameter, the plain assignment and `catch (e)` do not.
        assert_eq!(by, vec![("f", 9), ("cb", 1), ("g", 1)], "{by:?}");
    }

    #[test]
    fn brain_method_needs_all_three_floors_and_names_the_longest_lived_locals() {
        // A 13-line unit binding 6 locals with cognitive 2: the floors decide.
        let src = "\
fn f(a: u8) -> u8 {
    let first = a;
    let second = a;
    let (pat, tern) = (a, a);
    let p = a;
    let third = a;
    if a > 1 { let late = a; }
    if a > 2 { }
    let _ = p;
    let _ = second;
    let _ = first;
    let _ = 0;
}
";
        let f = file("b.rs", Language::Rust, src);
        let brain = |lines: usize, cognitive: u32, locals: usize| Cfg { brain_min_lines: lines, brain_min_cognitive: cognitive, brain_min_locals: locals, ..Cfg::default() };
        let (_, fs) = analyze_file(&f, &brain(10, 2, 6), &TestsCfg::default(), &NamingCfg::default(), &FallbackCfg::default());
        let u = &fs[0];
        assert_eq!((u.lines, u.cognitive, u.locals), (13, 2, 6), "{u:?}");
        assert!(u.brain);
        // Longest first-to-last mention: first 2-11, second 3-10, p 5-9; the rest never reach the list.
        let spans: Vec<(&str, usize, usize)> = u.longest_locals.iter().map(|l| (l.name.as_str(), l.first_line, l.last_line)).collect();
        assert_eq!(spans, vec![("first", 2, 11), ("second", 3, 10), ("p", 5, 9)], "{spans:?}");
        // The one-letter `p` was measured by the naming pass: its span is the measured one.
        assert_eq!(u.short_bindings.iter().find(|s| s.name == "p").map(|s| (s.decl_line, s.last_line)), Some((5, 9)));
        // One floor missed each way: no label, no list.
        for cfg in [brain(14, 2, 6), brain(10, 3, 6), brain(10, 2, 7)] {
            let (_, fs) = analyze_file(&f, &cfg, &TestsCfg::default(), &NamingCfg::default(), &FallbackCfg::default());
            assert!(!fs[0].brain && fs[0].longest_locals.is_empty(), "{cfg:?}");
        }
        assert_eq!(Cfg::default(), Cfg { cognitive_hard: 15, brain_min_lines: 100, brain_min_cognitive: 15, brain_min_locals: 15 });
    }

    #[test]
    fn rust_regions_sharing_a_line_are_counted_once() {
        let src = "#[cfg(test)] use a; #[cfg(test)] use b;\nfn f() {}\n";
        let (fm, _) = analyze_file(&file("t.rs", Language::Rust, src), &Cfg::default(), &TestsCfg::default(), &NamingCfg::default(), &FallbackCfg::default());
        assert_eq!((fm.test_regions.len(), fm.inline_test_lines), (2, 1), "{fm:?}");
    }

    #[test]
    fn rust_match_and_else_if() {
        let src = "\
impl S {
    fn m(&self, a: u8) -> u8 {
        if a > 1 && a < 5 || a == 9 { 1 } else if a == 2 { 2 } else { 3 }  // +1 +1 +1 +1 +1
    }
}
fn f(a: Option<u8>) {
    match a { Some(x) if x > 1 => {}, _ => {} }   // +1 (cyclomatic +2 arms)
    for i in 0..3 { let c = |x| if x { 1 } else { 0 }; }  // for +1, closure nests: if +3, else +1
}
";
        let (_, fs) = analyze_file(&file("s.rs", Language::Rust, src), &Cfg::default(), &TestsCfg::default(), &NamingCfg::default(), &FallbackCfg::default());
        assert_eq!(fs[0].name, "S.m");
        assert_eq!(fs[0].cognitive, 5, "{:?}", fs[0]);
        assert_eq!(fs[0].params, 1);
        assert_eq!(fs[1].cognitive, 6, "{:?}", fs[1]);
        assert_eq!(fs[1].cyclomatic, 1 + 2 + 1 + 1 /* two arms, for, if */, "{:?}", fs[1]);
    }
}
