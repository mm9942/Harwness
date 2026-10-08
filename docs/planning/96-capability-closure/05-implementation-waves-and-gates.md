# PL-96 / 05 — Implementation waves, ownership, tests, gates and rollback

> All waves are **proposals**, not code delivered in this planning PR. Every wave needs a separate branch/worktree, a bounded diff, a reviewer and an exact-SHA verification record. Avoid running Cargo builds concurrently across waves sharing a target directory.

## Gate hierarchy

**G0 Evidence:** pin `dev` HEAD, main HEAD, baseline merge-base and source map; source/consumer trace plus current-vs-planned statuses. No build claims.

**G1 Clean checkout:** ensure every Cargo member exists on disk *from Git*, no stub manifests; `Cargo.lock` conflict markers absent; `cargo metadata --locked --no-deps`. Check any private/local-only project references explicitly, don't silently fabricate packages.

**G2 Cheap syntax/contracts:** `cargo fmt --all --check`; plan links/path lint; targeted JSON/TOML/MD checks; `cargo xtask gates arch` or exact CLI subcommand confirmed from `xtask`. Run as supported by verified tools; no false positives from unknown commands.

**G3 Focused behavior:** package-scoped `cargo test -p <package> ...`, pure unit/property tests, intentional deny fixtures, mock external providers. Tests must read the actual production code path, not a duplicate policy implementation.

**G4 Architecture/security:** `cargo xtask gates all` (or documented canonical command verified before execution), Warden privileged dependency/no-C-build gates, deny check, affected CI matrix, source-level unsafe rule, child authority checks. No new Warden dependencies without explicit TCB review.

**G5 Integration/faults:** disposable E2E run with real runtime composition, crash/resume, tenant isolation, revocation, denied/unavailable permissions, provider adapter and network fixtures. Report environment limitations, skip reasons and exact head.

**G6 Rollout:** staged operational evidence + rollback thresholds only where needed (Cloudflare, gateway, Telegram, proxy, Warden); never interpret passing tests as deployment.

## Wave inventory

| Wave | Priority | Single primary owner / files | Output and explicit pass criterion |
|---|---|---|---|
| W00 | P0 | release/cargo; `main/Cargo.lock`, root `Cargo.toml` and tracked cloud package inventory | main lock valid; dev clean checkout with real members, no stubs; `cargo metadata --locked` green |
| W01 | P0 | DoD Warden TCB; `dod/crates/harw-dod-warden{,-proto}`, Escalator + privileged binary | v2 signed request on actual enforcement call path; no v1 executable ingress; nonce persists across restart; failed proof never executes |
| W02 | P0 | provider/context; `harw-provider-http/src/anthropic.rs` and shared wire fixture tests | `data_block` retained correctly across provider transport schemas; zero duplicate/untrusted elevation |
| W03 | P1 | `harw-catalog`, `harw-core`, `harw-context`; no registry file ownership | enabled triggered skill fragments delivered within budget at model call; exact source and omission recorded |
| W04 | P1 | `harw-registry-defaults`, native tool crates and `harw-agent-runner`; one scribe for common registry files | all 42 supported tools are actually registered under intended profiles; allowed/denied model calls and bwrap Cargo path verified |
| W05 | P1 | effective discovery + `harw-ops`/CLI; dependent on W04 | `tools.search/list/describe` reflect real effective provider and grants, not static catalog alone |
| W06 | P0 before Telegram merge | operations grammar/policy + Telegram channel adapter; dedicated reviewer | nested `/dream review ... accept` denied under read-only channel policy, safe review works; normal TUI unaffected |
| W07 | P1 | `harw-session-host`, `harw-session-com`, `harw-session-daemon`, `harw-cli` compose | shared owner/lease proposed then implemented in narrow stages; two clients same durable session, no remote rights inheritance |
| W08 | P1 | ToolHost composition and Gateway sandbox; depends on W07 and W05 | scoped tool.list/tool.call on authorized agent; disabled host refuses; revocation and approval binding tested |
| W09 | P1 | `harw-runtime` memory/diary hooks, `harw-ops` learning, job store | active session produces idempotent scoped milestone proposal and eventual approved retrieval; no global private memory |
| W10 | P1 | review/CI/branch ledger and port fixes; avoid overlapping W00 edits | #88–#142 unique work inventoried, post-merge verification debt tracked; no lost branches |
| W11 | P2 | pattern projections / TUI / AgentTree; depends on event semantics from W07/W09 | live bounded projection preserving raw event IDs; correct waiting/failure statuses |
| W12 | P2 | Cloudflare Worker, HTTP caching and telemetry; explicit deploy signoff | cache keys/lanes verified and staged rollout; billable cost and failure thresholds monitored |
| W13 | P2 | paper and benchmark artifact harness; independent reviewer | reproducible experiments and per-claim verification/CI status on pinned SHA |

