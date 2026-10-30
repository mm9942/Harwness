# Plan-store WAL consolidation

Status: LOCAL IMPLEMENTATION, not pushed, not a claim of full automatic orchestration.

## Why this prerequisite exists

A callback receipt must not rely on a plan mutation that can replay only half a
batch, race another store instance at the same expected revision, or report a
failure after its durable history already advanced. All three failures were
reproduced as failing regression tests before this change.

## Contract

- A complete new batch is a single LF-terminated, versioned JSON record with
  plan identity, base revision, events and a BLAKE3-derived integrity digest.
  Initial Create additionally records exact plan state, including tenant and
  timestamps. The reader validates the whole record before exposing any event.
- Legacy per-event JSONL remains readable. Mixed logs are supported. Revisions
  must be contiguous; checkpoint-covered events are still validated. A checkpoint
  must fall on a complete transaction boundary. No complete corrupt record is
  silently discarded. An unterminated EOF is repaired only under the root lock.
- The WAL append plus file/directory synchronization is the commit point.
  Before it, ordinary failure must leave a durably rolled-back log. If rollback
  cannot be established, return `CommitIndeterminate` and fence the instance.
  After it, checkpoint or compaction failure is maintenance failure; return the
  committed result and advance memory. Reopen recovers from WAL with or without
  a checkpoint.
- Create and catalog activation are **not** one transaction. Activation failure
  after committed Create returns `CommitIndeterminate` and fences the instance;
  callers must reopen, inspect state, and explicitly select the intended plan.
  Retrying Create blindly is incorrect. Catalog post-rename synchronization
  failure has the same uncertainty contract.
- Lock order: optional caller-held JobLeaseGuard → local plan lock → nonblocking
  root OS try-lock. No callback acquires a job lock while owning the plan lock.
  The checked publication callback stays immediately before authoritative WAL
  writing; it must not reenter the store. This does not claim an atomic
  transaction across JobStore and PlanStore or eliminate wall-clock expiry in
  the tiny check-to-I/O interval.
- All fallible reads and mutations refresh disk state under the lock. New WAL
  tails and catalog state are synchronized before exposure, resolving an earlier
  writer's indeterminate persistence. `revision()` remains an infallible cached
  compatibility hint and must not authorize admission or CAS.

## Costs and boundaries

Normal refresh scans plan directories and reads only each plan's unseen WAL tail;
no history is rewritten per mutation. Startup/full history recovery is linear in
history size. Rare WAL-only plans without a checkpoint currently require full
candidate scanning until a checkpoint is available. Existing history is cached
as before. Each serialized/read record is bounded to 16 MiB, including legacy
records; oversized data fails closed.

The root, existing WAL prefix, and stable lock inode are trusted runtime storage.
All writers must cooperate with this locking/format version. New readers support
old records; **old binaries must not write new framed logs**. Digests/seals are
not keyed authentication and do not defend against a local actor who can replace
store files, lock files, or consistently recompute integrity data. Incremental
reads assume existing prefixes are immutable. Historical partial legacy batches
cannot be retrospectively identified. Snapshot-only legacy stores are readable
but mutation is refused until complete audit history is restored; this patch
intentionally does not invent a baseline migration or missing events.

## Validation coverage

- Two separately opened instances: stale CAS rejected, refresh/retry succeeds.
- Separate process: root lock contention is nonblocking; release allows work.
  Explicit unlock prevents inherited fork descriptors prolonging a closed lock.
- Every byte truncation of framed Create and two-event transactions: no prefix
  accepted; complete corruption, duplicates/gaps and digest tampering rejected.
- WAL-only recovery preserves exact tenant and timestamps; interrupted first
  Create can retry without phantom plan registration.
- Checkpoint failure after committed WAL leaves live/reopened revisions equal;
  orphan seals do not invalidate a genuine legacy unsealed checkpoint.
- Short write and synchronization fault injection: durable rollback or explicit
  indeterminate outcome; poisoned instance rejects further fallible operations.
- Independent catalog updates preserve each other's metadata; checked callback
  rejection still publishes no replayable events.
- Real legacy JSONL plus new frames; snapshot inside a batch rejected;
  snapshot-only legacy cannot acknowledge an unrecoverable append.

The integration report records exact test commands/results. Full workspace,
platform, filesystem power-loss and CI validation remain separate requirements.
This persistence slice alone does not enable prerequisite execution, file-scope
writes, trusted structural reconciliation, or automatic plan/coding orchestration.

## Local verification (2026-10-29)

On the integration worktree, Rust 1.98.1, existing CI-compatible local profiles:

- `cargo test --offline -p harw-plan --lib`: 396 passed.
- `cargo clippy --offline -p harw-plan --all-targets -- -D warnings`: clean.
- `cargo test --offline -p harw-plan --doc`: 48 passed, 1 ignored.
- `cargo test --offline -p harw-plan-bridge --lib job_bridge`: 39 passed,
  407 outside this targeted selection.
- Independent read-only reviewers approved the final source within the stated
  trust/compatibility boundaries, including the final per-instance config guard.

The initial three regression tests failed on the pre-change source with the
specific stale CAS, partial revision replay and post-WAL error results described
above. Subsequent review added the legacy orphan-seal, first-Create crash,
relative-root, synchronization and configuration regressions; these are not
inferred solely from a successful compile.
- `cargo clippy --offline -p harw-plan-bridge --all-targets -- -D warnings`:
  clean after the callback regression run.
