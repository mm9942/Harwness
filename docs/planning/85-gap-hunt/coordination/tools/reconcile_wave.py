#!/usr/bin/env python3
"""Reconcile one finished one-file wave after misplaced writes (catalog P14).

usage: reconcile_wave.py <wave-script.js> <run-dir> [--apply]
- files: the wave's own files (from the embedded findings)
- for each file, look for modified copies in the stray worktrees
  (contract-a, kit-3); main clean -> move; identical -> drop; both differ ->
  keep main, back up the stray copy, flag for review
- flags for re-review: every file whose review/repair result mentions a
  worktree path, every file the old logic reported as repaired without a
  re-review, and every moved or conflicting file
Writes <scratchpad>/reconcile-<wave>.json.
"""
import json, os, re, subprocess, sys, shutil, filecmp
REPO = os.environ.get('R16_REPO', os.getcwd())
SP = os.environ['R16_SCRATCH']
STRAY = ['contract-a', 'kit-3']
WF = os.environ['R16_SUBAGENTS']
script, run = sys.argv[1], sys.argv[2]
apply = '--apply' in sys.argv
src = open(script).read()
A = json.loads(re.search(r'const A = (\{.*?\})\n', src).group(1))
files = sorted({f['file'] for f in A['findings']})
def git(*a, cwd=REPO):
    return subprocess.run(['git', '-C', cwd, *a], capture_output=True, text=True)
def dirty(cwd, f):
    return git('diff', '--quiet', '--', f, cwd=cwd).returncode != 0
# journal: results per label
labs, res = {}, {}
for l in open(f'{WF}/{run}/journal.jsonl'):
    o = json.loads(l)
    if o.get('type') == 'started': labs[o['key']] = o['label']
    if o.get('type') == 'result': res.setdefault(labs.get(o['key'], ''), []).append(o['result'])
out = {'wave': A['key'], 'run': run, 'files': {}}
bk = f"{SP}/misplaced-backup/{A['key']}"
for f in files:
    e = {'actions': [], 'review': []}
    for w in STRAY:
        wt = f'{REPO}/.worktrees/{w}'
        if not os.path.isdir(wt) or not dirty(wt, f):
            continue
        if git('rev-parse', f'HEAD:{f}').stdout != git('rev-parse', f'HEAD:{f}', cwd=wt).stdout and not dirty(REPO, f):
            e['actions'].append(f'{w}: base differs, left alone'); e['review'].append('base-differs'); continue
        os.makedirs(os.path.dirname(f'{bk}/{w}/{f}'), exist_ok=True)
        if apply: shutil.copy2(f'{wt}/{f}', f'{bk}/{w}/{f}')
        if not dirty(REPO, f):
            e['actions'].append(f'{w}: moved to main'); e['review'].append('moved')
            if apply: shutil.copy2(f'{wt}/{f}', f'{REPO}/{f}')
        elif filecmp.cmp(f'{wt}/{f}', f'{REPO}/{f}', shallow=False):
            e['actions'].append(f'{w}: identical, dropped')
        else:
            e['actions'].append(f'{w}: differs from main, main kept, stray copy backed up'); e['review'].append('conflict')
        if apply: git('checkout', '--', f, cwd=wt)
    for lab, rs in res.items():
        if lab.split(':', 1)[-1] != f: continue
        txt = json.dumps(rs)
        if '.worktrees/' in txt: e['review'].append(f'{lab.split(":")[0]} ran in a worktree')
    fr = [x for x in (json.loads(open(o).read()) if False else []) ]
    out['files'][f] = e
# old-logic status per file from the task output, if given via --result
for a in sys.argv[3:]:
    if a.startswith('--result='):
        r = json.load(open(a.split('=', 1)[1]))['result']
        for x in r.get('files', []):
            if x.get('repaired') or x.get('status') == 'repaired':
                out['files'].setdefault(x['file'], {'actions': [], 'review': []})['review'].append('repaired without re-review')
            if x.get('ok') is False and not x.get('repaired'):
                out['files'].setdefault(x['file'], {'actions': [], 'review': []})['review'].append('not ok')
        out['ripple'] = r.get('ripple')
out['needs_review'] = sorted(f for f, e in out['files'].items() if e['review'])
json.dump(out, open(f"{SP}/reconcile-{A['key']}.json", 'w'), indent=1)
for f, e in out['files'].items():
    if e['actions'] or e['review']: print(f, e['actions'], sorted(set(e['review'])))
print('needs review:', len(out['needs_review']), 'of', len(files))
