# Channel Ingress Layer — Design, with Telegram as First Binding

> Status: implemented · Last reviewed: 2026-09-24

Scope: `harw-channel` (core abstraction) + `harw-channel-telegram` (first concrete binding)
Depends on: `harw-types` (IDs, `SessionKey`), `harw-session-store` (append-only journal), `harw-sandbox` (visibility scopes), `harw-catalog` (capability manifests), `harw-protocol` (approvals, events)

---

## 1. Why a channel layer exists

Harwness sessions are addressed by a stable `SessionKey`, resolved to a `SessionId` by the session store. Everything upstream of that resolution — "a human typed something into Telegram/Slack/email" — is the concern of this layer. The channel layer's only job is:

1. Turn a wire-format inbound event (Telegram `Update`, Slack event, IMAP message, …) into a harness-native `InboundEvent` addressed at a `SessionKey`.
2. Turn harness-native outbound content (`TurnItem`s, approval prompts, status updates) into whatever the remote surface can render, respecting that surface's capability limits.
3. Enforce the channel's own admission gate (allowlist/pairing, mention rules, rate limits) *before* anything reaches the session/tool/policy layers — a channel is a perimeter, not a trust boundary override.

A channel adapter never grants capabilities. It can only reduce what a session may do (§5) and it never fabricates catalog entries, policy grants, or filesystem access on its own authority.

---

## 2. `harw-channel`: the core abstraction

### 2.1 Identity types

```rust
// harw-types (extends ids.rs with the same newtype_id! macro pattern)
newtype_id!(ChannelId);   // e.g. "telegram", "telegram:support-bot", "slack:acme-workspace"
newtype_id!(PeerId);      // channel-local identity of the human/entity: Telegram user id, Slack user id, email address hash
newtype_id!(TenantId);    // the harwness-side account/organization this peer is bound to
newtype_id!(ThreadRef);   // optional: channel-local sub-conversation (Telegram thread_id, Slack thread_ts, email In-Reply-To chain)
```

`ChannelId` is not just "telegram" — it identifies a *configured binding* (one bot token, one workspace app install, one mailbox), because an operator may run several Telegram bots. The channel kind (`telegram`, `slack`, …) is a field on the binding's config, not encoded uniquely in `ChannelId` beyond a namespacing convention (`telegram:<bot-slug>`).

`PeerId` is channel-local and meaningless outside that channel — a Telegram numeric user id has no relationship to a Slack user id for the same human. Binding a `PeerId` to a `TenantId` is what pairing (§3.2) accomplishes; that binding is authoritative harness-side state, not something a channel infers from message content.

### 2.2 `SessionKey` derivation

```rust
pub struct SessionKey {
    pub tenant: TenantId,
    pub channel: ChannelId,
    pub peer: PeerId,
    pub thread: Option<ThreadRef>,
}
```

Derivation is a pure function `ChannelAdapter::derive_session_key(&InboundEvent) -> SessionKey` — deterministic, no I/O, no side effects, so replays and recovery-marker reconciliation in the session store produce identical keys for identical events. Two inbound events with the same `(tenant, channel, peer, thread)` tuple resolve to the same session; changing any field is by definition a different conversation. This is the one place channel-specific "what counts as a separate conversation" logic lives (Telegram DM topic vs. group forum topic vs. flat group — see §3.3); every other layer only ever sees the resulting `SessionKey`.

`tenant` is deliberately not derivable from the wire event alone in the general case — it comes from the pairing/binding table (§3.2). Until a peer is paired, there is no `TenantId`, and the adapter must reject or degrade to an unauthenticated onboarding flow rather than inventing a tenant.

### 2.3 `ChannelAdapter` trait

