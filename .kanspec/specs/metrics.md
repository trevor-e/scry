---
feature: 'Tree-sitter per-function metrics: cognitive/cyclomatic complexity, nesting, length, params'
code: [src/metrics/**]
---
# metrics

## Rules
- Cognitive complexity follows SonarSource: +1 per control-flow break (if/for/while/switch/match/catch/ternary/comprehension-for), +1 more per nesting level it sits in, +1 flat for else/elif/else-if, +1 per run of the same boolean operator. Nested functions, lambdas and closures add a nesting level but no increment. `with` is not a break, and Python's try/for/while `else` is not a branch; only the `else` of an `if` counts.
- Cyclomatic complexity is 1 + decision points (branches, loops, cases/arms, catch/except, ternaries, every boolean operator). `else` is not a decision point, and a switch/match head adds nothing beyond its cases.
- Reported units are named functions/methods; arrow functions and function expressions are units when bound to a name (variable, property, field) or when no unit encloses them (route handlers, `forwardRef`/`memo` components, `describe` blocks), named after the call they are passed to: `app.post('/x')`. Inline callbacks inside a unit nest into it.
- A nested unit is reported on its own and also counts toward every enclosing unit, so a React component's score includes its handlers.
- Methods are qualified `Owner.method` using the nearest class/impl/trait ancestor.
- Parameter counts exclude `self`/`cls`, Rust `self` receivers, and Python's bare `*` / `/` separators.
- The "hard to follow" threshold is cognitive > 15 (`COGNITIVE_HARD`); files report how many units exceed it.
- Parse errors are reported per file but never abort analysis: tree-sitter recovers locally and the rest of the file is still measured.
- The AST walk is iterative (explicit stack), never recursive: a single deeply nested expression must not overflow a worker thread's stack and abort the scan.
- The "hard to follow" threshold is `[metrics].cognitive_hard` in `scry.toml`.
- A unit whose start lies inside an inline test region (see `regions`) stays in the functions list tagged `in_test = true` and `scry metrics` prints it with ` (in inline tests)`; file totals (`functions`, `total_cognitive`, `max_cognitive`, `max_nesting`, `complex_functions`) count only untagged units.
- Every file reports `inline_test_lines` (lines spanned by its merged regions) and `test_regions[]` with kind and line range.
