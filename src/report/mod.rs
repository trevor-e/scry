//! The composite: fold every pass into one ranked, explained list.
//!
//! Ranking follows the hotspot idea (Tornhill): the files most likely to hurt
//! are the ones that are both hard to change *and* changed often. Each signal
//! is percentile-normalised within the repo so no thresholds need tuning per
//! language, and every ranked file carries the reasons it ranked, in words.

use crate::clones::{CloneReport, ClonePair};
use crate::config::{Report as Cfg, Tests as TestsCfg, Weights};
use crate::deps::{Cycle, DepGraph};
use crate::discover::{FileKind, SourceFile};
use crate::history::History;
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
    pub test_regions: Vec<TestRegion>,
    /// `1026 in #[cfg(test)] mod at 1283-2308`, printed after the line count when the inline
    /// test ratio is at or above `[tests].report_inline_ratio_above`.
    pub inline_test_note: Option<String>,
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
    pub cognitive_hard: u32,
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
    pub cognitive_hard: u32,
    pub tests: &'a TestsCfg,
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
            if let Some(n) = cycle_size.get(f.path.as_str()) {
                reasons.push(format!("in an import cycle of {n} files"));
            }
            if p_fanin[i] >= cfg.reason_fanin_percentile && s.fan_in >= cfg.reason_min_fan_in {
                reasons.push(format!("imported by {} files: a change here fans out", s.fan_in));
            }
            if s.clone_ratio >= cfg.reason_clone_ratio {
                reasons.push(format!("{:.0}% of its lines are duplicated elsewhere ({} lines)", s.clone_ratio * 100.0, s.clone_lines));
            }
            if let Some(hs) = hidden_by_file.get(f.path.as_str()) {
                for h in hs.iter().take(cfg.reason_hidden_partners) {
                    let other = if h.a == f.path { &h.b } else { &h.a };
                    reasons.push(format!("changes together with {other} ({}x) but neither imports the other", h.together));
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
        cognitive_hard: inp.cognitive_hard,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn td() -> Vec<String> {
        crate::config::Discover::default().test_dirs
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
        let size_only = Cfg {
            without_history: Weights { hotspot: 0.0, fixes: 0.0, complexity: 0.0, coupling: 0.0, clones: 0.0, size: 1.0 },
            ..Cfg::default()
        };
        let r = build(Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm, functions: &functions, deps: &deps, clones: &clones, cognitive_hard: 15, tests: &tests }, 10, &size_only, &td());
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
        let r = build(Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm, functions: &functions, deps: &deps, clones: &clones, cognitive_hard: 15, tests: &strict }, 10, &size_only, &td());
        assert!(r.hotspots[1].inline_test_note.is_none());
        assert!(r.hotspots[1].signals.has_tests);
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
        let inputs = || Inputs { root: String::new(), files: &files, history: None, file_metrics: &fm, functions: &[], deps: &deps, clones: &clones, cognitive_hard: 15, tests: &tests };
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
