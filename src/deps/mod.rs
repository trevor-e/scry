//! Import graph: who depends on whom, and where the graph is tangled.
//!
//! Imports are pulled from tree-sitter nodes, resolved against the discovered
//! file set (never the filesystem, so the graph matches what other passes see),
//! and fed to Tarjan's SCC. Cycles are reported at two granularities: file
//! cycles are the concrete tangle, directory cycles are the architectural one.
//! Every edge knows what it carries (`use` / `mod` / `type_only`, distinct names),
//! so a large file cycle also gets the cheapest import to cut and what that buys.

use crate::config::Deps as Cfg;
use crate::discover::{FileKind, SourceFile};
use crate::lang::Language;
use petgraph::algo::tarjan_scc;
use petgraph::graph::{DiGraph, NodeIndex};
use rayon::prelude::*;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use tree_sitter::Node;

#[derive(Debug, Default, Clone, Serialize)]
pub struct FileDeps {
    /// Distinct non-test files importing this one.
    pub fan_in: usize,
    /// Distinct in-repo files this one imports.
    pub fan_out: usize,
    /// Distinct test files importing this one (test proximity signal).
    pub test_refs: usize,
    /// Bare specifiers / third-party modules that did not resolve in-repo.
    pub external: usize,
    pub in_cycle: bool,
    /// fan_out / (fan_in + fan_out); 1.0 = depends on everything, nothing depends on it.
    pub instability: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// TS `import type` / `import { type X }` / `export type { X } from`: erased at runtime.
    TypeOnly,
    Use,
    /// A Rust `mod x;` declaration contributes: the edge cannot be cut by removing an import.
    Mod,
}

/// One import edge and what it carries.
#[derive(Debug, Clone, Serialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
    /// Distinct imported names, plus `glob_import_symbol_cost` per wildcard. For a `mod` edge
    /// this is informational: its cut cost is infinite.
    pub symbols: u32,
    pub names: Vec<String>,
    pub glob: bool,
    /// Line of the first contributing import.
    pub line: usize,
}

/// A single edge removed from a cycle and the largest cycle that leaves.
#[derive(Debug, Clone, Serialize)]
pub struct EdgeCut {
    #[serde(flatten)]
    pub edge: Edge,
    pub largest_after: usize,
}

/// Dropping every non-`mod` import one member makes inside the cycle.
#[derive(Debug, Clone, Serialize)]
pub struct HubCut {
    pub member: String,
    pub imports: usize,
    pub symbols: u32,
    /// One of the dropped imports is a wildcard, so `symbols` is a floor.
    pub glob: bool,
    pub largest_after: usize,
}

/// Cut analysis of one file cycle (only for cycles with `min_cycle_size_to_cut` members).
#[derive(Debug, Clone, Serialize)]
pub struct Cuts {
    pub internal_edges: usize,
    pub mod_edges: usize,
    /// Edges left out of the search when `ignore_type_only_imports`.
    pub type_only_edges: usize,
    /// Largest cycle among the members before any cut (smaller than the member count only
    /// when type-only edges were carrying part of it).
    pub base: usize,
    /// Cheapest single import whose removal leaves the smallest cycle (then fewest symbols).
    pub single: Option<EdgeCut>,
    /// Best hub cut, kept only when it leaves a smaller cycle than `single`.
    pub hub: Option<HubCut>,
    /// Greedy best-single-edge cuts that bring every cycle under `min_cycle_size_to_cut`;
    /// empty when that takes more than `max_cut_set` edges.
    pub cut_set: Vec<EdgeCut>,
    /// `single` still leaves at least `no_single_cut_share` of the members in a cycle.
    pub no_single_break: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Cycle {
    pub members: Vec<String>,
    /// Deepest directory containing every member (`.` for the root).
    pub dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cuts: Option<Cuts>,
}

#[derive(Debug, Default, Serialize)]
pub struct DepGraph {
    pub files: HashMap<String, FileDeps>,
    pub file_cycles: Vec<Cycle>,
    pub dir_cycles: Vec<Cycle>,
    pub edges: usize,
    #[serde(skip)]
    edge_map: HashMap<(String, String), Edge>,
}

impl DepGraph {
    /// True when either file imports the other, directly.
    pub fn connected(&self, a: &str, b: &str) -> bool {
        self.edge_map.contains_key(&(a.to_string(), b.to_string()))
            || self.edge_map.contains_key(&(b.to_string(), a.to_string()))
    }

    /// In-repo files that both `a` and `b` import directly, sorted.
    pub fn shared_imports(&self, a: &str, b: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .edge_map
            .keys()
            .filter(|(s, t)| s == a && self.edge_map.contains_key(&(b.to_string(), t.clone())))
            .map(|(_, t)| t.clone())
            .collect();
        out.sort();
        out
    }

    /// The edge `from -> to`, if any.
    pub(crate) fn edge(&self, from: &str, to: &str) -> Option<&Edge> {
        self.edge_map.get(&(from.to_string(), to.to_string()))
    }

    #[cfg(test)]
    pub(crate) fn add_edge(&mut self, from: &str, to: &str) {
        self.edge_map.insert(
            (from.to_string(), to.to_string()),
            Edge { from: from.into(), to: to.into(), kind: EdgeKind::Use, symbols: 1, names: vec![], glob: false, line: 1 },
        );
        self.edges = self.edge_map.len();
    }
}

#[derive(Debug)]
struct RawImport {
    /// Module spec as written: `a.b.c`, `./x`, `crate::y`.
    spec: String,
    /// Python: `from a.b import c` – `c` may itself be a submodule.
    names: Vec<String>,
    /// Python relative-import level (number of leading dots).
    level: usize,
    /// What the import brings in: the leaf names, or a wildcard.
    syms: Vec<String>,
    glob: bool,
    kind: EdgeKind,
    line: usize,
}

impl RawImport {
    fn new(spec: String, syms: Vec<String>, glob: bool, kind: EdgeKind, line: usize) -> Self {
        Self { spec, names: vec![], level: 0, syms, glob, kind, line }
    }
}

/// What one file's imports of one target add up to, before it becomes an `Edge`.
#[derive(Debug, Default, Clone)]
struct Carried {
    is_mod: bool,
    names: BTreeSet<String>,
    globs: u32,
    type_names: BTreeSet<String>,
    type_globs: u32,
    line: usize,
}

impl Carried {
    fn add(&mut self, r: &RawImport) {
        if self.line == 0 || r.line < self.line {
            self.line = r.line;
        }
        match r.kind {
            EdgeKind::Mod => {
                self.is_mod = true;
                self.names.extend(r.syms.iter().cloned());
            }
            EdgeKind::Use => {
                self.names.extend(r.syms.iter().cloned());
                self.globs += u32::from(r.glob);
            }
            EdgeKind::TypeOnly => {
                self.type_names.extend(r.syms.iter().cloned());
                self.type_globs += u32::from(r.glob);
            }
        }
    }

