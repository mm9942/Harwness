---
id: PL-96
title: "Capability closure: evidence-grounded runtime integration, safety gates and scientific verification"
status: proposed
date: 2026-10-09
baseline_dev: 090a9d2b786a3a6c025182d8da9c7b28bb7cbece
baseline_main: 90160f4f2b88f80c6721c20469ed8322ddc8861a
scope: [architecture, tooling, context, skills, sessions, cloud, channels, security, work, research]
---

# PL-96 — Capability closure and evidence-grounded integration

> **REVIEW / PLANNING ONLY — no implementation, deployment, merge, branch deletion, permission change or security approval is performed by this PR.** The findings are pinned to the exact repository commits above. An existing source-level test, a claim in a historical PR body, a passing unrelated workflow and an end-to-end verification are four different types of evidence.

## Thesis

Harwness already has strong typed contracts for runtime entry, authority, durable work, agent definitions, tool providers, session control and knowledge. The main remaining risk is **capability closure**: a component can exist, be documented and pass isolated tests while no correctly authorized production entry actually exposes and exercises it.

For every capability, independently establish:

`declared -> compiled -> registered -> authorized -> reachable -> invoked -> observed -> verified`

A missing transition must be reported as a specific integration gap, never disguised by a larger tool list, new agent persona, prompt-only instruction or a successful unit test.

**Goals:** convert the October 9 bottom-up audit into reproducible claims, safe integration waves, falsification tests, PR rescue decisions and measurable research hypotheses. Distinguish **architecture ownership** from repository directory names; preserve DoD's Warden TCB and first-party safe Rust.

## What was actually examined

- Current `dev` tree and files at `090a9d2b786a3a6c025182d8da9c7b28bb7cbece` (4,237 Git tree entries; recursive tree not truncated), root manifest and both lockfiles.
- Current `main` at `90160f4f2b88f80c6721c20469ed8322ddc8861a`.
- Producer/consumer source paths across registry profiles, native tool crates, skill catalog, provider wire builders, session daemon/composition, Telegram operation adapter, project trust, Warden protocol, and runtime assembly.
- Merged PRs #124, #126, #127, #131, #132, #136, #137, #138, #139, #140; open PRs #88, #89, #90, #91, #95, #134, #141, #142; relevant historical closed branches.
- Existing plans PL-60, PL-65, PL-69, PL-71, PL-90, PL-93, PL-94, PL-95 and the paper at `docs/research/harwness-paper.md`.

**Methods used for this audit:** connected GitHub source/metadata reads, exact-file crosschecks, tree/manifest comparisons, PR changed-file metadata and visible CI history. **Not performed:** a local Cargo build, `cargo metadata`, `cargo xtask gates`, end-to-end execution, live Telegram, deployment or independent security exploitation. Individual PR authors' reported test results are not reinterpreted as a fresh successful run.

## Executive counterchecks

| ID | Finding on pinned baseline | Evidence tier | Action |
|---|---|---|---|
| E01 | `main/Cargo.lock` contains **129 complete merge-conflict blocks**; `dev/Cargo.lock` has none. PR #132 fixed `dev`, not `main`. | Direct file inspection | P0 release/build repair in a dedicated branch |
| E02 | `dev/Cargo.toml` references five `harw-cloud` members without matching tracked paths. PR #136 used **untracked stub manifests** for some test runs. | Manifest + tree + PR #136 | P0 clean-checkout reproducibility |
| E03 | DoD crates are **already root-workspace members** on `dev`; a pre-PL-60 separate-workspace claim is obsolete. | `Cargo.toml` and PL-60 | Preserve Warden TCB gates, do not plan a second merge |
| E04 | 42 typed common-command tools exist in four crates but are not admitted by registry/profile/model surfaces. | PL-93 and source | P1 effective tool wiring |
| E05 | PL-95 S1 trigger parsing/selection exists; selection-to-context injection is not production-wired. | `harw-catalog` + PR #126 | P1 deterministic skill delivery |
| E06 | OpenAI Responses/Chat append `ModelRequest::data_block`; Anthropic Messages currently omits it. | Direct wire builder comparison | P0 provider-parity regression test and correction |
| E07 | Local and remote session ingresses have separate hosts/state; UDS daemon mounts `ToolHost::disabled()`. | Direct composition code | P1 common-owner design plus explicit tool-gateway mount |
| E08 | Unmerged Telegram branch validates only the **first token** for `ChannelReduced`; nested mutating `/dream review … accept` passes that syntactic filter. | Unmerged Telegram branch; see evidence register | P0 fix before exposing general remote commands |
| E09 | Six agent authority roles are sealed; many specialist definitions are separate and not all discovered files are startable. | Role enum + Roster + profile | P1 truthful agent discoverability |
| E10 | PR #95 is labeled provider-lifecycle docs but its compare metadata spans 433 files (298 Rust) against old `main`. | PR file-list + base | P1 rebuild docs-only diff from current base |
| E11 | PR #91 documents a no-op/limited listener, but current `dev` already composes production `CoreTurnDriver` and `PortOffer::All`; older PR is partly superseded. | PR text versus dev code | P1 semantic branch reconciliation |
| E12 | `harw-dod-warden-proto` has SignedAuthorization v2, while `harw-dod-warden::Warden::handle` still verifies v1 AuthorizationProof. | Direct Warden code | **P0 security gate**: keep privileged production rollout blocked until verified migration |
| E13 | PR #142 rescues a few files but remaining stale-branch work is not exhausted. | Open draft + compare | P1 no-delete rescue ledger |
| E14 | Adaptive Cloudflare lanes code has no asserted production deployment; tiny-expert training is an untrained prototype. | PR #138/#139 | Research/evaluation, not production claims |

