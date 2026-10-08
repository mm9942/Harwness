# 01 — CURRENT: source-grounded agent and orchestration architecture

> Verified against `dev@197a92e92d438bf6bf93b129bb964f4852686149`; documentation-only design review. Planning proposals are not deployed features.

## 1. Authority versus specialization

The closed organizational role enum is in `harw-agent-dsl/src/roles.rs`: UserInterface, RootOrchestrator, ChildOrchestrator, Worker, UiaWorker, AgentSteward. `can_spawn` establishes the hard role upper bound; a ChildOrchestrator spawning another ChildOrchestrator additionally requires an *exact named definition grant*. Worker does not spawn durable agents; agent-as-tool is a separately governed path. Composed Agent does not add a role. Definitions and IR live in `harw-agent-dsl/`; `harw-agent-compiler/` compiles agent definitions; runtime admission remains authoritative.

The built-in `harw-registry-defaults/agents/root-orchestrator.toml` restricts `[spawn].child_orchestrators` to coding-, research- and analysis-orchestrator. Its `[delegation].targets` is also explicit and intersects runtime visibility. The `child-orchestrator-base.toml` sets a read-only coordination ceiling, depth and budget. Because definition inheritance can prefer base sections, specialization must not assume a custom `[tools]` or `[spawn]` override grants anything. Existing specialist child orchestrators such as `harw-home/assets/agents/intel-analysis-orchestrator`, `wargaming-orchestrator`, `evidence-review-orchestrator` and `dependency-research-orchestrator` exist without thereby becoming universally spawnable from Root. Needed changes must be tested in effective IR and runtime admission.

## 2. Runtime boundaries

`harw-core/src/child_controller.rs` governs child admission, lifecycle, budgets, cancellation and return. `harw-runtime/src/children.rs` assembles effective child registries and model routing. `harw-core/src/delegation_visibility.rs` limits advertised targets and uses `agents.delegate` above eight visible targets. `harw-core-bridge/src/delegate_wave.rs` provides bounded fan-out and joins, while `harw-plan-bridge/src/cells.rs` offers cell-level stages and barriers. Root/sub-orchestrator instructions are in `harw-registry-defaults/knowledge/roles/`; current text tells them to delegate deep reads. This is policy, not yet evidence of optimal automatic delegation under failure.

`harw-job-core` owns durable job substrate. `harw-plan-bridge/src/work_driver.rs`, `harw-cli/src/job_worker_work_driver.rs`, `harw-ops/src/work_driver.rs` own the existing WorkDriver: bounded worker rounds, central verification, judgement and human-only goal achievement. It is a domain-specific iterative supervisor, not a generic adaptive chain runtime. `harw-core/src/guard.rs` detects no-progress, duplicate delegation, read budgets, budget overruns and repeated tool failures. Chain integration should reuse it, with typed progress rather than counting all tool calls as improvement.

## 3. Chain planning and historical drift

PL-69 `docs/planning/69-durable-agent-chains/README.md` describes a generic durable chain with lease-epoch checkpoint and decisions. `REVISION-model-turn-chain.md` corrects the model: one nested, cuttable Model-Turn-Chain with per-turn `(model, effort)`, fan-out, terminal synthesis, decision, repeated reorientation and dynamic key-driver drift. Generic runtime implementation is **not CURRENT**. Chains are execution graphs, not organizational roles, agents, or generic permission grants.

Context programs in `harw-registry-defaults/agents/context-programs/orchestrate.toml` require plan and summarized child returns, exclude raw child transcripts. The context ledger, feedback and learning/maintenance paths in `harw-context-ledger/`, `harw-core/src/turn_feedback.rs`, `harw-cli/src/job_worker_learning.rs` and `job_worker_memory.rs` provide pieces of evidence/learning infrastructure. A global semantic chain checkpoint and adaptive intent-state projector are still proposed.

## 4. The Gap-Hunt exemplar (actual source)

