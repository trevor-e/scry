//! Short-name live range (P48): one-letter bindings and how far apart their uses sit.
//!
//! An LLM reaches for `t`, `p`, `h` in a 250-line function the way a human does in a
//! five-line one, so the reader meets `p` on line 830 and scrolls back to 690 to learn it is
//! the proposal. The measure is not declaration-to-last-use (that is the block length the
//! report ranks already) but the widest gap between two consecutive uses, with every subtree
//! that rebinds the letter (`|t|` closures, `Some(t)` arms, `for t`, lambda and comprehension
//! targets) left out of the search and a later `let t` / assignment ending it. The result is
//! an attribute on units the report already ranks and never a score input by default.

use crate::config::Naming as Cfg;
use crate::lang::Language;
use crate::metrics;
use serde::Serialize;
use std::collections::HashSet;
use tree_sitter::Node;

/// One short binding of a unit and its use pattern.
#[derive(Debug, Clone, Serialize)]
pub struct ShortBinding {
    pub name: String,
    /// The line of the `let` / assignment / `for` / `with` that binds it…
    pub decl_line: usize,
    /// …and of its last use (the declaration line when it is never read).
    pub last_line: usize,
    /// Use sites after the declaration, inside the enclosing block and outside every rebinding
    /// subtree.
    pub uses: usize,
    /// Largest line distance between two consecutive uses, the declaration counting as the
    /// first; what the `short_name_min_gap` gate reads.
    pub max_gap: usize,
    /// The two uses the widest gap lies between.
    pub gap_from: usize,
    pub gap_to: usize,
    /// `last_line - decl_line`: the raw live span, kept for the JSON and gated on nowhere.
    pub span: usize,
    /// Lines of the enclosing block (the loop body for a `for` binding).
    pub block_lines: usize,
}

/// What one unit binds.
#[derive(Debug, Clone, Default)]
pub struct UnitBindings {
    /// Every binding of the unit, short or not (parameters excluded).
    pub bindings: usize,
    /// The short ones, with their use pattern, in declaration order.
    pub short: Vec<ShortBinding>,
    /// Short bindings whose `max_gap` reaches `short_name_min_gap`; 0 when the unit binds fewer
    /// than `short_name_min_other_bindings` other names.
    pub long_short_bindings: usize,
}

/// A binding found in a unit: where the search for its uses starts and how far it reaches.
struct Binding<'t> {
    name: String,
    decl_line: usize,
    /// The subtree the uses are searched in: the enclosing block, a loop or `with` body, a
    /// `for (…;…;…)` statement.
    scope: Node<'t>,
    /// Uses start at this byte: after the binding statement, or at the body of a loop.
    from: usize,
}

/// The bindings of one unit node, the short ones measured. Nested units are left to their own
/// measurement; closures and inline callbacks belong to the unit like the rest of its body.
pub fn unit_bindings(unit: Node, lang: Language, src: &[u8], cfg: &Cfg) -> UnitBindings {
    let bindings = collect(unit, lang, src, cfg);
    let mut out = UnitBindings { bindings: bindings.len(), ..Default::default() };
    for b in &bindings {
        if is_short(&b.name, cfg) {
            out.short.push(measure(b, lang, src));
        }
    }
    if bindings.len() > cfg.short_name_min_other_bindings {
        out.long_short_bindings = out.short.iter().filter(|s| s.max_gap >= cfg.short_name_min_gap).count();
    }
    out
}

/// The attribute line for a unit: `render_page_html (lines 532-746): this block is too long
/// to carry a one-letter name: h (declared line 538, last used line 745, 7 uses, widest gap
/// 104 lines between lines 601 and 705, in a 208-line block)`, then `; 3 one-letter bindings
/// have a use gap of 30+ lines: p, o, h` when the unit has two or more. `None` when it has none.
pub fn unit_reason(name: &str, start_line: usize, end_line: usize, short: &[ShortBinding], long_short_bindings: usize, cfg: &Cfg) -> Option<String> {
    if long_short_bindings == 0 {
        return None;
    }
    let mut far: Vec<&ShortBinding> = short.iter().filter(|s| s.max_gap >= cfg.short_name_min_gap).collect();
    far.sort_by(|a, b| b.max_gap.cmp(&a.max_gap).then(a.decl_line.cmp(&b.decl_line)));
    let w = far.first()?;
    let mut r = format!(
        "{name} (lines {start_line}-{end_line}): this block is too long to carry a one-letter name: {} (declared line {}, last used line {}, {} use{}, widest gap {} lines between lines {} and {}, in a {}-line block)",
        w.name, w.decl_line, w.last_line, w.uses, if w.uses == 1 { "" } else { "s" }, w.max_gap, w.gap_from, w.gap_to, w.block_lines
    );
    if long_short_bindings >= 2 {
        let mut names: Vec<&str> = Vec::new();
        for s in &far {
            if !names.contains(&s.name.as_str()) {
                names.push(&s.name);
            }
        }
        r.push_str(&format!("; {long_short_bindings} one-letter bindings have a use gap of {}+ lines: {}", cfg.short_name_min_gap, names.join(", ")));
    }
    Some(r)
}

