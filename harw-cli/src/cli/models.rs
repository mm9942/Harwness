//! Grammatik von `harw model` (Alias `models`): Modelle entdecken/verwalten
//! und interne Modellstellen konfigurieren.

use clap::{Subcommand, ValueHint};

use super::values::OnOff;

/// Aktionen des `harw model`-Subcommands.
///
/// Ohne Unterbefehl (`Command::Model { action: None }`) entspricht das dem
/// `list`-Zweig (siehe `crate::models::run`).
#[derive(Debug, Subcommand)]
pub enum ModelsAction {
    /// Listet aktivierte Provider, konfigurierte Modelle (mit markiertem
    /// Standardmodell) und die internen Modellstellen samt Auflösung.
    List,
    /// Fragt die verfügbaren Modelle der konfigurierten Anbieter ab.
    ///
    /// Remote-Aufrufe prüfen die Adresse mit dem URL-Guard (https, keine
    /// privaten Adressen, keine Weiterleitungen).
    Scan {
        /// Nur diesen Provider abfragen; ohne Angabe alle aktivierten
        /// Provider.
        #[arg(value_hint = ValueHint::Other)]
        provider: Option<String>,
        /// Veraltet und nur noch zur Rückwärtskompatibilität akzeptiert:
        /// jeder erfolgreiche Scan synchronisiert Modell-Dateien ohnehin.
        #[arg(long, hide = true)]
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
    #[command(alias = "delete")]
    Remove {
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
    /// Dieselbe Rollenauflösung und Modellwahl wie der Chat-Befehl /models.
    Roles {
        #[command(subcommand)]
        action: Option<RoleModelAction>,
    },
    /// Bildverständnis-Modell dieses Profils anzeigen oder bewusst auswählen.
    Image {
        #[command(subcommand)]
        action: Option<ImageModelAction>,
    },
    /// Setzt das globale Standardmodell.
    Default {
        /// Modell-ID, wie in `models/<id>.toml` deklariert.
        #[arg(value_hint = ValueHint::Other)]
        id: String,
    },
    /// Zeigt den Anbieter-Katalog oder aktualisiert ihn aus models.dev.
    Catalog {
        /// Modell-Listen aus models.dev aktualisieren.
        #[arg(long)]
        refresh: bool,
    },
}

/// Feste Argumentstruktur für die profilweite Rollenwahl.
#[derive(Debug, Subcommand)]
pub enum RoleModelAction {
    /// Zeigt Rollen, Quellen und Reasoning-Gewicht ohne laufende Chat-Sitzung.
    Show,
    /// Setzt ein Modell für eine bestimmte Rolle in diesem Profil.
    Set {
        #[arg(value_name = "ROLE")]
        role: String,
        #[arg(value_name = "MODEL")]
        target: String,
        /// Optionales Gewicht für eine Rolle mit eigener Reasoning-Konfiguration.
        #[arg(long, value_name = "LEVEL")]
        effort: Option<String>,
    },
    /// Entfernt die explizite Modellwahl dieser Rolle.
    Reset {
        #[arg(value_name = "ROLE")]
        role: String,
    },
}

/// Bildverständnis ist nicht allgemeine Textmodellwahl oder Bildgenerierung.
#[derive(Debug, Subcommand)]
pub enum ImageModelAction {
    /// Zeigt die gespeicherte Wahl bzw. den aufgelösten Standard.
    Show,
    /// Wählt ein Modell mit bestätigtem Bild-Input.
    Set {
        #[arg(value_name = "MODEL")]
        target: String,
    },
    /// Entfernt die explizite Wahl ohne einen Anbieter einzusetzen.
    Reset,
}

/// Aktionen des `harw model internal`-Subcommands.
#[derive(Debug, Subcommand)]
pub enum InternalAction {
    /// Zeigt jede interne Modellstelle mit ihrer effektiven Auflösung
    /// (explizit / OpenRouter-Standard / Hauptmodell).
    Show,
    /// Setzt eine interne Modellstelle explizit auf `model`, optional bei
    /// einem anderen Provider als `harness.default_provider`.
    Set {
        /// Stellen-Schlüssel, z. B. `session_title` oder `session-title`.
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::ModelsAction;
    use crate::cli::{Cli, Command};
    use crate::test_support::{TestError, TestResult, ctx};

    fn action(args: &[&str]) -> TestResult<Option<ModelsAction>> {
        let cli = Cli::try_parse_from(args).map_err(ctx("models-Aufruf sollte parsen"))?;
        match cli.command {
            Some(Command::Model { action }) => Ok(action),
            other => Err(TestError::Unexpected(format!(
                "erwartete Model, bekam {other:?}"
            ))),
        }
    }

    #[test]
    fn scan_parses_provider_and_flags() -> TestResult {
        let parsed = action(&[
            "harw",
            "models",
            "scan",
            "openrouter",
            "--free-only",
            "--prune",
        ])?;
        let Some(ModelsAction::Scan {
            provider,
            free_only,
            prune,
            add,
        }) = parsed
        else {
            return Err(TestError::Unexpected("erwartete Scan".into()));
        };
        assert_eq!(provider.as_deref(), Some("openrouter"));
        assert!(free_only);
        assert!(prune);
        assert!(!add);
        Ok(())
    }

    #[test]
    fn remove_alias_delete_parses() -> TestResult {
        let parsed = action(&["harw", "models", "delete", "openrouter/some-model"])?;
        let Some(ModelsAction::Remove { target }) = parsed else {
            return Err(TestError::Unexpected("erwartete Remove".into()));
        };
        assert_eq!(target, "openrouter/some-model");
        Ok(())
    }

    #[test]
    fn add_without_target_parses() -> TestResult {
        let parsed = action(&["harw", "models", "add"])?;
        let Some(ModelsAction::Add { target }) = parsed else {
            return Err(TestError::Unexpected("erwartete Add".into()));
        };
        assert_eq!(target, None);
        Ok(())
    }
}