`docs/planning/85-gap-hunt/kit/skills/gap-hunt/SKILL.md` defines an **operational adaptive workflow**: scout disjoint crate areas; parallel `gap-hunt-area.js` read-only find/critic/tiered verification; normalize/deduplicate; separate one-file findings for `gap-fix.js` from multi-file `contract-wave.js`; ripple check; one central build; write immutable wave manifests/gate records; commit approved named files only; PR to dev. `R15-patterns.md` records fail-open, drift, cross-file ripple, missing integration and throughput defects, including the “intent lens” rejecting false positives. These are reference practices from the repository, not proof Claude Code has a generic Harwness chain API.

**Runtime boundary clarification (review #135):** Source admission must be checked via `harw-agent-dsl/src/roles.rs::can_spawn`, the exact `root-orchestrator.toml [spawn].child_orchestrators` and `[delegation].targets`, `harw-core/src/delegation_visibility.rs`, and the managed child spawner. Root may spawn Worker explorers directly when admitted; there is no universal requirement for an intervening ChildOrchestrator. WorkDriver is a separate `[work_driver]`-gated, approval-protected supervisor in `harw-ops/src/work_driver.rs`, not a universally callable chain primitive. See `docs/planning/85-gap-hunt/kit/workflows/` and `docs/guides/work-driver.md`.

The key lesson is **conditional topology evolution**: a finding may be disproven and end; become one-file fix; become multi-file contract; trigger ripple, new investigation, re-review or stop. The goal and base SHA remain pinned. Each writing workflow receives one worktree/branch and disjoint file ownership; only the main integration performs the central build. Fixes are never accepted just because a model says “done.”

## 5. Domain building blocks and constraints

| Domain | Existing assets | Critical integration condition |
|---|---|---|
| Exploration | `explorer`, `explore.*`, `fs.*`, `deps.*` | read-only, frontiers by path, no full-tree context dumps |
| Web/dependencies | `researcher-web`, `researcher-deps`, `dependency-research-orchestrator` | effective network+provider route; 403 and Egress failures stop repeat loops |
| Intel | `intel-analysis-orchestrator`, evidence-collector/critic, method-auditor, pattern-analyst, synthesis-writer | separated evidence and narrative claims |
| Wargaming | `wargaming-orchestrator`, scenario-player, systems-modeller | exploratory scenario analysis, not an empirical forecast |
| Matrix Game | `matrix-game-master`, `matrix.*`, four seat families, `docs/design/matrix-game.md` | Master currently **root-orchestrator** launched by UIA; engine alone owns dice, game turns, and gated start/run/finish |
| Business | `business-writing-pyramid`, `author-review-pipeline`, business-author/reviewer | independent review before approval, no self-review |
| LaTeX | `latex-report`, `uia-latex-writer`, `latex.template/check/build` | UIA-special role, render approved source in small sections |
| Coding | coding-orchestrator, planner/explorer/executor, WorkDriver | typed edit scopes, diff/gate review, provenance and cancellation |
| Knowledge | Diary, Dream, Palace, Memory Steward, context ledger | memory content is evidence, not authorization |
| DoD | Detect–Orient–Defend architecture | preserve small privileged TCB, approvals and boundary between observations and responses |

## 6. Observed gap classes, not automatically outstanding bugs

Session traces and `docs/planning/85-gap-hunt/coordination/claude-field-report.md` report large root context consumption, unavailable model `403`, network Egress refusal, “no delegation capability,” overshooting model/output limits and repeated blocked research. These are motivation and historical evidence; confirm each against current code and live tests before labeling a present defect. Stronger orchestration instructions alone will not repair a missing tool executor or inherited network scope. Track `advertised`, `resolvable`, `admitted`, `executed` capabilities independently.

## 7. Scope boundaries

Current source and tests supersede design docs and old planning examples. Retain WorkDriver, the existing Matrix Game root, concrete role ceilings and the job lifecycle. No new authorization is implied by this documentation. The rest of PL-90 defines a target adapter over these components with explicit migration gates.
