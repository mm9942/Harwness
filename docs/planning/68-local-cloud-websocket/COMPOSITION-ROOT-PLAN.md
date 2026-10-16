# Composition root and CoreSessionFactory: build plan

Stand: consolidate/main 37d55af. Befund 3 (approval actor) is applied there
(37d55af). Befunde 1 and 2 remain open; this is the plan for them.

## What exists

- `harw-session-daemon::compose::{Daemon, DaemonServices}`: `Daemon::start(host_config, services, uds)`
  opens the `SessionHost` and binds the socket; `run(shutdown)` serves through the Com layer.
  No production caller yet.
- `harw-session-driver::{CoreTurnDriver::with_factory, CoreSessionFactory, CoreSession, SessionWiring}`:
  the driver and the factory trait. No implementation of `CoreSessionFactory`.
- `DurableTranscripts`, `DurableApprovals` (harw-session-host): production ports.
- Assembly references to copy from, not to reinvent:
  `harw-cli/src/gateway/telegram_session.rs:733` (`AgentSession::new_with_id` + spawn context),
  `harw-tui/src/app.rs:11848` (`new_with_id(..).with_spawn_context(..).with_turn_event_sink(..)`),
  `harw-runtime/src/assembly.rs` (registry/sandbox assembly).

## Step 1: `GatewaySessionFactory` (Befund 2)

Where: `harw-cli` (it owns provider config, registry and sandbox), new file
`harw-cli/src/gateway/session_factory.rs`; no change to harw-session-driver.

`build(session_id, title, wiring)`:
1. Registry and sandbox as the telegram session does (tier-derived, no widening).
2. Spawn context with the approval actor of the **local operator** for the
   daemon socket (`ApprovalActor::Operator { id }` from the host identity).
   This is the actor the durable record binds; the driver now resumes the
   core with it regardless of who resolved (37d55af), so remote devices with
   the opt-in right (`allow_approval_when`) can resolve.
3. `AgentSession::new_with_id(id, role, title, registry, wiring.event_tx)
   .with_spawn_context(..).with_turn_event_sink(wiring.turn_tx)`.
4. Provider from the resolved config; store = `TranscriptStateStore` under the sessions root.

Tests (no network): fake `ModelProvider`; build twice for one id is idempotent;
approval actor in the parked request equals the spawn-context actor.

## Step 2: `harw gateway serve` (Befund 1)

Where: `harw-cli` subcommand next to `gateway.rs`.

1. `CoreTurnDriver::with_factory(CoreDriverConfig::new(sessions_root), Arc<GatewaySessionFactory>)`.
2. `DaemonServices::new(driver, DurableTranscripts, DurableApprovals)`.
3. `Daemon::start(HostConfig, services, UdsConfig { path: $XDG_RUNTIME_DIR/harw/session.sock })`
   (same default as `harw attach`, `attach_cmd.rs`).
4. SIGINT/SIGTERM -> `watch` shutdown -> `run` drains.
5. Optional later: self-cloud ingress via `NodeListener` + `ComServer::service_for(RemoteLayer)`;
   remote approval only with `allow_approval_when` (decided: opt-in).

## Step 3: end-to-end proof

One test in harw-cli: start `gateway serve` in-process on a tempdir socket with
the fake provider, `harw attach` equivalent (`harw-session-ws` client) sends a
turn that parks, a **second** client resolves, the turn completes, replay after
reconnect shows the transcript. This is the test that makes the wiring real.

## Ownership and order

Shared files (root Cargo.toml, arch-policy, lib.rs exports) only via scribe
commits. The cli files are `harw-cli/**`, owned by the integration session.
Order: 1 -> 2 -> 3, central build on a frozen SHA.
