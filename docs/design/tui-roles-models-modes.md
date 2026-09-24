# Roles, Models, Modes, and Approval in the TUI

> Status: implemented · Last reviewed: 2026-09-24

Binding for the TUI and `harw-ops`. Related: `tui-command-contract.md` §8 (commands, keys, prefixes).
Sources: `harw-config/src/role_models.rs`, `harw-config/src/internal_models.rs`,
`harw-config/src/uia_worker_models.rs`,
`harw-ops/src/{models,model,mode,effort}.rs`,
`harw-extension-api/src/approval_mode.rs`, `harw-runtime/src/approval.rs`
(`ApprovalChain::for_child`), `harw-runtime/src/assembly.rs`
(`effective_approval_mode`), `harw-runtime/src/config.rs`
(`apply_model_override`), `harw-cli/src/main.rs` (`resolve_startup_mode`).

---

## 1. Model per role

`harw_config::resolve_role_models(&config)` returns exactly one row per
`ModelRole`: `RoleModelRow { role, provider, model, source, reasoning_effort }`.
`/models show` and the F8 view display exactly this table, plus the running
session's live model.

| Role (`key`) | Label | Configuration | Possible origin (`RoleModelSource`) |
|---|---|---|---|
| `uia` | User Interaction Agent (UIA) | `uia_provider` + `uia_model` | `UiaPin` (both set **and** the provider exists and is enabled), else `DefaultModel`, else `Unset` |
| `uia-worker` | UIA worker | `[uia_worker_models] uia_worker`, else the older pin `uia_worker_model` | a fixed choice (`provider/model`) or `InheritsUia` (value `"uia"`). No longer coupled to the UIA provider — see §1.1 |
| `orchestrator` | Orchestrator | `[internal_models.root_orchestrator]` | `Explicit`, else `DefaultModel`/`Unset` — **never** the OpenRouter default |
| `sub-orchestrator` | Sub-orchestrator | `[internal_models.sub_orchestrator]` | `Explicit`, else `InheritsOrchestrator` (the orchestrator row's provider/model) — never the OpenRouter default |
| `worker-simple` | Worker (simple) | `[internal_models.worker_simple]` | `Explicit` / `OpenRouterDefault` / `DefaultModel` / `Unset` |
| `worker-complex` | Worker (complex) | `[internal_models.worker_complex]` | as above |
| `explorer` | Explorer | `[internal_models.explorer]` | as above |
| `research` | Research | `[internal_models.research]` | as above |
| `compaction` | Compaction | `[internal_models.compaction_summary]` | as above |
| `title` | Session title | `[internal_models.session_title]` (legacy: `session.title_model`) | as above |
| `memory` | Memory consolidation | `[internal_models.memory_consolidation]` | as above |
| `dream` | Dream reflection | `[internal_models.dream_reflection]` | as above |
| `auto-classifier` | Auto-mode classifier | `[internal_models.auto_classifier]` | `Explicit`, else the active provider's fast model (Anthropic: `claude-haiku-4-5`; otherwise by name markers like `haiku`, `mini`, `flash`); never the OpenRouter default |

Origin in detail:

| `RoleModelSource` | Label | Meaning |
|---|---|---|
| `Explicit` | explicitly chosen | `[internal_models.<slot>]` with `model` set |
| `OpenRouterDefault` | OpenRouter default | no explicit choice, `use_openrouter_defaults = true` (the default) and a provider named `openrouter` is enabled and authenticated. Model: `nvidia/nemotron-3-super-120b-a12b` for explorer, research, worker (complex); `nvidia/nemotron-3.5-lightning` for title, compaction, memory, dream, worker (simple) |
| `UiaPin` | UIA pin | `uia_provider`/`uia_model` |
| `UiaWorkerPin` | UIA worker pin | `uia_worker_model` |
| `InheritsUia` | inherits from UIA | a UIA worker with no pin of its own |
| `InheritsOrchestrator` | inherits from the orchestrator | a sub-orchestrator with no choice of its own |
| `DefaultModel` | default model | `default_provider`/`default_model`. For internal slots this means, at runtime: the caller uses its own active model (for children, the parent's model) |
| `Unset` | unset | no `default_model` configured |

Special case: an `[internal_models.<slot>]` entry **without** `model` (even
with `provider` set) forces the main model (`DefaultModel`), regardless of
`use_openrouter_defaults`.

Reasoning effort per row: a role-specific `[reasoning]` field (`uia`,
`root_orchestrator`, `sub_orchestrator`, `worker_simple`, `worker_complex`),
else the model's `default_reasoning_effort`, else the provider's, else empty.

Runtime mapping of child roles: `explorer` → Explorer; `researcher-web`,
`researcher-deps`, `analyst` → Research; `memory-steward` → Memory;
`agent-steward` → Worker (complex); orchestrator definitions via their
organizational role (`root-orchestrator` → Orchestrator, `child-orchestrator`
→ Sub-orchestrator); workers by task complexity (simple/complex); the
`uia-worker` family (`uia-worker`, `uia-explorer`, `uia-writer`,
`uia-shell-worker`) gets its model from the UIA session.

### 1.1 Per-role model choice for UIA workers

Every role in the `uia-worker` family has its own choice under
`[uia_worker_models]` (TOML keys use `_` instead of `-`):

```toml
[uia_worker_models]
uia_worker       = "uia"                       # follows the UIA (also follows a live switch)
uia_shell_worker = "anthropic/claude-sonnet-5" # fixed choice, split at the first `/`
# uia_writer, uia_latex_writer, uia_explorer
```

- Without an entry, the older pin `uia_worker_model` is treated as a fixed
  choice, now paired with the provider that owns the model in the catalog,
  otherwise "same as UIA."
- A fixed choice holds only as long as its provider is authenticated;
  otherwise the role falls back to "same as UIA" with a notice. A switch of
  the UIA provider therefore never gets stuck on a worker binding.
- A running worker keeps its model until its run ends; new workers pick up
  the new choice.
- Set via `/models worker [<role|all> <uia|target>]` or in `/models` (F8):
  after the UIA choice, the "UIA worker models" section opens directly
  (Enter picks a role's model, `a` sets all to "same as UIA", Esc closes).

## 2. When a model change takes effect

**Rule: every model and effort choice takes effect from the next session —
except `/model` (and `/effort`), which change the running session live.**

| Command | Writes | Effect |
|---|---|---|
| `/model switch <id>` | the session's live state (provider + model, atomically, even across providers) | **immediately** (from the next turn) |
| `/effort <level>` | the session's live state | immediately |
| `/models set <role> <target>` | the profile's `config.toml` (`uia_*`, `uia_worker_model`, or `[internal_models.*]`) | from the next session |
| `/models reset <role>` | removes the explicit choice | from the next session |
| `/models worker <role\|all> <uia\|target>` | the profile's `config.toml` (`[uia_worker_models]`) | new workers immediately, running ones keep their model |
| `/uia-model`, `/uia-worker-model`, `/uia-effort` | the profile's `config.toml` | from the next session |

`/models set uia-worker` also accepts models from other providers. `/mode`
no longer changes the model. None of these commands has a model tool: a
model may choose neither its own model nor its children's.

## 3. Mode, approval, and Shift+Tab

Two independent axes:

| | Interaction mode (`InteractionMode`) | Approval mode (`ApprovalMode`) |
|---|---|---|
| Values | `chat`, `plan`, `explore`, `work`, `shell` | `ask` (`AlwaysAsk`), `auto` (`Delegated`), `full` (`FullAccess`) |
| Controls | which tools the model sees, and the sandbox ceiling | whether an otherwise-allowed tool call still asks |
| Change | `/mode <mode>`, F7 ("Mode" section) | `/permissions mode <ask\|auto\|full> [--session\|--project\|--global]`, F7 ("Approval" section, `--session` scope) |
| Takes effect | at the **next turn boundary** (queued; status line shows "(pending)") | **immediately**, even mid-turn |
| Default | `[mode] default` (defaults to `chat`), persisted via `/mode default <mode>` or `d` in F7 | `[permissions].default_mode`, else `auto` |

`Shift+Tab` quick-cycles both axes:
`ask → auto → full → plan → ask`. The `plan` step remembers the current
mode, sets approval to `ask`, and requests mode `plan`; the next step sets
only the approval back and restores the remembered mode. `Shift+Tab` has no
effect while a `/`-popup is open. Status line:
`Mode: <mode> · Approval: <ask|auto|full>`.

### 3.0 Plan mode

The `plan` step is a full-fledged plan mode:

- Status line "⏸ plan mode on (shift+tab to cycle)" in its own color;
  a composer hint "Plan mode – nothing will be changed."
- The lock takes effect **immediately**, even mid-turn (`PlanModeGate`):
  nothing that writes, no execution. The only write tool is `plan.write`,
  and it writes only under `.harw/plans/<slug>.md`.
- The agent may start read-only children (`explorer`/`researcher`, at most
  3 in parallel) and ask structured follow-up questions via `ask_user`
  (root only, TUI only).
- `plan.exit` opens a window with the rendered plan and three options:
  1. implement in auto mode → mode `work`, approval `auto`;
  2. implement, approve changes individually → `work`, `ask`;
  3. keep planning, with free-text feedback to the agent.
- `plan.enter` is only a suggestion from the agent; only "yes" switches
  the mode.
- The approved plan stays as pinned context on every request and survives
  compaction. `/plan show|list|open|edit` manages plan files; `/plan` or
  `/mode plan` switches into it.

### 3.0.1 Auto mode

In `auto`, the runtime releases whatever is listed in `AUTO_APPROVED_TOOLS`.
For everything else, this order applies:

1. `ALWAYS_ASK_TOOLS` (`process.kill`, `host.sudo_exec`, `agent.cancel`, …)
   always ask.
2. Deny rules, then allow rules (`[[permissions.deny]]`/`[[permissions.allow]]`
   with `tool` and optionally `match` or `path`). Deny rules also apply in
   `ask`.
3. A deterministic pre-filter: writes outside the workspace, into `.git`/
   `.harw`, or to credential paths; `rm -rf` outside the workspace;
   `git push --force`; `curl … | sh`; network access outside policy — a
   match is never an approval.
4. The classifier (role `auto-classifier`, no tools, secrets stripped
   beforehand) decides `allow|ask|deny` with a category and a reason.
   An error, a parse failure, or taking more than 10s → falls back to
   asking.

The tool cell shows "auto ✓ <reason>" or "rejected by auto mode ·
<category>"; `/permissions log` lists recent decisions. After 3 consecutive
rejections, or 20 in the session, the session falls back to `ask`. From the
third similar manual approval on, the dialog offers "yes, and allow from
now on: <pattern>" (session or project scope), never for `ALWAYS_ASK_TOOLS`
or risky patterns.

### 3.1 Children follow approval live, capped at `auto`

`ApprovalChain::for_child` gives every child a **follower cell**
(`ApprovalModeCell::follower(ApprovalMode::Delegated)`): on every check, the
child reads the parent cell's current mode, capped at `auto`
(`ApprovalMode::capped_at`). Consequences:

- Root `ask` → children `ask`; root `auto` or `full` → children `auto`.
- A change at the root (command, F7, Shift+Tab) reaches already **running**
  children immediately.
- A child never gets `full`.
- A local `set` on a child's cell detaches that child; it stays capped,
  and its parent and siblings are unaffected.

Allow rules (`AllowRuleSet`) are shared unchanged (a rule never lets a
child do more than its own tool surface permits).

## 4. Precedence at startup

Applies to `harw`/`harw chat`, `harw exec`, and `harw analyze`.

| Setting | Precedence (highest first) | On error |
|---|---|---|
| Interaction mode | `--mode` > `[mode] default` (defaults to `chat`) | an unknown name is an error in **both** cases (no silent fallback to `chat`) |
| Root agent | `--agent` > `active_agent_definition` (persisted via `/agent use <name>`, cleared via `/agent use --clear`, both from the next session) | an unknown name is a startup error (fail-closed); `/agent use` checks the name and role (root, child orchestrator, or worker) before saving |
| Approval mode | `--approval` > project `[permissions].default_mode` > global `[permissions].default_mode` > the entry point's default (`auto` for all entry points) | an invalid `--approval` value is a parser error; an invalid config value is skipped |
| Model | `--model` > `default_provider`/`default_model` (or a UIA pin) | `--model` is checked against `config.models`: first as a catalog key, then a model ID, then an alias (with multiple matches, the alphabetically first key wins); no match is a configuration error |

`--model` sets `default_model` (a catalog key) and `default_provider` (that
entry's provider) for this run, and lifts a UIA pin (`uia_provider`/
`uia_model`) for this run, so the choice also applies to the interactive
session. The remaining roles follow from that per §1 (e.g. `DefaultModel`,
`InheritsUia`). None of this is persisted.

## 5. Open

- Showing the model in agent orchestration events
  (`AgentOrchestrationEvent.model`, `ChildRecord.model`) for displaying a
  child's model: planned, not yet implemented.
