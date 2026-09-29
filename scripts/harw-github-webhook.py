#!/usr/bin/env python3
"""Signed GitHub issue_comment webhook -> Harw paper work packet.

The receiver deliberately uses GitHub CLI (gh) for all GitHub reads/writes.
The webhook body is only trusted after HMAC-SHA256 verification and never
becomes shell text. All subprocesses use argv arrays with shell=False.
"""

from __future__ import annotations

import hashlib
import hmac
import json
import os
import re
import shutil
import subprocess
import sys
import threading
from dataclasses import dataclass
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

MAX_BODY_BYTES = 1024 * 1024
DEFAULT_BIND = "127.0.0.1"
DEFAULT_PORT = 8788
DEFAULT_PATH = "/github"
DEFAULT_REPO = "mm9942/Harwness"
DEFAULT_TRIGGER = "@harw paper"
DEFAULT_PACKET = "paper/HARW_WORK_PACKET.md"
DELIVERY_RE = re.compile(r"^[A-Za-z0-9-]{1,128}$")


class TriggerError(RuntimeError):
    pass


@dataclass(frozen=True)
class Trigger:
    delivery_id: str
    repo: str
    pr: int
    actor: str
    body: str


@dataclass(frozen=True)
class Settings:
    bind: str
    port: int
    path: str
    repo: str
    trigger: str
    packet: str
    repo_root: Path
    state_root: Path
    allowed_actors: frozenset[str]
    branch_prefixes: tuple[str, ...]
    secret: bytes


def env_settings() -> Settings:
    secret = os.environ.get("HARW_GITHUB_WEBHOOK_SECRET", "")
    if not secret:
        raise TriggerError("HARW_GITHUB_WEBHOOK_SECRET is required")

    repo_root = Path(os.environ.get("HARW_REPO_ROOT", ".")).resolve()
    state_root = Path(
        os.environ.get(
            "HARW_GITHUB_TRIGGER_STATE",
            str(Path.home() / ".harw" / "github-trigger"),
        )
    ).expanduser().resolve()

    actors = {
        value.strip()
        for value in os.environ.get("HARW_GITHUB_ALLOWED_ACTORS", "mm9942").split(",")
        if value.strip()
    }
    prefixes = tuple(
        value.strip()
        for value in os.environ.get("HARW_GITHUB_BRANCH_PREFIXES", "paper/").split(",")
        if value.strip()
    )
    if not actors:
        raise TriggerError("HARW_GITHUB_ALLOWED_ACTORS must not be empty")
    if not prefixes:
        raise TriggerError("HARW_GITHUB_BRANCH_PREFIXES must not be empty")

    path = os.environ.get("HARW_GITHUB_WEBHOOK_PATH", DEFAULT_PATH)
    if not path.startswith("/") or "{" in path or "}" in path:
        raise TriggerError("HARW_GITHUB_WEBHOOK_PATH must be a literal absolute path")

    return Settings(
        bind=os.environ.get("HARW_GITHUB_WEBHOOK_BIND", DEFAULT_BIND),
        port=int(os.environ.get("HARW_GITHUB_WEBHOOK_PORT", str(DEFAULT_PORT))),
        path=path,
        repo=os.environ.get("HARW_GITHUB_REPO", DEFAULT_REPO),
        trigger=os.environ.get("HARW_GITHUB_TRIGGER", DEFAULT_TRIGGER).strip(),
        packet=os.environ.get("HARW_GITHUB_WORK_PACKET", DEFAULT_PACKET),
        repo_root=repo_root,
        state_root=state_root,
        allowed_actors=frozenset(actors),
        branch_prefixes=prefixes,
        secret=secret.encode("utf-8"),
    )


def verify_signature(secret: bytes, body: bytes, signature: str | None) -> bool:
    if not signature or not signature.startswith("sha256="):
        return False
    supplied = signature[len("sha256=") :]
    if len(supplied) != 64:
        return False
    expected = hmac.new(secret, body, hashlib.sha256).hexdigest()
    return hmac.compare_digest(expected, supplied)


