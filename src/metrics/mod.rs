//! Per-function complexity from tree-sitter.
//!
//! Cognitive complexity follows the SonarSource definition: control-flow
//! breaks cost 1, each level of nesting they sit in costs 1 more, `else`/`elif`
//! cost 1 flat, and a run of the same boolean operator costs 1 for the run.
//! Cyclomatic complexity is the classic 1 + decision points and is kept
//! because it is what most people have an intuition for, but nesting-aware
//! cognitive complexity is the one that predicts "hard to change safely".

use crate::discover::SourceFile;
use crate::lang::Language;
use rayon::prelude::*;
use serde::Serialize;
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
}

pub const COGNITIVE_HARD: u32 = 15;

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
};

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

pub fn analyze_all(files: &[SourceFile]) -> (Vec<FileMetrics>, Vec<FunctionMetrics>) {
    let per_file: Vec<(FileMetrics, Vec<FunctionMetrics>)> =
        files.par_iter().map(analyze_file).collect();
    let mut file_metrics = Vec::with_capacity(per_file.len());
    let mut funcs = Vec::new();
    for (fm, fs) in per_file {
        file_metrics.push(fm);
        funcs.extend(fs);
    }
    (file_metrics, funcs)
}

pub fn analyze_file(file: &SourceFile) -> (FileMetrics, Vec<FunctionMetrics>) {
    let mut parser = file.lang.parser();
    let prof = profile(file.lang);
    let src = file.content.as_bytes();
    let mut funcs = Vec::new();
    let mut parse_errors = false;
    if let Some(tree) = parser.parse(src, None) {
        let root = tree.root_node();
        parse_errors = root.has_error();
        collect_units(root, prof, src, &file.path, &mut funcs);
    }
    let fm = FileMetrics {
        path: file.path.clone(),
        functions: funcs.len(),
        total_cognitive: funcs.iter().map(|f| f.cognitive).sum(),
        max_cognitive: funcs.iter().map(|f| f.cognitive).max().unwrap_or(0),
        max_nesting: funcs.iter().map(|f| f.max_nesting).max().unwrap_or(0),
        complex_functions: funcs.iter().filter(|f| f.cognitive > COGNITIVE_HARD).count(),
        parse_errors,
    };
    (fm, funcs)
}

fn collect_units(root: Node, prof: &Profile, src: &[u8], path: &str, out: &mut Vec<FunctionMetrics>) {
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
            out.push(measure(node, prof, src, path));
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push((child, in_unit || is_unit));
        }
    }
}

fn measure(node: Node, prof: &Profile, src: &[u8], path: &str) -> FunctionMetrics {
    let mut acc = Acc::default();
    // Children of the unit start at nesting 0; the unit itself is not a nesting level.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, prof, src, 0, &mut acc);
    }
    let start_line = node.start_position().row + 1;
    let end_line = node.end_position().row + 1;
    FunctionMetrics {
        file: path.to_string(),
        name: unit_name(node, prof, src),
        start_line,
        end_line,
        lines: end_line - start_line + 1,
        params: count_params(node, prof, src),
        cyclomatic: 1 + acc.cyclomatic,
        cognitive: acc.cognitive,
        max_nesting: acc.max_nesting,
    }
}

#[derive(Default)]
struct Acc {
    cognitive: u32,
    cyclomatic: u32,
    max_nesting: u32,
}

fn walk(root: Node, prof: &Profile, src: &[u8], nesting: u32, acc: &mut Acc) {
    let mut stack: Vec<(Node, u32)> = vec![(root, nesting)];
    while let Some((node, nesting)) = stack.pop() {
        let kind = node.kind();
        let mut child_nesting = nesting;

        if prof.units.contains(&kind) || prof.nest_only.contains(&kind) {
            // Nested callable: its body sits one level deeper, no increment of its own.
            child_nesting = nesting + 1;
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

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push((child, child_nesting));
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
        let (_, fs) = analyze_file(&file("w.py", Language::Python, src));
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
        let (_, fs) = analyze_file(&file("r.ts", Language::TypeScript, src));
        let names: Vec<&str> = fs.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["app.post('/x')", "Comp", "describe('suite')", "sw"], "{fs:?}");
        assert_eq!(fs[0].cognitive, 5, "{:?}", fs[0]);
        assert_eq!(fs[3].cyclomatic, 3, "{:?}", fs[3]); // 1 + two cases, not the switch head
    }

    #[test]
    fn deep_expression_does_not_overflow() {
        let expr = std::iter::repeat_n("a", 60_000).collect::<Vec<_>>().join(" + ");
        let src = format!("export const s = {expr};\nfunction f() {{ return {expr}; }}\n");
        let (fm, fs) = analyze_file(&file("deep.ts", Language::TypeScript, &src));
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
        let (_, fs) = analyze_file(&file("f.py", Language::Python, src));
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
        let (_, fs) = analyze_file(&file("k.ts", Language::TypeScript, src));
        let names: Vec<&str> = fs.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["K.m", "cb", "g"], "{fs:?}");
        assert_eq!(fs[0].cognitive, 8, "{:?}", fs[0]);
        assert_eq!(fs[0].params, 2);
        assert_eq!(fs[1].cognitive, 1);
        assert_eq!(fs[2].cognitive, 1);
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
        let (_, fs) = analyze_file(&file("s.rs", Language::Rust, src));
        assert_eq!(fs[0].name, "S.m");
        assert_eq!(fs[0].cognitive, 5, "{:?}", fs[0]);
        assert_eq!(fs[0].params, 1);
        assert_eq!(fs[1].cognitive, 6, "{:?}", fs[1]);
        assert_eq!(fs[1].cyclomatic, 1 + 2 + 1 + 1 /* two arms, for, if */, "{:?}", fs[1]);
    }
}