    fn into_edge(self, from: &str, to: &str, glob_cost: u32) -> Edge {
        let kind = if self.is_mod {
            EdgeKind::Mod
        } else if !self.names.is_empty() || self.globs > 0 {
            EdgeKind::Use
        } else {
            EdgeKind::TypeOnly
        };
        let (names, globs) = if kind == EdgeKind::TypeOnly { (self.type_names, self.type_globs) } else { (self.names, self.globs) };
        Edge {
            from: from.to_string(),
            to: to.to_string(),
            kind,
            symbols: names.len() as u32 + globs * glob_cost,
            names: names.into_iter().collect(),
            glob: globs > 0,
            line: self.line,
        }
    }
}

pub fn build(all: &[SourceFile], cfg: &Cfg) -> DepGraph {
    // Third-party code checked into the tree is not part of this repo's graph:
    // its cycles are not ours to fix. Generated files may be imported, but
    // their own imports are not walked, so they never form cycles either.
    let files: Vec<&SourceFile> = all.iter().filter(|f| f.kind != FileKind::Vendored).collect();
    let known: HashSet<&str> = files.iter().map(|f| f.path.as_str()).collect();
    let kind_of: HashMap<&str, FileKind> = files.iter().map(|f| (f.path.as_str(), f.kind)).collect();

    type PerFile = (usize, Vec<(String, Carried)>, usize);
    let per_file: Vec<PerFile> = files
        .par_iter()
        .enumerate()
        .map(|(i, f)| {
            let raws = if f.kind == FileKind::Generated { Vec::new() } else { extract(f) };
            let mut targets: BTreeMap<String, Carried> = BTreeMap::new();
            let mut external = 0usize;
            for r in raws {
                match resolve(&f.path, f.lang, &r, &known, cfg) {
                    Some(t) => {
                        for t in t {
                            if t != f.path {
                                targets.entry(t).or_default().add(&r);
                            }
                        }
                    }
                    None => external += 1,
                }
            }
            (i, targets.into_iter().collect(), external)
        })
        .collect();

    let mut graph: DiGraph<usize, ()> = DiGraph::new();
    let idx: HashMap<&str, NodeIndex> =
        files.iter().enumerate().map(|(i, f)| (f.path.as_str(), graph.add_node(i))).collect();
    let mut out = DepGraph::default();
    let mut fan_in: HashMap<&str, usize> = HashMap::new();
    let mut test_refs: HashMap<&str, usize> = HashMap::new();

    for (i, targets, external) in per_file {
        let src = files[i].path.as_str();
        let is_test = kind_of[src] == FileKind::Test;
        let fan_out = targets.len();
        for (t, carried) in targets {
            let (Some(&a), Some(&b)) = (idx.get(src), idx.get(t.as_str())) else { continue };
            graph.add_edge(a, b, ());
            if is_test {
                *test_refs.entry(files[graph[b]].path.as_str()).or_default() += 1;
            } else {
                *fan_in.entry(files[graph[b]].path.as_str()).or_default() += 1;
            }
            let edge = carried.into_edge(src, &t, cfg.glob_import_symbol_cost);
            out.edge_map.insert((src.to_string(), t), edge);
        }
        out.files.insert(src.to_string(), FileDeps { fan_out, external, ..Default::default() });
    }
    out.edges = out.edge_map.len();

    for (p, d) in out.files.iter_mut() {
        d.fan_in = fan_in.get(p.as_str()).copied().unwrap_or(0);
        d.test_refs = test_refs.get(p.as_str()).copied().unwrap_or(0);
        let total = d.fan_in + d.fan_out;
        d.instability = if total == 0 { 0.0 } else { d.fan_out as f64 / total as f64 };
    }

    // File-level cycles.
    for scc in tarjan_scc(&graph) {
        if scc.len() < 2 {
            continue;
        }
        let mut members: Vec<String> = scc.iter().map(|n| files[graph[*n]].path.clone()).collect();
        members.sort();
        for m in &members {
            if let Some(d) = out.files.get_mut(m) {
                d.in_cycle = true;
            }
        }
        let rust = scc.iter().all(|n| files[graph[*n]].lang == Language::Rust);
        let cuts = (members.len() >= cfg.min_cycle_size_to_cut && (cfg.cut_rust_cycles || !rust))
            .then(|| cuts_for(&members, &out.edge_map, cfg));
        out.file_cycles.push(Cycle { dir: common_dir(&members), members, cuts });
    }
    out.file_cycles.sort_by_key(|c| std::cmp::Reverse(c.members.len()));

    // Directory-level cycles: collapse files to their directory, drop self-edges.
    let mut dgraph: DiGraph<String, ()> = DiGraph::new();
    let mut didx: HashMap<String, NodeIndex> = HashMap::new();
    let mut dedges: HashSet<(String, String)> = HashSet::new();
    for (a, b) in out.edge_map.keys() {
        let (da, db) = (dir_of(a), dir_of(b));
        if da != db && kind_of[a.as_str()] != FileKind::Test {
            dedges.insert((da.to_string(), db.to_string()));
        }
    }
    for (a, b) in &dedges {
        let na = *didx.entry(a.clone()).or_insert_with(|| dgraph.add_node(a.clone()));
        let nb = *didx.entry(b.clone()).or_insert_with(|| dgraph.add_node(b.clone()));
        dgraph.add_edge(na, nb, ());
    }
    for scc in tarjan_scc(&dgraph) {
        if scc.len() < 2 {
            continue;
        }
        let mut members: Vec<String> = scc.iter().map(|n| dgraph[*n].clone()).collect();
        members.sort();
        out.dir_cycles.push(Cycle { dir: common_dir_of(members.iter().map(|m| m.as_str())), members, cuts: None });
    }
    out.dir_cycles.sort_by_key(|c| std::cmp::Reverse(c.members.len()));
    out
}

fn dir_of(path: &str) -> &str {
    path.rfind('/').map(|i| &path[..i]).unwrap_or("")
}

/// Deepest directory containing every file; `.` at the root.
fn common_dir(files: &[String]) -> String {
    common_dir_of(files.iter().map(|f| dir_of(f)))
}

fn common_dir_of<'a>(dirs: impl Iterator<Item = &'a str>) -> String {
    let mut common: Option<Vec<&str>> = None;
    for d in dirs {
        let parts: Vec<&str> = d.split('/').filter(|s| !s.is_empty()).collect();
        common = Some(match common {
            None => parts,
            Some(c) => c.iter().zip(&parts).take_while(|(a, b)| a == b).map(|(a, _)| *a).collect(),
        });
    }
    match common {
        Some(c) if !c.is_empty() => c.join("/"),
        _ => ".".to_string(),
    }
}

// ---------- cuts ----------

/// One internal edge of a cycle, in local member indices; `cost` is `u64::MAX` for `mod`.
struct Internal<'a> {
    from: usize,
    to: usize,
    cost: u64,
    edge: &'a Edge,
}

/// Largest SCC among `n` members once the edges at `removed` positions are gone.
fn largest_after(n: usize, edges: &[Internal], removed: &[bool]) -> usize {
    let mut g: DiGraph<(), ()> = DiGraph::with_capacity(n, edges.len());
    let nodes: Vec<NodeIndex> = (0..n).map(|_| g.add_node(())).collect();
    for (i, e) in edges.iter().enumerate() {
        if !removed[i] {
            g.add_edge(nodes[e.from], nodes[e.to], ());
        }
    }
    tarjan_scc(&g).iter().map(|c| c.len()).max().unwrap_or(0)
}

