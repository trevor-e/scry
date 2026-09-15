//! The composite: fold every pass into one ranked, explained list.
//!
//! Ranking follows the hotspot idea (Tornhill): the files most likely to hurt
//! are the ones that are both hard to change *and* changed often. Each signal
//! is percentile-normalised within the repo so no thresholds need tuning per
//! language, and every ranked file carries the reasons it ranked, in words.

use crate::clones::{CloneKind, CloneReport, ClonePair, Loc, TableRef};
use crate::clumps::{self, Clump, ClumpsReport};
use crate::config::{History as HistoryCfg, Plan as PlanCfg, Report as Cfg, Tests as TestsCfg, Weights};
use crate::dead::{self, DeadReport, FileDead, Shape, SymbolReport};
use crate::declared::{self, DeclaredReport, Feature, Misplaced, Orphan, UnreadKnob};
use crate::deps::{Cycle, DepGraph};
use crate::helpers::{self, Family, HelpersReport, Inlined};
use crate::discover::{FileKind, SourceFile};
use crate::history::{CoChange, History};
use crate::mentions::{MentionIndex, Symbol};
use crate::metrics::{FileMetrics, FunctionMetrics};
use crate::plan::{self, Step};
use crate::regions::{RegionKind, TestRegion};
use crate::strings::{self, NearPair, StringsReport};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Clone, Serialize)]
pub struct Signals {
    /// Whole file. The size signal uses `lines - inline_test_lines`.
    pub lines: usize,
    /// Lines spanned by inline test regions (Rust `#[cfg(test)]`).
    pub inline_test_lines: usize,
    pub commits: usize,
    pub fix_commits: usize,
    pub authors: usize,
    pub functions: usize,
    pub max_cognitive: u32,
    pub total_cognitive: u32,
    pub complex_functions: usize,
    pub fan_in: usize,
    pub fan_out: usize,
    pub in_cycle: bool,
    /// `logic_clone_lines + table_clone_lines`.
    pub clone_lines: usize,
    pub logic_clone_lines: usize,
    /// Cloned lines covered only by table pairs (uniform sibling entries, not duplicated logic).
    pub table_clone_lines: usize,
    /// `(logic + [clones].table_weight x table) / (lines - inline_test_lines)`.
    pub clone_ratio: f64,
    /// Test units (functions in Test files, tests inside inline `#[cfg(test)]` regions) whose
    /// body names one of the file's symbols as an identifier token; at least 1 when a test file
    /// imports the file or a same-stem test sits in its tree. 0 turns the no-tests multiplier on.
    pub test_units: usize,
    /// Public symbols the mention index holds for the file (names of `[tests].min_name_len`+).
    pub public_symbols: usize,
    /// Lines of exported symbols with no production reference outside the file (dead,
    /// test-only, in-file-only; see `dead`)…
    pub dead_lines: usize,
    /// …over `lines - inline_test_lines`. Percentile-ranked, weight `[report.weights] dead`.
    pub dead_ratio: f64,
    /// This file's top-level definitions in a reported same-name family (see `helpers`)…
    pub helper_copies: usize,
    /// …and inline copies of some helper's body in it. Their sum is percentile-ranked with
    /// weight `[helpers].weight`.
    pub inlined_idioms: usize,
    /// Literal sites in an exact message family or a config-literal family (see `strings`).
    /// Percentile-ranked, weight `[report.weights] strings`.
    pub family_literals: usize,
    /// Functions of the file in a reported parameter clump (see `clumps`). Percentile-ranked,
    /// weight `[clumps].weight`.
    pub clump_members: usize,
    /// Config knobs declared in this file that no code reads (see `declared`). Percentile-ranked,
    /// weight `[declared].weight`.
    pub unread_knobs: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Hotspot {
    pub path: String,
    pub score: f64,
    pub signals: Signals,
    pub reasons: Vec<String>,
    pub worst_functions: Vec<FunctionMetrics>,
    pub test_regions: Vec<TestRegion>,
    /// `1026 in #[cfg(test)] mod at 1283-2308`, printed after the line count when the inline
    /// test ratio is at or above `[tests].report_inline_ratio_above`.
    pub inline_test_note: Option<String>,
    /// Public symbols named by no test unit, in line order (the reason lists the first few).
    pub unmentioned: Vec<Symbol>,
    /// Refactor steps in `[plan].kind_priority` order, at most `[plan].max_steps`…
    pub plan: Vec<Step>,
    /// …and how many more there were.
    pub plan_more: usize,
    /// Every checked exported symbol of the file with its category and the five counters.
    pub dead_symbols: Vec<SymbolReport>,
    /// Never-read fields and never-constructed variants (Rust).
    pub dead_shapes: Vec<Shape>,
    /// Names of the same-name families this file defines a copy in (see `helpers`).
    pub helper_copies: Vec<String>,
    /// `(helper name, line)` of every helper body inlined in this file.
    pub inlined_idioms: Vec<(String, usize)>,
    /// `(family text, line)` of every literal of this file in a repeated-literal family.
    pub repeated_literals: Vec<(String, usize)>,
    /// `(tuple text, line)` of every function of this file in a parameter clump.
    pub clumps: Vec<(String, usize)>,
    /// `(section.knob, line)` of every config knob this file declares that no code reads.
    pub unread_knobs: Vec<(String, usize)>,
}

/// A co-change pair with no import between its members. In `hidden_coupling` when nothing
/// explains it; in `explained_coupling` when a shared import changed in the same commits.
#[derive(Debug, Clone, Serialize)]
pub struct HiddenCoupling {
    pub a: String,
    pub b: String,
    /// Co-commits including directory sweeps.
    pub together: usize,
    /// Co-commits without sweeps: the count the pair was judged on and the one printed.
    pub together_nonsweep: usize,
    pub strength: f64,
    /// Times more often than chance the pair ships together (non-sweep counts).
    pub lift: f64,
    /// A file both members import that changed in at least `[history].explained_min_share` of
    /// their non-sweep co-commits: the pair is shotgun surgery on it, not hidden coupling.
    pub explained_by: Option<String>,
    /// Non-sweep co-commits in which `explained_by` also changed.
    pub explained_commits: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DirSummary {
    pub dir: String,
    pub files: usize,
    pub lines: usize,
    pub total_cognitive: u32,
    pub largest_file: String,
    pub largest_lines: usize,
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub root: String,
    pub source_files: usize,
    pub test_files: usize,
    pub source_lines: usize,
    pub functions: usize,
    pub complex_functions: usize,
    pub history_window: Option<String>,
    pub commits_scanned: usize,
    /// Directory-sweep commits among `commits_scanned`: churn, but excluded from pair counts.
    pub sweep_commits: usize,
    pub cognitive_hard: u32,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub summary: Summary,
    pub hotspots: Vec<Hotspot>,
    pub dir_cycles: Vec<Cycle>,
    pub file_cycles: Vec<Cycle>,
    pub hidden_coupling: Vec<HiddenCoupling>,
    /// Co-change pairs with no import between them that a shared import explains (`explained_by`).
    pub explained_coupling: Vec<HiddenCoupling>,
    /// `5 directory-sweep commits (>= 50% of src/cmd) excluded from pair counts; still counted
    /// as churn`, printed under HIDDEN COUPLING when the window had sweeps.
    pub sweep_note: Option<String>,
    /// Top pairs; only the `logic` ones when tables are listed separately.
    pub clones: Vec<ClonePair>,
    /// Top `table` pairs when `[clones].list_tables_separately`; empty otherwise (they sit in `clones`).
    pub tables: Vec<ClonePair>,
    pub directories: Vec<DirSummary>,
    /// Repo totals and the top files by dead lines (see `dead`).
    pub dead: DeadSurface,
    /// Same-name helper families and inlined helper bodies (see `helpers`).
    pub helpers: HelpersSection,
    /// Exact message families, config-literal families and near-duplicate pairs (see `strings`).
    pub strings: StringsSection,
    /// Parameter tuples recurring across functions, with their unused slots (see `clumps`).
    pub clumps: ClumpsSection,
    /// Orphaned dependencies, dead feature flags and unread config knobs (see `declared`).
    pub declared: DeclaredSection,
}

/// The DECLARED section: totals, notes and the top findings of each kind.
#[derive(Debug, Default, Serialize)]
pub struct DeclaredSection {
    pub totals: declared::Totals,
    pub notes: Vec<String>,
    pub orphans: Vec<Orphan>,
    pub misplaced: Vec<Misplaced>,
    pub dead_features: Vec<Feature>,
    pub noop_features: Vec<Feature>,
    pub unread_knobs: Vec<UnreadKnob>,
}

/// The CLUMPS section: totals, notes and the top clumps.
#[derive(Debug, Default, Serialize)]
pub struct ClumpsSection {
    pub totals: clumps::Totals,
    pub notes: Vec<String>,
    pub clumps: Vec<Clump>,
    /// `[clumps].unused_prefix`, named in the totals line.
    pub unused_prefix: String,
}

/// The STRINGS section: totals, notes, the top families of each class and the top near pairs.
#[derive(Debug, Default, Serialize)]
pub struct StringsSection {
    pub totals: strings::Totals,
    pub notes: Vec<String>,
    pub families: Vec<strings::Family>,
    pub config: Vec<strings::Family>,
    pub near: Vec<NearPair>,
}

/// The HELPERS section: totals, notes, the top families and the top inlined helpers.
#[derive(Debug, Default, Serialize)]
pub struct HelpersSection {
    pub totals: helpers::Totals,
    pub notes: Vec<String>,
    pub families: Vec<Family>,
    pub inlined: Vec<Inlined>,
}

/// The DEAD SURFACE section: totals, notes and the files with the most dead lines.
#[derive(Debug, Default, Serialize)]
pub struct DeadSurface {
    pub totals: dead::Totals,
    pub modes: Vec<String>,
    pub notes: Vec<String>,
    pub files: Vec<DeadFileRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeadFileRow {
    pub path: String,
    pub pub_items: usize,
    pub dead_count: usize,
    pub test_only_count: usize,
    pub overexported_count: usize,
    pub dead_lines: usize,
    pub dead_ratio: f64,
}

pub struct Inputs<'a> {
    pub root: String,
    pub files: &'a [SourceFile],
    pub history: Option<&'a History>,
    pub file_metrics: &'a [FileMetrics],
    pub functions: &'a [FunctionMetrics],
    pub deps: &'a DepGraph,
    pub clones: &'a CloneReport,
    pub mentions: &'a MentionIndex,
    pub dead: &'a DeadReport,
    pub helpers: &'a HelpersReport,
    /// `[helpers].weight`.
    pub helpers_weight: f64,
    pub strings: &'a StringsReport,
    pub clumps: &'a ClumpsReport,
    /// `[clumps].weight`.
    pub clumps_weight: f64,
    /// `[clumps].unused_prefix`, for the totals line.
    pub clumps_prefix: &'a str,
    pub declared: &'a DeclaredReport,
    /// `[declared].weight`.
    pub declared_weight: f64,
    pub cognitive_hard: u32,
    pub tests: &'a TestsCfg,
    /// `[clones].list_tables_separately`.
    pub list_tables_separately: bool,
    /// `explained_min_share` and `sweep_fraction` (for the sweep note).
    pub history_cfg: &'a HistoryCfg,
    /// `[deps].dedupe_cycle_reason`: the cycle is described once under CYCLES and members get
    /// `in the 15-file src cycle (cut: a -> b, Sym)` instead of the count on every one.
    pub dedupe_cycle_reason: bool,
    pub plan: &'a PlanCfg,
}

/// The shared import that best explains a pair: the file both import that changed in the most of
/// the pair's non-sweep co-commits, when that is at least `explained_min_share` of them.
fn explained_by(c: &CoChange, hist: &History, deps: &DepGraph, min_share: f64) -> Option<(String, usize)> {
    let shared = deps.shared_imports(&c.a, &c.b);
    if shared.is_empty() {
        return None;
    }
    let mut counts = vec![0usize; shared.len()];
    let mut co = 0;
    for files in hist.co_commits.iter().filter(|f| f.contains(&c.a) && f.contains(&c.b)) {
        co += 1;
        for (i, f) in shared.iter().enumerate() {
            counts[i] += usize::from(files.contains(f));
        }
    }
    let (i, best) = counts.iter().enumerate().max_by(|x, y| x.1.cmp(y.1).then(y.0.cmp(&x.0)))?;
    (co > 0 && *best as f64 / co as f64 >= min_share).then(|| (shared[i].clone(), *best))
}

/// `5 directory-sweep commits (>= 50% of src/cmd) excluded from pair counts; still counted as churn`.
fn sweep_note(hist: &History, fraction: f64) -> Option<String> {
    if hist.sweep_commits == 0 {
        return None;
    }
    let mut by_dir: BTreeMap<&str, usize> = BTreeMap::new();
    for s in &hist.sweeps {
        *by_dir.entry(if s.dir.is_empty() { "." } else { s.dir.as_str() }).or_default() += 1;
    }
    let mut dirs: Vec<(&str, usize)> = by_dir.into_iter().collect();
    dirs.sort_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(y.0)));
    let names: Vec<&str> = dirs.iter().map(|(d, _)| *d).collect();
    let plural = if hist.sweep_commits == 1 { "" } else { "s" };
    Some(format!(
        "{} directory-sweep commit{plural} (>= {:.0}% of {}) excluded from pair counts; still counted as churn",
        hist.sweep_commits, fraction * 100.0, names.join(", ")
    ))
}

/// The clone reason. Plain when every cloned line is logic; otherwise the split in whole
/// percent, naming each table: `17% duplicated lines were 11% registry table (CHECKS, lines
/// 91-174) and 6% logic`. Either way it ends with the file's largest pair, both sides as
/// `symbol (lines)`: `; largest: park (932-962) <-> drop_ticket (1010-1036), 239 tokens`.
fn clone_reason(s: &Signals, tables: &[TableRef], path: &str, largest: Option<&ClonePair>) -> String {
    let largest = largest.map_or_else(String::new, |p| {
        let (mine, other) = if p.a.file == path { (&p.a, &p.b) } else { (&p.b, &p.a) };
        let table = p.kind == CloneKind::Table;
        format!("; largest: {} <-> {}, {} tokens", plan::side_text(mine, path, table), plan::side_text(other, path, table), p.tokens)
    });
    if s.table_clone_lines == 0 {
        return format!("{:.0}% of its lines are duplicated elsewhere ({} lines){largest}", s.clone_ratio * 100.0, s.clone_lines);
    }
    let denom = s.lines.saturating_sub(s.inline_test_lines).max(1) as f64;
    let pct = |n: usize| (100.0 * n as f64 / denom).round() as usize;
    // The parts sum to the headline: rounding each on its own can disagree by one.
    let head = pct(s.table_clone_lines + s.logic_clone_lines);
    let t = pct(s.table_clone_lines);
    let l = head.saturating_sub(t);
    let desc = match tables.first().map(|t| t.container_kind.as_str()) {
        Some(k) if tables.iter().all(|t| t.container_kind == k) => table_desc(k),
        _ => "table",
    };
    let named: Vec<String> = tables
        .iter()
        .map(|t| match &t.symbol {
            Some(sym) => format!("{sym}, lines {}-{}", t.start_line, t.end_line),
            None => format!("lines {}-{}", t.start_line, t.end_line),
        })
        .collect();
    if l == 0 {
        format!("{head}% duplicated lines were all {desc} ({}), no logic{largest}", named.join("; "))
    } else {
        format!("{head}% duplicated lines were {t}% {desc} ({}) and {l}% logic{largest}", named.join("; "))
    }
}

