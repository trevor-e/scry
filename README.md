# scry

Find the parts of a codebase most likely to need refactoring or to hide bugs,
and explain why in terms an LLM (or a person) can act on.

```
scry scan <repo>            # ranked hotspots with reasons + cycles, hidden coupling, clones
scry scan <repo> --json     # the same, machine-readable
scry files | history | metrics | deps | clones | mentions | dead | helpers | strings | clumps | declared <repo>   # one signal at a time
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
| helpers | top-level helpers grouped by name across files and classed verbatim / similar / different contract on a helper-specific normalizer (only bound names anonymised; callees, fields, macros and literals kept), each copy attributed to its introducing commit and `Claude-Session`; deliberate twins (same basename, re-export wrappers, per-adapter dirs, `#[cfg]` gates) suppressed; and small helper bodies (12-40 tokens) found as exact token sequences in other files, with `hoist and call` when the helper is private | each session writes the utility it needs without grepping for it: kanspec's `plural` is byte-identical in `cmd/flow.rs` and `cmd/status.rs` and re-typed with a second parameter in `cmd/proposal.rs`, three commits from two sessions; `io_err` and `render` twice each; the clones pass cannot see a 12-token idiom re-derived in place, and the same tool run on ripgrep and fd finds no verbatim family at all |
| strings | string literals in a message role (`format!` / `println!` / `anyhow!` / `bail!` arguments, `print` / `console.*` / `logger.*` calls, `raise` / `throw`, `return` / `Err` values; docstrings, attributes, asserts, imports, JSX attributes and gettext ids excluded) masked (`{id}`, `${x}`, `%s`, digits) and grouped by exact text across files; config literals (strftime patterns, ALL_CAPS env names, paths, URLs, MIME types, numbers >= 100 in one config role) grouped by text with the named constant, when one exists, pointed at; near-duplicate messages as information | every session hand-writes the same next-step hint and error phrasing without knowing which module owns the constant: kanspec formats one timestamp as `"%Y-%m-%dT%H:%MZ"` in four files while `logentry.rs` already names it `TS_FMT`, scrubs `GIT_WORK_TREE` in three files, and, once its own `fix!` macro is listed in `message_macros`, spells `kanspec show {id}` by hand in ten files (23x); ripgrep, fd, click and hono share almost no message text |
| clumps | every function's named parameters (receivers out) with their type text; every 3- and 4-name combination grouped repo-wide, reported at 4 functions or 2 files when at least 2 slots agree on type, collapsed into the largest tuple the members share, with the slots no member reads (`_`-prefixed or unreferenced in the body) counted per clump and the one caller they all have named; trait / override methods, callbacks and protocol tuples (`(ctx, param, value)`) skipped; weight 0 | agents extend a family by copying the last sibling's signature and keep the dead slot: kanspec's five `plan_*` functions carry `(s: &Snapshot, f, a, _m: &Minter)` with `_m` unused in all five (`plan_decide` alone reads its `m`), while fd's `(stdout, entry, config)` recurs in six `print_entry*` functions with every slot read; clump frequency is similar in human and LLM code, the silenced slot is what separates them |
| declared | Cargo manifests parsed (workspace deps resolved to the member, `package` renames, optional flags, `[features]`, `required-features`); every crate reference in the manifest's scope (`use` trees, `x::` paths in type and value position, `extern crate`, every token tree) counted, so a dependency nothing references is an orphan with its birth commit, age, whether history ever imported it and the doc that promised it; a feature with no `cfg(feature = …)` / build-script / `required-features` consumer that gates only orphaned deps is dead, with the commit that removed its last consumer; a `Deserialize` struct in a config file (or a `toml::from_str` target) whose field no code reads outside `impl Default` / the serialize path / tests is an unread knob, rolled up to its `[section]` with the doc that documents it; an env name production code reads (`env::var`, `os.environ`, `process.env`; every grammar) that carries the product's prefix or a test keyword, that tests mention and no user doc, CI file or production setter names, is a test seam with the birth of its read and of its first test setter; weight 0 | agents declare the stack a design doc lists and never build it, and a cleanup pass deletes the last consumer and leaves the manifest behind: kanspec's `pulldown-cmark` was declared in the root commit and never imported in 101 commits, its `ci-homerunner` feature lost its only `#[cfg]` in 9f2f0f7 and still gates `rusqlite`, and `[ci.homerunner]` (4 knobs, documented) is accepted under `deny_unknown_fields` and read by nothing; ripgrep, fd, scry and the 16 human features have one orphan each at most (`fst` in an `#![allow(warnings)]` crate, fd's `libc`) and no dead feature. An agent asked for a clock or identity seam reaches for the process environment because it needs no plumbing, and the test lands in the same commit: kanspec reads six `KANSPEC_*` names (`KANSPEC_NOW`, `KANSPEC_ACTOR`, `KANSPEC_ID_SEED`, …) that only its tests set, each born with its test setter; ripgrep's `RIPGREP_CONFIG_PATH` is set by tests too but documented in the FAQ, and fd, click and hono read only foreign names |
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
`[dead.shapes]`, `[helpers]`, `[strings]`, `[clumps]`, `[declared]`. A file only has to name what it changes:

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