fn is_short(name: &str, cfg: &Cfg) -> bool {
    !name.starts_with('_') && name.chars().count() <= cfg.short_name_max_len && !cfg.short_name_allow.iter().any(|a| a == name)
}

fn text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    node.utf8_text(src).unwrap_or("")
}

fn line(node: Node) -> usize {
    node.start_position().row + 1
}

fn lines_of(node: Node) -> usize {
    node.end_position().row - node.start_position().row + 1
}

/// The nearest ancestor of one of `kinds`, stopping at the unit.
fn ancestor<'t>(node: Node<'t>, kinds: &[&str], unit: Node<'t>) -> Option<Node<'t>> {
    let mut cur = node.parent();
    while let Some(p) = cur {
        if kinds.contains(&p.kind()) {
            return Some(p);
        }
        if p.id() == unit.id() {
            break;
        }
        cur = p.parent();
    }
    None
}

/// Every binding of `unit`, in document order. The walk skips nested units (their bindings are
/// theirs) and, per grammar, reads the binding forms of the spec: Rust `let` and `for`
/// patterns, Python first-assignment / `for` / `with … as`, TS `variable_declarator` and
/// `for … of/in`.
fn collect<'t>(unit: Node<'t>, lang: Language, src: &[u8], cfg: &Cfg) -> Vec<Binding<'t>> {
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut stack: Vec<Node<'t>> = Vec::new();
    let mut cursor = unit.walk();
    for c in unit.children(&mut cursor).collect::<Vec<_>>().into_iter().rev() {
        stack.push(c);
    }
    while let Some(node) = stack.pop() {
        if metrics::is_nested_unit(node, lang) {
            continue;
        }
        let kind = node.kind();
        match lang {
            Language::Rust => match kind {
                "let_declaration" => {
                    if let (Some(pat), Some(scope)) = (node.child_by_field_name("pattern"), node.parent()) {
                        for name in pattern_names(pat, lang, src, cfg) {
                            out.push(Binding { name, decl_line: line(node), scope, from: node.end_byte() });
                        }
                    }
                }
                "for_expression" => {
                    if let (Some(pat), Some(body)) = (node.child_by_field_name("pattern"), node.child_by_field_name("body")) {
                        for name in pattern_names(pat, lang, src, cfg) {
                            out.push(Binding { name, decl_line: line(node), scope: body, from: body.start_byte() });
                        }
                    }
                }
                _ => {}
            },
            Language::Python => match kind {
                "expression_statement" => {
                    if let Some(a) = node.named_child(0).filter(|a| a.kind() == "assignment")
                        && let Some(left) = a.child_by_field_name("left")
                    {
                        let scope = ancestor(node, &["block"], unit).unwrap_or(unit);
                        for name in pattern_names(left, lang, src, cfg) {
                            // The first assignment per name is the binding; later ones rebind.
                            if seen.insert(name.clone()) {
                                out.push(Binding { name, decl_line: line(node), scope, from: node.end_byte() });
                            }
                        }
                    }
                }
                "for_statement" => {
                    if let (Some(left), Some(body)) = (node.child_by_field_name("left"), node.child_by_field_name("body")) {
                        for name in pattern_names(left, lang, src, cfg) {
                            seen.insert(name.clone());
                            out.push(Binding { name, decl_line: line(node), scope: body, from: body.start_byte() });
                        }
                    }
                }
                "with_statement" => {
                    if let Some(body) = node.child_by_field_name("body") {
                        for alias in with_aliases(node) {
                            for name in pattern_names(alias, lang, src, cfg) {
                                seen.insert(name.clone());
                                out.push(Binding { name, decl_line: line(node), scope: body, from: body.start_byte() });
                            }
                        }
                    }
                }
                _ => {}
            },
            _ => match kind {
                "lexical_declaration" | "variable_declaration" => {
                    let parent = node.parent();
                    // `for (let i = 0; …)`: the whole loop is the scope, else the enclosing block.
                    let scope = match parent {
                        Some(p) if p.kind() == "for_statement" => p,
                        _ => ancestor(node, &["statement_block"], unit).unwrap_or(unit),
                    };
                    let mut c = node.walk();
                    for d in node.named_children(&mut c).filter(|d| d.kind() == "variable_declarator") {
                        if let Some(pat) = d.child_by_field_name("name") {
                            for name in pattern_names(pat, lang, src, cfg) {
                                out.push(Binding { name, decl_line: line(d), scope, from: node.end_byte() });
                            }
                        }
                    }
                }
                "for_in_statement" => {
                    if let (Some(left), Some(body)) = (node.child_by_field_name("left"), node.child_by_field_name("body")) {
                        for name in pattern_names(left, lang, src, cfg) {
                            out.push(Binding { name, decl_line: line(node), scope: body, from: body.start_byte() });
                        }
                    }
                }
                _ => {}
            },
        }
        let mut c = node.walk();
        let children: Vec<Node> = node.children(&mut c).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    out
}

