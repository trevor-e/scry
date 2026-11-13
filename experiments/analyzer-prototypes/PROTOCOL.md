# Analyzer experiment contract

The user requested prototypes, real Rust/TypeScript repository runs, and an honest audit of useful findings and false positives.

- Work only in your assigned subdirectory; do not edit production scry code or another prototype.
- Implement a rerunnable Python CLI taking repository paths. Use the temporary interpreter at `/private/tmp/scry-analyzer-venv/bin/python`; tree-sitter, tree-sitter-rust, and tree-sitter-typescript are being installed there.
- Treat corpus repositories as read-only. Scan tracked source files, omit vendored/generated/dependency trees, and explicitly classify test/example contexts. Do not execute downloaded project code or build scripts.
- Record repository HEAD, scan scope, candidate counts, unsupported cases, and exact source locations. Store raw machine-readable results separately from manual audit judgments.
- Use narrow syntax rules and abstain when semantics are unknown. Missing knowledge is not a confirmed defect.
- Validate the detector through small positive and negative fixtures exercising real CLI paths. Synthetic examples do not count as corpus findings.
- Inspect actual real-source candidates, including their surrounding code. Explain useful findings, intentional differences, false positives, and unknowns. Do not claim a defect or a precision rate without evidence.
- Keep implementation, fixture validation, raw results, and audit JSON in your subdirectory. Return concise counts and the strongest source references to the parent.
- Shared public corpus root: `/private/tmp/scry-analyzer-corpus`. Parent will provide available repository names as clones complete.
