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
| history | churn, fix commits, authors, co-change pairs (from `git log`); a commit that rewrites most of one directory is a *sweep* (still churn, never pair evidence), a fix word on a commit over the 25-file mass-edit cap is not a fix, bot authors (dependabot, renovate, `[bot]`) never count toward the author set, and every pair carries its lift over chance | the strongest defect predictor is *how often a thing changes*; an agent session that edits every command file at once says nothing about which of them belong together, a 49-file "fix eleven defects" pass says nothing about which file was broken, and a bot is not a second maintainer |
| metrics | cognitive + cyclomatic complexity, nesting, length, params per function | nesting-aware complexity predicts "hard to change safely" |
| deps | import graph, SCC cycles (file and directory), fan-in/out, instability; every edge knows its kind (`use` / `mod` / `type_only`) and how many names it carries, so a large cycle comes with its cheapest single-import cut, a hub cut and a greedy cut set | tangles and hubs are where a change fans out; an agent adds `use crate::x` wherever convenient, so its cycle is dense and the report should say what cutting one import buys instead of repeating the cycle on every member |
| clones | near-exact duplicates via normalised-token winnowing; a uniform table (dispatch `match`, registry array, map literal) matching its own second half is dropped, and pairs whose both sides are runs of uniform entries are tagged `table` and listed under a TABLES sub-heading with the logic/table split in the file's reason | two copies of a rule drift into two rules; two parallel maps over one enum must drift together, but they are not duplicated logic |
| regions | inline test regions in Rust source (`#[cfg(test)]` mods and items, `#[test]` fns) | a 2,300-line file that is half `mod tests` is a 1,300-line file; clones and complexity inside tests are not production findings |
| report | percentile-normalised composite, reasons per file | one ranked list, no thresholds to tune per language |

The headline score is the hotspot idea from Tornhill's *Your Code as a Crime
Scene*: churn × complexity, with boosts for fix commits, coupling, duplication
and missing tests. Every ranked file carries the reasons it ranked, with
function names and line ranges. Import cycles are described once, under
CYCLES, with the cheapest import to cut and the cycle that would leave
(`15 -> 12`), a hub cut, and `no single import breaks this cycle` when that is
the truth; members say which cycle they are in and only the two files of the
cut edge name the symbol. Co-change pairs with no import between them ("hidden
coupling") are called out separately: no static tool can see those.
Sweep commits are left out of those pair counts, pairs below 3x lift are dropped
once the window is long enough, and a pair whose members both import a file that
changed in the same commits is reported as shotgun surgery on that import instead.

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
sweep_min_files = 8                      # a sweep must touch this many files of one directory (default 6)
fix_mass_edit_cap = false                # count fix words on commits over max_cochange_commit_size (default true: they are not fixes)
bot_patterns = ["[bot]", "renovate"]     # author-name substrings that never count as an author (default adds dependabot, pre-commit-ci)
min_lift = 2.0                           # drop co-change pairs under this lift once the window has 20 commits (default 3.0)

[deps]
min_cycle_size_to_cut = 6                # only cycles this large get a cut suggestion (default 4)
cut_rust_cycles = false                  # keep the deduped count line, drop the cut text for Rust

[clones]
min_tokens = 100
table_weight = 0.25                      # dampen table pairs in clone_ratio (default 1.0)

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