/// What a table container reads as in a reason.
fn table_desc(container_kind: &str) -> &'static str {
    match container_kind {
        "match_block" | "switch_body" => "dispatch table",
        "array_expression" | "array" | "list" | "tuple" => "registry table",
        "enum_variant_list" | "enum_body" => "enum table",
        "object" | "dictionary" | "field_initializer_list" => "map table",
        "field_declaration_list" | "interface_body" => "field table",
        // Import/mod runs under a file root, method runs in an impl body, statement runs.
        _ => "uniform run",
    }
}

/// Percentile rank in [0,1]: share of other values strictly below this one.
pub(crate) fn percentiles<T: PartialOrd + Copy>(values: &[T]) -> Vec<f64> {
    let n = values.len();
    if n < 2 {
        return vec![0.0; n];
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| values[a].partial_cmp(&values[b]).unwrap());
    let mut out = vec![0.0; n];
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j + 1 < n && values[order[j + 1]] == values[order[i]] {
            j += 1;
        }
        let p = i as f64 / (n - 1) as f64;
        for k in i..=j {
            out[order[k]] = p;
        }
        i = j + 1;
    }
    out
}

fn stem(path: &str) -> String {
    let file = path.rsplit('/').next().unwrap_or(path);
    let s = file.split('.').next().unwrap_or(file).to_ascii_lowercase();
    s.trim_start_matches("test_")
        .trim_end_matches("_test")
        .trim_end_matches("_tests")
        .trim_end_matches("tests")
        .to_string()
}

/// Directory a test file speaks for: everything above its first test-directory
/// component. `backend/tests/unit/test_x.py` → `backend`; `src/a/x.test.ts` → `src/a`;
/// `tests/test_x.py` → `` (the whole repo).
fn test_scope<'a>(path: &'a str, test_dirs: &[String]) -> &'a str {
    let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
    let mut end = 0;
    for comp in dir.split('/').filter(|c| !c.is_empty()) {
        if test_dirs.iter().any(|t| t == comp) {
            break;
        }
        end += comp.len() + usize::from(end > 0);
    }
    &dir[..end]
}

/// Test stems with the directory tree they cover: `test_combat.py`,
/// `combat.test.ts`, `BattleScreen.spec.tsx` → `combat`, `battlescreen`.
/// A `test_utils.py` under `b/tests/` says nothing about `a/utils.py`.
fn test_stems(files: &[SourceFile], test_dirs: &[String]) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for f in files.iter().filter(|f| f.kind == FileKind::Test) {
        out.entry(stem(&f.path)).or_default().push(test_scope(&f.path, test_dirs).to_string());
    }
    out
}

fn stem_tested(tstems: &HashMap<String, Vec<String>>, f: &SourceFile) -> bool {
    let dir = f.module_dir();
    tstems.get(&stem(&f.path)).is_some_and(|scopes| {
        scopes.iter().any(|s| s.is_empty() || dir == s || dir.starts_with(&format!("{s}/")))
    })
}

/// `1026 in #[cfg(test)] mod at 1283-2308`: the largest region's kind and range, then `+N more`
/// when there are others (a file of a hundred `#[cfg(test)]` items lists them all in `test_regions[]`).
fn inline_test_note(regions: &[TestRegion], inline_test_lines: usize) -> String {
    let Some(biggest) = regions.iter().max_by_key(|r| r.lines()) else { return format!("{inline_test_lines} in inline tests") };
    let what = match biggest.kind {
        RegionKind::CfgTestMod => "#[cfg(test)] mod",
        RegionKind::TestFn => "#[test] fn",
        RegionKind::CfgTestItem => "#[cfg(test)] item",
    };
    let more = match regions.len() {
        0 | 1 => String::new(),
        n => format!(", +{} more", n - 1),
    };
    format!("{inline_test_lines} in {what} at {}-{}{more}", biggest.start_line, biggest.end_line)
}

/// `4 of 6 public symbols are named by no test: router (lines 204-229), request_ctx (lines 243-257)`,
/// the first `listed` symbols then `+N more`; `unmentioned[]` in `--json` has them all.
fn unmentioned_reason(unmentioned: &[Symbol], public: usize, listed: usize) -> String {
    let named: Vec<String> = unmentioned.iter().take(listed).map(|s| format!("{} (lines {}-{})", s.name, s.start_line, s.end_line)).collect();
    let more = match unmentioned.len().saturating_sub(listed) {
        0 => String::new(),
        n => format!(", +{n} more"),
    };
    format!("{} of {public} public symbols are named by no test: {}{more}", unmentioned.len(), named.join(", "))
}

