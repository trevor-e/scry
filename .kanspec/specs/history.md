---
feature: 'Git-derived signals: churn, fix commits, authors, temporal co-change'
code: [src/history/**]
---
# history

## Rules
- History comes from one `git log --no-merges --name-only` over a `--since` window; merge commits are excluded so a merge never double-counts churn.
- A commit is a fix commit when a word in its subject starts with a fix-like stem (fix, bug, regress, broke, crash, wrong, flaky…). Word starts, not substrings: "prefix" is not a fix.
- Co-change is only computed for commits touching 2–25 tracked files; larger commits are mass edits and are ignored. A pair is reported at ≥3 shared commits and strength ≥0.4 where strength = together / min(commits_a, commits_b).
- Only paths in the caller's tracked set (normally Source files) are counted, so tests, fixtures and generated files never inflate churn.
- Renames are not followed (`--no-renames`); a renamed file restarts its history.
