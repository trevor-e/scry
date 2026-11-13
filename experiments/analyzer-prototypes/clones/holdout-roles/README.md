# Frozen role-aware matcher on a second fresh batch

The frozen detector emitted **zero alerts** on ten new repositories. Its broad search found four pairs, all rejected. The predeclared sample therefore selected all four: **two appropriate rejections, one useful maintenance lead suppressed, and one unresolved lifecycle concern**.

This batch provides no positive evidence that the new parameter-regrouping fallback generalizes: neither that fallback nor the exact matcher accepted a pair. Zero emitted alerts cannot establish precision. The rejection audit demonstrates a remaining coverage limitation without measuring end-to-end recall.

## Frozen run and coverage

Commit `e26b084` recorded all three detector hashes, the ten repositories, and the sampling algorithm before any downloads or results. The detector remains byte-for-byte identical to `7e198b5`, with `--matching roles --commits 200 --similarity 0.85` and the same pinned parser versions. No rules, thresholds, exclusions, history limits, or repositories changed after seeing results.

| Repository | Pinned revision | Eligible files | Fix-labelled commits | Changed bodies compared | Emitted | Rejected |
|---|---|---:|---:|---:|---:|---:|
| axum | `9ef7944fa534` | 114 | 43 | 34 | 0 | 0 |
| hyper | `b638fc0de351` | 71 | 64 | 56 | 0 | 0 |
| uv | `9b4fe9027f16` | 660 | 6 | 4 | 0 | 0 |
| Helix | `a714a818a3a4` | 224 | 43 | 40 | 0 | 0 |
| Bevy | `41a1282e376c` | 1,455 | 43 | 42 | 0 | 1 |
| TypeScript | `056d69edb807` | 85 | 40 | 7 | 0 | 0 |
| VS Code | `54ab79431a58` | 9,981 | 12 | 27 | 0 | 1 |
| Angular | `551c446d98e2` | 3,252 | 74 | 154 | 0 | 0 |
| tRPC | `e2e07e70947c` | 440 | 81 | 117 | 0 | 0 |
| Excalidraw | `82d5847e57dc` | 511 | 98 | 435 | 0 | 2 |
| **Total** | | **16,793** | **504** | **916** | **0** | **4** |

Every repository supplied 200 non-merge history commits, for 2,000 total. The fix-label filter then selected 504; this is not a run over 2,000 fixes. Eligible source comprises 2,714 Rust and 14,079 TypeScript/TSX files. There are 177,700 extracted named function bodies before the 45–1,200-token candidate filter. VS Code supplies most of those bodies, so the repositories are not equally sized trials.

No selected parent was missing, and no pair was omitted by the per-function cap. Forty-nine HEAD files contain parse errors; unaffected functions can still be included, and historical parse errors are not separately counted. File exclusions, unsupported syntax, short/large functions, similarity thresholds, fix-label selection, and older history limit coverage. See [the method audit](audit-method.json) for exact counts and parser-error paths.

## Rejection audit

The predeclared maximum was twelve, initially six per source language, with unused quota transferred. Candidate and repository hashes determine a round-robin order across repositories. Only one Rust and three TypeScript pairs existed, so **all four were selected and audited**, with no unclassified rejected pairs. [The sample record](results/sample.json) preserves identities and ranks; its ordering was independently recomputed.

| Pair | Source-review judgment | What the audit established |
|---|---|---|
| Bevy read-only → mutable contiguous fetch | Appropriate rejection | The mutable implementation never had the failing assertion; the shared batching correction already applies to both. |
| Excalidraw ellipse-distance helpers | **Useful maintenance lead suppressed** | The rotated copy retains numerical cases corrected in the math helper, but no current production caller was found. |
| Excalidraw group → frame lookup | Appropriate rejection | The detected edit preserves a TypeScript subtype; the frame helper has a broader contract and different membership logic. |
| VS Code editor → panel terminal dropdown | **Unresolved** | The panel can also create items for two toolbar owners; indirect UI reconstruction may make its singleton lifetime safe, but ordering is unverified. |

### Excalidraw: missed numerical maintenance fix

