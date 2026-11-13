#!/usr/bin/env python3
"""Run the frozen detector against pinned fresh checkouts; never alter its rules."""
import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import hashlib
import json
from pathlib import Path
import subprocess
import sys


def main():
    here = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, default=here / 'results')
    parser.add_argument('--jobs', type=int, default=3)
    args = parser.parse_args()
    protocol = json.loads((here / 'protocol.json').read_text())
    corpus = json.loads((here / 'corpus.json').read_text())
    before = verify_detector(here.parent, protocol)
    if {r['name'] for r in corpus} != {r['name'] for r in protocol['repositories']}:
        raise ValueError('Corpus differs from preselected set')
    for repo in corpus:
        if git(repo['path'], 'rev-parse', 'HEAD') != repo['revision']:
            raise ValueError('Revision differs: ' + repo['name'])
        if git(repo['path'], 'status', '--porcelain', '--untracked-files=no'):
            raise ValueError('Tracked worktree is dirty: ' + repo['name'])
    args.output.mkdir(parents=True, exist_ok=True)
    with ThreadPoolExecutor(max_workers=max(1, args.jobs)) as pool:
        futures = [pool.submit(run_repo, here.parent, protocol, repo, args.output) for repo in corpus]
        reports = [future.result() for future in as_completed(futures)]
    after = verify_detector(here.parent, protocol)
    reports.sort(key=lambda r: r['repo'])
    metadata = dict(detector_sha256_before=before, detector_sha256_after=after,
                    parameters=protocol['parameters'], python=sys.version,
                    commands=[r['command'] for r in reports],
                    reports=[{k: v for k, v in r.items() if k != 'command'} for r in reports])
    (args.output / 'run.json').write_text(json.dumps(metadata, indent=2) + '\n')


def run_repo(detector, protocol, repo, output):
    options = protocol['parameters']
    command = [sys.executable, str(detector / 'probe.py'), repo['path'],
               '--matching', options['matching'], '--commits', str(options['commits']),
               '--similarity', str(options['similarity'])]
    completed = subprocess.run(command, capture_output=True, text=True, check=True)
    result = json.loads(completed.stdout)
    if len(result) != 1 or result[0]['revision'] != repo['revision']:
        raise ValueError('Unexpected scan result: ' + repo['name'])
    row = result[0]
    destination = output / (repo['name'] + '.json')
    destination.write_text(json.dumps(row, indent=2) + '\n')
    summary = dict(repo=repo['name'], revision=row['revision'],
                   findings=len(row['findings']), rejected=len(row['rejected_candidates']),
                   source_files=row['source_files'], history_commits=row['history_commits'],
                   command=command)
    print(json.dumps({k: v for k, v in summary.items() if k != 'command'}), flush=True)
    return summary


def verify_detector(root, protocol):
    hashes = {name: hashlib.sha256((root / name).read_bytes()).hexdigest()
              for name in protocol['detector_sha256']}
    if hashes != protocol['detector_sha256']:
        raise ValueError('Detector changed from frozen version')
    return hashes


def git(root, *args):
    return subprocess.check_output(['git', '-C', root, *args], text=True).strip()


if __name__ == '__main__':
    main()
