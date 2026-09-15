---
feature: 'Git-derived signals: churn, fix commits, authors, temporal co-change'
code: [src/history/**]
---
# history

## Rules
- History comes from one `git log --no-merges --name-only` over a `--since` window; merge commits are excluded so a merge never double-counts churn. It runs with `core.quotePath=off` so non-ASCII paths arrive unquoted, and `git rev-parse --show-prefix` is stripped from every path so a scan rooted below the work-tree root still joins.
- A commit is a fix commit when a whole word of its subject is in the fix-word list (fix/fixes/fixed, bug, regression, broke, crash, wrong, flaky…). Whole words, not prefixes: "prefix" and "fixtures" are not fixes.
- With `fix_mass_edit_cap` (default on) a fix-worded commit touching more than `max_cochange_commit_size` (25) files in total (tracked or not: the same cap co-change uses) is not a fix commit for any file it touched; it still counts as a commit. A 49-file "review pass: fix eleven defects" says nothing about which file was broken. Such commits are counted in `capped_fix_commits` at the top and per file.
- Authors are distinct author names over the file's commits, excluding commits whose author name contains a `bot_patterns` entry (case-insensitive substring; default `[bot]`, dependabot, renovate, pre-commit-ci). Bot commits still count as commits (churn) and are reported as `bot_commits` at the top and per file, so the "single author (bus factor 1)" reason and the authors count ignore dependabot and friends.
- Co-change is only computed for commits touching at most 25 files in total (tracked or not) and at least 2 tracked ones; larger commits are mass edits and are ignored. Pairs sort by together_nonsweep, strength, lift, then path, so output order is stable. A pair is reported at ≥3 shared commits and strength ≥0.4 where strength = together_nonsweep / min(commits_a, commits_b), both judged on non-sweep counts.
- A commit is a directory sweep when, for some directory holding ≥ `sweep_min_dir_files` (4) tracked files, it touches ≥ `sweep_fraction` (50%) of them and ≥ `sweep_min_files` (6) of them: an agent session rewriting every command file at once. Sweeps count toward per-file commits, fix commits (unless over the mass-edit cap) and authors (unless by a bot) (they are real churn) and toward a pair's raw `together`, never toward `together_nonsweep`; the mass-edit cap still applies on top. Each sweep is listed with its hash, subject, file count and the directory that qualified it.
- Every pair carries lift = together_nonsweep / (commits_a × commits_b / N) with the per-file commit counts (sweeps included) and N = non-sweep commits in the window: how many times more often than chance the two ship together. When N ≥ `min_commits_for_lift` (20), pairs with lift < `min_lift` (3.0) are dropped (`lift_applied`); below that lift is reported and not applied.
- Only paths in the caller's tracked set (normally Source files) are counted, so tests, fixtures and generated files never inflate churn; directory sizes for the sweep rule come from the same set.
- `--json` carries `sweep_commits`, `lift_applied`, `bot_commits`, `capped_fix_commits` and `sweeps[]` at the top, `sweep_commits`, `bot_commits`, `capped_fix_commits` per file, and `together`, `together_nonsweep`, `strength`, `lift` per pair.
- Renames are not followed (`--no-renames`); a renamed file restarts its history.
- `scry history` orders its file table by commits, fix commits, then path, so ties print the same way on every run; its header says `fix cap off` instead of a capped count when `fix_mass_edit_cap` is off.
- The window, fix-word list, fix cap, bot patterns, co-change thresholds, sweep rule, lift gate and explained share are `[history]` settings in `scry.toml`; `--since` on the CLI overrides the file.
