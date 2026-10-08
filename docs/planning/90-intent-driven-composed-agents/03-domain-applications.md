# 03 — Applying composed adaptive agents to Harwness domains

> **Integration design** over the pinned dev baseline. Existing assets cited by path are current; recipes, adapters and transition policies proposed below are not implemented merely because an existing specialist shares a similar name.

## Shared adapter protocol

Each domain adapter defines a typed `IntentInput`, evidence schema, permitted transition set, admissible specialist tools, success predicates, optional external-effect policy, and an output envelope. It must not own its own generic checkpoint scheduler, authority engine or provider pacing subsystem. Those belong to the shared composition/job runtime. Domain-specific verification remains domain-owned (compiler/tests for code; source/method review for research; independent business review for writing; deterministic Matrix rules and event log for game simulations).

```text
abstract composed agent (shared)
    + domain intent/evidence/recipe (specialized)
    + existing admitted tools/roles (unchanged)
    = one externally addressable capability
```

### 1. Explore sub-orchestrator and repository cartographer

**CURRENT:** `harw-registry-defaults/agents/explorer.toml`, `explore.*`, `fs.*`, `deps.*`; root delegates to existing explorer worker, subject to target and role ceiling. **TARGET:** an optional `explore-orchestrator` with ChildOrchestrator authority only after exact root permit, bounded runtime and verified inheritance, plus a composed `explore-agent` using small model + adaptive exploration chain.

**Input:** objective (“map write path from API to storage”), repo/worktree/base, allowed read prefixes, target result structure, relevance and evidence policy, limit. **State:** subsystem frontier, module and symbol IDs, call/dependency edges, source ranges, confidence, unresolved references and conflicting interpretations. **Transition candidates:** inspect manifest; search symbol; list matching files; follow caller/callee; retrieve relevant docs; delegate disjoint source exploration; verify an interface; reorient on new trust boundary; synthesize a map; escalate missing source.

A poor implementation dumps entire repository lists into model context. The good one indexes/searches deterministically and passes minimal candidates. Each result should include path:line, symbol, commit, why relevant and next uncertainty. A bounded explorer may choose whether to inspect `Cargo.lock`, a feature flag, a test fixture or a generated client after evidence, not before. No tool call is a sign of progress unless it advances coverage or narrows an unknown.

**Suggested skills:** structural exploration, call-path tracing, dependency tracing, architecture map. Nested cognitive chains may perform recursive graph exploration without generating new OS processes/agents. Only an admitted ChildOrchestrator can spawn worker explorers.

**Acceptance:** answer coverage, cited path/line anchors, explicit unknowns, no read outside scope, no gigantic transcript; adversarial test with misleading README and conflicting implementation.

### 2. Directory/file research and evidence collector

**CURRENT:** `harw-home/assets/agents/evidence-collector`, `source-researcher`, `harw-registry-defaults/agents/researcher.toml`. **TARGET:** a composed `directory-research-agent` for path-bounded retrieval and a `file-research-agent` for single-document provenance. Both can use explicit `hypothesis → query → inspect → cite → update` steps. They return claim/evidence graph, not narrative guesses.

A directory job should first infer likely index roots and file types, then validate candidate results and stop at calibrated coverage; a changed path or unexpected generated file can cause reorientation. For PDFs, extract text and page provenance through `doc.read_pdf`; inaccessible figures are marked unknown. For archives, rely on approved archive tools only after verified support; never read arbitrary binary as lossy text and conclude absence. Freshness and scope should be part of evidence identity.

### 3. Web research, dependency research and external-source preflight

**CURRENT:** `researcher-web`, `researcher-deps`, `dependency-researcher`, `harw-home/assets/agents/dependency-research-orchestrator`. `research-orchestrator` may possess constrained NetworkAccess for children while forbidding direct `web.*` execution. **TARGET:** intent-addressable `web-research-agent` and `dependency-research-agent`; task states enumerate known source classes (official docs, source tags, registries, issues, security advisory), actual retrieved URLs/versions and unresolved material questions.

A chain chooses the next source based on the latest evidence, but the runtime first validates model credentials, source-domain Egress, permitted tool, query budget and provider pace. On 403 from a disabled model, abandon that route and report `model_unavailable`; on host allowlist refusal, `source_unavailable_by_policy`. An unrelated local document is not promoted to official primary evidence. Dependency claims must pin exact versions, features and call sites, not only crate name; uncertain third-party API behavior can trigger a minimal authorized source-review stage.

Cross-domain handoff example: Explore discovers OpenDAL adapter → dependency research checks exact version/source API → evidence critic reviews confidence → Coding Orchestrator may request a fix only after rights/approval and testable acceptance.

### 4. Intelligence, patterns, trends and causal analysis

