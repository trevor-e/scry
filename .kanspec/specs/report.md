---
feature: Composite hotspot ranking and human/JSON report for LLM consumption
code: [src/report/**, src/main.rs, src/config.rs]
---
# report

## Rules
- The report is written for an LLM reader: every ranked item carries the reasons it ranked, with symbol names and line ranges, never a bare number.
- Each analysis pass has a standalone subcommand (`files`, `history`, …) so a signal can be inspected on its own before it is folded into the composite score.
- Every signal is percentile-ranked within the repo's Source files before weighting; a file with a zero signal gets 0, not its tie percentile, so untouched files never score on churn.
- With history (defaults): score = 100 × (0.45·√(churn·complexity) + 0.15·fixes + 0.15·complexity + 0.10·coupling + 0.10·clones + 0.05·size), ×1.15 when no test references the file. Without history the churn terms are dropped and complexity is weighted 0.55.
- complexity = 0.6·pct(max cognitive) + 0.4·pct(total cognitive); coupling = max(pct(fan_in), 0.6 if in a cycle).
- `has_tests` is true when a test file imports the file, when a test file's normalised stem equals the file's stem *and* the test sits in the same directory tree (the test's path above its first test-directory component is an ancestor of the file), or when a Rust file carries an inline `#[cfg(test)]` module.
- History counts as present only when at least one commit joined a discovered file; commits that touched nothing we track (subdirectory scans, shallow clones) fall back to the static weights, and `scan` warns.
- Hidden coupling = co-change pairs where neither file imports the other, minus the explained ones; the top two partners appear as reasons on each file, as `changes together with X (4x, 3.5x more often than chance; 2 sweep commits ignored) but neither imports the other`: the non-sweep count, lift to one decimal, and the sweep commits left out of that count (clause omitted when none were).
- A pair is `explained_by` a file f when both members import f and f changed in ≥ `[history].explained_min_share` (0.5) of the pair's non-sweep co-commits (the file both import that changed in the most of them wins). Explained pairs leave `hidden_coupling` for `explained_coupling`, print under an EXPLAINED sub-heading, and their reason reads `changes together with X (5x): both import src/ctx.rs, which changed in 4 of those commits`.
- When the window had sweep commits the HIDDEN COUPLING section ends with `5 directory-sweep commits (>= 50% of src/cmd) excluded from pair counts; still counted as churn` (`sweep_note` in `--json`, `summary.sweep_commits` for the count); every hidden or explained pair carries `together`, `together_nonsweep`, `lift`, `explained_by` and `explained_commits` in `--json`.
- The `directories` section rolls Source files up by directory with the largest file named, so "largest file per module" is answered without ranking data files.
- Every weight, the no-tests multiplier, the cycle coupling floor and every reason threshold are `[report]` settings in `scry.toml`; `scry config` prints the effective values. With no file present the score is unchanged from the constants it replaced.
- Inline test regions come from metrics data, never a string search: `has_tests` is true when a file has at least one region; the size signal, the summary's `source_lines` and the directory rollup use `lines - inline_test_lines`; `signals.inline_test_lines` and `test_regions[]` are in `--json`.
- A hotspot whose inline test ratio (`inline_test_lines / lines`) is at or above `[tests].report_inline_ratio_above` (0.5) prints `N in #[cfg(test)] mod at a-b` after its line count (`inline_test_note` in `--json`); below it, the plain line count.
- A Source file whose inline test ratio is above `[tests].reclassify_file_above_ratio` (0.9) is a Test file for ranking: never a hotspot, counted under `test_files`.
- Units tagged `in_test` never head a worst-function reason or appear in `worst_functions`, and are left out of the summary's function counts.
