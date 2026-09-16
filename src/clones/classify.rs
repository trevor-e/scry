use super::{CloneKind, Tokens};
use crate::{discover::SourceFile, metrics};
use tree_sitter::Node;

pub(super) fn pair_kind(a: &[CloneKind], b: &[CloneKind], table: Option<&str>, share: f64) -> CloneKind {
    for kind in [CloneKind::Signature, CloneKind::Configuration] {
        if kind == CloneKind::Configuration && table.is_some_and(|k| !matches!(k, "argument_list" | "arguments")) {
            return CloneKind::Table;
        }
        let dominates = |side: &[CloneKind]| side.iter().filter(|k| **k == kind).count() as f64 >= share * side.len() as f64;
        if dominates(a) && dominates(b) {
            return kind;
        }
    }
    if table.is_some() { CloneKind::Table } else { CloneKind::Logic }
}

pub(super) fn contexts(t: &Tokens, file: &SourceFile) -> Vec<CloneKind> {
    let mut out = vec![CloneKind::Logic; t.hashes.len()];
    let Some(tree) = &t.tree else { return out };
    let units = metrics::unit_nodes(tree.root_node(), file.lang);
    for unit in &units {
        let end = unit.child_by_field_name("body").map_or(unit.end_byte(), |body| body.start_byte());
        mark(&mut out, t, unit.start_byte(), end, CloneKind::Signature);
    }
    let unit_ids: std::collections::HashSet<_> = units.iter().map(|u| u.id()).collect();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if unit_ids.contains(&n.id()) {
            continue;
        }
        if matches!(n.kind(), "function_signature_item" | "method_signature" | "function_signature" | "abstract_method_signature") {
            mark(&mut out, t, n.start_byte(), n.end_byte(), CloneKind::Signature);
            continue;
        }
        if matches!(n.kind(), "assignment" | "variable_declarator" | "const_item" | "static_item") {
            let value = n.child_by_field_name("right").or_else(|| n.child_by_field_name("value"));
            if value.is_some_and(|v| declarative(v, file.content.as_bytes())) {
                mark(&mut out, t, n.start_byte(), n.end_byte(), CloneKind::Configuration);
                continue;
            }
        }
        let mut cursor = n.walk();
        stack.extend(n.named_children(&mut cursor));
    }
    out
}

fn mark(out: &mut [CloneKind], t: &Tokens, start: usize, end: usize, kind: CloneKind) {
    let lo = t.starts.partition_point(|s| *s < start);
    let hi = t.ends.partition_point(|e| *e <= end);
    if lo < hi {
        out[lo..hi].fill(kind);
    }
}

fn declarative(node: Node<'_>, src: &[u8]) -> bool {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        let k = n.kind();
        if k.contains("comprehension")
            || matches!(
                k,
                "lambda"
                    | "arrow_function"
                    | "function_expression"
                    | "await"
                    | "await_expression"
                    | "if_expression"
                    | "conditional_expression"
                    | "block"
            )
        {
            return false;
        }
        if matches!(k, "call" | "call_expression") {
            let Some(fun) = n.child_by_field_name("function") else { return false };
            let name = fun.utf8_text(src).unwrap_or("").rsplit(['.', ':']).next().unwrap_or("");
            // Constructor-shaped calls are a heuristic for declarative configuration.
            if !name.starts_with(char::is_uppercase) {
                return false;
            }
        }
        let mut cursor = n.walk();
        stack.extend(n.named_children(&mut cursor));
    }
    true
}
