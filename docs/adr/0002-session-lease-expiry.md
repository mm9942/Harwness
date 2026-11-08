# ADR 0002 — Host work phases (session leases) expire

> Status: accepted · Date: 2026-11-08 · Tracking: PL-100 Phase 4
> ("Prozesswerkzeuge härten"), threat B22

**Amends:** the user decision of 2026-09-24 that a host work phase has no
expiry and ends only when the user ends it. That decision is recorded in
[`interaction-contract.md`](../design/interaction-contract.md) (the
`/sandbox-lease` entry), in
[`mediated-process-execution.md`](../design/mediated-process-execution.md)
(the `SessionLease` bullet) and in
[`tui-command-contract.md`](../design/tui-command-contract.md) (the
`/sandbox-lease` row and the `Ctrl+H` row). On the expiry point this ADR
supersedes those passages.
**Binds to:** [PL-100 README](../planning/100-dod-ai-integration/README.md)
§4 Phase 4 ("`SessionLease` mit Ablauf") and
[THREAT-MODEL](../planning/100-dod-ai-integration/THREAT-MODEL.md) B22.

ADRs are numbered in order and never renumbered (see ADR 0001). This
record keeps the sections *Context*, *Decision*, *Consequences*,
*Security* and *Alternatives considered*.

---

## 1. Context

A host work phase (`HostApprovalScope::SessionLease`, "Host-Arbeitsphase")
lets `shell.exec` and related tools run on the host without the
Bubblewrap sandbox. The user grants it in the local host-permit dialog, by
`/sandbox-lease request`, or by answering a `request_host` escalation. The
grant is process-wide (`HostPermitSessionRegistry::mark_global_approval`):
the root session and every child agent of the same `harw` process run on
the host.

On 2026-09-24 the user decided that such a phase has no expiry. Only
`Ctrl+H` (`ChatApp::end_host_mode`) or the typed `/sandbox-lease revoke`
ended it. The ledger permits issued under it (`ProcessPermitLedger`,
`ttl = None`) did not expire either.

PL-100 connects DoD findings to agents. THREAT-MODEL B22 names the
resulting risk: after a finding, an agent (attacker A3, prompt injection)
can keep using host tools such as `process.kill` or `host.sudo_exec`
without asking, because an open-ended global `SessionLease` from hours or
days ago still covers it. A phase the user has forgotten about is exactly
the state a compromised agent needs.

Phase 4 of PL-100 therefore plans "`SessionLease` mit Ablauf". The merged
lane `p4-proc-hardening` added the mechanism to `harw-sandbox`.
`DEFAULT_SESSION_LEASE_TTL`, `LeaseInstant` and `LeaseWindow` live in
`process_permit.rs`. `ProcessPermitLedger::with_lease_ttl` and
`issue_for_lease` live there too. `HostPermitSessionRegistry` gained
`with_lease_ttl`, `covering_lease`, `session_lease`, `global_lease` and
`lease_remaining`. This record states the decision and how the callers
use it.

## 2. Decision

### 2.1 A host work phase expires

- Every host work phase gets a window: `granted_at` is the moment of
  approval, and `expires_at = granted_at + ttl`. Once the window ends, the
  phase counts as absent. `is_session_approved` and `covering_lease` return
  `false` or `None`, and the next host command needs a new approval
  (outside Full Access the dialog asks again; see the Full Access bullet
  below).
- The default `ttl` is **8 hours** (`DEFAULT_SESSION_LEASE_TTL`,
  28 800 s).
- A phase is never extended silently. Approving again while it is active
  changes nothing. After it ends, a new approval opens a new window. The
  model cannot extend a phase, and it still cannot end one.
- The user can still end a phase early with `Ctrl+H` or
  `/sandbox-lease revoke`. Single-use approvals stay single-use.
