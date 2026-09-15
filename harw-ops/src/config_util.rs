//! Wiederverwendbare Config-Discovery-Helfer für ops-Handler.
//!
//! Bündelt das Muster "harw_home::config_layers + discover_config", das in
//! mehreren Ops (model, provider, …) mehrfach vorkam.
//!
//! # Verantwortungsbereich
//! Stellt [`load_default_config`] bereit, um die Initialisierung von
//! `HARW_HOME`, Profil-Layern und Config-Discovery auf eine einzelne,
//! einheitliche Zeile zu reduzieren. Stellt außerdem
//! [`persist_default_selection`] bereit, mit dem `/provider switch` und
//! `/model switch` den zuletzt gewählten Provider/Modell als Standard für
//! künftige Sitzungen in der Profil-`config.toml` verankern — bestes Bemühen,
//! niemals ein Fehler für den Aufrufer (der In-Session-Wechsel steht bereits).
//!
//! # Exportierte Typen
//! Keine öffentlichen Typen — alle Items sind `pub(crate)`.
//!
//! # Nebenläufigkeit
//! Zustandslos; sicher von mehreren Threads aus aufrufbar.
//!
//! # Fehlertypen
//! - [`harw_operations::OpError::Execution`]: wenn die Config-Discovery fehlschlägt.
//! - [`persist_default_selection`] liefert nie `Err` — Persistenzfehler werden
//!   als menschenlesbare Notiz zurückgegeben, nicht propagiert.
//!
//! # Spec-Referenz
//! harwness Plan v2 — Config-Discovery-Konsolidierung; Folgeauftrag
//! „zuletzt gewählter Provider/Modell bleibt Standard".

use harw_config::ResolvedConfig;
use harw_operations::OpError;

/// Lädt die Config aus dem aktiven `HARW_HOME` und dessen Layern.
///
/// # Description
/// Ruft [`harw_home::home_dir`] und anschließend
/// [`harw_home::config_layers`] auf, damit das aktive Profil wie beim CLI-Chat
/// berücksichtigt wird. Die resultierenden Layer werden an
/// [`harw_config::discover_config`] übergeben. Fehler werden in
/// [`OpError::Execution`] mit einem optionalen Kontext-Präfix umgewandelt.
///
/// Fehlt ein Layer-Verzeichnis, überspringt [`harw_config::discover_config`] es
/// stillschweigend — das Ergebnis ist dann ein leeres [`ResolvedConfig`] mit
/// `None`-Defaults, aber kein Fehler.
///
/// # Arguments
/// - `context` (`&str`): Präfix für die Fehlermeldung, z.B. `"Config-Discovery fehlgeschlagen"`.
///   Bei leerem Kontext wird nur die Rohmeldung ohne Präfix übernommen.
///
/// # Returns
/// Die aufgelöste [`ResolvedConfig`].
///
/// # Errors
/// - [`OpError::Execution`]: wenn die Home-Auflösung, Layer-Ermittlung oder
///   [`harw_config::discover_config`] fehlschlägt.
///   Die Fehlermeldung lautet `"{context}: {e}"` wenn `context` nicht leer ist,
///   andernfalls nur `"{e}"`.
///
/// # Panics
/// Nie.
///
/// # Concurrency
/// Zustandslos; sicher von mehreren Threads aus aufrufbar.
///
/// # Examples
/// ```rust,ignore
/// // Called from within harw-ops — module is pub(crate).
/// let config = crate::config_util::load_default_config("Config-Discovery fehlgeschlagen")?;
/// ```
pub(crate) fn load_default_config(context: &str) -> Result<ResolvedConfig, OpError> {
    let home = harw_home::home_dir().map_err(|error| execution_error(context, error))?;
    let layers =
        harw_home::config_layers(&home).map_err(|error| execution_error(context, error))?;
    harw_config::discover_config(&layers).map_err(|error| execution_error(context, error))
}

fn execution_error(context: &str, error: impl std::fmt::Display) -> OpError {
    if context.is_empty() {
        OpError::Execution(error.to_string())
    } else {
        OpError::Execution(format!("{context}: {error}"))
    }
}