**CURRENT:** `intel-analysis-orchestrator`, `evidence-collector`, `pattern-analyst`, `systems-modeller`, `evidence-critic`, `method-auditor`, `synthesis-writer`; `analysis-workflow` specifies four waves. **TARGET:** the waves become a *default recipe* not a rigid loop. After initial fan-out, a high-impact contradiction may trigger adversarial falsification before further collection; a changed key driver may generate entirely different data queries; a verified stable picture proceeds to independent method review and synthesis.

Represent competing hypotheses and evidence for/against each; keep links, observation date and distinction between measured trend, inferred tendency and speculative scenario. A new event can invalidate part of a synthesis and reopen only affected branches, rather than regenerate all analysis. A critic sees source material without being instructed to approve. Domain completion requires evaluated uncertainty, not majority vote of agreeable agents.

### 5. Scenario sub-orchestrator

**CURRENT:** `wargaming-orchestrator`, `scenario-player`, `systems-modeller` and `scenario-wargaming` skill. **TARGET:** intent-driven scenario branch construction. A parent gives decision question, candidate drivers, horizon and constraints; the agent selects dissimilar plausible worlds, relevant players and potential shocks *conditional on fresh evidence*. A newly falsified assumption reorients the world set; it must not always play a fixed count of moves just to satisfy a recipe. Keep role-local knowledge isolation and distinguish observed facts from hypothetical moves. Return robust options, world-dependent outcomes, signposts and limits; no fictitious “probability” unless calibrated from data.

### 6. Matrix Game: retain game engine authority

**CURRENT:** `matrix-game-master` is its own UIA-spawned RootOrchestrator, per `harw-registry-defaults/agents/matrix-game-master.toml`; `harw-ops/src/matrix/game_master.rs` and `docs/design/matrix-game.md` own deterministic game phases, dice, seats and approvals. `matrix.start/run/finish` remain approval-gated; the game master cannot acquire unrestricted network/shell or become arbitrary child of a different root.

**TARGET v1 (compatible):** an intent-addressable Matrix Game facade from UIA, wrapping existing Master and engine. An optional `matrix-research-suborchestrator` prepares the evidence snapshot *before* UIA admission; because UIA cannot directly spawn arbitrary ChildOrchestrators and Master is root, orchestration must respect route ownership and perhaps run sequential handoffs with typed evidence package instead of pretending an illegal root-to-root hierarchy. A future `matrix-suborchestrator` may only exist after explicit role and authority design/test, not by relabeling the master.

**Cycle:** intent → fact/evidence research → scenario proposal → independent assumption check → UIA approval → deterministic seat/round engine → inspect actual simulated moves → optionally reorient research questions / inject sourced events under policy → next game phase or termination → engine-authentic AAR → evidence-qualified report. Dice/randomness and material state changes are engine owned; model only chooses among permitted requests. A simulated surprise can trigger a new *analysis* subchain, not a fabricated game rule. Reject claims that a game is a forecast. A new user request mid-game is a versioned intent event, never an unlogged mutation to the scenario.

### 7. Business writing: claims first, edits second

**CURRENT:** `business-writing-pyramid` skill defines answer-first SCQ, logical grouping and evidence per claim. `author-review-pipeline` and `business-author` / `business-reviewer` separate draft, independent review and final approval. **TARGET:** `business-paper-composed-agent` accepts audience, decision, evidence set, narrative contract, scope and output. It builds a claim graph, asks for missing/rebutting evidence, drafts a bounded storyline, has an independent review, and reorients sections when evidence invalidates a claim.

Writing-cycle states: `Researching` → `StorylineProposed` → `StorylineReviewed` → `DraftInProgress` → `EvidenceReviewed` → `Approved`. These are *milestones*, not an obligatory order of repeated generation; a late source conflict may return to research. The reviewer never self-writes the approved content and the author never silently marks its own unsupported assertion reviewed. Draft writes occur in small sections with stable IDs and immutable approved storyline revision.

**Typed handoff:** `PaperPackageV1` contains audience, question, approved claims and evidence IDs, narrative hierarchy, versions, approved reviewer verdict, sections, citations, open caveats, output metadata. A reorientation invalidates only affected sections and triggers scoped review.

### 8. Scientific-business paper and LaTeX output

**CURRENT:** `latex-report`, `latex-writing`, `xelatex-compile`, `uia-latex-writer`, `latex.template/check/build`, `harw-report.sty` and `business-paper.tex`. UIA spawns this specialized UiaWorker; Root and ChildOrchestrator cannot spawn it. **TARGET:** a paper generation recipe hands a *reviewed* package back to UIA and then to the LaTeX worker. A `research-paper` template would extend current template-kind catalogue without breaking Matrix's existing `business-paper` consumer.

