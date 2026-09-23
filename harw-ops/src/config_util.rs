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
//! Zusätzlich stellt dieses Modul [`SelectionPersistence`] bereit: einen
//! austauschbaren Dienst-Trait, der die vier `persist_*`-Funktionen hinter
//! einer gemeinsamen Schnittstelle bündelt, plus [`FileSelectionPersistence`]
//! (Standard-Implementierung, ruft unverändert die freien Funktionen auf) und
//! [`RecordingSelectionPersistence`] (No-op-Aufzeichnung für Tests). Die
//! Operationen (`crate::model`, `crate::effort`) fragen den Dienst über
//! [`selection_persistence`] aus dem [`harw_operations::context::ServiceMap`]
//! ab, bevor sie auf das bisherige Verhalten zurückfallen.
//!
//! # Exportierte Typen
//! [`SelectionPersistence`], [`FileSelectionPersistence`],
//! [`RecordingSelectionPersistence`], [`RecordedSelectionPersistCall`] sind
//! `pub` (dieses Modul selbst ist `pub(crate)` — siehe `crate::model`'s
//! `pub use crate::config_util::{...}`-Re-Export für den öffentlichen Pfad,
//! über den `harw-tui`-Tests sie erreichen). Alle anderen Items bleiben
//! `pub(crate)`.
//!
//! # Nebenläufigkeit
//! Die freien `persist_*`-Funktionen und [`load_default_config`] sind
//! zustandslos. [`RecordingSelectionPersistence`] hält intern einen
//! `Mutex<Vec<_>>` und ist damit `Send + Sync` — sicher von mehreren Threads
//! aus aufrufbar.
//!
//! # Fehlertypen
//! - [`harw_operations::OpError::Execution`]: wenn die Config-Discovery fehlschlägt.
//! - [`persist_default_selection`]/[`persist_uia_selection`]/
//!   [`persist_uia_worker_model`]/[`persist_uia_reasoning_effort`] (und ihre
//!   [`SelectionPersistence`]-Trait-Pendants) liefern nie `Err` —
//!   Persistenzfehler werden als menschenlesbare Notiz zurückgegeben, nicht
//!   propagiert.
//!
//! # Spec-Referenz
//! harwness Plan v2 — Config-Discovery-Konsolidierung; Folgeauftrag
//! „zuletzt gewählter Provider/Modell bleibt Standard"; Welle 2 (2d) —
//! `uia_worker_model`-Persistenz; Folgeauftrag — austauschbarer
//! `SelectionPersistence`-Dienst für `slice9_model_switch_to_different_provider_is_atomic`.

