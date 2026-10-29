# PL-97 review fixes — work split between the Claude session and the Codex helper

Status: IN IMPLEMENTATION (coordination document, removed or folded into the
ledger in PL-97 X1).

PR #154 carries PL-97 (`ccr-54aa3dfd-o46he2`). Its review produced 16 threads.
The owner asked the Codex helper to take over the fixes that do not collide
with the zones the Claude session is still building. This file is the single
place that says who fixes what, on which branch, and how the fixes come back
into #154.

## 1. How this works

1. **Branch.** The helper pushes fixes as commits to `pl97/review-fixes`, the
   branch of the draft PR that adds this file (base `ccr-54aa3dfd-o46he2`).
   One commit per item, subject `fix(<crate>): <short title> (PR #154 review, H<n>)`.
   Never rebase or force-push this branch; merge `ccr-54aa3dfd-o46he2` into it
   when you need newer code.
2. **Scope.** Only the files named for the item. If a fix needs a file from
   section 4 ("in flight"), stop and say so in the PR instead of editing it.
3. **Proof.** Every item adds a regression test that fails before the fix.
   Before pushing, run for each changed crate:
   `cargo fmt -p <crate> -- --check`,
   `cargo clippy -p <crate> --tests -- -D warnings` and
   `cargo test -p <crate>`. Put the commands and results in the commit body.
4. **Report.** Reply on the PR #154 thread with the commit SHA. Do not resolve
   the thread; the Claude session resolves it after the merge into #154.
5. **Integration.** The Claude session merges `pl97/review-fixes` into
   `ccr-54aa3dfd-o46he2` after each batch, re-runs the gates and resolves the
   threads. Conflicts with in-flight zones are resolved on the Claude side.

## 2. Items for the Codex helper

