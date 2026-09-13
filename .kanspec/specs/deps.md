---
feature: Import graph, cycles (SCC), fan-in/out, instability
code: [src/deps/**]
---
# deps

## Rules
- Imports resolve against the discovered file set only, never the filesystem, so the graph is consistent with every other pass. Unresolved specifiers count as `external`.
- Python: absolute imports try each ancestor directory of the importing file as a root (nearest first); `from pkg import name` also edges to `pkg/name.py` when it exists. Relative imports resolve from the file's package.
- JS/TS: relative specifiers try the exact path, then the JS-family extensions, then `index.*`; `./x.js` matches `x.ts`. `@/` and `~/` map to the nearest `src/`. Bare specifiers are external.
- Rust: `mod x;` resolves to `x.rs` / `x/mod.rs` under the file's child dir; `crate::`, `super::` and `self::` paths resolve to the longest prefix naming a file.
- Cycles are Tarjan SCCs of size ≥2, reported at file level and again with files collapsed to directories (test files excluded from the directory graph).
- `fan_in` counts non-test importers; test importers are reported separately as `test_refs` and feed the test-proximity signal.
- `instability` = fan_out / (fan_in + fan_out).
