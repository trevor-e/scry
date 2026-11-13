#!/usr/bin/env python3
"""Rerun both pinned corpora and compare every candidate with the strict matcher."""
import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import hashlib
import importlib.metadata
import json
from pathlib import Path
import subprocess
import sys


def main():
    here = Path(__file__).resolve().parent
    detector = here.parent
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument('--output', type=Path, default=here / 'results')
    cli.add_argument('--jobs', type=int, default=3)
    args = cli.parse_args()
    manifests = {'original': read(detector.parent / 'corpus.json'),
                 'holdout': read(detector / 'holdout/corpus.json')}
    baselines = {'original': read(detector / 'tightened.json')['repositories'],
                 'holdout': [read(detector / 'holdout/results' / (r['name'] + '.json'))
                             for r in manifests['holdout']]}
    before = source_hashes(detector)
    for corpus in manifests.values():
        for repo in corpus:
            if git(repo['path'], 'rev-parse', 'HEAD') != repo['revision']:
                raise ValueError('Revision changed: ' + repo['name'])
            if git(repo['path'], 'status', '--porcelain', '--untracked-files=no'):
                raise ValueError('Tracked source changed: ' + repo['name'])
    reports = []
    with ThreadPoolExecutor(max_workers=max(1, args.jobs)) as pool:
        futures = [pool.submit(scan, detector, group, repo, args.output)
                   for group, corpus in manifests.items() for repo in corpus]
        for future in as_completed(futures):
            group, result, command = future.result()
            reports.append((group, result, command))
            print(json.dumps(dict(corpus=group, repo=result['repo'],
                                  findings=len(result['findings']))), flush=True)
    after = source_hashes(detector)
    if before != after:
        raise ValueError('Detector changed during the run')
    judgments = prior_judgments(detector, baselines)
    comparison = [compare(group, baseline,
                          [r for g, r, _ in reports if g == group], judgments)
                  for group, baseline in baselines.items()]
    write(args.output / 'comparison.json', dict(
        methodology='Both corpora now inform matcher development; this is an in-sample comparison, not a fresh holdout.',
        corpora=comparison))
    write(args.output / 'run.json', dict(
        parameters=dict(matching='roles', commits=200, similarity=0.85),
        source_sha256_before=before, source_sha256_after=after,
        python=sys.version, dependencies={name: importlib.metadata.version(name)
            for name in ('tree-sitter', 'tree-sitter-rust', 'tree-sitter-typescript')},
        manifests=manifests, commands=[command for _, _, command in sorted(reports, key=lambda x: (x[0], x[1]['repo']))]))


def scan(detector, group, repo, output):
    command = [sys.executable, str(detector / 'probe.py'), repo['path'],
               '--matching', 'roles', '--commits', '200', '--similarity', '0.85']
    completed = subprocess.run(command, capture_output=True, text=True, check=True)
    data = json.loads(completed.stdout)
    if len(data) != 1 or data[0]['revision'] != repo['revision']:
        raise ValueError('Unexpected scan: ' + repo['name'])
    write(output / group / (repo['name'] + '.json'), data[0])
    return group, data[0], command


