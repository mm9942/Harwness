//! `gateway.*` — inspection and administration of the gateway (the
//! persistent session host) as operations (R18 contract
//! `docs/planning/65-cloud-sessions/contracts/R18-tool-gateway.md` §6, D-B,
//! field evidence F1).
//!
//! # Operations
//! | Name | Model tool | Approval | Tier | Command | Web |
//! |---|---|---|---|---|---|
//! | `gateway.status` | readonly | none | maintainer | `/gateway-status` | `GET /api/gateway/status` |
//! | `gateway.connections.list` | readonly | none | maintainer | `/gateway-connections` | `GET /api/gateway/connections` |
//! | `gateway.sessions.list` | readonly | none | maintainer | — | `GET /api/gateway/sessions` |
//! | `gateway.listeners.list` | readonly | none | maintainer | `/gateway-listeners` | `GET /api/gateway/listeners` |
//! | `gateway.tools.list` | readonly | none | maintainer | `/gateway-tools` | `GET /api/gateway/tools` |
//! | `gateway.connections.revoke` | mutation | **always** | owner | `/gateway-revoke` | `POST`, always |
//! | `gateway.drain` | mutation | **always** | owner | `/gateway-drain` | `POST`, always |
//! | `gateway.listeners.set` | mutation | **always** | owner | — | `POST`, always |
//! | `gateway.tools.grant` | mutation | **always** | owner | — | `POST`, always |
//! | `gateway.tools.narrow` | mutation | **always** | owner | — | `POST`, always |
//!
//! Local diagnostics (coordinator addendum to F1, see [`health`] and
//! [`logs`]; the channel reads moved here in the R18a integration because F1
//! needs them when no gateway session is connected):
//!
//! | Name | Model tool | Approval | Tier | Command | Web |
//! |---|---|---|---|---|---|
//! | `gateway.channels.list` | readonly | none | maintainer | `/gateway-channels` | `GET /api/gateway/channels` |
//! | `gateway.channels.connect_info` | readonly | none | maintainer | — | `GET /api/gateway/channels/connect-info` |
//! | `gateway.health` | readonly | none | maintainer | `/gateway-health` | `GET /api/gateway/health` |
//! | `gateway.logs` | readonly | none | maintainer | `/gateway-logs` | `GET /api/gateway/logs` |
//!
//! Tiers follow the cap ceilings of the contract (§2.3): `gateway_read` is
//! granted from Maintainer up, `gateway_admin` only to Owner.
//!
//! # Model surface
//! Read operations are free model tools; mutations are model tools with
//! `approval = "always"` on the model **and** the web surface, so every call
//! asks a human. Approval never expands authority: the gateway still checks
//! the caller's `gateway_read`/`gateway_admin` caps and tenant scope. Key
//! operations (`infra.auth.keys.*`) are not part of this module and keep no
//! model surface (see the rule in `crate::infra`).
//!
//! Every tool description tells the model to use the tool instead of running
//! `harw gateway …`/`harw channel …` through `shell.exec` ([`SHELL_HINT`]):
//! the `harw` binary is not available inside the sandbox, and on the host the
//! shell escape costs an approval round trip (F1).
//!
//! # Service, never a socket
//! The ten port-backed operations read `Arc<dyn harw_protocol::GatewayPort>`
//! from the [`OpContext`] (like `crate::infra` reads
//! `InfrastructureAvailability`); none of them opens a socket itself.
//! Without that service every one of them answers [`OpError::NotAvailable`]
//! ("gateway not configured"), fail closed.
//!
//! The four diagnostics operations do **not** need the port:
//! `gateway.health`/`gateway.logs` inspect the local harw home (daemon
//! process, sockets, log files) through `Arc<harw_home::ResolvedHomeContext>`
//! and use the port only to add the session host's own status when it is
//! present; `gateway.channels.*` read the channel configuration
//! (`Arc<ResolvedConfig>`) and answer [`OpError::NotAvailable`] only without
//! it.
//!
//! # Registration
//! Not part of [`crate::register_all`]. The runtime registers
//! [`register_gateway`] only when it is connected to a gateway (its
//! `GatewayContributor` holds the port) and [`register_gateway_diagnostics`]
//! for UIA roots; both only for the User Interface Agent.
//!
//! # Secrets
//! Output never contains tokens, keys or credentials. Every text that comes
//! from the gateway, the configuration or a log file goes through
//! [`sanitize`]: control characters are stripped, token-shaped substrings are
//! replaced by `[redacted]` (`harw_memory::redact`) and the text is bounded.
//! Channel bot-token references are never shown.

use std::sync::Arc;
use std::time::Duration;

use harw_config::{ChannelToml, ResolvedConfig};
use harw_macros::operation;
use harw_operations::registry::{OperationRegistry, RegistryError};
use harw_operations::{OpContext, OpError, OpOutput, Operation};
use harw_protocol::session_wire::{
    GatewayConnectionInfo, GatewayDrainParams, GatewayListenerInfo, GatewayListenerSetParams,
    GatewayRevokeParams, GatewayStatus, GatewayToolRights, GatewayToolRightsParams,
    PrincipalSummary, SessionSummary,
};
use harw_protocol::{
    ClientCaps, GatewayPort, PortError, PortFuture, ToolDescriptor, ToolPlacement,
};
use serde::Deserialize;
use serde_json::{Value, json};

pub mod health;
pub mod logs;

pub use health::GatewayHealthOperation;
pub use logs::GatewayLogsOperation;

/// Number of operations [`register_gateway`] adds.
pub const GATEWAY_OP_COUNT: usize = 10;

/// Number of operations [`register_gateway_diagnostics`] adds.
pub const GATEWAY_DIAGNOSTICS_OP_COUNT: usize = 4;

/// The read-only port-backed operations (free model tools).
pub const GATEWAY_READ_OPS: [&str; 5] = [
    "gateway.status",
    "gateway.connections.list",
    "gateway.sessions.list",
    "gateway.listeners.list",
    "gateway.tools.list",
];

/// The mutating operations (model tools with `approval = "always"`).
pub const GATEWAY_MUTATION_OPS: [&str; 5] = [
    "gateway.connections.revoke",
    "gateway.drain",
    "gateway.listeners.set",
    "gateway.tools.grant",
    "gateway.tools.narrow",
];

/// The local diagnostics operations (free model tools, no port needed).
pub const GATEWAY_DIAGNOSTICS_OPS: [&str; GATEWAY_DIAGNOSTICS_OP_COUNT] = [
    "gateway.health",
    "gateway.logs",
    "gateway.channels.list",
    "gateway.channels.connect_info",
];

