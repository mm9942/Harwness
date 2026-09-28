#!/usr/bin/env python3
"""Build the follow-up gap-fix wave <wave>-2 for a finished old-logic wave.
usage: build_followup.py <wave-key> <wave-script.js>
Reads reconcile-<wave-key>.json (needs_review, ripple). Every file that needs
review gets its original findings as verify-only items; every ripple item with
a real repo path becomes a new finding for its file. Files with verify-only
items and nothing else run reviewOnly. Refuses files that another wave script
owns. root is the main tree."""
import json, os, re, sys, glob
REPO = os.environ.get('R16_REPO', os.getcwd())
SP = os.environ['R16_SCRATCH'].rstrip('/') + '/'
key, script = sys.argv[1], sys.argv[2]
W = json.loads(re.search(r'const A = (\{.*?\})\n', open(script).read()).group(1))
rec = json.load(open(SP + f'reconcile-{key}.json'))
norm = lambda p: str(p).split('Harwness/')[-1]
rip = [i for i in (rec.get('ripple') or []) if i.get('file') and '/' in str(i['file']) and '.worktrees/' not in str(i['file'])]
own = {}
for p in glob.glob(SP + 'fix-r16-w?.js') + glob.glob(SP + 'contract-c-2.js') + glob.glob(SP + 'ripple-web.js'):
    A = json.loads(re.search(r'const A = (\{.*?\})\n', open(p).read()).group(1))
    fs = {f['file'] for f in A.get('findings', [])}
    for c in A.get('clusters', []): fs |= set(c['files'])
    if A['key'] != key: own[A['key']] = fs
findings = []
def verify(f):
    return {**f, 'title': f'[verify only, already fixed by wave {key}] ' + f['title'],
            'fix': f'Already applied in the working tree by wave {key} (its repair was never re-reviewed, or its fix/review ran in the wrong checkout). Verify the applied fix against this finding; correct it only if it is wrong or incomplete; do not redo it. Original fix: ' + f['fix']}
ripfiles = {norm(i['file']) for i in rip}
need = set(rec['needs_review'])
for f in W['findings']:
    if f['file'] in need or f['file'] in ripfiles: findings.append(verify(f))
for i in rip:
    sev = 'high' if i['problem'].lstrip().upper().startswith('HIGH') or 'Test failure' in i['problem'] else 'low'
    findings.append({'file': norm(i['file']), 'line': i.get('line') or 1, 'severity': sev, 'pattern': 'P1', 'category': f'ripple after {key}',
                     'title': i['problem'][:140], 'description': i['problem'], 'evidence': f'Cross-file ripple check of wave {key}.', 'fix': i.get('fix', ''), 'test_idea': ''})
clash = {f['file']: [k for k, v in own.items() if f['file'] in v] for f in findings}
clash = {k: v for k, v in clash.items() if v}
if clash: sys.exit(f'files owned by other waves: {clash}')
review_only = sorted(f for f in need if all(x['title'].startswith('[verify only') for x in findings if x['file'] == f))
A = {'key': f'{key}-2', 'root': REPO, 'findings': findings, 'reviewOnly': review_only}
src = open(REPO + '/docs/planning/85-gap-hunt/kit/workflows/gap-fix.js').read()
assert 'checkRoot' in src and "rippleStatus === 'clear'" in src
open(SP + f'fix-{key}-2.js', 'w').write(src.replace('const A = args || {}', 'const A = ' + json.dumps(A, ensure_ascii=False)))
print(f'{len(findings)} findings on {len({f["file"] for f in findings})} files; reviewOnly {review_only}')
