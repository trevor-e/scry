#!/usr/bin/env python3
"""Rerun the four read-only experiments against the pinned corpus checkouts."""
import argparse
import json
from pathlib import Path
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--corpus-root', type=Path, default=Path('/private/tmp/scry-analyzer-corpus'))
    parser.add_argument('--output', type=Path, required=True, help='directory for fresh raw results')
    parser.add_argument('--repos', nargs='+', help='optional subset of corpus names')
    parser.add_argument('--checks', nargs='+', choices=['config', 'clones', 'serde', 'build'], default=['config', 'clones', 'serde', 'build'])
    args = parser.parse_args()
    here = Path(__file__).resolve().parent
    corpus = json.loads((here / 'corpus.json').read_text())
    known = {row['name'] for row in corpus}
    if args.repos and set(args.repos) - known:
        parser.error('unknown repositories: ' + ', '.join(sorted(set(args.repos) - known)))
    selected = [row for row in corpus if not args.repos or row['name'] in args.repos]
    roots = []
    for row in selected:
        root = (args.corpus_root / row['name']).resolve()
        head = subprocess.check_output(['git', '-C', str(root), 'rev-parse', 'HEAD'], text=True).strip()
        if head != row['revision']:
            parser.error(f'{row["name"]}: expected {row["revision"]}, found {head}')
        dirty = subprocess.check_output(['git', '-C', str(root), 'status', '--porcelain', '--untracked-files=no'], text=True)
        if dirty:
            parser.error(f'{row["name"]}: tracked source differs from pinned revision')
        roots.append(str(root))
    args.output.mkdir(parents=True, exist_ok=True)
    scripts = {'config': 'config/prototype.py', 'clones': 'clones/probe.py', 'serde': 'serde/check.py', 'build': 'build/probe.py'}
    for check in args.checks:
        result = subprocess.run([sys.executable, str(here / scripts[check]), *roots], capture_output=True, check=True, text=True)
        data = json.loads(result.stdout)
        if check == 'clones':
            data = {'check': 'missed_clone_fix', 'repositories': data}
        destination = args.output / f'{check}.json'
        destination.write_text(json.dumps(data, indent=2) + '\n')
        print(f'{check}: wrote {destination}', flush=True)
    (args.output / 'corpus.json').write_text(json.dumps(selected, indent=2) + '\n')


if __name__ == '__main__':
    main()