def compare(group, baseline, current, judgments):
    old = {r['repo']: r for r in baseline}
    new = {r['repo']: r for r in current}
    if old.keys() != new.keys():
        raise ValueError('Repository sets changed')
    scope = ('revision', 'source_files', 'function_bodies', 'history_commits',
             'fix_labelled_commits', 'changed_functions_compared', 'missing_history_parents',
             'parse_error_files', 'candidates_omitted_by_per_function_cap', 'broad_candidates')
    rows, repositories = [], []
    for name, previous in old.items():
        result = new[name]
        for key in scope:
            if previous[key] != result[key]:
                raise ValueError(f'Coverage changed: {name}: {key}')
        old_pairs = pairs(previous)
        new_pairs = pairs(result)
        if old_pairs.keys() != new_pairs.keys():
            raise ValueError('Broad candidate identities changed: ' + name)
        old_kept = {identity(name, f) for f in previous['findings']}
        new_kept = {identity(name, f) for f in result['findings']}
        for key, finding in old_pairs.items():
            unchanged = lambda f: {k: v for k, v in f.items() if k != 'structural_comparison'}
            if unchanged(finding) != unchanged(new_pairs[key]):
                raise ValueError('Broad candidate evidence changed: ' + str(key))
            rows.append(dict(repo=name, commit=finding['commit'], changed=finding['changed'],
                survivor=finding['survivor'], prior_judgment=judgments.get(key, 'not_audited'),
                emitted_before=key in old_kept, emitted_after=key in new_kept,
                structural_comparison=new_pairs[key]['structural_comparison']))
        repositories.append(dict(repo=name, revision=previous['revision'],
            before=len(old_kept), after=len(new_kept), rejected=len(result['rejected_candidates'])))
    return dict(corpus=group, scope_unchanged=True, broad_candidates_and_evidence_unchanged=True,
        before=sum(r['before'] for r in repositories), after=sum(r['after'] for r in repositories),
        useful_emitted=sum(r['emitted_after'] and r['prior_judgment'] == 'useful_review' for r in rows),
        known_false_positives_emitted=sum(r['emitted_after'] and r['prior_judgment'] == 'false_positive' for r in rows),
        unaudited_emitted=sum(r['emitted_after'] and r['prior_judgment'] == 'not_audited' for r in rows),
        newly_emitted=[r for r in rows if r['emitted_after'] and not r['emitted_before']],
        newly_suppressed=[r for r in rows if r['emitted_before'] and not r['emitted_after']],
        repositories=repositories, candidates=rows)


def prior_judgments(detector, baselines):
    judgments = {identity(f['repo'], f): f['classification']
                 for f in read(detector / 'audit.json')['findings']}
    holdout = {r['repo']: r for r in baselines['holdout']}
    playwright = read(detector / 'holdout/audit-playwright.json')
    finding = holdout['playwright']['findings'][0]
    if finding['survivor'] != playwright['finding']:
        raise ValueError('Playwright audit identity changed')
    judgments[identity('playwright', finding)] = playwright['classification']
    judgments[identity('vitest', holdout['vitest']['rejected_candidates'][0])] = 'false_positive'
    for case in read(detector / 'holdout/audit-pnpm-sample.json')['cases']:
        finding = holdout['pnpm']['rejected_candidates'][case['rejected_index']]
        if finding['commit'] != case['commit'] or finding['survivor']['symbol'] != case['survivor_symbol']:
            raise ValueError('pnpm audit identity changed')
        judgments[identity('pnpm', finding)] = {'false_positive_avoided': 'false_positive',
            'useful_review_suppressed': 'useful_review'}[case['judgment']]
    for case in read(detector / 'holdout/audit-pnpm-sql.json')['candidates']:
        finding = holdout['pnpm']['rejected_candidates'][case['rejected_index']]
        if finding['survivor']['path'] != case['survivor']['path']:
            raise ValueError('SQL audit identity changed')
        judgments[identity('pnpm', finding)] = 'false_positive'
    return judgments


def pairs(repo):
    all_pairs = repo['findings'] + repo['rejected_candidates']
    result = {identity(repo['repo'], f): f for f in all_pairs}
    if len(result) != len(all_pairs):
        raise ValueError('Duplicate candidate identity')
    return result


def identity(repo, finding):
    return (repo, finding['commit'], finding['changed']['path'], finding['changed']['symbol'],
            finding['survivor']['path'], finding['survivor']['symbol'])


def source_hashes(detector):
    return {name: hashlib.sha256((detector / name).read_bytes()).hexdigest()
            for name in ('probe.py', 'bindings.py', 'input_roles.py')}


def read(path):
    return json.loads(path.read_text())


def write(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2) + '\n')


def git(root, *args):
    return subprocess.check_output(['git', '-C', root, *args], text=True).strip()


if __name__ == '__main__':
    main()
