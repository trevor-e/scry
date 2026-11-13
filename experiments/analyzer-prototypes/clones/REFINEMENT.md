# Missed clone fixes after stricter matching

The same ten pinned repositories now produce **3 findings instead of 11**. All three previously useful locations survive, and all eight audited false positives are filtered. The useful locations represent two issues: Nushell's module-path fallback and two oh-my-pi timeout helpers.

This is an in-sample refinement against the existing source audit. It is not a held-out precision measurement, and the retained findings remain review leads rather than reproduced runtime defects.

## Before and after

| Repository | Initial findings | Stricter findings | Audited false positives filtered |
|---|---:|---:|---:|
| Nushell | 3 | 1 | 2 |
| oh-my-pi | 6 | 2 | 4 |
| Tauri | 2 | 0 | 2 |
| adulting | 0 | 0 | 0 |
| Hono | 0 | 0 | 0 |
| kanspec | 0 | 0 | 0 |
| ripgrep | 0 | 0 | 0 |
| scry | 0 | 0 | 0 |
| typescript-eslint | 0 | 0 | 0 |
| Vite | 0 | 0 | 0 |
| **Total** | **11** | **3** | **8** |

The comparison verifies identical HEADs, 6,418 source files, 55,840 function bodies, 1,508 history commits, 504 fix-labelled commits, 1,078 changed bodies compared, parser-error lists, history gaps, and candidate caps. The broad discovery stage still generates exactly the original 11 pairs. The reduction comes entirely from the new structural check.

## What changed

The existing near-clone search and edit-location evidence still identify possible pairs. A candidate is now emitted only when the **complete pre-fix signature and body match the surviving implementation**, allowing conservative renaming of local bindings:

- Parameter slots follow declaration order. Swapping argument uses no longer disappears into a generic identifier token.
- Supported local declarations get distinct slots, and references resolve through lexical scopes. Callback parameters used as callees can be renamed, so `fn(signal)` and `run(signal)` match when they refer to corresponding parameters.
- Literals, operators, external callee names, member names, types, defaults, modifiers, and receiver expressions remain significant. Object and Rust struct shorthand retain both their property key and their binding reference.
- Comments, whitespace, and optional trailing commas in parameter/type lists do not affect equality.
- Unsupported binding constructs use literal identifiers throughout the whole signature/body. This avoids guessing through macros, complex destructuring, classic TypeScript loops, switch bodies, JSX, dynamic eval, nested declarations, and custom binding contexts. It can miss renamed copies in these forms.

The scanner records rejected candidates and the first failed comparison: signature or complete body. It does not contain repository, path, function-name, or audited-candidate allowlists. Similarity thresholds, history windows, and discovery exclusions are unchanged.

The normalizer is intentionally conservative. An unsupported function can still match when its full signature/body token sequences are identical apart from the function name, comments, whitespace, and permitted separators. Nushell's retained pair takes this path because its Rust destructuring is not modeled. The timeout pairs use resolved local binding slots.

## Useful findings retained