Rendering algorithm: validate package → materialize semantic section sources → render bounded chapters → compile without shell escape → inspect diagnostics/page layout → fix only typography/source encoding → recompile up to explicit iteration cap → report PDF/TeX/Markdown provenance. Failed compilation cannot authorize rewriting business claims. Truncated LLM output is solved through sectional writing and checkpoints, not larger single tool responses. Every scientific claim retains citation; mock citations are not acceptable.

### 9. Coding, debugging and adaptive Bug Hunt

**CURRENT:** `coding-orchestrator`, `planner`, `explorer`, `executor`, WorkDriver, `docs/planning/85-gap-hunt/kit/` operational Claude-style workflows. Gap hunt is a concrete useful **adaptive control policy** and must be translated into typed chain transitions, not blindly copied as a fixed list of stages.

**Target top-level intent:** diagnose/fix defects satisfying criteria/invariants, pinned base SHA, write boundary, approval and central verification. Example:

```text
Scout repo areas
   ↓
Parallel read-only hunters → adversarial critics
   ↓
normalized verified findings
   ├─ false positive → reject with evidence (stop branch)
   ├─ single-file → reserve one file → fix → review
   ├─ cross-file → contract with exact paths → per-file writers
   └─ unclear → new targeted read-only inquiry
                     ↓
              path-specific ripple review
                     ├─ findings → reorient new affected-file contract
                     ├─ missing verification → blocked
                     └─ clear
                           ↓
                     central build/gates
                     ├─ red → diagnostic frontier (new bounded fix cycle)
                     └─ green → record frozen-SHA gate evidence
                           ↓
                      propose completed
                     (human approval if required)
```

Copy the *properties* of `gap-hunt-area`, `gap-fix`, `gap-verify`, `contract-wave` and `layered-wave`: read-only scouting, differentiated verification by severity, disjoint path ownership, separate worktrees/branches, immutable wave manifest, frozen central build record and human-only goal achievement. A model may propose one-file fix but a dependency audit can show the fix spans multiple crates: reclassify to contract wave, with approvals/rights preserved. It may change research strategy when code refutes a finding. No subagent independently runs cargo when the authorized workflow prescribes centralized gates. Do not `git add -A` and sweep others' changes.

**Failure handling:** `401/403` model route → provider preflight; repeated failed tool → circuit breaker; readonly profile but write requested → verified escalation, not shell; external API returns contradictory facts → evidence critic. Track `time_to_first_valid_evidence`, `verified_findings_per_token`, `repeated_errors`, `patch_regressions`, `scope_conflicts` and `successful_accepted_diffs`.

### 10. Dream, Diary, Palace, Learning and global context

**CURRENT:** independent knowledge surfaces; on dev `harw-context-ledger`, `turn_feedback`, `job_worker_learning` and `job_worker_memory` implement capture/learning/maintenance components. **TARGET:** explicit cognitive maintenance segments can be scheduled or nested by evidence/state triggers without bypassing privacy or scope. Examples: low coverage → retrieve relevant Palace nodes; large context → summary compaction with source handles; completed task → Diary event; repeated verified correction → learning extract candidate; contradictory memory → resolve or quarantine; global memory promotion → separate privileged approval and deduplicated index path. Session-local/Project/Global scopes remain separately authorized and synchronized. No raw model CoT persistence.

### 11. DoD — Detect, Orient, Defend

DoD is a peer security domain with a minimal privileged TCB; it is not a free-floating “AI security agent”. **TARGET:** Detect observations are immutable source-classified events; Orient uses read-only bounded analytical agents to build hypotheses and risk scenarios; Defend proposes an allowed response with human policy/approval and separate privileged executor. If orientation discovers a new attack path it can reorient detection queries; it cannot grant itself new network isolation/host kill powers. Safeguard precedence: host authorization and evidence verification over model confidence. Track decisions, evidence, policy version and irreversible side effects.

### 12. Additional future families

A `security-inspection-suborchestrator` can compose current auditor/scanner/triager specialists; `incident-forensics` explores logs and causal graphs; `docs-and-diagram` produces verified documentation/source links; `model-lifecycle` researches deprecations before opening reviewed update PRs; `cloud-gateway` maps endpoints/routes with bounded read rights; `dependency-upgrade` coordinates cargo source review with central build. These are candidate recipes/family registrations. Name each only after exact type, authority, route and owner are verified. Avoid a vast context-injected role registry: dynamically retrieve a small relevant catalogue.

## Why these integrations share one runtime but not one workflow

Each composed capability shares durable state, transitions, capability preflight, authorization intersection, scheduling and checkpoint semantics. Each domain owns the meaning of successful evidence, allowed side effects and acceptance. Research does not need filesystem writes; Matrix must not fake engine turns; LaTeX must not change approved claims; coding must be allowed to edit scoped files; DoD privileged defense must remain a separately approved act. Reuse the mechanism; keep domain authority and validation distinct.