pub fn build(inp: Inputs, top: usize, cfg: &Cfg, test_dirs: &[String]) -> Report {
    let fm: HashMap<&str, &FileMetrics> = inp.file_metrics.iter().map(|m| (m.path.as_str(), m)).collect();
    let inline_ratio = |f: &SourceFile| {
        let t = fm.get(f.path.as_str()).map_or(0, |m| m.inline_test_lines);
        if f.lines == 0 { 0.0 } else { t as f64 / f.lines as f64 }
    };
    // A Source file that is nearly all inline tests is a test file for ranking purposes.
    let reclassified = |f: &SourceFile| inline_ratio(f) > inp.tests.reclassify_file_above_ratio;
    let source: Vec<&SourceFile> =
        inp.files.iter().filter(|f| f.kind == FileKind::Source && !reclassified(f)).collect();
    let reclassified_files = inp.files.iter().filter(|f| f.kind == FileKind::Source && reclassified(f)).count();
    // Files the plan treats as test code: Test-classified for ranking, though the clone pass saw them.
    let test_like: HashSet<&str> = inp.files.iter().filter(|f| f.kind != FileKind::Source || reclassified(f)).map(|f| f.path.as_str()).collect();
    // A clone run side is test code when its file is, or its start line lies in a test region.
    let in_tests = |l: &Loc| {
        test_like.contains(l.file.as_str())
            || fm.get(l.file.as_str()).is_some_and(|m| m.test_regions.iter().any(|r| r.start_line <= l.start_line && l.start_line <= r.end_line))
    };
    let mut pairs_by_file: HashMap<&str, Vec<&ClonePair>> = HashMap::new();
    for p in &inp.clones.pairs {
        pairs_by_file.entry(p.a.file.as_str()).or_default().push(p);
        if p.b.file != p.a.file {
            pairs_by_file.entry(p.b.file.as_str()).or_default().push(p);
        }
    }
    let source_lines = |f: &SourceFile| f.lines.saturating_sub(fm.get(f.path.as_str()).map_or(0, |m| m.inline_test_lines));
    let tstems = test_stems(inp.files, test_dirs);
    let empty_hist = History::default();
    let hist = inp.history.unwrap_or(&empty_hist);

    let signals: Vec<Signals> = source
        .iter()
        .map(|f| {
            let h = hist.files.get(&f.path);
            let m = fm.get(f.path.as_str());
            let d = inp.deps.files.get(&f.path);
            let c = inp.clones.files.get(&f.path);
            let mn = inp.mentions.files.get(&f.path);
            let dd = inp.dead.files.get(&f.path);
            let hp = inp.helpers.files.get(&f.path);
            let st = inp.strings.files.get(&f.path);
            let cl = inp.clumps.files.get(&f.path);
            let dc = inp.declared.files.get(&f.path);
            // An importing test file or a same-stem test is evidence too: worth one unit.
            let other_evidence = d.is_some_and(|d| d.test_refs > 0) || stem_tested(&tstems, f);
            Signals {
                lines: f.lines,
                inline_test_lines: m.map_or(0, |m| m.inline_test_lines),
                commits: h.map_or(0, |h| h.commits),
                fix_commits: h.map_or(0, |h| h.fix_commits),
                authors: h.map_or(0, |h| h.authors),
                functions: m.map_or(0, |m| m.functions),
                max_cognitive: m.map_or(0, |m| m.max_cognitive),
                total_cognitive: m.map_or(0, |m| m.total_cognitive),
                complex_functions: m.map_or(0, |m| m.complex_functions),
                fan_in: d.map_or(0, |d| d.fan_in),
                fan_out: d.map_or(0, |d| d.fan_out),
                in_cycle: d.is_some_and(|d| d.in_cycle),
                clone_lines: c.map_or(0, |c| c.clone_lines),
                logic_clone_lines: c.map_or(0, |c| c.logic_clone_lines),
                table_clone_lines: c.map_or(0, |c| c.table_clone_lines),
                clone_ratio: c.map_or(0.0, |c| c.clone_ratio),
                test_units: mn.map_or(0, |m| m.test_units).max(usize::from(other_evidence)),
                public_symbols: mn.map_or(0, |m| m.public_symbols),
                dead_lines: dd.map_or(0, |d| d.dead_lines),
                dead_ratio: dd.map_or(0.0, |d| d.dead_ratio),
                helper_copies: hp.map_or(0, |h| h.helper_copies),
                inlined_idioms: hp.map_or(0, |h| h.inlined_idioms),
                family_literals: st.map_or(0, |s| s.family_literals),
                clump_members: cl.map_or(0, |c| c.members),
                unread_knobs: dc.map_or(0, |d| d.unread_knobs.len()),
            }
        })
        .collect();

    let p_commits = percentiles(&signals.iter().map(|s| s.commits).collect::<Vec<_>>());
    let p_fix = percentiles(&signals.iter().map(|s| s.fix_commits).collect::<Vec<_>>());
    let p_maxcog = percentiles(&signals.iter().map(|s| s.max_cognitive).collect::<Vec<_>>());
    let p_totcog = percentiles(&signals.iter().map(|s| s.total_cognitive).collect::<Vec<_>>());
    let p_lines = percentiles(&signals.iter().map(|s| s.lines.saturating_sub(s.inline_test_lines)).collect::<Vec<_>>());
    let p_fanin = percentiles(&signals.iter().map(|s| s.fan_in).collect::<Vec<_>>());
    let p_clone = percentiles(&signals.iter().map(|s| s.clone_lines).collect::<Vec<_>>());
    let p_dead = percentiles(&signals.iter().map(|s| s.dead_ratio).collect::<Vec<_>>());
    let p_helpers = percentiles(&signals.iter().map(|s| s.helper_copies + s.inlined_idioms).collect::<Vec<_>>());
    let p_strings = percentiles(&signals.iter().map(|s| s.family_literals).collect::<Vec<_>>());
    let p_clumps = percentiles(&signals.iter().map(|s| s.clump_members).collect::<Vec<_>>());
    let p_declared = percentiles(&signals.iter().map(|s| s.unread_knobs).collect::<Vec<_>>());
    // Commits that touched none of our files (a subdirectory scan of a larger
    // repo, a shallow clone) are not history we can rank on.
    let have_history = !hist.files.is_empty();

    // Worst functions per file, for the explanation. Units inside inline test regions are not
    // production code and never head a reason; the plan's extract steps read the full list.
    let mut units: HashMap<&str, Vec<&FunctionMetrics>> = HashMap::new();
    for f in inp.functions.iter().filter(|f| !f.in_test) {
        units.entry(f.file.as_str()).or_default().push(f);
    }
    let mut worst: HashMap<&str, Vec<&FunctionMetrics>> = units.clone();
    for v in worst.values_mut() {
        v.sort_by_key(|f| std::cmp::Reverse(f.cognitive));
        v.truncate(3);
    }

    // Co-change partners with no import relation, indexed by file. A pair whose members both
    // import a file that changed in the same commits is explained by that import instead.
    let (mut hidden, mut explained): (Vec<HiddenCoupling>, Vec<HiddenCoupling>) = (Vec::new(), Vec::new());
    for c in hist.co_changes.iter().filter(|c| !inp.deps.connected(&c.a, &c.b)) {
        let (explained_by, explained_commits) = explained_by(c, hist, inp.deps, inp.history_cfg.explained_min_share).unzip();
        let h = HiddenCoupling {
            a: c.a.clone(), b: c.b.clone(), together: c.together, together_nonsweep: c.together_nonsweep, strength: c.strength, lift: c.lift,
            explained_by, explained_commits: explained_commits.unwrap_or(0),
        };
        if h.explained_by.is_some() { explained.push(h) } else { hidden.push(h) }
    }
    hidden.sort_by(|x, y| y.together_nonsweep.cmp(&x.together_nonsweep));
    explained.sort_by(|x, y| y.together_nonsweep.cmp(&x.together_nonsweep));
    let mut hidden_by_file: HashMap<&str, Vec<&HiddenCoupling>> = HashMap::new();
    let mut explained_by_file: HashMap<&str, Vec<&HiddenCoupling>> = HashMap::new();
    for (list, index) in [(&hidden, &mut hidden_by_file), (&explained, &mut explained_by_file)] {
        for h in list {
            index.entry(h.a.as_str()).or_default().push(h);
            index.entry(h.b.as_str()).or_default().push(h);
        }
    }
    let cycle_of: HashMap<&str, &Cycle> = inp
        .deps
        .file_cycles
        .iter()
        .flat_map(|c| c.members.iter().map(move |m| (m.as_str(), c)))
        .collect();

    let mut hotspots: Vec<Hotspot> = source
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let s = &signals[i];
            let churn = if s.commits == 0 { 0.0 } else { p_commits[i] };
            let ms = cfg.complexity_max_share;
            let cx = if s.max_cognitive == 0 { 0.0 } else { ms * p_maxcog[i] + (1.0 - ms) * p_totcog[i] };
            let hotspot = (churn * cx).sqrt();
            let coupling = if s.fan_in == 0 { 0.0 } else { p_fanin[i] }.max(if s.in_cycle { cfg.cycle_coupling } else { 0.0 });
            let clone = if s.clone_lines == 0 { 0.0 } else { p_clone[i] };
            let fix = if s.fix_commits == 0 { 0.0 } else { p_fix[i] };
            let w: &Weights = if have_history { &cfg.with_history } else { &cfg.without_history };
            let dead_p = if s.dead_lines == 0 { 0.0 } else { p_dead[i] };
            let helpers_p = if s.helper_copies + s.inlined_idioms == 0 { 0.0 } else { p_helpers[i] };
            let strings_p = if s.family_literals == 0 { 0.0 } else { p_strings[i] };
            let clumps_p = if s.clump_members == 0 { 0.0 } else { p_clumps[i] };
            let declared_p = if s.unread_knobs == 0 { 0.0 } else { p_declared[i] };
            let base = w.hotspot * hotspot + w.fixes * fix + w.complexity * cx + w.coupling * coupling + w.clones * clone + w.size * p_lines[i] + w.dead * dead_p + inp.helpers_weight * helpers_p + w.strings * strings_p + inp.clumps_weight * clumps_p + inp.declared_weight * declared_p;
            let score = 100.0 * base * if s.test_units > 0 { 1.0 } else { cfg.no_tests_multiplier };

            let mut reasons = Vec::new();
            let window = hist.window.trim_end_matches(" ago");
            if have_history && p_commits[i] >= cfg.reason_churn_percentile && s.commits > 0 {
                // No author with commits means every commit was a bot's.
                let authors = if s.authors == 0 { "0 authors (all bot commits)".to_string() } else { format!("{} authors", s.authors) };
                reasons.push(format!("{} commits in the last {window} ({} fix commits, {authors})", s.commits, s.fix_commits));
            } else if s.fix_commits >= cfg.reason_min_fix_commits {
                reasons.push(format!("{} fix commits in the last {window}", s.fix_commits));
            }
            if s.complex_functions > 0 {
                let w = worst.get(f.path.as_str()).and_then(|v| v.first());
                let mut r = format!("{} function(s) over cognitive {}", s.complex_functions, inp.cognitive_hard);
                if let Some(w) = w {
                    r.push_str(&format!("; worst {} at {} (lines {}-{}, nesting {})", w.name, w.cognitive, w.start_line, w.end_line, w.max_nesting));
                }
                reasons.push(r);
            }
            if let Some(c) = cycle_of.get(f.path.as_str()) {
                reasons.push(cycle_reason(c, &f.path, inp.dedupe_cycle_reason));
            }
            if p_fanin[i] >= cfg.reason_fanin_percentile && s.fan_in >= cfg.reason_min_fan_in {
                reasons.push(format!("imported by {} files: a change here fans out", s.fan_in));
            }
            let pairs: &[&ClonePair] = pairs_by_file.get(f.path.as_str()).map_or(&[], Vec::as_slice);
            if s.clone_ratio >= cfg.reason_clone_ratio {
                // Pairs are sorted by tokens: the first touching the file is its largest.
                reasons.push(clone_reason(s, inp.clones.files.get(&f.path).map_or(&[], |c| c.tables.as_slice()), &f.path, pairs.first().copied()));
            }
            // One partner budget per file: hidden partners first, then explained ones.
            let hidden_partners = hidden_by_file.get(f.path.as_str()).into_iter().flatten();
            let explained_partners = explained_by_file.get(f.path.as_str()).into_iter().flatten();
            for h in hidden_partners.chain(explained_partners).take(cfg.reason_hidden_partners) {
                let other = if h.a == f.path { &h.b } else { &h.a };
                reasons.push(match &h.explained_by {
                    None => format!("changes together with {other} ({}x, {:.1}x more often than chance{}) but neither imports the other", h.together_nonsweep, h.lift, sweeps_ignored(h)),
                    Some(via) => format!("changes together with {other} ({}x): both import {via}, which changed in {} of those commits", h.together_nonsweep, h.explained_commits),
                });
            }
            if s.test_units == 0 {
                reasons.push("no test unit names any of its symbols".to_string());
            }
            let unmentioned = inp.mentions.files.get(&f.path).map(|m| m.unmentioned.clone()).unwrap_or_default();
            if s.public_symbols >= inp.tests.unmentioned_min_symbols
                && unmentioned.len() as f64 / s.public_symbols as f64 >= inp.tests.unmentioned_share_reason
            {
                reasons.push(unmentioned_reason(&unmentioned, s.public_symbols, cfg.reason_unmentioned_listed));
            }
            // Dead surface: per-symbol dead / test-only lines, the in-file-only line, dead shapes.
            let empty_dead = FileDead::default();
            let dead_file = inp.dead.files.get(&f.path).unwrap_or(&empty_dead);
            reasons.extend(dead_file.reasons.iter().cloned());
            // Helpers: same-name families this file defines a copy in, helper bodies it inlines.
            let helper_file = inp.helpers.files.get(&f.path);
            reasons.extend(helper_file.into_iter().flat_map(|h| h.reasons.iter().cloned()));
            // Strings: the message-family line and the config-literal lines.
            let strings_file = inp.strings.files.get(&f.path);
            reasons.extend(strings_file.into_iter().flat_map(|s| s.reasons.iter().cloned()));
            // Clumps: the parameter tuples this file's functions are members of.
            let clumps_file = inp.clumps.files.get(&f.path);
            reasons.extend(clumps_file.into_iter().flat_map(|c| c.reasons.iter().cloned()));
            // Declared: the config knobs this file declares that no code reads.
            let declared_file = inp.declared.files.get(&f.path);
            reasons.extend(declared_file.into_iter().flat_map(|d| d.reasons.iter().cloned()));
            if s.commits >= cfg.reason_bus_factor_min_commits {
                match s.authors {
                    1 => reasons.push("single author over the window (bus factor 1)".to_string()),
                    0 => reasons.push("no non-bot author over the window (bus factor 0)".to_string()),
                    _ => {}
                }
            }
            let test_regions = fm.get(f.path.as_str()).map(|m| m.test_regions.clone()).unwrap_or_default();
            // No note when the knob counts every line as source: regions are listed, but 0 lines are tests.
            let inline_test_note = (s.inline_test_lines > 0 && inline_ratio(f) >= inp.tests.report_inline_ratio_above)
                .then(|| inline_test_note(&test_regions, s.inline_test_lines));
            let (plan, plan_more) = plan::build(
                &plan::Facts {
                    path: &f.path,
                    pairs,
                    in_tests: &in_tests,
                    regions: &test_regions,
                    functions: units.get(f.path.as_str()).map_or(&[], Vec::as_slice),
                    cycle: cycle_of.get(f.path.as_str()).copied(),
                    cognitive_hard: inp.cognitive_hard,
                },
                inp.plan,
            );
            Hotspot {
                path: f.path.clone(),
                score,
                signals: s.clone(),
                reasons,
                worst_functions: worst.get(f.path.as_str()).map(|v| v.iter().map(|f| (*f).clone()).collect()).unwrap_or_default(),
                test_regions,
                inline_test_note,
                unmentioned,
                plan,
                plan_more,
                dead_symbols: dead_file.symbols.clone(),
                dead_shapes: dead_file.shapes.clone(),
                helper_copies: helper_file.map(|h| h.families.clone()).unwrap_or_default(),
                inlined_idioms: helper_file.map(|h| h.idioms.clone()).unwrap_or_default(),
                repeated_literals: strings_file.map(|s| s.literals.clone()).unwrap_or_default(),
                clumps: clumps_file.map(|c| c.clumps.clone()).unwrap_or_default(),
                unread_knobs: declared_file.map(|d| d.unread_knobs.clone()).unwrap_or_default(),
            }
        })
        .collect();
    hotspots.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    hotspots.truncate(top);

    // Per-directory rollup.
    let mut dirs: BTreeMap<&str, DirSummary> = BTreeMap::new();
    for (i, f) in source.iter().enumerate() {
        let d = f.module_dir();
        let e = dirs.entry(d).or_insert_with(|| DirSummary {
            dir: d.to_string(), files: 0, lines: 0, total_cognitive: 0, largest_file: String::new(), largest_lines: 0,
        });
        let lines = source_lines(f);
        e.files += 1;
        e.lines += lines;
        e.total_cognitive += signals[i].total_cognitive;
        if lines > e.largest_lines {
            e.largest_lines = lines;
            e.largest_file = f.path.clone();
        }
    }
    let mut directories: Vec<DirSummary> = dirs.into_values().collect();
    directories.sort_by_key(|d| std::cmp::Reverse(d.lines));

    let summary = Summary {
        root: inp.root,
        source_files: source.len(),
        test_files: inp.files.iter().filter(|f| f.kind == FileKind::Test).count() + reclassified_files,
        source_lines: source.iter().map(|f| source_lines(f)).sum(),
        functions: inp.functions.iter().filter(|f| !f.in_test).count(),
        complex_functions: inp.functions.iter().filter(|f| !f.in_test && f.cognitive > inp.cognitive_hard).count(),
        history_window: inp.history.map(|h| h.window.clone()),
        commits_scanned: hist.commits_scanned,
        sweep_commits: hist.sweep_commits,
        cognitive_hard: inp.cognitive_hard,
    };

    Report {
        summary,
        hotspots,
        dir_cycles: inp.deps.dir_cycles.clone(),
        file_cycles: inp.deps.file_cycles.clone(),
        tables: if inp.list_tables_separately { inp.clones.pairs.iter().filter(|p| p.kind == CloneKind::Table).take(top).cloned().collect() } else { Vec::new() },
        hidden_coupling: hidden,
        explained_coupling: explained,
        sweep_note: sweep_note(hist, inp.history_cfg.sweep_fraction),
        clones: inp.clones.pairs.iter().filter(|p| !inp.list_tables_separately || p.kind == CloneKind::Logic).take(top).cloned().collect(),
        directories: directories.into_iter().take(top).collect(),
        dead: DeadSurface {
            totals: inp.dead.totals.clone(),
            modes: inp.dead.modes.clone(),
            notes: inp.dead.notes.clone(),
            files: dead::top_files(inp.dead, top).into_iter().map(|(p, d)| DeadFileRow {
                path: p.clone(), pub_items: d.pub_items, dead_count: d.dead_count, test_only_count: d.test_only_count,
                overexported_count: d.overexported_count, dead_lines: d.dead_lines, dead_ratio: d.dead_ratio,
            }).collect(),
        },
        helpers: HelpersSection {
            totals: inp.helpers.totals.clone(),
            notes: inp.helpers.notes.clone(),
            families: inp.helpers.families.iter().take(top).cloned().collect(),
            inlined: inp.helpers.inlined.iter().take(top).cloned().collect(),
        },
        strings: StringsSection {
            totals: inp.strings.totals.clone(),
            notes: inp.strings.notes.clone(),
            families: inp.strings.families.iter().take(top).cloned().collect(),
            config: inp.strings.config.iter().take(top).cloned().collect(),
            near: inp.strings.near.iter().take(top).cloned().collect(),
        },
        clumps: ClumpsSection {
            totals: inp.clumps.totals.clone(),
            notes: inp.clumps.notes.clone(),
            clumps: inp.clumps.clumps.iter().take(top).cloned().collect(),
            unused_prefix: inp.clumps_prefix.to_string(),
        },
        declared: DeclaredSection {
            totals: inp.declared.totals.clone(),
            notes: inp.declared.notes.clone(),
            orphans: inp.declared.orphans.iter().take(top).cloned().collect(),
            misplaced: inp.declared.misplaced.iter().take(top).cloned().collect(),
            dead_features: inp.declared.dead_features.iter().take(top).cloned().collect(),
            noop_features: inp.declared.noop_features.iter().take(top).cloned().collect(),
            unread_knobs: inp.declared.unread_knobs.iter().take(top).cloned().collect(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn td() -> Vec<String> {
        crate::config::Discover::default().test_dirs
    }

    fn hcfg() -> HistoryCfg {
        HistoryCfg::default()
    }

    fn pcfg() -> PlanCfg {
        PlanCfg::default()
    }

    /// An empty dead-surface report, for the tests that are not about it.
    fn nodead() -> &'static DeadReport {
        static NODEAD: std::sync::OnceLock<DeadReport> = std::sync::OnceLock::new();
        NODEAD.get_or_init(DeadReport::default)
    }

    fn nodeclared() -> &'static DeclaredReport {
        static NODECLARED: std::sync::OnceLock<DeclaredReport> = std::sync::OnceLock::new();
        NODECLARED.get_or_init(DeclaredReport::default)
    }

    fn nohelpers() -> &'static HelpersReport {
        static NOHELPERS: std::sync::OnceLock<HelpersReport> = std::sync::OnceLock::new();
        NOHELPERS.get_or_init(HelpersReport::default)
    }

    fn noclumps() -> &'static ClumpsReport {
        static R: std::sync::OnceLock<ClumpsReport> = std::sync::OnceLock::new();
        R.get_or_init(ClumpsReport::default)
    }

    fn nostrings() -> &'static StringsReport {
        static NOSTRINGS: std::sync::OnceLock<StringsReport> = std::sync::OnceLock::new();
        NOSTRINGS.get_or_init(StringsReport::default)
    }

    fn fmetrics(path: &str, total: u32, max: u32, complex: usize) -> FileMetrics {
        FileMetrics {
            path: path.into(), functions: 1, total_cognitive: total, max_cognitive: max, max_nesting: 0, complex_functions: complex,
            parse_errors: false, inline_test_lines: 0, test_regions: vec![],
        }
    }

    fn region(kind: RegionKind, start_line: usize, end_line: usize) -> TestRegion {
        TestRegion { kind, start_byte: 0, end_byte: 0, start_line, end_line }
    }

    #[test]
    fn inline_test_regions_shrink_size_and_count_as_tests() {
        let sf = |p: &str, lines: usize| SourceFile {
            path: p.into(), lang: crate::lang::Language::Rust, kind: FileKind::Source, lines, bytes: 0, content: String::new(),
        };
        // a.rs is mostly tests: ranks small on size and has tests. b.rs is all source, no tests.
        // c.rs is 95% tests: reclassified, not a hotspot.
        let files = vec![sf("a.rs", 2308), sf("b.rs", 1500), sf("c.rs", 100)];
        let mut a = fmetrics("a.rs", 1, 1, 0);
        a.inline_test_lines = 1300;
        a.test_regions = vec![region(RegionKind::CfgTestMod, 1009, 2308)];
        let mut c = fmetrics("c.rs", 1, 1, 0);
        c.inline_test_lines = 95;
        c.test_regions = vec![region(RegionKind::CfgTestMod, 6, 100)];
        let fm = vec![a, fmetrics("b.rs", 1, 1, 0), c];
        let functions = vec![
            FunctionMetrics { file: "a.rs".into(), name: "src".into(), start_line: 1, end_line: 2, lines: 2, params: 0, cyclomatic: 1, cognitive: 1, max_nesting: 0, in_test: false },
            FunctionMetrics { file: "a.rs".into(), name: "tst".into(), start_line: 1300, end_line: 1360, lines: 61, params: 0, cyclomatic: 1, cognitive: 40, max_nesting: 3, in_test: true },
        ];
        let deps = DepGraph::default();
        let clones = CloneReport::default();
        let tests = TestsCfg::default();
        let hcfg = hcfg();
        let pcfg = pcfg();
        // The inline test names a symbol of a.rs; nothing names b.rs.
        let mut mentions = MentionIndex::default();
        mentions.files.insert("a.rs".into(), crate::mentions::FileMentions { test_units: 1, inline_units: 1, ..Default::default() });
        let size_only = Cfg {
            without_history: Weights { hotspot: 0.0, fixes: 0.0, complexity: 0.0, coupling: 0.0, clones: 0.0, size: 1.0, dead: 0.0, strings: 0.0 },
            ..Cfg::default()
        };
        let r = build(Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm, functions: &functions, deps: &deps, clones: &clones, cognitive_hard: 15, mentions: &mentions, tests: &tests, list_tables_separately: true, history_cfg: &hcfg, dedupe_cycle_reason: true, plan: &pcfg, dead: nodead(), helpers: nohelpers(), helpers_weight: 0.0, strings: nostrings(), clumps: noclumps(), clumps_weight: 0.0, clumps_prefix: "_", declared: nodeclared(), declared_weight: 0.0 }, 10, &size_only, &td());
        let paths: Vec<&str> = r.hotspots.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec!["b.rs", "a.rs"], "{r:?}");
        let a = &r.hotspots[1];
        assert_eq!((a.signals.lines, a.signals.inline_test_lines), (2308, 1300));
        assert_eq!(a.signals.test_units, 1);
        assert_eq!(r.hotspots[0].signals.test_units, 0);
        assert_eq!(a.inline_test_note.as_deref(), Some("1300 in #[cfg(test)] mod at 1009-2308"));
        assert_eq!(a.test_regions.len(), 1);
        assert_eq!(a.worst_functions.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), vec!["src"]);
        assert_eq!((r.summary.source_files, r.summary.test_files, r.summary.source_lines, r.summary.functions), (2, 1, 1008 + 1500, 1));
        assert_eq!((r.directories[0].lines, r.directories[0].largest_file.as_str(), r.directories[0].largest_lines), (2508, "b.rs", 1500));
        let text = render(&r, 10);
        assert!(text.contains("a.rs  (2308 lines, 1300 in #[cfg(test)] mod at 1009-2308)"), "{text}");
        assert!(text.contains("b.rs  (1500 lines)"), "{text}");
        // Below the ratio knob the note is absent; the inline unit still counts.
        let strict = TestsCfg { report_inline_ratio_above: 0.9, ..TestsCfg::default() };
        let r = build(Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm, functions: &functions, deps: &deps, clones: &clones, cognitive_hard: 15, mentions: &mentions, tests: &strict, list_tables_separately: true, history_cfg: &hcfg, dedupe_cycle_reason: true, plan: &pcfg, dead: nodead(), helpers: nohelpers(), helpers_weight: 0.0, strings: nostrings(), clumps: noclumps(), clumps_weight: 0.0, clumps_prefix: "_", declared: nodeclared(), declared_weight: 0.0 }, 10, &size_only, &td());
        assert!(r.hotspots[1].inline_test_note.is_none());
        assert_eq!(r.hotspots[1].signals.test_units, 1);
        // The inline_modules knob gates line counts and tags, not the mention index.
        let off = TestsCfg { inline_modules: false, ..TestsCfg::default() };
        let r = build(Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm, functions: &functions, deps: &deps, clones: &clones, cognitive_hard: 15, mentions: &mentions, tests: &off, list_tables_separately: true, history_cfg: &hcfg, dedupe_cycle_reason: true, plan: &pcfg, dead: nodead(), helpers: nohelpers(), helpers_weight: 0.0, strings: nostrings(), clumps: noclumps(), clumps_weight: 0.0, clumps_prefix: "_", declared: nodeclared(), declared_weight: 0.0 }, 10, &size_only, &td());
        assert_eq!(r.hotspots.iter().find(|h| h.path == "a.rs").unwrap().signals.test_units, 1);
        // Knob off, the metrics pass lists the regions but counts 0 inline test lines: no note,
        // even when the ratio threshold would always show one.
        let mut fm_off = fm.clone();
        fm_off.iter_mut().for_each(|m| m.inline_test_lines = 0);
        let always = TestsCfg { inline_modules: false, report_inline_ratio_above: 0.0, ..TestsCfg::default() };
        let r = build(Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm_off, functions: &functions, deps: &deps, clones: &clones, cognitive_hard: 15, mentions: &mentions, tests: &always, list_tables_separately: true, history_cfg: &hcfg, dedupe_cycle_reason: true, plan: &pcfg, dead: nodead(), helpers: nohelpers(), helpers_weight: 0.0, strings: nostrings(), clumps: noclumps(), clumps_weight: 0.0, clumps_prefix: "_", declared: nodeclared(), declared_weight: 0.0 }, 10, &size_only, &td());
        let a = r.hotspots.iter().find(|h| h.path == "a.rs").unwrap();
        assert!(a.inline_test_note.is_none() && a.test_regions.len() == 1, "{:?}", a.inline_test_note);
        // Many regions: the largest one's kind and range, the rest counted.
        let many = [region(RegionKind::CfgTestItem, 18, 20), region(RegionKind::CfgTestMod, 66, 210), region(RegionKind::CfgTestItem, 24, 26)];
        assert_eq!(inline_test_note(&many, 151), "151 in #[cfg(test)] mod at 66-210, +2 more");
        assert_eq!(inline_test_note(&many[..1], 3), "3 in #[cfg(test)] item at 18-20");
    }

    #[test]
    fn dead_ratio_is_a_weighted_signal_and_dead_reasons_reach_the_hotspot() {
        use crate::dead::{Category, FileDead, SymbolReport, Totals, Visibility};
        let sf = |p: &str| SourceFile { path: p.into(), lang: crate::lang::Language::Rust, kind: FileKind::Source, lines: 100, bytes: 0, content: String::new() };
        let files = vec![sf("a.rs"), sf("b.rs"), sf("c.rs")];
        let fm: Vec<FileMetrics> = files.iter().map(|f| fmetrics(&f.path, 1, 1, 0)).collect();
        let (deps, clones, tests, hcfg, mentions, pcfg) = (DepGraph::default(), CloneReport::default(), TestsCfg::default(), hcfg(), MentionIndex::default(), pcfg());
        let mut dead = DeadReport { totals: Totals { files_indexed: 3, pub_items: 2, dead: 1, ..Totals::default() }, ..DeadReport::default() };
        let symbol = SymbolReport {
            name: "install".into(), display: "pub fn install".into(), kind: "fn", visibility: Visibility::Public, start_line: 143, end_line: 147, category: Category::Dead,
            external_prod_refs: 0, external_test_refs: 0, own_prod_refs: 0, own_test_refs: 0, doc_mentions: 5, samename: 1, test_files: vec![],
        };
        dead.files.insert("a.rs".into(), FileDead { pub_items: 1, dead_count: 1, dead_lines: 5, dead_ratio: 0.05, symbols: vec![symbol], reasons: vec!["pub fn install (a.rs 143-147) is called nowhere in 3 files (5 doc mentions)".into()], ..FileDead::default() });
        dead.files.insert("b.rs".into(), FileDead { pub_items: 1, overexported_count: 1, dead_lines: 20, dead_ratio: 0.2, own_file_only_share: 1.0, ..FileDead::default() });
        let dead_only = Cfg {
            without_history: Weights { hotspot: 0.0, fixes: 0.0, complexity: 0.0, coupling: 0.0, clones: 0.0, size: 0.0, dead: 1.0, strings: 0.0 },
            ..Cfg::default()
        };
        let r = build(Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm, functions: &[], deps: &deps, clones: &clones, cognitive_hard: 15, mentions: &mentions, tests: &tests, list_tables_separately: true, history_cfg: &hcfg, dedupe_cycle_reason: true, plan: &pcfg, dead: &dead, helpers: nohelpers(), helpers_weight: 0.0, strings: nostrings(), clumps: noclumps(), clumps_weight: 0.0, clumps_prefix: "_", declared: nodeclared(), declared_weight: 0.0 }, 10, &dead_only, &td());
        let paths: Vec<&str> = r.hotspots.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec!["b.rs", "a.rs", "c.rs"], "{r:?}");
        let a = &r.hotspots[1];
        assert_eq!((a.signals.dead_lines, a.signals.dead_ratio), (5, 0.05));
        assert!(a.reasons.contains(&"pub fn install (a.rs 143-147) is called nowhere in 3 files (5 doc mentions)".to_string()), "{:?}", a.reasons);
        assert_eq!(a.dead_symbols.len(), 1);
        // A zero signal scores 0, not its tie percentile.
        assert_eq!(r.hotspots[2].score, 0.0);
        assert!(r.hotspots[0].score > r.hotspots[1].score && r.hotspots[1].score > 0.0);
        assert_eq!(r.dead.files.iter().map(|d| (d.path.as_str(), d.dead_lines)).collect::<Vec<_>>(), vec![("b.rs", 20), ("a.rs", 5)]);
        let text = render(&r, 10);
        assert!(text.contains("\nDEAD SURFACE  (exported symbols with no production use outside their file; fields never read, variants never constructed)\n  2 pub items checked in 3 files: 1 dead, 0 test-only, 0 referenced only in-file (0% with no external production use); 0 dead shapes\n  lines ratio  d/t/i of items  path\n     20   20%      0/0/1 of 1  b.rs\n      5    5%      1/0/0 of 1  a.rs\n"), "{text}");
        assert!(text.contains("        - pub fn install (a.rs 143-147) is called nowhere in 3 files (5 doc mentions)\n"), "{text}");
        // Nothing indexed: the section says none.
        let r = build(Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm, functions: &[], deps: &deps, clones: &clones, cognitive_hard: 15, mentions: &mentions, tests: &tests, list_tables_separately: true, history_cfg: &hcfg, dedupe_cycle_reason: true, plan: &pcfg, dead: nodead(), helpers: nohelpers(), helpers_weight: 0.0, strings: nostrings(), clumps: noclumps(), clumps_weight: 0.0, clumps_prefix: "_", declared: nodeclared(), declared_weight: 0.0 }, 10, &dead_only, &td());
        assert!(render(&r, 10).contains("DEAD SURFACE  (exported symbols with no production use outside their file; fields never read, variants never constructed)\n  none\n"));
    }

    #[test]
    fn helper_signals_are_weighted_by_the_helpers_knob_and_the_section_prints() {
        use crate::helpers::{Family, FamilyClass, FileHelpers, Hit, Inlined, Totals};
        let sf = |p: &str| SourceFile { path: p.into(), lang: crate::lang::Language::Rust, kind: FileKind::Source, lines: 100, bytes: 0, content: String::new() };
        let files = vec![sf("a.rs"), sf("b.rs"), sf("c.rs")];
        let fm: Vec<FileMetrics> = files.iter().map(|f| fmetrics(&f.path, 1, 1, 0)).collect();
        let (deps, clones, tests, hcfg, mentions, pcfg) = (DepGraph::default(), CloneReport::default(), TestsCfg::default(), hcfg(), MentionIndex::default(), pcfg());
        let mut h = HelpersReport { totals: Totals { definitions: 12, files: 3, multi_file_names: 1, verbatim: 1, families: 1, small_helpers: 4, inlined_helpers: 1, inlined_share: 0.25, git_lookups: 2, ..Totals::default() }, notes: vec!["attribution: 2 git lookups".into()], ..HelpersReport::default() };
        h.families.push(Family { name: "plural".into(), kind: "fn", class: FamilyClass::Verbatim, best_jaccard: 1.0, files: 2, copies: vec![], pairs: vec![], commits: 2, sessions: 2, annotation: None, inlined: None, line: "plural  2 copies in 2 files, verbatim (Jaccard 1.00): a.rs:1-3, c.rs:5-7 (verbatim); from 2 commits / 2 sessions".into() });
        h.inlined.push(Inlined { name: "io_err".into(), file: "c.rs".into(), start_line: 20, end_line: 22, visibility: crate::dead::Visibility::Private, tokens: 18, also_defined: vec![], hits: vec![Hit { file: "b.rs".into(), line: 9, reachable: false }, Hit { file: "b.rs".into(), line: 30, reachable: false }], files: 1, line: "body of io_err (c.rs:20-22, 18 tokens) is inlined 2 times in 1 file: b.rs:9, b.rs:30 - io_err is private; hoist and call".into() });
        h.files.insert("a.rs".into(), FileHelpers { helper_copies: 1, families: vec!["plural".into()], reasons: vec!["defines plural (lines 1-3), also defined verbatim in c.rs:5 - 2 copies from 2 commits / 2 sessions; hoist one".into()], ..FileHelpers::default() });
        h.files.insert("b.rs".into(), FileHelpers { inlined_idioms: 2, idioms: vec![("io_err".into(), 9), ("io_err".into(), 30)], reasons: vec!["body of io_err (c.rs:20-22) is inlined 2 times in 1 file: b.rs:9, b.rs:30 - io_err is private; hoist and call".into()], ..FileHelpers::default() });
        let none = Cfg {
            without_history: Weights { hotspot: 0.0, fixes: 0.0, complexity: 0.0, coupling: 0.0, clones: 0.0, size: 0.0, dead: 0.0, strings: 0.0 },
            ..Cfg::default()
        };
        let inputs = |w: f64| Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm, functions: &[], deps: &deps, clones: &clones, cognitive_hard: 15, mentions: &mentions, tests: &tests, list_tables_separately: true, history_cfg: &hcfg, dedupe_cycle_reason: true, plan: &pcfg, dead: nodead(), helpers: &h, helpers_weight: w, strings: nostrings(), clumps: noclumps(), clumps_weight: 0.0, clumps_prefix: "_", declared: nodeclared(), declared_weight: 0.0 };
        let r = build(inputs(1.0), 10, &none, &td());
        let paths: Vec<&str> = r.hotspots.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec!["b.rs", "a.rs", "c.rs"], "{r:?}");
        let b = &r.hotspots[0];
        assert_eq!((b.signals.helper_copies, b.signals.inlined_idioms, b.inlined_idioms.len()), (0, 2, 2));
        assert_eq!(b.reasons.last().unwrap(), "body of io_err (c.rs:20-22) is inlined 2 times in 1 file: b.rs:9, b.rs:30 - io_err is private; hoist and call");
        let a = &r.hotspots[1];
        assert_eq!((a.signals.helper_copies, a.helper_copies.as_slice()), (1, &["plural".to_string()][..]));
        assert_eq!(a.reasons.last().unwrap(), "defines plural (lines 1-3), also defined verbatim in c.rs:5 - 2 copies from 2 commits / 2 sessions; hoist one");
        assert!(r.hotspots[0].score > r.hotspots[1].score && r.hotspots[1].score > 0.0 && r.hotspots[2].score == 0.0);
        // Weight 0 (the default): the reasons and the section stay, the score ignores them.
        let r0 = build(inputs(0.0), 10, &none, &td());
        assert!(r0.hotspots.iter().all(|h| h.score == 0.0));
        assert!(r0.hotspots.iter().find(|h| h.path == "a.rs").unwrap().reasons.iter().any(|x| x.starts_with("defines plural")));
        let text = render(&r, 10);
        assert!(text.contains("\nHELPERS  (same-name helpers defined in several files; small helper bodies inlined instead of called)\n  1 names defined in 2+ files: 1 verbatim, 0 similar, 0 different contract; 1 of 4 small helpers inlined elsewhere (25.0%)\n  attribution: 2 git lookups\n  plural  2 copies in 2 files, verbatim (Jaccard 1.00): a.rs:1-3, c.rs:5-7 (verbatim); from 2 commits / 2 sessions\n  INLINED  (helper bodies found as exact token sequences elsewhere)\n  body of io_err (c.rs:20-22, 18 tokens) is inlined 2 times in 1 file: b.rs:9, b.rs:30 - io_err is private; hoist and call\n"), "{text}");
        assert_eq!(r.helpers.families.len(), 1);
        // Nothing indexed: the section says none.
        let r = build(Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm, functions: &[], deps: &deps, clones: &clones, cognitive_hard: 15, mentions: &mentions, tests: &tests, list_tables_separately: true, history_cfg: &hcfg, dedupe_cycle_reason: true, plan: &pcfg, dead: nodead(), helpers: nohelpers(), helpers_weight: 0.0, strings: nostrings(), clumps: noclumps(), clumps_weight: 0.0, clumps_prefix: "_", declared: nodeclared(), declared_weight: 0.0 }, 10, &none, &td());
        assert!(render(&r, 10).contains("HELPERS  (same-name helpers defined in several files; small helper bodies inlined instead of called)\n  none\n"));
    }

    #[test]
    fn clump_signals_are_weighted_by_the_clumps_knob_and_the_section_prints() {
        use crate::clumps::{Clump, FileClumps, Member, Totals};
        let sf = |p: &str| SourceFile { path: p.into(), lang: crate::lang::Language::Rust, kind: FileKind::Source, lines: 100, bytes: 0, content: String::new() };
        let files = vec![sf("a.rs"), sf("b.rs"), sf("c.rs")];
        let fm: Vec<FileMetrics> = files.iter().map(|f| fmetrics(&f.path, 1, 1, 0)).collect();
        let (deps, clones, tests, hcfg, mentions, pcfg) = (DepGraph::default(), CloneReport::default(), TestsCfg::default(), hcfg(), MentionIndex::default(), pcfg());
        let reason = "parameters (s, f, a, _m) recur in 5 functions across 2 files (plan_start a.rs:1, plan_ship :2, plan_park :3, plan_drop :4, plan_repair b.rs:1); `_m` is unused in all 5 - introduce a PlanInput struct or drop the slot";
        let line = " 5 fns   2 files  (s: &Snapshot, f, a, _m: &Minter)  plan_start a.rs:1, plan_ship :2, plan_park :3, plan_drop :4, plan_repair b.rs:1; _m unused in 5/5";
        let mut c = ClumpsReport { totals: Totals { functions: 20, in_clumps: 5, clumps: 1, files: 2, silenced_functions: 5, skipped_trait_impls: 3, ..Totals::default() }, notes: vec!["3 functions not analysed: 3 trait / override methods and callbacks, 0 protocol tuples, 0 overloads / dunders / stubs, 0 same-name duplicates".into()], ..ClumpsReport::default() };
        c.clumps.push(Clump { params: vec!["s".into(), "f".into(), "a".into(), "_m".into()], types: vec![Some("&Snapshot".into()), None, None, Some("&Minter".into())], variants: BTreeMap::new(), functions: vec![Member { file: "a.rs".into(), name: "plan_start".into(), line: 1 }], files: 2, unused_slots: BTreeMap::from([("_m".to_string(), 5)]), single_caller: None, line: line.into() });
        c.files.insert("a.rs".into(), FileClumps { members: 4, clumps: vec![("(s, f, a, _m)".into(), 1), ("(s, f, a, _m)".into(), 2), ("(s, f, a, _m)".into(), 3), ("(s, f, a, _m)".into(), 4)], reasons: vec![reason.into()] });
        c.files.insert("b.rs".into(), FileClumps { members: 1, clumps: vec![("(s, f, a, _m)".into(), 1)], reasons: vec![reason.into()] });
        let none = Cfg {
            without_history: Weights { hotspot: 0.0, fixes: 0.0, complexity: 0.0, coupling: 0.0, clones: 0.0, size: 0.0, dead: 0.0, strings: 0.0 },
            ..Cfg::default()
        };
        let inputs = |w: f64| Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm, functions: &[], deps: &deps, clones: &clones, cognitive_hard: 15, mentions: &mentions, tests: &tests, list_tables_separately: true, history_cfg: &hcfg, dedupe_cycle_reason: true, plan: &pcfg, dead: nodead(), helpers: nohelpers(), helpers_weight: 0.0, strings: nostrings(), clumps: &c, clumps_weight: w, clumps_prefix: "_", declared: nodeclared(), declared_weight: 0.0 };
        let r = build(inputs(1.0), 10, &none, &td());
        let paths: Vec<&str> = r.hotspots.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec!["a.rs", "b.rs", "c.rs"], "{r:?}");
        let a = &r.hotspots[0];
        assert_eq!((a.signals.clump_members, a.clumps.len()), (4, 4));
        assert_eq!(a.reasons.last().unwrap(), reason);
        assert_eq!(r.hotspots[1].signals.clump_members, 1);
        assert!(r.hotspots[0].score > r.hotspots[1].score && r.hotspots[1].score > 0.0 && r.hotspots[2].score == 0.0);
        // Weight 0 (the default): the reasons and the section stay, the score ignores them.
        let r0 = build(inputs(0.0), 10, &none, &td());
        assert!(r0.hotspots.iter().all(|h| h.score == 0.0));
        assert!(r0.hotspots.iter().find(|h| h.path == "b.rs").unwrap().reasons.iter().any(|x| x.starts_with("parameters (s, f, a, _m)")));
        let text = render(&r, 10);
        assert!(text.contains(&format!("\nCLUMPS  (parameter tuples recurring across functions; slots no member reads)\n  1 clumps over 5 of 20 functions (25.0%) in 2 files; 5 functions (25.0%) carry a _-prefixed parameter\n  3 functions not analysed: 3 trait / override methods and callbacks, 0 protocol tuples, 0 overloads / dunders / stubs, 0 same-name duplicates\n  {line}\n")), "{text}");
        assert_eq!(r.clumps.clumps.len(), 1);
        // Nothing analysed: the section says none.
        let r = build(Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm, functions: &[], deps: &deps, clones: &clones, cognitive_hard: 15, mentions: &mentions, tests: &tests, list_tables_separately: true, history_cfg: &hcfg, dedupe_cycle_reason: true, plan: &pcfg, dead: nodead(), helpers: nohelpers(), helpers_weight: 0.0, strings: nostrings(), clumps: noclumps(), clumps_weight: 0.0, clumps_prefix: "_", declared: nodeclared(), declared_weight: 0.0 }, 10, &none, &td());
        assert!(render(&r, 10).contains("CLUMPS  (parameter tuples recurring across functions; slots no member reads)\n  none\n"));
    }

    #[test]
    fn unread_knob_reasons_reach_the_config_file_and_the_declared_section_prints() {
        use crate::declared::{FileDeclared, Orphan, Totals, UnreadKnob};
        let sf = |p: &str| SourceFile { path: p.into(), lang: crate::lang::Language::Rust, kind: FileKind::Source, lines: 100, bytes: 0, content: String::new() };
        let files = vec![sf("src/config.rs"), sf("src/main.rs")];
        let fm: Vec<FileMetrics> = files.iter().map(|f| fmetrics(&f.path, 1, 1, 0)).collect();
        let (deps, clones, tests, hcfg, mentions, pcfg) = (DepGraph::default(), CloneReport::default(), TestsCfg::default(), hcfg(), MentionIndex::default(), pcfg());
        let knob = "[ci.homerunner] (src/config.rs:182-204, 4 knobs: homerunner, bin, db, api) is deserialised from TOML under deny_unknown_fields and documented at docs/config.md:91-94, but no code reads any of them - the program accepts the section and ignores it";
        let orphan = "pulldown-cmark declared in Cargo.toml:39 since 988d9b5 (13 days, 101 commits) and never imported by any src/ or tests/ file; mentioned in DESIGN.md:632 - wire it or drop it";
        let mut d = DeclaredReport { totals: Totals { manifests: 1, deps: 24, checked_deps: 21, orphans: 1, features: 1, config_structs: 8, knobs: 33, unread_knobs: 4, ..Totals::default() }, notes: vec!["2 dev-dependencies not checked (check_dev_dependencies)".into()], ..DeclaredReport::default() };
        d.orphans.push(Orphan { manifest: "Cargo.toml".into(), name: "pulldown-cmark".into(), ident: "pulldown_cmark".into(), line: 39, section: "dependencies".into(), optional: false, gated_by: vec![], birth: Some(("988d9b5".into(), "2026-08-31".into())), age_days: Some(13), commits_since: Some(101), ever_imported: None, doc_mention: Some(("DESIGN.md".into(), 632)), lint_silenced: None, line_text: orphan.into() });
        d.unread_knobs.push(UnreadKnob { section: "[ci.homerunner]".into(), struct_name: "HomerunnerCfg".into(), file: "src/config.rs".into(), start_line: 182, end_line: 204, knobs: vec!["homerunner".into(), "bin".into(), "db".into(), "api".into()], unread: vec!["homerunner".into()], source: "TOML".into(), doc: Some(("docs/config.md".into(), 91, 94)), test_only: false, rollup: true, line_text: knob.into() });
        d.files.insert("src/config.rs".into(), FileDeclared { unread_knobs: vec![("ci.homerunner.homerunner".into(), 182)], reasons: vec![knob.into()] });
        let none = Cfg {
            without_history: Weights { hotspot: 0.0, fixes: 0.0, complexity: 0.0, coupling: 0.0, clones: 0.0, size: 0.0, dead: 0.0, strings: 0.0 },
            ..Cfg::default()
        };
        let inputs = |w: f64| Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm, functions: &[], deps: &deps, clones: &clones, cognitive_hard: 15, mentions: &mentions, tests: &tests, list_tables_separately: true, history_cfg: &hcfg, dedupe_cycle_reason: true, plan: &pcfg, dead: nodead(), helpers: nohelpers(), helpers_weight: 0.0, strings: nostrings(), clumps: noclumps(), clumps_weight: 0.0, clumps_prefix: "_", declared: &d, declared_weight: w };
        let r = build(inputs(1.0), 10, &none, &td());
        let a = r.hotspots.iter().find(|h| h.path == "src/config.rs").unwrap();
        assert_eq!((a.signals.unread_knobs, a.unread_knobs.clone()), (1, vec![("ci.homerunner.homerunner".to_string(), 182)]));
        assert_eq!(a.reasons.last().unwrap(), knob);
        let b = r.hotspots.iter().find(|h| h.path == "src/main.rs").unwrap();
        assert!(a.score > 0.0 && b.score == 0.0 && b.signals.unread_knobs == 0);
        // Weight 0 (the default): the reason and the section stay, the score ignores them.
        let r0 = build(inputs(0.0), 10, &none, &td());
        assert!(r0.hotspots.iter().all(|h| h.score == 0.0));
        assert!(r0.hotspots.iter().find(|h| h.path == "src/config.rs").unwrap().reasons.contains(&knob.to_string()));
        let text = render(&r, 10);
        assert!(text.contains(&format!("\nDECLARED  (dependencies no file imports; feature flags nothing checks; config knobs no code reads)\n  1 of 24 declared deps orphaned (4.2%: pulldown-cmark) in 1 manifest; 0 of 1 features dead, 0 no-op; 4 of 33 config knobs in 8 structs unread\n  2 dev-dependencies not checked (check_dev_dependencies)\n  ORPHANED DEPS  (declared, referenced by no file in the manifest's scope)\n  {orphan}\n  UNREAD KNOBS  (accepted by a Deserialize struct, read by no code)\n  {knob}\n")), "{text}");
        assert_eq!((r.declared.orphans.len(), r.declared.unread_knobs.len()), (1, 1));
        // No manifest and no candidate: the section says none.
        let r = build(Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm, functions: &[], deps: &deps, clones: &clones, cognitive_hard: 15, mentions: &mentions, tests: &tests, list_tables_separately: true, history_cfg: &hcfg, dedupe_cycle_reason: true, plan: &pcfg, dead: nodead(), helpers: nohelpers(), helpers_weight: 0.0, strings: nostrings(), clumps: noclumps(), clumps_weight: 0.0, clumps_prefix: "_", declared: nodeclared(), declared_weight: 0.0 }, 10, &none, &td());
        assert!(render(&r, 10).contains("DECLARED  (dependencies no file imports; feature flags nothing checks; config knobs no code reads)\n  none\n"));
    }

    #[test]
    fn a_file_touched_only_by_bots_has_bus_factor_zero() {
        use crate::history::FileHistory;
        let sf = |p: &str| SourceFile { path: p.into(), lang: crate::lang::Language::Rust, kind: FileKind::Source, lines: 100, bytes: 0, content: String::new() };
        let files = vec![sf("bot.rs"), sf("solo.rs"), sf("pair.rs"), sf("quiet.rs")];
        let fm: Vec<FileMetrics> = files.iter().map(|f| fmetrics(&f.path, 1, 1, 0)).collect();
        let mut hist = History { window: "6 months ago".into(), commits_scanned: 20, ..History::default() };
        for (p, commits, authors) in [("bot.rs", 9, 0), ("solo.rs", 6, 1), ("pair.rs", 5, 2), ("quiet.rs", 1, 1)] {
            let mut fh = FileHistory::default();
            fh.commits = commits;
            fh.authors = authors;
            hist.files.insert(p.into(), fh);
        }
        let (deps, clones, tests, hcfg, mentions, pcfg) = (DepGraph::default(), CloneReport::default(), TestsCfg::default(), hcfg(), MentionIndex::default(), pcfg());
        let r = build(Inputs { root: String::new(), files: &files, history: Some(&hist), file_metrics: &fm, functions: &[], deps: &deps, clones: &clones, cognitive_hard: 15, mentions: &mentions, tests: &tests, list_tables_separately: true, history_cfg: &hcfg, dedupe_cycle_reason: true, plan: &pcfg, dead: nodead(), helpers: nohelpers(), helpers_weight: 0.0, strings: nostrings(), clumps: noclumps(), clumps_weight: 0.0, clumps_prefix: "_", declared: nodeclared(), declared_weight: 0.0 }, 10, &Cfg::default(), &td());
        let reasons = |p: &str| r.hotspots.iter().find(|h| h.path == p).unwrap().reasons.clone();
        assert!(reasons("bot.rs").contains(&"9 commits in the last 6 months (0 fix commits, 0 authors (all bot commits))".to_string()), "{:?}", reasons("bot.rs"));
        assert!(reasons("bot.rs").contains(&"no non-bot author over the window (bus factor 0)".to_string()));
        assert!(reasons("solo.rs").contains(&"single author over the window (bus factor 1)".to_string()), "{:?}", reasons("solo.rs"));
        assert!(!reasons("pair.rs").iter().any(|r| r.contains("bus factor")), "{:?}", reasons("pair.rs"));
        assert!(!reasons("quiet.rs").iter().any(|r| r.contains("bus factor")), "{:?}", reasons("quiet.rs"));
    }

    #[test]
    fn table_pairs_list_under_tables_and_the_clone_reason_states_the_split() {
        use crate::clones::{FileClones, Loc};
        let sf = |p: &str| SourceFile { path: p.into(), lang: crate::lang::Language::Rust, kind: FileKind::Source, lines: 500, bytes: 0, content: String::new() };
        let loc = |file: &str, s: usize, e: usize, sym: &str| Loc { file: file.into(), start_line: s, end_line: e, symbol: sym.into() };
        let table = ClonePair { a: loc("a.rs", 91, 134, "CHECKS"), b: loc("a.rs", 136, 174, "CHECKS"), tokens: 117, kind: CloneKind::Table, container_kind: Some("array_expression".into()), entry_count: Some(6) };
        let logic = ClonePair { a: loc("a.rs", 200, 230, "run"), b: loc("b.rs", 10, 40, "go"), tokens: 150, kind: CloneKind::Logic, container_kind: None, entry_count: None };
        let mut clones = CloneReport { pairs: vec![logic, table], files: HashMap::new() };
        clones.files.insert("a.rs".into(), FileClones {
            clone_lines: 115, logic_clone_lines: 31, table_clone_lines: 84, clone_ratio: 0.23, pairs: 3,
            tables: vec![TableRef { symbol: Some("CHECKS".into()), container_kind: "array_expression".into(), start_line: 91, end_line: 174 }],
        });
        let files = vec![sf("a.rs"), sf("b.rs")];
        let fm = vec![fmetrics("a.rs", 1, 1, 0), fmetrics("b.rs", 1, 1, 0)];
        let deps = DepGraph::default();
        let tests = TestsCfg::default();
        let hcfg = hcfg();
        let pcfg = pcfg();
        let mentions = MentionIndex::default();
        let inputs = |sep: bool| Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm, functions: &[], deps: &deps, clones: &clones, cognitive_hard: 15, mentions: &mentions, tests: &tests, list_tables_separately: sep, history_cfg: &hcfg, dedupe_cycle_reason: true, plan: &pcfg, dead: nodead(), helpers: nohelpers(), helpers_weight: 0.0, strings: nostrings(), clumps: noclumps(), clumps_weight: 0.0, clumps_prefix: "_", declared: nodeclared(), declared_weight: 0.0 };
        let r = build(inputs(true), 10, &Cfg::default(), &td());
        assert_eq!((r.clones.len(), r.tables.len()), (1, 1));
        assert_eq!(r.clones[0].kind, CloneKind::Logic);
        let text = render(&r, 10);
        assert!(text.contains("  TABLES  (uniform entries on both sides, not duplicated logic)\n   117 tok  a.rs CHECKS table (91-134)  <->  CHECKS table (136-174)  (array_expression, 6 entries)\n"), "{text}");
        assert!(text.contains("\n   150 tok  a.rs run (200-230)  <->  b.rs go (10-40)\n"), "{text}");
        let a = r.hotspots.iter().find(|h| h.path == "a.rs").unwrap();
        assert_eq!((a.signals.logic_clone_lines, a.signals.table_clone_lines), (31, 84));
        assert!(a.reasons.iter().any(|r| r == "23% duplicated lines were 17% registry table (CHECKS, lines 91-174) and 6% logic; largest: run (200-230) <-> b.rs go (10-40), 150 tokens"), "{:?}", a.reasons);
        // The plan: pairs largest first, both symbols, `keep one` only within the file.
        let plan: Vec<&str> = a.plan.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(plan, vec!["run (200-230) duplicates b.rs go (10-40), 150 tokens", "CHECKS table (91-134) duplicates CHECKS table (136-174), 117 tokens: keep one"]);
        assert!(text.contains("        PLAN\n          1) run (200-230) duplicates b.rs go (10-40), 150 tokens\n          2) CHECKS table (91-134) duplicates CHECKS table (136-174), 117 tokens: keep one\n"), "{text}");
        assert!(!render_with(&r, 10, false).contains("PLAN"));
        // Inline: one list, the table pair annotated, no sub-heading.
        let r = build(inputs(false), 10, &Cfg::default(), &td());
        assert_eq!((r.clones.len(), r.tables.len()), (2, 0));
        let text = render(&r, 10);
        assert!(!text.contains("TABLES") && text.contains("CHECKS table (136-174)  (array_expression, 6 entries)"), "{text}");
    }

    #[test]
    fn test_clone_runs_fold_into_one_step_and_never_canonicalise() {
        use crate::clones::Loc;
        use crate::plan::StepKind;
        let sf = |p: &str, lines: usize| SourceFile { path: p.into(), lang: crate::lang::Language::Rust, kind: FileKind::Source, lines, bytes: 0, content: String::new() };
        let loc = |file: &str, s: usize, e: usize, sym: &str| Loc { file: file.into(), start_line: s, end_line: e, symbol: sym.into() };
        let pair = |a: Loc, b: Loc, tokens: usize| ClonePair { a, b, tokens, kind: CloneKind::Logic, container_kind: None, entry_count: None };
        // a.rs has a test module at 100-200; c.rs is 95% inline tests, so a Test file for ranking.
        let files = vec![sf("a.rs", 300), sf("b.rs", 100), sf("c.rs", 100)];
        let mut a = fmetrics("a.rs", 1, 1, 0);
        a.inline_test_lines = 101;
        a.test_regions = vec![region(RegionKind::CfgTestMod, 100, 200)];
        let mut c = fmetrics("c.rs", 1, 1, 0);
        c.inline_test_lines = 95;
        c.test_regions = vec![region(RegionKind::CfgTestMod, 6, 100)];
        let fm = vec![a, fmetrics("b.rs", 1, 1, 0), c];
        let clones = CloneReport {
            pairs: vec![
                pair(loc("a.rs", 120, 130, "t_one"), loc("a.rs", 150, 160, "t_two"), 100),
                pair(loc("a.rs", 10, 20, "alpha"), loc("c.rs", 50, 60, "t_far"), 90),
                pair(loc("a.rs", 30, 40, "gamma"), loc("b.rs", 5, 15, "delta"), 80),
            ],
            files: HashMap::new(),
        };
        let (deps, tests, hcfg, mentions, pcfg) = (DepGraph::default(), TestsCfg::default(), hcfg(), MentionIndex::default(), pcfg());
        fn constrain<'a, F: Fn(&'a PlanCfg) -> Inputs<'a>>(f: F) -> F { f }
        let inputs = constrain(|pcfg| Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm, functions: &[], deps: &deps, clones: &clones, cognitive_hard: 15, mentions: &mentions, tests: &tests, list_tables_separately: true, history_cfg: &hcfg, dedupe_cycle_reason: true, plan: pcfg, dead: nodead(), helpers: nohelpers(), helpers_weight: 0.0, strings: nostrings(), clumps: noclumps(), clumps_weight: 0.0, clumps_prefix: "_", declared: nodeclared(), declared_weight: 0.0 });
        let r = build(inputs(&pcfg), 10, &Cfg::default(), &td());
        assert!(r.hotspots.iter().all(|h| h.path != "c.rs"));
        let a = r.hotspots.iter().find(|h| h.path == "a.rs").unwrap();
        // alpha's duplicate lies in c.rs, a Test file for ranking: alpha's own side is source, so it keeps its step.
        assert_eq!(a.plan.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), vec![
            "fold 2 clone runs in #[cfg(test)] mod at 100-200 into shared test helpers",
            "alpha (10-20) duplicates c.rs t_far (50-60), 90 tokens",
            "gamma (30-40) duplicates b.rs delta (5-15), 80 tokens",
        ], "{:?}", a.plan);
        assert_eq!((a.plan[0].kind, a.plan_more), (StepKind::FoldTestClones, 0));
        let b = r.hotspots.iter().find(|h| h.path == "b.rs").unwrap();
        assert_eq!(b.plan.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), vec!["delta (5-15) duplicates a.rs gamma (30-40), 80 tokens"]);
        // Folding off: the test runs leave the plan without becoming canonicalise steps.
        let no_fold = PlanCfg { fold_test_clones: false, ..PlanCfg::default() };
        let r = build(inputs(&no_fold), 10, &Cfg::default(), &td());
        let a = r.hotspots.iter().find(|h| h.path == "a.rs").unwrap();
        assert_eq!(a.plan.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), vec!["alpha (10-20) duplicates c.rs t_far (50-60), 90 tokens", "gamma (30-40) duplicates b.rs delta (5-15), 80 tokens"]);
    }

    #[test]
    fn hidden_coupling_prints_lift_and_sweeps_and_explained_pairs_leave_the_list() {
        use crate::history::{CoChange, FileHistory, SweepCommit};
        let sf = |p: &str| SourceFile { path: p.into(), lang: crate::lang::Language::Rust, kind: FileKind::Source, lines: 100, bytes: 0, content: String::new() };
        let names = ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs", "g.rs", "x.rs"];
        let files: Vec<SourceFile> = names.iter().map(|p| sf(p)).collect();
        let fm: Vec<FileMetrics> = names.iter().map(|p| fmetrics(p, 1, 1, 0)).collect();
        let pair = |a: &str, b: &str, together, nonsweep, strength, lift| CoChange { a: a.into(), b: b.into(), together, together_nonsweep: nonsweep, strength, lift };
        let sweep = |hash: &str| SweepCommit { hash: hash.into(), subject: "sweep".into(), files: 5, dir: "".into(), dir_touched: 5, dir_files: 5 };
        let v = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        let mut hist = History {
            window: "6 months ago".into(), commits_scanned: 30, sweep_commits: 2, lift_applied: true, sweeps: vec![sweep("h1"), sweep("h2")],
            // a.rs has two hidden partners (b, e) and two explained ones (g, c): one budget of two.
            co_changes: vec![
                pair("a.rs", "b.rs", 6, 4, 0.8, 3.5), pair("c.rs", "d.rs", 5, 5, 1.0, 4.0), pair("a.rs", "d.rs", 3, 3, 0.5, 3.2),
                pair("a.rs", "e.rs", 3, 3, 0.5, 3.1), pair("a.rs", "g.rs", 5, 3, 0.5, 3.0), pair("a.rs", "c.rs", 3, 3, 0.5, 3.0),
            ],
            // c and d shipped together five times; x.rs went along in four of them. a with g and
            // with c three times each, x.rs along twice.
            co_commits: vec![
                v(&["c.rs", "d.rs", "x.rs"]), v(&["c.rs", "d.rs", "x.rs"]), v(&["c.rs", "d.rs", "x.rs"]), v(&["c.rs", "d.rs", "x.rs"]), v(&["c.rs", "d.rs"]), v(&["a.rs", "b.rs"]),
                v(&["a.rs", "g.rs", "x.rs"]), v(&["a.rs", "g.rs", "x.rs"]), v(&["a.rs", "g.rs"]), v(&["a.rs", "c.rs", "x.rs"]), v(&["a.rs", "c.rs", "x.rs"]), v(&["a.rs", "c.rs"]), v(&["a.rs", "e.rs"]),
            ],
            ..History::default()
        };
        for p in names {
            let mut fh = FileHistory::default();
            fh.commits = 8;
            hist.files.insert(p.into(), fh);
        }
        let mut deps = DepGraph::default();
        deps.add_edge("c.rs", "x.rs");
        deps.add_edge("d.rs", "x.rs");
        deps.add_edge("a.rs", "x.rs");
        deps.add_edge("g.rs", "x.rs");
        deps.add_edge("a.rs", "d.rs"); // a<->d import each other: not hidden at all
        let clones = CloneReport::default();
        let tests = TestsCfg::default();
        let mentions = MentionIndex::default();
        let dflt = HistoryCfg::default();
        let pcfg = pcfg();
        let strict = HistoryCfg { explained_min_share: 0.9, ..HistoryCfg::default() };
        fn constrain<'a, F: Fn(&'a HistoryCfg) -> Inputs<'a>>(f: F) -> F { f }
        let inputs = constrain(|hcfg| Inputs { root: String::new(), files: &files, history: Some(&hist), file_metrics: &fm, functions: &[], deps: &deps, clones: &clones, cognitive_hard: 15, mentions: &mentions, tests: &tests, list_tables_separately: true, history_cfg: hcfg, dedupe_cycle_reason: true, plan: &pcfg, dead: nodead(), helpers: nohelpers(), helpers_weight: 0.0, strings: nostrings(), clumps: noclumps(), clumps_weight: 0.0, clumps_prefix: "_", declared: nodeclared(), declared_weight: 0.0 });
        let r = build(inputs(&dflt), 10, &Cfg::default(), &td());
        assert_eq!(r.hidden_coupling.iter().map(|h| (h.a.as_str(), h.b.as_str(), h.together, h.together_nonsweep)).collect::<Vec<_>>(), vec![("a.rs", "b.rs", 6, 4), ("a.rs", "e.rs", 3, 3)]);
        assert!(r.hidden_coupling.iter().all(|h| h.explained_by.is_none()));
        let e = &r.explained_coupling;
        assert_eq!(e.iter().map(|h| (h.a.as_str(), h.b.as_str(), h.explained_by.as_deref(), h.explained_commits)).collect::<Vec<_>>(), vec![("c.rs", "d.rs", Some("x.rs"), 4), ("a.rs", "g.rs", Some("x.rs"), 2), ("a.rs", "c.rs", Some("x.rs"), 2)]);
        assert_eq!(r.summary.sweep_commits, 2);
        assert_eq!(r.sweep_note.as_deref(), Some("2 directory-sweep commits (>= 50% of .) excluded from pair counts; still counted as churn"));
        let reasons = |p: &str| r.hotspots.iter().find(|h| h.path == p).unwrap().reasons.iter().filter(|x| x.contains("changes together")).cloned().collect::<Vec<_>>();
        let reason = |p: &str| reasons(p).first().cloned().unwrap_or_default();
        assert_eq!(reason("a.rs"), "changes together with b.rs (4x, 3.5x more often than chance; 2 sweep commits ignored) but neither imports the other");
        assert_eq!(reason("b.rs"), "changes together with a.rs (4x, 3.5x more often than chance; 2 sweep commits ignored) but neither imports the other");
        assert_eq!(reason("c.rs"), "changes together with d.rs (5x): both import x.rs, which changed in 4 of those commits");
        assert_eq!(reason("x.rs"), "");
        // Four partners, a budget of two: the hidden ones first, the explained ones only when it stretches.
        assert_eq!(reasons("a.rs").len(), 2, "{:?}", reasons("a.rs"));
        assert_eq!(reasons("a.rs")[1], "changes together with e.rs (3x, 3.1x more often than chance) but neither imports the other");
        let text = render(&r, 10);
        assert!(text.contains("   4x 0.80  lift  3.5  a.rs  <->  b.rs  (2 sweep commits ignored)\n   3x 0.50  lift  3.1  a.rs  <->  e.rs\n  EXPLAINED  (both import a file that changed in the same commits: shotgun surgery on it)\n   5x 1.00  c.rs  <->  d.rs  via x.rs (4 of 5)\n   3x 0.50  a.rs  <->  g.rs  via x.rs (2 of 3)  (2 sweep commits ignored)\n   3x 0.50  a.rs  <->  c.rs  via x.rs (2 of 3)\n  2 directory-sweep commits (>= 50% of .) excluded from pair counts; still counted as churn\n"), "{text}");
        let wide = Cfg { reason_hidden_partners: 3, ..Cfg::default() };
        let r3 = build(inputs(&dflt), 10, &wide, &td());
        let a3: Vec<&String> = r3.hotspots.iter().find(|h| h.path == "a.rs").unwrap().reasons.iter().filter(|x| x.contains("changes together")).collect();
        assert_eq!(a3.len(), 3);
        assert_eq!(a3[2], "changes together with g.rs (3x): both import x.rs, which changed in 2 of those commits");
        // A stricter share leaves every pair unexplained: hidden, c<->d with no sweep clause (none were ignored).
        let r = build(inputs(&strict), 10, &Cfg::default(), &td());
        assert_eq!(r.hidden_coupling.len(), 5);
        assert!(r.explained_coupling.is_empty());
        let reason = |p: &str| r.hotspots.iter().find(|h| h.path == p).unwrap().reasons.iter().find(|x| x.contains("changes together")).cloned().unwrap_or_default();
        assert_eq!(reason("c.rs"), "changes together with d.rs (5x, 4.0x more often than chance) but neither imports the other");
        // No sweeps: no footer. No hidden pairs but explained ones: no "none" either.
        hist.sweep_commits = 0;
        hist.sweeps.clear();
        hist.co_changes.retain(|c| c.a == "c.rs");
        let r = build(Inputs { root: String::new(), files: &files, history: Some(&hist), file_metrics: &fm, functions: &[], deps: &deps, clones: &clones, cognitive_hard: 15, mentions: &mentions, tests: &tests, list_tables_separately: true, history_cfg: &dflt, dedupe_cycle_reason: true, plan: &pcfg, dead: nodead(), helpers: nohelpers(), helpers_weight: 0.0, strings: nostrings(), clumps: noclumps(), clumps_weight: 0.0, clumps_prefix: "_", declared: nodeclared(), declared_weight: 0.0 }, 10, &Cfg::default(), &td());
        let text = render(&r, 10);
        assert!(r.sweep_note.is_none() && !text.contains("directory-sweep"));
        assert!(r.hidden_coupling.is_empty() && r.explained_coupling.len() == 1);
        assert!(text.contains("HIDDEN COUPLING  (change together, no import between them)\n  EXPLAINED"), "{text}");
    }

    #[test]
    fn cycle_reason_is_deduped_and_cycles_print_one_block_each() {
        use crate::deps::{Cuts, Edge, EdgeCut, EdgeKind, HubCut};
        let sf = |p: &str| SourceFile { path: p.into(), lang: crate::lang::Language::Rust, kind: FileKind::Source, lines: 100, bytes: 0, content: String::new() };
        let files = vec![sf("src/paths.rs"), sf("src/plan.rs"), sf("src/scan.rs"), sf("src/ctx.rs"), sf("src/t/a.ts"), sf("src/jsx/a.ts"), sf("src/jsx/c.ts"), sf("top.rs")];
        let fm: Vec<FileMetrics> = files.iter().map(|f| fmetrics(&f.path, 1, 1, 0)).collect();
        let edge = |from: &str, to: &str, names: &[&str]| Edge { from: from.into(), to: to.into(), kind: EdgeKind::Use, symbols: names.len() as u32, names: names.iter().map(|s| s.to_string()).collect(), glob: false, line: 16 };
        let mut deps = DepGraph::default();
        deps.file_cycles.push(crate::deps::Cycle {
            members: files[..4].iter().map(|f| f.path.clone()).collect(),
            dir: "src".into(),
            cuts: Some(Cuts {
                internal_edges: 9, mod_edges: 0, type_only_edges: 0, base: 4,
                single: Some(EdgeCut { edge: edge("src/paths.rs", "src/plan.rs", &["EntityRef"]), largest_after: 4 }),
                hub: Some(HubCut { member: "src/paths.rs".into(), imports: 4, symbols: 9, glob: false, largest_after: 3 }),
                cut_set: vec![EdgeCut { edge: edge("src/paths.rs", "src/plan.rs", &["EntityRef"]), largest_after: 4 }, EdgeCut { edge: edge("src/plan.rs", "src/scan.rs", &["ScanToken", "Tok"]), largest_after: 3 }],
                no_single_break: true,
            }),
        });
        deps.file_cycles.push(crate::deps::Cycle { members: vec!["src/a.rs".into(), "src/b.rs".into()], dir: "src".into(), cuts: None });
        // Carried by type-only imports only, in part, and across the repo root.
        let members = |dir: &str, n: usize| (0..n).map(|i| format!("{dir}/{}.ts", (b'a' + i as u8) as char)).collect::<Vec<_>>();
        let no_cuts = |base: usize, type_only: usize, single: Option<EdgeCut>| Cuts { internal_edges: 12, mod_edges: 0, type_only_edges: type_only, base, single, hub: None, cut_set: vec![], no_single_break: false };
        deps.file_cycles.push(crate::deps::Cycle { members: members("src/t", 6), dir: "src/t".into(), cuts: Some(no_cuts(1, 8, None)) });
        deps.file_cycles.push(crate::deps::Cycle { members: members("src/jsx", 6), dir: "src/jsx".into(), cuts: Some(no_cuts(4, 3, Some(EdgeCut { edge: edge("src/jsx/a.ts", "src/jsx/b.ts", &["X"]), largest_after: 2 }))) });
        deps.file_cycles.push(crate::deps::Cycle { members: vec!["src/b.rs".into(), "top.rs".into()], dir: ".".into(), cuts: None });
        let clones = CloneReport::default();
        let tests = TestsCfg::default();
        let hcfg = hcfg();
        let pcfg = pcfg();
        let mentions = MentionIndex::default();
        let inputs = |dedupe: bool| Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm, functions: &[], deps: &deps, clones: &clones, cognitive_hard: 15, mentions: &mentions, tests: &tests, list_tables_separately: true, history_cfg: &hcfg, dedupe_cycle_reason: dedupe, plan: &pcfg, dead: nodead(), helpers: nohelpers(), helpers_weight: 0.0, strings: nostrings(), clumps: noclumps(), clumps_weight: 0.0, clumps_prefix: "_", declared: nodeclared(), declared_weight: 0.0 };
        let r = build(inputs(true), 10, &Cfg::default(), &td());
        let reason = |p: &str| r.hotspots.iter().find(|h| h.path == p).unwrap().reasons.iter().find(|x| x.contains("cycle")).cloned().unwrap_or_default();
        assert_eq!(reason("src/paths.rs"), "in the 4-file src cycle (cut: paths.rs -> plan.rs, EntityRef)");
        assert_eq!(reason("src/plan.rs"), "in the 4-file src cycle (cut: paths.rs -> plan.rs, EntityRef)");
        assert_eq!(reason("src/scan.rs"), "in the 4-file src cycle");
        assert_eq!(reason("src/t/a.ts"), "in the 6-file src/t cycle (type-only, no runtime cycle)");
        assert_eq!(reason("src/jsx/a.ts"), "in the 6-file src/jsx cycle (runtime cycle 4; cut: a.ts -> b.ts, X)");
        assert_eq!(reason("src/jsx/c.ts"), "in the 6-file src/jsx cycle (runtime cycle 4)");
        assert_eq!(reason("top.rs"), "in the 2-file top-level cycle");
        let text = render(&r, 10);
        assert!(text.contains("\nCYCLES\n  4-file cycle in src: cheapest cut src/paths.rs -> src/plan.rs imports 1 symbol (EntityRef, line 16) -> largest remaining cycle 4; hub cut: drop paths.rs's 4 imports (9 symbols) -> 3; no single import breaks this cycle\n    files: src/paths.rs, src/plan.rs, src/scan.rs, src/ctx.rs\n    cut set of 2 imports dissolves it: src/paths.rs -> src/plan.rs (EntityRef), src/plan.rs -> src/scan.rs (ScanToken, Tok)\n  2-file cycle in src\n    files: src/a.rs, src/b.rs\n"), "{text}");
        assert!(text.contains("\n  6-file cycle in src/t: 8 type-only imports ignored, largest runtime cycle 1; no runtime cycle\n"), "{text}");
        assert!(text.contains("\n  6-file cycle in src/jsx: 3 type-only imports ignored, largest runtime cycle 4; cheapest cut src/jsx/a.ts -> src/jsx/b.ts imports 1 symbol (X, line 16) -> largest remaining cycle 2\n"), "{text}");
        assert!(text.contains("\n  2-file cycle at the repo root\n    files: src/b.rs, top.rs\n"), "{text}");
        // Without dedupe: the old count line on every member.
        let r = build(inputs(false), 10, &Cfg::default(), &td());
        assert!(r.hotspots.iter().all(|h| h.reasons.iter().any(|x| x.starts_with("in an import cycle of "))), "{:?}", r.hotspots);
        assert!(r.hotspots.iter().find(|h| h.path == "src/scan.rs").unwrap().reasons.contains(&"in an import cycle of 4 files".to_string()));
    }

    #[test]
    fn test_units_gate_the_multiplier_and_the_unmentioned_reason() {
        use crate::deps::FileDeps;
        use crate::mentions::FileMentions;
        let sf = |p: &str| SourceFile { path: p.into(), lang: crate::lang::Language::Rust, kind: FileKind::Source, lines: 100, bytes: 0, content: String::new() };
        let sym = |name: &str, line: usize| Symbol { name: name.into(), kind: "fn", start_line: line, end_line: line + 9, public: true };
        // named.rs: 36 units name it, all symbols named. gap.rs: 2 units, 4 of 6 public symbols unnamed.
        // imported.rs: no unit names it but a test file imports it. stem.rs: same-stem test in its tree.
        // silent.rs: nothing at all, 2 public symbols (below the share rule's minimum).
        // many.rs: 8 of 8 unnamed, so the reason lists five and counts the rest.
        let files = vec![sf("src/named.rs"), sf("src/gap.rs"), sf("src/imported.rs"), sf("src/stem.rs"), sf("src/silent.rs"), sf("src/many.rs"),
            SourceFile { path: "tests/stem.rs".into(), lang: crate::lang::Language::Rust, kind: FileKind::Test, lines: 10, bytes: 0, content: String::new() }];
        let fm: Vec<FileMetrics> = files.iter().map(|f| fmetrics(&f.path, 1, 1, 0)).collect();
        let mut mentions = MentionIndex::default();
        mentions.files.insert("src/named.rs".into(), FileMentions { test_units: 36, test_file_units: 36, symbols: 4, public_symbols: 4, ..Default::default() });
        mentions.files.insert("src/gap.rs".into(), FileMentions { test_units: 2, test_file_units: 2, symbols: 7, public_symbols: 6, unmentioned: vec![sym("router", 204), sym("request_ctx", 243), sym("serve_forever", 260), sym("shutdown_now", 280)], ..Default::default() });
        mentions.files.insert("src/imported.rs".into(), FileMentions { symbols: 1, public_symbols: 1, unmentioned: vec![sym("load_all", 1)], ..Default::default() });
        mentions.files.insert("src/stem.rs".into(), FileMentions::default());
        mentions.files.insert("src/silent.rs".into(), FileMentions { symbols: 2, public_symbols: 2, unmentioned: vec![sym("alpha_fn", 1), sym("beta_fn", 20)], ..Default::default() });
        mentions.files.insert("src/many.rs".into(), FileMentions { symbols: 8, public_symbols: 8, unmentioned: (0..8).map(|i| sym(&format!("thing_{i}"), 10 * i + 1)).collect(), ..Default::default() });
        let mut deps = DepGraph::default();
        deps.files.insert("src/imported.rs".into(), FileDeps { test_refs: 1, ..Default::default() });
        let clones = CloneReport::default();
        let tests = TestsCfg::default();
        let hcfg = hcfg();
        let pcfg = pcfg();
        fn constrain<'a, F: Fn(&'a TestsCfg) -> Inputs<'a>>(f: F) -> F { f }
        let inputs = constrain(|tests| Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm, functions: &[], deps: &deps, clones: &clones, mentions: &mentions, cognitive_hard: 15, tests, list_tables_separately: true, history_cfg: &hcfg, dedupe_cycle_reason: true, plan: &pcfg, dead: nodead(), helpers: nohelpers(), helpers_weight: 0.0, strings: nostrings(), clumps: noclumps(), clumps_weight: 0.0, clumps_prefix: "_", declared: nodeclared(), declared_weight: 0.0 });
        let r = build(inputs(&tests), 10, &Cfg::default(), &td());
        let hot = |p: &str| r.hotspots.iter().find(|h| h.path == p).unwrap();
        let units = |p: &str| hot(p).signals.test_units;
        assert_eq!((units("src/named.rs"), units("src/gap.rs"), units("src/imported.rs"), units("src/stem.rs"), units("src/silent.rs")), (36, 2, 1, 1, 0));
        // Every signal is equal, so the multiplier is the only difference in score: silent and many pay it.
        let base = hot("src/named.rs").score;
        assert!((hot("src/silent.rs").score - base * 1.15).abs() < 1e-9, "{} vs {base}", hot("src/silent.rs").score);
        assert_eq!(hot("src/imported.rs").score, base);
        let reasons = |p: &str| hot(p).reasons.clone();
        assert_eq!(reasons("src/named.rs"), Vec::<String>::new());
        assert_eq!(reasons("src/gap.rs"), vec!["4 of 6 public symbols are named by no test: router (lines 204-213), request_ctx (lines 243-252), serve_forever (lines 260-269), shutdown_now (lines 280-289)"]);
        assert_eq!(reasons("src/imported.rs"), Vec::<String>::new()); // one symbol: below the minimum
        assert_eq!(reasons("src/silent.rs"), vec!["no test unit names any of its symbols"]);
        assert_eq!(reasons("src/many.rs"), vec!["no test unit names any of its symbols", "8 of 8 public symbols are named by no test: thing_0 (lines 1-10), thing_1 (lines 11-20), thing_2 (lines 21-30), thing_3 (lines 31-40), thing_4 (lines 41-50), +3 more"]);
        assert_eq!(hot("src/many.rs").unmentioned.len(), 8);
        assert!(!render(&r, 10).contains("no test file references it"));
        // A stricter share or a higher minimum turns the per-file reason off; the no-unit reason stays.
        let strict = TestsCfg { unmentioned_share_reason: 0.7, ..TestsCfg::default() };
        let r2 = build(inputs(&strict), 10, &Cfg::default(), &td());
        assert!(r2.hotspots.iter().find(|h| h.path == "src/gap.rs").unwrap().reasons.is_empty());
        let big = TestsCfg { unmentioned_min_symbols: 9, ..TestsCfg::default() };
        let r3 = build(inputs(&big), 10, &Cfg::default(), &td());
        assert_eq!(r3.hotspots.iter().find(|h| h.path == "src/many.rs").unwrap().reasons, vec!["no test unit names any of its symbols"]);
    }

    #[test]
    fn test_scope_stops_at_the_first_test_dir() {
        assert_eq!(test_scope("backend/tests/unit/test_x.py", &td()), "backend");
        assert_eq!(test_scope("src/a/x.test.ts", &td()), "src/a");
        assert_eq!(test_scope("tests/test_x.py", &td()), "");
        assert_eq!(test_scope("test_x.py", &td()), "");
        assert_eq!(test_scope("src/a/__tests__/x.test.ts", &td()), "src/a");
    }

    #[test]
    fn weights_change_the_ranking() {
        let sf = |p: &str, lines: usize| SourceFile {
            path: p.into(), lang: crate::lang::Language::Python, kind: FileKind::Source, lines, bytes: 0, content: String::new(),
        };
        let files = vec![sf("big.py", 1000), sf("small.py", 10)];
        let fm = vec![
            fmetrics("big.py", 1, 1, 0),
            fmetrics("small.py", 30, 30, 1),
        ];
        let deps = DepGraph::default();
        let clones = CloneReport::default();
        let tests = TestsCfg::default();
        let hcfg = hcfg();
        let pcfg = pcfg();
        let mentions = MentionIndex::default();
        let inputs = || Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm, functions: &[], deps: &deps, clones: &clones, cognitive_hard: 15, mentions: &mentions, tests: &tests, list_tables_separately: true, history_cfg: &hcfg, dedupe_cycle_reason: true, plan: &pcfg, dead: nodead(), helpers: nohelpers(), helpers_weight: 0.0, strings: nostrings(), clumps: noclumps(), clumps_weight: 0.0, clumps_prefix: "_", declared: nodeclared(), declared_weight: 0.0 };
        let by_default = build(inputs(), 10, &Cfg::default(), &td());
        assert_eq!(by_default.hotspots[0].path, "small.py");
        let size_only = Cfg {
            without_history: Weights { hotspot: 0.0, fixes: 0.0, complexity: 0.0, coupling: 0.0, clones: 0.0, size: 1.0, dead: 0.0, strings: 0.0 },
            ..Cfg::default()
        };
        let by_size = build(inputs(), 10, &size_only, &td());
        assert_eq!(by_size.hotspots[0].path, "big.py");
    }

    #[test]
    fn stem_match_is_scoped_to_the_test_dir_tree() {
        let sf = |p: &str, kind: FileKind| SourceFile {
            path: p.into(), lang: crate::lang::Language::Python, kind, lines: 1, bytes: 0, content: String::new(),
        };
        let files = vec![
            sf("a/utils.py", FileKind::Source),
            sf("b/utils.py", FileKind::Source),
            sf("b/sub/utils.py", FileKind::Source),
            sf("b/tests/test_utils.py", FileKind::Test),
        ];
        let t = test_stems(&files, &td());
        assert!(!stem_tested(&t, &files[0]));
        assert!(stem_tested(&t, &files[1]));
        assert!(stem_tested(&t, &files[2]));
    }
}

