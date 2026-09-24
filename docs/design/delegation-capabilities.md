# Delegation capabilities and the local organization graph

> Status: implemented · Last reviewed: 2026-09-24

## Purpose

An organizational role is a global security ceiling, but neither an agent
catalog nor a model-facing surface. An agent may only know and request target
agents for which its own, already-verified definition issues a concrete
delegation capability at runtime.

This contract prevents two distinct classes of error:

1. A model could guess an existing role not meant for its position, or infer
   one from an error message.
2. A blanket `ChildOrchestrator → ChildOrchestrator` permission could become,
   by accident, an unbounded recursive delegation authority.

## Terms

- **Organizational role**: a closed category (`UserInterface`,
  `RootOrchestrator`, `ChildOrchestrator`, `Worker`, `UiaWorker`,
  `AgentSteward`). The role matrix is only a necessary upper bound.
- **Agent definition**: a versioned, trustedly-embedded specialization with
  role, tool surface, budget, and spawn contract.
- **Delegation capability**: a concrete authority, transferred from a parent,
  to start exactly one target definition within tighter bounds.
- **Child lease**: the runtime resource created after successful admission of
  a child. A lease is never created directly from model JSON.

## Visibility

The visible delegation set of an agent is:

```text
DefinitionDeclaredTargets
∩ RoleMatrixTargets
∩ ParentGrantedTargets
∩ RemainingDepth
∩ RemainingBudget
∩ AuthorityCeiling
∩ ContextCeiling
∩ ReadWriteScopeCeiling
```

Only this intersected set is exposed as a tool parameter and as local
organizational knowledge. If it is empty, there is no spawn/delegation
surface and no agent catalog.

The intersection is monotone: no child may hold more authority, context, network
or filesystem access, tool rights, budget, or remaining depth than its parent.

## Role projections

| Role | May know via delegation |
| --- | --- |
| UIA | Only explicitly issued entry capabilities, normally `activate-root`. No worker or sub-orchestrator names. |
| Root orchestrator | Its own concrete worker and subtree capabilities, plus responsibility for goal, budget, and synthesis. |
| Sub-orchestrator | Only its delegated subtree, local workers, and explicitly passed-down subtree capabilities. No siblings or root-exclusive targets. |
| Worker | No persistent delegation and no agent catalog. |
| `uia-worker` (`UiaWorker`) | Its own, fully encapsulated organizational role; only the UIA may spawn it, and it has no persistent delegation or agent catalog of its own. |
| `agent-steward` (`AgentSteward`) | An internal agent that explicitly owns and applies knowledge of agent definitions; both the UIA and the root orchestrator may spawn it, and it has no persistent delegation or agent catalog of its own. |

## Recursive sub-orchestration

`ChildOrchestrator → ChildOrchestrator` in the role matrix does not mean every
child orchestrator can start further child orchestrators. Such an admission
additionally requires an exact target definition that

1. is declared in the parent's own definition,
2. was actually passed down to this parent's run, and
3. lies within every remaining ceiling.

The runtime treats a nonexistent target as invisible. On rejection it must not
disclose names, roles, or other properties of further registered agents.

## UIA routing

The UIA is a local, interactive entry point. It never creates a worker
directly. Delegable work is handed as a work intent to a visible root
capability:

```text
UIA → root orchestrator → worker or bounded sub-orchestrator subtree
```

A rejected UIA→worker admission is therefore not a normal runtime path, but a
state that must never be exposed.

## Migration state

The existing role matrix and the exact `allowed_child_orchestrators` check
remain, as a fail-closed admission boundary, in place during migration.
Follow-up work replaces free role-based model input with
capability-identified requests and projects the visible set into tool schemas
and context programs.

## Root vs. sub-orchestrator: positional, not categorical

Project stance (a binding decision by the project owner, not the subject of
this analysis):

> A root orchestrator is an orchestrator that is only called that because
> there are sub-orchestrators below it.

The `RootOrchestrator`/`ChildOrchestrator` distinction is therefore
**positional** (place in the tree: does the agent itself have an orchestrator
parent, or is it the root of the current assignment), not **categorical**
(not a fundamentally different rights class with capabilities the other role
is categorically denied).

### Current deviations from the target architecture

The current code treats the two roles as categorically distinct in three
places. These are not bugs and not a justification for the distinction — only
the documented current state against the target above:

1. **Spawn matrix — `AgentSteward` only from `RootOrchestrator`.**
   `harw-agent-dsl/src/roles.rs`, function `can_spawn`: the
   `(RootOrchestrator, AgentSteward) => true` arm has no matching
   `ChildOrchestrator` arm, so `(ChildOrchestrator, AgentSteward)` falls
   through to `(ChildOrchestrator, _) => false`. A `ChildOrchestrator` cannot
   spawn `AgentSteward`, even when it positionally acts as the root of its
   own subtree.
2. **Different default reasoning-effort weights.**
   `harw-core/src/child_controller.rs`, `struct RoleEffortWeights` with
   `impl Default for RoleEffortWeights`: `root_orchestrator: High`,
   `root_orchestrator_with_subs: Medium`, `sub_orchestrator: Medium`.
   `RoleEffortWeights::for_child` always reads `self.sub_orchestrator` (fixed
   `Medium`) for `ChildOrchestrator`, while `RootOrchestrator` only drops to
   `Medium` when it itself carries sub-orchestrator grants
   (`has_child_orchestrator_grants`). A `ChildOrchestrator` with no
   sub-orchestrator grants of its own therefore gets the same weight as a
   `RootOrchestrator` WITH such grants — not the same as a `RootOrchestrator`
   without them.
3. **`ChildOrchestrator` has no built-in instance.** Under
   `harw-registry-defaults/agents/`, no agent currently carries
   `role = "child-orchestrator"` as an actual role assignment. The string
   only appears in two justification comments —
   `harw-registry-defaults/agents/families/security/security.toml` and
   `harw-registry-defaults/agents/organization/default.toml` — both
   explaining why the leader role described there (`analyst` and
   `context-steward` respectively) carries `role = "worker"` instead of
   `role = "child-orchestrator"`. The role therefore exists only in the type
   system (`AgentRoleId::ChildOrchestrator`) and in the spawn matrix, not in
   any configuration actually in use.

### No rework in this pass

Merging the two into a single orchestrator role with a positional marker
(instead of two separate `AgentRoleId` variants) is a **separate, not yet
started** piece of rework. It touches the spawn matrix (`can_spawn`) and its
tests in `harw-agent-dsl/src/roles.rs` directly, as well as
`RoleEffortWeights` in `harw-core/src/child_controller.rs`.

This unification is deliberately **not** done alongside the current
spawn-delegation fix: running both changes at once would make it impossible
to tell, if a problem surfaced, whether the cause was the delegation fix or
the role rework. The unification follows as its own, separately testable
step once the spawn-delegation fix is complete and verified.
