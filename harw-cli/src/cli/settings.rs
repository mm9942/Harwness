//! Grammatik von `harw settings`: Provider, Modelle, Freigaben und
//! einzelne Konfigurationswerte.

use clap::{Args, Subcommand, ValueHint};

use super::values::{ApiDialect, PermissionMode};

/// Auszuführende Aktion unter `harw settings`.
///
/// Ohne diesen Subcommand (`Cli::command == Some(Command::Settings { action:
/// None })`) startet [`crate::settings::run`] das interaktive Menü.
#[derive(Debug, Subcommand)]
pub enum SettingsAction {
    /// Provider verwalten (`providers/<name>.toml` im aktiven Profil).
    Provider {
        #[command(subcommand)]
        action: SettingsProviderAction,
    },
    /// Standardmodell verwalten.
    Model {
        #[command(subcommand)]
        action: SettingsModelAction,
    },
    /// Freigabe-Standardmodus und Allow-/Deny-Regeln — derselbe Schreibpfad
    /// wie das `/permissions`-Panel der TUI.
    Permissions {
        #[command(subcommand)]
        action: SettingsPermissionsAction,
    },
    /// Liest einen einzelnen, punktgetrennten Konfigurationsschlüssel.
    Get {
        /// Punktgetrennter Schlüsselpfad, z. B. `permissions.default_mode`.
        #[arg(value_hint = ValueHint::Other)]
        key: String,
        #[command(flatten)]
        scope: SettingsScopeArgs,
    },
    /// Setzt einen einzelnen, punktgetrennten Konfigurationsschlüssel.
    Set {
        /// Punktgetrennter Schlüsselpfad, z. B. `permissions.default_mode`.
        #[arg(value_hint = ValueHint::Other)]
        key: String,
        /// Neuer Wert als Text; wird als TOML-String geschrieben (siehe
        /// `crate::settings`). Ohne Wert wird der Schlüssel gelöscht.
        #[arg(value_hint = ValueHint::Other)]
        value: Option<String>,
        #[command(flatten)]
        scope: SettingsScopeArgs,
    },
}

/// Ziel-Ebene (`SettingScope`) eines `get`/`set`/`permissions`-Aufrufs.
///
/// `--global` ist die Vorgabe, wenn keines der beiden Flags gesetzt ist;
/// `--global` und `--project` schließen sich gegenseitig aus, unabhängig von
/// der Reihenfolge (siehe `AnalyzeArgs::bottom_up`/`top_down` für dasselbe
/// Muster).
#[derive(Debug, Clone, Copy, Args)]
pub struct SettingsScopeArgs {
    /// Schreibt/liest die dauerhafte User-Ebene (`~/.harw/…`). Vorgabe.
    #[arg(long, conflicts_with = "project")]
    pub global: bool,
    /// Schreibt/liest die dauerhafte Projekt-Ebene
    /// (`~/.harw/profiles/<p>/projects/<key>/settings.toml`).
    #[arg(long)]
    pub project: bool,
}

impl SettingsScopeArgs {
    /// Löst die Flags in einen [`harw_config::SettingScope`] auf.
    ///
    /// # Returns
    /// [`harw_config::SettingScope::Project`], wenn `--project` gesetzt ist,
    /// sonst [`harw_config::SettingScope::Global`] (Vorgabe). Nie `Session`
    /// — die CLI-Grammatik kennt keinen Weg, eine reine Speicher-Ebene
    /// anzusprechen.
    #[must_use]
    pub fn resolve(self) -> harw_config::SettingScope {
        if self.project {
            harw_config::SettingScope::Project
        } else {
            harw_config::SettingScope::Global
        }
    }
}