/// `in the 15-file src cycle (cut: paths.rs -> plan.rs, EntityRef)`: the cut clause only on the
/// two files of the cheapest cut edge. A cycle that type-only imports carry in part says so,
/// `(type-only, no runtime cycle)` or `(runtime cycle 4; cut: …)`, so the reason agrees with the
/// CYCLES block. Without dedupe, `in an import cycle of 15 files`.
fn cycle_reason(c: &Cycle, path: &str, dedupe: bool) -> String {
    if !dedupe {
        return format!("in an import cycle of {} files", c.members.len());
    }
    let mut notes: Vec<String> = Vec::new();
    if let Some(cuts) = &c.cuts && cuts.base < c.members.len() {
        notes.push(if cuts.base < 2 { "type-only, no runtime cycle".to_string() } else { format!("runtime cycle {}", cuts.base) });
    }
    notes.extend(c.cut_clause(path));
    let note = if notes.is_empty() { String::new() } else { format!(" ({})", notes.join("; ")) };
    let dir = if c.dir == "." { "top-level" } else { c.dir.as_str() };
    format!("in the {}-file {dir} cycle{note}", c.members.len())
}

/// `; 2 sweep commits ignored` inside a hidden-coupling reason, empty when none were.
fn sweeps_ignored(h: &HiddenCoupling) -> String {
    match h.together - h.together_nonsweep {
        0 => String::new(),
        1 => "; 1 sweep commit ignored".to_string(),
        n => format!("; {n} sweep commits ignored"),
    }
}

