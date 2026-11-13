# Matching repacked inputs without restoring known false positives

The bounded relaxation catches pnpm's previously missed legacy star/unstar reader. Across both pinned ten-repository corpora, **that is the only output change: four useful review locations become five**. All fourteen previously audited false-positive pairs remain rejected. The five emitted locations remain source-supported review leads, not reproduced runtime defects.

| Corpus | Strict matcher | New matcher | Previously audited false positives emitted |
|---|---:|---:|---:|
| Original ten repositories | 3 | 3 | 0 → 0 |
| Subsequent ten repositories | 1 | 2 | 0 → 0 |
| **Total** | **4** | **5** | **0 → 0** |

Both corpora now inform development. The second corpus is no longer a held-out evaluation, and these counts do not establish general precision or recall.

## What changes in the output

The new alert is pnpm's [fetchPackumentWithStars](https://github.com/pnpm/pnpm/blob/63765162e6346aa5a667be9c1b5ef61103f1c58a/pnpm11/registry-access/commands/src/star/common.ts#L120-L137). Fix [`f876766`](https://github.com/pnpm/pnpm/commit/f876766c67811c61bd6421dcf9f61481718cc72a) added `?write=true` to metadata reads before modifying a registry package document. The star/unstar fallback still reads the plain URL, edits `users`, and PUTs the document using its fetched `_rev`. Its modern endpoints avoid this fallback; registry behavior and support for `write=true` were not executed. The [prior source audit](../holdout/audit-pnpm-sample.json) remains the judgment source.

The strict matcher rejected the pair because its signatures differed. The new matcher identifies these corresponding inputs from their consistent uses throughout the complete body:

| Pre-fix deprecation reader | Surviving star reader |
|---|---|
| Direct `fetchFromRegistry` parameter | Object property `fetchFromRegistry` |
| Object property `packageUrl` | Direct `pkgUrl` parameter |
| Object property `authHeader` | Object property `authHeader` |
| Object property `packageName` | Object property `packageName` |

The local response variable is renamed consistently. The final `as PackageMeta` and `as PackumentWithStars` assertions each match their function's declared return alias. After this narrowly defined normalization, the entire bodies match.

All existing alerts remain:

- Nushell's module-path fallback in `ExportUse::run`: one location.
- oh-my-pi's abort-only catalog timeout helpers: two locations.
- Playwright's WebView evaluation method retaining nested protocol arguments: one location.

No other repository gains or loses an alert. [The comparison](results/comparison.json) records all twenty repository counts and all twenty-nine broad candidate pairs. The prior [original-corpus report](../REFINEMENT.md) and [subsequent-corpus report](../holdout/README.md) contain the retained findings' source evidence and limitations.

## The bounded rule

The new default, `--matching roles`, first runs the existing exact matcher unchanged. Only rejected pairs can enter the new fallback:

1. Both sides must be TypeScript/TSX named function declarations with fully supported binding syntax. Rust, methods, arrows, and unsupported contexts keep the old matching behavior.
2. Inputs may move between direct parameters and flat object parameters. At least one direct/property crossing must retain the same name as an anchor. Every input must be used, and its mapping must be one-to-one and consistent throughout the body.
3. Inputs that remain object properties must keep their keys. Inputs that remain direct parameters must keep their positions and type annotations. Defaults, optional/rest parameters, nested/computed patterns, duplicate keys, and generic functions abstain. Object parameters require simple named type aliases; inline field types are not silently discarded.
4. All executable statements, calls, external names, members, property keys, literals, operators, and local annotations must match. Extra guards or partial clones do not qualify. Callable modifiers and async/await remain significant.
5. Only a terminal return's nominal `as` assertion may normalize with its matching declared return alias, optionally under `Promise`. Other type assertions remain significant. `arguments`, its shorthand use, escaped identifiers, and unresolved binding contexts abstain.

The implementation adds no repository, path, function-name, or candidate allowlists. The initial search, 0.85 similarity threshold, 200-commit limit, token bounds, exclusions, edit evidence, and candidate cap are unchanged. `--matching exact` retains the prior strict decision rule; `--matching loose` retains the original broad behavior.

This is a source-review heuristic. Named object aliases are not resolved to field types, and callers, getters, backend behavior, or same-spelled imports can still differ. Matching a complete body with repacked inputs does not prove semantic equivalence.

## False-positive checks

The original eight false positives remain filtered, including Nushell row/column differences, Tauri path/schema contracts, and oh-my-pi controller/projection differences. All six audited false positives from the subsequent corpus also remain filtered: Vitest's process/thread worker distinction and pnpm's legacy-directory, dist-tag, hoisted-cache, and two MySQL/PostgreSQL pairs.

Ten other pnpm pairs were not source-classified in the earlier rejection sample. They remain rejected; they are not counted as avoided false positives. No previously unaudited pair becomes an alert.

The final rerun verifies identical pinned revisions, all discovery coverage counters, and the identities **and evidence** of all twenty-nine broad pairs. The strict matcher emitted four and rejected twenty-five; the new matcher emits five and rejects twenty-four. The change comes solely from accepting the previously audited pnpm pair.

## Validation and reproducibility

The final CLI suite passes 33 focused input-regrouping cases, the 19 existing binding cases, and the exact → relaxed → propagated sequence: 55 CLI invocations. Every named negative first asserts that it reaches broad candidate discovery. The original Rust/TypeScript propagation suite also passes. Synthetic repositories exercise the real command and Git history path; downloaded corpus applications, tests, and builds are not executed.

Review caught an escaped spelling of `arguments` that initially bypassed the packing guard. The fallback now checks identifier spellings throughout the function before binding normalization, and the CLI suite covers the regression. The final full-corpus run uses this corrected version.

Coverage is unchanged: 16,079 eligible files, 132,892 extracted bodies, 3,508 available non-merge history commits, 1,147 fix-labelled commits, and 3,234 changed bodies compared. The existing two missing history parents and sixty-two HEAD files with parse errors remain limitations. No pair was omitted by the per-function cap. Extracted-body counts precede the token-size index filter.

With the original pinned checkouts and parser environment, run from the repository root:

```sh
/private/tmp/scry-analyzer-venv/bin/python experiments/analyzer-prototypes/clones/test_input_roles.py
/private/tmp/scry-analyzer-venv/bin/python experiments/analyzer-prototypes/clones/test_probe.py
/private/tmp/scry-analyzer-venv/bin/python experiments/analyzer-prototypes/clones/relaxed/run.py --output /private/tmp/scry-clone-relaxed-rerun
```

The runner checks clean pinned source, scans every repository, checks detector hashes before and after, and fails if search coverage or broad candidate evidence differs from the saved strict runs. [Run metadata](results/run.json) records commands, source hashes, dependencies, and both manifests. [Validation](validation.json) records completed checks. Raw reports are in `results/original/` and `results/holdout/`.

Prior raw results and audits are preserved. To reproduce the historical frozen matcher and its hash-enforcing runner, use checkout `5ed5432`; the current runner evaluates the new matcher against those saved results.
