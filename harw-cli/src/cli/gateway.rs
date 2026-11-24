//! Grammatik von `harw gateway`: Service-Aktionen und Telemetrie-Ziele.

use std::path::PathBuf;

use clap::{Args, Subcommand, ValueHint, value_parser};
use harw_types::SessionId;

/// Zusätzliche Telemetrie-Exportziele für `harw gateway`.
///
/// # Description
/// Der rotierende JSONL-Sink unter `<home>/telemetry`
/// ([`harw_home::paths::telemetry_dir`]) läuft **immer** — er ist kein Flag,
/// weil `security.*`/`warden.*`-Metriken ohne Operator-Freigabe ausschließlich
/// dorthin gehen müssen (siehe `crate::observe`-Moduldoku). Beide Flags hier
/// steuern ausschließlich *zusätzliche* Ziele für gewöhnliche Metriken; ohne
/// sie bleibt jedes zusätzliche Ziel **aus** — kein Endpunkt bindet, kein
/// Stapel wird gesendet.
#[derive(Debug, Args)]
pub struct TelemetryArgs {
    /// Bindet einen lokalen Prometheus-`/metrics`-Endpunkt auf `127.0.0.1:<PORT>`
    /// (nur Loopback, siehe `harw_observe_prom::BindAddr`). Ohne dieses Flag
    /// bindet kein Endpunkt.
    #[arg(long, value_name = "PORT")]
    pub metrics_prometheus_port: Option<u16>,
    /// Exportiert nicht-geschützte Metriken als OTLP/JSON-Stapel per HTTP an
    /// diesen Collector, z. B. `http://127.0.0.1:4318/v1/metrics`. Ohne
    /// dieses Flag wird kein Stapel gesendet. `security.*`/`warden.*`
    /// erreichen dieses Ziel nie, unabhängig von diesem Flag (siehe
    /// `crate::observe`-Moduldoku).
    #[arg(long, value_name = "URL", value_hint = ValueHint::Url)]
    pub metrics_otlp_endpoint: Option<String>,
}

/// Aktionen für die dedizierte `harw-gateway.service`-Unit.
#[derive(Debug, Subcommand)]
pub enum GatewayAction {
    /// Unit schreiben, aktivieren und starten.
    Install,
    /// Gateway-Dienst starten.
    Start,
    /// Gateway-Dienst geordnet stoppen.
    Stop,
    /// Gateway-Dienst neu starten.
    Restart,
    /// Gateway-Dienst beim Login aktivieren.
    Enable,
    /// Gateway-Dienst beim Login deaktivieren.
    Disable,
    /// Status des Gateway-Dienstes anzeigen.
    Status,
    /// Inspect or operate an authenticated session host (not the service manager).
    Control(GatewayControlArgs),
    /// Export historical creation evidence from the active private writer profile (local only).
    #[cfg(all(target_os = "linux", feature = "direct-owner-placement"))]
    OwnerCreationReceipt {
        /// Exact tenant expected by the operator.
        #[arg(long)]
        tenant: String,
        /// Exact hosted session ID.
        session: String,
    },
    /// Verify a historical owner creation receipt from stdin, without writing placement.
    #[cfg(all(target_os = "linux", feature = "owner-placement-hub"))]
    VerifyOwnerCreationReceipt {
        /// Absolute path to an independently provisioned private receipt pin.
        #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
        pin: PathBuf,
        /// Exact tenant independently expected by the operator.
        #[arg(long)]
        tenant: String,
        /// Exact session ID independently expected by the operator.
        session: String,
    },
    /// Verify the current isolated writer over the protected co-located Unix lane.
    #[cfg(all(target_os = "linux", feature = "owner-placement-hub"))]
    VerifyOwnerCreationReadiness {
        /// Absolute operator-private configuration for the one exact proof.
        #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
        pin: PathBuf,
        /// Exact tenant independently expected by the operator.
        #[arg(long)]
        tenant: String,
        /// Exact session ID independently expected by the operator.
        session: String,
    },
    /// Serve the explicitly provisioned, read-only owner placement Hub on Linux.
    #[cfg(all(target_os = "linux", feature = "owner-placement-hub"))]
    OwnerPlacementHub {
        /// Absolute path to an operator-private Hub configuration.
        #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
        config: PathBuf,
    },
}

/// The same pinned host aliases and local socket selection as `harw attach`.
#[derive(Debug, Args)]
pub struct GatewayControlArgs {
    /// Explicit local Unix socket; without a target use the default runtime socket.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath, conflicts_with = "host")]
    pub socket: Option<PathBuf>,
    /// A configured host alias with the enrolled, pinned server identity.
    #[arg(long, value_name = "ALIAS")]
    pub host: Option<String>,
    #[command(subcommand)]
    pub action: GatewayControlAction,
}

