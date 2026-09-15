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
}

impl Default for Metrics {
    fn default() -> Self {
        Self { cognitive_hard: 15 }
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
    /// Drop a family when one defining file imports the family's name (or everything) from
    /// another: a re-export wrapper, not a copy.
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
    /// …with at least this many distinct normalized tokens in its body, one keyword or operator
    /// among them and one kept name or literal.
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
        std::fs::write(dir.join(FILE_NAME), "[clones]\nmin_tokens = 50\n\n[report.with_history]\nhotspot = 0.6\n\n[discover]\ntest_dirs = [\"qa\"]\n").unwrap();
        let c = Config::load(&dir, None).unwrap();
        assert_eq!(c.clones.min_tokens, 50);
        assert_eq!(c.clones.k, 30);
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