/// Best single cuttable edge on top of `removed`: smallest largest-after, then cheapest, then
/// edge order (cheapest first, so ties are stable). Tries at most `max_edges_tried` edges.
fn best_single(n: usize, edges: &[Internal], removed: &[bool], cfg: &Cfg) -> Option<(usize, usize)> {
    let mut best: Option<(usize, u64, usize)> = None;
    let mut scratch = removed.to_vec();
    for (i, e) in edges.iter().enumerate().filter(|(i, e)| !removed[*i] && e.cost != u64::MAX).take(cfg.max_edges_tried) {
        scratch[i] = true;
        let after = largest_after(n, edges, &scratch);
        scratch[i] = false;
        if best.is_none_or(|(a, c, _)| (after, e.cost) < (a, c)) {
            best = Some((after, e.cost, i));
        }
    }
    best.map(|(after, _, i)| (i, after))
}

fn cuts_for(members: &[String], edge_map: &HashMap<(String, String), Edge>, cfg: &Cfg) -> Cuts {
    let n = members.len();
    let pos: HashMap<&str, usize> = members.iter().enumerate().map(|(i, m)| (m.as_str(), i)).collect();
    let mut all: Vec<&Edge> = edge_map.values().filter(|e| pos.contains_key(e.from.as_str()) && pos.contains_key(e.to.as_str())).collect();
    all.sort_by(|a, b| (a.from.as_str(), a.to.as_str()).cmp(&(b.from.as_str(), b.to.as_str())));
    let internal_edges = all.len();
    let mod_edges = all.iter().filter(|e| e.kind == EdgeKind::Mod).count();
    let type_only_edges = all.iter().filter(|e| e.kind == EdgeKind::TypeOnly).count();
    let mut edges: Vec<Internal> = all
        .into_iter()
        .filter(|e| !(cfg.ignore_type_only_imports && e.kind == EdgeKind::TypeOnly))
        .map(|e| Internal {
            from: pos[e.from.as_str()],
            to: pos[e.to.as_str()],
            cost: if e.kind == EdgeKind::Mod { u64::MAX } else { u64::from(e.symbols) },
            edge: e,
        })
        .collect();
    edges.sort_by_key(|e| e.cost);
    let none = vec![false; edges.len()];
    let base = largest_after(n, &edges, &none);
    let cut = |i: usize, after: usize| EdgeCut { edge: edges[i].edge.clone(), largest_after: after };

    let single = (base >= 2).then(|| best_single(n, &edges, &none, cfg)).flatten().map(|(i, after)| cut(i, after));

    let mut hub: Option<HubCut> = None;
    if cfg.report_hub_cut && base >= 2 {
        let mut best: Option<(usize, u64, usize)> = None;
        for (m, name) in members.iter().enumerate() {
            let removed: Vec<bool> = edges.iter().map(|e| e.from == m && e.cost != u64::MAX).collect();
            let imports = removed.iter().filter(|r| **r).count();
            if imports == 0 {
                continue;
            }
            let after = largest_after(n, &edges, &removed);
            let dropped = || edges.iter().zip(&removed).filter(|(_, r)| **r).map(|(e, _)| e);
            let cost: u64 = dropped().map(|e| e.cost).sum();
            if best.is_none_or(|(a, c, _)| (after, cost) < (a, c)) {
                best = Some((after, cost, m));
                hub = Some(HubCut { member: name.clone(), imports, symbols: cost as u32, glob: dropped().any(|e| e.edge.glob), largest_after: after });
            }
        }
        if let (Some(h), Some(s)) = (&hub, &single) && h.largest_after >= s.largest_after {
            hub = None;
        }
    }

    // Greedy: keep taking the best single edge on the residual until every cycle is small.
    let mut removed = none.clone();
    let mut set: Vec<EdgeCut> = Vec::new();
    let mut dissolved = largest_after(n, &edges, &removed) < cfg.min_cycle_size_to_cut;
    while !dissolved && set.len() < cfg.max_cut_set {
        let Some((i, after)) = best_single(n, &edges, &removed, cfg) else { break };
        removed[i] = true;
        set.push(cut(i, after));
        dissolved = after < cfg.min_cycle_size_to_cut;
    }
    let cut_set = if dissolved { set } else { Vec::new() };

    // Only meaningful over a runtime cycle: with none there is nothing a cut could break.
    let no_single_break = base >= 2 && single.as_ref().is_none_or(|s| s.largest_after as f64 >= cfg.no_single_cut_share * n as f64);
    Cuts { internal_edges, mod_edges, type_only_edges, base, single, hub, cut_set, no_single_break }
}

/// The file name, with its directory when the name alone says nothing: `searcher/mod.rs`,
/// `dom/index.ts`, `pkg/__init__.py`.
fn short_name(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    let generic = name == "mod.rs" || name == "__init__.py" || name.starts_with("index.");
    match path[..path.len() - name.len()].trim_end_matches('/').rsplit('/').next() {
        Some(parent) if generic && !parent.is_empty() => &path[path.len() - name.len() - parent.len() - 1..],
        _ => name,
    }
}

/// `EntityRef`, `A, B, C, D, +3 more`, `*` for a wildcard.
pub fn symbol_names(names: &[String], glob: bool) -> String {
    let mut shown: Vec<String> = names.iter().take(4).cloned().collect();
    if names.len() > 4 {
        shown.push(format!("+{} more", names.len() - 4));
    }
    if glob {
        shown.push("*".to_string());
    }
    shown.join(", ")
}

/// `1 symbol`, `3 symbols`, `>= 20 symbols` for a wildcard.
pub fn symbol_count(e: &Edge) -> String {
    match (e.glob, e.symbols) {
        (true, n) => format!(">= {n} symbols"),
        (false, 1) => "1 symbol".to_string(),
        (false, n) => format!("{n} symbols"),
    }
}

impl Cycle {
    /// The one-line block header: `15-file cycle in src: cheapest cut src/paths.rs -> src/plan.rs
    /// imports 1 symbol (EntityRef, line 16) -> largest remaining cycle 12; hub cut: drop
    /// paths.rs's 4 imports (9 symbols) -> 11; no single import breaks this cycle`.
    pub fn headline(&self) -> String {
        let mut o = format!("{}-file cycle {}", self.members.len(), if self.dir == "." { "at the repo root".to_string() } else { format!("in {}", self.dir) });
        let Some(c) = &self.cuts else { return o };
        let mut parts: Vec<String> = Vec::new();
        if c.type_only_edges > 0 && c.base < self.members.len() {
            let s = if c.type_only_edges == 1 { "" } else { "s" };
            parts.push(format!("{} type-only import{s} ignored, largest runtime cycle {}", c.type_only_edges, c.base));
        }
        match &c.single {
            Some(s) => parts.push(format!(
                "cheapest cut {} -> {} imports {} ({}, line {}) -> largest remaining cycle {}",
                s.edge.from, s.edge.to, symbol_count(&s.edge), symbol_names(&s.edge.names, s.edge.glob), s.edge.line, s.largest_after
            )),
            None if c.base >= 2 => parts.push(format!("no cuttable import: all {} internal edges are mod declarations", c.mod_edges)),
            None => parts.push("no runtime cycle".to_string()),
        }
        if let Some(h) = &c.hub {
            let s = if h.imports == 1 { "" } else { "s" };
            let floor = if h.glob { ">= " } else { "" };
            parts.push(format!("hub cut: drop {}'s {} import{s} ({floor}{} symbols) -> {}", short_name(&h.member), h.imports, h.symbols, h.largest_after));
        }
        if c.no_single_break && c.base >= 2 {
            parts.push("no single import breaks this cycle".to_string());
        }
        o.push_str(": ");
        o.push_str(&parts.join("; "));
        o
    }

