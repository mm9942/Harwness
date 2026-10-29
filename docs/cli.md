# `harw` command line

This page describes the command tree of the `harw` program, the global
flags and session flags, the behavior of `--json`, the Harw-DoD commands
(`harw dod`), the `harw kill` process killer, and the mapping from older
spellings to the current commands. The
exact grammar of your installed build is always available via `harw --help`
and `harw <command> --help`.

## Overview

```text
harw [PROMPT] [-r [SESSION]] [--all]            Start chat (same as `harw chat`)

Working
  chat [PROMPT] [-r [SESSION]] [--all]          Interactive chat
  exec PROMPT…                                  One-shot request, no UI
  analyze [CRATE] [--workspace] [--order bottom-up|top-down]
          [--dry-run] [--max-parallel N]        Workspace dependency analysis
  session list [--all] | show ID | resume ID    Saved sessions

Configuration
  config                                        Interactive menu (alias: settings)
  config get KEY | set KEY [VALUE]              Read/write a single key
         [--global | --project]                 (set without VALUE deletes the key)
  config permissions get | set-mode ask|auto|full
         | allow TOOL [--pattern M] | deny TOOL [--pattern M]
         | unallow INDEX | undeny INDEX
  config provider … | config model default ID
  provider list | add NAME --api DIALECT --base-url URL [--auth REF] [--models A,B]
                  [--auth-header bearer|x-api-key|api-key|none] [--no-auth]
                  [--allow-insecure-lan]
           | remove NAME | enable NAME | disable NAME
           | scan [NAME] [--free-only] [--prune]
  model                                         List models (alias: models)
  model list | scan [NAME] [--free-only] [--prune] | add [PROVIDER/MODEL]
        | remove PROVIDER/MODEL | default ID | internal … | catalog [--refresh]
  auth login | token | import | status | prune [PROVIDER]
  project trust | untrust | status [DIR]

Agents and knowledge
  agent uia-new | list [QUERY] | skills [ARGS…] | plugins [ARGS…]
  knowledge index [build [--source docs|knowledge] [--force] | status]
            | memory [ARGS…] | proposals [ARGS…]
  jobs list [FILTER] | show ID | approve ID [--note TEXT]
       | deny ID [--reason TEXT] | cancel ID | retry ID
  cycle list [STATE [--limit N]] | status ID | stop ID [--reason TEXT]
  tool list [FAMILY] | describe NAME            Tool index (same data as `tool.index`)

Services
  gateway [install|start|stop|restart|enable|disable|status]
  serve [--config-dir DIR]
  web [--socket PATH] [--system [--socket-group GROUP] | --systemd-socket]
  service install | status | uninstall
  mcp setup [SERVER] | check [SERVER]
  channel connect telegram [--pair CODE]

System
  init | onboard | doctor [--config-dir DIR] | update
  dod status | doctor | sensors
      | findings [-n N] [--severity S] [--rule ID] [--since 24h]
      | config check [--config PATH] | config suggest
      | enable | disable | restart                 (root only)
      | warden status | warden enable | warden disable   (enable/disable root only)
      | install --print-systemd [UNIT]
  uninstall [--scope …] [--dry-run] [--yes]
  completions [SHELL] [--install | --uninstall] [--dry-run]
  bug-report [--type …] [--title …] …
  sandbox [status]
  debug echo TEXT… | classify TEXT…
  kill [ARGS…]
```

`agent skills`, `agent plugins`, `knowledge memory` and `knowledge proposals`
forward all following arguments unchanged, including ones with a leading
hyphen. They correspond to the chat commands `/skills`, `/plugins`,
`/memory` and `/context-proposal`.

