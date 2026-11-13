"""CLI histories for narrowly matching differently packed TypeScript inputs."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from test_tightened import cases as exact_cases, git, initialize_history, rename


class InputRoleHistories(unittest.TestCase):
    def test_regrouped_inputs_are_a_new_finding_then_disappear_after_propagation(self):
        original, sibling = regrouped_pair()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            initialize_history(root, 'ts', original, sibling)
            strict = scan(root, 'exact')
            self.assertEqual(strict['broad_candidates'], 1)
            self.assertEqual(strict['findings'], [])
            self.assertEqual(survivors(scan(root)), {'sibling.ts'})
            (root / 'sibling.ts').write_text(sibling.replace('> 3', '>= 3', 1))
            git(root, 'commit', '-am', 'propagate boundary handling')
            self.assertEqual(scan(root)['findings'], [])

    def test_regrouping_respects_executable_behavior_and_binding_boundaries(self):
        for name, original, sibling, expected in role_cases():
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                initialize_history(root, 'ts', original, sibling)
                result = scan(root)
                self.assertEqual(result['broad_candidates'], 1, 'Must exercise the matching gate')
                self.assertEqual(survivors(result), {'sibling.ts'} if expected else set())

    def test_existing_binding_and_behavior_contracts_still_hold(self):
        for name, suffix, original, sibling, expected in exact_cases():
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                initialize_history(root, suffix, original, sibling)
                result = scan(root)
                self.assertEqual(result['broad_candidates'], 1)
                self.assertEqual(survivors(result), {f'sibling.{suffix}'} if expected else set())


def role_cases():
    original, sibling = regrouped_pair()
    aliased = sibling.replace('packageName, authHeader, fetchRecord', 'packageName: name, authHeader: credential, fetchRecord')
    aliased = aliased.replace('authHeaderValue: authHeader', 'authHeaderValue: credential').replace('${packageName}', '${name}')
    yield 'flat_property_aliases', original, aliased, True

    yield 'runtime_literal', original, sibling.replace('fullMetadata: true', 'fullMetadata: false'), False
    yield 'member_name', original, sibling.replace('reply.text()', 'reply.bytes()'), False
    yield 'external_constructor', original, sibling.replace('new Error(', 'new RangeError('), False
    yield 'additional_guard', original, sibling.replace('return await reply.json()', 'if (!reply.ok) return null;\n  return await reply.json()'), False
    yield 'input_use_not_bijective', original, sibling.replace('${packageName}', '${authHeader}'), False

    changed_key = sibling.replace('packageName, authHeader, fetchRecord', 'packageName, secret, fetchRecord')
    changed_key = changed_key.replace('authHeaderValue: authHeader', 'authHeaderValue: secret')
    yield 'different_object_property_key', original, changed_key, False

    defaulted = sibling.replace('url: string)', 'url: string = defaultUrl)')
    yield 'parameter_default', original, defaulted, False
    yield 'object_field_default', original, sibling.replace('packageName, authHeader, fetchRecord', 'packageName, authHeader = fallbackHeader, fetchRecord'), False
    yield 'optional_parameter', original, sibling.replace('url: string)', 'url?: string)'), False
    yield 'rest_parameter', original, sibling.replace('url: string)', '...url: string[])'), False
    yield 'object_rest', original, sibling.replace('packageName, authHeader, fetchRecord', 'packageName, authHeader, ...fetchRecord'), False
    yield 'nested_object_pattern', original, sibling.replace('packageName, authHeader, fetchRecord', 'package: { name: packageName }, authHeader, fetchRecord'), False
    yield 'computed_property', original, sibling.replace('packageName, authHeader, fetchRecord', '[nameKey]: packageName, authHeader, fetchRecord'), False
    yield 'generic_function', original, sibling.replace('second(', 'second<T>('), False
    yield 'unused_extra_input', original, sibling.replace('url: string)', 'url: string, unused: string)'), False
    unused_original = original.replace('authHeader, packageName, packageUrl', 'authHeader, packageName, packageUrl, unused')
    unused_sibling = sibling.replace('packageName, authHeader, fetchRecord', 'packageName, authHeader, fetchRecord, unused')
    yield 'same_unused_input_on_both_sides', unused_original, unused_sibling, False
    yield 'mismatched_return_assertion', original, sibling.replace('as StarData', 'as OtherData'), False
    yield 'missing_return_assertion', original, sibling.replace(' as StarData', ''), False
    yield 'primitive_return_assertion', original, sibling.replace('Promise<StarData>', 'Promise<string>').replace('as StarData', 'as string'), False

    annotated = original.replace('const response =', 'const response: RawResponse =')
    annotated_sibling = sibling.replace('const reply =', 'const reply: OtherResponse =')
    yield 'local_type_annotation', annotated, annotated_sibling, False

    inline_original = original.replace('RequestArgs', '{ authHeader: string; packageName: string; packageUrl: string }')
    inline_sibling = sibling.replace('PackedArgs', '{ packageName: string; authHeader: number; fetchRecord: Reader }')
    yield 'inline_object_field_type_is_not_erased', inline_original, inline_sibling, False

    body_asserted = original.replace('const response =', 'const checkedName = packageName as Name;\n  const response =')
    sibling_asserted = sibling.replace('const reply =', 'const checkedName = packageName as OtherName;\n  const reply =')
    yield 'nonreturn_type_assertion', body_asserted, sibling_asserted, False

    original_operator = original.replace('authHeaderValue:', 'formatVersion: 2 + 1,\n    authHeaderValue:')
    sibling_operator = sibling.replace('authHeaderValue:', 'formatVersion: 2 - 1,\n    authHeaderValue:')
    yield 'operator', original_operator, sibling_operator, False

    positional = original.replace('}: RequestArgs)', '}: RequestArgs, limit: number)')
    positional = positional.replace('response.status > 3', 'response.status > 3 && limit > 0')
    packed = sibling.replace('url: string)', 'url: string, limit: number)')
    packed = packed.replace('reply.status > 3', 'reply.status > 3 && limit > 0')
    yield 'remaining_positional_input', positional, packed, True
    yield 'remaining_positional_type_changed', positional, packed.replace('limit: number', 'limit: string'), False
    yield 'remaining_positional_index_changed', positional, packed.replace('url: string, limit: number', 'limit: number, url: string'), False

    no_anchor = rename(sibling, {'fetchRecord': 'readRecord'})
    yield 'no_named_crossing_anchor', original, no_anchor, False

    same_packing = rename(original, {'first': 'second', 'response': 'reply'})
    same_packing = same_packing.replace('RequestArgs', 'OtherArgs').replace('RecordData', 'StarData')
    yield 'no_actual_input_regrouping', original, same_packing, False

    nested = original.replace('const response =', 'function nested() { return 1; }\n  const response =')
    nested_sibling = sibling.replace('const reply =', 'function nested() { return 1; }\n  const reply =')
    yield 'unsupported_nested_declaration', nested, nested_sibling, False

    arguments_original = original.replace('const response =', 'const firstArgument = arguments[0];\n  const response =')
    arguments_sibling = sibling.replace('const reply =', 'const firstArgument = arguments[0];\n  const reply =')
    yield 'arguments_depends_on_parameter_packing', arguments_original, arguments_sibling, False

    shorthand_original = original.replace('const response =', 'const captured = { arguments };\n  const response =')
    shorthand_sibling = sibling.replace('const reply =', 'const captured = { arguments };\n  const reply =')
    yield 'shorthand_arguments_depends_on_parameter_packing', shorthand_original, shorthand_sibling, False

    escaped_read = r'const firstArgument = argum\u0065nts[0];' + '\n  '
    escaped_original = original.replace('const response =', escaped_read + 'const response =')
    escaped_sibling = sibling.replace('const reply =', escaped_read + 'const reply =')
    yield 'escaped_arguments_depends_on_parameter_packing', escaped_original, escaped_sibling, False


def regrouped_pair():
    original = '''async function first(fetchRecord: Reader, { authHeader, packageName, packageUrl }: RequestArgs): Promise<RecordData> {
  const response = await fetchRecord(packageUrl, {
    authHeaderValue: authHeader,
    fullMetadata: true,
  });
  if (response.status > 3) {
    const message = await response.text();
    throw new Error(`Failed ${packageName}: ${message}`);
  }
  return await response.json() as RecordData;
}
'''
    sibling = '''async function second({ packageName, authHeader, fetchRecord }: PackedArgs, url: string): Promise<StarData> {
  const reply = await fetchRecord(url, {
    authHeaderValue: authHeader,
    fullMetadata: true,
  });
  if (reply.status > 3) {
    const detail = await reply.text();
    throw new Error(`Failed ${packageName}: ${detail}`);
  }
  return await reply.json() as StarData;
}
'''
    return original, sibling


def scan(root, matching='roles'):
    probe = Path(__file__).with_name('probe.py')
    completed = subprocess.run([sys.executable, str(probe), str(root), '--matching', matching],
                               capture_output=True, text=True, check=True)
    return json.loads(completed.stdout)[0]


def survivors(result):
    return {finding['survivor']['path'] for finding in result['findings']}


if __name__ == '__main__':
    unittest.main()
