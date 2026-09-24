# Config Structure — the `.harw/` Tree

> Status: implemented · Last reviewed: 2026-09-24

Implemented in `harw-config` (this doc is the source of truth
for the schema; the crate is kept in sync with it).

Inspiration: OpenClaw's single `openclaw.json` (`inspirations/openclaw/docs/gateway/configuration.md`,
`config-channels.md`, `config-agents.md`, `config-tools.md`) proved a
single-file config is workable for a small deployment but conflates
concerns (agents, channels, providers, tools, gateway, logging all in one
JSON blob) that Harwness keeps separate for auditability and layering.
Nothing here reuses OpenClaw's field names, JSON shape, or code — only the
*idea* that "one coherent tree of settings, one loader" beats bespoke
per-subsystem config files.

Harwness instead uses a **layered TOML tree**, matching the pattern already
established by `harw-config` (`docs/design/channel-ingress-telegram.md` §3.1,
`docs/design/interaction-contract.md`, `docs/design/secrets-and-audit.md`):
one typed `*_toml.rs` module per config surface, directory-based discovery,
later layers override/merge earlier ones.

---

## 1. Layering order (precedence ascending)

```
1. built-ins           (compiled-in defaults, no files on disk)
2. ~/.harw/             (user/home-level: personal defaults across all repos)
3. <repo>/.harw/        (repo-level: project-specific overrides, checked in
                         minus secrets — secret refs are safe to check in,
                         the resolved secret values never are)
4. explicit overrides   (CLI --config-dir flags, HARW_CONFIG_EXTRA env, tests)
```

`discover_config(layers: &[PathBuf])` (`harw-config/src/discovery.rs`) walks
this list in order and merges. This document extends it with the sections
below; the merge semantics per section are in §3.

## 2. Full tree

```
.harw/
├── config.toml              # harness-wide settings (§2.1)
├── auth.toml                 # credential references + KEK provenance (§2.2)
├── keybindings.toml          # TUI keybindings (§2.3, schema already fixed
│                              # by docs/design/tui-command-contract.md §4.4)
├── channels/
│   ├── telegram-support.toml # one [[channel.telegram]]-shaped file per binding,
│   │                          # OR one channels/telegram.toml with multiple
│   │                          # [[channel.telegram]] tables — both accepted,
│   │                          # see §3.3
│   └── telegram-ops.toml
├── providers/
│   ├── openai.toml
│   ├── anthropic.toml
│   └── local-ollama.toml
├── models/
│   ├── gpt-5.toml
│   ├── claude-sonnet.toml
│   └── llama-local.toml
├── agents/
│   ├── default/
│   │   ├── agent.toml
│   │   └── system.md
│   └── researcher/
│       ├── agent.toml
│       └── system.md
├── skills/
│   ├── web-search/
│   │   ├── skill.toml
│   │   └── instructions.md
│   └── code-review/
│       ├── skill.toml
│       └── instructions.md
├── mcps/
│   └── docs.toml                # declarative server metadata, not a launch grant
└── plugins/
    ├── slack-bridge.toml     # CapabilityBundle manifest (§2.7)
    └── jira-sync.toml
```

Every file layer (`~/.harw/`, `<repo>/.harw/`, overrides) may re-declare any
subset of this tree; nothing is mandatory except that a `config.toml` must
exist in *some* layer by the time discovery finishes, or `harw doctor`
reports `MissingHarnessConfig`.

### 2.1 `config.toml` — harness-wide settings

```toml
# .harw/config.toml

workspace_root = "."                 # relative to this file's directory, or absolute
default_provider = "anthropic"
default_model = "claude-sonnet"
policy_profile = "standard"          # named policy profile, resolved against harw-policy's registry

[logging]
level = "info"                       # trace|debug|info|warn|error — matches the --log CLI convention
target_module_paths = false          # mirrors tracing_subscriber::fmt().with_target(...)
json = false                         # structured JSON logs vs. human-readable

[tui]
theme = "default-dark"
keybindings_file = "keybindings.toml"   # relative to this config.toml's own layer directory

[session]
store_dir = "sessions"               # relative to workspace_root
journal_format = "jsonl"
retention_days = 90                  # 0 = keep forever

[policy]
default_visibility_scope = "self"    # Self|DescendantTree|ExplicitlyGranted|OperatorOnly (harw-policy)
require_approval_for = ["exec", "fs-write-outside-workspace"]

[tools.doc]
remote_ocr = "ask"                   # off|ask|on — may doc.read_pdf upload PDFs to a remote OCR
                                     # service (Mistral)? "ask" asks before every upload
```