```rust
/// Behavior every channel binding implements. Named after the behavior
/// (ingress + egress + capability declaration), not the transport.
pub trait ChannelAdapter: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Static, load-time description of what this channel can render/accept.
    fn capabilities(&self) -> ChannelCapabilities;

    /// Pure mapping from a wire event to a harness session address.
    /// No I/O; must be stable across process restarts.
    fn derive_session_key(&self, event: &InboundEvent) -> Result<SessionKey, Self::Error>;

    /// Admission gate: allowlist/pairing/mention/rate-limit checks that must
    /// pass before an inbound event is allowed to reach the session runtime.
    /// Returns Admitted, Rejected(reason), or Deferred (e.g. awaiting pairing approval).
    fn admit(&self, event: &InboundEvent, key: &SessionKey) -> Admission;

    /// Render outbound harness content into zero or more channel-native
    /// send operations, downgrading unsupported constructs per `capabilities()`.
    fn render_outbound(&self, content: &OutboundContent) -> Vec<ChannelSendOp>;

    /// Long-running ingress loop. Implementations push InboundEvent onto
    /// the provided channel; the harness core drives dispatch.
    fn run_ingress(&self, sink: std::sync::mpsc::Sender<InboundEvent>) -> Result<(), Self::Error>;
}

pub struct ChannelCapabilities {
    pub markdown: MarkdownSupport,     // None | BasicV1 | ExtendedTables | Native
    pub max_message_len: usize,
    pub attachments: AttachmentSupport,  // max size, allowed kinds, count limit
    pub edits: bool,                     // can amend a previously sent message
    pub reactions: bool,
    pub threads: ThreadSupport,          // None | Flat | Native (forum-style)
    pub inline_actions: bool,            // buttons/inline keyboards for approvals
}
```

Design notes:

- `admit` runs *before* `derive_session_key` is trusted for anything beyond routing — an event can be routed to compute a would-be key, checked against admission, and dropped without ever creating or touching a session. This keeps unauthenticated traffic from creating session-store journal entries.
- `render_outbound` is total: it always returns *something* renderable, downgrading rather than failing. A trait method should not need to signal "cannot render" for content the harness itself produced; it degrades (markdown → plain text, table → code block, edit → new message) per `ChannelCapabilities`.
- `run_ingress` owns the transport loop (long-poll, webhook server, IMAP idle, websocket) and is the only place a channel touches network I/O in the hot path. It is intentionally coarse-grained (own the loop, not a callback-per-update) so a binding can choose its own concurrency model (thread-per-poll vs. tokio task) without constraining the trait.

### 2.4 Attachment intake and `TaskPackage` hygiene

Any inbound attachment becomes a `TaskPackage` attachment candidate subject to the existing hygiene rules (assumed from prior design work): byte-size ceiling, per-message count ceiling, MIME allowlist/validation by sniffing (not trusting the channel-reported MIME string), a digest manifest entry (content hash recorded before the bytes are handed to any tool), and an expiry after which the blob is purged from local cache regardless of session lifetime. The channel layer's obligation is narrow: download once, validate early, and reject-with-reason before the bytes ever reach `TaskPackage` construction if a limit is exceeded — it does not get its own, looser limits.

---

## 3. Telegram binding (`harw-channel-telegram`)

### 3.1 Bot configuration (`harw-config` TOML schema)

Following the existing `harw-config` pattern of one typed `*_toml.rs` module per config surface (`provider_toml.rs`, `model_toml.rs`, …), Telegram bindings get `channel_telegram_toml.rs`:

```toml
# harwness.toml — one [[channel.telegram]] table array entry per bot binding
[[channel.telegram]]
id = "telegram:support-bot"          # -> ChannelId
tenant_binding = "pairing"           # "pairing" | "static" (see 3.2)
bot_token_ref = "env:TELEGRAM_SUPPORT_BOT_TOKEN"   # never a literal token in TOML
transport = "long_poll"              # "long_poll" | "webhook"

[channel.telegram.transport_webhook]  # only read when transport = "webhook"
public_url = "https://ingress.example.com/telegram/support-bot"
secret_token_ref = "env:TELEGRAM_SUPPORT_BOT_WEBHOOK_SECRET"
listen_addr = "127.0.0.1:8443"

[channel.telegram.groups]
require_mention = true
observe_unmentioned = false          # if true, log-but-don't-dispatch (see 3.4)
allowed_chats = []                   # empty = no groups admitted by default
group_allowed_senders = []           # sender allowlist scoped to groups only

[channel.telegram.topics]
mode = "per_topic_session"           # "per_topic_session" | "shared_session" (see 3.3)

[channel.telegram.rate_limit]
max_updates_per_peer_per_min = 20
max_outbound_per_chat_per_sec = 1    # stays under Telegram's ~30 msg/s global, ~1/s per-chat guidance

[channel.telegram.commands]
menu_source = "policy_visible"       # only commands the requesting peer's visibility scope can see are registered
unknown_command_fallback = "reply_help"  # "reply_help" | "ignore" | "route_to_agent"
```