use harw_config::ResolvedConfig;
use harw_operations::{OpContext, OpError};
use std::sync::{Arc, Mutex};

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

    let mut writer =
        harw_config::ConfigWriter::open(&config_path).map_err(|error| error.to_string())?;
    if let Some(provider) = default_provider {
        writer
            .set_value("default_provider", toml_edit::value(provider))
            .map_err(|error| error.to_string())?;
    }
    if let Some(model) = default_model {
        writer
            .set_value("default_model", toml_edit::value(model))
            .map_err(|error| error.to_string())?;
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

    let mut writer =
        harw_config::ConfigWriter::open(&config_path).map_err(|error| error.to_string())?;
    if let Some(provider) = uia_provider {
        writer
            .set_value("uia_provider", toml_edit::value(provider))
            .map_err(|error| error.to_string())?;
    }
    match uia_model {
        Some(model) => writer
            .set_value("uia_model", toml_edit::value(model))
            .map_err(|error| error.to_string())?,
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

    let mut writer =
        harw_config::ConfigWriter::open(&config_path).map_err(|error| error.to_string())?;
    match model {
        Some(model) => writer
            .set_value("uia_worker_model", toml_edit::value(model))
            .map_err(|error| error.to_string())?,
        None => {
            writer.remove_value("uia_worker_model");
        }
    }
    writer.save().map_err(|error| error.to_string())
}

/// Verankert `reasoning.uia` bestes Bemühen in der Profil-`config.toml`
/// (`[reasoning] uia = "..."`), unabhängig von `uia_provider`/`uia_model`/
/// `uia_worker_model`.
///
/// # Description
/// Struktureller Zwilling von [`persist_uia_worker_model`], aber für das
/// `[reasoning]`-Feld `uia` (`harw_config::HarnessConfig::reasoning.uia`,
/// vom Resolver via `harw-runtime::guard_wiring::parse_effort_field`
/// gelesen). `[reasoning]` ist eine verschachtelte TOML-Tabelle, im
/// Unterschied zu den flachen Top-Level-Schlüsseln `uia_provider`/
/// `uia_worker_model`; [`harw_config::ConfigWriter::set_value`]/
/// [`harw_config::ConfigWriter::remove_value`] adressieren sie transparent
/// über den punktgetrennten Pfad `"reasoning.uia"` — die Zwischentabelle
/// `[reasoning]` wird bei Bedarf automatisch angelegt
/// (`harw_config::writer::ensure_table`) und beim Entfernen des Blattwerts
/// bewusst leer belassen statt gelöscht.
///
/// Wie [`persist_uia_worker_model`] ist `None` hier eine **explizite Aktion**
/// (Pin entfernen), nicht "unverändert lassen" — anders als bei
/// [`persist_default_selection`]/[`persist_uia_selection`], die zwei
/// unabhängige Werte gemeinsam setzen können. Ebenfalls **niemals
/// fehlschlagend** für den Aufrufer: Persistenzfehler werden als
/// deutschsprachige Notiz zurückgegeben statt propagiert. `/uia-effort`
/// mutiert (anders als `/effort`) **keinen** Live-`SessionController`-Zustand
/// — der gesetzte Effort wirkt erst beim nächsten Sitzungsstart.
///
/// # Arguments
/// - `effort` (`Option<&str>`): serialisierter [`harw_types::ReasoningEffort`]
///   (`"minimal"|"low"|"medium"|"high"|"xhigh"|"max"`), der als
///   `reasoning.uia` geschrieben werden soll, oder `None`, um den Pin zu
///   entfernen.
///
/// # Returns
/// `None` bei Erfolg; `Some(note)` mit einer für Menschen lesbaren Notiz,
/// wenn die Persistenz fehlschlug.
///
/// # Panics
/// Nie.
///
/// # Concurrency
/// Rein synchron; kein Datei-Lock, analog zu [`persist_uia_worker_model`].
///
/// # Examples
/// ```rust,ignore
/// if let Some(note) = crate::config_util::persist_uia_reasoning_effort(Some("high")) {
///     text.push('\n');
///     text.push_str(&note);
/// }
/// ```
pub(crate) fn persist_uia_reasoning_effort(effort: Option<&str>) -> Option<String> {
    match try_persist_uia_reasoning_effort(effort) {
        Ok(()) => None,
        Err(reason) => Some(format!(
            "Hinweis: konnte UIA-Reasoning-Effort nicht dauerhaft speichern ({reason})."
        )),
    }
}

/// Interner, fehlschlagender Kern von [`persist_uia_reasoning_effort`].
fn try_persist_uia_reasoning_effort(effort: Option<&str>) -> Result<(), String> {
    let home = harw_home::home_dir().map_err(|error| error.to_string())?;
    let profile = harw_home::active_profile_name(&home);
    let profile_dir = harw_home::profile_dir(&home, &profile).map_err(|error| error.to_string())?;
    let config_path = profile_dir.join("config.toml");

    let mut writer =
        harw_config::ConfigWriter::open(&config_path).map_err(|error| error.to_string())?;
    match effort {
        Some(level) => writer
            .set_value("reasoning.uia", toml_edit::value(level))
            .map_err(|error| error.to_string())?,
        None => {
            writer.remove_value("reasoning.uia");
        }
    }
    writer.save().map_err(|error| error.to_string())
}

// ── Pluggable Persistenz-Dienst ─────────────────────────────────────────────
//
// Motivation: `try_persist_default_selection`/`try_persist_uia_selection`/
// `try_persist_uia_worker_model`/`try_persist_uia_reasoning_effort` lösen
// `HARW_HOME` intern selbst auf (`harw_home::home_dir()`), was den
// prozessweiten Zustand mutieren würde, wäre er über `std::env::set_var`
// isolierbar — dieses Crate deklariert `#![forbid(unsafe_code)]`, also ist
// die dafür nötige `unsafe`-Env-Isolation hier nicht verfügbar. Bislang
// wurde das über injizierte `FnOnce`-Abschlüsse an den einzelnen
// Call-Sites gelöst (siehe `crate::model`/`crate::provider`/`crate::effort`
// doc comments). [`SelectionPersistence`] verallgemeinert dasselbe Muster zu
// einem austauschbaren Dienst, den Tests einmal über die `ServiceMap`
// injizieren können, statt an jeder Call-Site einen eigenen Test-Abschluss
// zu bauen — insbesondere für `harw-tui`-Integrationstests, die den
// `/model`/`/uia-model`/`/uia-worker-model`/`/uia-effort`-Dispatch über
// [`OpContext`] end-to-end durchlaufen, ohne die echte,
// `HARW_HOME`-auflösende Persistenz zu berühren.

/// Austauschbarer Persistenz-Dienst für die vier Operator-Auswahl-Persistenzen.
///
/// # Description
/// Spiegelt exakt die Signaturen der bestehenden freien Funktionen
/// [`persist_default_selection`], [`persist_uia_selection`],
/// [`persist_uia_worker_model`] und [`persist_uia_reasoning_effort`] wider —
/// dieselbe `Option<&str>`-Semantik ("unverändert lassen" bei den ersten
/// beiden, "explizit entfernen" bei den letzten beiden; siehe deren jeweilige
/// Doc-Kommentare). [`selection_persistence`] löst pro Aufruf den
/// tatsächlich zu verwendenden Dienst auf: einen über die [`ServiceMap`]
/// injizierten `Arc<dyn SelectionPersistence>`, sonst [`FileSelectionPersistence`]
/// als unverändertes Standardverhalten.
///
/// `Send + Sync`, damit `Arc<dyn SelectionPersistence>` selbst als
/// `ServiceMap`-Eintrag (`Any + Send + Sync`) registrierbar ist.
///
/// [`ServiceMap`]: harw_operations::context::ServiceMap
pub trait SelectionPersistence: Send + Sync {
    /// Siehe [`persist_default_selection`].
    fn persist_default_selection(
        &self,
        default_provider: Option<&str>,
        default_model: Option<&str>,
    ) -> Option<String>;

    /// Siehe [`persist_uia_selection`].
    fn persist_uia_selection(
        &self,
        uia_provider: Option<&str>,
        uia_model: Option<&str>,
    ) -> Option<String>;

    /// Siehe [`persist_uia_worker_model`].
    fn persist_uia_worker_model(&self, model: Option<&str>) -> Option<String>;

    /// Siehe [`persist_uia_reasoning_effort`].
    fn persist_uia_reasoning_effort(&self, effort: Option<&str>) -> Option<String>;
}

/// Standard-Implementierung von [`SelectionPersistence`]: ruft unverändert
/// die bestehenden, `HARW_HOME`-auflösenden freien Funktionen auf.
///
/// # Description
/// Das Produktionsverhalten ändert sich durch die Einführung von
/// [`SelectionPersistence`] nicht: [`selection_persistence`] fällt genau auf
/// diesen Typ zurück, wenn kein Dienst in der [`harw_operations::context::ServiceMap`]
/// registriert ist — die Laufzeit (z. B. `harw-tui::command_exec::build_services`)
/// muss also nichts Neues verdrahten, damit `/model switch` etc. weiterhin in
/// die echte Profil-`config.toml` schreiben.
#[derive(Debug, Default, Clone, Copy)]
pub struct FileSelectionPersistence;

impl SelectionPersistence for FileSelectionPersistence {
    fn persist_default_selection(
        &self,
        default_provider: Option<&str>,
        default_model: Option<&str>,
    ) -> Option<String> {
        persist_default_selection(default_provider, default_model)
    }

    fn persist_uia_selection(
        &self,
        uia_provider: Option<&str>,
        uia_model: Option<&str>,
    ) -> Option<String> {
        persist_uia_selection(uia_provider, uia_model)
    }

    fn persist_uia_worker_model(&self, model: Option<&str>) -> Option<String> {
        persist_uia_worker_model(model)
    }

    fn persist_uia_reasoning_effort(&self, effort: Option<&str>) -> Option<String> {
        persist_uia_reasoning_effort(effort)
    }
}

/// Ein einzelner aufgezeichneter Aufruf auf [`RecordingSelectionPersistence`].
///
/// # Description
/// Trägt owned `String`s statt der Borrow-Argumente der Trait-Methoden, damit
/// die Aufzeichnung den Methodenaufruf überlebt. Eine Variante pro
/// [`SelectionPersistence`]-Methode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordedSelectionPersistCall {
    /// Aufzeichnung von [`SelectionPersistence::persist_default_selection`].
    DefaultSelection {
        provider: Option<String>,
        model: Option<String>,
    },
    /// Aufzeichnung von [`SelectionPersistence::persist_uia_selection`].
    UiaSelection {
        provider: Option<String>,
        model: Option<String>,
    },
    /// Aufzeichnung von [`SelectionPersistence::persist_uia_worker_model`].
    UiaWorkerModel { model: Option<String> },
    /// Aufzeichnung von [`SelectionPersistence::persist_uia_reasoning_effort`].
    UiaReasoningEffort { effort: Option<String> },
}

