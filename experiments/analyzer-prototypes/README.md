# Four analyzer prototypes on Rust and TypeScript repositories

The missed-clone-fix experiment produced the strongest review leads: three locations across two issues. Config-default drift found one benchmark metadata discrepancy. The Serde check produced no candidates, and the build-script check produced no confident omissions. None of the real-repository leads was reproduced as a runtime defect. The checks ran against ten pinned source snapshots; all emitted candidates were inspected in context.

These are standalone experiments. They do not change scry's scan output, scoring, or configuration.

**Clone follow-up:** the current default matcher reduces the original 11 candidates to 3 while retaining all three useful locations. See [the refinement report](clones/REFINEMENT.md). The tables and judgments below record the initial experiment; `clones/raw.json` preserves that baseline. The rerun command uses the refined default.

**Fresh-corpus evaluation:** the frozen matcher emitted one useful Playwright review lead across ten new repositories. A sample of rejected pairs contained six avoided false positives and one useful pnpm lead suppressed by signature differences. See [the holdout report](clones/holdout/README.md); this is separate from the initial results below.

**Input-regrouping follow-up:** the current default also recognizes bounded TypeScript parameter repacking. Rerunning both corpora adds only the missed pnpm lead (4 → 5 useful locations total); all fourteen audited false-positive pairs remain filtered. See [the comparison and validation](clones/relaxed/README.md). Both corpora now inform development.

**Frozen regrouping evaluation:** ten additional repositories produce no alerts. Auditing all four rejected pairs finds two appropriate rejections, one Excalidraw maintenance miss, and one unresolved VS Code lifecycle concern. The new fallback still has no positive holdout example. See [the second fresh-batch report](clones/holdout-roles/README.md).

## Results

| Prototype | Actual analysis coverage | Emitted candidates | Source-review outcome |
|---|---|---:|---|
| Config-default drift | 11,422 files; 1,661 recognized literal-key env reads; 59 supported default sites | 3 groups | 1 useful benchmark discrepancy; 2 intentional differences |
| Missed clone fixes | 6,418 production-path files; 55,840 function bodies; 1,508 available history commits; 504 fix-labelled commits; 1,078 changed bodies compared | 11 pairs | 3 useful locations across 2 issues; 8 false positives |
| Shadowed Serde variants | 3,150 Rust files; 57 untagged declarations; 41 derived enums; 56 of 100 variants modeled; 27 comparable ordered pairs | 0 | No demonstrated real-corpus yield; 8 nonfindings/exclusions inspected |
| Missing build rerun declarations | 133 manifests; 24 manifest-selected build scripts; 35 recognized input reads | 0 confident omissions | 8 abstentions inspected; 3 Android env reads form 1 unresolved lead; 6 reads covered and 21 Cargo-managed inputs excluded |

Candidate units differ between checks. These counts are not comparable defect rates. Thresholds and exclusions were developed on this corpus, not tested against a separate held-out corpus. Useful means a source-supported review lead, not an automatically actionable bug.

## Useful source findings

### Clone fixes in Nushell