Rules:
- `bot_token_ref` and `secret_token_ref` are always indirection strings (`env:...` today; a future `harw-secrets` crate can add `keyring:...`/`vault:...`). `harw-config` never accepts a literal token value — the loader rejects the config if a ref does not resolve, rather than silently treating a bare string as the token.
- One config entry = one bot = one `ChannelId`. Running the same bot token under two configs is a load-time error (`harw-config` cross-checks token refs are not duplicated across `[[channel.telegram]]` entries), matching the real Telegram constraint that only one process may long-poll a given token.

### 3.2 Allowlist / pairing flow

Two `tenant_binding` modes:

- **`static`** — the operator hardcodes `PeerId → TenantId` mappings directly in config (simple single-operator deployments). No pairing flow; equivalent to Hermes's flat `TELEGRAM_ALLOWED_USERS`, but harwness always requires an explicit tenant, never an implicit "default tenant."
- **`pairing`** — the default, dynamic flow for multi-tenant or self-service deployments:

  1. An unpaired Telegram user DMs the bot. The adapter's `admit()` sees no `PeerId → TenantId` binding, so it does not derive a real `SessionKey`; instead it returns `Admission::Deferred(Onboarding)`.
  2. The adapter's onboarding responder (a narrow, session-free code path — it must not run through the full agent turn machinery) replies with a short pairing code and instructions: *"Give this code to your operator: `7F2K-9QRT`. It expires in 15 minutes."* The code is generated and stored (with expiry) by `harw-session-store`'s pairing table, not by the channel binding itself, so the same pairing primitive is reusable by future channels.
  3. Out of band (CLI, admin UI, or an `OperatorOnly`-gated command in an already-paired operator session — see §4), the operator runs `harw pairing approve 7F2K-9QRT --tenant acme-corp`. This writes the `PeerId → TenantId` binding as an append-only journal entry, same durability guarantees as session events.
  4. The next inbound message from that Telegram user resolves normally: `admit()` finds the binding, `derive_session_key()` produces a real key, and the session runtime proceeds.

  Pairing codes are single-use, short-TTL, and scoped to one `ChannelId` — a code issued by the support bot cannot be redeemed against a different bot binding, closing the "leaked code used on the wrong bot" case.

- Revocation: an operator can revoke a `PeerId → TenantId` binding at any time; this does not delete session history (append-only journal) but does make `admit()` reject subsequent events from that peer until re-paired.

### 3.3 Group vs. DM semantics; topics/threads

- **DM**: `PeerId` = Telegram user id. `ThreadRef` = Telegram forum topic `message_thread_id` when the user's DM has Topics mode enabled (Bot API 9.4+), else `None`. Root DM (`thread_id` absent or `1`/General) is treated as a distinct `ThreadRef::None` session — a "lobby" session in the operator-curated case, ordinary chat in the simple case. This mirrors the observed pattern in the Hermes reference material (root-DM-as-lobby vs. per-topic session) without adopting its state machine — harwness's session identity is derived structurally from `(chat_id, thread_id)`, with no separate "topic mode enabled" flag needed since `ThreadRef` naturally defaults to `None`.
- **Group/supergroup**: `PeerId` in a group context is *not* the sender — session identity in groups is keyed on the group, since a shared conversation is the point. We model this as `PeerId = ChatId` for group-scoped sessions, with the actual sending Telegram user id carried as a per-message attribute (`SenderRef`) on the `InboundEvent`, used for policy/audit, not for `SessionKey` derivation. This keeps "who is in the room" separate from "which conversation this is," matching how visibility scopes (`Self`, `DescendantTree`, `ExplicitlyGranted`, `OperatorOnly`) need to reason about a specific acting human inside a shared session.
- **Topics config (`channel.telegram.topics.mode`)**:
  - `per_topic_session` (default for forum-enabled groups): each `thread_id` maps to a distinct `SessionKey` (`thread` field populated) — matches native Telegram forum topic isolation, no extra bookkeeping needed since it falls directly out of `derive_session_key`.
  - `shared_session`: all topics in a chat collapse to `thread: None`, useful for a small team channel that wants one continuous conversation regardless of which topic a reply landed in. This is a config choice per binding, not per-message inference.