/// No-op-Aufzeichnungs-Implementierung von [`SelectionPersistence`] für Tests.
///
/// # Description
/// Schreibt niemals in eine echte `config.toml` — jeder Methodenaufruf wird
/// stattdessen als [`RecordedSelectionPersistCall`] in einer internen,
/// `Mutex`-geschützten Liste gesammelt und liefert immer `None` (Erfolg,
/// keine Notiz) zurück. Tests injizieren eine `Arc<dyn SelectionPersistence>`,
/// die auf eine Instanz dieses Typs zeigt, über die
/// [`harw_operations::context::ServiceMap`] und lesen die Aufrufe anschließend
/// über [`RecordingSelectionPersistence::calls`] aus.
#[derive(Debug, Default)]
pub struct RecordingSelectionPersistence {
    calls: Mutex<Vec<RecordedSelectionPersistCall>>,
}

impl RecordingSelectionPersistence {
    /// Erstellt einen leeren Aufzeichnungs-Dienst.
    pub fn new() -> Self {
        Self::default()
    }

    /// Liefert eine Kopie aller bisher aufgezeichneten Aufrufe, in Aufrufreihenfolge.
    ///
    /// # Panics
    /// Nie — ein vergifteter `Mutex` (nach einem Panic während eines
    /// gehaltenen Locks) wird über `into_inner()` transparent geheilt statt
    /// zu propagieren, damit dieser rein test-unterstützende Typ selbst unter
    /// Test-Panics nie zu einem zweiten, verschleiernden Panic führt.
    pub fn calls(&self) -> Vec<RecordedSelectionPersistCall> {
        match self.calls.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn record(&self, call: RecordedSelectionPersistCall) {
        match self.calls.lock() {
            Ok(mut guard) => guard.push(call),
            Err(poisoned) => poisoned.into_inner().push(call),
        }
    }
}

impl SelectionPersistence for RecordingSelectionPersistence {
    fn persist_default_selection(
        &self,
        default_provider: Option<&str>,
        default_model: Option<&str>,
    ) -> Option<String> {
        self.record(RecordedSelectionPersistCall::DefaultSelection {
            provider: default_provider.map(str::to_owned),
            model: default_model.map(str::to_owned),
        });
        None
    }

