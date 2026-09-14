---
id: q-4e7f
title: Inside an inline `mod tests { }` a Rust `use super::*` names the file's own module, not the parent file; resolving it naively gives every module a false edge to main.rs/lib.rs (extract_rust counts enclosing mod_items and rebases the path)
paths: [src/deps/**]
severity: gotcha
status: active
source: null
fixed_by: null
---
Inside an inline `mod tests { }` a Rust `use super::*` names the file's own module, not the parent file; resolving it naively gives every module a false edge to main.rs/lib.rs (extract_rust counts enclosing mod_items and rebases the path)
