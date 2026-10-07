---
id: COPILOT-BACKLOG
title: Copilot backlog (GitHub Copilot coding agent, Sonnet)
status: open
tags: [backlog, copilot]
related:
  - ../../../.github/copilot-instructions.md
  - ../70-decisions/README.md
  - ../90-migration-ledger/MIGRATION_LEDGER.md
---

# Copilot backlog

Small, self-contained tasks that the GitHub Copilot coding agent (model
Sonnet) can complete on its own. Each task becomes its own issue, assigned to
Copilot. Each issue leads to a PR against **`dev`**.

Rules for all tasks: [`.github/copilot-instructions.md`](../../../.github/copilot-instructions.md).
The task texts below are in English and are used as the issue text.

Order: C-01 to C-05 are independent of each other. C-07 requires
R14 to be on `dev`; C-06 and C-09 are done in R15. C-08 is only
a proposal, no code.

## Focus for Copilot: branches and README

Copilot mainly takes care of branch maintenance and the README. Code rounds
continue to run through the local orchestrator with a single central build.

| ID | Task |
| --- | --- |
| G-01 | Maintain `dev` as the stable integration branch: PRs from rounds against `dev`, green states only |
| G-02 | Close superseded stacked PRs (#25, #26) once their content is in `dev`, with the note "merged into dev" |
| G-03 | List merged and orphaned `claude/*` and `copilot/*` branches and delete them after confirmation |
| G-04 | Update the root `README.md` (see below) |
| G-05 | Prepare the release PR `dev` → `main` when a state is to be released |

### G-04 — Update the root README.md

**Why:** the README describes the state before R11. Since then the project has
gained rings and the arch gate, the job family, the KMS/auth hub, the work
driver and the decision vault.

**Do:** update `README.md`:
- **Short architecture overview.** Rings F/I/C/J/D/A and what lives where;
  link `docs/architecture/`.
- **Work driver.** One paragraph; link `docs/guides/work-driver.md` and
  `docs/planning/70-decisions/`.
- **KMS/auth hub.** One paragraph; link `docs/guides/kms-operations.md`.
- **Branch model.** `dev` is the integration branch, `main` is releases, one
  PR per round or task.
- **Build and check commands.** The same list as
  `.github/copilot-instructions.md`.

Keep the logo and the existing tone. Don't touch the Getting-Started steps
beyond what is wrong. No book titles, authors or quotes.

**Done when:** every link resolves and no statement contradicts the code.

## Code tasks (optional)

| ID | Task | Size |
| --- | --- | --- |
| C-01 | Remove expired arch exceptions (session-store → job-store) | medium |
| C-02 | `webui/lib/generated/operations.ts` regenerate | small |
| C-03 | Update architecture docs after R14 | small |
| C-04 | `HARW-DRIVER-006`: WorkDriver tool without `[work_driver]` | small |
| C-05 | Guard the `driven-orchestrator` example with a test | small |
| C-06 | Provider pacing hook for TPM limits — done (R15) | medium |
| C-07 | Auth hub: connection lifetime test | small |
| C-08 | Proposal DEC-009: path-level write rights in the sandbox | text only |
| C-09 | Wire a sandboxed verify runner into the work driver — done (R15) | medium |
| C-10 | Proposal DEC-010: "cloud home" for ephemeral containers | text only |

---

## C-01 — Remove expired arch exceptions (session-store → job-store)

**Why:** `xtask/arch-policy.toml` still has two `[[exceptions]]` from
`harw-session-store` (ring I) to `harw-job-core` / `harw-job-store` (ring J),
marked `until = "R11 W2"` and `until = "R12"`. Both are expired.

**Do:**
- Move the job persistence adapter (`harw-session-store/src/job_store.rs`,
  `JobStore`) into `harw-job-store`, or behind a trait that `harw-job-store`
  implements, so that `harw-session-store` no longer depends on any J crate.
- Update all call sites (grep `harw_session_store::JobStore` / `job_store`).
- Delete both exceptions from `xtask/arch-policy.toml`.

**Done when:** `cargo run -q -p xtask -- gates` is green without the
exceptions, behaviour is unchanged (existing job tests pass unmodified), and
no new public third-party types appear.

## C-02 — Regenerate `webui/lib/generated/operations.ts`

**Why:** the generated operation table is stale. The `infra.*` routes are
missing, `analyze` still shows GET, and the three `work_driver.*` routes
(`/api/work-driver/enqueue|status|stop`) are absent.

**Do:** run the generator in `xtask` (see `xtask/src/webui.rs` for the exact
subcommand) and commit only the regenerated file. If the generator itself is
wrong, fix it in `xtask/src/webui.rs` and add a test there.

**Done when:** the file matches the registry (there is a drift check in
`xtask` tests; make sure it passes).

## C-03 — Update architecture docs after R14

**Why:** R14 removed two dependency inversions: `harw-agent-compiler` →
`harw-registry-defaults` (now an install slot, `BuiltinDefaults`) and
`harw-config` → `harw-agent-dsl` (raw `agent_sources`, parsed in
`harw-registry-defaults/src/config_agents.rs`).

**Do:**
- `docs/architecture/harw-dependency-inversions.md`: mark R1 resolved, with how
  it was done.
- `docs/architecture/harw-workspace-inventory.md`: fix the internal and reverse
  dependency lists for `harw-agent-compiler`, `harw-registry-defaults`,
  `harw-config`, `harw-ops`, `harw-runtime` against the current `Cargo.toml`
  files.

**Done when:** every row matches the manifests. You can check this by script;
describe the check in the PR.

## C-04 — Diagnostic `HARW-DRIVER-006`

**Why:** an agent definition can list `work_driver.enqueue` (or
`.status`/`.stop`) in `[tools]` without a `[work_driver]` section. It then
silently loses the tool.

**Do:**
- Add `HARW-DRIVER-006` to `harw-agent-dsl/src/diagnostics.rs` (the `codes`
  module and `CATALOG`), same style as 001–005, severity Error.
- In `harw-agent-dsl/src/lower_v2.rs`, report it when `work_driver` is `None`
  and `tools.admitted` contains any of `WORK_DRIVER_TOOLS`.
- Add tests (lowering reports it; with the section there is no diagnostic).
  Also update the diagnostic-code catalog tests.

**Done when:** `cargo test -p harw-agent-dsl` is green.

## C-05 — Guard the `driven-orchestrator` example with a test

**Why:** `examples/agents/driven-orchestrator/definition.toml` is the reference
WorkDriver definition and has no automated check.

**Do:** add a test (in `harw-registry-defaults/tests/` or
`harw-agent-compiler/tests/`, whichever already compiles example definitions)
that lowers it and asserts:
- no error diagnostics;
- `work_driver` is `Some` and has the expected roles;
- the three `work_driver.*` tools survive the roster clamp.

**Done when:** the test fails if someone removes `[work_driver]` or breaks the
roles.

## C-06 — Provider pacing hook for TPM limits (after R14 is on `dev`)

**Status:** done in R15 (`ModelProvider::pacing_wait`, HTTP providers via `ProviderRateLimiter`, pause in the work driver before each wave chunk; see DEC-003).

**Why:** the work driver's job worker can't reach
`ProviderRateLimiter::pending_wait` / `wait_for_slot`
(`harw-provider-http/src/rate_limiter.rs`) through `dyn ModelProvider`. It
only reacts to HTTP 429 and can't pace against tokens per minute in advance.
See `docs/planning/70-decisions/DEC-003-provider-limits.md`.

**Do:**
- Add a defaulted method to the `ModelProvider` trait, e.g.
  `fn pacing_wait(&self) -> Option<Duration> { None }`, with no third-party
  types.
- Implement it for the HTTP providers from their `ProviderRateLimiter`.
- In `harw-cli/src/job_worker_work_driver.rs`, await it before starting a wave.

**Done when:** a unit test with a fake provider that reports a wait delays the
wave. Existing providers compile unchanged thanks to the default.

## C-07 — Auth hub: connection lifetime test (after R14 is on `dev`)

**Why:** `harw-auth-hub/src/server.rs` closes connections gracefully after
`CONNECTION_MAX_LIFETIME` (60 s), with a `CONNECTION_CLOSE_GRACE` of 5 s.
There is no test.

**Do:** make both durations configurable through the existing serve config or
a test-only builder, without changing the defaults. Then add to
`harw-auth-hub/tests/hub.rs`:
- an in-flight request that finishes even when the lifetime expires during it;
- the connection closes afterwards.

**Done when:** the test runs in under 5 s with shortened durations.

## C-08 — Proposal DEC-009: path-level write rights in the sandbox (text only)

**Why:** WorkDriver workers may only write their scope's `owned_paths`. Today
that is checked after the fact (snapshot + `validate_patch`) and escalated,
not prevented. See `docs/planning/70-decisions/DEC-007-worker-rights.md`.

**Do:** write `docs/planning/70-decisions/DEC-009-path-level-writes.md` with
status `proposed`. Cover:
- options via Landlock path rules, bwrap bind mounts and the fs tool layer;
- how `PermissionSet` / `SandboxSpec` would carry per-path write rights;
- the migration path.

No code changes.

**Done when:** the note follows the format of the other decision notes and
links DEC-004, DEC-006 and DEC-007.

## C-09 — Wire a sandboxed verify runner into the job worker (after R14 is on `dev`)

**Status:** done in R15 (`harw-cli/src/verify_sandbox.rs`, sandboxed verify runner in the work driver job; without a runner `fallback_verifier` remains).

**Why:** the work driver job (`harw-cli/src/job_worker_work_driver.rs`) can't
reach the sandboxed `CoordinatorVerifyRunner`
(`harw-plan-bridge/src/verify_exec.rs`). It falls back to `NoSandboxRunner`:
every Command step is unverifiable, and a run escalates at its first Verify.
Commands never run unsandboxed.

**Do:**
- Add `verify_runner: Option<…>` to `JobWorkerContext` in
  `harw-cli/src/job_worker.rs`. Build the coordinator at the job worker's
  composition root: a `harw_job_runtime::Coordinator` with `FsJobRecordStore`
  (add the `harw-job-store` dependency to harw-cli) and a `LinuxExecutor`
  (Landlock trampoline via `harw-job-exec`, or bwrap), with the workspace root
  set to the runtime cwd.
- `VerifyRunner` uses an async fn, so forward to the `Arc` through a small
  newtype.
- In the work driver: use the runner when present; keep `fallback_verifier`
  otherwise.

**Done when:** a test with a fake coordinator runs a Command step to Passed,
and without a runner the escalation behaviour is unchanged.

## C-10 — Proposal DEC-010: "cloud home" for ephemeral containers (text only)

**Why:** Harwness (and its work driver workers) will run in ephemeral cloud
containers, like Claude Code on the web or Copilot's own environment. Such a
container is reclaimed when idle, has a fixed disk quota and a network policy,
and gets secrets injected. `harw-home` (`~/.harw`, profiles, scaffolding) only
knows a long-lived local home today.

**Do:** write `docs/planning/70-decisions/DEC-010-cloud-home.md` with status
`proposed`, following the format of the other notes. Cover:
- what must persist outside the container: job store, KMS sealed store plus
  its KEK credential, knowledge and transcripts, and how;
- a declarative environment profile: setup script, allowed egress hosts,
  secret references (never values), disk-quota-aware build settings
  (`CARGO_INCREMENTAL=0`, no debug info);
- how a `harw` profile selects it (`HARW_PROFILE`);
- idle and reclaim handling for running jobs (leases, restart recovery);
- the mapping to `.github/workflows/copilot-setup-steps.yml` as the Copilot
  counterpart.

Link DEC-003, DEC-004 and DEC-005. No code.