- Group membership alone is not authorization — `admit()` for group events additionally checks `groups.allowed_chats` / `groups.group_allowed_senders` from config, independent of pairing (a peer can be un-paired for DMs but allowed to trigger the bot inside an allowlisted group, or vice versa — the two allowlists are orthogonal, following the "sender vs. chat" split observed in the reference material).

### 3.4 Mention gating in groups

- `require_mention = true` (recommended default for any multi-purpose group): the bot only dispatches on `@botusername` mention, reply-to-bot, `/command@botusername`, or a configured wake-word pattern. All other group traffic is dropped at `admit()` before session creation.
- `observe_unmentioned = true` (opt-in, only meaningful alongside `require_mention = true`): non-triggering messages from an *already-allowlisted* chat are still appended to that chat's shared-session transcript as passive context (tagged with sender identity so the model treats them as observed room chatter, not direct instructions), but do not trigger a turn. This requires Telegram to actually deliver those messages to the bot (BotFather privacy mode off, or bot promoted to group admin) — the config loader should warn (not silently no-op) when `observe_unmentioned` is set but the operator hasn't documented that the corresponding BotFather setting was changed, since harwness cannot verify that server-side toggle itself.
- Mention parsing happens in `admit()`, before `derive_session_key` — a non-triggering message in a `require_mention` group never gets a `SessionKey` computed at all when `observe_unmentioned = false`, keeping "dropped noise" out of the session store entirely.

### 3.5 Rate limits

