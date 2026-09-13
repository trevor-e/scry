---
feature: Near-exact clone detection via normalized-token winnowing fingerprints
code: [src/clones/**]
---
# clones

## Rules
- Tokens are tree-sitter leaves with identifiers → `ID`, strings/JSX text → `STR`, numeric and boolean/null literals → `NUM`, comments dropped. Everything else keeps its node kind, so renamed copies match and reordered statements do not.
- Fingerprints are winnowed k-grams (K=30 tokens, W=20): any shared run of ≥ K+W-1 tokens is guaranteed to be found. Fingerprints seen in more than 40 places are boilerplate and skipped.
- Matches on one diagonal merge into runs, then each run is extended token-by-token in both directions while the tokens still match, so reported ranges are exact rather than fingerprint-sampled.
- Runs under `MIN_TOKENS` (70) are dropped. Per file pair, a run whose A-range and B-range both overlap a longer accepted run is the same clone at an offset and is dropped.
- Same-file runs whose two ranges overlap each other are repetition (tables, unrolled loops), not clones, and are dropped.
- Per-file `clone_lines` is the union of all reported ranges touching the file; `clone_ratio` divides by total lines.
- Only Source files are compared. Tests and fixtures duplicate by design.
