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
//! [`persist_uia_selection`] ist der strukturelle Zwilling für `uia_provider`/
//! `uia_model`; [`persist_uia_worker_model`] verankert zusätzlich
//! `uia_worker_model` — ein einzelner Wert ohne eigenes
//! `uia_worker_provider`-Pendant (der Worker teilt sich den effektiven
//! UIA-Provider mit `/uia-model`), der anders als die beiden anderen keinen
//! Live-`SessionController`-Pfad hat und erst beim nächsten Sitzungsstart wirkt.
//!
//! # Exportierte Typen
//! Keine öffentlichen Typen — alle Items sind `pub(crate)`.
//!
//! # Nebenläufigkeit
//! Zustandslos; sicher von mehreren Threads aus aufrufbar.
//!
//! # Fehlertypen
//! - [`harw_operations::OpError::Execution`]: wenn die Config-Discovery fehlschlägt.
//! - [`persist_default_selection`]/[`persist_uia_selection`]/
//!   [`persist_uia_worker_model`] liefern nie `Err` — Persistenzfehler werden
//!   als menschenlesbare Notiz zurückgegeben, nicht propagiert.
//!
//! # Spec-Referenz
//! harwness Plan v2 — Config-Discovery-Konsolidierung; Folgeauftrag
//! „zuletzt gewählter Provider/Modell bleibt Standard"; Welle 2 (2d) —
//! `uia_worker_model`-Persistenz.

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

/// Verankert `uia_provider`/`uia_model` bestes Bemühen in der
/// Profil-`config.toml`, unabhängig von `default_provider`/`default_model`.
///
/// # Description
/// Struktureller Zwilling von [`persist_default_selection`]: wird nach einer
/// (künftigen) UIA-spezifischen Provider-/Modell-Auswahl aufgerufen und
/// schreibt die Schlüssel `uia_provider`/`uia_model` statt
/// `default_provider`/`default_model`. `None`-Argumente werden nicht
/// geschrieben; sind beide `None`, geschieht nichts. Ebenfalls **niemals
/// fehlschlagend** für den Aufrufer — Persistenzfehler werden als
/// deutschsprachige Notiz zurückgegeben statt propagiert.
///
/// # Arguments
/// - `uia_provider` (`Option<&str>`): kanonische Provider-ID für die
///   UIA-Sitzung, oder `None`, um den Schlüssel unverändert zu lassen.
/// - `uia_model` (`Option<&str>`): kanonische Modell-ID; `None` entfernt einen
///   bestehenden UIA-Modell-Pin, damit ein Providerwechsel kein inkompatibles
///   Modell wiederbelebt.
///
/// # Returns
/// `None` bei Erfolg oder wenn beide Argumente `None` sind; `Some(note)` mit
/// einer für Menschen lesbaren Notiz, wenn die Persistenz fehlschlug.
///
/// # Panics
/// Nie.
///
/// # Concurrency
/// Rein synchron; kein Datei-Lock, analog zu [`persist_default_selection`].
///
/// # Examples
/// ```rust,ignore
/// if let Some(note) = crate::config_util::persist_uia_selection(Some("anthropic"), None) {
///     text.push('\n');
///     text.push_str(&note);
/// }
/// ```
pub(crate) fn persist_uia_selection(
    uia_provider: Option<&str>,
    uia_model: Option<&str>,
) -> Option<String> {
    if uia_provider.is_none() && uia_model.is_none() {
        return None;
    }
    match try_persist_uia_selection(uia_provider, uia_model) {
        Ok(()) => None,
        Err(reason) => Some(format!(
            "Hinweis: konnte UIA-Auswahl nicht dauerhaft speichern ({reason})."
        )),
    }
}

/// Interner, fehlschlagender Kern von [`persist_uia_selection`].
fn try_persist_uia_selection(
    uia_provider: Option<&str>,
    uia_model: Option<&str>,
) -> Result<(), String> {
    let home = harw_home::home_dir().map_err(|error| error.to_string())?;
    let profile = harw_home::active_profile_name(&home);
    let profile_dir = harw_home::profile_dir(&home, &profile).map_err(|error| error.to_string())?;
    let config_path = profile_dir.join("config.toml");

    let mut writer = harw_config::ConfigWriter::open(&config_path).map_err(|error| error.to_string())?;
    if let Some(provider) = uia_provider {
        writer.set_value("uia_provider", toml_edit::value(provider));
    }
    match uia_model {
        Some(model) => writer.set_value("uia_model", toml_edit::value(model)),
        None => {
            writer.remove_value("uia_model");
        }
    }
    writer.save().map_err(|error| error.to_string())
}