Fix [`4850bf3`](https://github.com/excalidraw/excalidraw/commit/4850bf336fe0a8501af49d84deebc7fb56357cc2) updates the [math helper](https://github.com/excalidraw/excalidraw/blob/82d5847e57dcb6fb5982833ea9ec2137fe0b2bd5/packages/math/src/ellipse.ts#L102-L146) for circle and axis cases, including a zero-denominator guard and nonzero quadrant-sign handling. The [rotated helper](https://github.com/excalidraw/excalidraw/blob/82d5847e57dcb6fb5982833ea9ec2137fe0b2bd5/packages/utils/src/shape.ts#L404-L467) retains the old numerical core. Rotation does not eliminate those cases.

The strict matcher rejects its different signature, and the fallback does not support these arrow functions. Even adding arrow support would not suffice: the legitimate rotation code makes their complete bodies different. This is a partial-clone limitation, outside the narrow input-regrouping relaxation.

Its practical significance is limited: repository searches found only geometry-test callers of the exported `pointOnEllipse` wrapper, and the utility package's root index does not re-export it. Treat this as a propagation, consolidation, or obsolete-code cleanup lead, **not a demonstrated editor bug**. Existing numerical regression tests were inspected but not executed. [Detailed audit](audit-excalidraw.json).

### VS Code: unresolved toolbar ownership

Fix [`20b6105`](https://github.com/microsoft/vscode/commit/20b6105c1dd7e383ddba2af85a30dd483ef28a67) replaces the editor's single dropdown with independent items for multiple title toolbars. The [terminal panel](https://github.com/microsoft/vscode/blob/54ab79431a5858c9546c7161e5afe5fd9e8570d6/src/vs/workbench/contrib/terminal/browser/terminalView.ts#L289-L323) retains a singleton. The complete-body comparison notices `TerminalLocation.Panel` versus `Editor`, and methods are outside the new fallback.

The different location does not establish that rejection is correct. The pane-header toolbar creates an item, while the [merged container](https://github.com/microsoft/vscode/blob/54ab79431a5858c9546c7161e5afe5fd9e8570d6/src/vs/workbench/browser/parts/views/viewPaneContainer.ts#L592-L595) can request another from the same pane. Replacing the singleton disposes the first. Unmerging exposes the header without directly rebuilding its toolbar in that path.

Menu/context updates may rebuild it indirectly. Two independent source reviews could not establish that ordering or a visible failure, so this pair is **unresolved**, not counted as a useful miss or an avoided false positive. The focused question is the terminal pane's merged-to-unmerged lifecycle, rather than the editor's pinned-row scenario. [Detailed evidence and counterevidence](audit-vscode.json).

### Appropriate rejections

For Bevy, the read-only fetch had an incorrect assertion comparing a requested subrange with the whole table. The [mutable survivor](https://github.com/bevyengine/bevy/blob/41a1282e376ce0dcd9808e1b3f6c071803363eb4/crates/bevy_ecs/src/query/fetch.rs#L3036-L3078) never contained that assertion. The same commit corrects the shared generic batching code used by both. Missing replacement debug checks is a consistency difference, not evidence the original failure remains. [Audit](audit-bevy.json).

For Excalidraw's [frame helper](https://github.com/excalidraw/excalidraw/blob/82d5847e57dcb6fb5982833ea9ec2137fe0b2bd5/packages/element/src/frame.ts#L271-L282), the group-helper fix propagates `NonDeleted` subtype information without changing runtime selection. Frame callers needing live children filter them explicitly. A generic API improvement may be possible, but no missed behavioral correction was established. [Audit](audit-excalidraw.json).

## What this says about generalization

The complete frozen detector is quiet on this batch, but quietness alone is not evidence of quality. The broad search produced only four candidate pairs, and neither acceptance path yielded a positive example. The new regrouping rule therefore still lacks a positive example outside its development corpora.

Reviewing all four rejected pairs identifies one source-supported maintenance miss and one unresolved concern while confirming two distractions were filtered. These are judgments about this small broad-candidate set. They do not measure misses that never reached the candidate generator, and they do not justify a precision or recall percentage. All judgments are source-based; no corpus application, test, build, UI, or registry request was executed.

## Reproduce

The fixed corpus and full revisions are in [corpus.json](corpus.json); [protocol.json](protocol.json) records the precommitted freeze and sampling rule. Initial checkouts used `git clone --depth 300 --single-branch`; graph depth can include more than 300 commits through merges. To recreate missing checkouts at their pinned revisions, initialize each manifest path, add its recorded origin, fetch `--depth 300 origin REVISION`, and check out `FETCH_HEAD` detached.

With those checkouts and the parser environment from `experiments/analyzer-prototypes/requirements.txt`, run from the repository root:

```sh
/private/tmp/scry-analyzer-venv/bin/python experiments/analyzer-prototypes/clones/holdout-roles/run.py --output /private/tmp/scry-clone-roles-holdout-rerun
```

The runner refuses changed detector hashes, parser versions, repository selections, dirty tracked source, or different pinned HEADs. It saves raw per-repository reports, complete sample ranks, and [run metadata](results/run.json). The source-audit judgments are separate from generated outputs. [Verification](verification.json) reconciles the run and all four audits. Earlier experiment artifacts and detector code remain unchanged.
