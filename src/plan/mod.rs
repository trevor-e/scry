//! A per-file refactor plan from findings the other passes already made.
//!
//! No new analysis: each step restates a clone pair (both sides resolved to a
//! symbol), a unit over the cognitive threshold, or the cheapest cut of the
//! file's import cycle, in a fixed kind order and with no predicted effect.
//! Clone runs inside inline test regions fold into one step naming the region.

use crate::clones::{CloneKind, ClonePair, Loc};
use crate::config::Plan as Cfg;
use crate::deps::{symbol_count, symbol_names, Cycle};
use crate::metrics::FunctionMetrics;
use crate::regions::{RegionKind, TestRegion};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    FoldTestClones,
    CanonicaliseClone,
    Extract,
    CutCycleEdge,
}

/// Every step kind, for validating `[plan].kind_priority`.
pub const KINDS: [StepKind; 4] = [StepKind::FoldTestClones, StepKind::CanonicaliseClone, StepKind::Extract, StepKind::CutCycleEdge];

impl StepKind {
    /// The spelling `[plan].kind_priority` uses.
    pub fn name(self) -> &'static str {
        match self {
            StepKind::FoldTestClones => "fold_test_clones",
            StepKind::CanonicaliseClone => "canonicalise_clone",
            StepKind::Extract => "extract",
            StepKind::CutCycleEdge => "cut_cycle_edge",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Step {
    /// 1-based position in the printed plan.
    pub n: usize,
    pub kind: StepKind,
    /// The symbol the step is about in this file: the clone run's, the unit's, or the cut
    /// edge's source file.
    pub symbol: Option<String>,
    /// Its line span (a clone run's, a unit's, a test region's).
    pub lines: Option<(usize, usize)>,
    /// The other side: the duplicate's `symbol (lines)` (prefixed by its file when elsewhere),
    /// the cut edge's target file.
    pub target: Option<String>,
    pub text: String,
}

/// What the plan for one file is built from; every field is data another pass produced.
pub struct Facts<'a> {
    pub path: &'a str,
    /// Clone pairs with at least one side in the file. Each side of this file is planned on its
    /// own: a side in test code folds, a source side is a canonicalise step even when its
    /// duplicate lies in test code elsewhere.
    pub pairs: &'a [&'a ClonePair],
    /// Whether a run side starts inside an inline test region or a Test-classified file.
    pub in_tests: &'a dyn Fn(&Loc) -> bool,
    /// The file's own test regions, named by the fold step.
    pub regions: &'a [TestRegion],
    /// The file's source units.
    pub functions: &'a [&'a FunctionMetrics],
    /// The file cycle the file is a member of.
    pub cycle: Option<&'a Cycle>,
    pub cognitive_hard: u32,
}

/// `park (932-962)`, `src/rulesdoc.rs render_text (577-594)`, `CHECKS table (91-134)`: a run side
/// as the plan and the CLONES section print it, with its file only when that is not `this`.
pub fn side_text(l: &Loc, this: &str, table: bool) -> String {
    let file = if l.file == this { String::new() } else { format!("{} ", l.file) };
    let table = if table { " table" } else { "" };
    format!("{file}{}{table} ({}-{})", l.symbol, l.start_line, l.end_line)
}

