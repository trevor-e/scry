"""CLI regression histories for binding-aware whole-function clone matching."""
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

PROBE = Path(__file__).with_name('probe.py')


class TightenedHistoryFixtures(unittest.TestCase):
    def test_matching_respects_bindings_and_observable_differences(self):
        for name, suffix, original, sibling, expected in cases():
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                initialize_history(root, suffix, original, sibling)
                result = scan(root)
                self.assertEqual(result['broad_candidates'], 1, 'Fixture must reach the new structural check')
                survivors = {finding['survivor']['path'] for finding in result['findings']}
                self.assertEqual(survivors, {f'sibling.{suffix}'} if expected else set())

    def test_callback_fix_disappears_after_propagation(self):
        original = callback_body()
        sibling = renamed_callback_body()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            initialize_history(root, 'ts', original, sibling)
            self.assertEqual(
                {f['survivor']['path'] for f in scan(root)['findings']},
                {'sibling.ts'},
            )
            (root / 'sibling.ts').write_text(sibling.replace('> 3', '>= 3', 1))
            git(root, 'commit', '-am', 'propagate boundary handling')
            self.assertEqual(scan(root)['findings'], [])


def cases():
    callback = callback_body()
    renamed = renamed_callback_body()
    yield 'renamed_callback_and_locals', 'ts', callback, renamed, True
    yield 'optional_trailing_parameter_comma', 'ts', callback, renamed.replace('=> number): number[] {', '=> number,): number[] {'), True

    numeric = '''export function first(left: number, right: number, items: number[]): number[] {
  const output: number[] = [];
  for (const item of items) {
    const total = item * left - right;
    if (total > 3) {
      output.push(total);
    } else {
      output.push(0);
    }
  }
  return output;
}
'''
    swapped = rename(numeric, {'first': 'second', 'left': 'a', 'right': 'b', 'items': 'entries'})
    swapped = swapped.replace('item * a - b', 'item * b - a')
    yield 'swapped_parameter_uses', 'ts', numeric, swapped, False

    external = callback.replace(', operation: (value: number) => number', '').replace('operation(item)', 'adjust(item)')
    yield 'different_external_callee', 'ts', external, rename(external, {'first': 'second', 'adjust': 'scale'}), False
    member = callback.replace('const output: number[] = [];',
        'const output: number[] = [];\n  const bounds = { low: 1, high: 7 };\n  output.push(bounds.low);')
    yield 'different_member', 'ts', member, rename(member, {'first': 'second'}).replace('bounds.low', 'bounds.high'), False
    yield 'different_literal', 'ts', callback, renamed.replace('.push(0)', '.push(1)'), False
    operator = callback.replace('const output: number[] = [];',
        'const output: number[] = [];\n  const seed = 2 + 1;\n  output.push(seed);')
    yield 'different_operator', 'ts', operator, rename(operator, {'first': 'second'}).replace('const seed = 2 + 1', 'const seed = 2 - 1'), False

    shadowed = '''export function first(value: number): number {
  let total = value * 2 + 1;
  let observed = total;
  {
    let value = total + 5;
    observed = value * 2;
    if (observed > 10) {
      total += observed;
    } else {
      total -= observed;
    }
  }
  if (total > 3) {
    return total + value;
  }
  return observed + value;
}
'''
    shadow_renamed = '''export function second(input: number): number {
  let sum = input * 2 + 1;
  let seen = sum;
  {
    let local = sum + 5;
    seen = local * 2;
    if (seen > 10) {
      sum += seen;
    } else {
      sum -= seen;
    }
  }
  if (sum > 3) {
    return sum + input;
  }
  return seen + input;
}
'''
    yield 'lexical_shadowing_renamed_consistently', 'ts', shadowed, shadow_renamed, True
    yield 'outer_binding_used_instead_of_shadow', 'ts', shadowed, shadow_renamed.replace('seen = local * 2', 'seen = input * 2'), False

    shorthand = callback.replace('): number[] {', '): Record<string, number[]> {').replace('return output;', 'return { output };')
    yield 'shorthand_property_key_is_observable', 'ts', shorthand, rename(shorthand, {'first': 'second', 'output': 'results'}), False

    different_type = rename(callback, {'first': 'second'}).replace('items: number[]', 'items: string[]').replace('value: number', 'value: string')
    yield 'different_parameter_types', 'ts', callback, different_type, False
    defaulted = numeric.replace('right: number, items: number[]', 'right: number, items: number[] = []')
    yield 'different_parameter_defaults', 'ts', defaulted, rename(defaulted, {'first': 'second'}).replace('= []):', '= [1]):'), False
    yield 'copied_hunk_in_different_function', 'ts', callback, renamed.replace('return results;', 'results.reverse();\n  return results;'), False

    default_shadow = "const value = 7;\n" + numeric.replace(
        'first(left: number, right: number, items: number[])',
        'first(left: number = value, right: number = 1, items: number[] = [])',
    ).replace('left: number = value', 'value: number = value').replace('item * left - right', 'item * value - right')
    default_external = rename(default_shadow, {'first': 'second', 'value': 'input'})
    default_external = default_external.replace('const input = 7;', 'const value = 7;').replace('input: number = input', 'input: number = value')
    yield 'parameter_default_tdz_differs_from_free_binding', 'ts', default_shadow, default_external, False

    switch_scope = """const source = 7;
export function first(mode: number, value: number): number {
  let total = value * 2 + 1;
  switch (mode) {
    case 0:
      let inner = source;
      total += inner;
      break;
    default:
      total += value;
  }
  if (total > 3) {
    return total + source;
  }
  return total - source;
}
"""
    switch_external = rename(switch_scope, {'first': 'second'}).replace('total += inner', 'total += source')
    yield 'switch_binding_differs_from_free_variable', 'ts', switch_scope, switch_external, False

    dynamic = shadowed.replace('return observed + value;', 'return (eval)("value");')
    dynamic_other = rename(dynamic, {'first': 'second', 'value': 'input'}).replace('(eval)("input")', '(eval)("value")')
    yield 'parenthesized_eval_preserves_binding_names', 'ts', dynamic, dynamic_other, False

    jsx = shadowed.replace('): number {', '): unknown {').replace('return observed + value;', 'return <value />;')
    yield 'lowercase_jsx_tag_is_observable', 'tsx', jsx, rename(jsx, {'first': 'second', 'value': 'input'}), False

    rust = '''pub fn first(items: &[i32]) -> Vec<i32> {
    let mut output = Vec::new();
    for item in items {
        let adjusted = item * 2 + 1;
        if adjusted > 3 {
            output.push(adjusted);
        } else {
            output.push(0);
        }
    }
    output
}
'''
    yield 'rust_renamed_parameters_and_locals', 'rs', rust, rename(rust, {'first': 'second', 'items': 'entries', 'item': 'entry', 'adjusted': 'sum', 'output': 'results'}), True

    struct_prefix = '#[derive(Default)]\nstruct Bag { output: Vec<i32>, results: Vec<i32> }\n'
    rust_shorthand = rust.replace('-> Vec<i32>', '-> Bag').replace('    output\n}', '    Bag { output, ..Default::default() }\n}')
    rust_shorthand_renamed = rename(rust_shorthand, {'first': 'second', 'output': 'results'})
    yield 'rust_shorthand_field_key_is_observable', 'rs', struct_prefix + rust_shorthand, struct_prefix + rust_shorthand_renamed, False


