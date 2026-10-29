# Local coding-cycle / PL-97 consolidation

Status: **LOCAL INTEGRATION CANDIDATE — NOT RELEASE ACCEPTANCE** (2026-10-29).
No upstream branch was rewritten and this consolidation has not been pushed.

## Immutable inputs

| Input | Revision | Treatment |
| --- | --- | --- |
| Merged dev baseline | `fcb12b41f5e53c19cc20607a63c98b829452b505` | Common ancestor |
| Local coding-scope WIP | `56cab4d1` | Preserved |
| Frozen local workflow, lifetime, checkpoint and callback changes | `5c3480dc` | Snapshot commit, including previously untracked files |
| PR #154 PL-97 implementation (snapshot) | `ea690e9203ab630388d54c7454612b615dcccc7c` | Used as base; PR #154 is still an open draft |
| PR #156 security review fixes | `22b0a27557cc41b22f0789b4a691d770a1485605` | Merged |
| Codex unknown export / Warden state fix | `5bb503bcf65cf7e07bd8b4cf69f66e61d713be92` | Cherry-picked |
| Codex consumer provenance propagation | `3983e115cd12541d9eb52170cdcb8fca8c01ee39` | Cherry-picked |
| Codex cancelled activity rebases | `e9443bc0539657bc3ec076ced9f2d704bb230907` | Cherry-picked |
| PR #153 and dependency-order / JobScope clarification | `88653ff81016baeab22ab849f833b777715f82c1` | Merged |

## Integration decisions

- Preserve both dynamic workflow guidance and triggered skill injection. The
  workflow is built from the effective tool inventory before request assembly;
  bounded skill blocks are appended afterward. Skill provenance, reconciliation
  and ledger recording remain intact. Neither mechanism grants write authority.
- Preserve both opaque tool lifetime guards and PL-97 activity macro metadata.
  Activity descriptors describe execution; they do not replace authorization.
- The combined configuration table has 200 entries and 101 `ProfileReplaces`
  entries: the trusted single-agent topology field and all four skill fields
  coexist. The second checksum required a fix despite Git merging it cleanly.
- Formatting-only normalization of three inherited PL-97 files is separate from
  behavioral changes.
- Integration is performed in a separate worktree. The active coding checkout and
  its uncommitted work are not overwritten.

## Acceptance boundaries

The PL-97 input is explicitly a safety snapshot. Its remaining work in
[PL-97 resume notes](../97-harw-dod-mission-control-chain/wip/RESUME.md) is not
made complete by this merge. In particular, report-file integrity without
Landlock, refusing `/` as the plain Bwrap workspace, attempt-directory cleanup,
backend-documentation consistency, and BPF multi-handle update behavior still
require their own verified resolution.

Likewise, workflow prompts are not proof of successful autonomous execution.
Plan readiness, durable checkpoints, attempt/assignment fencing and retained
filesystem tool lifetimes do not authorize automatic historical-worker
retirement or guarantee process/remote-tool quiescence. Unsupported plan-store
implementations reject checked publication; legacy unbound plan jobs require
trusted re-admission. No safe-default permission is broadened here.

Validation results are recorded below only after commands finish; a successful
text merge is not evidence of CI, security acceptance or runtime completeness.

## Local validation (2026-10-29)

- Combined library tests: activity 53, config 406, core 757, DoD client 43,
  plan 368, plan-bridge 443, session-store 138, filesystem tools 124 and
  tool vocabulary 74 passed. The DoD count includes a new deterministic
  missing-error test; rootless ownership tests preserve fail-closed trust.
- CLI `job_worker` selection: 253 passed (696 unrelated tests filtered).
- Added request-level regression, run separately: 1 passed. It assembles both
  workflow and bundled/imported skills with and without a context program,
  checks the stable prefix, imported evidence separation, inclusion labels,
  reconciliation on repeated requests and content-free ledger records.
- Additional operations/runtime libraries: 933 + 496 passed. **Three tests
  remain environment-blocked**, not waived or marked successful:
  - two gateway health tests cannot bind Unix sockets (`EPERM`);
  - the active sandbox lease child test rejects the fixed-path `prlimit`
    binary because this environment exposes it and its parents as UID 65534,
    rather than trusted root ownership.
  The requested socket-test sandbox escalation could not initialize its own
  sandbox; no denied directory was accessed and no bypass was attempted.
- Strict Clippy (`--all-targets -- -D warnings`) passed for CLI and the nine
  library packages above. The added request regression is checked separately
  with core Clippy before committing.
- All six architecture gates passed via `cargo run -p xtask -- gates`:
  edges 3221, privileges 6, Warden dependency budget 52, Warden C-build 52,
  architecture 2399 and spawn 1737. The existing documented BLAKE3 C/ASM
  exception remains unchanged.
- Workspace formatting and `git diff --check` passed before the final docs
  update; final formatting is rechecked after the request regression.