    /// `cut set of 3 imports dissolves it: src/a.rs -> src/b.rs (X), …`, when the greedy set did.
    pub fn cut_set_line(&self) -> Option<String> {
        let c = self.cuts.as_ref()?;
        if c.cut_set.is_empty() {
            return None;
        }
        let edges: Vec<String> = c.cut_set.iter().map(|e| format!("{} -> {} ({})", e.edge.from, e.edge.to, symbol_names(&e.edge.names, e.edge.glob))).collect();
        let s = if c.cut_set.len() == 1 { "" } else { "s" };
        Some(format!("cut set of {} import{s} dissolves it: {}", c.cut_set.len(), edges.join(", ")))
    }

    /// `cut: paths.rs -> plan.rs, EntityRef` for a member on the cheapest cut edge; the report
    /// joins it into the member's reason.
    pub fn cut_clause(&self, member: &str) -> Option<String> {
        let s = self.cuts.as_ref()?.single.as_ref()?;
        (s.edge.from == member || s.edge.to == member)
            .then(|| format!("cut: {} -> {}, {}", short_name(&s.edge.from), short_name(&s.edge.to), symbol_names(&s.edge.names, s.edge.glob)))
    }
}

// ---------- extraction ----------

fn extract(file: &SourceFile) -> Vec<RawImport> {
    let mut parser = file.lang.parser();
    let src = file.content.as_bytes();
    let Some(tree) = parser.parse(src, None) else { return Vec::new() };
    let mut raws = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        match file.lang {
            Language::Python => extract_python(n, src, &mut raws),
            Language::Rust => extract_rust(n, src, &mut raws),
            _ => extract_js(n, src, &mut raws),
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    raws
}

fn t<'a>(n: Node, src: &'a [u8]) -> &'a str {
    n.utf8_text(src).unwrap_or("")
}

fn line_of(n: Node) -> usize {
    n.start_position().row + 1
}

/// True when `n` has an anonymous `type` keyword child (`import type …`, `{ type X }`).
fn has_type_keyword(n: Node) -> bool {
    let mut c = n.walk();
    n.children(&mut c).any(|ch| !ch.is_named() && ch.kind() == "type")
}

fn extract_python(n: Node, src: &[u8], out: &mut Vec<RawImport>) {
    match n.kind() {
        "import_statement" => {
            let mut c = n.walk();
            for ch in n.named_children(&mut c) {
                let name = match ch.kind() {
                    "dotted_name" => t(ch, src),
                    "aliased_import" => ch.child_by_field_name("name").map(|x| t(x, src)).unwrap_or(""),
                    _ => continue,
                };
                out.push(RawImport::new(name.to_string(), vec![name.to_string()], false, EdgeKind::Use, line_of(n)));
            }
        }
        "import_from_statement" => {
            let Some(module) = n.child_by_field_name("module_name") else { return };
            let (spec, level) = match module.kind() {
                "relative_import" => {
                    let text = t(module, src);
                    let dots = text.chars().take_while(|c| *c == '.').count();
                    (text[dots..].to_string(), dots)
                }
                _ => (t(module, src).to_string(), 0),
            };
            let mut names = Vec::new();
            let mut c = n.walk();
            for ch in n.children_by_field_name("name", &mut c) {
                let nm = match ch.kind() {
                    "aliased_import" => ch.child_by_field_name("name").map(|x| t(x, src)).unwrap_or(""),
                    _ => t(ch, src),
                };
                if !nm.is_empty() {
                    names.push(nm.to_string());
                }
            }
            let mut c = n.walk();
            let glob = n.children(&mut c).any(|ch| ch.kind() == "wildcard_import");
            out.push(RawImport { spec, syms: names.clone(), names, level, glob, kind: EdgeKind::Use, line: line_of(n) });
        }
        _ => {}
    }
}

fn extract_js(n: Node, src: &[u8], out: &mut Vec<RawImport>) {
    match n.kind() {
        "import_statement" | "export_statement" => {
            let Some(s) = n.child_by_field_name("source") else { return };
            let spec = unquote(t(s, src));
            let line = line_of(n);
            let all_type = has_type_keyword(n);
            // Names split by whether they survive to runtime: `import { type A, b }` is one
            // type-only import and one value import of the same file.
            let (mut value, mut types): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
            // `export * from './x'`: the star is an anonymous token with no named node around it.
            let mut c = n.walk();
            let mut glob = n.children(&mut c).any(|ch| !ch.is_named() && ch.kind() == "*");
            let mut stack = vec![n];
            while let Some(x) = stack.pop() {
                match x.kind() {
                    "import_specifier" | "export_specifier" => {
                        let name = x.child_by_field_name("name").map(|y| t(y, src).to_string()).unwrap_or_default();
                        if all_type || has_type_keyword(x) { types.push(name) } else { value.push(name) }
                        continue;
                    }
                    "namespace_import" | "namespace_export" => {
                        glob = true;
                        continue;
                    }
                    "identifier" if x.parent().is_some_and(|p| p.kind() == "import_clause") => {
                        let name = t(x, src).to_string();
                        if all_type { types.push(name) } else { value.push(name) }
                        continue;
                    }
                    "string" => continue,
                    _ => {}
                }
                let mut c = x.walk();
                for ch in x.children(&mut c) {
                    stack.push(ch);
                }
            }
            let bare = value.is_empty() && types.is_empty() && !glob;
            if all_type && !glob {
                out.push(RawImport::new(spec, types, false, EdgeKind::TypeOnly, line));
                return;
            }
            if !types.is_empty() {
                out.push(RawImport::new(spec.clone(), types, false, EdgeKind::TypeOnly, line));
            }
            if !value.is_empty() || glob || bare {
                // `export * from`, `import * as ns` are wildcards; a bare `import './x'` counts as one.
                let syms = if bare { vec![spec.clone()] } else { value };
                out.push(RawImport::new(spec, syms, glob, if all_type { EdgeKind::TypeOnly } else { EdgeKind::Use }, line));
            }
        }
        "call_expression" => {
            let Some(f) = n.child_by_field_name("function") else { return };
            let callee = t(f, src);
            if callee != "require" && callee != "import" {
                return;
            }
            if let Some(args) = n.child_by_field_name("arguments") {
                if let Some(a) = args.named_child(0).filter(|a| a.kind() == "string") {
                    let spec = unquote(t(a, src));
                    out.push(RawImport::new(spec.clone(), vec![spec], false, EdgeKind::Use, line_of(n)));
                }
            }
        }
        _ => {}
    }
}