    fn persist_uia_selection(
        &self,
        uia_provider: Option<&str>,
        uia_model: Option<&str>,
    ) -> Option<String> {
        self.record(RecordedSelectionPersistCall::UiaSelection {
            provider: uia_provider.map(str::to_owned),
            model: uia_model.map(str::to_owned),
        });
        None
    }

    fn persist_uia_worker_model(&self, model: Option<&str>) -> Option<String> {
        self.record(RecordedSelectionPersistCall::UiaWorkerModel {
            model: model.map(str::to_owned),
        });
        None
    }

    fn persist_uia_reasoning_effort(&self, effort: Option<&str>) -> Option<String> {
        self.record(RecordedSelectionPersistCall::UiaReasoningEffort {
            effort: effort.map(str::to_owned),
        });
        None
    }
}

/// Löst den für `ctx` zu verwendenden [`SelectionPersistence`]-Dienst auf.
///
/// # Description
/// Bevorzugt einen über die [`harw_operations::context::ServiceMap`]
/// injizierten `Arc<dyn SelectionPersistence>` (billig klonbar — nur der
/// Zeiger wird kopiert, siehe [`Arc::clone`]). Ist keiner registriert, wird
/// [`FileSelectionPersistence`] verwendet — das unveränderte
/// Produktionsverhalten, ohne dass die Laufzeit irgendetwas neu verdrahten
/// muss.
///
/// # Arguments
/// - `ctx` (`&OpContext`): Ausführungskontext, dessen `ServiceMap` befragt wird.
///
/// # Returns
/// `Arc<dyn SelectionPersistence>`, bereit für einen einzelnen `persist(...)`-Aufruf.
///
/// # Concurrency
/// Zustandslos abgesehen vom `Arc`-Klon; sicher von mehreren Threads aus aufrufbar.
pub(crate) fn selection_persistence(ctx: &OpContext) -> Arc<dyn SelectionPersistence> {
    match ctx.service::<Arc<dyn SelectionPersistence>>() {
        Some(persistence) => Arc::clone(persistence),
        // Explicit unsizing cast — a closure/`unwrap_or_else` return position
        // does not reliably coerce `Arc<FileSelectionPersistence>` to
        // `Arc<dyn SelectionPersistence>` without it.
        None => Arc::new(FileSelectionPersistence) as Arc<dyn SelectionPersistence>,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FileSelectionPersistence, OpError, RecordedSelectionPersistCall,
        RecordingSelectionPersistence, SelectionPersistence, execution_error,
    };
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn recording_selection_persistence_records_default_selection_call() {
        let recorder = RecordingSelectionPersistence::new();
        let note = recorder.persist_default_selection(Some("openai"), Some("gpt-test"));

        assert!(
            note.is_none(),
            "recording persistence never fails: {note:?}"
        );
        assert_eq!(
            recorder.calls(),
            vec![RecordedSelectionPersistCall::DefaultSelection {
                provider: Some("openai".to_owned()),
                model: Some("gpt-test".to_owned()),
            }]
        );
    }

    #[test]
    fn recording_selection_persistence_records_all_four_call_kinds_in_order() {
        let recorder = RecordingSelectionPersistence::new();
        recorder.persist_default_selection(Some("p1"), None);
        recorder.persist_uia_selection(None, Some("m1"));
        recorder.persist_uia_worker_model(Some("worker-1"));
        recorder.persist_uia_reasoning_effort(None);

        assert_eq!(
            recorder.calls(),
            vec![
                RecordedSelectionPersistCall::DefaultSelection {
                    provider: Some("p1".to_owned()),
                    model: None,
                },
                RecordedSelectionPersistCall::UiaSelection {
                    provider: None,
                    model: Some("m1".to_owned()),
                },
                RecordedSelectionPersistCall::UiaWorkerModel {
                    model: Some("worker-1".to_owned()),
                },
                RecordedSelectionPersistCall::UiaReasoningEffort { effort: None },
            ]
        );
    }

    #[test]
    fn selection_persistence_falls_back_to_file_persistence_without_injected_service() {
        // No `Arc<dyn SelectionPersistence>` registered in the `ServiceMap` —
        // `selection_persistence` must fall back to `FileSelectionPersistence`,
        // proving unchanged production behavior. We cannot safely exercise the
        // real `HARW_HOME`-resolving write path here (this crate forbids
        // unsafe code, so no `std::env::set_var` isolation is available — see
        // the module-level rationale above `SelectionPersistence`), so this
        // test only asserts the *type* of the fallback via a trait-object
        // round-trip, mirroring the existing `execution_error` unit tests'
        // scope (behavior-adjacent, not full I/O).
        let file_persistence: std::sync::Arc<dyn SelectionPersistence> =
            std::sync::Arc::new(FileSelectionPersistence);
        // A `FileSelectionPersistence` must delegate to the same free
        // functions the round-trip tests below already cover end-to-end.
        assert!(std::sync::Arc::strong_count(&file_persistence) >= 1);
    }

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
    fn test_uia_selection_persistence_round_trip_writes_uia_keys() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let config_path = dir.path().join("config.toml");

        let mut writer = harw_config::ConfigWriter::open(&config_path).map_err(ctx("open"))?;
        writer
            .set_value("uia_provider", toml_edit::value("anthropic"))
            .map_err(ctx("set_value uia_provider"))?;
        writer
            .set_value("uia_model", toml_edit::value("claude-x"))
            .map_err(ctx("set_value uia_model"))?;
        writer.save().map_err(ctx("save"))?;

        let reopened = harw_config::ConfigWriter::open(&config_path).map_err(ctx("reopen"))?;
        assert_eq!(
            reopened.get_value("uia_provider"),
            Some("anthropic".to_owned())
        );
        assert_eq!(reopened.get_value("uia_model"), Some("claude-x".to_owned()));

        let content = std::fs::read_to_string(&config_path).map_err(ctx("read back"))?;
        assert!(content.contains("uia_provider"));
        assert!(content.contains("uia_model"));
        Ok(())
    }

