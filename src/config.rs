//! Every tunable in one place, loadable from `scry.toml`.
//!
//! Precedence: built-in defaults < `scry.toml` at the scanned root < `--config <file>`
//! < explicit CLI flags. Every section and every key is optional, so a repo's
//! file only has to name what it changes. `scry config <root>` prints the
//! effective result as TOML, which is the easiest starting point for a new file.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// File name looked for at the scanned root.
pub const FILE_NAME: &str = "scry.toml";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub discover: Discover,
    pub history: History,
    pub metrics: Metrics,
    pub deps: Deps,
    pub clones: Clones,
    pub report: Report,
    pub tests: Tests,
    pub plan: Plan,
    pub dead: Dead,
    pub helpers: Helpers,
    pub strings: Strings,
    pub clumps: Clumps,
    pub declared: Declared,
    pub comments: Comments,
    pub naming: Naming,
    pub fallback: Fallback,
}

impl Config {
    /// Defaults, then the root's `scry.toml` if present, then `explicit` if given.
    pub fn load(root: &Path, explicit: Option<&Path>) -> Result<Self> {
        let mut cfg = Config::default();
        let at_root = root.join(FILE_NAME);
        if at_root.is_file() {
            cfg = Self::parse_into(cfg, &at_root)?;
        }
        if let Some(p) = explicit {
            cfg = Self::parse_into(cfg, p)?;
        }
        Ok(cfg)
    }

    /// A file's contents override `base` field by field; sections it omits keep `base`.
    fn parse_into(base: Self, path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        // Layering: serialize the base, splice the file's tables over it, deserialize.
        let mut merged: toml::Table = toml::Table::try_from(&base).context("serializing defaults")?;
        let over: toml::Table = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        merge_tables(&mut merged, over);
        merged.try_into().with_context(|| format!("invalid {}", path.display()))
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).expect("config serializes")
    }
}

fn merge_tables(base: &mut toml::Table, over: toml::Table) {
    for (k, v) in over {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge_tables(b, o),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

// ---------- discover ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Discover {
    /// Gitignore-style globs dropped from the walk entirely (`art/**`, `scripts/*.py`).
    pub exclude: Vec<String>,
    /// Any path component in this list marks the file Vendored.
    pub vendor_dirs: Vec<String>,
    /// Any path component in this list marks the file Test.
    pub test_dirs: Vec<String>,
    /// Any path component in this list marks the file Data.
    pub data_dirs: Vec<String>,
    /// Any path component in this list marks the file Generated.
    pub generated_dirs: Vec<String>,
    /// File-stem prefixes that mark a Test file (`test_x.py`).
    pub test_stem_prefixes: Vec<String>,
    /// File-stem suffixes that mark a Test file (`x_test.go`, `x.spec.ts`).
    pub test_stem_suffixes: Vec<String>,
    /// Exact stems that are Test files.
    pub test_stems: Vec<String>,
    /// Stem substrings that mark a Data file.
    pub data_stem_contains: Vec<String>,
    /// Lower-cased phrases in the first `generated_marker_lines` lines that mark Generated.
    pub generated_markers: Vec<String>,
    pub generated_marker_lines: usize,
    /// A file this long or longer with a control-flow line share below
    /// `data_max_logic_density` is Data, not Source.
    pub data_min_lines: usize,
    pub data_max_logic_density: f64,
}

impl Default for Discover {
    fn default() -> Self {
        Self {
            exclude: vec![],
            vendor_dirs: strings(&[
                "node_modules", "vendor", "third_party", "thirdparty", "dist", "build", "target",
                ".venv", "venv", "site-packages", "__pycache__", ".git",
            ]),
            test_dirs: strings(&["test", "tests", "__tests__", "e2e", "spec", "specs", "testing"]),
            data_dirs: strings(&["fixtures", "fixture", "snapshots", "__snapshots__", "testdata"]),
            generated_dirs: strings(&["migrations", "generated", "__generated__", "gen"]),
            test_stem_prefixes: strings(&["test_"]),
            test_stem_suffixes: strings(&["_test", ".test", ".spec", ".snapshots", "Probes"]),
            test_stems: strings(&["conftest"]),
            data_stem_contains: strings(&["fixture"]),
            generated_markers: strings(&[
                "@generated", "do not edit", "auto-generated", "automatically generated", "generated by",
            ]),
            generated_marker_lines: 8,
            data_min_lines: 150,
            data_max_logic_density: 0.04,
        }
    }
}

// ---------- history ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct History {
    /// `git log --since` window.
    pub since: String,
    /// Whole words that make a commit subject a fix.
    pub fix_words: Vec<String>,
    /// Commits touching more files than this are mass edits: no co-change evidence, and (when
    /// `fix_mass_edit_cap`) no fix evidence either. Judged on everything the commit touched.
    pub max_cochange_commit_size: usize,
    /// A fix word on a commit over `max_cochange_commit_size` does not make it a fix commit for
    /// the files it touched (a 49-file "review pass: fix eleven defects" says nothing about which
    /// file was broken); it still counts as a commit.
    pub fix_mass_edit_cap: bool,
    /// Case-insensitive substrings of an author name that mark a bot. Bot commits still count as
    /// commits but never toward a file's author set, so the bus-factor reason ignores them.
    pub bot_patterns: Vec<String>,
    /// A co-change pair needs at least this many shared commits…
    pub min_cochange_together: usize,
    /// …and together / min(commits_a, commits_b) at least this. Both are judged on non-sweep counts.
    pub min_cochange_strength: f64,
    /// A commit is a directory sweep when, for some directory holding at least
    /// `sweep_min_dir_files` tracked Source files, it touches at least this share of them…
    pub sweep_fraction: f64,
    /// …in a directory holding at least this many tracked Source files (smaller directories can
    /// never qualify a sweep)…
    pub sweep_min_dir_files: usize,
    /// …and at least this many of them (a 2-file fix in a 4-file directory is not a sweep).
    /// Sweeps still count as churn (commits, fix commits, authors) but never toward pair counts.
    pub sweep_min_files: usize,
    /// Pairs with lift below this are dropped, where lift = together_nonsweep / (commits_a x
    /// commits_b / N) with the per-file commit counts (sweeps included) and N = non-sweep commits…
    pub min_lift: f64,
    /// …but only once N is at least this; below it lift is reported and not applied.
    pub min_commits_for_lift: usize,
    /// A pair whose members both import a file that changed in at least this share of their
    /// co-commits is `explained_by` that import: shotgun surgery on it, not hidden coupling.
    pub explained_min_share: f64,
}

impl Default for History {
    fn default() -> Self {
        Self {
            since: "6 months ago".into(),
            fix_words: strings(&[
                "fix", "fixes", "fixed", "fixing", "bugfix", "bugfixes", "hotfix", "hotfixes",
                "bug", "bugs", "buggy", "regression", "regressions", "regress", "regressed",
                "broke", "broken", "crash", "crashes", "crashed", "crashing",
                "repair", "repairs", "repaired", "correct", "corrects", "corrected", "correction",
                "patch", "patched", "wrong", "incorrect", "flake", "flaky", "flakey",
            ]),
            max_cochange_commit_size: 25,
            fix_mass_edit_cap: true,
            bot_patterns: strings(&["[bot]", "dependabot", "renovate", "pre-commit-ci"]),
            min_cochange_together: 3,
            min_cochange_strength: 0.4,
            sweep_fraction: 0.5,
            sweep_min_dir_files: 4,
            sweep_min_files: 6,
            min_lift: 3.0,
            min_commits_for_lift: 20,
            explained_min_share: 0.5,
        }
    }
}