/// The `with_item`s of a Python `with` statement.
fn with_items(with: Node) -> Vec<Node> {
    let mut out = Vec::new();
    let mut c = with.walk();
    for clause in with.named_children(&mut c).filter(|n| n.kind() == "with_clause").collect::<Vec<_>>() {
        let mut cc = clause.walk();
        out.extend(clause.named_children(&mut cc).filter(|n| n.kind() == "with_item"));
    }
    out
}

/// The `as` targets of a Python `with` statement.
fn with_aliases(with: Node) -> Vec<Node> {
    with_items(with).into_iter().filter_map(|item| item.child_by_field_name("value").filter(|v| v.kind() == "as_pattern")?.child_by_field_name("alias")).collect()
}

/// The context expressions of a Python `with` statement (`open(p)` in `with open(p) as p`).
fn with_values(with: Node) -> Vec<Node> {
    with_items(with).into_iter().filter_map(|item| { let v = item.child_by_field_name("value")?; if v.kind() == "as_pattern" { v.named_child(0) } else { Some(v) } }).collect()
}

/// The names a binding pattern introduces: the bare identifier, or with
/// `short_name_include_pattern_bindings` every identifier the pattern binds (constructor and
/// field names, subscript / attribute targets and default values excluded).
fn pattern_names(pat: Node, lang: Language, src: &[u8], cfg: &Cfg) -> Vec<String> {
    let bare = match lang {
        Language::Python => matches!(pat.kind(), "identifier" | "as_pattern_target"),
        _ => pat.kind() == "identifier",
    };
    if bare {
        let id = if pat.kind() == "as_pattern_target" { pat.named_child(0) } else { Some(pat) };
        return id.filter(|n| n.kind() == "identifier").map(|n| vec![text(n, src).to_string()]).unwrap_or_default();
    }
    if !cfg.short_name_include_pattern_bindings {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut stack = vec![pat];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "identifier" | "shorthand_property_identifier_pattern" => out.push(text(n, src).to_string()),
            // Targets that are not new names, and default values.
            "attribute" | "subscript" | "member_expression" | "subscript_expression" | "scoped_identifier" | "call" => {}
            "assignment_pattern" => {
                if let Some(l) = n.child_by_field_name("left") {
                    stack.push(l);
                }
            }
            "tuple_struct_pattern" | "struct_pattern" => {
                let mut c = n.walk();
                for ch in n.children(&mut c).collect::<Vec<_>>().into_iter().rev() {
                    if !matches!(ch.kind(), "identifier" | "scoped_identifier") || n.child_by_field_name("type").is_none_or(|t| t.id() != ch.id()) {
                        stack.push(ch);
                    }
                }
            }
            "field_pattern" => {
                // `S { p }` binds p; `S { p: q }` binds q; `S { p: 0 }` binds nothing.
                match n.child_by_field_name("pattern") {
                    Some(p) => stack.push(p),
                    None => {
                        if let Some(id) = n.child_by_field_name("name").filter(|i| i.kind() == "shorthand_field_identifier") {
                            out.push(text(id, src).to_string());
                        }
                    }
                }
            }
            "pair_pattern" => {
                if let Some(v) = n.child_by_field_name("value") {
                    stack.push(v);
                }
            }
            _ => {
                let mut c = n.walk();
                for ch in n.children(&mut c).collect::<Vec<_>>().into_iter().rev() {
                    stack.push(ch);
                }
            }
        }
    }
    out
}