fn extract_rust(n: Node, src: &[u8], out: &mut Vec<RawImport>) {
    match n.kind() {
        "mod_item" if n.child_by_field_name("body").is_none() => {
            if let Some(name) = n.child_by_field_name("name") {
                let name = t(name, src).to_string();
                out.push(RawImport::new(format!("mod:{name}"), vec![name], false, EdgeKind::Mod, line_of(n)));
            }
        }
        "use_declaration" => {
            if let Some(arg) = n.child_by_field_name("argument") {
                // Inside `mod tests { … }` (an inline module), `super` is the file's
                // own module, not the parent file.
                let mut depth = 0;
                let mut cur = n.parent();
                while let Some(p) = cur {
                    depth += usize::from(p.kind() == "mod_item");
                    cur = p.parent();
                }
                let mut paths = Vec::new();
                use_paths(arg, src, "", &mut paths);
                for (p, leaf) in paths {
                    let Some(p) = rebase_inline(&p, depth) else { continue };
                    if p == "crate" || p == "super" || p == "self"
                        || p.starts_with("crate::") || p.starts_with("super::") || p.starts_with("self::")
                    {
                        let (syms, glob) = match leaf {
                            Some(name) => (vec![name], false),
                            None => (vec![], true),
                        };
                        out.push(RawImport::new(p, syms, glob, EdgeKind::Use, line_of(n)));
                    }
                }
            }
        }
        _ => {}
    }
}

/// Rewrite a `use` path written `depth` inline modules deep so it is relative to
/// the file's own module. `super::x` one level down is `self::x`; a path that
/// never leaves the inline modules names nothing on disk.
fn rebase_inline(spec: &str, depth: usize) -> Option<String> {
    if depth == 0 {
        return Some(spec.to_string());
    }
    let mut segs: Vec<&str> = spec.split("::").collect();
    if segs.first() == Some(&"crate") {
        return Some(spec.to_string());
    }
    let mut d = depth;
    while d > 0 && segs.first() == Some(&"super") {
        segs.remove(0);
        d -= 1;
    }
    if d > 0 {
        return None; // still inside the inline module(s)
    }
    match segs.first() {
        Some(&"super") | Some(&"self") => {}
        _ => segs.insert(0, "self"),
    }
    Some(segs.join("::"))
}

/// Flatten a `use` tree: `crate::{a::X, b::{self, Y}}` → `crate::a::X`, `crate::b`, `crate::b::Y`,
/// each with the leaf name it imports (`None` for a wildcard).
fn use_paths(n: Node, src: &[u8], prefix: &str, out: &mut Vec<(String, Option<String>)>) {
    let with_prefix = |p: &str| if prefix.is_empty() { p.to_string() } else { format!("{prefix}::{p}") };
    let leaf = |p: &str| p.rsplit("::").next().unwrap_or(p).to_string();
    match n.kind() {
        "scoped_use_list" => {
            let inner = n.child_by_field_name("path").map(|p| with_prefix(t(p, src))).unwrap_or_else(|| prefix.to_string());
            if let Some(list) = n.child_by_field_name("list") {
                use_paths(list, src, &inner, out);
            }
        }
        "use_list" => {
            let mut c = n.walk();
            for ch in n.named_children(&mut c) {
                use_paths(ch, src, prefix, out);
            }
        }
        "use_as_clause" => {
            if let Some(p) = n.child_by_field_name("path") {
                use_paths(p, src, prefix, out);
            }
        }
        "use_wildcard" => {
            // `path::*` depends on the module at `path`; a bare `*` on the prefix.
            let mut c = n.walk();
            match n.named_children(&mut c).next() {
                Some(p) => out.push((with_prefix(t(p, src)), None)),
                None if !prefix.is_empty() => out.push((prefix.to_string(), None)),
                None => {}
            }
        }
        // `self` inside a list names the prefix itself.
        "self" if !prefix.is_empty() => out.push((prefix.to_string(), Some(leaf(prefix)))),
        _ => {
            let p = with_prefix(t(n, src));
            let l = leaf(&p);
            out.push((p, Some(l)));
        }
    }
}

fn unquote(s: &str) -> String {
    s.trim_matches(|c| c == '"' || c == '\'' || c == '`').to_string()
}

// ---------- resolution ----------

fn resolve(from: &str, lang: Language, imp: &RawImport, known: &HashSet<&str>, cfg: &Cfg) -> Option<Vec<String>> {
    match lang {
        Language::Python => resolve_python(from, imp, known),
        Language::Rust => resolve_rust(from, imp, known),
        _ => resolve_js(from, imp, known, cfg).map(|p| vec![p]),
    }
}

fn join(dir: &str, rest: &str) -> String {
    let mut parts: Vec<&str> = if dir.is_empty() { vec![] } else { dir.split('/').collect() };
    for seg in rest.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn ancestors(dir: &str) -> Vec<String> {
    let mut v = Vec::new();
    let mut d = dir.to_string();
    loop {
        v.push(d.clone());
        if d.is_empty() {
            break;
        }
        d = dir_of(&d).to_string();
    }
    v
}

fn py_candidates(base: &str, module: &str) -> [String; 2] {
    let rel = module.replace('.', "/");
    let p = join(base, &rel);
    [format!("{p}.py"), format!("{p}/__init__.py")]
}

fn resolve_python(from: &str, imp: &RawImport, known: &HashSet<&str>) -> Option<Vec<String>> {
    let dir = dir_of(from);
    let roots: Vec<String> = if imp.level > 0 {
        let mut d = dir.to_string();
        for _ in 1..imp.level {
            d = dir_of(&d).to_string();
        }
        vec![d]
    } else {
        ancestors(dir)
    };
    let mut hits = Vec::new();
    for root in &roots {
        let mut found = false;
        if imp.spec.is_empty() {
            // `from . import x` – x are submodules of the package at root.
            for nm in &imp.names {
                for c in py_candidates(root, nm) {
                    if known.contains(c.as_str()) {
                        hits.push(c);
                        found = true;
                        break;
                    }
                }
            }
        } else {
            for c in py_candidates(root, &imp.spec) {
                if known.contains(c.as_str()) {
                    hits.push(c);
                    found = true;
                    break;
                }
            }
            if found {
                for nm in &imp.names {
                    let sub = format!("{}.{}", imp.spec, nm);
                    for c in py_candidates(root, &sub) {
                        if known.contains(c.as_str()) {
                            hits.push(c);
                            break;
                        }
                    }
                }
            }
        }
        if found {
            return Some(hits);
        }
    }
    None
}

const JS_EXTS: &[&str] = &[".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts"];

fn resolve_js(from: &str, imp: &RawImport, known: &HashSet<&str>, cfg: &Cfg) -> Option<String> {
    let dir = dir_of(from);
    let spec = imp.spec.as_str();
    let alias = cfg.js_aliases.iter().find_map(|(pre, target)| spec.strip_prefix(pre.as_str()).map(|rest| (target, rest)));
    let bases: Vec<String> = if spec.starts_with("./") || spec.starts_with("../") || spec == "." || spec == ".." {
        vec![join(dir, spec)]
    } else if let Some((target, rest)) = alias {
        // `@/x` -> `<nearest ancestor>/<target>/x`.
        ancestors(dir).into_iter().map(|a| join(&join(&a, target), rest)).collect()
    } else {
        return None;
    };
    for base in bases {
        if known.contains(base.as_str()) {
            return Some(base);
        }
        // `./x.js` written for ESM output usually means `./x.ts` in source.
        let stem = base.strip_suffix(".js").or_else(|| base.strip_suffix(".jsx")).unwrap_or(&base);
        for ext in JS_EXTS {
            let c = format!("{stem}{ext}");
            if known.contains(c.as_str()) {
                return Some(c);
            }
        }
        for ext in JS_EXTS {
            let c = format!("{base}/index{ext}");
            if known.contains(c.as_str()) {
                return Some(c);
            }
        }
    }
    None
}

fn resolve_rust(from: &str, imp: &RawImport, known: &HashSet<&str>) -> Option<Vec<String>> {
    let dir = dir_of(from);
    let file = from.rsplit('/').next().unwrap_or(from);
    // Where this file's child modules live.
    let child_dir = if matches!(file, "main.rs" | "lib.rs" | "mod.rs") {
        dir.to_string()
    } else {
        join(dir, file.trim_end_matches(".rs"))
    };
    if let Some(name) = imp.spec.strip_prefix("mod:") {
        for c in [format!("{}/{name}.rs", child_dir), format!("{}/{name}/mod.rs", child_dir)] {
            let c = c.trim_start_matches('/').to_string();
            if known.contains(c.as_str()) {
                return Some(vec![c]);
            }
        }
        return None;
    }
    let (head, path) = imp.spec.split_once("::").unwrap_or((imp.spec.as_str(), ""));
    let base = match head {
        "crate" => {
            // Crate root = nearest ancestor containing lib.rs or main.rs.
            ancestors(dir).into_iter().find(|a| {
                known.contains(join(a, "lib.rs").as_str()) || known.contains(join(a, "main.rs").as_str())
            })?
        }
        "super" => dir_of(&child_dir).to_string(),
        _ => child_dir.clone(),
    };
    let segs: Vec<&str> = path.split("::").filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        // `use super::*` / `use crate::*`: the module file at `base` itself.
        let stem = format!("{base}.rs");
        for c in [join(&base, "mod.rs"), join(&base, "lib.rs"), join(&base, "main.rs"), stem] {
            if known.contains(c.as_str()) {
                return Some(vec![c]);
            }
        }
        return None;
    }
    // Longest prefix that names a file wins.
    for n in (1..=segs.len()).rev() {
        let rel = segs[..n].join("/");
        for c in [join(&base, &format!("{rel}.rs")), join(&base, &format!("{rel}/mod.rs"))] {
            if known.contains(c.as_str()) {
                return Some(vec![c]);
            }
        }
    }
    None
}


