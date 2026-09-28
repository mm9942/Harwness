#!/usr/bin/env python3
"""Write the immutable manifest of one fix wave (catalog P11/P14).

A wave's "complete" is a model report. The manifest pins what that report is
about, so a resume, a re-cut or a later audit can check it against git:

    base_sha        commit the wave's branch was cut from
    branch          the wave's own branch
    head_sha        the wave's content commit (the manifest is committed on
                    top of it, so it can name it)
    files           the exact file set the wave may change
    findings        stable ids (F-<sha1 of file:line:title>) with file, line,
                    severity, pattern and title
    workflow_runs   run ids of every workflow that wrote or reviewed it
    disposition     complete flag, open files, ripple status and ripple item
                    ids, cluster statuses, as the workflow returned them
    goal            (schema 2) goal id, statement and acceptance criteria the
                    wave served, and the per-criterion coverage and deltas the
                    goal check returned; `achieved` is always false: only a
                    human declares a goal achieved
    central_build   the commands the central build must run over the final
                    integration head

Schema 2 waves embed `goal` and `base` in their args. The manifest refuses a
--base that does not resolve to the same commit as the wave's pinned base, so
a wave cannot be recorded against another state than it was checked against.

The file lands at docs/planning/85-gap-hunt/waves/<wave>.json in the given
checkout and is never edited afterwards: a re-cut wave gets a new name.

usage: wave_manifest.py --repo DIR --wave NAME --branch BRANCH --base REF
                        --head REF --script WAVE.js [--run ID]...
                        [--result TASK_OUTPUT.json] [--note TEXT] FILE...
"""
import argparse
import hashlib
import json
import os
import re
import subprocess
import sys

CENTRAL_BUILD = [
    "env CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 (cloud container, every step)",
    "cargo fmt --all",
    "cargo clippy --workspace --all-targets -- -D warnings",
    "cargo test --workspace (including doc tests)",
    "cargo run -q -p xtask -- gates",
    "cargo deny check",
    "make -C dod clippy test",
    "actionlint",
]


def rev(repo, ref):
    out = subprocess.run(["git", "-C", repo, "rev-parse", "--verify", ref + "^{commit}"],
                         capture_output=True, text=True)
    if out.returncode != 0:
        sys.exit(f"unknown commit {ref!r} in {repo}")
    return out.stdout.strip()


def wave_args(script):
    """The args a wave script embeds as `const A = {...}`."""
    match = re.search(r"const A = (\{.*?\})\n", open(script, encoding="utf-8").read())
    if not match:
        sys.exit(f"{script}: no embedded `const A = {{...}}`")
    return json.loads(match.group(1))


def finding_id(f):
    key = f"{f.get('file', '')}:{f.get('line', 0)}:{f.get('title', '')}"
    return "F-" + hashlib.sha1(key.encode("utf-8")).hexdigest()[:12]


def findings_of(args):
    raw = list(args.get("findings", []))
    for cluster in args.get("clusters", []):
        raw += [dict(f, cluster=cluster.get("id")) for f in cluster.get("findings", [])]
    out = []
    for f in raw:
        item = {"id": finding_id(f)}
        for k in ("cluster", "file", "line", "severity", "pattern", "title"):
            if f.get(k) is not None:
                item[k] = f[k]
        out.append(item)
    return sorted(out, key=lambda x: x["id"])


def load_result(path):
    result = json.load(open(path, encoding="utf-8"))
    return result.get("result", result)


def goal_of(args, result):
    goal = args.get("goal")
    if not goal:
        return None
    out = {
        "id": goal.get("id"),
        "statement": goal.get("statement"),
        "criteria": [{"id": c.get("id"), "text": c.get("text")} for c in goal.get("criteria", [])],
        "achieved": False,
    }
    if result is not None:
        out["coverage"] = [{k: c.get(k) for k in ("id", "status", "evidence", "reason") if c.get(k) is not None}
                           for c in result.get("coverage", [])]
        out["deltas"] = list(result.get("deltas", []))
    return out


def disposition_of(result):
    d = {k: result[k] for k in ("complete", "rippleStatus", "missing") if k in result}
    if "open" in result:
        d["open"] = [{"file": x.get("file"), "status": x.get("status"), "reason": x.get("reason")}
                     for x in result["open"]]
    if "files" in result:
        d["files"] = [{k: x[k] for k in ("file", "status", "ok", "repaired") if k in x}
                      for x in result["files"]]
    if isinstance(result.get("ripple"), list):
        d["ripple"] = [{k: x.get(k) for k in ("id", "file", "line")} for x in result["ripple"]]
    if "reports" in result:
        d["clusters"] = [{"id": r.get("id"), "status": r.get("status"), "reason": r.get("reason")}
                         for r in result["reports"]]
    return d


def main():
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("--repo", required=True)
    p.add_argument("--wave", required=True)
    p.add_argument("--branch", required=True)
    p.add_argument("--base", required=True)
    p.add_argument("--head", required=True)
    p.add_argument("--script", required=True)
    p.add_argument("--run", action="append", default=[])
    p.add_argument("--result")
    p.add_argument("--note")
    p.add_argument("files", nargs="+")
    a = p.parse_args()
    args = wave_args(a.script)
    base_sha = rev(a.repo, a.base)
    if args.get("base") and rev(a.repo, args["base"]) != base_sha:
        sys.exit(f"--base {a.base} is not the wave's pinned base {args['base']}")
    result = load_result(a.result) if a.result else None
    if not re.fullmatch(r"[A-Za-z0-9._-]+", a.wave):
        sys.exit("wave name must be [A-Za-z0-9._-]+")
    out = os.path.join(a.repo, "docs/planning/85-gap-hunt/waves", a.wave + ".json")
    if os.path.exists(out):
        sys.exit(f"{out} exists: manifests are immutable, re-cut the wave under a new name")
    manifest = {
        "schema": 2 if args.get("goal") else 1,
        "wave": a.wave,
        "branch": a.branch,
        "base_sha": base_sha,
        "head_sha": rev(a.repo, a.head),
        "files": sorted(set(a.files)),
        "findings": findings_of(args),
        "workflow_runs": a.run,
        "disposition": disposition_of(result) if result is not None else None,
        "goal": goal_of(args, result),
        "central_build": CENTRAL_BUILD,
    }
    if a.note:
        manifest["note"] = a.note
    os.makedirs(os.path.dirname(out), exist_ok=True)
    with open(out, "w", encoding="utf-8") as fh:
        json.dump(manifest, fh, indent=1, ensure_ascii=False)
        fh.write("\n")
    print(out)


if __name__ == "__main__":
    main()
