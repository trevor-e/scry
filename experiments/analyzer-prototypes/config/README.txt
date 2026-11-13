Environment default drift prototype

Run:
  /private/tmp/scry-analyzer-venv/bin/python experiments/analyzer-prototypes/config/validate.py
  /private/tmp/scry-analyzer-venv/bin/python experiments/analyzer-prototypes/config/prototype.py REPO [REPO ...] --output results.json

Dependencies: tree-sitter 0.26.1, tree-sitter-rust 0.24.2, tree-sitter-typescript 0.23.2.
The scan reads tracked files via git ls-files; it never executes corpus code.

raw-results.json: locations, repo HEADs, counts, unsupported shapes, syntax-only candidates.
audit.json: independent source-inspection judgments and limitations.
validation.json: 19 CLI fixture outcomes; fixtures live in validate.py.

Result: three candidate groups across ten repositories. One useful benchmark-report
discrepancy, two intentional differences, zero confirmed runtime defects.
No production or Rust candidate groups. See audit.json before using counts.