#[cfg(test)]
mod tests {
    use super::*;

    fn sf(path: &str, content: &str) -> SourceFile {
        let lang = Language::from_path(std::path::Path::new(path)).unwrap();
        let kind = crate::discover::classify(path, content, 1, &crate::config::Discover::default());
        SourceFile { path: path.into(), lang, kind, lines: 1, bytes: content.len(), content: content.into() }
    }

    #[test]
    fn python_cycle_and_relative_imports() {
        let files = vec![
            sf("backend/engine/__init__.py", ""),
            sf("backend/engine/a.py", "from engine import b\nfrom .c import thing\n"),
            sf("backend/engine/b.py", "import engine.a\n"),
            sf("backend/engine/c.py", "from . import a\n"),
            sf("backend/tests/test_a.py", "from engine.a import x\n"),
        ];
        let g = build(&files, &Cfg::default());
        assert_eq!(g.file_cycles.len(), 1);
        assert_eq!(g.file_cycles[0].members, vec!["backend/engine/a.py", "backend/engine/b.py", "backend/engine/c.py"]);
        let a = &g.files["backend/engine/a.py"];
        assert_eq!(a.test_refs, 1);
        assert_eq!(a.fan_in, 2); // b and c
        assert!(g.dir_cycles.is_empty());
    }

    #[test]
    fn js_alias_from_config() {
        let files = vec![
            sf("web/app/a.ts", "import x from '#lib/x'\n"),
            sf("web/lib/x.ts", ""),
        ];
        assert_eq!(build(&files, &Cfg::default()).files["web/app/a.ts"].external, 1);
        let mut cfg = Cfg::default();
        cfg.js_aliases.insert("#lib/".into(), "lib".into());
        assert_eq!(build(&files, &cfg).files["web/app/a.ts"].fan_out, 1);
    }

    #[test]
    fn ts_resolution_and_dir_cycle() {
        let files = vec![
            sf("src/a/x.ts", "import { y } from '../b/y'\nexport * from './z.js'\n"),
            sf("src/a/z.ts", ""),
            sf("src/b/y.tsx", "import x from '@/a/x'\nimport React from 'react'\n"),
            sf("src/b/index.ts", "export const q = require('./y')\n"),
        ];
        let g = build(&files, &Cfg::default());
        assert_eq!(g.files["src/a/x.ts"].fan_out, 2);
        assert_eq!(g.files["src/b/y.tsx"].external, 1);
        assert_eq!(g.file_cycles.len(), 1);
        assert_eq!(g.dir_cycles.len(), 1);
        assert_eq!(g.dir_cycles[0].members, vec!["src/a", "src/b"]);
        assert!(g.connected("src/b/index.ts", "src/b/y.tsx"));
    }

    #[test]
    fn rust_grouped_use_and_wildcards() {
        let files = vec![
            sf("src/main.rs", "mod deps;\nmod lang;\nmod util;\n"),
            sf("src/deps/mod.rs", "use crate::{lang::Language, util};\n"),
            sf("src/lang/mod.rs", "use crate::deps::{self, Cycle as C};\n"),
            sf("src/util.rs", "use super::*;\nuse self::inner::x;\n"),
        ];
        let g = build(&files, &Cfg::default());
        assert_eq!(g.files["src/deps/mod.rs"].fan_out, 2);
        assert_eq!(g.files["src/lang/mod.rs"].fan_out, 1);
        assert!(g.connected("src/util.rs", "src/main.rs"));
        assert_eq!(g.file_cycles.len(), 1);
    }

    #[test]
    fn inline_test_module_super_is_the_file_itself() {
        let files = vec![
            sf("src/main.rs", "mod a;\nmod b;\n"),
            sf("src/a.rs", "pub fn f() {}\n#[cfg(test)]\nmod tests {\n    use super::*;\n    use super::super::b::g;\n    mod deeper { use super::super::f; }\n}\n"),
            sf("src/b.rs", "pub fn g() {}\n"),
        ];
        let g = build(&files, &Cfg::default());
        assert_eq!(g.files["src/a.rs"].fan_out, 1, "{:?}", g.files["src/a.rs"]);
        assert!(g.connected("src/a.rs", "src/b.rs"));
        assert!(g.edge("src/a.rs", "src/main.rs").is_none(), "{:?}", g.edge_map.keys());
    }

    #[test]
    fn vendored_code_is_not_in_the_graph() {
        let files = vec![
            sf("vendor/lib/a.js", "import {b} from './b'\n"),
            sf("vendor/lib/b.js", "import {a} from './a'\n"),
            sf("src/x.ts", "import {a} from '../vendor/lib/a'\n"),
        ];
        let g = build(&files, &Cfg::default());
        assert!(g.file_cycles.is_empty());
        assert!(!g.files.contains_key("vendor/lib/a.js"));
        assert_eq!(g.files["src/x.ts"].external, 1);
    }

