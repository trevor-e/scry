# Understanding a report

## Clone categories

Scry matches normalized tokens, then checks AST context on both sides:

- **Logic:** potential shared behavior to review.
- **Signature:** predominantly function headers or interface declarations.
- **Configuration:** predominantly declarative assignments outside functions,
  including constructor-shaped calls. Uppercase constructor names are a heuristic.
- **Table:** uniform repeated entries, such as registries and dispatch tables.

A match must satisfy the configured `dominant_child_share` on both sides to be
classified as a signature or configuration. Recognized tables retain their category;
constructor keyword arguments can be configuration instead. Categories are
heuristics, not proof that two operations have the same behavior.

Logic matches appear first. Signatures and configuration remain visible but
contribute zero to the clone score. Tables use `[clones].table_weight` (default 1).
Only logic matches produce shared-helper suggestions; Scry does not recommend
removing one public operation because another has similar code.

## Families and source differences

Related logic matches form families. Multiple fragments between the same functions
share one comparison and one plan entry per file. Families can be connected
transitively; not every member necessarily matches every other member.

Comparisons use enclosing functions when possible. If a run crosses a function
boundary, the function containing most of its matched tokens supplies the context.
The original matching ranges are preserved in each family's `matches` field.

`shared` contains exact matching line blocks after trimming indentation and ignoring
blank lines. `differences` preserves changed names, literals, comments, and logic,
with file paths and source ranges. These are source differences to inspect, not
claims of a bug or semantic equivalence.

JSON retains up to 12 directly matched comparisons per family, 3 shared blocks and
8 difference blocks per comparison, and 12 lines per excerpt. Functions over 500
lines skip the full comparison. Omitted counts, `truncated`, and `skipped` make
these limits explicit. Text shows the strongest comparison per family; use `--json`
to explore the remaining comparisons.

## Scores and cycles

Each hotspot's `score_breakdown` includes every signal's normalized value, weight,
and contribution in points. Contributions sum to `subtotal`; multiplying by
`test_multiplier` gives `total`, which equals the hotspot score. Text rounds points
for readability; JSON keeps full precision. Scores are relative to the scan and
are not constrained to 0–100.

The clone percentile uses `weighted_clone_lines`: logic lines plus table lines
multiplied by `table_weight`. Thus table weight changes ranking relative to logic;
a weight of zero removes the table contribution. Line counts are disjoint: overlaps
are assigned to logic first, then tables, configuration, and signatures.

`file_cycles` includes type-only imports. `runtime_file_cycles` removes those edges
and recomputes strongly connected components, even below the cycle-cut threshold.
Only runtime membership supplies the cycle score boost. Ordinary import fan-in
still includes type-only imports. Runtime here means an import not recognized as
type-only; Scry does not model import execution order or lazy-import safety.
