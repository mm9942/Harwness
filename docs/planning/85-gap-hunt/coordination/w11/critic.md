**Verdict:** DEC-048 has the right shape: a WS carrier inside harw-web, one registry path, an F-ring contract and the Warden gated. It is not implementable as written, though. 11 contract entries fail against the current code, 5 of them in a way that breaks the build or the design. Nothing was edited and no cargo or rustc was run.

## Blocking corrections

1. **The approval actor and tenant are missing for hosted root sessions.**
   - `AskUser` needs `spawn_context().approval_actor`, otherwise it fails with `MissingApprovalActor` (`harw-core/src/turn_loop.rs:4789-4794`).
   - The record tenant comes from `spawn_context().sandbox.workspace().tenant()` (`:4811-4813`). Root sessions have no spawn context (`session.rs:596`).
   - Every web approver resolves to `Operator{id:"owner"}` (`harw-types/src/principal.rs:204`, `harw-web/src/security.rs:393-411`, `harw-cli/src/web.rs:337-341`).
   - `ensure_request_visible` hides tenant-`None` records from tenant callers (`harw-ops/src/approval.rs:221-241`). So a tenant-scoped client can never approve its own hosted session.
   - Fix: add `tenant` and `approval_actor: ApprovalActor` to `TurnSubmission`, and have `CoreTurnDriver` build a `SpawnContext`. The workspace tenant is not the caller tenant, so how the hosted tenant reaches `ApprovalRecord` is an operator decision.
2. **The `#[operation]` bounds block the planned arg types.** Args must be `Default + DeserializeOwned + harw_operations::FromRawArgs` (`harw-macros/src/operation.rs:738-755`).
   - harw-ops cannot implement `FromRawArgs` for harw-protocol types (orphan rule), and F cannot depend on I.
   - Fix: harw-ops-local `*Args` structs with `deny_unknown_fields`, whose `Default` sentinel fails `validate`, converted into the protocol params.
3. **`PartialEq` cannot be derived.** `TurnEvent`/`SessionEvent` (`harw-protocol/src/events.rs:10,50`) and `ApprovalRequest` (`approvals.rs:124`) lack it, so `SessionFrame` and `FrameEnvelope` cannot derive it. Drop the derive there and compare `serde_json::Value` in tests.
4. **`deny_unknown_fields` everywhere contradicts "additive".**
   - Restrict it to client→host params. Outbound types (`HelloAck`, `AttachAck`, `SessionSummary`, `FrameEnvelope`, payloads) must accept unknown fields.
   - `#[serde(other)]` together with `deny_unknown_fields` rejects an unknown `WaitReason` kind that carries fields.
   - Add `#[serde(other)]` to `ResyncReason` and `SubmitResult`.
   - A nested `TurnEvent` has no Unknown variant (`events.rs:50-52`), so a new variant breaks the whole frame. Document this in the DEC.
   - The unknown-kind test must send a non-null `data`.
5. **The durable cursor cannot be stamped on live frames.**
   - `save_turn` returns `()` (`harw-core/src/state_store.rs:543-548`). The sequence is assigned privately (`:713-777`). `AgentEvent` carries no sequence (`agent_events.rs:66-75`).
   - Fix: the writer task publishes durable frames only from `read_from(last_published)` after each LiveSink event or driver return. Live deltas carry the last published cursor.
   - Define `durable` as "next sequence to deliver" (PL-65 `README.md:155-156`).
   - Run attach (Fanout registration plus head snapshot) inside the writer task, so there is no gap or duplicate at the replay→live seam.
6. **Upgraded connections escape the cap and shutdown.**
   - `upgrade::on` has to be spawned, and `serve_connection` finishes at the 101. So the accept-branch permit (`server.rs:488-514`) is released and `abort_all` (`:518-519`) never reaches WebSocket tasks.
   - Fix: move both permits into the WebSocket task and track those tasks in `WsShared`, so `serve_until` can drain them.
