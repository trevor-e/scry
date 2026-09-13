---
feature: Composite hotspot ranking and human/JSON report for LLM consumption
code: [src/report/**, src/main.rs]
---
# report

## Rules
- The report is written for an LLM reader: every ranked item carries the reasons it ranked, with symbol names and line ranges, never a bare number.
- Each analysis pass has a standalone subcommand (`files`, `history`, …) so a signal can be inspected on its own before it is folded into the composite score.