def callback_body():
    return '''export function first(items: number[], operation: (value: number) => number): number[] {
  const output: number[] = [];
  for (const item of items) {
    const adjusted = operation(item);
    const total = adjusted * 2 + 1;
    if (total > 3) {
      output.push(total);
    } else {
      output.push(0);
    }
  }
  return output;
}
'''


def renamed_callback_body():
    return rename(callback_body(), {'first': 'second', 'items': 'entries', 'operation': 'invoke', 'output': 'results', 'item': 'entry', 'adjusted': 'scaled', 'total': 'sum'})


def initialize_history(root, suffix, original, sibling):
    if original.count('> 3') != 1:
        raise ValueError('Each fixture must contain exactly one boundary to fix')
    git(root, 'init', '-q')
    git(root, 'config', 'user.email', 'fixture@example.invalid')
    git(root, 'config', 'user.name', 'Fixture')
    (root / f'original.{suffix}').write_text(original)
    (root / f'sibling.{suffix}').write_text(sibling)
    git(root, 'add', '.')
    git(root, 'commit', '-m', 'initial implementations')
    (root / f'original.{suffix}').write_text(original.replace('> 3', '>= 3', 1))
    git(root, 'commit', '-am', 'fix boundary handling')


def scan(root):
    completed = subprocess.run([sys.executable, str(PROBE), str(root)], capture_output=True, text=True, check=True)
    return json.loads(completed.stdout)[0]


def rename(source, replacements):
    return re.sub(r'\b(?:' + '|'.join(re.escape(name) for name in replacements) + r')\b', lambda match: replacements[match[0]], source)


def git(root, *args):
    subprocess.run(['git', '-C', str(root), '-c', 'commit.gpgsign=false', '-c', 'core.hooksPath=/dev/null', *args], capture_output=True, text=True, check=True)


if __name__ == '__main__':
    unittest.main()
