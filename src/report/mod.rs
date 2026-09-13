//! The composite: fold every pass into one ranked, explained list.
//!
//! Ranking follows the hotspot idea (Tornhill): the files most likely to hurt
//! are the ones that are both hard to change *and* changed often. Each signal
//! is percentile-normalised within the repo so no thresholds need tuning per
//! language, and every ranked file carries the reasons it ranked, in words.

use crate::clones::{CloneReport, ClonePair};
use crate::deps::{Cycle, DepGraph};
use crate::discover::{FileKind, SourceFile};
use crate::history::History;
use crate::metrics::{COGNITIVE_HARD, FileMetrics, FunctionMetrics};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Clone, Serialize)]
pub struct Signals {
    pub lines: usize,
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
    pub clone_lines: usize,
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
}

#[derive(Debug, Clone, Serialize)]
pub struct HiddenCoupling {
    pub a: String,
    pub b: String,
    pub together: usize,
    pub strength: f64,
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
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub summary: Summary,
    pub hotspots: Vec<Hotspot>,
    pub dir_cycles: Vec<Cycle>,
    pub file_cycles: Vec<Cycle>,
    pub hidden_coupling: Vec<HiddenCoupling>,
    pub clones: Vec<ClonePair>,
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

/// Test stems: `test_combat.py`, `combat.test.ts`, `BattleScreen.spec.tsx` → `combat`, `battlescreen`.
fn test_stems(files: &[SourceFile]) -> HashSet<String> {
    files.iter().filter(|f| f.kind == FileKind::Test).map(|f| stem(&f.path)).collect()
}

pub fn build(inp: Inputs, top: usize) -> Report {
    let source: Vec<&SourceFile> = inp.files.iter().filter(|f| f.kind == FileKind::Source).collect();
    let fm: HashMap<&str, &FileMetrics> = inp.file_metrics.iter().map(|m| (m.path.as_str(), m)).collect();
    let tstems = test_stems(inp.files);
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
                clone_ratio: c.map_or(0.0, |c| c.clone_ratio),
                has_tests: d.is_some_and(|d| d.test_refs > 0) || tstems.contains(&stem(&f.path)),
            }
        })
        .collect();

    let p_commits = percentiles(&signals.iter().map(|s| s.commits).collect::<Vec<_>>());
    let p_fix = percentiles(&signals.iter().map(|s| s.fix_commits).collect::<Vec<_>>());
    let p_maxcog = percentiles(&signals.iter().map(|s| s.max_cognitive).collect::<Vec<_>>());
    let p_totcog = percentiles(&signals.iter().map(|s| s.total_cognitive).collect::<Vec<_>>());
    let p_lines = percentiles(&signals.iter().map(|s| s.lines).collect::<Vec<_>>());
    let p_fanin = percentiles(&signals.iter().map(|s| s.fan_in).collect::<Vec<_>>());
    let p_clone = percentiles(&signals.iter().map(|s| s.clone_lines).collect::<Vec<_>>());
    let have_history = hist.commits_scanned > 0;

    // Worst functions per file, for the explanation.
    let mut worst: HashMap<&str, Vec<&FunctionMetrics>> = HashMap::new();
    for f in inp.functions {
        worst.entry(f.file.as_str()).or_default().push(f);
    }
    for v in worst.values_mut() {
        v.sort_by_key(|f| std::cmp::Reverse(f.cognitive));
        v.truncate(3);
    }

    // Co-change partners with no import relation, indexed by file.
    let mut hidden: Vec<HiddenCoupling> = hist
        .co_changes
        .iter()
        .filter(|c| !inp.deps.connected(&c.a, &c.b))
        .map(|c| HiddenCoupling { a: c.a.clone(), b: c.b.clone(), together: c.together, strength: c.strength })
        .collect();
    hidden.sort_by(|x, y| y.together.cmp(&x.together));
    let mut hidden_by_file: HashMap<&str, Vec<&HiddenCoupling>> = HashMap::new();
    for h in &hidden {
        hidden_by_file.entry(h.a.as_str()).or_default().push(h);
        hidden_by_file.entry(h.b.as_str()).or_default().push(h);
    }
    let cycle_size: HashMap<&str, usize> = inp
        .deps
        .file_cycles
        .iter()
        .flat_map(|c| c.members.iter().map(move |m| (m.as_str(), c.members.len())))
        .collect();

    let mut hotspots: Vec<Hotspot> = source
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let s = &signals[i];
            let churn = if s.commits == 0 { 0.0 } else { p_commits[i] };
            let cx = if s.max_cognitive == 0 { 0.0 } else { 0.6 * p_maxcog[i] + 0.4 * p_totcog[i] };
            let hotspot = (churn * cx).sqrt();
            let coupling = if s.fan_in == 0 { 0.0 } else { p_fanin[i] }.max(if s.in_cycle { 0.6 } else { 0.0 });
            let clone = if s.clone_lines == 0 { 0.0 } else { p_clone[i] };
            let fix = if s.fix_commits == 0 { 0.0 } else { p_fix[i] };
            let base = if have_history {
                0.45 * hotspot + 0.15 * fix + 0.15 * cx + 0.10 * coupling + 0.10 * clone + 0.05 * p_lines[i]
            } else {
                0.55 * cx + 0.15 * coupling + 0.15 * clone + 0.15 * p_lines[i]
            };
            let score = 100.0 * base * if s.has_tests { 1.0 } else { 1.15 };

            let mut reasons = Vec::new();
            let window = hist.window.trim_end_matches(" ago");
            if have_history && p_commits[i] >= 0.8 && s.commits > 0 {
                reasons.push(format!(
                    "{} commits in the last {window} ({} fix commits, {} authors)",
                    s.commits, s.fix_commits, s.authors
                ));
            } else if s.fix_commits >= 2 {
                reasons.push(format!("{} fix commits in the last {window}", s.fix_commits));
            }
            if s.complex_functions > 0 {
                let w = worst.get(f.path.as_str()).and_then(|v| v.first());
                let mut r = format!("{} function(s) over cognitive {COGNITIVE_HARD}", s.complex_functions);
                if let Some(w) = w {
                    r.push_str(&format!("; worst {} at {} (lines {}-{}, nesting {})", w.name, w.cognitive, w.start_line, w.end_line, w.max_nesting));
                }
                reasons.push(r);
            }
            if let Some(n) = cycle_size.get(f.path.as_str()) {
                reasons.push(format!("in an import cycle of {n} files"));
            }
            if p_fanin[i] >= 0.9 && s.fan_in >= 5 {
                reasons.push(format!("imported by {} files: a change here fans out", s.fan_in));
            }
            if s.clone_ratio >= 0.15 {
                reasons.push(format!("{:.0}% of its lines are duplicated elsewhere ({} lines)", s.clone_ratio * 100.0, s.clone_lines));
            }
            if let Some(hs) = hidden_by_file.get(f.path.as_str()) {
                for h in hs.iter().take(2) {
                    let other = if h.a == f.path { &h.b } else { &h.a };
                    reasons.push(format!("changes together with {other} ({}x) but neither imports the other", h.together));
                }
            }
            if !s.has_tests {
                reasons.push("no test file references it".to_string());
            }
            if s.authors == 1 && s.commits >= 5 {
                reasons.push("single author over the window (bus factor 1)".to_string());
            }
            Hotspot {
                path: f.path.clone(),
                score,
                signals: s.clone(),
                reasons,
                worst_functions: worst.get(f.path.as_str()).map(|v| v.iter().map(|f| (*f).clone()).collect()).unwrap_or_default(),
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
        e.files += 1;
        e.lines += f.lines;
        e.total_cognitive += signals[i].total_cognitive;
        if f.lines > e.largest_lines {
            e.largest_lines = f.lines;
            e.largest_file = f.path.clone();
        }
    }
    let mut directories: Vec<DirSummary> = dirs.into_values().collect();
    directories.sort_by_key(|d| std::cmp::Reverse(d.lines));

    let summary = Summary {
        root: inp.root,
        source_files: source.len(),
        test_files: inp.files.iter().filter(|f| f.kind == FileKind::Test).count(),
        source_lines: source.iter().map(|f| f.lines).sum(),
        functions: inp.functions.len(),
        complex_functions: inp.functions.iter().filter(|f| f.cognitive > COGNITIVE_HARD).count(),
        history_window: inp.history.map(|h| h.window.clone()),
        commits_scanned: hist.commits_scanned,
    };

    Report {
        summary,
        hotspots,
        dir_cycles: inp.deps.dir_cycles.clone(),
        file_cycles: inp.deps.file_cycles.clone(),
        hidden_coupling: hidden,
        clones: inp.clones.pairs.iter().take(top).cloned().collect(),
        directories: directories.into_iter().take(top).collect(),
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
        s.source_files, s.source_lines, s.test_files, s.functions, s.complex_functions, COGNITIVE_HARD
    );
    match &s.history_window {
        Some(w) => { let _ = writeln!(o, "history: {} commits since {w}", s.commits_scanned); }
        None => { let _ = writeln!(o, "history: unavailable (not a git repo or --no-history)"); }
    }

    let _ = writeln!(o, "\nHOTSPOTS  (score = churn x complexity, boosted by fixes, coupling, clones, missing tests)");
    for (i, h) in r.hotspots.iter().take(top).enumerate() {
        let _ = writeln!(o, "{:>2}. {:>5.1}  {}  ({} lines)", i + 1, h.score, h.path, h.signals.lines);
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
        let _ = writeln!(o, "  files [{}] {}", c.members.len(), c.members.join(", "));
    }

    let _ = writeln!(o, "\nHIDDEN COUPLING  (change together, no import between them)");
    if r.hidden_coupling.is_empty() {
        let _ = writeln!(o, "  none");
    }
    for h in r.hidden_coupling.iter().take(top) {
        let _ = writeln!(o, "  {:>2}x {:.2}  {}  <->  {}", h.together, h.strength, h.a, h.b);
    }

    let _ = writeln!(o, "\nCLONES  (largest near-exact duplicates)");
    if r.clones.is_empty() {
        let _ = writeln!(o, "  none");
    }
    for c in r.clones.iter().take(top) {
        let _ = writeln!(o, "  {:>4} tok  {}:{}-{}  <->  {}:{}-{}", c.tokens, c.a.file, c.a.start_line, c.a.end_line, c.b.file, c.b.start_line, c.b.end_line);
    }

    let _ = writeln!(o, "\nDIRECTORIES  (by source lines)");
    let _ = writeln!(o, "  {:>5} {:>7} {:>7}  dir  (largest file)", "files", "lines", "cog");
    for d in r.directories.iter().take(top) {
        let _ = writeln!(o, "  {:>5} {:>7} {:>7}  {}  ({} @ {} lines)", d.files, d.lines, d.total_cognitive, d.dir, d.largest_file.rsplit('/').next().unwrap_or(""), d.largest_lines);
    }
    o
}
