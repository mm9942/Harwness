# `harw` command line

This page describes the command tree of the `harw` program, the global
flags and session flags, the behavior of `--json`, the `harw kill` process
killer, and the mapping from older spellings to the current commands. The
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
  agent uia-new | skills [ARGS…] | plugins [ARGS…]
  knowledge index [build [--source docs|knowledge] [--force] | status]
            | memory [ARGS…] | proposals [ARGS…]
  jobs list [FILTER] | show ID | approve ID [--note TEXT]
       | deny ID [--reason TEXT] | cancel ID | retry ID

Services
  gateway [install|start|stop|restart|enable|disable|status]
  serve [--config-dir DIR] | web [--socket PATH]
  service install | status | uninstall
  mcp setup [SERVER] | check [SERVER]
  channel connect telegram [--pair CODE]

System
  init | onboard | doctor [--config-dir DIR] | update
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
| `--approval ask\|auto\|full` | Approval mode for this session only: `ask` prompts on every tool call, `auto` lets uncritical calls through and checks the rest with a pre-filter and classifier, asking when in doubt, `full` never asks. Overrides the configured default. |
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
- **Operations** (`harw jobs …`, `harw agent skills|plugins`,
  `harw knowledge memory|proposals`) return their structured data where
  available, otherwise `{"text": "…"}` with the text output.

Commands without a JSON form — such as interactive dialogs like
`harw agent uia-new`, `harw knowledge index` or `harw provider …` — abort
with `--json` with the message `` `<command>` does not support --json ``
instead of ignoring the flag. The exit status is then non-zero.

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
| `harw analyze --bottom-up` / `--top-down` | `harw analyze --order bottom-up` / `--order top-down` |
| `/skills`, `/plugins` (chat only) | `harw agent skills …`, `harw agent plugins …` |
| `/memory`, `/context-proposal` (chat only) | `harw knowledge memory …`, `harw knowledge proposals …` |
| Jobs, chat only | `harw jobs list\|show\|approve\|deny\|cancel\|retry` |
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
