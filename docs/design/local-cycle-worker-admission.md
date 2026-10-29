# Local cycle execution: deferred pending shared admission

Status: design investigation only. No automatic TUI worker is installed or
started by this change. `make install` remains binary installation; existing
configuration and the `serve` worker are unchanged. Cycle tool registration is
already enabled by the existing planning default, but registration does not
supply an executing worker.

## Why the prototype was withdrawn

The existing `JobLanes`/`ResizablePermits` objects are process-local. The
`lane-limits.json` and `lane-status.json` files exchange advisory operator
settings/status, not atomic capacity ownership. Individual JobStore claims fence
one job; they do not reserve a lane-wide slot. Starting a worker per TUI could
therefore exceed the configured aggregate concurrency when several TUIs or
`serve` run together. Applying `/jobs permits` to each local semaphore does not
fix that race. A TUI-only lock would not coordinate with `serve` either.

The prototype host, its local scheduler and its unused recovery API were removed
from production code rather than exposing unverified automatic execution.

## Required shared design

Use one authoritative admission mechanism across every consumer (`serve`, local
TUI, and any future worker). Reserve capacity atomically before claim and tie
reservations to durable fenced job leases. Define lock ordering, expiry and crash
recovery, live limit reductions, authoritative configuration, and mixed-version
behavior before enabling additional consumers. Reuse existing job execution and
approval paths; do not enable MCP listeners, gateways or services implicitly.

A future local consumer must match job kind, tenant, workspace id and approved
canonical workspace root before leasing; leave unrelated jobs untouched; page
past foreign records; retain per-job budgets, approvals and lease fencing. It
must own a bounded shutdown lifecycle outside the TUI resume factory, and recover
only its own expired jobs under an atomic scope-and-expiry check. Operator permit
changes and status must reflect the same shared authority rather than independent
per-process counters.

## Required verification before activation

- Multiple real worker processes plus `serve` never exceed shared lane limits.
- `/jobs permits` changes apply globally; lowering drains without cancelling
  approved work, and raising exposes exactly the new available capacity.
- Crashes and lease expiry recover reservations without duplicate execution;
  renewed leases cannot be revoked by a stale expiry observation.
- Foreign tenant/root/kind records remain unclaimed and unmodified, even ahead
  of eligible jobs on multiple pages.
- TUI close, error and resume do not leak or duplicate workers; shutdown during
  an active claim is safely recoverable.
- Disabled planning, missing providers and malformed limits fail visibly without
  enabling services or changing user configuration.
- Run the pinned Rust workspace tests and multiprocess integration suite. This
  environment had no Cargo/Rust toolchain, so the withdrawn prototype was not
  compiled or runtime-validated.
