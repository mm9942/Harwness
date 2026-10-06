#!/usr/bin/env python3
"""Create or update Harw's GitHub webhook using the authenticated gh CLI."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from typing import Any


def gh_json(args: list[str], *, input_obj: Any | None = None) -> Any:
    payload = None if input_obj is None else json.dumps(input_obj)
    try:
        completed = subprocess.run(
            ["gh", *args],
            input=payload,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=True,
        )
    except FileNotFoundError:
        raise RuntimeError("gh CLI is not installed") from None
    except subprocess.CalledProcessError as exc:
        raise RuntimeError(f"gh failed with exit {exc.returncode}") from None
    try:
        return json.loads(completed.stdout) if completed.stdout.strip() else None
    except json.JSONDecodeError:
        raise RuntimeError("gh returned invalid JSON") from None


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", default=os.environ.get("HARW_GITHUB_REPO", "mm9942/Harwness"))
    parser.add_argument("--url", required=True, help="Public HTTPS URL ending in the configured webhook path")
    args = parser.parse_args()

    secret = os.environ.get("HARW_GITHUB_WEBHOOK_SECRET", "")
    if not secret:
        print("HARW_GITHUB_WEBHOOK_SECRET is required", file=sys.stderr)
        return 2
    if not args.url.startswith("https://"):
        print("--url must be HTTPS", file=sys.stderr)
        return 2

    hooks = gh_json(["api", f"repos/{args.repo}/hooks"])
    if not isinstance(hooks, list):
        print("unexpected hooks response", file=sys.stderr)
        return 2

    existing = next(
        (
            hook
            for hook in hooks
            if isinstance(hook, dict)
            and isinstance(hook.get("config"), dict)
            and hook["config"].get("url") == args.url
        ),
        None,
    )

    body = {
        "name": "web",
        "active": True,
        "events": ["issue_comment"],
        "config": {
            "url": args.url,
            "content_type": "json",
            "secret": secret,
            "insecure_ssl": "0",
        },
    }

    if existing is None:
        result = gh_json(
            ["api", "--method", "POST", f"repos/{args.repo}/hooks", "--input", "-"],
            input_obj=body,
        )
        action = "created"
    else:
        hook_id = existing.get("id")
        if not isinstance(hook_id, int):
            print("matching hook has no numeric id", file=sys.stderr)
            return 2
        result = gh_json(
            ["api", "--method", "PATCH", f"repos/{args.repo}/hooks/{hook_id}", "--input", "-"],
            input_obj=body,
        )
        action = "updated"

    hook_id = result.get("id") if isinstance(result, dict) else None
    print(f"{action} webhook id={hook_id} repo={args.repo} url={args.url}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