A module-reload fix added `module.real_file()` as a fallback after `find_in_dirs_env` in [Use::run](https://github.com/nushell/nushell/blob/7bff3d1d04d65529ee87f4b076448d7e9f538d09/crates/nu-cmd-lang/src/core_commands/use_.rs#L120). [ExportUse::run](https://github.com/nushell/nushell/blob/7bff3d1d04d65529ee87f4b076448d7e9f538d09/crates/nu-cmd-lang/src/core_commands/export_use.rs#L108) retains the entire normalized old body, including the lookup without that fallback.

Both implementations use the resulting path to populate `FILE_PWD` and `CURRENT_FILE` before evaluating an export-env block. The parser shares `parse_use`, then selects the export-use declaration. The new regression test covers bare-name `use`; the corresponding export-use case deserves inspection. This is the clearest Rust missed-propagation lead, but runtime reachability and intended export-env behavior were not executed here.

Evidence: commit `ab6c901127083f940c39e68670850dbd69f1ee10`, `nu-engine/src/env.rs:289`, `nu-parser/src/parse_module.rs:265`, and `tests/modules/mod.rs:1518` at the pinned Nushell revision.

### Clone fixes in oh-my-pi

The coding-agent [timeout helper](https://github.com/can1357/oh-my-pi/blob/c66e2f6ac7c66192c093377ef6701a1c37e12b0f/packages/coding-agent/src/config/model-discovery.ts#L40) gained an independent rejection and `Promise.race`: its comment explains that abort alone can leave a fetch pending. A [regression test](https://github.com/can1357/oh-my-pi/blob/c66e2f6ac7c66192c093377ef6701a1c37e12b0f/packages/coding-agent/test/model-discovery.test.ts#L1168) supplies a never-settling fetch and expects the deadline to reject.

Two catalog siblings still only abort and then await the operation:

- [withOpenAICompatibleDiscoveryTimeout](https://github.com/can1357/oh-my-pi/blob/c66e2f6ac7c66192c093377ef6701a1c37e12b0f/packages/catalog/src/discovery/openai-compatible.ts#L12).
- [withCatalogDiscoveryTimeout](https://github.com/can1357/oh-my-pi/blob/c66e2f6ac7c66192c093377ef6701a1c37e12b0f/packages/catalog/src/provider-models/openai-compat.ts#L46).

The catalog accepts custom fetch implementations, and a local LM Studio metadata path uses the second helper when no signal is supplied (`provider-models/openai-compat.ts:3197`). This is one missed-hardening family at two locations. Some callers already supply an outer deadline; the result does not establish that all discovery calls can hang. The existing regression test was inspected, not run.

### Config defaults in adulting

At revision `c8baee7ca4cecbfdfc687dff7f4823c278720596`, `frontend/playwright-ct.perf.config.ts:29` declares `PERF_DPR` default 1. `frontend/src/perf/PaintProbe.perf.tsx:28` defaults its printed DPR to 2, and line 104 includes that value in the measurement label. The harness explicitly matches these perf tests.

The probe does not read the actual browser DPR, and no repository assignment to `PERF_DPR` was found. The source therefore has inconsistent declared harness and reporting defaults when the variable is unset. Reading the effective browser value would make the label authoritative. Actual Playwright device-option precedence and historical benchmark runs were not evaluated, so this is a benchmark metadata lead, not evidence that past performance measurements are wrong.

## False positives and cases that need more information

| Check | Real example | Why it should not become a defect finding |
|---|---|---|
| Clone fixes | Tauri `normalize` versus `resource_relpath` in `tauri-utils/src/resources.rs:17` | One preserves filesystem roots; the other intentionally produces bundle-relative names and removes drive prefixes. |
| Clone fixes | Nushell `split_column` versus `split_row` | Duplicate column-name validation does not apply to a command producing a list of values. |
| Clone fixes | oh-my-pi agent-tree method versus `projectAgentTree` | The method must clear persistent maps; the pure projection constructs new maps on every call. |
| Clone fixes | oh-my-pi `/btw` versus `/omfg` controllers and footers | Branch-promotion checks and dynamically changing branch availability belong to `/btw`; `/omfg` saves rules. |
| Clone fixes | Tauri permission-list reader versus global-schema reader | The fix rebases paths nested inside a list. The schema reader consumes the Cargo-provided file directly. |
| Config defaults | Adulting `PERF_CPU` defaults 1 and 4 | They select intentionally different benchmark workloads and are applied to the browser. |
| Config defaults | oh-my-pi `PATH` defaults empty versus `/usr/bin:/bin` | Independent test subprocesses have different setup needs. One cross-scope group duplicates this same evidence. |
| Serde | Tauri integer-then-float variants | Integer overlap is intentional; fractional inputs still reach the later float variant. Full shadowing is absent. |
| Serde | kanspec/adulting untagged response enums | They derive Serialize only. Deserialization ordering does not apply. |
| Build declarations | Nushell `RUSTC_LINKER`; Tauri dynamic `CARGO_FEATURE_*` | Cargo manages these inputs; the latter remains a raw abstention because the prototype cannot resolve the dynamic key. |
| Build declarations | Tauri directory iteration; oh-my-pi dynamically read files | A parent directory or the same resolved binding is already watched. Direct string matching would be misleading. |

All 11 clone pairs and all 3 config groups have individual judgments in their audit JSON. The table gives representative explanations; it is not an additional candidate count.

The build audit found three plausible Android input omissions in `tauri/crates/tauri/build.rs:286,320,321`: `WRY_ANDROID_KOTLIN_FILES_OUT_DIR`, `TAURI_ANDROID_PROJECT_PATH`, and `TAURI_ANDROID_PACKAGE_UNESCAPED`. They affect generated file destinations/content, but external build helpers and other rebuild triggers are unresolved. They remain one possible lifecycle issue outside the confident-candidate count. No Android build was executed.

Serde abstained on 44 of 100 variants in derived enums. Examples include external types, flattening and custom deserialization. Zero emitted findings does not prove those enums are free of shadowing. Named structs accepting sequence representations, unlike inline untagged struct variants, also defeated a tempting but incorrect object-only heuristic.

## Recommendations

1. Keep missed-clone fixes as a review aid. Show the actual fix, surviving fragment, and both contracts. Three useful locations out of eleven candidates is insufficient for an automatic defect label or scoring penalty.
2. Keep config-default drift opt-in and include benchmark/tooling scopes. Its sole useful result here was benchmark metadata; only four supported Rust default sites were found. Constants and wrappers need better coverage before interpreting zero Rust findings.
3. Develop build-helper summaries before promoting missing rerun declarations. The conservative scanner correctly avoided false certainty, but every interesting unmatched input remained unresolved.
4. Keep the Serde prototype experimental. Its semantic fixture checks pass, but this corpus demonstrated no practical yield.

## Corpus and reproducibility

The corpus contains public upstream snapshots of Tauri, Nushell, ripgrep, Vite, Hono, and typescript-eslint, plus clean HEAD snapshots of local scry, kanspec, adulting, and oh-my-pi. `corpus.json` records full revisions, source origins, file counts, and shallow-history status. Source snapshots reside in `/private/tmp/scry-analyzer-corpus` and may eventually be removed by temporary-directory cleanup.

The public clones used depth 120 for Tauri and 160 for the other public repositories. Local snapshots retain available local history. The clone prototype inspects at most 200 non-merge commits per repository, and skips two commits with unavailable parents. To reproduce the historical candidate set, preserve those history boundaries as well as HEAD. All checkers read tracked source; none executes the corpus repositories, their tests, or their build scripts.

The four checkers deliberately have different discovery rules. Config records test/example/benchmark scopes separately; clones exclude those paths; Serde classifies them; build uses Cargo manifest-selected scripts. Parse recovery and unsupported forms are recorded in raw results. Config's 315 parse-error files are partial-coverage files, not clean analyses.

From the repository root:

```sh
python3 -m venv /private/tmp/scry-analyzer-venv
/private/tmp/scry-analyzer-venv/bin/pip install -r experiments/analyzer-prototypes/requirements.txt
/private/tmp/scry-analyzer-venv/bin/python experiments/analyzer-prototypes/run.py --output /private/tmp/scry-analyzer-rerun
```

The runner verifies every checkout's pinned HEAD and clean tracked files, then writes fresh raw results to the output directory. `--repos scry nushell` selects a subset; `--checks clones` selects a check; `--corpus-root PATH` selects relocated checkouts. Run each individual checker directly to analyze other repositories. The recorded audit judgments describe only the pinned run and are not generated by rerunning the scanners.

## Validation

Executed with Python 3.15.2 and the pinned parser dependencies in `requirements.txt`:

- Config: 19 CLI fixture cases covering scope, parsing, defaults, precedence, and abstentions.
- Clones: one history test with Rust and TypeScript subcases; each runs the CLI with a surviving copy, then after propagation and verifies the candidate disappears.
- Build: two CLI tests covering 15 fixture repositories; an additional dependency-free Cargo fixture demonstrates stale output without a watch and correct rebuilding after adding it.
- Serde: two CLI tests plus two real Serde runtime tests. Synthetic cases include six shadowing cases, five nonshadowing cases, and five abstentions.
- Combined runner: all four checks executed successfully against pinned scry, and their results were compared with the saved corpus outputs.

Only our synthetic fixture code was compiled/executed. Synthetic positives are not included in corpus finding counts.

```sh
/private/tmp/scry-analyzer-venv/bin/python experiments/analyzer-prototypes/config/validate.py
/private/tmp/scry-analyzer-venv/bin/python experiments/analyzer-prototypes/clones/test_probe.py
/private/tmp/scry-analyzer-venv/bin/python experiments/analyzer-prototypes/build/test_probe.py
/private/tmp/scry-analyzer-venv/bin/python experiments/analyzer-prototypes/build/validate_incremental.py
/private/tmp/scry-analyzer-venv/bin/python experiments/analyzer-prototypes/serde/test_cli.py
cargo test --offline --manifest-path experiments/analyzer-prototypes/serde/fixtures/Cargo.toml --target-dir /private/tmp/scry-serde-fixture-target
```

The Serde fixture lockfile requires its dependencies in Cargo's cache for the offline command.

## Evidence files

| Check | Raw scan | Human source audit |
|---|---|---|
| Config | `config/raw-results.json` | `config/audit.json` |
| Clones | `clones/raw.json` | `clones/audit.json` |
| Serde | `serde/raw-corpus.json` | `serde/audit.json` |
| Build | `build/raw.json` | `build/audit.json` |

Clone `changed` coordinates refer to pre-fix source; `changed_at_fix` coordinates refer to the fix commit; `survivor` coordinates refer to pinned HEAD. Current source links above use independently checked HEAD lines.