Notes:
- `[logging]`, `[tui]`, `[session]`, `[policy]`, `[tools.doc]` are all optional tables;
  every field inside them has a hard-coded default so a bare `config.toml`
  with only `default_provider`/`default_model` remains valid (backward
  compatible with the pre-existing two-field `HarnessConfig`).
- `tui.keybindings_file` is resolved relative to the *layer* the
  `config.toml` came from, so a repo-level `.harw/config.toml` can point at
  a repo-level `.harw/keybindings.toml` without an absolute path.

### 2.2 `auth.toml` — credential references

```toml
# .harw/auth.toml
# Never a raw secret value. Every entry is a SecretRef string in the
# env:/file:/keyring:/secrets: grammar (§4). This file is safe to commit
# to a repo (it holds *references*, e.g. env var names, not the values).

[kek]
provenance = "key_file"              # "key_file" | "keyring" | "env_seed" (docs/design/secrets-and-audit.md §3)
key_file_path = "~/.config/harwness/kek.seed"
# keyring_entry = "keyring:harwness/kek"   # only when provenance = "keyring"
# env_seed_var  = "HARWNESS_KEK_SEED"      # only when provenance = "env_seed"

[credentials]
# Free-form named refs, consumable by provider/channel configs via
# `auth = "cred:openai-default"` shorthand, OR providers/channels may embed
# a SecretRef directly (env:/file:/keyring:/secrets:) without going through
# this table at all. This table exists for operators who want one place to
# audit "what secrets does this deployment reference," not because indirection
# through it is required.
openai-default = "env:OPENAI_API_KEY"
telegram-support-bot = "env:TELEGRAM_SUPPORT_BOT_TOKEN"
```

### 2.3 `keybindings.toml`