/// Verankert `uia_worker_model` bestes Bemühen in der Profil-`config.toml`,
/// unabhängig von `uia_provider`/`uia_model`/`default_provider`/`default_model`.
///
/// # Description
/// Struktureller Zwilling von [`persist_uia_selection`], aber für **einen
/// einzigen** Wert: es gibt bewusst kein `uia_worker_provider`-Pendant — der
/// UIA-Worker teilt sich den effektiven UIA-Provider mit `/uia-model`
/// (`harness.uia_provider`, aufgelöst über
/// [`crate::model::effective_uia_selection`]), nicht einen eigenen. Im
/// Gegensatz zu [`persist_default_selection`]/[`persist_uia_selection`], bei
/// denen `None` „diesen Schlüssel unverändert lassen" bedeutet (weil dort
/// zwei Werte unabhängig voneinander gesetzt werden können), ist `None` hier
/// eine explizite Aktion: den Pin entfernen. Ebenfalls **niemals
/// fehlschlagend** für den Aufrufer — Persistenzfehler werden als
/// deutschsprachige Notiz zurückgegeben statt propagiert. Anders als
/// `/uia-model switch` hat der UIA-Worker-Modell-Wechsel keinen
/// Live-`SessionController`-Pfad: `uia_worker_model` wirkt erst beim
/// nächsten Sitzungsstart, wie andere `internal_models.*`-Punkte.
///
/// # Arguments
/// - `model` (`Option<&str>`): kanonische Modell-ID für den UIA-Worker, die
///   als `uia_worker_model` geschrieben werden soll, oder `None`, um den Pin
///   zu entfernen.
///
/// # Returns
/// `None` bei Erfolg; `Some(note)` mit einer für Menschen lesbaren Notiz,
/// wenn die Persistenz fehlschlug.
///
/// # Panics
/// Nie.
///
/// # Concurrency
/// Rein synchron; kein Datei-Lock, analog zu [`persist_uia_selection`].
///
/// # Examples
/// ```rust,ignore
/// if let Some(note) = crate::config_util::persist_uia_worker_model(Some("claude-worker-x")) {
///     text.push('\n');
///     text.push_str(&note);
/// }
/// ```
pub(crate) fn persist_uia_worker_model(model: Option<&str>) -> Option<String> {
    match try_persist_uia_worker_model(model) {
        Ok(()) => None,
        Err(reason) => Some(format!(
            "Hinweis: konnte UIA-Worker-Modell nicht dauerhaft speichern ({reason})."
        )),
    }
}

/// Interner, fehlschlagender Kern von [`persist_uia_worker_model`].
fn try_persist_uia_worker_model(model: Option<&str>) -> Result<(), String> {
    let home = harw_home::home_dir().map_err(|error| error.to_string())?;
    let profile = harw_home::active_profile_name(&home);
    let profile_dir = harw_home::profile_dir(&home, &profile).map_err(|error| error.to_string())?;
    let config_path = profile_dir.join("config.toml");

    let mut writer = harw_config::ConfigWriter::open(&config_path).map_err(|error| error.to_string())?;
    match model {
        Some(model) => writer.set_value("uia_worker_model", toml_edit::value(model)),
        None => {
            writer.remove_value("uia_worker_model");
        }
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

    // ── UIA-Auswahl-Persistenz-Rundlauf, ohne echte HARW_HOME-Env-Mutation ──
    // Analog zum Muster in `harw-ops/src/permissions.rs`
    // (`test_permissions_persistence_round_trip_default_mode_and_rules`):
    // `try_persist_uia_selection` selbst löst `HARW_HOME` über
    // `harw_home::home_dir()` auf, was den Prozess-weiten Zustand mutieren
    // würde (nicht thread-sicher, würde parallel laufende Tests gefährden).
    // Der Rundlauf testet daher denselben `ConfigWriter`-Schreibpfad direkt
    // gegen ein temporäres Verzeichnis.
    #[test]
    fn test_uia_selection_persistence_round_trip_writes_uia_keys() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("config.toml");

        let mut writer = harw_config::ConfigWriter::open(&config_path).expect("open");
        writer.set_value("uia_provider", toml_edit::value("anthropic"));
        writer.set_value("uia_model", toml_edit::value("claude-x"));
        writer.save().expect("save");

        let reopened = harw_config::ConfigWriter::open(&config_path).expect("reopen");
        assert_eq!(reopened.get_value("uia_provider"), Some("anthropic".to_owned()));
        assert_eq!(reopened.get_value("uia_model"), Some("claude-x".to_owned()));

        let content = std::fs::read_to_string(&config_path).expect("read back");
        assert!(content.contains("uia_provider"));
        assert!(content.contains("uia_model"));
    }

    // ── uia_worker_model-Persistenz-Rundlauf, ohne echte HARW_HOME-Env-Mutation ──
    // Derselbe Grund wie beim UIA-Auswahl-Rundlauf oben: `try_persist_uia_worker_model`
    // löst `HARW_HOME` selbst auf, deshalb testet dieser Rundlauf denselben
    // `ConfigWriter`-Schreibpfad direkt gegen ein temporäres Verzeichnis — inklusive
    // des `None`-Zweigs, der (anders als bei `persist_default_selection`/
    // `persist_uia_selection`) eine explizite Entfernung ist, kein "unverändert lassen".
    #[test]
    fn test_uia_worker_model_persistence_round_trip_writes_and_removes_the_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("config.toml");

        let mut writer = harw_config::ConfigWriter::open(&config_path).expect("open");
        writer.set_value("uia_worker_model", toml_edit::value("claude-worker-x"));
        writer.save().expect("save");

        let reopened = harw_config::ConfigWriter::open(&config_path).expect("reopen");
        assert_eq!(
            reopened.get_value("uia_worker_model"),
            Some("claude-worker-x".to_owned())
        );
        let content = std::fs::read_to_string(&config_path).expect("read back");
        assert!(content.contains("uia_worker_model"));

        // `None` removes the key (an explicit action, unlike the "leave
        // unchanged" semantics of `persist_default_selection`/`persist_uia_selection`).
        let mut writer = harw_config::ConfigWriter::open(&config_path).expect("reopen for removal");
        assert!(writer.remove_value("uia_worker_model"));
        writer.save().expect("save after removal");

        let reopened = harw_config::ConfigWriter::open(&config_path).expect("reopen after removal");
        assert!(reopened.get_value("uia_worker_model").is_none());
    }
}
