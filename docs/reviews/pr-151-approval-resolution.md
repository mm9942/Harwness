# PR #151: approval-resolution boundary review

Reviewed on 2026-10-26. Baseline: `f0a4a96d8c6a88d1de53a28b1279e3a71c0471b2`.
Base: `dev` at `96211a012c78f6e36f02a4e50c47f386ba69b90e`.
Review: <https://github.com/mm9942/Harwness/pull/151#discussion_r4300519761>.

## Scope and implementation

This is a production behavior change in `ApprovalStore::resolution()`, alongside
CI repairs. A lock-free reader can open a missing `.resolved.json` just before a
resolver atomically publishes it. When the following `symlink_metadata` sees a
regular file, the reader now retries once instead of reporting corruption.
There are at most two reads; the retry does not issue or resolve an approval.

The authority transition remains in `resolve_inner`: a non-blocking per-session
`fs4` lock covers previous-resolution detection, pending-record identity,
bound-actor validation (for `resolve`), server-clock TTL and durable publication.
`resolve_any_actor` deliberately relies on host authorization of responders; it
does not enforce the issuance actor. This existing contract is not changed.
`DurableApprovals` retries lock contention and reads the persisted winner.

## Trust boundary and persistence

The boundary is between the host's typed, authorized approval transition and
lock-free consumers of its on-disk terminal record. The model, callback payload
and client clock do not gain authority through the retry. A successful read is
still deserialized and checked against both requested session and request IDs.
A rejection remains a rejection; missing state never synthesizes approval.

Records live under `<root>/approvals/<session>/`: `<request>.pending.json`,
`<request>.payload.json`, `<request>.resolved.json` and `.lock`. Pending records
hold request, session, call, bound actor, issue time and optional tenant. Terminal
records hold those IDs, responding actor, decision, optional comment, resolution
time and tenant. Publication uses a temporary file in the same directory, file
`sync_all`, atomic `persist` and parent-directory sync. Sync errors propagate;
a reported error can occur after publication, so a retry must inspect durable
state rather than assume that no decision was written.

The root and ancestor directories must be controlled by the host account.
Unix `open_nofollow` protects the final component, not arbitrary parent symlinks,
hard links or hostile replacement of the storage directory. This change does
not establish a stronger filesystem-adversary model. The non-Unix reader uses a
metadata check followed by a read and has no equivalent atomic no-follow
opening guarantee; concurrent hostile path replacement there is not verified.

## Fail-closed analysis

- A genuinely absent entry returns `Ok(None)`. It conveys no approval.
- A visible symlink is corruption, including a dangling symlink. On Unix both
  read attempts independently use `O_NOFOLLOW`; the precheck is not the only
  protection against a symlink substituted before opening.
- A directory is rejected. FIFOs, devices and sockets are rejected by
  `harw_fsutil` (non-blocking open followed by descriptor type validation),
  possibly as `Io` rather than `ApprovalCorrupt`. Both are errors, not authority.
- Invalid JSON and foreign session/request IDs return `ApprovalCorrupt`.
- Other I/O failures propagate. A second NotFound with a still-visible regular
  entry fails closed rather than retrying indefinitely. Disappearance confirmed
  by metadata returns `None`; this does not turn a rejection into permission.
- Host readers map store errors to `HostError::Storage` rather than a successful
  transition. No production-code change was needed in this follow-up review.

## Tests added or strengthened

- `concurrent_double_resolve_has_one_winner`: synchronize eight responders with
  a barrier; require one winner and seven losers; compare every loser's label
  with the actual winner; check durable actor, decision, request and call IDs;
  reopen and prove a later response cannot overwrite that decision.
- Directory terminal entry: corruption, never absence.
- FIFO without a writer: error within a five-second receive deadline. Requires
  `mkfifo(1)`; failure to create the fixture fails the test rather than skipping.
- Symlinks to valid approval JSON and to a missing target: corruption; also
  exercise the same no-follow reader used by the retry, and preserve the target.
- Foreign session ID: corruption (foreign request and malformed JSON tests
  already existed).
- Persisted rejection: survives reopening and cannot be replaced by approval.

These tests target double consumption, decision overwrite, accepting a symlink
as authority, treating a non-file as absence, blocking on a FIFO, and accepting
a terminal record keyed to another session. They do not deterministically force
the exact NotFound/rename/metadata interval; the barrier test remains a
concurrency regression test, not exhaustive interleaving coverage.

## Verification evidence and remaining gates

### Baseline `f0a4a96d` — retrieved evidence, not a new execution

CI run [38986977569](https://github.com/mm9942/Harwness/actions/runs/38986977569),
`workflow_dispatch`, completed successfully on 2026-10-17. Searching only for
PR-triggered runs missed this evidence. Retrieved job metadata and x64/arm64 logs
pin the jobs to the full baseline SHA.

| Check | Baseline evidence |
| --- | --- |
| x64 fmt, clippy, architecture gates, nextest, doc tests | All steps successful; job 117260705111 |
| arm64 fmt, clippy, architecture gates, nextest, doc tests | All steps successful; job 117260705050 |
| `concurrent_double_resolve_has_one_winner` | Explicit PASS in both job logs: x64 00:50:26Z; arm64 00:51:01Z |
| nextest | Each architecture: 15,107 passed, 46 skipped; skips are not verified tests |
| DoD package-scoped build/tests | Job 117260705013 successful |
| Darwin backend checks | Job 117260705046 successful; not a macOS approval-store test claim |
| Native agent build | Job 117260704982 successful |
| cargo deny | Job 117260704953 successful; this is historical advisory evidence |
| actionlint | Job 117260705011 successful |

### Follow-up tests and documentation — not yet Rust-verified

This follow-up changes only tests and this document; it does not change
production logic or repeat #149/#150. Local `python3 -m unittest discover -s
scripts/tests -v`: 14 passed. `git diff --check`: passed.

The local environment has no `cargo`, `rustc`, `rustup` or `actionlint`.
Attempted Cargo checks (fmt, focused store and host tests, clippy, nextest,
doc tests, gates and deny) and actionlint could not start. Compilation,
formatting and execution of the new Rust tests are **not verified**. Baseline
CI success must not be attributed to this follow-up commit. No deterministic
race injection, non-Unix hostile-replacement test or crash/power-loss test ran.

Before merging, dispatch `.github/workflows/ci.yml` on the current PR branch
`ccr-c2654e2c-284ibb`, and verify its recorded head equals the new PR SHA. The
workflow has only `workflow_dispatch`; pushing this commit does not start it.
The connected tooling used for this review has no dispatch operation; rerunning
an old job would still test the old SHA. Required focused commands are:

```sh
cargo test -p harw-session-store approval::tests
cargo test -p harw-session-host --test durable_adapters concurrent_double_resolve_has_one_winner -- --exact
cargo fmt --all -- --check
cargo clippy --workspace --tests -- -D warnings
cargo nextest run --workspace
cargo test --workspace --doc
cargo run -q -p xtask -- gates
cargo deny check
actionlint
```

## Merge and supersedence

At the inspected base, `dev` is an ancestor of the baseline and
`git merge-tree --write-tree HEAD origin/dev` succeeds without conflicts.
PR #150's head `3b3fd2fa15156798e0880ff71b9bd052f5eab01b` is already an
ancestor. #149's doc-lint patch is present byte-for-byte, although its standalone
commit is not an ancestor. Do not cherry-pick either PR again. Recheck both refs
and mergeability after publishing this follow-up. A conflict-free merge does
not imply approval to merge: keep the PR draft until the new head's gates pass.