Schema is fixed by `docs/design/tui-command-contract.md` §4.4
(`[global]`, `[view.transcript]`, `[view.work-graph]`, `[view.process-list]`,
`[view.pager]` tables of `{ keys = [...], within_ms = ... }`). Not
re-specified here; `harw-config` does not yet parse it (owned by the future
`harw-tui` crate's `KeybindingConfig`), but `config.toml`'s `tui.keybindings_file`
field is the pointer to it, and discovery treats it as a location, not a
parsed structure, until `harw-tui` exists.

### 2.4 `channels/*.toml`

One file per binding, or one file per channel *kind* holding several
`[[channel.telegram]]` table-array entries — `harw-config` accepts both:
discovery reads every `*.toml` under `channels/`, parses it as
`ChannelFileToml { channel: ChannelSectionToml }` where
`ChannelSectionToml` currently has one variant, `telegram: Vec<TelegramChannelToml>`
(future kinds add sibling `Vec<...>` fields), and flattens every entry it
finds into `ResolvedConfig::channels: HashMap<String, ChannelToml>` keyed by
`ChannelId` (the `id` field, e.g. `"telegram:support-bot"`).

Schema per entry — see `docs/design/channel-ingress-telegram.md` §3.1 for the
full annotated example; reproduced here only as the canonical field list this
crate implements:

```toml
[[channel.telegram]]
id = "telegram:support-bot"
kind = "telegram"                     # redundant with the table name but kept explicit
                                       # for the tagged-enum representation (§3 below)
tenant_binding = "pairing"             # "pairing" | "static"
bot_token_ref = "env:TELEGRAM_SUPPORT_BOT_TOKEN"
transport = "long_poll"                # "long_poll" | "webhook"
enabled = true

[channel.telegram.transport_webhook]
public_url = "https://ingress.example.com/telegram/support-bot"
secret_token_ref = "env:TELEGRAM_SUPPORT_BOT_WEBHOOK_SECRET"
listen_addr = "127.0.0.1:8443"

[channel.telegram.groups]
require_mention = true
observe_unmentioned = false
allowed_chats = []
group_allowed_senders = []

[channel.telegram.topics]
mode = "per_topic_session"             # "per_topic_session" | "shared_session"

[channel.telegram.rate_limit]
max_updates_per_peer_per_min = 20
max_outbound_per_chat_per_sec = 1

[channel.telegram.attachments]
max_bytes = 20_000_000                 # Telegram getFile ceiling
max_count_per_message = 10
mime_allowlist = ["image/*", "application/pdf", "text/plain"]

[channel.telegram.commands]
menu_source = "policy_visible"
unknown_command_fallback = "reply_help"
```

### 2.5 `providers/*.toml`

Extends the existing `ProviderToml`. The plaintext `api_key: Option<String>`
field is **deprecated**, not removed outright (backward compatibility for
already-written configs), replaced going forward by `auth: Option<SecretRef>`:

```toml
# .harw/providers/openai.toml
name = "openai"
api = "openai-chat"                   # dialect tag: openai-chat|anthropic-messages|ollama|...
base_url = "https://api.openai.com/v1"
auth = "env:OPENAI_API_KEY"           # SecretRef — never a literal key
enabled = true
models = ["gpt-5"]

[headers]
"X-Org-Id" = "acme"

[origin_allowlist]                    # which channels/agents may route through this provider
agents = ["default", "researcher"]
channels = ["telegram:support-bot"]   # empty/absent = no restriction
```

Legacy form still parses:

```toml
name = "old-style"
api = "openai-chat"
base_url = "https://api.openai.com/v1"
api_key = "sk-literal-value-here"     # DEPRECATED — parses, but harw doctor
                                       # hard-errors: plaintext secret in config
```

`harw doctor`'s plaintext-secret detector treats any non-empty `api_key`
field as an automatic hard error (§5), regardless of whether the value looks
like a real key — the field's mere presence is the violation, since its
entire purpose historically was holding a literal secret.

### 2.6 `models/*.toml`

Extended with alias and capability metadata (still backward compatible —
all new fields are `#[serde(default)]`):

```toml
# .harw/models/gpt-5.toml
id = "gpt-5"                          # canonical ModelId
name = "GPT-5"
provider = "openai"                   # provider route
aliases = ["gpt5", "openai-flagship"]
context_window = 400000
max_tokens = 128000
reasoning = true
input_types = ["text", "image"]

[capabilities]
tool_use = true
streaming = true
vision = true
json_mode = true
```

### 2.7 `agents/*/agent.toml`, `skills/*/skill.toml`, `mcps/*.toml`

`AgentToml.skills` remains the declared skill relation. The optional nested
`[suggestions]` table is distinct: it names advisory skills, plugins, and MCP
servers that a catalog resolver may expose as concise definitions when this
agent is spawned. It is never an enable/install/permission grant.

```toml
# .harw/agents/researcher/agent.toml
name = "researcher"
skills = ["source-review"]

[suggestions]
skills = ["source-review", "web-research"]
plugins = ["git-review"]
mcps = ["docs"]
```

`mcps/*.toml` contains declarative metadata (`name`, `description`,
`transport`, endpoint or command metadata, tools, and `enabled`). Discovery
does not launch a server. Runtime launch is permitted only after catalog,
sandbox, and policy selection against a frozen per-run snapshot.

`transport = "streamable_http"` is the default deployment choice for Harwness:
it centralizes MCP upgrades, survives worker restarts, and fits shared Telegram
and job-oriented workflows. Remote endpoints must use HTTPS (plain HTTP is
loopback-only) and require the effective `NetworkAccess` permission. `stdio`
is reserved for explicitly local worker integrations and additionally requires
`ExecuteProcess`; it is started only through the Bubblewrap backend.

The standalone Harwness MCP listener, once enabled by the runtime composition,
binds the validated loopback `mcp_listener.listen_addr` (default
`127.0.0.1:1337`) with the fixed `/mcp` Streamable HTTP endpoint. Local channel
and job-worker integrations therefore use `http://127.0.0.1:1337/mcp` by
default; this is an endpoint identity, not a substitute for the separate HTTPS
requirement for remote MCPs. A remote binding is rejected by configuration: it
must terminate TLS and authenticate before forwarding into this local listener.

```toml
# .harw/mcps/workflow.toml
name = "workflow"
description = "Tenant-scoped task and workflow operations"
transport = "streamable_http"
url = "https://mcp.example.com/workflow"
auth = "env:HARW_WORKFLOW_MCP_TOKEN"
tools = ["tasks.search", "tasks.claim", "tasks.complete"]
enabled = true
```

### 2.8 `plugins/*.toml` — CapabilityBundle manifests

```toml
# .harw/plugins/slack-bridge.toml
name = "slack-bridge"
version = "0.3.0"
source = "registry"                    # "registry" | "path" | "git"
enabled = true
description = "Slack channel binding (future harw-channel-slack)"

[capabilities]
tools = ["slack.send", "slack.read"]
channels = ["slack"]
network_egress = ["slack.com"]
```

Implemented as a declarative `PluginToml` manifest in `harw-config`. Discovery
does not load plugin code; the runtime must validate manifest provenance,
operator approval, and a frozen per-run catalog snapshot before activation.

## 3. Merge semantics

| Section | Merge rule |
|---|---|
| `config.toml` | **Replace-whole** per layer — the last layer that defines `config.toml` wins entirely (matches existing behavior: `resolved.harness = cfg`). Fields are not merged field-by-field across layers, only whole-file replacement, to keep "what does my effective config.toml look like" answerable by reading exactly one file. |
| `auth.toml` | **Replace-whole per layer**, same rationale as `config.toml` — but keys inside `[credentials]` are useful to *reference* across layers (a repo `.harw/auth.toml` can add new refs without repeating the home-level ones)? No: to keep the security story simple ("one file is the whole picture of what this layer trusts"), `auth.toml` also replaces whole-file per layer that defines it. Operators who want additive behavior put all credentials in one layer. |
| `channels/*.toml` | **Merge by `ChannelId`** (the `id` field) across layers — a repo layer can add a new channel binding or override an existing one by re-declaring the same `id`; last layer wins per-id, matching `providers`/`models`/`agents`/`skills` today. |
| `providers/*.toml` | **Merge by name** (existing behavior, unchanged). |
| `models/*.toml` | **Merge by id** (existing behavior, unchanged, `aliases` do not need separate merge logic — an alias collision across layers is last-wins same as any other field since the whole file replaces per name). |
| `agents/*/agent.toml` | **Merge by name** (existing, unchanged). |
| `skills/*/skill.toml` | **Merge by name** (existing, unchanged). |
| `keybindings.toml` | Not parsed by `harw-config` yet (§2.3) — pointer only. When `harw-tui` implements it, expected rule: merge by action key (`quit`, `approve`, ...) so a repo layer can rebind one key without re-declaring the whole map — deferred, not implemented here. |
| `plugins/*.toml` | Merge by name; last layer wins for the whole manifest. Discovery is declarative only. |
| `mcps/*.toml` | Merge by name; last layer wins for the whole server metadata record. Discovery never launches an MCP process. |

## 4. Secret-reference grammar

Shared verbatim across `providers/*.toml` (`auth`), `channels/*.toml`
(`bot_token_ref`, `secret_token_ref`), and `auth.toml` (`[credentials]`
values, `[kek]` provenance fields). Defined once as `SecretRef` in
`harw-config::auth_toml`:

```
env:NAME            -> read from environment variable NAME at resolve time
file:PATH           -> read file contents at PATH (trimmed of trailing newline)
keyring:ENTRY       -> read from the OS keyring under service/entry ENTRY
                       (format: "service/username" or a single opaque entry name)
secrets:ID          -> read from harw-secrets's sealed SecretStore by SecretId/name
```

Parsing rules (`SecretRef: FromStr`):
- Exactly one of the four prefixes, followed by `:` and a non-empty
  remainder — anything else (no prefix, unknown prefix, empty remainder)
  is a hard parse error, **not** treated as a literal secret value. This is
  the load-bearing rule: `harw-config` must never silently accept a bare
  string as a secret in a field typed `SecretRef`.
- `Deserialize` for `SecretRef` goes through `FromStr` (`#[serde(try_from = "String")]`
  or a manual `Deserialize` calling `str::parse`) so a malformed ref fails at
  TOML-parse time with a clear location, not later when something tries to
  resolve it.
- Resolution (actually reading the environment/file/keyring/secrets-store)
  is explicitly **not** `harw-config`'s job — this crate only parses and
  validates the grammar; resolving `keyring:`/`secrets:` refs requires
  `harw-secrets`, which does not exist yet, so `SecretRef::Keyring`/`Secrets`
  variants parse successfully today but have no resolver — that resolver
  callable lives in whichever crate `cargo add`s `harw-secrets` once it's
  implemented (`harw-provider`, `harw-channel-telegram`, etc.), consistent
  with `docs/design/secrets-and-audit.md`'s "harw-secrets is called by, not
  the same crate as, harw-config."

## 5. Validation rules (`harw doctor`)

Not implemented as a `harw doctor` binary in this pass (no such crate/binary
exists yet) — but `harw-config` exposes the data `harw doctor` will need
(`ConfigError` variants below) and the checks it must perform once it exists:

1. **Duplicate names within one merge key.** Two `providers/*.toml` files
   declaring `name = "openai"` in the *same* layer is already an ambiguous
   overwrite (last file read wins, order is filesystem-dependent) — flagged
   as `ConfigError::DuplicateName { kind, name }` rather than silently
   picking one. Cross-layer duplication is fine (that's how overriding
   works); same-layer duplication is not.
2. **Unresolved references.** Every `SecretRef` must parse (§4); every
   `providers` entry named in an `agent.toml`'s `providers`/`primary_provider`
   list must exist in `ResolvedConfig::providers` after full discovery;
   every `models` entry's `provider` field must name a provider that exists;
   every `channel.telegram.*_ref` must be a well-formed `SecretRef`.
   Reported as `ConfigError::UnresolvedRef { kind, reference }`.
3. **Plaintext-secret detection → hard error.** Any deprecated `api_key`
   field with a non-empty value on a `ProviderToml`, or (were it ever added)
   any field typed `String` instead of `SecretRef` holding something that
   looks like a live credential, is a hard error:
   `ConfigError::PlaintextSecret { file, field }`. This is intentionally not
   a warning — the whole point of the `SecretRef` grammar is that plaintext
   never round-trips through a config file on disk.
4. **Channel token-reuse.** Two `[[channel.telegram]]` entries (possibly in
   different files) resolving to the same `bot_token_ref` is an error
   (`docs/design/channel-ingress-telegram.md` §3.1) —
   `ConfigError::DuplicateChannelToken { reference }`.
5. **Layer/file existence sanity.** `config.toml` missing from every layer,
   an `agents/*/agent.toml` naming a `system_file` that does not exist on
   disk, or a `channels/*.toml` naming a `keybindings_file` that does not
   exist — all surfaced as `ConfigError::ReadFailed`/`ConfigError::Invalid`
   (existing variants) or the new variants above.

## 6. Open questions

- **`plugins/*.toml` shape** depends on `harw-catalog::CapabilityBundle`,
  which is not implemented yet — §2.8's schema is a sketch, not locked.
- **`keybindings.toml` merge-by-action-key** is designed but not implemented;
  belongs to `harw-tui`, not `harw-config`, once that crate exists.
- **`auth.toml` whole-file-replace vs. `[credentials]` sub-merge** (§3) — the
  simpler whole-file rule is chosen for this pass; may need revisiting once
  real multi-layer deployments (home + repo) show whether operators actually
  want additive credential refs across layers.
- **Where `harw doctor` itself lives** (own crate vs. subcommand of an
  existing `harw` CLI binary) is not decided — this document only specifies
  the checks, not their home.
</content>