7. **Composition order in harw-cli.**
   - The factory is built at `harw-cli/src/web.rs:355-359`, but the runtime only at `:361-364`. `LocalHost::open` needs a runtime, so open the host inside `block_on`.
   - `current_thread` puts the turn loop, WebSocket traffic and synchronous fsync (`store.rs:164-185`) on one thread. harw-cli tokio lacks `rt-multi-thread` (`harw-cli/Cargo.toml:98`). Either add it or use `spawn_blocking`; this is a decision.
8. **`ScriptedDriver` is invisible to the integration tests.** As `#[cfg(test)]` in `src/test_support.rs`, `tests/common/mod.rs` cannot see it. Move it into `tests/common/`.
9. **Host lease and restart.**
   - `open` must take a host-level lease (`<root>/host.lock`). Otherwise `test_second_host_same_root_is_writer_busy` means nothing.
   - Add `async fn shutdown(&self)` that joins the writer tasks and releases the leases. Otherwise the in-process restart test fails intermittently with `WriterBusy`.
10. **The error codes cannot pass through `OpError`.** It has only three String variants (`harw-operations/src/error.rs:33-45`).
    - `LockContended` mapped to `Execution("busy")` becomes -32002, not -32003.
    - Revoked and NotFound have no path through `OpError`.
    - Fix: a fixed PortError→OpError table with stable reason tokens that ws.rs maps to codes. Foreign and unknown sessions get the same token.
11. **An unsupported major loses the id.** `ProtocolVersion` rejects the whole `RequestEnvelope` (`wire.rs:19-40,69`).
    - Fix: parse to `Value`, extract `id` and `protocol.major`, then answer -32005.
    - A client-sent `Response` or `Notification` (untagged `WireMessage`, `wire.rs:143-147`) gets -32600.

## Non-blocking corrections

- **Principal id.** `CallerScope` uses `Principal::id` = `uid:<n>` (`harw-cli/src/runtime_web.rs:92-99`). The hub principal (`identity.rs:160-165`) never reaches `OpContext`, because `scope_op_context` (`:194-203`) sets only tenant and summary. Document this or insert it.
- **Identity re-check.** It must compare tier, tenant and principal, not only look for errors; any change closes with 1008.
- **Idle timeout.** Apply it only when there are no attachments and nothing in flight. Pongs count as activity.
- **Memory.** 64 × 8 × 4 MiB is 2 GiB. The replay `Vec` can hold 1024 × 1 MiB (`reader.rs:22`). Add a global outbound byte budget and a replay byte cap, and clamp `tail_items`.
- **Submit size.** 48 KiB of text can escape to more than 64 KiB of JSON, which gives a 1009 close instead of -32602. Raise the WebSocket inbound limit to 128 KiB or bound the encoded size. The same applies to `MAX_BODY_BYTES` (`server.rs:194`).
- **Origin rejection** applies only to `/v1/ws`. The webui runs in the browser behind a proxy (`webui/next.config.mjs:10-14`) and browsers always send Origin, so `/events` cannot be retired without item 7's allowlist. The DEC table must say so.
- **Hosted sessions only.** Attach and list require a `HostedSessionRecord`, because `tenant_admits(None, _)` is true (`context.rs:415-419`). Validate the `SessionId` through `transcript_path` (`store.rs:136`) before any file lookup.
- **`event.frame`.** Set `correlation_id = attachment_id` (`wire.rs:122-131`). Start the pump only after the attach response is written. The pump→writer channel must be bounded.
- **Lookup by operation name.** `from_operation` can return several adapters (`harw-operations/src/adapter/web.rs:199-224`). Require exactly one entry, otherwise NotFound.
- **Pending approvals.** Use `pending_all` (`harw-session-store/src/approval.rs:404`); `pending` (`:307`) returns a single record. Sweep periodically for approvals resolved outside `approval.resolve`.
- **Legacy writers.** The writer lease does not stop them; they only take the per-append lock (`store.rs:351-361`). `head()` must read the generation before and after the scan and retry if it changed.
- **Generation resync.** `TranscriptStore::rewrite` has no production caller, so `Resync{Generation}` can only be reached in tests. Say so in the DEC.
- **SSE flood.** `OperationCompleted` is published for every WebSocket request (`server.rs:734-738`), which floods the 64-slot SSE bus.
- **Restart behaviour.** A session with a durable user message and no completion becomes `Interrupted` with no automatic rerun. In-memory queued submits are lost. Document both.
- **DEC format.** Use YAML frontmatter and the sections Entscheidung / Warum / Folgen / Wo im Code / Verwandt (`DEC-008-no-tui.md:1-63`), not a fenced id block.
- **"Workbench".** In this repo it is the knowledge surface (`harw-knowledge/src/workbench.rs:1-13`). W11 delivers turn-level wait reasons only, not agent waits.

