---
feature: Walk a repo, classify files (source/test/data/generated/vendored), detect language
code: [src/discover/**, src/lang/**]
---
# discovery

## Rules
- Only `Source` files are ever ranked. Test, Data, Generated and Vendored files are tracked for context (test proximity, size totals) but never appear as refactor candidates.
- Classification is path-first, then content: vendored dirs > generated markers/dirs > test dirs/names > fixture dirs/names > logic-density threshold (≥150 lines with <4% control-flow lines is Data).
- Walks honour `.gitignore` and skip hidden dirs; anything the repo does not track is not analysed.
- Paths are reported relative to the scanned root with `/` separators on every platform.