/// The diagnostics operations that inspect the local harw home (and carry
/// [`SHELL_DIAGNOSTICS_HINT`]); a subset of [`GATEWAY_DIAGNOSTICS_OPS`].
pub const GATEWAY_HOME_DIAGNOSTICS_OPS: [&str; 2] = ["gateway.health", "gateway.logs"];

/// Sentence every `gateway.*` tool description carries (contract §6, F1).
pub const SHELL_HINT: &str = "Use this instead of running `harw gateway …`/`harw channel …` through `shell.exec`; the `harw` binary is not available inside the sandbox.";

/// Additional sentence of the local diagnostics tools (coordinator addendum).
pub const SHELL_DIAGNOSTICS_HINT: &str = "Use this instead of ps/ls/tail through the shell.";

/// Answer of every port-backed operation without a gateway port.
pub const NOT_CONFIGURED: &str =
    "gateway not configured (this runtime is not connected to a gateway)";

/// Longest gateway-reported text shown verbatim (labels, names, addresses).
const MAX_REPORTED_TEXT: usize = 128;

/// Longest tool description shown by `gateway.tools.list`.
const MAX_DESCRIPTION_TEXT: usize = 240;

/// Longest free-text reason accepted for `gateway.connections.revoke`.
const MAX_REASON_CHARS: usize = 256;

/// Longest tool name accepted by `gateway.tools.grant`/`narrow`.
const MAX_TOOL_NAME_CHARS: usize = 128;

/// Upper bound for one gateway call. A read that does not answer is reported
/// as unavailable; a mutation that does not answer has an unknown outcome.
const GATEWAY_CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// `retry_after_ms` sent with `gateway.drain` when the caller names none.
pub const DEFAULT_DRAIN_RETRY_AFTER_MS: u64 = 30_000;

/// Registers the ten port-backed `gateway.*` operations.
///
/// # Description
/// Called by the runtime's `GatewayContributor` only when the runtime is
/// connected to a gateway (the contributor holds the port) and the root is
/// the User Interface Agent. Uses `try_register`, so a second call on the
/// same registry reports the collision instead of panicking.
///
/// # Errors
/// The first [`RegistryError`] (name or web-route collision). Operations
/// registered before the collision stay registered.
///
/// # Example
/// ```rust
/// use harw_operations::registry::OperationRegistry;
///
/// # fn main() -> Result<(), harw_operations::registry::RegistryError> {
/// let mut registry = OperationRegistry::new();
/// let added = harw_ops::gateway_ops::register_gateway(&mut registry)?;
/// assert_eq!(added, harw_ops::gateway_ops::GATEWAY_OP_COUNT);
/// assert!(registry.find_by_name("gateway.status").is_some());
/// assert!(registry.find_by_name("gateway.drain").is_some());
/// # Ok(())
/// # }
/// ```
pub fn register_gateway(registry: &mut OperationRegistry) -> Result<usize, RegistryError> {
    let ops: [Arc<dyn Operation>; GATEWAY_OP_COUNT] = [
        Arc::new(GatewayStatusOperation),
        Arc::new(GatewayConnectionsListOperation),
        Arc::new(GatewaySessionsListOperation),
        Arc::new(GatewayListenersListOperation),
        Arc::new(GatewayToolsListOperation),
        Arc::new(GatewayConnectionsRevokeOperation),
        Arc::new(GatewayDrainOperation),
        Arc::new(GatewayListenersSetOperation),
        Arc::new(GatewayToolsGrantOperation),
        Arc::new(GatewayToolsNarrowOperation),
    ];
    for op in ops {
        registry.try_register(op)?;
    }
    Ok(GATEWAY_OP_COUNT)
}

/// Registers the four local diagnostics operations (`gateway.health`,
/// `gateway.logs`, `gateway.channels.list`, `gateway.channels.connect_info`).
///
/// # Description
/// They inspect the local harw home and its configuration and do not need a
/// gateway port, so the runtime registers them for every UIA root
/// (`GatewayDiagnosticsContributor`) — also when no gateway session is
/// connected (F1: "how do I connect a channel" is asked exactly then).
///
/// # Errors
/// The first [`RegistryError`] (name or web-route collision).
///
/// # Example
/// ```rust
/// use harw_operations::registry::OperationRegistry;
///
/// # fn main() -> Result<(), harw_operations::registry::RegistryError> {
/// let mut registry = OperationRegistry::new();
/// let added = harw_ops::gateway_ops::register_gateway_diagnostics(&mut registry)?;
/// assert_eq!(added, harw_ops::gateway_ops::GATEWAY_DIAGNOSTICS_OP_COUNT);
/// assert!(registry.find_by_name("gateway.logs").is_some());
/// assert!(registry.find_by_name("gateway.channels.list").is_some());
/// # Ok(())
/// # }
/// ```
pub fn register_gateway_diagnostics(
    registry: &mut OperationRegistry,
) -> Result<usize, RegistryError> {
    let ops: [Arc<dyn Operation>; GATEWAY_DIAGNOSTICS_OP_COUNT] = [
        Arc::new(GatewayHealthOperation),
        Arc::new(GatewayLogsOperation),
        Arc::new(GatewayChannelsListOperation),
        Arc::new(GatewayChannelsConnectInfoOperation),
    ];
    for op in ops {
        registry.try_register(op)?;
    }
    Ok(GATEWAY_DIAGNOSTICS_OP_COUNT)
}

// ── Arguments ────────────────────────────────────────────────────────────────

/// Arguments of the list/status reads (none).
#[derive(Debug, Default, Deserialize, harw_macros::FromRawArgs, harw_macros::OpArgs)]
#[serde(deny_unknown_fields)]
pub struct GatewayReadArgs {}

/// Arguments of `gateway.channels.connect_info`.
#[derive(Debug, Default, Deserialize, harw_macros::OpArgs)]
#[serde(deny_unknown_fields)]
pub struct GatewayConnectInfoArgs {
    /// Channel kind (`telegram`). Default: every supported kind.
    #[serde(default)]
    pub channel: Option<String>,
}

impl harw_operations::FromRawArgs for GatewayConnectInfoArgs {
    /// No command surface (contract §6).
    fn from_raw_args(_tokens: &[String]) -> Result<Self, OpError> {
        Err(no_command_surface("gateway.channels.connect_info"))
    }
}

