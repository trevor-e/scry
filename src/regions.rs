//! Inline test regions: the parts of a Source file that are test code.
//!
//! Rust keeps unit tests next to the code they test, so a `#[cfg(test)] mod tests`
//! inflates a file's size and can carry clones and complex functions that are not
//! production code. This module finds those regions on an already-parsed tree so
//! metrics and clones can tag or skip them without a second parse. Rust only.

use serde::Serialize;
use tree_sitter::Node;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegionKind {
    /// `#[cfg(test)] mod …` — the whole module body.
    CfgTestMod,
    /// `#[cfg(test)]` on any other item (fn, impl, use, macro).
    CfgTestItem,
    /// A bare `#[test] fn` outside any cfg(test) region.
    TestFn,
}

#[derive(Debug, Clone, Serialize)]
pub struct TestRegion {
    pub kind: RegionKind,
    #[serde(skip)]
    pub start_byte: usize,
    #[serde(skip)]
    pub end_byte: usize,
    pub start_line: usize,
    pub end_line: usize,
}

impl TestRegion {
    pub fn lines(&self) -> usize {
        self.end_line - self.start_line + 1
    }
}

/// Lines covered by `regions` (sorted by start): the union of their line spans, so two
/// regions sharing a line (`#[cfg(test)] use a; #[cfg(test)] use b;`) count it once.
pub fn inline_lines(regions: &[TestRegion]) -> usize {
    let mut covered = 0usize;
    let mut covered_to = 0usize;
    for r in regions {
        let s = r.start_line.max(covered_to + 1);
        if r.end_line >= s {
            covered += r.end_line - s + 1;
            covered_to = r.end_line;
        }
    }
    covered
}

/// Index of the region holding `byte`, if any, in the (sorted, merged) `regions`.
pub fn index_of(regions: &[TestRegion], byte: usize) -> Option<usize> {
    let i = regions.partition_point(|r| r.start_byte <= byte);
    (i > 0 && byte < regions[i - 1].end_byte).then(|| i - 1)
}

/// True when `byte` lies inside one of the (sorted, merged) `regions`.
pub fn contains(regions: &[TestRegion], byte: usize) -> bool {
    index_of(regions, byte).is_some()
}