    #[test]
    fn rust_mod_and_crate_paths() {
        let files = vec![
            sf("src/main.rs", "mod deps;\nmod lang;\n"),
            sf("src/deps/mod.rs", "use crate::lang::Language;\n"),
            sf("src/lang/mod.rs", "use super::deps::Cycle;\n"),
        ];
        let g = build(&files, &Cfg::default());
        assert_eq!(g.files["src/main.rs"].fan_out, 2);
        assert!(g.connected("src/deps/mod.rs", "src/lang/mod.rs"));
        assert_eq!(g.file_cycles.len(), 1);
    }

    #[test]
    fn rust_edges_carry_kind_and_distinct_symbols() {
        let files = vec![
            sf("src/main.rs", "mod a;\nmod e;\nuse crate::a::Top;\n"),
            sf("src/a.rs", "use crate::e::{B, C};\nuse crate::e::D;\nuse crate::e::B;\nuse crate::f::*;\n"),
            sf("src/e.rs", ""),
            sf("src/f.rs", ""),
        ];
        let g = build(&files, &Cfg::default());
        let m = g.edge("src/main.rs", "src/a.rs").unwrap();
        assert_eq!((m.kind, m.symbols, m.line), (EdgeKind::Mod, 2, 1), "{m:?}");
        assert_eq!(m.names, vec!["Top", "a"]);
        let ae = g.edge("src/a.rs", "src/e.rs").unwrap();
        assert_eq!((ae.kind, ae.symbols, ae.glob, ae.line), (EdgeKind::Use, 3, false, 1));
        assert_eq!(ae.names, vec!["B", "C", "D"]);
        let af = g.edge("src/a.rs", "src/f.rs").unwrap();
        assert_eq!((af.kind, af.symbols, af.glob, af.line), (EdgeKind::Use, 20, true, 4));
        assert_eq!(symbol_names(&af.names, af.glob), "*");
    }

    #[test]
    fn ts_type_only_imports_are_tagged_and_python_counts_names() {
        let files = vec![
            sf("src/x.ts", "import type { A, B } from './y'\nimport { type C, D, E as F } from './z'\nimport * as ns from './w'\nexport type { H } from './v'\nimport './side'\nexport * from './star'\n"),
            sf("src/star.ts", ""),
            sf("src/y.ts", "import { X } from './x'\n"),
            sf("src/z.ts", ""),
            sf("src/w.ts", ""),
            sf("src/v.ts", ""),
            sf("src/side.ts", ""),
            sf("pkg/__init__.py", ""),
            sf("pkg/p.py", "from pkg.q import a, b as c\nfrom pkg.r import *\nimport pkg.s\n"),
            sf("pkg/q.py", ""),
            sf("pkg/r.py", ""),
            sf("pkg/s.py", ""),
        ];
        let g = build(&files, &Cfg::default());
        let e = |a: &str, b: &str| g.edge(a, b).unwrap();
        assert_eq!((e("src/x.ts", "src/y.ts").kind, e("src/x.ts", "src/y.ts").symbols), (EdgeKind::TypeOnly, 2));
        let z = e("src/x.ts", "src/z.ts");
        assert_eq!((z.kind, z.symbols, z.names.clone()), (EdgeKind::Use, 2, vec!["D".to_string(), "E".to_string()]));
        assert!(e("src/x.ts", "src/w.ts").glob && e("src/x.ts", "src/w.ts").symbols == 20);
        assert_eq!(e("src/x.ts", "src/v.ts").kind, EdgeKind::TypeOnly);
        assert_eq!((e("src/x.ts", "src/side.ts").symbols, e("src/x.ts", "src/side.ts").names.clone()), (1, vec!["./side".to_string()]));
        let star = e("src/x.ts", "src/star.ts");
        assert_eq!((star.kind, star.glob, star.symbols, star.names.is_empty()), (EdgeKind::Use, true, 20, true), "{star:?}");
        assert_eq!((e("pkg/p.py", "pkg/q.py").symbols, e("pkg/p.py", "pkg/q.py").names.clone()), (2, vec!["a".to_string(), "b".to_string()]));
        assert!(e("pkg/p.py", "pkg/r.py").glob);
        assert_eq!(e("pkg/p.py", "pkg/s.py").symbols, 1);
        // x <-> y is a cycle only through a type-only import: no runtime cycle to cut.
        let mut cfg = Cfg { min_cycle_size_to_cut: 2, ..Cfg::default() };
        let g = build(&files, &cfg);
        let c = g.file_cycles.iter().find(|c| c.members.contains(&"src/x.ts".to_string())).unwrap();
        let cuts = c.cuts.as_ref().unwrap();
        assert_eq!((cuts.type_only_edges, cuts.base, cuts.single.is_none(), cuts.no_single_break), (1, 1, true, false), "{cuts:?}");
        assert_eq!(c.headline(), "2-file cycle in src: 1 type-only import ignored, largest runtime cycle 1; no runtime cycle");
        cfg.ignore_type_only_imports = false;
        let g = build(&files, &cfg);
        let c = g.file_cycles.iter().find(|c| c.members.contains(&"src/x.ts".to_string())).unwrap();
        let s = c.cuts.as_ref().unwrap().single.as_ref().unwrap();
        assert_eq!((s.edge.from.as_str(), s.largest_after), ("src/y.ts", 1));
    }

    fn cycle_files() -> Vec<SourceFile> {
        vec![
            sf("src/lib.rs", "mod a;\nmod b;\nmod c;\n"),
            sf("src/a.rs", "mod a2;\n"),
            sf("src/a/a2.rs", "use crate::b::B;\n"),
            sf("src/b/mod.rs", "use crate::a::A;\nuse crate::c::C;\n"),
            sf("src/c.rs", "use crate::a::A;\nuse crate::b::B2;\n"),
        ]
    }

    /// Four files each importing the other three: no single edge shrinks the cycle.
    fn dense_files() -> Vec<SourceFile> {
        let names = ["a", "b", "c", "d"];
        let mut v = vec![sf("src/lib.rs", "mod a;\nmod b;\nmod c;\nmod d;\n")];
        for n in names {
            let body: String = names.iter().filter(|o| **o != n).map(|o| format!("use crate::{o}::{};\n", o.to_uppercase())).collect();
            v.push(sf(&format!("src/{n}.rs"), &body));
        }
        v
    }

