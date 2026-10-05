//! Grammatik von `harw device` und `harw node` (PL-94, Zyklus N1): enrollte
//! Geräte des Own-Cloud-Listeners verwalten und den Listener-Zustand zeigen.

use clap::{Subcommand, ValueEnum, ValueHint};

/// Berechtigungsstufe eines enrollten Geräts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DeviceTier {
    /// Nur lesen und beobachten.
    #[value(name = "observer")]
    Observer,
    /// Sitzungen bedienen.
    #[value(name = "operator")]
    Operator,
    /// Sitzungen und Einstellungen pflegen.
    #[value(name = "maintainer")]
    Maintainer,
    /// Volle Rechte; braucht beim Erhöhen `--yes-owner`.
    #[value(name = "owner")]
    Owner,
}

/// Aktionen des `harw device`-Subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum DeviceAction {
    /// Listet die enrollten Geräte (Gerät, Node-ID, Mandant, Tier, Status, Label).
    ///
    /// Zeilen der Registry-Datei, die der Parser verwirft, werden mit
    /// Zeilennummer als Warnung gemeldet; sie gewähren weiterhin nichts.
    List,
    /// Setzt den Tier eines Geräts; ändert nur dessen Zeile in `node-devices.conf`.
    ///
    /// Die Datei wird atomar (temporäre Datei + Umbenennen, Modus 0600)
    /// geschrieben, Kommentare und andere Zeilen bleiben unverändert. Der Tier
    /// wirkt beim nächsten Handshake des Geräts.
    SetTier {
        /// Geräte-ID (genau eine Zeile der Registry muss passen).
        #[arg(value_name = "DEVICE", value_hint = ValueHint::Other)]
        device: String,
        /// Neuer Tier.
        #[arg(value_enum, value_name = "TIER")]
        tier: DeviceTier,
        /// Bestätigt ausdrücklich das Erhöhen auf `owner`.
        #[arg(long)]
        yes_owner: bool,
    },
    /// Widerruft ein Gerät (idempotent).
    ///
    /// Neue Handshakes werden sofort abgelehnt; ein laufendes Gateway kappt
    /// bestehende Verbindungen beim nächsten Registry-Scan.
    Revoke {
        /// Geräte-ID.
        #[arg(value_name = "DEVICE", value_hint = ValueHint::Other)]
        device: String,
    },
}

/// Aktionen des `harw node`-Subcommands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
pub enum NodeAction {
    /// Zeigt `[session_listener]`, Bind-Adresse, Dateirechte, Gerätezahlen und Probleme.
    ///
    /// Liest und erzeugt keinen Schlüssel im Auth-Hub.
    Status,
}
