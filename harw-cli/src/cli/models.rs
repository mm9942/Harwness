//! Grammatik von `harw models`: Modelle entdecken/verwalten und interne
//! Modellstellen konfigurieren.

use clap::{Subcommand, ValueHint};

use super::values::OnOff;

/// Aktionen des `harw models`-Subcommands (Addendum C).
///
/// Ohne diesen Subcommand (`Cli::command == Some(Command::Models { action:
/// None })`) entspricht das dem `list`-Zweig (siehe `crate::models::run`).
#[derive(Debug, Subcommand)]
pub enum ModelsAction {
    /// Listet aktivierte Provider, konfigurierte Modelle (mit markiertem
    /// Standardmodell) und die internen Modellstellen samt Auflösung.
    List,
    /// Fragt die `/models`-Endpunkte konfigurierter Provider ab
    /// (`harw_provider_http::discovery::list_models`).
    Scan {
        /// Nur diesen Provider abfragen; ohne Angabe alle aktivierten
        /// Provider.
        #[arg(value_hint = ValueHint::Other)]
        provider: Option<String>,
        /// Veraltet und nur noch zur Rückwärtskompatibilität akzeptiert:
        /// jeder erfolgreiche Scan synchronisiert Modell-Dateien ohnehin.
        #[arg(long)]
        add: bool,
        /// Zeigt/übernimmt nur kostenlose Modelle (Preis 0 oder
        /// `:free`-Suffix der Modell-ID).
        #[arg(long)]
        free_only: bool,
        /// Entfernt nicht mehr gemeldete Modelle aus der Auswahl und dem
        /// lokalen Cache (`models/*.toml`). Ohne dieses Flag ergänzt der
        /// Scan nur neue/aktuelle Modelle und meldet veraltete Einträge als
        /// „nicht mehr gemeldet“, ohne sie zu löschen. Ein von der aktiven
        /// Konfiguration referenziertes Modell (`default_model`,
        /// `uia_model`, `session.title_model`, `internal_models.*`) wird
        /// auch mit `--prune` nie entfernt.
        #[arg(long)]
        prune: bool,
    },
    /// Fügt ein live entdecktes Modell zur sichtbaren Auswahl hinzu.
    Add {
        /// `provider/modell`; ohne Angabe öffnet sich der Auswahl-Picker.
        #[arg(value_hint = ValueHint::Other)]
        target: Option<String>,
    },
    /// Entfernt ein Modell aus der sichtbaren Auswahl und dem lokalen Cache.
    Delete {
        /// `provider/modell`.
        #[arg(value_hint = ValueHint::Other)]
        target: String,
    },
    /// Interne Modellstellen verwalten (Session-Titel, Kompaktierungs-
    /// Zusammenfassung, Speicher-Konsolidierung, Traumreflexion, Explorer,
    /// Recherche). Ohne Unterbefehl entspricht dies `internal show`.
    Internal {
        #[command(subcommand)]
        action: Option<InternalAction>,
    },
    /// Setzt das globale Standardmodell — derselbe Schreibpfad wie `harw
    /// settings model default`.
    Default {
        /// Modell-ID, wie in `models/<id>.toml` deklariert.
        #[arg(value_hint = ValueHint::Other)]
        id: String,
    },
}

/// Aktionen des `harw models internal`-Subcommands.
#[derive(Debug, Subcommand)]
pub enum InternalAction {
    /// Zeigt jede interne Modellstelle mit ihrer effektiven Auflösung
    /// (explizit / OpenRouter-Standard / Hauptmodell).
    Show,
    /// Setzt eine interne Modellstelle explizit auf `model`, optional bei
    /// einem anderen Provider als `harness.default_provider`.
    Set {
        /// Stellen-Schlüssel, z. B. `session_title` oder `session-title`
        /// (siehe `harw_config::InternalModelPoint::parse`).
        #[arg(value_hint = ValueHint::Other)]
        point: String,
        /// Modell-ID beim gewählten Provider.
        #[arg(value_hint = ValueHint::Other)]
        model: String,
        /// Provider-Name; ohne Angabe `harness.default_provider`.
        #[arg(long, value_hint = ValueHint::Other)]
        provider: Option<String>,
    },
    /// Erzwingt für diese Stelle das Hauptmodell der Sitzung (leere Wahl).
    Main {
        /// Stellen-Schlüssel.
        #[arg(value_hint = ValueHint::Other)]
        point: String,
    },
    /// Entfernt eine explizite Wahl für diese Stelle; die Auflösung fällt
    /// zurück auf den OpenRouter-Standard bzw. das Hauptmodell.
    Reset {
        /// Stellen-Schlüssel.
        #[arg(value_hint = ValueHint::Other)]
        point: String,
    },
    /// Schaltet `use_openrouter_defaults` global an (`on`) oder aus (`off`).
    OpenrouterDefaults {
        /// `on` oder `off`.
        #[arg(value_enum)]
        state: OnOff,
    },
}