def parse_trigger(
    *,
    payload: dict[str, Any],
    delivery_id: str,
    event: str,
    settings: Settings,
) -> Trigger | None:
    if event != "issue_comment":
        return None
    if not DELIVERY_RE.fullmatch(delivery_id):
        raise TriggerError("invalid X-GitHub-Delivery")
    if payload.get("action") != "created":
        return None

    repository = payload.get("repository") or {}
    if repository.get("full_name") != settings.repo:
        raise TriggerError("repository is not allowlisted")

    issue = payload.get("issue") or {}
    if "pull_request" not in issue:
        return None

    comment = payload.get("comment") or {}
    actor = str((comment.get("user") or {}).get("login") or "")
    if actor not in settings.allowed_actors:
        raise TriggerError("comment actor is not allowlisted")

    association = str(comment.get("author_association") or "").upper()
    if association not in {"OWNER", "MEMBER", "COLLABORATOR"}:
        raise TriggerError("comment author association is not trusted")

    body = str(comment.get("body") or "").strip()
    accepted = {settings.trigger, "/harw paper"}
    if body not in accepted:
        return None

    pr = issue.get("number")
    if not isinstance(pr, int) or pr <= 0:
        raise TriggerError("invalid pull request number")

    return Trigger(
        delivery_id=delivery_id,
        repo=settings.repo,
        pr=pr,
        actor=actor,
        body=body,
    )


def run(
    argv: list[str],
    *,
    cwd: Path | None = None,
    input_text: str | None = None,
    stdout=None,
    stderr=None,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            argv,
            cwd=cwd,
            input=input_text,
            text=True,
            stdout=stdout if stdout is not None else subprocess.PIPE,
            stderr=stderr if stderr is not None else subprocess.PIPE,
            check=check,
        )
    except FileNotFoundError as exc:
        raise TriggerError(f"required executable not found: {argv[0]}") from exc
    except subprocess.CalledProcessError as exc:
        raise TriggerError(
            f"{argv[0]} failed with exit {exc.returncode}"
        ) from exc


def gh_json(args: list[str], *, cwd: Path) -> Any:
    result = run(["gh", *args], cwd=cwd)
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        raise TriggerError("gh returned invalid JSON") from exc


def fetch_pr(trigger: Trigger, settings: Settings) -> dict[str, Any]:
    data = gh_json(
        ["api", f"repos/{trigger.repo}/pulls/{trigger.pr}"],
        cwd=settings.repo_root,
    )
    if not isinstance(data, dict):
        raise TriggerError("gh pull response is not an object")
    return data


def validate_pr(trigger: Trigger, settings: Settings) -> tuple[str, str]:
    pr = fetch_pr(trigger, settings)
    if pr.get("state") != "open":
        raise TriggerError("pull request is not open")
    if pr.get("draft") is not True:
        raise TriggerError("pull request must be draft")

    head = pr.get("head") or {}
    branch = str(head.get("ref") or "")
    sha = str(head.get("sha") or "")
    head_repo = str((head.get("repo") or {}).get("full_name") or "")
    if head_repo != trigger.repo:
        raise TriggerError("cross-repository pull requests are not accepted")
    if not any(branch.startswith(prefix) for prefix in settings.branch_prefixes):
        raise TriggerError("pull request branch is outside the allowed prefixes")
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        raise TriggerError("pull request head SHA is invalid")
    return branch, sha


def post_comment(trigger: Trigger, settings: Settings, body: str) -> None:
    run(
        [
            "gh",
            "pr",
            "comment",
            str(trigger.pr),
            "--repo",
            trigger.repo,
            "--body",
            body,
        ],
        cwd=settings.repo_root,
    )


def changed_paths(worktree: Path) -> list[str]:
    changed: set[str] = set()
    for argv in (
        ["git", "diff", "--name-only", "--relative"],
        ["git", "diff", "--cached", "--name-only", "--relative"],
        ["git", "ls-files", "--others", "--exclude-standard"],
    ):
        result = run(argv, cwd=worktree)
        changed.update(line.strip() for line in result.stdout.splitlines() if line.strip())
    return sorted(changed)


def ensure_paper_only(paths: list[str]) -> None:
    outside = [path for path in paths if path != "paper" and not path.startswith("paper/")]
    if outside:
        raise TriggerError(
            "Harw changed paths outside paper/: " + ", ".join(outside[:8])
        )


