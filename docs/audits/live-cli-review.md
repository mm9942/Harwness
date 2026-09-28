# Live CLI review — `harw` on `dev`

> Review type: static reviewer pass over the live command surface  
> Reviewed ref: `dev@79b471d9b64544714d74306b3356711daf8c0f7a`  
> Date: 2026-09-27  
> Scope: the current `harw` binary, its clap grammar, dispatch path, runtime bridge,
> service entrypoints, and its relationship to TUI/slash/model-tool commands.
>
> This is not a roadmap. It records what the code on the pinned ref actually
> does. Planning documents are supporting context only; code, tests and
> executable registries win when they disagree.
>
> No workspace build or live provider call is claimed by this review.

---

## 1. Executive review

The current Harw command surface is easy to misread if one starts from
`docs/cli.md` or the TUI alone.

There are **three different command planes**:

1. **Process CLI** — `harw ...`, parsed by clap in `harw-cli/src/cli/*`.
2. **Session command plane** — slash commands such as `/goal`, `/plan`,
   `/memory`, `/approve`, derived from registered Operations plus a small
   TUI-local command set.
3. **Model/tool plane** — Operations exposed to the model as tools, including
   planning and WorkDriver tools when the plan feature is enabled.

They overlap deliberately, but they are not aliases of one another.

The most important reviewer consequence is:

```text
/goal achieve "finished"
/plan inspect
/work_driver status
```

are live session commands/tools while:

```text
harw goal achieve ...
harw plan inspect
harw work-driver status
```

are **not** root clap subcommands on this ref.

Conversely, `harw gateway`, `harw service`, `harw web`, `harw serve`,
`harw auth`, `harw provider`, `harw agent build`, and `harw kill`
are process-level CLI entrypoints with behavior that is not represented by a
same-named slash command.

This split is sound, but it needs to be documented as an architecture boundary,
not treated as incidental CLI trivia.

---

## 2. Process entry: what runs before a command

The executable is intentionally tiny:

```text
harw-cli/src/main.rs
    -> harw_cli::main_entry()
```

The effective startup path in `harw-cli/src/lib.rs` is:

```text
install compiler defaults
        |
raw argv special-case for "harw kill"
        |
parse clap grammar
        |
apply -C / --profile process globals
        |
decide whether this invocation starts the TUI
        |
resolve logging config / RUST_LOG / --log
        |
install tracing
        |
TUI only: trigger non-blocking UIA auto-build
        |
dispatch(cli)
        |
exit 0 or 2
```

### Reviewer note: `kill` is intentionally outside the normal path

`harw kill` is recognized from raw `argv` before normal clap parsing and
before Harw tracing is initialized.

That is not accidental duplication. Killer owns its own grammar and logging.
Forwarding the tail unchanged prevents root-global options such as `--log`
or `--verbose` from stealing flags that belong to killer. The sudo helper can
re-enter the same executable through the hidden helper protocol under the
`kill` subcommand path.

Reviewers should therefore inspect `harw-killer` as the command authority for
the tail after `harw kill`, not infer semantics from `harw-cli::Command::Kill`
alone.

---

## 3. Root clap command tree on the pinned ref

The authoritative root parser is `harw-cli/src/cli/mod.rs::Command`.

### Work

- `harw [PROMPT]` — root chat without an explicit subcommand.
- `harw chat [PROMPT]`
- `harw exec PROMPT...`
- `harw analyze ...`
- `harw session list|show|resume`

### Configuration

- `harw config ...` (visible alias: `settings`)
- `harw provider ...`
- `harw model ...` (visible alias: `models`)
- `harw auth ...`
- `harw project ...`

### Agents and knowledge

- `harw agent ...`
- `harw knowledge ...`
- `harw jobs ...`

### Services

- `harw gateway [ACTION]`
- `harw serve`
- `harw web`
- `harw service ...`
- `harw mcp ...`
- `harw channel connect ...`

### System

- `harw init`
- `harw onboard`
- `harw doctor`
- `harw update`
- `harw install --print-systemd ...`
- `harw uninstall ...`
- `harw completions ...`
- `harw bug-report ...`
- `harw sandbox ...`
- `harw debug echo|classify ...`
- `harw kill ...`

### Hidden compatibility spellings

The parser still accepts hidden older variants such as `connect`, `lens`,
`uia`, `catalog`, `run`, and `classify`. Dispatch prints a migration
hint and routes them to the current implementation.

`settings` and `models` are different: they are visible aliases attached
to the current `config` and `model` variants rather than hidden legacy
variants.

---

## 4. Global flags are global syntactically, not semantically

`GlobalArgs` is flattened at the root and marked `global = true`, so its
flags may appear before or after subcommands.

Two classes matter:

### Process-wide flags

- `--home`
- `--profile`
- `-C/--cwd`
- `--log`
- `--log-sensitive`
- `-v/--verbose`
- `--json`

### Session-start flags

- `--mode`
- `--approval`
- `--model`
- `--goal`
- `--agent`
- `--add-dir`