/// `  (array_expression, 6 entries)` after a table pair.
pub fn table_note(c: &ClonePair) -> String {
    match (&c.container_kind, c.entry_count) {
        (Some(k), Some(n)) => format!("  ({k}, {n} entries)"),
        _ => String::new(),
    }
}

/// ` 239 tok  src/cmd/flow.rs park (932-962)  <->  drop_ticket (1010-1036)`: side B names its
/// file only when it differs, a table pair says `table` after each symbol and ends with the
/// container note.
pub fn pair_line(c: &ClonePair) -> String {
    let table = c.kind == CloneKind::Table;
    format!("{:>4} tok  {}  <->  {}{}", c.tokens, plan::side_text(&c.a, "", table), plan::side_text(&c.b, &c.a.file, table), table_note(c))
}

/// The `PLAN` block under a hotspot, or nothing when the plan is empty.
fn render_plan(h: &Hotspot) -> String {
    if h.plan.is_empty() && h.plan_more == 0 {
        return String::new();
    }
    let mut o = "        PLAN\n".to_string();
    for s in &h.plan {
        o.push_str(&format!("          {}) {}\n", s.n, s.text));
    }
    if h.plan_more > 0 {
        o.push_str(&format!("          (+{} more)\n", h.plan_more));
    }
    o
}