Some W00–W02 and W06 security work may proceed in separate disjoint worktrees, **but release integration is gated sequentially**. W04–W05 share registry files and must be serialized or one explicitly owned integration worktree. W07/W08 share SessionHost/Com and must be layered.

## P0 negative tests, minimum

**W00:**
- Checkout fresh `main` and fresh `dev`, no local/untracked stubs.
- Conflict markers must fail gating at all branches, not only `main`.
- Every `members = [ ... ]` package has a genuine tracked `Cargo.toml`; do not silently alter member list.
- Re-running metadata with `--locked` does not modify Cargo.lock.

**W01:**
- v1 unsigned proof → reject and no side effect.
- forged v2 signature → reject without consuming nonce.
- stale/replayed proof → reject across restart.
- changed target action/cgroup/TTL/issuer → reject.
- concurrent identical signed command → at most one execution.
- audit record follows committed accepted action; no Warden dependency into browser/models/application layer.

**W02:**
- Same logical fixture in Anthropic/OpenAI Chat/Responses contains the exact data block in an explicitly permitted data/user location.
- Empty data block adds nothing; no duplicate injected data.
- Tool results keep untrusted envelope; chosen tokens/effort/markers not broken.
- A context section denied by a ceiling is absent, with omission event.

**W06:**
- Telegram read-only `/dream review` succeeds only with underlying rights.
- Read-only `/dream review <id> accept <proposal>` rejects (also through `/op`).
- TUI permitted behavior remains unchanged; no Owner principal fabricated.
- A menu entry is not an authorization mechanism.

## Owner decision and release blocking

Critical **P0** issues must not be waved through by a high model confidence score or a human review comment without technical evidence. For Warden enforcement, the default is **no privileged rollout** while v1 is present on a reachable enforcement path. For main publishing, require a clean lockfile and clean checkout with tracked package members. For Telegram, avoid exposing generic remote write commands until W06.

Use a consistent release evidence record:

```yaml
wave: WXX
target_branch: <branch>
baseline_sha: <40-hex>
head_sha: <40-hex>
files_owned: []
source_paths: []
contract_tests: []
negative_tests: []
gate_runs:
  - command: <exact command>
    environment: <runner details>
    status: passed | failed | skipped
    log_url: <URL>
remaining_risks: []
rollback: <precise steps>
reviewer: <independent reviewer>
owner_approval: pending
```

A skipped command means **not verified**; no invented green checkmarks.

## Rollback boundaries

- Tool registry: disable new provider registration by config/feature while retaining old tool names and existing shell governance.
- Context: revert targeted adapter path without changing model/tool authority.
- Skill injection: fail closed to existing pull-based `skills.search/load`, keep source manifests.
- Remote ToolHost/Cloud Home: revert to separate local and remote hosts without ever sharing storage unsafely; terminate stale owner leases.
- Warden: **do not roll back to accepting unsigned v1** on privileged ingress; keep privileged operations disabled until fixed.
- Telegram: disable registry-projected remote operations, retaining established paired native channel functions.
- Cache/proxy: restore known-good deployed Worker/proxy revision, preserve tenant partitioning and inspect billing/429 telemetry.

## Review checklist for every implementation PR

- [ ] Baseline and head SHA pinned; producer and consumer code reviewed.
- [ ] Distinguish defined, registered, admitted, executed and deployed.
- [ ] No hidden authority widening or shell/host escalation fallback.
- [ ] Negative tests target the **actual** production composition path.
- [ ] All file ownership and branch/worktree dependencies explicit.
- [ ] Focused gates and central CI on exact head recorded, or explicitly skipped.
- [ ] No untracked build stub, shadow implementation, unapproved external deployment.
- [ ] Rollback, compatibility, state migrations and unresolved owner decisions documented.