The parser accepts the session flags globally, but
`reject_misplaced_session_flags` rejects them outside chat/exec/analyze
instead of silently ignoring them. `--add-dir` is additionally refused for
`analyze`.

That is a good fail-closed UX pattern: broad clap placement is used for
ergonomics, while runtime semantics remain explicit.

---

## 5. `--json` is a contract, not a formatter switch

`reject_unsupported_json` prevents commands without a JSON contract from
quietly printing human text.

The central allow-path currently includes:
- session list/show paths;
- jobs;
- knowledge;
- agent;
- provider.

Submodules then narrow further. For example provider handling currently
requires text output in `provider_cmd::run`.

Reviewer implication: the central allow-list means "this family is responsible
for deciding", not "every subcommand in this family supports JSON".

---

## 6. The command bridge is the key reuse mechanism

Several shell CLI commands do **not** reimplement their domain logic.

`harw-cli/src/op_bridge.rs` builds a small local runtime and dispatches the
same registered slash Operation through `CommandAdapter`.

Observed shell-to-operation mappings include:

| Shell CLI | Operation path |
|---|---|
| `harw jobs list` | `/ps` |
| `harw jobs show ID` | `/review ID` |
| `harw jobs approve` | `/approve` |
| `harw jobs deny` | `/deny` |
| `harw jobs cancel` | `/cancel` |
| `harw jobs retry` | `/retry` |
| `harw knowledge memory ...` | `/memory ...` |
| `harw knowledge proposals ...` | `/context-proposal ...` |
| `harw agent skills ...` | the `/skills` operation path |
| `harw agent plugins ...` | the `/plugins` operation path |

The bridge creates:
- an `EntryKind::Analyze` runtime;
- a command-only surface;
- an Echo model that should never be used for the operation itself;
- the active profile's job store;
- plan services when enabled;
- memory using the same UIA/profile selection rule as the chat runtime.

It then resolves the Operation from the assembled runtime, checks the effective
permission tier, builds an `OpContext`, and dispatches through the same
adapter used by the session command plane.

### Session-bound operations are explicitly refused

The bridge rejects operations that only make sense against a live session,
including `/status`, `/usage`, `/new`, `/compact`, and `/mode`.

This is a good boundary. The shell command bridge is not a fake session.

---

## 7. Plan / Goal / WorkDriver: live, but not root CLI subcommands

Planning services are composed in `harw-cli/src/lib.rs` and passed into the
runtime assembly from a single `PlanToolConfig`.

When planning is enabled, the runtime registers:
- plan/goal/research-family operations through `register_plan_tools`;
- WorkDriver operations through `register_work_driver_tools`.

The TUI command registry derives slash-command specs from Operation metadata.

Therefore `/plan` and `/goal` are first-class live commands even though
the root `Command` enum contains no `Plan` or `Goal` variant.

### Human-only goal completion remains a distinct boundary

The CLI composition uses actors with explicit provenance. The startup CLI
actor is `human:cli`; worker mutations use a separate job actor. Goal status
validation prevents a model actor from marking a goal `Achieved` or
`Abandoned`.

Reviewers must preserve that distinction when adding future process-CLI
wrappers. A hypothetical `harw goal achieve` must not accidentally route
through a model-actor path.

---

## 8. `harw analyze` and `/analyze` must not be conflated

The root clap grammar has a dedicated `harw analyze` workflow with crate /
workspace selection, dependency order, dry-run and parallelism flags.

The operation registry also has an `analyze` planning operation when the plan
surface is enabled.

The shared name does not prove identical semantics.

Reviewer rule: map each user-visible spelling to its concrete parser and
handler before claiming parity.

---

## 9. Agent CLI is much larger than the short overview suggests

The live `harw agent` grammar currently includes:

- `uia-new`
- `list`
- `skills`
- `plugins`
- `check`
- `build`
- `inspect`
- `graph`
- `explain`
- `new`
- `fmt`
- `diff`
- `test`
- `run`
- `versions`
- `use`
- `clean`
- `doctor`
- hidden install/auto-build helpers.

The compiler-facing commands share `harw-agent-compiler::commands` with the
in-session `/agent` command surface rather than maintaining a second compiler
implementation.

### Confirmed documentation drift

The overview block in `docs/cli.md` currently summarizes `harw agent` only
as:

```text
agent uia-new | list [QUERY] | skills [ARGS...] | plugins [ARGS...]
```

That is materially incomplete relative to the live parser.

This is documentation drift, not evidence that the compiler commands are
missing.

---

## 10. Provider/config/model paths intentionally converge

There are multiple ergonomic spellings, but the write paths converge.

Examples:

- `harw provider add/remove/enable/disable/list` maps into the same
  `SettingsProviderAction` implementation used by `harw config provider`.
- `harw provider scan` routes into the model scan path.
- `harw model ...` owns model discovery/selection behavior.
- secret input is represented as references; plaintext provider auth is
  rejected by settings/provider handling.

Reviewer consequence: do not classify these as duplicate business logic just
because the clap enums overlap. Check the handler mapping first.