/// Arguments of `gateway.connections.revoke`: `<connection> <reason…>`.
#[derive(Debug, Default, Deserialize, harw_macros::OpArgs)]
#[serde(deny_unknown_fields)]
pub struct GatewayRevokeArgs {
    /// Connection id as shown by `gateway.connections.list`.
    #[serde(default)]
    pub connection: Option<u64>,
    /// Why the connection is revoked (audit; required).
    #[serde(default)]
    pub reason: Option<String>,
}

impl harw_operations::FromRawArgs for GatewayRevokeArgs {
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        let connection = tokens
            .first()
            .map(|token| parse_u64(token, "connection"))
            .transpose()?;
        let reason = tokens
            .get(1..)
            .filter(|rest| !rest.is_empty())
            .map(|rest| rest.join(" "));
        Ok(Self { connection, reason })
    }
}

/// Arguments of `gateway.drain`: `[retry_after_ms]`.
#[derive(Debug, Default, Deserialize, harw_macros::OpArgs)]
#[serde(deny_unknown_fields)]
pub struct GatewayDrainArgs {
    /// Retry hint sent to clients in milliseconds (default 30000).
    #[serde(default)]
    pub retry_after_ms: Option<u64>,
}

impl harw_operations::FromRawArgs for GatewayDrainArgs {
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        let retry_after_ms = tokens
            .first()
            .map(|token| parse_u64(token, "retry_after_ms"))
            .transpose()?;
        Ok(Self { retry_after_ms })
    }
}

/// Arguments of `gateway.listeners.set`.
#[derive(Debug, Default, Deserialize, harw_macros::OpArgs)]
#[serde(deny_unknown_fields)]
pub struct GatewayListenerSetArgs {
    /// Listener name as shown by `gateway.listeners.list`.
    #[serde(default)]
    pub name: Option<String>,
    /// `true` enables, `false` disables the listener.
    #[serde(default)]
    pub enabled: Option<bool>,
}

impl harw_operations::FromRawArgs for GatewayListenerSetArgs {
    /// No command surface (contract §6).
    fn from_raw_args(_tokens: &[String]) -> Result<Self, OpError> {
        Err(no_command_surface("gateway.listeners.set"))
    }
}

/// Arguments of `gateway.tools.grant` / `gateway.tools.narrow`.
#[derive(Debug, Default, Deserialize, harw_macros::OpArgs)]
#[serde(deny_unknown_fields)]
pub struct GatewayToolRightsArgs {
    /// Agent principal id as shown by `gateway.tools.list`.
    #[serde(default)]
    pub agent: Option<String>,
    /// Exact tool names (grant: the new grant; narrow: names to remove).
    #[serde(default)]
    pub tools: Option<Vec<String>>,
}

impl harw_operations::FromRawArgs for GatewayToolRightsArgs {
    /// No command surface (contract §6).
    fn from_raw_args(_tokens: &[String]) -> Result<Self, OpError> {
        Err(no_command_surface("gateway.tools.grant/narrow"))
    }
}

fn no_command_surface(name: &str) -> OpError {
    OpError::NotAvailable(format!(
        "`{name}` has no command surface; use the model tool or the web surface"
    ))
}

fn parse_u64(token: &str, field: &str) -> Result<u64, OpError> {
    token
        .trim()
        .parse()
        .map_err(|_| OpError::InvalidArguments(format!("{field} must be a non-negative integer")))
}

// ── Read operations ──────────────────────────────────────────────────────────

/// Status of the gateway: host epoch, draining, connection/session/turn
/// counts, listeners, served tools and whether a gateway sandbox is
/// available.
///
/// # Errors
/// [`OpError::NotAvailable`] without a gateway port or when the gateway does
/// not answer; [`OpError::Execution`] for other gateway errors.
#[operation(
    name = "gateway.status",
    summary = "Zeigt den Zustand des Gateways (Sitzungs-Host): Epoche, Draining, Verbindungen, Sitzungen, laufende Turns, Listener, Werkzeuge, Sandbox. Use this instead of running `harw gateway …`/`harw channel …` through `shell.exec`; the `harw` binary is not available inside the sandbox.",
    domain = "execution",
    permission = "maintainer",
    command(path = "/gateway-status", visibility = "tui_only", busy = "immediate"),
    model_tool(readonly, approval = "none"),
    // Reine Abfrage (`GatewayPort::status`), keine Mutation.
    web(path = "/api/gateway/status", method = "get", approval = "none")
)]
async fn gateway_status(ctx: &OpContext, _args: GatewayReadArgs) -> Result<OpOutput, OpError> {
    let port = gateway_port(ctx)?;
    let status = call("gateway.status", false, port.status()).await?;
    let (text, data) = render_status("Gateway status:", &status);
    Ok(OpOutput {
        text,
        data: Some(data),
    })
}

/// Live connections of the gateway (tenant-filtered by the gateway).
///
/// # Errors
/// As [`gateway_status`].
#[operation(
    name = "gateway.connections.list",
    summary = "Listet die Verbindungen des Gateways (Prinzipal, Mandant, Caps, angehängte Sitzungen). Use this instead of running `harw gateway …`/`harw channel …` through `shell.exec`; the `harw` binary is not available inside the sandbox.",
    domain = "execution",
    permission = "maintainer",
    command(path = "/gateway-connections", visibility = "tui_only", busy = "immediate"),
    model_tool(readonly, approval = "none"),
    // Reine Abfrage (`GatewayPort::connections`), keine Mutation.
    web(path = "/api/gateway/connections", method = "get", approval = "none")
)]
async fn gateway_connections_list(
    ctx: &OpContext,
    _args: GatewayReadArgs,
) -> Result<OpOutput, OpError> {
    let port = gateway_port(ctx)?;
    let result = call("gateway.connections.list", false, port.connections()).await?;
    let mut lines = Vec::with_capacity(result.connections.len() + 1);
    let mut entries = Vec::with_capacity(result.connections.len());
    lines.push(format!("Gateway connections: {}", result.connections.len()));
    for connection in &result.connections {
        let (line, data) = render_connection(connection);
        lines.push(line);
        entries.push(data);
    }
    Ok(OpOutput {
        text: lines.join("\n"),
        data: Some(json!({ "connections": entries })),
    })
}

