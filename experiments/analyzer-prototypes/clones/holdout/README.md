# Frozen missed-clone detector on ten fresh repositories

This is the historical frozen evaluation at checkout `5ed5432`. The current matcher and the rerun of both corpora are documented in [the subsequent refinement](../relaxed/README.md). Use that historical checkout to reproduce this report with its hash-enforcing runner.

The unchanged detector emitted **one useful review lead in Playwright** and no clearly false-positive alerts. It emitted nothing in the other nine repositories. A manual sample of rejected pairs found **six avoided false positives and one useful pnpm lead that the strict matcher missed**. These are source-review judgments; neither useful lead was reproduced as a runtime defect.

The ten projects were selected and committed in `ddbe489` before downloading/scanning them. None appeared in the original corpus. No repositories were added after seeing results, and no detection rules, thresholds, or exclusions changed.

## Results

Each repository contributed 200 non-merge history commits, filtered by the existing fix/bug/regression subject rule. “Broad pairs” are the unchanged initial candidate search; “emitted” means the complete-function matcher accepted the pair.

| Repository | Pinned revision | Eligible files | Changed functions compared | Broad pairs | Emitted | Rejected |
|---|---|---:|---:|---:|---:|---:|
| Tokio | `ab0814fc8c62` | 487 | 9 | 0 | 0 | 0 |
| Clap | `ec93e71255c0` | 124 | 20 | 0 | 0 | 0 |
| Ruff | `124210f4fde5` | 1,908 | 17 | 0 | 0 | 0 |
| Starship | `e3b64da0b660` | 255 | 23 | 0 | 0 | 0 |
| bat | `8cf33bdde9a6` | 45 | 13 | 0 | 0 | 0 |
| pnpm | `63765162e634` | 4,827 | 1,638 | 16 | 0 | 16 |
| TanStack Query | `19f555cf919a` | 479 | 29 | 0 | 0 | 0 |
| Playwright | `8534271f0413` | 717 | 135 | 1 | 1 | 0 |
| Vitest | `47c87ebb5025` | 546 | 158 | 1 | 0 | 1 |
| Vue core | `21738b527bac` | 273 | 114 | 0 | 0 | 0 |
| **Total** | | **9,661** | **2,156** | **18** | **1** | **17** |

Coverage was 6,310 eligible Rust files and 3,351 TypeScript/TSX files; 77,052 extracted named function bodies; 2,000 non-merge commits; and 643 fix-labelled commits. Extracted-body counts precede the 45–1,200-token candidate filter. The pnpm snapshot contains both Rust (3,491 eligible files) and TypeScript (1,336), so repository labels should not be treated as isolated language samples. The five Rust projects produced no broad pairs; that does not establish an absence of missed fixes.

## Useful emitted lead: Playwright WebView evaluation