/// Verankert `default_provider`/`default_model` bestes Bemühen in der
/// Profil-`config.toml` (`<home>/profiles/<aktives-profil>/config.toml`) —
/// denselben Datei- und Schlüsselnamen, die `harw onboard` beim Ersteinstieg
/// schreibt (siehe `harw-cli/src/onboarding.rs`).
///
/// # Description
/// Wird nach einem **bereits erfolgreichen** `/provider switch` oder
/// `/model switch` aufgerufen. Öffnet die Datei über
/// [`harw_config::ConfigWriter`] (erhält Kommentare/Formatierung), setzt nur
/// die übergebenen Schlüssel und schreibt sie zurück. `None`-Argumente werden
/// nicht geschrieben — ein reiner Provider-Wechsel ohne bekanntes aktives
/// Modell hinterlässt also keinen veralteten `default_model`-Eintrag.
///
/// Diese Funktion ist bewusst **niemals fehlschlagend** für den Aufrufer: der
/// In-Session-Wechsel über den [`harw_operations::SessionController`] hat zu
/// diesem Zeitpunkt bereits stattgefunden und darf durch eine
/// Persistenz-Panne nicht rückgängig gemacht werden. Jeder interne Fehler
/// (Home nicht auflösbar, Schreibfehler, Validierung) wird stattdessen als
/// deutschsprachige Notiz zurückgegeben, die der Aufrufer an die
/// Operator-Ausgabe anhängen kann.
///
/// # Arguments
/// - `default_provider` (`Option<&str>`): kanonische Provider-ID, die als
///   neuer Standard geschrieben werden soll, oder `None`, um den Schlüssel
///   unverändert zu lassen.
/// - `default_model` (`Option<&str>`): kanonische Modell-ID, analog.
///
/// # Returns
/// `None` bei Erfolg (nichts zu berichten) oder wenn beide Argumente `None`
/// sind (nichts zu tun); `Some(note)` mit einer für Menschen lesbaren Notiz,
/// wenn die Persistenz fehlschlug.
///
/// # Panics
/// Nie.
///
/// # Concurrency
/// Rein synchron; kein Datei-Lock — parallele Aufrufe auf dieselbe Datei sind
/// nicht abgesichert, analog zu [`harw_config::ConfigWriter`] selbst.
///
/// # Examples
/// ```rust,ignore
/// // Nach einem erfolgreichen controller.set_active_provider(...):
/// if let Some(note) = crate::config_util::persist_default_selection(Some("anthropic"), None) {
///     text.push('\n');
///     text.push_str(&note);
/// }
/// ```
pub(crate) fn persist_default_selection(
    default_provider: Option<&str>,
    default_model: Option<&str>,
) -> Option<String> {
    if default_provider.is_none() && default_model.is_none() {
        return None;
    }
    match try_persist_default_selection(default_provider, default_model) {
        Ok(()) => None,
        Err(reason) => Some(format!(
            "Hinweis: konnte nicht dauerhaft als Standard für künftige Sitzungen gespeichert werden ({reason})."
        )),
    }
}

/// Interner, fehlschlagender Kern von [`persist_default_selection`].
fn try_persist_default_selection(
    default_provider: Option<&str>,
    default_model: Option<&str>,
) -> Result<(), String> {
    let home = harw_home::home_dir().map_err(|error| error.to_string())?;
    let profile = harw_home::active_profile_name(&home);
    let profile_dir = harw_home::profile_dir(&home, &profile).map_err(|error| error.to_string())?;
    let config_path = profile_dir.join("config.toml");

    let mut writer = harw_config::ConfigWriter::open(&config_path).map_err(|error| error.to_string())?;
    if let Some(provider) = default_provider {
        writer.set_value("default_provider", toml_edit::value(provider));
    }
    if let Some(model) = default_model {
        writer.set_value("default_model", toml_edit::value(model));
    }
    writer.save().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{OpError, execution_error};

    #[test]
    fn execution_error_adds_context_prefix() {
        let error = execution_error("Config-Discovery fehlgeschlagen", "broken home");

        assert!(matches!(
            error,
            OpError::Execution(message)
                if message == "Config-Discovery fehlgeschlagen: broken home"
        ));
    }

    #[test]
    fn execution_error_keeps_raw_message_without_context() {
        let error = execution_error("", "broken config layers");

        assert!(matches!(
            error,
            OpError::Execution(message) if message == "broken config layers"
        ));
    }

}