/// Test regions of a parsed Rust tree, sorted by start and merged so a region
/// nested inside another is folded into the outer one. Empty for other grammars.
pub fn test_regions(root: Node, src: &[u8]) -> Vec<TestRegion> {
    let mut found: Vec<TestRegion> = Vec::new();
    // Explicit stack: never recurse on the AST.
    let mut stack: Vec<Node> = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "attribute_item"
            && let Some(kind) = attribute_kind(node, src)
            && let Some(item) = attributed_item(node)
        {
            let kind = match (kind, item.kind()) {
                (Attr::CfgTest, "mod_item") => RegionKind::CfgTestMod,
                (Attr::CfgTest, _) => RegionKind::CfgTestItem,
                (Attr::Test, "function_item") => RegionKind::TestFn,
                (Attr::Test, _) => continue,
            };
            found.push(TestRegion {
                kind,
                start_byte: item.start_byte(),
                end_byte: item.end_byte(),
                start_line: item.start_position().row + 1,
                end_line: item.end_position().row + 1,
            });
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    merge(found)
}

enum Attr {
    CfgTest,
    Test,
}

/// `#[cfg(test)]` or `#[test]`; anything else (including `#[cfg(not(test))]`) is None.
fn attribute_kind(attribute_item: Node, src: &[u8]) -> Option<Attr> {
    let attr = attribute_item.named_child(0).filter(|n| n.kind() == "attribute")?;
    let name = attr.named_child(0).filter(|n| n.kind() == "identifier")?;
    let name = name.utf8_text(src).ok()?;
    let args = attr.child_by_field_name("arguments");
    match (name, args) {
        ("test", None) => Some(Attr::Test),
        ("cfg", Some(tt)) if tt.kind() == "token_tree" => {
            let inner = tt.utf8_text(src).ok()?.trim().trim_start_matches('(').trim_end_matches(')').trim();
            (inner == "test").then_some(Attr::CfgTest)
        }
        _ => None,
    }
}

/// The item an outer attribute applies to: the next named sibling, looking past
/// further attributes and comments that sit between the attribute and its item.
fn attributed_item(attribute_item: Node) -> Option<Node> {
    let mut next = attribute_item.next_named_sibling();
    while let Some(n) = next {
        match n.kind() {
            "attribute_item" | "line_comment" | "block_comment" => next = n.next_named_sibling(),
            _ => return Some(n),
        }
    }
    None
}

fn merge(mut regions: Vec<TestRegion>) -> Vec<TestRegion> {
    regions.sort_by_key(|r| (r.start_byte, std::cmp::Reverse(r.end_byte)));
    let mut out: Vec<TestRegion> = Vec::with_capacity(regions.len());
    for r in regions {
        match out.last_mut() {
            Some(last) if r.start_byte < last.end_byte => {
                // Nested (or overlapping) in the previous region: fold it in, keep the outer kind.
                if r.end_byte > last.end_byte {
                    last.end_byte = r.end_byte;
                    last.end_line = r.end_line;
                }
            }
            _ => out.push(r),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::Language;

    fn regions(src: &str) -> Vec<TestRegion> {
        let tree = Language::Rust.parser().parse(src, None).unwrap();
        test_regions(tree.root_node(), src.as_bytes())
    }

    #[test]
    fn cfg_test_mod_is_one_region_and_nested_tests_merge_into_it() {
        let src = "\
fn a() {}
#[cfg(test)]
#[allow(dead_code)]
mod tests {
    use super::*;
    #[test]
    fn t1() { if a() {} }
    #[cfg(test)]
    mod inner { #[test] fn t2() {} }
}
fn b() {}
";
        let rs = regions(src);
        assert_eq!(rs.len(), 1, "{rs:?}");
        assert_eq!(rs[0].kind, RegionKind::CfgTestMod);
        assert_eq!((rs[0].start_line, rs[0].end_line), (4, 10));
        assert_eq!(inline_lines(&rs), 7);
        assert!(contains(&rs, src.find("fn t1").unwrap()));
        assert!(!contains(&rs, src.find("fn b").unwrap()));
        assert!(!contains(&rs, src.find("fn a").unwrap()));
    }

    #[test]
    fn cfg_test_items_and_bare_test_fns_at_any_depth() {
        let src = "\
#[cfg(test)]
pub(crate) fn helper() {}
impl S {
    #[cfg(test)]
    pub(crate) fn scripted(&self) {}
    fn real(&self) {}
}
#[test]
fn bare() {}
#[cfg(not(test))]
fn not_test() {}
#[cfg(test)]
use std::io;
#[test]
mod odd {}
";
        let rs = regions(src);
        let kinds: Vec<(RegionKind, usize)> = rs.iter().map(|r| (r.kind, r.start_line)).collect();
        assert_eq!(
            kinds,
            vec![(RegionKind::CfgTestItem, 2), (RegionKind::CfgTestItem, 5), (RegionKind::TestFn, 9), (RegionKind::CfgTestItem, 13)],
            "{rs:?}"
        );
        assert!(!contains(&rs, src.find("fn real").unwrap()));
        assert!(!contains(&rs, src.find("fn not_test").unwrap()));
    }

    #[test]
    fn two_regions_on_one_line_count_it_once() {
        let src = "#[cfg(test)] use a; #[cfg(test)] use b;\nfn f() {}\n#[cfg(test)]\nfn t() {\n}\n";
        let rs = regions(src);
        assert_eq!(rs.iter().map(|r| (r.start_line, r.end_line)).collect::<Vec<_>>(), vec![(1, 1), (1, 1), (4, 5)], "{rs:?}");
        assert_eq!(inline_lines(&rs), 3);
        assert_eq!(index_of(&rs, src.find("use b").unwrap()), Some(1));
        assert_eq!(index_of(&rs, src.find("fn t").unwrap()), Some(2));
        assert_eq!(index_of(&rs, src.find("fn f").unwrap()), None);
    }

    #[test]
    fn other_grammars_get_no_regions() {
        let src = "def test_x():\n    pass\n";
        let tree = Language::Python.parser().parse(src, None).unwrap();
        assert!(test_regions(tree.root_node(), src.as_bytes()).is_empty());
    }
}
