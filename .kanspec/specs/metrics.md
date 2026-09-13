---
feature: 'Tree-sitter per-function metrics: cognitive/cyclomatic complexity, nesting, length, params'
code: [src/metrics/**]
---
# metrics

## Rules
- Cognitive complexity follows SonarSource: +1 per control-flow break (if/for/while/switch/match/catch/ternary/comprehension-for), +1 more per nesting level it sits in, +1 flat for else/elif/else-if, +1 per run of the same boolean operator. Nested functions, lambdas and closures add a nesting level but no increment.
- Cyclomatic complexity is 1 + decision points (branches, loops, cases/arms, catch/except, ternaries, every boolean operator). `else` is not a decision point.
- Reported units are named functions/methods; arrow functions and function expressions are units only when bound to a name (variable, property, field). Inline callbacks nest into the enclosing unit.
- A nested unit is reported on its own and also counts toward every enclosing unit, so a React component's score includes its handlers.
- Methods are qualified `Owner.method` using the nearest class/impl/trait ancestor.
- Parameter counts exclude `self`/`cls` and Rust `self` receivers.
- The "hard to follow" threshold is cognitive > 15 (`COGNITIVE_HARD`); files report how many units exceed it.
- Parse errors are reported per file but never abort analysis: tree-sitter recovers locally and the rest of the file is still measured.