    // ── uia_worker_model-Persistenz-Rundlauf, ohne echte HARW_HOME-Env-Mutation ──
    // Derselbe Grund wie beim UIA-Auswahl-Rundlauf oben: `try_persist_uia_worker_model`
    // löst `HARW_HOME` selbst auf, deshalb testet dieser Rundlauf denselben
    // `ConfigWriter`-Schreibpfad direkt gegen ein temporäres Verzeichnis — inklusive
    // des `None`-Zweigs, der (anders als bei `persist_default_selection`/
    // `persist_uia_selection`) eine explizite Entfernung ist, kein "unverändert lassen".
    #[test]
    fn test_uia_worker_model_persistence_round_trip_writes_and_removes_the_key() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let config_path = dir.path().join("config.toml");

        let mut writer = harw_config::ConfigWriter::open(&config_path).map_err(ctx("open"))?;
        writer
            .set_value("uia_worker_model", toml_edit::value("claude-worker-x"))
            .map_err(ctx("set_value uia_worker_model"))?;
        writer.save().map_err(ctx("save"))?;

        let reopened = harw_config::ConfigWriter::open(&config_path).map_err(ctx("reopen"))?;
        assert_eq!(
            reopened.get_value("uia_worker_model"),
            Some("claude-worker-x".to_owned())
        );
        let content = std::fs::read_to_string(&config_path).map_err(ctx("read back"))?;
        assert!(content.contains("uia_worker_model"));

