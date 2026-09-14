# scry

Find the parts of a codebase most likely to need refactoring or to hide bugs,
and explain why in terms an LLM (or a person) can act on.

```
scry scan <repo>            # ranked hotspots with reasons + cycles, hidden coupling, clones
scry scan <repo> --json     # the same, machine-readable
scry files | history | metrics | deps | clones <repo>   # one signal at a time
scry ast <file> [--errors]  # tree-sitter debugging aid
```

Rust + tree-sitter. Python, TypeScript/TSX, JavaScript and Rust today; a new
language is one grammar crate plus a node-kind table.

## What it measures

| Pass | Signal | Why it matters |
|---|---|---|
| discovery | file kind (source/test/data/generated/vendored), language | raw metrics on the wrong kind of file are confident nonsense |
| history | churn, fix commits, authors, co-change pairs (from `git log`) | the strongest defect predictor is *how often a thing changes* |
| metrics | cognitive + cyclomatic complexity, nesting, length, params per function | nesting-aware complexity predicts "hard to change safely" |
| deps | import graph, SCC cycles (file and directory), fan-in/out, instability | tangles and hubs are where a change fans out |
| clones | near-exact duplicates via normalised-token winnowing | two copies of a rule drift into two rules |
| regions | inline test regions in Rust source (`#[cfg(test)]` mods and items, `#[test]` fns) | a 2,300-line file that is half `mod tests` is a 1,300-line file; clones and complexity inside tests are not production findings |
| report | percentile-normalised composite, reasons per file | one ranked list, no thresholds to tune per language |

The headline score is the hotspot idea from Tornhill's *Your Code as a Crime
Scene*: churn × complexity, with boosts for fix commits, coupling, duplication
and missing tests. Every ranked file carries the reasons it ranked, with
function names and line ranges. Co-change pairs with no import between them
("hidden coupling") are called out separately: no static tool can see those.

## Configuration

Every threshold, weight and name list is a setting. Defaults are built in; a
`scry.toml` at the scanned root overrides what it names; `--config <file>`
layers on top of that; explicit flags such as `--since` win over both.

```
scry config <repo> > scry.toml   # dump the effective settings, edit what you need
```

Sections match the passes: `[discover]`, `[history]`, `[metrics]`, `[deps]`,
`[clones]`, `[report]`, `[tests]`. A file only has to name what it changes:

```toml
[discover]
exclude = ["art/**", "scripts/*.py"]     # dropped from the walk entirely
test_dirs = ["test", "tests", "qa"]      # replaces the default list

[history]
fix_words = ["fix", "fixes", "fixed", "bug", "regression"]

[clones]
min_tokens = 100

[report.with_history]
hotspot = 0.5                            # other weights keep their defaults
```

Unknown keys are errors, so a typo cannot silently fall back to a default.

## Build

```
cargo build --release
./target/release/scry scan ../some-repo
```

Work is tracked with kanspec (`kanspec ready`, `kanspec status`).
