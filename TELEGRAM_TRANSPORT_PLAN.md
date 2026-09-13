# Secure Telegram Bot Integration for Harwness — Telegram-Scoped

## Context

Harwness already has a fully-designed, largely-implemented "channel ingress"
architecture (`harw-channel`, `harw-channel-telegram`) that handles pairing,
replay-dedup, admission, sandboxing-by-intersection, and outbound rendering
for Telegram — entirely without touching the network. Two authoritative
design docs already exist in-repo and were read in full to ground this plan:
`docs/design/channel-ingress-telegram.md` (transport/rendering/security
model) and `docs/design/telegram-sandbox-work-requests.md` (the mutating-ops
contract: Telegram supplies *intent only*, the harness resolves identity,
workspace, sandbox, and approval server-side).

Confirmed by direct code inspection, the piece missing is specifically the
**real, token-owning Telegram transport**: `harw-channel-telegram`
deliberately has zero `reqwest`/`tokio` dependencies; `TelegramChannel::run_ingress`
fails closed (`TelegramChannelError::IngressUnavailable`) unless an external
transport feeds it a `std::sync::mpsc::Receiver<InboundEvent>` via
`with_ingress_receiver`; and `harw-cli/src/gateway.rs` hard-codes
`TelegramIngressMode::DisabledUntilSecureAdapterTransport` with the
`channels` subsystem future permanently `std::future::pending()`.

**Scope boundary — explicit per user instruction.** Another effort (Codex)
is concurrently working on everything else on the broader punch list: DSL →
runtime integration, managed child-agent/core-bridge composition, MCP
lifecycle correlation, the typed tool-failure wire boundary, and general
verification. **This plan touches Telegram only.** It does not modify
`harw-core` (`AgentSession`, `SpawnContext`, `run_turn`, `turn_loop.rs`),
`harw-sandbox` (`WorkspaceRegistry`, `SandboxSpec`, the `bwrap` launcher),
`harw-session-store`, `harw-tool-fs`, `harw-tool-shell`, or any non-Telegram
code in `harw-cli`. Those crates already contain the enforcement machinery
this integration depends on (confirmed: `harw-core::turn_loop::tool_execution_context`
already refuses to run any tool without a resolved `SpawnContext.sandbox` —
`harw-core/src/turn_loop.rs:396-410`) — wiring an actual agent turn to an
admitted Telegram event is exactly the "core-bridge composition" /
"DSL → runtime integration" work Codex owns, not this plan's job.

This plan's code changes are confined to: `harw-channel-telegram` (existing
crate), a new `harw-channel-telegram-transport` crate, and the
Telegram-specific TOML schema in `harw-config/src/channel_toml.rs`. The one
necessarily-shared file touched is `harw-cli/src/gateway.rs`, and only the
lines already dedicated to Telegram (`TelegramIngressMode`,
`telegram_ingress_mode()`, `telegram_ingress_status()`, the `channels`
future) — no other subsystem in that file is touched.

**The hand-off seam.** Once this plan's work lands, `TelegramChannel` will
correctly admit real Telegram traffic and produce `Dispatch::Admitted`
events (via `harw-channel::dispatch_inbound`, which already exists and is
channel-agnostic) and this plan's transport crate will expose an outbound
sender capable of `sendMessage`/`editMessageText`/`answerCallbackQuery`
calls. Actually constructing an `AgentSession` for an admitted event,
running a turn, and streaming results back is the "bridge" — deliberately
**not** built here, since it requires touching `harw-core`/`harw-sandbox`
session-construction, which is out of scope. This plan defines the seam
precisely (§ "Hand-off interface") so that work can plug in without needing
any changes on the Telegram side.

**Decisions already made with the user:**
- Two new pieces: `harw-channel-telegram-transport` crate (HTTP/reqwest/tokio)
  plus enhancements to the existing `harw-channel-telegram` crate — no new
  bridge/runtime crate.
