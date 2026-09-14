---
id: q-3ecf
title: scan parses each file once in main.rs parse_once and hands the tree to metrics::analyze_tree / deps::imports / clones::tokenize; a new tree-sitter pass must take Option<&Tree> and be wired there, not call lang.parse itself, or it silently re-parses every file
paths: ['src/main.rs,src/metrics/**,src/deps/**,src/clones/**']
severity: gotcha
status: active
source: t-02b6
fixed_by: null
---
scan parses each file once in main.rs parse_once and hands the tree to metrics::analyze_tree / deps::imports / clones::tokenize; a new tree-sitter pass must take Option<&Tree> and be wired there, not call lang.parse itself, or it silently re-parses every file
