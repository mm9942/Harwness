# GitHub webhook trigger for Harw paper work

This bridge is intentionally small. GitHub delivers a signed `issue_comment`
webhook; the receiver authenticates and narrows that event, then uses the
already-authenticated `gh` CLI for every GitHub read/write.

It does **not** introduce another GitHub SDK into Harw.

## Trigger

On an open **draft** PR in `mm9942/Harwness`, owned by the same repository,
with a head branch under `paper/`, an allowlisted owner/collaborator can post:

```
@harw paper
```

The exact alias `/harw paper` is also accepted. Arbitrary text after either
command is not accepted.

For PR #75 this launches `paper/HARW_WORK_PACKET.md`.

## Trust boundary

A trigger is accepted only when all of these hold:

1. `X-Hub-Signature-256` verifies with the configured HMAC-SHA256 secret.
2. GitHub event is `issue_comment` and action is `created`.
3. The comment belongs to an issue that is a pull request.
4. Repository exactly matches `HARW_GITHUB_REPO`.
5. Comment author is explicitly allowlisted.
6. Author association is `OWNER`, `MEMBER`, or `COLLABORATOR`.
7. `gh api` confirms the PR is still open and draft.
8. `gh api` confirms the PR head belongs to the same repository.
9. Branch matches an allowed prefix (default `paper/`).
10. The delivery ID has not already been processed.

The comment body never becomes shell text. All `gh`, `git`, and `harw`
calls use argv arrays.

## What the worker does

The receiver creates a detached worktree at the exact PR head SHA, runs:

```
harw --cwd <worktree> --approval full --goal <fixed-goal> exec <fixed-prompt>
```

The prompt tells Harw to execute `paper/HARW_WORK_PACKET.md` using the
business-author / business-reviewer / LaTeX pipeline described there.

`--approval full` removes interactive approval prompts for this unattended
run. It does **not** add capabilities beyond the mounted Harw/agent ceilings.

After Harw exits:

- all changed paths must be under `paper/`;
- the PR head SHA is re-read through `gh api`;
- any concurrent PR update aborts the push;
- only `paper/` is staged;
- the commit is pushed normally, never force-pushed;
- a short completion/failure comment is posted through `gh pr comment`.

No merge, release, tag, arXiv submission, or Hugging Face publication is
performed.

## Start the receiver

Requirements:

- authenticated `gh` CLI with read/write access to the repository;
- `git`;
- installed/configured `harw`;
- Python 3;
- the Harwness repository available locally.

Set a random webhook secret without placing it in Git history:

```sh
export HARW_GITHUB_WEBHOOK_SECRET='<random secret>'
export HARW_GITHUB_ALLOWED_ACTORS='mm9942'
export HARW_REPO_ROOT='/path/to/Harwness'
python3 scripts/harw-github-webhook.py
```

The default listener is loopback-only:

```
127.0.0.1:8788/github
```

Expose it through an HTTPS reverse proxy/tunnel of your choice. Do not change
the listener to `0.0.0.0` merely to make GitHub reach it.

## Configure GitHub through gh

Use the same secret in the receiver environment and in the webhook
configuration. The helper sends the secret in the request body over stdin to
`gh api`, not as a process argument:

```sh
export HARW_GITHUB_WEBHOOK_SECRET='<same secret>'
python3 scripts/configure-harw-github-webhook.py \
  --repo mm9942/Harwness \
  --url https://<public-host>/github
```

The helper is idempotent for the exact URL: it updates the matching hook or
creates one if absent. Only the `issue_comment` event is subscribed.

## Optional environment

```
HARW_GITHUB_WEBHOOK_BIND=127.0.0.1
HARW_GITHUB_WEBHOOK_PORT=8788
HARW_GITHUB_WEBHOOK_PATH=/github
HARW_GITHUB_REPO=mm9942/Harwness
HARW_GITHUB_TRIGGER='@harw paper'
HARW_GITHUB_WORK_PACKET=paper/HARW_WORK_PACKET.md
HARW_GITHUB_ALLOWED_ACTORS=mm9942
HARW_GITHUB_BRANCH_PREFIXES=paper/
HARW_GITHUB_TRIGGER_STATE=~/.harw/github-trigger
```

Multiple allowed actors or prefixes are comma-separated.

## Delivery and failure behavior

GitHub may retry webhooks. A per-delivery lock prevents parallel duplicate
runs. Successful deliveries become durable `.done` markers. Failed runs
release their lock so a later GitHub redelivery can retry.

The local run log is stored under:

```
~/.harw/github-trigger/logs/<delivery-id>.log
```

The PR comment contains only a sanitized one-line status, never the model log,
webhook secret, or full incoming payload.

## Tests

```sh
python3 -m unittest scripts/tests/test_harw_github_webhook.py -v
```

The unit tests cover signature verification, exact trigger matching, repository
pinning, PR-only behavior, and actor/association rejection.

## Current scope

This is a deliberately narrow integration for the paper Draft PR. A future
`harw-channel-github` can promote the same event policy into the long-running
Harw gateway if GitHub becomes a general conversational/work channel. Until
then, this bridge avoids widening the runtime and reuses the existing `gh`
boundary already established by `harw pr-review`.