/// Does `pat` (a parameter list, match pattern, `let` pattern, comprehension target…) bind
/// `name`? Any identifier of that text inside it counts, except under an attribute, member or
/// subscript target (`self.p = …`, `d[p] = …` bind nothing); a constructor or field named by
/// one letter is not worth a second walk.
fn binds(pat: Node, name: &str, src: &[u8]) -> bool {
    let mut stack = vec![pat];
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "identifier" | "shorthand_property_identifier_pattern" | "shorthand_field_identifier") && text(n, src) == name {
            return true;
        }
        if matches!(n.kind(), "attribute" | "subscript" | "member_expression" | "subscript_expression") {
            continue;
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    false
}

/// What the search does with a node that may rebind the name.
enum Step<'t> {
    /// Plain: descend into every child.
    Descend,
    /// A rebinding subtree: skipped whole.
    Skip,
    /// Descend into these children only (a rebinding `let` reads the old name in its value; a
    /// Python attribute's `.name` is not an identifier of the local).
    Only(Vec<Node<'t>>),
    /// A rebinding of the name in the block `block_id`: its values are searched, and nothing
    /// after it in that block counts.
    Rebind(Vec<Node<'t>>, usize),
}

fn field_binds(node: Node, field: &str, name: &str, src: &[u8]) -> bool {
    node.child_by_field_name(field).is_some_and(|f| binds(f, name, src))
}

/// `if let` / `while let` (and let chains): when a `let_condition` in the condition binds
/// `name`, the parts that still read the outer name: the values of the let conditions and the
/// plain conditions up to the binding one (`if let Some(t) = t.next()` reads the outer `t` on
/// its right), and the `else` branch. `None` when nothing in the condition binds it.
fn outer_reads_of_condition<'t>(node: Node<'t>, name: &str, src: &[u8]) -> Option<Vec<Node<'t>>> {
    let cond = node.child_by_field_name("condition")?;
    let mut c = cond.walk();
    let parts: Vec<Node> = if cond.kind() == "let_chain" { cond.named_children(&mut c).collect() } else { vec![cond] };
    let mut out = Vec::new();
    let mut bound = false;
    for p in parts {
        if p.kind() == "let_condition" {
            out.extend(p.child_by_field_name("value"));
            if field_binds(p, "pattern", name, src) {
                bound = true;
                break;
            }
        } else {
            out.push(p);
        }
    }
    if !bound {
        return None;
    }
    out.extend(node.child_by_field_name("alternative"));
    Some(out)
}

