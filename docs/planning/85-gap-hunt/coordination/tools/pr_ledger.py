#!/usr/bin/env python3
"""Render the PR #37 wave ledger from the wave manifests on the integration head."""
import json, os, subprocess, sys
REPO = os.environ.get('R16_REPO', os.getcwd())
def git(*a): return subprocess.run(['git', '-C', REPO, *a], capture_output=True, text=True).stdout
head = git('rev-parse', 'origin/claude/r16-integration').strip()
names = [l.split('/')[-1][:-5] for l in git('ls-tree', '--name-only', head, 'docs/planning/85-gap-hunt/waves/').split() if l.endswith('.json')]
FOLLOWUP = {'wa-egress': 'ripple-egress', 'wa-authz': 'ripple-authz', 'wa-web': 'ripple-web'}
merges, order = {}, []
for line in reversed(git('log', '--first-parent', '--format=%H %s', f'origin/dev..{head}').splitlines()):
    sha, subj = line.split(' ', 1)
    if subj.startswith('Merge claude/r16/'):
        w = subj.split()[1].split('/')[-1]; merges[w] = sha[:7]; order.append(w)
rows = []
for n in sorted(names, key=lambda n: order.index(n) if n in order else 999):
    m = json.loads(git('show', f'{head}:docs/planning/85-gap-hunt/waves/{n}.json'))
    d = m.get('disposition') or {}
    if 'clusters' in d:
        st = ', '.join(f"{c['id']} {c['status']}" for c in d['clusters'])
    else:
        files = d.get('files') or []
        ok = sum(1 for f in files if f.get('status') in ('ok', 'repaired') or f.get('ok') or f.get('repaired'))
        st = f"{ok}/{len(files)} files ok" if files else '-'
    rip = d.get('rippleStatus')
    if n in FOLLOWUP: rip = f"findings → `{FOLLOWUP[n]}`"
    elif rip is None: rip = 'n/a (contract review)' if 'clusters' in d else '-'
    rows.append(f"| `{n}` | {merges.get(n, '?')} | `{m['head_sha'][:7]}` | {len(m['files'])} | {len(m['findings'])} | {st} | {rip} | not built |")
print(f"Integration head: `{head[:7]}` (the central build runs once over the frozen final head)\n")
print('| Wave | Merge | Content | Files | Findings | Review | Ripple | Tests |')
print('|---|---|---|---:|---:|---|---|---|')
print('\n'.join(rows))
