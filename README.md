# scry

Find the parts of a codebase most likely to need refactoring or to hide bugs,
and explain why in terms an LLM (or a person) can act on.

```
scry scan <repo>            # ranked hotspots with reasons + cycles, hidden coupling, clones
scry scan <repo> --json     # the same, machine-readable
scry files | history | metrics | deps | clones | mentions | dead <repo>   # one signal at a time
scry plan <file> [--root <repo>]   # one file's refactor plan (the scan pipeline, one file's output)
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
| mentions | test units (functions in test files, tests inside inline `#[cfg(test)]` regions) that name a source file's symbols as identifier tokens, never in strings; the public symbols no test names | "a test file imports it" is false for CLI-style suites that drive the binary: kanspec's `cmd/status.rs` is imported by no test and named by 51 test functions; the no-tests multiplier now needs zero naming units, and a file most of whose public symbols no test names says which ones, with line ranges |
| plan | per-hotspot refactor steps from findings already made: every clone run resolved to the metrics unit holding its start line (else the preceding top-level item), one `canonicalise_clone` step per pair naming both symbols (`park (932-962) duplicates drop_ticket (1010-1036), 239 tokens: keep one`), clone runs inside `#[cfg(test)]` folded into one step naming the region, an `extract` step per unit over the cognitive threshold, the cycle's cheapest cut when this file is its source; `scry plan <file>` prints one | a CLONES line with two line ranges and no names is a lookup the agent has to do itself; a step that names the symbol on both sides is something it can act on, and a predicted score would be fake precision under a percentile table |
| dead | a symbol index (every definition, every identifier reference by bare name, split into production and test context) built on the trees already parsed; exported symbols referenced nowhere (DEAD), only by tests (TESTONLY, strict: no production use even in-file, named with the test files), or only inside their own file (one per-file percentile line, never per-symbol advice); Rust struct fields nothing reads and enum variants nothing constructs; mode auto/library/application from the manifests, so a library's public API is exempt | LLM sessions write `pub fn` / `export` by default and never narrow visibility, so rustc's `dead_code` goes silent behind an all-`pub mod` lib.rs: kanspec's `hooks::install` has no caller in 81 files, `Git::is_ignored` lives only for two integration tests, and 154 of its 708 pub items are used only in their own file; a Rust 2021 `format!("{BIN_ENV}")` is a use, so string contents count |
| report | percentile-normalised composite, reasons per file | one ranked list, no thresholds to tune per language |

The headline score is the hotspot idea from Tornhill's *Your Code as a Crime
Scene*: churn × complexity, with boosts for fix commits, coupling, duplication,
dead surface (lines of exported symbols with no production use outside their
file) and missing tests (no test unit names any of the file's symbols). Every
ranked file carries the reasons it ranked, with function names and line ranges. Import cycles are described once, under
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
`[clones]`, `[report]`, `[tests]`, `[plan]`, `[dead.symbols]`, `[dead.test_only]`,
`[dead.shapes]`. A file only has to name what it changes:

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

[tests]
min_name_len = 6                         # symbols shorter than this never count as named by a test (default 5)
unmentioned_share_reason = 0.7           # reason when this share of a file's public symbols is named by no test (default 0.5)

[plan]
max_steps = 10                           # steps printed per hotspot before "(+N more)" (default 6)
symbol_fallback = "none"                 # print "?" for a clone run no function holds (default names the preceding item)

[dead.symbols]
mode = "application"                     # check plain `pub` / `export` even in a library crate (default auto: read the manifests)
languages = ["rust", "typescript"]       # TypeScript is checked only in application mode; otherwise the report says to run knip
min_lines = 1                            # also check one-line items (default 2)

[dead.test_only]
min_external_test_refs = 3               # a symbol touched by fewer test references is not test-only (default 2)

[report.with_history]
hotspot = 0.5                            # other weights keep their defaults
dead = 0.0                               # drop the dead-surface term (default 0.05)
```

Unknown keys are errors, so a typo cannot silently fall back to a default.

## Build

```
cargo build --release
./target/release/scry scan ../some-repo
```

Work is tracked with kanspec (`kanspec ready`, `kanspec status`).