/// Sessions hosted by the gateway (tenant-filtered by the gateway).
///
/// # Errors
/// As [`gateway_status`].
#[operation(
    name = "gateway.sessions.list",
    summary = "Listet die vom Gateway gehosteten Sitzungen (Zustand, Titel, Mandant, angehängte Clients, Modell). Use this instead of running `harw gateway …`/`harw channel …` through `shell.exec`; the `harw` binary is not available inside the sandbox.",
    domain = "execution",
    permission = "maintainer",
    model_tool(readonly, approval = "none"),
    // Reine Abfrage (`GatewayPort::sessions`), keine Mutation.
    web(path = "/api/gateway/sessions", method = "get", approval = "none")
)]
async fn gateway_sessions_list(
    ctx: &OpContext,
    _args: GatewayReadArgs,
) -> Result<OpOutput, OpError> {
    let port = gateway_port(ctx)?;
    let sessions = call("gateway.sessions.list", false, port.sessions()).await?;
    let mut lines = Vec::with_capacity(sessions.len() + 1);
    let mut entries = Vec::with_capacity(sessions.len());
    lines.push(format!("Gateway sessions: {}", sessions.len()));
    for session in &sessions {
        let (line, data) = render_session(session);
        lines.push(line);
        entries.push(data);
    }
    Ok(OpOutput {
        text: lines.join("\n"),
        data: Some(json!({ "sessions": entries })),
    })
}

/// Listeners of the gateway (local UDS, node transport) and whether each is
/// enabled.
///
/// # Errors
/// As [`gateway_status`].
#[operation(
    name = "gateway.listeners.list",
    summary = "Listet die Listener des Gateways (Art, Adresse, aktiv). Use this instead of running `harw gateway …`/`harw channel …` through `shell.exec`; the `harw` binary is not available inside the sandbox.",
    domain = "execution",
    permission = "maintainer",
    command(path = "/gateway-listeners", visibility = "tui_only", busy = "immediate"),
    model_tool(readonly, approval = "none"),
    // Reine Abfrage (`GatewayPort::listeners`), keine Mutation.
    web(path = "/api/gateway/listeners", method = "get", approval = "none")
)]
async fn gateway_listeners_list(
    ctx: &OpContext,
    _args: GatewayReadArgs,
) -> Result<OpOutput, OpError> {
    let port = gateway_port(ctx)?;
    let result = call("gateway.listeners.list", false, port.listeners()).await?;
    let mut lines = Vec::with_capacity(result.listeners.len() + 1);
    let mut entries = Vec::with_capacity(result.listeners.len());
    lines.push(format!("Gateway listeners: {}", result.listeners.len()));
    for listener in &result.listeners {
        let (line, data) = render_listener(listener);
        lines.push(line);
        entries.push(data);
    }
    Ok(OpOutput {
        text: lines.join("\n"),
        data: Some(json!({ "listeners": entries })),
    })
}

/// Tools the gateway tool host serves and the current tool grants of the
/// connected agent principals.
///
/// # Errors
/// As [`gateway_status`].
#[operation(
    name = "gateway.tools.list",
    summary = "Listet die Werkzeuge, die das Gateway ausführt, und die Werkzeug-Freigaben der verbundenen Agenten. Use this instead of running `harw gateway …`/`harw channel …` through `shell.exec`; the `harw` binary is not available inside the sandbox.",
    domain = "execution",
    permission = "maintainer",
    command(path = "/gateway-tools", visibility = "tui_only", busy = "immediate"),
    model_tool(readonly, approval = "none"),
    // Reine Abfrage (`GatewayPort::tools`), keine Mutation.
    web(path = "/api/gateway/tools", method = "get", approval = "none")
)]
async fn gateway_tools_list(ctx: &OpContext, _args: GatewayReadArgs) -> Result<OpOutput, OpError> {
    let port = gateway_port(ctx)?;
    let result = call("gateway.tools.list", false, port.tools()).await?;
    let mut lines = Vec::with_capacity(result.tools.len() + result.grants.len() + 2);
    lines.push(format!("Gateway tools: {}", result.tools.len()));
    let tools: Vec<Value> = result
        .tools
        .iter()
        .map(|tool| {
            let (line, data) = render_tool(tool);
            lines.push(line);
            data
        })
        .collect();
    lines.push(format!("Tool grants: {}", result.grants.len()));
    let grants: Vec<Value> = result
        .grants
        .iter()
        .map(|grant| {
            let (line, data) = render_rights(grant);
            lines.push(format!("  {line}"));
            data
        })
        .collect();
    Ok(OpOutput {
        text: lines.join("\n"),
        data: Some(json!({ "tools": tools, "grants": grants })),
    })
}

/// Configured external channels (Telegram, …): id, kind, enabled,
/// transport, tenant binding — never the bot-token reference.
///
/// Needs no gateway port (diagnostics group): the channel configuration is
/// local, and F1 asks for it exactly when no gateway session is connected.
///
/// # Errors
/// [`OpError::NotAvailable`] without the channel configuration.
#[operation(
    name = "gateway.channels.list",
    summary = "Listet die konfigurierten Kanäle (z. B. Telegram) mit Art, Aktiv-Status, Transport und Mandantenbindung — nie Tokens. Use this instead of running `harw gateway …`/`harw channel …` through `shell.exec`; the `harw` binary is not available inside the sandbox.",
    domain = "execution",
    permission = "maintainer",
    command(path = "/gateway-channels", visibility = "tui_only", busy = "immediate"),
    model_tool(readonly, approval = "none"),
    // Reine Abfrage der Kanal-Konfiguration, keine Mutation.
    web(path = "/api/gateway/channels", method = "get", approval = "none")
)]
async fn gateway_channels_list(
    ctx: &OpContext,
    _args: GatewayReadArgs,
) -> Result<OpOutput, OpError> {
    let config = channel_config(ctx)?;
    let (lines, entries) = render_channels(config);
    let text = if entries.is_empty() {
        "No channels configured. See gateway.channels.connect_info for how an operator connects one."
            .to_owned()
    } else {
        let mut text = format!("Configured channels: {}", entries.len());
        for line in lines {
            text.push('\n');
            text.push_str(&line);
        }
        text
    };
    Ok(OpOutput {
        text,
        data: Some(json!({ "channels": entries })),
    })
}