- **Full Access is the exception.** Under `ApprovalMode::FullAccess` the
  TUI answers every host-permit prompt itself with `SessionLease`
  (`auto_grant_host_permit`, user decision 2026-09-24 "Full Access never
  asks"). This includes the prompt of a model-issued
  `/sandbox-lease request`. After expiry the next host command therefore
  opens a fresh window without a human decision. Expiry bounds forgotten
  phases only outside Full Access.

### 2.2 Configuration key

`[sandbox] host_lease_ttl_secs` (`SandboxSection` in
`harw-config/src/harness_config.rs`) sets the duration in seconds:

- The default is 28 800 (`DEFAULT_HOST_LEASE_TTL_SECS`).
- The allowed range is 1 s to 7 days (`HOST_LEASE_TTL_SECS_RANGE`). `0`,
  negative values and values above 604 800 are parse errors, not clamped.
- The runtime assembly (`RuntimeAssembly::build`) builds both the
  `ProcessPermitLedger` and the `HostPermitSessionRegistry` with
  `with_lease_ttl(SandboxSection::effective_host_lease_ttl())`.
- The key is profile-scoped and security-critical (`ProfileReplaces`, like
  `sandbox.execution`). Home and profile set it. An untrusted project
  layer cannot set it, so a cloned repository cannot lengthen host
  phases. This needs the `FIELD_TABLE` row and the `merge_sandbox` entry
  listed in §3; without them a layer file cannot set the key.

### 2.3 Suspend-safe clock

Instants are `LeaseInstant`s. Each one holds two readings of the same
moment:

- a monotonic reading (`Instant`, which stands still during suspend);
- a reading that counts suspend: `CLOCK_BOOTTIME` on Linux and Android,
  the wall clock elsewhere.

A deadline counts as reached as soon as **either** clock reaches it. An
8-hour phase therefore ends after 8 hours of real time, even if the
machine slept in between. Setting the wall clock back extends nothing,
because the monotonic reading still bounds the window. `LeaseInstant`
carries no calendar time. Surfaces therefore show the remaining
**duration**, never an end time.

### 2.4 Ledger permits are bound to the window

A permit issued under a running phase must not outlive it.

- `ProcessPermitLedger::issue_for_lease(request, window)` issues the
  permit. Its `expires_at` is the earlier of `now + lease_ttl` and the
  window end, compared per clock. An expired window yields
  `ProcessPermitError::Expired`. It never yields a fresh full-length
  permit.
- `shell.exec` (`ShellExecutor::authorize_host_command`) reads the window
  once with `covering_lease(session)` and issues through
  `issue_for_lease`. It does not first check `is_session_approved` and
  then issue a free-standing permit.
- After a fresh approval, the executor records the process-wide phase
  (`mark_global_approval`), reads the covering window and binds the
  permit to it. If no active window covers the session at that point, no
  permit is issued (fail-closed). The shared helper `issue_and_remember`
  does this for every `SessionLease` caller, including the `request_host`
  escalation path.
- The asking session gets no window of its own. Before this decision the
  prompt path also called `mark_session_approved` for the asking (often
  child) session. `Ctrl+H` clears only the root session and the global
  phase, so such a child window survived it, and the child kept running
  host commands without asking for up to the full lease. The global
  window already covers every session of the process.
- A remembered lease-bound permit is not returned by `lookup_permit` once
  no phase covers its session. The ledger entry itself has expired by then
  as well.

A remaining time (`lease_remaining`) is never used as a permit `ttl`. A
`None` there would mean "full `lease_ttl`" and fail open.

### 2.5 Surfaces

- An expired window stays stored until it is revoked or replaced
  (`session_lease`, `global_lease`). It has no effect on authorization.
  It only lets surfaces say "expired".
- `/sandbox-lease status` reports one of four states, in this order:
  - "aktiv, noch … (danach neue Freigabe nötig, unter Full Access ohne
    Rückfrage …)", with the remaining time rounded up to whole minutes;
  - "Einmalfreigabe offen";
  - "abgelaufen";
  - "aus".
- `/sandbox-lease request` treats an expired global phase as absent and
  asks again. A grant reports the remaining time.
- The model-tool summary of `sandbox-lease` names the expiry (default 8 h).
- TUI: the status mark (`host_mode_active`) disappears as soon as the
  phase ends.
  - `ChatApp::end_host_mode` (`Ctrl+H`) cleans up an active phase **and**
    an expired one that is still stored. Before this decision it returned
    early once the phase had expired and left the state behind.
  - `ChatApp::poll_host_lease_expiry` runs at the start of every round of
    the TUI event loop (`run_loop`). For a stored but expired phase it
    removes the windows, emits a one-time "Host work phase expired —
    isolation is active again." line and requests a redraw. A pending
    process-wide single-use approval survives this automatic cleanup
    (`Ctrl+H` still ends it).
  - A timer arm of the event loop wakes the loop after
    `ChatApp::host_lease_poll_delay`: the remaining time, capped at 30 s,
    because tokio timers do not count suspend.

## 3. Consequences

- **Long host sessions ask again.** After 8 hours (or the configured
  value) the next host command shows the host-permit dialog again.
  Expiry is checked when a command is authorized. A host process that is
  already running is not stopped.
- **Full Access re-grants automatically.** Under Full Access the dialog
  is not shown: the TUI answers the new prompt and a new window starts.
  The user sees the "host work phase approved without asking" line
  again. B22 under Full Access is therefore not reduced by expiry (see
  §4).