This is 4,089 passing tests across the stated selections and three blocked
checks, **not** a full-workspace CI run. No macOS, live BPF/Landlock integration,
full DoD deployment, remote dispatch or release installation is certified.

## Follow-up: BPF partial-update reconciliation (2026-10-29)

On local integration head `8b08931d`, PR #154 review comment
[`discussion_r4407819921`](https://github.com/mm9942/Harwness/pull/154#discussion_r4407819921)
was reproduced: a failed update could leave one object map changed while the
shared tracker stayed unchanged, and a later empty plan falsely reported recovery.

- `ScopeRescan` now retains the inverse of an uncommitted delta. Every handle,
  including the handle that returned an error after possibly changing entries,
  must acknowledge that rollback before a new delta is applied. Failed rollback
  keeps the scope degraded; reconciliation state is bounded by one delta, not
  accumulated indefinitely under cgroup churn.
- Successful rollback restores the last confirmed observation scope before a
  fresh walk/plan is applied. This is **not atomic kernel-map revocation**: maps
  may temporarily disagree, and rollback may temporarily reinsert a vanished ID.
  No new observation authority or root is introduced. Recovery is not reported
  while reconciliation is uncertain; counters describe committed logical deltas,
  not all individual kernel writes.
- A pre-existing compile mismatch in `main::fs_scope_roots` was repaired: the
  existing `KERNEL_INTERFACE_ROOTS` are returned as `FsRoot::read` rather than
  untyped paths. The regression checks those exact allowlisted paths and read
  access. No additional Landlock path or list permission is enabled.

Validation:

- Three new regressions were observed failing before the reconciliation fix.
- `cargo test -p harw-probe-bpf`: **87 passed**, including 13 scope-rescan tests
  and five new regressions covering insert/disappearance, repeated rollback
  failure, remove/incomplete walk, remove/same-ID return, and partial per-entry
  mutation followed by a second failed fresh plan.
- `cargo clippy -p harw-probe-bpf --tests -- -D warnings`: passed.
- Changed-file rustfmt check and `git diff --check`: passed.
- `cargo clippy -p harw-probe-bpf --all-targets -- -D warnings`: **failed** on
  existing unused production surfaces: `ProbeError::{Config,
  CgroupScopeUnresolved,CgroupRootInvalid,CgroupScopeRejected}`,
  `RootAccess::ListDirectories`, `FsRoot::list_directories`, and
  `ScopeRescan::with_startup`. No warning suppression was added.

**E3b production acceptance remains open.** `main::profile_descendant_scope()`
explicitly returns `None`; production profile-to-cgroup resolution and the
list-only cgroup-root Landlock mount are not wired by this patch. Passing fixture
regressions is not proof of live BPF loading, kernel-map behavior, or an enabled
production rescan. This local follow-up is not full-workspace CI or release
acceptance, and has not been pushed.

## Cloud and native-image follow-on integration

The local candidate also merges PR #157 (`4dc053c3`) and PR #158
(`95aa4f25`, including `8dd077b8`), on top of the local sandbox and BPF
follow-up fixes through `26b33601`. These upstream PR combinations were not
previously verified together.

Integration preserves the Cloud operation-provider capability split and the
runtime image-inspector service. The DoD service-manager spawn exception stays;
the Cloud exceptions removed by #157 remain removed. Cargo lock/manifest changes
are composed rather than replacing either dependency set.

Verified corrections discovered during integration:

- `fs.view_image` is the eighth filesystem tool. Its name and Observe activity
  are aligned with provider order; glob/grep activity indices move accordingly.
  The reviewed golden change adds only the native-image schema and the image
  hint in `fs.read`'s description. The image blocking closure retains its cloned
  execution context, including the existing lifetime guard.
- The auxiliary `image.inspect` path additionally carries that guard through
  the model-operation adapter and `OpContext` into its nested image read. This
  prevents cancellation of the outer future from prematurely dropping the
  dispatch lifetime. The guard grants no permission and does not establish
  quiescence for unrelated remote/process effects.
- The combined configuration table contains 205 entries / 106 ProfileReplaces;
  the base operation registry contains 53 operations, not the independently
  updated counts from either upstream branch.
- A #157 host summary call needed explicit MutexGuard dereferencing to compile.
- The #157 rollback tombstone was originally mistaken for a never-admitted
  legacy session on subsequent opens. The runtime now rejects that tombstone
  before factory construction/hydration. The regression that previously hung
  now has a bounded timeout and passes without recreating a legacy runtime.

These are local integration changes, not upstream publication or a declaration
that hosted/cloud execution and image-budget integration are fully complete.

Follow-on validation, separate from the earlier pre-#157 test counts:

- Config 408, filesystem tools 126, session-host 111 and session-driver 2
  library tests passed.
- Base operation registration 3, auxiliary image model 3, Cloud enrollment
  integration 3, nested-operation lifetime 1 and core reasoning dispatch
  continuity 3 passed.
- Provider tests: native-image serialization 12 (the two HTTP listener cases
  explicitly excluded), reasoning selection/replay 20, Anthropic replay 7
  passed. These selections do not claim full provider/network coverage.
- Normal runtime-library compilation exposed a missing production dependency:
  `harw-tools` was previously only a dev-dependency. It is now a normal
  dependency, with no new third-party package. Cloud fixtures now use the
  actual assembly sandbox accessor; provider tests import Mutex explicitly
  rather than relying on a removed parent-module import.
- Disk pressure was relieved only by removing obsolete regenerable test
  executables, with an external cleanup log. No source, evidence log or
  live checkout was removed.
- Follow-on strict all-target Clippy passed for CLI, runtime, ops, operations,
  provider-http, tool-fs, config, session-driver and session-host.
- Final six gates passed after the production dependency correction: edges
  3232, privileges 6, Warden 52/52, architecture 2410, spawn 1749.

## Plan-job dispatch ownership boundary (local prerequisite only)

Plan-node admission now persists a typed `dispatch_owner`. Existing payloads
without that field retain legacy routing only when the remaining execution
contract is valid. Recovery and staged-job reuse normalize only an absent
legacy owner; null, unknown variants, extra owner fields and prerequisite
owners fail closed. Exact payload equality and scope/budget/retry checks remain
in force, including an old unbound Pending job left before assignment CAS.

`WorkDriverPrerequisite` records a typed parent WorkId and nonzero claim epoch
as routing data, **not authority**. Such jobs are deliberately disabled before
model/tool execution. The completion follow-up independently reloads and parses
the persisted source job before entering legacy admission, so a prerequisite
cannot silently launch Coding through that path.

This is not the automatic Research → Contract → Coding implementation. A
trusted live-parent claim, reserved parent budget, read-only prerequisite
admission, structural plan baseline and bounded progress-revision reconciliation
are still required before prerequisite dispatch can be enabled. No new phase
executor, write authority or parent-budget capability is introduced here.

Validation for this local slice: all 446 plan-bridge library tests and 66
CLI job-worker selected tests passed; strict all-target Clippy for plan-bridge
and CLI, workspace formatting and diff checks passed. Regression coverage
includes serde/backward defaults, forged/unknown owner rejection, no model
call for a prerequisite, independent follow-up isolation, and bound/unbound
Pending recovery. The parser and follow-up regressions, and the unbound legacy
Pending regression, were observed failing before their fixes. An independent
review checked the final owner/recovery boundaries. This is targeted validation,
not a full workspace, live-parent or automatic-phase execution certification.

## Immutable WorkDriver admission baseline (local prerequisite only)

New plan-bound `work_driver.enqueue` jobs capture the selected tenant-visible
plan using fallible `current()` (fresh WAL), never advisory `revision()`.
Selected plan ID, an existing goal revision binding and any plan-side goal
binding must agree. A supplied frozen approval revision is checked against that
same snapshot. The job records its revision and a domain-separated content
digest of the complete typed Plan JSON; it does not duplicate plan/model text.
The compact baseline is runtime-produced, not an enqueue argument or authority.

Every round compares the baseline before report, dispatch or verification.
Same-revision replacements, status/evidence changes, scope/tenant changes and
unsupported digest schema versions fail closed. Ordinary progress also blocks:
there is deliberately no receipt acceptance or automatic re-binding yet. Old
jobs with no baseline retain their existing optional pin semantics. Planless
jobs remain planless. This snapshot check is not a lock across execution and
cannot prevent a mutation after the check; it closes admission identity gaps,
not cross-store transactions or execution-time revocation.

Version 1 hashes ordered typed structs/vectors and scalars (no unordered maps),
including progress and timestamps. Serialized representation changes after an
upgrade can require re-enqueueing. Hashing allocates proportional to the already
loaded plan; only the fixed-size digest, ID and revision enter the job. Future
trusted parent-owned receipts need a separate versioned structural admission
contract, authoritative WAL transactions and live parent lease/scope/shared
budget validation; this baseline does not enable prerequisite execution.

Validation: 32 selected WorkDriver ops tests and 65 CLI WorkDriver tests passed,
including matching-baseline dispatch and changed-baseline rejection before
spawning/verification. Strict all-target Clippy for ops and CLI passed. Independent
review found no blocker; its positive-dispatch coverage/documentation requests
were incorporated. This is targeted validation, not an automatic-phase or full
workspace certification.

## Guarded natural prerequisites (local, not release acceptance)

The source-collection vertical slice is connected through admitted descriptors,
minimal ID-only reader turns, durable artifacts, live-parent-fenced atomic plan
receipts and baseline-chain reconciliation. See
[the detailed implementation checkpoint](09-natural-prerequisites-plan.md#12-local-implementation-checkpoint-2026-10-29).
It proves source coverage, not semantic research correctness. New opt-in coding
turns use exact-file inherited ceilings and remain fail-closed while the isolated
writer backend is unavailable. No installation permission broadening, push or
merge is implied.
