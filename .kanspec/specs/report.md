---
feature: Composite hotspot ranking and human/JSON report for LLM consumption
code: [src/report/**, src/main.rs]
---
# report

## Rules
- The report is written for an LLM reader: every ranked item carries the reasons it ranked, with symbol names and line ranges, never a bare number.
- Each analysis pass has a standalone subcommand (`files`, `history`, …) so a signal can be inspected on its own before it is folded into the composite score.
- Every signal is percentile-ranked within the repo's Source files before weighting; a file with a zero signal gets 0, not its tie percentile, so untouched files never score on churn.
- With history: score = 100 × (0.45·√(churn·complexity) + 0.15·fixes + 0.15·complexity + 0.10·coupling + 0.10·clones + 0.05·size), ×1.15 when no test references the file. Without history the churn terms are dropped and complexity is weighted 0.55.
- complexity = 0.6·pct(max cognitive) + 0.4·pct(total cognitive); coupling = max(pct(fan_in), 0.6 if in a cycle).
- `has_tests` is true when a test file imports the file or a test file's normalised stem equals the file's stem.
- Hidden coupling = co-change pairs where neither file imports the other; the top two partners appear as reasons on each file.
- The `directories` section rolls Source files up by directory with the largest file named, so "largest file per module" is answered without ranking data files.