| Id | Sev | Thread (PR #154) | Files | What "done" means |
|---|---|---|---|---|
| H1 | P1 | `harw-dod-client/src/findings.rs:210` | `harw-dod-client/src/findings.rs` (+ tests) | `complete = true` only with full, stable coverage of the export generation (or a validated append-order contract). An out-of-order old timestamp after unread recent findings and a rotation between the two opens both stay partial. Tests for both. |
| H2 | P2 | `harw-dod-client/Cargo.toml:21` | `Cargo.toml` (workspace `rust-version`) or `harw-dod-client/Cargo.toml` | The declared MSRV builds `time` 0.3.53. Either raise the workspace `rust-version` to at least 1.88 (check that no other crate claims a lower MSRV on purpose) or pin a `time` version that supports 1.85. Say which in the commit. |
| H3 | P1 | `dod/crates/harw-sentinel/src/main.rs:936` | `dod/crates/harw-sentinel/src/{main,health_export,export}.rs` | Findings and health export destinations are compared by resolved identity: non-normal components (`.`, `..`) are rejected, trusted parents resolved, canonical parent + basename compared before either writer starts. Tests for a dot-component alias and a symlinked-parent alias. |
| H4 | P1 | `harw-home/src/bundle.rs:1001` | `harw-home/src/bundle.rs`, files vendored under `harw-home/` | No `include_str!` escapes the `harw-home` package. Vendor the four tool-crate skill files under `harw-home/assets/skills/` with a drift test against the originals, and show that `cargo package -p harw-home --allow-dirty --no-verify` lists them (or that `cargo package -p harw-home --allow-dirty` builds). |
| H5 | P2 | `harw-tool-job/src/model.rs:617` | `harw-tool-job/src/model.rs` (+ tests) | `cgroup_path()` returns a dedicated job cgroup only with proof (runtime-attested attempt identity plus a delegated root, or an explicit persisted proof); a shared leaf that merely matches `harw-job-*` and forged metadata yield `None`. Do not edit `manager.rs` or `launcher.rs`. |
| H6 | P2 | `docs/setup/dod.md:270` | `docs/setup/dod.md` (only the security-hub paragraph) | The documented hub setup names the required `[findings] path = "/var/lib/harw-dod/export/findings.jsonl"` and `[server] host = ...` before the drop-in and restart. |
| H7 | P1 | `docs/planning/97-.../README.md:1462` | `harw-core/src/skill_injection.rs` (state snapshot), `harw-core/src/session.rs`, `harw-core/src/state_store.rs`, `harw-session-store/src/injection.rs`, `harw-runtime/src/assembly.rs` (injector construction only) | The injector's active excerpts, dedupe state and charged session budget survive a restart of the same `SessionId` (versioned snapshot or deterministic reconstruction from the injection records), hydrated before the next turn. Test: trigger, restart, non-matching `continue`; the active excerpt and the charged budget are preserved. |
| H8 | P2 | `harw-core/src/skill_injection.rs:459` | `harw-catalog` (`SkillSource`), `harw-core/src/skill_injection.rs`, `harw-runtime/src/assembly.rs` | Imported provenance travels through `SkillIndex` and the assembly seed, so `[skills] inject_imported = false` suppresses imported entries in production. Assembly-level test: default suppressed, opt-in delivered as `Evidence`. |
| H9 | P2 | `harw-core/src/skill_injection.rs:960` | `harw-core/src/skill_injection.rs` (`collect_string_leaves`, `head_and_tail`) | The tool-result scan is bounded while traversing: bounded head/tail accumulators, explicit node and depth limits. Regression with millions of one-byte leaves and a deeply nested value proves bounded work. |
| H10 | P2 | `harw-core/src/turn_loop.rs:6128` | `harw-core/src/turn_loop.rs` (delivery confirmation), `harw-core/src/skill_injection.rs` (`confirm_delivered`) | A delivery is acknowledged only after its injection event is durably appended; a failed append stays pending and is retried idempotently. Fail-once `StateStore` test. |

H7–H10 touch `harw-core/src/skill_injection.rs`. Two Claude zones also add
small, separate functions there (B3: a shared preview helper; B2: a
tool-availability filter). Keep your edits to the functions named above; the
Claude session resolves any textual conflict when it merges.

## 3. Already fixed

| Thread (PR #154) | Fix |
|---|---|
| `harw-dod-client/src/status.rs:328` (Warden reported enabled while unknown) | Codex `5bb503bc`, picked as `64d3831c` |
| `harw-activity/tests/replay.rs:29` (untracked fixtures) | `a7111029` |
| `harw-activity/src/reducer.rs:619` (cancelled rebases) | Codex `e9443bc0`, picked as `ff7ca74a` |

The security review in PR #156 (findings 2, 3 and 6) is merged as `c471013f`.

## 4. In flight on the Claude side (do not edit)

| Zone | Files |
|---|---|
| C2, later C3/C4 (TUI) | `harw-tui/**`, `harw-tui-layout/**` |
| D3 `harw dod`, then B2 tool index | `harw-cli/src/{cli/*,dod_cmd.rs,tool_cmd.rs,lib.rs}`, `harw-ops/src/{dod.rs,tool_index_op.rs,lib.rs}`, `harw-registry-defaults/**`, `harw-instructions/src/baseline.rs`, `harw-runtime/src/security_signals.rs`, `harw-security-hub/src/findings.rs`, `docs/cli.md` |
| B3 `harw skill` | `harw-cli/src/{cli/skill.rs,skill_cmd.rs}`, `harw-ops/src/skills.rs` |
| E1b sandbox chain, then E2/E3 | `harw-job-*/**`, `harw-command/**`, `harw-sandbox/src/bwrap.rs`, `harw-cli/src/verify_sandbox.rs`, `harw-tool-shell/**`, `harw-tool-job/src/{manager,launcher}.rs`, `harw-agent-runner/**`, `harw-config/**`, `harw-install/**`, `deploy/**`, `Makefile`, `scripts/**` |
| E3c probe wiring | `dod/crates/harw-probe-bpf/**`, `dod/crates/harw-dod-bpf/**` |

These three #154 threads are fixed inside those zones:
`harw-tools/src/provider_macro.rs:511` (activity metadata after registration,
WP M3), `docs/design/dod-system-operations-authority.md:413`
(`permission_request!` call site, WP D3) and
`dod/crates/harw-probe-bpf/src/scope_rescan.rs:231` (per-handle reconcile,
WP E3c).