def prepare_worktree(
    trigger: Trigger,
    settings: Settings,
    branch: str,
    sha: str,
) -> Path:
    delivery = trigger.delivery_id[:24]
    worktree = settings.state_root / "worktrees" / f"pr-{trigger.pr}-{delivery}"
    if worktree.exists():
        shutil.rmtree(worktree)
    worktree.parent.mkdir(parents=True, exist_ok=True)

    run(["git", "fetch", "--no-tags", "origin", branch], cwd=settings.repo_root)
    run(["git", "worktree", "add", "--detach", str(worktree), sha], cwd=settings.repo_root)
    return worktree


def cleanup_worktree(settings: Settings, worktree: Path) -> None:
    try:
        run(
            ["git", "worktree", "remove", "--force", str(worktree)],
            cwd=settings.repo_root,
            check=False,
        )
    finally:
        if worktree.exists():
            shutil.rmtree(worktree, ignore_errors=True)


def run_harw(trigger: Trigger, settings: Settings, worktree: Path, log_path: Path) -> None:
    packet = worktree / settings.packet
    if not packet.is_file():
        raise TriggerError(f"work packet missing: {settings.packet}")

    prompt = (
        f"Execute {settings.packet} exactly for GitHub PR #{trigger.pr}. "
        "Use Harw's bundled business-author -> business-reviewer -> business-author "
        "-> business-reviewer -> latex-writer pipeline described by that packet. "
        "This is a draft research-paper pass. Do not merge, publish, tag, release, "
        "or mutate GitHub. Work only inside paper/. Missing evidence, literature, "
        "results, or author metadata must remain explicit instead of being invented."
    )
    goal = (
        "Produce the reviewed draft artifacts required by paper/HARW_WORK_PACKET.md "
        "while preserving scientific evidence boundaries and draft status."
    )

    log_path.parent.mkdir(parents=True, exist_ok=True)
    with log_path.open("w", encoding="utf-8") as log:
        result = run(
            [
                "harw",
                "--cwd",
                str(worktree),
                "--approval",
                "full",
                "--goal",
                goal,
                "exec",
                prompt,
            ],
            cwd=worktree,
            stdout=log,
            stderr=subprocess.STDOUT,
            check=False,
        )
    if result.returncode != 0:
        raise TriggerError(f"harw exec failed with exit {result.returncode}")


def commit_and_push(
    trigger: Trigger,
    settings: Settings,
    worktree: Path,
    branch: str,
    original_sha: str,
) -> str | None:
    paths = changed_paths(worktree)
    if not paths:
        return None
    ensure_paper_only(paths)

    latest_branch, latest_sha = validate_pr(trigger, settings)
    if latest_branch != branch or latest_sha != original_sha:
        raise TriggerError(
            "PR head changed while Harw was working; refusing to push stale output"
        )

    run(["git", "add", "--", "paper"], cwd=worktree)
    run(
        [
            "git",
            "-c",
            "user.name=Harw",
            "-c",
            "user.email=harw@localhost",
            "commit",
            "-m",
            "docs(paper): Harw webhook authoring pass",
        ],
        cwd=worktree,
    )
    sha = run(["git", "rev-parse", "HEAD"], cwd=worktree).stdout.strip()
    run(
        ["git", "push", "origin", f"HEAD:refs/heads/{branch}"],
        cwd=worktree,
    )
    return sha


def delivery_paths(trigger: Trigger, settings: Settings) -> tuple[Path, Path]:
    deliveries = settings.state_root / "deliveries"
    deliveries.mkdir(parents=True, exist_ok=True)
    return (
        deliveries / f"{trigger.delivery_id}.lock",
        deliveries / f"{trigger.delivery_id}.done",
    )


