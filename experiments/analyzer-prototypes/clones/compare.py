#!/usr/bin/env python3
"""Compare a fresh clone scan with the pinned baseline and its prior source audit."""
import argparse
import json
from pathlib import Path


def main():
    here = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('current', type=Path)
    parser.add_argument('--baseline', type=Path, default=here / 'raw.json')
    parser.add_argument('--audit', type=Path, default=here / 'audit.json')
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    comparison = compare(json.loads(args.baseline.read_text())['repositories'],
                         json.loads(args.current.read_text())['repositories'],
                         json.loads(args.audit.read_text())['findings'])
    output = json.dumps(comparison, indent=2) + '\n'
    if args.output:
        args.output.write_text(output)
    else:
        print(output, end='')


def compare(baseline, current, audit):
    old_repos = {r['repo']: r for r in baseline}
    new_repos = {r['repo']: r for r in current}
    if old_repos.keys() != new_repos.keys():
        raise ValueError('Repository sets differ')
    scope_keys = ['revision', 'source_files', 'function_bodies', 'history_commits',
                  'fix_labelled_commits', 'changed_functions_compared', 'missing_history_parents',
                  'parse_error_files', 'candidates_omitted_by_per_function_cap']
    for name, old in old_repos.items():
        for key in scope_keys:
            if old[key] != new_repos[name][key]:
                raise ValueError(f'Corpus or search coverage changed: {name}: {key}')
    old_findings = {identity(repo['repo'], f): f for repo in baseline for f in repo['findings']}
    kept = {identity(repo['repo'], f): f for repo in current for f in repo['findings']}
    rejected = {identity(repo['repo'], f): f for repo in current for f in repo['rejected_candidates']}
    if set(kept) & set(rejected):
        raise ValueError('Same candidate both emitted and rejected')
    if set(old_findings) != set(kept) | set(rejected):
        raise ValueError('Broad candidate generation changed; cannot attribute delta only to new gate')
    judgments = {identity(f['repo'], f): f['classification'] for f in audit}
    if old_findings.keys() != judgments.keys():
        raise ValueError('Baseline source-audit set differs from baseline candidate set')
    rows = []
    for key, original in old_findings.items():
        finding = kept.get(key) or rejected[key]
        rows.append(dict(repo=key[0], commit=original['commit'], changed=original['changed'],
            survivor=original['survivor'], prior_judgment=judgments[key],
            outcome='retained' if key in kept else 'suppressed', evidence=finding['structural_comparison']))
    return dict(
        methodology='Same pinned revisions, scope counters and broad candidates; prior source judgments reused. In-sample refinement, not a held-out precision estimate.',
        corpus_unchanged=True, broad_candidates_unchanged=True,
        summary=dict(before=len(old_findings), after=len(kept),
            useful_before=sum(v == 'useful_review' for v in judgments.values()),
            useful_retained=sum(judgments[k] == 'useful_review' for k in kept),
            false_positive_before=sum(v == 'false_positive' for v in judgments.values()),
            false_positive_remaining=sum(judgments[k] == 'false_positive' for k in kept),
            confirmed_runtime_defects=0),
        repositories=[dict(repo=name, revision=old_repos[name]['revision'],
            before=len(old_repos[name]['findings']), after=len(new_repos[name]['findings']),
            rejected=len(new_repos[name]['rejected_candidates'])) for name in sorted(old_repos)],
        findings=rows)


def identity(repo, finding):
    return (repo, finding['commit'], finding['changed']['path'], finding['changed']['symbol'],
            finding['survivor']['path'], finding['survivor']['symbol'])


if __name__ == '__main__':
    main()
