---
id: q-eb76
title: tree-sitter-typescript 0.23 rejects a bare '&' inside JSX text ("skills & items") that TSX/Babel accept; shows up as parse_errors=true on otherwise valid files. Recovery is local, metrics stay usable.
paths: [src/metrics/**, src/lang/**]
severity: gotcha
status: active
source: t-cabf
fixed_by: null
---
tree-sitter-typescript 0.23 rejects a bare '&' inside JSX text ("skills & items") that TSX/Babel accept; shows up as parse_errors=true on otherwise valid files. Recovery is local, metrics stay usable.