/// How an operator connects a channel — the information `harw channel
/// connect --help` gives a human — plus the current configuration state.
/// Never contains secrets. Needs no gateway port (diagnostics group).
///
/// # Errors
/// - [`OpError::NotAvailable`] without the channel configuration.
/// - [`OpError::InvalidArguments`] for an unsupported channel kind.
#[operation(
    name = "gateway.channels.connect_info",
    summary = "Erklärt, wie ein Operator einen Kanal (Telegram) anbindet und koppelt, und zeigt den aktuellen Konfigurationsstand — nie Tokens. Use this instead of running `harw gateway …`/`harw channel …` through `shell.exec`; the `harw` binary is not available inside the sandbox.",
    domain = "execution",
    permission = "maintainer",
    model_tool(readonly, approval = "none"),
    // Statische Anleitung plus Konfigurationsstand, keine Mutation.
    web(path = "/api/gateway/channels/connect-info", method = "get", approval = "none")
)]
async fn gateway_channels_connect_info(
    ctx: &OpContext,
    args: GatewayConnectInfoArgs,
) -> Result<OpOutput, OpError> {
    let config = channel_config(ctx)?;
    let kind = match args.channel.as_deref().map(str::trim) {
        None | Some("") => "telegram",
        Some(kind) if kind.eq_ignore_ascii_case("telegram") => "telegram",
        Some(_) => {
            return Err(OpError::InvalidArguments(
                "channel must be `telegram` (the only supported channel kind)".to_owned(),
            ));
        }
    };
    let configured: Vec<Value> = render_channels(config).1;
    Ok(OpOutput {
        text: connect_info_text(kind, &configured),
        data: Some(json!({
            "channel": kind,
            "steps": TELEGRAM_CONNECT_STEPS,
            "configured": configured,
        })),
    })
}

// ── Mutating operations ──────────────────────────────────────────────────────

/// Revokes one gateway connection (W00 §7 revocation semantics; also
/// cancels the principal's in-flight tool calls).
///
/// # Authority
/// Owner tier; `approval = "always"` on the model and web surfaces. The
/// gateway still requires `gateway_admin` and applies the tenant filter.
///
/// # Errors
/// - [`OpError::InvalidArguments`] without `connection` or `reason`.
/// - [`OpError::NotAvailable`] without a gateway port.
/// - [`OpError::Execution`] for an unknown connection or when the outcome is
///   unknown (timeout, transport failure).
#[operation(
    name = "gateway.connections.revoke",
    summary = "Widerruft eine Gateway-Verbindung (beendet auch ihre laufenden Werkzeugaufrufe). Fragt jedes Mal nach. Use this instead of running `harw gateway …`/`harw channel …` through `shell.exec`; the `harw` binary is not available inside the sandbox.",
    domain = "execution",
    permission = "owner",
    command(path = "/gateway-revoke", visibility = "tui_only"),
    model_tool(approval = "always"),
    // Mutation: POST, `approval = "always"` wie die Modell-Tool-Fläche.
    web(path = "/api/gateway/connections/revoke", method = "post", approval = "always")
)]
async fn gateway_connections_revoke(
    ctx: &OpContext,
    args: GatewayRevokeArgs,
) -> Result<OpOutput, OpError> {
    let connection = args
        .connection
        .ok_or_else(|| OpError::InvalidArguments("connection is required".to_owned()))?;
    let reason = bounded_required(args.reason.as_deref(), "reason", MAX_REASON_CHARS)?;
    let port = gateway_port(ctx)?;
    let result = call(
        "gateway.connections.revoke",
        true,
        port.revoke_connection(GatewayRevokeParams {
            connection,
            reason: reason.clone(),
        }),
    )
    .await?;
    let text = if result.revoked {
        format!(
            "Connection #{connection} revoked ({}).",
            sanitize(&reason, MAX_REASON_CHARS)
        )
    } else {
        format!("Connection #{connection} was already gone; nothing revoked.")
    };
    Ok(OpOutput {
        text,
        data: Some(json!({ "connection": connection, "revoked": result.revoked })),
    })
}

/// Sets the gateway draining: attached clients get `HostDraining`, new
/// turns and tool calls are refused, running work finishes.
///
/// # Authority
/// Owner tier; `approval = "always"`; the gateway admits only unscoped
/// callers.
///
/// # Errors
/// As [`gateway_connections_revoke`].
#[operation(
    name = "gateway.drain",
    summary = "Versetzt das Gateway in Draining: neue Turns und Werkzeugaufrufe werden abgelehnt, laufende enden regulär. Fragt jedes Mal nach. Use this instead of running `harw gateway …`/`harw channel …` through `shell.exec`; the `harw` binary is not available inside the sandbox.",
    domain = "execution",
    permission = "owner",
    command(path = "/gateway-drain", visibility = "tui_only"),
    model_tool(approval = "always"),
    // Mutation: POST, `approval = "always"` wie die Modell-Tool-Fläche.
    web(path = "/api/gateway/drain", method = "post", approval = "always")
)]
async fn gateway_drain(ctx: &OpContext, args: GatewayDrainArgs) -> Result<OpOutput, OpError> {
    let retry_after_ms = args.retry_after_ms.unwrap_or(DEFAULT_DRAIN_RETRY_AFTER_MS);
    let port = gateway_port(ctx)?;
    let status = call(
        "gateway.drain",
        true,
        port.drain(GatewayDrainParams { retry_after_ms }),
    )
    .await?;
    let (text, data) = render_status("Gateway is draining:", &status);
    Ok(OpOutput {
        text,
        data: Some(data),
    })
}

/// Enables or disables a configured gateway listener.
///
/// # Authority
/// Owner tier; `approval = "always"`; unscoped callers only.
///
/// # Errors
/// As [`gateway_connections_revoke`]; an unknown listener is an
/// [`OpError::Execution`] naming it.
#[operation(
    name = "gateway.listeners.set",
    summary = "Schaltet einen konfigurierten Gateway-Listener ein oder aus. Fragt jedes Mal nach. Use this instead of running `harw gateway …`/`harw channel …` through `shell.exec`; the `harw` binary is not available inside the sandbox.",
    domain = "execution",
    permission = "owner",
    model_tool(approval = "always"),
    // Mutation: POST, `approval = "always"` wie die Modell-Tool-Fläche.
    web(path = "/api/gateway/listeners/set", method = "post", approval = "always")
)]
async fn gateway_listeners_set(
    ctx: &OpContext,
    args: GatewayListenerSetArgs,
) -> Result<OpOutput, OpError> {
    let name = bounded_required(args.name.as_deref(), "name", MAX_REPORTED_TEXT)?;
    let enabled = args
        .enabled
        .ok_or_else(|| OpError::InvalidArguments("enabled is required".to_owned()))?;
    let port = gateway_port(ctx)?;
    let listener = call(
        "gateway.listeners.set",
        true,
        port.set_listener(GatewayListenerSetParams { name, enabled }),
    )
    .await?;
    let (line, data) = render_listener(&listener);
    Ok(OpOutput {
        text: format!("Listener updated:\n{line}"),
        data: Some(data),
    })
}