`dod install --print-systemd [UNIT]` (formerly `harw install
--print-systemd`) prints the systemd system units that are embedded from
`deploy/systemd/` (the single canonical source, see
[DoD setup](setup/dod.md#systemd-units-and-the-deploy-tree)), rendered with the
default paths of `dod/scripts/install.sh`. Without `UNIT` it prints every
unit, each after a `# ---- deploy/systemd/<name> ----` header. It writes,
enables and starts nothing.

## Provider (`harw provider`)

`harw provider add NAME --api DIALECT --base-url URL` creates
`providers/NAME.toml` in the active profile, or overwrites an existing one.
The following options apply:

- `--auth REF` names a secret reference (`env:VAR`, `secrets:NAME`, …). A
  plaintext key is rejected.
- `--auth-header bearer|x-api-key|api-key|none` selects how the key is
  transported. Without it, the dialect's default applies (`bearer`).
- `--no-auth` means: no key and no auth header, e.g. for a local vLLM or
  LM Studio server. This option excludes `--auth` and `--auth-header`.
- `--allow-insecure-lan` allows `http` to a private LAN IP (`10/8`,
  `172.16/12`, `192.168/16`). Otherwise `http` is only allowed for loopback.

If the base URL points at the local machine (or into the LAN with
`--allow-insecure-lan`), `add` without `--auth` automatically writes
`auth_header = "none"` and `max_concurrency = 1`.

`harw provider scan [NAME]` queries `/models` and writes one
`models/<id>.toml` per model. The context window (`context_window`) comes
from `context_length`, from vLLM's `max_model_len`, or, for local providers,
additionally from LM Studio's `GET /api/v0/models`. If the server reports
`supported_parameters` without `"tools"`, the scan sets
`[capabilities] tool_calling = false`. An existing `context_window` is kept
if the server does not report one.

Optional fields in `providers/<name>.toml`: `request_timeout_secs`,
`stream_idle_timeout_secs`, `retry_timeouts`, `max_tokens_field`
(`"max_tokens"`, `"max_completion_tokens"` or `"both"`),
`send_reasoning_effort`, `strict_tools`, `parallel_tool_calls` and
`allow_insecure_lan`. Local providers have their own defaults. The overview
and the startup commands for vLLM and LM Studio are in
[setup/local-models.md](setup/local-models.md).

## Skills (`harw skill`)

`harw skill` shows the skill catalog, previews which skill sections a task
would trigger, and checks a skill directory. `list`, `show` and `preview` run
the same operation as the chat command `/skills` (`/skills list`,
`/skills show NAME`, `/skills preview …`), so terminal and chat answer
identically. With `--json`, `preview` and `validate` print their structured
data; `list` and `show` print `{"text": "…"}` with the text output.

| Action | Effect |
| --- | --- |
| `list` | The configured skills with status and description. |
| `show NAME` | Description, declared tools and MCPs of one skill. |
| `preview [TEXT…] [--tool NAME]… [--error TEXT]… [--path PATH]… [--command CMD]… [--budget BYTES]` | Without any model call: the skill sections the first round of a fresh session would inject (context section `skills.triggered`) for this input — each with skill, section, trigger kind and key, bytes and trust class — and every hit that was dropped, with its reason (`already_delivered`, `turn_budget`, `session_budget`). |
| `validate DIR` | Reads `DIR/skill.toml` and `DIR/triggers.toml` the way the skill index does (same file, size and safe-read checks) and prints every error and every warning of the `triggers.toml` check verbatim (e.g. `triggers.keywords: 17 Einträge, nur die ersten 16 behalten`). A `DIR` that is a symlink or whose name starts with `.` is an error, because the index skips such skill directories. Needs no Harwness home. |

The `preview` inputs stand for what the turn loop reads from a session's
history:

- `TEXT` — a user message (keywords and error codes),
- `--tool` — the first use of a tool,
- `--error` — tool output (error codes such as `E0505`),
- `--path` — a path argument of an `fs.*`/`doc.*` call,
- `--command` — a command line of a `shell.*`/`process.*`/`job.start` call.

At least one input is required; the options are repeatable. The selection
uses the same `[skills]` settings and the same selection code as the turn
loop. The skill index is the session's own when one is available; otherwise
it is built the same way the runtime builds it (the trusted config layers of
the bound home, including the project settings directory, plus the bundled
skills, each with its `triggers.toml`). The `Index:` line (JSON `index`) says
which one was used, and also lists skills the index skipped and trigger
warnings it recorded. A native build with an embedded bundle reads only that
bundle, so its sessions can differ from a rebuilt index.

The per-round budget is `[skills] per_turn_bytes` (or `--budget`, 256 to
65536 bytes); the session cap is `[skills] per_session_bytes`, at most four
per-round budgets. The preview assumes an unknown model window, for which the
per-round cap applies unchanged. A session uses about 3 % of its window
(4 bytes per token), capped at the same value, so it reaches the cap only
from a window of about `per_turn_bytes / 0.12` tokens (about 34k tokens for
the default 4096 bytes, about 546k for 65536) and selects less below that.
The `Budget:` line (JSON `budget.full_cap_window_tokens`) gives the exact
threshold. The preview also selects when `[skills] triggered_injection =
false` and then says that a session would inject none of it.

In the chat, `/skills preview "TEXT" --command "cargo test"` and
`/skills validate DIR` (relative to the workspace root) do the same. Both are
read-only and run immediately, even during a turn; they are not available
over channels such as Telegram.

Exit codes: `0` success, `1` failure (the operation fails, `validate`
reports an error or a warning, the directory cannot be resolved, or the
output cannot be written), `2` usage error (no `preview` input, a `--budget`
out of range).

```bash
# Which skill sections would a failing test pull in?
harw skill preview "cargo test fails with E0505 in src/lib.rs" --command "cargo test"

# Check a skill directory before bundling it
harw skill validate harw-tool-sys/skills/sys-tools
```

## Tools (`harw tool`)

`harw tool` shows the tool index of this build: the same data the model gets
from the read-only model tool `tool.index`. It reads only data built into the
`harw` binary (the capability catalog, the registry profiles, the real tool
descriptions and the bundled skill manifests), needs no configuration and no
Harwness home, and writes nothing. It lists every tool the build knows, not
only the tools a particular session or role may call.

| Action | Effect |
| --- | --- |
| `list` | The tool families with their tool counts. A family is the name prefix before the first `.` (`fs`, `git`, `dod`, …); names without a `.` belong to `core`. |
| `list FAMILY` | One line per tool of the family: name, capability class, the first sentence of its description, the skill that explains it (`skill: …`) and the profiles that register it (or `always available`). |
| `describe NAME` | One tool: family, provider and crate, capability class, profiles, linked skill, how the default approval policy treats a call, the full description and every parameter with type, whether it is required, and its description. |

The linked skill comes from the bundled skill manifests (a skill whose
`skill.toml` is the only one to list the tool in `tools = [...]`) and from
`#[tool(skill = "...")]` bindings of the tool providers; a tool that several
skills list has no link. With `--json`, `list` prints the families (or the
tools of the family) and `describe` the entry, the description, the parameter
list and the parameter schema.

Exit codes: `0` success, `1` unknown family or unknown tool (the message lists
the known families), `2` usage error.

```bash
# Which read-only Harw-DoD tools are there, and what do they take?
harw tool list dod
harw tool describe dod.findings --json
```

The system prompt of every session with tools carries a matching
`## Tool families` block (each family of the session's own tools with its
count) and points the model at `tool.index` for details.

## Global flags

Global flags may appear before or after any command and apply to every
command.

| Flag | Effect |
| --- | --- |
| `--home DIR` | Uses `DIR` instead of `HARW_HOME` or `~/.harw` as the Harwness directory. |
| `--profile NAME` | Uses profile `NAME` instead of the active profile (takes precedence over `HARW_PROFILE`). |
| `-C DIR`, `--cwd DIR` | Behaves as if `harw` had been started in `DIR` (project detection, session list, jobs). |
| `--log LEVEL` | Log filter, e.g. `info` (default), `debug`, or `harw_core=debug,info`. |
| `--log-sensitive` | Also logs prompts, tool arguments and responses. Debugging only. |
| `-v`, `--verbose` | Shows every tool call with all arguments instead of a short preview. |
| `--json` | Prints the result as JSON (see below). |

## Session flags

Session flags are accepted anywhere, like global flags, but only take
effect for `chat` (including `harw` with no command), `exec` and `analyze`.
On every other command `harw` aborts with an error message pointing at
`chat`, `exec` and `analyze`, rather than silently ignoring the flag.

| Flag | Effect |
| --- | --- |
| `--mode MODE` | Starts in the given interaction mode (`chat`, `plan`, `explore`, `work`, `shell`). |
| `--approval ask\|auto\|full` | Approval mode for this session only: `ask` prompts on every tool call, `auto` lets uncritical calls through and checks the rest with a pre-filter and classifier, asking when in doubt, `full` never asks (not even for `process.kill`, `host.sudo_exec` or remote OCR uploads; only an explicit deny rule refuses a call). Overrides the configured default. |
| `--model ID` | Uses model `ID` for this session instead of the default model. |
| `--goal TEXT` | Sets a goal for the session to work towards at startup. |
| `--agent NAME` | Starts the session with agent definition `NAME` as its root (a built-in role such as `root-orchestrator`, or a custom definition) instead of the configured `active_agent_definition`. Applies to this session only; to make it permanent use `/agent use NAME` (remove with `/agent use --clear`), effective from the next session. |
| `--add-dir PATH` | Allows file access without confirmation under `PATH` as well (repeatable). |

The chat flags `-r/--resume` and `--all` are not global: they only apply to
`harw` with no command and to `harw chat`. `--all` also exists for
`harw session list`.

Sessions with no user turn at all are no longer saved. Older empty sessions
are hidden from the `harw -r` picker; they remain reachable via `--all` or
an explicit ID. A resumed session shows a resume notice instead of a new
greeting.

## Checking credentials (`harw auth`)

- Credentials are normalized on read: all ASCII whitespace (newlines, CRLF
  from `.env`, wrapped pastes) is stripped. This also applies to
  `CLAUDE_CODE_OAUTH_TOKEN` and `ANTHROPIC_API_KEY`.
- `harw auth token` validates the format of an Anthropic token before
  storing it: setup/OAuth tokens start with `sk-ant-oat`, API keys with
  `sk-ant-api`, and only printable ASCII is allowed. The error message
  names the cause, never the token value.
- `harw auth status` appends a short format check to each Anthropic token
  file, likewise without printing the value.

## Output with `--json`

Commands with a JSON form write exactly one formatted JSON document to
standard output with `--json`; notices and errors still go to standard
error.

- **Tables** (e.g. `harw session list`) become an array of objects whose
  keys are the column headers.
- **Single values** (e.g. `harw session show ID`) become an object.
- **Operations** (`harw jobs …`, `harw cycle …`, `harw agent skills|plugins`,
  `harw knowledge memory|proposals`) return their structured data where
  available, otherwise `{"text": "…"}` with the text output.
- **`harw dod …`** prints one object per action (the same structure as the
  `/dod` command and the `dod.*` model tools; service actions report the
  `systemctl` steps they ran).
- **`harw tool …`** prints the same structure as the `tool.index` model tool
  (`describe` adds the description, the parameters and their schema).

Commands without a JSON form — such as interactive dialogs like
`harw agent uia-new`, `harw knowledge index` or `harw provider …` — abort
with `--json` with the message `` `<command>` does not support --json ``
instead of ignoring the flag. The exit status is then non-zero.

## Harw-DoD (`harw dod`)

`harw dod` reads and controls the Harw-DoD suite (Detect · Orient · Defend,
see [DoD setup](setup/dod.md)). The read-only actions work as any user and
on hosts without the suite (they report "not installed"); everything goes
through the read-only client `harw-dod-client`, so the CLI, the `/dod` chat
command and the model tools `dod.status`, `dod.findings` and `dod.sensors`
agree.

| Action | Effect |
| --- | --- |
| `status` | Install state, system config and active profile, the six units, Warden state, sentinel health and the high/critical finding counts of the `[dod] findings_window_hours` window. Counts are prefixed `≥` and marked "partial" when older export data was not read (`lower_bound: true` in `--json`). |
| `doctor` | Read-only prerequisites: cgroup v2, kernel BTF, tracefs, Landlock, `systemctl`/`systemd-sysusers`/`systemd-tmpfiles`, the daemons, the unit files, the config and both exports. Exit 1 when a prerequisite is missing. The build toolchain checks stay with `make -C dod doctor`. |
| `findings [-n N] [--severity S] [--rule ID] [--since SPAN]` | The newest findings of the bounded export tail (default 50, at most 200), at or above severity `S`, of one rule, observed within `SPAN` (`90m`, `24h`, `7d`). |
| `sensors` | The sentinel health report (`health.json`): per-sensor state, readings, errors, last success, IPC counters; flagged stale after 2 minutes without an update. |
| `config check [--config PATH]` | Validates `/etc/harw-dod/config.toml` (or `PATH`) exactly like the daemons do (no symlinks, root-owned, not group/world-writable). Exit 0 only for a valid config that selects an `active_profile`. |
| `config suggest` | Prints a profile template for harw's job cgroups; the delegated cgroup root is filled in by PL-97 E3. |
| `enable` | Root only. Checks the config, then `systemctl daemon-reload` and `systemctl enable --now harw-dod.target` (`make -C dod enable`). The Warden stays disabled. |
| `disable` | Root only. `systemctl disable --now harw-dod.target harw-sentinel.service harw-probe-bpf.service harw-probe-fs.service`, then disables `harw-warden.socket` and `harw-warden.service` (errors there are ignored), like `make -C dod disable`. |
| `restart` | Root only. Checks the config, `systemctl daemon-reload`, `systemctl restart harw-dod.target`. |
| `warden status` | Warden state, guard file and socket unit. |
| `warden enable` | Root only. Creates `/etc/harw-dod` (mode `0750`, only when missing) and the guard file `/etc/harw-dod/warden.enable`, then `systemctl daemon-reload` and `systemctl enable --now harw-warden.socket` (`make -C dod enable-warden`). `kill_requires_human` stays true. |
| `warden disable` | Root only. `systemctl disable --now harw-warden.socket harw-warden.service`, then removes the guard file. |
| `install --print-systemd [UNIT]` | Prints the embedded units (see above). |

The service actions only call `systemctl` and write (or remove) the one
guard file; they never run `cargo` or `make`, and harw never talks to the
Warden or the escalation binaries. Membership in `harw-dod-readers` (needed
to read the exports as a regular user) is still granted by hand, see
[DoD setup](setup/dod.md).

Only the system export `/var/lib/harw-dod/export` (`source: system`),
which only the sentinel writes, is verified. Findings and sensor health
from the harw home fallback (`<home>/dod/export`, used only when the system
export directory does not exist) or from a configured `[dod] export_dir`
are labelled `UNVERIFIED` with the source and its hint in the text output
of `status`, `findings` and `sensors`, and carry `"verified": false`,
`"source"` and `"hint"` in `--json` (the same as `/dod` and the `dod.*`
model tools); they are not Harw-DoD facts, and `doctor` reports these
exports as `WARN`. A system export that exists but cannot be read is
reported as unreadable with the hint to add the user to `harw-dod-readers`;
it is never replaced by the home fallback.

Exit codes: `0` success, `1` failure (a `systemctl` step failed, a
prerequisite is missing, an export is unreadable), `2` usage error, `3`
refused (not root, or no valid `active_profile` for `enable`/`restart`).

## `harw kill`

`harw kill` selects processes precisely and terminates them reliably. It is
a thin wrapper: every argument after `kill` is forwarded unchanged to the
bundled `killer` binary (crate `harw-killer`; see `harw-killer/README.md`
and `harw-killer/src/cli.rs` for the full flag reference), so
`harw kill --help` shows `killer`'s own help. Linux only.

The engine uses **pidfd**-based process selection: it matches processes by
exact executable basename and/or PID, sends **SIGKILL immediately**
(no SIGTERM phase), waits, and sends a **second SIGKILL** to survivors.
Both signal attempts use the same pidfd handle, so a killed PID cannot be
reused by an unrelated process in between. PID 1, `killer` itself and its
own ancestors (such as the calling shell) are always protected.

Common flags (see `harw-killer/src/cli.rs` for the authoritative list):

| Flag | Effect |
| --- | --- |
| `-p`, `--process NAME…` | One or more exact executable basenames to match (repeatable). |
| `--pid PID…` | One or more explicit PIDs to match (repeatable). |
| `--uid UID` | Restricts the selection to this effective UID. |
| `-n`, `--dry-run` | Preview the selection only; no signal, no confirmation, no sudo. |
| `-y`, `--yes` | Skip the interactive confirmation. |
| `-t`, `--timeout SECONDS` | Seconds to wait before the second SIGKILL (default 5). |
| `--kill-wait SECONDS` | Seconds to wait after the second SIGKILL before reporting a survivor (default 2). |
| `--json` | Machine-readable report on stdout; diagnostics on stderr. |
| `--no-sudo` | Never invoke `sudo` for processes owned by another user. |

At least one selector (`--process` or `--pid`) is required; `harw kill`
never selects "all processes". Processes owned by another user are handled
as a separate group via `sudo` unless `--no-sudo` is given.

```bash
# Preview which rustc/cargo processes would be selected
harw kill -p rustc cargo -n

# Kill all `node` processes owned by the current user, no confirmation
harw kill --process node -y

# Kill an explicit PID
harw kill --pid 12345 -y

# Wait 3 seconds before the second SIGKILL instead of the default 5
harw kill -p cargo -y -t 3
```

The agent tool `process.kill` uses the same underlying engine
(`harw_killer::api`) but is more restricted: it only ever terminates
processes owned by the current user (processes of other users are reported
as errors, never killed — no `sudo`), it always requires user approval
before running, and it is never invoked via `sudo`.

## Older spellings

The following spellings still work but no longer appear in `--help`. On
use, `harw` prints a note on standard error pointing at the new name, e.g.
``Note: `harw lens` is now `harw knowledge index`.`` `settings` and `models`
are aliases of `config` and `model` respectively, and remain valid
permanently.

| old | new |
| --- | --- |
| `harw settings …` | `harw config …` |
| `harw settings provider …` | `harw provider …` (or `harw config provider …`) |
| `harw models …` | `harw model …` |
| `harw models delete TARGET` | `harw model remove TARGET` |
| `harw models scan …` | `harw provider scan …` (or `harw model scan …`) |
| `harw catalog [--refresh]` | `harw model catalog [--refresh]` |
| `harw connect --channel telegram [--pair CODE]` | `harw channel connect telegram [--pair CODE]` |
| `harw lens [build\|status]` | `harw knowledge index [build\|status]` |
| `harw uia new` | `harw agent uia-new` |
| `harw run TEXT…` | `harw debug echo TEXT…` |
| `harw classify TEXT…` | `harw debug classify TEXT…` |
| `harw install --print-systemd [UNIT]` | `harw dod install --print-systemd [UNIT]` |
| `harw analyze --bottom-up` / `--top-down` | `harw analyze --order bottom-up` / `--order top-down` |
| `/skills`, `/plugins` (chat only) | `harw agent skills …`, `harw agent plugins …` |
| `/memory`, `/context-proposal` (chat only) | `harw knowledge memory …`, `harw knowledge proposals …` |
| Jobs, chat only | `harw jobs list\|show\|approve\|deny\|cancel\|retry` |
| `/cycle` (chat only) | `harw cycle list\|status\|stop` |
| `harw -r SESSION` | still valid; also `harw session resume SESSION` |
| `HARW_PROFILE=NAME harw …` | still valid; also `harw --profile NAME …` |

`--bottom-up` and `--top-down` are mutually exclusive with each other and
with `--order`; combining them is a parse error.

## Examples

```bash
# Start chat in explore mode with a different model
harw --mode explore --model openrouter/qwen/qwen3-coder "How is the project structured?"

# One-shot request in a different directory, without confirmation prompts
harw -C ~/src/project --approval full exec "Format all Rust files"

# List sessions across all projects as JSON and resume one
harw session list --all --json
harw session resume 3f2a

# Add a provider, query its models, set the default model
harw provider add local --api ollama --base-url http://localhost:11434
harw provider scan local
harw model default local/qwen3

# Attach a local vLLM server without a key and read its context window
harw provider add vllm --api openai-chat --base-url http://localhost:8000/v1 --no-auth
harw provider scan vllm

# Refresh the provider catalog from models.dev
harw model catalog --refresh

# Approve or deny a pending job
harw jobs list
harw jobs approve job-17 --note "Reviewed"
harw jobs deny job-18 --reason "Too broad"

# Search memory, manage skills
harw knowledge memory search "build failure"
harw agent skills list

# Connect Telegram and finish pairing
harw channel connect telegram
harw channel connect telegram --pair ABCD-EFGH

# Check with a different profile
harw --profile work doctor

# Analyze bottom-up from the roots, preview only
harw analyze --order top-down --dry-run

# Kill stray build processes
harw kill -p rustc cargo -n
harw kill -p rustc cargo -y
```
