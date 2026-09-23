//! Grammatik von `harw gateway`: Service-Aktionen und Telemetrie-Ziele.

use clap::{Args, Subcommand, ValueHint};

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
}