## Missing tests

- `test_attach_during_append_no_gap_no_dup`
- `test_live_frame_cursor_matches_durable_sequence`
- `test_driver_lag_resyncs_all_attachments`
- `test_oversize_outbound_frame_becomes_resync`
- `test_shutdown_drains_ws_with_1001`
- `test_upgraded_connections_count_against_cap`
- `test_identity_recheck_tier_change_closes`
- `test_idle_timeout_spares_attached_observer`
- `test_hello_negotiates_min_minor`
- `test_client_response_frame_is_invalid_request`
- `test_hello_ack_ignores_unknown_fields`
- `test_replay_bounded_by_bytes`
- `ws_tenant_scoped_owner_resolves_own_hosted_approval`
- `ws_attach_non_hosted_transcript_is_not_found`
- `ws_attach_unsafe_session_id_is_not_found`
- `ws_http_submit_share_dedupe`
- `ws_child_agent_frame_scoped_to_root_session`
- `test_open_marks_midturn_session_interrupted`
- One harw-cli end-to-end test through the real `web_op_context` and `CoreTurnDriver` with the Echo model. The harness in harw-session-host duplicates the private composition.

## Anchor drift

- HEAD is `72fb2ab` (R16 wa-web merged), not `0fc69ed`. `harw-web/src/*` and `harw-protocol/src/*` are clean now. Dirty and relevant: `harw-ops/src/lib.rs`, `harw-operations/src/lib.rs`, `xtask/src/gate_edges.rs`, `harw-cli/src/lib.rs`.
- `RevokingResolver` is at `harw-web/tests/identity_routes.rs:128-160`.
- The `deny.toml` `[bans].deny` list is at `:74-78`.

These cited facts check out: the arch-policy lines (`:45,52,97,345,383-390`), tungstenite 0.30 `rust-version = "1.85"`, the `assert_valid` panic, node-transport serving without upgrades, the existing DEC references ending at DEC-047, and the `CRYPTO_AND_HTTP_STACK` test being additive (`gate_edges.rs:866-885`).

## New operator decisions

1. How the hosted tenant reaches `ApprovalRecord` (see blocking item 1).
2. The runtime flavour for `harw web`.
3. The Origin allowlist, now blocking for the webui migration.
4. A global memory budget.
5. The principal source: uid or hub.

## Placement

Unchanged:
- `session_wire.rs` and `session_port.rs` in harw-protocol (F).
- Store, lease and hosted records in harw-session-store (I).
- harw-session-host as A, with no listener.
- `ws.rs` inside harw-web (A).
- The arg wrappers and error table in `harw-ops/src/session.rs` (A).

## Tests added and central build

No tests were added. Once W11 lands, the central build runs, with `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0` in the cloud container:
1. `cargo fmt --all`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace` (including doc tests)
4. `cargo run -q -p xtask -- gates`
5. `cargo deny check`
6. `make -C dod clippy test` (`--locked`, so commit the `Cargo.lock` first)
7. `actionlint`