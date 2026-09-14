# Appendix: per-proposal verification chapters

Generated 2026-09-14 by the analyzer brainstorm workflow (see `docs/analyzer-brainstorm.md`).
Each chapter was written by an agent from the prototype and skeptic verdicts for one cluster of proposals.
Numbers are the prototype measurements on the six corpora named in the main document; they were spot-checked, not re-derived.
Proposal ids (P01..P75) match the main document. Tier A/B chapters carry implementation Rules; tier C entries are two lines.

## Dead surface and reachability (shared name->file reference index)

One new pass, `dead`, builds a single index once (name -> file -> {prod_refs, test_refs, doc_mentions}) from the trees already parsed, and every proposal in this cluster is a category or annotation on that index. Ship order: P01 (index + DEAD/TESTONLY/OVEREXPORTED), then P03 (fields/variants on the same walk), then P05 (lazy history annotation). P02 is a category of P01, not a pass. P04 and P06 are rejected; the only thing worth salvaging from P04 (manifest entrypoints) is a helper P01 needs anyway.

### P01 unreachable and over-exported symbols (tier A)

**Measures.** Unit = symbol: `pub`/exported/top-level function, method, struct, enum, trait, type alias, interface, class, const, static. Per symbol: `external_prod_refs`, `external_test_refs`, `own_prod_refs`, `own_test_refs`, `doc_mentions` (comment/docstring nodes; counted, never a use), `samename` (number of definitions sharing the name). Categories: DEAD = all four ref counts 0; TESTONLY = `external_prod_refs 0 and own_prod_refs 0 and (external_test_refs + own_test_refs) > 0` (P02, strict); OVEREXPORTED = `external_prod_refs 0 and own_prod_refs > 0`. Per file: `pub_items`, `dead_count`, `test_only_count`, `own_file_only_share = OVEREXPORTED / pub_items`, `dead_lines`, `dead_ratio = dead_lines / source lines` (percentile-ranked). Repo: totals and share of exported symbols that are unreachable.

**Why it matters for LLM code.** LLMs write `pub fn` / `export` by default and never narrow visibility, so rustc `dead_code` (which exempts anything public behind an all-`pub mod` lib.rs) goes silent; kanspec has 420 `pub fn` versus 4 `pub(crate) fn` and lib.rs:29-58 declares all 30 modules `pub mod`. Prototype (reach.py, all pub item kinds, application mode): share of pub/exported items with zero external production references — kanspec 177/754 = 23.5% (154 OVEREXPORTED: 86 struct, 42 fn, 11 enum, 11 const, 3 type, 1 trait; 20 TESTONLY; 3 DEAD of which 2 are the inline-format false positives), scry 13/50 = 26% (12 OVEREXPORTED, 1 TESTONLY), fd 7/91 = 7.7% (all OVEREXPORTED), ripgrep 77/872 = 8.8% but in library mode 16/872 = 1.8% and all 18 DEAD are public API of published crates. Functions only: kanspec 65/429 = 15%, scry 5/19 = 26%, fd 4/71 = 6%, ripgrep 67/711 = 9%. hono (TS, 119 export-target root files exempt): 59/680 = 8.7% (26 OVEREXPORTED, 26 TESTONLY, 7 DEAD; 3 of the 7 DEAD are JSX namespace-convention types). click (Python, 84 `__init__` api names exempt): 46/174 = 26% but Python has no visibility keyword so OVEREXPORTED there is just "no leading underscore". Hand-verified hits: kanspec/src/hooks.rs:143-147 `pub fn install` DEAD, 0 callers in 81 .rs files, 5 doc mentions (superseded by `plan_install`); kanspec/src/cmd/decision.rs:218-237 `pub fn plan_accept` OVEREXPORTED (1 prod call at :211, 2 test calls, all in-file); kanspec/src/server.rs 12 of 13 pub items referenced only inside the file (`Assets` line 56, `AppState` 59-68, `Tick` 78-81, `ApiError` 86) yet lib.rs:55 says `pub mod server`; per-file dead_ratio top: kanspec src/scan.rs 0.226 (6/31), src/ci.rs 0.219 (7/9), src/setup.rs 0.21 (9/12); fd max 0.113 (1 item). Name-collision rate: kanspec 0/430 pub fn names duplicated; ripgrep `has_written` x3, `new_no_color` x2. Two corrections to earlier drafts: (a) Rust 2021 inline format args (`format!("{BIN_ENV}")`) are `string_content` inside a `token_tree`, not identifiers — kanspec hooks.rs:53 `BIN_ENV` and :61 `BRANCH_TICKET_KEY` were false DEAD hits; (b) scry's `classify` has an own-file production caller (discover/mod.rs:98), so it is OVEREXPORTED+tested, not DEAD.

**Detection.** One extra walk over the already-parsed trees (t-02b6); hash map `name -> file -> [prod_refs, test_refs]`; no graph ops.
- Rust defs: `function_item`, `struct_item`, `enum_item`, `trait_item`, `type_item`, `const_item`, `static_item`, `function_signature_item`, `mod_item` with a `visibility_modifier` child (text `pub`; `pub(crate)`/`pub(super)` show `(crate)` as a child node and are tagged restricted). Name from `name:` field (`identifier` for fn/const/static, `type_identifier` for struct/enum/trait/type). Skip the def's own name node by byte range. Skip `function_item` under `impl_item` with a `trait:` field. Skip items whose preceding `attribute_item` siblings match `skip_attrs` (`#[test]` = `attribute > identifier`; `#[tokio::test]` = `attribute > scoped_identifier`). Test context = files scry classes Test, `**/testutil*.rs`, `**/test_utils*`, tests/, benches/, examples/, fuzz/, `mod_item` preceded by `attribute_item` whose arguments `token_tree` contains `cfg(test)`, and `#[test]` functions.
- Rust refs: `identifier`, `type_identifier`, `field_identifier`, `shorthand_field_identifier`, `scoped_identifier name:`, `scoped_type_identifier name:`, plus every `identifier` inside a `token_tree` (macro bodies are unparsed). Doc mentions: `line_comment` (with `doc:` field) / `block_comment`, counted separately.
- Rust api_roots: in lib.rs, `use_declaration` with `visibility_modifier` (`scoped_identifier`, `scoped_use_list > use_list > use_as_clause|identifier`, `use_wildcard`). mode=auto: Cargo.toml with `[lib]` and no `[[bin]]` -> library (plain `pub` exempt; only `pub(crate)`/`pub(super)` checked); `[[bin]]` present -> application (kanspec: `[lib]` + two `[[bin]]` -> application; ripgrep crates `[lib]` only -> library).
- TS/TSX defs: `export_statement declaration:(function_declaration|class_declaration|abstract_class_declaration|interface_declaration|type_alias_declaration|enum_declaration name:)`, `declaration:(lexical_declaration (variable_declarator name:(identifier)))`, `export_clause > export_specifier name: alias:` (a re-export is a reference to the origin, not new surface), `export default`, `export_statement source:` files treated as barrels. Refs: `identifier`, `type_identifier`, `property_identifier`, `shorthand_property_identifier(_pattern)`, excluding nodes under `import_statement`/`import_clause`/`named_imports` (an unused import is not a use). api_roots: `**/index.ts`, package.json `exports`/`main`/`module`/`types`/`bin` resolved by TARGET with dist->src remapping (`./dist/preset/quick.js` -> `src/preset/quick.ts`), never by key; wildcard entries (`./utils/*`) expand as globs. Test context: Test files plus `call_expression function:(identifier)` in {describe, it, test}.
- Python defs: `module > function_definition|class_definition name:`, `module > decorated_definition definition:`, `module > expression_statement (assignment left:(identifier))`; api names from `__init__.py` imports and `assignment left:(identifier)` text `__all__` with `list > string > string_content`. Refs: `identifier`, `attribute attribute:(identifier)`. Test context: Test files, `function_definition name:` starting with `test_`, `decorated_definition` whose decorator text contains `pytest`.
- string_refs: a `string_literal`/`string`/`string_fragment`/`string_content` counts as a reference to `name` when any dot- or colon-separated segment equals `name` (`'app.apps.AppConfig'`, `'pkg.mod:func'`) or it matches `\{name[:}]` (inline format args).
- Aggregation alternative from the naming lens, worth keeping as a fallback: the clone pass already tokenizes every file; the pre-normalization identifier text gives the same index in O(total tokens) and is robust to `use crate::x::*` and barrels without any import resolution.

**Reason line.**
`pub fn install (hooks.rs 143-147) is called nowhere in 81 files (5 doc mentions)`
`pub fn Git::is_ignored (git.rs 833-835) has no production caller: 4 uses, all in tests/git_real.rs, tests/lock.rs; move into the test crate or delete`
`12 of 13 pub items are referenced only inside this file (92%, 99th percentile): Assets (line 56), AppState (lines 59-68), Tick (lines 78-81), ApiError (line 86) and 8 more`
`pub fn parse (cli.rs 40-88) shares its name with 7 other definitions; reachability not assessed`

**Knobs.**
```toml
[dead.symbols]
enabled = true
mode = "auto"                 # auto | library | application
api_roots = ["src/lib.rs", "**/index.ts", "**/__init__.py"]
entrypoints = ["src/main.rs", "src/bin/**", "build.rs", "benches/**", "examples/**", "fuzz/**", "**/__main__.py", "conftest.py", "build/**", "scripts/**", "*.config.ts"]
read_manifests = true         # Cargo.toml [[bin]]/[lib], package.json exports (by target), pyproject scripts
skip_attrs = ["test", "bench", "no_mangle", "wasm_bindgen", "pyfunction", "tauri::command", "proc_macro"]
skip_decorators = ["pytest.fixture", "app.route", "router.*"]
skip_names = ["IntrinsicAttributes", "ElementChildrenAttribute", "ElementType", "IntrinsicElements"]
skip_name_prefixes = ["_", "test_"]
ts_framework_exports = ["default", "metadata", "generateStaticParams", "loader", "action", "config"]
string_refs = true
min_lines = 2
kinds = ["fn", "type", "const"]
languages = ["rust"]          # ts requires mode = "application"; python reports DEAD only
overexported_min_items = 4
overexported_min_share = 0.5
max_reported_per_file = 5

[report.weights]
dead = 0.05                   # dead_ratio percentile
```

**Objection and answer.** Skeptic: almost every hit is visibility hygiene (kanspec 44 OVEREXPORTED vs 1 DEAD for fns), which rustc's `unreachable_pub` plus one lib.rs edit would report with type info; hono's `./utils/*` wildcard export makes `NotEqual` and `jsxEscape` public API; and `parse` x8, `as_str` x6, `render` x5, `new` x4, `run` x4 make the commonest names unflaggable. Answer, folded into the design: OVEREXPORTED is never a per-symbol reason and never says "narrow to private" — it is one per-file percentile line, gated on `pub_items >= 4 and share >= 0.5` (server.rs 12/13, ci.rs 7/9, setup.rs 9/12 pass; no fd/ripgrep file does). DEAD and TESTONLY are the per-symbol reasons. `unreachable_pub` is allow-by-default, needs the maintainer to already know lib.rs is the problem, and says nothing about test-only; scry's reason names that gap. Exports are resolved by target with dist->src remapping and wildcard globs, so the hono cases are exempt (verified: "src/jsx/jsx-runtime.ts:51 jsxEscape and src/utils/types.ts:10 NotEqual are now exempt because their files are package export targets"). Collisions only hide, never create, hits; symbols with `samename > 1` are skipped from DEAD/TESTONLY and the reason says so. Launch Rust-first (bin+lib crates auto-detected from Cargo.toml); TS only under explicit `mode = "application"` with the framework-export skip list, otherwise the report prints "run knip"; Python ships DEAD/TESTONLY only.

**Folds into.** New report section `Dead surface` (DEAD, TESTONLY per symbol; OVEREXPORTED per file); `dead_ratio` as a new signal with weight `dead = 0.05`; reason lines on the hotspot entry; `--json` gets `dead.symbols[]` with the five counters per symbol.

**Cost.** Cheap: one tree walk, one hash map; kanspec (81 files) sub-second in the prototype.

**Rules.**
- Build the index once per scan from every discovered Source and Test file; never from the filesystem or from unparsed files.
- A definition's own name node is excluded from its ref count by byte range.
- A reference in a `token_tree` counts as prod or test according to its enclosing context, same as any identifier.
- `doc_mentions` never satisfy reachability; they are printed in the reason.
- Test context is the union of scry's Test classification, `extra_test_globs`, `#[cfg(test)]` mods and `#[test]` fns; a ref inside test context is `test`, everything else is `prod`.
- Trait-impl methods, `skip_attrs`/`skip_decorators` items, `skip_names`, `skip_name_prefixes`, items under `min_lines`, and api_roots/entrypoint files (in library mode, or always when listed) are never defined into the index as candidates but still contribute refs.
- `mode = "auto"`: library iff Cargo.toml has `[lib]` and no `[[bin]]`, or package.json has `exports`, or pyproject has no `[project.scripts]`; library exempts plain `pub` / `export` and checks only restricted visibility.
- DEAD requires all four counts 0 and `samename == 1`; TESTONLY requires `external_prod_refs == 0 and own_prod_refs == 0` (see P02); OVEREXPORTED is aggregated per file only.
- string_refs matches whole strings, dotted/colon path segments, and `\{name[:}]` format args; off only by knob.
- Python never emits OVEREXPORTED. TS emits nothing unless `mode = "application"`; in auto/library the section prints one line pointing at knip.
- `dead_ratio` is percentile-ranked with zero -> 0 like every other signal; the weight is 0.05 and lives under `[report.weights]`.
- Reasons list at most `max_reported_per_file` symbols, DEAD before TESTONLY, longest line range first.

### P02 test-only production surface (tier B)

**Measures.** Category of P01's index, not a pass. Unit = symbol. TESTONLY = `external_prod_refs 0 and own_prod_refs 0 and (external_test_refs + own_test_refs) >= min_external_test_refs`. Per file: `test_only_count`, `test_only_lines`; per test file: the inverse listing (which Source symbols it alone keeps alive); repo: share of exported symbols that are test-only.