/// Aktionen des `harw settings provider`-Subcommands.
#[derive(Debug, Subcommand)]
pub enum SettingsProviderAction {
    /// Listet alle Provider des aktiven Profils.
    List,
    /// Legt einen neuen Provider an (oder überschreibt einen gleichnamigen).
    Add {
        /// Provider-Name (Dateiname `providers/<name>.toml`).
        #[arg(value_hint = ValueHint::Other)]
        name: String,
        /// API-Dialekt, z. B. `openai-chat`, `openai-responses`,
        /// `anthropic-messages`, `ollama`.
        #[arg(long, value_enum)]
        api: ApiDialect,
        /// Basis-URL des Providers.
        #[arg(long = "base-url", value_name = "URL", value_hint = ValueHint::Url)]
        base_url: String,
        /// Secret-Referenz (`env:VAR` oder `secrets:NAME`); ein Klartext-Key
        /// wird abgelehnt.
        #[arg(long, value_hint = ValueHint::Other)]
        auth: Option<String>,
        /// Modell-IDs dieses Providers, kommagetrennt.
        #[arg(long, value_delimiter = ',', value_hint = ValueHint::Other)]
        models: Vec<String>,
    },
    /// Entfernt einen Provider.
    Remove {
        /// Provider-Name.
        #[arg(value_hint = ValueHint::Other)]
        name: String,
    },
    /// Aktiviert einen zuvor deaktivierten Provider.
    Enable {
        /// Provider-Name.
        #[arg(value_hint = ValueHint::Other)]
        name: String,
    },
    /// Deaktiviert einen Provider, ohne ihn zu löschen.
    Disable {
        /// Provider-Name.
        #[arg(value_hint = ValueHint::Other)]
        name: String,
    },
}

/// Aktionen des `harw settings model`-Subcommands.
#[derive(Debug, Subcommand)]
pub enum SettingsModelAction {
    /// Setzt das Standardmodell (`default_model`) der globalen Ebene.
    Default {
        /// Modell-ID, wie in `models/<id>.toml` deklariert.
        #[arg(value_hint = ValueHint::Other)]
        id: String,
    },
}

/// Aktionen des `harw settings permissions`-Subcommands — derselbe
/// Schreibpfad wie das `/permissions`-Panel (Contract §2/§5 Zeile A2).
#[derive(Debug, Subcommand)]
pub enum SettingsPermissionsAction {
    /// Zeigt Standardmodus, Timeout und Allow-/Deny-Regeln der gewählten
    /// Ebene.
    Get {
        #[command(flatten)]
        scope: SettingsScopeArgs,
    },
    /// Setzt den Standard-Freigabemodus (`ask`, `auto` oder `full`).
    SetMode {
        /// `ask`, `auto` oder `full`.
        #[arg(value_enum)]
        mode: PermissionMode,
        #[command(flatten)]
        scope: SettingsScopeArgs,
    },
    /// Hängt eine Allow-Regel an.
    Allow {
        /// Werkzeugname, z. B. `shell.exec`.
        tool: String,
        /// Optionales Muster (Shell-Präfix bzw. Pfad-Glob).
        #[arg(long, value_hint = ValueHint::Other)]
        pattern: Option<String>,
        #[command(flatten)]
        scope: SettingsScopeArgs,
    },
    /// Hängt eine Deny-Regel an.
    Deny {
        /// Werkzeugname, z. B. `fs.write`.
        tool: String,
        /// Optionales Muster (Shell-Präfix bzw. Pfad-Glob).
        #[arg(long, value_hint = ValueHint::Other)]
        pattern: Option<String>,
        #[command(flatten)]
        scope: SettingsScopeArgs,
    },
    /// Entfernt eine Allow-Regel per Index (siehe `permissions get`).
    Unallow {
        /// Index in der Allow-Liste, 0-basiert.
        index: usize,
        #[command(flatten)]
        scope: SettingsScopeArgs,
    },
    /// Entfernt eine Deny-Regel per Index (siehe `permissions get`).
    Undeny {
        /// Index in der Deny-Liste, 0-basiert.
        index: usize,
        #[command(flatten)]
        scope: SettingsScopeArgs,
    },
}
