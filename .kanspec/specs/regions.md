---
feature: 'Inline test regions in Rust Source files: #[cfg(test)] mods and items, bare #[test] fns'
code: [src/regions.rs]
---
# regions

## Rules
- A test region is the byte range of the item an `attribute_item` applies to (the next named sibling, looking past further attributes and comments) when the attribute is `cfg` with token-tree text exactly `test`, or of a `function_item` following an attribute whose text is exactly `test`. `#[cfg(not(test))]` is not a region.
- Region kind is `cfg_test_mod` when the item is a `mod_item`, `test_fn` for a bare `#[test]` fn, else `cfg_test_item` (fn inside an impl, free fn, `use`, macro).
- Regions are found at every depth of the tree; a region nested in another is merged into the outer one (byte ranges), and the inline line count is the union of the regions' line spans, so a line shared by two regions (`#[cfg(test)] use a; #[cfg(test)] use b;`) counts once and the count never exceeds the file.
- Regions are computed on the caller's already-parsed tree (metrics and clones each pass their own), never with a second parse.
- Rust only. TS, TSX, JS and Python files never get regions. `[tests].inline_modules = false` turns the effects off (no unit tags, no line subtraction, no clone stripping); metrics still list the regions, and the mentions pass finds the units inside them on the same tree, so `test_units` does not change with the knob.
