#!/usr/bin/env python3
"""Run the frozen matcher, then select the predeclared rejection audit sample."""
import argparse
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor, as_completed
import hashlib
import importlib.metadata
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
    protocol = read(here / 'protocol.json')
    corpus = read(here / 'corpus.json')
    before = verify_detector(here.parent, protocol)
    versions = {name: importlib.metadata.version(name) for name in protocol['dependencies']}
    if versions != protocol['dependencies']:
        raise ValueError('Parser versions differ from frozen protocol')
    expected = {r['name']: r for r in protocol['repositories']}
    if len(corpus) != len(expected) or {r['name'] for r in corpus} != expected.keys():
        raise ValueError('Corpus differs from preselected set')
    for repo in corpus:
        if any(repo[k] != value for k, value in expected[repo['name']].items()):
            raise ValueError('Repository selection changed: ' + repo['name'])
        if git(repo['path'], 'rev-parse', 'HEAD') != repo['revision']:
            raise ValueError('Pinned revision changed: ' + repo['name'])
        if git(repo['path'], 'remote', 'get-url', 'origin') != repo['origin']:
            raise ValueError('Repository origin changed: ' + repo['name'])
        if git(repo['path'], 'status', '--porcelain', '--untracked-files=no'):
            raise ValueError('Tracked worktree is dirty: ' + repo['name'])
    reports = []
    with ThreadPoolExecutor(max_workers=max(1, args.jobs)) as pool:
        futures = [pool.submit(scan, here.parent, protocol, repo, args.output) for repo in corpus]
        for future in as_completed(futures):
            report, command = future.result()
            reports.append((report, command))
            print(json.dumps(summary(report)), flush=True)
    after = verify_detector(here.parent, protocol)
    reports.sort(key=lambda row: row[0]['repo'])
    write(args.output / 'sample.json', select_rejections([r for r, _ in reports], protocol))
    write(args.output / 'run.json', dict(
        detector_sha256_before=before, detector_sha256_after=after,
        protocol_sha256=digest((here / 'protocol.json').read_bytes()),
        runner_sha256=digest(Path(__file__).read_bytes()), parameters=protocol['parameters'],
        dependencies=versions, python=sys.version, commands=[c for _, c in reports],
        reports=[summary(r) for r, _ in reports]))


def scan(detector, protocol, repo, output):
    options = protocol['parameters']
    command = [sys.executable, str(detector / 'probe.py'), repo['path'],
               '--matching', options['matching'], '--commits', str(options['commits']),
               '--similarity', str(options['similarity'])]
    completed = subprocess.run(command, capture_output=True, text=True, check=True)
    data = json.loads(completed.stdout)
    if len(data) != 1 or data[0]['revision'] != repo['revision']:
        raise ValueError('Unexpected scan: ' + repo['name'])
    write(output / (repo['name'] + '.json'), data[0])
    return data[0], command


def select_rejections(reports, protocol):
    policy = protocol['audit']['rejected_sample']
    groups = {'Rust': defaultdict(list), 'TypeScript': defaultdict(list)}
    for report in reports:
        for index, finding in enumerate(report['rejected_candidates']):
            suffix = Path(finding['changed']['path']).suffix
            if suffix not in {'.rs', '.ts', '.tsx'}:
                raise ValueError('Unexpected candidate language')
            language = 'Rust' if suffix == '.rs' else 'TypeScript'
            key = identity(report['repo'], finding)
            encoded = json.dumps(key, ensure_ascii=True, separators=(',', ':'))
            rank = digest((policy['seed'] + '\n' + encoded).encode())
            groups[language][report['repo']].append(dict(
                identity=key, repo=report['repo'], rejected_index=index, language=language, rank_hash=rank))
    orders = {}
    for language, repositories in groups.items():
        ranked_repos = sorted(repositories, key=lambda name: (
            digest((policy['seed'] + '\nrepo:' + name).encode()), name))
        for values in repositories.values():
            values.sort(key=lambda row: (row['rank_hash'], row['identity']))
        order = []
        for round_index in range(max((len(values) for values in repositories.values()), default=0)):
            for name in ranked_repos:
                if round_index < len(repositories[name]):
                    row = dict(repositories[name][round_index], language_order=len(order))
                    order.append(row)
        orders[language] = order
    quota = policy['per_language_initial']
    selected = [row for order in orders.values() for row in order[:quota]]
    target = min(policy['maximum'], sum(len(order) for order in orders.values()))
    remainder = [row for order in orders.values() for row in order[quota:]]
    selected.extend(remainder[:target - len(selected)])
    return dict(policy=policy, population={language: len(order) for language, order in orders.items()},
        selected_counts={language: sum(row['language'] == language for row in selected) for language in orders},
        selected=selected, complete_language_orders=orders)


def summary(report):
    return dict(repo=report['repo'], revision=report['revision'], findings=len(report['findings']),
        exact_acceptances=sum(f['structural_comparison']['reason'] == 'same_complete_pre_fix_structure'
                              for f in report['findings']),
        role_acceptances=sum(f['structural_comparison']['reason'] == 'same_complete_body_after_input_regrouping'
                             for f in report['findings']),
        rejected=len(report['rejected_candidates']), source_files=report['source_files'],
        history_commits=report['history_commits'])


def identity(repo, finding):
    return [repo, finding['commit'], finding['changed']['path'], finding['changed']['symbol'],
            finding['survivor']['path'], finding['survivor']['symbol']]


def verify_detector(root, protocol):
    hashes = {name: digest((root / name).read_bytes()) for name in protocol['detector_sha256']}
    if hashes != protocol['detector_sha256']:
        raise ValueError('Detector changed from frozen version')
    return hashes


def digest(data):
    return hashlib.sha256(data).hexdigest()


def read(path):
    return json.loads(path.read_text())


def write(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2) + '\n')


def git(root, *args):
    return subprocess.check_output(['git', '-C', root, *args], text=True).strip()


if __name__ == '__main__':
    main()