/// The steps in `kind_priority` order, cut at `max_steps`, and how many were cut.
pub fn build(f: &Facts, cfg: &Cfg) -> (Vec<Step>, usize) {
    let mut steps: Vec<(usize, Step)> = Vec::new();
    let rank = |k: StepKind| cfg.kind_priority.iter().position(|p| p == k.name());
    let step = |kind: StepKind, symbol: Option<String>, lines: Option<(usize, usize)>, target: Option<String>, text: String| {
        Step { n: 0, kind, symbol, lines, target, text }
    };

    // Clone runs, decided per side of this file: a side in test code folds, any other side is
    // a canonicalise step (once per pair) naming the duplicate, wherever that lies.
    let mut fold: Vec<Option<usize>> = Vec::new(); // region index per folded run
    let mut canon: Vec<(&ClonePair, &Loc, &Loc)> = Vec::new(); // (pair, this file's side, the other)
    for p in f.pairs {
        for (mine, other) in [(&p.a, &p.b), (&p.b, &p.a)] {
            if mine.file != f.path {
                continue;
            }
            if (f.in_tests)(mine) {
                fold.push(f.regions.iter().position(|r| r.start_line <= mine.start_line && mine.start_line <= r.end_line));
            } else if !canon.last().is_some_and(|(q, _, _)| std::ptr::eq(*q, *p)) {
                canon.push((p, mine, other));
            }
        }
    }
    if cfg.fold_test_clones && !fold.is_empty() && let Some(r) = rank(StepKind::FoldTestClones) {
        // The region holding the most runs is named; the rest are counted.
        let mut counts = vec![0usize; f.regions.len()];
        let mut elsewhere = 0usize;
        for i in &fold {
            match i {
                Some(i) => counts[*i] += 1,
                None => elsewhere += 1,
            }
        }
        let runs = |n: usize| if n == 1 { "1 clone run".to_string() } else { format!("{n} clone runs") };
        let best = (0..counts.len()).max_by_key(|i| (counts[*i], std::cmp::Reverse(*i)));
        let (text, lines) = match best.filter(|i| counts[*i] > 0) {
            Some(i) => {
                let region = &f.regions[i];
                let other = fold.len() - counts[i];
                let more = if other == 0 { String::new() } else { format!(", +{other} in other test regions") };
                (format!("fold {} in {} at {}-{} into shared test helpers{more}", runs(counts[i]), region_desc(region.kind), region.start_line, region.end_line), Some((region.start_line, region.end_line)))
            }
            None => (format!("fold {} in test code into shared test helpers", runs(elsewhere)), None),
        };
        steps.push((r, step(StepKind::FoldTestClones, None, lines, None, text)));
    }
    if let Some(r) = rank(StepKind::CanonicaliseClone) {
        // Largest first; a same-file pair once, from its earlier side outside test code.
        canon.sort_by(|(p, _, _), (q, _, _)| q.tokens.cmp(&p.tokens).then_with(|| p.a.start_line.cmp(&q.a.start_line)).then_with(|| p.b.start_line.cmp(&q.b.start_line)));
        for (p, mine, other) in canon {
            let table = p.kind == CloneKind::Table;
            let target = side_text(other, f.path, table);
            let keep = if other.file == f.path { ": keep one" } else { "" };
            let text = format!("{} duplicates {target}, {} tokens{keep}", side_text(mine, f.path, table), p.tokens);
            steps.push((r, step(StepKind::CanonicaliseClone, Some(mine.symbol.clone()), Some((mine.start_line, mine.end_line)), Some(target), text)));
        }
    }
    if let Some(r) = rank(StepKind::Extract) {
        let mut hard: Vec<&FunctionMetrics> = f.functions.iter().copied().filter(|u| u.cognitive > f.cognitive_hard).collect();
        hard.sort_by(|a, b| b.cognitive.cmp(&a.cognitive).then_with(|| a.start_line.cmp(&b.start_line)));
        for u in hard {
            let text = format!("extract from {} (cognitive {}, lines {}-{}, nesting {})", u.name, u.cognitive, u.start_line, u.end_line, u.max_nesting);
            steps.push((r, step(StepKind::Extract, Some(u.name.clone()), Some((u.start_line, u.end_line)), None, text)));
        }
    }
    if let Some(r) = rank(StepKind::CutCycleEdge)
        && let Some(s) = f.cycle.and_then(|c| c.cuts.as_ref()).and_then(|c| c.single.as_ref())
        && s.edge.from == f.path
    {
        let text = format!(
            "cut the import {} -> {}: {} ({}, line {}) -> largest remaining cycle {}",
            s.edge.from, s.edge.to, symbol_count(&s.edge), symbol_names(&s.edge.names, s.edge.glob), s.edge.line, s.largest_after
        );
        steps.push((r, step(StepKind::CutCycleEdge, Some(s.edge.from.clone()), Some((s.edge.line, s.edge.line)), Some(s.edge.to.clone()), text)));
    }

    steps.sort_by_key(|(r, _)| *r);
    let total = steps.len();
    let mut out: Vec<Step> = steps.into_iter().take(cfg.max_steps).map(|(_, s)| s).collect();
    for (i, s) in out.iter_mut().enumerate() {
        s.n = i + 1;
    }
    (out, total.saturating_sub(cfg.max_steps))
}

