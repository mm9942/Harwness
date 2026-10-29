# Harw cloud implementation ledger

Baseline: `dev@fcb12b41f5e53c19cc20607a63c98b829452b505`.
Delivery branch: `codex/harw-cloud-implementation`. Updated 2026-10-29.

This implements initial portions of the accepted full-cloud plan, using existing
PR designs rather than introducing another scheduler. The target remains all
six stages below. This document is not a claim that those stages have all landed.

## Accepted decisions and source ownership

- Cloud Home/profile/hub vocabulary: #82/#83, #88/#90 and #93; runtime code takes
  precedence over historical missing-source claims.
- Session contracts and transport: #54/#57, #98 and current SessionHost code.
- Worker startup and placement: #80/#85 and PL-66.
- Existing UI/capability surfaces: #143/#144 and #154; no separate replacement UI.
- Coding admission, writer limits and central verification: #153.
- Destination execution/security: #154/#156; inspect and consume exact tested
  commits rather than assuming the unmerged branches are available on dev.
- Integration plan and full acceptance contract: #155 and the existing dossier.

The first useful milestone is transparent operations. Linux and macOS are client
and administration targets; strict jobs use an explicitly configured Linux node
or Linux VM. Defaults are user services/LaunchAgents, existing self-managed nodes,
no automatic infrastructure purchasing, and no silent backend or session-owner
failover. Existing ask/auto/full and per-device remote-approval opt-in are retained.

## Delivery status

| Stage | Changes in this delivery | Still required before stage acceptance |
| --- | --- | --- |
| A: diagnostics and administration | Canonical bounded tenant-filtered registry reads; bound profile diagnostics; TCP reachability separated from readiness; typed user service start/stop/restart and explicit CLI selection. | Profile-bound production service adapter, validated configuration/status evidence, install/readback/rollback, system service/LaunchDaemon mode, native macOS acceptance. |
| B: device trust | Existing registry and approval-opt-in parser reused; incorrect service enable/disable enrollment behavior removed; pairing expiry parsed by instant with injected clock and redacted debug output. | Durable registry + peer-pin enrollment transaction, pairing crash/replay/concurrency tests and cleanup evidence. |
| C: remote sessions | Node alias attach retains expected identity and connects via existing authenticated transport; selected-home config provenance; local and remote transcript startup repair; trusted creator execution admission, persisted key binding and live device-policy invalidation. | Shared local/remote SessionHost, reconnect supervisor, durable owner/generation directory, HTTP/WS proxy composition and restart/partition acceptance. |
| D: explicit remote build | Existing persisted process jobs can be claimed through the coordinator without a second identity; atomic expected-record comparison prevents substitution. No remote execution enabled. | Trusted approval-preserving core-task projection and destination bridge, verified source/output/scratch restrictions, artifact transfer, capacity reservation and reconciliation. |
| E: coding workers | No new remote source writes enabled. | #153 writer gate plus #154/#156 destination evidence, central verification, scoped mutation and failure recovery. |
| F: placement and on-demand workers | Pure required-dimension filter plus durable destination slot reservation prerequisite on the existing record store; no automatic selection/provisioning enabled. | Destination composition, global attempt fencing across nodes, measured CPU/RAM admission, deterministic score, singleflight startup and separate provider budgets. |

The local human TUI operator receives an explicit read-only selected-profile
administration capability, never a fabricated tenant. An explicit authenticated
tenant takes precedence and restricts the list. The workspace tenant (for example
`tui`) is an isolation namespace, **not** device tenant identity. Hosted/remote
cloud model operations remain unavailable until that identity bridge exists.
Ordinary non-cloud operation tenant behavior is unchanged.

## Ownership and continuation

Sol coordinates assembly and dependency contracts, Terra independently reviews
identity and runtime boundaries, and Luna zones implement bounded independent
service/recovery work. Shared assembly, manifests/lockfile and release assets
have one owner at a time. External sessions coordinate through small PR commits
and exact verification reports; no direct session communication is assumed.

Next dependency order: finish A service binding and B enrollment; compose C's
single owner before directory/reconnect; enable D only after destination refusal
and output-isolation tests; then E and F. Persist WorkId/request-to-JobId/attempt/
lease mapping in the existing job lifecycle. Keep `JobScopeId` distinct from a
versioned optional write ceiling; reject incompatible scope-bearing messages.
Do not start both current admission paths for the same request.

## Verification scope

Focused tests exercise selected profiles, tenant refusal/filtering, canonical
legacy/approval records, bounded reads, typed service dispatch, wrong peer pins,
configuration provenance and transcript repair/contention/corruption. Real kernel
containment, actual systemd/launchd administration, AuthHub-to-interactive-TUI
acceptance, macOS execution and remote build/coding/placement are not established
by these tests. The PR records exact commands and outcomes; a missing CI run is
not a passing check.

## Implementation contracts added after the initial slice

`Coordinator::start_existing` consumes an existing process envelope in its trusted
store namespace and preserves WorkId/attempt history. The filesystem coordinator
store compares the entire prevalidated record while holding its record lock
before allocating a lease. Custom stores without this atomic method refuse.
This is not yet the core admission adapter: translating governed task inputs into
process envelopes must retain their approval provenance and destination scope.

`FsNodeCapacityStore` uses one canonical destination-owned root, existing record
locks and atomic persistence. It accounts explicit slots, not measured CPU/RAM.
Policy revision, freshness and active/draining/revoked state are checked before
reservation and start publication. A trusted clock is sampled at mutation and
again before publication. Start admission is one-shot; uncertain effects remain
charged after lease expiry until fenced supervisor reconciliation. Expired
never-started reservations are durably tombstoned before their slots are reused.
A separate create-only bootstrap witness prevents automatic reset after ledger
quarantine. Destination composition must provision the root durably and never
use a different root per process. Global admission still owns fencing across
multiple destinations; this node-local store does not provide that alone.

Placement's pure filter rejects missing or unenforced required dimensions before
selection. A returned compatible candidate is not authorization, capacity, or
permission to start. There is no automatic fallback or provider budget here.

Creator execution admission is minted from authenticated connection facts; persisted
creator metadata is only an authority ceiling. Runtime hydration retains that
admission through its execution driver, including generation checks and cleanup
of unpublished creation. After restart, new-format sessions require fresh creator
authentication before executing. Observer attachment cannot restore creator power.
This does not activate a shared local/remote host.

Remote peer pins are read through bounded no-follow access. Changes latch new
authentication closed until restart. Trusted per-node key fingerprints bind session
ownership to the authenticated key. Registry changes invalidate both connected and
detached creator authority, including tier, tenant and approval-opt-in changes.
The watcher is poll-bounded (five seconds by default), not an atomic per-effect
policy fence. Recovered registry policy can admit a fresh connection; explicit
revocation remains permanent.

The process coordinator revalidates its persisted lease and cancellation immediately
before executor start, after its final asynchronous store read. This narrows stale
start races but does not provide an atomic lease-to-OS-spawn transaction. Destination
composition and reconciliation must close that remaining boundary before activation.

## User-directed verification pause

The user explicitly paused Cargo compilation during implementation. No subsequent
Cargo build, check, clippy or test result is claimed for the final changes.
Formatting, source review and diff checks continue; authored regression tests
remain unexecuted. Earlier focused passes cover intermediate snapshots only.
The implementation stays draft until the final combined tree is compiled and
its required tests and platform acceptance gates run.
