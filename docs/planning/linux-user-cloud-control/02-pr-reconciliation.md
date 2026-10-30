# PR reconciliation and integration order

**Snapshot:** 2026-10-16, repository `mm9942/Harwness`, base B and comparison heads defined in [README](README.md#1-frozen-evidence-and-limits). PR links below are live discussions; frozen head values belong to this snapshot.

## 1. The decisive lineage

```text
dev B = 6e34f2e...  (#142 merge)
  |
  | 15 additional commits
  v
#145, base dev = 6c228e017...
  |
  | 1 additional commit
  v
N = c7ff325fa...
  +-- #146, base main, draft
  +-- #147, base main, not draft
```

Both N PRs contain the same source. They are not independent implementations. `compare_commits(B, N)` reports ahead 16, behind 0. `compare_commits(P145, N)` reports ahead 1, behind 0 and changes only:

- `harw-session-remote/src/port.rs` (+1);
- `harw-step-list/src/lib.rs` (+2/-2);
- `harw-tui/src/attach.rs` (+26/-1).

The title of #147 is a container wall-time fix, but its complete main-relative PR has 389 commits and 1,016 changed files in the metadata inspected. This does not establish that its claimed container behavior is missing; it establishes that the title cannot be used as a diff boundary. Verify the relevant read-back behavior, identify where it first entered the ancestry, and extract only a genuinely missing fix with its tests. Do not merge #146 and #147 as two separate feature deliveries.

The N comparison also adds the missing cloud source tree, StepList and remote-session changes, broad runtime fixes, generated TypeScript, and paper/build artifacts. This new design PR deliberately imports none of that aggregate diff. Its parent is B and its only changes are the dossier and implementation plan.

## 2. All open PRs visible at the cutoff

This is an inventory of the 19 open PRs returned during review, not a claim that every line of every patch was audited. `Absorbed` below is a content disposition supported by #142/#98 or current source, not a request to close a branch automatically. No existing PR is modified by this dossier.

| PR | Base / head at snapshot | Relation to this program | Disposition |
|---|---|---|---|
| [#147](https://github.com/mm9942/Harwness/pull/147) | main / `c7ff325fa...` | Container title, shared broad head N | Isolate timeout semantics and tests; do not wholesale merge or duplicate #146. |
| [#146](https://github.com/mm9942/Harwness/pull/146) | main / `c7ff325fa...` | Shared N, TUI/remote continuation work | Treat N as one candidate content snapshot. Rebase/extract onto a repaired dev baseline. |
| [#145](https://github.com/mm9942/Harwness/pull/145) | dev / `6c228e017...` | Immediate predecessor of N; includes cloud restoration and broader runtime/UI work | Separate baseline-restoration, cloud-adapter correctness, remote continuation and UI slices. Preserve attribution. |
| [#144](https://github.com/mm9942/Harwness/pull/144) | dev / `9dea6a95b...` | Chat-first Mini Live design, three pinned projections | Preferred display direction to reconcile with #137; do not add a second authoritative scheduler or automatic progress LLM. |
| [#143](https://github.com/mm9942/Harwness/pull/143) | dev / `db8c07116...` | Capability closure audit and existing gap register | Reuse evidence and gates; this dossier adds host-wide process and cloud integration contracts. Refresh old findings against N. |
| [#141](https://github.com/mm9942/Harwness/pull/141) | dev / `ab33dedd5...` | CommandPort test portability and related fixture fixes | Compare semantic content with N; retain missing tests only. Do not reintroduce obsolete direct-spawn fixtures. |
| [#134](https://github.com/mm9942/Harwness/pull/134) | dev / `2de19c8a6...` | Cycle-composed agent design | #142 rescued the doc addendum, not all possible branch content. Reconcile remaining prompt changes separately. |
| [#133](https://github.com/mm9942/Harwness/pull/133) | main / `571e8aa90...` | Documentation translation/cleanup | Ancillary; do not let older CURRENT descriptions overwrite this pinned audit. |
| [#111](https://github.com/mm9942/Harwness/pull/111) | main / `ffcd8d09d...` | Blocking-handoff guard | #142 explicitly drops it as obsolete after background-only handoff changes. Preserve only demonstrably relevant regressions. |
| [#109](https://github.com/mm9942/Harwness/pull/109) | dev / `7b1543aef...` | Older macro/integration work | Compare against #98 consolidation; not a fresh cloud feature by virtue of remaining open. |
| [#106](https://github.com/mm9942/Harwness/pull/106) | dev / `91db7b590...` | Telegram operation/command registry | Parked by #142. Must preserve nested-command and effective-permission checks before joining the common control plane. |
| [#95](https://github.com/mm9942/Harwness/pull/95) | main / `1d7c4e8b5...` | Older broad integration/provider lifecycle line | Scope by actual delta, not title. No implicit incorporation into this program. |
| [#92](https://github.com/mm9942/Harwness/pull/92) | dev / `918b5c856...` | Reverse-proxy policy and edge/SOCKS design | #142 rescued one SOCKS design document. A running proxy still requires an adapter and deployment validation. |
| [#91](https://github.com/mm9942/Harwness/pull/91) | dev / `4d1298307...` | Initial local session daemon | #98/current composition supersedes its old no-op-driver state. Do not build a second daemon from the older plan. |
| [#90](https://github.com/mm9942/Harwness/pull/90) | dev / `3ec23e56c...` | Deployment-profile implementation candidate | Parked by #142. Salvage the pure profile/mount model only after compatibility review; test real composition, not just an enum. |
| [#89](https://github.com/mm9942/Harwness/pull/89) | dev / `586cebaf6...` | Registry-derived tool index | Rescued by #142. Index existence is not the same as every implementation being registered and callable. |
| [#88](https://github.com/mm9942/Harwness/pull/88) | dev / `006afe004...` | SSH/Cloudflare tunnel policy | Policy sources rescued by #142; job-backed tunnel lifecycle remains a separate integration. |
| [#87](https://github.com/mm9942/Harwness/pull/87) | main / `20af202b2...` | WIP preservation snapshot | No automatic merge recommendation. Inspect only specific missing content with a current consumer and tests. |
| [#85](https://github.com/mm9942/Harwness/pull/85) | stacked on on-demand-worker branch / `8e150a2bd...` | On-demand boot review, node-wide limits, ownership races | Docs rescued by #142. Boot single-flight and capacity contracts still need implementation. |

Short head values above are labels, not fetch keys for automation. The machine-readable manifest freezes the main comparison heads. Refresh all candidate PR metadata and exact refs before implementation; changed heads invalidate old approval and test evidence.

## 3. Already landed anchors and what they do not prove

### #98: consolidation, not a complete cloud product

[PR #98](https://github.com/mm9942/Harwness/pull/98) is confirmed merged into dev on 2026-10-05, merge commit `017cfa07554cc4c60f5bce849f3f90754d77fdda`, source head `8cdd6b2c8b90a31a60444d85a62d9ca9d81ef6b2`.

It incorporates session transport/composition, memory, variable job lanes, retention, learning scaffolding, client/mobile and container foundations. Its description explicitly leaves OCI/K8s executor selection unwired into the service and notes missing image/signature policy. Its historical real-engine coverage is rootful Podman and a kube-apiserver/etcd exercise, not proof of rootless production or kubelet behavior.

The #116 job-capacity fix was incorporated through a different implementation in consolidation. Matching only commit ancestry would miss such a semantic integration; compare the reservation critical section and its tests. The repaired local JobManager limit still does not establish a node-wide reservation authority.

### #131: async jobs really landed

[PR #131](https://github.com/mm9942/Harwness/pull/131) is confirmed merged into dev on 2026-10-07, merge commit `7b8578a85b18eda9ca80d61620efa760b74e0b76`, source head `2c4fbc68cad1ba8d3eb2e587059cba6033f38c53`.

It routes workload commands through `harw-command`, introduces durable agent submissions and makes asynchronous delegation the default when the durable substrate exists. Do not describe the entire async-first layer as merely an unmerged plan. Its own remaining-delta document still calls for agent rehydration/reconciliation and exact-head validation. These are the continuation tasks, not a reason to replace the job architecture.

### #142: selective rescue is selective

[PR #142](https://github.com/mm9942/Harwness/pull/142) is B. It rescues the tool index, tunnel policy, agent-tree vocabulary and selected design work. Its body explicitly parks deployment-profile and Telegram integration work. A closed stale source branch and a rescue commit are not blanket evidence that every feature on that branch was absorbed.

### #143: prior review is a dependency, not our own fresh test run

[PR #143](https://github.com/mm9942/Harwness/pull/143) contains source-level evidence for missing cloud members, tool registration, provider-context parity and existing security gates. Its older base must be refreshed against B/N. Publicly documented security blockers should be closed before widening host control; new vulnerability details belong in the private process from SECURITY.md.

## 4. Closed base:dev search coverage

The following is the complete number set returned by the closed `base:dev` search used in this review. The connector search omits reliable merged-state fields, so **closed is not relabeled merged**. Only explicitly verified merges above receive that claim.

| Group | PRs returned | Correlation / treatment |
|---|---|---|
| Latest integration and intent work | #142, #135, #136, #139, #138, #140 | #136 supplies intent-cycle building blocks, not a full fleet executor. #138 is model-lane policy, not host capacity. #139 is untrained research. #140's paper has an older baseline. |
| Review/design refinements | #86, #83, #84, #124, #137 | Preserve deterministic projections, generation fencing and scope boundaries. Reconcile #137 with newer #144 before choosing UI work. |
| Runtime, tools, provider and baseline | #132, #131, #129, #130, #127, #126, #125, #123 | Separate build repair, job runtime, model gateway, tool discoverability and cloud enrollment. #130's suggestion that missing workspace members are inert must not be repeated. |
| Hub, UI, worker and semantic model | #82, #81, #76, #74, #68, #78 | Hub profiles and activity projections are distinct from actual backend mounting. #74's older worker path must not recreate a competing executor. #78 has insufficient descriptive content for a feature claim. |
| Container and adjacent maintenance | #96, #120, #116, #70, #121, #117, #97, #118, #79 | Container policy and job reservations feed this program. Keep license/version gates; model lifecycle and research remain separate domains, integrated only through existing contracts. |
| Session and platform foundations | #54, #60, #49, #57, #48, #61, #47, #59, #58, #98 | #54/#57 establish protocol and tool authority; #98 supplies later composition. Tailscale reachability does not replace node/device authority. Binary installation is not evidence of a valid cloud role. |
| Historical consolidation | #63, #69, #65, #71, #67, #64, #33, #34, #39, #41, #45, #38, #37 | Preserve bound-home resolution (#69), surface distinctions (#39), job/provider pacing (#34), and existing security gates. WIP snapshot #33 is not a current fix source without comparison. |

Related stacked work #80, #93, #94, #100, #101, #105 and #122 is referenced by these integration PRs. Their incorporation statements are attributed to the integrating PR, not presented as independent full patch reviews performed here.

## 5. Proposed integration sequence

1. **B0: reproducible baseline.** Extract or reconstruct legitimate missing cloud sources with minimal functional claims. Validate clean manifests/lock/layer/license/version consistency. Do not merge generated files, papers and unrelated UI changes merely to make those packages appear.
2. **B1: canonical adapter repair.** Reconcile cloud enrollment parsing, bound profile, semantic command names, service assets and readiness. Every cloud command must land on an actual service or fail explicitly as unavailable.
3. **B2: retain already-landed job semantics.** Incorporate only missing portability and recovery fixes; preserve #131 and #116/#98 guarantees.
4. **H: host inventory and node-wide admission.** Build the user/process contract in isolated read-only and own-job slices before external mutation.
5. **C: cloud ownership and execution.** Implement profile composition, canonical directory, fresh node offers, reservation and remote execution/artifacts. Keep node-local enforcement independent.
6. **U: projection/operations convergence.** Apply the reconciled #144 display approach to the real inventory/job/session sources. Telegram and other clients must use identical authorization semantics.
7. **V: freeze one integrated SHA.** Run the prescribed build, boundary, fault and real-host matrix. Only then consider a dev-to-main release.

No branch closing, PR retargeting, merging, deployment or billing change is performed by this dossier. Every future extraction records source PR/head, files retained, semantic tests and superseded contracts, so the next cleanup does not need to rediscover this history.