Corrections to earlier high-level reviews: `dev` is **not** a separate DoD workspace; `SessionHost` is **not** entirely unused; `PortOffer::All` does **not** mean `tool.call` can execute; the mere existence of typed `cargo.*` code does **not** mean those new tools are registered. Likewise an open/closed PR state does not prove absence/presence of its features on `dev`.

## Proposed target

1. **Truthful effective capability projection** derived from runtime-mounted providers, authority, current profile and actual call path; distinguish installed, advertised, authorized, reachable and verified.
2. **Small model-visible working set** with on-demand tool-family discovery and deterministic, source-attributed skill sections. No accidental fallback to shell where a typed operation is appropriate; never silently widen authority.
3. **Provider-neutral context integrity** with tests for all wire serializers, strict trust framing, omissions and token budgets. Preserve stable cacheable prefixes while injecting fresh content in the correct channel.
4. **Durable, scoped knowledge lifecycle**: capture -> candidate -> verification -> scoped promotion -> embedding/index -> next-session retrieval; never confuse shared search with shared agent private memory.
5. **One authoritative session owner**, device-/principal-specific capability grants, independently persisted state, leases/fencing, explicit remote ToolHost wiring and end-to-end rejection tests.
6. **One operation grammar with surface-specific policy**, including nested subcommands and effect classes; channel menus are projections, never grants.
7. **Reviewable integration waves** with disjoint write sets, migration ledger, verified gates, rollback and proof of production composition.
8. **Empirical scientific evaluation** on pinned artifact/model/config manifests, not simulated claims of benchmark superiority.

## Non-goals and guardrails

- No new authority role, privileged shortcut or general-purpose tool with weaker checks.
- No automatic promotion of Dream/Learn/Diary content to global facts or permissions.
- No model-visible all-tools dump or prompt-only security boundary.
- No silent live Cloudflare changes, Pingora addition, model training or Warden deployment.
- No replacement of the established JobManager/RuntimeAssembly with a second lifecycle.
- No generic framework extraction into the Warden privileged dependency closure.
- No branch deletion until exclusive changes are accounted for and owner approval recorded.
- A new plan is **not** the source of truth if current Rust source or executable gates disagree.

## Document map and review order

1. [01-evidence-register.md](01-evidence-register.md) — counterchecked findings, competing explanations, confidence/limits, code and PR citations.
2. [02-capability-context-and-knowledge.md](02-capability-context-and-knowledge.md) — tools, native-shell routing, triggered skills, context parity, agent selection, caching, memory.
3. [03-security-sessions-and-surfaces.md](03-security-sessions-and-surfaces.md) — remote host, Gateway ToolHost, Telegram, Warden, project trust, edge proxy.
4. [04-pr-branch-reconciliation.md](04-pr-branch-reconciliation.md) — PR matrix, supersession, change rescue and merge discipline.
5. [05-implementation-waves-and-gates.md](05-implementation-waves-and-gates.md) — wave contracts, owners, test commands, negative cases, release gates.
6. [06-research-evaluation.md](06-research-evaluation.md) — falsifiable comparisons, metrics, reproducibility and scientific paper delta.

## Decision ledger (owner review required)

| Decision | Proposed direction | Not automatically authorized |
|---|---|---|
| Release baseline | Repair `main` lock and `dev` manifest reproducibility before broad feature merges | No force-push/main rewrite |
| Capability registry | Effective registry is runtime truth; static catalog remains search index | Do not convert index into permission authority |
| Shared Cloud Home | Stable session owner + per-connection authority + lease/fencing | Do not merge remote/local rights or blindly merge storage dirs |
| Tool vs shell | Typed tool first when semantics match; honest unavailable/fallback state | No automatic host execution or sudo |
| Telegram | Full grammar + effects + principal checking | No Owner elevation or hidden remote mutation |
| Dream/Learn | Scoped review-gated candidates, background checkpointing by durable event | No silent global promotion |
| Warden | Signed proof migration + adversarial tests + no v1 enforcement | No release waiver by documentation only |
| Branch rescue | Prove unique content, rebase/cherry-pick minimal changes | No broad blind merges or automated stale deletion |

## Definition of done for PL-96 itself

This **documentation PR** is ready for review when all six companion files exist, links and pinned sources resolve, CURRENT/PROPOSED statements are separated, every P0 has an executable falsifier and a named future integration wave, and no code or runtime permission is modified. Individual implementation waves need their **own** PRs and tests; merging this planning PR must never be represented as implementing any of them.

Primary existing references: [PL-93](../93-common-command-tools/README.md), [PL-95](../95-cli-tools-skills/README.md), [PL-90](../90-intent-driven-composed-agents/README.md), [PL-60](../50-dod-integration/README.md), [research paper](../../research/harwness-paper.md), [PR #142](https://github.com/mm9942/Harwness/pull/142), [PR #136](https://github.com/mm9942/Harwness/pull/136).

**Review status: DRAFT. Implementation status: NOTHING in PL-96 has been implemented by this change.**
