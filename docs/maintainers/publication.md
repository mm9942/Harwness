# Preparing Harwness for public visibility

Repository visibility exposes more than the current default-branch files.
Review all branches, reachable Git history, tags, releases and their assets,
issues, pull requests, comments, attachments, and Actions logs or artifacts.

## Repository description

Suggested GitHub **About** description:

> Rust agent harness with typed authority, governed tools, durable jobs, CLI/TUI, and optional Detect · Orient · Defend host security.

Keep the homepage empty until the intended project website is verified.
Topics should describe implemented capabilities. Existing project topics
include `agent-harness`, `agent-orchestration`, and `agentic-ai`.

The About description is repository metadata; changing this file or the
README does not update that field.

## Choose the publication commit

1. Record the exact commit and workspace version to publish.
2. Compare `main`, `dev`, and any similarly named branches. Branch names
   are case-sensitive; `Dev` and `dev` are different refs.
3. Integrate only reviewed changes and run verification on the final commit.
4. Confirm the README, installer, source archive, release notes, and version
   describe that same intended state. An installer hosted on a mirror is a
   separate artifact and must be checked separately.

## Check the actual workspace and repository

Run these read-only commands in the working clone intended for publication:

```bash
git status --short --untracked-files=all
git diff --check
git diff --cached --check
git ls-files
git branch -a
git tag --list
git log --all --oneline
```

Review untracked and ignored local files before preparing any filesystem ZIP.
A Git source archive includes tracked content; a ZIP of the working directory
may also include credentials, sessions, downloads, build output, and worktrees.

Run a maintained secret scanner over the full reachable history and every
branch/tag, with redacted output, then inspect the findings. A filename scan
or a small set of token patterns is not a complete secret scan. Review
personal information and internal-project references separately.

Deleting a file in a new commit or adding it to `.gitignore` does not remove
earlier copies from history or PR discussions. Rotate any actual exposed
credentials. Agree on any history rewrite or branch deletion before carrying
it out; these are not cosmetic cleanup operations.

## Verification evidence

Use [CONTRIBUTING.md](../../CONTRIBUTING.md) and the checked-in CI workflow.
At minimum, record the command, commit, environment, and result for:

- installer tests;
- formatting, clippy, Rust tests, and architecture gates;
- package-scoped DoD verification;
- dependency and license checks;
- the relevant interface tests and a fresh installation smoke test.

Treat unavailable or skipped checks as unverified. Do not infer a successful
build from the presence of tests or from a successful build of another branch.

## GitHub settings and community entry points

- Set the About description and check the project links and topics.
- Confirm the desired branch rules and working review/check requirements.
- Confirm private vulnerability reporting is enabled and usable before
  relying on the reporting route in [SECURITY.md](../../SECURITY.md).
- Provide an actual private contact route for Code of Conduct reports.
- Review the issue templates, contribution guide, licenses, and release text.
- Inspect the repository's existing discussions and attachments for private
  information before changing visibility.

Change visibility only after recording what was verified and resolving
publication-blocking findings. Public visibility and a stable production
release are separate decisions.
