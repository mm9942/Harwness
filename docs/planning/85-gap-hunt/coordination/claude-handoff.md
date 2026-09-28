# Handoff to Harw: consolidated Claude state, mapped onto `r16-dev-integration`

This note makes Harw the only writer for R16 integration. From here I do not push to `claude/r16-integration` or to any wave branch, and I implement no blocker. I review on request only. Everything I still hold is listed below, sorted by your plan node.

### `baseline`: pin check
- `origin/dev` is now `79b471d`, one commit past your pin, from the merge of #33 (`claude/r15-wip-snapshot`). Its tree is **identical** to `1ad9021`: `git diff 1ad9021 79b471d` is empty. The pin holds, and both SHAs describe the same CURRENT.
- `merge-base(dev, r16-integration) = 1ad9021`. Integration is 24 commits ahead, and `git merge-tree` against `dev` reports no conflicts.

### `blocker-a` / `blocker-b`: no code from me
I did not start either one; there is no local diff and no branch. These are the entry points I had mapped:
- **A:**
  - `dod/crates/harw-dod-warden/src/warden.rs` (`Warden::handle` → `request.proof.verify`) and `lib.rs`
  - `harw-warden/src/{ipc,protocol,warden_factory}.rs`
  - `dod/crates/harw-dod-warden-proto/src/{proof,signed}.rs`
- **B:**
  - `harw-ops/src/sandbox_lease.rs::handle_revoke`, which sweeps only the caller's session in the ledger.
  - `harw-sandbox/src/process_permit.rs`, which has `revoke_session` but no `revoke_all`.
  - `harw-sandbox/src/host_permit_session.rs`: `remembered` is never cleared by `revoke_global_approval`, so `lookup_permit` still hands a child its old id.
  - Both halves (ledger and `remembered`) must be cleared. Clear the registry flags first, so a racing lookup fails closed.

### `baseline-contract` (D)
The `claude/r16/pr-baseline` worktree sits at `a856dea` with **no changes**, because its agent died at the session limit. Nothing is in flight.

### `ripple`: open, not integrated
| Item | State |
|---|---|
| `ripple-web` (9 items from `wa-web`) | Not done. The worktree is at `72fb2ab`, and its review and repairs died at the session limit. |
| `r16-w1` … `r16-w8` one-file waves | **Unreviewed and unbuilt.** 95 dirty files in the main checkout, preserved as a commit on `claude/r16/wip-snapshot` (`bbd603b`). Some fixers ran in the wrong checkout (catalog P14), and the repairs had no re-review. Treat this as input to re-review against the base, not as a result. Rough file counts: w1 16, w2 14, w3 12, w4/w4-2 12, w5 14, w6 13, w7 14, w8/w8-2 15 (some files appear in more than one wave). |
| `contract-c` (c15, c28, c31, c35) | Pushed, **not merged**: `6893f33` with its manifest. It overlaps the w-waves on `harw-runtime/src/assembly.rs` and `harw-core-bridge/src/agent_tool.rs`. |
| E: route-level resolver tests | Not started. |

### `freeze`: manifests
There are immutable manifests for the 9 merged waves plus `contract-c`, under `docs/planning/85-gap-hunt/waves/`. As proposed in the review, the final build should write its own manifest keyed by the frozen SHA.

### Wave 2 (outside this goal; recorded, not started)
- **euid source:** `harw-project-discovery/src/discovery.rs` and `harw-sandbox/src/bwrap.rs:1273-1290` read the owner of `/proc/self`; use `rustix::process::geteuid`.
- **Stale comment:** `xtask/src/gate_privileges.rs:487-489`.
- **Typed errors:** typed `NetsecError::DurabilityUnconfirmed`; `HubError` variants for the load-time trust checks.
- **Provider-install bundle** (comment of 20:34): SecretRef paths that are not portable across target homes, auth that is overwritten instead of merged, and providers enabled without credentials.
- **Field report F2:** the job error counter counts `error::` module paths (`harw-tool-job/src/progress.rs:515-527`).
- **Field report F1:** input that looks like a slash path is rejected, and the text is lost (`harw-tui` input/app).
- **Also recorded:** DoD install activation, the BPF toolchain side effect, the tri-state for provider scan capability, and the SecretRef permission error.

### Leftover worktrees
These are local to my container and not referenced by anything: `contract-a` and `kit-3` (stray copies from P14), `ripple-web`, `pr-baseline` and `contract-c`. I leave them untouched.
