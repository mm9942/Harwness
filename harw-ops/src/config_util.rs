//! Wiederverwendbare Config-Discovery-Helfer für ops-Handler.
//!
//! Bündelt das Muster "harw_home::config_layers + discover_config", das in
//! mehreren Ops (model, provider, …) mehrfach vorkam.
//!
//! # Verantwortungsbereich
//! Stellt [`load_default_config`] bereit, um die Initialisierung von
//! `HARW_HOME`, Profil-Layern und Config-Discovery auf eine einzelne,
//! einheitliche Zeile zu reduzieren.
//!
//! # Exportierte Typen
//! Keine öffentlichen Typen — alle Items sind `pub(crate)`.
//!
//! # Nebenläufigkeit
//! Zustandslos; sicher von mehreren Threads aus aufrufbar.
//!
//! # Fehlertypen
//! - [`harw_operations::OpError::Execution`]: wenn die Config-Discovery fehlschlägt.
//!
//! # Spec-Referenz
//! harwness Plan v2 — Config-Discovery-Konsolidierung.

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
