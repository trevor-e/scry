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
- Hidden coupling = co-change pairs where neither file imports the other; the top two partners appear as reasons on each file.
- The `directories` section rolls Source files up by directory with the largest file named, so "largest file per module" is answered without ranking data files.
- Every weight, the no-tests multiplier, the cycle coupling floor and every reason threshold are `[report]` settings in `scry.toml`; `scry config` prints the effective values. With no file present the score is unchanged from the constants it replaced.