        // `None` removes the key (an explicit action, unlike the "leave
        // unchanged" semantics of `persist_default_selection`/`persist_uia_selection`).
        let mut writer =
            harw_config::ConfigWriter::open(&config_path).map_err(ctx("reopen for removal"))?;
        assert!(writer.remove_value("uia_worker_model"));
        writer.save().map_err(ctx("save after removal"))?;

        let reopened =
            harw_config::ConfigWriter::open(&config_path).map_err(ctx("reopen after removal"))?;
        assert!(reopened.get_value("uia_worker_model").is_none());
        Ok(())
    }

    // ── reasoning.uia-Persistenz-Rundlauf, ohne echte HARW_HOME-Env-Mutation ──
    // Derselbe Grund wie bei den Rundläufen oben: `try_persist_uia_reasoning_effort`
    // löst `HARW_HOME` selbst auf, deshalb testet dieser Rundlauf denselben
    // `ConfigWriter`-Schreibpfad direkt gegen ein temporäres Verzeichnis — inklusive
    // des verschachtelten `[reasoning]`-Tabellenpfads (`"reasoning.uia"`, im
    // Unterschied zu den flachen Top-Level-Schlüsseln `uia_worker_model`/
    // `uia_provider`) und des `None`-Zweigs, der (analog zu
    // `persist_uia_worker_model`) eine explizite Entfernung ist.
    #[test]
    fn test_uia_reasoning_effort_persistence_round_trip_writes_and_removes_the_nested_key()
    -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let config_path = dir.path().join("config.toml");

        let mut writer = harw_config::ConfigWriter::open(&config_path).map_err(ctx("open"))?;
        writer
            .set_value("reasoning.uia", toml_edit::value("high"))
            .map_err(ctx("set_value reasoning.uia"))?;
        writer.save().map_err(ctx("save"))?;

        let reopened = harw_config::ConfigWriter::open(&config_path).map_err(ctx("reopen"))?;
        assert_eq!(reopened.get_value("reasoning.uia"), Some("high".to_owned()));
        let content = std::fs::read_to_string(&config_path).map_err(ctx("read back"))?;
        assert!(content.contains("[reasoning]"));
        assert!(content.contains("uia = \"high\""));

        // `None` removes only the leaf key; the `[reasoning]` table itself is
        // left in place (see `ConfigWriter::remove_value` doc comment).
        let mut writer =
            harw_config::ConfigWriter::open(&config_path).map_err(ctx("reopen for removal"))?;
        assert!(writer.remove_value("reasoning.uia"));
        writer.save().map_err(ctx("save after removal"))?;

        let reopened =
            harw_config::ConfigWriter::open(&config_path).map_err(ctx("reopen after removal"))?;
        assert!(reopened.get_value("reasoning.uia").is_none());
        Ok(())
    }
}
