"""CLI fixtures: historic edit, surviving sibling, and propagated update."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

PROBE = Path(__file__).with_name('probe.py')
TS_BODY = '''export function NAME(items: number[]): number[] {
  const output: number[] = [];
  for (const item of items) {
    const adjusted = item * 2 + 1;
    if (adjusted > 3) {
      output.push(adjusted);
    } else {
      output.push(0);
    }
  }
  return output;
}
'''
RS_BODY = '''pub fn NAME(items: &[i32]) -> Vec<i32> {
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


class HistoryFixtures(unittest.TestCase):
    def test_rust_and_typescript_retained_then_propagated_fix(self):
        for suffix, body in [('ts', TS_BODY), ('rs', RS_BODY)]:
            with self.subTest(suffix=suffix), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                def git(*args):
                    subprocess.run(['git', '-C', directory, *args], check=True, capture_output=True)
                git('init')
                git('config', 'user.email', 'fixture@example.invalid')
                git('config', 'user.name', 'Fixture')
                one, two = root / f'one.{suffix}', root / f'two.{suffix}'
                one.write_text(body.replace('NAME', 'one'))
                two.write_text(body.replace('NAME', 'two'))
                (root / 'tests').mkdir()
                (root / 'tests' / f'fixture.{suffix}').write_text(body.replace('NAME', 'fixture'))
                git('add', '.')
                git('commit', '-m', 'initial implementations')
                one.write_text(one.read_text().replace('> 3', '>= 3'))
                git('commit', '-am', 'fix boundary handling')
                result = json.loads(subprocess.check_output([sys.executable, str(PROBE), directory]))[0]
                self.assertTrue(result['findings'])
                self.assertEqual({x['survivor']['path'] for x in result['findings']}, {f'two.{suffix}'})
                two.write_text(two.read_text().replace('> 3', '>= 3'))
                git('commit', '-am', 'propagate boundary handling')
                result = json.loads(subprocess.check_output([sys.executable, str(PROBE), directory]))[0]
                self.assertEqual(result['findings'], [])


if __name__ == '__main__':
    unittest.main()
