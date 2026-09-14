---
feature: 'Inline test regions in Rust Source files: #[cfg(test)] mods and items, bare #[test] fns'
code: [src/regions.rs]
---
# regions

## Rules
- A test region is the byte range of the item an `attribute_item` applies to (the next named sibling, looking past further attributes and comments) when the attribute is `cfg` with token-tree text exactly `test`, or of a `function_item` following an attribute whose text is exactly `test`. `#[cfg(not(test))]` is not a region.
- Region kind is `cfg_test_mod` when the item is a `mod_item`, `test_fn` for a bare `#[test]` fn, else `cfg_test_item` (fn inside an impl, free fn, `use`, macro).
- Regions are found at every depth of the tree; a region nested in another is merged into the outer one, so line counts never double-count.
- Regions are computed on the caller's already-parsed tree (metrics and clones each pass their own), never with a second parse.
- Rust only. TS, TSX, JS and Python files never get regions; `[tests].inline_modules = false` turns detection off everywhere.
