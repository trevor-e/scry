---
feature: Import graph, cycles (SCC), fan-in/out, instability
code: [src/deps/**]
---
# deps

## Rules
- Imports resolve against the discovered file set only, never the filesystem, so the graph is consistent with every other pass. Unresolved specifiers count as `external`.
- Vendored files are not in the graph at all (their cycles are not ours). Generated files can be imported but their own imports are not walked, so they never form cycles.
- Python: absolute imports try each ancestor directory of the importing file as a root (nearest first); `from pkg import name` also edges to `pkg/name.py` when it exists. Relative imports resolve from the file's package.
- JS/TS: relative specifiers try the exact path, then the JS-family extensions, then `index.*`; `./x.js` matches `x.ts`. `@/` and `~/` map to the nearest `src/`. Bare specifiers are external.
- Rust: `mod x;` resolves to `x.rs` / `x/mod.rs` under the file's child dir; `crate::`, `super::` and `self::` paths resolve to the longest prefix naming a file. `use` trees are flattened (`crate::{a::X, b::{self, Y}}` is three paths), `path::*` depends on the module at `path`, and inside an inline `mod … { }` each `super` first climbs the inline module before it reaches the parent file.
- Cycles are Tarjan SCCs of size ≥2, reported at file level and again with files collapsed to directories (test files excluded from the directory graph).
- `fan_in` counts non-test importers; test importers are reported separately as `test_refs` and feed the test-proximity signal.
- `instability` = fan_out / (fan_in + fan_out).
- JS/TS import-prefix aliases are `[deps].js_aliases` in `scry.toml` (`"@/" = "src"` by default); a target is looked for under every ancestor of the importing file.
- Every edge carries `kind` (`use`, `mod` for a Rust `mod x;` declaration, `type_only` for TS `import type` / `{ type X }` / `export type { X } from`) and `symbols`: distinct imported names per resolved file (Rust `use` leaves, TS specifiers and default names, Python `from x import a, b` names; a plain `import x` / `require` is one), plus `[deps].glob_import_symbol_cost` (20) per wildcard (`use x::*`, `from x import *`, `import * as ns`, `export * from`). A file that both declares `mod x;` and `use`s it has one `mod` edge.
- A file cycle with at least `min_cycle_size_to_cut` (4) members gets `cuts`: `type_only` edges are left out of the searched subgraph when `ignore_type_only_imports`; up to `max_edges_tried` (400) internal non-`mod` edges are removed one at a time, cheapest first, and the best single cut is the one leaving the smallest largest SCC, then the fewest symbols (`mod` edges are never proposed). The hub cut drops every non-`mod` import one member makes inside the cycle and is kept when `report_hub_cut` and it leaves a smaller cycle than the single edge. The greedy cut set repeats the best single edge on the residual until every cycle is under `min_cycle_size_to_cut` and is reported only when that takes at most `max_cut_set` (6) edges. `no_single_break` is set when there is a runtime cycle (largest SCC ≥ 2 without the ignored type-only edges) and the best single cut still leaves at least `no_single_cut_share` (0.8) of the members in a cycle. The hub cut records `glob` when one of the dropped imports is a wildcard, so its symbol count prints as a floor (`>= 24 symbols`).
- Printed file names are basenames, except `mod.rs`, `index.*` and `__init__.py`, which carry their directory (`searcher/mod.rs`, `dom/index.ts`): the name alone says nothing in a crate with several. A cycle whose members share no directory (`dir` = `.`) prints `at the repo root`.
- `cut_rust_cycles = false` computes no cuts for an all-Rust cycle (the cycle keeps its count line); cuts sit in `file_cycles[].cuts` of `scry deps --json` and `scry scan --json`, and every cycle carries `dir`, the deepest directory holding all its members.

