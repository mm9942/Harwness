#!/usr/bin/env python3
"""Fail-closed release metadata and exact-commit GitHub CI evidence checks."""
import argparse
import json
import os
from pathlib import Path
import re
import tomllib
import urllib.request


REQUIRED_JOBS = {
    'detect changes': (),
    'fmt, clippy, test (root workspace, x64)': (
        'cargo fmt --check', 'Offline installer tests', 'cargo clippy',
        'xtask gates', 'cargo nextest run', 'cargo test --doc'),
    'fmt, clippy, test (root workspace, arm64)': (
        'cargo fmt --check', 'Offline installer tests', 'cargo clippy',
        'xtask gates', 'cargo nextest run', 'cargo test --doc'),
    'test (dod domain, package-scoped)': (
        'Build DoD binaries (package-scoped)', 'cargo test (dod crates, -p selection)'),
    'cargo deny check': ('cargo deny check',),
    'actionlint': ('actionlint',),
    'test (macOS, Darwin job backend)': (
        'cargo test (job core + Darwin backend)',
        'cargo test (command port + job runtime on Darwin)',
        'cargo check (Linux backend compiles empty on macOS)'),
    'native agent build (harw agent build --native)': ('Native backend end to end',),
}


def validate_ci(run, jobs, sha, repository):
    expected = dict(head_sha=sha, path='.github/workflows/ci.yml',
                    event='workflow_dispatch', status='completed', conclusion='success')
    for key, value in expected.items():
        if run.get(key) != value:
            raise ValueError(f'CI {key}: expected {value!r}, got {run.get(key)!r}')
    if run.get('repository', {}).get('full_name') != repository:
        raise ValueError('CI belongs to another repository')
    for name, required_steps in REQUIRED_JOBS.items():
        matching = [job for job in jobs if job.get('name') == name]
        if len(matching) != 1:
            raise ValueError(f'CI requires exactly one job: {name}')
        job = matching[0]
        if job.get('status') != 'completed' or job.get('conclusion') != 'success':
            raise ValueError(f'CI job did not execute successfully: {name}')
        for step in required_steps:
            matches = [s for s in job.get('steps', []) if s.get('name') == step]
            if len(matches) != 1 or matches[0].get('status') != 'completed' or matches[0].get('conclusion') != 'success':
                raise ValueError(f'CI step missing, skipped or unsuccessful: {name}: {step}')


def validate_metadata(tag, root, publishing=False):
    if not re.fullmatch(r'v(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)', tag):
        raise ValueError('Only stable vX.Y.Z tags are supported')
    workspace = tomllib.loads((root / 'Cargo.toml').read_text())['workspace']
    version = workspace['package']['version']
    if tag != f'v{version}':
        raise ValueError('Release tag does not match workspace version')
    lock = tomllib.loads((root / 'Cargo.lock').read_text())
    local = {p['name']: p['version'] for p in lock['package'] if 'source' not in p}
    for member in workspace['members']:
        manifest = tomllib.loads((root / member / 'Cargo.toml').read_text())
        package = manifest['package']
        member_version = package['version']
        if member_version == {'workspace': True}:
            member_version = version
        if member_version != version or local.get(package['name']) != version:
            raise ValueError(f'Workspace/lockfile version mismatch: {member}')
        sections = [manifest] + list(manifest.get('target', {}).values())
        for section in sections:
            for kind in ('dependencies', 'dev-dependencies', 'build-dependencies'):
                for name, dep in section.get(kind, {}).items():
                    if isinstance(dep, dict) and 'path' in dep and 'version' in dep:
                        if dep['version'] not in (version, f'={version}'):
                            raise ValueError(f'Internal version pin mismatch: {member}/{name}')
    changelog = (root / 'CHANGELOG.md').read_text()
    header = re.search(rf'^## \[{re.escape(version)}\] — (.+)$', changelog, re.M)
    if not header or (publishing and not re.fullmatch(r'\d{4}-\d{2}-\d{2}', header[1])):
        raise ValueError('Publication requires a dated changelog entry; Upcoming is not released')
    print(f'Metadata consistent: {len(workspace["members"])} packages, {tag}')


def github(path):
    request = urllib.request.Request('https://api.github.com/' + path, headers={
        'Authorization': 'Bearer ' + os.environ['GH_TOKEN'],
        'Accept': 'application/vnd.github+json', 'X-GitHub-Api-Version': '2022-11-28',
    })
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('tag')
    parser.add_argument('--publish', action='store_true')
    parser.add_argument('--ci-run-id')
    parser.add_argument('--sha')
    args = parser.parse_args()
    validate_metadata(args.tag, Path('.'), args.publish)
    if args.publish:
        if not args.ci_run_id or not args.ci_run_id.isdecimal() or not re.fullmatch(r'[0-9a-f]{40}', args.sha or ''):
            raise ValueError('Publication requires --ci-run-id and the exact --sha')
        repo = os.environ['GITHUB_REPOSITORY']
        path = f'repos/{repo}/actions/runs/{args.ci_run_id}'
        run = github(path)
        jobs = []
        page = 1
        while True:
            batch = github(f'{path}/jobs?filter=latest&per_page=100&page={page}')['jobs']
            jobs.extend(batch)
            if len(batch) < 100:
                break
            page += 1
        validate_ci(run, jobs, args.sha, repo)
        print(f'Exact-commit CI evidence accepted: {run["html_url"]} ({args.sha})')


if __name__ == '__main__':
    try:
        main()
    except (ValueError, KeyError, OSError) as error:
        raise SystemExit(f'release-gate: {error}') from error
