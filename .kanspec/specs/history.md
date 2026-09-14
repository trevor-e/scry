---
feature: 'Git-derived signals: churn, fix commits, authors, temporal co-change'
code: [src/history/**]
---
# history

## Rules
- History comes from one `git log --no-merges --name-only` over a `--since` window; merge commits are excluded so a merge never double-counts churn. It runs with `core.quotePath=off` so non-ASCII paths arrive unquoted, and `git rev-parse --show-prefix` is stripped from every path so a scan rooted below the work-tree root still joins.
- A commit is a fix commit when a whole word of its subject is in the fix-word list (fix/fixes/fixed, bug, regression, broke, crash, wrong, flaky…). Whole words, not prefixes: "prefix" and "fixtures" are not fixes.
- Co-change is only computed for commits touching at most 25 files in total (tracked or not) and at least 2 tracked ones; larger commits are mass edits and are ignored. Pairs sort by together_nonsweep, strength, lift, then path, so output order is stable. A pair is reported at ≥3 shared commits and strength ≥0.4 where strength = together_nonsweep / min(commits_a, commits_b), both judged on non-sweep counts.
- A commit is a directory sweep when, for some directory holding ≥ `sweep_min_dir_files` (4) tracked files, it touches ≥ `sweep_fraction` (50%) of them and ≥ `sweep_min_files` (6) of them: an agent session rewriting every command file at once. Sweeps count toward per-file commits, fix commits and authors (they are real churn) and toward a pair's raw `together`, never toward `together_nonsweep`; the mass-edit cap still applies on top. Each sweep is listed with its hash, subject, file count and the directory that qualified it.
- Every pair carries lift = together_nonsweep / (commits_a × commits_b / N) with the per-file commit counts (sweeps included) and N = non-sweep commits in the window: how many times more often than chance the two ship together. When N ≥ `min_commits_for_lift` (20), pairs with lift < `min_lift` (3.0) are dropped (`lift_applied`); below that lift is reported and not applied.
- Only paths in the caller's tracked set (normally Source files) are counted, so tests, fixtures and generated files never inflate churn; directory sizes for the sweep rule come from the same set.
- `--json` carries `sweep_commits`, `lift_applied` and `sweeps[]` at the top, `sweep_commits` per file, and `together`, `together_nonsweep`, `strength`, `lift` per pair.
- Renames are not followed (`--no-renames`); a renamed file restarts its history.
- The window, fix-word list, co-change thresholds, sweep rule, lift gate and explained share are `[history]` settings in `scry.toml`; `--since` on the CLI overrides the file.
