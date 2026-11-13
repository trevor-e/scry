# Shadowed Serde variants experiment

**Result: zero real findings.** Across 3,150 tracked Rust files in ten repositories,
the scanner found 57 untagged enum declarations. Of these, 41 derive Deserialize;
56 of their 100 variants had supported shapes, yielding 27 comparable ordered pairs.
No earlier variant contained a later variant's complete modeled JSON shape.
This does not establish precision or prove the remaining variants reachable.

The useful distinction is full shadowing versus partial overlap: overlapping
variants are often intentional. Tauri's integer-then-float enum, for example,
still admits fractional values in the second variant. Similarly, serialize-only
response enums in kanspec and adulting are outside this check's scope.

Do not promote this to a default analyzer on the present evidence. It remains
a candidate for a focused serialization corpus or an optional framework check.

## Run

From the repository root, using the experiment's temporary interpreter:

```sh
/private/tmp/scry-analyzer-venv/bin/python experiments/analyzer-prototypes/serde/check.py /path/to/repository --output /tmp/serde-results.json
/private/tmp/scry-analyzer-venv/bin/python experiments/analyzer-prototypes/serde/test_cli.py
cargo test --offline --manifest-path experiments/analyzer-prototypes/serde/fixtures/Cargo.toml --target-dir /private/tmp/scry-serde-fixture-target
```

The CLI accepts multiple repository roots. It scans tracked `.rs` files and
records HEAD, source locations, exclusions, unsupported cases, and contexts.
`--fixtures` permits a non-Git fixture directory. Dependency/generated trees and
fixture-data paths are excluded; test and example code is classified separately.
Downloaded corpus code is never compiled or executed. Only our synthetic fixture
crate is executed, using cached dependencies and a separate temporary target dir.

Dependencies: Python 3.10+, tree-sitter 0.26.1, tree-sitter-rust 0.24.2.
Runtime fixtures lock Serde 1.0.229 and serde_json 1.0.154.

## What is modeled

- Built-in JSON scalar shapes, integer ranges, Option, Vec, Box, and string-keyed maps.
- Required fields, Option fields, ordinary defaults, field renames, and unknown-field policy.
- Locally resolved, non-generic derived structs and simple aliases.
- Named structs' sequence representation, as well as their object representation.
- Inline untagged struct variants' object-only representation.

The last distinction prevents a misleading finding: an earlier struct with one
field may accept a later two-field struct's object representation, but the later
variant remains reachable through a two-element JSON sequence. Another guard
rejects the optional-field trap: a later permissive object may accept an arbitrary
value for an earlier variant's optional field, causing that earlier variant to fail.
Enum `rename_all` changes variant names; it does not rename variant fields.

The scanner abstains on custom Deserialize/default functions, flatten, aliases,
conditional fields/variants, external or recursive types, and other unsupported
attributes. The analysis concerns JSON value shapes, not all Serde formats or
duplicate-key document behavior. Unknown knowledge is never treated as a defect.

## Evidence

- `raw-corpus.json`: raw results for all ten repository HEADs.
- `audit.json`: manual review judgments for eight representative nonfindings and exclusions.
- `raw-fixtures.json`: raw synthetic CLI results.
- `fixtures/cases.rs`: six positive examples, five negative examples, five abstention examples.
- `test_cli.py`: two CLI tests; the Rust fixture crate has two behavior tests.

All four tests pass. Synthetic examples are not counted as real findings.

Serde's [enum representation documentation](https://serde.rs/enum-representations.html)
specifies first-successful-variant behavior. The named-struct versus inline-variant
sequence distinction was checked against local Serde derive source and executed
through the synthetic runtime fixtures.