def claim_delivery(trigger: Trigger, settings: Settings) -> bool:
    lock, done = delivery_paths(trigger, settings)
    if done.exists():
        return False
    try:
        fd = os.open(lock, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    except FileExistsError:
        return False
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        handle.write(f"pr={trigger.pr}\nactor={trigger.actor}\n")
    return True


def finish_delivery(trigger: Trigger, settings: Settings, *, success: bool) -> None:
    lock, done = delivery_paths(trigger, settings)
    if success:
        try:
            os.replace(lock, done)
        except FileNotFoundError:
            done.touch(mode=0o600, exist_ok=True)
    else:
        try:
            lock.unlink()
        except FileNotFoundError:
            pass


def process_trigger(trigger: Trigger, settings: Settings) -> None:
    worktree: Path | None = None
    success = False
    log_path = settings.state_root / "logs" / f"{trigger.delivery_id}.log"
    try:
        branch, sha = validate_pr(trigger, settings)
        worktree = prepare_worktree(trigger, settings, branch, sha)
        run_harw(trigger, settings, worktree, log_path)
        commit = commit_and_push(trigger, settings, worktree, branch, sha)
        if commit:
            post_comment(
                trigger,
                settings,
                "Harw paper pass completed and pushed "
                f"`{commit[:12]}` to `{branch}`. "
                "The PR remains draft; publication is not authorized.",
            )
        else:
            post_comment(
                trigger,
                settings,
                "Harw paper pass completed without workspace changes. "
                "The PR remains draft.",
            )
        success = True
    except Exception as exc:  # boundary: report one sanitized failure line
        message = str(exc).replace("\n", " ").strip()
        try:
            post_comment(
                trigger,
                settings,
                "Harw paper trigger failed: "
                f"`{message[:500]}`. No forced push was attempted.",
            )
        except Exception:
            pass
    finally:
        if worktree is not None:
            cleanup_worktree(settings, worktree)
        finish_delivery(trigger, settings, success=success)


class Handler(BaseHTTPRequestHandler):
    server_version = "HarwGitHubTrigger/1"

    def do_POST(self) -> None:  # noqa: N802
        settings: Settings = self.server.settings  # type: ignore[attr-defined]
        if self.path != settings.path:
            self.send_error(HTTPStatus.NOT_FOUND)
            return

        length_header = self.headers.get("Content-Length")
        try:
            length = int(length_header or "")
        except ValueError:
            self.send_error(HTTPStatus.LENGTH_REQUIRED)
            return
        if length < 0 or length > MAX_BODY_BYTES:
            self.send_error(HTTPStatus.REQUEST_ENTITY_TOO_LARGE)
            return

        body = self.rfile.read(length)
        if not verify_signature(
            settings.secret,
            body,
            self.headers.get("X-Hub-Signature-256"),
        ):
            self.send_error(HTTPStatus.UNAUTHORIZED)
            return

        try:
            payload = json.loads(body)
            if not isinstance(payload, dict):
                raise TriggerError("payload is not an object")
            trigger = parse_trigger(
                payload=payload,
                delivery_id=self.headers.get("X-GitHub-Delivery", ""),
                event=self.headers.get("X-GitHub-Event", ""),
                settings=settings,
            )
        except (json.JSONDecodeError, TriggerError):
            self.send_error(HTTPStatus.BAD_REQUEST)
            return

        if trigger is None:
            self.send_response(HTTPStatus.NO_CONTENT)
            self.end_headers()
            return

        if not claim_delivery(trigger, settings):
            self.send_response(HTTPStatus.NO_CONTENT)
            self.end_headers()
            return

        threading.Thread(
            target=process_trigger,
            args=(trigger, settings),
            name=f"harw-pr-{trigger.pr}",
            daemon=True,
        ).start()
        self.send_response(HTTPStatus.ACCEPTED)
        self.end_headers()

    def log_message(self, _format: str, *_args: object) -> None:
        # Do not log request paths/headers/body; structured Harw/GitHub logs
        # can be added later without risking webhook-secret or comment leakage.
        return


class Server(ThreadingHTTPServer):
    settings: Settings


def main() -> int:
    try:
        settings = env_settings()
        settings.state_root.mkdir(parents=True, exist_ok=True)
        server = Server((settings.bind, settings.port), Handler)
        server.settings = settings
    except Exception as exc:
        print(f"harw github trigger: {exc}", file=sys.stderr)
        return 2

    print(
        f"harw github trigger: listening on {settings.bind}:{settings.port}{settings.path} "
        f"for {settings.repo}; trigger={settings.trigger!r}",
        file=sys.stderr,
    )
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