Commit [`bac1f9f`](https://github.com/microsoft/playwright/commit/bac1f9f67e9cff2a8e7ecbac25d56ec3bb1d15c6) moves serialized evaluation arguments into function source to avoid protocol JSON nesting limits. The fixed [WebKit method](https://github.com/microsoft/playwright/blob/8534271f0413bae6a455b7cf43b2637391bc614b/packages/playwright-core/src/server/webkit/wkExecutionContext.ts#L66-L89) explains the WebKit-specific rationale. The [WebView counterpart](https://github.com/microsoft/playwright/blob/8534271f0413bae6a455b7cf43b2637391bc614b/packages/playwright-core/src/server/webkit/webview/wvExecutionContext.ts#L66-L88) still sends values nested inside `Runtime.callFunctionOn.arguments` and is textually identical to the entire pre-fix method.

This counterpart is used by the stock-WebKit/Mobile-Safari backend, receives the shared evaluation serializer's output, and has no equivalent flattening in its transport. The existing deep-argument regression (`EV-032`) belongs to a test directory included by the WebView configuration. These facts make propagation worth reviewing.

The two backends use different sessions and WebKit distributions. The source does not establish the stock backend's actual nesting limit or a failing test result. An unaffected parser could make this a false positive. **Judgment: useful review lead, not a confirmed bug.** [Full evidence and counterevidence](audit-playwright.json).

## False positives filtered, and a useful miss

All emitted candidates were audited. Separately, seven of the seventeen rejected pairs were selected for contextual review. The remaining ten were not manually classified; do not count all seventeen rejections as avoided false positives.

| Rejected case | Source-review judgment | Why |
|---|---|---|
| Vitest fork-worker constructor → thread-worker constructor | Avoided false positive | The new `distPath` feeds a child-process PID watchdog. Worker threads use a different entrypoint and `thread.terminate()`. |
| pnpm recorded directory → legacy directory checker | Avoided false positive | Different lockfile formats and child checks; the compatibility path explicitly takes no new fixes. |
| pnpm package metadata → dist-tags reader | Avoided false positive | Direct tag endpoints do not read and write back a package-document revision. |
| pnpm shared-slot cache helper → hoisted cache helper | Avoided false positive | The new empty-cache handling drives a shared-slot staging decision absent from the hoisted importer. |
| pnpm MySQL → PostgreSQL account listing | Avoided false positive | Deadline-aware acquisition is already present; the remaining decoder fix concerns MySQL binary collation. |
| pnpm MySQL → PostgreSQL owned-token lookup | Avoided false positive | Same backend-specific decoding issue; PostgreSQL also has a different query and row layout. |
| pnpm deprecation metadata → legacy star metadata | **Useful review lead suppressed** | Both read, modify, and write a package document; different parameter layouts and return types fail exact matching. |

The clearest avoided false positive is [Vitest's thread worker](https://github.com/vitest-dev/vitest/blob/47c87ebb5025010141cf8aa5dc69c5b57c10d2fa/packages/vitest/src/node/pools/workers/threadsWorker.ts#L19-L27). Its `workers/threads.js` literal differs from the fork worker's entrypoint, so the complete-body rule rejects the pair. The added fork watchdog requires a child-process PID; copying it to a thread worker would be inappropriate. [Audit](audit-method.json).

The useful miss is pnpm's [legacy star/unstar reader](https://github.com/pnpm/pnpm/blob/63765162e6346aa5a667be9c1b5ef61103f1c58a/pnpm11/registry-access/commands/src/star/common.ts#L120-L137). Fix [`f876766`](https://github.com/pnpm/pnpm/commit/f876766c67811c61bd6421dcf9f61481718cc72a) added `?write=true` to metadata reads before package-document mutations to obtain a current revision. The star reader still requests the plain URL; its [caller](https://github.com/pnpm/pnpm/blob/63765162e6346aa5a667be9c1b5ef61103f1c58a/pnpm11/registry-access/commands/src/star/common.ts#L89-L112) edits `users` and PUTs the complete document using the fetched `_rev`. The shared fetch layer does not add the query for it.

The exact matcher rejects this pair at the signature comparison: the parameter organization, destructured types, and return types differ. This is a concrete coverage cost of the frozen rule, not a new emitted finding. It affects a legacy fallback; modern star endpoints bypass it, and registry support for `write=true` plus actual stale-read behavior remain untested. [Four-case pnpm audit](audit-pnpm-sample.json); [two SQL cases](audit-pnpm-sql.json).

## Interpretation and limits

The frozen rule produces a very small review queue on this fresh corpus and filters several concrete distractions. It also suppresses a useful source-review lead because syntactic identity is stricter than a shared operational contract. One emitted candidate, whose runtime impact is unconfirmed, is insufficient to estimate general precision. The selected rejection sample is not a recall measurement.

No selected fix commit lacked its parent, and no candidate was omitted by the per-function cap. Ten HEAD files contained parse errors; unaffected function bodies may still be included. Historical parse errors are not separately counted. The exact file lists are in [the method audit](audit-method.json) and raw reports.

Existing limits remain: tests/examples/generated/vendor paths are excluded; small/large functions, unsupported syntax, partial clones, changed signatures, matching across file extensions, renames/deletions, and older history can be missed. Matching names do not resolve external types, imports, receiver state, or backend behavior. No downloaded application code, tests, build scripts, or registry requests were executed.

## Frozen configuration and reproduction

This evaluation used the detector byte-for-byte identical to commit `f11f4268a0be6934425de33c4f3e494579de473a`:

| File | SHA-256 |
|---|---|
| `probe.py` | `2ba87ddaf9f90e456aee0fd8ce44ef15855018c486ea5394561631f9d796fdac` |
| `bindings.py` | `aeaaa9d1555b5535840efcefe890d63531e59f86a850e2abc5adfe3ee2a349e8` |

Every invocation used `--matching exact --commits 200 --similarity 0.85`. [protocol.json](protocol.json) records the advance selection and frozen rules; [corpus.json](corpus.json) records origins, full revisions, and checkout locations. Initial clones used `--depth 300 --single-branch`. Git depth is graph depth, so available commit counts can exceed 300 in repositories with merges.

With those pinned checkouts and the existing environment, run from the scry repository root:

```sh
/private/tmp/scry-analyzer-venv/bin/python experiments/analyzer-prototypes/clones/holdout/run.py --output /private/tmp/scry-clone-holdout-rerun
```

To recreate cleaned-up checkouts, initialize each manifest path, add its recorded origin, fetch its full pinned revision with `git fetch --depth 300 origin REVISION`, and check out `FETCH_HEAD` detached. Install the parser versions from `experiments/analyzer-prototypes/requirements.txt` in a virtual environment. The runner checks pinned HEADs and clean tracked files, verifies detector hashes before and after scanning, and writes one raw report per repository. Manual audit judgments are not regenerated by the runner.

Evidence: [run metadata and exact commands](results/run.json), [raw reports](results), [verification](verification.json), and the audit files linked above. The previous experiment's raw results and judgments remain unchanged.
