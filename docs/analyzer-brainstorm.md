# New analyzers for LLM-written code

*Brainstorm and verification, 2026-09-14. Companion: `docs/analyzer-brainstorm-appendix.md` carries a per-proposal chapter with detection details, knobs and implementation Rules for everything ranked below.*

## Implementation status (2026-09-15)

Everything in the three waves below is built on this branch, each item reviewed by an adversarial agent, fixed, and verified against the corpora; the tree runs 166 unit tests (28 before) with no new clippy warnings. Kanspec tickets exist for every item; the "worth a ticket, not first" items are ticketed and not built.

| wave | items | commits |
|---|---|---|
| 1, fix the existing report | regions (P54), clone self-match and tables (P59), co-change sweeps and lift (P60), cycle cuts (P61), bots and fix cap, graded has_tests (P55), clone symbols and PLAN (P62) | 59dc3cb, b06fba6, 0d6e397, a563183, 8140a6f, b295048, 44201db; findings c4def05, b70c807 |
| 2, new cross-file passes | dead (P01-03), helpers (P07-08), strings (P10), clumps (P46-47), declared (P64-66), env seams (P67) | a83e014, e200561, 3d74124, 89e2d40, 5847a64, 6e663fb; findings 144618b |
| 3, richer reasons | comments (P17-18), short names (P48), locals (P44), parse defaults (P28) | df4392c, bf60a08, 938b8cd, 1c8814b; findings 9141c30 |