/// Replaces an agent principal's tool grant within its ceiling (its
/// parent's grant, or the configured UIA ceiling).
///
/// # Authority
/// Owner tier; `approval = "always"`. A name outside the ceiling is refused
/// by the gateway, never silently dropped.
///
/// # Errors
/// As [`gateway_connections_revoke`].
#[operation(
    name = "gateway.tools.grant",
    summary = "Ersetzt die Werkzeug-Freigabe eines Agenten innerhalb seiner Obergrenze. Fragt jedes Mal nach. Use this instead of running `harw gateway …`/`harw channel …` through `shell.exec`; the `harw` binary is not available inside the sandbox.",
    domain = "execution",
    permission = "owner",
    model_tool(approval = "always"),
    // Mutation: POST, `approval = "always"` wie die Modell-Tool-Fläche.
    web(path = "/api/gateway/tools/grant", method = "post", approval = "always")
)]
async fn gateway_tools_grant(
    ctx: &OpContext,
    args: GatewayToolRightsArgs,
) -> Result<OpOutput, OpError> {
    let params = rights_params(args)?;
    let port = gateway_port(ctx)?;
    let rights = call("gateway.tools.grant", true, port.grant_tools(params)).await?;
    let (line, data) = render_rights(&rights);
    Ok(OpOutput {
        text: format!("Tool grant replaced: {line}"),
        data: Some(data),
    })
}

/// Removes tool names from an agent principal's grant; descendants are
/// narrowed to `descendant ∩ parent` immediately.
///
/// # Authority
/// Owner tier; `approval = "always"`.
///
/// # Errors
/// As [`gateway_connections_revoke`].
#[operation(
    name = "gateway.tools.narrow",
    summary = "Entzieht einem Agenten (und seinen Delegierten) Werkzeuge. Fragt jedes Mal nach. Use this instead of running `harw gateway …`/`harw channel …` through `shell.exec`; the `harw` binary is not available inside the sandbox.",
    domain = "execution",
    permission = "owner",
    model_tool(approval = "always"),
    // Mutation: POST, `approval = "always"` wie die Modell-Tool-Fläche.
    web(path = "/api/gateway/tools/narrow", method = "post", approval = "always")
)]
async fn gateway_tools_narrow(
    ctx: &OpContext,
    args: GatewayToolRightsArgs,
) -> Result<OpOutput, OpError> {
    let params = rights_params(args)?;
    let port = gateway_port(ctx)?;
    let rights = call("gateway.tools.narrow", true, port.narrow_tools(params)).await?;
    let (line, data) = render_rights(&rights);
    Ok(OpOutput {
        text: format!("Tool grant narrowed: {line}"),
        data: Some(data),
    })
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn gateway_port(ctx: &OpContext) -> Result<&Arc<dyn GatewayPort>, OpError> {
    ctx.service::<Arc<dyn GatewayPort>>()
        .ok_or_else(|| OpError::NotAvailable(NOT_CONFIGURED.to_owned()))
}

fn channel_config(ctx: &OpContext) -> Result<&ResolvedConfig, OpError> {
    ctx.service::<Arc<ResolvedConfig>>()
        .map(AsRef::as_ref)
        .ok_or_else(|| {
            OpError::NotAvailable("the channel configuration is not available".to_owned())
        })
}

/// Awaits one gateway call with [`GATEWAY_CALL_TIMEOUT`] and maps its error.
async fn call<T>(what: &str, mutation: bool, future: PortFuture<'_, T>) -> Result<T, OpError> {
    match tokio::time::timeout(GATEWAY_CALL_TIMEOUT, future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(map_port_error(what, error, mutation)),
        Err(_) if mutation => Err(OpError::Execution(format!(
            "{what} timed out; the outcome is unknown — check with gateway.status before retrying"
        ))),
        Err(_) => Err(OpError::NotAvailable(format!(
            "{what}: the gateway did not answer in time"
        ))),
    }
}

/// Maps a port error to an operation error. Messages from the gateway are
/// sanitized; a failed mutation in transit reports an unknown outcome.
fn map_port_error(what: &str, error: PortError, mutation: bool) -> OpError {
    match error {
        PortError::Transport(detail) if mutation => OpError::Execution(format!(
            "{what} failed in transit ({}); the outcome is unknown — check with gateway.status \
             before retrying",
            sanitize(&detail, MAX_REPORTED_TEXT)
        )),
        PortError::Transport(detail) => OpError::NotAvailable(format!(
            "the gateway is unreachable: {}",
            sanitize(&detail, MAX_REPORTED_TEXT)
        )),
        PortError::Denied(reason) => OpError::NotAvailable(format!(
            "the gateway denied {what}: {}",
            sanitize(&reason, MAX_REPORTED_TEXT)
        )),
        PortError::Revoked => OpError::NotAvailable("gateway access was revoked".to_owned()),
        PortError::NotFound => OpError::Execution(format!(
            "{what}: not found (unknown connection, listener, agent or session, or not visible \
             to this caller)"
        )),
        other => OpError::Execution(format!(
            "{what}: {}",
            sanitize(&other.to_string(), MAX_REPORTED_TEXT)
        )),
    }
}

fn bounded_required(value: Option<&str>, field: &str, max: usize) -> Result<String, OpError> {
    let value = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| OpError::InvalidArguments(format!("{field} is required")))?;
    if value.chars().count() > max || value.chars().any(char::is_control) {
        return Err(OpError::InvalidArguments(format!(
            "{field} must be at most {max} characters without control characters"
        )));
    }
    Ok(value.to_owned())
}

fn rights_params(args: GatewayToolRightsArgs) -> Result<GatewayToolRightsParams, OpError> {
    let agent = bounded_required(args.agent.as_deref(), "agent", MAX_REPORTED_TEXT)?;
    let raw = args
        .tools
        .ok_or_else(|| OpError::InvalidArguments("tools is required".to_owned()))?;
    let mut tools = Vec::with_capacity(raw.len());
    for tool in &raw {
        tools.push(bounded_required(
            Some(tool),
            "tool name",
            MAX_TOOL_NAME_CHARS,
        )?);
    }
    tools.sort();
    tools.dedup();
    if tools.is_empty() {
        return Err(OpError::InvalidArguments(
            "tools must name at least one tool".to_owned(),
        ));
    }
    Ok(GatewayToolRightsParams { agent, tools })
}

