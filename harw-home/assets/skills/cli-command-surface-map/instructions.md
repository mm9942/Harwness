# Harw command-surface map

**Rule:** Surface identity is provenance, not spelling. Two commands with the same word may be unrelated; two different spellings may deliberately share one Operation.

## Surfaces to distinguish

1. **Process CLI** — `harw ...`; grammar comes from clap under `harw-cli/src/cli`.
2. **Session/TUI command** — `/... `; normally derived from an Operation's `Surface::Command`, plus genuinely TUI-local commands/prefixes.
3. **Model tool** — an Operation exposed through model-tool metadata/adapters. Availability and approval can differ from slash visibility.
4. **Service/control runtime role** — the process/runtime started or managed by an invocation, for example Gateway, MCP serve, Web control socket, or lifecycle manager. This is a runtime role, not automatically another syntax.

One invocation can occupy more than one row: for example a root CLI wrapper may bridge into an Operation, while a daemon command starts a distinct service composition.

## Mapping procedure

For every spelling under review, record:

| Field | Question |
|---|---|
| spelling | What exactly does the user type? |
| syntax owner | clap enum, TUI catalog, Operation metadata, prefix parser, other? |
| adapter | direct dispatch, `op_bridge`, `CommandAdapter`, model adapter, lifecycle adapter? |
| handler | Which concrete function/module owns behavior? |
| runtime role | Chat/TUI, Analyze bridge, Gateway, MCP serve, Web, lifecycle, other? |
| authority | Which principal/tier/approval actor/policy is used? |
| state | Which stores/files/session objects are read or mutated? |
| output | text, JSON, TUI surface, event stream, exit code? |
| aliases/parity | Is this an alias, compatibility spelling, or merely a same-name command? |

## Hard checks

- A root `Command` variant proves grammar only.
- A `Surface::Command` proves slash visibility only when the Operation is registered in the assembled runtime.
- A model-tool name proves model exposure only when the tool adapter/profile admits it.
- A service name does not imply that `harw service`, `harw gateway`, `harw serve` and `harw web` share one runtime.
- `harw analyze` and `/analyze` are a canonical same-name case: trace both before claiming equivalence.
- `/plan`, `/goal` and WorkDriver operations may be live while no corresponding root clap subcommand exists.
- Shell wrappers such as `harw jobs` may intentionally translate to differently named slash Operations.

## Output

Prefer a table and one short conclusion. Label relationships as one of:

- **same handler**
- **adapter to same Operation**
- **same domain, different handler**
- **same spelling, different surface**
- **compatibility alias**
- **no corresponding surface**

Never use "equivalent" without showing the handler/adapter evidence.