    #[test]
    fn cut_search_never_proposes_a_mod_edge_and_reports_hub_and_set() {
        let cfg = Cfg::default();
        let g = build(&cycle_files(), &cfg);
        assert_eq!(g.file_cycles.len(), 1);
        let c = &g.file_cycles[0];
        assert_eq!((c.members.len(), c.dir.as_str()), (4, "src"));
        let cuts = c.cuts.as_ref().unwrap();
        assert_eq!((cuts.internal_edges, cuts.mod_edges, cuts.base), (6, 1, 4));
        let s = cuts.single.as_ref().unwrap();
        assert_eq!((s.edge.from.as_str(), s.edge.to.as_str(), s.edge.kind, s.largest_after), ("src/a/a2.rs", "src/b/mod.rs", EdgeKind::Use, 2));
        let h = cuts.hub.as_ref().unwrap();
        assert_eq!((h.member.as_str(), h.imports, h.symbols, h.glob, h.largest_after), ("src/b/mod.rs", 2, 2, false, 1));
        assert!(!cuts.no_single_break);
        assert_eq!(cuts.cut_set.iter().map(|e| (e.edge.from.as_str(), e.edge.to.as_str(), e.largest_after)).collect::<Vec<_>>(), vec![("src/a/a2.rs", "src/b/mod.rs", 2)]);
        // `mod.rs` names nothing on its own: the short name carries its directory.
        assert_eq!(c.headline(), "4-file cycle in src: cheapest cut src/a/a2.rs -> src/b/mod.rs imports 1 symbol (B, line 1) -> largest remaining cycle 2; hub cut: drop b/mod.rs's 2 imports (2 symbols) -> 1");
        assert_eq!(c.cut_set_line().as_deref(), Some("cut set of 1 import dissolves it: src/a/a2.rs -> src/b/mod.rs (B)"));
        assert_eq!(c.cut_clause("src/b/mod.rs").as_deref(), Some("cut: a2.rs -> b/mod.rs, B"));
        assert!(c.cut_clause("src/a.rs").is_none());
        assert_eq!((short_name("src/dom/index.ts"), short_name("pkg/__init__.py"), short_name("index.ts"), short_name("src/x.rs"), short_name("mod.rs")), ("dom/index.ts", "pkg/__init__.py", "index.ts", "x.rs", "mod.rs"));
        // Without the hub, and with the hub not beating the single, it is absent.
        let cfg = Cfg { report_hub_cut: false, ..Cfg::default() };
        assert!(build(&cycle_files(), &cfg).file_cycles[0].cuts.as_ref().unwrap().hub.is_none());
    }

    #[test]
    fn search_knobs_bound_the_edges_tried_and_price_globs() {
        // a2 -> b costs 2 now, so it sorts after the four 1-symbol edges; with one edge tried the
        // search stops at the first of those, which leaves the whole cycle.
        let mut files = cycle_files();
        files[2] = sf("src/a/a2.rs", "use crate::b::{B, B2};\n");
        let g = build(&files, &Cfg::default());
        let s = g.file_cycles[0].cuts.as_ref().unwrap().single.as_ref().unwrap();
        assert_eq!((s.edge.from.as_str(), s.edge.symbols, s.largest_after), ("src/a/a2.rs", 2, 2));
        let g = build(&files, &Cfg { max_edges_tried: 1, ..Cfg::default() });
        let s = g.file_cycles[0].cuts.as_ref().unwrap().single.as_ref().unwrap();
        assert_eq!((s.edge.from.as_str(), s.edge.to.as_str(), s.largest_after), ("src/b/mod.rs", "src/a.rs", 4));
        // A wildcard costs the knob, and a hub that drops one says `>= N symbols`.
        files[2] = sf("src/a/a2.rs", "use crate::b::B;\n");
        files[3] = sf("src/b/mod.rs", "use crate::a::*;\nuse crate::c::C;\n");
        let g = build(&files, &Cfg { glob_import_symbol_cost: 3, ..Cfg::default() });
        let e = g.edge("src/b/mod.rs", "src/a.rs").unwrap();
        assert_eq!((e.glob, e.symbols), (true, 3));
        let c = &g.file_cycles[0];
        let cuts = c.cuts.as_ref().unwrap();
        assert_eq!(cuts.single.as_ref().unwrap().edge.from.as_str(), "src/a/a2.rs");
        let h = cuts.hub.as_ref().unwrap();
        assert_eq!((h.member.as_str(), h.imports, h.glob, h.symbols, h.largest_after), ("src/b/mod.rs", 2, true, 4, 1));
        assert!(c.headline().ends_with("hub cut: drop b/mod.rs's 2 imports (>= 4 symbols) -> 1"), "{}", c.headline());
        // The share knob decides when a single cut is called no break at all.
        let g = build(&dense_files(), &Cfg { no_single_cut_share: 1.01, ..Cfg::default() });
        assert!(!g.file_cycles[0].cuts.as_ref().unwrap().no_single_break);
    }

    #[test]
    fn dense_cycle_says_no_single_import_breaks_it_and_caps_the_cut_set() {
        let g = build(&dense_files(), &Cfg::default());
        let c = &g.file_cycles[0];
        let cuts = c.cuts.as_ref().unwrap();
        assert_eq!((cuts.internal_edges, cuts.mod_edges), (12, 0));
        let s = cuts.single.as_ref().unwrap();
        assert_eq!((s.edge.from.as_str(), s.edge.to.as_str(), s.largest_after), ("src/a.rs", "src/b.rs", 4));
        assert!(cuts.no_single_break);
        let h = cuts.hub.as_ref().unwrap();
        assert_eq!((h.member.as_str(), h.imports, h.symbols, h.largest_after), ("src/a.rs", 3, 3, 3));
        // The greedy set drops a.rs's three imports (largest 3 < min size 4); a cap of two hides it.
        assert_eq!(cuts.cut_set.iter().map(|e| (e.edge.to.as_str(), e.largest_after)).collect::<Vec<_>>(), vec![("src/b.rs", 4), ("src/c.rs", 4), ("src/d.rs", 3)]);
        assert!(cuts.cut_set.iter().all(|e| e.edge.kind != EdgeKind::Mod));
        assert!(c.headline().ends_with("-> largest remaining cycle 4; hub cut: drop a.rs's 3 imports (3 symbols) -> 3; no single import breaks this cycle"), "{}", c.headline());
        let cfg = Cfg { max_cut_set: 2, ..Cfg::default() };
        let g = build(&dense_files(), &cfg);
        assert!(g.file_cycles[0].cuts.as_ref().unwrap().cut_set.is_empty());
        assert!(g.file_cycles[0].cut_set_line().is_none());
    }

    #[test]
    fn rust_cuts_can_be_switched_off_and_small_cycles_get_no_cuts() {
        let cfg = Cfg { cut_rust_cycles: false, ..Cfg::default() };
        let g = build(&cycle_files(), &cfg);
        assert!(g.file_cycles[0].cuts.is_none());
        assert_eq!(g.file_cycles[0].headline(), "4-file cycle in src");
        assert!(g.file_cycles[0].cut_set_line().is_none() && g.file_cycles[0].cut_clause("src/b/mod.rs").is_none());
        let cfg = Cfg { min_cycle_size_to_cut: 5, ..Cfg::default() };
        assert!(build(&cycle_files(), &cfg).file_cycles[0].cuts.is_none());
    }

    #[test]
    fn common_dir_is_the_deepest_shared_directory() {
        assert_eq!(common_dir(&["src/a.rs".to_string(), "src/cmd/b.rs".to_string()]), "src");
        assert_eq!(common_dir(&["crates/x/src/a.rs".to_string(), "crates/x/src/b/c.rs".to_string()]), "crates/x/src");
        assert_eq!(common_dir(&["a.rs".to_string(), "src/b.rs".to_string()]), ".");
    }
}