#[derive(Debug, Args)]
pub struct HostPageArgs {
    /// Opaque cursor received from the preceding page on this host.
    #[arg(long)]
    pub cursor: Option<String>,
    /// Maximum rows for this request.
    #[arg(long, value_parser = value_parser!(u32).range(1..=200))]
    pub limit: Option<u32>,
}

#[derive(Debug, Subcommand)]
pub enum GatewayControlAction {
    /// Report the selected transport and its negotiated features and permissions.
    Inspect,
    /// List one page of authorized session metadata.
    Sessions {
        /// Select a project key; an empty value selects projectless sessions.
        #[arg(long)]
        project: Option<String>,
        /// Select the metadata-only Internal page instead of ordinary sessions.
        #[arg(long, conflicts_with = "project")]
        internal: bool,
        #[command(flatten)]
        page: HostPageArgs,
    },
    /// List one page of authorized project metadata.
    Projects {
        #[command(flatten)]
        page: HostPageArgs,
    },
    /// Read one session's status from the explicitly selected pinned owner.
    /// Requires --host OWNER_ALIAS and owner's negotiated Observe support.
    DirectOwnerStatus {
        /// Exact session ID; never resolved from names or history.
        #[arg(value_name = "SESSION_ID")]
        session: SessionId,
    },
    /// Verify one session against both the pinned Onion owner and Root Hub.
    /// Requires --host OWNER_ALIAS; never falls back to direct transport or V1.
    #[cfg(feature = "onion")]
    FencedOwnerStatus {
        /// Separately pinned Onion alias for the Root Hub.
        #[arg(long, value_name = "HUB_ALIAS")]
        hub: String,
        /// Exact session ID; never resolved from names or history.
        #[arg(value_name = "SESSION_ID")]
        session: SessionId,
    },
    /// Set a title on one exact hosted session.
    Rename {
        #[arg(value_name = "SESSION_ID")]
        session: String,
        #[arg(value_name = "TITLE")]
        title: String,
    },
    /// Move one exact hosted session to an existing project without creating directories.
    Move {
        #[arg(value_name = "SESSION_ID")]
        session: String,
        #[arg(value_name = "PROJECT_KEY")]
        project: String,
    },
    /// Create an empty remote work session. Use the same ID to resolve an uncertain outcome.
    CreateEmpty {
        #[arg(long, value_name = "ID")]
        operation_id: String,
    },
    /// Resolve a create-empty receipt without sending a duplicate request.
    ResolveEmpty {
        #[arg(long, value_name = "ID")]
        operation_id: String,
    },
    /// Use the separately provisioned Assistant room for this enrolled identity.
    Assistant {
        #[command(subcommand)]
        action: HostedAssistantAction,
    },
    /// Read Library metadata for this enrolled identity. No image bytes are sent to stdout.
    Library,
}

#[derive(Debug, Subcommand)]
pub enum HostedAssistantAction {
    /// Report room status without showing conversations.
    Status,
    /// Read completed turns from this exact room.
    History,
    /// Provision the encrypted room on the selected Gateway.
    Provision,
    /// Read exactly one text message from stdin; the ID also recovers a lost acknowledgement.
    Send {
        #[arg(long, value_name = "ID")]
        message_id: String,
        /// Include this completed turn from the same Cloud Assistant room (repeat up to four times).
        #[arg(long, value_name = "TURN_ID")]
        recall: Vec<String>,
    },
    /// Use a separate explicitly shared room, never the device-private room.
    Shared {
        #[command(subcommand)]
        action: HostedSharedAssistantAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum HostedSharedAssistantAction {
    /// Show room state and eligible device requests, without conversation text.
    Status,
    /// Print completed turns only if this device has been explicitly admitted.
    History,
    /// Explicitly create a fresh room for the current verified user.
    Provision,
    /// Ask the founding device to allow this exact device and key.
    RequestJoin,
    /// Approve a pending request after checking its full device key fingerprint.
    Approve {
        #[arg(long, value_name = "REQUEST_ID")]
        request_id: String,
        #[arg(long, value_name = "HEX")]
        fingerprint: String,
    },
    /// Withdraw your own request or remove a device as the founder.
    Remove {
        #[arg(long, value_name = "REQUEST_ID")]
        request_id: String,
    },
    /// Revoke this shared room, without touching the device-private room.
    Revoke {
        #[arg(long)]
        confirm: bool,
    },
    /// Read a message from stdin. Reuse the same ID after a lost acknowledgement.
    Send {
        #[arg(long, value_name = "ID")]
        message_id: String,
    },
}
