#!/usr/bin/env python3
"""Counts completed verifier agents across workflow journals (gap-hunt kit).

Usage: verify_counter.py [--mark]   (--mark stores the current total as "last commit")
Prints: total, since_last_commit, busy writers (fix/repair agents started but not finished).
Commit rule (Mia): commit at the end of each workflow, or when since_last_commit >= 90
and no fixer/repair is writing.
"""
import glob, json, os, sys
BASE = os.environ.get('GAP_HUNT_WORKFLOWS_DIR', os.path.expanduser('~/.claude/projects'))  # searched recursively
STATE = os.environ.get('GAP_HUNT_COUNTER_STATE', '.gap-hunt-verify-counter.state')
total = 0
writers = []
for j in glob.glob(BASE + '/**/workflows/*/journal.jsonl', recursive=True):
    keys, done = {}, set()
    for line in open(j):
        try:
            e = json.loads(line)
        except ValueError:
            continue
        if e.get('type') == 'started':
            keys[e['key']] = e.get('label', '')
        elif e.get('type') == 'result':
            done.add(e['key'])
    for k, lab in keys.items():
        if k in done and lab.startswith('verify'):
            total += 1
        if k not in done and (lab.startswith('fix') or lab.startswith('repair')):
            writers.append(f"{os.path.basename(os.path.dirname(j))}:{lab}")
last = int(open(STATE).read()) if os.path.exists(STATE) else 0
if '--mark' in sys.argv:
    open(STATE, 'w').write(str(total))
    last = total
print(json.dumps({'total': total, 'since_last_commit': total - last, 'busy_writers': writers}))