/// Text from outside this process, safe to show: control characters
/// stripped, token-shaped substrings redacted (`harw_memory::redact`), at
/// most `max` characters (an ellipsis marks a cut).
///
/// # Example
/// ```rust
/// let shown = harw_ops::gateway_ops::sanitize("label\u{1b}[31m Bearer abcdefghijklmnop", 64);
/// assert!(!shown.contains("abcdefghijklmnop"));
/// assert!(!shown.contains('\u{1b}'));
/// ```
#[must_use]
pub fn sanitize(text: &str, max: usize) -> String {
    let visible: String = text.chars().filter(|c| !c.is_control()).collect();
    let redacted = harw_memory::redact(&visible);
    if redacted.chars().count() <= max {
        return redacted;
    }
    let mut cut: String = redacted.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

fn short(text: &str) -> String {
    sanitize(text, MAX_REPORTED_TEXT)
}

/// The serde wire name of a unit-like enum value (`kebab`/`snake` case as
/// declared), or of the tag field of an internally tagged enum.
fn wire_name<T: serde::Serialize>(value: &T, tag: &str) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(name)) => name,
        Ok(Value::Object(map)) => map
            .get(tag)
            .and_then(Value::as_str)
            .map_or_else(|| "unknown".to_owned(), str::to_owned),
        _ => "unknown".to_owned(),
    }
}

fn placement_text(placement: &ToolPlacement) -> String {
    match placement {
        ToolPlacement::Gateway { node: Some(node) } => format!("gateway@{}", short(node)),
        other => wire_name(other, "kind"),
    }
}

fn caps_names(caps: &ClientCaps) -> Vec<&'static str> {
    [
        (caps.observe, "observe"),
        (caps.steer, "steer"),
        (caps.approve, "approve"),
        (caps.control, "control"),
        (caps.tool_call, "tool_call"),
        (caps.gateway_read, "gateway_read"),
        (caps.gateway_admin, "gateway_admin"),
    ]
    .into_iter()
    .filter_map(|(on, name)| on.then_some(name))
    .collect()
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

pub(crate) fn render_status(title: &str, status: &GatewayStatus) -> (String, Value) {
    let node = status.node.as_deref().map(short);
    let sandbox = if status.sandbox_available {
        "available".to_owned()
    } else {
        "unavailable (every tool.call is refused)".to_owned()
    };
    let text = format!(
        "{title}\n\
         node           {}\n\
         host epoch     {}\n\
         draining       {}\n\
         connections    {}\n\
         sessions       {} ({} running turns)\n\
         listeners      {}\n\
         tools          {}\n\
         sandbox        {sandbox}",
        node.as_deref().unwrap_or("-"),
        status.host_epoch,
        yes_no(status.draining),
        status.connections,
        status.sessions,
        status.running_turns,
        status.listeners,
        status.tools,
    );
    let data = json!({
        "node": node,
        "host_epoch": status.host_epoch,
        "draining": status.draining,
        "connections": status.connections,
        "sessions": status.sessions,
        "running_turns": status.running_turns,
        "listeners": status.listeners,
        "tools": status.tools,
        "sandbox_available": status.sandbox_available,
    });
    (text, data)
}

fn render_principal(principal: &PrincipalSummary) -> (String, Value) {
    match principal {
        PrincipalSummary::Device { device } => {
            let device = device.as_ref().map(|device| short(device.as_str()));
            (
                format!("device {}", device.as_deref().unwrap_or("local")),
                json!({ "kind": "device", "device": device }),
            )
        }
        PrincipalSummary::Agent {
            agent,
            role,
            parent,
        } => {
            let agent = short(agent);
            let role = wire_name(role, "role");
            let parent = parent.as_deref().map(short);
            let text = match &parent {
                Some(parent) => format!("agent {agent} ({role}, delegated by {parent})"),
                None => format!("agent {agent} ({role})"),
            };
            (
                text,
                json!({ "kind": "agent", "agent": agent, "role": role, "parent": parent }),
            )
        }
        _ => ("unknown principal".to_owned(), json!({ "kind": "unknown" })),
    }
}

fn render_connection(connection: &GatewayConnectionInfo) -> (String, Value) {
    let label = short(&connection.label);
    let (principal_text, principal) = render_principal(&connection.principal);
    let tenant = connection
        .tenant
        .as_ref()
        .map(|tenant| short(tenant.as_str()));
    let caps: Option<Vec<&str>> = connection.granted.as_ref().map(caps_names);
    let caps_text = match &caps {
        Some(caps) if caps.is_empty() => "none".to_owned(),
        Some(caps) => caps.join(","),
        None => "before hello".to_owned(),
    };
    let since = connection.since.to_string();
    let line = format!(
        "#{} {label}: {principal_text}, tenant {}, caps {caps_text}, attached {}, since {since}",
        connection.connection,
        tenant.as_deref().unwrap_or("-"),
        connection.attached,
    );
    let data = json!({
        "connection": connection.connection,
        "label": label,
        "principal": principal,
        "tenant": tenant,
        "caps": caps,
        "attached": connection.attached,
        "since": since,
    });
    (line, data)
}

fn render_session(session: &SessionSummary) -> (String, Value) {
    let id = short(session.session_id.as_str());
    let state = wire_name(&session.state, "state");
    let title = session.title.as_deref().map(short);
    let tenant = session.tenant.as_ref().map(|tenant| short(tenant.as_str()));
    let model = session.model.as_deref().map(short);
    let updated = session.updated_at.to_string();
    let line = format!(
        "{id} {state}: {}, tenant {}, attached {}, model {}, updated {updated}",
        title.as_deref().unwrap_or("(untitled)"),
        tenant.as_deref().unwrap_or("-"),
        session.attached,
        model.as_deref().unwrap_or("-"),
    );
    let data = json!({
        "session_id": id,
        "state": state,
        "title": title,
        "tenant": tenant,
        "attached": session.attached,
        "model": model,
        "updated_at": updated,
    });
    (line, data)
}

fn render_listener(listener: &GatewayListenerInfo) -> (String, Value) {
    let name = short(&listener.name);
    let kind = wire_name(&listener.kind, "kind");
    let address = short(&listener.address);
    let state = if listener.enabled {
        "enabled"
    } else {
        "disabled"
    };
    (
        format!("{name} ({kind}) {address}: {state}"),
        json!({ "name": name, "kind": kind, "address": address, "enabled": listener.enabled }),
    )
}

fn render_tool(tool: &ToolDescriptor) -> (String, Value) {
    let name = short(&tool.name);
    let approval = wire_name(&tool.approval, "approval");
    let placement = placement_text(&tool.placement);
    let description = sanitize(&tool.description, MAX_DESCRIPTION_TEXT);
    (
        format!("  {name} (approval {approval}, runs in {placement})"),
        json!({
            "name": name,
            "approval": approval,
            "placement": placement,
            "parallel_safe": tool.parallel_safe,
            "description": description,
        }),
    )
}