fn region_desc(kind: RegionKind) -> &'static str {
    match kind {
        RegionKind::CfgTestMod => "#[cfg(test)] mod",
        RegionKind::TestFn => "#[test] fn",
        RegionKind::CfgTestItem => "#[cfg(test)] item",
    }
}

/// `plan for src/cmd/flow.rs:` then one line per step and `(+N more)`; `no plan` when empty.
pub fn render(path: &str, steps: &[Step], more: usize) -> String {
    if steps.is_empty() && more == 0 {
        return "no plan\n".to_string();
    }
    let mut o = format!("plan for {path}:\n");
    for s in steps {
        o.push_str(&format!("  {}) {}\n", s.n, s.text));
    }
    if more > 0 {
        o.push_str(&format!("  (+{more} more)\n"));
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deps::{Cuts, Edge, EdgeCut, EdgeKind};

    fn loc(file: &str, s: usize, e: usize, sym: &str) -> Loc {
        Loc { file: file.into(), start_line: s, end_line: e, symbol: sym.into() }
    }

    fn pair(a: Loc, b: Loc, tokens: usize, kind: CloneKind) -> ClonePair {
        let table = kind == CloneKind::Table;
        ClonePair { a, b, tokens, kind, container_kind: table.then(|| "match_block".into()), entry_count: table.then_some(8) }
    }

    fn unit(name: &str, s: usize, e: usize, cognitive: u32, nesting: u32) -> FunctionMetrics {
        FunctionMetrics { file: "src/flow.rs".into(), name: name.into(), start_line: s, end_line: e, lines: e - s + 1, params: 0, cyclomatic: 1, cognitive, max_nesting: nesting, in_test: false, phases: Vec::new(), bindings: 0, short_bindings: Vec::new(), long_short_bindings: 0, locals: 0, brain: false, longest_locals: Vec::new(), parse_defaults: 0, fallbacks: 0, fallback_sites: Vec::new() }
    }

    fn cycle(from: &str, to: &str) -> Cycle {
        let edge = Edge { from: from.into(), to: to.into(), kind: EdgeKind::Use, symbols: 1, names: vec!["EntityRef".into()], glob: false, line: 16 };
        Cycle { members: vec![from.into(), to.into()], dir: "src".into(), cuts: Some(Cuts { internal_edges: 2, mod_edges: 0, type_only_edges: 0, base: 2, single: Some(EdgeCut { edge, largest_after: 1 }), hub: None, cut_set: vec![], no_single_break: false }) }
    }

    fn region(s: usize, e: usize) -> TestRegion {
        TestRegion { kind: RegionKind::CfgTestMod, start_byte: 0, end_byte: 0, start_line: s, end_line: e }
    }

    #[test]
    fn steps_follow_kind_priority_and_name_symbols_on_both_sides() {
        let this = "src/flow.rs";
        let pairs = [
            pair(loc(this, 842, 858, "ship"), loc(this, 928, 942, "park"), 112, CloneKind::Logic),
            pair(loc(this, 932, 962, "park"), loc(this, 1010, 1036, "drop_ticket"), 239, CloneKind::Logic),
            pair(loc("src/derive.rs", 1080, 1097, "att"), loc(this, 577, 594, "render_text"), 103, CloneKind::Logic),
            pair(loc(this, 30, 43, "Ticket"), loc(this, 153, 166, "Ticket"), 79, CloneKind::Table),
            // Three runs inside the file's test module (one pair wholly inside, one side of another).
            pair(loc(this, 1100, 1120, "t_one"), loc(this, 1130, 1150, "t_two"), 150, CloneKind::Logic),
            pair(loc("src/other.rs", 10, 30, "f"), loc(this, 1200, 1220, "t_three"), 90, CloneKind::Logic),
        ];
        let refs: Vec<&ClonePair> = pairs.iter().collect();
        let regions = vec![region(1067, 1469)];
        let in_tests = |l: &Loc| l.file == this && l.start_line >= 1067;
        let units = [unit("ladder", 405, 656, 32, 5), unit("small", 1, 10, 3, 1), unit("done", 75, 257, 18, 2)];
        let funcs: Vec<&FunctionMetrics> = units.iter().collect();
        let cyc = cycle(this, "src/plan.rs");
        let facts = Facts { path: this, pairs: &refs, in_tests: &in_tests, regions: &regions, functions: &funcs, cycle: Some(&cyc), cognitive_hard: 15 };
        let (steps, more) = build(&facts, &Cfg::default());
        let texts: Vec<&str> = steps.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec![
            "fold 3 clone runs in #[cfg(test)] mod at 1067-1469 into shared test helpers",
            "park (932-962) duplicates drop_ticket (1010-1036), 239 tokens: keep one",
            "ship (842-858) duplicates park (928-942), 112 tokens: keep one",
            "render_text (577-594) duplicates src/derive.rs att (1080-1097), 103 tokens",
            "Ticket table (30-43) duplicates Ticket table (153-166), 79 tokens: keep one",
            "extract from ladder (cognitive 32, lines 405-656, nesting 5)",
        ], "{steps:?}");
        assert_eq!(more, 2); // extract from done, cut the import
        assert_eq!(steps.iter().map(|s| s.n).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5, 6]);
        assert_eq!((steps[0].kind, steps[0].lines), (StepKind::FoldTestClones, Some((1067, 1469))));
        assert_eq!((steps[1].symbol.as_deref(), steps[1].lines, steps[1].target.as_deref()), (Some("park"), Some((932, 962)), Some("drop_ticket (1010-1036)")));
        assert_eq!(steps[3].target.as_deref(), Some("src/derive.rs att (1080-1097)"));
        assert!(steps.iter().all(|s| !s.text.contains("score") && !s.text.contains('%') && !s.text.contains("rank")), "{texts:?}");
        assert_eq!(render(this, &steps, more), "plan for src/flow.rs:\n  1) fold 3 clone runs in #[cfg(test)] mod at 1067-1469 into shared test helpers\n  2) park (932-962) duplicates drop_ticket (1010-1036), 239 tokens: keep one\n  3) ship (842-858) duplicates park (928-942), 112 tokens: keep one\n  4) render_text (577-594) duplicates src/derive.rs att (1080-1097), 103 tokens\n  5) Ticket table (30-43) duplicates Ticket table (153-166), 79 tokens: keep one\n  6) extract from ladder (cognitive 32, lines 405-656, nesting 5)\n  (+2 more)\n");
        // A wider cap shows the rest: extracts by cognitive, then the cut with P61's wording.
        let wide = Cfg { max_steps: 10, ..Cfg::default() };
        let (steps, more) = build(&facts, &wide);
        assert_eq!(more, 0);
        assert_eq!(steps[6].text, "extract from done (cognitive 18, lines 75-257, nesting 2)");
        assert_eq!(steps[7].text, "cut the import src/flow.rs -> src/plan.rs: 1 symbol (EntityRef, line 16) -> largest remaining cycle 1");
        assert_eq!((steps[7].kind, steps[7].target.as_deref()), (StepKind::CutCycleEdge, Some("src/plan.rs")));
        // Priority reorders; a kind left out is not planned; folding off drops the test runs silently.
        let order = Cfg { kind_priority: vec!["cut_cycle_edge".into(), "extract".into()], max_steps: 10, ..Cfg::default() };
        let (steps, _) = build(&facts, &order);
        assert_eq!(steps.iter().map(|s| s.kind).collect::<Vec<_>>(), vec![StepKind::CutCycleEdge, StepKind::Extract, StepKind::Extract]);
        let no_fold = Cfg { fold_test_clones: false, max_steps: 10, ..Cfg::default() };
        let (steps, _) = build(&facts, &no_fold);
        assert!(steps.iter().all(|s| s.kind != StepKind::FoldTestClones) && !steps.iter().any(|s| s.text.contains("t_one")), "{steps:?}");
        assert_eq!(steps.iter().filter(|s| s.kind == StepKind::CanonicaliseClone).count(), 4);
    }

    #[test]
    fn the_cut_step_needs_this_file_as_the_edge_source_and_an_empty_plan_renders_no_plan() {
        let cyc = cycle("src/paths.rs", "src/plan.rs");
        let in_tests = |_: &Loc| false;
        let facts = Facts { path: "src/plan.rs", pairs: &[], in_tests: &in_tests, regions: &[], functions: &[], cycle: Some(&cyc), cognitive_hard: 15 };
        let (steps, more) = build(&facts, &Cfg::default());
        assert!(steps.is_empty() && more == 0);
        assert_eq!(render("src/plan.rs", &steps, more), "no plan\n");
        let facts = Facts { path: "src/paths.rs", ..facts };
        let (steps, _) = build(&facts, &Cfg::default());
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].kind, StepKind::CutCycleEdge);
    }

    #[test]
    fn folded_runs_name_the_busiest_region_and_count_the_others() {
        let this = "a.rs";
        let pairs = [
            pair(loc(this, 20, 30, "t1"), loc(this, 40, 50, "t2"), 100, CloneKind::Logic),
            pair(loc(this, 120, 130, "u1"), loc("b.rs", 1, 10, "x"), 90, CloneKind::Logic),
            // This file's side is source; the duplicate sits in a Test-classified file (b.rs) and
            // in this file's own test module: each source side still gets its step.
            pair(loc(this, 70, 80, "alpha"), loc("c.rs", 1, 10, "t_far"), 85, CloneKind::Logic),
            pair(loc(this, 110, 118, "t3"), loc(this, 82, 90, "beta"), 75, CloneKind::Logic),
        ];
        let refs: Vec<&ClonePair> = pairs.iter().collect();
        let regions = vec![region(10, 60), region(100, 140)];
        let in_tests = |l: &Loc| l.file == "c.rs" || l.file == this && ((10..=60).contains(&l.start_line) || (100..=140).contains(&l.start_line));
        let facts = Facts { path: this, pairs: &refs, in_tests: &in_tests, regions: &regions, functions: &[], cycle: None, cognitive_hard: 15 };
        let (steps, _) = build(&facts, &Cfg::default());
        assert_eq!(steps.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), vec![
            "fold 2 clone runs in #[cfg(test)] mod at 10-60 into shared test helpers, +2 in other test regions",
            "alpha (70-80) duplicates c.rs t_far (1-10), 85 tokens",
            "beta (82-90) duplicates t3 (110-118), 75 tokens: keep one",
        ], "{steps:?}");
        // A cap that leaves no step still says how many were cut, as `--json` does.
        let none = Cfg { max_steps: 0, ..Cfg::default() };
        let (steps, more) = build(&facts, &none);
        assert!(steps.is_empty() && more == 3);
        assert_eq!(render(this, &steps, more), "plan for a.rs:\n  (+3 more)\n");
    }
}
