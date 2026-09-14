---
feature: 'Git-derived signals: churn, fix commits, authors, temporal co-change'
code: [src/history/**]
---
# history

## Rules
- History comes from one `git log --no-merges --name-only` over a `--since` window; merge commits are excluded so a merge never double-counts churn. It runs with `core.quotePath=off` so non-ASCII paths arrive unquoted, and `git rev-parse --show-prefix` is stripped from every path so a scan rooted below the work-tree root still joins.
- A commit is a fix commit when a whole word of its subject is in the fix-word list (fix/fixes/fixed, bug, regression, broke, crash, wrong, flaky…). Whole words, not prefixes: "prefix" and "fixtures" are not fixes.
- Co-change is only computed for commits touching at most 25 files in total (tracked or not) and at least 2 tracked ones; larger commits are mass edits and are ignored. Pairs sort by together, strength, then path, so output order is stable. A pair is reported at ≥3 shared commits and strength ≥0.4 where strength = together / min(commits_a, commits_b).
- Only paths in the caller's tracked set (normally Source files) are counted, so tests, fixtures and generated files never inflate churn.
- Renames are not followed (`--no-renames`); a renamed file restarts its history.
- The window, fix-word list and co-change thresholds are `[history]` settings in `scry.toml`; `--since` on the CLI overrides the file.
