# scry

Find code worth reviewing, with ranked hotspots, reasons, and source locations.
Supports Python, TypeScript/TSX, JavaScript, and Rust. Built with Rust and tree-sitter.

## Quick start

From this checkout:

```bash
cargo install --path .
scry scan /path/to/repo
```

Use the repository root to include Git history. Scans use the last six months by default.

## Usage

```bash
scry scan <repo> --json                 # Machine-readable report
scry scan <repo> --top 10               # Limit entries per section
scry scan <repo> --since "3 months ago"  # Change the history window
scry scan <repo> --no-history           # Static analysis only
scry plan <file> --root <repo>          # Refactor suggestions for one file
scry metrics <repo>                    # Run an individual analyzer
```

Run `scry --help` for all commands or `scry <command> --help` for options.

## Analyzers

| Analyzer | What it finds |
|---|---|
| History | Frequently changed files, fix commits, and files that change together. |
| Metrics | Complex, deeply nested, or oversized functions. |
| Dependencies | Import cycles and heavily depended-on files. |
| Clones | Repeated code, with table-shaped matches identified separately. |
| Test mentions | Symbols referenced by tests and public symbols no test names. |
| Dead symbols | Unused exports, test-only code, and unread Rust fields. |
| Helpers | Similar helpers and helper bodies copied across files. |
| Strings | Repeated messages and configuration literals. |
| Parameter clumps | Parameter groups repeated across functions, including unused parameters. |
| Declarations | Unused Cargo dependencies, features, config fields, and test-only environment settings. |
| Comments | Comment sections that suggest where large files or functions could split. |
| Naming | Short variable names whose uses are far apart. |
| Fallbacks | Defaults that may hide failed parsing or missing values. |

File discovery separates source, tests, data, generated, and vendored files.
Inline Rust tests are identified separately. Findings feed the hotspot ranking
and per-file refactor suggestions.

## Reading the report

Hotspot scores combine change frequency and complexity with signals such as
coupling and duplication. Scores are relative to the scanned repository, not
quality grades or defect probabilities.

Findings include symbols and line ranges to inspect. Treat refactor suggestions
as review candidates: matching code or values can be intentional, and test
mentions are not measured coverage: integration and parent-component tests can
exercise a file without naming its symbols.

Static import and re-export blocks are excluded from clone suggestions.
Environment-name findings require a literal key in a recognized environment
access, rather than uppercase spelling alone. Test-support directories such as
`testutils`, `test_utils`, and `test_support` are classified as tests by default;
override `[discover].test_dirs` for repositories with different conventions.

## Configuration

Add a `scry.toml` at the repository root with only the settings you want to change:

```toml
[discover]
exclude = ["art/**"]

[clones]
min_tokens = 100

[plan]
max_steps = 10
```

To see every available setting:

```bash
scry config <repo>
```

Settings apply in this order: defaults → repository `scry.toml` →
`--config <file>` → command-line flags. Unknown keys are errors.

## Language notes

- Python and TypeScript dead-symbol checks are opt-in via `[dead.symbols]`.
  Dependency-declaration checks currently support Cargo manifests only.
- Python absolute imports use detected package roots. Set `[deps].py_roots`
  for custom layouts or scripts with sibling imports, such as
  `[".", "src", "scripts/scm"]`. Namespace-package imports are supported.
- Python `TYPE_CHECKING` and `typing.TYPE_CHECKING` guards are excluded from
  runtime cycle-cut suggestions. Aliased guards and compound conditions are
  not evaluated.

## Development

```bash
cargo build --release
cargo test
```

Work is tracked with kanspec (`kanspec ready`, `kanspec status`).