- **Configurable within a hard bound.** Users who need longer phases set
  `[sandbox] host_lease_ttl_secs` in their home or profile configuration,
  up to 7 days. Nothing can make a phase unbounded again.
- **Tests change.** Tests that asserted "no expiry" were rewritten. In
  `harw-tool-shell`, the old `..._does_not_expire_until_user_revokes` test
  became expiry tests. They do not sleep: they use windows that ended in
  the past and the `_at` variants. In `harw-ops`, the `/sandbox-lease
  status` text test now expects a remaining time. New tests cover the
  window-bound permits, the asking child without a window of its own, the
  configuration bounds, the "abgelaufen" status and the TUI cleanup.
- **Design documents are superseded on this point.** The passages listed
  under *Amends* said "no expiry". This record supersedes them; the
  companion changes below update them to point here.
- **Companion changes outside the first change.** The following must
  land together with it:
  - the `FIELD_TABLE` row (`harw-config/src/scope.rs`), the
    `merge_sandbox` entry (`harw-config/src/merge.rs`) and the
    exhaustive-field test (`harw-config/tests/config_scope_exhaustive.rs`)
    for `sandbox.host_lease_ttl_secs`;
  - the translated `app_notices.host_lease_expired` text;
  - the remaining time in the `/status` host-lease line
    (`harw-ops/src/status.rs`);
  - the passages listed under *Amends*, `config-scopes.md`,
    `config-structure.md`, the ADR index in `docs/README.md` and the
    remaining "no expiry" code comments (`exec/escalation.rs`,
    `host_permit_prompt.rs`).

  Without the merge entry, the key has no effect: every layer file is
  merged through `merge_layer_into`, so the runtime always uses the
  default of 8 hours. Without the exhaustive-test update the
  `harw-config` integration tests do not compile. Without the
  translation every language shows the English expiry line. The
  authorization effect of expiry does not depend on any of them.

## 4. Security

- **B22 is narrowed, not closed.** Outside Full Access, an agent can no
  longer rely on a forgotten host phase from a previous day. Within a
  running window it still uses host tools without asking. The taint rule
  (PL-100 decision O1) and `ALWAYS_ASK` for `job.*` address that
  separately.
- **Full Access is not bounded by expiry.** Under Full Access the TUI
  re-grants an expired phase on the next prompt without a human. The
  taint rule applies only outside Full Access (O1). B22 under Full Access
  therefore stays an accepted residual risk of the mode the user chose.
- **`Ctrl+H` ends host access for every session.** Approval opens only
  the process-wide window, which `Ctrl+H` revokes. No child session keeps
  a window of its own.
- **No fail-open paths.** An unrepresentable window end is not recorded,
  so no phase exists. A missing covering window after an approval means
  no permit. A poisoned registry or ledger lock reads as "no phase" or
  "not authorized".
- **No silent renewal.** Neither the model nor repeated approvals extend
  a running window. Outside Full Access a new window needs a new human
  decision after expiry. Under Full Access the standing mode choice is
  that decision (see above).
- **Untrusted configuration cannot lengthen phases.** The key is
  profile-scoped. The parser rejects values above 7 days, and
  `effective_host_lease_ttl_secs` clamps defensively.
- **Clock manipulation.** Setting the wall clock back does not extend a
  phase, because the monotonic clock bounds it. Suspend does not extend a
  phase, because `CLOCK_BOOTTIME` bounds it.
- **Residual race.** The TUI expiry cleanup may end a phase that the user
  approves in the same instant. It then reports "ended". This is
  fail-closed and visible. A single-use approval granted in the same
  instant may likewise be lost (fail-closed).

## 5. Alternatives considered

- **Keep the 2026-09-24 decision (no expiry).** Rejected. B22 shows that
  an open-ended process-wide phase gives a compromised agent host access
  for as long as the process lives.
- **Idle timeout that renews on use.** Rejected. The agent itself would
  keep the phase alive by using it. Renewal must come from a human
  decision.
- **Wall clock only.** Rejected: setting the clock back would extend a
  phase.
- **Monotonic clock only.** Rejected: suspend would extend a phase by the
  time the machine slept.
- **Clip permits with `ttl = lease_remaining(..)`.** Rejected. An expired
  phase yields `None`, and `None` means a full-length permit (fail-open).
  Binding to the `LeaseWindow` through `issue_for_lease` cannot fall back
  that way.
- **Shorter default (1 h).** Rejected as a default for usability. The
  configuration key allows it.
- **Stop running host processes at expiry.** Rejected for this change.
  Expiry governs authorization, and stopping processes belongs to
  `job.*` and `process.kill`, with their own approval rules.