#[cfg(test)]
pub fn render(r: &Report, top: usize) -> String {
    render_with(r, top, true)
}

/// `render`, with the per-hotspot PLAN block only when `[plan].include_in_text_report`.
pub fn render_with(r: &Report, top: usize, plans: bool) -> String {
    use std::fmt::Write;
    let mut o = String::new();
    let s = &r.summary;
    let _ = writeln!(o, "scry scan of {}", s.root);
    let _ = writeln!(
        o,
        "{} source files ({} lines), {} test files, {} functions ({} over cognitive {})",
        s.source_files, s.source_lines, s.test_files, s.functions, s.complex_functions, s.cognitive_hard
    );
    match &s.history_window {
        Some(w) => { let _ = writeln!(o, "history: {} commits since {w}", s.commits_scanned); }
        None => { let _ = writeln!(o, "history: unavailable (not a git repo or --no-history)"); }
    }

    let _ = writeln!(o, "\nHOTSPOTS  (score = churn x complexity, boosted by fixes, coupling, clones, missing tests)");
    for (i, h) in r.hotspots.iter().take(top).enumerate() {
        let note = h.inline_test_note.as_ref().map(|n| format!(", {n}")).unwrap_or_default();
        let _ = writeln!(o, "{:>2}. {:>5.1}  {}  ({} lines{note})", i + 1, h.score, h.path, h.signals.lines);
        for reason in &h.reasons {
            let _ = writeln!(o, "        - {reason}");
        }
        if plans {
            o.push_str(&render_plan(h));
        }
    }

    let _ = writeln!(o, "\nCYCLES");
    if r.dir_cycles.is_empty() && r.file_cycles.is_empty() {
        let _ = writeln!(o, "  none");
    }
    for c in r.dir_cycles.iter().take(top) {
        let _ = writeln!(o, "  dirs  {}", c.members.join(" <-> "));
    }
    for c in r.file_cycles.iter().take(top) {
        let _ = writeln!(o, "  {}", c.headline());
        let _ = writeln!(o, "    files: {}", c.members.join(", "));
        if let Some(l) = c.cut_set_line() {
            let _ = writeln!(o, "    {l}");
        }
    }

    let _ = writeln!(o, "\nHIDDEN COUPLING  (change together, no import between them)");
    if r.hidden_coupling.is_empty() && r.explained_coupling.is_empty() {
        let _ = writeln!(o, "  none");
    }
    // `  (2 sweep commits ignored)` after a row, so the printed count is auditable.
    let ignored_note = |h: &HiddenCoupling| {
        let ignored = sweeps_ignored(h);
        if ignored.is_empty() { String::new() } else { format!("  ({})", &ignored[2..]) }
    };
    for h in r.hidden_coupling.iter().take(top) {
        let _ = writeln!(o, "  {:>2}x {:.2}  lift {:>4.1}  {}  <->  {}{}", h.together_nonsweep, h.strength, h.lift, h.a, h.b, ignored_note(h));
    }
    if !r.explained_coupling.is_empty() {
        let _ = writeln!(o, "  EXPLAINED  (both import a file that changed in the same commits: shotgun surgery on it)");
        for h in r.explained_coupling.iter().take(top) {
            let _ = writeln!(o, "  {:>2}x {:.2}  {}  <->  {}  via {} ({} of {}){}", h.together_nonsweep, h.strength, h.a, h.b, h.explained_by.as_deref().unwrap_or(""), h.explained_commits, h.together_nonsweep, ignored_note(h));
        }
    }
    if let Some(n) = &r.sweep_note {
        let _ = writeln!(o, "  {n}");
    }

    let _ = writeln!(o, "\nCLONES  (largest near-exact duplicates)");
    if r.clones.is_empty() {
        let _ = writeln!(o, "  none");
    }
    for c in r.clones.iter().take(top) {
        let _ = writeln!(o, "  {}", pair_line(c));
    }
    if !r.tables.is_empty() {
        let _ = writeln!(o, "  TABLES  (uniform entries on both sides, not duplicated logic)");
        for c in r.tables.iter().take(top) {
            let _ = writeln!(o, "  {}", pair_line(c));
        }
    }

    let _ = writeln!(o, "\nDEAD SURFACE  (exported symbols with no production use outside their file; fields never read, variants never constructed)");
    if r.dead.totals.pub_items == 0 && r.dead.notes.is_empty() {
        let _ = writeln!(o, "  none");
    } else {
        let _ = writeln!(o, "  {}", dead::totals_line(&DeadReport { totals: r.dead.totals.clone(), ..DeadReport::default() }));
    }
    for n in &r.dead.notes {
        let _ = writeln!(o, "  {n}");
    }
    if !r.dead.files.is_empty() {
        let _ = writeln!(o, "  {:>5} {:>5}  {:>14}  path", "lines", "ratio", "d/t/i of items");
    }
    for d in r.dead.files.iter().take(top) {
        let _ = writeln!(o, "  {:>5} {:>4.0}%  {:>14}  {}", d.dead_lines, d.dead_ratio * 100.0, format!("{}/{}/{} of {}", d.dead_count, d.test_only_count, d.overexported_count, d.pub_items), d.path);
    }

    let _ = writeln!(o, "\nHELPERS  (same-name helpers defined in several files; small helper bodies inlined instead of called)");
    if r.helpers.totals.definitions == 0 {
        let _ = writeln!(o, "  none");
    } else {
        let _ = writeln!(o, "  {}", helpers::totals_line(&HelpersReport { totals: r.helpers.totals.clone(), ..HelpersReport::default() }));
    }
    for n in &r.helpers.notes {
        let _ = writeln!(o, "  {n}");
    }
    for f in r.helpers.families.iter().take(top) {
        let _ = writeln!(o, "  {}", f.line);
    }
    if !r.helpers.inlined.is_empty() {
        let _ = writeln!(o, "  INLINED  (helper bodies found as exact token sequences elsewhere)");
        for i in r.helpers.inlined.iter().take(top) {
            let _ = writeln!(o, "  {}", i.line);
        }
    }

    let _ = writeln!(o, "\nSTRINGS  (message literals spelled in several files; config literals with no shared constant)");
    if r.strings.totals.literals == 0 {
        let _ = writeln!(o, "  none");
    } else {
        let _ = writeln!(o, "  {}", strings::totals_line(&StringsReport { totals: r.strings.totals.clone(), ..StringsReport::default() }));
    }
    for n in &r.strings.notes {
        let _ = writeln!(o, "  {n}");
    }
    for f in r.strings.families.iter().take(top) {
        let _ = writeln!(o, "  {}", f.line);
    }
    if !r.strings.config.is_empty() {
        let _ = writeln!(o, "  CONFIG  (time formats, env names, URLs, MIME types, paths, numbers in one config role)");
        for f in r.strings.config.iter().take(top) {
            let _ = writeln!(o, "  {}", f.line);
        }
    }
    if !r.strings.near.is_empty() {
        let _ = writeln!(o, "  NEAR  (two spellings of one message, information only)");
        for p in r.strings.near.iter().take(top) {
            let _ = writeln!(o, "  {}", p.line);
        }
    }

    let _ = writeln!(o, "\nCLUMPS  (parameter tuples recurring across functions; slots no member reads)");
    if r.clumps.totals.functions == 0 {
        let _ = writeln!(o, "  none");
    } else {
        let _ = writeln!(o, "  {}", clumps::totals_line(&ClumpsReport { totals: r.clumps.totals.clone(), ..ClumpsReport::default() }, &r.clumps.unused_prefix));
    }
    for n in &r.clumps.notes {
        let _ = writeln!(o, "  {n}");
    }
    if r.clumps.totals.functions > 0 && r.clumps.clumps.is_empty() {
        let _ = writeln!(o, "  none");
    }
    for c in r.clumps.clumps.iter().take(top) {
        let _ = writeln!(o, "  {}", c.line);
    }

    let _ = writeln!(o, "\nDECLARED  (dependencies no file imports; feature flags nothing checks; config knobs no code reads)");
    o.push_str(&declared::render_section(&DeclaredReport {
        totals: r.declared.totals.clone(), notes: r.declared.notes.clone(), orphans: r.declared.orphans.clone(), misplaced: r.declared.misplaced.clone(),
        dead_features: r.declared.dead_features.clone(), noop_features: r.declared.noop_features.clone(), unread_knobs: r.declared.unread_knobs.clone(), ..DeclaredReport::default()
    }, top));

    let _ = writeln!(o, "\nDIRECTORIES  (by source lines)");
    let _ = writeln!(o, "  {:>5} {:>7} {:>7}  dir  (largest file)", "files", "lines", "cog");
    for d in r.directories.iter().take(top) {
        let _ = writeln!(o, "  {:>5} {:>7} {:>7}  {}  ({} @ {} lines)", d.files, d.lines, d.total_cognitive, d.dir, d.largest_file.rsplit('/').next().unwrap_or(""), d.largest_lines);
    }
    o
}
