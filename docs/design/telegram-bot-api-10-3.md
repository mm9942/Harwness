# Telegram Bot API 10.3: Harwness channel analysis and target contract

Status: researched 2026-10-04. Implementation base:
`dev@4e49a173a8431a062f715310f7ce2bc07546e278`.

## Sources

Primary upstream documentation:
- Telegram Bot API: https://core.telegram.org/bots/api
- Bot FAQ: https://core.telegram.org/bots/faq
- Bot features: https://core.telegram.org/bots/features
- Bot API changelog: https://core.telegram.org/bots/api-changelog

The current Bot API is **10.3 (2026-08-24)**. Of direct relevance to
Harwness, 10.3 adds stop controls for `sendMessageDraft` /
`sendRichMessageDraft`, the `stopped_message_generation` update, additional
Rich Message blocks/buttons and more ephemeral-message capabilities. Private
chat topics and `sendMessageDraft` landed earlier and are now mature enough to
treat as a first-class channel capability rather than an experiment.

## CURRENT state in Harwness

Harwness already has a strong split:

- `harw-channel-telegram`: admission, pairing, group/topic policy, durable chat
  state, work requests and approval tokens.
- `harw-channel-telegram-transport`: token-owning Bot API client, long polling,
  webhook ingress, update mapping, callback queries, bounded downloads,
  retry/backoff, renderer and rate limiting.
- `harw-cli::gateway`: composes Telegram with the governed runtime,
  attachments, work-request lifecycle, callbacks and per-chat model sessions.

Both long polling and webhooks are supported. The webhook path uses Telegram's
secret token. The Bot API client already obeys `retry_after` on HTTP/API 429
and bounded 5xx retries. Outbound rendering is throttled per chat.

The cloud Bot API currently documents a **20 MB download limit through
`getFile`**. Harwness additionally applies its own configured byte ceiling.
Telegram's local Bot API server removes the download ceiling and supports much
larger uploads, but Harwness currently hardcodes
`https://api.telegram.org`; a trusted configurable API base remains a
follow-up.

## The command ownership bug

Telegram currently owns a second hand-written `HANDLED_COMMANDS` list in
`harw-cli/src/gateway/telegram_commands.rs`. The actual slash-command contract,
however, already lives in `#[operation(command(...))]` and
`OperationMeta::surfaces`.

That duplication is the main architectural defect: adding a command can make it
available in the TUI/runtime while Telegram never learns about it. It also makes
the documentation's "single operation registry" claim false.

The target rule is:

> `#[operation]` + the runtime `OperationRegistry` are the source of truth.
> Telegram projects that metadata into its own menu and dispatch surface.

Telegram-specific lifecycle commands (`/workspace`, `/task`,
`/request`, work-request `/approve` etc.) remain native because they operate
on channel state rather than the generic operation contract. Native commands
win name collisions.

## Telegram command projection constraints

Telegram constrains `BotCommand.command` to 1-32 lowercase ASCII
letters/digits/underscores and descriptions to 1-256 characters.
`setMyCommands` accepts at most 100 commands per scope.

Therefore arbitrary internal command paths cannot simply be copied into the
menu. The Telegram projection must:

1. enumerate the runtime's registered `Surface::Command` entries;
2. exclude `TuiOnly`;
3. normalize unsupported path characters to `_`;
4. reserve native Telegram command names;
5. use a deterministic `harw_` prefix on collisions;
6. keep `/op <canonical-path> [args...]` as the lossless escape hatch;
7. cap the *visible menu* at 100 while keeping the registry fully dispatchable.

Menu publication is UX only; it is not authorization.

## Remote authority and `ChannelReduced`

"All slash tools available from Telegram" must not mean "Telegram can execute
all local-only forms".

Three independent gates remain mandatory:

1. Telegram admission (pinned/allowed sender, pairing and group policy).
2. Command surface metadata:
   - `TuiOnly`: never remote.
   - `ChannelParity`: complete command grammar remotely.
   - `ChannelReduced`: explicitly enumerated remote forms.
3. `OperationMeta.permission` plus the runtime sandbox/capability set.

This change makes `ChannelReduced` executable policy rather than prose:
`#[operation(command(..., channel_subcommands = "..."))]` generates a
fail-closed allowlist. `-` means the bare form; `*` means every argument
form.