Three headline numbers in this document only reproduce with looser knobs than the briefs' shipped defaults, and are filed as decisions rather than tuned: the inlined-helper share (plural's body is 11 tokens, under the 12-token floor), the literal families (kanspec's hint strings are shorter than min_len 20 and live in its own `fix!` macro), and the (a, f, s) clump (only one slot agrees on type). Scan time is 1.7x the pre-wave baseline and still under a second on every corpus; about half of the growth is per-symbol git lookups, ticketed. Several other prototype numbers drifted because the corpora themselves changed (scry grew from 3.1k to 16k lines while being built).

## TL;DR

Ten brainstorming lenses produced 87 raw analyzer ideas. They were merged into 75 canonical proposals, and every one was prototyped on two LLM-written repos (scry, kanspec) and four human-written baselines (ripgrep, fd, click, hono), then attacked by an independent skeptic. 15 survived as clear wins, 21 as worth a ticket with a narrower design, and 39 were rejected with numbers.

The two most useful findings cut against intuition:

- **Comment volume is not an LLM tell.** ripgrep has more comment lines per code line than kanspec at every percentile, and the heaviest-commented functions in human repos are the ones a maintainer would fight to keep. What separates LLM code is what the comments are *about*: rule-style section banners (37 per 1k comment lines in both LLM repos, 0 in all four human repos) and in-function phase labels. Those are not smells to delete; they are split plans the author already wrote down, and the report should print them as such.
- **"Rewrites the same function" is real, but it does not look like a token clone.** It shows up as the same small helper redefined per session (`plural` three times in kanspec, from two different Claude sessions), as helper bodies inlined instead of called (below the clone pass's 70-token floor), and as the same CLI hint string hand-spelled in 10 files. Name-keyed and literal-keyed cross-file passes catch it; the winnowing clone pass cannot.

Two more surprises worth knowing before building anything: about a third of the clone pairs scry prints on Rust repos today are inline `#[cfg(test)]` fixtures, and the top kanspec clone pair is one `match` matching its own second half. The first wave below fixes the existing report before adding to it.

## How this was produced

- **Lenses:** comments and docs; over-engineering; defensive code; cross-file consistency; agent-era git history; dead and unreferenced code; test quality; the classical smell catalogue; naming and public surface; and the report consumer's view. A completeness critic then added two gap lenses (declared surface that nothing consumes, and Rust ownership churn).
- **Corpora:** `scry/src` (3.1k lines, LLM) and `kanspec/src` (34k lines, LLM; 67 of 79 commits carry `Claude-Session` trailers from 7 sessions) versus ripgrep and fd (Rust, human), click (Python, human) and hono (TypeScript, human).
- **Verification:** for each cluster of proposals, one agent wrote throwaway prototypes and measured the distribution on all six corpora and inspected the top hits by eye; a second agent argued each proposal was aesthetic, already a linter rule, or drowned in false positives. Tiers combine both verdicts. Headline numbers were spot-checked by hand afterwards (banner counts, one-letter bindings, `plural` definitions, the literal families, the self-matching clone, the trailer counts).
- **Caveats:** the LLM corpora are Rust only, and both come from one person's prompting style, so anything that fired on kanspec but not on scry is flagged as house style rather than an LLM property. The human baselines are shallow clones, so a few history claims have no human baseline at all. kanspec is 14 days old with a median gap of 11 minutes between commits, which confounds every history signal.

## The two questions you asked

### Verbose comments

Measured, not a tell:

- In-body comment-to-code ratio at p99: ripgrep 1.00, click 0.96, fd 0.75 versus kanspec 0.50, scry 0.36. The top hits in human repos are Windows path notes and regex subtleties. A "comment load outlier" reason would tell an agent to delete the best comments in the repo. Rejected (P19).
- "Docstring restates the signature" is rarer in the LLM corpora than in the human ones (0.12 versus 0.97 to 1.35 per kloc). Dropped at merge.
- Duplicated comment paragraphs run 6 to 18 times higher in human code (trait-impl docs and JSDoc must be copied). Rejected (P21). Stale-by-blame comments were 0 of 5 wrong on inspection. Rejected (P20).

Measured, a tell, and actionable:

- **Banner partitions (P17).** Rule-style comments (`// ── The ladder ───`) that cut a file into named sections: 15 kanspec files have 3 or more such sections with the largest over 80 lines, scry has 1, all four human repos have 0. The section titles are the module names. Reason: "scan.rs is cut into 4 labelled sections: 'The seals' 60-233, 'The ladder' 312-979 (665 lines, 12 fns) ... extract the largest as ladder.rs".
- **Phase labels inside long functions (P18).** `// ── pass 1: ...`, `// step 2`: 5 to 9 kanspec functions, 0 in any human corpus at any threshold, and every hit is already over the cognitive threshold (`attention` at 30 labels YOU/AGENT/WATCHING at lines 786, 913, 977; `ladder` at 32 labels six rungs). Printed as a sub-line of the existing "worst X at N" reason, with an estimated post-split cognitive per phase.
- **Process narration (P16, as a knob, not a pass).** Ticket ids, design-doc references and shouted emphasis inside comments run 59 per 1k comment lines in kanspec and 0 in every human repo, but also 0 in scry. That is your house style, not LLMs in general, so ship it as a user-supplied comment-marker regex on the lexical smells ticket (t-eccd) with empty defaults, plus a banner count.

### Rewriting the same function

Where it actually shows up, in order of yield:

- **Same-name helpers in several files (P07).** kanspec: 20 of 580 top-level names are defined in two or more files; the reportable ones include `plural` byte-identical in cmd/flow.rs:430 and cmd/status.rs:469 plus a divergent `plural(n, noun)` in cmd/proposal.rs:168, `io_err` twice, `render` twice. ripgrep and fd: 0 verbatim copies. Session attribution says the three `plural`s came from two sessions.
- **Inlined idioms of an existing helper (P08).** With callee names kept and a 12-token floor, 3.9% of kanspec helpers have their body spelled out inline somewhere else (`plural`'s body appears 7 times in 6 files; `io_err`'s at lock.rs:90, 100, 106 and store.rs:974) versus 0 to 0.7% in the human repos. This needs its own normalizer: the clone pass's full anonymisation turns a 12-token body into a shape that matches every two-way string pick.
- **Repeated literals (P10).** 15.3% of kanspec's prose string literals belong to an exact cross-file family (52 families; `kanspec doctor` in 11 files, `kanspec show {id}` in 10) versus scry 0%, fd 0%, ripgrep 0.6%, click 1.6%, hono 2.5%. The config-literal variant finds the timestamp pattern `%Y-%m-%dT%H:%MZ` in 4 files and five different spellings of time-to-string in 12 files.
- **View structs rebuilt by copying (P72, observation kept, fix rejected).** Six kanspec report structs each clone the same five fields of `t.fm`. The prescribed fix (a borrowing struct) is wrong for serde DTOs, so it stays an observation on a hotspot, not a pass.
- **Function-level wholesale rewrites in history (P26).** `start` in cmd/flow.rs carries lines from 8 of the file's 16 commits. Real, but fd shows the same rewrite rate, so it is a churn attribution, not an LLM tell.

Verbatim identical bodies at the function level remain t-8868's job (the structural clone tier), provided it lowers its floor to short helpers. The token clone pass should not be tuned for this: raising its floor loses logic clones and lowering it drowns in `use` statements (4,407 repeated 14-grams in kanspec, nearly all imports).

## Wave 1: make the existing report honest

All cheap, all measured, and each one changes what the top-15 hotspots say today.

1. **Inline test-module split (P54, Rust only).** Tag `#[cfg(test)] mod` byte ranges as test regions; subtract them from the size signal and the clone token stream, tag metrics units inside them, and count their `use super::*` as a test reference. kanspec carries 24% of its source lines inside inline test modules (ripgrep 25%, fd 19%, scry 17%, so this is a correctness fix, not a tell). Clone pairs that lie entirely inside test regions: kanspec 3 of 15, scry 5 of 10, fd 6 of 11, ripgrep 2 of 15. derive.rs drops from 2307 to 1282 lines; doctor.rs's "17% duplicated" is 54% inline test functions and 27% a static registry.
2. **Clone self-match fix and table tagging (P59).** The #1 kanspec pair, lib.rs:133-156 against 159-182 (488 tokens), is the first half of one uniform `match` matching its second half; the existing overlap rule only catches overlapping diagonals. Fix: drop runs whose two ranges share the same smallest covering container node. Then tag surviving runs whose covering node is a run of uniform same-kind siblings (match arms, static arrays, switch cases, dict literals) as `table` and list them under their own heading. 4 of the top 10 kanspec pairs are tables; table share of clone tokens is 9.3% in kanspec versus 0.02% ripgrep and 1.4% hono. Keep `table_weight` at 1.0 by default so parallel maps that must drift together still rank.
3. **Co-change sweep exclusion, lift, and "explained by" (P60).** Exclude directory-sweep commits (touching at least half of a directory with 4 or more files, and at least 6 files) from pair counts only, keep them in churn; require lift of 3 over a random-commit null model; and when both members import a file that changed in most of their co-commits, print "both import ctx.rs, which changed in 4 of those commits" instead of "hidden coupling". kanspec hidden pairs go 31 to 12 to 6; the number of top-15 hotspots carrying a co-change line goes 9 to 3; the four dropped `done.rs` pairs have zero co-commits left once the five integration sweeps are removed. fd keeps both of its real pairs (lift 9.0 and 13.6).
4. **Cycle reason dedupe and cut suggestion (P61).** "In an import cycle of 15 files" appears on 15 of 56 ranked kanspec files. Print the cycle once with its cheapest cut and name the symbol: paths.rs to plan.rs imports one symbol (`EntityRef`) and cutting it shrinks the SCC 15 to 12; the hub cut (paths.rs's 4 imports) gets to 11; only 5 of the 72 internal edges shrink it at all. Members carry "in the 15-file src cycle (cut: paths.rs -> plan.rs, EntityRef)". Type-only TS imports are excluded; a Rust-cycles-are-idiomatic knob is available but default on, since the score already charges Rust cycles.
5. **Symbol names on clone pairs, and a per-hotspot plan (P62, narrowed).** Resolve every clone run to its enclosing metrics unit: flow.rs 932-962 against 1010-1036 is `park` against `drop_ticket`. Then a PLAN sub-heading per hotspot that orders existing signals (test-region fold, clone pairs by tokens, extract candidates by cognitive, the cycle cut). No predicted score effects: moving flow.rs's test module out *raised* its score from 72.8 to 82.5 because every signal is a within-repo percentile.
6. **A graded `has_tests` (P55, file level only).** Replace the boolean with "referenced by N test units" using exact identifier matches of the file's symbols in test bodies, counting inline test regions. Today cmd/status.rs and cmd/comment.rs print "no test file references it" while 36 and 15 test units name their symbols through subcommand strings. Step the multiplier on 0 versus at least 1; do not go further, because click's near-100%-covered public API is 38% "never named by a test" (singletons, exceptions observed as exit codes).
7. **Bots out of the bus factor (from P13).** click has 138 dependabot and pre-commit-ci commits; "single author (bus factor 1)" is wrong wherever bots commit. One `bot_patterns` list on the log scry already runs.
8. **Fix-word counting under the mass-edit cap.** "Review pass: fix eleven defects" touches 49 files and inflates fix counts on 25 kanspec files; apply the 25-file cap the co-change code already uses.

## Wave 2: new cross-file analyzers

Each of these needs something no linter has: a repo-wide name or literal index, or the git log.

9. **Repeated literal families (P10).** Exact families of string literals in a message role (arguments of `format!`, `println!`, `anyhow!`, `raise`, `throw`, `console.*`, `logger.*`), minimum 20 characters, in 3 or more files; plus config-shaped literals (strftime patterns, env names, paths, URLs, numbers in a const or field-initializer role) in 2 or more files. Near-duplicate families by Jaccard did not separate the corpora and go in as information only. Section with weight 0, plus a per-file reason: "9 of its strings recur elsewhere: 'kanspec show {id}' (line 314) is spelled in 10 files; centralise". Cheap.
10. **Same-name helpers and inlined idioms (P07 + P08).** Group free functions and top-level consts by name across Source files (name length at least 4; sibling-directory twins like `adapter/*/` suppressed), classify each family as verbatim, similar (body Jaccard at least 0.5) or different contract (parameter count or return type differs), and attribute each copy to its introducing commit and session. Then, for every helper of 6 to 40 tokens, search the token streams for its body with identifiers masked but callee names, field names and literals kept (floor 12 tokens, 2 or more occurrences in 2 or more files). Reason: "defines plural (lines 430-436), also defined verbatim in cmd/status.rs:469 and as plural(n, noun) in cmd/proposal.rs:168; its body is also inlined 7 times in 6 files instead of called". Cheap; the same-signature-low-Jaccard class was noise in every corpus and is off by default.
11. **Dead and over-exported symbols, test-only surface, dead shapes (P01 + P02 + P03).** One extra walk builds name to file to [prod refs, test refs] over every parsed tree (including identifiers inside Rust `token_tree`s and `{name}` inline format args, which the prototype found as the main false-positive class). Categories: DEAD (no reference anywhere), TESTONLY (referenced only from test context, strict rule: 8 of 8 correct on kanspec), OVEREXPORTED (referenced only inside its own file). Rust-first, since `lib.rs` declaring every module `pub mod` silences rustc's dead-code lint: kanspec has 394 `pub fn` against 25 `pub(crate) fn`, and 23.5% of its pub items (26% of scry's) have zero external production references versus fd 7.7% and ripgrep 8.8% (1.8% in library mode). `hooks::install` has no caller in 54 files; `git.rs` `is_ignored`/`is_tracked`/`object_exists` exist only for tests. Report DEAD and TESTONLY per symbol; OVEREXPORTED as one per-file percentile line without per-symbol "narrow to private" advice. Never-constructed enum variants and never-read fields (`Ticket.mtime` written at 9 sites, read nowhere; `Color::Blue` matched but never built) ride along as a sub-reason: tiny yield, 5 of 5 precision. Library mode auto-detected from the manifest. Cheap.
12. **Parameter tuple clumps (P46, with P47's silenced-slot annotation).** Parameter-name tuples of 3 or more that recur across 4 or more functions or 2 or more files, requiring type text to agree per slot where annotations exist. kanspec: `(a, f, s)` in 8 functions across 5 files and `(s: &Snapshot, f: &Facts, a, _m: &Minter)` in 5, with `_m` unused in all 5. Human clumps exist too (fd's `(config, entry, stdout)` x7) but have no silenced member. Reason: "introduce a PlanInput struct or drop the slot". Cheap.
13. **Declared surface nothing consumes (P64 + P65 + P66, one Cargo-only pass).** Dependencies declared in Cargo.toml that no file imports (kanspec: `pulldown-cmark` and `rusqlite`, born in the root commit and never imported in 102 commits; also real hits in ripgrep's `fst` and fd's `libc`), features no `cfg(feature)` consumes (`ci-homerunner`, whose last consumer was removed by the review-pass commit; 0 of 16 human features dead), and serde config sections parsed and never read (`[ci.homerunner]`, four documented fields, zero reads outside config.rs). What scry adds over cargo-machete is the history line: "declared in 988d9b5, never imported since; feature orphaned by 9f2f0f7". Skip dev-dependencies; treat `required-features` as consumers; count reads in the defining file too (the prototype's biggest false-positive source was excluding them). Cheap. JS and Python manifests later, if at all.
14. **Test-only env seams (P67).** Environment variables read by production code whose only setters and mentions live in test files and that no user doc mentions: kanspec has 6 (`KANSPEC_ACTOR`, `_ACTOR_KIND`, `_GH_FIXTURES`, `_ID_SEED`, `_NOW`, `_NO_BROWSER`), the four human repos 0 (ripgrep's `RIPGREP_CONFIG_PATH` is exempt by its doc mentions). Weight 0, reason "inject or document". Product prefixes come from the manifest's package and bin names. Cheap.

## Wave 3: richer reasons on functions the report already ranks

These add nothing to the score. They make the existing "worst X at N" line say what to do.

15. **Banner partitions and phase sections (P17 + P18)** as one comments pass sharing a banner regex; details above. Emit banners only for files already above the 80th size percentile or in the hotspot list; emit phases only for functions over the cognitive threshold. Cheap.
16. **Short-name live range (P48).** One-letter bindings are about 20% of all bindings in both LLM repos versus 1 to 4% in the human ones, and the ones that stay live for 30 or more lines run 23 per 1000 bindings in kanspec versus at most 2 in any human repo. Every top hit is a 150 to 290 line block (`h` in board.rs lives 538 to 745; `t` in derive.rs 787 to 992; `p` in proposal.rs 690 to 884). Word the reason as "this block is too long to carry a one-letter name", make last-use rebinding-aware (closure parameters and match patterns re-bind the same letters), and use the largest gap between uses rather than the raw span so dense accumulators do not fire. Cheap.
17. **Locals on FunctionMetrics (P44).** Add a `locals` count and append it to the cognitive reason: "close: cognitive 42, 258 lines, 34 locals". Not a tell (kanspec 0.6% brain methods, fd 0.65%) but the most precise "split this" reason tested; nothing in the top 10 of any corpus was noise. Cheap.
18. **Parse-default fallbacks (P28, narrowed).** Raw fallback density does not separate (kanspec 0.14 per function, fd 0.12, click 0.12), but a literal default applied to the result of `split`/`next`/`parse`/`strip_prefix`/`from_utf8` does: every real hit was that shape and no human corpus put one in its top hits. scry's own `history/mod.rs:92-98` turns a malformed git-log line into author "" at timestamp 0 with three `fields.next().unwrap_or("")`. Extra reason on hotspots when a function has 2 or more such sites. Cheap.

## Worth a ticket, not first

- **Provenance block (P13).** Parse agent author names and `Co-Authored-By` / `Claude-Session` trailers from the log already fetched: repo-level agent commit share, distinct sessions, delete-to-add ratio; per file, agent line share and session count ("touched by 5 sessions in 14 commits"). Not a score input: it is a property of the repo (kanspec 99% agent lines) and it reads scry, which is 100% LLM-written, as 0% because its commits carry no trailers.
- **Function-level churn via blame (P26).** Reason-only, computed for the top-N candidates, honouring `.git-blame-ignore-revs`. It attributes a file's churn to named functions, which is what the hotspot line lacks; the "LLMs rewrite whole functions" claim did not survive (fd 17% wholesale rewrites, kanspec 21%).
- **Error erasure (P29, narrowed).** `Err(_)` arms that answer the question with a literal (`Err(_) => true` in `glob_matches_anything`), and `let _ =` on non-write calls; `.ok()?` in Option-returning functions runs 19% in kanspec but 18% in fd, so it is not the tell. Documented "best-effort" sites must be exempt.
- **Write-then-rewrite and fix latency (P22, P23).** 11 of kanspec's 46 born-in-window files were rewritten more than their birth size within 7 days (0 human files reach 0.5), and 17 of 25 large additions got their first fix within 8 hours (ripgrep's fastest is 141 hours). Both are confounded by repo age, squash merging and commit density; ship only as extra phrases on files already ranked by churn, with grafted and root commits excluded.
- **Grab-bag split direction (P41).** Connected components over the symbol-to-importer graph, reported as a "which way to split" clause on the existing "imported by N files" reason; label shared-type modules instead of calling them grab-bags. 1 to 2 hits per repo, some real (scan.rs holds two `done`-command types).
- **Assertion-free tests (P49) and twin tests (P52).** Zero-assert tests are 0.5 to 1% in every corpus once assertion macros and helpers resolve through the import graph, so a per-test list, not a percentile; twin tests are 0% in both LLM repos and 5 to 10% in the human ones, but every hit is a mechanical `parametrize` / `it.each` rewrite, so a knob on the clone pass (`include_tests`, identifier-preserving hashing). Substring assertions on CLI output are 30% of kanspec's assertion sites versus 2 to 12% elsewhere (P50), but that is the correct idiom for unstable output, so at most a note on P49's list.
- **Orphaned-by commit (P05).** For DEAD symbols only, behind an `--explain-dead` flag or inside `scry context`: the commit that removed the last caller and the sibling that replaced it (`hooks::install` lost its callers to `plan_install` in 2530096). Works; too pickaxe-heavy for every scan.
- **Doc-declared surface drift (P68).** Names in a README or design doc's stack line, config fence, or `PRODUCT_` env mention that the manifest or code no longer has. One real hit across six repos (`gray_matter` in DESIGN.md); exact when it fires, so a low-priority addition to the declared pass.
- **Rust ownership: same value re-owned in a loop (P71) and borrowed returns every caller re-owns (P74).** `close` clones `i.id` seven times in one loop body; `blocked_by` returns `Vec<&TicketId>` and 4 of 6 callers immediately `.cloned().collect()`. Real and precise, but about one hit per repo and the fix needs type knowledge; annotation on an existing hotspot only.
- **Generic uniform-sibling table detection also as a size dampener (P40 residue).** Append "defines 55 types, 1 control-flow line" to the size reason for registries like cli.rs; no weight.

## Rejected, so nobody re-proposes them

| id | proposal | why it lost |
|---|---|---|
| P04 | orphan files / test-only modules | 0 real hits in six corpora once manifests are read; the entrypoint detection is infrastructure for P01 |
| P06 | dangling identifiers in comments | no separation (6.6% vs 4.3-5.1% unresolved); the git confirmation as specified always matches |
| P09 | hand-rolled stdlib utilities | 2 real hits in six corpora, both already found by P07/P08; kanspec's `join` is a documented house helper |
| P11 | convention majority / per-file dissent | 0 files dissent on 2+ conventions anywhere; every dissent is test code or semantically required |
| P12 | identifier vocabulary drift | LLM repos have 0 spelling-variant families, ripgrep 3; the flagship `tempdir`/`temp_dir` is two different APIs |
| P14 | session burstiness | measures merge strategy; kanspec's burst files are the human landing worktrees, and all were revisited later |
| P15 | naming profile outlier | within-repo z-distance never reaches 2.0 in six repos; sentence test names are the TS convention |
| P16 | comment prose fingerprint (as a pass) | fitted to one house style: scry scores 0 process refs, 0 em-dashes; survives only as a regex knob on t-eccd |
| P19 | documentation load outliers | human tails heavier at every percentile; the hits are the comments worth keeping |
| P20 | stale comments by blame age | 0 of 5 inspected hits were wrong; dead on shallow clones; expensive |
| P21 | comment-carrying clones | inverted: ripgrep 41.8 duplicate-paragraph groups per 1k vs kanspec 2.3 (trait-impl docs) |
| P24 | accretion-only files | dominated by repo age and renames; survivors are declarative tables |
| P25 | shotgun surgery / divergent change | under its own knobs kanspec yields 0; topic count collapses to commit-subject conventions |
| P27 | mega-commit provenance | uniform in a young repo, one famous commit in a mature one; folds into P26's text |
| P30 | guard prologue | human Python/TS has more guard chains; penalises the flat structure cognitive complexity rewards |
| P31 | infallible Result ceremony | ripgrep 2.1% vs kanspec 1.4%; clippy `unnecessary_wraps` covers it |
| P32 | guard accretion via blame | direction reversed (fd 0.38, ripgrep 0.26, kanspec 0.11); every hit is a wholesale rewrite |
| P33 | single-caller helper ladders | fd 35% ~ kanspec 36%; every top hit is a readable named decomposition; contradicts extract-function advice |
| P34 | pass-through delegation | LLM repos have the lowest rate (scry 0%, kanspec 2.1%, hono 5.2%); hits are idiomatic re-exports |
| P35 | redundant caller guard | 1 marginal pair in 844 units, documented as intentional |
| P36 | optional parameter creep | inverted (LLM optional ratio 0.009/0.020 vs human 0.054/0.061); 0 flags in LLM non-test code |
| P37 | argument order contradicts names | 0 strict hits in ~1,800 calls; pylint W1114 exists; revisit only in diff mode (t-4569) |
| P38 | LCOM4 cohesion split | with sibling calls counted, LLM code is *more* cohesive (27% vs 35% at LCOM4 >= 3) |
| P39 | feature envy | ~1% of methods everywhere, highest in human corpora; needs types to separate data from behaviour |
| P40 | god file / type zoo | 4-6% of files in every corpus, all intentional registries; residue is a phrase on the size reason |
| P42 | single-implementation abstractions | default run reports 0 on all six corpora; kanspec's only candidate is macro-generated |
| P43 | same-file parameter objects | 1-10% with no separation; the fix contradicts `too_many_arguments` |
| P45 | message chains / config drilling | 7.2 vs 0.3 per kloc but nearly all `x.fm.field.clone()`; an accessor relocates the chain |
| P47 | silenced parameter families (standalone) | two kanspec families, one a deliberate capability token; the other is P46's clump |
| P50 | weak / tautological assertion share | substring checks are the right idiom for unstable CLI output; tautologies are linter territory |
| P51 | setup-to-assert ratio | counted in statements, every corpus has median 1-2; the gate fires on nothing in either LLM repo |
| P53 | mock-heavy tests | neither LLM corpus contains a mock; 0 of 5 human hits is a problem |
| P57 | test re-implements the oracle | 0 instances in six corpora |
| P58 | tests section / test-file score | composite of analyzers that do not exist; ranking driven by which assertion idiom the list missed |
| P63 | generated-code index / CI gate | measures provenance, not risk; fd fails on false components; survives only as a plain `--gate` over existing signals |
| P69 | planned-feature cluster | its single hit is a recorded, documented deferral (D-24) with live consumers |
| P70 | allocation churn density | 30 vs 9.4 per kloc but not defect-predictive by its own blame check; #1 hit is noise |
| P72 | clone-fan struct assembly (as a pass) | targets are serde DTOs; the prescribed lifetime fix is wrong |
| P73 | allocation at a call boundary | 9 of 10 sites are required (`glyph::OK` is a `char`); clippy `unnecessary_to_owned` covers the rest |
| P75 | conversion round-trips | spelling habit (`as_str().to_string()`); the Display/From impls it asks for already exist |
| merged | docstring restates signature | rarer in LLM code than human (0.12 vs 0.97-1.35 per kloc) |
| merged | annotated type re-check | unmeasurable on Rust corpora; needs type info; typescript-eslint territory |
| merged | name-behaviour mismatch | 0 hits in scry, 1 in kanspec, more in human code; per-function, no cross-file component |
| merged | clone-then-return getters | 1 hit in kanspec, 2 in ripgrep; the broader pattern is a human idiom |

## What the verification taught us

- **Count what comments and names are about, not how many there are.** The signals that fired on both LLM repos and on none of the human ones are all content or structure counts: banners (37 per 1k comment lines, 0 human), one-letter bindings (~20% vs 1-4%), pub items nobody outside the file references (23-26% vs 2-9%), repeated hint strings, test-only env seams. Every volume metric (comment ratio, function length, guard count, Option count, fallback density) either did not separate or ran the wrong way.
- **Distrust anything that fired on kanspec but not on scry.** Em-dashes, ticket references, `Claude-Session` trailers, write-then-rewrite, fix latency and literal families all read as LLM tells on kanspec and as human on scry. They measure one team's prompting and commit style. They can still be useful knobs; they cannot be defaults.
- **Several classical LLM-smell intuitions inverted.** The LLM repos were more vocabulary-consistent, more cohesive, declared fewer optional parameters, wrote fewer pass-through wrappers, and named more of their public functions in tests than the human baselines.
- **History signals mostly measured workflow.** Two 108-file integration commits, an 11-minute median commit gap and squash-flat blame dominate every git-derived proposal on kanspec. Where blame earned its keep it was as attribution to named functions, not as a new score term.
- **The clone pass needs the fixes more than it needs a lower floor.** Inline test fixtures, self-matching uniform runs and registry tables account for most of what it prints on Rust repos today; the duplication LLMs actually produce sits below its floor and is better caught by name, literal and structural passes.

## Suggested kanspec tickets

If the waves above are filed as tickets, the natural grain is one per numbered item, with 1 to 5 and 7 to 8 as small changes to existing passes, 9 to 14 as new passes with a spec each, and 15 to 18 as additions to the metrics reason. The appendix chapter for each id carries the Rules a spec can start from.
