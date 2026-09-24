# Agent Capabilities Audit

> Status: partially implemented · Last reviewed: 2026-09-24

**Original scope:** 6 checkpoints, checked statically (offline, no API key
available in that environment); live-only points were marked as such.
**This update:** each finding below has been re-checked against the
current code; three of the four original backlog items are now resolved.

## Result overview

| # | Checkpoint | Status | Evidence |
|---|-----------|--------|----------|
| 1 | Tools visible through to `ModelRequest.tools` | Implemented | `harw-core/src/model.rs` — `pub tools: Vec<ToolSpec>`; `turn_loop::collect_tools` gathers every `ToolProvider::tools()` |
| 2 | MCP tools in the tool merge | Implemented | `harw-mcp-client/src/tool_bridge.rs` — `McpToolProvider` now implements `ToolProvider` directly, feeding MCP-discovered tools into `collect_tools` like any other provider |
| 3 | Skills load path | Implemented | `harw-ops/src/skills.rs` — `list`/`show`/`activate`/`deactivate` are real, backed by `SkillStatePersistence`/`LayeredSkillStatePersistence`, plus a proposal review workflow (`proposals`/`review`/`accept`/`reject`) |
| 4 | System/role prompts from config | Implemented | `turn_loop::load_instructions` iterates `session.registry().instructions_providers()`; `LoadedInstructions.system_prompt` maps into `ModelRequest.system_prompt` |
| 5 | Agent role configs (`registry_factory`, `capability_snapshot`) | Implemented | `child_controller.rs` — `capability_snapshot()` as a trait method, `registry_factory: Arc<dyn ChildRegistryFactory>`, both consulted in `admit()` |
| 6 | parent→child→grandchild routing | Implemented at the admission/depth level; live round-trip still unverified | `harw-core/tests/child_controller.rs` has dedicated grandchild-admission tests (e.g. `admit_propagates_the_root_trace_id_across_a_grandchild`, `a_role_that_forbids_grandchildren_is_still_admissible_as_a_child`); a full 3-level turn against a real model provider has not been re-confirmed in this pass |

## Detailed findings

### (1) Tool visibility → `ModelRequest.tools`

`collect_tools(session)` in `harw-core/src/turn_loop.rs` gathers the tool
specs of every `ToolProvider` in the session registry and deduplicates by
function name. The turn loop attaches the result to `ModelRequest.tools`.
The provider bridge forwards `tools` as the OpenAI/Anthropic wire field.
**Fully wired through** — any tool provider that joins a session registry
is visible to the model.

### (2) MCP → `ToolProvider` — resolved since the original audit

At the time of the original audit, `mcp_runtime` only offered plan/spawn
utilities (planning and spawning stdio/HTTP MCP servers), with no
`ToolProvider` adapter feeding MCP-discovered tools into `collect_tools` —
the bridge from MCP to the tool registry was assembled externally by the
extension loader, not demonstrated end to end.

**Now implemented:** `harw-mcp-client/src/tool_bridge.rs` defines
`McpToolProvider`, which implements `ToolProvider` directly. MCP tools are
merged into the callable tool set the same way any other provider's tools
are.

### (3) Skills — resolved since the original audit

At the time of the original audit, `/skills`' operation surface existed
(channel-parity command, read-only model-tool visibility) but its `run()`
body was a stub, with a TODO marking real skill listing/activation as
future work.

**Now implemented:** `harw-ops/src/skills.rs` implements `list`, `show`,
`activate`, and `deactivate` for real, toggling a skill manifest's
`enabled` field through `harw_config::ConfigWriter` (comment-preserving,
atomic, with backup) — the same persistence path `/permissions` and
`/model switch` use. A further layer beyond the original scope now exists
too: a skill-proposal review workflow (`proposals`, `review <id>`, `accept
<id>`, `reject <id> [reason]`), reading and deciding on skill proposals
stored under a profile's `skills/.proposals/` directory.

### (4) System/role prompts

The pipeline `InstructionsProvider → LoadedInstructions.system_prompt →
ModelRequest.system_prompt` is coherent. The first non-empty system
instruction wins (a deliberate first-nonempty rule rather than
concatenation, to avoid duplicates). The role determines, via the
registry, what arrives here.

### (5) `registry_factory` + `capability_snapshot`

Both symbols are live and are actually consulted in `admit()`
(`child_controller.rs`). The test suite
(`harw-core/tests/child_controller.rs`) covers both accept and reject
paths.

### (6) Nested routing — admission-level tests now exist; live round-trip still open

The `parent_depth()` guard protects against infinite recursion; all
existing depth tests pass. **What was missing at the time of the original
audit** was any test exercising a 3-level turn (parent → child →
grandchild → result bubbling back up) — undemonstrable offline, though the
test infrastructure (`ManagedAgentSpawner`, `SessionManager`) existed.

**Now implemented, at the admission/depth-accounting level:**
`harw-core/tests/child_controller.rs` has dedicated grandchild tests,
including trace-ID propagation across a grandchild admission and grandchild
admissibility under a role that itself forbids further grandchildren. These
confirm the admission/depth machinery handles three levels correctly.
**Still open:** an actual live turn round-trip through three levels against
a real model provider (parent turn → child turn → grandchild turn →
response bubbling up) was not re-verified in this pass and would need a
live provider to confirm end to end.

## Live verification (requires an API key)

Not completed without a live provider. When available, these should run:

1. `/status`, `/help`, `/memory list` — already verified offline.
2. A turn with a real tool call against a provider (round trip: tool
   schema sent → model requests a tool call → result → second model call →
   final answer).
3. A turn with an MCP-registered tool (now structurally ready per finding
   2 above).
4. `/agent spawn <role>` with a role that itself calls a sub-agent tool
   (levels: turn → child turn → grandchild turn).
5. A reasoning-effort trace: `--log debug` plus `/memory record reflection
   ...`, observing `agent.turn` spans with `reasoning_effort=medium|high`
   across the three levels.

## Remaining backlog

- ~~MCP `ToolProvider` adapter~~ — resolved (finding 2).
- ~~Real skill loader~~ — resolved (finding 3).
- **Grandchild live round-trip** — admission/depth logic is tested;
  a live, three-level turn against a real provider is still unverified
  (finding 6).
- Auto-detection of correction signals from user turns in `harw-memory` —
  not re-checked in this pass; carried forward unverified.