fn step<'t>(node: Node<'t>, parent_id: usize, lang: Language, name: &str, src: &[u8]) -> Step<'t> {
    let kind = node.kind();
    let skip_if = |b: bool| if b { Step::Skip } else { Step::Descend };
    // A loop rebinding the name: its iterable still reads the outer one; the body is the new name's.
    let only_iter = |field: &str| if field_binds(node, field, name, src) { Step::Only(node.child_by_field_name(if field == "left" { "right" } else { "value" }).into_iter().collect()) } else { Step::Descend };
    match lang {
        Language::Rust => match kind {
            "closure_expression" => skip_if(field_binds(node, "parameters", name, src)),
            "match_arm" => skip_if(field_binds(node, "pattern", name, src)),
            "if_expression" | "while_expression" => match outer_reads_of_condition(node, name, src) {
                Some(reads) => Step::Only(reads),
                None => Step::Descend,
            },
            "for_expression" => only_iter("pattern"),
            "let_declaration" => {
                if field_binds(node, "pattern", name, src) {
                    Step::Rebind(node.child_by_field_name("value").into_iter().collect(), parent_id)
                } else {
                    Step::Descend
                }
            }
            // Items see no local; lifetimes and labels are spelled with an identifier child.
            "function_item" | "const_item" | "static_item" | "struct_item" | "enum_item" | "impl_item" | "trait_item"
            | "mod_item" | "macro_definition" | "type_item" | "use_declaration" | "attribute_item" | "lifetime" | "label" => Step::Skip,
            _ => Step::Descend,
        },
        Language::Python => match kind {
            // A nested `def p` / `class p` rebinds the name like an assignment.
            "function_definition" | "class_definition" if field_binds(node, "name", name, src) => Step::Rebind(Vec::new(), parent_id),
            "lambda" | "function_definition" => skip_if(field_binds(node, "parameters", name, src)),
            "list_comprehension" | "set_comprehension" | "dictionary_comprehension" | "generator_expression" => {
                let mut c = node.walk();
                skip_if(node.named_children(&mut c).any(|ch| ch.kind() == "for_in_clause" && field_binds(ch, "left", name, src)))
            }
            "for_statement" => only_iter("left"),
            // `with open(p) as p`: the context expressions read the outer name; the body is the new one's.
            "with_statement" => {
                if with_aliases(node).into_iter().any(|a| binds(a, name, src)) { Step::Only(with_values(node)) } else { Step::Descend }
            }
            "except_clause" => {
                let mut c = node.walk();
                skip_if(node.named_children(&mut c).any(|ch| ch.kind() == "as_pattern" && field_binds(ch, "alias", name, src)))
            }
            "case_clause" => {
                let mut c = node.walk();
                skip_if(node.named_children(&mut c).any(|ch| ch.kind() == "case_pattern" && binds(ch, name, src)))
            }
            "expression_statement" => match node.named_child(0).filter(|a| a.kind() == "assignment") {
                Some(a) if field_binds(a, "left", name, src) => Step::Rebind(a.child_by_field_name("right").into_iter().collect(), parent_id),
                _ => Step::Descend,
            },
            // A walrus rebinds the name; its value reads the old one.
            "named_expression" if field_binds(node, "name", name, src) => Step::Only(node.child_by_field_name("value").into_iter().collect()),
            "attribute" => Step::Only(node.child_by_field_name("object").into_iter().collect()),
            "keyword_argument" => Step::Only(node.child_by_field_name("value").into_iter().collect()),
            "global_statement" | "nonlocal_statement" | "import_statement" | "import_from_statement" | "dotted_name" | "decorator" => Step::Skip,
            _ => Step::Descend,
        },
        _ => match kind {
            // A bare arrow parameter (`t => t.id`) is the `parameter` field, not `parameters`.
            "arrow_function" | "function_expression" | "function_declaration" | "generator_function" | "generator_function_declaration"
            | "method_definition" => skip_if(field_binds(node, "parameters", name, src) || field_binds(node, "parameter", name, src)),
            "catch_clause" => skip_if(field_binds(node, "parameter", name, src)),
            "for_in_statement" => only_iter("left"),
            "lexical_declaration" | "variable_declaration" => {
                let mut c = node.walk();
                let decls: Vec<Node> = node.named_children(&mut c).filter(|d| d.kind() == "variable_declarator").collect();
                if decls.iter().any(|d| field_binds(*d, "name", name, src)) {
                    Step::Rebind(decls.iter().filter_map(|d| d.child_by_field_name("value")).collect(), parent_id)
                } else {
                    Step::Descend
                }
            }
            "import_statement" | "export_statement" | "type_alias_declaration" | "interface_declaration" => Step::Skip,
            _ => Step::Descend,
        },
    }
}

fn is_use(node: Node, lang: Language) -> bool {
    match lang {
        Language::Rust | Language::Python => node.kind() == "identifier",
        _ => matches!(node.kind(), "identifier" | "shorthand_property_identifier"),
    }
}