**Why it matters for LLM code.** Sessions make helpers `pub` so an integration test can poke them, then never wire them in. Loose definition (external_prod_refs 0, any test ref): kanspec 17/430 pub fn = 4.0% (20/754 all pub items = 2.7%), scry 1/19, fd 0/71, ripgrep application mode 12/694 = 1.7% (all public library API exercised by the crate's own tests), ripgrep library mode 2/872, hono 26/680 = 3.8%, click 10/174 = 5.7%. Strict definition (own_prod_refs 0 too): kanspec 8 (config.rs:276 `render_default`, git.rs:385 `object_exists`, :833 `is_ignored`, :837 `is_tracked`, out.rs:33 `plain`, transitions.rs:207 `allowed_from`, :227 `verbs_str`, triage.rs:1022 `scripted`), scry 0, fd 0, ripgrep-lib 1. Precision under the strict rule was 8/8 on kanspec by grep; under the loose rule about a third of kanspec hits (out.rs `pad_visible` own_prod=1 other_test=9, `visible_len` own_prod=4 other_test=11, paths.rs `common_dir` own_prod=7) are used in production in-file. kanspec/src/git.rs:833-835 `is_ignored` and :837-839 `is_tracked` are called only from tests/git_real.rs:571-575 and tests/lock.rs:182; `git log -S"fn is_ignored"` shows it was born in 988d9b5 (2026-08-31) in the same commit as its test. The skeptic's grep is right that `classify` (discover/mod.rs:98) and `states_str(states_for(*verb))` / `allowed_slice(Some(State::Review))` (error.rs:236, :540, :573) have production callers; the strict rule already excludes them.

**Detection.** Same index and test-context rules as P01. Emit TESTONLY only when every non-definition reference is in test context and the count meets the floor. Optional corroboration through P05's birth lookup: the symbol and its first test reference added in the same commit.

**Reason line.**
`pub fn Git::is_ignored (git.rs 833-835) has no production caller: 4 uses, all in tests/git_real.rs, tests/lock.rs; born with its test in 988d9b5 (2026-08-31); move into the test crate or delete`
`all 6 exported symbols in transitions.rs are test-only (lines 195-234), kept alive by tests/transition_table.rs`

**Knobs.**
```toml
[dead.test_only]
enabled = true
min_external_test_refs = 2
require_same_birth_commit = false   # needs P05's birth lookup; on = fewer, surer hits
treat_benches_as_prod = false
extra_test_globs = ["**/testutil*.rs", "**/test_utils*", "**/fixtures/**"]
max_reported_per_file = 5
```

**Objection and answer.** Skeptic: two of three flagship examples had production callers, and what survives is a facade-helper test seam (`Git::is_ignored`) that the Rust community treats as legitimate — rustc counts a cfg(test) use as a use on purpose, and `#[cfg(test)]` cannot even hide a symbol from a tests/ integration crate. Answer: the strict definition (own_prod_refs == 0) is mandatory and removes both wrong examples; `min_external_test_refs = 2` keeps a symbol touched once by one test off the list; the reason names the test files and says "move into the test crate or delete", never `#[cfg(test)]`; library mode exempts plain `pub` exactly as P01 so ripgrep's 12 in-repo-tested API fns vanish. Whether a seam is legitimate is the maintainer's call — scry's job is to say the seam exists and where.

**Folds into.** P01's `Dead surface` section (TESTONLY rows) and per-file rollup line; the inverse per-test-file listing goes to `--json` only.

**Cost.** Zero beyond P01.

**Rules.**
- TESTONLY is computed from the P01 index; no second walk.
- Strict: `own_prod_refs == 0` and `external_prod_refs == 0`; any prod ref, even in the defining file, demotes to OVEREXPORTED.
- Floor `min_external_test_refs` counts external + own test refs together.
- Library mode exempts plain `pub`/`export`; restricted-visibility symbols are still checked.
- Files matching `extra_test_globs` are test context regardless of scry's classification until t-7634 lands.
- When every exported symbol in a file is TESTONLY, print one rollup line instead of per-symbol lines (this replaces P04's TEST_ONLY_MODULE).
- The reason never suggests `#[cfg(test)]` when any referencing test lives under `tests/`.
- With `require_same_birth_commit = true`, drop symbols whose birth commit (P05) contains no test reference.

### P03 write-only fields and never-constructed variants (tier A)

**Measures.** Rust only. Unit = struct field or enum variant, reported as a sub-reason on the defining file. Field: `writes` (struct-expression initialisers, assignments), `reads` (field access not on the left of an assignment, field patterns, token_tree mentions); report NEVER_READ when `reads == 0`. Variant: `constructions` (expression-context paths, struct-expression paths, `Self::V`, wildcard-imported bare names, token_tree mentions), `matches` (pattern-context paths); report NEVER_CONSTRUCTED when `constructions == 0`, sub-label "handled but never produced" when `matches > 0`. Per file: `dead_shape_count`; no score weight.

**Why it matters for LLM code.** LLMs model states and fields speculatively and the compiler is silent because the items are pub. kanspec: 3 never-constructed variants of 324 (0.9%) + 1 never-read field of 971 (0.1%) = 4 hits in 34k lines; scry 0/18 variants, 0/182 fields; fd 0/46, 0/140; ripgrep 1 hit (crates/index/src/index.rs:14 field `Index.db`, reads 0 writes 2) of 264 variants / 687 fields, after the `use FastMatchResult::*` rule rescued 2 false variant hits (core.rs:16-17, 5 and 1 bare uses). Hits: kanspec/src/model.rs:81 `pub mtime: SystemTime` written at 9 sites (store.rs:870 `ticket_from`, derive.rs:1339, triage.rs:1152, ...) and never read anywhere in src/ or tests/; kanspec/src/out.rs:387 `Color::Blue` only in the match arm at :402; kanspec/src/error.rs:153 `GateCode::Undispositioned` and :223 `GateDetail::Undispositioned`, matched once each, never raised, only a module-doc mention at error.rs:4 (serde Serialize derive present, but that does not construct). 5/5 real on kanspec+ripgrep after the wildcard rule. Separation is weak (4 per 34k LLM lines vs 1 per ~100k human lines), so it is a reason line, not a ranking signal.

**Detection.** Same walk as P01.
- Fields: `struct_item name:(type_identifier) body:(field_declaration_list (field_declaration [visibility_modifier] name:(field_identifier) type:))`. Reads: `field_expression field:(field_identifier)` unless it is the `left:` of `assignment_expression`/`compound_assignment_expr`; `field_pattern name:(shorthand_field_identifier|field_identifier)` under `struct_pattern`; `\.name\b` inside any `token_tree`. Writes: `struct_expression body:(field_initializer_list (field_initializer field:) | shorthand_field_initializer)`.
- Variants: `enum_item body:(enum_variant_list (enum_variant name:(identifier)))`. Constructions: `scoped_identifier path: name:` with no ancestor in {`match_pattern`, `tuple_struct_pattern`, `struct_pattern`, `or_pattern`, `field_pattern`, `let_declaration pattern:`}; `scoped_type_identifier` whose parent is `struct_expression`; `scoped_identifier path:(identifier "Self")` inside any `impl_item` whose `type:` names the enum (`Self::Named{..}` in a pattern appears as `scoped_type_identifier path:(identifier "Self")`); bare `identifier` in expression context when a `use_declaration argument:(use_wildcard ...)` ending in the enum name is in scope at file level or in any enclosing block (ripgrep walk.rs:139-220 `use self::DirEntryInner::*` is block-scoped); identifiers inside `token_tree`.
- Skips: derives are `attribute_item (attribute (identifier "derive") arguments:(token_tree (identifier)...))` — a type is a candidate only if every derive is in `allow_derives`; `#[non_exhaustive]` (attribute > identifier) skips "never mentioned" but not "matched but never produced"; `#[repr]` skips; an `impl_item trait:` of `FromStr`/`TryFrom`/`From`/`Default` for the enum exempts its variants; `_`-prefixed fields skipped; the enclosing type must have `external_prod_refs > 0` in P01 (a dead type's fields are noise on top of the type).
- TS enums / class fields and Python Enum / `self.x` halves: node shapes confirmed (`enum_declaration name: body:(enum_body name:(property_identifier))`, `public_field_definition`, `class_definition superclasses:(argument_list (identifier "Enum"))`) but unprototyped and framework-bound; not shipped.

**Reason line.**
`field Ticket.mtime (model.rs 81) is written at 9 sites (store.rs 870, derive.rs 1339, ...) and never read`
`variant Color::Blue (out.rs 387) is matched at out.rs 402 but never constructed: a state nothing produces`

**Knobs.**
```toml
[dead.shapes]
enabled = true
languages = ["rust"]
allow_derives = ["Debug", "Clone", "Copy", "PartialEq", "Eq", "Hash", "Default", "PartialOrd", "Ord"]
skip_attrs = ["repr"]
non_exhaustive_hides_handled = false
exempt_impl_traits = ["FromStr", "TryFrom", "From", "Default", "Deref"]
string_refs = true
report_handled_never_produced = true
require_type_reachable = true
max_reported_per_file = 5
```

**Objection and answer.** Skeptic: this re-implements rustc `dead_code` with less precision (no derive-generated reads, no Deref, no trait objects), yield is 3 variants + 2 fields in 34k lines, `Check.about` and `Ticket.mtime` sit on Debug-deriving structs so `{:?}` output may be the read, and the non-Rust halves need framework catalogs. Answer: rustc is silent on every kanspec hit because the items are pub; scry fills exactly that gap and nothing more. The design takes the narrowing: Rust only; candidate types must derive nothing outside `allow_derives` and carry no `#[repr]`; `Self::V` in any impl for the enum, block-scoped `use Enum::*`, and `FromStr`/`TryFrom`/`From`/`Default` impls count as constructions; `_`-prefixed fields skipped; only types P01 finds referenced outside their file. Debug reads are accepted as a known miss — a field whose only reader is `{:?}` is still a field no code path consults, and the reason says "never read", not "delete". TS and Python are dropped until a framework catalog exists.

**Folds into.** Sub-reason in P01's `Dead surface` section on the defining file; `--json` `dead.shapes[]`. No weight.

**Cost.** Zero beyond P01's walk plus one pattern-ancestor check per scoped path.

**Rules.**
- Rust only; runs inside the P01 walk, never as a separate pass.
- A struct/enum is a candidate only if its derive token_tree contains nothing outside `allow_derives`, it has no `skip_attrs`, and (when `require_type_reachable`) P01 recorded `external_prod_refs > 0` for the type.
- A `field_expression` on the `left:` of an assignment is a write, not a read.
- Any identifier or `\.name\b` inside a `token_tree` counts as a read/construction.
- Pattern context = nearest ancestor in {match_pattern, tuple_struct_pattern, struct_pattern, or_pattern, field_pattern, let_declaration pattern}; a scoped path in pattern context is a match, otherwise a construction.
- `Self::V` counts as a construction when inside an `impl_item` whose `type:` is the enum; wildcard `use` at any scope makes bare names in that scope constructions.
- An `impl_item trait:` in `exempt_impl_traits` for the type exempts all its variants.
- `#[non_exhaustive]` suppresses "never mentioned" only; "matched but never produced" still prints unless `non_exhaustive_hides_handled = true`.
- `_`-prefixed fields are never reported.
- Same-named fields/variants on other types merge ref sets (can only hide a hit).

### P04 orphan files and test-only modules (tier C)
Rejected: 0 real hits in six corpora once manifests are read (every candidate was a fuzz target, build script or documented public API), scry's deps pass already emits Rust `mod` edges (scry/src/deps/mod.rs:269), and TEST_ONLY_MODULE is P02's per-file rollup; keep only the manifest-entrypoint helper (Cargo `[[bin]]`/`[lib]`, package.json exports by target with dist->src remapping, pyproject scripts) as shared infrastructure for P01's `api_roots`/`entrypoints` and the deps fan_in.

### P05 orphaned by commit (tier B)

**Measures.** Annotation on P01's DEAD/TESTONLY rows, run lazily. Unit = dead symbol. Output: birth commit (oldest pickaxe commit whose diff adds the definition), orphaning commit (newest commit with removed call sites > 0 and added call sites == 0; hash, date, subject, commits-ago), and a replacement candidate only when a `+` line adjacent to a removed call line in the same hunk calls a different symbol. Per repo: count orphaned inside the window, count born without a caller, median commits-ago.

**Why it matters for LLM code.** LLM refactors add a new entry point beside the old one and leave the old one compiling. kanspec: 23 DEAD/TESTONLY rows, 2 skipped (samename > 1: `LsRow`, `detect`), 21 analysed: 2 found an orphaning commit (`install` -> 2530096 2026-09-04 'fix Kanspec migration blockers', 0 commits ago, 2 call lines removed, replacement candidate `plan_install`; `render_default` -> 308a748 2026-09-03 'start/init: refuse a main that has never seen the store'), 19 never had a production caller since birth (born in 988d9b5/6a674af/c94cfb9 alongside their tests). scry: 1 symbol (`classify`), born without an external caller in 046280d. Cost on kanspec: 66 `git show` calls for 21 symbols, a few seconds. Human baselines are single-commit snapshots here (every symbol reports birth=3fce3b5), so no baseline number exists. Because 19 of 21 are "born test-only", the pass is mostly a birth-date annotation; the orphaning case is the rare, high-value outcome.

**Detection.** Input = P01's DEAD/TESTONLY list filtered to `samename == 1`, capped at `max_symbols`. Per symbol: `git log --no-merges --format=%H%x00%ad%x00%s --date=short -S<name> -- <source dirs>` over the `[history]` window; for the newest `per_symbol_commits` candidates, `git show --format= --unified=0 -M <hash> -- <dirs>` and count `-`/`+` lines matching `\b<name>\s*\(|::<name>\b|\.<name>\b` outside the `fn <name>` definition line and outside test context. Orphaning commit = newest with `removed > 0 and added == 0 and removed >= min_removed_calls`. Replacement = the identifier called on a `+` line in the same hunk adjacent to a `-` call line; the stem-affix guess is dropped. Birth = oldest pickaxe commit whose diff adds `fn <name>` (or `def`/`function`/`const`); when P02's `require_same_birth_commit` is on, also check that diff for a test-context reference.

**Reason line.**
`hooks::install (hooks.rs 143-147) lost its last production caller in 2530096 "fix Kanspec migration blockers" (0 commits ago), which switched those call sites to plan_install`
`Git::is_ignored (git.rs 833-835) has had no production caller since it was added in 988d9b5 (2026-08-31, 14 days ago)`

**Knobs.**
```toml
[dead.orphaned_by]
enabled = false               # on via `scry scan --explain-dead` or inside `scry context <paths>`
max_symbols = 50
per_symbol_commits = 5
min_removed_calls = 2
window = "inherit"            # from [history]
```

**Objection and answer.** Skeptic: it is an explanation decorator that inherits P01's precision, costs a pickaxe per symbol, is what an agent runs by hand for the few symbols it will act on, names the wrong sibling in big commits, and is wrong under file renames until t-e6fa. Answer, all taken: it never runs at scan time — only under `--explain-dead` or `scry context` (t-b342); only for unique-name DEAD/TESTONLY symbols; the orphaning commit needs `>= 2` removed call sites; the replacement is named only when the `+` and `-` lines are adjacent in one hunk (exactly the hooks::install shape); `git show -M` so moved callers are not removals; birth is reported as "first seen at this path" until t-e6fa. What remains is the one thing an agent cannot grep for cheaply: which commit, and what replaced it.

**Folds into.** Suffix on P01/P02 reason lines when enabled; `--json` `dead.symbols[].history {birth, orphaned_by, replacement}`.

**Cost.** Moderate, bounded: `max_symbols x (1 log + per_symbol_commits show)`; kanspec 21 symbols -> a few seconds. Zero by default.

**Rules.**
- Off by default; enabled by `--explain-dead` or when the P01 row is inside a `scry context` path set.
- Input rows only from P01 DEAD/TESTONLY with `samename == 1`; cap `max_symbols` by longest line range first.
- Pickaxe with the language's definition keyword for birth; bare name for candidates.
- Call-site match = `\bname\s*\(|::name\b|\.name\b`, excluding the definition line and test-context files.
- Orphaning commit requires `removed >= min_removed_calls and added == 0` after `git show -M`.
- Replacement named only from a `+` line adjacent to a `-` call line in the same hunk; otherwise omitted.
- If no orphaning commit and birth diff has no production call, print the "born without a caller" variant.
- Until t-e6fa lands, birth is "first seen at this path".

### P06 dangling identifier references in comments (tier C)
Rejected: no separation (kanspec 6.6% unresolved vs ripgrep 4.3%, click 5.1%), precision about 4 real of 29 on kanspec with the specified confirmation catching 2, and `git log -S` on a bare name is vacuous because the comment line itself matches; the one useful check (a doc comment naming a retired symbol, confirmed with definition-keyword `-G` pickaxes) belongs as a reason line inside t-eccd's lexical smells, reusing P01's identifier set.

## cross-file consistency — re-implemented helpers, literals and conventions
Two proposals clear the bar (P07, P10), one is worth a narrowed follow-up (P08), three are rejected (P09, P11, P12). Shared plumbing: P07 and P08 need a second, *helper-specific* normalizer distinct from the clones one (bindings anonymized, callees/fields/macro names/format strings kept, keywords recovered inside Rust `token_tree`); P07 and P10 both need a `cfg(test)`-module and Test-file exclusion done by ancestor walk. Build the extractor of "top-level definition units" once and let P07, P08 (and the rejected P09 annotation idea) share it.

### P07 same-name helper defined in several files (tier A)

**Measures**
Unit = name family: every top-level free function / const / static (not a trait impl) in Source files, keyed by name, with >= 2 defining files. Per family: pairwise 5-gram Jaccard on normalized body tokens, signature equality (parameter count + return-type text), and the introducing commit and session trailer of each copy. Per file: count of its definitions that belong to a reportable family. Classes: **verbatim** (Jaccard 1.0), **similar** (>= `min_body_jaccard`), **different-contract** (same name, signature differs or Jaccard < threshold — info only), **sibling-type fan** (annotation on a verbatim/similar family whose bodies are a single match/switch over string arms with differing first-param types).

**Why it matters for LLM code**
Each session writes the utility it needs without grepping for it. Evidence from kanspec/src: `fn plural(n: usize) -> &'static str` byte-identical at cmd/flow.rs:430-436 and cmd/status.rs:469-475 plus `pub(crate) fn plural(n: usize, noun: &str) -> String` at cmd/proposal.rs:168-170; git blame shows the copies came from three commits (c59889e, 5733dab, 9f2f0f7) across two Claude-Session trailers. `fn repo_relative(ctx, p)` returns `Option<String>` at scan.rs:946 but `String` with a fallback at cmd/quirk.rs:300. `fn io_err` identical 3-line fn at cmd/init.rs:373-375 and store.rs:254-256. `fn render` identical at cache.rs:210-214 and setup.rs:434-438. `fn status_word` over three enums at cmd/decision.rs:409, cmd/proposal.rs:324, cmd/quirk.rs:174 (Jaccard 1.0). Prototype (p07.py, p07_v2.out) — names defined in >= 2 files / top-level defs: scry 2/60 (both sibling-suppressed, 0 reportable); kanspec 20/580 (3.4%), 16 reportable = verbatim 3 + similar 2 + same-sig-low-jaccard 6 + divergent 5; fd 0/66; ripgrep 11/201 (5.5%), verbatim 0 similar 0; click 4/162, verbatim 2 (single-letter TypeVars V, R — killed by min_name_len); hono 26/533 (4.9%), 15 sibling-suppressed, verbatim 2 + similar 1 (`unauthorizedResponse` middleware/jwk/jwk.ts:176-196 = jwt/jwt.ts:165-185, human copy-paste). Verbatim+similar rate: kanspec 0.9% of defs, hono 0.6%, click 0, ripgrep 0, fd 0. Separation is weak, but every verbatim/similar hit inspected was a real hoisting opportunity; session attribution is the LLM-specific twist and works on kanspec's trailers (git log -L430,436:src/cmd/flow.rs returns c59889e in 6 ms; the status.rs copy came from 5733dab, a different session).

**Detection**
- Rust: `function_item`, `const_item`, `static_item` whose ancestor chain has no `impl_item` with a `trait:` field and no `trait_item`. Inherent-impl methods keyed `Type::name` (knob `include_inherent_methods`, default false — including them produces ripgrep `read`/`write` twins and kanspec `join`/`slug` method noise). Skip a definition, or a whole `mod_item`, whose preceding `attribute_item` siblings (`prev_named_sibling`) contain `cfg(` with any of `cfg_gate_attrs`; `#[cfg(test)]` mods excluded the same way.
- Python: `module` children `function_definition` and `decorated_definition` (skip when any `decorator` text contains `overload`; skip when the name is `__getattr__`, `__dir__` — deprecation shims); `expression_statement > assignment` with an ALL_CAPS `identifier` left.
- TS/JS: `function_declaration` and `lexical_declaration > variable_declarator` with `arrow_function`/`function_expression` value, directly under `program` or `export_statement.declaration`; `method_definition` skipped.
- Signature = `parameters`/`formal_parameters` named-child count + `return_type` text. Body similarity = 5-gram Jaccard over the *helper normalizer* tokens (see P08) of the body field. Consts compared on raw text (a const whose value is one literal normalizes to STR/NUM and every string const looks verbatim: ripgrep TEMPLATE x3, hono DEFAULT_CONCURRENCY, click V/R).
- Twin suppression (generalized per skeptic): drop a family when (a) all defining files share the same basename in different directories, or (b) any two defining files share a relative-path suffix of >= 1 component under different parent dirs (jsx/base.ts vs jsx/dom/base.ts, index/enabled.rs vs index/disabled.rs), or (c) one defining file imports or re-exports the other in the import graph (hono middleware/jwt vs utils/jwt), or (d) a file matches `sibling_dir_globs`.
- Sibling-type fan annotation: every body is a single `match_expression` / `match_statement` / `switch_statement` whose arms yield `string_literal`/`string`, first param types differ -> append "same match over N types".
- Attribution: `git log -L<start>,<end>:<file> --format=%H -s` (first commit returned), then `git log -1 --format=%B` and trailer regex `^(Claude-Session|Co-Authored-By): `; count distinct sessions.

**Reason line**
`defines plural (lines 469-475) that is also defined verbatim in cmd/flow.rs:430 and as plural(n, noun) in cmd/proposal.rs:168 — 3 copies from 3 commits / 2 sessions; hoist one`
Different-contract families print as info in the section only: `repo_relative: scan.rs:946 returns Option<String>, cmd/quirk.rs:300 returns String (Jaccard 0.3)`.

**Knobs**
`[helpers] min_files = 2, min_body_jaccard = 0.5, min_name_len = 6, report_divergent = false, compare_consts_raw = true, include_inherent_methods = false, include_test_helpers = false, attribute_commits = true, ignore_names = ["new","default","main","fmt","from","parse","run","read","write","get","set","len","is_empty","from_str"], cfg_gate_attrs = ["unix","windows","target_os","target_family","feature"], sibling_dir_globs = ["**/adapter/*/**","**/platform/*/**"], suppress_path_suffix_twins = true, suppress_import_linked = true, weight = 0`

**Objection and answer**
Objection: the deliberate-twin class (hono server-vs-DOM `css`/`cx`/`keyframes` in helper/css/index.ts vs jsx/dom/css.ts, `getNameSpaceContext` jsx/base.ts:56 vs jsx/dom/render.ts:111, re-export wrappers `verify`/`sign`/`decode` middleware/jwt vs utils/jwt, ripgrep `read`/`write` gated by cfg on the `mod`, click `__getattr__` shims) is larger than two suppressions admit; verbatim copies are t-8868's job; "wants a trait" is opinion. Answer, folded in: suppression is generalized to path-suffix twins, import/re-export-linked files, mod-level cfg and `__getattr__`; inherent methods excluded; `min_name_len` raised to 6 (loses `search`/`escape`/`types`); the same-sig-low-Jaccard class is off by default (noise in every corpus); "wants a trait" is demoted to an annotation, not a class; the section carries zero score weight. The pass keeps what t-8868 cannot: the name key catches same-name-different-contract pairs (`repo_relative`) and sub-70-token verbatim copies (`plural`, `io_err`), and adds commit/session attribution.

**Folds into**
New report section "Re-implemented helpers" (families sorted by files x Jaccard) plus a per-file reason on each defining file. Weight 0 in the composite score; `helper_copies` emitted in `--json`.

**Cost**
Cheap: one hash map over ~600 top-level defs, O(copies^2) Jaccard for the few surviving families, one `git log -L` per reported definition (6 ms each; skipped when `attribute_commits = false`).

**Rules**
- Units are top-level free functions, consts and statics in Source files: Rust `function_item`/`const_item`/`static_item` with no `impl_item` (`trait:` field) or `trait_item` ancestor; Python `module`-level `function_definition`, `decorated_definition` (not `@overload`, not `__getattr__`/`__dir__`) and ALL_CAPS `assignment`; TS `function_declaration` and `variable_declarator` with a function value under `program` or `export_statement`. Methods are excluded unless `include_inherent_methods`, then keyed `Type::name`.
- A definition (or its enclosing `mod_item`) is skipped when a preceding `attribute_item` contains `cfg(` with any `cfg_gate_attrs` entry or `cfg(test)`. Test files and `include_test_helpers = false` remove test helpers.
- Families group by exact name across distinct files; names shorter than `min_name_len` or in `ignore_names` never form families.
- A family is suppressed when its files share a basename in different dirs, share a relative-path suffix under different parents, are linked by an import/re-export edge, or match `sibling_dir_globs`.
- Body similarity is 5-gram Jaccard over helper-normalizer tokens; consts compare on raw text. Classes: verbatim (1.0), similar (>= `min_body_jaccard`), different-contract (signature differs or below threshold; section-only, off unless `report_divergent`).
- A verbatim/similar family whose bodies are each a single match/switch over string arms with differing first-param types gets the annotation "same match over N types".
- Each reported copy is attributed via `git log -L<start>,<end>:<file>` and trailer parsing; the reason counts commits and distinct sessions.
- The reason names the symbol, its line range, every other location with its class, and ends "hoist one". The section lists families sorted by files x Jaccard, then path.
- Score weight is `weight` (default 0); `helper_copies` per file is in `--json`. Every threshold and list is a `[helpers]` setting.

### P08 inlined idiom of an existing helper (tier B)

**Measures**
Unit = (helper, inline occurrence). For each top-level function with 12..=40 helper-normalized body tokens, search every other Source file's helper-normalized token stream for an exact match of the body sequence (outside any definition of the same name). Per helper: occurrences and files; per file: inlined idioms with the helper they should call. Repo stat: share of small helpers inlined somewhere.

**Why it matters for LLM code**
Sessions re-derive small idioms in place even when a helper exists. `plural`'s body is inlined at cmd/done.rs:469 and cmd/init.rs:156 (`if ahead == 1 { "" } else { "s" }`) and in a different shape (`e.rules == 1`, `open.len() == 1`) at rulesdoc.rs:570, triage.rs:293/478/867, cmd/done.rs:540. The clones pass cannot see this: ~12 tokens against a 70-token floor, and a repo-wide 14-gram prototype found 4407 repeated 14-grams in kanspec, almost all imports and signatures. Prototype (p08.py; p08_floors.out, p08_keep.out, p08_v2.out): fully anonymized floor 8 is noise — kanspec 48/244 helpers flagged (19.7%), ripgrep 178/507 (35.1%), kanspec `cmd` "inlined 204x", ripgrep `escape` 329x. Callee-names-kept floor 12: kanspec 8/205 (3.9%), hono 1/137 (0.7%), ripgrep 0/326, fd 0/43, click 0/50, scry 0/12 — clean LLM/human separation. Real hits in that mode: `io_err` cmd/init.rs:373 body `KsError::internal(anyhow::anyhow!(STR, ID.display()))` inlined at lock.rs:90/100/106 and store.rs:974; `plural` cmd/flow.rs:430 at cmd/done.rs:469, cmd/init.rs:156; hono `toArray` jsx/children.ts:3 `Array.isArray(x) ? x : [x]` inlined in hono-base.ts:356, jsx/base.ts:481, jsx/dom/css.ts:135, middleware/combine:148, secure-headers:254. Noise found and designed out: `anchor` ids.rs:239 `format!("{}.{}", ..)` vs cmd/flow.rs:958 `format!("{} - {}", ..)` (STR masking equated different format strings); `stem` store.rs:167 vs paths.rs:49 (any 4-call chain, full anonymization).

**Detection**
- Helper candidates from the P07 extractor; body = `block` / block / `statement_block` or expression body; strip outer braces, a leading `return`, a trailing `;`.
- Helper normalizer (new, shared with P07; not the clones one): anonymize only *bound* names — params, `let`/`const`/`var` patterns, closure params, `for` targets — to `ID`; keep callee names (`call_expression` function, `field_identifier` under `field_expression`, `scoped_identifier` segments, `macro_invocation` macro name, TS `property_identifier`, Python `attribute` names) verbatim; keep string literals that are the first argument of `format!`/`println!`/`eprintln!`/`anyhow!`/`bail!`/`write!` and Python f-strings/`.format` receivers (else -> STR); numbers -> NUM; comments dropped; inside Rust `macro_invocation > token_tree`, `identifier` nodes whose text is a Rust keyword (`if`, `else`, `match`, `return`, ...) are emitted as keywords.
- Optional single-slot wildcard (`allow_one_wildcard`): one `ID`-position in the helper may match any single expression node (`n == 1` matching `e.rules == 1`); off by default, needed to reach the proposal's 7-copy claim.
- Index helpers by their first 4 tokens; rolling exact match over every Source file's stream; map hits back to lines via token byte offsets; discard hits inside any definition of the same name; require >= `min_distinct_kinds` token kinds and at least one non-ID/STR/NUM token and at least one kept literal or call name.
- Reachability: if the helper is not `pub`/exported and the hit file cannot import it, phrase the fix as "hoist and call".

**Reason line**
`body of plural (cmd/status.rs:469-475) is inlined 2 times in 2 files instead of called: cmd/init.rs:156, cmd/done.rs:469 — plural is private; hoist and call`

**Knobs**
`[inlined_helpers] min_tokens = 12, max_helper_tokens = 40, min_occurrences = 2, min_files = 2, min_distinct_kinds = 3, keep_format_strings = true, allow_one_wildcard = false, require_reachable = false, ignore_names = ["main","new","default"], weight = 0`

**Objection and answer**
Objection: with identifiers, strings and numbers masked, a 12-token body is a shape, not an idiom — `if ID == NUM { STR } else { STR }` is also ripgrep printer/src/standard.rs:1318 `if remaining == 1 { "match" } else { "matches" }`; the human verified the kanspec hits by reading literals the normalizer discards; all three `plural` copies are private so "call it" is not a fix; inlining a 3-line idiom is untidiness, not risk. Answer, folded in: callee/field/macro names and format strings are kept (mandatory, not a knob), keyword recovery in token trees, floor 12 — the prototype shows this is what turns 19.7%/35.1% noise into 3.9% vs 0-0.7%; the reason says "hoist and call" for private helpers; score weight is zero. Tier B because the yield after refinement is ~8 kanspec helpers of which 2-3 are clearly real, and it depends on the new normalizer landing first.

**Folds into**
Reason line on the file that inlines, and a subsection under "Re-implemented helpers" (same section as P07). Weight 0.

**Cost**
Cheap: one pass over token streams with a 4-token prefix index (kanspec 34k lines in ~3 s in Python; sub-second in Rust).

**Rules**
- Candidate helpers are P07 units whose helper-normalized body has `min_tokens`..=`max_helper_tokens` tokens, >= `min_distinct_kinds` kinds, at least one keyword/operator token and at least one kept literal or call name; names in `ignore_names` are skipped.
- The helper normalizer anonymizes only bound names (params, let/const/var/closure/for bindings) to `ID`; callee, field, method, macro and path names are kept; a string that is the first argument of a format-family macro/call is kept when `keep_format_strings`; other strings -> `STR`, numbers -> `NUM`; comments dropped; keyword-spelled identifiers inside Rust `token_tree` become keywords.
- Bodies drop outer braces, a leading `return`, a trailing `;` so tail-expression and `return` forms match.
- Hits are exact token-sequence matches in other Source files, excluding any span of a definition with the same name; with `allow_one_wildcard` one `ID` slot may match one expression node.
- A helper is reported when hits reach `min_occurrences` across `min_files` files; each hit maps to `file:line` via token byte offsets.
- The reason names the helper, its definition range, the count, every hit location, and says "instead of called" when the helper is reachable from the hit file (pub/exported and importable per the deps graph), else "hoist and call".
- Weight 0; per-file `inlined_idioms` in `--json`. All thresholds are `[inlined_helpers]` settings.

### P09 hand-rolled stdlib utilities (tier C)
Rejected: two real hits across six corpora, both already surfaced by P07/P08; the catalogue encodes a dependency policy (kanspec out.rs:47 `join` is documented as the ONE house spelling; hono is zero-dependency by design), clippy `manual_*`/ruff FURB/unicorn already lint the expression, and half the prototype hits were impl-method wrappers. Only the manifest-contradiction slice ("defines strip_ansi while strip-ansi-escapes is in Cargo.toml") has content — file it as one entry in t-eccd.

### P10 repeated literals across files (tier A)

**Measures**
Two literal classes, each a family list, one collection pass.
(1) **Prose message families**: string literals >= `min_len`, prose (>= 2 words, contains whitespace), in a message role, masked (`{..}`, `${..}`, `%s`, digits) and lowercased; exact family = same masked text in >= `min_files` files. Per file: count of literals in cross-file families, percentile-ranked.
(2) **Configuration literal families** (the alternate, folded in): strftime/format patterns (`%Y`/`%H`/`%d`), ALL_CAPS env-style names >= 6 chars, path-like strings, URLs, MIME types, and numbers (>= 100 or float) in a config role (`const_item`/`static_item`, `field_initializer`, TS `pair`, Python `keyword_argument`/`default_parameter`, builder calls) whose role name matches across files after case/underscore folding. Reported with whether any occurrence is already a named constant.
(3) Near-duplicate prose pairs (word-set Jaccard >= `near_jaccard`, < 1.0, >= `min_words`) as info only.

**Why it matters for LLM code**
Every session hand-writes the same next-step hint and error phrasing, and no session knows which module owns the constant. kanspec/src: `"kanspec show {id}"` 23x in 10 files (cmd/flow.rs:314, 786, 1059, cmd/repair.rs:104, cmd/scan.rs:239 ...), `"kanspec doctor"` 24x in 11 files, `"kanspec scan --explain {id}"` 7x in 5 files, `"kanspec rules"` 8x in 5 files, `"unknown (squash suspected, no gh)"` in board.rs:1202, derive.rs:1376, git.rs:946, scan.rs:1118; near 0.89 `"Knowledge check — branch touched {} (spec: {}) ..."` cmd/done.rs:492 vs triage.rs:320; near 0.8 scan.rs:580 vs git.rs:489 two spellings of one `git log --grep --format=%H` command. Config class: `"%Y-%m-%dT%H:%MZ"` at board.rs:422, cmd/proposal.rs:426, logentry.rs:23, scan.rs:200, scan.rs:972 (and the repo formats time 4 other ways: `"%Y-%m-%dT%H:%M:%SZ"` logentry.rs:87, lock.rs:64; `to_rfc3339_opts` cmd/ticket.rs:219, cmd/features.rs:179, cmd/done.rs:365/448; `to_rfc3339()` gh.rs:590; `"%Y-%m-%d"` doctor.rs:294, derive.rs:373, board.rs:331); `"GIT_WORK_TREE"`/`"GIT_INDEX_FILE"` at gh.rs:325-326, git.rs:276-277, paths.rs:126-127; `300` as `Duration::from_secs(300)` cache.rs:356 and `fetch_max_age_secs: 300` config.rs:134 — same TTL, two homes; the same-role rule kills `240` (cache_age_secs board.rs:1144 vs DETAIL_MAX gh.rs:76). Prototype (p10.py, p10_v2.out) with the prose rule — share of prose literals (>= 10 chars) in exact cross-file families: kanspec 192/1256 = 15.3% (52 families; 22/54 files have >= 3 family literals; doctor.rs 21, derive.rs 15, store.rs 15); scry 0/203; fd 0/48; ripgrep 7/1130 = 0.6% (3 families); click 10/613 = 1.6% (5 families, all deprecation shims in __init__.py vs core.py); hono 23/905 = 2.5% (9 families). Without the prose rule: kanspec 23.9%, hono 37.8% (HTTP header names, content-type x36 in 25 files) — the rule is essential. Near-dup pairs as share of distinct literals: kanspec 2.3%, ripgrep 1.5%, click 4.9%, hono 2.8%, fd 0 — does not separate. Cleanest LLM/human separation in the cluster.

**Detection**
- Rust: `string_literal` and `raw_string_literal` (both appear as normal nodes inside `macro_invocation > token_tree`). Python: `string` -> concatenate `string_content`, `interpolation` children are holes. TS/JS: `string > string_fragment`; `template_string` with `template_substitution` holes.
- Exclusions by ancestor walk: `use_declaration`, `attribute_item` (doc/clap/serde attrs), macro named `assert*`/`debug_assert*`, `cfg(test)` `mod_item`; Python `import_*`, `assert_statement`, calls whose function starts with `assert`, docstrings (first `expression_statement` of a `module`/`class`/`function` block); TS `import_statement`/`export_statement` sources, calls whose callee starts with `expect`/`assert`, `jsx_attribute` values; Test files.
- Message role (prose class only, per skeptic): the literal is an argument of `format!`/`println!`/`eprintln!`/`anyhow!`/`bail!`/`write!`/`writeln!`/`panic!`, `raise`/`throw`, `console.*`/`logger.*`/`log::*`/`tracing::*`, or a `return`/`Err(...)` value — enumerated in `message_macros`/`message_callees`. Literals failing the prose rule or matching `nonprose_regex` (plus header-like `^[a-z]+(-[a-z]+)+$`) route to the config class or are dropped.
- Config class: literal kind by regex; role name = attached `field_identifier` / `identifier` / `property_identifier` / `keyword_argument` name / builder `field_identifier`, folded (`_` removed, lowercased); numbers in `ignore_numbers` skipped unless in a string class; strings in a string class match regardless of role.
- Near pairs: rare-word inverted index (df <= `rare_word_df`), pairwise Jaccard within buckets, cross-file only.

**Reason line**
`9 of its strings recur elsewhere: "kanspec show {id}" (line 314) is spelled in 10 files (23x) — centralise the hint text`
Config class: `timestamp pattern "%Y-%m-%dT%H:%MZ" (line 422) is duplicated in cmd/proposal.rs:426, logentry.rs:23, scan.rs:200 and scan.rs:972 with no shared constant; repo also formats time 4 other ways`
When a named constant exists: `... — use FETCH_MAX_AGE from config.rs:134`.

**Knobs**
`[strings] min_len = 10, min_files = 2, exact_min_words = 2, require_message_role = true, message_macros = ["format","println","eprintln","anyhow","bail","write","writeln","panic"], message_callees = ["console","logger","log","tracing","raise","throw"], near_jaccard = 0.8, min_words = 3, rare_word_df = 40, include_raw = true, exclude_assert_calls = true, exclude_docstrings = true, exclude_attributes = true, exclude_jsx_attributes = true, ignore_patterns = [], nonprose_regex = "^(%|[A-Z_]{4,}$|[./~]|https?://|[a-z]+(-[a-z]+)+$)", config_classes = ["strftime","env_name","path","url","mime","number"], ignore_numbers = [0,1,2,10,100,1024], weight = 0.03`

**Objection and answer**
Objection: near-dup families at 0.6 cluster diagnostics that differ in the one meaningful word (spec/path/scope globs); CLI hints and HTTP constants repeat by design; docstrings, attribute token trees, JSX/Tailwind strings and gettext ids flood families; hono's most-repeated strings live in *.test.ts so Test classification is load-bearing; the per-file percentile ranks files that talk to users, not risky ones. Answer, folded in: the message-role requirement plus docstring/attribute/JSX/import exclusions remove every listed class except deliberate hints; `near_jaccard` raised to 0.8 with near families info-only and outside the score; the config-literal class (drift-bug story, no linter covers it) is kept with the same-role rule. On the hint objection the design disagrees on purpose: a hint printed from 10 commands by hand *is* the finding ("wants a fixes catalogue") — a repo that wants it silenced adds the string to `ignore_patterns`. Score weight is kept small (0.03) because the 15.3% vs <= 2.5% separation is the strongest in the cluster and the per-file count is percentile-ranked like every other signal; the maintainer can zero it.

**Folds into**
New report section "Repeated literals" (exact prose families, config families, then near pairs), per-file reason, and a small composite weight (`weight`, default 0.03, taken from the `size` slice) on the percentile of per-file family-literal count.

**Cost**
Cheap: linear literal walk plus a rare-word inverted index for near pairs.

**Rules**
- Literals are collected from Source files only: Rust `string_literal`/`raw_string_literal` (also inside `token_tree`), Python `string` with `interpolation` as holes, TS `string`/`template_string` with `template_substitution` as holes.
- A literal is dropped when any ancestor is an import/use, an attribute, a `cfg(test)` mod, an assert-family macro/call/statement, a docstring position, an import/export source, or a `jsx_attribute`.
- Masking: interpolation holes, `%s`-style specifiers and digit runs become one placeholder; text is lowercased and trimmed.
- Prose class: length >= `min_len`, >= `exact_min_words` words with whitespace, not matching `nonprose_regex`, and (when `require_message_role`) an argument of a `message_macros`/`message_callees` call or a `return`/`Err` value. An exact family is one masked text in >= `min_files` distinct files.
- Config class: a literal matching a `config_classes` regex (strftime, env name, path, URL, MIME) in any position, or a number not in `ignore_numbers` in a config role (const/static, field initializer, object pair, keyword argument, default parameter, builder call). Numbers match only when the folded role name matches across files; string classes match regardless of role.
- Near pairs are cross-file prose literals with word-set Jaccard in [`near_jaccard`, 1.0) and >= `min_words` words, bucketed by words with df <= `rare_word_df`; info-only.
- Per-file `family_literals` = count of literals in exact prose or config families; percentile-ranked with zero -> 0; contributes `weight` to the score.
- Each family lists every `file:line`; if any occurrence is a named const/static, the reason says "use NAME from file:line".
- Families sort by files desc, occurrences desc, then text, so `--top` is stable. All thresholds and lists are `[strings]` settings.

### P11 convention majority and per-file dissent (tier C)
Rejected: within any single repo, LLM or human, every convention pair is near-unanimous (dissenting files: scry 0/9, kanspec 1/54 — all seven `.expect` votes in cmd/repair.rs's `#[cfg(test)]` module — fd 1/22, ripgrep 4/86, click 0/16, hono 7/186; every inspected dissent was semantically required or test code), the motivating evidence (`.context` in scry vs `.map_err` in kanspec) is cross-repo and invisible to a scan, and the equivalent pairs are already one-way lints. The API-family finding (five timestamp spellings) is P10's config class.

### P12 identifier vocabulary drift (tier C)
Rejected: measured at declaration sites the signal inverts — LLM corpora produce zero spelling-variant families and zero synonym dissents (scry 0/9, kanspec 0/54) while humans produce a handful of non-actionable ones (ripgrep `line_term`/`lineterm` across public crate APIs, click `help`/`help_` keyword avoidance, hono `crossorigin`/`crossOrigin` HTML-vs-DOM); the flagship `tempdir` vs `temp_dir` is two library APIs (`tempfile::tempdir` vs `std::env::temp_dir`), and without types the synonym table guesses intent. At most a one-line repo dominant-form table for rename hints, riding in t-eccd.

## provenance and machine-written fingerprints
### P13 agent provenance and session attribution (tier A)

**Measures**
- Per commit: `is_agent` (author name/email or any `Co-Authored-By` trailer matches `agent_patterns`), `is_bot` (matches `bot_patterns`; excluded from every count), `session` (first capture of `session_pattern` over the `Claude-Session` trailer, else none), lines added/deleted (`--numstat`).
- Per file: `agent_commits / commits`, `human_commits` (commits with neither flag), `distinct_sessions`, `max_single_commit_added`; with blame enabled, `agent_line_share` and `unknown_line_share` (blame shas outside the window).
- Per repo: agent commit share, agent line share, distinct sessions, delete/add ratio, count of files with `human_commits == 0`.
- Unit: file (repo block alongside). Function-level detail only by intersecting blame lines with the metrics units.

**Why it matters for LLM code**
It is the one signal that names the category instead of a symptom. kanspec: `git log --format='%an|%ae|%(trailers:key=Co-Authored-By,valueonly)'` gives 35 commits authored `Claude <noreply@anthropic.com>` and 32 human-authored commits carrying `Co-Authored-By: Claude Opus 5 (1M context)` or `Claude Fable 5.1`: 67 of 79 non-merge commits are agent work (85%), 7 distinct `Claude-Session` ids. Blame over src/**/*.rs: 98.6% of 34,092 lines across 54 files are agent-authored; 52/54 files >= 0.9; 38/54 files have zero human-only commits; src/cmd/ticket.rs (1,044 lines), cmd/decision.rs, model.rs, ids.rs are 100%. The top hotspots were each written by several independent sessions: src/derive.rs 14 commits / 5 sessions, src/cmd/flow.rs 16 / 5, src/cmd/proposal.rs 15 / 6, src/doctor.rs 11 / 5. Whole-history numstat: kanspec +87567 / -6041 (delete/add 0.069), scry +5068 / -327 (0.065); the largest single commit 693389a adds 671 lines across 14 files and deletes 12. Baselines (re-cloned with history): ripgrep 400 commits, 0 agent, 1 dependabot, 0.0% on all files; click 1084 non-merge commits, 0 agent, 138 bot commits excluded, core.py has 217 human-only commits. Honest limit: scry (100% LLM-written) has 26 commits, only 2 with a Claude trailer, author always the human -> 0.0% on all 9 files; the signal sees only workflows that leave trailers or agent identities. Second limit: inside kanspec the never-human flag lands on 38 of 54 files (70%), so it cannot rank files within a uniform repo.

**Detection**
- Git only for the file-level signal. Keep the existing `--name-only` log untouched except for `--name-only` -> `--numstat` (path is the text after the second tab; `-\t-` is binary, count 0). Add a second, cheap log for identity: `git log --no-merges --since=<window> --format='%x01%H%x02%an%x02%ae%x02%at%x02%(trailers:key=Claude-Session,valueonly)%x02%(trailers:key=Co-Authored-By,valueonly,separator=%x03)'`. Split on `\x01`, then on `\x02` at most 5 times; everything after the fifth separator is the trailer blob. This ordering is mandatory: `%(trailers:...)` emits a trailing newline per value and a malformed body can put newlines inside a value (one click commit does), and the current parser treats line 2 onward of a record as paths. Join the two logs by hash.
- Classification: lowercase name, email and trailer blob; `is_bot` first (`[bot]`, dependabot, renovate, pre-commit-ci), then `is_agent` on any `agent_patterns` substring. Bots are neither agent nor human.
- Session id: `session_pattern` regex (default `session_[A-Za-z0-9]+`) over the Claude-Session value; absent -> the commit's own hash prefix when `is_agent`, else `human:<author>`.
- Blame (opt-in): `git blame -w -M --line-porcelain HEAD -- <path>` per Source file; `author-mail`/sha map through the commit table; shas outside the window are `unknown`, never human. `-w -M` so a `cargo fmt` or mass-rename commit does not take over lines.
- Bus-factor fix (ship regardless): distinct-author count for the existing "single author over the window (bus factor 1)" reason excludes `is_bot` commits; 138 of 1222 click commits are bots.
- Attachment (from the consistency alternate): any finding that already carries an introducing commit (future clone/duplicate-helper passes, t-8868) gets a `sessions` list for free from the same table; no new lookups.

**Reason line**
Per file, only when the repo is mixed (see Knobs): `100% of its 1,044 lines are agent-attributed (author 'Claude' or Co-Authored-By: Claude on 6/6 commits; 3 of those are human-authored commits carrying an assistant trailer, so this is an upper bound); no trailer-less human commit touched it in the window`
Per file, any repo: `touched by 6 different agent sessions (16 commits, 100% agent-attributed; largest single commit +671 lines): no session owns its design`
Repo section, always: `provenance: 67 of 79 commits agent-attributed (85%), 99% of source lines; 7 distinct agent sessions; 38 of 54 files never touched by a trailer-less human commit; delete/add ratio 0.07 (code is only ever added)`

**Knobs**
```toml
[history.provenance]
enabled = true
agent_patterns = ["claude", "copilot", "codex", "cursor", "devin", "aider", "noreply@anthropic.com"]
bot_patterns = ["[bot]", "dependabot", "renovate", "pre-commit-ci"]
match_trailers = true
session_pattern = "session_[A-Za-z0-9]+"
min_share = 0.9              # file-level agent share to emit the share reason
mixed_repo_max_share = 0.8   # per-file share reason only when the repo share is below this
min_sessions = 4             # sessions reason
blame = false                # agent_line_share needs one blame per Source file
never_human_multiplier = 1.0 # score-neutral; opt-in review prioritisation
```

**Objection and answer**
Objection: the signal is a property of the repo, not the file (kanspec 99%, ripgrep 0%); percentile ranking cannot use it; attribution is wrong both ways (25 hand-written-looking Trevor commits carry an always-on trailer; trailer-less Trevor commits c4989d3 and 2fb3ad4 read as agent work and would clear the never-human flag); "no human-only commit" is not "no human review" since every kanspec ticket ships through a PR; 'Claude' is a French given name; no demonstrated relation to defects. Answer folded in: it is built as a provenance label, not a score input (`never_human_multiplier` defaults to 1.0, weight-neutral); the reason says "agent-attributed" and "trailer-less human commit", never "no human has written", and carries the upper-bound caveat whenever a counted commit is human-authored with a trailer; the per-file share reason is gated on `mixed_repo_max_share` so a uniform repo gets one repo line, not 38 reasons; the session-count reason is the part that does vary within kanspec (6/5/5/5 sessions on the four biggest hotspots vs 1-2 elsewhere) and needs no blame; blame is opt-in and runs `-w -M`; bots are excluded from both agent and human counts, which fixes the existing bus-factor reason; identity splits are the user's `.mailmap` problem and are noted in the JSON as `authors_raw`. What remains after narrowing is a label the other LLM-tell analyzers can be evaluated against, plus a genuinely new "assembled by N sessions" reason.

**Folds into**
No score weight. A new `provenance` report section (repo block, then the per-file lines that pass the gates) and a `provenance` JSON block per hotspot and per repo. The bus-factor bot exclusion changes an existing reason. P14's survivor (median inter-commit gap, share of commits inside <=15-minute runs) is one informational line in the same repo block, needing only `%at` from the identity log. t-16ca can read the per-file flag; t-0530 should treat `is_agent` authors as a category, not one more name.

**Cost**
Cheap without blame: one extra `git log` without `--name-only`, joined by hash (kanspec 79 commits, ripgrep 400, click 1084: negligible). Blame: one process per Source file, 54 files / 34k lines of kanspec in ~2 s (0.86 s reported on src alone); minutes on a monorepo, hence opt-in.

**Rules**
- Identity log runs once per scan under the same `--since` as the history log; format fields in this order: hash, author name, email, author time, Claude-Session values, Co-Authored-By values with `%x03` separator; trailer blob is last and may contain newlines; split each record on `\x02` at most 5 times.
- A commit is `bot` if name or email contains any `bot_patterns` entry (case-insensitive); `bot` commits are excluded from agent, human and distinct-author counts.
- A commit is `agent` if not `bot` and name, email or (when `match_trailers`) the trailer blob contains any `agent_patterns` entry (case-insensitive).
- `caveat` per file is true when any counted agent commit has an author who does not match `agent_patterns` (human-authored with trailer); the share reason must append the upper-bound clause whenever `caveat` is true.
- Session key: first `session_pattern` match in the Claude-Session value; else hash[..7] for agent commits; else `human:<name>`. `distinct_sessions` counts agent keys only.
- `--name-only` becomes `--numstat`; per-file `added`/`deleted` summed over its commits; repo `delete_add_ratio = deleted / added` (0 when added is 0); `max_single_commit_added` per file.
- Per-file share reason fires iff `enabled`, repo agent line share (or commit share when `blame = false`) < `mixed_repo_max_share`, file share >= `min_share`, and `human_commits == 0`; wording uses "agent-attributed" and "trailer-less human commit" verbatim.
- Sessions reason fires iff `distinct_sessions >= min_sessions`; it names session count, commits, agent commit share and `max_single_commit_added`.
- Repo provenance line always prints when at least one agent or bot commit exists; silent otherwise (human repos print nothing).
- Blame runs only when `blame = true`, with `-w -M --line-porcelain HEAD`, Source files only; shas not in the commit table are `unknown` and count toward neither share; JSON reports `agent_line_share`, `human_line_share`, `unknown_line_share`.
- Score: multiply by `never_human_multiplier` only when the share reason fired; default 1.0.
- JSON: per hotspot `provenance = {agent_commits, human_commits, bot_commits, agent_commit_share, agent_line_share?, distinct_sessions, sessions[], max_single_commit_added, caveat}`; repo `provenance = {agent_commit_share, agent_line_share?, distinct_sessions, delete_add_ratio, never_human_files, files}`.
- Names only in output; emails are matched, never printed.
- Bus factor: the "single author" reason counts distinct non-bot author names.

### P14 session burstiness (tier C)
Rejected: measures merge strategy, not risk. Rebase-merged PR series trip `min_largest_share` for almost every file, trailer-less kanspec human commits land 2-13 minutes apart like the agent ones, and with honest thresholds (5 commits / 0.6 / not revisited) zero kanspec and zero baseline files fire; the top hits were the human's 08-31 integration batches. Only the repo-level gap line survives, as one line inside P13's provenance block.

### P15 naming profile outlier (tier C)
Rejected: the within-repo z-score never fires (max profile_distance 1.99 in six corpora, all five top hits noise: parser one-char lets, hono's 421 `res`), a wholly LLM-written repo has no outlier by construction, and the TS test-name feature is inverted by BDD convention (1,270 of 1,922 hono it() names begin with 'should'). The cross-repo absolute fingerprint (one-char lets 0.20-0.29 vs <= 0.06) is provenance, not a refactoring reason; the "N of M tests named as sentences" tag for Rust/Python only is worth revisiting if a weak-assertion analyzer exists to point it at.

### P16 comment prose style fingerprint (tier C)
Rejected: the strongest separator is fitted to kanspec's mandated vocabulary (t-xxxx, D-nn, §, ARCHITECTURE.md), scry, the second LLM corpus, scores 0 process refs, 0 em-dashes and 0 shouts, the history regex matches passive "is used to", and "delete or move to the ticket" is advice a maintainer would reject for their own traceability comments (ripgrep: 218 issue/PR-link lines). What survives is a user-supplied comment-regex class list with empty defaults plus a banner count as a knob on t-eccd, weight 0.

## Comment structure, load and staleness

Five proposals, two kept. The two survivors are actionability add-ons, not smells: they decorate hotspots the report already ranks with the split points the author (usually an LLM) wrote down as comments. The three rejected ones (comment load, blame staleness, duplicated comment paragraphs) all failed the same way: on human corpora their top hits are the comments a maintainer wants kept, so the reason line would tell an agent to undo good work.

Shared substrate for both survivors: a `comments` pass that walks every Source file's tree once, collects non-doc comment nodes with their parent node, and exposes a `banner` regex. P17 consumes top-level comments (parent `source_file` / `module` / `program`); P18 consumes body-level comments (parent is a unit's body block). Same node kinds, same regex, one walk.

### P17 file-level banner partitions (tier A)

**Measures**
Per Source file: the top-level rule/title banner comments, the sections they delimit (line spans), the named functions/items each section contains, the largest section's line count, and the percentile of section count within the repo. Unit: file; sub-unit: section.

**Why it matters for LLM code**
An LLM narrates sub-files inside one file where a human would split. Prototype counts (top-level banner lines / files with banners / hits at >= 3 sections and largest >= 80 lines): kanspec 146 / 27 of 54 / 15; scry 8 / 2 of 9 / 1; fd 0 / 0 of 22 / 0; ripgrep 0 / 0 of 88 / 0; click 0 / 0 of 16 / 0; hono 0 / 0 of 187 / 0. grep cross-check for indented rules (`^\s+//\s*[-=─]{4,}`): kanspec 63, every human corpus 0. kanspec sections-per-file distribution: 1:5, 2:7, 3:6, 4:2, 5:1, 6:4, 7:1, 8:1.

Top hits: kanspec scan.rs (1344 lines): 4 sections 'The seals'@59, 'The ladder's value types'@233, 'The ladder'@311, 'The recorded human override'@979; largest 314-978 (665 lines). kanspec derive.rs (2308 lines): 6 sections, largest 'the clock, read from the snapshot' 1249-2308 (1060 lines), and it is scry's #1 hotspot already. kanspec cmd/flow.rs (1470 lines): 7 sections named ready/start/ship/park/drop, largest 'drop' 969-1470 (502 lines). Borderline: kanspec cli.rs, 8 clap-tree sections, largest 228 lines. Noise: scry config.rs (316 lines), 6 sections of per-pass config structs, largest 99 lines.

The section names are the module names, which is what makes the output actionable rather than a restatement of the size percentile.

**Detection**
- Rust: `line_comment` with no `outer_doc_comment_marker` / `inner_doc_comment_marker` child, or `block_comment` without those markers, whose parent is `source_file`. Comments inside `mod tests { ... }` sit under `declaration_list` and are excluded for free.
- Python: `comment` whose parent is `module`.
- TS/TSX/JS: `comment` whose parent is `program`.
- Banner text (after stripping the comment marker) matches `^\s*[-=─═━*#_]{min_rule_len,}\s*(\S.*?)?\s*[-=─═━*#_]*$` or `^\s*(──|--|==)\s*\S.*$`. Count rule characters as chars, not bytes (`─` is 3 bytes; two box-drawing chars must not satisfy min_rule_len = 6).
- Skip editor markers: text starting with `#region`, `#endregion`, `%%`, or matching `endregion`.
- Banners within `pair_gap` lines of each other merge into one boundary (rule / title / rule triple = one boundary). Title = inline text on the rule, else the first non-banner comment line inside the pair. A boundary with no title is dropped (skeptic's narrowing: every counted banner carries a title).
- Boundary at line <= 3 followed by `use`/`import`/`from` is a license or file header; skip it, and also skip the second rule of that pair.
- Sections = spans between consecutive boundaries (last runs to EOF). For each section, list the named units from the metrics pass whose start line falls inside it.
- Rows: derive from byte offsets, not `start_point.row` (py-tree-sitter-rust 0.24 returned corrupt rows for comments containing multi-byte rule chars; verify scry's vendored grammar before trusting rows).
- Emit only when the file is already a hotspot candidate: file lines >= `min_file_lines` and size percentile >= `min_size_percentile` (or in the printed hotspot list).

**Reason line**
`scan.rs is cut into 4 labelled sections: 'The seals' 60-233, 'The ladder's value types' 234-311, 'The ladder' 312-979 (665 lines, 12 fns), 'The recorded human override' 980-1344 — extract the largest as ladder.rs`

**Knobs**
`[comments.banners]` min_rule_len = 6, rule_chars = "-=─═━*#_", pair_gap = 2, min_sections = 3, min_section_lines = 100, min_file_lines = 400, min_size_percentile = 80, skip_top_of_file = true, skip_markers = ["#region", "#endregion", "%%"], require_title = true, enabled = true.

**Objection and answer**
Objection: the only defect-predictive half (length) is already the size signal; banners are one author's style, and the hardest hits (cli.rs clap tree, scry config.rs registry) are files a human would not split. Answer, folded into the design: this is a split-plan annotation, not a smell and not a score input; it emits only for files already above the size percentile or in the hotspot list, so on human repos (0 banners everywhere) it is silent and costs nothing; min_section_lines = 100 and min_file_lines = 400 drop config.rs; the reason lists sections with their function counts and says "extract the largest", never "wants to be N modules". cli.rs still prints, and that is fine: the sections are the split an agent would use if asked to shrink it, and the maintainer can raise min_section_lines.

**Folds into**
No score weight. A per-hotspot annotation line under the file's existing reasons, and a `sections` array on the hotspot in `--json` (name, start, end, lines, functions). Also consumed by `scry context <paths>` (t-b342) when the path is a hotspot.

**Cost**
Cheap. One pass over already-parsed trees plus regex on comment text; no git, no graph.

**Rules**
- Top-level comment = comment node whose parent is the file root (`source_file`, `module`, `program`); Rust doc comments (any `*_doc_comment_marker` child) are never banners.
- A banner is a top-level comment whose stripped text matches the rule regex with >= `min_rule_len` rule *characters* from `rule_chars`, or the short-rule form (`──`, `--`, `==` followed by text). Text starting with a `skip_markers` entry is not a banner.
- Banners within `pair_gap` lines merge into one boundary; the boundary's title is the rule's inline text, else the first non-rule comment line between the pair. Untitled boundaries are dropped when `require_title`.
- With `skip_top_of_file`, a boundary whose first line is <= 3 and whose next non-comment node is an import/use is dropped, together with any second rule inside the same pair.
- Sections are the spans between boundaries; the first spans from line 1 to the first boundary and the last to EOF. A section's functions are the metrics units whose start line lies inside it.
- A file is reported when lines >= `min_file_lines`, its size percentile >= `min_size_percentile` (or it is in the hotspot list), sections >= `min_sections`, and the largest section >= `min_section_lines`.
- The reason names every section with its span and marks the largest with its line and function count. Not a score input.
- `mod tests` and Test-classified files are excluded (parent is `declaration_list`; Test files are not ranked).
- Rows are computed from byte offsets, never from tree-sitter `start_point.row` on comment nodes.
- All thresholds live under `[comments.banners]` in `scry.toml`.

### P18 function-level phase sections and extract-method plan (tier A)

**Measures**
Per function already over the cognitive threshold: the phase comments inside its body, each phase's line span and line count, an estimated post-split cognitive per phase, and the locals shared across phases. Unit: function; sub-unit: phase.

**Why it matters for LLM code**
LLMs write long functions but pre-annotate their phases, so the split points are in the source. Prototype (functions with any phase comment / phase comments / hits at proposed floors / hits at line-floor variant): kanspec 12 / 32 (28 rule-style, 4 numbered) / 5 / 9; scry 0 / 0 / 0 / 0; fd 0 / 0 / 0 / 0; ripgrep 2 / 2 / 0 / 0; click 1 / 5 (all in one `__init__`) / 0 / 1; hono 0 / 0 / 0 / 0. Cognitive of the kanspec hits: attention 30, ladder 32, done 18, render_text 14, plan_done 11 — the pass lands on the repo's top-complexity functions.

Top hits: kanspec derive.rs `attention` (778-1065, cognitive 30): 3 phases YOU@786 (126 lines), AGENT@913 (63), WATCHING@977 (88). kanspec scan.rs `ladder` (405-656, cognitive 32): 6 rung phases of 15-79 lines each; the proposed min_stmts = 3 floor drops it because each rung is one big `if let` block, so a line floor is the right knob. kanspec cmd/done.rs `done` (75-257, cognitive 18): 'step 1: verify merged'@85 (46 lines), 'steps 2 + 3'@132 (39), 'step 4'@172 (85). Noise removed by the span floor: kanspec cmd/ticket.rs `ticket_for_branch` (755-788), three numbered fallbacks of 7-12 lines in a 34-line fn. Human near-miss: click core.py `Option.__init__` (2979-3058) with 'Phase 1..5' comments, two phases >= 8 lines — a fair extract-method hint even in human code.

Today's reason stops at "worst attention at 30 (lines 778-1065, nesting 3)" and gives the agent nothing to cut along.

**Detection**
- Units and body nodes from the metrics Profile: Rust `function_item` -> `block`; Python `function_definition` -> `block`; TS/JS `function_declaration` / `method_definition` / arrow -> `statement_block`.
- Phase comment = Rust `line_comment` without a doc marker child, Python `comment`, TS `comment`, whose parent is the body block or is one nesting level below it (a `match` arm block, an `if` block) so match-arm banners like kanspec lib.rs:123-173 count. Depth is a knob (`max_depth` = 1).
- Text matches one of `patterns` (case-insensitive). Default list, narrowed per the skeptic: `^\s*[─═━]{2,}\s*\S` (box-drawing rules only; drop `--` and `==` which match `// --flag` prose in ripgrep main.rs:398, lowargs.rs:467, kanspec board.rs:47), `^\s*(step|pass|phase|stage)\s*\d`, `^\s*\d+[.):]\s`. Ordinal words (`first`, `then`, `finally`) are not in the default list.
- Phase span = from the comment row to the row before the next phase comment at the same depth, or the body's end row. Phase lines = span rows minus blank rows.
- Rust `#[cfg(test)]` modules: walk ancestors to a `mod_item` whose preceding `attribute_item` sibling contains `test`; skip when `skip_test_modules`.
- Post-split estimate: re-run the cognitive walker over each phase's statements with nesting re-based to 0 and sum the increments (the walker already computes per-node increments).
- Shared locals: identifiers bound by `let` / assignment / parameter before the phase and read inside >= 2 phases; report the count and up to 4 names as a difficulty hint.
- Cross-reference with the metrics pass by (file, unit name, start line). Emit only when the function's cognitive >= `cross_with_cognitive_min` (default = `metrics.cognitive_threshold`).

**Reason line**
`attention (lines 778-1065, cognitive 30) already labels 3 phases: YOU 786-912, AGENT 913-976, WATCHING 977-1065 — extract each as a helper (est. cognitive 12 / 9 / 9; 4 locals shared: snap, now, out, owner)`

Printed as a sub-line of the existing "worst X at N" metrics reason.

**Knobs**
`[comments.phases]` min_phases = 2, min_phase_lines = 8, min_span_lines = 40, max_depth = 1, patterns = ["^\\s*[─═━]{2,}\\s*\\S", "^\\s*(step|pass|phase|stage)\\s*\\d", "^\\s*\\d+[.):]\\s"], skip_test_modules = true, cross_with_cognitive_min = 15 (follows `metrics.cognitive_threshold` when unset), estimate_cognitive = true, max_shared_locals_named = 4.

**Objection and answer**
Objection: phase comments do not predict defects; cognitive already measures the long function; a third of kanspec's 63 indented banners sit in `mod tests`; the regex fires on `// --flag` prose and `// Finally, ...` sentences; the headline `replay` example (cognitive 14) is one metrics does not flag. Answer, folded in: it is a sub-reason of the metrics line and never a smell or score input; `cross_with_cognitive_min` defaults to the cognitive threshold, so `replay` is not reported and only functions the report already calls hard to follow get decorated; `--`/`==` and ordinal patterns are removed from the defaults; test modules are skipped by walking to the `cfg(test)` mod; banners one level below the body (match arms) count so the lib.rs evidence is detected, not just cited; the shared-locals count is printed so the reason never promises an easy cut when the phases share a dozen mutable locals.

**Folds into**
No score weight. Sub-line of the metrics "over cognitive" reason; a `phases` array on the function in `--json` (label, start, end, lines, est_cognitive, shared_locals). Input to any later refactor-plan output and to t-16ca's hook warning.

**Cost**
Cheap. Comment walk on already-parsed trees; the cognitive re-run is bounded to over-threshold functions (a handful per repo).

**Rules**
- Runs only for units whose cognitive >= `cross_with_cognitive_min`; when unset, the value is `metrics.cognitive_threshold`.
- A phase comment is a non-doc comment node whose parent is the unit's body block or a block at most `max_depth` levels below it, and whose stripped text matches one of `patterns` (case-insensitive).
- A phase spans from its comment to the line before the next phase comment at the same depth, or the body end. Phase lines exclude blank lines.
- A unit is reported when it has >= `min_phases` phases, every phase has >= `min_phase_lines` lines, and the unit spans >= `min_span_lines` lines.
- Units inside a Rust `#[cfg(test)]` `mod_item` are skipped when `skip_test_modules`. Test files are already unranked.
- With `estimate_cognitive`, each phase's cognitive is the sum of the walker's increments over the phase's statements with nesting re-based to 0.
- Shared locals are names bound before a phase and referenced in >= 2 phases; the reason prints the count and the first `max_shared_locals_named` names.
- The reason is printed as a sub-line of the metrics "worst X at N" line, naming each phase label and span. Never a standalone hotspot reason, never a score input.
- Shares the comment node kinds and banner regex with `[comments.banners]`; one tree walk feeds both.
- All thresholds live under `[comments.phases]` in `scry.toml`.

### P19 documentation load outliers (tier C)
Rejected: the tails do not separate LLM from human code (p95 comment:code ratio kanspec 0.30 vs ripgrep 0.45, click 0.77, hono 0.25) and the top hits on human corpora (ripgrep hyperlink/mod.rs `from_path` 68/30, click `_process_args_for_options` 21/12) are the comments a maintainer would fight to keep, so the reason line is wrong advice; the module-header and private-doc alternates are recommended practice, not smells.

### P20 stale comments by blame age (tier C)
Rejected: precision on the one corpus with history was 0/5 on inspection (every flagged comment, including the flagship git.rs:424, still holds after the bulk review commit beneath it), the human corpora on disk are shallow clones so no baseline exists, and depth-1 CI checkouts (t-4569) would make it emit nothing or a spurious zero age gap; the stale-marker alternate is moot (0 markers in both LLM corpora, t-eccd already counts them).

### P21 comment-carrying clones (tier C)
Rejected: inverted by the corpora (duplicate comment groups per 1k paragraphs: kanspec 2.3, scry 0, ripgrep 41.8, hono 32.3, click 13.1), and the human mass is trait-impl docs and JSDoc boilerplate that must be copied; the one useful residue (a plain paragraph tying two bodies that drifted below the 70-token floor) belongs as a 30-line divergence tag inside t-8868's structural clone tier, not a comments pass.

## Git history beyond commit counts (numstat and blame)

Three passes survive; all three sit on the same two pieces of new plumbing, so build them in this order: the numstat log switch (P22's cost, reused by everything), then the blame→metrics-unit join (P26), then P22 and P23 as reasons that consume both. Shared definitions used below:

- **reformat/move commit**: touches ≥ `history.max_cochange_commit_size` files and repo-wide adds within `reformat_tolerance` (10%) of dels. Computed once from the numstat log; excluded from every measure in this chapter and passed to blame as `--ignore-rev`.
- **boundary set**: every sha in `.git/shallow` ∪ `git rev-list --max-parents=0 HEAD` ∪ commits older than `--since`. Lines or births attributed to it are "before window", never a signal.
- **mass fix commit**: a `subject_is_fix` commit touching > `max_cochange_commit_size` files. Never a chain link, never a "fixed within N days" phrase.

### P26 function-level churn and wholesale rewrites via blame (tier B)

**Measures.** Per metrics unit (function/method/name-bound closure) with lines ≥ `min_lines`: `distinct_commits` (distinct blame shas over the unit's line range, boundary pseudo-commit and ignored revs excluded), `commits_per_100_lines`, `share_of_file_commits` = distinct_commits / blame-visible commits of the file, `dominant_sha` + `dominant_share`, and a secondary `rewritten` annotation. Per file: functions over threshold, max share. Unit: function; reported as a reason on the file, not a score term.

**Why it matters for LLM code.** The claim that survives is attribution, not authorship. kanspec `src/cmd/flow.rs` `start` (lines 188-297, 110 lines) carries lines from 8 of the 16 commits that touched the file, 74% from c59889e; `ship` (767-849) from 6; `src/cmd/status.rs` `status` (80-197) from 7 of 8; `src/cmd/done.rs` `done` (75-257, 183 lines) 7/9 commits, 86% from c59889e; `src/derive.rs` `attention` (778-1065, cognitive 30) from 5. Prototype fn-churn hits (≥ 20 lines, ≥ 4 distinct commits, share ≥ 0.4): kanspec 52/498 (10%), fd 5/60 (8%), ripgrep 5/658 (0.8%), scry 1/46. "Rewritten wholesale" (≥ 60% from a later commit): kanspec 107 (21%), fd 10 (17%), scry 3 (7%), ripgrep 29 (4%). fd is as churny per function as kanspec, so the pitch is "churn concentrates in these named functions", never "LLMs regenerate whole functions". Every top kanspec hit was a real concentration, and blame distinct commits matched `git log -L` on the cited functions (board.rs build 3/3, derive.rs attention 5/5, flow.rs start 8/9). Cost measured: kanspec 55 files / 34k lines 1.5s, ripgrep 84 files 2.4s, hono 178 src files 3.2s (~18 ms/file).

**Detection.**
- Per candidate file: `git -c core.quotePath=off blame --line-porcelain -w [-M -C] [--ignore-revs-file <f>] [--ignore-rev <sha>]… -- <path>`. Candidates = files with history commits ≥ `min_file_commits`, or the top `max_files` by provisional score; never every Source file.
- Parse header `<sha40> <orig> <final> [n]`; `author-time`, `author-mail`, `summary` on a sha's first block. Map shas in the boundary set, merge commits (`git rev-list --merges`), and reformat/move commits to one "before window" pseudo-commit.
- Honor `.git-blame-ignore-revs` and `blame.ignoreRevsFile` when present; always add the reformat/move list.
- Join `final` line numbers to metrics unit ranges: Rust `function_item` (name: `identifier`, nested in `impl_item`/`trait_item` `declaration_list`); Python `function_definition` (name: `identifier`); TS/JS `function_declaration` (name: `identifier`), `method_definition` (name: `property_identifier`), `arrow_function` whose parent `variable_declarator` has name: `identifier`. Nested units count toward enclosing units as metrics does; reporting suppresses a nested unit whose range lies inside an already-reported unit.
- Fold in P27's fact as a clause: when the dominant sha touched ≥ `mega_commit_files` files or inserted ≥ `mega_commit_lines` lines (from the numstat log, Source rows only), append its size.
- `rewritten` = distinct_commits ≥ `rewrite_min_commits` and dominant sha ≠ the sha of the unit's oldest line and dominant_share ≥ `rewrite_share` and dominant sha is not a merge/reformat/move.

**Reason line.**
`start (lines 188-297, 110 lines) carries lines from 8 of the 16 commits that touched this file, 74% from c59889e (a 5-file, +4,881 commit); ship (lines 767-849) from 6: churn concentrates in 2 functions`
Template: `<fn> (lines a-b, N lines) carries lines from K of the M commits that touched this file[, P% from <sha>[ (a F-file, +L commit)]][; <fn2> (lines c-d) from K2]: churn concentrates in J function(s)`; with `rewritten`: `… and was rewritten wholesale (P% of its lines from <sha>, newer than the function)`.

**Knobs.**
```
[blame]
enabled = true
ignore_whitespace = true
follow_moves = false            # adds -M -C
max_lines = 20000
timeout_secs = 60
max_files = 200                 # top score candidates
min_file_commits = 3            # or any file with this many commits
ignore_revs_file = ".git-blame-ignore-revs"
honor_git_ignore_revs = true    # blame.ignoreRevsFile
reformat_tolerance = 0.10
[blame.fnchurn]
min_lines = 20
min_distinct_commits = 4
min_share_of_file_commits = 0.4
rewrite_share = 0.6
rewrite_min_commits = 3
reason_max_functions = 3
mega_commit_files = 20
mega_commit_lines = 2000
score_weight = 0.0              # reserved; reason-only in v1
```

**Objection and answer.** Distinct commits per function rise with length × age, and "rewritten" fires on any two-commit function whose second commit was larger, so kanspec's 21% vs scry's 7% is commits-per-day of a 14-day repo, not a code property; the proposal reads as a scoring change. Answer, folded in: no score term (`score_weight = 0.0`, no per-function rank); report `commits_per_100_lines` beside `share_of_file_commits` and percentile-rank within the repo; `rewritten` requires ≥ 3 commits, a dominant sha different from the creation sha and not a merge/reformat/move (fd's `construct_config` "81% from a dependabot merge" and ripgrep's `dir.rs` "dominant = deps bump" both drop out); blame is capped to candidates so a 5k-file repo does not pay 90 s.

**Folds into.** A new reason on existing hotspots; `--json` gains `files[].functions[].blame {distinct_commits, per_100_lines, share, dominant_sha, dominant_share, rewritten}`; home for P23's blame-backed link and P27's commit-size clause. Later, behind `score_weight`, the churn × complexity term may be sharpened by attributing churn to the complex unit.

**Cost.** Moderate: one blame per candidate file, ~18 ms/file sequential; parallelize per file; t-4569 can key output by blob hash + HEAD.

**Rules.**
- Blame runs only for files that meet `min_file_commits` or are among `max_files` top provisional scores; skip files over `max_lines`; a blame exceeding `timeout_secs` yields no reason and a parse-warning-style note.
- Every sha in the boundary set, every merge commit, every reformat/move commit, and every sha in the ignore-revs sources maps to the single pseudo-commit `before-window`, which never counts as distinct.
- A unit's distinct set is the set of non-pseudo shas over its full line range (nested units included).
- Report a unit when lines ≥ `min_lines` and distinct ≥ `min_distinct_commits` and share ≥ `min_share_of_file_commits`; at most `reason_max_functions` per file, ordered by distinct desc then lines desc; never report a unit nested inside a reported unit.
- `rewritten` is an annotation on a reported unit only, never a standalone reason.
- The mega-commit clause prints only when the dominant sha meets `mega_commit_files` or `mega_commit_lines`, sizes measured over Source rows of the numstat log.
- The reason must name each function, its line range, K, M, and (when ≥ 0.5) the dominant share and sha.
- Percentile of the file's max share feeds no score in v1.

### P22 write-then-rewrite (tier B)

**Measures.** Per Source file born inside the window: `created_lines` (adds in the `--diff-filter=A` commit), `peak_lines` (max running adds−dels across the file's window commits, in author-time order), `rewrite_deletions` = numstat deletions to the path in non-fix, non-reformat commits with author time within `window_days` of birth, `rewrite_ratio` = rewrite_deletions / peak_lines, `rewrite_commits`, `heaviest_commit` (sha, +adds/−dels); separately `fix_deletions` and `fix_commits` inside the same window from fix-subject commits. With blame (P26): the unit whose current lines mostly come from the heaviest rewrite commit. Unit: file; reason only.

**Why it matters for LLM code.** kanspec `src/cmd/proposal.rs`: born 144 lines in 988d9b5 (08-31 03:35), then +80/−13 at 07:54, +777/−69 at 23:45 (f739113), +167 at 23:56, and +237/−258 in 9f2f0f7 (09-02 07:51 'Review pass: fix eleven defects and remove ~780 lines of duplication'): 418 lines deleted across 13 commits inside 7 days = 2.9× its birth size; today 78% of `close` (lines 687-944) still comes from the day-one rewrite f739113. Eight kanspec src files score ≥ 1.4 (server.rs 3.0, done.rs 2.3, comment.rs 2.2, doctor.rs 1.6). Prototype, files born in window with ≥ 50 lines, count ≥ 1.0 / ≥ 0.5 / max: kanspec 46: 11 / 21 / 3.01 (median 0.22); scry 8: 0 / 0 / 0.27; ripgrep 16: 0 / 0 / 0.15; fd 4: 0 / 0 / 0.27; click 0 born-in-window src files; hono 19: 0 / 0 / 0.34. Zero human files ≥ 0.5 across 39 baseline files. Defects found: server.rs reads 3.01 on birth adds although it grew +646 four hours after birth (32e367a) before the review cut 174 (hence the peak denominator); all four top hits are one 49-file cleanup commit 9f2f0f7, which is itself a fix commit (hence the fix split). Separation is "weak": it detects a land-then-cleanup session pattern, and scry (also LLM-written) never trips. The actionable payload is "this file's surviving text was never read as a diff".

**Detection.**
- Replace `--name-only` with `--numstat` in the existing history call: `git -c core.quotePath=off log --no-merges --no-renames --numstat --since=<window> --format=%x01%H%x02%an%x02%ae%x02%at%x02%s`; rows with `-` (binary) skipped. Use `%at`, not `%ct`.
- Second call: `git log --no-merges --no-renames --diff-filter=A --name-only --since=<window> --format=%x01%H`; a path added more than once takes the oldest.
- Drop births whose commit is in the boundary set (grafts, shallow, roots) unless `root_births = true`; a true root (max-parents=0, not in `.git/shallow`) has a knowable size but is an import, not a write, by default.
- Denominator = peak size inside the window (running adds−dels), knob `denominator = "peak" | "birth"`.
- Rewrite deletions exclude reformat/move commits and, with `exclude_fix_commits`, fix-subject commits, which go to the separate `fixed within N days of landing` phrase.
- Blame join (P26): for the heaviest rewrite commit, the unit with the most lines attributed to it and its share of that unit.

**Reason line.**
Template: `born at C lines (<sha>, <date>) and rewritten within D days: R lines deleted across K commits, heaviest <sha> (+a/-d)[; P% of <fn> (lines a-b) is from the day-N rewrite <sha>][; fixed within D days of landing by <sha> '<subject>' (+a/-d)]`
Example under these rules (9f2f0f7 is a fix commit and moves to the trailing clause): `born at 144 lines (988d9b5, 08-31 03:35) and rewritten within 7 days, heaviest f739113 (+777/-69); 78% of close (lines 687-944) is from the day-one rewrite f739113; fixed within 7 days of landing by 9f2f0f7 'Review pass: fix eleven defects…' (+237/-258)`

**Knobs.**
```
[history.rewrite]
window_days = 7
min_created_lines = 50
min_ratio = 0.5
denominator = "peak"
ignore_reformat_commits = true
exclude_fix_commits = true
skip_boundary_births = true
root_births = false
reason_ratio_percentile = 0.9
```

**Objection and answer.** It measures workflow and repo age, not the file: squash-merge repos hide the iteration on the PR branch (hono max 0.34, its only 1.0 a benchmark file moved in one commit c43c53f, 390/390); in a trunk repo it collapses into churn (proposal.rs's 13 commits already rank it), and the heaviest "rewrite" 9f2f0f7 is the remediation the tool exists to recommend. Answer, folded in: no score slot — a reason attached only to files already ranked by churn; fix-subject commits are split out into their own phrase so a fix is never scored as a rewrite; move/reformat commits (c43c53f) are excluded by the reformat rule; peak denominator kills the server.rs inflation; boundary/graft births (kanspec's two roots, click's 7, fd's 4) are skipped by consulting all of `.git/shallow` and every root, not "the commit has no parent"; the blame clause is what makes the reason actionable ("review `close` whole"). Squash-merge invisibility is accepted and documented: the pass describes trunk-based agent sessions. Note the default `root_births = false` drops the kanspec headline in that repo; the maintainer flips it for repos whose root is a real first commit.

**Folds into.** A reason on churn-ranked hotspots; `--json` `files[].history.rewrite {created_lines, peak_lines, rewrite_deletions, rewrite_ratio, rewrite_commits, heaviest, fix_deletions, fix_commits, day_one_function}`. The numstat switch is shared infrastructure for P23/P26.

**Cost.** Cheap: numstat replaces name-only in the same log call; one extra `--diff-filter=A` log.

**Rules.**
- The history pass parses numstat; existing commit counts, fix counts, authors and co-change are computed from the same rows unchanged.
- Birth = oldest `--diff-filter=A` commit for the path; skip when in the boundary set (unless `root_births`), when `created_lines < min_created_lines`, or when the birth commit is a reformat/move commit.
- Rewrite window = commits with `at - birth_at <= window_days * 86400`, excluding reformat/move commits and (when `exclude_fix_commits`) fix-subject commits; deletions summed per path.
- Ratio uses `peak_lines` (running adds−dels over the window, initial = created_lines) when `denominator = "peak"`.
- Emit the reason only when `rewrite_ratio >= min_ratio` and the ratio is at or above `reason_ratio_percentile` among born-in-window files, and only on files already in the hotspot list.
- The `fixed within D days of landing` clause is independent: prints whenever any non-mass fix-subject commit deletes ≥ 1 line inside the window, with sha, subject and +a/−d.
- The blame clause prints only when P26 attributes ≥ 50% of some unit's lines to the heaviest rewrite commit; name the unit, range and share.
- Ratio contributes to no score.

### P23 fix cadence: fix-of-fix chains and fix latency (tier B)

**Measures.** Per file: fix commits (existing `subject_is_fix`, plus 'Revert…' subjects), each changing ≥ `min_fix_lines` in the file (numstat), excluding mass fix commits; a **chain** is a run of fixes each within `max_gap_days` of the previous *and* linked by continuity: (a) blame-backed — the later fix's diff removes lines the earlier fix added, (b) same author, (c) subject shares a content word/symbol with the previous fix or with the commit being fixed, or (d) subject starts with 'Revert'. Signal = longest chain length (percentile); `chain_span`, chain commits (sha, subject, Δt, link kind). Landing latency is folded into the same pass as a phrase: for each addition ≥ `min_added_lines`, count of linked fixes within `landing_window_days`. With blame: the unit holding most lines from chain commits. Unit: file.

**Why it matters for LLM code.** kanspec `src/out.rs`: c94cfb9 08-31 09:22 'fix: pad the id column by VISIBLE width, so colour no longer jams id into title' → 4beebf8 10:46 'Fix the two defects the re-audit flagged as pre-dogfooding blockers' → 1be7ac1 11:20 'fix: measure display columns, and let a fix speak the binary the user typed' → 9f2f0f7 09-02 07:51 → a65012b 09-03 03:42; `git show 1be7ac1` deletes the visible_len lines c94cfb9 added — a real fix-of-fix. Prototype with > 25-file fix commits excluded, files with chain ≥ 3 / ≥ 4 / max: kanspec 6/56 / 2 / 4; scry 0/9 / 0 / 2; ripgrep 0/80 / 0 / 1; fd 1/20 / 0 / 3; click 3/17 / 0 / 3; hono 5/221 / 1 / 4 (client.ts, 13 fixes of 19 commits, conventional-commit style). Without the exclusion kanspec has 25 files ≥ 3, all inflated by two mega commits. Repo fix share: scry 0.22, kanspec 0.29, ripgrep 0.19, fd 0.32, click 0.27, hono 0.51. Gap-only top-5 were 2 real / 3 noise: derive.rs chain 4 in 3h is three unrelated audit findings (d5fd078 +198, d7caa2a +58, 8f67fcb +256, nothing re-fixed); flow.rs chain 3 ends in a65012b 'ctx: no process globals … fix lines are respelled', a fix-word false positive; hono client.ts's chain of 4 in 24.6h is four independent PRs by four authors merged in one sweep. The blame-backed link is exactly what separated out.rs from derive.rs. Latency alone does not survive: kanspec 17/25 and scry 7/7 files fixed ≤ 8h vs click 2/13, hono 3/77, ripgrep 0/16 (min 141h), fd 0/4 (min 25h) — but human fastest first-fixes are minutes (hono d2737ab 14:14 → b89dae0 'fix the type errors' 14:25; click 27b3ee2 11:32 → 1ac08db 'Fix ruff E501 line-too-long' 11:36), so it is a phrase, not a signal.

**Detection.**
- Pure reuse of the numstat log: per file, fix events (author time, sha, author, subject, adds/dels in this file); drop mass fix commits and fixes with adds+dels < `min_fix_lines`; dedupe identical subjects (backport batches).
- Sort by time; candidate links = consecutive fixes with gap ≤ `max_gap_days`; a link holds if any enabled `link` mode matches. Subject mode: tokenize subjects (lowercase, drop stop words and the fix list), match on a shared token ≥ 4 chars or a shared identifier-looking token.
- Blame mode, only for candidate links: `git diff-tree -p --unified=0 --no-color <sha>^ <sha> -- <path>` → deleted ranges in the parent; `git blame -w --line-porcelain -L a,b <sha>^ -- <path>` per range; link holds if any deleted line's sha is the previous fix. Bounded: runs only for files with a gap-chain ≥ `min_chain`.
- Landing phrase: additions ≥ `min_added_lines` (numstat adds in this file, including the birth commit); count linked or same-author fixes within `landing_window_days` whose time > addition time; ignore a fix that is the addition itself.
- Function naming via P26's join: the unit with the most current lines from chain commits.

**Reason line.**
Template: `K fix commits within Hh, each undoing part of the last (<date> -> <date>): <sha> '<subject…>' -> <sha> (+Δ[, removes lines added by <sha>]) -> …; <fn> (lines a-b) holds most of the chain's lines`
Example: `4 fix commits within 62h, each undoing part of the last: c94cfb9 'pad the id column by VISIBLE width…' -> 4beebf8 (+1h24) -> 1be7ac1 (+34m, removes visible_len lines added by c94cfb9) -> …; write (lines 174-215) holds most of the chain's lines`
Landing phrase, appended to a file's existing churn/fix reasons: `a 413-line addition (6a674af, 08-31 04:14) was fixed 4.8h later by 5721c1c 'illegal-transition refusals name a command clap actually accepts'; N of its M additions over 50 lines were fixed within 7 days`

**Knobs.**
```
[history.fixchain]
max_gap_days = 2
min_chain = 3
link = ["blame", "author", "subject", "revert"]
min_fix_lines = 5
exclude_commits_over = 25           # defaults to history.max_cochange_commit_size
max_fix_share = 0.6                 # per file; skip files above it
count_reverts_as_fix = true
dedupe_subjects = true
reason_min_chain = 3
[history.fixchain.landing]
min_added_lines = 50
window_days = 7
min_fixes = 2
```

**Objection and answer.** Gap-based chains measure batching cadence: hono client.ts's 4-in-24.6h is four authors' PRs merged in a sweep, while hono's real fix-of-fix (0432f81 → 50b8788) is 13 months apart; chain length tracks fix count, which `fix_commits` already ranks; the latency baseline is wrong because lint/type follow-ups land in minutes. Answer, folded in: a link requires continuity (blame-backed removal, same author, shared subject content, or Revert) — the merge sweep fails all four; `min_fix_lines` drops lint follow-ups; `max_fix_share = 0.6` per file drops conventional-commit files where 'fix' is the type; the 13-month case is out of scope by design (a chain is about stabilising by trial, which is a short-horizon event) and stays visible through `fix_commits`; latency is demoted to a phrase on files already ranked, requiring `min_fixes` linked fixes rather than one fast follow-up.

**Folds into.** A reason on existing hotspots; longest chain as `--json` `files[].history.fixchain {longest, span_hours, commits[{sha, subject, dt_hours, link}], function}` and `history.landing {additions[{sha, adds, first_fix_sha, hours, fixes_in_window}]}`. No score term; the existing `fixes` slot already carries the count.

**Cost.** Cheap for gap+author+subject links; blame links cost one `diff-tree` plus one range-blame per candidate link, only on files with a candidate chain.

**Rules.**
- Fix events per file: `subject_is_fix` or (when enabled) subject starting with `Revert`; drop commits over `exclude_commits_over` paths; drop fixes with adds+dels in this file < `min_fix_lines`; drop the file when fixes/commits > `max_fix_share`; dedupe identical subjects keeping the earliest.
- Two consecutive fixes are linked iff gap ≤ `max_gap_days * 86400` and at least one enabled link mode holds; `revert` holds when the later subject starts with `Revert`; `author` when `%ae` matches; `subject` when the tokenized subjects share a content token; `blame` when a deleted line in the later fix's diff of this path blames to the earlier fix at the later fix's parent.
- Blame-mode checks run only when the gap-only chain for the file already reaches `min_chain`.
- Chain = maximal run of linked fixes; report the longest; emit the reason when length ≥ `reason_min_chain`, listing every commit with sha, truncated subject (first), Δt from previous, and the link kind when it is `blame`.
- Landing phrase: for each addition ≥ `min_added_lines`, count fixes within `window_days` that are linked (any mode) to the addition; print only when count ≥ `min_fixes` and only on files already in the hotspot list; never print a fix that is the addition itself.
- Function clause prints only when P26 blame is available; name the unit with the most current lines from chain commits and its range.
- Longest chain is percentile-ranked for JSON but contributes to no score.

### P24 accretion-only files (tier C)
Rejected: deletion ratio is dominated by repo age and `--no-renames` moves (click all-adds, hono 43% and ripgrep 38% at ≤ 0.1 beside kanspec's 60%), and its surviving hits are declarative tables or write-once code (kanspec model.rs/cli.rs gated out, ripgrep flags/parse.rs human and fine); the corroborators it leans on (logic density, clone_ratio) do all the discriminating work.

### P25 shotgun surgery and divergent change (tier C)
Rejected: commit-subject conventions, not code shape, decide the numbers (kanspec proposal.rs 15 topics in 16 commits; hono topics = fix/feat/chore; click 0 topics), under its own knobs kanspec yields 0 wide commits, and pairwise co-change already is the shotgun-surgery signal.

### P27 mega-commit provenance (tier C)
Rejected as a standalone pass and multiplier: `exclude_root_commit` suppresses its own headline (988d9b5 is a kanspec root), ripgrep's 082245d supplies 91% of hiargs.rs and is exactly as "unreviewed" as kanspec's integrations yet not a problem, and lines untouched since a big landing are the lines that never needed a fix; its one durable fact — the dominant commit's file/line size — ships as the clause inside P26's reason (`mega_commit_files` / `mega_commit_lines`).

## defensive idioms and error handling
### P28 fallback density (tier B)

**Measures.** Unit = function (same units as metrics). Primary: `parse_defaults` = count of sites where a literal default (`""`, `0`, `false`, `()`, `{}`, `[]`, `None`, `String::new()`, `Vec::new()`, `Default::default()`) is applied to the result of a fallible transform — the receiver chain contains `parse`, `split`, `split_whitespace`, `splitn`, `split_once`, `lines`, `next`, `strip_prefix`, `from_utf8`, or `get` on a value that itself came from one of those. Secondary, reported but not flagged on: `fallbacks` = all value-swallowing defaults (`unwrap_or*`, `.ok()` feeding `?`/`unwrap_or*`, Python `.get(k, lit)`/`getattr(o, k, lit)`/`x or lit`, TS `?? lit`) and `density` = fallbacks / top-level statements, percentile-ranked over functions with `>= min_statements`. Per file: sum of parse_defaults and the max-density function.

**Why it matters for LLM code.** Prototype: fallbacks per function scry 0.42 (13.9/kloc), kanspec 0.14 (5.0/kloc), fd 0.12 (5.3/kloc), ripgrep 0.02 (0.9/kloc), click 0.12 (5.3/kloc), hono 0.16 without `?.` (4.1/kloc; 0.43 with `?.`). Density p95 over functions >= 5 statements: scry 0.40 (n=49), kanspec 0.20 (391), fd 0.17 (28), ripgrep 0.00 (565), click 0.20 (136), hono 0.33 (170). Raw density separates LLM Rust from ripgrep only; fd/click/hono match kanspec. What separates is the parse_defaults subset: parse_defaults in the top-3 functions scry 5+1, kanspec 2+0+3, fd 0, ripgrep 0, click 0, hono 0, and every real hit was a default applied to the output of split/next/parse. Hits: `scry/src/history/mod.rs:64-151 collect` (density 0.92, 5 parse-defaults at 92-98: `fields.next().unwrap_or("")` x3, `parse().ok().unwrap_or(0)`) — a malformed git log line becomes author "" at timestamp 0 silently. `kanspec/src/paths.rs:39-97 discover` (0.58, 3 parse-defaults at 47-49: `lines.next().unwrap_or_default()` on rev-parse output) — a short reply becomes an empty git_dir. Noise when flagged on density alone: `kanspec/src/ctx.rs:48-92 detect` (config-with-defaults), `kanspec/src/cmd/comment.rs:98-179 fold_threads` (Option<String> field folding), fd `print_entry_colorized`, hono `compress`, click `password_option`.

**Detection.** Node kinds verified with `scry ast`:
- Rust: `call_expression` whose `function` is `field_expression` with `field: field_identifier` in `methods_rust`; `.ok()` counted only when its parent is `try_expression` or it is the receiver of an `unwrap_or*` call (else it double-counts). Default literal = argument is `integer_literal` / `string_literal` / `boolean_literal` / `unit_expression`, or `unwrap_or_default`/`or_default`, or a call to `String::new` / `Vec::new` / `Default::default` (`scoped_identifier` text). Receiver chain: walk `value` of `field_expression`, `function` of `call_expression`/`generic_function`, and the child of `try_expression`; a chain containing a `field_identifier` in `parse_methods` makes the site a parse_default. `fields.next().unwrap_or("")` parses as `call_expression(function: field_expression(value: call_expression(function: field_expression(field: next)), field: unwrap_or), arguments: (string_literal))`.
- Python: `call` with `function: attribute(attribute: identifier in python_default_getters)` and `argument_list` with 2 named children, 2nd in {`dictionary`, `list`, `none`, `string`, `integer`, `false`}; `call` to identifier `getattr` with 3 args; `boolean_operator` with operator `or` and `right` in those kinds. Parse_default when the object chain contains a `call` to `attribute` in {`split`, `partition`, `get`, `pop`} whose own object chain contains `split`/`partition`/`splitlines`/`readline`, or `int`/`float` wrapping. `x or []` inside `__init__`/`__post_init__` is never counted.
- TypeScript: `binary_expression` whose operator token is `??` and `right` in {`object`, `array`, `string`, `number`, `null`, `undefined`, `true`, `false`}; `||` only when `ts_count_or = true`; `member_expression`/`call_expression`/`subscript_expression` with an `optional_chain` field only when `count_optional_chain = true` (default false — it multiplied hono density 2.7x with zero real problems). Parse_default when the left chain contains `property_identifier` in {`split`, `match`, `exec`, `parseInt`, `parseFloat`, `JSON.parse`, `shift`, `pop`}.
- Units: `function_item` / `function_definition` / `function_declaration` / `method_definition` / `arrow_function` bound via `variable_declarator`. Statements = named children of `block` / `statement_block`, dropping a leading docstring `expression_statement` in Python. Skip `#[cfg(test)]` `mod_item` bodies. No git, no graph.

**Reason line.** `collect defaults 5 parsed values to ""/0 (lines 92-98: fields.next().unwrap_or("") at 92, 93, 94; parse().ok().unwrap_or(0) at 96, 98): a malformed input is silently accepted`. When density is also in the tail append `; 0.92 fallbacks per statement, 97th percentile`.

**Knobs.**
```toml
[fallback]
methods_rust = ["unwrap_or", "unwrap_or_default", "unwrap_or_else", "or_default"]
parse_methods_rust = ["parse", "split", "split_whitespace", "splitn", "split_once", "lines", "next", "strip_prefix", "from_utf8", "get"]
python_default_getters = ["get", "getattr", "pop", "setdefault"]
ts_operators = ["??"]
ts_count_or = false
count_optional_chain = false
min_parse_defaults = 2        # sites in one function to emit the reason
min_statements = 5            # for the density percentile only
flag_percentile = 90          # density tail, secondary reason only
exempt_fn_patterns = ["^default", "^from_env", "^with_", "^parse_args", "^from_matches", "^configure", "^__init__$"]
weight = 0.0                  # extra reason by default; opt-in score weight
```

**Objection and answer.** Objection: without types tree-sitter cannot separate defaulting an absent option (correct) from defaulting a failed operation (the defect), the headline `comment.rs:119` example is the benign kind, kanspec 0.17 vs fd 0.15 is noise, and TS `?.` would make the score a style score. Answer, folded into the design: the flag condition is the parse_defaults subset only (`>= 2` sites in one function whose receiver chain contains a syntactically fallible transform), which is the shape every real hit had and no human corpus put in its top hits; `unwrap_or` on a struct-field Option has no call in its chain and is never a parse_default; `?.`, `||`, `.get(k, 0)`, and `or []` in constructors are off or excluded by default; density survives only as a secondary percentile phrase, and there is no score weight unless the maintainer turns one on.

**Folds into.** Extra reason on hotspots plus a `fallback` block per function in `--json` (`parse_defaults`, `fallbacks`, `density`, sites with lines). No new section; `weight` defaults to 0.

**Cost.** Cheap: one AST walk per function during metrics.

**Rules.**
- Count a fallback site only at the outermost default call; `x.parse().ok().unwrap_or(0)` is one site, not two.
- A site is a parse_default iff its receiver chain contains a `parse_methods_*` name AND the default argument is a literal/empty constructor.
- Emit the reason iff parse_defaults >= `min_parse_defaults` and the enclosing unit name matches none of `exempt_fn_patterns`; the reason lists every parse_default line.
- Density is computed for units with >= `min_statements`; append the percentile phrase iff density is at or above `flag_percentile`; never emit a reason on density alone.
- `.ok()` counts only as parent `try_expression` or as receiver of a `methods_rust` call.
- Never count `optional_chain` unless `count_optional_chain`; never count `||` unless `ts_count_or`; never count `boolean_operator or` inside `__init__`.
- Skip units inside `#[cfg(test)]` modules and test files.
- JSON: `fallback: { parse_defaults, fallbacks, statements, density, density_pct, sites: [{line, kind, default}] }` per function.

### P29 error erasure and passive catch-all handlers (tier B)

**Measures.** Unit = function. Rust and TypeScript only; Python is t-eccd's per-file except detector, consumed not re-implemented. `erasures` = sites of four shapes: (a) `Err(_)` / unused-`Err(e)` arm whose value is a non-error literal or constructor (`true`, `None`, `Ok(())`, `Ok(Utc::now())`, `break`, `continue`, `return` with such a value); (b) let-else `let Ok(x) = .. else { return Ok(())/None/… }`; (c) `.ok()?` inside a `-> Option` function that also returns `Some(<computed>)` on another path; (d) `let _ =` on a non-write call, clustered >= `cluster_min` in one block; TS: `catch_clause` with empty or log-only body. `documented` = a comment within `comment_lines` above the site or on the function matches `intentional_words`. Score per function = sum(site weight) where documented sites weigh `documented_weight`; ranked by repo percentile, tiebroken by deps' existing file fan-in (never by name-matched call counts).

**Why it matters for LLM code.** Prototype after the macro allowlist: erasures per kloc scry 1.93 (6 sites; raw `let _ =` is 21 of which 20 are `writeln!`, so the 6.77/kloc headline was output macros), kanspec 1.76 (60; 30 of 60 raw `let _ =` are macros), fd 2.18 (11), ripgrep 0.38 (19), click 1.41 (18, all except: pass), hono 0.54 (14, all empty catch). Functions with >= 2 erasures: scry 1/103 (1.0%), kanspec 14/1256 (1.1%), fd 2/218 (0.9%), ripgrep 2/2880 (0.1%), click 2/584 (0.3%), hono 0/655. Option-returning functions containing `.ok()?`: scry 2/9, kanspec 13/67 (19%), fd 3/17 (18%), ripgrep 5/248 (2%). Kind mix in kanspec: `let _ =` call 23, `.ok()?` 17, if-let-Ok-without-else 16, `Err(_)=>default` 4. Density does not separate LLM from human; documentation does: every intentional kanspec hit carried a "best-effort" comment (`kanspec/src/lock.rs:145-157 write_note`, `store.rs:985-992 fsync_dir`, `cmd/flow.rs:597-604 unmake`) and every undocumented multi-site hit was a caller-facing ambiguity: `kanspec/src/git.rs:652-671 fetch_age` (3x `.ok()?`, 3 callers: missing FETCH_HEAD, git failure and clock error all become None), `git.rs:609-623 ahead_behind` (3x `.ok()?`, 6 callers: 'no upstream' and 'git failed' identical to every caller), `store.rs:261-311 parse_rules` `Err(_) => break`. Shape (a) examples: `kanspec/src/scan.rs:958 Err(_) => true` in glob_matches_anything, `ctx.rs:317 Err(_) => Ok(Utc::now())`. Alternate's evidence: `scry/src/main.rs:140-143 Err(e) => { eprintln!(...); None }` degrades the whole scan silently; `server.rs:739 Err(_) => return` abandons the watch loop.

**Detection.** Node kinds verified with `scry ast`:
- (a) `match_arm` whose `pattern` is `match_pattern > tuple_struct_pattern(type: identifier "Err")` with zero named children after the type (`Err(_)`) or one `identifier` not referenced in the arm's `value`; value (or last statement of its `block`) is `boolean_literal`, `identifier` `None`, `call_expression` with function `Ok`/`Some`/`Vec::new`/`Default::default`, `break_expression`, `continue_expression`, or `return_expression` whose child is one of those or absent. Same test on `if_expression` with `let_condition` pattern `Ok(..)` and no `alternative` (the alternate's "broad + passive" classification is subsumed here: broad = wildcard/unused binding, passive = default-yielding value).
- (b) `let_declaration` with `pattern: tuple_struct_pattern(type: Ok)` and an `alternative` block whose last child is `return_expression` with a shape-(a) value.
- (c) `try_expression` whose child is `call_expression > field_expression(field: "ok")`, inside a `function_item` whose `return_type` text starts with `Option`; count only if the body also contains a `Some(...)` call or tail whose argument is not a literal.
- (d) `let_declaration` with no `pattern` field (that is how `let _ =` parses) whose `value` is `call_expression`/`await_expression` and whose callee `field_identifier`/`identifier` does not match `ignore_callees`; `macro_invocation` whose `macro` identifier is not in `ignore_macros`. Count only when >= `cluster_min` such declarations share one `block`.
- TS: `try_statement` with `handler: catch_clause` whose `body: statement_block` is empty or whose every statement is `expression_statement > call_expression(function: member_expression(object: identifier in log_names))`; no `parameter` field is the `catch {}` form. Comment `// Do nothing` on the same line counts as documented.
- Documented: scan `line_comment`/`block_comment` siblings within `comment_lines` above the site and the doc comment of the unit for a case-insensitive whole-word match of `intentional_words`.
- Exempt: units inside `impl_item` whose `trait` field text is `Drop`; unit names matching `exempt_fn_patterns`; `#[cfg(test)]` `mod_item` bodies; test files.
- Fan-in: reuse `deps` file fan-in of the containing file as a log2(1+fan_in) multiplier only when `fan_in_weight = true`; no call-site name matching.

**Reason line.** `ahead_behind erases 3 failures into None (lines 609-623: .ok()? at 613, 620, 621) with no comment saying so; callers cannot tell 'no upstream' from 'git failed'`. Documented form: `write_note ignores 4 write results (lines 153-156, documented best-effort)`. Shape (a): `glob_matches_anything answers true when the glob fails to compile (line 958: Err(_) => true)`.

**Knobs.**
```toml
[erasure]
languages = ["rust", "typescript"]
ignore_macros = ["writeln", "write", "println", "eprintln", "debug", "trace", "info", "warn", "error"]
ignore_callees = ["write", "write_all", "flush", "send", "raise", "shutdown"]
cluster_min = 3                 # let _ = sites per block before shape (d) counts
min_erasures = 2                # sites per function before the reason is emitted
exempt_fn_patterns = ["^drop$", "^shutdown", "^cleanup", "^unmake", "^rollback", "^remove_", "^best_effort"]
log_names = ["console", "logger", "log"]
intentional_words = ["best-effort", "best effort", "ignore", "intentional", "don't care", "cleanup", "do nothing"]
comment_lines = 3
documented_weight = 0.25
fan_in_weight = true
flag_percentile = 90
weight = 0.0                    # extra reason and section by default
```

**Objection and answer.** Objection: name-matched fan-in is unsound (methods named `run`/`get`/`next` collide, receiver calls resolve to zero); stripped of it the finding is a ruff/eslint lint in two languages and duplicates t-eccd; and the detector matches deliberate human idioms exactly — click `_compat.py:77-172` capability probes, fd `command.rs:48-54` EPIPE-tolerant `let _ = stdout.write_all(..)`, ripgrep `gitignore.rs:611-646` optional-file loaders with `.ok()?`. Answer, folded in: fan-in is deps' already-resolved file fan-in, never call-site matching; Python is delegated to t-eccd so click is never re-reported; shape (d) excludes write/flush/send callees so fd's `write_all` cluster is out; shape (c) requires a computed `Some` on another path so ripgrep's read-file-or-None loaders (whose only Some is the read result) are out; and documented sites are kept but weighted 0.25 and named "documented" in the reason, so the pass never tells an agent to fix `write_note`.

**Folds into.** Extra reason on hotspots; a new report section "erasures" listing every function with >= `min_erasures` undocumented sites (like the clone-pair section); `erasure` block per function in `--json`; `weight` 0 by default with the knob available once the maintainer sees it rank sensibly.

**Cost.** Cheap: one AST walk per unit plus a comment lookup per site; no git.

**Rules.**
- A site belongs to exactly one shape (a)-(d)/TS-catch; shapes are evaluated in that order and the first match wins.
- Shape (a) requires the `Err` binding to be `_`, absent, or unreferenced in the arm value; a referenced `e` (formatted into an error, logged then re-raised) is not an erasure.
- Shape (c) counts only in `-> Option` units that contain a `Some(x)` call or tail where `x` is not a literal.
- Shape (d) counts only when >= `cluster_min` qualifying `let _ =` declarations share one block, and each callee is outside `ignore_callees`/`ignore_macros`.
- Documented sites weigh `documented_weight`; the reason says "documented <matched word>" instead of "with no comment saying so".
- Emit the reason iff the unit has >= `min_erasures` sites and is not exempt; list every site line and shape.
- Fan-in multiplier uses `deps.fan_in` of the containing file; never count call sites by name.
- Python: read t-eccd's per-file swallowed-except counts and do nothing else; TS: only the catch shape.
- Skip `impl Drop` bodies, `#[cfg(test)]` modules, test files.
- JSON: `erasure: { sites: [{line, shape, documented}], score, pct }` per function; section `erasures: [{file, function, lines, sites, undocumented}]`.

### P30 guard prologue (tier C)
Rejected: the metric penalises the flattened early-return form that cognitive complexity, pylint R1705/R1720 and eslint no-else-return recommend; the prototype found human corpora have more leading guard chains (click 0.9%, hono 1.1%) than kanspec (0.5%), every inspected hit was deliberate dispatch, and the one bad case (`fix_closed_agree` let-else returning `Ok(())` on a parse failure) is P29 shape (b).

### P31 infallible Result ceremony (tier C)
Rejected: infallible share is 1.4% in kanspec vs 2.1% in ripgrep with no kanspec file reaching a 20% share; the two non-trivial hits (`read_comments`, `fix_closed_agree`) are P29 erasures wearing a Result signature, the remaining case (`completions`) is clippy::unnecessary_wraps per function, and the Python/TS variants need callee types tree-sitter cannot see.

### P32 guard accretion (history) (tier C)
Rejected: the direction is reversed (mean accretion fd 0.38, ripgrep 0.26 vs kanspec 0.11, scry 0.00) because human bodies get rewritten under a founding-commit fn line while squash-flat LLM repos (36% of kanspec functions single-commit) yield "no history"; every top kanspec hit was a wholesale rewrite by one later commit, indistinguishable from bolted-on guards without per-hunk diff analysis, and it depends on P30 for line classification. If ever revisited, gate on >= 5 distinct blame commits per file and use `git log -L`/`-G` diff-based commit lists per hotspot, not blame.

## Name-resolved call graph (callers, callees, arguments)

All five proposals in this cluster were prototyped on a real tree-sitter index (`proto/name_resolved_call_graph_callers_callees_arguments_/index.py`) and all five are rejected. The only durable output is that name-keyed function/caller index itself; build it as infrastructure only if a proposal outside this cluster needs it, not on the strength of anything below.

### P33 single-caller helper ladders (tier C)
Rejected: single-caller share does not separate LLM from human code (scry 60%, kanspec 36%, fd 35%, click 25% vs ripgrep 13% is library-vs-binary layout, and ripgrep's `pub(crate)` fns are mislabeled public), ladder depth >= 3 is absent in kanspec (0 chains), and every top hit (scry metrics/mod.rs, fd main.rs, kanspec cmd/proposal.rs) is a readable stepdown decomposition, which is the refactor scry's own cognitive > 15 reason prescribes, so the reason text would tell an agent to inline well-named 3-30 line functions.

### P34 pass-through delegation layers (tier C)
Rejected: the signal runs the wrong way (pass-through rate scry 0%, kanspec 2.1% vs ripgrep 3.0%, click 3.8%, hono 5.2%), the headline hits are named partial applications (kanspec derive.rs:570-577 unresolved/answered), public facades (hono request.ts:147) and newtype delegation (ripgrep walk.rs DirEntry) that no reviewer would remove, and forwarding chains need module-qualified callee resolution the deps pass does not provide (name-resolved chains were all collisions such as `join -> join`).

### P35 redundant caller guard (tier C)
Rejected: zero true positives once read (kanspec gh.rs:277 vs :211 is the specific-before-generic error idiom; comment.rs:578 vs decision.rs:79 are different variables sharing a name), yield after the required filters is 0-1 pairs per 1k units with none in scry or fd, shadowed-hard-error count is 0 in every corpus, and deciding that the callee guards the same value needs data flow through struct fields and receivers that tree-sitter cannot give; the one narrow survivable form (caller soft-exits on a condition its same-file private callee raises on) belongs as a cross-function case inside t-eccd, not a pass.

### P36 optional parameter creep (tier C)
Rejected: the premise is inverted on the evidence (mean optional-parameter ratio scry 0.009, kanspec 0.020 vs fd 0.061, ripgrep 0.054; kanspec's 348 `Option<` are fields and return types), the headline example derive.rs:1906 `anchor` is inside `#[cfg(test)]` and excluded by the proposal's own knob, uniform call-site flags in non-test code are 0 in scry/kanspec/fd and 3 in ripgrep (twin constructors, API symmetry), and the 'always omitted' class that produced 15 of 18 human-corpus flags is a Python/TS default working as intended; if ever revisited it should be the JetBrains-style 'parameter always receives the same value' inspection on private, unique-named, >= 3-caller functions, Rust/TS first.

### P37 argument order contradicts parameter names (tier C)
Rejected: zero strict hits in ~1,800 arity-matched calls across six corpora and all 7 relaxed hits intentional (ripgrep dir.rs `create_gitignore(dir, dir, ..)`, hono `insertIntoHead` naming), pylint W1114 and SonarSource S2234 already ship the rule for Python/JS/TS, and rustc/tsc catch every swap where types differ; the only place it has a non-zero base rate is t-4569's diff-scoped PR mode (a diff that reorders a unique function's parameters, checked against its unchanged call sites), and it should be filed there if at all.

## Module and type cohesion, speculative abstractions

Six proposals were prototyped against all six corpora. Five fell to the same finding: the shape they flag is an idiom (config bags, registries, parsers, return records, extension traits), occurs at the same rate in ripgrep/click/hono as in kanspec/scry, and the "fix" is one no maintainer would apply. One survives as a wording extension of an existing reason, not as a new score term.

### P41 low-cohesion grab-bag modules (tier B)

**Measures**
Unit: file. For each pub/exported symbol, the set of *other* discovered non-test files that import it by name. `clusters` = connected components over symbols, two symbols joined when they share an importer. `cohesion` = mean pairwise Jaccard of importer sets. `importers` = fan-in (already computed). Report when `pub_symbols >= min_pub_symbols`, `importers >= min_importers`, `clusters >= min_clusters`, `cohesion <= max_jaccard`. Rank by `fan_in x clusters`. Not scored.

**Why it matters for LLM code**
It is not an LLM tell; the prototype found human corpora hit at the same rate. It earns a place because it gives the existing fan-in reason a direction. Proposal evidence: "kanspec/src/cli.rs exports 55 referenced pub symbols to 27 importers with mean Jaccard 0.06 and 12 disjoint importer clusters (each cmd/*.rs pulls its own arg structs); kanspec/src/project.rs and cmd/ticket.rs split into 3-4 clusters; ripgrep's only comparable file is ignore/src/pathutil.rs (5 symbols, 5 importers, Jaccard 0.10, 3 clusters)". Prototype (`p41_grabbag.py`, `p41.out`): "Files passing the gate (>=5 referenced pub symbols, >=4 importers) | clusters>=3 | clusters>=3 & Jaccard<=0.15: scry 1 | 1 | 1; kanspec 10 | 2 | 2; ripgrep 7 | 1 | 1; fd 0; hono 13 | 2 | 1. Jaccard distributions: kanspec [0.05 0.10 0.12 0.13 0.18 0.21 0.23 0.23 0.23 0.59], ripgrep [0.10 0.26 0.27 0.36 0.37 0.44 0.65], hono [0.01 0.14 0.14 0.15 0.15 0.16 0.19 0.19 0.22 0.25 0.30 0.70 0.72]. kanspec cli.rs: 53 symbols, 27 importers, 17 clusters, Jaccard 0.05". Actionable hits: "kanspec src/scan.rs 3 clusters / J 0.12: {ScanOpts, ConfirmFacts, plan_confirm} vs {NoCodeWaiver, Landed}->cmd/done.rs vs {ScanToken}->plan.rs: NoCodeWaiver/Landed are done-command concepts living in scan.rs -> real, small, actionable"; "hono src/utils/url.ts 5 clusters / J 0.14: url.ts serves the trie router, hono-base and cookie/request with disjoint function sets -> real grab-bag". Noise: "scry src/config.rs 6 clusters / J 0.05: each pass imports only its own config section", cli.rs (excluded by `ignore_stems`). Verdict: refine; "Yields 1-2 findings per repo, all of which are what the reason claims".

**Detection**
Deps pass today stores file edges only (`DepGraph.edge_set: HashSet<(String,String)>`, `FileDeps.fan_in`); it must additionally record imported names per edge.
- Rust exports: `function_item | struct_item | enum_item | trait_item | type_item | const_item | static_item` with a `visibility_modifier` child whose parent is `source_file` or a `mod_item > declaration_list` (fns inside `impl_item` excluded). References: `use_declaration` argument `scoped_identifier(path:, name:)`, `scoped_use_list(path:, list: use_list)`, `use_as_clause(path:)`, `use_wildcard`; `crate::`/`super::`/`self::` resolved as deps already does; cross-crate via `Cargo.toml [package].name` -> src map; `use crate::a::Name` where `a::Name` is not a module resolves to symbol `Name` of module `a`. Inline path-qualified references (`scoped_identifier`/`scoped_type_identifier` such as `cli::StartArgs`) count as a reference when the path prefix resolves to the file.
- TS/TSX exports: `export_statement(declaration: function_declaration | class_declaration | interface_declaration | type_alias_declaration | enum_declaration | lexical_declaration > variable_declarator)` and export lists. References: `import_statement(source:) -> import_specifier(name:)`, resolved with the deps pass's `.ts/.tsx/index` and `.js`-suffix rules; `import * as ns` then `ns.name` member reads.
- Python exports: module-level `function_definition | class_definition` without leading underscore. References: `import_from_statement(module_name: relative_import | dotted_name, name: dotted_name | aliased_import)`; `import pkg.module as m` then `attribute(object: m)` reads.
- Re-exports (`mod.rs`, `__init__.py`, `index.ts` barrel): traverse, attributing the importer to the file that defines the symbol; do not stop at the barrel.
- Glob imports (`use foo::*`, `from x import *`): mark the importing file as an unresolved importer of the target; if a target file has any unresolved importer, it is skipped with `unresolved_glob_imports` set in JSON.
- Graph ops: union-find over symbols keyed by importer; mean pairwise Jaccard over importer sets; for each cluster, its symbol list and importer list.
- Type-share: fraction of exported symbols that are `struct/enum/trait/type/interface/type_alias/class-without-methods`; >= `types_share_label` labels the file "shared-types module" instead of "disjoint consumer groups".
- Alternate's ideas: name-vocabulary Jaccard and dumping-ground rank boost dropped (taste, per skeptic). Per-file `--numstat` growth is not part of this pass; if wanted later it belongs in the history pass as its own churn detail.
- No git, no LLM.

**Reason line**
`imported by 27 files in 3 disjoint groups (cohesion 0.12): {ScanOpts, ConfirmFacts, plan_confirm} by cmd/scan.rs and 4 others; {NoCodeWaiver, Landed} only by cmd/done.rs; {ScanToken} only by plan.rs — moving a group next to its consumers cuts fan-in`
For type-heavy files: `imported by 64 files as a shared-types module (mean 2 names per importer)` (no split advice).

**Knobs**
```toml
[cohesion]
min_pub_symbols = 5
min_importers = 4
min_clusters = 3
max_jaccard = 0.15
max_groups_in_reason = 3
types_share_label = 0.7
ignore_stems = ["cli", "prelude", "index", "mod", "__init__", "config", "lib"]
skip_reexport_only_files = true
weight = 0.0
```

**Objection and answer**
Skeptic: low cohesion is the correct shape of a shared-types module (hono src/types.ts, 64 importers each pulling 1-4 names; kanspec model.rs, cli.rs), and the resolver needed (globs, re-exports, path-qualified refs, `import module as m`) is not what deps has; the proposal's own `ignore_stems` removes its headline. Answer, folded in: the pass is not scored (`weight = 0.0`) and emits only as a clause on the existing "imported by N files" reason, so a wrong partition costs a clause, not rank; files whose exports are >= `types_share_label` types are labelled "shared-types module" and never given split advice; the resolver handles re-exports and path-qualified references, and any file with an unresolved glob importer is skipped and marked, rather than mis-partitioned; the name-vocabulary alternate and dumping-ground boost are removed. cli.rs staying in `ignore_stems` is correct: a clap Command enum must list every subcommand, so the split is not actionable.

**Folds into**
Deps pass (per-edge names) plus the report's fan-in reason at `report/mod.rs:280`; a `consumer_groups` array per file in `--json`. No new section, no score weight.

**Cost**
Moderate: one extra name-set per edge during the existing import walk; union-find per gated file is O(symbols x importers). Barrel traversal is the only new resolution work.

**Rules**
- Deps records, per resolved edge, the set of imported names (empty set = glob or side-effect import; flagged `glob`).
- Exported symbols per file collected per grammar as listed under Detection; `#[cfg(test)]` mods and test files excluded from both sides.
- Re-export files resolve through to the defining file; a file consisting only of re-exports is skipped when `skip_reexport_only_files`.
- Path-qualified inline references count as imports of the named symbol when the prefix resolves in-repo.
- Gate: `pub_symbols >= min_pub_symbols`, `fan_in >= min_importers`, stem not in `ignore_stems`, no glob importers.
- Clusters: union-find over symbols sharing >= 1 importer; symbols with zero importers excluded from clustering (they are dead-surface, a different pass).
- Cohesion: mean pairwise Jaccard of importer sets over referenced symbols; zero pairs -> skip.
- If `types_share >= types_share_label`: emit shared-types reason, never split advice.
- Else if `clusters >= min_clusters && cohesion <= max_jaccard`: replace the plain fan-in reason with the grouped form, listing up to `max_groups_in_reason` largest groups by symbol count, each with its symbols and its importer count (name the importer when exactly one).
- Otherwise the existing fan-in reason text is unchanged.
- JSON: `consumer_groups: [{symbols, importers}]`, `cohesion`, `types_share`, `unresolved_glob_imports: bool` per file.
- `weight` exists but defaults 0.0; when raised, the percentile of `clusters x fan_in` enters the score alongside coupling.
- Tests: kanspec scan.rs must yield 3 groups with NoCodeWaiver/Landed -> cmd/done.rs; hono src/types.ts must emit the shared-types form; scry config.rs must be suppressed by `ignore_stems`; a fixture with `use super::*` must be skipped and marked.

### P38 LCOM4 cohesion split per impl/class (tier C)
Dropped: the headline numbers (Layout 12, Git 8, 88% vs 68%) were a counting bug (`self.method()` counted as a field); with calls linking methods, kanspec is more cohesive than ripgrep (27% vs 35% at LCOM4>=3) and every remaining top hit is a getter bag or args struct.

### P39 feature envy (tier C)
Dropped: ~1% of methods in every corpus and highest in human code; without type resolution the hits are parsers/builders consuming a string or a `ctx`, and `match self` enum methods score own=0 by construction, so the pass flags exactly the methods that belong where they are.

### P40 god file / type zoo (tier C)
Dropped: files with >= 15 types are 4-6% of files in every corpus and every top hit is an intentional registry (clap `cli.rs`, ripgrep `flags/defs.rs`, click `types.py`, hono `intrinsic-elements.ts`); the only residue is appending "defining N types" to the existing size reason, which needs no pass.

### P42 single-implementation abstractions (tier C)
Dropped: after its own knobs (macro-generated skipped, public API skipped) the default run reports zero findings on all six corpora; kanspec has 3 traits in 34k lines, so there is no evidence LLM code over-abstracts here.

### P43 same-file parameter objects (tier C)
Dropped: strict rate is 1-10% of small structs with no LLM/human separation, the flagship `Tokens` is a data record stored in a `Vec` not a two-argument stand-in, and the recommended fix (replace a named record with a tuple) is one clippy pushes against.

## Function shape, parameters and local names

Five proposals; two ship (P46 clumps, P48 short-name gaps), one folds into the metrics pass as a field plus reason text (P44), two are rejected (P45, P47) with their one useful datum absorbed by P46. Nothing here needs git or the import graph; everything is one extra tree walk per unit, so the cost column is uniformly "cheap" and the question is only signal quality.

### P46 parameter tuple clumps (tier A)

**Measures.** Unit: function, aggregated repo-wide. For each function the sorted tuple of parameter names (self/cls/this excluded, params capped at 8) and, per slot, the declared type text where an annotation exists. For every k-subset with k >= `min_group`, the set of (file, function, line) sites using it; subsets whose site set equals a superset's collapse into the superset. Per reported clump: functions, distinct files, per-slot type agreement (how many members share the same type text), and per-slot silenced/unused count (name starts with `unused_prefix`, or zero references in the body). Emitted as a repo-level list, not a per-file score.

**Why it matters for LLM code.** Agents extend a family by copying the last sibling's signature and leave the dead slot rather than drop it. kanspec: `plan_repair(s: &Snapshot, f: &Facts, a: &RepairArgs, _m: &Minter)` at kanspec/src/cmd/repair.rs:79, plan_start cmd/flow.rs:300, plan_ship :853, plan_park :938, plan_drop :1016 all carry `_m: &Minter` unused, while cmd/decision.rs:78 plan_decide uses `m`; src/triage.rs has (a, touched, s) across 6 functions. Prototype (collapsed clumps / functions in any clump / functions analysed): scry 3 / 12 / 64, kanspec 11 / 27 / 774, fd 1 / 7 / 140, ripgrep 6 / 19 / 1274, click 12 / 33 / 363 (before overload/dunder skips), hono 8 / 21 / 702. Cross-file: kanspec (a, f, s) 8 fns in 5 files, (_m, a, f, s) 5 fns in 2 files with `_m` silenced in all 5; ripgrep (bytes, range, searcher) 5 fns in 3 files, (matcher, printer, searcher) 4 in 2; hono (name, opt, value) 7 in 2. Functions with an `_x` parameter: kanspec 11/774 (1.4%), hono 11/702 (1.6%), ripgrep 3/1274 (0.24%), click 1/339, fd 0, scry 0. Clump frequency is similar in human and LLM code (1.5%-5% of functions), so this is a refactor lead rather than a tell; what separates is the silenced member: human clumps have 0 silenced members after the trait/cfg exclusions.

**Detection.**
- Rust: `function_item parameters:(parameters (parameter pattern:(identifier) type:))`, skip `self_parameter`. Skip functions under `impl_item` with a `trait:` field, under `trait_item`, `function_signature_item`, `#[test]` fns and `#[cfg(test)]` mods. Same-name functions in one file (cfg-gated variants, ripgrep walk.rs `from_entry_os`) count once.
- Python: `parameters` children `identifier`, `typed_parameter` (first identifier child), `default_parameter name:`, `typed_default_parameter name:`; type text from `typed_parameter`'s `type` child when present. Skip self/cls, methods of `class_definition` with `superclasses:`, `@overload`, dunder names, and stub bodies (only `...`/pass/docstring) — without the last three click floods the list with protocol tuples.
- TS: `formal_parameters > required_parameter|optional_parameter pattern:(identifier) type:(type_annotation)`; skip `method_definition` in a class with `class_heritage`, `abstract_method_signature`, interface `method_signature`, overload declarations, and `arrow_function` whose parent is `arguments`.
- Body references (for the silenced attribute): identifier / `shorthand_field_identifier` / `shorthand_property_identifier` nodes in `body:`, plus `{name}` / `{name:` captures inside `string_literal` and `raw_string_literal` under a macro `token_tree` (Rust format! inline captures). Without the capture scan kanspec git.rs and hooks.rs produced 8 false families.
- Single-caller note (from the skeptic): for a clump, collect identifier nodes equal to each member's name outside its own definition; if every member is referenced from exactly one function, attach "all called from `run`" as an attribute. Text match only, no resolution.
- Grouping: `HashMap<Vec<String>, Vec<Site>>` over k-subsets of sorted names.

**Reason line.**
`parameters (s: &Snapshot, f, a, _m: &Minter) recur in 5 functions across 2 files: plan_repair cmd/repair.rs:79, plan_start cmd/flow.rs:300, plan_ship :853, plan_park :938, plan_drop :1016; slot _m is unused in 5/5, slot f varies (Facts, StartFacts, ShipFacts): introduce a shared input struct or drop the slot`
A slot whose type text agrees across all members prints with its type; a varying slot prints bare with its variants listed; the final clause is "drop the slot" only when a slot is unused in every member, otherwise "introduce a shared input struct".

**Knobs.** `[clumps] min_group = 3, min_functions = 4, min_files = 2, report_if_either = true` (report when functions >= min_functions OR files >= min_files), `min_typed_slots = 2` (at least this many slots must agree on type text where annotations exist; Python clumps with no annotations fall back to `min_functions + 1`), `min_tuple_len = 3` (applied to the joined tuple, not per member, so (s, f, a) survives), `unused_prefix = "_"`, `count_overrides = false`, `skip_dunder = true`, `skip_overloads = true`, `dedupe_same_name_in_file = true`, `note_single_caller = true`, `weight = 0.0`.

**Objection and answer.** Uniform sibling signatures are frequently the design (a dispatcher calls every plan_* the same way); click's `callback(ctx, param, value)` and fd's `(config, entry, stdout)` are free-function protocols the trait filter cannot see; `min_name_len = 2` would have excluded the flagship (s, f, a, _m); and the "agreeing types" idea fails on its own example because plan_start takes `&StartFacts`, plan_ship `&ShipFacts`. The design folds all four: min_name_len is replaced by a whole-tuple length gate; type agreement is per slot and reported, with `min_typed_slots = 2` as the entry bar (s: &Snapshot and _m: &Minter agree, f is reported as varying — which is exactly why the reason says "struct or drop the slot" rather than "PlanCtx"); single-file clumps need 4 members, cross-file need 2 files; the single-caller attribute lets the agent see the dispatcher case; weight 0 keeps it out of the hotspot rank. It is a lead, not a defect predictor, and is filed as such.

**Folds into.** New pass `clumps` and a new report section "Parameter clumps" (after clone pairs), each entry with the silenced-slot attribute that was P47. JSON: `clumps: [{params, types, functions:[{file,name,line}], files, unused_slots:{name: k}, single_caller}]`. Weight 0.0; no change to the composite.

**Cost.** Cheap: one walk over already-parsed trees, hash grouping over at most C(8,3)+C(8,4)+... subsets per function.

**Rules.**
- A clump is a set of >= `min_group` parameter names shared, as a subset of the sorted name tuple, by >= `min_functions` functions or by functions in >= `min_files` files. Subsets whose member set equals a superset's collapse into the superset.
- Receivers (`self`, `cls`, `this`, Rust `self_parameter`) never enter the tuple; params are capped at 8 per function.
- Trait impls, `trait_item` and `function_signature_item` fns, Python methods of classes with `superclasses`, `@overload`s, dunders and stub bodies, TS methods in classes with `class_heritage`, abstract/interface signatures, overload declarations and callback arrows passed as arguments are excluded unless `count_overrides`.
- Functions with the same name in one file count as one site (`dedupe_same_name_in_file`).
- A slot's type agrees when every annotated member has identical type text; a clump is reported only when >= `min_typed_slots` slots agree, or, when no member is annotated, when it has >= `min_functions + 1` members.
- A slot is unused in a member when its name occurs in no identifier / shorthand-field / shorthand-property node in the body and in no `{name}` / `{name:` capture inside a string literal under a macro token tree; a slot starting with `unused_prefix` is unused by definition.
- The reason names the tuple with agreeing types inline, the member sites with file:line, unused slots as "unused in k/n", varying slots with their type variants, and a single caller when `note_single_caller` finds one; it recommends "drop the slot" only for a slot unused in n/n, never "remove".
- Clumps are a separate report section with `weight` (default 0) into the composite; they are never a per-file reason.
- All thresholds are `[clumps]` settings in `scry.toml`.

### P48 short-name live range (tier A)

**Measures.** Unit: function, rolled up to file. For each qualifying binding (name length <= `short_name_max_len`, not in `short_name_allow`, not starting with `_`): name, declaration line, use lines within the enclosing block, `max_gap` = largest line distance between consecutive uses (declaration counts as the first use), use count, enclosing block length. Per function: `far_short_bindings` = count with max_gap >= `short_name_min_gap`; worst binding by gap. Per file: sum, and `short_binding_share` = short bindings / all bindings (JSON only). Percentile-ranked within repo.

**Why it matters for LLM code.** One-letter bindings (outside the allow list) as share of all bindings: scry 21.0% (74/353), kanspec 19.2% (301/1566) vs fd 2.7% (5/188), ripgrep 4.0% (74/1860), click 3.6% (38/1070), hono 1.2% (17/1458). Bindings with live span >= 30 lines: kanspec 36 (23.0 per 1000 bindings), scry 5 (14.2/1000) vs ripgrep 2 (1.1), click 2 (1.9), hono 2 (1.4), fd 0. Span p90: kanspec 36, scry 19, ripgrep 12, click 22, hono 33, fd 20; enclosing-block p90: kanspec 108, scry 62, ripgrep 36, click 70, hono 38, fd 22. Top long-lived names in kanspec: t (11), d (5), o (4), p (4). Every top hit is a real 150-290 line block: board.rs:538->745 `let mut h = String::new()` in render_page_html; derive.rs:787->992 `for t in s.tickets.values()` in attention; cmd/proposal.rs:690->884 `let p = open_proposal(...)` in close; cmd/done.rs:79->236 `t`; cmd/decision.rs:453->590 `let mut r = WhyReport{..}`; scry report/mod.rs:215->334 `f` and 232->335 `h` in build. Human hits (ripgrep parse.rs:226 `p` 56 lines, hono hono-base.ts:423 `c` 43 lines) are borderline but not wrong. The prototype measured declaration-to-last-use span, not gap; the gap variant below is the skeptic's narrowing and is unvalidated.

**Detection.**
- Bindings. Rust: `let_declaration pattern:(identifier)` (`let mut` is the same node with a `(mutable_specifier)` sibling; there is no mut_pattern), `for_expression pattern:(identifier)`; `let _ =` has no pattern field and is skipped. Python: first `expression_statement > assignment left:(identifier)` per name in a `function_definition` block, `for_statement left:(identifier)`, `with_item value:(as_pattern alias:(as_pattern_target (identifier)))`. TS: `variable_declarator name:(identifier)` under `lexical_declaration` / `variable_declaration`, including `for_statement initializer:`. Excluded everywhere: `closure_parameters`, `lambda_parameters`, `for_in_clause` targets, arrow `formal_parameters`, `#[cfg(test)]` mods / test files (already excluded by discovery).
- Enclosing block: nearest `block` (Rust, Python) or `statement_block` (TS) ancestor.
- Rebinding-aware use search (the skeptic's fix). Walk the block's statements after the declaration depth-first. A subtree is a shadow and is skipped entirely when it rebinds the name: Rust `closure_expression` whose `closure_parameters` contains the identifier, `match_arm pattern:(match_pattern ...)` containing it, `if_expression`/`while_expression` with `condition:(let_condition pattern: ...)` containing it (consequence/body skipped), `for_expression pattern:` containing it, a nested `let_declaration` of the same name (ends the range; nothing after it in the block counts); Python `lambda_parameters`, `for_in_clause left:`, nested `for_statement left:`, `named_expression name:`, nested assignment to the name (ends the range); TS arrow/function `formal_parameters` containing it, a nested `variable_declarator` of the same name. Any other `identifier` node with equal text is a use.
- Gap = max over consecutive (use_i, use_i+1) of the line difference; declaration is use_0.

**Reason line.**
Attached to a unit already over `cognitive_hard` or with lines >= `min_unit_lines`:
`short name p (declared line 690) is next used 140 lines later (line 830) in a 258-line block; 3 one-letter bindings have a use gap over 30 lines: p, o, h`
The first clause names the worst binding; the trailing list is present only when `far_short_bindings` >= 2.

**Knobs.** `[naming] short_name_max_len = 1, short_name_allow = ["i","j","k","n","x","y","z","_"], short_name_min_gap = 30, short_name_min_other_bindings = 2, short_name_include_pattern_bindings = false, min_unit_lines = 100, emit_short_binding_share = true, weight_into_complexity = 0.0`.

**Objection and answer.** Declaration-to-last-use collapses to block length, which scry already ranks; text-match last use is inflated by `|t|` closures and match/if-let rebinding of the same letter (kanspec has 56 `|t|` closures in the same files where `let t` lives), so the headline spans p 690->947, t 79->261 are unverified; dense accumulators like `let mut h` (pushed on dozens of consecutive lines) are the obvious HTML buffer, not a comprehension hazard; the 15% vs 2% rate is two maintainers' style and scry ranks within repo anyway; "rename" is cheap and not defect-reducing. The design accepts every point: rebinding subtrees are excluded from the search and a nested let ends the range; the quantity is the largest gap between consecutive uses so `h` with a use every few lines scores ~0 while declare-early/use-far scores high; the reason is an attribute on units already flagged by cognitive or lines, weight 0, so it competes with nothing; `short_binding_share` is JSON-only and never a reason. One gate remains before promotion beyond an attribute: rerun the prototype with the gap metric on kanspec and confirm that no flagged binding is an accumulator; the corpus numbers above are for span and must not be quoted for gap.

**Folds into.** metrics pass: a `short_bindings: Vec<{name, decl_line, worst_gap, gap_line, uses}>` field on FunctionMetrics, the attribute clause appended to the existing "over cognitive" reason or to a new lines-based reason for units >= `min_unit_lines`; per-file `short_binding_share` in JSON. Cross-reference P44: when both fire on one unit (close, attention, build) the two clauses share the same reason line.

**Cost.** Cheap: one linear walk per unit; the shadow check is a kind test on ancestors already on the stack.

**Rules.**
- A short binding is a `let`/assignment/`for`/`with` binding whose name has <= `short_name_max_len` characters, is not in `short_name_allow`, and does not start with `_`; closure, lambda, comprehension and arrow parameters are not bindings.
- Uses are searched in the enclosing block only, after the declaration; a subtree that rebinds the name (closure params, match arm, `let_condition`, `for` pattern, lambda/comprehension/arrow params) is skipped whole, and a later `let`/assignment/`variable_declarator` of the same name ends the range.
- The measure is the largest line gap between consecutive uses, the declaration counting as the first use; a binding qualifies when that gap >= `short_name_min_gap`.
- A unit reports short bindings only when it has >= `short_name_min_other_bindings` other bindings, and only when it is already over `cognitive_hard` or has >= `min_unit_lines` lines.
- The reason names the worst binding, its declaration line, the gap and the line it is next used, and the block length; it never says "rename".
- `short_binding_share` is emitted per file in JSON when `emit_short_binding_share` and never becomes a reason.
- `weight_into_complexity` (default 0) is the only route into the composite.
- Promotion to a standalone reason requires a validation run on kanspec showing no accumulator among flagged bindings.
- All settings live under `[naming]` in `scry.toml`.

### P44 brain method (tier B)

**Measures.** Unit: function. `locals` = distinct local binding names inside the unit (a destructuring pattern counts as one unless `count_pattern_members`; closure bodies fold into the enclosing unit; nested named functions do not). Brain conjunction: lines >= `min_lines` AND cognitive >= `min_cognitive` AND locals >= `min_locals`. No per-file percentile of its own.

**Why it matters for LLM code.** LLM functions accrete `let` after `let` because each fix adds one more intermediate instead of extracting a helper. Functions with >= 20 local bindings: kanspec 7/1256 (0.6%), scry 5/105 (report/mod.rs:169 `build` 44 lets in 181 lines, main.rs:110 `main` 36/198) vs ripgrep 3/2895 (0.1%; worst hiargs from_low_args 24 lets in 220 lines) and fd 0/218 (worst construct_config 18). Prototype: brain methods / functions: scry 4/70 (5.7%), kanspec 5/844 (0.59%), fd 1/155 (0.65%), ripgrep 1/2318 (0.04%), click 2/584 (0.34%), hono 2/208 (0.96%). Share of cognitive>15 functions that are also brain: scry 4/12, kanspec 5/27, fd 1/4, ripgrep 1/38, click 2/26, hono 2/15. Top hits: scry report/mod.rs:169 build (181 lines, cog 62, 45 locals) — real, though ~10 of the 45 are one-letter lets inside a map closure; kanspec cmd/proposal.rs:687 close (258/42/34); kanspec derive.rs:778 attention (288/30/23); hono LinearRouter.match (126/149/24) — a deliberate hot-path loop; ripgrep hiargs.rs:114 from_low_args (220/38/24) — a known config-assembly function. Not an LLM tell by rate (kanspec 0.59% ~ fd 0.65%), but nothing in the top 10 is noise.

**Detection.**
- Rust: `let_declaration pattern:(identifier | tuple_pattern | tuple_struct_pattern | struct_pattern ...)` and `for_expression pattern:` inside the unit's `block`; descend into `closure_expression` bodies (their lets count toward the enclosing unit, matching nesting attribution today); do not descend into nested `function_item`. `let _ =` (no pattern field) is not a binding. Optional via `count_match_bindings`: `match_arm pattern:` and `let_condition pattern:` identifiers.
- Python: `expression_statement > assignment left:(identifier | pattern_list | tuple_pattern)`, `for_statement left:`, `with_item ... alias:(as_pattern_target (identifier))`, `named_expression name:`; `augmented_assignment` excluded; distinct names per `function_definition`.
- TS: `lexical_declaration | variable_declaration > variable_declarator name:(identifier | object_pattern | array_pattern)`, including `for_statement initializer:`.
- Lines and cognitive come from FunctionMetrics; join is free since the count is taken on the same walk.

**Reason line.** The existing cognitive reason gains the locals count, and the "brain method" label when the conjunction holds:
`3 function(s) over cognitive 15; worst close at 42 (lines 687-944, nesting 3, 34 locals) — brain method: 258 lines, cognitive 42, 34 local variables`
Without the conjunction the reason is unchanged except for the `, 34 locals` insertion.

**Knobs.** `[metrics] report_locals = true, brain_min_lines = 100, brain_min_cognitive = 15, brain_min_locals = 15, count_pattern_members = false, count_match_bindings = false, brain_weight = 0.0`.

**Objection and answer.** The brain set is a strict subset of what scry already reports as cognitive > 15 (close and attention are already top hotspots); locals is near-collinear with lines (33/258, 25/288; from_low_args 39 raw lets in 220; construct_config 18/146); the reason_example's "locals live across 200+ lines and mark extraction seams" is P48's machinery, not a count; distinct-name counting is wrong both ways in Rust (shadowing collapses, match-arm bindings inflate). The design concedes and narrows: no new section, no new category in the score, no `[brain]` table; `locals` becomes a FunctionMetrics field, the count is appended to the existing reason, the "brain method" label is text only, and `brain_weight` defaults to 0 with the option to spend up to 0.05 of the complexity weight later. The seam wording is dropped; P48 owns it. Shadowing and match-arm bindings are handled by counting distinct names (shadowing idiom counts once, by design) and by leaving match/if-let bindings off unless `count_match_bindings`. Tie-breaking among units already over the cognitive knob is the one ranking effect: the worst function per file is chosen by cognitive, then locals, then lines.

**Folds into.** metrics pass: `locals: usize` on FunctionMetrics, reason text in report/mod.rs where the "over cognitive" string is built, JSON field per function. No new section or subcommand.

**Cost.** Cheap: the same traversal that computes cognitive.

**Rules.**
- `locals` is the number of distinct binding names introduced by `let`/assignment/`for`/`with`/walrus/declarator inside a unit; a destructuring pattern is one binding unless `count_pattern_members`; match-arm and `if let`/`while let` bindings count only when `count_match_bindings`.
- Closure and lambda bodies count toward the enclosing unit; nested named functions do not, and are counted on their own.
- Rebinding a name that already exists in the unit does not increase `locals`.
- A unit is a brain method when lines >= `brain_min_lines`, cognitive >= `brain_min_cognitive` and locals >= `brain_min_locals`; the label is appended to the reason and never forms its own section.
- The worst function named in the cognitive reason is chosen by cognitive, then locals, then lines.
- `locals` appears on every function in `--json` when `report_locals`; `brain_weight` (default 0) is subtracted from the complexity weight when set.
- Thresholds are `[metrics]` settings in `scry.toml`.

### P45 message chains and config drilling (tier C)
Rejected: tree-sitter cannot separate data-struct reads (serde config trees, Fetch API shapes, `self.config.*` in the owning type) from Demeter violations, pure data depth >= 3 is essentially absent outside kanspec (19) and hono (28), and the proposal itself ships weight 0 — a reason that tells an agent to add accessors to config structs is negative-value noise.

### P47 silenced and unused parameter families (tier C)
Rejected as a pass: across ~3.3k functions it yields two families, one a deliberate capability token (`_t: &LockToken`) and the other P46's clump (`_m: &Minter`); its only useful output, "slot unused in k of n", is now the silenced-slot attribute of a P46 clump.

## test strength (per test unit)
Shared substrate: P49 defines the test-unit extractor and assertion-site detector (with macro and helper resolution); P52 reuses the unit extractor. Both report in a new `tests` section and never feed the hotspot score. The tests pass runs over Test-classified files only, which today's passes skip entirely.

### P49 assertion-free tests (tier B)

**Measures.** Per test unit (Python `def test_*` at module or class level; TS `it()`/`test()` callback; Rust `#[test]`/`#[tokio::test]` `function_item`, including inside `#[cfg(test)] mod` bodies and inside function-defining test macros such as `rgtest!`): number of assertion sites, resolved through project macros and test helpers; in Rust also the number of fallible-call sites (`unwrap`, `expect`, `?`, `panic!`, `unwrap_or_else`). Output is a per-test list of units with zero assertion sites, not a percentile.

**Why it matters for LLM code.** It is not an LLM tell; it is a high-precision, two-minute fix. Prototype final zero-assert share (should_panic/xfail exempt): scry 0/28 = 0%; kanspec 4/580 = 0.7%; ripgrep 5/841 = 0.6%; fd 1/157 = 0.6%; click 3/557 = 0.5%; hono 16/2959 = 0.5%. Before macro/helper resolution the human baselines were badly over-flagged: ripgrep 89/492 = 18.1% (assert_eq_printed!), then 71/841 = 8.4% (rgtest `.assert_err()`), fd 3/157 = 1.9% (local macro_rules! check), kanspec 6/580 (cross-file `.ok()` helper, clap `debug_assert()`). Real hits: ripgrep crates/index/src/literal.rs:976-999 `scratch` (all commented-out println) and crates/core/flags/defs.rs:8002-8024 `available_shorts` (eprintln only) are debug scratch tests that always pass. kanspec tests/lock.rs:50-90 `lock_child` (0 asserts, 5 unwrap/expect) is a child-process worker role under `#[test]`, `#[ignore]`d with "Not part of the suite"; it is exempt under the design below. kanspec src/plan.rs:465-478 `create_then_set_in_one_plan_is_legal` asserts via `.unwrap()`; it is exempt by default and reported only with `count_fallible_as_assert = false`.

**Detection.**
- Test units. Rust: `function_item` whose preceding `attribute_item` siblings contain `attribute > identifier` `test`, `scoped_identifier` whose name is `test` (tokio::test), or `should_panic`/`ignore`; recurse into `mod_item > declaration_list`. Units defined by a macro in `tests.test_macros` (e.g. `rgtest!`) are `macro_invocation > token_tree` with no `function_item`: scan tokens (an `identifier` followed by a `!` sibling, or an `identifier` preceded by `.`) against the same lists; `token_tree` has no `field_identifier` nodes. Python: `function_definition` whose `name` starts with `test_` and whose parent (through `decorated_definition`) is `module` or the `block` of a `class_definition`; this excludes click tests/test_context.py:31 `def test(foo)` (a nested command). TS: `call_expression` whose `function` is `identifier` `it`/`test` or `member_expression` `it.each`/`.skip`/`.only`, with an `arrow_function`/`function_expression` argument whose body is a `statement_block`.
- Assertion sites. Rust: `macro_invocation` whose `macro` (`identifier` or last segment of `scoped_identifier`) is in `assert_macros` or starts with an `assert_macro_prefixes` entry (needed for ripgrep's `assert_eq_printed!`); `call_expression` whose `function` is `field_expression > field_identifier`, `identifier` or `scoped_identifier` starting with an `assert_name_prefixes` entry, minus `assert_name_excludes` (`expect` alone is `Option::expect`; `expect_*` is mockall). Python: `assert_statement`; `call` whose `function` is `attribute` with attribute text starting `assert` (`self.assertEqual`, `mock.assert_called`); `with_item` whose value is a `call` of `pytest.raises`/`pytest.warns`; `decorator` `pytest.mark.xfail`. TS: `call_expression` whose `function` is `member_expression` whose object chain (through `await_expression`, `.not`, `.resolves`, `.rejects`) bottoms out at `call_expression` with `function` `identifier` `expect`/`expectTypeOf`; `identifier` `assert*`; `type_alias_declaration` whose value `generic_type` names an entry of `type_assert_names` (hono src/types.test.ts:105 and its 587 siblings).
- macro_rules resolution (Rust). Every same-repo `macro_definition` whose body mentions an assert macro becomes an assert macro (fd src/exec/mod.rs:448 `macro_rules! check`; ripgrep `matched!/ignored!/syntax!`, 21 macros). Without it fd had 3 false hits and ripgrep 89.
- Helper resolution. Build `name -> assertion count` for every `function_item`/`function_definition`/`function_declaration` and method in Test-classified files, resolved as a fixed point up to `helper_depth`. Scope: same file, then files reachable from the test file through the import graph (`mod`/`use` for Rust, `conftest.py` and relative imports for Python), then all Test files by name. kanspec tests/common/mod.rs `pub fn ok(self)` asserts the exit code and is called as `repo.ks([..]).ok()` from every tests/*.rs; same-file-only resolution flags real tests.
- Builder terminal methods. A method call named in `terminal_methods` on a value constructed in the same body counts as an assertion site (ripgrep crates/searcher/src/searcher/glue.rs:387-520 `SearcherTester::new(..)...test()` asserts inside crates/searcher/src/testutil.rs:250).
- Fallible sites (Rust): `call_expression` with `field_identifier` `unwrap`/`expect`/`unwrap_or_else`, `try_expression`, `macro_invocation` `panic`.
- Exempt: `#[should_panic]`, `#[ignore]`, `pytest.mark.xfail`, TS type tests (a unit whose only statements are `type_alias_declaration`s, or whose name matches `exempt_name_patterns`).
- No git or graph ops beyond the existing import graph.

**Reason line.**
`available_shorts (crates/core/flags/defs.rs, lines 8002-8024) asserts nothing: 0 assertion sites, 0 unwrap/expect calls; it passes unless something panics`
When a resolution rule kept a unit off the list nothing prints; when a unit is listed with fallible calls the count is printed so the agent sees it is a does-not-error test.

**Knobs.** `[tests] assert_macros = ["assert","assert_eq","assert_ne","assert_matches","assert_snapshot","assert_debug_snapshot","assert_json_snapshot","proptest","debug_assert"]`, `assert_macro_prefixes = ["assert","eqnice"]`, `assert_name_prefixes = ["assert","expect","check","verify","eqnice","should","must"]`, `assert_name_excludes = ["expect"]`, `terminal_methods = ["test","run","check","verify"]`, `type_assert_names = ["Expect","Equal","expectTypeOf"]`, `test_macros = ["rgtest"]`, `resolve_macro_rules = true`, `helper_scope = "test_files"` (`"same_file"` | `"imports"` | `"test_files"`), `helper_depth = 2`, `count_fallible_as_assert = true`, `exempt_name_patterns = ["type error","typecheck"]`, `min_zero_assert_tests_to_report = 1`.

**Objection and answer.** Skeptic: in Rust `unwrap`/`expect`/`?`/`unwrap_or_else(panic!)` are the assertion mechanism, two of three cited kanspec exemplars assert that way and the third is `#[ignore]`; cross-file harness methods (`.test()` in another file) defeat one-level same-file resolution; yield is a handful per repo, not a ranking. Answer, folded in: Rust units with any fallible site are exempt by default (`count_fallible_as_assert`), `#[ignore]` is exempt, helpers resolve across files via the import graph with a repo-wide Test-file fallback, `terminal_methods` covers builder harnesses, the type-alias exemption is a hard default, and the output is a flat per-test list with no percentile and no score input. Expected yield 0-10 hits per repo at ~100% precision; priced as a side-signal of the tests pass whose real value is the substrate P52 needs.

**Folds into.** New report section `tests` (per-test list); JSON `tests.assertion_free[]`. No score weight.

**Cost.** Cheap: one walk over Test files (already parsed), a repo-wide macro_rules and helper map, fixed-point iteration bounded by `helper_depth`.

**Rules.**
- Run only over files classified Test; skip Data/Generated/Vendored.
- A test unit is a Rust `function_item` under a `test`/`tokio::test` attribute, a Python `test_*` `function_definition` at module or class-block level, a TS `it`/`test`/`it.each` call with a function argument, or a `tests.test_macros` invocation (token scan).
- An assertion site is: an assert macro (list or prefix, plus every same-repo `macro_rules!` whose body contains one); a call whose callee name starts with an `assert_name_prefixes` entry and is not in `assert_name_excludes`; `assert_statement`; `pytest.raises`/`warns` with-item; an `expect`/`expectTypeOf` chain; a `type_assert_names` alias; a call to a helper whose resolved assertion count is > 0; a `terminal_methods` call on a value constructed in the same body.
- Helper resolution is a fixed point over `helper_scope` to `helper_depth`; a helper's count is the max over its definitions when names collide.
- Rust fallible sites are counted separately; with `count_fallible_as_assert = true` a unit with fallible sites > 0 is not listed.
- `#[should_panic]`, `#[ignore]`, `xfail`, type-only bodies and `exempt_name_patterns` are never listed.
- Reason prints unit name, path, line range, assertion-site count, fallible-site count.
- List is sorted by file then line; emitted only when its length >= `min_zero_assert_tests_to_report`.
- Every list above is a `[tests]` knob; user values extend defaults unless `replace = true`.

### P50 weak and tautological assertion share (tier C)
Rejected: tautologies are already warn-by-default in clippy (`assertions_on_constants`, `eq_op`), pylint (W1503, R0124), ruff (PLR0124, B011, PT015) and eslint-plugin-jest, and the shape-only share measures test level (CLI integration suites are the top percentile in every repo, and `assert!(stdout.contains(..))` is the correct idiom for unstable banners); the one surviving case (sole bare presence check on the direct return value) is a two-line addition to P49's list, not an analyzer.

### P51 setup-to-assert ratio (tier C)
Rejected: with statements as the unit every corpus has median 1-2 and p90 <= 6 setup statements and the gate (ratio >= 10, >= 15 statements) fires on 0 tests in scry, kanspec, ripgrep, fd and click; the flagship exemplar (kanspec tests/cli_smoke.rs:50-98) is a table test with 0 top-level setup statements; the only actionable sub-signal, the shared-prefix fixture hint, is absorbed by P52's `fixture_candidate` variant.

### P52 twin tests (tier B)

**Measures.** Per test unit: a hash of the body with literals replaced by placeholders, identifiers kept, comments dropped, the unit's own name replaced. Per enclosing scope (describe callback, `mod`, class or module), groups of >= `min_twins` units sharing a hash, each with >= `twin_min_stmts` statements; share of the scope's tests inside a group. Optional `fixture_candidate` variant: hash only the first `shared_prefix_stmts` normalized statements and report groups of >= `min_twins`.

**Why it matters for LLM code.** It is the inverse of an LLM tell and is proposed as a maintenance signal: scope-bound twin groups (>= 3 members, >= 2 statements) / tests covered: scry 0 / 0%; kanspec 0 / 0% (1 pair at >= 2); ripgrep 12 groups / 40 of 841 = 4.8% (9 groups with >= 3 statements, 29 tests); fd 3 groups / 15 of 157 = 9.6% (0 groups at >= 3 statements); click 0 / 0% (5 pairs); hono 71 groups / 277 of 2959 = 9.4% (65 groups at >= 3 statements, 259 tests). Largest scope-bound groups: hono helper/cookie 11 tests x 4 stmts, middleware/cache 9 x 4, bearer-auth 8 x 7, logger 8 x 5, hono.test.ts 7 x 3; ripgrep crates/cli/src/escape.rs two groups of 5 x 2-3 stmts, crates/ignore/src/dir.rs 3 x 8, printer/standard.rs 3 x 6; fd tests/tests.rs 8 x 2, src/filter/size.rs 4 x 2. Real hits: hono src/middleware/bearer-auth/index.test.ts:720-760 (8 tests, 7 statements each, identical except the request URL) and src/middleware/logger/index.test.ts:44-68 (8 x 5, differ only in path and expected log prefix). Fixture variant: hono src/utils/jwt/jwt.test.ts:1026-1077 (and 1228, 1373, 1178, 1272, 1325), 6 tests each with 14-17 statements regenerating a key pair and hand-signing a token before 1 expect. Scope binding is the non-optional refinement: file-scoped grouping reported 102 groups/536 tests (18%) in hono, scope-bound 71/277 (9.4%), because hono.test.ts:245 and :264 are byte-identical modulo literals but live in different describe blocks with different `app` setup.

**Detection.** Units from P49's extractor; body is the Rust `block`, Python `block`, TS `statement_block`. Emit leaf token text in order, replacing Rust `string_literal`/`raw_string_literal`/`char_literal`/`integer_literal`/`float_literal`/`boolean_literal`, Python `string`/`integer`/`float`/`true`/`false`/`none`, TS `string`/`template_string`/`number`/`true`/`false`/`null` with `LIT`; drop `line_comment`/`block_comment`/`comment`; replace the unit's own name token; hash. Identifiers stay so `from_string(LIT)` and `from_bytes(LIT)` do not merge. Scope key = nearest ancestor of kind `statement_block` (describe callback), `declaration_list` (mod), class `block`, or `module`. Group by (file, scope, hash). `rgtest!`-style token-only units are excluded. Rewrite suggestion is language-gated: `pytest.mark.parametrize` for Python; `it.each`/`test.each` for TS; for Rust `#[test_case]`/`rstest` only if `test-case` or `rstest` appears in a discovered Cargo.toml, otherwise "extract a shared helper fn". Reuses the clone tokenizer with an identifier-preserving flag; no git or graph ops.

**Reason line.**
`src/middleware/bearer-auth/index.test.ts: 8 tests share one body shape differing only in literals (lines 720-760, 7 statements each; first is 'should authorize' line 720) — it.each candidate`
Rust form ends `— extract a shared helper fn` (or `— #[test_case] candidate` when the crate already depends on it). Fixture variant: `src/utils/jwt/jwt.test.ts: 6 tests begin with the same 14 statements (lines 1026, 1178, 1228, 1272, 1325, 1373) — extract a fixture`.

**Knobs.** `[tests] twins = true`, `min_twins = 3`, `twin_min_stmts = 3`, `twin_scope = "enclosing"` (`"file"` | `"enclosing"`), `twin_share_reason = 0.25`, `fixture_candidate = true`, `shared_prefix_stmts = 4`, `rust_table_crates = ["test-case","rstest"]`.

**Objection and answer.** Skeptic: folding Rust twins into a loop loses per-case names, filters and independent failures; hono uses `it.each` only 37 times against 2123 `it`/`test` sites; two-statement cases are the idiomatic Rust unit-test shape; it is a configuration variant of the clones pass. Answer, folded in: `twin_min_stmts = 3` removes every 2-statement group (fd's 8 x 2 house style, ripgrep escape.rs) while keeping all hono hits; the Rust suggestion never recommends a loop, only a helper fn or an already-present table crate; scope binding keeps describe-named parametrizations out; the item is reported, never scored, so a maintainer who prefers named cases loses nothing. It cannot be a clones knob because the clones pass erases identifiers, has a 70-token floor and no unit or scope boundaries; it shares only the tokenizer.

**Folds into.** `tests` section, `twins[]` and `fixture_candidates[]` in JSON. No score weight.

**Cost.** Cheap: one tokenization per test unit, hash map per scope.

**Rules.**
- Requires P49's unit extractor; runs over Test files only.
- Normalized body: leaf tokens in order, literal kinds listed above -> `LIT`, comments dropped, own name -> `SELF`; hash the joined text.
- Units with fewer than `twin_min_stmts` top-level statements or no statement body (macro units) are skipped.
- Group key is (file, scope node id, hash) with `twin_scope = "enclosing"`; (file, hash) with `"file"`.
- Report groups with >= `min_twins` members; sort by member count desc, then first line.
- Suggestion text is chosen by language: Python `parametrize`, TS `it.each`/`test.each`, Rust `#[test_case]`/`rstest` only when a discovered Cargo.toml names a `rust_table_crates` entry, else "extract a shared helper fn".
- With `fixture_candidate = true`, additionally hash the first `shared_prefix_stmts` normalized statements of units with more than that many statements; report groups >= `min_twins` not already reported as full twins.
- Reason prints path, member count, line list or range, statement count, first member's name and line, suggestion.
- Scope summary prints the twin share when it is >= `twin_share_reason`.

### P53 mock-heavy tests (tier C)
Rejected: neither LLM-written corpus contains a single mock (scry 0, kanspec 2 sites), and in the human corpora every inspected hit (click tests/test_termui.py:988-1005, tests/test_arguments.py:82-102, hono components.test.tsx:1327-1342, ssg.test.tsx:414-433) is a legitimate environment-isolation or spy-contract test, giving ~0% precision; revisit only with an LLM-written Python/TS corpus that mocks, counting dependency replacement rather than bare `vi.fn()` handlers and reconciling its name lists with P49.

## test coverage proxies, staleness and test-file ranking
Ordering recommendation: P54 first (it is a correctness fix that every other proposal here leans on), then P55's file-level half, then P56's single repo-relative line, then P58 once the per-unit test analyzers from the other clusters exist. P57 is rejected.

### P54 inline test-module split (tier B)

**Measures**
Per Source file: `source_lines`, `inline_test_lines`, `inline_test_ratio`, and a list of test regions (byte range, line range, region kind: `cfg_test_mod` | `cfg_test_item` | `test_fn`). Per metrics unit and per clone run: a `kind: source | test` tag. Unit: file (line ranges) and function.

**Why it matters for LLM code**
It does not separate LLM from human code — the original claim is wrong and the prototype says so: inline test lines / all source lines are kanspec 8326/34146 (24.4%), scry 531/3112 (17.1%), ripgrep 14182/55946 (25.3%), fd 957/5076 (18.9%); click and hono 0. It matters because it corrects three signals scry already emits:
- Size: derive.rs 2307 -> 1282, doctor.rs 1777 -> 965, cmd/flow.rs 1469 -> 1065; ripgrep defs.rs 8161 -> 3788, ignore/dir.rs 1613 -> 1115, walk.rs 2740 -> 2192; fd exec/mod.rs 486 -> 274. Inflation of 1.4-2.2x on top hotspots.
- Clones: pairs lying entirely inside inline test regions are kanspec 3/15, scry 5/10, ripgrep 2/15 (+5 one-sided, all in defs.rs), fd 6/11 — e.g. kanspec src/cmd/done.rs:811-832 <-> 843-864 (178 tok) is repeated args()/triage() fixture setup inside `mod tests` (starts 551); fd src/filter/size.rs:97-131 <-> 132-166 (455 tok) is a size-parse case table inside tests 77-219.
- worst_functions inside test regions: kanspec 1/45, ripgrep 1/45, scry 0/25, fd 0/40 (small; complexity is barely affected).
- has_tests: Rust files whose only tests are inline currently read as "no test file references it" unless report/mod.rs:165's string check catches them.

**Detection**
Rust only (confirmed with `scry ast`):
- `(attribute_item (attribute (identifier) arguments: (token_tree (identifier))))` where the attribute identifier text is `cfg` and the token_tree text is `test`, followed by its next named sibling. If that sibling is `mod_item`, the region is the whole `mod_item` (kind `cfg_test_mod`). If it is any other item (`function_item`, `impl_item`, `use_declaration`, `macro_definition`) the region is that item alone (kind `cfg_test_item`) — this covers the skeptic's ripgrep cases (ignore/dir.rs:170-171 `pub(crate) fn` inside an impl, globset/glob.rs:1065-1071 free fns, printer/macros.rs:2-3 macro_export). Apply the rule at every nesting depth, so `#[cfg(test)]` inside a `declaration_list` of an impl is caught; nested regions merge into the outer one.
- `(attribute_item (attribute (identifier)))` with text `test` followed by `function_item` (kind `test_fn`) — for `#[test]` fns not under a cfg(test) mod.
- Multiple regions per file are normal (kanspec has 38 cfg(test) sites across 37 files); the module name is never consulted (kanspec src/cmd/proposal.rs:1334 `mod page_tests`).
- The TS and Python legs are dropped: hono has 0 describe( calls outside `*.test.ts`; Python module-level `test_*` in Source files are pytest fixtures or CLI subcommands named `test`, and click's testing.py ships TestCase-style API as Source.
Consumers: metrics tags units whose byte range lies inside a region as `test` (keeps them, does not drop them); clones strips region bytes from the token stream before winnowing; discovery's line count reports `source_lines`; report replaces the string check at report/mod.rs:165 with `regions.len() > 0`. No git, no graph ops.

**Reason line**
No hotspot reason. The size reason becomes: `2308 lines, 1026 in #[cfg(test)] mod at 1283-2308`. A per-function reason inside a region gets the suffix ` (in inline tests)`, e.g. `worst parse_fixture at 18 (lines 1300-1360, nesting 3, in inline tests)`.

**Knobs**
```toml
[tests]
inline_regions = true          # tag cfg(test)/#[test] regions in Rust Source files
inline_tag_metrics = true      # metrics units inside a region carry kind = "test"
inline_strip_clones = true     # clone tokenizer skips region bytes
report_inline_ratio_above = 0.5  # --json and size reason only; 0.3 fires on 15 kanspec / 22 ripgrep files (an idiom, not a finding)
```
Dropped from the proposal: `inline_test_callees`, `python_test_prefix`, `count_inline_tests_as_refs`, `reclassify_file_above_ratio` (fires on 1 file across all corpora, ripgrep glue.rs 0.92, and a 90%-test Rust file is still the module's only source).

**Objection and answer**
Objection: the LLM premise is false, the corrected signals are the light ones (size 0.05, clones 0.10), churn stays unsplit, and suppressing done.rs:811-832 <-> 843-864 loses a finding (it is a twin test). Answer: the design accepts all of it. It is filed as a correctness prerequisite, not a tell; the score effect is a side benefit and the real deliverable is region tags that P55, P56, P58 and the twin-test analyzer in the other cluster consume. The done.rs pair is not lost: clone runs inside regions are tagged `test`, not deleted, and the twin-test analyzer reads exactly those. Churn is explicitly not split (P56's `inline_hunks`). The cfg(test) helper problem (kanspec src/triage.rs:1021 `#[cfg(test)] pub(crate) fn scripted`) only bites `count_inline_tests_as_refs`, which is dropped.

**Folds into**
Existing passes (metrics, clones, discovery line count, report has_tests). New `--json` fields: `inline_test_lines`, `inline_test_ratio`, `test_regions[]`, `kind` on metrics units and clone runs. No score weight.

**Cost**
Cheap: one extra walk of the already-parsed tree per Rust file; strictly reduces clone tokenization work.

**Rules**
- A test region is the byte range of the named sibling that immediately follows an `attribute_item` whose attribute is `cfg` with token_tree text exactly `test`, or of a `function_item` immediately following an `attribute_item` whose attribute text is exactly `test`.
- Regions are found at every depth; a region nested in another is merged into the outer one.
- Region kind is `cfg_test_mod` when the sibling is `mod_item`, `test_fn` for a bare `#[test]` fn, else `cfg_test_item`.
- `source_lines` = file lines minus lines fully inside a region; the size signal and the size reason use `source_lines`.
- A metrics unit whose range lies inside a region is kept and tagged `kind = "test"`; its reason text ends with ` (in inline tests)`; worst_functions ranking for the hotspot score considers only `kind = "source"` units.
- The clone tokenizer emits no tokens for bytes inside a region when `inline_strip_clones` is on; the twin-test analyzer receives region token streams separately.
- `has_inline_tests` is true iff the file has at least one region; report/mod.rs:165 reads it from the tag, not from a string search.
- Only files whose `inline_test_ratio >= report_inline_ratio_above` get the `N in #[cfg(test)] mod at a-b` suffix on the size reason.
- TS, TSX, JS and Python files never get regions in this ticket.

### P55 symbol mention coverage (tier B)

**Measures**
Per Source file: `test_refs_count` = number of test units (test files per discovery, plus P54 inline regions) that contain an exact identifier match for at least one public symbol defined in the file; `unmentioned[]` = public symbols (name, line range) matched by no test unit. Unit: file for the reason; symbol for `--json` only. `has_tests` becomes `test_refs_count >= 1`.

**Why it matters for LLM code**
Again not a tell — inverted: public symbols named by no test unit are kanspec 66/396 (17%), scry 6/17 (35%), ripgrep 201/593 (34%), fd 41/67 (61%), click 95/204 (47%, examples/ counted as Source), hono 236/675 (35%, exported types included). The Claude-written repo has the best mention coverage. The value is fixing `has_tests` for CLI-style suites: on kanspec's 15 hotspots scry says has_tests=false for src/cmd/status.rs and src/cmd/comment.rs, yet 36 and 15 test units name their symbols; the other 12 agree. Conversely, imports do not mean tests: src/ids.rs is imported 23 times. Per-file gaps that survive the noise are rare but real: kanspec src/server.rs router 204-229, request_ctx 243-257 (2/3 unmentioned, 1 test unit reaches the file). Files with zero mentioning test units: kanspec 0, ripgrep 5, fd 6, click 5, hono 20.

**Detection**
Public symbols per Source file, from the metrics units (name + lines already present):
- Rust: `(function_item (visibility_modifier) name: (identifier))`; `pub(crate)` parses as `(visibility_modifier (crate))` and counts (this is a `[[bin]]` crate reality — kanspec's API is crate-visible). Skip units inside `(impl_item trait: (type_identifier) …)` (trait impls are named via the trait) and units inside P54 regions.
- Python: module-level `function_definition` / `class_definition` whose `name: (identifier)` does not start with `_`, in files whose basename does not start with `_`.
- TS/TSX/JS: `(export_statement declaration: (function_declaration | class_declaration | lexical_declaration (variable_declarator name: (identifier) value: (arrow_function))))`. Not `type`/`interface`/plain consts/re-exports (hono's noise source).
Test unit token sets: one `HashSet<&str>` per test unit of Rust `identifier` + `field_identifier`, Python `identifier`, TS `identifier` + `property_identifier`. No string-literal scan (skeptic: it makes `batch`, `open`, `read` always mentioned while the ignore list removes the subcommand names the CLI tests do use). Intersect after removing `ignore_names` and names shorter than `min_name_len`. Exclude files under `examples|docs` path globs. Files whose language is Rust and whose crate has doctests are unaffected: doctests are invisible to tree-sitter, so the design counts `///` fences containing ```` ``` ```` in a file as one test unit that mentions every public symbol of that file (cheap, honest, opt-out).

**Reason line**
File level only, and only for hotspots already ranked: `named by no test unit (0 test files or inline tests reference its 6 public symbols)`. When `test_refs_count >= 1` the multiplier is off and no line prints. The per-symbol list (`src/hooks.rs: plan_install 156-224, plan_remove 226-260, …`) goes to `--json` for t-b342, never to a reason — the prototype's top hits (hooks.rs plan_install, exercised by 23 tests via cmd/init.rs:79; fd src/filesystem.rs 15/16 e2e-covered) show it is true but misleading.

**Knobs**
```toml
[tests]
mention_index = true
ignore_names = ["new","default","run","main","from","into","fmt","build","get","set","push","read","write","parse","exists","clone","delete","commit","execute","handler"]
min_name_len = 5               # 4 still lets execute/commit/is_empty collide
exclude_paths = ["examples/**", "docs/**"]
doctest_counts_as_unit = true  # Rust: a /// fence in the file = one mentioning unit
has_tests_source = "mentions"  # "mentions" | "imports" (today's deps.test_refs) | "either"
```
Auto-disabled when t-2e7a finds a coverage file.

**Objection and answer**
Objection: "named by no test body" is not coverage on a well-tested repo (click 38% of public defs unnamed at ~100% line coverage: StringParamType via click.STRING, UsageError observed as exit codes); pub in bin crates is module visibility; doctests are invisible; tests_quality measures where asserts live (kanspec tests/common 693 lines, ripgrep eqnice!), not test strength. Answer: the design keeps only the file-level step. `tests_quality` and the linear no_tests blend are dropped; the multiplier is a step (0 refs = full 1.15, >=1 = none). Per-symbol output is JSON-only and worded "named by no test unit", never "uncovered". The string scan is gone; the doctest unit closes the ripgrep/globset gap; `pub(crate)` is deliberately kept because the alternative ("lib crates only, pub without (crate)") turns the signal off for exactly the CLI repos scry targets. lcov (t-2e7a) overrides when present.

**Folds into**
Report pass: replaces the boolean `has_tests` source; the x1.15 multiplier is unchanged. `--json`: `test_refs_count`, `unmentioned[]` per file.

**Cost**
Cheap: one token set per test unit, one hash lookup per public symbol.

**Rules**
- A test unit is a discovery Test file, a P54 region, or (Rust, when `doctest_counts_as_unit`) the set of `///` fences in a Source file.
- A public symbol is defined by the grammar rules above; trait-impl methods, symbols inside P54 regions, names in `ignore_names`, and names shorter than `min_name_len` are excluded.
- A test unit references file F iff its identifier set contains any public symbol of F; string literals never count.
- `test_refs_count(F)` = number of referencing units; the doctest unit counts as one.
- `has_tests(F)` = `test_refs_count >= 1` under `"mentions"`; `deps.test_refs > 0` under `"imports"`; either under `"either"`.
- The x1.15 no-tests multiplier applies iff `has_tests` is false; no partial multiplier.
- The reason `named by no test unit (…)` prints only on ranked hotspots with `has_tests = false`; the symbol list appears only in `--json`.
- Files matching `exclude_paths` are neither indexed nor reasoned about.
- When t-2e7a reports a coverage file, `has_tests_source` is ignored and coverage wins.

### P56 stale tests and untested churn (tier B)

**Measures**
Per Source file S over the history window: `substantive_commits` (numstat adds+dels >= `min_lines` in S), `untested_commits` (those touching no Test-classified path), `untested_share`; repo-wide `repo_untested_share` over all substantive Source commits. Unit: file. `stale_commits` (commits to S after its mapped tests last changed) is computed for `--json` only.

**Why it matters for LLM code**
The corpora invert the premise: source-commit rows whose commit touched no Test path are kanspec 85/424 (20%), scry 4/30 (13%), ripgrep 136/228 (60%), fd 82/118 (69%), click 137/389 (35%), hono 48/240 (20%). kanspec commits are large (median 5 files, p90 19, max 108), so they nearly always carry a tests/ file. Layout dominates: with Rust inline-test hunks counted as tested the numbers become kanspec 4/424 (1%), ripgrep 87/228 (38%), fd 36/118 (31%). `stale_commits` never exceeds 6 in any corpus (kanspec max 2 src/lib.rs, ripgrep 4 printer/json.rs vs tests/json.rs 2024-10-16, click 6 parser.py vs test_parser.py 2023-09-01); the one file passing `stale_min_commits=5` with a fix in all six corpora is click parser.py. What survives is the repo-relative contrast: ripgrep crates/ignore/src/pathutil.rs 3/3 untested (July 2026, 'add routine for checking if a path is hidden', 'refactor is_hidden'); click parser.py and utils.py; kanspec src/triage.rs and src/config.rs 2 of 6 substantive commits (33%) against a 19-20% norm — modest, but a linter cannot compute it.

**Detection**
Extend the history pass's single `git log --no-merges --name-only --date=short --format='%H %ad %s' --since=…` to `--numstat` (same walk, adds per-path line counts). Per commit: split paths by discovery FileKind; `tested = any Test path present`; also tested if any path matches `snapshot_globs` (`**/__snapshots__/*.snap`, `**/*.snap`) — snapshot updates are test changes. For each Source path with adds+dels >= `min_lines`: bump `substantive`, bump `untested` if not tested. Rust files with P54 regions: with `inline_hunks = "off"` the file is excluded from the signal (not "always tested" — that is the skeptic's collapse; not "untested" — that reads ripgrep as 60-69% by layout). With `inline_hunks = "on"`, run `git log --no-merges -U0 -p --format=%H -- <file>` once per such file, parse `@@ -a,b +c,d @@`, and mark the commit tested if any hunk's new range overlaps a P54 region located by parsing `git show <rev>:<file>` (~20 s for kanspec's 37 files / 102 commits; opt-in). Exclude Data/Generated files and paths matching `nonproduct_globs` (`docs/**`, `build/**`, `perf-*/**`, `scripts/**`) — ripgrep default_types.rs 26/26, click docs/conf.py 5/8, hono build/build.ts 4/8, perf-measures/…/process-results.ts 6/6 are the prototype's noise. Test->source mapping (for `stale_commits`, JSON only): deps.test_refs, directory-aware stem match (`index.ts`/`mod.rs` use the parent directory name — plain stems gave hono's middleware/*/index.ts zero mapped tests), P55 mention index with `mention_min = 2` or one unique >= 6-char name. t-e6fa renames improve the mapping when it lands.

**Reason line**
`4 of its 7 substantive commits (>= 10 lines) shipped with no test change; repo norm 20%; largest 0a6dcfb (+41/-34 "The prime cap: a ranked, soft, named spec-rules budget")`
Printed as an extra reason on files already ranked as hotspots; never a hotspot of its own.

**Knobs**
```toml
[tests]
untested_churn = true
min_lines = 10                  # substantive commit threshold per file
untested_min_commits = 10
untested_min_delta = 0.25       # file share - repo share, absolute
min_repo_commits = 200          # gate the whole signal below this
inline_hunks = "off"            # "off" excludes Rust inline-test files; "on" inspects hunks
snapshot_globs = ["**/__snapshots__/*.snap", "**/*.snap"]
nonproduct_globs = ["docs/**", "build/**", "scripts/**", "perf-*/**"]
stale_commits_json = true       # stale_commits / last_test_date in --json only
```

**Objection and answer**
Objection: counts of 6-14 make a percentile a coin flip; the inline guard collapses the Rust signal to nothing; `stale_commits` is a repo-age artifact (kanspec's whole history is 2026-08-31..2026-09-13); top hits on human repos are data tables and files whose inline tests moved in the same commit. Answer: no percentile — the file is reported only when its share exceeds the repo norm by an absolute 0.25 with >= 10 substantive commits and the repo has >= 200 commits in the window; `stale_commits` and `last_test_date` are JSON-only; Data/Generated/non-product paths are excluded; inline-test Rust files are excluded unless hunks are inspected, so the signal never reads "always tested". The signal is an extra reason line and carries no score weight. Fix-word matching is not used for gating (the skeptic's 'fix typo' case).

**Folds into**
History pass (one extra numstat column, one extra reason line). `--json`: `substantive_commits`, `untested_commits`, `untested_share`, `repo_untested_share`, `largest_untested_commit`, `stale_commits`, `last_test_date`, `mapped_tests[]`.

**Cost**
Cheap by default (numstat on the same walk). `inline_hunks = "on"` is moderate: one `git log -p` per inline-test file plus `git show` per touching revision.

**Rules**
- The history walk adds `--numstat`; a commit is `tested` iff it touches a Test-classified path or a `snapshot_globs` path.
- A commit is substantive for file S iff adds+dels for S >= `min_lines`.
- `untested_share(S)` = untested substantive commits / substantive commits; `repo_untested_share` is the same ratio over all eligible Source files.
- Files excluded from the signal: Data, Generated, Vendored, `nonproduct_globs`, and Rust files with P54 regions when `inline_hunks = "off"`.
- With `inline_hunks = "on"`, a commit to a region-bearing file is tested if any `@@ … +c,d @@` new-side range overlaps a region of that file at that revision.
- The reason prints iff the repo has >= `min_repo_commits` in the window, S has >= `untested_min_commits` substantive commits, and `untested_share - repo_untested_share >= untested_min_delta`.
- The reason names the largest untested substantive commit (short hash, +adds/-dels, subject).
- The signal adds no score weight and creates no hotspot.
- `stale_commits`, `last_test_date`, `mapped_tests` are emitted in `--json` only; mapping uses deps.test_refs, directory-aware stems, and the P55 mention index.

### P57 test re-implements the oracle (tier C)
Rejected: zero instances in six corpora (~640 test units, 34k+ source lines tokenized; ripgrep's 85 raw runs were all a brace-matcher artifact in defs.rs), and the failure mode reshapes control flow that token winnowing cannot match. If ever revisited it is a `clones.file_kinds = ["source"]` knob with kind labels on clone pairs, added after t-8868 exists and a real instance is found.

### P58 tests section and test-file score (tier B)

**Measures**
Per Test file with >= 1 test unit: `units`, `zero_assert_share`, `weak_share`, `setup_per_assert_median`, `twin_share`, `naming_share`, `lines`, `stale_days` (days since the file's last commit minus the newest commit of its mapped Source files). Each percentile-ranked among the repo's Test files (zero -> 0). Per test unit (JSON): path, name, lines, assertion count, assert macros/methods used, weak flag. Unit: test file (section), test unit (`--json`). Test files with 0 units are listed as helpers, not ranked.

**Why it matters for LLM code**
Weak separation. Prototype per-repo means (after the assertion-vocabulary fixes): kanspec 24 test files (+2 helpers) / 269 units, zero_assert 0.01, weak 0.18, setup/assert median 8.4, sentence-named 0.94, stale median 10 d, score max 51 / median 30; ripgrep 11 (+8 helpers) / 270 units once `rgtest!` is a test macro, zero_assert 0.24 (artifact of `.assert_err()` not in the list), weak 0.01, setup 8.7, naming 0.04, stale 316 d, max 70 / median 29; click 44 (+14) / 557, zero 0.01, weak 0.23, setup 4.1, naming 0.25, max 61 / median 35; hono 134 (+4) / 2777, zero 0.03, weak 0.04, setup 4.5, naming 0.54, max 69 / median 22; fd 1 test file with units -> suppressed. Only the sentence-name share separates (0.94 vs 0.04/0.25/0.54). The best single finding: hono src/compose.test.ts, 546 days since last change while compose.ts moved, plus 'should work with 0 middleware' 505-507 with no expect. The calibration claim (ripgrep near zero) failed: ripgrep tests/regression.rs topped the ranking on setup ratio (r3179_global_gitignore_cwd 226-273, 53 statements per assertion — appropriate for e2e) and stale_days. kanspec tests/cli_well_formed.rs ranked #1 (71) until `Cli::command().debug_assert()` (line 12) was made an assertion (47 after). kanspec tests/single_write_path.rs (44): 5/6 tests all-weak by construction (meta-tests over the source tree).

**Detection**
Test units: Rust `(attribute_item (attribute (identifier)))` text `test` + `function_item`, and `(macro_invocation macro: (identifier))` whose name is in `test_macros` (ripgrep `rgtest!` defines 333 tests); Python module- or class-level `function_definition` with prefix `test_`; TS `call_expression` with callee `it`/`test` (first `string_fragment` argument is the name). Assertions per unit: Rust `macro_invocation` names in `assert_macros` and `field_expression` `field_identifier` names in `assert_methods` (confirmed: `x.assert_err()` parses as `(call_expression function: (field_expression … field: (field_identifier)))`); Python `assert_statement`, `self.assert*` attribute calls, `pytest.raises`; TS `expect(`, `expectTypeOf(`. Weak = assertion whose only predicate is in `weak_predicates` (is_ok/is_some/contains/len; `is None`/`in`/`len(`; toBeDefined/toBeTruthy/toContain). Setup ratio = statements before the first assertion / assertions. Zero-assert, weak, setup, twin and naming come from the per-unit analyzers in the other clusters; this ticket owns only the unit detector, the assertion-vocabulary knobs, the unit rows, `stale_days`, and the section. Test files come from discovery FileKind::Test (t-7634 makes testutils dirs Test; a Test file with 0 units is a helper).

**Reason line**
Single-signal lines, no composite score:
`tests/transition_table.rs — 2 of 27 tests assert only shape (lines 934-960, 1157-1185); 1 test with 71 setup statements for 1 assertion (lines 795-868); unchanged since 2026-08-31 while src/transitions.rs changed 6 times`
Ordered by the count of firing signals, then by the worst percentile among them.

**Knobs**
```toml
[tests]
section = true
min_test_files = 3
top = 10
test_macros = ["rgtest"]
assert_macros = ["assert", "assert_eq", "assert_ne", "debug_assert", "eqnice", "assert_snapshot", "assert_debug_snapshot", "prop_assert", "prop_assert_eq"]
assert_methods = ["assert_err", "assert_exit_code", "debug_assert", "assert_success"]
weak_predicates = ["is_ok", "is_some", "contains", "len", "is_empty", "toBeDefined", "toBeTruthy", "toContain"]
stale_days_reason = 180        # relative to mapped source files, not repo head
signals = ["zero_assert", "weak", "setup", "twins", "stale"]  # naming/mocks/size off by default
```
No `weights` table.

**Objection and answer**
Objection: a weighted sum with no calibration target (no defect ground truth for test files); six of eight inputs do not exist; correctness depends on per-repo assertion vocabulary discovered after a wrong first run; naming/mocks/size are taste (eslint prefer-lowercase-title enforces the opposite); Rust inline-test repos (scry: 0 Test files) get nothing; stale_days imports P56's age artifact. Answer: the composite score is dropped — the section lists test files with single-signal reasons and ranks by how many fire; naming, mocks and size are off by default (naming is exposed as an opt-in signal since it is the only one that separated LLM from human); the assertion vocabulary ships as knobs with the defaults above so ripgrep/hono/click do not outrank kanspec on a first run; `stale_days` is relative to mapped source files and gated at 180 days so a 14-day repo cannot fire; P54 regions count as test units for the JSON rows so t-b342/t-16ca get coverage on inline-only Rust repos even when the file section is suppressed. The per-unit JSON rows are the deliverable that needs no score.

**Folds into**
New report section `tests` (after per-directory summaries) and a `tests[]` array in `--json` with per-file and per-unit rows for t-b342 and t-16ca. No hotspot score weight. Lands after the zero-assert/weak-assert/setup/twin analyzers from the other clusters.

**Cost**
Cheap: aggregation over already-parsed trees and the existing git walk.

**Rules**
- A test unit is a `#[test]` `function_item`, a `macro_invocation` named in `test_macros`, a Python `function_definition` with prefix `test_` at module or class level, or a TS `call_expression` with callee `it`/`test`; P54 regions contribute units to the JSON rows.
- A Test file with 0 units is listed under `helpers` and never ranked.
- Assertions are counted per the `assert_macros`/`assert_methods`/language rules above; unknown vocabulary is a knob gap, so the section prints the top three unrecognized macro/method names seen in test units under a `hint:` line.
- The section prints only when >= `min_test_files` Test files have units.
- Each file gets one reason per firing signal; files are ordered by firing-signal count, ties by the worst percentile; at most `top` files.
- `stale_days` = days between the file's last commit and the newest commit of its mapped Source files (P56 mapping); it fires only when >= `stale_days_reason` and at least one mapped Source file changed since.
- Percentiles are computed among the repo's Test files only; zero signal ranks 0.
- No composite score is computed or printed; `--json` carries raw values and percentiles per signal.
- Per-unit JSON rows carry path, unit name, line range, assertion count, assertion names used, weak flag.
- `naming`, `mocks`, `size` are computed only when listed in `signals`.

## report actionability — dampening, cuts, plans and gate

Four proposals reduce noise in signals scry already prints and turn the survivors into a named edit. Ordering for tickets: P59 (a clone-pass bug fix plus a tagger), P60 (history-pass filter), P61 (deps-pass cut search), then P62 (a rendering layer that consumes the first three). P63 is rejected.

### P59 table-shaped clone dampening (tier A)

**Measures**
Per clone pair: `kind = logic | table`, plus `container_kind` and `entry_count` when table. Per file: `clone_lines` split into `logic_clone_lines` and `table_clone_lines`; `clone_ratio = (logic + table_weight x table) / lines`. Two things at once: (1) a clone-pass fix that drops same-container self-matches outright; (2) a tagger for the pairs that remain. Unit: clone pair and file.

**Why it matters for LLM code**
LLMs emit long uniform dispatch/registry tables and winnowing reports them as the repo's biggest duplicates. Prototype (`p59_tables.py`): kanspec 5/128 pairs tagged table = 9.3% of clone tokens (1080/11554); 4 of the top 10 printed pairs are tables (lib.rs 133-156<->159-182 488 tok, lib.rs 133-144<->145-156 238, error.rs 163-184<->186-207 132, doctor.rs 91-134<->136-174 117) plus cli.rs 83-127<->132-178 (21 enum_variants). Human corpora: scry 0/10, fd 0/11, ripgrep 1/6219 (0.02% of tokens; flags/defs.rs 47-100<->101-155, the FLAGS registry array), click 0/11, hono 1/48 (1.4%; utils/mime.ts 36-61<->62-87). Zero false positives among the 7 tagged pairs. Table share of clone tokens: LLM 9.3% vs human 0-1.4%, but the absolute count is small (5 pairs), so this is a dampener, not a ranking signal.

Correction to the proposal's own evidence: of doctor.rs's 304 clone lines only 83 (27%) are the CHECKS registry; 173 (54%) are inline test functions (lines 966+). With tables alone doctor.rs drops from 17% to about 12% duplicated; the rest needs the inline-test-clone dampening from another cluster.

**Detection**
Part 1 (bug fix, `clones/mod.rs`): the existing `fa == fb` rule (`runs.retain(!overlaps(sa,len,sb,len))`) only catches overlapping diagonals; a uniform run longer than 2 x `min_tokens` matches its own second half and escapes (kanspec lib.rs 133-156<->159-182 and error.rs 163-184<->186-207 are one `match` each). Fix: when `fa == fb`, map both token ranges to byte ranges, find each range's smallest covering named node, and drop the run if both resolve to the same container node (or one is an ancestor of the other).

Part 2 (tagger, runs on every surviving pair, both sides): find the smallest node covering the run's byte range, walk parents until a container is hit. Containers verified with `scry ast`:
- Rust: `match_expression > match_block > match_arm`; `static_item | const_item | let_declaration > ... > array_expression > struct_expression | reference_expression`; `enum_variant_list > enum_variant`; `field_declaration_list > field_declaration`.
- TS: `switch_statement > switch_body > switch_case`; `object > pair`; `array > object`; `interface_body > property_signature`.
- Python: `match_statement > block > case_clause`; `dictionary > pair`; `list > dictionary`; `import_from_statement` runs under `module`.
The skeptic's list of misses (impl_item + `#[test]` function_item siblings under `source_file`, property_signature runs, import runs, `token_tree` in macros) argues for a generic rule: ANY container whose covered children are >= `table_min_entries` consecutive same-kind named siblings. For `match_block` the entry kind is fixed (`match_arm`); for array/list/object/body containers use the most common child kind.

Entry test: the prototype's `table_max_entry_nodes = 12` tagged nothing (one-line dispatch arms have 20-22 named nodes, registry struct entries 17-24). What works is shape uniformity: skeleton-hash each entry (identifiers, literals, scoped paths collapsed to leaves), require the dominant skeleton to cover >= `table_min_dominant_shape` of entries, and no control-flow node inside an entry. Rust `try_expression` (the `?` operator) must NOT be in the control-flow set; it silently killed every dispatch arm in the first run.

**Reason line**
`src/lib.rs:133-182 is a dispatch table (48 match_arms of one shape), not duplicated logic — listed under TABLES`
`src/doctor.rs: 17% duplicated lines were 11% registry table (CHECKS, lines 91-174) and 6% logic`

**Knobs**
```
[clones]
drop_same_container_self_match = true
table_min_entries = 6
table_min_dominant_shape = 0.6
table_max_entry_nodes = 40          # loose safety cap only
table_control_kinds = { rust = ["if_expression","match_expression","for_expression","while_expression","loop_expression","closure_expression"], typescript = [...], python = [...] }
table_weight = 1.0                  # 0.25 is opt-in
list_tables_separately = true
```

**Objection and answer**
Skeptic: the headline pairs are a self-match bug, not a taxonomy problem; the whitelist misses what human tables look like (so tables are not an LLM tell); and `table_weight = 0.25` hides parallel maps over one enum that must drift together (Python dict / TS object have no exhaustiveness check).
Answer, folded in: (1) the self-match is fixed in the clone pass with no new knob; (2) the tagger is generic over any container of uniform siblings, not a whitelist; (3) default `table_weight = 1.0` so parallel-map drift still ranks, with tables listed under their own heading so an agent sees "two parallel enum-to-string maps" rather than "duplicated logic". The 0.25 dampener is opt-in. The LLM-vs-human difference (9.3% vs 0-1.4%) is reported as information, not scored.

**Folds into**
Clone pass (fix), CLONES section (new TABLES sub-heading), per-file clone reason text, `clone_kind` field in `--json`. No score weight change at default.

**Cost**
Cheap: one covering-node walk per run over trees already parsed.

**Rules**
- Clone runs where `fa == fb` and both ranges resolve to the same smallest covering container node (or one contains the other) are dropped before dedupe; no report line is emitted for them.
- After dedupe, each surviving run is tagged on both sides: smallest covering node -> walk up until a node has >= `table_min_entries` consecutive same-kind named children overlapping the run.
- Entry kind: fixed `match_arm` under `match_block`, `switch_case` under `switch_body`, `case_clause` under a `match_statement` block; otherwise the most common child kind among covered children.
- An entry is uniform when its skeleton hash (identifiers, literals, scoped paths, strings, numbers collapsed to a leaf token) equals the dominant skeleton; run is table iff dominant share >= `table_min_dominant_shape` and no entry contains a node in `table_control_kinds` for that grammar and no entry exceeds `table_max_entry_nodes` named nodes.
- `table_control_kinds` must never include Rust `try_expression`.
- A pair is `table` iff both sides tag table; otherwise `logic`.
- Per-file `clone_lines` split into `logic_clone_lines` and `table_clone_lines`; `clone_ratio` uses `logic + table_weight x table`.
- `list_tables_separately = true` prints table pairs under a `TABLES` sub-heading with container kind and entry count; hotspot reasons state the split in whole percent.
- `--json` gains `kind`, `container_kind`, `entry_count` per pair and the two per-file line counts.
- Test: kanspec lib.rs 133-156<->159-182 and error.rs 163-184<->186-207 must vanish; doctor.rs 91-134<->136-174, cli.rs 83-127<->132-178, ripgrep defs.rs 47-100<->101-155, hono mime.ts 36-61<->62-87 must tag table; fd/click/scry top-10 must tag nothing.

### P60 co-change sweep and lift dampening (tier A)

**Measures**
Per commit: `sweep` flag. Per pair: `together` (all commits, unchanged), `together_nonsweep`, `lift = together / (commits_a x commits_b / N)`, `explained_by` (a shared import that changed in the same commits). Per file: at most `max_partners_listed` partner lines. Unit: commit, pair, file.

**Why it matters for LLM code**
Agent sessions edit every command file in one commit, so co-change fires everywhere. Prototype (`p60_cochange.py`, replica matched scry exactly on all six corpora: 49/49, 6/6, 3/3, 0/0, 5/5, 2/2): kanspec (79 commits, median 2 files/commit, p90 12, 3 mass edits): 5 sweep commits (6%); hidden pairs 31 -> 12 after sweep exclusion -> 6 after lift>=3; lift on the 12 survivors min 1.9 / median 3.0 / max 4.9; top-15 hotspots carrying a 'changes together with' line 9 -> 3; share of busy (>=5 commits) src file pairs flagged 31/780 = 4.0% -> 6/780 = 0.8% (the proposal's 33% figure did not reproduce under any denominator). fd (95 commits): 2 hidden pairs before and after, lifts 9.0 and 13.6. ripgrep (78): 0 pairs; click (195): 0 hidden; hono (242): 0 hidden; scry (26): 0 hidden. Sweep commits exist in human repos too (click 1, ripgrep 2 at SMF=1) but never produced hidden pairs.

Dropped pairs are pure noise: decision.rs<->done.rs 5x, done.rs<->flow.rs 5x, done.rs<->quirk.rs 5x, flow.rs<->quirk.rs 5x all have ZERO co-commits left once the 5 sweeps are removed ('Round C integration: land S5+S6' 20 files, 'S3 (scan)...' 76 files, 'Round A integration' 76 files, 'Review pass: fix eleven defects and remove ~780 lines' 49 files, 'architecture: retire the parallel-build ownership rules' 17 files). Survivors are real: assets/app.js<->src/board.rs 4x lift 4.4, src/cmd/decision.rs<->src/cmd/quirk.rs 4x lift 3.5, src/cmd/rules.rs<->src/fm.rs 3x lift 4.9; fd exec/mod.rs<->sanitize.rs 4x lift 9.0, dir_entry.rs<->sanitize.rs 3x lift 13.6.

**Detection**
Same `git log --no-merges --no-renames --name-only --format=%x01%H%x02%an%x02%ct%x02%s --since=` already parsed. Directory sizes from the history tracked set (Source files only; using deps' file list, which includes tests, gave 79 pairs vs scry's 49). Sweep = for some directory with >= `sweep_min_dir_files` tracked files, the commit touches >= `sweep_fraction` of them AND >= `sweep_min_files` absolute (without the floor, hono's 2-file fix commits in 4-file middleware dirs are sweeps: 21/242). Lift from the per-file commit counts already held. Explained filter (skeptic's idea, folded in): for a candidate pair (a,b), look up files both import in the deps graph; if such a file was touched in >= half the pair's co-commits, the pair is `explained_by` that file and is printed as shotgun surgery on the import rather than hidden coupling. The cluster reason is not worth building: after dampening the largest same-dir cluster in any corpus is 2 files.

**Reason line**
`changes together with src/cmd/quirk.rs (4x, 3.5x more often than chance; 2 sweep commits ignored) but neither imports the other`
`changes together with src/cmd/ticket.rs (5x): both import src/ctx.rs, which changed in 4 of those commits`
Hidden-coupling section footer: `5 directory-sweep commits (>= 50% of src/cmd) excluded from pair counts; still counted as churn`.

**Knobs**
```
[history]
sweep_fraction = 0.5
sweep_min_dir_files = 4
sweep_min_files = 6
min_lift = 3.0
min_commits_for_lift = 20
explained_min_share = 0.5
```
`max_partners_listed` is dropped: `[report] reason_hidden_partners` (default 2) already exists.

**Objection and answer**
Skeptic: the two src/cmd sweeps also touched ctx.rs/out.rs/store.rs, so "both changed because ctx.rs changed" is a better, cheaper explanation than dropping the commit; a sweep still counts toward churn (0.45 weight) but not coupling; N=76 makes lift a noisy ratio; colocated-test layouts make every feature commit a sweep.
Answer, folded in: keep sweeps in per-file churn (they are real edits) and in the pair's raw `together`; use the non-sweep count only for the hidden-coupling verdict, and print the exclusion so the number is auditable. Add the explained filter as a first-class reason, which converts the ctx.rs case into a named target. `sweep_min_files = 6` protects the 4-file colocated-test directories. Lift is gated by `min_commits_for_lift` and printed as information below it. The 31 -> 6 drop on kanspec with 2 -> 2 on fd is the evidence that this discriminates rather than deletes.

**Folds into**
History pass (sweep flag, kept for any future shotgun-surgery signal), hidden-coupling section, the coupling component of the score (fewer pairs feed it), `sweep` and `lift`/`explained_by` fields in `--json`.

**Cost**
Cheap: one pass over commits already in memory; union-find not needed.

**Rules**
- `history` tracked set = discovered Source files only.
- A commit is `sweep` iff for some directory D with >= `sweep_min_dir_files` tracked files it touches >= `sweep_fraction x |D|` and >= `sweep_min_files` of them. Commits over `max_cochange_commit_size` files remain excluded as today.
- Sweep commits count toward per-file `commits`, `fix_commits`, authors; they do not count toward `together_nonsweep`.
- Pair rule unchanged (`min_cochange_together`, `min_cochange_strength`) but evaluated on `together_nonsweep` and non-sweep `commits`.
- `lift = together_nonsweep / (commits_a x commits_b / N)` with N = non-sweep commits in the window. When N >= `min_commits_for_lift`, pairs with lift < `min_lift` are dropped; below N, lift is printed but not applied.
- A pair is `explained_by f` when f is imported by both members and f was touched in >= `explained_min_share` of the pair's co-commits; explained pairs leave `hidden_coupling` and print the shared-import line instead.
- Reason text prints raw count, lift to one decimal, and the number of sweep commits ignored for that pair.
- `--json`: per commit `sweep: bool`; per pair `together`, `together_nonsweep`, `lift`, `explained_by`.
- Test: kanspec decision.rs<->done.rs, done.rs<->flow.rs, done.rs<->quirk.rs, flow.rs<->quirk.rs must drop; decision.rs<->quirk.rs must survive; fd's two pairs must survive with lifts 9.0 and 13.6.

### P61 cycle cut suggestion (tier A)

**Measures**
Per file-SCC with size >= `min_cycle_size_to_cut`: for each internal edge, `symbols` (distinct imported names; globs cost `glob_import_symbol_cost`; `mod` declarations infinite) and `largest_after` (largest SCC of the induced subgraph with the edge removed); best single-edge cut, best hub cut (all out-edges of one node), and a greedy cut set that dissolves the SCC. Unit: SCC and edge.

**Why it matters for LLM code**
Agents add `use crate::x` wherever convenient, so an LLM repo tends to have one dense SCC. Prototype (`p61_cut.py`, reproduces scry's 15-file kanspec SCC exactly): kanspec 55 files, 382 edges, one SCC of 15 with 72 internal edges (density 4.8 edges/node), 36 carrying a single symbol, 0 globs; only 5/72 single-edge removals shrink the SCC at all; best: src/paths.rs -> src/plan.rs (1 symbol: EntityRef, line 16) 15 -> 12; src/plan.rs -> src/scan.rs (ScanToken) 15 -> 13; hub cut src/paths.rs (drop 4 imports, 9 symbols) -> 11. fd: SCC of 11, 24 edges (2 mod), 9/22 cuts shrink it; dir_entry.rs -> config.rs (Config) 11 -> 5; hub config.rs (3 symbols) -> 3. ripgrep: SCCs 10/7/5/3; flags SCC of 10 is mod-edge dominated, best use-edge cut only 10 -> 9, hub defs.rs (4 symbols) -> 7; ignore crate SCC of 7: pathutil.rs -> walk.rs (DirEntry) 7 -> 3, hub walk.rs -> 1. scry: no cycles. Today's report puts 'in an import cycle of 15 files' on 4 of kanspec's top-15 hotspots and on 15 of 56 ranked files. Not a separator (human repos have cycles with cheaper cuts, 2.2-2.6 edges/node): the point is the LLM cycle is denser and single-edge cuts barely help, which the report should say instead of repeating the cycle line on every member.

**Detection**
deps already has the edge set and Tarjan. Add symbol counts in the existing import walk: Rust `use_declaration` argument -> `use_list` / `scoped_use_list` / `use_as_clause` / `use_wildcard` / `self`, distinct leaf names per resolved file; `mod_item` without body is a `mod` edge (uncuttable: in ripgrep 9 of the 26 internal edges of the flags SCC are `mod` declarations). TS: `import_statement > import_clause > named_imports > import_specifier` count; `import type` (a `type` keyword child of `import_statement`) and `import_specifier` with a `type` prefix are excluded from cut candidates (erased, no runtime cycle). Python: `import_from_statement` `dotted_name` / `aliased_import` count; `import_statement` = 1. Search: sort internal edges by cost, remove each, rerun Tarjan on the induced subgraph (72 edges x 15 nodes is microseconds), keep the best (smallest largest_after, then cost). Hub cut: remove all non-`mod` out-edges of each node. Greedy cut set: repeat best-single-edge on the residual until largest component < `min_cycle_size_to_cut` or the set exceeds `max_cut_set`; report the set only if it terminates.

**Reason line**
CYCLES section: `15-file cycle in src: cheapest cut src/paths.rs -> src/plan.rs imports 1 symbol (EntityRef, line 16) -> largest remaining cycle 12; hub cut: drop paths.rs's 4 imports (9 symbols) -> 11; no single import breaks this cycle`
Hotspot reason (deduped): `in the 15-file src cycle (cut: paths.rs -> plan.rs, EntityRef)` on members; only members on a cut edge carry the symbol.

**Knobs**
```
[deps]
min_cycle_size_to_cut = 4
max_edges_tried = 400
max_cut_set = 6
glob_import_symbol_cost = 20
report_hub_cut = true
dedupe_cycle_reason = true
cut_rust_cycles = true            # skeptic asked for false; see below
ignore_type_only_imports = true
```

**Objection and answer**
Skeptic: import cycles are not an LLM tell and in Rust/Python are not a defect; greedy single-edge removal is a weak feedback-arc-set approximation and "15 -> 12" is not an action; symbol cost misses inline `crate::x::Y` paths and `use super::*`; a 1-symbol edge can be the most central dependency.
Answer, folded in: `dedupe_cycle_reason` ships regardless, because the repeated line is noise. Cut suggestions are ranked by resulting size and print the symbol names so the agent judges centrality (fd dir_entry.rs -> config.rs is a real tension but semantically central; the line must say `Config`). The greedy cut set is reported only when it dissolves the SCC; otherwise the report says "no single import breaks this cycle" and gives the hub. Type-only TS imports are ignored. Rust stays on by default because the existing `cycle_coupling` floor already scores Rust cycles; if the maintainer accepts the "idiomatic" argument the right change is to that floor, not to hide the cut, and `cut_rust_cycles = false` is available. Inline paths and globs are a known undercount; globs get a fixed high cost and the reason says `>= N symbols` for them.

**Folds into**
Deps pass (symbol counts, `mod` edge kind), CYCLES section (one block per SCC), hotspot reason dedupe, `cycles[].cuts` in `--json`. No score change.

**Cost**
Moderate to implement (symbol counting per grammar), negligible at runtime.

**Rules**
- Each edge carries `kind: use | mod | type_only` and `symbols: u32`; `mod` edges cost infinite; `use_wildcard` / `from x import *` cost `glob_import_symbol_cost`; `type_only` edges are excluded from the SCC used for cuts when `ignore_type_only_imports`.
- For each file SCC with `members.len() >= min_cycle_size_to_cut`: try up to `max_edges_tried` internal edges in ascending cost; record `largest_after`; best = min (largest_after, cost).
- Hub cut per member = remove all its non-`mod` out-edges inside the SCC; report when `report_hub_cut` and it beats the best single edge.
- Greedy cut set = iterate best-single-edge on the residual subgraph until largest component < `min_cycle_size_to_cut`; report only if reached within `max_cut_set` edges, listing every edge with its symbols.
- If best single `largest_after >= 0.8 x size`, append `no single import breaks this cycle`.
- With `dedupe_cycle_reason`, the CYCLES section prints the full block once; hotspot reasons print `in the N-file <dir> cycle (cut: a -> b, Sym)` and never the count alone.
- Rust: `cut_rust_cycles = false` suppresses the cut text but keeps the deduped count line.
- Test: kanspec best single = paths.rs -> plan.rs (EntityRef) 15 -> 12, hub paths.rs -> 11; fd dir_entry.rs -> config.rs 11 -> 5; ripgrep flags SCC must never propose a `mod` edge; scry has no cycles.

### P62 file refactor plan (tier B)

**Measures**
Per hotspot file: `plan: [{n, kind, symbol, lines, target}]` where kind is in {`canonicalise_clone`, `fold_test_clones`, `extract`, `cut_cycle_edge`} today, extended only as producing passes land (`table` from P59 is a tag on clone steps, not a step). No `expected_effect`. Clone runs resolve to their enclosing unit. Unit: file and step.

**Why it matters for LLM code**
The kanspec report says of src/cmd/flow.rs only that it co-changes with ticket.rs and quirk.rs, while the CLONES section lists flow.rs:932-962 <-> 1010-1036 with no symbol. Prototype (`p62_plan.py`): that pair (239 tok) resolves to `park` and `drop_ticket` (the proposal's plan_park/plan_drop names are wrong; metrics knows them as park/drop_ticket); 842-858<->928-942 = ship<->park. Of kanspec's clone runs in the top hotspots, 19/21 in derive.rs, 13/19 in doctor.rs, 8/11 in done.rs and 2/34 in flow.rs sit inside `mod tests` (184/210, 173/323, 131/182, 16/443 run-lines); proposal.rs has 18 runs, most at top level (no enclosing function, printed as '?'). Expected-effect is not honest under a percentile table: moving flow.rs's 405 test lines out changed its score 72.8 -> 82.5 and rank 6 -> 3 (lines 1469 -> 1065, clone_ratio 0.14 -> 0.18 so a new '18% duplicated' reason appeared, and 'no test file references it' fired because tests/flow_moved.rs's `use super::*` no longer resolves). The proposal's 'score 72.8 -> ~55' has the wrong sign. Canonical-copy tiebreak is vacuous: clone pairs are overwhelmingly within one file (kanspec 14/15, fd 11/11, click 11/11, ripgrep 10/10 of the listed pairs).

**Detection**
Post-report over data already in the report input. Clone run -> enclosing unit = smallest metrics unit whose [start_line, end_line] contains the run's start line; fallback = nearest preceding top-level named item (Rust `function_item | impl_item | struct_item | enum_item | static_item | const_item`, TS `function_declaration | class_declaration | lexical_declaration`, Python `function_definition | class_definition | expression_statement` with assignment). Inline test region = Rust `mod_item` with body whose previous named sibling is an `attribute_item` containing `cfg(test)`; runs inside it (or inside a Test-classified file) fold into one `fold_test_clones` step per file. `extract` step = each unit over `cognitive_hard`, worst first. `cut_cycle_edge` = P61's best cut when the file is on that edge. Steps ordered by fixed kind priority, then by tokens/cognitive within kind. `scry plan <file>` prints one file's plan.

**Reason line**
`plan for src/cmd/flow.rs: 1) park (932-962) duplicates drop_ticket (1010-1036), 239 tokens — keep one, parameterise on the verb; 2) ship (842-858) duplicates park (928-942), 101 tokens; 3) extract from ladder (cognitive 32, lines 410-612, nesting 4)`
CLONES section line: `src/cmd/flow.rs park (932-962) <-> drop_ticket (1010-1036), 239 tokens`.

**Knobs**
```
[plan]
max_steps = 6
fold_test_clones = true
symbol_fallback = "preceding_item"     # or "none"
kind_priority = ["fold_test_clones", "canonicalise_clone", "extract", "cut_cycle_edge"]
include_in_text_report = true
```

**Objection and answer**
Skeptic: a rendering layer over passes that do not exist (sections, clumps, process comments, move-tests); expected-effect is fake precision under percentiles; the canonical tiebreak collapses to "the earlier one"; move_inline_tests is hostile to the Rust idiom.
Answer, folded in: ship only the step kinds whose inputs exist (clone symbols, worst function, P61 cut) and add kinds as passes land; drop `expected_effect`, `min_expected_delta` and `canonical_tiebreak` entirely (within-file pairs list both symbols and say "keep one"); no move-tests step — test clones become a single fold step that names the region. The verified, still-standing piece is symbol resolution in the CLONES section and hotspot reasons, which is what t-b342 and t-16ca need to consume. Separate follow-up for the scorer: count `use super::*` from tests/ files as a test reference, or the no-tests reason contradicts any test-moving advice.

**Folds into**
CLONES section (symbol names on every pair), a PLAN sub-heading per hotspot, `plan` array in `--json`, new `scry plan <file>` subcommand. No score change.

**Cost**
Cheap: table lookups over report input; one tree walk per file for the test-region and fallback item.

**Rules**
- Every clone run (both sides) resolves to `symbol`: the innermost metrics unit containing its start line, else with `symbol_fallback = preceding_item` the nearest preceding top-level named item, else `?`.
- CLONES section and hotspot clone reasons print `symbol (lines)` on both sides; table pairs (P59) print `table` after the symbol.
- Runs whose start lies inside a `cfg(test)` module or a Test-classified file are excluded from `canonicalise_clone`; when `fold_test_clones`, they produce at most one `fold_test_clones` step per file naming the region and run count.
- `canonicalise_clone` steps are per pair, descending tokens; within-file pairs name both symbols and say `keep one`; cross-file pairs name the file of each copy and never pick a canonical.
- `extract` steps: one per unit with cognitive > `metrics.cognitive_hard`, descending cognitive, quoting cognitive, lines and nesting.
- `cut_cycle_edge`: present only when P61 reports a cut edge with this file as source; text is P61's cut line.
- Steps ordered by `kind_priority` then within-kind metric; truncated at `max_steps` with `(+N more)`.
- No step carries a predicted score, ratio or rank.
- `scry plan <file>` = the scan pipeline with output restricted to that file's plan; exit 0 with `no plan` when empty.
- Test: kanspec flow.rs 932-962<->1010-1036 must print `park <-> drop_ticket`; derive.rs's 19 test-module runs must collapse to one fold step; proposal.rs runs must show a preceding item name, not `?`.

### P63 generated-code index and CI gate (tier C)

Rejected: it measures provenance, not risk, and its components do not separate on the corpora (inline tests 0.17-0.24 across all four Rust repos, table clones top every human clone list, delete/add tracks repo age, co-change share tracks commit granularity), leaving comment-style rates that are a pre-commit regex, not a cross-file signal; the only survivable residue is a plain `--gate` over existing signals, which is a delivery feature of t-4569, not an analyzer.

## declared-surface-manifest
All three proposals are one `[declared]` pass: parse manifests once, build one identifier-reference index over the existing trees, emit three verdicts (orphan dep, dead feature, unread knob) into one new report section "Declared but unconsumed". Ship Cargo/Rust first; package.json and pyproject sit behind `languages` off by default.

### P64 orphaned and misplaced dependencies (tier A)

**Measures**
Unit = (manifest, dependency). refs = parsed reference nodes naming the crate in the manifest's scope (files whose nearest manifest up the tree is this one, skipping nested manifest dirs). Orphan when refs == 0. Per orphan: birth commit of the manifest line, ever_imported (any `ident::` in scope history), commits since birth, and doc mentions of the dep name in `*.md`. Placement (behind `check_placement`): refs split into prod vs test-gated; misplaced when refs_prod == 0 and refs_test > 0.

**Why it matters for LLM code**
kanspec Cargo.toml:39 `pulldown-cmark = { version = "0.13.4", default-features = false, features = ["html"] }` — copied from the DESIGN.md:632 stack list ('**pulldown-cmark** for markdown rendering'); `grep -rn pulldown src tests` is empty and `git log -S'pulldown_cmark' -- src` is empty across all 102 commits since the root commit 988d9b5. The agent declared the stack the design doc listed and never built the renderer. Prototype: kanspec 2/24 declared deps orphaned (8.3%: pulldown-cmark, rusqlite; both born in root commit 988d9b5, never imported across 102 commits, 0 manifest touches since); scry 0/12; fd 1/24 (4.2%: libc, Cargo.toml:63 under `[target.'cfg(all(unix, not(target_os="redox")))']`, no `libc` token anywhere in fd/**/*.rs — a human leftover, nix covers it); ripgrep 1/81 across 12 manifests (1.2%: crates/index fst, born be739c7 2026-07-20, crate carries `#![allow(warnings)]` at src/lib.rs:1). LLM total 2/36 = 5.6% vs human 2/105 = 1.9%. Placement: 0 true positives in all 4 repos. Absolute counts are tiny; the separation is by kind (design-doc-declared, born-and-never-touched) more than by rate. hono: 24 devDependencies, 4 "orphans" all consumed implicitly (vitest --coverage plugin, nested runtime-tests/bun/tsconfig.json, vitest.config.ts oxc plugin) = 4/24 FP if dev-deps were on. click: 8 dependency-groups, 7 tool-only groups.

**Detection**
Rust (verified with `scry ast`):
- `use_declaration argument:` — recurse through `scoped_use_list(path:, list: use_list)`, `scoped_identifier(path:, name:)`, `use_list`, `use_as_clause(path:, alias:)`, bare `identifier`; leftmost identifier of each leaf path = crate. A `scoped_identifier` with absent path (`::log::debug!`) → name is the crate.
- `scoped_identifier path: (identifier)` anywhere (incl. under `generic_function function:`), `scoped_type_identifier path: (identifier)` (type position — `regex::Regex` in a signature is NOT a scoped_identifier), `macro_invocation macro: (scoped_identifier)`, `extern_crate_declaration name: (identifier)`.
- Every `token_tree` (attribute arguments AND macro bodies — `lazy_static!`, `format!`, `assert_eq!`): an `identifier` whose next anonymous sibling is `::`. `#[derive(clap::Parser)]` is four sibling identifiers in a token_tree.
- In-code name is the manifest KEY; `package = "..."` is the registry name (`memmap = { package = "memmap2" }` is used as `memmap::Mmap`). `-` → `_`.
- `[workspace.dependencies]`: consumers are member manifests' `x.workspace = true` / `x = { workspace = true }` lines; resolve to the member and count refs there.
- Crate exempt if its lib.rs/main.rs has an inner `attribute_item` with `allow(warnings)` or `allow(unused)`.
- Placement split: discovery Source/Test label, `#[cfg(test)]` ancestor `attribute_item`, `#[test]` functions, and `#[cfg(test)] mod x;` (attribute_item before a body-less `mod_item`) propagating to the resolved file (ripgrep searcher/src/testutil.rs).
Python (`languages` includes "python"): `import_statement name: (dotted_name|aliased_import name: (dotted_name))` first identifier; `import_from_statement module_name: (dotted_name)`; `relative_import` skipped; `if_statement condition: (identifier)`=TYPE_CHECKING marks test-like. Needs a real distribution→module table before default-on.
TS/JS: `import_statement source: (string (string_fragment))`, `export_statement … source:`, `call_expression function: (identifier)`=require, `call_expression function: (import)` (dynamic import is NOT an identifier). Package = first segment or `@scope/name`; `@types/*` exempt; names in package.json `scripts` values or any `tool_config_globs` file count as consumed.
Git (orphans only): `git log --format=%h%x09%ad --date=short -S'<key>' -- <manifest> | tail -1` → birth; `git log --format=%h -S'<ident>::' -- <scope> | head -1` → ever_imported / last commit that used it.

**Reason line**
`pulldown-cmark declared in Cargo.toml:39 since 988d9b5 (14 days, 102 commits) and never imported by any src/ or tests/ file; listed as the markdown renderer in DESIGN.md:632 — wire it or drop it (and the design line)`
Template: `<dep> declared in <manifest>:<line> since <hash> (<age>, <n> commits) and {never imported by any <scope> file | last used in <hash>, removed in <hash>}[; mentioned in <doc>:<line>] — wire it or drop it`
Placement: `tempfile is declared in [dependencies] (Cargo.toml:41) but its only 3 references are in tests/lifecycle.rs and a #[cfg(test)] module in src/store.rs — move it to [dev-dependencies]`

**Knobs**
```toml
[declared]
languages = ["rust"]                       # add "javascript", "python" to opt in
manifest_globs = ["**/Cargo.toml", "**/package.json", "**/pyproject.toml"]
check_dev_dependencies = false
check_extras = false
check_placement = false
side_effect_deps = ["*-sys", "tikv-jemallocator", "openssl", "getrandom", "@vitest/coverage-*", "tslib", "core-js", "react"]
import_aliases = { pillow = "PIL", beautifulsoup4 = "bs4", pyyaml = "yaml" }
tool_config_globs = ["*.config.*", "**/tsconfig*.json", ".eslintrc*", ".github/workflows/*", "Dockerfile*", "Procfile", "Makefile", "justfile"]
doc_globs = ["*.md", "docs/**"]
min_age_commits = 5
weight = 0.0
```

**Objection and answer**
Objection: cargo-machete / knip / deptry / rustc `unused_crate_dependencies` already do the zero-reference test; the Rust detection misses type-position paths and macro bodies, `[workspace.dependencies]` would all be orphans, fd's libc breaks the proposal's own baseline claim, and Python/JS without an environment drown in name-mismatch and string-loaded-package FPs. Answer, folded in: Rust-only by default; `scoped_type_identifier` and every `token_tree` scanned; workspace deps resolved through member `workspace = true` lines; `side_effect_deps` pre-populated; `allow(warnings)` crates exempt; libc is a real orphan and prints as one (nix covers it — the reason says so via ever_imported=never). The reason leads with what machete cannot print: birth commit, age, whether an import ever existed and which commit removed it, and the doc mention that turns "orphan" into "planned and never built". JS/Python stay opt-in until a distribution→module table and a Dockerfile/Procfile scan exist.

**Folds into** New report section "Declared but unconsumed" (shared with P65/P66); JSON `declared.orphans[]`; weight 0.0 in the composite (manifests are not Source files and are not ranked). Feeds P65's `dep:x` edge.

**Cost** Cheap: one toml parse per manifest, one lookup in an identifier index the deps pass already implies, two git subprocesses per orphan.

**Rules**
- Scope of a manifest = discovered files whose nearest ancestor manifest is it; nested manifest dirs excluded.
- Parse `[dependencies]`, `[build-dependencies]`, `[target.*.dependencies]`; `[dev-dependencies]` only with `check_dev_dependencies`; `[workspace.dependencies]` resolved to members via `workspace = true`.
- Identifier = table key with `-`→`_`; never the `package` value.
- Reference node kinds: as listed under Detection; token_tree rule applies to every token_tree in the file.
- Optional deps (`optional = true`) are never reported here; hand them to P65.
- Skip crates whose root file carries `#![allow(warnings)]`/`#![allow(unused)]`; skip names matching `side_effect_deps` (glob).
- Skip deps whose birth is < `min_age_commits` commits ago.
- Orphan when refs == 0 in scope (Source + Test). Run git enrichment only for orphans.
- Placement only with `check_placement`; requires `#[cfg(test)] mod x;` file propagation; no section line unless it fires.
- One line per orphan, sorted by age desc; include doc mention when a `doc_globs` file contains the dep name.
- Never print a line when zero orphans; the section is absent, not empty.

### P65 dead feature flag (tier A)

**Measures**
Unit = (Cargo.toml, feature). Elements classified: `dep:x`, forwarding `x/f` / `x?/f`, sibling feature, implicit feature of an optional dep. consumers = cfg references in crate scope + build.rs `CARGO_FEATURE_*` reads + `required-features` manifest entries + docs.rs metadata. dead when consumers == 0 AND every element is a `dep:x` with x orphaned (P64) or a transitively dead sibling. noop when element list empty and consumers == 0. Forwarding = alive. `default` never reported. History: last commit touching a consumer → "orphaned by <hash>" vs "never consumed".

**Why it matters for LLM code**
kanspec Cargo.toml:26 `ci-homerunner = ["dep:rusqlite"]` with the comment at :23-25 'v0.2 ... Moves into default when ci.rs lands'. `grep -rn 'feature = "ci-homerunner"' src tests` is empty, rusqlite (Cargo.toml:58, optional) has zero imports, and src/ci.rs exists (born 988d9b5) — its module doc at ci.rs:7 still says 'rusqlite sits behind the non-default ci-homerunner feature'; src/cmd/flow.rs:839 repeats it. `git log -S'feature = "ci-homerunner"' -- src` lists 988d9b5, 6a674af and 9f2f0f7 (2026-09-02, 'Review pass: fix eleven defects and remove ~780 lines of duplication'), whose diff deletes the only `#[cfg(feature = "ci-homerunner")]` line — the cleanup pass removed the consumer and left the manifest, the optional dep and the config section behind. Prototype: kanspec 1/1 non-default feature dead; scry no [features]; fd 3 features, 0 dead (use-jemalloc 1 consumer, completions 5 consumers + 16 CI mentions, `base = ["use-jemalloc"]` alive transitively); ripgrep 13 features over 6 manifests: 7 alive (pcre2 forwarding 10 consumers, unstable-index 9, printer serde 5, globset arbitrary 4, serde1 1, grep pcre2 1), 6 noop `= []` with DEPRECATED comments suppressed, 0 dead. Human 0/16 dead; LLM 1/1. n=1, so the claim is precision, not statistics.

**Detection**
- `attribute_item (attribute (identifier)=cfg|cfg_attr arguments: (token_tree …))`: recursive scan for `identifier`=feature followed by `=` and `string_literal (string_content)`; nested `all/any/not` fall out of the recursion.
- `macro_invocation macro: (identifier)=cfg (token_tree (identifier)=feature (string_literal …))` for `cfg!(…)`.
- Any other `token_tree` (proc-macro `quote!` bodies) containing the same triple counts.
- build.rs: `string_literal` `CARGO_FEATURE_<NAME uppercased, -→_>` inside `call_expression arguments`; `println!("cargo:rustc-cfg=…")` bodies naming the feature.
- Manifest consumers: `required-features = […]` on `[[bin]]`/`[[example]]`/`[[test]]`/`[[bench]]`; `[package.metadata.docs.rs] features`; `[package.metadata.cargo-all-features]`. Other workspace manifests with `features = ["x"]` on this crate are printed as "enabled by <manifest>" but do not make it alive.
- Scope includes `examples/`, `benches/`, `tests/` regardless of discovery label.
- CI mentions: `feature_ci_globs` grep for the name → "consumed only by CI", still reported.
- Sibling liveness evaluated transitively (fd `base = ["use-jemalloc"]`).
- `git log --format=%h%x09%ad -S'feature = "<name>"' -- <scope>` for the killing commit; dead features only.

**Reason line**
`feature ci-homerunner (Cargo.toml:26) gates optional dep rusqlite (Cargo.toml:58) and nothing in src/ or tests/ checks cfg(feature = "ci-homerunner"); its last consumer was removed in 9f2f0f7 (2026-09-02) and the comment at Cargo.toml:23-25 promises 'moves into default when ci.rs lands' — src/ci.rs has existed since 988d9b5 without it`
Template: `feature <name> (<manifest>:<line>) gates optional dep <x> (<manifest>:<line>) and nothing in <scope> checks cfg(feature = "<name>"); {last consumer removed in <hash> (<date>) | never consumed}[; consumed only by CI: <file>]`

**Knobs**
```toml
[declared]
report_noop_features = false
report_bare_dead_features = false        # dead feature gating no dep: one-line cleanup, off by default
feature_ci_globs = [".github/workflows/*", "Makefile", "justfile", "**/*.sh"]
check_extras = false
min_age_commits = 5
```

**Objection and answer**
Objection: Cargo-only, fires about once per LLM repo and never on fd/ripgrep; misses `required-features`, proc-macro-emitted cfg, examples/-scoped consumers; and cleanup cruft is not a defect predictor. Answer: all three consumer classes are in the detection list above; examples/benches/tests are always in scope; the section only prints when the dead feature gates an orphaned `dep:` (bare dead features behind `report_bare_dead_features`). The value is precision: 0 FP on 16 human features, no static tool covers `[features]`, and "removed in 9f2f0f7" points the agent at the exact cleanup that left the stale docs at ci.rs:7 and flow.rs:839.

**Folds into** Second verdict in the "Declared but unconsumed" section; JSON `declared.dead_features[]`. Dedupe: an optional dep gated only by a dead feature prints here, never in P64.

**Cost** Cheap: shares the manifest parse and the P64 index; one regex-grade token_tree walk; one git -S per dead feature.

**Rules**
- Parse `[features]` per manifest; classify elements; `default` skipped.
- alive if any element forwards (`x/f`, `x?/f`), any consumer exists, or any sibling element is alive (fixpoint).
- noop if elements empty and consumers == 0; reported only with `report_noop_features`.
- dead if not alive, not noop, and every element resolves to an orphaned dep or a dead sibling.
- Print dead features gating >= 1 orphaned `dep:x`; others only with `report_bare_dead_features`.
- CI-only consumers downgrade the line, never suppress it.
- Skip features whose manifest line is younger than `min_age_commits`.
- Python extras: only with `check_extras`; dead iff every package in the extra is a P64 orphan.

### P66 unread config knob (tier B)

**Measures**
Unit = config struct field, rolled up to the section when the parent field is unread. Candidates: a struct that is the type target of `toml::from_str` / `serde_*::from_*` / `BaseSettings` / `z.object(…).parse`, OR a `Deserialize`-derived struct in a file matching `config_file_regex` or named by `config_struct_regex`. reads = field accesses and destructures anywhere in the repo, excluding `impl Default`, `impl Serialize`, derive output and test-gated code (those print "read only by tests"). Unread when reads == 0. Enrichment: field/section name inside a toml/yaml fence in `public_doc_globs`.

**Why it matters for LLM code**
kanspec src/config.rs:178-183 `pub struct CiCfg { pub provider: CiProvider, pub homerunner: HomerunnerCfg }` and :197-203 `pub struct HomerunnerCfg { pub bin: PathBuf, pub db: PathBuf, pub api: String }`, both `#[derive(Deserialize)]` with `#[serde(default, deny_unknown_fields)]`. `grep -rn '\.homerunner\|HomerunnerCfg' src` outside config.rs: 0 hits (ci.rs reads `~/.config/homerunner/config.toml` directly at ci.rs:107; only `ctx.cfg.ci.provider` is read at ci.rs:71,177). The knobs are documented at docs/config.md:91-93 as `[ci.homerunner] bin = ... db = ...`. Every other config.rs field has >= 1 read. Prototype, default mode: kanspec 8 candidate structs / 33 fields / 4 unread (12%), rolling up to one section; scry 8 structs / 49 fields / 0 unread; fd 0 candidates; ripgrep 0 candidates; click 0; hono 0; registry serde_json 1 struct / 1 field / 0 unread. Wide mode (any Deserialize struct): kanspec 31 structs / 160 fields / 10 unread + 1 test-only, of which 4 real, 5 noise from same-file reads (lock.rs LockOwner.host, ci.rs CiStatus.job, gh.rs PrInfo.merged_at, cache.rs GitState.version) and 1 (ci.rs:51 CiStatus.local_only) that belongs to P03. Precision 100% (4/4) in default mode, 40% wide. Human-vs-LLM separation is untested: no human corpus on disk has a serde/pydantic/zod config struct. Tier B for that reason.

**Detection**
Rust: `attribute_item (attribute (identifier)=derive arguments: (token_tree (identifier)=Deserialize …))`; `struct_item name: (type_identifier) body: (field_declaration_list (field_declaration name: (field_identifier) type: …))`; consumption-site selector `generic_function type_arguments: (type_arguments (type_identifier))` on `toml::from_str` etc. Reads: `field_expression field: (field_identifier)` at every nesting level; `struct_pattern (field_pattern name: (shorthand_field_identifier | field_identifier))` — `remaining_field_pattern` reads only the named fields; `index_expression` with a matching `string_literal` (Value walking). Section roll-up = field whose type_identifier is another candidate. Exclusions: `impl_item` whose trait is `Default` or `Serialize`; `#[cfg(test)]` ancestors and `#[cfg(test)] mod x;` propagation (from P64); Test files.
Python: `class_definition superclasses: (argument_list (identifier))` in {BaseSettings, BaseModel} or `@dataclass`; fields = class-body `assignment left: (identifier) type:`. Reads: `attribute attribute: (identifier)`, `subscript subscript: (string (string_content))`; any `getattr`/`vars`/`asdict`/`model_dump`/`dict`/`**` applied to an identifier bound to the class marks all fields read.
TS: `interface_declaration body: (interface_body (property_signature name: (property_identifier)))`, `z.object({..})` keys as `object (pair key: (property_identifier))`. Reads: `member_expression property: (property_identifier)`, `subscript_expression index: (string (string_fragment))`, `object_pattern (shorthand_property_identifier_pattern | pair_pattern key:)` in `const { a } = cfg` and formal parameters (hono src/hono-base.ts:173, src/utils/body.ts:104).
Field names shorter than `min_field_name_len` are skipped only when they collide with another candidate's field (collisions produce false negatives, never false positives).

**Reason line**
`[ci.homerunner] (src/config.rs:182-203, 4 knobs: homerunner, bin, db, api) is deserialised from kanspec.toml under deny_unknown_fields and documented at docs/config.md:91-93, but no code reads any of them — the program accepts the section and ignores it`
Template: `[<section>] (<file>:<lines>, <n> knobs: <names>) is deserialised from <source>[ and documented at <doc>:<lines>], but no code reads {any of them | <field>} — the program accepts it and ignores it`; milder: `<Struct>.<field> (<file>:<line>) is read only by tests`

**Knobs**
```toml
[declared]
config_file_regex = "(^|/)(config|settings|cfg|options)[^/]*\\.(rs|py|ts|tsx|js)$"
config_struct_regex = "(Cfg|Config|Settings|Options)$"
config_is_public_api = false
require_bin_target = true             # skip lib-only crates unless config_is_public_api is set explicitly
require_sibling_read_or_doc = true    # report only if the struct has >= 1 read field or the knob is documented
public_doc_globs = ["README*", "docs/**", "*.1", "man/**"]
min_field_name_len = 3
weight = 0.0
```

**Objection and answer**
Objection: without types "read" is a name match; excluding the defining file drops the accessor/validate/merge methods that live beside the struct (scry config.rs:27-56, kanspec config.rs:223-282); TS destructuring and Python dynamic reads are invisible; the filename selector catches DTOs and misses config in `cli.rs`; and the 0-FP baseline is vacuous because no human corpus has a candidate. Answer, folded in: reads are counted everywhere including the defining file, with only Default/Serialize impls and test-gated code excluded; destructuring and dynamic-read forms are in the read list; candidates are selected by consumption site first, filename/struct-name second; lib-only crates are skipped; and a finding prints only when the struct is demonstrably consumed elsewhere (>= 1 read sibling) or the knob is documented — the documented-dead-knob case is the one worth a line, and no linter in any of the four languages reports it (rustc dead_code is silenced by `pub` in a lib target and by `derive(Serialize)`).

**Folds into** Third verdict in "Declared but unconsumed"; also attached as a weight-0 reason on the config file's hotspot entry; JSON `declared.unread_knobs[]`. Weight stays 0.0 until a human corpus with a serde/pydantic/zod config struct exists to calibrate against.

**Cost** Cheap: one walk over already-parsed trees; a per-repo map field-name → read count; a grep over doc fences per unread knob.

**Rules**
- Candidate struct: type target of a deserialise call, else Deserialize-derived struct in a `config_file_regex` file or matching `config_struct_regex`; Python BaseSettings/BaseModel/@dataclass and TS interface/zod under the same file/name rule.
- Skip crates with no `[[bin]]`/src/main.rs when `require_bin_target` and `config_is_public_api` is unset.
- reads counted in every file; excluded: `impl Default`, `impl Serialize`, `#[serde(skip)]` fields, Test files and `#[cfg(test)]` code (counted separately as test reads).
- Any dynamic whole-struct read (`getattr`, `vars`, `asdict`, `model_dump`, `**x`, `toml::Value` walk with no literal) marks every field read.
- Field with reads == 0 and test_reads > 0 → "read only by tests" line, never scored.
- Unread parent whose type is a candidate → report the section once; knob count = parent + descendants.
- With `require_sibling_read_or_doc`: print only if the struct has >= 1 read field or the knob appears in a `public_doc_globs` fence.
- Line names struct, file, line range, knob count and names, source (manifest/file it is deserialised from), doc location when found.
- `min_field_name_len` collision rule: skip a short name only if another candidate declares the same field.

## declared-surface-docs-and-rollup
### P67 test-only env seam (tier A)

**Measures**
Unit = env var name, emitted once per name with every production read site attached (kanspec has exactly one read per name). Class A (the finding): a production read of a product-prefixed or test-keyword env var whose only setters/mentions are test files or Test-classified dirs, with zero mentions in user-facing docs and no production setter. Class B (narrowed, own reason, ranked below A): a test-mode branch in a function body — `cfg!(test)`, `process.env.NODE_ENV === 'test'`, `'pytest' in sys.modules` — or a `#[cfg(not(test))]` item that has a `#[cfg(test)]` twin of the same name. Never bare `#[cfg(test)]` on an item. Reads inside `#[cfg(test)] mod` / `#[test] fn` are masked (brace-matched, not rest-of-file). Enrichment: birth commit of the read and of the first test setter.

**Why it matters for LLM code**
An agent asked to add a clock or identity seam reaches for the process environment because it needs no plumbing; the test that exercises it lands in the same commit. kanspec production reads: src/ctx.rs:316 KANSPEC_NOW, src/ctx.rs:49-50 KANSPEC_ACTOR / KANSPEC_ACTOR_KIND (doc comment at ctx.rs:39 literally says '(tests)'), src/store.rs:889 KANSPEC_ID_SEED, src/gh.rs:194 + src/scan.rs:1246 KANSPEC_GH_FIXTURES, src/cmd/up.rs:158 KANSPEC_NO_BROWSER. Test mentions: NOW 13, ACTOR 8, ACTOR_KIND 10, ID_SEED 5, GH_FIXTURES 8, NO_BROWSER 3. User-doc mentions (docs/, README): 0 for all six; ARCHITECTURE.md:2125-2127 describes them as 'the one and only mock seam'. Birth: KANSPEC_NO_BROWSER read and its test setter both born in 32e367a (2026-08-31); KANSPEC_ID_SEED read and test in 988d9b5 — seam and test land together.

Prototype (p67_env_seam.py): kanspec: 15 distinct production env names read (56 Source / 26 Test files); 6 SEAMS (KANSPEC_ACTOR ctx.rs:49 8 test mentions, ACTOR_KIND ctx.rs:50 10, GH_FIXTURES gh.rs:194 7, ID_SEED store.rs:889 5, NOW ctx.rs:316 13, NO_BROWSER cmd/up.rs:158 3), 0 doc mentions for all six; KANSPEC_BUILD_VERSION exempt as compile-time; lock.rs:298 KANSPEC_TEST_LOCK_ROOT and scan.rs:1246 correctly masked as test code; Class B 0; stale 0. scry: 0 env reads. fd: 3 names (MSYSTEM/NO_COLOR/TERM) -> 0 seams, Class B 1 (`#[cfg(not(test))] fn now()` src/filter/time.rs:12). ripgrep: 11 names -> 0 seams (RIPGREP_CONFIG_PATH: 3 test mentions but 3 doc mentions in FAQ.md/GUIDE.md -> exempt), Class B 0. click: 6 names -> 0 (COMP_WORDS/COMP_CWORD documented in docs/shell-completion.md; PAGER/LESS/TERM foreign), Class B 0. hono: 1 name (BENCHMARK_TS_IMPL_LABEL) -> 0, Class B 0. Separates LLM from human: yes — 6/6 on the LLM corpus, 0 Class A on four human baselines, with the two exemptions (documented, foreign name) doing the work.

**Detection**
All node kinds verified with `scry ast`.
- Rust reads: `call_expression function:(scoped_identifier path:(.. name:(identifier)=env) name:(identifier)=var|var_os) arguments:(arguments (string_literal (string_content)))`. Compile-time `env!`/`option_env!`: `macro_invocation macro:(identifier) (token_tree (string_literal (string_content)))` — off unless `include_compile_time_env`. Setters: `env::set_var` same call shape; `Command::env` is `call_expression function:(field_expression field:(field_identifier)=env) arguments:(arguments (string_literal) ..)`. Test-scope mask: `attribute_item (attribute (identifier)=cfg arguments:(token_tree (identifier)=test))` preceding `mod_item`; `attribute_item (attribute (identifier)=test)` preceding `function_item`. Class B: `macro_invocation macro:(identifier)=cfg (token_tree (identifier)=test)` inside a `block`; `#[cfg(not(test))]` is `attribute ... (token_tree (identifier)=not (token_tree (identifier)=test))` — report only when a `#[cfg(test)]` sibling item of the same name exists in the file.
- Python reads: `subscript value:(attribute object:(identifier)=os attribute:(identifier)=environ) subscript:(string (string_content))`; `call function:(attribute object:(attribute os environ) attribute:(identifier)=get)`; `call function:(attribute object:(identifier)=os attribute:(identifier)=getenv)`. Setter: same subscript as `assignment left:`. Class B: `comparison_operator (string) (attribute object:(identifier)=sys attribute:(identifier)=modules)` with string `pytest`.
- TS reads: `member_expression object:(member_expression object:(identifier)=process property:(property_identifier)=env) property:(property_identifier)`; `subscript_expression ... index:(string (string_fragment))`; `Deno.env.get` = `call_expression function:(member_expression object:(member_expression Deno env) property:get)`; `import.meta.env.X` = `member_expression object:(member_expression object:(meta_property) property:env)`. Setter: same as `assignment_expression left:`. Class B: `binary_expression left:(member_expression ..NODE_ENV) right:(string (string_fragment))=test`.
- Product prefix: Cargo `[package] name` plus every `[[bin]] name` (kanspec also yields `KS`; parse the whole `[[bin]]` table — ripgrep puts `name` third), package.json `name`, pyproject `[project] name`; uppercase, `-` -> `_`, plus `[declared].env_prefixes`.
- Mentions: whole-word substring scan of every discovered file plus `public_doc_globs` and `setter_file_globs` (names are rare uppercase tokens; cheap). Files matching `setter_file_globs` (.github/**, Dockerfile*, Makefile, justfile, docker-compose*, .env*) count as non-test setters, so CI-set knobs are exempt.
- Git: `git log --format=%h -S'<NAME>' -- src` and `-- tests`, tail = birth; only for confirmed seams (6 calls on kanspec). Graph: none; reuses discovery's Source/Test classification.

**Reason line**
`src/store.rs:889 id_seed reads KANSPEC_ID_SEED (born 988d9b5 together with its first test setter); set only by 5 test files and mentioned in no user doc (ARCHITECTURE.md:2126 calls it a determinism override) — a test seam wired through the process environment: inject via Ctx or document it as public`

Class B: `src/filter/time.rs:12 now() is #[cfg(not(test))] with a #[cfg(test)] twin — a test-mode branch compiled into production`

**Knobs**
`[declared]` env_prefixes = [] (derived from manifest names), env_keywords = ["TEST", "FIXTURE", "SEED", "MOCK", "FAKE", "STUB", "REPLAY"], min_test_mentions = 1, public_doc_globs = ["README*", "docs/**", "doc/**", "*.1", "man/**", "GUIDE*", "FAQ*"], design_doc_globs = ["DESIGN*.md", "ARCHITECTURE*.md"], setter_file_globs = [".github/**", "Dockerfile*", "Makefile", "justfile", "docker-compose*", ".env*"], include_compile_time_env = false, report_test_mode_branches = true, report_stale_test_env = false, weight = 0.0.

**Objection and answer**
Objection: Class A is a design preference, and on its only positive corpus the agent learns nothing new — ctx.rs:39 already says '(tests)', and up.rs:156-157 documents KANSPEC_NO_BROWSER as for 'a test suite (and a headless CI box)', so 1 of 6 is an intended knob. Class B over-fires on idiomatic Rust: `#[cfg(test)]` on non-mod items is the standard test-helper hook (ripgrep dir.rs:170, gitignore.rs:442, globset/glob.rs:168/300/1066/1071, searcher/sink.rs:444/471, kanspec triage.rs:1022), and fd time.rs:12 fires on a human baseline. The stale-test-env rule is wrong today: KANSPEC_BIN is consumed via the string constant hooks.rs:53 and generated shell `${KANSPEC_BIN:-ks}` (hooks.rs:516); KANSPEC_BUILD_VERSION is set by build.rs:18 via rustc-env.

Answer, folded into the design above: Class A stays, weight 0, phrased as 'inject or document' — the fix is the same whether NO_BROWSER is a seam or an undocumented knob, and the value is the cross-file join (set only by tests, documented nowhere) that no linter makes; a doc comment inside the file is not documentation a user finds. Class B is reduced to the three in-body branches plus `#[cfg(not(test))]` items with a same-name `#[cfg(test)]` twin; bare `#[cfg(test)]` items are never reported; Class B is its own reason, ranked below A, since it fires on human code too (fd). The stale-test-env sub-rule ships off by default; if enabled, 'consumer' also counts the name as a string literal anywhere in Source or build.rs, `[env]`/rustc-env, and compile-time reads. CI/Docker/Makefile setters are indexed as non-test setters. Dynamic names (`env::var_os(&self.env_name)`, ripgrep index.rs:132) remain a false negative only.

**Folds into**
New report section "test seams" (weight 0.0; no score contribution). Each Class A entry is also attached as a reason to the hotspot of the file holding the read. P68 direction 2 (product env reads absent from user docs) is this pass's condition (c) and is not duplicated there.

**Cost**
Cheap: one AST query per file over trees already parsed, one substring scan for a handful of uppercase tokens, at most 2 `git log -S` calls per confirmed seam.

**Rules**
- Read sites collected from Source files only; skip any node whose ancestor chain contains a `mod_item` preceded by `#[cfg(test)]` or a `function_item` preceded by `#[test]` (brace-matched by the tree, never by line offset).
- A name is a candidate iff it starts with a product prefix or contains an `env_keywords` entry (case-sensitive, uppercase).
- Product prefixes = uppercased, `-`->`_` forms of: Cargo `[package].name` and every `[[bin]].name`; package.json `name` (scope stripped); pyproject `[project].name`; plus `env_prefixes`. A manifest name in `generic_prefix_stoplist` (app, core, cli, server) derives no prefix.
- Candidate is a seam iff: >= `min_test_mentions` whole-word mentions in Test files; 0 mentions in files matching `public_doc_globs`; 0 mentions in files matching `setter_file_globs`; no production setter node in Source.
- `design_doc_globs` mentions do not exempt; they are quoted in the reason as context.
- Compile-time reads (`env!`, `option_env!`, `import.meta.env` at build) are excluded unless `include_compile_time_env`.
- Emit one finding per name; list every read site as `file:line symbol`; symbol = nearest enclosing named unit from the metrics pass.
- Birth: tail of `git log --format=%h -S'<NAME>' -- <source dirs>` and `-- <test dirs>`; print "born <h> together with its first test setter" when the hashes match, else both.
- Class B (when `report_test_mode_branches`): `cfg!(test)` in a block; `NODE_ENV === 'test'` / `!== 'test'`; `'pytest' in sys.modules`; `#[cfg(not(test))]` item with a `#[cfg(test)]` item of the same name in the same file. Never a bare `#[cfg(test)]` item. Own reason text, sorted after Class A.
- Stale test env (when `report_stale_test_env`): product-prefixed name set in Test with no read node AND no string-literal occurrence in Source, build.rs, `[env]`, or rustc-env output.
- Section rows sorted by test-mention count desc; JSON key `test_seams` with fields name, class, reads[], test_mentions, born_read, born_test, doc_mentions[].

### P68 doc-declared surface drift (tier B)

**Measures**
Unit = (doc file:line, name, class). Two classes shipped: (a) package names — backticked or bold tokens after a label-position stack/crates/dependencies/packages marker, checked against manifest dependency keys, features, workspace members, dev/build deps, and binary names; (c) env vars — `<PREFIX>_[A-Z0-9_]+` tokens in docs, checked against P67's read index plus any string-literal occurrence in Source or build.rs. Direction 1 only (documented, not declared). Classes (b) config keys and (d) CLI flags exist behind knobs, off by default. Direction 2 is dropped here (it is P67 condition (c)). Output: top drift lines plus per-doc drift ratio.

**Why it matters for LLM code**
LLM-driven repos are the ones that keep a prose design doc, and the doc is written before the code settles. kanspec DESIGN.md:632 declares the stack 'serde with toml for config and YAML frontmatter via gray_matter' — Cargo.toml has no gray_matter and its comment block at :60-64 says 'DELIBERATELY ABSENT: gray_matter — cannot serialize at all'; the same line declares pulldown-cmark, which is declared but orphaned (proposal 1). Measured baselines: kanspec docs `kanspec ... --flag` spans vs src/cli.rs: 0 drift after the builtin exemption; fd README flags vs src/cli.rs: 0 after deriving kebab-case longs from clap field names; every `key =` in kanspec docs/config.md exists in src/config.rs (0 doc-only); ARCHITECTURE.md's file tree names 60+ .rs files and all exist.

Prototype (p68_doc_drift.py): Naive version (keyword anywhere on line, any toml fence): kanspec a=23 b=38 c=2, fd a=2, ripgrep a=2, click b=15 a=2, hono d=1 — almost all noise. Refined version: kanspec a=3 (all DESIGN.md:632), b=0 (docs/config.md: 38 keys declared, 0 drift; ARCHITECTURE.md Cargo.toml quote skipped), c=0 (KANSPEC_BIN exempted via hooks.rs const), d=0 (66 longs, ~30 doc spans); scry 0/11 declared; fd 0/9 README flag spans (55 longs); ripgrep d: 44 `rg --x` spans across FAQ/GUIDE, 1 residual (`--no-encoding`, a name_negated flag the prototype did not index -> 0 with it); click 0 (107 longs; all toml fences were pyproject/briefcase quotes); hono 0 after requiring the binary to be the command. Of the kanspec a=3: gray_matter real; `serde_yml` noise-ish (named as alternative to serde_yaml_ng, which is in Cargo.toml:47); `cargo-dist` noise (release tool). Separates LLM from human: weak — it discriminates repos that maintain a design doc.

**Detection**
- Markdown: line lexer, no grammar — track fence open/close and info string, backtick spans, `**bold**` spans.
- Class (a): candidate line matches `stack_line_regex` in label position; only tokens after the label; token must be backticked or bold. Code index: root and workspace manifests' `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`, `[features]` keys, `[workspace] members` names, `[[bin]] name`; package.json dependencies/devDependencies/name/bin; pyproject `[project] dependencies` + optional-dependencies + scripts. Sentence-level exemption: drop the token if the same sentence contains an `alternative_markers` word.
- Class (c): tokens `<PREFIX>_[A-Z0-9_]+` with PREFIX from P67; hit iff no P67 read node AND no string-literal occurrence of the name in any Source file or build.rs (clears KANSPEC_BIN via hooks.rs:53). Skip names read dynamically is not detectable — accept the false positive class.
- Class (b), knob-gated: `[section]` and `key =` lines in fences tagged toml/ini/yaml -> Rust `field_declaration name:(field_identifier)` inside `struct_item` plus `serde(rename)` strings; skip fences whose first header is package/project/build-system/tool.*/dependencies/lib/bin/features/workspace/profile; require >= 50% of a fence's keys to match before reporting the rest.
- Class (d), knob-gated: `--long` tokens in backtick spans or fenced commands where the product binary is the command token (after `^`, `$`, `&&`, `|`, `;`). Rust index: `attribute_item (attribute (identifier)=arg|clap arguments:(token_tree (identifier)=long (string_literal (string_content))))` or kebab-cased `field_declaration name:(field_identifier)` when `long` has no value; builder `.long("x")`; trait style `fn name_long(&self) -> &'static str { "x" }` and `fn name_negated(..) { Some("no-x") }` (ripgrep). Python `call function:(attribute .. attribute:(identifier)=add_argument|option) arguments:(argument_list (string (string_content)))`. TS `call_expression function:(member_expression property:(property_identifier)=option) arguments:(arguments (string (string_fragment)))`. Bins from `[[bin]] name` (parse the whole table), package.json `bin`, pyproject `[project.scripts]`.
- Git: none. Graph: none.

**Reason line**
`DESIGN.md:632 declares crate gray_matter in the stack list but no Cargo.toml depends on it (Cargo.toml:60-64 says it was deliberately dropped) — update the design line`

(The pulldown-cmark half of the original example is an orphan-dep finding and is printed by that pass, not here.)

**Knobs**
`[declared]` doc_globs = ["README*", "docs/**/*.md", "doc/**/*.md", "*.1", "man/**", "GUIDE*", "FAQ*"], design_doc_globs = [] (opt-in: ["DESIGN*.md", "ARCHITECTURE*.md"]), exclude_doc_globs = ["CHANGELOG*"], stack_line_regex = "(?i)(^|\\*\\*|#+\\s*)(stack|crates?|dependencies|packages?)\\s*:", alternative_markers = ["unmaintained", "instead", "successor", "not", "absent", "alternative", "or"], drift_classes = ["packages", "env"] (add "config_keys", "cli_flags" to enable), cli_file_regex = "(^|/)(cli|args|flags|commands?)[^/]*\\.(rs|py|ts)$", builtin_flags = ["--help", "--version", "--color", "--no-color"], weight = 0.0.

**Objection and answer**
Objection: doc hygiene, not a maintenance-cost predictor; the showcase line DESIGN.md:632 carries ~15 tokens of which two are real drifts while `cargo-dist`, `cargo install`, **serde_yaml** ('is unmaintained'), `serde_yaml_ng`/`serde_yml` would all be reported without a growing exemption list; the naive regex hits fd README.md:561/:575 (`fd` is not a dep) and hono README.md:44 (`hono/tiny` with 'zero dependencies'); class (c) misfires on KANSPEC_BIN (docs/config.md:167, consumed via hooks.rs:53); (b) and (d) measured 0 drift everywhere; design docs are dated plans ('v0.1 — the weekend cut') so including them guarantees drift nobody treats as a bug.

Answer, folded in: only classes (a) and (c) ship; (b)/(d) stay knob-gated off until any corpus shows non-zero drift. Label-position regex plus tokens-after-label only — this is what took kanspec a from 23 to 3 and fd/ripgrep to 0. Binary names, workspace members, features, dev/build deps are all in the exemption index (clears fd `fd`, hono `hono/tiny`, ripgrep `pcre2`). Sentence-level alternative_markers drop serde_yaml/serde_yml. Any token that is a string literal in Source or build.rs is exempt (clears KANSPEC_BIN). Default doc_globs are user-facing only; design/architecture docs are opt-in, and when opted in the reason quotes the line so a 'v0.2:' qualifier is visible. Residual: `cargo-dist` on DESIGN.md:632 still fires unless the token appears in dist-workspace.toml / `[workspace.metadata.dist]` — index those files as tooling names. Yield is honestly sparse: one real hit across six repos, but every hit is a one-line fix and no other tool makes the doc-vs-manifest join.

**Folds into**
New repo-level report section "doc drift" (weight 0.0; never a hotspot reason). JSON key `doc_drift` with rows {doc, line, name, class, quote, nearest_declared}. Consumes P67's env read index and the orphan-dep pass's manifest index.

**Cost**
Cheap: a line lexer over a handful of markdown files, string-set lookups against indexes other passes already build; no git, no graph.

**Rules**
- Scan files matching `doc_globs` plus `design_doc_globs`, minus `exclude_doc_globs`; track fence state so fenced content is never lexed as prose for class (a).
- Class (a): a line is a stack line iff `stack_line_regex` matches; candidates are backticked/bold tokens strictly after the match end. Normalise `_`/`-` before comparing. Hit iff token is absent from: dependency keys (all kinds, root and workspace manifests), feature names, workspace member names, binary names, tooling names from dist-workspace.toml / `[workspace.metadata.dist]`, and the sentence has no `alternative_markers` word.
- Class (c): token matches `<PREFIX>_[A-Z][A-Z0-9_]*`; hit iff no P67 read node and no string literal equal to the name in Source or build.rs.
- Class (b)/(d) only when listed in `drift_classes`; (d) requires the product binary as the command token; `builtin_flags` and negated longs are exempt.
- One row per (doc, line, name); per-doc ratio = drift rows / candidate tokens; print top rows by doc then line; quote the source line verbatim, truncated at 120 chars.
- Never emit direction 2 here; P67 owns undocumented env reads.

### P69 planned feature cluster (tier C)

Rejected: its single ground-truth cluster is a documented, deliberate deferral with live consumers (kanspec ci.rs:7 cites D-24; HomerunnerCfg fields read at config.rs:357-359; CiProvider::Homerunner dispatched at ci.rs:73/:144; docs/config.md:97-98 tells users it lands in v0.2), so 'build it or delete all five' is wrong on the showcase, and the rollup cannot be built or measured until the orphan-dep, dead-feature and unread-config passes exist — revisit only as a presentation-layer grouping over structural edges (feature -> dep:x, config field type -> struct), never name stems or promissory comments.

## allocation-density-rust
Three Rust-only proposals built on one syntactic substrate (allocation-call sites per `function_item`). Only P71 clears the bar; P70 and P72 are rejected on the skeptic's evidence, and P71 is redesigned to carry its own site walker so it does not depend on P70's percentile machinery.

### P70 allocation churn density (tier C)
Rejected: by the proposer's own blame check the signal has no defect or churn correlation (allocation lines added after the header commit at 40% vs 43% baseline; no clone-only commits), the prototype's per-unit ranking is ~40% precise by eye (#1 kanspec hit `paint` out.rs:393 is owo_colors Display noise, #2/#3 are P72's story), and clippy owns every removable sub-case with types (useless_format, clone_on_copy, clone_on_ref_ptr, unnecessary_to_owned, redundant_clone); what remains is a style tell the density-per-100-lines cannot separate from serialization boundaries.

### P71 same value re-owned in one function (tier B)

**Measures**
Per unit (metrics pass `function_item`; closures and nested fns count toward the enclosing unit): groups of allocation sites (`.clone()/.to_string()/.to_owned()/.to_vec()/.to_path_buf()/.into_owned()`, `String::from(x)`) keyed by receiver byte text, receiver restricted to `identifier` or `field_expression`. For each group: receiver text, effective count after branch collapsing, site lines, the shared loop ancestor (kind + line), and whether the receiver is bound outside every loop. Per repo: flagged units / units. No percentile; no score contribution.

**Why it matters for LLM code**
Corpus (alloc.py, cfg(test) and tests/ excluded, min_repeats=4, before refinement): scry 0 of 70 units, kanspec 4 of 843 (0.47%), ripgrep 3 of 2298 (0.13%), fd 0 of 156. After the two refinements (same-scope requirement; >= n-1 sites in distinct match arms suppresses; unit named `clone` ignored): kanspec 2 (`close`, `compute`), ripgrep 0, scry 0, fd 0. The surviving hits are the best actionable sentences in the cluster: `kanspec/src/cmd/proposal.rs:687` `close` clones `i.id` at 772 (`evidence.exact.get(&i.id.to_string())`), 776 (`item: i.id.clone()`), 781, 800, 811, 814, 847 (`unmet.push((i.id.clone(), ..))`), all inside `for i in &p.items` (line 757); `derive.rs:1188` `compute` binds `let id = &t.fm.id;` then `id.clone()` as the key into four maps (1202, 1206, 1209, 1212) in one loop. The refinements are load-bearing: `store.rs:45` `load_snapshot` (`fm.id.clone()` at 81, 96, 121, 124) is three different `fm` bindings in three loops, and ripgrep `printer/src/color.rs:314` `from_str` (`s.to_string()` x4 at 317, 325, 332, 339: one early return + 3 arms) is mutually exclusive arms — min_repeats=4 alone does not keep it out, as the skeptic notes. Separation LLM/human is weak by volume, so this is a clause, never a section.

**Detection**
Rust grammar (verified with `scry ast`):
- Site: `call_expression` whose `function` is `field_expression` with `field` `field_identifier` in `methods` and empty `arguments`; the turbofish form `call_expression > generic_function > field_expression`; `call_expression` whose `function` is `scoped_identifier` with text `String::from` and one argument. Exclude `scoped_identifier` text in `shared_clone_paths` (`Rc::clone`, `Arc::clone`).
- Receiver: the `value` of the `field_expression` (or the `String::from` argument, `&`-stripped). Keep only `identifier` / `field_expression`; drop `call_expression`/`macro_invocation` receivers (fresh values). Skip receivers whose text matches `ignore_receivers`.
- Macro bodies: for `macro_invocation` whose `macro` is in `reparse_macros`, take `token_tree` bytes minus delimiters, parse `fn __m(){ (<tokens>); }`, skip if `root.has_error()`, walk with the site rules, offset rows by the token_tree start row; depth 2 for nested macros. (kanspec: 11% of sites live there — needed so `writeln!(.., i.id.clone())` counts.)
- Branch collapse: sites whose nearest `match_arm` ancestors (within the unit) are distinct count as one; same for sites in sibling `if_expression` consequence vs `else_clause` blocks. Count = collapsed count.
- Same-scope rule: the group qualifies only if (a) all surviving sites share at least one `for_expression`/`while_expression`/`loop_expression`/`closure_expression` ancestor (intersection of ancestor sets keyed by byte range — nearest-loop-only is wrong for nested loops), or (b) the receiver's root identifier is a `parameter` of the unit or a `let_declaration` outside every loop in the unit. Rule (b) resolves the root by walking the unit's `let_declaration` patterns and `parameters` — re-bound loop variables (`for path in ..` binding `fm` three times) fail both.
- Test exclusion: skip `mod_item`/`function_item` with a preceding `attribute_item` sibling containing `cfg(test)` or `#[test]`; skip discovery's Test files. Skip units named in `ignore_unit_names`.
- No git or graph ops. Own minimal walker; shares `metrics::collect_units` for unit ranges only.

**Reason line**
`` `close` (src/cmd/proposal.rs:687-944) re-owns `i.id` 7 times in one `for` body (line 757; sites 772, 776, 781, 800, 811, 814, 847) ``
Observation only; no prescription ("clone once" / "key by `&TicketId`") is printed — removability depends on callee types the pass cannot see.

**Knobs**
`[ownership.repeat] enabled = true, languages = ["rust"], methods = ["clone","to_string","to_owned","to_vec","to_path_buf","into_owned"], count_string_from = true, shared_clone_paths = ["Rc::clone","Arc::clone"], reparse_macros = ["format","println","eprintln","write","writeln","print","eprint","anyhow","bail","ensure","panic","assert","assert_eq","debug_assert","vec"], reparse_depth = 2, min_repeats = 4, receiver_kinds = ["identifier","field_expression"], collapse_branches = true, require_same_scope = true, ignore_receivers = [], ignore_unit_names = ["clone","default"], max_listed_lines = 8, only_on_hotspots = true`

**Objection and answer**
Objection: the count is verifiable but the fix is not — every clone in `close` feeds a different owner (`Disposition::Shipped { item }`, `unmet.push`, a `HashMap<String,_>` lookup), so "clone once" saves at most one clone, and the human hits are all idioms (per-arm error construction, Arc snapshots in ripgrep `dir.rs:443-460`, two-owner `insert`+`push` patterns). Answer, folded into the design: the reason is worded as an observation with no prescription; branch collapsing plus the same-scope requirement removes the per-arm and re-bound-variable classes (ripgrep goes to 0); `ignore_receivers` handles named Arc holders; and it only ever prints as an extra clause on a unit that is already a hotspot for another reason (`only_on_hotspots = true`), so a two-owner idiom in a non-hotspot never surfaces. Remaining residue (Copy receivers, Arc fields spelled `.clone()`) is accepted because the clause costs one line on a file the agent is already going to read.

**Folds into**
An extra clause on an existing hotspot reason (weight 0, no score input, no section). In `--json`: `hotspots[].ownership_repeat = [{unit, receiver, count, loop_line, lines}]`. Consumed by t-b342 / t-16ca via the per-unit result.

**Cost**
Cheap: one extra walk of trees already parsed; macro re-parse is a handful of small parses per file. No history or graph work.

**Rules**
- Unit = `function_item` from `metrics::collect_units`; sites inside closures/nested fns attribute to the enclosing unit.
- Site = allocation call per the detection shape; `Rc::clone`/`Arc::clone` excluded by path text; receiver must be `identifier` or `field_expression`.
- Macro token trees in `reparse_macros` are re-parsed as `fn __m(){ (<tokens>); }`; skip on `has_error`; rows offset by token_tree start; recurse to `reparse_depth`.
- Group sites by receiver byte text within a unit.
- Collapse: sites in distinct `match_arm` ancestors, or in `if` consequence vs `else_clause`, count as one.
- Qualify only if collapsed count >= `min_repeats` AND (all sites share a loop/closure ancestor OR receiver root is a parameter / a let outside every loop).
- Skip `cfg(test)`/`#[test]` items, discovery Test files, units in `ignore_unit_names`, receivers matching `ignore_receivers`.
- Emit only on units already carrying a hotspot reason when `only_on_hotspots`; list at most `max_listed_lines` lines then "…".
- Reason text is an observation; never prints a fix.
- Every list and threshold above lives under `[ownership.repeat]`.

### P72 clone-fan struct assembly (tier C)
Rejected: every cited hit (`Card` board.rs:82, `Thread` cmd/comment.rs:36, `ProposalPage` cmd/proposal.rs:1048) is a `#[derive(Serialize)]` DTO where owned fields are mandatory, the prescribed `impl From<&Ticket>` merely relocates the same clones (and would itself be flagged), Arc-field fans (ripgrep IgnoreInner) are indistinguishable without types, and 13 sites in 34k lines with no churn or defect correlation is a preference about where clones sit, not a maintenance predictor.

## allocation-at-call-boundary-rust
Three Rust-only proposals that share one artefact: a repo-wide function-signature index (key `Type::name` or bare name, parameter kinds, return type) built from `function_item` nodes with `#[cfg(test)]` excluded. Two of the three (P73, P75) fail on inspection because the allocation they flag is, in nearly every cited site, the only route to the required type — a fact tree-sitter cannot see. The one that survives (P74) is the only signal where the syntax of the *callers* proves the callee's return type is being undone. Build the signature index for P74; it is reusable if a typed pass ever arrives.

### P73 allocation at a call boundary (tier C)
Rejected: 9 of the 10 kanspec hits are allocations that cannot be deleted (`glyph::OK` is a `char` const at out.rs:106, not a `&str`; git.rs:330, config.rs:238, decision.rs:481 are Display-only receivers), the `delete` verdict fired 0 times on 92 kloc across four corpora, and where the receiver type is knowable clippy's `unnecessary_to_owned` (perf, warn-by-default) already reports it with types; the owned-parameter alternate's single hit (`git_err`, git.rs:865) moves the String into `KsError::Git`, so by-value is the correct signature.

### P74 borrowed collection re-owned by callers (tier B)

**Measures**
Per repo-defined callee whose `return_type` is `Vec<&T>` or `impl Iterator<Item = &T>`: non-test call sites, sites that re-own the result (`.cloned()`/`.copied()` before `.collect()` on the call's chain, or on a let-bound identifier the call was assigned to), distinct caller files. Unit of report: one annotation per callee clearing the ratio. No per-file density, no percentile.

**Why it matters for LLM code**
An LLM writes the callee to hand out borrows (lifetime-correct, looks idiomatic) and then, at each new call site, writes the same `.into_iter().cloned().collect()` because the caller stores the ids in an owned struct field. Nobody sees all the sites at once, so the pattern accretes. Corpus: callees returning `Vec<&T>`/`&[T]`/iter of `&T`: kanspec 10 fns, 19 resolved sites, 4 re-owned; ripgrep 35 fns, 11 resolved sites, 0 re-owned; scry 0 fns; fd 0 fns. Callee rule (>=3 calls, >=50% re-owned): kanspec 1 hit (`blocked_by`), ripgrep 0, scry 0, fd 0. The hit: `kanspec/src/derive.rs:69` `blocked_by -> Vec<&'s TicketId>`, re-owned at board.rs:278, cmd/flow.rs:88, cmd/ticket.rs:290 (`derive::blocked_by(..).into_iter().cloned().collect()` into a `Vec<TicketId>` struct field) and derive.rs:1200-1202 (`let b = blocked_by(s, t)` … `b.is_empty()` … `b.into_iter().cloned().collect()`); read-only at cmd/ticket.rs:814 (`open.first()`); derive.rs:1432 is under `#[test]`. triage.rs:573 `unchecked -> Vec<&Step>` at 1/3 re-owned (triage.rs:283) correctly fails the ratio. Human baseline: ripgrep's 35 borrowed-returning fns have 0 re-owning callers — its slice accessors (ignore/src/types.rs:158 `globs() -> &[String]`, :253, searcher line_buffer.rs:261 `buffer() -> &[u8]`) are the reason `&[T]` is excluded below. Separation is real but the expected yield is ~1 callee per LLM-written repo and 0 per human repo.

**Detection**
Rust grammar only (confirmed via `scry ast`):
- Signature index (shared, whole repo, `#[cfg(test)]` and `tests/` excluded): `function_item{name, return_type}`. Match return_type when it is `generic_type{type: type_identifier "Vec", type_arguments: (reference_type …)}` (first argument is a `reference_type`, lifetime optional) or `abstract_type{trait: generic_type{type: type_identifier "Iterator", type_arguments: (type_binding name: "Item" type: reference_type)}}`. Never match `reference_type` returns (`&[T]`, `&str`) or `Option<&T>`. Key = `ImplType::name` from the nearest `impl_item` ancestor's `type` field (generics stripped), plus bare name; keep entries whose key or bare name is unique repo-wide.
- Call resolution: `call_expression{function: identifier | scoped_identifier{path, name} | field_expression{field: field_identifier} | generic_function}`; `scoped_identifier` resolves `Type::name` by the path's last segment else bare name; `field_expression` (method call) resolves by unique bare name only when `resolve_methods` is on and the name is not in the std-method stoplist (the prototype found `Fixes::iter` swallowing all 186 `.iter()` calls without it).
- Re-own, direct form: from the resolved `call_expression` walk up while the parent is `field_expression | call_expression | generic_function` (a method chain; `arguments` containing closures, as in `.filter(|x| ..)`, do not break it); collect `field_identifier` names in order; re-owned when `cloned` or `copied` appears and `collect` appears after it.
- Re-own, let-bound form: when the call is the `value` of a `let_declaration{pattern: identifier}`, scan the remaining siblings of the enclosing `block` for any chain rooted at that `identifier` that satisfies the direct-form test; re-owned if found (derive.rs:1200-1202). A read of the same binding elsewhere does not cancel this: the caller still ends up owning.
- Call sites with a `#[cfg(test)]` `attribute_item` on any ancestor `mod_item`/`function_item`, or `#[test]` on the enclosing fn, are excluded from both numerator and denominator.
- Macro bodies (`writeln!`/`format!` token trees) are not re-parsed in v1: a chain inside one is a miss, not a false hit.
- No git or graph ops.

**Reason line**
`` `blocked_by` (src/derive.rs:69) returns `Vec<&TicketId>` but 4 of 5 non-test callers re-own it (`.cloned().collect()` at src/board.rs:278, src/cmd/flow.rs:88, src/cmd/ticket.rs:290, src/derive.rs:1202) — add an owned-returning variant or return an iterator ``
Template: `` `<fn>` (<file>:<line>) returns `<ret>` but <n> of <m> non-test callers re-own it (`.cloned().collect()` at <sites>) — add an owned-returning variant or return an iterator ``. `<ret>` is the return_type text with lifetimes stripped. Sites are capped at `max_listed_sites`, then `and N more`.

**Knobs**
```toml
[ownership.reowned_return]
enabled = true
min_calls = 3
min_ratio = 0.75        # 1.0 = skeptic's setting; reports nothing on any corpus in hand
count_copied = true     # .copied() counts toward the ratio
track_let_bound = true  # credit `let b = f(..); … b.into_iter().cloned().collect()`
resolve_methods = true
method_stoplist = ["iter","into_iter","get","len","is_empty","as_ref","as_str","clone","new","from","into","to_string","map","filter","collect","keys","values","first","last","push","insert","contains","unwrap","expect"]
max_listed_sites = 6
annotation_only = true  # print only on files already ranked as hotspots; never a section
```

**Objection and answer**
Objection: a borrowed return is a deliberate design (derive.rs:62-68 documents why `blocked_by` ties its lifetime to the snapshot); changing the return type taxes the read-only callers and alters a public API for no behavioural gain; the numbers were wrong (six callers, two read borrowed); `&[T]` would match every slice accessor in ripgrep; the per-file density line ranks the standard filter-then-own idiom; `.copied()` on Copy types is free.
Answer, folded into the design: (1) the reason no longer says "change the return type" — it says add an owned-returning variant or an iterator, which is additive and leaves ticket.rs:814 untouched; the read-only callers are the reason the fix is a variant, not a replacement. (2) `&[T]`, `&str`, `Option<&T>` are excluded outright; only `Vec<&T>` and `impl Iterator<Item = &T>` qualify, because a fresh `Vec` of borrows is already an allocation the callee chose to make, so replacing it with a `Vec` of owned values costs the callee one clone per element, not a new allocation. (3) The density line and percentile are dropped; the pass emits one annotation per callee and only on a file the existing score already ranks. (4) The denominator counts non-test callers only and credits the let-bound form, so the skeptic's recount (4 of 5 non-test, once derive.rs:1200-1202 is one re-owning site) is what the pass prints; at `min_ratio = 1.0` the pass prints nothing on any corpus, which is why the default is 0.75 and the knob is exposed. (5) `.copied()` on `Vec<&u32>` is free at runtime but the callee still forces every caller to write the chain; since the fix is an owned variant, `Vec<u32>` is strictly simpler — counted, knob to disable. (6) Callers that must own to escape a borrow scope are not a false-positive class; they are exactly the evidence that the callee should hand out owned values.

**Folds into**
No score weight (yield is too sparse to rank). One annotation line appended to the callee file's hotspot reasons when that file is already in the ranked list; a `ownership.reowned_returns[]` array in `--json` (callee key, file:line, return type, sites, ratio) regardless of ranking, so `scry context` (t-b342) can surface it for the caller files too. Shares the signature index with nothing built today; keep it as a module so a later typed pass can reuse it.

**Cost**
Cheap. One walk over already-parsed Rust trees to build the index, one walk to resolve calls and inspect chains; O(calls). No history or graph input. Zero cost on repos with no Rust.

**Rules**
- Rust files only; skip files classified Test and any `mod_item`/`function_item` with a `#[cfg(test)]` or `#[test]` attribute sibling, at both index and call-site time.
- Index every non-test `function_item` whose `return_type` is `Vec<&T>` (generic_type Vec whose first type argument is reference_type) or `impl Iterator<Item = &T>` (abstract_type with a type_binding Item whose type is reference_type). Never index `&[T]`, `&str`, `Option<&T>`, `Cow`.
- Key entries by `ImplType::name` (impl_item ancestor's `type`, generics stripped) and by bare name; drop bare-name keys that are not unique repo-wide; drop bare-name method keys that are in `method_stoplist`.
- Resolve a call by scoped_identifier last-segment `Type::name`, then unique bare name; field_expression calls only when `resolve_methods` is true.
- A site is re-owned when its ascending method chain contains `cloned`|`copied` (subject to `count_copied`) followed later by `collect`; or, when `track_let_bound` is true and the call is a `let_declaration` value with an identifier pattern, when any later statement in the same block has such a chain rooted at that identifier.
- Report a callee when non-test sites >= `min_calls` and re-owned/sites >= `min_ratio`.
- Print the reason only on a callee file already present in the ranked hotspot list when `annotation_only` is true; always emit the JSON entry.
- Reason text follows the template above verbatim; list at most `max_listed_sites` sites in file order, then `and N more`.
- Every listed site must carry `file:line` of the `call_expression` (not of the `collect`).
- Macro token trees are not re-parsed; document this as a known miss.
- All settings live under `[ownership.reowned_return]`; unknown keys are a config error.

### P75 conversion round-trips (tier C)
Rejected: the headline premise is false — kanspec's id newtypes already have `Display` (ids.rs:63) and `From<Id> for String` (ids.rs:76), so `k.as_str().to_string()` is one allocation spelled differently, not a round-trip, and 0 of 18 kanspec sites are removable; every pair in the table is decidable only with the receiver's type (`.as_ref().to_owned()` on `impl AsRef<T>` is the only way to own; `.clone().into_iter()` on a borrowed field is mandatory), and the typed cases are clippy's `unnecessary_to_owned`/`implicit_clone`/`redundant_clone`.
