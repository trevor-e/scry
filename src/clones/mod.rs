//! Near-exact clone detection.
//!
//! Tokens come from tree-sitter leaves with identifiers, strings and numbers
//! collapsed to classes, so renamed copies still match. Every k-token window is
//! hashed; winnowing keeps one fingerprint per w-window, which guarantees any
//! shared run of at least k+w-1 tokens is found while indexing a fraction of
//! the hashes. Matching fingerprints on the same diagonal are merged into runs.
//!
//! Two AST-aware steps follow: a same-file run whose two ranges sit in one
//! container (the first half of a `match` matching its second half) is a
//! repetition, not a copy, and is dropped; a surviving run whose sides are both
//! runs of uniform sibling entries (a dispatch match, a registry array, a map
//! literal) is tagged `table` so it is listed apart from duplicated logic.
//! Every surviving run then resolves to a symbol: the innermost metrics unit
//! holding its start line, else the nearest preceding top-level item, else `?`.

use crate::config::{Clones as Cfg, SymbolFallback, Tests as TestsCfg};
use crate::discover::SourceFile;
use crate::lang::Language;
use crate::{metrics, regions};
use rayon::prelude::*;
use serde::Serialize;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use tree_sitter::Node;

#[derive(Debug, Clone, Serialize)]
pub struct Loc {
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,
    /// The innermost metrics unit whose line span holds `start_line` (`park`, `Store<'c>.transact`);
    /// with `[plan].symbol_fallback = preceding_item`, else the nearest preceding top-level named
    /// item (`CHECKS`, `ApproveReport`, `_baseMimes`); else `?`.
    pub symbol: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CloneKind {
    Logic,
    Table,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClonePair {
    pub a: Loc,
    pub b: Loc,
    pub tokens: usize,
    /// `table` when both sides lie in a run of uniform sibling entries; `logic` otherwise.
    pub kind: CloneKind,
    /// Side A's container node kind (`match_block`, `array_expression`, `object`) when `kind` is table.
    pub container_kind: Option<String>,
    /// Consecutive same-kind entries side A covers when `kind` is table.
    pub entry_count: Option<usize>,
}

/// One table a file's clone pairs touch: the union of the table-pair ranges under one symbol.
#[derive(Debug, Clone, Serialize)]
pub struct TableRef {
    pub symbol: Option<String>,
    pub container_kind: String,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct FileClones {
    /// Lines covered by at least one clone range (union) = `logic_clone_lines + table_clone_lines`.
    pub clone_lines: usize,
    /// Lines covered by at least one `logic` pair.
    pub logic_clone_lines: usize,
    /// Lines covered only by `table` pairs.
    pub table_clone_lines: usize,
    /// `(logic + table_weight x table) / lines`, against the lines outside inline test regions.
    pub clone_ratio: f64,
    pub pairs: usize,
    pub tables: Vec<TableRef>,
}

#[derive(Debug, Default, Serialize)]
pub struct CloneReport {
    pub pairs: Vec<ClonePair>,
    pub files: HashMap<String, FileClones>,
}

struct Tokens {
    hashes: Vec<u64>,
    lines: Vec<usize>,
    /// Byte span of each token, so a token range maps back onto the tree.
    starts: Vec<usize>,
    ends: Vec<usize>,
    /// Lines inside inline test regions, which emitted no tokens.
    inline_test_lines: usize,
    /// The regions that emitted no tokens (empty when `[tests].inline_modules` is off).
    test_regions: Vec<regions::TestRegion>,
    /// The parsed tree, kept for the container walks (`Tree` is `Send + Sync`).
    tree: Option<tree_sitter::Tree>,
    /// Kind classes of the file's grammar, shared by every file of that grammar.
    kinds: Arc<Kinds>,
}

pub fn detect(files: &[SourceFile], cfg: &Cfg, tests: &TestsCfg, fallback: SymbolFallback) -> CloneReport {
    let k = cfg.k;
    // Kind classes once per grammar, not per file: a grammar has a thousand kinds to classify.
    let mut kinds: HashMap<Language, Arc<Kinds>> = HashMap::new();
    for f in files {
        kinds.entry(f.lang).or_insert_with(|| Arc::new(Kinds::new(f.lang, control_kinds(cfg, f.lang))));
    }
    let toks: Vec<Tokens> = files.par_iter().map(|f| tokenize(f, tests, Arc::clone(&kinds[&f.lang]))).collect();

    // fingerprint hash -> (file, token position)
    let mut index: HashMap<u64, Vec<(usize, usize)>> = HashMap::new();
    for (fi, t) in toks.iter().enumerate() {
        for (pos, h) in winnow(&t.hashes, cfg.k, cfg.w) {
            index.entry(h).or_default().push((fi, pos));
        }
    }

    // Candidate matches grouped by unordered file pair.
    let mut by_pair: HashMap<(usize, usize), Vec<(usize, usize)>> = HashMap::new();
    for locs in index.values() {
        if locs.len() < 2 || locs.len() > cfg.max_locations {
            continue;
        }
        let mut files_seen: Vec<usize> = locs.iter().map(|l| l.0).collect();
        files_seen.sort_unstable();
        files_seen.dedup();
        if files_seen.len() > cfg.max_files {
            continue;
        }
        for i in 0..locs.len() {
            for j in i + 1..locs.len() {
                let (mut x, mut y) = (locs[i], locs[j]);
                if x.0 > y.0 || (x.0 == y.0 && x.1 > y.1) {
                    std::mem::swap(&mut x, &mut y);
                }
                if x.0 == y.0 && y.1 - x.1 < k {
                    continue; // overlapping with itself
                }
                by_pair.entry((x.0, y.0)).or_default().push((x.1, y.1));
            }
        }
    }

    // Symbol names once per file that has a candidate match: units and top-level items by line.
    let mut used = vec![false; files.len()];
    for (fa, fb) in by_pair.keys() {
        used[*fa] = true;
        used[*fb] = true;
    }
    let symbols: Vec<Option<SymbolIndex>> =
        toks.par_iter().zip(files.par_iter()).zip(used.par_iter()).map(|((t, f), u)| if *u { SymbolIndex::new(t, f) } else { None }).collect();

    let mut pairs: Vec<ClonePair> = by_pair
        .into_par_iter()
        .flat_map_iter(|((fa, fb), mut matches)| {
            // Same diagonal = same offset between the two positions.
            matches.sort_by_key(|(a, b)| (*a as i64 - *b as i64, *a));
            let mut runs: Vec<(usize, usize, usize)> = Vec::new(); // (startA, startB, len)
            let mut cur: Option<(usize, usize, usize)> = None;
            for (a, b) in matches {
                match cur {
                    Some((sa, sb, len)) if a as i64 - b as i64 == sa as i64 - sb as i64 && a <= sa + len => {
                        cur = Some((sa, sb, (a + k) - sa));
                    }
                    _ => {
                        if let Some(r) = cur.take() {
                            runs.push(r);
                        }
                        cur = Some((a, b, k));
                    }
                }
            }
            if let Some(r) = cur {
                runs.push(r);
            }
            let ta = &toks[fa];
            let tb = &toks[fb];
            let root_a = root_children(ta);
            let root_b_own;
            let root_b = if fa == fb {
                &root_a
            } else {
                root_b_own = root_children(tb);
                &root_b_own
            };
            // Fingerprints sample the k-grams, so a run's ends are up to W tokens
            // short on each side. Extend while the underlying tokens still match.
            for r in runs.iter_mut() {
                let (mut sa, mut sb, mut len) = *r;
                while sa > 0 && sb > 0 && ta.hashes[sa - 1] == tb.hashes[sb - 1] {
                    sa -= 1;
                    sb -= 1;
                    len += 1;
                }
                while sa + len < ta.hashes.len() && sb + len < tb.hashes.len() && ta.hashes[sa + len] == tb.hashes[sb + len] {
                    len += 1;
                }
                *r = (sa, sb, len);
            }
            // Repetitive code matches itself on every shifted diagonal. Keep the
            // longest run and drop any run whose A-range *and* B-range both
            // overlap an accepted run: that is the same clone seen at an offset.
            runs.retain(|(_, _, len)| *len >= cfg.min_tokens);
            // Within one file, ranges that overlap each other are a repeating
            // pattern (a table, an unrolled loop), not a copy.
            if fa == fb {
                runs.retain(|(sa, sb, len)| !overlaps(*sa, *len, *sb, *len));
            }
            // Longest first: a run whose A-range and B-range both overlap an
            // accepted run is the same clone seen at an offset and is dropped.
            // So are two ranges of one uniform container: a table longer than
            // 2 x min_tokens matches its own second half on a clean diagonal.
            // Such a run is dropped as if never found, so it neither prints nor
            // hides the shorter runs under it; the AST test only runs on a run
            // that is about to be kept, since a suppressed run suppresses nothing.
            runs.sort_by_key(|(_, _, len)| std::cmp::Reverse(*len));
            let self_match = fa == fb && cfg.drop_same_container_self_match;
            let mut kept: Vec<(usize, usize, usize)> = Vec::new();
            for r in runs {
                let dup = kept.iter().any(|k| overlaps(r.0, r.2, k.0, k.2) && overlaps(r.1, r.2, k.1, k.2));
                if !(dup || self_match && table_self_match(ta, &root_a, r.0, r.1, r.2, cfg)) {
                    kept.push(r);
                }
            }
            kept.into_iter()
                .map(|(sa, sb, len)| {
                    let side_a = locate(ta, &root_a, sa, len, cfg);
                    let side_b = locate(tb, root_b, sb, len, cfg);
                    // A pair is a table only when both sides are.
                    let table = match (&side_a, &side_b) {
                        (Some(t), Some(_)) => Some(t.clone()),
                        _ => None,
                    };
                    ClonePair {
                        a: loc(&files[fa].path, ta, sa, len, symbols[fa].as_ref(), fallback),
                        b: loc(&files[fb].path, tb, sb, len, symbols[fb].as_ref(), fallback),
                        tokens: len,
                        kind: if table.is_some() { CloneKind::Table } else { CloneKind::Logic },
                        container_kind: table.as_ref().map(|t| t.container_kind.clone()),
                        entry_count: table.as_ref().map(|t| t.entries),
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect();
    // Full key: HashMap and rayon order vary between runs, and `--top` must not.
    pairs.sort_by(|p, q| {
        q.tokens
            .cmp(&p.tokens)
            .then_with(|| p.a.file.cmp(&q.a.file))
            .then_with(|| p.a.start_line.cmp(&q.a.start_line))
            .then_with(|| p.b.file.cmp(&q.b.file))
            .then_with(|| p.b.start_line.cmp(&q.b.start_line))
    });

    // Per-file union of cloned lines, split by pair kind, plus the tables touched.
    let mut ranges: HashMap<&str, Vec<(usize, usize, CloneKind)>> = HashMap::new();
    let mut tables: HashMap<&str, Vec<TableRef>> = HashMap::new();
    for p in &pairs {
        for side in [&p.a, &p.b] {
            ranges.entry(&side.file).or_default().push((side.start_line, side.end_line, p.kind));
            if let Some(kind) = &p.container_kind {
                let refs = tables.entry(&side.file).or_default();
                let symbol = (side.symbol != "?").then(|| side.symbol.clone());
                match refs.iter_mut().find(|t| t.symbol == symbol && t.container_kind == *kind) {
                    Some(t) => {
                        t.start_line = t.start_line.min(side.start_line);
                        t.end_line = t.end_line.max(side.end_line);
                    }
                    None => refs.push(TableRef { symbol, container_kind: kind.clone(), start_line: side.start_line, end_line: side.end_line }),
                }
            }
        }
    }
    let mut per_file = HashMap::new();
    for (fi, f) in files.iter().enumerate() {
        let Some(rs) = ranges.get(f.path.as_str()) else { continue };
        let clone_lines = covered_lines(rs.iter().map(|r| (r.0, r.1)));
        let logic = covered_lines(rs.iter().filter(|r| r.2 == CloneKind::Logic).map(|r| (r.0, r.1)));
        let table = clone_lines - logic;
        let mut refs = tables.remove(f.path.as_str()).unwrap_or_default();
        refs.sort_by_key(|t| (t.start_line, t.end_line));
        per_file.insert(
            f.path.clone(),
            FileClones {
                clone_lines,
                logic_clone_lines: logic,
                table_clone_lines: table,
                // Against the lines that could hold a clone: test regions emitted no tokens.
                clone_ratio: match f.lines.saturating_sub(toks[fi].inline_test_lines) {
                    0 => 0.0,
                    lines => (logic as f64 + cfg.table_weight * table as f64) / lines as f64,
                },
                pairs: rs.len(),
                tables: refs,
            },
        );
    }
    CloneReport { pairs, files: per_file }
}

/// Size of the union of inclusive line ranges.
fn covered_lines(ranges: impl Iterator<Item = (usize, usize)>) -> usize {
    let mut rs: Vec<(usize, usize)> = ranges.collect();
    rs.sort_unstable();
    let mut covered = 0usize;
    let mut end = 0usize;
    for (s, e) in rs {
        let s = s.max(end + 1);
        if e >= s {
            covered += e - s + 1;
            end = e;
        }
    }
    covered
}

fn overlaps(s1: usize, l1: usize, s2: usize, l2: usize) -> bool {
    s1 < s2 + l2 && s2 < s1 + l1
}

fn loc(path: &str, t: &Tokens, start: usize, len: usize, symbols: Option<&SymbolIndex>, fallback: SymbolFallback) -> Loc {
    let end = (start + len - 1).min(t.lines.len().saturating_sub(1));
    let start_line = t.lines[start];
    let symbol = symbols.map_or_else(|| "?".to_string(), |s| s.resolve(start_line, fallback));
    Loc { file: path.to_string(), start_line, end_line: t.lines[end], symbol }
}

// ---------- symbol resolution ----------

/// What names a run of one file: its metrics units by line span and its top-level named items by
/// start line, both read off the tokenizer's tree, so a run's symbol is the name `scry metrics`
/// reports for the unit around it.
struct SymbolIndex {
    /// `(start_line, end_line, name)` in document order.
    units: Vec<(usize, usize, String)>,
    /// `(start_line, name)` of the root's named items, in document order. An item starts on
    /// its first outer attribute (`#[derive(Debug)]` is a sibling node in the Rust grammar),
    /// and items inside a region the tokenizer skipped are left out: a run can only start on
    /// source the tokens came from, and the `#[test] fn` above a struct is not its name.
    items: Vec<(usize, String)>,
}

impl SymbolIndex {
    fn new(t: &Tokens, file: &SourceFile) -> Option<Self> {
        let root = t.tree.as_ref()?.root_node();
        let src = file.content.as_bytes();
        let units = metrics::unit_nodes(root, file.lang)
            .into_iter()
            .map(|n| (n.start_position().row + 1, n.end_position().row + 1, metrics::unit_name_of(n, file.lang, src)))
            .collect();
        let mut items = Vec::new();
        let mut attrs_from: Option<usize> = None;
        let mut cursor = root.walk();
        for n in root.named_children(&mut cursor) {
            if n.kind() == "attribute_item" {
                attrs_from.get_or_insert(n.start_position().row + 1);
                continue;
            }
            if t.kinds.transparent(n) {
                continue;
            }
            let from = attrs_from.take().unwrap_or(n.start_position().row + 1);
            if regions::contains(&t.test_regions, n.start_byte()) {
                continue;
            }
            if let Some(name) = item_name(n, src) {
                items.push((from, name));
            }
        }
        Some(SymbolIndex { units, items })
    }

    /// The innermost unit whose span holds `line` (the shortest; the later one on a tie), else
    /// the last item starting at or before `line` when the fallback asks for it, else `?`.
    fn resolve(&self, line: usize, fallback: SymbolFallback) -> String {
        let unit = self
            .units
            .iter()
            .filter(|(s, e, _)| *s <= line && line <= *e)
            .min_by_key(|(s, e, _)| (e - s, std::cmp::Reverse(*s)));
        if let Some((_, _, name)) = unit {
            return name.clone();
        }
        match fallback {
            SymbolFallback::PrecedingItem => self.items.iter().rev().find(|(s, _)| *s <= line).map_or_else(|| "?".to_string(), |(_, n)| n.clone()),
            SymbolFallback::None => "?".to_string(),
        }
    }
}

/// The name of a top-level item a run can fall back to: Rust `function_item | impl_item |
/// struct_item | enum_item | static_item | const_item`, TS `function_declaration |
/// class_declaration | lexical_declaration`, Python `function_definition | class_definition`
/// and an `expression_statement` holding an assignment; looked for through a TS
/// `export_statement` and a Python `decorated_definition`. Anything else names nothing.
fn item_name(node: Node, src: &[u8]) -> Option<String> {
    let node = match node.kind() {
        "export_statement" => node.child_by_field_name("declaration")?,
        "decorated_definition" => node.child_by_field_name("definition")?,
        _ => node,
    };
    let name = match node.kind() {
        "function_item" | "struct_item" | "enum_item" | "static_item" | "const_item" | "function_declaration" | "class_declaration"
        | "function_definition" | "class_definition" => node.child_by_field_name("name")?,
        "impl_item" => node.child_by_field_name("type")?,
        "lexical_declaration" => node.named_child(0).filter(|d| d.kind() == "variable_declarator")?.child_by_field_name("name")?,
        "expression_statement" => node.named_child(0).filter(|a| a.kind() == "assignment")?.child_by_field_name("left")?,
        _ => return None,
    };
    name.utf8_text(src).ok().map(str::to_string)
}

/// Byte span of a token range: start of its first token to the end of its last.
fn byte_range(t: &Tokens, start: usize, len: usize) -> (usize, usize) {
    let end = (start + len - 1).min(t.ends.len().saturating_sub(1));
    (t.starts[start], t.ends[end])
}

/// Node-kind classes of one grammar indexed by kind id, built once per grammar, so the entry
/// walks compare integers: `Node::kind` validates a C string on every call.
#[derive(Debug)]
struct Kinds {
    /// Skeleton leaf class (`ID`, `STR`, `NUM`) for names, paths and literals; None keeps structure.
    leaf: Vec<Option<&'static str>>,
    /// Comments, and Rust outer attributes, which sit between entries without being entries.
    transparent: Vec<bool>,
    /// `[clones].table_control_kinds` for the grammar.
    control: Vec<bool>,
}

impl Kinds {
    fn new(lang: Language, control: &[String]) -> Self {
        let g = lang.grammar();
        let n = g.node_kind_count();
        let mut k = Kinds { leaf: Vec::with_capacity(n), transparent: Vec::with_capacity(n), control: Vec::with_capacity(n) };
        for id in 0..n {
            let kind = g.node_kind_for_id(id as u16).unwrap_or("");
            k.leaf.push(skeleton_leaf(kind));
            k.transparent.push(TRANSPARENT_KINDS.contains(&kind));
            k.control.push(control.iter().any(|c| c == kind));
        }
        k
    }

    fn transparent(&self, n: Node) -> bool {
        self.transparent.get(n.kind_id() as usize).copied().unwrap_or(false)
    }
}

/// Node kinds hashed as one leaf token when comparing entry shapes: names, paths and literals
/// vary between entries of one table; structure does not.
fn skeleton_leaf(kind: &str) -> Option<&'static str> {
    if STRING_KINDS.contains(&kind) || matches!(kind, "string_content" | "string_fragment") {
        Some("STR")
    } else if NUMBER_KINDS.contains(&kind) || kind == "boolean_literal" {
        Some("NUM")
    } else if kind.contains("identifier")
        || matches!(kind, "self" | "attribute" | "member_expression" | "field_expression" | "dotted_name" | "generic_type" | "primitive_type")
    {
        Some("ID")
    } else {
        None
    }
}

/// Nodes that sit between table entries without being entries: comments, and Rust outer
/// attributes, which the grammar keeps as siblings of the item they decorate.
const TRANSPARENT_KINDS: &[&str] = &["comment", "line_comment", "block_comment", "attribute_item"];

/// The `[clones].table_control_kinds` key for a grammar.
fn control_key(lang: Language) -> &'static str {
    match lang {
        Language::Rust => "rust",
        Language::Python => "python",
        Language::TypeScript | Language::Tsx | Language::JavaScript => "typescript",
    }
}

fn control_kinds(cfg: &Cfg, lang: Language) -> &[String] {
    cfg.table_control_kinds.get(control_key(lang)).map_or(&[], Vec::as_slice)
}

/// The root's named children, once per file: tree-sitter reaches a child by a linear walk, and
/// a file root can have thousands, so ranges find their top-level item by binary search.
fn root_children(t: &Tokens) -> Vec<Node<'_>> {
    let Some(tree) = &t.tree else { return Vec::new() };
    let root = tree.root_node();
    let mut cursor = root.walk();
    root.named_children(&mut cursor).collect()
}

/// One node on a range's chain with its named, non-transparent children under the range.
struct Level<'a> {
    node: Node<'a>,
    kids: Vec<Node<'a>>,
}

fn overlap(c: Node, sb: usize, eb: usize) -> usize {
    c.end_byte().min(eb).saturating_sub(c.start_byte().max(sb))
}

/// Root-to-node chain of the node a byte range belongs to: down from the root through every
/// named child spanning the range, then on into any child spanning at least
/// `dominant_child_share` of its bytes. Run extension bleeds a few tokens past an item's
/// boundary (its neighbour's closing brace); the bleed must not lift a range out of its own
/// item, while the halves of one container have no dominant child. One cursor descent that
/// also records each level's children under the range, so nothing walks back up with
/// `Node::parent` (a descent from the root on every call) or rescans a level.
fn resolve<'a>(t: &'a Tokens, root_kids: &[Node<'a>], sb: usize, eb: usize, cfg: &Cfg) -> Option<Vec<Level<'a>>> {
    let root = t.tree.as_ref()?.root_node();
    let need = cfg.dominant_child_share * (eb - sb) as f64;
    let mut chain: Vec<Level> = Vec::new();
    let mut kids = Vec::new();
    let mut next = None;
    for c in &root_kids[root_kids.partition_point(|c| c.end_byte() <= sb)..] {
        if c.start_byte() >= eb {
            break;
        }
        if !t.kinds.transparent(*c) {
            kids.push(*c);
        }
        if next.is_none() && overlap(*c, sb, eb) as f64 >= need {
            next = Some(*c);
        }
    }
    chain.push(Level { node: root, kids });
    while let Some(node) = next {
        next = None;
        let mut kids = Vec::new();
        let mut cursor = node.walk();
        if cursor.goto_first_child_for_byte(sb).is_some() {
            loop {
                let c = cursor.node();
                if c.start_byte() >= eb {
                    break;
                }
                if c.is_named() && sb < c.end_byte() {
                    if !t.kinds.transparent(c) {
                        kids.push(c);
                    }
                    if next.is_none() && overlap(c, sb, eb) as f64 >= need {
                        next = Some(c);
                    }
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        chain.push(Level { node, kids });
    }
    Some(chain)
}

/// Both ranges resolve to one container node (or one range's node sits inside the other's)
/// and that container's entries across the two ranges are a uniform table: the first half of
/// a `match`, registry array or map literal matching its second half. Two sibling items of one
/// container that merely copy each other are not uniform and stay clones.
fn table_self_match(t: &Tokens, root_kids: &[Node], sa: usize, sb: usize, len: usize, cfg: &Cfg) -> bool {
    let (a0, a1) = byte_range(t, sa, len);
    let (b0, b1) = byte_range(t, sb, len);
    let (u0, u1) = (a0.min(b0), a1.max(b1));
    let Some(chain) = resolve(t, root_kids, u0, u1, cfg) else { return false };
    // The container: the innermost chain node spanning both ranges. A range resolves to it
    // unless one of its children dominates the range; the rule needs at least one to.
    let Some(lca) = chain.iter().rposition(|l| l.node.start_byte() <= u0 && l.node.end_byte() >= u1) else { return false };
    let dominated = |s: usize, e: usize| chain[lca].kids.iter().any(|c| overlap(*c, s, e) as f64 >= cfg.dominant_child_share * (e - s) as f64);
    if dominated(a0, a1) && dominated(b0, b1) {
        return false;
    }
    find_table(&chain[..=lca], u0, u1, &t.kinds, cfg).is_some()
}

// ---------- table tagging ----------

#[derive(Debug, Clone)]
struct TableSide {
    container_kind: String,
    entries: usize,
}

/// The uniform-sibling container around a run, if any.
fn locate(t: &Tokens, root_kids: &[Node], start: usize, len: usize, cfg: &Cfg) -> Option<TableSide> {
    let (sb, eb) = byte_range(t, start, len);
    let chain = resolve(t, root_kids, sb, eb, cfg)?;
    find_table(&chain, sb, eb, &t.kinds, cfg)
}

/// Up the chain from its innermost node, the first one with at least `table_min_entries`
/// consecutive same-kind named children spanning at least `dominant_child_share` of the range
/// `sb..eb` decides: a table when those entries are uniform, otherwise nothing. Entries that
/// merely sit under the range (six imports at the top of a copied file) are not the run, and
/// the level above is asked instead.
fn find_table(chain: &[Level], sb: usize, eb: usize, kinds: &Kinds, cfg: &Cfg) -> Option<TableSide> {
    for (i, l) in chain.iter().enumerate().rev() {
        if let Some(entries) = table_entries(chain, i, cfg.table_min_entries) {
            // The comments, attributes and punctuation next to the entries belong to them: a
            // run that ends on the `#[serde(default)]` of the entry after the last one covered
            // is still the table.
            let (mut first, mut last) = (entries[0].start_byte(), entries[entries.len() - 1].end_byte());
            let mut next = entries[entries.len() - 1].next_sibling();
            while let Some(n) = next.filter(|n| !n.is_named() || kinds.transparent(*n)) {
                last = n.end_byte();
                next = n.next_sibling();
            }
            let mut prev = entries[0].prev_sibling();
            while let Some(n) = prev.filter(|n| !n.is_named() || kinds.transparent(*n)) {
                first = n.start_byte();
                prev = n.prev_sibling();
            }
            let covered = last.min(eb).saturating_sub(first.max(sb));
            if (covered as f64) < cfg.dominant_child_share * (eb - sb) as f64 {
                continue;
            }
            return uniform(&entries, kinds, cfg).then(|| TableSide { container_kind: l.node.kind().to_string(), entries: entries.len() });
        }
    }
    None
}

/// Entry kind fixed by the container: arms under a match block, cases under a switch body or
/// a `match` statement's block. Elsewhere the most common child kind is the entry.
fn fixed_entry_kind(chain: &[Level], i: usize) -> Option<&'static str> {
    match chain[i].node.kind() {
        "match_block" => Some("match_arm"),
        "switch_body" => Some("switch_case"),
        "block" if i > 0 && chain[i - 1].node.kind() == "match_statement" => Some("case_clause"),
        _ => None,
    }
}

/// The longest stretch of consecutive same-kind named children of chain level `i` under the
/// range, when it has at least `min` of them.
fn table_entries<'a>(chain: &[Level<'a>], i: usize, min: usize) -> Option<Vec<Node<'a>>> {
    let kids = &chain[i].kids;
    if kids.len() < min.max(1) {
        return None;
    }
    let fixed = fixed_entry_kind(chain, i).and_then(|name| kids.iter().find(|c| c.kind() == name).map(|c| c.kind_id()));
    let entry_kind = match fixed {
        Some(id) => id,
        None => {
            let mut counts: Vec<(u16, usize)> = Vec::new();
            for c in kids {
                match counts.iter_mut().find(|(k, _)| *k == c.kind_id()) {
                    Some(e) => e.1 += 1,
                    None => counts.push((c.kind_id(), 1)),
                }
            }
            counts.iter().max_by_key(|(_, n)| *n)?.0
        }
    };
    let mut best: Vec<Node> = Vec::new();
    let mut run: Vec<Node> = Vec::new();
    for &c in kids {
        if c.kind_id() == entry_kind {
            run.push(c);
        } else {
            if run.len() > best.len() {
                best = std::mem::take(&mut run);
            }
            run.clear();
        }
    }
    if run.len() > best.len() {
        best = run;
    }
    (best.len() >= min.max(1)).then_some(best)
}

struct Shape {
    hash: u64,
    named_nodes: usize,
    has_control: bool,
}

/// Skeleton hash of an entry (named nodes only, leaves collapsed), its named-node count and
/// whether a control-flow node sits inside it. Stops as soon as the count passes `max_nodes`:
/// an entry that large is not a table row, and walking the rest would cost the pass its speed.
/// One cursor, pre-order; anonymous nodes and their subtrees are skipped.
fn shape(entry: Node, kinds: &Kinds, max_nodes: usize) -> Shape {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut named_nodes = 0usize;
    let mut has_control = false;
    let mut cursor = entry.walk();
    let mut depth = 0u32;
    loop {
        let n = cursor.node();
        let id = n.kind_id() as usize;
        let mut descend = false;
        if n.is_named() && !(kinds.transparent(n) && depth > 0) {
            named_nodes += 1;
            if named_nodes > max_nodes {
                break;
            }
            if kinds.control.get(id).copied().unwrap_or(false) {
                has_control = true;
            }
            match kinds.leaf.get(id).copied().flatten() {
                Some(leaf) => (depth, leaf).hash(&mut h),
                None => {
                    (depth, id).hash(&mut h);
                    descend = true;
                }
            }
        }
        if descend && cursor.goto_first_child() {
            depth += 1;
            continue;
        }
        loop {
            if depth == 0 {
                return Shape { hash: h.finish(), named_nodes, has_control };
            }
            if cursor.goto_next_sibling() {
                break;
            }
            cursor.goto_parent();
            depth -= 1;
        }
    }
    Shape { hash: h.finish(), named_nodes, has_control }
}

/// Table iff the dominant skeleton covers >= `table_min_dominant_shape` of the entries, no entry
/// holds a control-flow node and none exceeds `table_max_entry_nodes` named nodes.
fn uniform(entries: &[Node], kinds: &Kinds, cfg: &Cfg) -> bool {
    let mut counts: HashMap<u64, usize> = HashMap::new();
    for e in entries {
        let s = shape(*e, kinds, cfg.table_max_entry_nodes);
        if s.has_control || s.named_nodes > cfg.table_max_entry_nodes {
            return false;
        }
        *counts.entry(s.hash).or_default() += 1;
    }
    let dominant = counts.values().copied().max().unwrap_or(0);
    dominant as f64 >= cfg.table_min_dominant_shape * entries.len() as f64
}

/// Winnowing (Schleimer, Wilkerson, Aiken 2003): hash every k-gram, keep the
/// minimum of each w-window, rightmost on ties, de-duplicated. Any shared run
/// of at least k+w-1 tokens is guaranteed to share a fingerprint.
fn winnow(hashes: &[u64], k: usize, w: usize) -> Vec<(usize, u64)> {
    let (k, w) = (k.max(1), w.max(1));
    if hashes.len() < k {
        return Vec::new();
    }
    let grams: Vec<u64> = hashes.windows(k).map(hash_slice).collect();
    let mut out = Vec::new();
    let mut last: Option<usize> = None;
    for start in 0..=grams.len().saturating_sub(w) {
        let win = &grams[start..(start + w).min(grams.len())];
        let mut best = 0;
        for (i, g) in win.iter().enumerate() {
            if *g <= win[best] {
                best = i;
            }
        }
        let pos = start + best;
        if last != Some(pos) {
            out.push((pos, grams[pos]));
            last = Some(pos);
        }
        if start + w >= grams.len() {
            break;
        }
    }
    out
}

fn hash_slice(s: &[u64]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn hash_str(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

const STRING_KINDS: &[&str] = &[
    "string", "template_string", "string_literal", "raw_string_literal", "concatenated_string",
    "jsx_text", "char_literal",
];
const NUMBER_KINDS: &[&str] = &[
    "integer", "float", "number", "integer_literal", "float_literal", "true", "false", "none",
    "null", "undefined",
];

fn tokenize(file: &SourceFile, tests: &TestsCfg, kinds: Arc<Kinds>) -> Tokens {
    let mut parser = file.lang.parser();
    let src = file.content.as_bytes();
    let mut out = Tokens { hashes: Vec::new(), lines: Vec::new(), starts: Vec::new(), ends: Vec::new(), inline_test_lines: 0, test_regions: Vec::new(), tree: None, kinds };
    let Some(tree) = parser.parse(src, None) else { return out };
    // Inline test regions emit no tokens, so no run can lie inside one.
    let test_regions = if tests.inline_modules && file.lang == Language::Rust {
        regions::test_regions(tree.root_node(), src)
    } else {
        Vec::new()
    };
    out.inline_test_lines = regions::inline_lines(&test_regions);
    let mut stack: Vec<Node> = vec![tree.root_node()];
    let mut last_break: Option<usize> = None;
    while let Some(n) = stack.pop() {
        let kind = n.kind();
        if kind == "comment" || kind == "line_comment" || kind == "block_comment" {
            continue;
        }
        if !test_regions.is_empty()
            && let Some(i) = regions::index_of(&test_regions, n.start_byte())
        {
            // A skipped region must not splice the code before it onto the code after it: that
            // run would span the region and count its lines as cloned. One sentinel per region,
            // unique to this file, so no k-gram across the gap can match anything.
            if last_break != Some(i) {
                last_break = Some(i);
                let r = &test_regions[i];
                out.hashes.push(hash_str(&format!("\u{0}region:{}:{i}", file.path)));
                out.lines.push(r.start_line);
                out.starts.push(r.start_byte);
                out.ends.push(r.end_byte);
            }
            continue;
        }
        let class = if STRING_KINDS.contains(&kind) {
            Some("STR")
        } else if NUMBER_KINDS.contains(&kind) {
            Some("NUM")
        } else if n.child_count() == 0 {
            if kind.contains("identifier") { Some("ID") } else { Some(kind) }
        } else {
            None
        };
        if let Some(c) = class {
            out.hashes.push(hash_str(c));
            out.lines.push(n.start_position().row + 1);
            out.starts.push(n.start_byte());
            out.ends.push(n.end_byte());
            continue;
        }
        let mut cur = n.walk();
        let children: Vec<Node> = n.children(&mut cur).collect();
        for ch in children.into_iter().rev() {
            stack.push(ch);
        }
    }
    out.tree = Some(tree);
    out.test_regions = test_regions;
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::FileKind;
    use crate::lang::Language;

    fn sf(path: &str, content: String) -> SourceFile {
        SourceFile {
            path: path.into(),
            lang: Language::Python,
            kind: FileKind::Source,
            lines: content.lines().count(),
            bytes: content.len(),
            content,
        }
    }

    fn rs(path: &str, content: String) -> SourceFile {
        SourceFile { path: path.into(), lang: Language::Rust, kind: FileKind::Source, lines: content.lines().count(), bytes: content.len(), content }
    }

    /// A dispatch match of `n` arms of one shape, about 20 tokens each; `?` in every arm.
    fn dispatch(name: &str, n: usize) -> String {
        let mut s = format!("pub fn {name}(ctx: &Ctx, c: Cmd) -> Result<i32> {{\n    match c {{\n");
        for i in 0..n {
            s.push_str(&format!("        Cmd::V{i}(a) => ok(&cmd::run_{i}(ctx, a)?, ctx),\n"));
        }
        s + "    }\n}\n"
    }

    /// A registry array of `n` struct entries of one shape, about 18 tokens each.
    fn registry(name: &str, n: usize) -> String {
        let mut s = format!("pub static {name}: &[Check] = &[\n");
        for i in 0..n {
            s.push_str(&format!("    Check {{ id: \"c{i}\", about: \"check {i}\", run: check_{i}, fix: None }},\n"));
        }
        s + "];\n"
    }

    #[test]
    fn a_uniform_match_matching_its_own_second_half_is_dropped() {
        let files = [rs("a.rs", dispatch("dispatch", 12))];
        let r = detect(&files, &Cfg::default(), &TestsCfg::default(), SymbolFallback::PrecedingItem);
        assert!(r.pairs.is_empty(), "{:?}", r.pairs);
        assert!(r.files.is_empty());
        // The knob restores the old behaviour; the tagger then sees one table on both sides,
        // `?` in every arm notwithstanding.
        assert!(!Cfg::default().table_control_kinds["rust"].iter().any(|k| k == "try_expression"));
        let keep = Cfg { drop_same_container_self_match: false, ..Cfg::default() };
        let r = detect(&files, &keep, &TestsCfg::default(), SymbolFallback::PrecedingItem);
        assert_eq!(r.pairs.len(), 1, "{:?}", r.pairs);
        let p = &r.pairs[0];
        assert_eq!((p.kind, p.container_kind.as_deref(), p.entry_count), (CloneKind::Table, Some("match_block"), Some(6)), "{p:?}");
        assert_eq!((p.a.symbol.as_str(), p.b.symbol.as_str()), ("dispatch", "dispatch"));
        assert_eq!((p.a.start_line, p.a.end_line, p.b.start_line, p.b.end_line), (3, 8, 9, 14));
    }

    #[test]
    fn sibling_copies_in_one_container_stay_logic_clones() {
        // Two methods of one impl with the same body: same container, but not a uniform run.
        let unit = |p: &str| format!("        let {p}_a = compute({p}, 1) + other[2];\n        if {p}_a > 0 && !{p} {{ panic!(\"{{}}\", {p}_a); }}\n        for {p}_i in 0..3 {{ {p}_a += {p}_i * 2; }}\n        while {p}_a > 10 {{ {p}_a -= 1; }}\n        let {p}_b: Vec<u8> = {p}.iter().filter(|x| **x > 0).cloned().collect();\n        match {p}_b.first() {{ Some(v) => {p}_a += *v as i32, None => {p}_a = 0 }}\n");
        let src = format!(
            "impl S {{\n    fn alpha(&self, aa: &[u8]) -> i32 {{\n{}        aa_a\n    }}\n    fn beta(&self, bb: &[u8]) -> i32 {{\n{}        bb_a\n    }}\n}}\n",
            unit("aa"), unit("bb")
        );
        let r = detect(&[rs("a.rs", src)], &Cfg::default(), &TestsCfg::default(), SymbolFallback::PrecedingItem);
        assert_eq!(r.pairs.len(), 1, "{:?}", r.pairs);
        let p = &r.pairs[0];
        assert_eq!((p.kind, &p.container_kind, p.entry_count), (CloneKind::Logic, &None, None));
        // Named as `scry metrics` names them: the impl's methods.
        assert_eq!((p.a.symbol.as_str(), p.b.symbol.as_str()), ("S.alpha", "S.beta"));
        let f = &r.files["a.rs"];
        assert_eq!((f.logic_clone_lines, f.table_clone_lines), (f.clone_lines, 0));
        assert!(f.tables.is_empty());
    }

    #[test]
    fn parallel_registries_are_a_table_pair_and_split_the_file_ratio() {
        let files = [rs("a.rs", registry("A", 8)), rs("b.rs", format!("use x;\n{}", registry("B", 8)))];
        let r = detect(&files, &Cfg::default(), &TestsCfg::default(), SymbolFallback::PrecedingItem);
        assert_eq!(r.pairs.len(), 1, "{:?}", r.pairs);
        let p = &r.pairs[0];
        assert_eq!((p.kind, p.container_kind.as_deref(), p.entry_count), (CloneKind::Table, Some("array_expression"), Some(8)), "{p:?}");
        assert_eq!((p.a.symbol.as_str(), p.b.symbol.as_str()), ("A", "B"));
        let fa = &r.files["a.rs"];
        assert!(fa.table_clone_lines >= 8 && fa.logic_clone_lines == 0 && fa.clone_lines == fa.table_clone_lines, "{fa:?}");
        assert_eq!(fa.tables.len(), 1);
        assert_eq!((fa.tables[0].symbol.as_deref(), fa.tables[0].container_kind.as_str()), (Some("A"), "array_expression"));
        assert!((fa.clone_ratio - fa.clone_lines as f64 / 10.0).abs() < 1e-9, "{fa:?}");
        // table_weight scales only the table share of the ratio.
        let damp = Cfg { table_weight: 0.25, ..Cfg::default() };
        let r2 = detect(&files, &damp, &TestsCfg::default(), SymbolFallback::PrecedingItem);
        assert!((r2.files["a.rs"].clone_ratio - 0.25 * fa.clone_ratio).abs() < 1e-9);
    }

    #[test]
    fn an_entry_with_control_flow_makes_the_pair_logic() {
        let reg = |name: &str| {
            let mut s = format!("pub static {name}: &[Check] = &[\n");
            for i in 0..6 {
                s.push_str(&format!("    Check {{ id: \"c{i}\", fix: if cfg_{i} {{ fix_{i} }} else {{ skip_{i} }} }},\n"));
            }
            s + "];\n"
        };
        let files = [rs("a.rs", reg("A")), rs("b.rs", reg("B"))];
        let r = detect(&files, &Cfg::default(), &TestsCfg::default(), SymbolFallback::PrecedingItem);
        assert_eq!(r.pairs.len(), 1, "{:?}", r.pairs);
        assert_eq!(r.pairs[0].kind, CloneKind::Logic);
        // The control-flow list is a knob: with none listed the same pair is a table.
        let none = Cfg { table_control_kinds: Default::default(), ..Cfg::default() };
        assert_eq!(detect(&files, &none, &TestsCfg::default(), SymbolFallback::PrecedingItem).pairs[0].kind, CloneKind::Table);
        // A one-sided table is logic too: the array side is a table, the `vec!` token tree is not.
        let mixed = [rs("a.rs", registry("A", 8)), rs("b.rs", format!("fn f() {{\n    let v = vec![\n{}    ];\n}}\n", (0..8).map(|i| format!("        Check {{ id: \"c{i}\", about: \"check {i}\", run: check_{i}, fix: None }},\n")).collect::<String>()))];
        let r = detect(&mixed, &Cfg::default(), &TestsCfg::default(), SymbolFallback::PrecedingItem);
        // (The `vec!` body also matches its own second half inside the macro's token tree, a
        // repetition the container rule cannot see through a macro; that pair is not the point.)
        let cross: Vec<&ClonePair> = r.pairs.iter().filter(|p| p.a.file != p.b.file).collect();
        assert_eq!(cross.len(), 1, "{:?}", r.pairs);
        assert_eq!((cross[0].kind, cross[0].container_kind.is_none(), cross[0].entry_count), (CloneKind::Logic, true, None), "{:?}", cross[0]);
        assert!(r.files["a.rs"].tables.is_empty() && r.files["a.rs"].table_clone_lines == 0, "{:?}", r.files["a.rs"]);
    }

    #[test]
    fn shape_share_and_entry_size_knobs_decide_a_table() {
        // Eight registry entries of one shape: a table until the node cap is below an entry's size.
        let files = [rs("a.rs", registry("A", 8)), rs("b.rs", registry("B", 8))];
        assert_eq!(detect(&files, &Cfg::default(), &TestsCfg::default(), SymbolFallback::PrecedingItem).pairs[0].kind, CloneKind::Table);
        let small = Cfg { table_max_entry_nodes: 5, ..Cfg::default() };
        assert_eq!(detect(&files, &small, &TestsCfg::default(), SymbolFallback::PrecedingItem).pairs[0].kind, CloneKind::Logic);
        // Entries cycling through three shapes: the dominant one covers 3 of 8.
        let three = |name: &str| {
            let mut s = format!("pub static {name}: &[Check] = &[\n");
            for i in 0..8 {
                s.push_str(&match i % 3 {
                    0 => format!("    Check {{ id: \"c{i}\", run: check_{i} }},\n"),
                    1 => format!("    Check {{ id: \"c{i}\", about: \"check {i}\", run: check_{i}, fix: None }},\n"),
                    _ => format!("    Check {{ id: \"c{i}\", run: check_{i}, fix: Some(fix_{i}) }},\n"),
                });
            }
            s + "];\n"
        };
        let files = [rs("a.rs", three("A")), rs("b.rs", three("B"))];
        let r = detect(&files, &Cfg::default(), &TestsCfg::default(), SymbolFallback::PrecedingItem);
        assert_eq!(r.pairs.len(), 1, "{:?}", r.pairs);
        assert_eq!(r.pairs[0].kind, CloneKind::Logic);
        let loose = Cfg { table_min_dominant_shape: 0.3, ..Cfg::default() };
        assert_eq!(detect(&files, &loose, &TestsCfg::default(), SymbolFallback::PrecedingItem).pairs[0].kind, CloneKind::Table);
    }

    #[test]
    fn dominant_child_share_keeps_a_bleeding_run_on_its_own_item() {
        // The copied body's run extends through `} fn <name>(x: u8) -> u8 { x` of the next method
        // before the bodies differ. At 0.9 the range still resolves to the method; requiring the
        // whole range moves it up to the impl.
        let unit = |p: &str| format!("        let {p}_a = compute({p}, 1) + other[2];\n        if {p}_a > 0 && !{p} {{ panic!(\"{{}}\", {p}_a); }}\n        for {p}_i in 0..3 {{ {p}_a += {p}_i * 2; }}\n        while {p}_a > 10 {{ {p}_a -= 1; }}\n        let {p}_b: Vec<u8> = {p}.iter().filter(|x| **x > 0).cloned().collect();\n        match {p}_b.first() {{ Some(v) => {p}_a += *v as i32, None => {p}_a = 0 }}\n        let {p}_c = {p}_a.checked_add(7).unwrap_or_default();\n        if let Some(first) = {p}.first() {{ {p}_a += *first as i32; }}\n        loop {{ if {p}_a % 3 == 0 {{ break; }} {p}_a -= 1; }}\n        let {p}_d: usize = {p}.len().saturating_sub({p}_c as usize);\n        {p}_a += {p}_d as i32 * {p}_b.len() as i32;\n        assert!({p}_a >= i32::MIN);\n");
        let file = |owner: &str, f: &str, g: &str, op: &str| format!("impl {owner} {{\n    fn {f}(aa: &[u8]) -> i32 {{\n{}        aa_a\n    }}\n    fn {g}(x: u8) -> u8 {{ x {op} 1 }}\n}}\n", unit("aa"));
        let files = [rs("a.rs", file("S", "alpha", "beta", "+")), rs("b.rs", file("T", "gamma", "delta", "*"))];
        let r = detect(&files, &Cfg::default(), &TestsCfg::default(), SymbolFallback::PrecedingItem);
        assert_eq!(r.pairs.len(), 1, "{:?}", r.pairs);
        assert!(r.pairs[0].a.end_line > 8 && r.pairs[0].b.end_line > 8, "{:?}", r.pairs[0]);
        // The run's backward extension reaches the `impl S {` line, which no unit holds, so the
        // symbol falls back to the preceding top-level item (the impl) whatever the share says;
        // `none` prints `?` instead.
        assert_eq!((r.pairs[0].a.start_line, r.pairs[0].a.symbol.as_str(), r.pairs[0].b.symbol.as_str()), (1, "S", "T"), "{:?}", r.pairs);
        let r = detect(&files, &Cfg::default(), &TestsCfg::default(), SymbolFallback::None);
        assert_eq!((r.pairs[0].a.symbol.as_str(), r.pairs[0].b.symbol.as_str()), ("?", "?"));
        let whole = Cfg { dominant_child_share: 1.0, ..Cfg::default() };
        let r = detect(&files, &whole, &TestsCfg::default(), SymbolFallback::PrecedingItem);
        assert_eq!((r.pairs[0].a.symbol.as_str(), r.pairs[0].b.symbol.as_str()), ("S", "T"), "{:?}", r.pairs);
    }

    #[test]
    fn uniform_siblings_under_a_copied_block_do_not_make_it_a_table() {
        // Six identical imports and a copied function with control flow: the imports are entries
        // under the run, but they are not the run.
        let body = |p: &str| format!("    let {p}_a = compute({p}, 1) + other[2];\n    if {p}_a > 0 && !{p} {{ panic!(\"{{}}\", {p}_a); }}\n    for {p}_i in 0..3 {{ {p}_a += {p}_i * 2; }}\n    while {p}_a > 10 {{ {p}_a -= 1; }}\n    let {p}_b: Vec<u8> = {p}.iter().filter(|x| **x > 0).cloned().collect();\n    match {p}_b.first() {{ Some(v) => {p}_a += *v as i32, None => {p}_a = 0 }}\n");
        let uses = |p: &str| (0..6).map(|i| format!("use crate::{p}_{i}::{{Alpha{i}, Beta{i}, Gamma{i}}};\n")).collect::<String>();
        let files = [rs("a.rs", format!("{}pub fn alpha(aa: &[u8]) -> i32 {{\n{}    aa_a\n}}\n", uses("m"), body("aa"))), rs("b.rs", format!("{}pub fn beta(bb: &[u8]) -> i32 {{\n{}    bb_a\n}}\n", uses("n"), body("bb")))];
        let r = detect(&files, &Cfg::default(), &TestsCfg::default(), SymbolFallback::PrecedingItem);
        assert_eq!(r.pairs.len(), 1, "{:?}", r.pairs);
        let p = &r.pairs[0];
        assert!(p.a.start_line <= 6 && p.a.end_line >= 13, "{p:?}");
        assert_eq!((p.kind, &p.container_kind, p.entry_count), (CloneKind::Logic, &None, None));
        let f = &r.files["a.rs"];
        assert_eq!((f.table_clone_lines, f.tables.len()), (0, 0), "{f:?}");
        // Same file, drop path: two copied fns with six uniform `use` lines between them are
        // sibling copies, not a table matching its own second half, even when the first run
        // bleeds into the `use` line that follows both.
        let src = format!("pub fn alpha(aa: &[u8]) -> i32 {{\n{}    aa_a\n}}\n{}pub fn beta(bb: &[u8]) -> i32 {{\n{}    bb_a\n}}\nuse crate::z_0::{{Alpha0, Beta0, Gamma0}};\n", body("aa"), uses("m"), body("bb"));
        let r = detect(&[rs("s.rs", src)], &Cfg::default(), &TestsCfg::default(), SymbolFallback::PrecedingItem);
        assert_eq!(r.pairs.len(), 1, "{:?}", r.pairs);
        let p = &r.pairs[0];
        assert_eq!((p.a.start_line, p.a.end_line, p.b.start_line, p.b.end_line, p.kind), (1, 10, 16, 25, CloneKind::Logic), "{p:?}");
    }

    #[test]
    fn a_fallback_item_starts_on_its_attributes_and_is_never_a_stripped_test_fn() {
        // Copied `impl` bodies whose runs extend back over `use` and `#[derive] struct` lines,
        // each below a `#[test]` fn: named after the struct, or the fn before the tests, never
        // after the test fn (which emitted no tokens).
        let body = |p: &str| format!("    fn is_switch(&self) -> bool {{ false }}\n    fn name_long(&self) -> &'static str {{ \"{p}\" }}\n    fn doc_short(&self) -> &'static str {{ \"{p} doc\" }}\n    fn update(&self, v: FlagValue, args: &mut LowArgs) -> Result<()> {{\n        let {p}_x = v.unwrap_switch();\n        if {p}_x && args.{p}.is_none() {{ args.{p} = Some({p}_x); }}\n        for {p}_i in 0..3 {{ args.count += {p}_i; }}\n        Ok(())\n    }}\n");
        let test_fn = |p: &str, s: &str| format!("#[cfg(test)]\n#[test]\nfn test_{p}() {{\n    let args = parse_low_raw([\"--{p}\"]).unwrap();\n    {s};\n}}\n");
        let with_struct = |p: &str, s: &str| format!("pub fn head_{p}(a: u8) -> u8 {{ a {s} 1 }}\n{}/// --{p}\n#[derive(Debug)]\nstruct {p};\n\nimpl Flag for {p} {{\n{}}}\n", test_fn(p, s), body(p));
        let files = [rs("a.rs", with_struct("Alpha", "assert_eq!(1, args.n)")), rs("b.rs", with_struct("Beta", "assert!(args.n > 0 && args.m < 3)"))];
        let r = detect(&files, &Cfg::default(), &TestsCfg::default(), SymbolFallback::PrecedingItem);
        assert_eq!(r.pairs.len(), 1, "{:?}", r.pairs);
        let p = &r.pairs[0];
        assert_eq!(p.a.start_line, 9, "{p:?}"); // the `#[derive]` line
        assert_eq!((p.a.symbol.as_str(), p.b.symbol.as_str()), ("Alpha", "Beta"), "{p:?}");
        // A run starting on a `use` after the test fn: the fn before the tests names it, and
        // with the regions un-stripped the test fn (now a token source) does.
        let with_use = |p: &str, s: &str| format!("pub fn head_{p}(a: u8) -> u8 {{ a {s} 1 }}\n{}use crate::flags::{{FlagValue, LowArgs}};\nimpl Flag for {p} {{\n{}}}\n", test_fn(p, s), body(p));
        let files = [rs("a.rs", with_use("Alpha", "assert_eq!(1, args.n)")), rs("b.rs", with_use("Beta", "assert!(args.n > 0 && args.m < 3)"))];
        let r = detect(&files, &Cfg::default(), &TestsCfg::default(), SymbolFallback::PrecedingItem);
        assert_eq!(r.pairs.len(), 1, "{:?}", r.pairs);
        assert_eq!((r.pairs[0].a.start_line, r.pairs[0].a.symbol.as_str(), r.pairs[0].b.symbol.as_str()), (8, "head_Alpha", "head_Beta"), "{:?}", r.pairs);
        let off = TestsCfg { inline_modules: false, ..TestsCfg::default() };
        let r = detect(&files, &Cfg::default(), &off, SymbolFallback::PrecedingItem);
        assert_eq!((r.pairs[0].a.symbol.as_str(), r.pairs[0].b.symbol.as_str()), ("test_Alpha", "test_Beta"), "{:?}", r.pairs);
    }

    #[test]
    fn runs_never_span_an_inline_test_region() {
        // Four copies of one impl body, each followed by a `#[cfg(test)]` fn (the shape of
        // ripgrep's flags/defs.rs). With the regions silently skipped, `A B` would match `C D`
        // across the tests between them and the region's lines would count as cloned.
        let unit = |p: &str| format!("        let {p}_a = compute({p}, 1) + other[2];\n        if {p}_a > 0 && !{p} {{ panic!(\"{{}}\", {p}_a); }}\n        for {p}_i in 0..3 {{ {p}_a += {p}_i * 2; }}\n        while {p}_a > 10 {{ {p}_a -= 1; }}\n        let {p}_b: Vec<u8> = {p}.iter().filter(|x| **x > 0).cloned().collect();\n        match {p}_b.first() {{ Some(v) => {p}_a += *v as i32, None => {p}_a = 0 }}\n");
        let mut src = String::new();
        for (i, p) in ["aa", "bb", "cc", "dd"].iter().enumerate() {
            src.push_str(&format!("impl S{i} {{\n    fn run(&self, {p}: &[u8]) -> i32 {{\n{}        {p}_a\n    }}\n}}\n#[cfg(test)]\n#[test]\nfn t_{i}() {{\n    assert_eq!(S{i}.run(&[{i}, {i}]), {i} + {i});\n    assert!(S{i}.run(&[]) == 0);\n}}\n", unit(p)));
        }
        let f = rs("a.rs", src.clone());
        let regions = regions::test_regions(Language::Rust.parser().parse(&src, None).unwrap().root_node(), src.as_bytes());
        assert_eq!(regions.len(), 4);
        let r = detect(&[f], &Cfg::default(), &TestsCfg::default(), SymbolFallback::PrecedingItem);
        assert!(!r.pairs.is_empty());
        for p in &r.pairs {
            for side in [&p.a, &p.b] {
                assert!(!regions.iter().any(|t| side.start_line <= t.end_line && t.start_line <= side.end_line), "{p:?} overlaps {regions:?}");
            }
        }
        let fc = &r.files["a.rs"];
        assert!(fc.clone_ratio <= 1.0 && fc.clone_lines <= src.lines().count() - regions::inline_lines(&regions), "{fc:?}");
    }

    /// Twelve structurally different lines, so the body does not repeat itself.
    fn body(p: &str) -> String {
        format!(
            "    {p}_a = compute({p}, 1) + other[2]\n\
    if {p}_a and not {p}:\n\
        raise ValueError({p}_a)\n\
    for {p}_i in range(3):\n\
        {p}_a += {p}_i * 2\n\
    while {p}_a > 10:\n\
        {p}_a -= 1\n\
    {p}_b = [{p}_x for {p}_x in {p} if {p}_x]\n\
    try:\n\
        {p}_c = {p}_b[0]\n\
    except IndexError:\n\
        {p}_c = None\n"
        )
    }

    #[test]
    fn renamed_copy_is_found_and_lines_are_right() {
        let a = format!("def alpha(x):\n{}\n    return x\n", body("aa"));
        let b = format!("import os\n\ndef beta(y):\n{}\n    return y\n", body("bb"));
        let r = detect(&[sf("a.py", a), sf("b.py", b)], &Cfg::default(), &TestsCfg::default(), SymbolFallback::PrecedingItem);
        assert_eq!(r.pairs.len(), 1, "{:?}", r.pairs);
        let p = &r.pairs[0];
        assert_eq!(p.a.file, "a.py");
        assert_eq!(p.b.file, "b.py");
        assert!(p.a.start_line <= 2 && p.a.end_line >= 13, "{p:?}");
        assert!(p.b.start_line <= 4 && p.b.end_line >= 15, "{p:?}");
        assert!(r.files["a.py"].clone_ratio > 0.7);
    }

    #[test]
    fn periodic_block_copied_into_many_files_is_still_found() {
        // 14 repeats x 6 files = 84 positions per fingerprint: more than the old
        // position cap, but only 6 files.
        let body = |p: &str| {
            let mut s = format!("def {p}_fn(x):\n");
            for i in 0..14 {
                s.push_str(&format!("    {p}_v{i} = compute_{i}(x, {i}) + other[{i}]\n    if {p}_v{i} and not x:\n        raise ValueError({p}_v{i})\n"));
            }
            s + "    return x\n"
        };
        let files: Vec<SourceFile> = ["a", "b", "c", "d", "e", "f"].iter().map(|p| sf(&format!("{p}.py"), body(p))).collect();
        let r = detect(&files, &Cfg::default(), &TestsCfg::default(), SymbolFallback::PrecedingItem);
        assert!(r.pairs.len() >= 15, "{} pairs", r.pairs.len());
        assert_eq!(r.files.len(), 6);
        // Deterministic order across runs.
        let again = detect(&files, &Cfg::default(), &TestsCfg::default(), SymbolFallback::PrecedingItem);
        let key = |r: &CloneReport| r.pairs.iter().map(|p| format!("{}:{}-{}:{}", p.a.file, p.a.start_line, p.b.file, p.b.start_line)).collect::<Vec<_>>();
        assert_eq!(key(&r), key(&again));
    }

    #[test]
    fn min_tokens_is_tunable() {
        let a = format!("def alpha(x):\n{}\n    return x\n", body("aa"));
        let b = format!("def beta(y):\n{}\n    return y\n", body("bb"));
        let files = [sf("a.py", a), sf("b.py", b)];
        let strict = Cfg { min_tokens: 10_000, ..Cfg::default() };
        assert!(detect(&files, &strict, &TestsCfg::default(), SymbolFallback::PrecedingItem).pairs.is_empty());
        assert_eq!(detect(&files, &Cfg::default(), &TestsCfg::default(), SymbolFallback::PrecedingItem).pairs.len(), 1);
    }

    #[test]
    fn rust_inline_test_regions_emit_no_tokens() {
        // The same twelve-line fixture twice inside `mod tests`, and once more in source.
        let unit = |p: &str| format!("    let {p}_a = compute({p}, 1) + other[2];\n    if {p}_a > 0 && !{p} {{ panic!(\"{{}}\", {p}_a); }}\n    for {p}_i in 0..3 {{ {p}_a += {p}_i * 2; }}\n    while {p}_a > 10 {{ {p}_a -= 1; }}\n    let {p}_b: Vec<u8> = {p}.iter().filter(|x| **x > 0).cloned().collect();\n    match {p}_b.first() {{ Some(v) => {p}_a += *v as i32, None => {p}_a = 0 }}\n");
        let src = format!(
            "pub fn alpha(aa: &[u8]) -> i32 {{\n{}    aa_a\n}}\n#[cfg(test)]\nmod tests {{\n    fn one(bb: &[u8]) -> i32 {{\n{}        bb_a\n    }}\n    fn two(cc: &[u8]) -> i32 {{\n{}        cc_a\n    }}\n}}\n",
            unit("aa"), unit("bb"), unit("cc")
        );
        let f = SourceFile { path: "a.rs".into(), lang: Language::Rust, kind: FileKind::Source, lines: src.lines().count(), bytes: src.len(), content: src };
        let files = [f];
        let off = TestsCfg { inline_modules: false, ..TestsCfg::default() };
        let with_tests = detect(&files, &Cfg::default(), &off, SymbolFallback::PrecedingItem);
        assert!(with_tests.pairs.iter().any(|p| p.a.start_line > 9 && p.b.start_line > 9), "{:?}", with_tests.pairs);
        let stripped = detect(&files, &Cfg::default(), &TestsCfg::default(), SymbolFallback::PrecedingItem);
        assert!(stripped.pairs.iter().all(|p| p.a.end_line <= 9 && p.b.end_line <= 9), "{:?}", stripped.pairs);
        assert!(stripped.pairs.len() < with_tests.pairs.len(), "{} vs {}", stripped.pairs.len(), with_tests.pairs.len());
    }

    #[test]
    fn different_code_is_not_a_clone() {
        let a = "\
def load(path):
    with open(path) as fh:
        data = json.load(fh)
    for key, value in data.items():
        if not isinstance(value, dict):
            raise ValueError(key)
    return {k: Entry(**v) for k, v in data.items()}

class Registry:
    def __init__(self):
        self.entries = {}
    def add(self, e):
        self.entries[e.id] = e
    def find(self, pred):
        return [e for e in self.entries.values() if pred(e)]
";
        let b = "\
async def handler(request):
    body = await request.json()
    try:
        user = await lookup(body['id'])
    except KeyError:
        return web.Response(status=400)
    while user.pending:
        await asyncio.sleep(0.1)
    match user.role:
        case 'admin': return admin_view(user)
        case _: return plain_view(user)
";
        let r = detect(&[sf("a.py", a.into()), sf("b.py", b.into())], &Cfg::default(), &TestsCfg::default(), SymbolFallback::PrecedingItem);
        assert!(r.pairs.is_empty(), "{:?}", r.pairs);
    }
}