/// The use pattern of one binding: every identifier of its name in its scope after the
/// declaration, rebinding subtrees left out, the range ended by a later binding of the name in
/// the same block.
fn measure(b: &Binding, lang: Language, src: &[u8]) -> ShortBinding {
    let name = b.name.as_str();
    let mut uses: Vec<usize> = Vec::new();
    // Blocks a rebinding ended: their children still on the stack are the new name's.
    let mut ended: Vec<usize> = Vec::new();
    let mut stack: Vec<(Node, usize)> = vec![(b.scope, 0)];
    while let Some((node, parent_id)) = stack.pop() {
        if node.end_byte() <= b.from || ended.contains(&parent_id) {
            continue;
        }
        if is_use(node, lang) {
            if text(node, src) == name {
                uses.push(line(node));
            }
            continue;
        }
        let children: Vec<Node> = match step(node, parent_id, lang, name, src) {
            Step::Skip => continue,
            Step::Only(v) => v,
            Step::Rebind(v, block) => {
                ended.push(block);
                v
            }
            Step::Descend => {
                let mut c = node.walk();
                node.children(&mut c).collect()
            }
        };
        let id = node.id();
        for child in children.into_iter().rev() {
            stack.push((child, id));
        }
    }
    uses.sort_unstable();
    let mut max_gap = 0;
    let (mut gap_from, mut gap_to) = (b.decl_line, b.decl_line);
    let mut prev = b.decl_line;
    for &u in &uses {
        if u.saturating_sub(prev) > max_gap {
            max_gap = u - prev;
            gap_from = prev;
            gap_to = u;
        }
        prev = u;
    }
    let last_line = uses.last().copied().unwrap_or(b.decl_line);
    ShortBinding {
        name: b.name.clone(),
        decl_line: b.decl_line,
        last_line,
        uses: uses.len(),
        max_gap,
        gap_from,
        gap_to,
        span: last_line.saturating_sub(b.decl_line),
        block_lines: lines_of(b.scope),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Metrics as MetricsCfg, Tests as TestsCfg};
    use crate::discover::{FileKind, SourceFile};
    use crate::metrics::FunctionMetrics;

    fn file(path: &str, lang: Language, content: &str) -> SourceFile {
        SourceFile { path: path.into(), lang, kind: FileKind::Source, lines: content.lines().count(), bytes: content.len(), content: content.into() }
    }

    fn units(path: &str, lang: Language, src: &str, cfg: &Cfg) -> Vec<FunctionMetrics> {
        let (_, fs) = metrics::analyze_file(&file(path, lang, src), &MetricsCfg::default(), &TestsCfg::default(), cfg, &crate::config::Fallback::default());
        fs
    }

    fn short<'a>(f: &'a FunctionMetrics, name: &str) -> &'a ShortBinding {
        f.short_bindings.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("no binding {name} in {:?}", f.short_bindings))
    }

    /// Lines of filler so a use lands on a chosen line.
    fn pad(n: usize) -> String {
        "    x();\n".repeat(n)
    }

    #[test]
    fn rust_gap_is_between_consecutive_uses_and_rebindings_are_excluded() {
        let cfg = Cfg::default();
        let src = format!(
            "fn f(s: &S) -> String {{\n    let mut h = String::new();\n    let p = s.open();\n    let q = 1;\n{}    h.push('a');\n{}    let r = s.items().map(|p| p + 1);\n    if let Some(p) = r.first() {{ p.go(); }}\n    match r {{ Some(p) => p.go(), None => {{}} }}\n    for p in r.iter() {{ p.go(); }}\n{}    p.close();\n    let p = p.trim();\n    p.done();\n    h\n}}\n",
            pad(20), pad(20), pad(10)
        );
        let fs = units("a.rs", Language::Rust, &src, &cfg);
        let f = &fs[0];
        // h, p, q, r, the `for p` loop variable and the shadowing `let p` are the unit's
        // bindings; closure, match-arm and if-let patterns are not.
        assert_eq!(f.bindings, 6, "{:?}", f.short_bindings);
        let p = short(f, "p");
        // Declared 3, next read at `p.close()` (the closure, if-let, match arm and for rebind it),
        // then `p.trim()` on the shadowing let, which ends the range: `p.done()` is the new p.
        assert_eq!((p.decl_line, p.gap_to, p.last_line, p.uses), (3, 60, 61, 2), "{p:?}");
        assert_eq!((p.max_gap, p.gap_from, p.span), (57, 3, 58));
        let h = short(f, "h");
        assert_eq!((h.decl_line, h.uses), (2, 2));
        assert_eq!((h.max_gap, h.gap_from, h.gap_to), (38, 25, 63), "{h:?}");
        // The `for p` binding scopes to its one-line body.
        assert_eq!(f.short_bindings.iter().filter(|s| s.name == "p").map(|s| (s.decl_line, s.block_lines)).collect::<Vec<_>>(), vec![(3, 64), (49, 1), (61, 64)]);
        assert_eq!(f.long_short_bindings, 2);
        // The reason names the worst binding and lists both.
        let r = unit_reason(&f.name, f.start_line, f.end_line, &f.short_bindings, f.long_short_bindings, &cfg).unwrap();
        assert_eq!(r, "f (lines 1-64): this block is too long to carry a one-letter name: p (declared line 3, last used line 61, 2 uses, widest gap 57 lines between lines 3 and 60, in a 64-line block); 2 one-letter bindings have a use gap of 30+ lines: p, h");
        assert!(!r.contains("rename"));
    }

    #[test]
    fn rust_if_let_while_let_and_for_rebinding_the_name_still_read_it_in_the_value_and_else() {
        let cfg = Cfg::default();
        let src = format!(
            "fn f(s: &S) {{\n    let a = 1;\n    let b = 2;\n    let mut t = s.first();\n{}    if let Some(t) = t.next() {{ t.go(); }} else {{ t.reset(); }}\n{}    while let Some(t) = t.next() && t.ok() {{ t.go(); }}\n{}    for t in t.children() {{ t.go(); }}\n    t.done();\n}}\n",
            pad(40), pad(20), pad(20)
        );
        let f = &units("a.rs", Language::Rust, &src, &cfg)[0];
        let t = short(f, "t");
        // Line 45: the `if let` value and the `else` branch (2 uses; the consequence is the new
        // t's); line 66: the `while let` value (the chain's `t.ok()` is the new t's); line 87: the
        // `for` iterable; line 88.
        assert_eq!((t.decl_line, t.uses, t.last_line, t.max_gap, t.gap_from, t.gap_to), (4, 5, 88, 41, 4, 45), "{t:?}");
    }

    #[test]
    fn rust_accumulator_used_every_few_lines_scores_a_small_gap() {
        let cfg = Cfg::default();
        let body: String = (0..40).map(|i| format!("    h.push_str(\"{i}\");\n{}", pad(4))).collect();
        let src = format!("fn f() -> String {{\n    let mut h = String::new();\n    let a = 1;\n    let b = 2;\n{body}    h\n}}\n");
        let f = &units("a.rs", Language::Rust, &src, &cfg)[0];
        let h = short(f, "h");
        assert_eq!(h.max_gap, 5, "{h:?}");
        assert!(h.span > 190);
        assert_eq!(f.long_short_bindings, 0);
    }

    #[test]
    fn allow_list_underscore_and_length_gate_short_names_and_min_other_bindings_gates_the_unit() {
        let mut cfg = Cfg::default();
        let src = format!("fn f() {{\n    let i = 0;\n    let _t = 0;\n    let p = 0;\n    let ab = 0;\n{}    use_(i, _t, p, ab);\n}}\n", pad(40));
        let f = &units("a.rs", Language::Rust, &src, &cfg)[0];
        assert_eq!(f.bindings, 4);
        assert_eq!(f.short_bindings.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), vec!["p"]);
        assert_eq!(f.long_short_bindings, 1);
        cfg.short_name_max_len = 2;
        let f = &units("a.rs", Language::Rust, &src, &cfg)[0];
        assert_eq!(f.short_bindings.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), vec!["p", "ab"]);
        // Too few other bindings: the count is 0 while the raw bindings stay in the list.
        cfg.short_name_min_other_bindings = 5;
        let f = &units("a.rs", Language::Rust, &src, &cfg)[0];
        assert_eq!((f.long_short_bindings, f.short_bindings.len()), (0, 2));
        assert!(unit_reason(&f.name, f.start_line, f.end_line, &f.short_bindings, f.long_short_bindings, &cfg).is_none());
    }

    #[test]
    fn rust_for_binding_scopes_to_the_body_and_macro_tokens_are_uses() {
        let cfg = Cfg::default();
        let src = format!("fn f(s: &S) {{\n    let a = 1;\n    let b = 2;\n    for t in s.items() {{\n        write!(out, \"{{}}\", t).ok();\n{}        t.done();\n    }}\n    t_outer();\n}}\n", pad(40));
        let f = &units("a.rs", Language::Rust, &src, &cfg)[0];
        let t = short(f, "t");
        assert_eq!((t.decl_line, t.uses, t.last_line, t.max_gap), (4, 2, 46, 41), "{t:?}");
        assert_eq!(t.block_lines, 44);
    }

    #[test]
    fn pattern_bindings_count_only_with_the_knob() {
        let mut cfg = Cfg::default();
        let src = "fn f() {\n    let (a, b) = (1, 2);\n    let Some(c) = x else { return };\n    let S { d, e: f } = s;\n    let _ = a + b + c + d + f;\n}\n";
        let f = &units("a.rs", Language::Rust, src, &cfg)[0];
        assert_eq!(f.bindings, 0);
        cfg.short_name_include_pattern_bindings = true;
        let f = &units("a.rs", Language::Rust, src, &cfg)[0];
        assert_eq!(f.short_bindings.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), vec!["a", "b", "c", "d", "f"]);
        assert_eq!(f.bindings, 5);
    }

    #[test]
    fn nested_units_keep_their_own_bindings_and_test_regions_are_tagged() {
        let cfg = Cfg::default();
        let src = "fn outer() {\n    let a = 1;\n    fn inner() { let b = 2; let c = 3; b + c }\n    let d = |q| q + a;\n    d(a);\n}\n";
        let fs = units("a.rs", Language::Rust, src, &cfg);
        assert_eq!(fs.iter().map(|f| (f.name.as_str(), f.bindings)).collect::<Vec<_>>(), vec![("outer", 2), ("inner", 2)]);
    }

    #[test]
    fn python_first_assignment_binds_later_ones_rebind_and_lambda_comprehension_targets_are_skipped() {
        let cfg = Cfg::default();
        let src = format!(
            "def f(s):\n    p = s.p\n    q = 1\n    r = 2\n    g = lambda p: p + 1\n    h = [p for p in s if p]\n    s.call(p=1)\n    s.p.q\n{}    p.close()\n    p = p.next()\n{}    p.done()\n    for t in s.items():\n        t.go()\n    with open(p) as w:\n        w.read()\n",
            pad(30), pad(30)
        );
        let f = &units("a.py", Language::Python, &src, &cfg)[0];
        assert_eq!(f.bindings, 7, "{:?}", f.short_bindings); // p q r g h, t, w
        let p = short(f, "p");
        // Declared 2; the lambda / comprehension / keyword / attribute `p` are not uses; next
        // read at `p.close()` (line 39), then `p.next()` on the reassignment ends the range.
        assert_eq!((p.decl_line, p.uses, p.gap_to, p.last_line), (2, 2, 39, 40), "{p:?}");
        assert_eq!(p.max_gap, 37);
        let w = short(f, "w");
        assert_eq!((w.decl_line, w.uses, w.max_gap), (74, 1, 1));
    }

    #[test]
    fn python_attribute_and_subscript_targets_and_loop_iterables_read_the_name_and_a_nested_def_rebinds() {
        let cfg = Cfg::default();
        let src = format!(
            "def f(s, d):\n    a = 1\n    b = 2\n    p = s.first()\n{}    self.p = p\n    d[p] = 1\n    for p in p.items():\n        p.go()\n    with open(p) as p:\n        p.read()\n{}    p.done()\n    def p():\n        pass\n    p.after()\n",
            pad(40), pad(20)
        );
        let f = &units("a.py", Language::Python, &src, &cfg)[0];
        let p = short(f, "p");
        // `self.p = p` and `d[p] = 1` (45, 46) bind nothing and read p; the `for` iterable (47) and
        // the `with` value (49) read the outer p while their bodies are the new one's; `def p`
        // (72) rebinds it, so `p.after()` is not a use.
        assert_eq!((p.decl_line, p.uses, p.last_line, p.max_gap), (4, 5, 71, 41), "{p:?}");
    }

    #[test]
    fn ts_declarators_for_loops_and_arrow_params() {
        let cfg = Cfg::default();
        let src = format!(
            "function f(s: S) {{\n  let h = 0;\n  const a = 1, b = 2;\n  for (let p = 0; p < 3; p++) {{\n{}    h += p;\n  }}\n  for (const t of s.items) {{ t.go(); }}\n  s.items.map((h) => h + 1).filter(h => h.ok);\n  try {{ }} catch (h) {{ h.x; }}\n  const o = {{ h, p: h }};\n  let h = 3;\n  return h;\n}}\n",
            pad(35)
        );
        let f = &units("a.ts", Language::TypeScript, &src, &cfg)[0];
        assert_eq!(f.bindings, 7, "{:?}", f.short_bindings); // h a b p t o h
        let p = short(f, "p");
        assert_eq!((p.decl_line, p.uses, p.max_gap, p.block_lines), (4, 3, 36, 38), "{p:?}");
        let h = short(f, "h");
        // Uses: the loop body (40), the shorthand and the value in the object literal (45); the
        // arrow parameters (parenthesised and bare) and the catch parameter rebind; `let h = 3`
        // ends the range.
        assert_eq!((h.decl_line, h.uses, h.last_line, h.max_gap), (2, 3, 45, 38), "{h:?}");
        assert_eq!(f.short_bindings.iter().filter(|s| s.name == "h").map(|s| s.decl_line).collect::<Vec<_>>(), vec![2, 46]);
    }
}