- **Both long-poll and webhook** transports, built from the start.
- A **device/identity-pinning gate**: only a pre-configured, pinned set of
  Telegram identities may ever reach the bot — checked first, before
  pairing or anything else; non-matching traffic is silently dropped (no
  reply, so an unrecognized sender can't even confirm the bot is reachable).
- Ingress stays **fail-closed by default** — the gateway will only ever
  enable real Telegram ingress once every precondition it can check
  (config, secrets, device-pinning list, *and* the presence of a registered
  hand-off consumer from the runtime-bridge work) actually holds. Until
  Codex's runtime-bridge work lands and registers itself, the transport can
  be fully built and tested but the gateway keeps `channels` parked on
  `pending()` exactly as today — this plan does not flip that switch on its
  own authority.
- Reference material: `inspirations/openclaw` (different project, TypeScript)
  confirms two conventions worth carrying over in spirit: never stream raw
  token-deltas to an external channel (coalesce/throttle instead), and
  channel/transport code stays strictly presentation/transport-only, never
  owning command trees or policy — both already match this plan's design.

---

## Architecture overview (Telegram side only)

```
Telegram servers
  │  long-poll getUpdates   OR   webhook POST + secret_token
  ▼
harw-channel-telegram-transport         [NEW crate — reqwest/tokio]
  client.rs        Bot API HTTP client (getMe, getUpdates, sendMessage,
                    editMessageText, getFile, setMyCommands,
                    answerCallbackQuery, setWebhook/deleteWebhook)
  ingress_long_poll.rs   dedicated OS thread, own mini tokio runtime
  ingress_webhook.rs     axum/hyper listener as a tokio::task on whatever
                          runtime the caller supplies
  mapping.rs        Update -> InboundEvent, plus pure parsing of the closed
                     `/request /review /approve /deny /cancel` command
                     grammar into a typed enum (parsing only — no
                     workspace/sandbox resolution, which is out of scope)
  dedup.rs          in-process fast-path sliding window (perf only — the
                     durable replay gate is PairingStore::claim_once,
                     already implemented, unchanged)
  render.rs         ChannelSendOp -> actual HTTP calls; throttled
                     editMessageText streaming; per-chat/global token buckets
  media.rs          file_id -> getFile -> size-check -> MIME sniff -> bytes
  offset.rs         durable long-poll offset persistence (self-contained —
                     does not extend harw-session-store)
  error.rs          TelegramTransportError (hand-written, no anyhow/thiserror)
  │
  │  std::sync::mpsc::Sender<InboundEvent>  (bounded)
  ▼
harw-channel-telegram (EXISTING crate, unchanged network posture)
  TelegramChannel::with_ingress_receiver(...).run_ingress(sink)
  admit() / derive_session_key() / render_outbound()
  + NEW: device/identity-pinning check (first predicate in admit())
  + CHANGED: opaque approval-callback tokens, replacing the current
    insecure `format!("{request_id}:{decision}")` payload (telegram.rs:293)
    — self-contained pending-approvals table keyed by (chat_id, message_id),
    stored inside this crate, no reach into harw-session-store
  │
  │  Dispatch::Admitted { key: SessionKey, event }   [existing type/fn,
  │  via harw_channel::dispatch_inbound — already generic, untouched]
  ▼
  >>> HAND-OFF SEAM — not built in this plan, owned by the runtime-bridge
  >>> work in progress elsewhere (session construction, SandboxSpec
  >>> resolution, run_turn, TurnEvent streaming back into this crate's
  >>> outbound sender)
```

### Hand-off interface

To make the seam concrete without implementing the far side, this plan
defines (in `harw-channel-telegram-transport`, as a plain trait with no
`harw-core`/`harw-sandbox` dependency):

```rust
/// Sink a runtime bridge registers to receive admitted Telegram events.
/// Implemented by whatever composes session construction — not by this crate.
pub trait AdmittedEventConsumer: Send + Sync {
    fn handle_admitted(&self, key: SessionKey, event: InboundEvent);
}

/// Outbound sending capability the runtime bridge holds onto to render
/// turn output back to the chat. Implemented here, consumed elsewhere.
pub trait TelegramOutbound: Send + Sync {
    fn send(&self, chat_id: i64, thread_id: Option<i64>, content: &OutboundContent);
    fn edit(&self, chat_id: i64, message_id: i64, content: &OutboundContent);
}
```

`harw-cli/src/gateway.rs`'s Telegram-specific section constructs the
transport and `TelegramChannel`, and — only if a `Box<dyn
AdmittedEventConsumer>` has actually been registered by the (separately
developed) runtime-bridge — wires `run_ingress` to call it; otherwise it
stays in the existing fail-closed `pending()` state. This is the entire
extent of this plan's gateway.rs involvement: no session/turn logic, just
"is a consumer registered, yes or no."

---

## Work items

### 1. Bot API transport crate (`harw-channel-telegram-transport`)

New workspace crate depending only on `harw-channel-telegram` + `harw-channel`
(never `harw-core`/`harw-sandbox`). New deps via `cargo add` (no hand-pinned
versions): `reqwest` (matching the version already used in `harw-cli`),
`tokio`, `secrecy`/`secrecy_08`, `backon` (retry/backoff — already reserved
as a TODO comment in `harw-channel/Cargo.toml:21`), `axum` or `hyper`
(webhook server).

- **`client.rs`**: `TelegramClient` holding `reqwest::Client` +
  `secrecy::SecretBox<str>` token, with a hand-written `Debug` impl that
  redacts the token (never derive `Debug` naively here). Methods: `get_me`,
  `get_updates(offset, timeout_secs, allowed_updates)`, `send_message`,
  `edit_message_text`, `get_file`, `download_file(max_bytes)`,
  `answer_callback_query`, `set_my_commands`, `set_webhook`,
  `delete_webhook`. 429 handling reads `retry_after` from Telegram's JSON
  error body (not a header) and sleeps exactly that long; 5xx gets
  exponential backoff via `backon` (base 1s, factor 2, cap ~30s, ~5 attempts).
- **`ingress_long_poll.rs`**: runs on its own dedicated `std::thread`, not a
  `tokio::task` sharing the gateway's `new_current_thread()` runtime
  (`harw-cli/src/gateway.rs:110`). A 30s `getUpdates` long-poll sharing that
  single-threaded executor risks starving other gateway subsystems on any
  CPU-bound hiccup. The thread builds its own small
  `tokio::runtime::Builder::new_current_thread()` (or uses
  `reqwest::blocking`), communicating outward only via the existing
  synchronous `Sender<InboundEvent>` — no `ChannelAdapter` interface change.
- **`ingress_webhook.rs`**: async HTTP listener (fine to run as a
  `tokio::task` on a shared runtime — accept/serve is what a current-thread
  executor multiplexes well). Verifies `X-Telegram-Bot-Api-Secret-Token`
  against the resolved `secret_token_ref` **before** parsing the body (401
  fast-reject). Converges on the same `InboundEvent` sink as long-poll.
- **`mapping.rs`**: `RawUpdate` → `InboundEvent`. `update.update_id` →
  `raw_event_id` (mandatory). `message.chat.id` → `PeerId`.
  `message.message_thread_id` → `ThreadRef`. Mention detection via
  `entities` scan + reply-to-bot (bot's own id/username resolved once via
  `get_me()` at startup, cached). Also: pure parsing of `/request
  <workspace-alias> <role> <task text>` / `/review <work-id>` / `/approve
  <work-id>` / `/deny <work-id>` / `/cancel <work-id>` into a typed
  `TelegramCommand` enum — parsing/validation of the grammar's *shape* only
  (conservative ASCII slug for the alias, rejecting anything containing
  `/`, `..`, `~`, or non-slug characters at the syntax level); resolving an
  alias against a real `WorkspaceRegistry` is explicitly out of scope here.
- **`dedup.rs`**: small in-process sliding window, documented as a
  performance optimization only — the actual security boundary is
  `PairingStore::claim_once` (already implemented, durable, file-locked,
  unchanged by this plan).
- **`render.rs`**: throttled `editMessageText` streaming. Holds an
  **in-memory-only** `(chat_id, status_key) -> message_id` cache (never
  durable — a restart just starts a fresh message). A per-chat token bucket
  (from `TelegramRateLimitToml.max_outbound_per_chat_per_sec`, default 1/s)
  and a global bucket (~30/s) gate actual HTTP calls; callers of
  `TelegramOutbound` coalesce updates rather than firing one HTTP call per
  delta, matching the "no token-delta channel messages" convention observed
  in `inspirations/openclaw`. Reuses the existing, already-tested
  `harw_channel::adapter::chunk_text` for the ~4096-UTF-16-unit split. New
  `sendMessageDraft`/Bot API 9.5+ streaming (confirmed real via the
  official changelog — introduced 9.3 Dec 2025, opened to all bots 9.5
  March 2026) is represented as a `StreamingStrategy::ThrottledEdit` enum
  with a reserved-but-unimplemented `Draft` variant, pending live
  verification of its exact request/response shape.
- **`media.rs`**: `getFile` → check declared size against the configured
  ceiling *before* downloading → stream to local cache → MIME sniffed from
  content (never trust Telegram's reported `mime_type`).
- **`offset.rs`**: durable long-poll offset persistence, self-contained
  inside this crate (file-lock + atomic-replace via `fs4`/`tempfile`,
  mirroring the idiom already used by `harw-channel::pairing_store` — but
  implemented locally here, not by extending `harw-session-store`). Path:
  `<profile>/channel-state/telegram/<channel_id>/offset.json`.
- **`error.rs`**: hand-written `TelegramTransportError` — `Transport(reqwest::Error)`,
  `ApiRejected{method,code,description}`, `TokenResolution{reference}`,
  `MalformedUpdate{update_id,reason}`, `AttachmentRejected{reason}`,
  `WebhookAuth`, `Config(harw_config::Error)` — `Display`/`Debug`
  (delegates)/`source()`/`From` impls, no `anyhow`/`thiserror`.

### 2. Device/identity pinning gate (`harw-channel-telegram`)

New admission predicate in `TelegramChannel::admit()`, checked **first**,
before pairing-deferral or mention logic. Extends
`TelegramChannelConfig`/`channel_toml.rs` with a pinned-identity list
(explicit Telegram user/chat IDs the operator configures). Non-matching
senders are silently dropped — no onboarding reply, so an unrecognized
sender cannot even confirm the bot exists. This closes the design doc's
"Open Question 1" for this deployment's threat model. Pairing still runs
*after* this gate for any operator wanting multi-tenant behavior among the
pinned set; single-operator deployments pin exactly one identity.

### 3. Opaque approval-callback tokens (`harw-channel-telegram`)

Replace `harw-channel-telegram/src/telegram.rs:293`'s
`callback_payload: format!("{}:{}", prompt.request_id, action.decision)`
with an opaque, random token generated per pending approval, verified
against a pending-approvals table keyed by `(chat_id, message_id)` —
**stored inside `harw-channel-telegram` itself**, not in
`harw-session-store`. A callback can never carry command arguments, paths,
or a capability set — only "approve/deny this exact previously-rendered
prompt." Wrong peer/thread/stale/replayed callback → terminal denial. This
is a self-contained fix to an existing file already inside this plan's
scope; it does not require the actual `WorkRequest`/sandbox resolution
machinery (which stays with the runtime-bridge work) to exist yet — it only
fixes how the *callback token itself* is generated and verified.

### 4. Config schema (`harw-config/src/channel_toml.rs`)

Extend `TelegramChannelToml` with the pinned-identity list field (and any
webhook-specific fields not already present). No changes to non-Telegram
config modules.

### 5. Gateway wiring (`harw-cli/src/gateway.rs`, Telegram section only)

Replace `TelegramIngressMode::DisabledUntilSecureAdapterTransport` with a
precondition-checked variant, touching only the already-Telegram-dedicated
lines in this file:

```rust
enum TelegramIngressMode {
    Enabled(TelegramIngressPlan),
    DisabledUntilSecureAdapterTransport(String), // now carries the reason
}
```

Preconditions: config declares an enabled Telegram binding; `bot_token_ref`
(and, for webhook, `secret_token_ref`) resolve; the device-pinning list is
non-empty and valid; **and** a runtime-bridge `AdmittedEventConsumer` has
been registered (by the separately-developed session/turn composition
work — this plan only checks for its presence, it does not implement it).
Any unmet precondition → stay `Disabled`, log the specific reason via
`tracing::warn!` (structured, never leaking secret values), `channels`
falls back to `std::future::pending()` exactly as today. This guarantees
Telegram ingress cannot go live from this plan's work alone — it requires
the concurrently-developed runtime bridge to also be present, matching the
"don't enable ingress until the full pipeline exists" decision without this
plan needing to build that pipeline itself.

---

## BotFather / operator setup → config mapping

| Real-world step | Codebase mapping |
|---|---|
| `@BotFather` → `/newbot` → name + `@username` | No code; produces the bot token |
| `/setprivacy` (Disabled = bot sees all group messages; Enabled = only `/commands`+mentions) | Interacts with `groups.require_mention` — if privacy stays Enabled, Telegram itself won't deliver unmentioned group messages |
| `/setcommands` | `client.set_my_commands(...)` at startup |
| Retrieve token | Stored via `SecretRef` — `env:TELEGRAM_SUPPORT_BOT_TOKEN` or `secrets:telegram-support-bot-token`. Literal tokens are a parse-time hard error (already tested, `channel_toml.rs:229`) |
| Pinned identity (device gate) | Operator notes their own Telegram numeric user id (e.g. via `@userinfobot`) and adds it to the new pinned-identity config list |
| Long-poll vs webhook | Both built; `transport` config field selects per-binding behavior; webhook needs `transport_webhook.public_url` (behind the operator's own TLS-terminating reverse proxy), `listen_addr`, and a random `secret_token_ref` registered via `setWebhook` |
| Test end-to-end | Requires the runtime-bridge precondition (§5) to also be satisfied — outside this plan's authority to flip on alone |

---

## Testing strategy

- **Unit** (no network/filesystem beyond `tempfile`): `mapping.rs` field
  mapping incl. edge cases and command-grammar parsing (valid/invalid alias
  shape, traversal/absolute/URL rejection at the syntax level); `dedup.rs`
  window eviction; token-bucket rate limiter with an injectable clock;
  `render.rs`'s `(chat_id, status_key) -> message_id` cache; `client.rs`
  retry/backoff decision extracted as a pure function; device-pinning
  admission predicate; opaque-callback-token generation/verification
  (replay, wrong-chat, wrong-message-id rejection).
- **Integration**, mocked Bot API: a small hand-rolled local HTTP responder
  (`axum`/`hyper`, already transitively available via `reqwest`) on
  `127.0.0.1:0` — no `wiremock`/`mockito`. Covers 429/Retry-After, 5xx
  backoff timing, long-poll offset advancement + restart-resume, webhook
  401-before-parse behavior.
- **`#[ignore]`d, real BotFather bot**: one end-to-end smoke test
  (`TELEGRAM_TEST_BOT_TOKEN` env-gated, skips cleanly if unset) verifying
  live `getUpdates`/`sendMessage` JSON shapes match `mapping.rs`.

---

## Critical files

- `docs/design/channel-ingress-telegram.md`, `docs/design/telegram-sandbox-work-requests.md` — authoritative specs (Telegram-relevant portions only)
- `harw-channel-telegram/src/telegram.rs` — `TelegramChannel`, `with_ingress_receiver`, `admit()` (device-pinning goes here), the insecure callback payload at line 293 (to replace)
- `harw-channel-telegram/src/config.rs` — config surface to extend
- `harw-channel/src/adapter.rs`, `harw-channel/src/dispatch.rs` — `ChannelAdapter`, `dispatch_inbound` (consumed, not modified)
- `harw-config/src/channel_toml.rs` — `TelegramChannelToml` schema extension
- `harw-cli/src/gateway.rs` — Telegram-dedicated lines only (`TelegramIngressMode`, `telegram_ingress_mode()`, `telegram_ingress_status()`, the `channels` future)
- `harw-channel/src/pairing_store.rs` — the file-lock/atomic-write idiom to mirror locally in the new transport crate's `offset.rs`

**Explicitly not touched by this plan**: `harw-core/*`, `harw-sandbox/*`,
`harw-session-store/*`, `harw-tool-fs/*`, `harw-tool-shell/*`, any
non-Telegram section of `harw-cli`.

## Execution note

Per this project's standing rules, once Rust source files are written and
`cargo check` passes, `rust-test-designer`, `rust-doc-writer`, and (since a
new `error.rs` is created) `rust-error-designer` must be spawned
concurrently in the background before any work item above is marked
complete. Once this plan is approved, the plan content will also be copied
to the workspace root (plan mode restricts edits to the designated plan
file only, so that copy happens immediately after approval).