| Finding | Why it survives | Evidence |
|---|---|---|
| Nushell `Use::run` to `ExportUse::run` | The sibling retains the complete old body and signature. | The [fixed implementation](https://github.com/nushell/nushell/blob/7bff3d1d04d65529ee87f4b076448d7e9f538d09/crates/nu-cmd-lang/src/core_commands/use_.rs#L120) uses `module.real_file()` when directory lookup fails; the [sibling](https://github.com/nushell/nushell/blob/7bff3d1d04d65529ee87f4b076448d7e9f538d09/crates/nu-cmd-lang/src/core_commands/export_use.rs#L108) does not. |
| oh-my-pi `withOpenAICompatibleDiscoveryTimeout` | The pre-fix helper matches after resolving the differently named callback parameter and ignoring a trailing signature comma. | The [catalog helper](https://github.com/can1357/oh-my-pi/blob/c66e2f6ac7c66192c093377ef6701a1c37e12b0f/packages/catalog/src/discovery/openai-compatible.ts#L12) retains abort-only handling. |
| oh-my-pi `withCatalogDiscoveryTimeout` | The entire old helper matches after consistent local binding renaming. | The [second catalog helper](https://github.com/can1357/oh-my-pi/blob/c66e2f6ac7c66192c093377ef6701a1c37e12b0f/packages/catalog/src/provider-models/openai-compat.ts#L46) also lacks the independent rejecting deadline added to the [coding-agent helper](https://github.com/can1357/oh-my-pi/blob/c66e2f6ac7c66192c093377ef6701a1c37e12b0f/packages/coding-agent/src/config/model-discovery.ts#L40). |

The prior source audit remains the judgment source. Some timeout callers have an outer deadline, and Nushell's runtime export-use behavior was not executed. Structural retention does not upgrade these to confirmed defects.

## False positives filtered

All eight original false positives fail complete-function comparison:

- Nushell row splitting differs from column splitting; column-name validation cannot simply propagate.
- Nushell current-frame variable lookup differs from type-alias lookup in signature and lookup policy.
- oh-my-pi's pure tree projection allocates fresh maps, unlike the instance method that needed to clear persistent maps.
- The `/omfg` controller and footer differ from `/btw`, whose fix concerns session promotion.
- The generic usage-history fallback has a different signature and already honors recorded status.
- Tauri's bundle-relative path conversion differs from filesystem normalization, including literals and public visibility.
- Tauri's direct schema reader differs from its nested permission-list reader in return type and body.

The `/omfg` controller and footer are two separate pairs. `comparison.json` contains all eleven identities, their original judgment, retained/suppressed outcome, and structural evidence.

## Validation and limits

The new CLI history suite covers 19 named cases plus propagation after a fix. Every named case is asserted to reach the broad candidate stage before the structural decision, so negative cases cannot pass just because discovery ignored them. The original Rust/TypeScript propagation test also passes.

Cases cover callback/local renaming, positional parameters, lexical shadowing, parameter-default temporal dead zones, external callees, properties, literals, operators, types/defaults, partial-copy rejection, trailing commas, TypeScript/Rust shorthand keys, switch-scope fallback, parenthesized eval, JSX tags, and Rust local renaming. Only synthetic test histories are executed; corpus application code is not run.

Expected recall cost: an otherwise useful copy with an extra guard, different type spelling, modifier, or unrelated local edit is now suppressed. The older candidate generator can also miss short functions, renamed/deleted files, changes invisible to its loose tokenizer, and history outside the configured window. These limits were not solved by this refinement.

Exact syntax still cannot prove equivalent behavior. The same external name may resolve to different imports, types, implementations, or receiver state. Existing callers may enforce the missing condition elsewhere. Keep this as a review aid and evaluate a fresh corpus before estimating general precision.

## Reproduce

From the repository root, using the existing pinned corpus and environment:

```sh
/private/tmp/scry-analyzer-venv/bin/python experiments/analyzer-prototypes/run.py --checks clones --output /private/tmp/scry-clone-rerun
/private/tmp/scry-analyzer-venv/bin/python experiments/analyzer-prototypes/clones/compare.py /private/tmp/scry-clone-rerun/clones.json
/private/tmp/scry-analyzer-venv/bin/python experiments/analyzer-prototypes/clones/test_probe.py
/private/tmp/scry-analyzer-venv/bin/python experiments/analyzer-prototypes/clones/test_tightened.py
```

The checker defaults to `--matching exact`. Pass `--matching loose` directly to `clones/probe.py` to recover the original detection behavior. The comparison command fails if revisions, scope counters, broad candidates, or audit identities changed.

Artifacts:

- `raw.json` and `audit.json`: unchanged initial evidence and source judgments.
- `tightened.json`: fresh full-corpus scan, emitted and rejected pairs, parameters, and scanner-source hashes.
- `comparison.json`: reproducible before/after calculation using the prior judgments.
- `refinement-validation.json`: executed validation and scope checks.