Two independent limiters, both enforced by the adapter (not delegated to core, since Telegram-specific numbers apply):
- **Inbound, per-peer**: `max_updates_per_peer_per_min` — protects against a single paired user hammering the bot; events beyond the limit are rejected at `admit()` with a throttling reason (and, if it's a human-visible surface, a single "you're sending too fast" reply, itself rate-limited so the warning doesn't spam).
- **Outbound, per-chat**: `max_outbound_per_chat_per_sec` — a token-bucket queue in front of `render_outbound`'s send operations, sized conservatively under Telegram's documented ~1 msg/sec/chat and ~30 msg/sec global guidance, so a burst of tool-progress updates from one long agent turn can't trip Telegram's own flood control (which would otherwise surface as opaque `429 Too Many Requests` errors deep in the send path).

### 3.6 Message chunking / markdown downgrade

`ChannelCapabilities::markdown` for the Telegram binding defaults to `BasicV1` (MarkdownV2-safe subset: bold, italic, code spans, code blocks, links) with `ExtendedTables`/`Native` opt-in only if the binding is later extended to Telegram's richer message API. `render_outbound` for Telegram:

1. Converts the harness's canonical markdown to Telegram MarkdownV2 escaping rules.
2. Flattens unsupported constructs the same tier the capability declares: tables become either row-bullet groups (narrow) or fenced code blocks (wide), consistent with the declared `markdown` tier rather than best-effort per-message guessing.
3. Splits any resulting text exceeding `max_message_len` (Telegram's ~4096 UTF-16 code unit cap) at paragraph/sentence boundaries into multiple `ChannelSendOp::SendMessage` operations, never mid-escape-sequence (so a chunk boundary can't land inside a MarkdownV2 escape and produce a malformed message).
4. Edits (`ChannelCapabilities::edits = true`) are used for progressive status updates (tool-progress, streaming previews) by keeping a `(chat_id, status_key) → message_id` cache identical in spirit to the pattern observed in the reference material, so repeated status pushes amend one bubble instead of spamming the chat — but this cache lives in the adapter's transient runtime state, never the session-store journal, since it's a rendering optimization, not conversation history.

### 3.7 Media/attachment intake

Telegram delivers attachments as `file_id` references, not bytes; the adapter must call `getFile` to resolve a download URL/path, then fetch. Intake pipeline:

1. `getFile` resolves size and path; if declared size already exceeds the configured byte ceiling, the adapter skips the download entirely and responds with a rejection message (no wasted download).
2. Bytes are streamed to local cache; MIME is *sniffed* from content, not trusted from Telegram's reported `mime_type` field.
3. On success, the attachment enters `TaskPackage` construction subject to the shared hygiene rules (§2.4): count ceiling per message/turn, digest manifest entry, expiry timer starting at intake (not at first use).
4. Telegram's public Bot API 20 MB `getFile` ceiling is the practical ceiling for this binding unless an operator runs a local Bot API server (out of scope for v1; noted as a future config knob `channel.telegram.local_bot_api_url` mirroring the pattern in the reference material, gated behind an explicit opt-in since it changes the trust boundary of file delivery).

---

## 4. Command parity and approval flows

### 4.1 Slash commands from Telegram

- BotFather's `/setcommands` menu is regenerated at bind startup (and on policy change) from the intersection of: commands registered in the harness's central command registry, and commands visible under the requesting peer's resolved visibility scope. A command an `OperatorOnly`-scoped peer cannot invoke should not appear in their `/` picker at all — `menu_source = "policy_visible"` means the menu is peer-relative, not one global static list, so `setMyCommands` is called per-scope (Telegram supports per-chat/per-user command scopes natively) rather than once globally.
- Commands arrive as ordinary `InboundEvent`s with a recognized `/name` prefix (with or without `@botusername` suffix, which the adapter strips before dispatch). Unknown `/foo` commands hit `unknown_command_fallback`: `reply_help` (default — points at `/help`), `ignore` (silent drop, useful for busy multi-bot groups), or `route_to_agent` (treat it as ordinary chat text — the agent decides whether it's a real request, useful for personas where "commands" are conversational).
- Command *authorization* is enforced by `harw-sandbox`, not the channel adapter — the adapter's job is only to attach the `SenderRef`/`SessionKey`/`ChannelId` context so policy can resolve visibility scope; the actual gate (e.g. an `OperatorOnly` command silently absent from the menu **and** rejected with an audit record if attempted anyway via typed text) lives in the policy layer, same as it would for a CLI-originated command. This keeps the channel layer from needing its own copy of authorization logic that could drift from the canonical policy engine.

### 4.2 Approval flows over Telegram

Work approvals (`ApprovalRequest`/`ApprovalResponse` in `harw-protocol::approvals`) render as inline keyboards:

```
⚠️ Exec approval requested (risk: elevated)
$ rm -rf /workspace/build-tmp
cwd: /workspace

[ ✅ Approve ]  [ ❌ Deny ]
```

- `render_outbound` maps an `ApprovalRequest` to a `ChannelSendOp::SendMessage` with inline keyboard buttons, one per available `ReviewDecision` variant (approve/deny; a future "approve once, always for this session" variant maps to an extra button if the protocol adds it).
- A button tap arrives as a Telegram `callback_query`, which the adapter converts directly into an `ApprovalResponse` — the mapping from callback payload to `ApprovalRequest.id` must be verified against an in-flight-approvals table keyed by `(chat_id, message_id)` so a stale or replayed callback for an already-resolved approval is rejected rather than silently re-applied.
- Every inbound approval decision — whether via button tap or typed `yes`/`no` fallback — is written to the audit log with: `SessionKey`, acting `SenderRef`, `ApprovalRequest.id`, decision, timestamp, and the raw channel event id (Telegram `update_id`) for forensic replay. Approvals are never accepted from a `PeerId` other than the one the request was addressed to, even if both are members of the same allowlisted group.
- Timeout: if no decision arrives within the configured window, the approval resolves to a safe default (deny) and the channel message is edited to reflect the timeout outcome, so the button state in Telegram never silently disagrees with the harness's actual decision.

---

## 5. Security model

- **No implicit escalation.** A channel binding carries a *sandbox profile* that can only intersect with (never union into) whatever capability bundle the resolved tenant/session would otherwise have. Concretely: `harw-sandbox` computes the session's rights from tenant/catalog/visibility-scope as usual, and the channel binding's profile (e.g. "Telegram-originated sessions never get raw filesystem write, regardless of tenant policy") is applied as an additional restriction, never as a grant. A channel adapter has no code path that can add a capability; it only has `deny`/`restrict` hooks.
- **Full audit of inbound commands.** Every admitted `InboundEvent` that results in a dispatched turn is journaled with: `SessionKey`, `SenderRef`, `ChannelId`, raw `update_id`, and a content hash (not necessarily full content, to keep the audit log from becoming a second copy of possibly-sensitive message bodies — full content already lives in the session transcript). Rejections at `admit()` are also logged (lighter-weight, no session context needed) so "bot ignored me" support questions are answerable from logs alone.
- **Replay/dedup via `update_id`.** Telegram's `update_id` is monotonically increasing per bot and is the basis for at-least-once delivery (long-poll `offset` acking, webhook retries). The adapter maintains a small sliding-window dedup cache (`update_id` → already-processed marker) so a Telegram-side retry of an update it never got an ack for cannot double-dispatch a turn or double-count toward rate limits. For long-poll, the confirmed `offset` is persisted (not just kept in memory) so a process restart resumes after the last acked update rather than re-processing or gapping.
- **Token storage.** Bot tokens and webhook secrets are never held as bare config values — `bot_token_ref`/`secret_token_ref` resolve through an indirection (`env:` today) at bind startup, held in memory only as long as the adapter needs them, and never logged (structured `tracing` fields for this module explicitly exclude the resolved token; only the *ref string*, e.g. `env:TELEGRAM_SUPPORT_BOT_TOKEN`, is safe to log).
- **Webhook mode specifics.** When `transport = "webhook"`, Telegram's `secret_token` header is verified on every inbound HTTP request before the body is even parsed as an `Update` — an unverified request is a 401, not a parse attempt. The listen address defaults to loopback-only; exposing it publicly is the operator's explicit responsibility (reverse proxy / TLS termination), matching the "never bind wide by default" posture used elsewhere in the harness's config surfaces.

---

## 6. Rust architecture sketch

### 6.1 Crate layout

```
harw-channel/                  # core, transport-agnostic
  src/
    lib.rs
    adapter.rs                 # ChannelAdapter trait, ChannelCapabilities
    ids.rs                     # re-exports/extends harw-types ids (ChannelId, PeerId, TenantId, ThreadRef)
    event.rs                   # InboundEvent, OutboundContent, ChannelSendOp, Admission
    pairing.rs                 # pairing-code primitive shared across channels (backed by harw-session-store)
    error.rs                   # Error enum (hand-written, no anyhow/thiserror)
    dispatch.rs                 # generic ingress→admit→session-key→dispatch pipeline the core drives

harw-channel-telegram/         # first concrete binding
  src/
    lib.rs
    config.rs                  # TelegramChannelConfig, parsed from harw-config's channel_telegram_toml
    client.rs                  # thin Bot API HTTP client (getUpdates, sendMessage, getFile, setMyCommands, answerCallbackQuery, ...)
    ingress_long_poll.rs        # long-poll loop
    ingress_webhook.rs          # webhook HTTP server loop
    mapping.rs                  # Telegram Update -> InboundEvent, derive_session_key, admit()
    render.rs                   # OutboundContent -> Telegram send ops (markdown downgrade, chunking, keyboards)
    media.rs                    # getFile + download + MIME sniff + TaskPackage handoff
    dedup.rs                    # update_id sliding window + persisted offset
    error.rs                    # TelegramChannelError (hand-written)
```

### 6.2 Event loop

- **Long-poll** (default): a dedicated `std::thread::spawn` loop per bot binding calling `getUpdates` with a long timeout (e.g. 30s) and the persisted `offset`. This is a natural fit for `std::thread` rather than an async task — one blocking HTTP long-poll per bot, CPU-idle between responses, no need for a shared runtime just for this. `harw-channel-telegram` can stay `tokio`-free entirely if the rest of the harness's I/O for this binding (outbound HTTP sends) also uses a blocking client; if the surrounding harness runtime is tokio-based for other reasons, the loop becomes a dedicated `tokio::task` per binding instead, sharing the runtime rather than spawning its own — chosen per how the surrounding `harw-core` runtime is set up, not fixed by this crate alone.
- **Webhook**: an HTTP server (one process-wide listener multiplexing multiple bot bindings by path, or one listener per binding — one listener per binding is simpler to reason about for token isolation, at the cost of more open ports) accepting POSTed `Update`s, verifying the secret-token header, and pushing onto the same `InboundEvent` sink the long-poll path uses — the two transports converge immediately after ingress so `mapping.rs`/`admit()`/dispatch code has no transport-conditional branches.
- Both transports feed a single `std::sync::mpsc::Sender<InboundEvent>` (or a bounded channel if backpressure matters — a bounded channel is preferable so a stalled session runtime naturally slows Telegram polling/ack rather than growing memory unboundedly) that the harness core drains.

### 6.3 Error handling

Per the workspace's standing rule (no `anyhow`/`thiserror`, hand-written enums with `From` impls and `source()`):

```rust
// harw-channel-telegram/src/error.rs
#[derive(Debug)]
pub enum TelegramChannelError {
    /// Bot API HTTP transport failure (network, timeout).
    Transport(reqwest::Error),
    /// Bot API returned a structured error payload (non-2xx or ok:false).
    ApiRejected { method: String, code: i64, description: String },
    /// Token or secret ref failed to resolve (missing env var, etc.).
    TokenResolution { reference: String },
    /// Inbound update failed schema validation (unexpected shape from Telegram).
    MalformedUpdate { update_id: Option<i64>, reason: String },
    /// Attachment exceeded configured byte/count ceiling before download.
    AttachmentRejected { reason: String },
    /// Webhook secret header missing or mismatched.
    WebhookAuth,
    /// Config parse/validation failure specific to this binding.
    Config(harw_config::Error),
}
// Display: human-readable, no jargon; Debug delegates to Display;
// impl std::error::Error::source() links Transport/Config to their inner error;
// From<reqwest::Error>, From<harw_config::Error> impls so `?` works end to end.
```

`harw-channel`'s own `error.rs` defines a transport-agnostic `ChannelError` (admission failures, pairing failures, generic dispatch errors) that `TelegramChannelError` variants map into via `From` where the core dispatch pipeline needs a uniform type; binding-specific detail (e.g. `ApiRejected`'s Telegram error code) is preserved as the `source()`.

### 6.4 Tracing fields

Following the workspace's structured-fields convention:

```rust
let span = tracing::info_span!(
    "telegram_ingress",
    channel_id = %config.id,
    transport = %config.transport_kind(),
);
let _guard = span.enter();

tracing::info!(update_id, chat_id, has_thread = thread_id.is_some(), "update admitted");
tracing::warn!(update_id, reason = %rejection_reason, "update rejected at admission");
tracing::debug!(chat_id, bucket_remaining, "outbound rate limiter state");
tracing::error!(update_id, error = %err, "failed to resolve attachment via getFile");
```

Bot tokens, webhook secrets, and raw message bodies are never included as field values — only IDs, counts, decisions, and reference strings (never resolved secret values).

---

## 7. Future channels — what stays in core vs. adapter

| Concern | Core (`harw-channel`) | Adapter-specific |
|---|---|---|
| `SessionKey` shape (`tenant/channel/peer/thread`) | ✅ fixed | — |
| Pairing-code primitive (issue/redeem/expire) | ✅ shared | Onboarding *copy* and trigger condition per channel |
| `ChannelCapabilities` shape | ✅ fixed struct | Populated values per channel's real limits |
| Markdown downgrade *policy* (declare a tier, degrade predictably) | ✅ shared contract | The actual escaping/flattening rules (MarkdownV2 vs. Slack `mrkdwn` vs. plain email HTML) |
| Attachment hygiene ceilings (byte/count/MIME/expiry) | ✅ shared, enforced once | Download/resolve mechanics (Telegram `getFile`, Slack `files.info`, email MIME multipart walk) |
| Rate limiting *mechanism* (token bucket) | ✅ shared utility | The actual numbers per platform's documented limits |
| Approval rendering *protocol* (`ApprovalRequest`/`Response`) | ✅ fixed | Inline keyboard (Telegram/Slack Block Kit buttons) vs. plain-text reply parsing (email: "reply APPROVE") |
| Command registration | ✅ policy-driven menu contents | BotFather `setMyCommands` vs. Slack app manifest slash commands vs. email has no menu (commands are subject-line conventions instead) |
| Group/DM/thread session-splitting *rules* | Adapter decides, but must express the result purely as `SessionKey` fields | Telegram forum topics vs. Slack channel+thread_ts vs. email `In-Reply-To` chains — each channel's own notion of "sub-conversation" |
| Sandbox profile intersection-only rule | ✅ enforced by policy layer regardless of channel | Which specific restrictions a given channel's default profile declares |

A future Slack binding reuses everything in the left column unchanged; a Matrix binding's room+thread model maps onto the same `ThreadRef`/`PeerId` split used for Telegram groups; an email binding is the interesting edge case — no real-time admission loop, `PeerId` = sender address, `ThreadRef` = `Message-Id`/`References` chain, attachments are MIME parts instead of `file_id` downloads, and there is no inline-keyboard approval path (falls back to structured reply-text parsing) — but none of that requires changing `ChannelAdapter`'s shape, only writing `harw-channel-email`.

---

## Open questions

1. **Pairing-code delivery channel.** Should the pairing code be delivered back over the *same* unauthenticated Telegram DM (as sketched in §3.2), or should the harness require a side-channel (e.g. the code is only ever shown to the operator via CLI, and the operator manually messages it to the peer)? The former is more self-service but means an attacker who can DM the bot learns a valid-looking pairing code exists at all (low sensitivity, but worth an explicit decision).
2. **`shared_session` topic mode and audit granularity.** If topics collapse to one session (§3.3), does the audit log still need to record which physical `thread_id` a message arrived on for support/debugging purposes, even though it doesn't affect `SessionKey`? Leaning yes (store as a non-key attribute on the journaled event), but not yet decided against the session-store journal's actual event schema.
3. **Local Bot API server support.** Deferred out of v1 scope (§3.7) but the config surface (`channel.telegram.local_bot_api_url`) is sketched. Needs a decision on whether harwness treats a locally-run Bot API server as part of the trust boundary (i.e., requires its own sandboxing story) or as a fully-trusted extension of the adapter's own process.
4. **Webhook multiplexing.** One HTTP listener per Telegram binding vs. one shared listener multiplexed by path (§6.2) — affects how `harw-config` validates port conflicts across multiple `[[channel.telegram]]` entries and whether TLS termination is expected to happen in-process or always upstream (reverse proxy). Not yet decided; leaning toward "always upstream reverse proxy, harwness listens plain HTTP on loopback" to keep the adapter out of the TLS-cert-management business.
5. **Cross-channel `TenantId` reuse.** If the same human pairs both a Telegram account and a future Slack account to the same `TenantId`, do their sessions ever need to be linkable (e.g. "continue this conversation on Slack")? Nothing in this design precludes it (both would carry the same `TenantId`), but no explicit cross-channel session handoff mechanism is designed yet — flagged for a later `harw-session-store` design note.