// ---------- metrics ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Metrics {
    /// Functions above this cognitive complexity are "hard to follow".
    pub cognitive_hard: u32,
    /// A unit is a brain method when it is at least this many lines long…
    pub brain_min_lines: usize,
    /// …with at least this cognitive complexity…
    pub brain_min_cognitive: u32,
    /// …and at least this many distinct local bindings (`locals` on the function). The label
    /// is text on the cognitive reason only: no section, no score weight.
    pub brain_min_locals: usize,
}

impl Default for Metrics {
    fn default() -> Self {
        Self { cognitive_hard: 15, brain_min_lines: 100, brain_min_cognitive: 15, brain_min_locals: 15 }
    }
}

// ---------- deps ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Deps {
    /// JS/TS import-prefix aliases: `"@/" = "src"` maps `@/x` to `<nearest src>/x`.
    /// The target is looked for under every ancestor of the importing file.
    pub js_aliases: BTreeMap<String, String>,
    /// A file cycle with at least this many members gets a cut suggestion; smaller ones only
    /// the deduped count line.
    pub min_cycle_size_to_cut: usize,
    /// Internal edges tried for the single-edge cut, cheapest first (an SCC of 15 files has ~70).
    pub max_edges_tried: usize,
    /// The greedy cut set is reported only when it dissolves the cycle within this many edges.
    pub max_cut_set: usize,
    /// Symbol cost of a wildcard import (`use x::*`, `from x import *`, `import * as ns`):
    /// what it pulls in is unknown, so it is never the cheap cut.
    pub glob_import_symbol_cost: u32,
    /// Also try dropping every non-`mod` import of one member (the hub cut); reported when it
    /// leaves a smaller cycle than the best single edge.
    pub report_hub_cut: bool,
    /// Print the cycle once under CYCLES and `in the 15-file src cycle (cut: a -> b, Sym)` on
    /// members, instead of `in an import cycle of 15 files` on every member.
    pub dedupe_cycle_reason: bool,
    /// Compute cuts for Rust cycles. Off keeps the deduped count line but no cut text: the
    /// idiomatic-Rust argument belongs in `[report].cycle_coupling`, not in hiding the cut.
    pub cut_rust_cycles: bool,
    /// TS `import type` / `import { type X }` edges are erased at runtime: leave them out of
    /// the cycle the cuts are searched on.
    pub ignore_type_only_imports: bool,
    /// `no single import breaks this cycle` is appended when the best single cut still leaves
    /// a cycle of at least this share of the members.
    pub no_single_cut_share: f64,
}

impl Default for Deps {
    fn default() -> Self {
        Self {
            js_aliases: [("@/".to_string(), "src".to_string()), ("~/".to_string(), "src".to_string())].into(),
            min_cycle_size_to_cut: 4,
            max_edges_tried: 400,
            max_cut_set: 6,
            glob_import_symbol_cost: 20,
            report_hub_cut: true,
            dedupe_cycle_reason: true,
            cut_rust_cycles: true,
            ignore_type_only_imports: true,
            no_single_cut_share: 0.8,
        }
    }
}

// ---------- clones ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Clones {
    /// Tokens per fingerprint window.
    pub k: usize,
    /// Winnowing window: one fingerprint kept per `w` consecutive k-grams.
    pub w: usize,
    /// Shortest run worth reporting, in tokens.
    pub min_tokens: usize,
    /// A fingerprint in more files than this is boilerplate.
    pub max_files: usize,
    /// Hard cap on positions per fingerprint.
    pub max_locations: usize,
    /// Drop a same-file run whose two ranges sit in one container (or nested ones) whose
    /// entries across both ranges are a uniform table: the first half of a `match`, array or
    /// map matching its second half. Sibling items that merely copy each other are kept.
    pub drop_same_container_self_match: bool,
    /// A run's range resolves to the smallest node spanning it, then down into a child spanning
    /// at least this share of its bytes: a run that spills a few tokens past its own item still
    /// resolves to that item, while the two halves of one container resolve to the container.
    /// The same share is what a table's entries must span of the run they are tagged on.
    pub dominant_child_share: f64,
    /// A run side is a table when its container has at least this many consecutive same-kind
    /// named children spanning at least `dominant_child_share` of the run (six imports at the
    /// top of a copied file are under the run, not the run)…
    pub table_min_entries: usize,
    /// …the dominant entry shape (names, paths and literals collapsed) covers at least this
    /// share of them…
    pub table_min_dominant_shape: f64,
    /// …no entry has more named nodes than this (a loose safety cap)…
    pub table_max_entry_nodes: usize,
    /// …and no entry contains one of these node kinds, per grammar (`rust`, `typescript`,
    /// `python`). Rust `try_expression` (`?`) must not be listed: it is in every dispatch arm.
    pub table_control_kinds: BTreeMap<String, Vec<String>>,
    /// Weight of table clone lines in `clone_ratio`: `(logic + table_weight x table) / lines`.
    /// 1.0 ranks parallel maps that must drift together; 0.25 dampens them.
    pub table_weight: f64,
    /// Print table pairs under a TABLES sub-heading of CLONES instead of inline.
    pub list_tables_separately: bool,
}

impl Default for Clones {
    fn default() -> Self {
        Self {
            k: 30,
            w: 20,
            min_tokens: 70,
            max_files: 40,
            max_locations: 2000,
            drop_same_container_self_match: true,
            dominant_child_share: 0.9,
            table_min_entries: 6,
            table_min_dominant_shape: 0.6,
            table_max_entry_nodes: 40,
            table_control_kinds: [
                ("rust", strings(&["if_expression", "match_expression", "for_expression", "while_expression", "loop_expression", "closure_expression"])),
                ("typescript", strings(&["if_statement", "switch_statement", "for_statement", "for_in_statement", "while_statement", "do_statement", "try_statement", "ternary_expression", "arrow_function", "function_expression"])),
                ("python", strings(&["if_statement", "for_statement", "while_statement", "try_statement", "match_statement", "with_statement", "conditional_expression", "lambda", "list_comprehension", "dictionary_comprehension", "set_comprehension", "generator_expression"])),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
            table_weight: 1.0,
            list_tables_separately: true,
        }
    }
}

// ---------- tests ----------

/// Inline test regions (`#[cfg(test)] mod`, `#[cfg(test)]` items, bare `#[test]` fns) in Rust
/// Source files, and the test-mention index that replaces "a test file imports it".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Tests {
    /// Apply inline test regions: metrics units inside one are tagged `in_test` and left out of
    /// file totals, `inline_test_lines` come off the size signal, and the clone tokenizer skips
    /// their bytes. Off means every line is source; regions are still detected and listed, and
    /// the mention index finds them on its own tree, so `count_inline_tests_as_refs` is
    /// unaffected.
    pub inline_modules: bool,
    /// A hotspot whose inline test lines / file lines is at or above this gets
    /// `N in #[cfg(test)] mod at a-b` after its line count.
    pub report_inline_ratio_above: f64,
    /// A Source file whose inline test ratio is above this is treated as a Test file for
    /// ranking: it is not a hotspot and does not count toward source lines.
    pub reclassify_file_above_ratio: f64,
    /// Functions inside inline test regions are test units: one that names a symbol of any
    /// Source file counts toward that file's `test_units`. Off, only Test files hold test units.
    pub count_inline_tests_as_refs: bool,
    /// Symbols with names shorter than this are never matched (`run`, `new`, `get`, `parse`
    /// collide with every test); 4 still lets `execute`-length names through, 5 does not
    /// let `commit`/`is_empty`-length generic names collide.
    pub min_name_len: usize,
    /// A file whose share of public symbols named by no test unit is at or above this gets
    /// `N of M public symbols are named by no test: a (lines 1-9), …`…
    pub unmentioned_share_reason: f64,
    /// …when it has at least this many public symbols (a two-symbol file has no share worth a line).
    pub unmentioned_min_symbols: usize,
}