Current reduced contracts:

| Command | Telegram forms |
| --- | --- |
| `/workbench` | `note`, `hypothesis` |
| `/diary` | bare, `show`, `today`, `search`, `note`, `agents` |
| `/ps` | all forms (read-only filters) |
| `/matrix` | bare, `show`, `list`, `replay`, `compare` |
| `/dream` | bare, `list`, `show`, `status`, `review`; **not `run`** |
| `/skills` | bare, `list`, `show` |
| `/learn` | bare, `scan`, `note`, `list`, `show`, `accept`, `reject` |

This keeps the existing workbench contract (`pin`/bare panel local), prevents
remote dream execution, and keeps skill activation/configuration out of the
reduced channel surface.

For direct human slash calls, admission may map a normal admitted sender to an
Operator principal and configured `security.admin_identities` to Maintainer.
Telegram must never manufacture an Owner principal. Model turns retain their
existing Observer channel principal; enabling direct human commands must not
silently widen model authority.

## Streaming

`sendMessageDraft` streams a private-chat preview for roughly 30 seconds. A
non-zero `draft_id` identifies the stream; the final answer must still be sent
with `sendMessage`. Bot API 10.3 adds `can_stop` and `keep_on_stop`; a
user stop arrives as `Update.stopped_message_generation`.

The transport now gets a typed `send_message_draft` primitive, but the renderer
should not enable Stop until the update is wired to the exact governed
turn/cancel token. Otherwise Telegram would show a Stop button that only stops
the visual draft, not the work behind it.

Recommended activation path:

`draft_id -> SessionKey + TurnId + CancelToken`

Then add `stopped_message_generation` to allowed updates, resolve the same
per-chat lane, cancel the matching turn and persist the final partial text with
`sendMessage`.

Groups keep the existing edit-message streaming fallback because
`sendMessageDraft` targets private chats.

## Rich Messages

Rich Messages are useful for Harwness: structured tables, expandable blocks,
references, buttons and file blocks map naturally to tool output, plans and
approvals. They should be a Telegram lowering layer, not a new core response
format:

`OutboundContent -> Telegram rich lowering -> plain text fallback`

This preserves channel neutrality in core and keeps older/unsupported clients
functional. Approval/security decisions remain server-side; rich buttons carry
opaque callback tokens, never raw authority.

## Rate limits and reliability

Telegram's public FAQ recommends approximately:
- <= 1 message/s in a single chat;
- <= 20 messages/minute in a group;
- about 30 messages/s for unpaid bulk broadcasting.

Harwness already has per-chat renderer throttling and honors API 429
`retry_after`. Keep those limits configurable and favor coalesced stream
updates over more messages.

Long polling must persist/update offsets only after accepted processing.
Webhook ingestion must remain idempotent. Telegram keeps pending updates for no
longer than roughly 24 hours, so Harwness must not use Telegram as a durable
queue.

## Files and local Bot API

Cloud `getFile` is intentionally treated as bounded input. Never use Telegram
file names as trusted paths; keep the existing Harwness cache/sandbox path.

A future local Bot API server setting belongs in trusted static config. It must
not be supplied by a chat, model output or command argument. Operationally, a
bot must be logged out from Telegram's cloud Bot API endpoint before moving it
to a local server.

## Follow-up backlog

1. Complete macro-derived Telegram command catalog and direct operation dispatch.
2. Wire draft Stop to governed turn cancellation.
3. Add typed RichMessage/InputRichMessage plus text fallback.
4. Add trusted configurable Bot API base/local-server mode.
5. Reconcile `getMyCommands` at startup and expose drift telemetry.
6. Add ephemeral messages only with an explicit retention/privacy policy.
7. Treat Guest Mode and bot-to-bot communication as new identity/admission
   classes, never as implicit extensions of pinned-user trust.

## Verification

No test may require live Telegram credentials.

Required:
- `cargo test -p harw-macros`
- `cargo test -p harw-operations`
- `cargo test -p harw-ops`
- `cargo test -p harw-channel-telegram-transport`
- `cargo test -p harw-cli telegram`
- workspace `cargo check`
- clippy against the repository's existing lint baseline