[helpers]
min_name_len = 6                         # names shorter than this never form a same-name family (default 4)
report_divergent = true                  # also list same-signature families whose bodies differ (default false)
min_tokens = 8                           # smallest helper body searched for as an inlined idiom (default 12)
allow_one_wildcard = true                # one parameter slot may match any expression at the hit (default false)
attribute_commits = false                # skip the per-copy `git log -S` lookups (default true, capped at 50)

[strings]
min_len = 10                             # shortest masked message text that can form a family (default 20)
min_files = 2                            # files a message must be spelled in (default 3; config literals use config_min_files = 2)
message_macros = ["format", "println", "eprintln", "anyhow", "bail", "fix"]   # the macros whose string arguments are messages
ignore_patterns = ["^kanspec show"]      # regexes; a hint printed from ten commands on purpose goes here

[clumps]
min_functions = 5                        # a one-file clump needs this many members (default 4; cross-file needs min_files = 2)
min_typed_slots = 1                      # slots that must agree on type (default 2; a tuple nobody annotates needs min_functions + 1 members)
protocol_tuples = [["c", "next"], ["ctx", "param", "value"], ["self", "request"]]   # callback shapes never analysed
weight = 0.02                            # rank on clump membership too (default 0.0: section and reasons only)

[declared]
check_dev_dependencies = true            # check [dev-dependencies] for orphans too (default false: counted, never reported)
side_effect_deps = ["*-sys", "openssl", "libc"]   # names consumed without an import (default adds tikv-jemallocator, getrandom, …)
report_noop_features = true              # list `x = []` features nothing consumes (default false: counted in a note)
config_formats = ["toml", "serde_yaml"]  # deserialise calls whose targets are config wherever they live (default ["toml"])
require_sibling_read_or_doc = false      # report an unread knob even when no sibling is read and no doc names it
env_prefixes = ["ks"]                    # extra env-name prefixes (the manifest names are derived automatically)
env_keywords = ["TEST", "FIXTURE", "SEED", "MOCK", "FAKE", "STUB", "REPLAY", "FROZEN"]   # a name with one of these is a candidate without a prefix
include_compile_time_env = true          # check env!() / option_env!() / import.meta.env reads too (default false: a note counts them)

[report.with_history]
hotspot = 0.5                            # other weights keep their defaults
dead = 0.0                               # drop the dead-surface term (default 0.05)
strings = 0.03                           # rank on repeated literals too (default 0.0: section and reasons only)
```

Unknown keys are errors, so a typo cannot silently fall back to a default.

## Build

```
cargo build --release
./target/release/scry scan ../some-repo
```

Work is tracked with kanspec (`kanspec ready`, `kanspec status`).
