//! Grammatik von `harw provider`: Modell-Anbieter verwalten und abfragen.

use clap::{Subcommand, ValueHint};

use super::values::ApiDialect;

/// Aktionen des `harw provider`-Subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum ProviderAction {
    /// Listet alle Anbieter des aktiven Profils auf.
    List,
    /// Legt einen neuen Anbieter an oder überschreibt einen gleichnamigen.
    Add {
        /// Name des Anbieters.
        #[arg(value_hint = ValueHint::Other)]
        name: String,
        /// API-Dialekt, z. B. `openai-chat`, `openai-responses`,
        /// `anthropic-messages`, `ollama`.
        #[arg(long, value_enum)]
        api: ApiDialect,
        /// Basis-URL des Anbieters.
        #[arg(long = "base-url", value_name = "URL", value_hint = ValueHint::Url)]
        base_url: String,
        /// Verweis auf das Geheimnis (`env:VAR` oder `secrets:NAME`); ein
        /// Schlüssel im Klartext wird abgelehnt.
        #[arg(long, value_hint = ValueHint::Other)]
        auth: Option<String>,
        /// Modell-IDs dieses Anbieters, kommagetrennt.
        #[arg(long, value_delimiter = ',', value_hint = ValueHint::Other)]
        models: Vec<String>,
        /// Credential-Transport: `bearer` (Standard bei Schlüssel), `x-api-key`,
        /// `api-key` oder `none`. Ohne Angabe: `none` für lokale Endpunkte
        /// ohne `--auth`, sonst die Vorgabe des Transports (Runde 7, Teil L1).
        #[arg(
            long = "auth-header",
            value_parser = ["bearer", "x-api-key", "api-key", "none"],
            conflicts_with = "no_auth"
        )]
        auth_header: Option<String>,
        /// Kein Schlüssel und kein Auth-Header, z. B. für einen lokalen
        /// vLLM- oder LM-Studio-Server (gleichbedeutend mit
        /// `--auth-header none` ohne `--auth`).
        #[arg(long = "no-auth", conflicts_with = "auth")]
        no_auth: bool,
        /// Erlaubt unverschlüsseltes `http` zu einer privaten LAN-IP
        /// (`10/8`, `172.16/12`, `192.168/16`); sonst nur Loopback.
        #[arg(long = "allow-insecure-lan")]
        allow_insecure_lan: bool,
    },
    /// Entfernt einen Anbieter.
    Remove {
        /// Name des Anbieters.
        #[arg(value_hint = ValueHint::Other)]
        name: String,
    },
    /// Aktiviert einen zuvor deaktivierten Anbieter.
    Enable {
        /// Name des Anbieters.
        #[arg(value_hint = ValueHint::Other)]
        name: String,
    },
    /// Deaktiviert einen Anbieter, ohne ihn zu löschen.
    Disable {
        /// Name des Anbieters.
        #[arg(value_hint = ValueHint::Other)]
        name: String,
    },
    /// Fragt die verfügbaren Modelle der Anbieter ab und übernimmt sie.
    Scan {
        /// Nur diesen Anbieter abfragen; ohne Angabe alle aktivierten.
        #[arg(value_hint = ValueHint::Other)]
        provider: Option<String>,
        /// Nur kostenlose Modelle anzeigen und übernehmen.
        #[arg(long)]
        free_only: bool,
        /// Nicht mehr gemeldete Modelle aus der Auswahl entfernen.
        #[arg(long)]
        prune: bool,
    },
}
