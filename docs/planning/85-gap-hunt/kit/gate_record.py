#!/usr/bin/env python3
"""Record the central build of one frozen integration SHA (catalog P11/P19).

A green build proves something only about the exact tree it ran over. This
script pins that tree:

    record  writes docs/planning/85-gap-hunt/waves/gates-<sha12>.json with the
            goal id, the frozen SHA and every command's exit code. It refuses
            unless HEAD is the frozen SHA and the working tree is clean, so the
            record describes exactly the state the commands saw. Every step of
            the central build must be listed; a missing step is an error, not
            a pass. The file is immutable: a new SHA gets a new record.
    check   exits 0 only if the record is green and HEAD differs from its SHA
            by nothing but gate records. Any other change after the frozen
            SHA invalidates the run and needs a full new one.

usage: gate_record.py record --repo DIR --sha REF --goal ID STEP=EXIT...
       gate_record.py check  --repo DIR --record FILE

STEP is one of: fmt clippy test gates deny dod actionlint workflow-gates.
"""
import argparse
import json
import os
import re
import subprocess
import sys

STEPS = {
    "fmt": "cargo fmt --all",
    "clippy": "cargo clippy --workspace --all-targets -- -D warnings",
    "test": "cargo test --workspace (including doc tests)",
    "gates": "cargo run -q -p xtask -- gates",
    "deny": "cargo deny check",
    "dod": "make -C dod clippy test",
    "actionlint": "actionlint",
    "workflow-gates": "node docs/planning/85-gap-hunt/kit/tests/workflow-gates.test.js",
}
ENV = "CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 (cloud container, every step)"
RECORD_DIR = "docs/planning/85-gap-hunt/waves"
RECORD_RE = re.compile(r"^docs/planning/85-gap-hunt/waves/gates-[0-9a-f]{12}\.json$")


def git(repo, *args):
    out = subprocess.run(["git", "-C", repo, *args], capture_output=True, text=True)
    if out.returncode != 0:
        sys.exit(f"git {' '.join(args)} failed: {out.stderr.strip()}")
    return out.stdout


def commit(repo, ref):
    return git(repo, "rev-parse", "--verify", ref + "^{commit}").strip()


def record(a):
    sha = commit(a.repo, a.sha)
    if commit(a.repo, "HEAD") != sha:
        sys.exit(f"HEAD is not the frozen SHA {sha}: check it out and run the build there")
    if git(a.repo, "status", "--porcelain", "--untracked-files=all").strip():
        sys.exit("the working tree is not clean: the commands did not run over the frozen SHA alone")
    if not re.fullmatch(r"[a-z0-9][a-z0-9._-]*", a.goal):
        sys.exit("goal id must be [a-z0-9._-]")
    results = {}
    for item in a.steps:
        m = re.fullmatch(r"([a-z-]+)=(-?\d+)", item)
        if not m or m.group(1) not in STEPS:
            sys.exit(f"bad step {item!r}: use STEP=EXIT with STEP in {', '.join(STEPS)}")
        if m.group(1) in results:
            sys.exit(f"step {m.group(1)} given twice")
        results[m.group(1)] = int(m.group(2))
    missing = [s for s in STEPS if s not in results]
    if missing:
        sys.exit(f"missing steps: {', '.join(missing)}; a step that did not run is not a pass")
    out = os.path.join(a.repo, RECORD_DIR, f"gates-{sha[:12]}.json")
    if os.path.exists(out):
        sys.exit(f"{out} exists: gate records are immutable")
    data = {
        "schema": 1,
        "goal": a.goal,
        "frozen_sha": sha,
        "env": ENV,
        "steps": [{"step": s, "command": STEPS[s], "exit": results[s]} for s in STEPS],
        "green": all(v == 0 for v in results.values()),
        "achieved": False,
    }
    os.makedirs(os.path.dirname(out), exist_ok=True)
    with open(out, "w", encoding="utf-8") as fh:
        json.dump(data, fh, indent=1)
        fh.write("\n")
    print(out)


def check(a):
    data = json.load(open(a.record, encoding="utf-8"))
    sha = data.get("frozen_sha", "")
    if not data.get("green"):
        sys.exit(f"invalid: the run over {sha[:12]} is not green")
    changed = [p for p in git(a.repo, "diff", "--name-only", sha, "HEAD").splitlines() if p]
    other = [p for p in changed if not RECORD_RE.match(p)]
    if other:
        sys.exit(f"invalid: {len(other)} file(s) changed after the frozen SHA {sha[:12]}, "
                 f"e.g. {other[0]}; run the full central build again")
    print(f"valid: green run over {sha[:12]} for goal {data.get('goal')}")


def main():
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("record")
    r.add_argument("--repo", required=True)
    r.add_argument("--sha", required=True)
    r.add_argument("--goal", required=True)
    r.add_argument("steps", nargs="+")
    c = sub.add_parser("check")
    c.add_argument("--repo", required=True)
    c.add_argument("--record", required=True)
    a = p.parse_args()
    record(a) if a.cmd == "record" else check(a)


if __name__ == "__main__":
    main()