impl Default for Tests {
    fn default() -> Self {
        Self {
            inline_modules: true,
            report_inline_ratio_above: 0.5,
            reclassify_file_above_ratio: 0.9,
            count_inline_tests_as_refs: true,
            min_name_len: 5,
            unmentioned_share_reason: 0.5,
            unmentioned_min_symbols: 3,
        }
    }
}

// ---------- plan ----------

/// What names a clone run that no metrics unit contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolFallback {
    /// The nearest preceding top-level named item (a `struct`, `impl`, `static`, a TS
    /// `const`, a Python assignment): the run sits in or after it.
    PrecedingItem,
    /// Print `?`.
    None,
}

/// The per-hotspot refactor plan: clone runs resolved to symbols, one step per finding the
/// other passes already made (a clone pair, a unit over `cognitive_hard`, a cycle cut).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Plan {
    /// Steps printed per hotspot; the rest are counted as `(+N more)`.
    pub max_steps: usize,
    /// Clone runs inside an inline test region (or a Test-classified file) become one
    /// `fold_test_clones` step per file naming the region and the run count. Off, they are
    /// silently left out; either way they never become `canonicalise_clone` steps.
    pub fold_test_clones: bool,
    /// A clone run's `symbol` is the innermost metrics unit containing its start line; when
    /// none does, `preceding_item` names the nearest preceding top-level item and `none`
    /// prints `?`.
    pub symbol_fallback: SymbolFallback,
    /// Step kinds in print order. A kind left out of the list is not planned.
    pub kind_priority: Vec<String>,
    /// Print the `PLAN` block under each hotspot in the text report; `--json` and `scry plan`
    /// carry the steps either way.
    pub include_in_text_report: bool,
}

impl Default for Plan {
    fn default() -> Self {
        Self {
            max_steps: 6,
            fold_test_clones: true,
            symbol_fallback: SymbolFallback::PrecedingItem,
            kind_priority: strings(&["fold_test_clones", "canonicalise_clone", "extract", "cut_cycle_edge"]),
            include_in_text_report: true,
        }
    }
}

// ---------- dead ----------

/// `auto` reads the manifests; `library` exempts plain `pub` / `export` (only restricted
/// visibility is checked); `application` checks every exported symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeadMode {
    Auto,
    Library,
    Application,
}

/// The dead-surface pass: exported symbols nothing outside their file uses (`[dead.symbols]`),
/// symbols only tests keep alive (`[dead.test_only]`), and Rust fields nothing reads / variants
/// nothing constructs (`[dead.shapes]`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Dead {
    pub symbols: DeadSymbols,
    pub test_only: DeadTestOnly,
    pub shapes: DeadShapes,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeadSymbols {
    /// Off: the index is still built (other passes read it) but nothing is categorised.
    pub enabled: bool,
    /// `auto` resolves the nearest manifest per file: a Cargo.toml with `[lib]` and no `[[bin]]`
    /// (nor `src/main.rs`) is a library, so is a package.json with `exports` and a pyproject
    /// without `[project.scripts]`; anything else is an application. A library exempts plain
    /// `pub` / `export` and checks only `pub(crate)` / `pub(super)`.
    pub mode: DeadMode,
    /// Files whose exported symbols are a library's public API: exempt in library mode. Globs
    /// match the repo-relative path and the manifest-relative path.
    pub api_roots: Vec<String>,
    /// Files whose exported symbols are never candidates (a binary's `main.rs` has no caller);
    /// their references still count.
    pub entrypoints: Vec<String>,
    /// Read Cargo.toml `[[bin]]` / `[lib]`, package.json `exports` / `main` / `module` / `types` /
    /// `bin` (by target, `dist/` remapped to `src/`, wildcards as globs) and pyproject
    /// `[project.scripts]` for the mode and for extra api roots and entrypoints.
    pub read_manifests: bool,
    /// A Rust item under one of these attributes (`#[test]`, `#[tokio::test]` by its last
    /// segment, `#[no_mangle]`) is never a candidate: something outside the crate calls it.
    pub skip_attrs: Vec<String>,
    /// Python decorators (exact, or a `prefix.*` glob) that mark a definition framework-called.
    pub skip_decorators: Vec<String>,
    /// Names never reported (JSX namespace conventions).
    pub skip_names: Vec<String>,
    /// Names starting with one of these are never reported.
    pub skip_name_prefixes: Vec<String>,
    /// TS exports a framework calls by convention (`default`, Next's `metadata`, Remix's
    /// `loader`): never candidates.
    pub ts_framework_exports: Vec<String>,
    /// TS calls whose arguments are test context (`describe('x', () => …)`).
    pub ts_test_calls: Vec<String>,
    /// A string literal references `name` when a dot- or colon-separated segment of it equals
    /// `name` (`'app.apps.AppConfig'`, `'pkg.mod:func'`) or it holds `{name}` / `{name:` (a
    /// Rust 2021 inline format argument, which is string content inside a `token_tree`).
    pub string_refs: bool,
    /// Items spanning fewer lines are never candidates (a one-line `pub const`).
    pub min_lines: usize,
    /// Symbol classes checked: `fn` (functions, methods), `type` (structs, enums, unions,
    /// traits, type aliases, classes, interfaces), `const` (consts, statics, variables), `mod`.
    pub kinds: Vec<String>,
    /// `rust`, `typescript` (also tsx and javascript), `python`. TypeScript is checked only under
    /// an explicit `mode = "application"`; otherwise the section says to run knip. Python never
    /// gets an in-file-only line.
    pub languages: Vec<String>,
    /// The `N of M pub items are referenced only inside this file` line needs at least this many
    /// candidates in the file…
    pub overexported_min_items: usize,
    /// …and at least this share of them referenced only in-file.
    pub overexported_min_share: f64,
    /// Per-symbol reason lines per file (dead before test-only, longest first); the rest are
    /// counted, and `dead_symbols[]` in `--json` has them all. Also how many in-file-only
    /// symbols the per-file line names.
    pub max_reported_per_file: usize,
}

