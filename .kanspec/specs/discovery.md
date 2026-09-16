---
feature: Walk a repo, classify files (source/test/data/generated/vendored), detect language
code: [src/discover/**, src/lang/**]
---
# discovery

## Rules
- Default test directories include the exact names `testutils`, `test_utils`, `testutil`, `test_util`, `testsupport` and `test_support`. They are test context even under src; arbitrary names containing "test" (such as contest or latest) are not. Replacing `discover.test_dirs` overrides these defaults too.
- Only `Source` files are ever ranked. Test, Data, Generated and Vendored files are tracked for context (test proximity, size totals) but never appear as refactor candidates.
- Classification is path-first, then content: vendored dirs > generated markers/dirs > test dirs/names > fixture dirs/names > logic-density threshold (≥150 lines with <4% control-flow lines is Data).
- A control-flow line carries a keyword (`if`, `for`, `return`, `try`, `else`…) as a whole word, or `=>`. Substrings never count: `sentry` and `country` are not `try`, so a route table or a country list is Data.
- Walks honour `.gitignore` and skip hidden dirs; anything the repo does not track is not analysed.
- Paths are reported relative to the scanned root with `/` separators on every platform.
- Directory name lists, test-file name rules, generated markers, the data-file threshold and `exclude` globs are all `[discover]` settings in `scry.toml`; the defaults are the historical constants.
