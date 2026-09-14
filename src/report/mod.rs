//! The composite: fold every pass into one ranked, explained list.
//!
//! Ranking follows the hotspot idea (Tornhill): the files most likely to hurt
//! are the ones that are both hard to change *and* changed often. Each signal
//! is percentile-normalised within the repo so no thresholds need tuning per
//! language, and every ranked file carries the reasons it ranked, in words.

use crate::clones::{CloneKind, CloneReport, ClonePair, TableRef};
use crate::config::{History as HistoryCfg, Report as Cfg, Tests as TestsCfg, Weights};
use crate::deps::{Cycle, DepGraph};
use crate::discover::{FileKind, SourceFile};
use crate::history::{CoChange, History};
use crate::metrics::{FileMetrics, FunctionMetrics};
use crate::regions::{RegionKind, TestRegion};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

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
    pub has_tests: bool,
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
}

pub struct Inputs<'a> {
    pub root: String,
    pub files: &'a [SourceFile],
    pub history: Option<&'a History>,
    pub file_metrics: &'a [FileMetrics],
    pub functions: &'a [FunctionMetrics],
    pub deps: &'a DepGraph,
    pub clones: &'a CloneReport,
    pub cognitive_hard: u32,
    pub tests: &'a TestsCfg,
    /// `[clones].list_tables_separately`.
    pub list_tables_separately: bool,
    /// `explained_min_share` and `sweep_fraction` (for the sweep note).
    pub history_cfg: &'a HistoryCfg,
    /// `[deps].dedupe_cycle_reason`: the cycle is described once under CYCLES and members get
    /// `in the 15-file src cycle (cut: a -> b, Sym)` instead of the count on every one.
    pub dedupe_cycle_reason: bool,
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
/// 91-174) and 6% logic`.
fn clone_reason(s: &Signals, tables: &[TableRef]) -> String {
    if s.table_clone_lines == 0 {
        return format!("{:.0}% of its lines are duplicated elsewhere ({} lines)", s.clone_ratio * 100.0, s.clone_lines);
    }
    let denom = s.lines.saturating_sub(s.inline_test_lines).max(1) as f64;
    let pct = |n: usize| (100.0 * n as f64 / denom).round() as usize;
    let (t, l) = (pct(s.table_clone_lines), pct(s.logic_clone_lines));
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
        format!("{}% duplicated lines were all {desc} ({}), no logic", t + l, named.join("; "))
    } else {
        format!("{}% duplicated lines were {t}% {desc} ({}) and {l}% logic", t + l, named.join("; "))
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
fn percentiles<T: PartialOrd + Copy>(values: &[T]) -> Vec<f64> {
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

/// `1026 in #[cfg(test)] mod at 1283-2308`; every merged region's range when there are several.
fn inline_test_note(regions: &[TestRegion], inline_test_lines: usize) -> String {
    let biggest = regions.iter().max_by_key(|r| r.lines()).map(|r| r.kind);
    let what = match biggest {
        Some(RegionKind::CfgTestMod) => "#[cfg(test)] mod",
        Some(RegionKind::TestFn) => "#[test] fn",
        _ => "#[cfg(test)] item",
    };
    let ranges: Vec<String> = regions.iter().map(|r| format!("{}-{}", r.start_line, r.end_line)).collect();
    format!("{inline_test_lines} in {what} at {}", ranges.join(", "))
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
                has_tests: d.is_some_and(|d| d.test_refs > 0) || stem_tested(&tstems, f) || m.is_some_and(|m| !m.test_regions.is_empty()),
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
    // Commits that touched none of our files (a subdirectory scan of a larger
    // repo, a shallow clone) are not history we can rank on.
    let have_history = !hist.files.is_empty();

    // Worst functions per file, for the explanation. Units inside inline test regions are not
    // production code and never head a reason.
    let mut worst: HashMap<&str, Vec<&FunctionMetrics>> = HashMap::new();
    for f in inp.functions.iter().filter(|f| !f.in_test) {
        worst.entry(f.file.as_str()).or_default().push(f);
    }
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
            let base = w.hotspot * hotspot + w.fixes * fix + w.complexity * cx + w.coupling * coupling + w.clones * clone + w.size * p_lines[i];
            let score = 100.0 * base * if s.has_tests { 1.0 } else { cfg.no_tests_multiplier };

            let mut reasons = Vec::new();
            let window = hist.window.trim_end_matches(" ago");
            if have_history && p_commits[i] >= cfg.reason_churn_percentile && s.commits > 0 {
                reasons.push(format!(
                    "{} commits in the last {window} ({} fix commits, {} authors)",
                    s.commits, s.fix_commits, s.authors
                ));
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
            if s.clone_ratio >= cfg.reason_clone_ratio {
                reasons.push(clone_reason(s, inp.clones.files.get(&f.path).map_or(&[], |c| c.tables.as_slice())));
            }
            if let Some(hs) = hidden_by_file.get(f.path.as_str()) {
                for h in hs.iter().take(cfg.reason_hidden_partners) {
                    let other = if h.a == f.path { &h.b } else { &h.a };
                    reasons.push(format!("changes together with {other} ({}x, {:.1}x more often than chance{}) but neither imports the other", h.together_nonsweep, h.lift, sweeps_ignored(h)));
                }
            }
            if let Some(hs) = explained_by_file.get(f.path.as_str()) {
                for h in hs.iter().take(cfg.reason_hidden_partners) {
                    let other = if h.a == f.path { &h.b } else { &h.a };
                    let via = h.explained_by.as_deref().unwrap_or("");
                    reasons.push(format!("changes together with {other} ({}x): both import {via}, which changed in {} of those commits", h.together_nonsweep, h.explained_commits));
                }
            }
            if !s.has_tests {
                reasons.push("no test file references it".to_string());
            }
            if s.authors == 1 && s.commits >= cfg.reason_bus_factor_min_commits {
                reasons.push("single author over the window (bus factor 1)".to_string());
            }
            let test_regions = fm.get(f.path.as_str()).map(|m| m.test_regions.clone()).unwrap_or_default();
            let inline_test_note = (!test_regions.is_empty() && inline_ratio(f) >= inp.tests.report_inline_ratio_above)
                .then(|| inline_test_note(&test_regions, s.inline_test_lines));
            Hotspot {
                path: f.path.clone(),
                score,
                signals: s.clone(),
                reasons,
                worst_functions: worst.get(f.path.as_str()).map(|v| v.iter().map(|f| (*f).clone()).collect()).unwrap_or_default(),
                test_regions,
                inline_test_note,
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
        let size_only = Cfg {
            without_history: Weights { hotspot: 0.0, fixes: 0.0, complexity: 0.0, coupling: 0.0, clones: 0.0, size: 1.0 },
            ..Cfg::default()
        };
        let r = build(Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm, functions: &functions, deps: &deps, clones: &clones, cognitive_hard: 15, tests: &tests, list_tables_separately: true, history_cfg: &hcfg, dedupe_cycle_reason: true }, 10, &size_only, &td());
        let paths: Vec<&str> = r.hotspots.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec!["b.rs", "a.rs"], "{r:?}");
        let a = &r.hotspots[1];
        assert_eq!((a.signals.lines, a.signals.inline_test_lines), (2308, 1300));
        assert!(a.signals.has_tests);
        assert!(!r.hotspots[0].signals.has_tests);
        assert_eq!(a.inline_test_note.as_deref(), Some("1300 in #[cfg(test)] mod at 1009-2308"));
        assert_eq!(a.test_regions.len(), 1);
        assert_eq!(a.worst_functions.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), vec!["src"]);
        assert_eq!((r.summary.source_files, r.summary.test_files, r.summary.source_lines, r.summary.functions), (2, 1, 1008 + 1500, 1));
        assert_eq!((r.directories[0].lines, r.directories[0].largest_file.as_str(), r.directories[0].largest_lines), (2508, "b.rs", 1500));
        let text = render(&r, 10);
        assert!(text.contains("a.rs  (2308 lines, 1300 in #[cfg(test)] mod at 1009-2308)"), "{text}");
        assert!(text.contains("b.rs  (1500 lines)"), "{text}");
        // Below the ratio knob the note is absent; the region still counts as tests.
        let strict = TestsCfg { report_inline_ratio_above: 0.9, ..TestsCfg::default() };
        let r = build(Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm, functions: &functions, deps: &deps, clones: &clones, cognitive_hard: 15, tests: &strict, list_tables_separately: true, history_cfg: &hcfg, dedupe_cycle_reason: true }, 10, &size_only, &td());
        assert!(r.hotspots[1].inline_test_note.is_none());
        assert!(r.hotspots[1].signals.has_tests);
    }

    #[test]
    fn table_pairs_list_under_tables_and_the_clone_reason_states_the_split() {
        use crate::clones::{FileClones, Loc};
        let sf = |p: &str| SourceFile { path: p.into(), lang: crate::lang::Language::Rust, kind: FileKind::Source, lines: 500, bytes: 0, content: String::new() };
        let loc = |file: &str, s: usize, e: usize, sym: &str| Loc { file: file.into(), start_line: s, end_line: e, symbol: Some(sym.into()) };
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
        let inputs = |sep: bool| Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm, functions: &[], deps: &deps, clones: &clones, cognitive_hard: 15, tests: &tests, list_tables_separately: sep, history_cfg: &hcfg, dedupe_cycle_reason: true };
        let r = build(inputs(true), 10, &Cfg::default(), &td());
        assert_eq!((r.clones.len(), r.tables.len()), (1, 1));
        assert_eq!(r.clones[0].kind, CloneKind::Logic);
        let text = render(&r, 10);
        assert!(text.contains("  TABLES  (uniform entries on both sides, not duplicated logic)\n   117 tok  a.rs:91-134  <->  a.rs:136-174  (array_expression, 6 entries)\n"), "{text}");
        let a = r.hotspots.iter().find(|h| h.path == "a.rs").unwrap();
        assert_eq!((a.signals.logic_clone_lines, a.signals.table_clone_lines), (31, 84));
        assert!(a.reasons.iter().any(|r| r == "23% duplicated lines were 17% registry table (CHECKS, lines 91-174) and 6% logic"), "{:?}", a.reasons);
        // Inline: one list, the table pair annotated, no sub-heading.
        let r = build(inputs(false), 10, &Cfg::default(), &td());
        assert_eq!((r.clones.len(), r.tables.len()), (2, 0));
        let text = render(&r, 10);
        assert!(!text.contains("TABLES") && text.contains("a.rs:136-174  (array_expression, 6 entries)"), "{text}");
    }

    #[test]
    fn hidden_coupling_prints_lift_and_sweeps_and_explained_pairs_leave_the_list() {
        use crate::history::{CoChange, FileHistory, SweepCommit};
        let sf = |p: &str| SourceFile { path: p.into(), lang: crate::lang::Language::Rust, kind: FileKind::Source, lines: 100, bytes: 0, content: String::new() };
        let files = vec![sf("a.rs"), sf("b.rs"), sf("c.rs"), sf("d.rs"), sf("x.rs")];
        let fm: Vec<FileMetrics> = ["a.rs", "b.rs", "c.rs", "d.rs", "x.rs"].iter().map(|p| fmetrics(p, 1, 1, 0)).collect();
        let pair = |a: &str, b: &str, together, nonsweep, strength, lift| CoChange { a: a.into(), b: b.into(), together, together_nonsweep: nonsweep, strength, lift };
        let sweep = |hash: &str| SweepCommit { hash: hash.into(), subject: "sweep".into(), files: 5, dir: "".into(), dir_touched: 5, dir_files: 5 };
        let v = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        let mut hist = History {
            window: "6 months ago".into(), commits_scanned: 30, sweep_commits: 2, lift_applied: true, sweeps: vec![sweep("h1"), sweep("h2")],
            co_changes: vec![pair("a.rs", "b.rs", 6, 4, 0.8, 3.5), pair("c.rs", "d.rs", 5, 5, 1.0, 4.0), pair("a.rs", "d.rs", 3, 3, 0.5, 3.2)],
            // c and d shipped together five times; x.rs went along in four of them.
            co_commits: vec![v(&["c.rs", "d.rs", "x.rs"]), v(&["c.rs", "d.rs", "x.rs"]), v(&["c.rs", "d.rs", "x.rs"]), v(&["c.rs", "d.rs", "x.rs"]), v(&["c.rs", "d.rs"]), v(&["a.rs", "b.rs"])],
            ..History::default()
        };
        for p in ["a.rs", "b.rs", "c.rs", "d.rs", "x.rs"] {
            let mut fh = FileHistory::default();
            fh.commits = 8;
            hist.files.insert(p.into(), fh);
        }
        let mut deps = DepGraph::default();
        deps.add_edge("c.rs", "x.rs");
        deps.add_edge("d.rs", "x.rs");
        deps.add_edge("a.rs", "d.rs"); // a<->d import each other: not hidden at all
        let clones = CloneReport::default();
        let tests = TestsCfg::default();
        let inputs = |hcfg: &'static HistoryCfg| Inputs { root: String::new(), files: &files, history: Some(&hist), file_metrics: &fm, functions: &[], deps: &deps, clones: &clones, cognitive_hard: 15, tests: &tests, list_tables_separately: true, history_cfg: hcfg, dedupe_cycle_reason: true };
        let r = build(inputs(Box::leak(Box::new(HistoryCfg::default()))), 10, &Cfg::default(), &td());
        assert_eq!(r.hidden_coupling.iter().map(|h| (h.a.as_str(), h.b.as_str(), h.together, h.together_nonsweep)).collect::<Vec<_>>(), vec![("a.rs", "b.rs", 6, 4)]);
        assert!(r.hidden_coupling[0].explained_by.is_none());
        let e = &r.explained_coupling;
        assert_eq!(e.iter().map(|h| (h.a.as_str(), h.b.as_str(), h.explained_by.as_deref(), h.explained_commits)).collect::<Vec<_>>(), vec![("c.rs", "d.rs", Some("x.rs"), 4)]);
        assert_eq!(r.summary.sweep_commits, 2);
        assert_eq!(r.sweep_note.as_deref(), Some("2 directory-sweep commits (>= 50% of .) excluded from pair counts; still counted as churn"));
        let reason = |p: &str| r.hotspots.iter().find(|h| h.path == p).unwrap().reasons.iter().find(|x| x.contains("changes together")).cloned().unwrap_or_default();
        assert_eq!(reason("a.rs"), "changes together with b.rs (4x, 3.5x more often than chance; 2 sweep commits ignored) but neither imports the other");
        assert_eq!(reason("b.rs"), "changes together with a.rs (4x, 3.5x more often than chance; 2 sweep commits ignored) but neither imports the other");
        assert_eq!(reason("c.rs"), "changes together with d.rs (5x): both import x.rs, which changed in 4 of those commits");
        assert_eq!(reason("x.rs"), "");
        let text = render(&r, 10);
        assert!(text.contains("   4x 0.80  lift  3.5  a.rs  <->  b.rs  (2 sweep commits ignored)\n  EXPLAINED  (both import a file that changed in the same commits: shotgun surgery on it)\n   5x 1.00  c.rs  <->  d.rs  via x.rs (4 of 5)\n  2 directory-sweep commits (>= 50% of .) excluded from pair counts; still counted as churn\n"), "{text}");
        // A stricter share leaves c<->d unexplained: hidden, with no sweep clause (none were ignored).
        let strict = Box::leak(Box::new(HistoryCfg { explained_min_share: 0.9, ..HistoryCfg::default() }));
        let r = build(inputs(strict), 10, &Cfg::default(), &td());
        assert_eq!(r.hidden_coupling.len(), 2);
        assert!(r.explained_coupling.is_empty());
        let reason = |p: &str| r.hotspots.iter().find(|h| h.path == p).unwrap().reasons.iter().find(|x| x.contains("changes together")).cloned().unwrap_or_default();
        assert_eq!(reason("c.rs"), "changes together with d.rs (5x, 4.0x more often than chance) but neither imports the other");
        // No sweeps: no footer.
        hist.sweep_commits = 0;
        hist.sweeps.clear();
        let r = build(Inputs { root: String::new(), files: &files, history: Some(&hist), file_metrics: &fm, functions: &[], deps: &deps, clones: &clones, cognitive_hard: 15, tests: &tests, list_tables_separately: true, history_cfg: strict, dedupe_cycle_reason: true }, 10, &Cfg::default(), &td());
        assert!(r.sweep_note.is_none() && !render(&r, 10).contains("directory-sweep"));
    }

    #[test]
    fn cycle_reason_is_deduped_and_cycles_print_one_block_each() {
        use crate::deps::{Cuts, Edge, EdgeCut, EdgeKind, HubCut};
        let sf = |p: &str| SourceFile { path: p.into(), lang: crate::lang::Language::Rust, kind: FileKind::Source, lines: 100, bytes: 0, content: String::new() };
        let files = vec![sf("src/paths.rs"), sf("src/plan.rs"), sf("src/scan.rs"), sf("src/ctx.rs")];
        let fm: Vec<FileMetrics> = files.iter().map(|f| fmetrics(&f.path, 1, 1, 0)).collect();
        let edge = |from: &str, to: &str, names: &[&str]| Edge { from: from.into(), to: to.into(), kind: EdgeKind::Use, symbols: names.len() as u32, names: names.iter().map(|s| s.to_string()).collect(), glob: false, line: 16 };
        let mut deps = DepGraph::default();
        deps.file_cycles.push(crate::deps::Cycle {
            members: files.iter().map(|f| f.path.clone()).collect(),
            dir: "src".into(),
            cuts: Some(Cuts {
                internal_edges: 9, mod_edges: 0, type_only_edges: 0, base: 4,
                single: Some(EdgeCut { edge: edge("src/paths.rs", "src/plan.rs", &["EntityRef"]), largest_after: 4 }),
                hub: Some(HubCut { member: "src/paths.rs".into(), imports: 4, symbols: 9, largest_after: 3 }),
                cut_set: vec![EdgeCut { edge: edge("src/paths.rs", "src/plan.rs", &["EntityRef"]), largest_after: 4 }, EdgeCut { edge: edge("src/plan.rs", "src/scan.rs", &["ScanToken", "Tok"]), largest_after: 3 }],
                no_single_break: true,
            }),
        });
        deps.file_cycles.push(crate::deps::Cycle { members: vec!["src/a.rs".into(), "src/b.rs".into()], dir: "src".into(), cuts: None });
        let clones = CloneReport::default();
        let tests = TestsCfg::default();
        let hcfg = hcfg();
        let inputs = |dedupe: bool| Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm, functions: &[], deps: &deps, clones: &clones, cognitive_hard: 15, tests: &tests, list_tables_separately: true, history_cfg: &hcfg, dedupe_cycle_reason: dedupe };
        let r = build(inputs(true), 10, &Cfg::default(), &td());
        let reason = |p: &str| r.hotspots.iter().find(|h| h.path == p).unwrap().reasons.iter().find(|x| x.contains("cycle")).cloned().unwrap_or_default();
        assert_eq!(reason("src/paths.rs"), "in the 4-file src cycle (cut: paths.rs -> plan.rs, EntityRef)");
        assert_eq!(reason("src/plan.rs"), "in the 4-file src cycle (cut: paths.rs -> plan.rs, EntityRef)");
        assert_eq!(reason("src/scan.rs"), "in the 4-file src cycle");
        let text = render(&r, 10);
        assert!(text.contains("\nCYCLES\n  4-file cycle in src: cheapest cut src/paths.rs -> src/plan.rs imports 1 symbol (EntityRef, line 16) -> largest remaining cycle 4; hub cut: drop paths.rs's 4 imports (9 symbols) -> 3; no single import breaks this cycle\n    files: src/paths.rs, src/plan.rs, src/scan.rs, src/ctx.rs\n    cut set of 2 imports dissolves it: src/paths.rs -> src/plan.rs (EntityRef), src/plan.rs -> src/scan.rs (ScanToken, Tok)\n  2-file cycle in src\n    files: src/a.rs, src/b.rs\n"), "{text}");
        // Without dedupe: the old count line on every member.
        let r = build(inputs(false), 10, &Cfg::default(), &td());
        assert!(r.hotspots.iter().all(|h| h.reasons.contains(&"in an import cycle of 4 files".to_string())), "{:?}", r.hotspots);
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
        let inputs = || Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm, functions: &[], deps: &deps, clones: &clones, cognitive_hard: 15, tests: &tests, list_tables_separately: true, history_cfg: &hcfg, dedupe_cycle_reason: true };
        let by_default = build(inputs(), 10, &Cfg::default(), &td());
        assert_eq!(by_default.hotspots[0].path, "small.py");
        let size_only = Cfg {
            without_history: Weights { hotspot: 0.0, fixes: 0.0, complexity: 0.0, coupling: 0.0, clones: 0.0, size: 1.0 },
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
/// two files of the cheapest cut edge. Without dedupe, `in an import cycle of 15 files`.
fn cycle_reason(c: &Cycle, path: &str, dedupe: bool) -> String {
    if !dedupe {
        return format!("in an import cycle of {} files", c.members.len());
    }
    format!("in the {}-file {} cycle{}", c.members.len(), c.dir, c.cut_note(path).unwrap_or_default())
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

pub fn render(r: &Report, top: usize) -> String {
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
    if r.hidden_coupling.is_empty() {
        let _ = writeln!(o, "  none");
    }
    for h in r.hidden_coupling.iter().take(top) {
        let ignored = sweeps_ignored(h);
        let note = if ignored.is_empty() { String::new() } else { format!("  ({})", &ignored[2..]) };
        let _ = writeln!(o, "  {:>2}x {:.2}  lift {:>4.1}  {}  <->  {}{note}", h.together_nonsweep, h.strength, h.lift, h.a, h.b);
    }
    if !r.explained_coupling.is_empty() {
        let _ = writeln!(o, "  EXPLAINED  (both import a file that changed in the same commits: shotgun surgery on it)");
        for h in r.explained_coupling.iter().take(top) {
            let _ = writeln!(o, "  {:>2}x {:.2}  {}  <->  {}  via {} ({} of {})", h.together_nonsweep, h.strength, h.a, h.b, h.explained_by.as_deref().unwrap_or(""), h.explained_commits, h.together_nonsweep);
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
        let _ = writeln!(o, "  {:>4} tok  {}:{}-{}  <->  {}:{}-{}{}", c.tokens, c.a.file, c.a.start_line, c.a.end_line, c.b.file, c.b.start_line, c.b.end_line, table_note(c));
    }
    if !r.tables.is_empty() {
        let _ = writeln!(o, "  TABLES  (uniform entries on both sides, not duplicated logic)");
        for c in r.tables.iter().take(top) {
            let _ = writeln!(o, "  {:>4} tok  {}:{}-{}  <->  {}:{}-{}{}", c.tokens, c.a.file, c.a.start_line, c.a.end_line, c.b.file, c.b.start_line, c.b.end_line, table_note(c));
        }
    }

    let _ = writeln!(o, "\nDIRECTORIES  (by source lines)");
    let _ = writeln!(o, "  {:>5} {:>7} {:>7}  dir  (largest file)", "files", "lines", "cog");
    for d in r.directories.iter().take(top) {
        let _ = writeln!(o, "  {:>5} {:>7} {:>7}  {}  ({} @ {} lines)", d.files, d.lines, d.total_cognitive, d.dir, d.largest_file.rsplit('/').next().unwrap_or(""), d.largest_lines);
    }
    o
}