impl Default for DeadSymbols {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: DeadMode::Auto,
            api_roots: strings(&["src/lib.rs", "**/index.ts", "**/__init__.py"]),
            entrypoints: strings(&["src/main.rs", "src/bin/**", "build.rs", "benches/**", "examples/**", "fuzz/**", "**/__main__.py", "conftest.py", "build/**", "scripts/**", "*.config.ts"]),
            read_manifests: true,
            skip_attrs: strings(&["test", "bench", "no_mangle", "wasm_bindgen", "pyfunction", "tauri::command", "proc_macro"]),
            skip_decorators: strings(&["pytest.fixture", "app.route", "router.*"]),
            skip_names: strings(&["IntrinsicAttributes", "ElementChildrenAttribute", "ElementType", "IntrinsicElements"]),
            skip_name_prefixes: strings(&["_", "test_"]),
            ts_framework_exports: strings(&["default", "metadata", "generateStaticParams", "loader", "action", "config"]),
            ts_test_calls: strings(&["describe", "it", "test"]),
            string_refs: true,
            min_lines: 2,
            kinds: strings(&["fn", "type", "const"]),
            languages: strings(&["rust"]),
            overexported_min_items: 4,
            overexported_min_share: 0.5,
            max_reported_per_file: 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeadTestOnly {
    pub enabled: bool,
    /// A symbol with no production reference is test-only once its test references (own file
    /// and others together) reach this; fewer, and it is not reported at all.
    pub min_external_test_refs: usize,
    /// Off: references from files matching `bench_globs` are test context.
    pub treat_benches_as_prod: bool,
    /// Benchmarks, examples and fuzz targets: test context unless `treat_benches_as_prod`.
    pub bench_globs: Vec<String>,
    /// Files that are test context whatever discovery classified them as.
    pub extra_test_globs: Vec<String>,
    /// Test-only reason lines per file.
    pub max_reported_per_file: usize,
}

impl Default for DeadTestOnly {
    fn default() -> Self {
        Self {
            enabled: true,
            min_external_test_refs: 2,
            treat_benches_as_prod: false,
            bench_globs: strings(&["benches/**", "examples/**", "fuzz/**"]),
            extra_test_globs: strings(&["**/testutil*.rs", "**/test_utils*", "**/fixtures/**"]),
            max_reported_per_file: 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeadShapes {
    pub enabled: bool,
    /// Only `rust` is implemented.
    pub languages: Vec<String>,
    /// A struct or enum is a candidate only when every derive is in this list (a derive outside
    /// it may read fields or construct variants on its own)…
    pub allow_derives: Vec<String>,
    /// …or in this one: derives that read every field but construct nothing, so a struct
    /// deriving one has no never-read field while an enum deriving one is still checked for
    /// never-constructed variants.
    pub serializing_derives: Vec<String>,
    /// A type under one of these attributes (`#[repr]`: layout is the point) is never a candidate.
    pub skip_attrs: Vec<String>,
    /// `#[non_exhaustive]` always hides "never constructed or matched"; on, it also hides
    /// "matched but never constructed".
    pub non_exhaustive_hides_handled: bool,
    /// An `impl Trait for Enum` with one of these traits (or `#[derive(Default)]`) constructs
    /// variants the walk cannot see: every variant of that enum is exempt.
    pub exempt_impl_traits: Vec<String>,
    /// `{name}` format captures inside macro strings count as field reads and variant
    /// constructions (path segments never do: `"index.db"` is a file name, not a read of `db`).
    pub string_refs: bool,
    /// Report a variant that is matched somewhere but constructed nowhere.
    pub report_handled_never_produced: bool,
    /// Only types with a production reference outside their file are checked: a dead type's
    /// fields are noise on top of the type.
    pub require_type_reachable: bool,
    /// Shape reason lines per file.
    pub max_reported_per_file: usize,
}

impl Default for DeadShapes {
    fn default() -> Self {
        Self {
            enabled: true,
            languages: strings(&["rust"]),
            allow_derives: strings(&["Debug", "Clone", "Copy", "PartialEq", "Eq", "Hash", "Default", "PartialOrd", "Ord"]),
            serializing_derives: strings(&["Serialize"]),
            skip_attrs: strings(&["repr"]),
            non_exhaustive_hides_handled: false,
            exempt_impl_traits: strings(&["FromStr", "TryFrom", "From", "Default", "Deref"]),
            string_refs: true,
            report_handled_never_produced: true,
            require_type_reachable: true,
            max_reported_per_file: 5,
        }
    }
}

// ---------- helpers ----------

/// The helpers pass: same-name helpers defined in several files (`min_files`, twin
/// suppression, attribution) and small helper bodies inlined where the helper should have been
/// called (`min_tokens` .. `max_helper_tokens`, `min_occurrences`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Helpers {
    /// A name family needs definitions in at least this many distinct Source files; an inlined
    /// helper needs hits in at least this many files.
    pub min_files: usize,
    /// Two bodies are `similar` when the 5-gram Jaccard of their helper-normalized tokens is at
    /// least this (1.0 is `verbatim`); below it with an equal signature they are `divergent`.
    pub min_body_jaccard: f64,
    /// Names shorter than this never form a family (single-letter TypeVars, `V`, `R`).
    pub min_name_len: usize,
    /// Report families whose copies share a signature but whose bodies fall under
    /// `min_body_jaccard` (noise in every corpus tried); they are section-only info either way.
    pub report_divergent: bool,
    /// Compare consts and statics on their raw value text: a const whose value is one literal
    /// would otherwise look verbatim with every other string const.
    pub compare_consts_raw: bool,
    /// Also group inherent-impl methods, keyed `Type::name` (produces `read` / `write` twins).
    pub include_inherent_methods: bool,
    /// Also group definitions in test context (Test files, `#[cfg(test)]` regions).
    pub include_test_helpers: bool,
    /// Attribute each reported copy to its introducing commit (`git log -S'fn name' -- file`,
    /// oldest hit) and the `Claude-Session` trailer of that commit; the reason counts commits
    /// and distinct sessions. Needs git history (`--no-history` turns it off).
    pub attribute_commits: bool,
    /// Copies attributed per scan; the rest print without commits.
    pub max_git_lookups: usize,
    /// Names that never form a family (trait-method conventions, entrypoints).
    pub ignore_names: Vec<String>,
    /// A definition, or a whole `mod`, under `#[cfg(...)]` naming one of these (or `test`) is
    /// a platform / feature twin, not a re-implementation, and never joins a family.
    pub cfg_gate_attrs: Vec<String>,
    /// A family with a defining file matching one of these globs is a deliberate per-adapter
    /// twin and is dropped.
    pub sibling_dir_globs: Vec<String>,
    /// Drop a family when two defining files share a basename in different directories
    /// (`jsx/base.ts` vs `jsx/dom/base.ts`): a server / DOM or enabled / disabled pair.
    pub suppress_path_suffix_twins: bool,
    /// Drop a family when its defining files are linked by an import edge either way (a
    /// re-export wrapper, or a deliberate twin beside the module it imports), not a copy.
    pub suppress_import_linked: bool,
    /// Score weight of the percentile of `helper_copies + inlined_idioms` (0: the section and
    /// reasons print, the ranking ignores them).
    pub weight: f64,
    /// Inlined idioms: a helper is a candidate when its normalized body has this many tokens…
    pub min_tokens: usize,
    /// …up to this many.
    pub max_helper_tokens: usize,
    /// Inline copies (outside any definition of the same name) needed to report a helper…
    pub min_occurrences: usize,
    /// …with at least this many distinct token kinds in its body (bound name, kept name,
    /// number, string, keyword / punctuation), one keyword or operator among them and one kept
    /// name or literal.
    pub min_distinct_kinds: usize,
    /// One bound-name (`ID`) slot of the helper may match any single expression node at the
    /// hit (`n == 1` matching `e.rules == 1`); off, hits are exact token sequences.
    pub allow_one_wildcard: bool,
    /// Only report hits in files that can call the helper (not private; same crate or package,
    /// or already importing its file).
    pub require_reachable: bool,
    /// Helpers never searched for as inlined idioms.
    pub inline_ignore_names: Vec<String>,
    /// Helper reason lines per file (families first, then inlined idioms); the rest are counted.
    pub max_reported_per_file: usize,
}

impl Default for Helpers {
    fn default() -> Self {
        Self {
            min_files: 2,
            min_body_jaccard: 0.5,
            min_name_len: 4,
            report_divergent: false,
            compare_consts_raw: true,
            include_inherent_methods: false,
            include_test_helpers: false,
            attribute_commits: true,
            max_git_lookups: 50,
            ignore_names: strings(&["new", "default", "main", "fmt", "from", "parse", "run", "read", "write", "get", "set", "len", "is_empty", "from_str"]),
            cfg_gate_attrs: strings(&["unix", "windows", "target_os", "target_family", "feature"]),
            sibling_dir_globs: strings(&["**/adapter/*/**"]),
            suppress_path_suffix_twins: true,
            suppress_import_linked: true,
            weight: 0.0,
            min_tokens: 12,
            max_helper_tokens: 40,
            min_occurrences: 2,
            min_distinct_kinds: 3,
            allow_one_wildcard: false,
            require_reachable: false,
            inline_ignore_names: strings(&["main", "new", "default"]),
            max_reported_per_file: 5,
        }
    }
}

// ---------- strings ----------

/// The repeated-literals pass: message strings spelled in several files (exact families on the
/// masked text), config literals (time formats, env names, paths, URLs, MIME types, numbers in
/// a config role) duplicated with no shared constant, and near-duplicate message pairs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Strings {
    /// A message literal is prose when its masked text has at least this many characters…
    pub min_len: usize,
    /// …and at least this many whitespace-separated words (it must contain whitespace).
    pub exact_min_words: usize,
    /// An exact message family needs sites in at least this many distinct Source files…
    pub min_files: usize,
    /// …and a config-literal family in at least this many.
    pub config_min_files: usize,
    /// Only literals in a message role are prose: an argument of a `message_macros` macro or a
    /// `message_calls` call, a `raise` / `throw` value, or a `return` / `Err(...)` value. Off,
    /// every prose-shaped literal counts.
    pub require_message_role: bool,
    /// Rust macros (last path segment) whose string arguments are messages.
    pub message_macros: Vec<String>,
    /// Callee name pieces (`console.log`, `logger.info`, `log::warn!`, `warnings.warn`, `print`)
    /// whose string arguments are messages; `raise` and `throw` here make those statements
    /// message roles.
    pub message_calls: Vec<String>,
    /// Two distinct message texts with word-set Jaccard at or above this (and below 1) are a
    /// near-duplicate pair…
    pub near_jaccard: f64,
    /// …when both have at least this many words…
    pub min_words: usize,
    /// …compared inside buckets of texts sharing a word seen in at most this many distinct texts.
    pub rare_word_df: usize,
    /// Near pairs print as information and never count toward `family_literals`.
    pub near_info_only: bool,
    /// Collect Rust raw string literals too.
    pub include_raw: bool,
    /// Drop strings inside assert-family macros, calls and statements (`assert!`, `assert_eq!`,
    /// `self.assertEqual`, `expect(...)`, `assert x, "msg"`).
    pub exclude_assert_calls: bool,
    /// Drop Python docstrings (the first statement of a module, class or function body).
    pub exclude_docstrings: bool,
    /// Drop strings inside Rust attributes and TS decorators (`#[serde(rename = "x")]`).
    pub exclude_attributes: bool,
    /// Drop JSX attribute values (`className="…"`).
    pub exclude_jsx_attributes: bool,
    /// Calls whose direct string argument is a translation id, never a message.
    pub gettext_calls: Vec<String>,
    /// Regexes; a literal matching one (as written, or masked) is never collected. A repo that
    /// prints one hint from ten commands on purpose lists it here.
    pub ignore_patterns: Vec<String>,
    /// A literal matching this (as written) is never prose: format strings, ALL_CAPS names,
    /// paths, URLs and header-like `content-type` words route to the config classes instead.
    pub nonprose_regex: String,
    /// Config classes checked, in order; the first match wins. `number` covers numeric literals.
    pub config_classes: Vec<String>,
    /// The regex per string class (`strftime`, `env_name`, `url`, `mime`, `path`), matched
    /// against the literal as written, in any position.
    pub config_patterns: BTreeMap<String, String>,
    /// An integer literal is a config candidate from this value up (floats always are)…
    pub number_min: i64,
    /// …unless it is one of these. A number needs a config role (const / static, struct field
    /// initializer, object pair, keyword argument, default parameter, top-level const, builder
    /// call) and forms a family only with the same folded role name in another file.
    pub ignore_numbers: Vec<i64>,
    /// Reason lines per file (the message line, then config families); the rest are counted.
    pub max_reported_per_file: usize,
    /// Sites a family line lists before `+N more`; `sites[]` in `--json` has them all.
    pub max_sites_listed: usize,
    /// Literal text is cut to this many characters in lines and reasons.
    pub display_text_len: usize,
}

impl Default for Strings {
    fn default() -> Self {
        Self {
            min_len: 20,
            exact_min_words: 2,
            min_files: 3,
            config_min_files: 2,
            require_message_role: true,
            message_macros: strings(&["format", "println", "eprintln", "write", "writeln", "anyhow", "bail", "panic", "assert", "assert_eq", "assert_ne", "debug_assert", "debug_assert_eq", "debug_assert_ne"]),
            message_calls: strings(&["print", "console", "logger", "logging", "log", "warnings", "tracing", "raise", "throw"]),
            near_jaccard: 0.8,
            min_words: 3,
            rare_word_df: 40,
            near_info_only: true,
            include_raw: true,
            exclude_assert_calls: true,
            exclude_docstrings: true,
            exclude_attributes: true,
            exclude_jsx_attributes: true,
            gettext_calls: strings(&["_", "gettext", "ngettext", "pgettext", "dgettext", "t", "i18n"]),
            ignore_patterns: vec![],
            nonprose_regex: "^(%|[A-Z_]{4,}$|[./~]|https?://|[a-z]+(-[a-z]+)+$)".into(),
            config_classes: strings(&["strftime", "env_name", "url", "mime", "path", "number"]),
            config_patterns: [
                ("strftime", "%[YmdHMS]"),
                ("env_name", "^[A-Z][A-Z0-9_]{5,}$"),
                ("url", r"^[a-z][a-z0-9+.-]*://\S+$"),
                ("mime", r"^(application|text|image|audio|video|multipart|message|font|model)/[a-z0-9.+*-]+$"),
                ("path", r"^(\.{1,2}/|~/|/)?[\w.@+-]+(/[\w.@+-]+)+/?$"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
            number_min: 100,
            ignore_numbers: vec![0, 1, 2, 10, 100, 1024],
            max_reported_per_file: 3,
            max_sites_listed: 8,
            display_text_len: 80,
        }
    }
}

// ---------- clumps ----------

/// The parameter-clumps pass: groups of parameter names recurring across functions, with the
/// slots no member reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Clumps {
    /// Smallest name combination grouped…
    pub min_group: usize,
    /// …and the largest (combinations of `min_group`..=`max_group` names are counted; groups
    /// with one member set collapse into the largest tuple those functions share).
    pub max_group: usize,
    /// Named parameters kept per function (receivers left out), in declared order.
    pub max_params: usize,
    /// A group is a clump at this many member functions…
    pub min_functions: usize,
    /// …or with members in this many distinct files…
    pub min_files: usize,
    /// …either one (on) or both (off).
    pub report_if_either: bool,
    /// Slots whose type text every annotated member agrees on, needed for a clump to be
    /// reported; a tuple no member annotates at all is reported at `min_functions + 1` members
    /// instead.
    pub min_typed_slots: usize,
    /// Characters of the joined tuple (`sfa` = 3), applied only to unannotated tuples: a tuple
    /// of one-letter names counts when typed.
    pub min_name_len: usize,
    /// A parameter starting with this is unused by definition; empty turns that off.
    pub unused_prefix: String,
    /// Leave out signatures the function does not own: Rust trait impls and trait items,
    /// Python methods of classes with superclasses, TS methods of classes with heritage, and
    /// callback arrows passed as arguments.
    pub skip_trait_impls: bool,
    /// Parameter name sets of callback protocols; a function whose parameters cover one is
    /// never analysed. Receivers (`self`, `cls`, `this`) are dropped before the check, so a
    /// tuple naming one never matches.
    pub protocol_tuples: Vec<Vec<String>>,
    /// Leave out Python `__dunder__` functions.
    pub skip_dunder: bool,
    /// Leave out Python `@overload` definitions.
    pub skip_overloads: bool,
    /// Functions with the same name in one file (cfg-gated variants) count once.
    pub dedupe_same_name_in_file: bool,
    /// Name the one function every member is referenced from (`all called from run`).
    pub note_single_caller: bool,
    /// Score weight of the percentile of `clump_members` (0: the section and reasons print,
    /// the ranking ignores them).
    pub weight: f64,
    /// Reason lines per file, then `(+N more parameter clumps in clumps)`.
    pub max_reported_per_file: usize,
    /// Member sites a line lists before `+N more`; `functions[]` in `--json` has them all.
    pub max_sites_listed: usize,
}

impl Default for Clumps {
    fn default() -> Self {
        Self {
            min_group: 3,
            max_group: 4,
            max_params: 8,
            min_functions: 4,
            min_files: 2,
            report_if_either: true,
            min_typed_slots: 2,
            min_name_len: 3,
            unused_prefix: "_".into(),
            skip_trait_impls: true,
            protocol_tuples: vec![strings(&["c", "next"]), strings(&["req", "res", "next"]), strings(&["ctx", "param", "value"])],
            skip_dunder: true,
            skip_overloads: true,
            dedupe_same_name_in_file: true,
            note_single_caller: true,
            weight: 0.0,
            max_reported_per_file: 3,
            max_sites_listed: 8,
        }
    }
}

// ---------- declared ----------

/// The declared-but-unconsumed pass: orphaned dependencies, dead feature flags, unread config
/// knobs (Cargo only in this version; the JavaScript and Python knobs are reserved) and env
/// names production code reads that only tests set (every grammar).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Declared {
    /// Languages whose manifests are checked; only `rust` is implemented (`javascript`,
    /// `python` are accepted and ignored until their legs land). The env-seam leg reads every
    /// grammar regardless.
    pub languages: Vec<String>,
    /// Manifest files, found by glob; only `Cargo.toml` is parsed, the others are counted.
    pub manifest_globs: Vec<String>,
    /// Check `[dev-dependencies]` too (off: they count in the total, never as orphans).
    pub check_dev_dependencies: bool,
    /// Report a `[dependencies]` entry only test code references, as `move it to
    /// [dev-dependencies]`.
    pub check_placement: bool,
    /// Globs of dependency names that are consumed without an import (linked, registered,
    /// enabled by a feature); never orphans.
    pub side_effect_deps: Vec<String>,
    /// Docs searched for an orphan's name (`; mentioned in DESIGN.md:632`).
    pub doc_globs: Vec<String>,
    /// An orphan or dead feature whose manifest line is younger than this many commits is not
    /// reported (needs git).
    pub min_age_commits: usize,
    /// Skip crates whose root file carries `#![allow(warnings)]` / `#![allow(unused)]` (off:
    /// their orphans print with a note that the crate silences warnings).
    pub skip_lint_silenced_crates: bool,
    /// Orphans and dead features enriched from git (birth commit, last consumer), most first.
    pub max_git_lookups: usize,
    /// List `x = []` features nothing consumes.
    pub report_noop_features: bool,
    /// List dead features gating no dependency (they enable only other dead features).
    pub report_bare_dead_features: bool,
    /// CI and build files grepped for a feature name: `consumed only by CI` downgrades the
    /// line, never suppresses it.
    pub feature_ci_globs: Vec<String>,
    /// Files whose `Deserialize` structs are config (`config.rs`, `settings.py`, …).
    pub config_file_regex: String,
    /// Struct names that are config wherever they live.
    pub config_struct_regex: String,
    /// Format crates whose deserialise targets (`toml::from_str::<T>`, `let c: T =
    /// toml::from_str(…)`) are config wherever they live; JSON and YAML targets are usually
    /// payloads and frontmatter, so they need the file or name rule unless listed here.
    pub config_formats: Vec<String>,
    /// Methods named like the write side of a config (`render`, `to_toml`, `serialize`, …) in
    /// an `impl` block for a config struct itself: their field reads are the Serialize path,
    /// never a read. The same names on any other type read normally.
    pub serialize_fn_regex: String,
    /// The config structs are public API: check lib-only crates too.
    pub config_is_public_api: bool,
    /// Skip crates with no `[[bin]]` / `src/main.rs` unless `config_is_public_api`.
    pub require_bin_target: bool,
    /// Report an unread knob only when a sibling field of its struct is read or the knob is
    /// documented (`public_doc_globs`).
    pub require_sibling_read_or_doc: bool,
    /// User-facing docs: their TOML / YAML fences document a knob (`documented at
    /// docs/config.md:91-93`), and an env name any of them mentions is public, never a seam.
    pub public_doc_globs: Vec<String>,
    /// A field name shorter than this is skipped only when another candidate struct declares
    /// the same name.
    pub min_field_name_len: usize,
    /// Knob names a line lists before `+N more`.
    pub max_knobs_listed: usize,
    /// Extra product prefixes for the env-seam rule, on top of the ones derived from every
    /// `[package]` / `[[bin]]` / package.json / pyproject name (uppercased, `-` -> `_`).
    pub env_prefixes: Vec<String>,
    /// Manifest names too generic to derive a prefix from (`app`, `core`, `cli`, `server`).
    pub env_prefix_stoplist: Vec<String>,
    /// An env name containing one of these (case-sensitive) is a candidate even without a
    /// product prefix; a name with neither is foreign and never reported.
    pub env_keywords: Vec<String>,
    /// Whole-word mentions in Test files or `#[cfg(test)]` regions a candidate needs to be a
    /// test seam.
    pub min_test_mentions: usize,
    /// CI, container and build files: an env name any of them mentions is set outside tests
    /// and never a seam.
    pub setter_file_globs: Vec<String>,
    /// Check compile-time reads too (`env!`, `option_env!`, `import.meta.env`); off: they are
    /// counted in a note.
    pub include_compile_time_env: bool,
    /// Score weight of the percentile of `unread_knobs + test_seams` (0: the section and
    /// reasons print, the ranking ignores them).
    pub weight: f64,
}

impl Default for Declared {
    fn default() -> Self {
        Self {
            languages: strings(&["rust"]),
            manifest_globs: strings(&["**/Cargo.toml", "**/package.json", "**/pyproject.toml"]),
            check_dev_dependencies: false,
            check_placement: false,
            side_effect_deps: strings(&["*-sys", "tikv-jemallocator", "openssl", "getrandom", "@vitest/coverage-*", "tslib", "core-js", "react"]),
            doc_globs: strings(&["*.md", "docs/**"]),
            min_age_commits: 5,
            skip_lint_silenced_crates: false,
            max_git_lookups: 50,
            report_noop_features: false,
            report_bare_dead_features: false,
            feature_ci_globs: strings(&[".github/workflows/*", "Makefile", "justfile", "**/*.sh"]),
            config_file_regex: r"(^|/)(config|settings|cfg|options)[^/]*\.(rs|py|ts|tsx|js)$".into(),
            config_struct_regex: "(Cfg|Config|Settings|Options)$".into(),
            config_formats: strings(&["toml"]),
            serialize_fn_regex: "^(render|to_toml|to_yaml|to_json|to_string|serialize|dump|write_to|save)$".into(),
            config_is_public_api: false,
            require_bin_target: true,
            require_sibling_read_or_doc: true,
            public_doc_globs: strings(&["README*", "docs/**", "doc/**", "*.1", "man/**", "GUIDE*", "FAQ*"]),
            min_field_name_len: 3,
            max_knobs_listed: 8,
            env_prefixes: Vec::new(),
            env_prefix_stoplist: strings(&["app", "core", "cli", "server"]),
            env_keywords: strings(&["TEST", "FIXTURE", "SEED", "MOCK", "FAKE", "STUB", "REPLAY"]),
            min_test_mentions: 1,
            setter_file_globs: strings(&[".github/**", "Dockerfile*", "Makefile", "justfile", "docker-compose*", ".env*"]),
            include_compile_time_env: false,
            weight: 0.0,
        }
    }
}

// ---------- comments ----------

/// Comments as structure (see `comments`): top-level banners that partition a file into
/// labelled sections, and phase labels inside a function over the cognitive threshold.
/// Annotations on findings the report already makes; never a score input.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Comments {
    pub banners: Banners,
    pub phases: Phases,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Banners {
    /// Partition files by their top-level banner comments (off: no `sections`, no reason).
    pub enabled: bool,
    /// Rule characters (from `rule_chars`, counted as characters, never bytes: `──` is two)
    /// a rule line needs; a shorter `──` / `--` / `==` followed by text is a banner too.
    pub min_rule_len: usize,
    /// The characters a rule is drawn with.
    pub rule_chars: String,
    /// Banners this many lines apart or closer merge into one boundary, so a rule / title /
    /// rule triple is one; the title is the rule's inline text, else the comment line between
    /// the pair.
    pub pair_gap: usize,
    /// A file is reported at this many labelled sections or more…
    pub min_sections: usize,
    /// …when its largest section has at least this many lines…
    pub min_section_lines: usize,
    /// …and the file has at least this many lines…
    pub min_file_lines: usize,
    /// …and its size percentile among Source files (0-100, on lines minus inline test lines)
    /// is at least this, or it is in the printed hotspot list.
    pub min_size_percentile: f64,
    /// A boundary on lines 1-3 whose next non-comment item is an import is a license or file
    /// header: skipped, together with the closing rule of its pair.
    pub skip_top_of_file: bool,
    /// Comment text starting with one of these (editor folding markers) is never a banner.
    pub skip_markers: Vec<String>,
    /// A boundary with no title (no inline text, no comment line inside the pair) is dropped.
    pub require_title: bool,
}

impl Default for Banners {
    fn default() -> Self {
        Self {
            enabled: true,
            min_rule_len: 6,
            rule_chars: "-=─═━*#_".into(),
            pair_gap: 2,
            min_sections: 3,
            min_section_lines: 100,
            min_file_lines: 400,
            min_size_percentile: 80.0,
            skip_top_of_file: true,
            skip_markers: strings(&["#region", "#endregion", "%%"]),
            require_title: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Phases {
    /// A unit is reported at this many labelled phases or more…
    pub min_phases: usize,
    /// …when every phase has at least this many non-blank lines…
    pub min_phase_lines: usize,
    /// …and the unit spans at least this many lines.
    pub min_span_lines: usize,
    /// Phase comments may sit in the body block or in a block this many levels below it (a
    /// match arm, an `if` block); 0 means the body only.
    pub max_depth: usize,
    /// Case-insensitive patterns a phase comment's text (marker stripped) must match. The
    /// defaults are box-drawing rules, `step|pass|phase|stage N` and `N.` / `N)` numbering;
    /// `--` / `==` rules and ordinal words (`first`, `finally`) are prose too often.
    pub patterns: Vec<String>,
    /// Skip units inside a Rust `#[cfg(test)]` region.
    pub skip_test_modules: bool,
    /// Units at or above this cognitive are checked; 0 (the default) means the units over
    /// `[metrics].cognitive_hard` (the report's own test, so a unit exactly at the
    /// threshold gets no phases: it has no "over cognitive" reason to ride on).
    pub cross_with_cognitive_min: u32,
    /// Estimate each phase's cognitive as a helper: the walker re-run over its statements
    /// with nesting re-based to 0.
    pub estimate_cognitive: bool,
    /// Shared locals the reason names before `+N more`.
    pub max_shared_locals_named: usize,
}

impl Default for Phases {
    fn default() -> Self {
        Self {
            min_phases: 2,
            min_phase_lines: 8,
            min_span_lines: 40,
            max_depth: 1,
            patterns: strings(&[r"^\s*[─═━]{2,}\s*\S", r"^\s*(step|pass|phase|stage)\s*\d", r"^\s*\d+[.):]\s"]),
            skip_test_modules: true,
            cross_with_cognitive_min: 0,
            estimate_cognitive: true,
            max_shared_locals_named: 4,
        }
    }
}

// ---------- naming ----------

/// The short-name live range (see `naming`): one-letter bindings and the widest gap between
/// their consecutive uses. Every knob of that measure lives here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Naming {
    /// A binding whose name has this many characters or fewer is a short name…
    pub short_name_max_len: usize,
    /// …unless it is one of these (index and coordinate letters); a name starting with `_`
    /// never counts either.
    pub short_name_allow: Vec<String>,
    /// A short binding is far-lived when the largest line gap between two consecutive uses
    /// (the declaration counting as the first use) is at least this.
    pub short_name_min_gap: usize,
    /// A unit reports its far-lived short bindings only when it binds at least this many other
    /// names besides the one reported.
    pub short_name_min_other_bindings: usize,
    /// Count the names a destructuring pattern binds (`let (a, b) = …`, `for (k, v) in …`,
    /// `const {p} = …`) as bindings too; off, only a bare identifier is a binding.
    pub short_name_include_pattern_bindings: bool,
    /// A unit at least this many lines long carries the attribute even when it is under the
    /// cognitive threshold and outside the file's `worst_functions`.
    pub min_unit_lines: usize,
    /// Emit `short_binding_share` (short bindings / all bindings) per file in `--json`.
    pub emit_short_binding_share: bool,
    /// Weight of the `short_binding_share` percentile in the composite score: the only route
    /// into the ranking, 0 by default so the attribute prints without moving anything.
    pub weight_into_complexity: f64,
}

impl Default for Naming {
    fn default() -> Self {
        Self {
            short_name_max_len: 1,
            short_name_allow: strings(&["i", "j", "k", "n", "x", "y", "z", "_"]),
            short_name_min_gap: 30,
            short_name_min_other_bindings: 2,
            short_name_include_pattern_bindings: false,
            min_unit_lines: 100,
            emit_short_binding_share: true,
            weight_into_complexity: 0.0,
        }
    }
}

// ---------- fallback ----------

/// Parse-default fallbacks (see `fallback`): a literal default applied to the result of a
/// fallible transform, counted per unit on the metrics walk and reported as a reason from
/// `min_sites` up. Never a score input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Fallback {
    /// Rust methods that apply a default (`x.unwrap_or(0)`, `x.or_default()`); every call is a
    /// fallback site, a parse default when the chain and the default qualify.
    pub methods_rust: Vec<String>,
    /// Rust call names that are fallible transforms (`.parse()`, `.next()`, `from_utf8(…)`): a
    /// site whose receiver chain holds one is a parse default. Arguments are not the chain.
    /// `get` is left out on purpose: `m.get(k).copied().unwrap_or(0)` defaults an absent key,
    /// the benign case, and a `get` on the output of a transform (`s.split(':').nth(1)`) is
    /// caught by the transform below it in the chain.
    pub parse_calls: Vec<String>,
    /// Rust constructor paths that count as a literal default, called (`String::new()`) or
    /// passed bare to `unwrap_or_else`; literal tokens, `()`, `[]` and `None` always do.
    pub empty_constructors: Vec<String>,
    /// Python methods (`d.get(k, lit)`, `d.pop(k, lit)`) and builtins (`getattr(o, k, lit)`)
    /// whose literal last argument is a default.
    pub python_default_getters: Vec<String>,
    /// Python call names that are fallible transforms.
    pub parse_calls_python: Vec<String>,
    /// Python units (bare name) in which `x or []` is never a site: a constructor defaulting an
    /// optional argument.
    pub python_skip_or_in: Vec<String>,
    /// TS binary operators whose literal right operand is a default.
    pub ts_operators: Vec<String>,
    /// Count `a || lit` too.
    pub ts_count_or: bool,
    /// Count `a?.b` / `a?.()` / `a?.[i]` as fallback sites (never parse defaults). Off: it
    /// multiplied hono's count 2.7x with nothing real behind it.
    pub count_optional_chain: bool,
    /// TS call names that are fallible transforms (`parse` covers `JSON.parse`).
    pub parse_calls_ts: Vec<String>,
    /// Parse-default sites one unit needs for the reason.
    pub min_sites: usize,
    /// Regexes; a unit whose bare name (owner stripped) matches one never gets the reason: a
    /// defaults / config assembler swallows on purpose.
    pub exempt_fn_patterns: Vec<String>,
    /// Reason lines per file, most sites first; the rest are counted.
    pub max_reported_per_file: usize,
}

impl Default for Fallback {
    fn default() -> Self {
        Self {
            methods_rust: strings(&["unwrap_or", "unwrap_or_default", "unwrap_or_else", "or_default"]),
            parse_calls: strings(&["parse", "split", "next", "strip_prefix", "from_utf8", "splitn", "split_once", "trim_start_matches"]),
            empty_constructors: strings(&["String::new", "Vec::new", "Default::default"]),
            python_default_getters: strings(&["get", "getattr", "pop", "setdefault"]),
            parse_calls_python: strings(&["split", "rsplit", "partition", "rpartition", "splitlines", "readline", "int", "float"]),
            python_skip_or_in: strings(&["__init__", "__post_init__"]),
            ts_operators: strings(&["??"]),
            ts_count_or: false,
            count_optional_chain: false,
            parse_calls_ts: strings(&["split", "match", "exec", "parseInt", "parseFloat", "parse", "shift", "pop"]),
            min_sites: 2,
            exempt_fn_patterns: strings(&["^default", "^from_env", "^with_", "^parse_args", "^from_matches", "^configure", "^__init__$"]),
            max_reported_per_file: 3,
        }
    }
}

// ---------- report ----------

/// Partial `[report.with_history]` tables are filled from the defaults by the
/// table merge in `Config::load`, so every field is required here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Weights {
    /// sqrt(churn × complexity). Zero without history.
    pub hotspot: f64,
    pub fixes: f64,
    pub complexity: f64,
    pub coupling: f64,
    pub clones: f64,
    pub size: f64,
    /// Percentile of `dead_ratio`: lines of exported symbols with no production reference
    /// outside their file, over source lines (see `dead`).
    pub dead: f64,
    /// Percentile of `family_literals`: the file's literals in exact message or config
    /// families (see `strings`). 0 by default: the section and reasons print, the ranking
    /// ignores them.
    pub strings: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Report {
    /// Weights when git history joined at least one file.
    pub with_history: Weights,
    /// Weights when it did not (`--no-history`, not a repo, subdirectory scan).
    pub without_history: Weights,
    /// Score multiplier for a file with `test_units == 0`: no test unit names one of its
    /// symbols, no test file imports it, no same-stem test sits in its tree.
    pub no_tests_multiplier: f64,
    /// complexity = this × pct(max cognitive) + (1 − this) × pct(total cognitive).
    pub complexity_max_share: f64,
    /// Coupling floor for a file inside an import cycle.
    pub cycle_coupling: f64,
    /// Reason thresholds.
    pub reason_churn_percentile: f64,
    pub reason_min_fix_commits: usize,
    pub reason_fanin_percentile: f64,
    pub reason_min_fan_in: usize,
    pub reason_clone_ratio: f64,
    pub reason_hidden_partners: usize,
    pub reason_bus_factor_min_commits: usize,
    /// Symbols the `named by no test` reason lists before `+N more`; `unmentioned[]` in `--json`
    /// is never cut.
    pub reason_unmentioned_listed: usize,
}

impl Default for Report {
    fn default() -> Self {
        Self {
            with_history: Weights { hotspot: 0.45, fixes: 0.15, complexity: 0.15, coupling: 0.10, clones: 0.10, size: 0.05, dead: 0.05, strings: 0.0 },
            without_history: Weights { hotspot: 0.0, fixes: 0.0, complexity: 0.55, coupling: 0.15, clones: 0.15, size: 0.15, dead: 0.05, strings: 0.0 },
            no_tests_multiplier: 1.15,
            complexity_max_share: 0.6,
            cycle_coupling: 0.6,
            reason_churn_percentile: 0.8,
            reason_min_fix_commits: 2,
            reason_fanin_percentile: 0.9,
            reason_min_fan_in: 5,
            reason_clone_ratio: 0.15,
            reason_hidden_partners: 2,
            reason_bus_factor_min_commits: 5,
            reason_unmentioned_listed: 5,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_toml() {
        let d = Config::default();
        let back: Config = toml::from_str(&d.to_toml()).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn partial_file_overrides_only_what_it_names() {
        let dir = std::env::temp_dir().join(format!("scry-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE_NAME), "[clones]\nmin_tokens = 50\n\n[report.with_history]\nhotspot = 0.6\n\n[discover]\ntest_dirs = [\"qa\"]\n\n[clumps]\nmin_typed_slots = 1\nprotocol_tuples = [[\"a\", \"b\"]]\n").unwrap();
        let c = Config::load(&dir, None).unwrap();
        assert_eq!(c.clones.min_tokens, 50);
        assert_eq!(c.clones.k, 30);
        assert_eq!((c.clumps.min_typed_slots, c.clumps.max_group), (1, 4));
        assert_eq!(c.clumps.protocol_tuples, vec![vec!["a".to_string(), "b".to_string()]]);
        assert_eq!(c.report.with_history.hotspot, 0.6);
        assert_eq!(c.report.with_history.fixes, 0.15);
        assert_eq!(c.discover.test_dirs, vec!["qa"]);
        assert_eq!(c.discover.vendor_dirs, Discover::default().vendor_dirs);
        // An explicit file layers on top of the root file.
        let extra = dir.join("more.toml");
        std::fs::write(&extra, "[clones]\nk = 40\n").unwrap();
        let c2 = Config::load(&dir, Some(&extra)).unwrap();
        assert_eq!((c2.clones.k, c2.clones.min_tokens), (40, 50));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unknown_keys_are_errors() {
        let dir = std::env::temp_dir().join(format!("scry-cfg-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE_NAME), "[clones]\nmin_token = 50\n").unwrap();
        let err = Config::load(&dir, None).unwrap_err();
        assert!(format!("{err:#}").contains("min_token"), "{err:#}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