---

## 11. Service commands are four different runtime roles

The names are close enough that reviewers should state the distinction
explicitly.

### `harw gateway`

With **no action**, runs the Gateway in the foreground.

The Gateway is the long-lived channel/telemetry-facing process composition.

With an action (`install/start/stop/restart/enable/disable/status`), the same
root command routes to gateway service management rather than starting the
foreground runtime.

### `harw serve`

Runs the local MCP listener composition and its job-worker path. It has its own
ordered shutdown behavior and configuration-root handling.

### `harw web`

Runs the local web/control surface over a Unix socket. It has system/systemd
socket modes and is not merely another name for Gateway.

### `harw service`

Manages the background service set. Current lifecycle code treats the managed
set as two services:
- `harw-serve`
- `harw-gateway`

This makes `harw service` a deployment/lifecycle umbrella, while
`harw gateway ACTION` is the convenient focused control path for one member.

---

## 12. TUI command inventory is generated from Operation metadata

The TUI registry does not own an independent hard-coded copy of every
Operation command.

`CommandRegistry::from_operation_registry` walks registered Operations and
derives command specs from `Surface::Command`, including:
- command path;
- visibility/scope;
- permission tier;
- domain;
- aliases;
- busy behavior.

It also rejects command/alias collisions.

This is one of the strongest architectural properties of the command system:
the slash-command catalog and operation authority metadata share a source.

TUI-local commands and prefix handling still exist separately where the action
is genuinely terminal/session UI behavior.

---

## 13. Review findings

### F1 — docs/cli.md agent inventory is stale

**Severity:** documentation / medium usability impact.

The live clap grammar exposes the agent compiler command family, while the
overview omits it. A reader using `docs/cli.md` as an inventory will miss a
large part of the executable CLI.

**Direction:** generate or test the documented root inventory against clap
metadata, or at minimum expand the agent section and add a drift test.

### F2 — the three command planes are insufficiently explicit in CLI docs

**Severity:** architecture documentation.

A reader can easily assume slash commands are shell CLI subcommands or assume
a shell command with the same word is backed by the same operation.

**Direction:** document Process CLI vs Session Command vs Model Tool as a
first-class boundary and link the TUI command contract.

### F3 — same-name commands require provenance labels in reviews

**Severity:** review correctness.

`analyze` demonstrates the problem: a root workflow and a planning operation
can share a name without sharing a handler contract.

**Direction:** any command review should record
`spelling -> parser -> handler -> authority source -> runtime surface`.

### F4 — JSON support is family-gated then subcommand-gated

**Severity:** low / maintainability.

The central whitelist is not the final truth. A static reviewer who only reads
`reject_unsupported_json` can overstate support.

**Direction:** review both the root gate and family handler before documenting
JSON behavior.

### F5 — `kill` has a deliberately separate parse/tracing lifecycle

**Severity:** high if accidentally "simplified".

Moving killer behind normal root parsing would alter argument ownership and
the sudo helper path.

**Direction:** preserve the raw-argv escape hatch unless the killer grammar is
redesigned together with root globals and helper reinvocation.

---

## 14. Reviewer method for future CLI changes

For every changed or newly documented command, record this chain:

```text
surface spelling
    -> parser / catalog source
    -> dispatch adapter
    -> runtime/service composition
    -> authority + approval source
    -> state/store ownership
    -> output/exit-code contract
    -> compatibility aliases
    -> docs/tests
```

Do not infer a step from naming.

For root CLI changes, inspect at minimum:
- `harw-cli/src/cli/mod.rs`;
- the relevant `harw-cli/src/cli/<domain>.rs`;
- `harw-cli/src/lib.rs::dispatch`;
- the actual handler module;
- `docs/cli.md`;
- parser tests.

For bridged operations also inspect:
- `harw-cli/src/op_bridge.rs`;
- Operation metadata/handler;
- TUI registry/catalog when command-visible.

For daemon commands also inspect:
- runtime composition entrypoint;
- lifecycle/service descriptor;
- shutdown and identity boundary.

For skill/agent compiler commands also inspect:
- `harw-agent-compiler::commands`;
- the in-session `/agent` surface;
- installed/bundled asset behavior.

---

## 15. What this review does not claim

This review does not claim:
- that every parser branch was executed live;
- that every daemon starts successfully on the reviewer host;
- that a real provider round-trip was exercised;
- that planning documents describe CURRENT unless code confirms it;
- that `docs/cli.md` is globally stale — one concrete drift is called out
  where source evidence is direct.

The pinned source is the evidence base for the statements above.

---

## 16. Skills added with this review

This review is accompanied by three bundled Harw skills:

- `cli-live-review` — trace a command from spelling to live runtime behavior.
- `cli-command-surface-map` — distinguish process CLI, slash/TUI operation,
  model-tool and service/control surfaces.
- `cli-doc-drift-review` — compare documentation/help claims against the live
  parser, dispatch and handler graph without treating docs as implementation.

They are intentionally small and composable. Loading one does not require
loading the complete review document.