fn render_rights(rights: &GatewayToolRights) -> (String, Value) {
    let agent = short(&rights.agent);
    let role = wire_name(&rights.role, "role");
    let tools: Vec<String> = rights.tools.iter().map(|tool| short(tool)).collect();
    let list = if tools.is_empty() {
        "(no tools)".to_owned()
    } else {
        tools.join(", ")
    };
    (
        format!("{agent} ({role}): {list}"),
        json!({ "agent": agent, "role": role, "tools": tools }),
    )
}

/// One line and one JSON entry per configured channel, sorted by id. The
/// bot-token reference and the pinned identities are never shown.
fn render_channels(config: &ResolvedConfig) -> (Vec<String>, Vec<Value>) {
    let mut ids: Vec<&String> = config.channels.keys().collect();
    ids.sort();
    let mut lines = Vec::with_capacity(ids.len());
    let mut entries = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(channel) = config.channels.get(id) else {
            continue;
        };
        match channel {
            ChannelToml::Telegram(telegram) => {
                let id = short(&telegram.id);
                let transport = short(&telegram.transport);
                let tenant = short(&telegram.tenant_binding);
                let state = if telegram.enabled {
                    "enabled"
                } else {
                    "disabled"
                };
                lines.push(format!(
                    "{id} (telegram): {state}, transport {transport}, tenant binding {tenant}, \
                     {} pinned identities, {} workspaces",
                    telegram.security.pinned_identities.len(),
                    telegram.workspaces.len(),
                ));
                entries.push(json!({
                    "id": id,
                    "kind": "telegram",
                    "enabled": telegram.enabled,
                    "transport": transport,
                    "tenant_binding": tenant,
                    "pinned_identities": telegram.security.pinned_identities.len(),
                    "allow_unpinned_pairing": telegram.security.allow_unpinned_pairing,
                    "workspaces": telegram.workspaces.len(),
                }));
            }
        }
    }
    (lines, entries)
}

/// What an operator does to connect Telegram (mirrors `harw channel
/// connect`, `harw-cli/src/connect.rs`). Steps for a human on the host; the
/// model relays them and never handles the token.
const TELEGRAM_CONNECT_STEPS: [&str; 5] = [
    "Create a bot with @BotFather and keep its token secret. Never paste the token into the chat; the assistant must not see, ask for or relay it.",
    "On the host, export the token as HARW_TELEGRAM_BOT_TOKEN in the environment of the operator's shell (the channel file stores only the reference env:HARW_TELEGRAM_BOT_TOKEN).",
    "On the host, run `harw channel connect telegram`: it verifies the bot, writes channels/telegram.toml (disabled until paired) and prints a one-time pairing code.",
    "Send `/pair <code>` to the bot from the Telegram account that should be paired, then run `harw channel connect telegram --pair <code>` on the host.",
    "Stop the gateway during pairing (both poll the same bot; Telegram delivers each update to one poller only), or pair through the running gateway with security.allow_unpinned_pairing; afterwards start the gateway again.",
];

fn connect_info_text(kind: &str, configured: &[Value]) -> String {
    let mut text = format!(
        "How an operator connects a {kind} channel (these steps run on the host, not in the \
         sandbox):"
    );
    for (index, step) in TELEGRAM_CONNECT_STEPS.iter().enumerate() {
        text.push_str(&format!("\n{}. {step}", index + 1));
    }
    if configured.is_empty() {
        text.push_str("\nCurrently configured: none.");
    } else {
        text.push_str("\nCurrently configured:");
        for channel in configured {
            let id = channel.get("id").and_then(Value::as_str).unwrap_or("?");
            let enabled = channel
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            text.push_str(&format!(
                "\n- {id}: {}",
                if enabled {
                    "enabled"
                } else {
                    "disabled (not paired yet?)"
                }
            ));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{
        GatewayRevokeArgs, MAX_REPORTED_TEXT, register_gateway, register_gateway_diagnostics,
        rights_params, sanitize,
    };
    use crate::test_support::{TestResult, ctx};
    use crate::testutil::toks;
    use harw_operations::FromRawArgs;
    use harw_operations::registry::OperationRegistry;

    #[test]
    fn sanitize_strips_control_redacts_tokens_and_bounds_length() {
        let shown = sanitize(
            "a\u{7}b Bearer abcdefghijklmnopqrstuv sk-abcdefghijklmnopqrstu",
            200,
        );
        assert!(!shown.contains('\u{7}'), "{shown}");
        assert!(!shown.contains("abcdefghijklmnopqrstuv"), "{shown}");
        assert!(!shown.contains("sk-abcdefghijklmnopqrstu"), "{shown}");
        let long = "x".repeat(MAX_REPORTED_TEXT * 2);
        assert_eq!(sanitize(&long, 10).chars().count(), 10);
    }

    #[test]
    fn revoke_args_parse_from_command_tokens() -> TestResult {
        let args = GatewayRevokeArgs::from_raw_args(&toks(&["7", "stale", "tablet"]))
            .map_err(ctx("from_raw_args"))?;
        assert_eq!(args.connection, Some(7));
        assert_eq!(args.reason.as_deref(), Some("stale tablet"));
        assert!(GatewayRevokeArgs::from_raw_args(&toks(&["x"])).is_err());
        Ok(())
    }

    #[test]
    fn rights_params_sort_dedup_and_refuse_empty_sets() -> TestResult {
        let params = rights_params(super::GatewayToolRightsArgs {
            agent: Some("uia-1".to_owned()),
            tools: Some(vec!["fs.read".into(), "fs.list".into(), "fs.read".into()]),
        })
        .map_err(ctx("rights_params"))?;
        assert_eq!(params.tools, ["fs.list", "fs.read"]);
        for tools in [None, Some(Vec::new()), Some(vec![" ".to_owned()])] {
            let result = rights_params(super::GatewayToolRightsArgs {
                agent: Some("uia-1".to_owned()),
                tools,
            });
            assert!(result.is_err(), "{result:?}");
        }
        Ok(())
    }

    #[test]
    fn registration_collides_instead_of_panicking() -> TestResult {
        let mut registry = OperationRegistry::new();
        register_gateway(&mut registry).map_err(ctx("register"))?;
        register_gateway_diagnostics(&mut registry).map_err(ctx("register diagnostics"))?;
        assert!(register_gateway(&mut registry).is_err());
        assert!(register_gateway_diagnostics(&mut registry).is_err());
        Ok(())
    }
}
