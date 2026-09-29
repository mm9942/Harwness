//! Wiederverwendbare Config-Discovery-Helfer für ops-Handler.
//!
//! Bündelt das Muster "Config-Layer + discover_config", das in mehreren Ops
//! (model, provider, …) mehrfach vorkam, und bindet es an den Root-Space der
//! Sitzung.
//!
//! # Verantwortungsbereich
//! Stellt [`bound_home`] bereit: den an die Sitzung gebundenen Root-Space
//! (`Arc<harw_home::ResolvedHomeContext>`, von der Laufzeit in jede
//! [`harw_operations::context::ServiceMap`] eingetragen). Fehlt die Bindung,
//! antwortet er mit [`OpError::NotAvailable`] — bewusst ohne Rückfall auf
//! `HARW_HOME` oder `~/.harw`. Darauf bauen [`bound_config_layers`] und
//! [`load_bound_config`] auf, die Config-Discovery der Ops. Der ältere
//! [`load_default_config`] liest den Root-Space des Prozesses und bleibt nur
//! für `crate::permissions`. Stellt außerdem
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
//! Für `/models` und `/mode default` kommen [`persist_internal_model`]
//! (`[internal_models.<stelle>]`, inkl. Orchestrator/Sub-Orchestrator),
//! [`clear_uia_selection`] (entfernt `uia_provider`/`uia_model`) und
//! [`persist_default_interaction_mode`] (`[mode] default`) hinzu, für
//! `/agent use` außerdem [`persist_active_agent`]
//! (`active_agent_definition`) — alle ebenfalls bestes Bemühen und erst ab
//! der nächsten Sitzung wirksam. Alle freien `persist_*`-/`clear_*`-Funktionen
//! bekommen das Profilverzeichnis ausdrücklich übergeben und lösen selbst
//! keinen Root-Space auf.
//!
//! Zusätzlich stellt dieses Modul [`SelectionPersistence`] bereit: einen
//! austauschbaren Dienst-Trait, der die `persist_*`-/`clear_*`-Funktionen hinter
//! einer gemeinsamen Schnittstelle bündelt, plus [`FileSelectionPersistence`]
//! (schreibt in die `config.toml` eines festen Profilverzeichnisses) und
//! [`RecordingSelectionPersistence`] (No-op-Aufzeichnung für Tests). Die
//! Operationen (`crate::model`, `crate::effort`, …) holen den Dienst über
//! [`selection_persistence`]: ein injizierter Dienst gewinnt, sonst schreibt
//! [`FileSelectionPersistence`] in das Profil des gebundenen Root-Space. Ohne
//! Bindung schlägt jede Persistenz geschlossen fehl und meldet das als Notiz.
//!
//! # Exportierte Typen
//! [`SelectionPersistence`], [`FileSelectionPersistence`],
//! [`RecordingSelectionPersistence`], [`RecordedSelectionPersistCall`] sind
//! `pub` (dieses Modul selbst ist `pub(crate)` — siehe `crate::model`'s
//! `pub use crate::config_util::{...}`-Re-Export für den öffentlichen Pfad,
//! über den `harw-tui`-Tests sie erreichen). Von außen lässt sich
//! [`FileSelectionPersistence`] nur über [`FileSelectionPersistence::for_home`]
//! bauen; ihr Konstruktor `new` ist `pub(crate)`. Alle anderen Items bleiben
//! `pub(crate)`.
//!
//! # Nebenläufigkeit
//! Die freien `persist_*`-Funktionen, [`bound_home`], [`load_bound_config`]
//! und [`load_default_config`] sind zustandslos. [`FileSelectionPersistence`]
//! hält nur einen unveränderlichen Pfad. [`RecordingSelectionPersistence`]
//! hält intern einen `Mutex<Vec<_>>` und ist damit `Send + Sync` — sicher von
//! mehreren Threads aus aufrufbar.
//!
//! # Fehlertypen
//! - [`harw_operations::OpError::NotAvailable`]: [`bound_home`]/
//!   [`load_bound_config`], wenn an die Sitzung kein Root-Space gebunden ist.
//! - [`harw_operations::OpError::Execution`]: wenn die Config-Discovery fehlschlägt.
//! - [`persist_default_selection`]/[`persist_uia_selection`]/
//!   [`persist_uia_worker_model`]/[`persist_uia_reasoning_effort`]/
//!   [`persist_internal_model`]/[`clear_uia_selection`]/
//!   [`persist_default_interaction_mode`]/[`persist_active_agent`] (und ihre
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
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Lädt die Config aus dem Root-Space des **Prozesses** (`HARW_HOME` bzw.
/// `~/.harw`) und dessen Layern.
///
/// # Description
/// Ruft [`harw_home::home_dir`] und anschließend
/// [`harw_home::config_layers`] auf, damit das aktive Profil wie beim CLI-Chat
/// berücksichtigt wird. Die resultierenden Layer werden an
/// [`harw_config::discover_config`] übergeben. Fehler werden in
/// [`OpError::Execution`] mit einem optionalen Kontext-Präfix umgewandelt.
///
/// Liest bewusst **nicht** den an die Sitzung gebundenen Root-Space. Nur
/// `crate::permissions` nutzt diesen Pfad noch; Operationen verwenden
/// [`load_bound_config`], das den gebundenen
/// `harw_home::ResolvedHomeContext` liest und ohne Bindung geschlossen
/// fehlschlägt.
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

/// Grund, mit dem Operationen ohne gebundenen Root-Space geschlossen
/// fehlschlagen (als [`OpError::NotAvailable`] oder als Persistenz-Notiz).
const UNBOUND_HOME: &str =
    "an diese Sitzung ist kein Root-Space gebunden (harw_home::ResolvedHomeContext fehlt)";

/// Liefert den an die Sitzung gebundenen Root-Space.
///
/// # Description
/// Liest den `Arc<harw_home::ResolvedHomeContext>`, den die Laufzeit auf
/// jeder Oberfläche in die [`harw_operations::context::ServiceMap`] einträgt.
/// Es gibt bewusst **keinen** Rückfall auf `HARW_HOME` oder `~/.harw`: eine
/// Sitzung, die mit einem anderen Root-Space gestartet wurde (z. B. über
/// `--home`), darf nie still in den Root-Space des Prozesses lesen oder
/// schreiben.
///
/// # Arguments
/// - `ctx` (`&OpContext`): Ausführungskontext, dessen `ServiceMap` befragt wird.
///
/// # Returns
/// Den gebundenen [`harw_home::ResolvedHomeContext`], geliehen aus `ctx`.
///
/// # Errors
/// - [`OpError::NotAvailable`]: wenn kein Root-Space gebunden ist.
///
/// # Panics
/// Nie.
///
/// # Examples
/// ```rust,ignore
/// // Called from within harw-ops — module is pub(crate).
/// let home = crate::config_util::bound_home(ctx)?;
/// let memories = home.profile_dir.join("memories");
/// ```
pub(crate) fn bound_home(ctx: &OpContext) -> Result<&harw_home::ResolvedHomeContext, OpError> {
    ctx.service::<Arc<harw_home::ResolvedHomeContext>>()
        .map(Arc::as_ref)
        .ok_or_else(|| OpError::NotAvailable(UNBOUND_HOME.to_owned()))
}

/// Baut die Config-Layer des gebundenen Root-Space in aufsteigender Präzedenz.
///
/// # Description
/// Root-Space und Profilverzeichnis kommen aus der Bindung selbst, nicht aus
/// [`harw_home::config_layers_report_at`]: dessen Profilwahl folgt
/// `HARW_PROFILE`/`active_profile` des Prozesses. Aus dem Bericht wird nur
/// der vertraute repo-lokale Layer (`<projekt>/.harw`, falls freigegeben)
/// übernommen.
///
/// # Arguments
/// - `home` (`&harw_home::ResolvedHomeContext`): der gebundene Root-Space.
///
/// # Returns
/// `[root, profil, (repo)]` — nicht existente Layer überspringt
/// [`harw_config::discover_config`] selbst.
///
/// # Errors
/// Wie [`harw_home::config_layers_report_at`] (Trust-Store unlesbar,
/// Projekt-Root nicht kanonisierbar, ungültiger Profilname).
pub(crate) fn bound_config_layers(
    home: &harw_home::ResolvedHomeContext,
) -> Result<Vec<PathBuf>, harw_home::HomeError> {
    let report = harw_home::config_layers_report_at(&home.home, &home.project.root)?;
    let mut layers = vec![home.home.clone(), home.profile_dir.clone()];
    layers.extend(report.layers.into_iter().skip(2));
    Ok(layers)
}

/// Lädt die Config aus dem an die Sitzung gebundenen Root-Space.
///
/// # Description
/// [`bound_home`], dann [`bound_config_layers`], dann
/// [`harw_config::discover_config`]. Das ist die Config-Discovery der
/// Operationen; [`load_default_config`] bleibt dem Prozess-Root-Space
/// vorbehalten.
///
/// # Arguments
/// - `ctx` (`&OpContext`): Ausführungskontext mit gebundenem Root-Space.
/// - `context` (`&str`): Präfix für Ausführungsfehler, wie bei
///   [`load_default_config`].
///
/// # Returns
/// Die aufgelöste [`ResolvedConfig`].
///
/// # Errors
/// - [`OpError::NotAvailable`]: kein Root-Space gebunden.
/// - [`OpError::Execution`]: Layer-Ermittlung oder
///   [`harw_config::discover_config`] schlägt fehl (`"{context}: {e}"`).
///
/// # Panics
/// Nie.
///
/// # Examples
/// ```rust,ignore
/// // Called from within harw-ops — module is pub(crate).
/// let config = crate::config_util::load_bound_config(ctx, "Config-Discovery fehlgeschlagen")?;
/// ```
pub(crate) fn load_bound_config(ctx: &OpContext, context: &str) -> Result<ResolvedConfig, OpError> {
    let home = bound_home(ctx)?;
    let layers = bound_config_layers(home).map_err(|error| execution_error(context, error))?;
    harw_config::discover_config(&layers).map_err(|error| execution_error(context, error))
}

/// Verankert `default_provider`/`default_model` bestes Bemühen in der
/// Profil-`config.toml` (`<profile_dir>/config.toml`, im Betrieb das Profil
/// des gebundenen Root-Space) — denselben Datei- und Schlüsselnamen, die
/// `harw onboard` beim Ersteinstieg schreibt (siehe
/// `harw-cli/src/onboarding.rs`).
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
/// (Schreibfehler, Validierung) wird stattdessen als deutschsprachige Notiz
/// zurückgegeben, die der Aufrufer an die Operator-Ausgabe anhängen kann.
/// Den Root-Space löst die Funktion nicht selbst auf.
///
/// # Arguments
/// - `profile_dir` (`&Path`): Profilverzeichnis, dessen `config.toml`
///   geschrieben wird.
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
/// let home = crate::config_util::bound_home(ctx)?;
/// if let Some(note) =
///     crate::config_util::persist_default_selection(&home.profile_dir, Some("anthropic"), None)
/// {
///     text.push('\n');
///     text.push_str(&note);
/// }
/// ```
pub(crate) fn persist_default_selection(
    profile_dir: &Path,
    default_provider: Option<&str>,
    default_model: Option<&str>,
) -> Option<String> {
    if default_provider.is_none() && default_model.is_none() {
        return None;
    }
    match try_persist_default_selection(profile_dir, default_provider, default_model) {
        Ok(()) => None,
        Err(reason) => Some(format!(
            "Hinweis: konnte nicht dauerhaft als Standard für künftige Sitzungen gespeichert werden ({reason})."
        )),
    }
}

/// Interner, fehlschlagender Kern von [`persist_default_selection`].
fn try_persist_default_selection(
    profile_dir: &Path,
    default_provider: Option<&str>,
    default_model: Option<&str>,
) -> Result<(), String> {
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
/// - `profile_dir` (`&Path`): Profilverzeichnis, dessen `config.toml`
///   geschrieben wird.
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
/// let home = crate::config_util::bound_home(ctx)?;
/// if let Some(note) =
///     crate::config_util::persist_uia_selection(&home.profile_dir, Some("anthropic"), None)
/// {
///     text.push('\n');
///     text.push_str(&note);
/// }
/// ```
pub(crate) fn persist_uia_selection(
    profile_dir: &Path,
    uia_provider: Option<&str>,
    uia_model: Option<&str>,
) -> Option<String> {
    if uia_provider.is_none() && uia_model.is_none() {
        return None;
    }
    match try_persist_uia_selection(profile_dir, uia_provider, uia_model) {
        Ok(()) => None,
        Err(reason) => Some(format!(
            "Hinweis: konnte UIA-Auswahl nicht dauerhaft speichern ({reason})."
        )),
    }
}

/// Interner, fehlschlagender Kern von [`persist_uia_selection`].
fn try_persist_uia_selection(
    profile_dir: &Path,
    uia_provider: Option<&str>,
    uia_model: Option<&str>,
) -> Result<(), String> {
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
/// - `profile_dir` (`&Path`): Profilverzeichnis, dessen `config.toml`
///   geschrieben wird.
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
/// let home = crate::config_util::bound_home(ctx)?;
/// if let Some(note) =
///     crate::config_util::persist_uia_worker_model(&home.profile_dir, Some("claude-worker-x"))
/// {
///     text.push('\n');
///     text.push_str(&note);
/// }
/// ```
pub(crate) fn persist_uia_worker_model(profile_dir: &Path, model: Option<&str>) -> Option<String> {
    match try_persist_uia_worker_model(profile_dir, model) {
        Ok(()) => None,
        Err(reason) => Some(format!(
            "Hinweis: konnte UIA-Worker-Modell nicht dauerhaft speichern ({reason})."
        )),
    }
}

/// Interner, fehlschlagender Kern von [`persist_uia_worker_model`].
fn try_persist_uia_worker_model(profile_dir: &Path, model: Option<&str>) -> Result<(), String> {
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

/// Runde 5, Teil G: verankert die Modellwahl **einer** UIA-Worker-Rolle
/// (`[uia_worker_models] <rolle> = "uia" | "provider/modell"`) bestes
/// Bemühen in der Profil-`config.toml`.
///
/// # Description
/// `value = None` entfernt den Eintrag der Rolle (dann greift der alte
/// `uia_worker_model`-Pin bzw. die Vorgabe „wie UIA“). Geschrieben wird
/// `<profile_dir>/config.toml`. **Niemals fehlschlagend** für den Aufrufer.
///
/// # Returns
/// `None` bei Erfolg; `Some(note)` bei unbekannter Rolle oder
/// Persistenzfehler.
pub(crate) fn persist_uia_worker_role_model(
    profile_dir: &Path,
    role: &str,
    value: Option<&str>,
) -> Option<String> {
    let result = open_profile_config_writer(profile_dir).and_then(|mut writer| {
        write_uia_worker_role_model(&mut writer, role, value)?;
        writer.save().map_err(|error| error.to_string())
    });
    match result {
        Ok(()) => None,
        Err(reason) => Some(format!(
            "Hinweis: konnte das Modell für „{role}“ nicht dauerhaft speichern ({reason})."
        )),
    }
}

/// Reiner Schreibkern von [`persist_uia_worker_role_model`] (ohne `save`).
///
/// # Errors
/// Unbekannte Rolle oder Fehler von [`harw_config::ConfigWriter::set_value`].
fn write_uia_worker_role_model(
    writer: &mut harw_config::ConfigWriter,
    role: &str,
    value: Option<&str>,
) -> Result<(), String> {
    let key = harw_config::UiaWorkerModelsToml::toml_key(role)
        .ok_or_else(|| format!("unbekannte UIA-Worker-Rolle „{role}“"))?;
    let path = format!("uia_worker_models.{key}");
    match value {
        Some(value) => writer
            .set_value(&path, toml_edit::value(value))
            .map_err(|error| error.to_string()),
        None => {
            writer.remove_value(&path);
            Ok(())
        }
    }
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
/// - `profile_dir` (`&Path`): Profilverzeichnis, dessen `config.toml`
///   geschrieben wird.
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
/// let home = crate::config_util::bound_home(ctx)?;
/// if let Some(note) =
///     crate::config_util::persist_uia_reasoning_effort(&home.profile_dir, Some("high"))
/// {
///     text.push('\n');
///     text.push_str(&note);
/// }
/// ```
pub(crate) fn persist_uia_reasoning_effort(
    profile_dir: &Path,
    effort: Option<&str>,
) -> Option<String> {
    match try_persist_uia_reasoning_effort(profile_dir, effort) {
        Ok(()) => None,
        Err(reason) => Some(format!(
            "Hinweis: konnte UIA-Reasoning-Effort nicht dauerhaft speichern ({reason})."
        )),
    }
}

/// Interner, fehlschlagender Kern von [`persist_uia_reasoning_effort`].
fn try_persist_uia_reasoning_effort(
    profile_dir: &Path,
    effort: Option<&str>,
) -> Result<(), String> {
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

/// Öffnet `<profile_dir>/config.toml` über [`harw_config::ConfigWriter`].
///
/// # Errors
/// Menschenlesbarer Grund als `String`, wenn die Datei nicht geöffnet werden
/// kann.
fn open_profile_config_writer(profile_dir: &Path) -> Result<harw_config::ConfigWriter, String> {
    harw_config::ConfigWriter::open(&profile_dir.join("config.toml"))
        .map_err(|error| error.to_string())
}

/// Verankert die Modellwahl einer internen Modellstelle
/// (`[internal_models.<stelle>]`) bestes Bemühen in der Profil-`config.toml`.
///
/// # Description
/// Struktureller Zwilling von [`persist_uia_worker_model`] für die
/// `internal_models`-Punkte (inklusive Orchestrator/Sub-Orchestrator). Die
/// Semantik der Argumente:
///
/// | `provider` | `model` | Wirkung |
/// |---|---|---|
/// | `None` | `None` | Entfernt `[internal_models.<stelle>]` vollständig („zurücksetzen“ — die Stelle fällt wieder auf ihren Standard zurück) |
/// | `Some(p)` | `Some(m)` | Setzt `provider = p` und `model = m` |
/// | `Some(p)` | `None` | Setzt `provider = p` und entfernt `model` (explizite Wahl ohne Modell ⇒ Hauptmodell, siehe [`harw_config::resolve_internal_model`]) |
/// | `None` | `Some(m)` | Setzt `model = m` und entfernt `provider` |
///
/// Wirkt erst ab der nächsten Sitzung. **Niemals fehlschlagend** für den
/// Aufrufer — Persistenzfehler werden als deutschsprachige Notiz
/// zurückgegeben statt propagiert.
///
/// # Arguments
/// - `profile_dir` (`&Path`): Profilverzeichnis, dessen `config.toml`
///   geschrieben wird.
/// - `point` ([`harw_config::InternalModelPoint`]): die Stelle; ihr
///   [`harw_config::InternalModelPoint::key`] bildet den TOML-Pfad.
/// - `provider` (`Option<&str>`): kanonischer Provider-Name.
/// - `model` (`Option<&str>`): kanonische Modell-ID.
///
/// # Returns
/// `None` bei Erfolg; `Some(note)` bei einem Persistenzfehler.
///
/// # Panics
/// Nie.
///
/// # Concurrency
/// Rein synchron; kein Datei-Lock, analog zu [`persist_uia_worker_model`].
pub(crate) fn persist_internal_model(
    profile_dir: &Path,
    point: harw_config::InternalModelPoint,
    provider: Option<&str>,
    model: Option<&str>,
) -> Option<String> {
    let result = open_profile_config_writer(profile_dir).and_then(|mut writer| {
        write_internal_model(&mut writer, point, provider, model)?;
        writer.save().map_err(|error| error.to_string())
    });
    match result {
        Ok(()) => None,
        Err(reason) => Some(format!(
            "Hinweis: konnte das Modell für „{}“ nicht dauerhaft speichern ({reason}).",
            point.key()
        )),
    }
}

/// Reiner Schreibkern von [`persist_internal_model`] auf einem bereits
/// geöffneten [`harw_config::ConfigWriter`] (ohne `save`) — getrennt, damit
/// Tests ihn direkt gegen eine temporäre Datei prüfen können.
///
/// # Errors
/// Menschenlesbarer Grund, wenn [`harw_config::ConfigWriter::set_value`]
/// scheitert.
fn write_internal_model(
    writer: &mut harw_config::ConfigWriter,
    point: harw_config::InternalModelPoint,
    provider: Option<&str>,
    model: Option<&str>,
) -> Result<(), String> {
    let base = format!("internal_models.{}", point.key());
    if provider.is_none() && model.is_none() {
        writer.remove_value(&base);
        return Ok(());
    }
    let provider_key = format!("{base}.provider");
    let model_key = format!("{base}.model");
    match provider {
        Some(provider) => writer
            .set_value(&provider_key, toml_edit::value(provider))
            .map_err(|error| error.to_string())?,
        None => {
            writer.remove_value(&provider_key);
        }
    }
    match model {
        Some(model) => writer
            .set_value(&model_key, toml_edit::value(model))
            .map_err(|error| error.to_string())?,
        None => {
            writer.remove_value(&model_key);
        }
    }
    Ok(())
}

/// Entfernt die UIA-Auswahl (`uia_provider` **und** `uia_model`) bestes
/// Bemühen aus der Profil-`config.toml`.
///
/// # Description
/// Gegenstück zu [`persist_uia_selection`] für `/models reset uia`: danach
/// fällt die UIA wieder auf `default_provider`/`default_model` zurück. Wirkt
/// erst ab der nächsten Sitzung; eine bereits laufende Live-Auswahl bleibt
/// unberührt. **Niemals fehlschlagend** für den Aufrufer.
///
/// # Arguments
/// - `profile_dir` (`&Path`): Profilverzeichnis, dessen `config.toml`
///   geschrieben wird.
///
/// # Returns
/// `None` bei Erfolg; `Some(note)` bei einem Persistenzfehler.
///
/// # Panics
/// Nie.
///
/// # Concurrency
/// Rein synchron; kein Datei-Lock.
pub(crate) fn clear_uia_selection(profile_dir: &Path) -> Option<String> {
    let result = open_profile_config_writer(profile_dir).and_then(|mut writer| {
        clear_uia_keys(&mut writer);
        writer.save().map_err(|error| error.to_string())
    });
    match result {
        Ok(()) => None,
        Err(reason) => Some(format!(
            "Hinweis: konnte die UIA-Auswahl nicht dauerhaft zurücksetzen ({reason})."
        )),
    }
}

/// Reiner Schreibkern von [`clear_uia_selection`] (ohne `save`).
fn clear_uia_keys(writer: &mut harw_config::ConfigWriter) {
    writer.remove_value("uia_provider");
    writer.remove_value("uia_model");
}

/// Verankert den Standard-Interaktionsmodus (`[mode] default`) bestes
/// Bemühen in der Profil-`config.toml`.
///
/// # Description
/// Wird von `/mode default <modus>` nach erfolgreicher Validierung über
/// `harw_core::InteractionMode::parse` aufgerufen; `mode` ist daher bereits
/// der kanonische Name (`chat|plan|explore|work|shell`). Wirkt erst für neue
/// Sitzungen; der Modus der laufenden Sitzung bleibt unberührt. **Niemals
/// fehlschlagend** für den Aufrufer.
///
/// # Arguments
/// - `profile_dir` (`&Path`): Profilverzeichnis, dessen `config.toml`
///   geschrieben wird.
/// - `mode` (`&str`): kanonischer Modusname.
///
/// # Returns
/// `None` bei Erfolg; `Some(note)` bei einem Persistenzfehler.
///
/// # Panics
/// Nie.
///
/// # Concurrency
/// Rein synchron; kein Datei-Lock.
pub(crate) fn persist_default_interaction_mode(profile_dir: &Path, mode: &str) -> Option<String> {
    let result = open_profile_config_writer(profile_dir).and_then(|mut writer| {
        write_default_interaction_mode(&mut writer, mode)?;
        writer.save().map_err(|error| error.to_string())
    });
    match result {
        Ok(()) => None,
        Err(reason) => Some(format!(
            "Hinweis: konnte den Standardmodus nicht dauerhaft speichern ({reason})."
        )),
    }
}

/// Reiner Schreibkern von [`persist_default_interaction_mode`] (ohne `save`).
///
/// # Errors
/// Menschenlesbarer Grund, wenn [`harw_config::ConfigWriter::set_value`]
/// scheitert.
fn write_default_interaction_mode(
    writer: &mut harw_config::ConfigWriter,
    mode: &str,
) -> Result<(), String> {
    writer
        .set_value("mode.default", toml_edit::value(mode))
        .map_err(|error| error.to_string())
}

/// Verankert die Wurzel-Agentendefinition (`active_agent_definition`)
/// bestes Bemühen in der Profil-`config.toml` oder entfernt sie.
///
/// # Description
/// Wird von `/agent use <name>` (nach Validierung des Namens) bzw.
/// `/agent use --clear` aufgerufen. `Some(name)` setzt den Top-Level-Schlüssel
/// `active_agent_definition` (Feld `harness.active_agent_definition`),
/// `None` entfernt ihn — dann entscheidet wieder die UIA bzw. die Vorgabe.
/// Wirkt erst ab der nächsten Sitzung; die laufende Sitzung bleibt
/// unberührt. Ein `--agent` beim Start gewinnt weiterhin über diesen Wert.
/// **Niemals fehlschlagend** für den Aufrufer.
///
/// # Arguments
/// - `profile_dir` (`&Path`): Profilverzeichnis, dessen `config.toml`
///   geschrieben wird.
/// - `name` (`Option<&str>`): Agentenname oder `None` zum Entfernen.
///
/// # Returns
/// `None` bei Erfolg; `Some(note)` bei einem Persistenzfehler.
///
/// # Panics
/// Nie.
///
/// # Concurrency
/// Rein synchron; kein Datei-Lock.
pub(crate) fn persist_active_agent(profile_dir: &Path, name: Option<&str>) -> Option<String> {
    let result = open_profile_config_writer(profile_dir).and_then(|mut writer| {
        write_active_agent(&mut writer, name)?;
        writer.save().map_err(|error| error.to_string())
    });
    match result {
        Ok(()) => None,
        Err(reason) => Some(format!(
            "Hinweis: konnte den aktiven Agenten nicht dauerhaft speichern ({reason})."
        )),
    }
}

/// Reiner Schreibkern von [`persist_active_agent`] (ohne `save`).
///
/// # Errors
/// Menschenlesbarer Grund, wenn [`harw_config::ConfigWriter::set_value`]
/// scheitert.
fn write_active_agent(
    writer: &mut harw_config::ConfigWriter,
    name: Option<&str>,
) -> Result<(), String> {
    match name {
        Some(name) => writer
            .set_value("active_agent_definition", toml_edit::value(name))
            .map_err(|error| error.to_string()),
        None => {
            writer.remove_value("active_agent_definition");
            Ok(())
        }
    }
}

// ── Pluggable Persistenz-Dienst ─────────────────────────────────────────────
//
// Die freien `persist_*`-/`clear_*`-Funktionen schreiben in ein übergebenes
// Profilverzeichnis. Welches das ist, entscheidet [`selection_persistence`]
// pro Aufruf aus dem [`OpContext`]: ein injizierter Dienst gewinnt, sonst
// schreibt [`FileSelectionPersistence`] in das Profil des an die Sitzung
// gebundenen Root-Space (`harw_home::ResolvedHomeContext`). Ohne Bindung
// schlägt die Persistenz geschlossen fehl (`UnboundSelectionPersistence`):
// jede Methode meldet eine Notiz und schreibt nichts, auch nicht nach
// `HARW_HOME` oder `~/.harw`.
//
// Tests injizieren einmal einen Dienst über die `ServiceMap`, statt an jeder
// Call-Site einen eigenen Abschluss zu bauen — insbesondere
// `harw-tui`-Integrationstests, die den
// `/model`/`/uia-model`/`/uia-worker-model`/`/uia-effort`-Dispatch über
// [`OpContext`] end-to-end durchlaufen, ohne eine echte `config.toml` zu
// berühren.

/// Austauschbarer Persistenz-Dienst für die Operator-Auswahl-Persistenzen (Default, UIA, UIA-Worker, UIA-Effort, interne Modellstellen).
///
/// # Description
/// Spiegelt die Signaturen der freien Funktionen
/// [`persist_default_selection`], [`persist_uia_selection`],
/// [`persist_uia_worker_model`] und [`persist_uia_reasoning_effort`] wider,
/// ohne deren `profile_dir`-Argument (das Ziel hält die Implementierung) —
/// dieselbe `Option<&str>`-Semantik ("unverändert lassen" bei den ersten
/// beiden, "explizit entfernen" bei den letzten beiden; siehe deren jeweilige
/// Doc-Kommentare). [`selection_persistence`] löst pro Aufruf den
/// tatsächlich zu verwendenden Dienst auf: einen über die [`ServiceMap`]
/// injizierten `Arc<dyn SelectionPersistence>`, sonst
/// [`FileSelectionPersistence`] für das Profil des gebundenen Root-Space;
/// ohne Bindung eine Implementierung, die geschlossen fehlschlägt.
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

    /// Siehe [`persist_internal_model`].
    fn persist_internal_model(
        &self,
        point: harw_config::InternalModelPoint,
        provider: Option<&str>,
        model: Option<&str>,
    ) -> Option<String>;

    /// Siehe [`clear_uia_selection`].
    fn clear_uia_selection(&self) -> Option<String>;

    /// Siehe [`persist_active_agent`].
    fn persist_active_agent(&self, name: Option<&str>) -> Option<String>;

    /// Runde 5, Teil G: siehe [`persist_uia_worker_role_model`].
    fn persist_uia_worker_role_model(&self, role: &str, value: Option<&str>) -> Option<String>;

    /// Siehe [`persist_default_interaction_mode`] (`/mode default <modus>`).
    ///
    /// # Beschreibung
    /// Mit Standard-Implementierung, damit bestehende Implementierungen
    /// unverändert bleiben; über den Dienst läuft der Aufruf, damit
    /// [`crate::live_config::MirroringSelectionPersistence`] ihn im
    /// Live-Schnappschuss spiegeln kann. Die Standard-Implementierung kennt
    /// kein Profilverzeichnis und schlägt deshalb geschlossen fehl: sie
    /// schreibt nichts und meldet das als Notiz. Implementierungen mit einem
    /// Ziel (etwa [`FileSelectionPersistence`]) überschreiben sie.
    fn persist_default_interaction_mode(&self, _mode: &str) -> Option<String> {
        Some(format!(
            "Hinweis: konnte den Standardmodus nicht dauerhaft speichern ({UNBOUND_HOME})."
        ))
    }
}

/// Datei-Implementierung von [`SelectionPersistence`]: schreibt über die
/// freien Funktionen in `<profile_dir>/config.toml`.
///
/// # Description
/// Das Profilverzeichnis ist fest an die Instanz gebunden. Im Betrieb baut
/// [`selection_persistence`] sie über [`FileSelectionPersistence::for_home`]
/// aus dem an die Sitzung gebundenen Root-Space
/// (`harw_home::ResolvedHomeContext`), wenn kein Dienst in der
/// [`harw_operations::context::ServiceMap`] registriert ist. Der Typ löst
/// selbst nie `HARW_HOME` oder `~/.harw` auf; ohne gebundenen Root-Space
/// entsteht keine Instanz, die Persistenz schlägt dann geschlossen fehl.
#[derive(Debug, Clone)]
pub struct FileSelectionPersistence {
    profile_dir: PathBuf,
}

impl FileSelectionPersistence {
    /// Schreibt künftig in `<profile_dir>/config.toml`.
    ///
    /// Nur crate-intern (Tests, [`Self::for_home`]); von außen entsteht eine
    /// Instanz nur aus einem gebundenen Root-Space.
    #[must_use]
    pub(crate) fn new(profile_dir: PathBuf) -> Self {
        Self { profile_dir }
    }

    /// Schreibt in das Profil des gebundenen Root-Space
    /// (`home.profile_dir`).
    #[must_use]
    pub fn for_home(home: &harw_home::ResolvedHomeContext) -> Self {
        Self::new(home.profile_dir.clone())
    }
}

impl SelectionPersistence for FileSelectionPersistence {
    fn persist_default_selection(
        &self,
        default_provider: Option<&str>,
        default_model: Option<&str>,
    ) -> Option<String> {
        persist_default_selection(&self.profile_dir, default_provider, default_model)
    }

    fn persist_uia_selection(
        &self,
        uia_provider: Option<&str>,
        uia_model: Option<&str>,
    ) -> Option<String> {
        persist_uia_selection(&self.profile_dir, uia_provider, uia_model)
    }

    fn persist_uia_worker_model(&self, model: Option<&str>) -> Option<String> {
        persist_uia_worker_model(&self.profile_dir, model)
    }

    fn persist_uia_reasoning_effort(&self, effort: Option<&str>) -> Option<String> {
        persist_uia_reasoning_effort(&self.profile_dir, effort)
    }

    fn persist_internal_model(
        &self,
        point: harw_config::InternalModelPoint,
        provider: Option<&str>,
        model: Option<&str>,
    ) -> Option<String> {
        persist_internal_model(&self.profile_dir, point, provider, model)
    }

    fn clear_uia_selection(&self) -> Option<String> {
        clear_uia_selection(&self.profile_dir)
    }

    fn persist_active_agent(&self, name: Option<&str>) -> Option<String> {
        persist_active_agent(&self.profile_dir, name)
    }

    fn persist_uia_worker_role_model(&self, role: &str, value: Option<&str>) -> Option<String> {
        persist_uia_worker_role_model(&self.profile_dir, role, value)
    }

    fn persist_default_interaction_mode(&self, mode: &str) -> Option<String> {
        persist_default_interaction_mode(&self.profile_dir, mode)
    }
}

/// Geschlossen fehlschlagende Persistenz für Sitzungen ohne gebundenen
/// Root-Space.
///
/// # Description
/// Schreibt nie etwas und fällt nie auf `HARW_HOME` oder `~/.harw` zurück.
/// Jede Methode meldet stattdessen eine Notiz, die der Aufrufer an die
/// Operator-Ausgabe anhängt; der In-Session-Wechsel bleibt davon unberührt.
#[derive(Debug, Clone, Copy)]
struct UnboundSelectionPersistence;

impl UnboundSelectionPersistence {
    /// Die Notiz, die jede Methode zurückgibt.
    fn note() -> Option<String> {
        Some(format!("Hinweis: nicht dauerhaft gespeichert ({UNBOUND_HOME})."))
    }
}

impl SelectionPersistence for UnboundSelectionPersistence {
    fn persist_default_selection(&self, _: Option<&str>, _: Option<&str>) -> Option<String> {
        Self::note()
    }

    fn persist_uia_selection(&self, _: Option<&str>, _: Option<&str>) -> Option<String> {
        Self::note()
    }

    fn persist_uia_worker_model(&self, _: Option<&str>) -> Option<String> {
        Self::note()
    }

    fn persist_uia_reasoning_effort(&self, _: Option<&str>) -> Option<String> {
        Self::note()
    }

    fn persist_internal_model(
        &self,
        _: harw_config::InternalModelPoint,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Option<String> {
        Self::note()
    }

    fn clear_uia_selection(&self) -> Option<String> {
        Self::note()
    }

    fn persist_active_agent(&self, _: Option<&str>) -> Option<String> {
        Self::note()
    }

    fn persist_uia_worker_role_model(&self, _: &str, _: Option<&str>) -> Option<String> {
        Self::note()
    }

    fn persist_default_interaction_mode(&self, _: &str) -> Option<String> {
        Self::note()
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
    /// Aufzeichnung von [`SelectionPersistence::persist_internal_model`].
    InternalModel {
        point: harw_config::InternalModelPoint,
        provider: Option<String>,
        model: Option<String>,
    },
    /// Aufzeichnung von [`SelectionPersistence::clear_uia_selection`].
    ClearUia,
    /// Aufzeichnung von [`SelectionPersistence::persist_active_agent`].
    ActiveAgent { name: Option<String> },
    /// Aufzeichnung von [`SelectionPersistence::persist_uia_worker_role_model`]
    /// (Runde 5, Teil G).
    UiaWorkerRoleModel { role: String, value: Option<String> },
    /// Aufzeichnung von
    /// [`SelectionPersistence::persist_default_interaction_mode`].
    DefaultInteractionMode { mode: String },
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

    fn persist_internal_model(
        &self,
        point: harw_config::InternalModelPoint,
        provider: Option<&str>,
        model: Option<&str>,
    ) -> Option<String> {
        self.record(RecordedSelectionPersistCall::InternalModel {
            point,
            provider: provider.map(str::to_owned),
            model: model.map(str::to_owned),
        });
        None
    }

    fn clear_uia_selection(&self) -> Option<String> {
        self.record(RecordedSelectionPersistCall::ClearUia);
        None
    }

    fn persist_active_agent(&self, name: Option<&str>) -> Option<String> {
        self.record(RecordedSelectionPersistCall::ActiveAgent {
            name: name.map(str::to_owned),
        });
        None
    }

    fn persist_uia_worker_role_model(&self, role: &str, value: Option<&str>) -> Option<String> {
        self.record(RecordedSelectionPersistCall::UiaWorkerRoleModel {
            role: role.to_owned(),
            value: value.map(str::to_owned),
        });
        None
    }

    fn persist_default_interaction_mode(&self, mode: &str) -> Option<String> {
        self.record(RecordedSelectionPersistCall::DefaultInteractionMode {
            mode: mode.to_owned(),
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
/// [`FileSelectionPersistence`] für das Profil des an die Sitzung gebundenen
/// Root-Space verwendet ([`bound_home`]); die Laufzeit trägt die Bindung auf
/// jeder Oberfläche ein. Fehlt sie, schlägt die Persistenz geschlossen fehl:
/// jede Methode schreibt nichts und meldet eine Notiz — es gibt keinen
/// Rückfall auf `HARW_HOME` oder `~/.harw`.
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
    let persistence = match ctx.service::<Arc<dyn SelectionPersistence>>() {
        Some(persistence) => Arc::clone(persistence),
        // Explicit unsizing casts — a closure/`unwrap_or_else` return position
        // does not reliably coerce `Arc<FileSelectionPersistence>` to
        // `Arc<dyn SelectionPersistence>` without them.
        None => match bound_home(ctx) {
            Ok(home) => Arc::new(FileSelectionPersistence::for_home(home))
                as Arc<dyn SelectionPersistence>,
            Err(_) => Arc::new(UnboundSelectionPersistence) as Arc<dyn SelectionPersistence>,
        },
    };
    // Live-Schnappschuss: mit registrierter Zelle wird jede gelungene
    // Persistenz auch im Speicher gespiegelt, damit Ansichten und
    // Folge-Operationen sofort den neuen Stand sehen.
    match ctx.service::<crate::live_config::SharedLiveConfig>() {
        Some(live) => Arc::new(crate::live_config::MirroringSelectionPersistence::new(
            persistence,
            Arc::clone(live),
        )) as Arc<dyn SelectionPersistence>,
        None => persistence,
    }
}

/// Baut einen gebundenen Root-Space unter `home` für Tests (Profil
/// `default`, Projekt `<home>/project` ohne Marker).
///
/// # Errors
/// [`crate::test_support::TestError::Context`], wenn das Projektverzeichnis
/// nicht angelegt oder die Bindung nicht aufgelöst werden kann.
#[cfg(test)]
pub(crate) fn test_home_context(
    home: &Path,
) -> crate::test_support::TestResult<Arc<harw_home::ResolvedHomeContext>> {
    use crate::test_support::ctx;

    let root = home.join("project");
    std::fs::create_dir_all(&root).map_err(ctx("create test project root"))?;
    let project = harw_home::ProjectRoot {
        trust_key: root.clone(),
        root,
        kind: harw_home::ProjectKind::Directory,
    };
    harw_home::ResolvedHomeContext::new(home, "default".to_owned(), project)
        .map(Arc::new)
        .map_err(ctx("bind test home context"))
}

#[cfg(test)]
mod tests {
    use super::{
        FileSelectionPersistence, OpError, RecordedSelectionPersistCall,
        RecordingSelectionPersistence, SelectionPersistence, clear_uia_keys, execution_error,
        load_bound_config, selection_persistence, test_home_context, write_active_agent,
        write_default_interaction_mode, write_internal_model,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_config::InternalModelPoint;
    use harw_operations::context::ServiceMap;

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

    /// Ohne injizierten Dienst schreibt `selection_persistence` in das Profil
    /// des gebundenen Root-Space — nie nach `HARW_HOME`.
    #[test]
    fn selection_persistence_writes_into_the_bound_home_profile() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut services = ServiceMap::new();
        services.insert(test_home_context(dir.path())?);
        let context = crate::knowledge_test_support::op_context(services)?;

        let note = selection_persistence(&context)
            .persist_default_selection(Some("provider-b"), Some("model-b"));
        assert!(note.is_none(), "{note:?}");

        let config_path = dir
            .path()
            .join("profiles")
            .join("default")
            .join("config.toml");
        let reopened = harw_config::ConfigWriter::open(&config_path).map_err(ctx("reopen"))?;
        assert_eq!(
            reopened.get_value("default_provider"),
            Some("provider-b".to_owned())
        );
        assert_eq!(
            reopened.get_value("default_model"),
            Some("model-b".to_owned())
        );
        Ok(())
    }

    /// Ohne gebundenen Root-Space schlägt jede Persistenz geschlossen fehl
    /// und meldet eine Notiz, statt auf den Prozess-Root-Space auszuweichen.
    #[test]
    fn selection_persistence_without_bound_home_fails_closed() -> TestResult {
        let context = crate::knowledge_test_support::op_context(ServiceMap::new())?;
        let persistence = selection_persistence(&context);

        let note = persistence
            .persist_default_selection(Some("provider-b"), Some("model-b"))
            .ok_or(TestError::Missing("note from persist_default_selection"))?;
        assert!(note.contains("Root-Space"), "{note}");

        let note = persistence
            .persist_default_interaction_mode("plan")
            .ok_or(TestError::Missing(
                "note from persist_default_interaction_mode",
            ))?;
        assert!(note.contains("Root-Space"), "{note}");
        Ok(())
    }

    /// `FileSelectionPersistence` schreibt jeden Schlüssel in die
    /// `config.toml` ihres eigenen Profilverzeichnisses;
    /// `clear_uia_selection` entfernt nur die UIA-Auswahl.
    #[test]
    fn file_selection_persistence_writes_all_keys_into_its_profile_dir() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let persistence = FileSelectionPersistence::new(dir.path().to_path_buf());

        let notes = [
            persistence.persist_uia_selection(Some("anthropic"), Some("claude-x")),
            persistence.persist_uia_worker_model(Some("worker-x")),
            persistence.persist_uia_reasoning_effort(Some("high")),
            persistence.persist_internal_model(
                InternalModelPoint::Explorer,
                Some("openrouter"),
                Some("nvidia/x"),
            ),
            persistence.persist_active_agent(Some("planner")),
            persistence.persist_uia_worker_role_model("uia-writer", Some("openai/gpt-5")),
            persistence.persist_default_interaction_mode("explore"),
        ];
        assert!(notes.iter().all(Option::is_none), "{notes:?}");

        let config_path = dir.path().join("config.toml");
        let reopened = harw_config::ConfigWriter::open(&config_path).map_err(ctx("reopen"))?;
        for (key, expected) in [
            ("uia_provider", "anthropic"),
            ("uia_model", "claude-x"),
            ("uia_worker_model", "worker-x"),
            ("reasoning.uia", "high"),
            ("internal_models.explorer.provider", "openrouter"),
            ("internal_models.explorer.model", "nvidia/x"),
            ("active_agent_definition", "planner"),
            ("uia_worker_models.uia_writer", "openai/gpt-5"),
            ("mode.default", "explore"),
        ] {
            assert_eq!(
                reopened.get_value(key),
                Some(expected.to_owned()),
                "key {key}"
            );
        }

        assert!(persistence.clear_uia_selection().is_none());
        let reopened =
            harw_config::ConfigWriter::open(&config_path).map_err(ctx("reopen after clear"))?;
        assert!(reopened.get_value("uia_provider").is_none());
        assert!(reopened.get_value("uia_model").is_none());
        assert_eq!(
            reopened.get_value("uia_worker_model"),
            Some("worker-x".to_owned())
        );
        Ok(())
    }

    /// `load_bound_config` liest den Root-Layer des gebundenen Root-Space.
    #[test]
    fn load_bound_config_reads_the_bound_root_layer() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::fs::write(dir.path().join("config.toml"), "default_model = \"bound\"\n")
            .map_err(ctx("seed root config"))?;
        let mut services = ServiceMap::new();
        services.insert(test_home_context(dir.path())?);
        let context = crate::knowledge_test_support::op_context(services)?;

        let config = load_bound_config(&context, "x").map_err(ctx("load bound config"))?;
        assert_eq!(config.harness.default_model.as_deref(), Some("bound"));
        Ok(())
    }

    /// Ohne gebundenen Root-Space ist `load_bound_config` nicht verfügbar und
    /// weicht nicht auf den Prozess-Root-Space aus.
    #[test]
    fn load_bound_config_without_bound_home_is_not_available() -> TestResult {
        let context = crate::knowledge_test_support::op_context(ServiceMap::new())?;

        let result = load_bound_config(&context, "x");
        assert!(matches!(result, Err(OpError::NotAvailable(_))));
        Ok(())
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

    // ── UIA-Auswahl-Persistenz-Rundlauf ──
    // Analog zum Muster in `harw-ops/src/permissions.rs`
    // (`test_permissions_persistence_round_trip_default_mode_and_rules`):
    // der Rundlauf prüft den `ConfigWriter`-Schreibpfad direkt gegen ein
    // temporäres Verzeichnis. Den Weg über ein Profilverzeichnis deckt
    // `file_selection_persistence_writes_all_keys_into_its_profile_dir` ab.
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

    // ── uia_worker_model-Persistenz-Rundlauf ──
    // Wie beim UIA-Auswahl-Rundlauf oben prüft dieser Rundlauf den
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

    /// Runde 5, Teil G: die Worker-Rollenwahl landet unter
    /// `[uia_worker_models]`, ist als `HarnessConfig` lesbar und lässt sich
    /// wieder entfernen; eine unbekannte Rolle wird abgelehnt.
    #[test]
    fn test_uia_worker_role_model_round_trip() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let config_path = dir.path().join("config.toml");

        let mut writer = harw_config::ConfigWriter::open(&config_path).map_err(ctx("open"))?;
        super::write_uia_worker_role_model(&mut writer, "uia-writer", Some("openai/gpt-5"))
            .map_err(crate::test_support::TestError::Unexpected)?;
        super::write_uia_worker_role_model(&mut writer, "uia-explorer", Some("uia"))
            .map_err(crate::test_support::TestError::Unexpected)?;
        assert!(
            super::write_uia_worker_role_model(&mut writer, "host-process-worker", Some("uia"))
                .is_err()
        );
        writer.save().map_err(ctx("save"))?;

        let content = std::fs::read_to_string(&config_path).map_err(ctx("read back"))?;
        let parsed: harw_config::HarnessConfig =
            toml::from_str(&content).map_err(ctx("parse harness config"))?;
        assert_eq!(
            parsed.uia_worker_models.get("uia-writer"),
            Some("openai/gpt-5")
        );
        assert_eq!(parsed.uia_worker_models.get("uia-explorer"), Some("uia"));

        let mut writer =
            harw_config::ConfigWriter::open(&config_path).map_err(ctx("reopen for removal"))?;
        super::write_uia_worker_role_model(&mut writer, "uia-writer", None)
            .map_err(crate::test_support::TestError::Unexpected)?;
        writer.save().map_err(ctx("save after removal"))?;
        let reopened =
            harw_config::ConfigWriter::open(&config_path).map_err(ctx("reopen after removal"))?;
        assert!(reopened.get_value("uia_worker_models.uia_writer").is_none());
        assert_eq!(
            reopened.get_value("uia_worker_models.uia_explorer"),
            Some("uia".to_owned())
        );
        Ok(())
    }

    // ── reasoning.uia-Persistenz-Rundlauf ──
    // Wie bei den Rundläufen oben prüft dieser Rundlauf den
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

    #[test]
    fn recording_selection_persistence_records_internal_model_and_clear_uia() {
        let recorder = RecordingSelectionPersistence::new();
        let first = recorder.persist_internal_model(
            InternalModelPoint::Explorer,
            Some("openrouter"),
            Some("nvidia/x"),
        );
        let second = recorder.persist_internal_model(InternalModelPoint::Research, None, None);
        let third = recorder.clear_uia_selection();

        assert!(first.is_none() && second.is_none() && third.is_none());
        assert_eq!(
            recorder.calls(),
            vec![
                RecordedSelectionPersistCall::InternalModel {
                    point: InternalModelPoint::Explorer,
                    provider: Some("openrouter".to_owned()),
                    model: Some("nvidia/x".to_owned()),
                },
                RecordedSelectionPersistCall::InternalModel {
                    point: InternalModelPoint::Research,
                    provider: None,
                    model: None,
                },
                RecordedSelectionPersistCall::ClearUia,
            ]
        );
    }

    /// Schreibkern von `persist_internal_model`: setzt beide Felder unter
    /// `[internal_models.<stelle>]` und entfernt die Tabelle beim Reset
    /// (`None`/`None`) vollständig.
    #[test]
    fn write_internal_model_sets_and_removes_the_point_table() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let config_path = dir.path().join("config.toml");

        let mut writer = harw_config::ConfigWriter::open(&config_path).map_err(ctx("open"))?;
        write_internal_model(
            &mut writer,
            InternalModelPoint::Explorer,
            Some("openrouter"),
            Some("nvidia/nemotron"),
        )
        .map_err(ctx("Schreibkern"))?;
        writer.save().map_err(ctx("save"))?;

        let reopened = harw_config::ConfigWriter::open(&config_path).map_err(ctx("reopen"))?;
        assert_eq!(
            reopened.get_value("internal_models.explorer.provider"),
            Some("openrouter".to_owned())
        );
        assert_eq!(
            reopened.get_value("internal_models.explorer.model"),
            Some("nvidia/nemotron".to_owned())
        );

        // Provider ohne Modell: `model` wird entfernt, `provider` bleibt.
        let mut writer = harw_config::ConfigWriter::open(&config_path).map_err(ctx("reopen2"))?;
        write_internal_model(
            &mut writer,
            InternalModelPoint::Explorer,
            Some("anthropic"),
            None,
        )
        .map_err(ctx("Schreibkern"))?;
        writer.save().map_err(ctx("save2"))?;
        let reopened = harw_config::ConfigWriter::open(&config_path).map_err(ctx("reopen3"))?;
        assert_eq!(
            reopened.get_value("internal_models.explorer.provider"),
            Some("anthropic".to_owned())
        );
        assert!(
            reopened
                .get_value("internal_models.explorer.model")
                .is_none()
        );

        // Reset: die ganze Stellen-Tabelle verschwindet.
        let mut writer = harw_config::ConfigWriter::open(&config_path).map_err(ctx("reopen4"))?;
        write_internal_model(&mut writer, InternalModelPoint::Explorer, None, None)
            .map_err(ctx("Schreibkern"))?;
        writer.save().map_err(ctx("save3"))?;
        let content = std::fs::read_to_string(&config_path).map_err(ctx("read back"))?;
        assert!(
            !content.contains("[internal_models.explorer]"),
            "Reset muss die Stellen-Tabelle entfernen: {content}"
        );
        Ok(())
    }

    #[test]
    fn clear_uia_keys_removes_provider_and_model() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "uia_provider = \"anthropic\"\nuia_model = \"claude-x\"\nuia_worker_model = \"w\"\n",
        )
        .map_err(ctx("seed"))?;

        let mut writer = harw_config::ConfigWriter::open(&config_path).map_err(ctx("open"))?;
        clear_uia_keys(&mut writer);
        writer.save().map_err(ctx("save"))?;

        let reopened = harw_config::ConfigWriter::open(&config_path).map_err(ctx("reopen"))?;
        assert!(reopened.get_value("uia_provider").is_none());
        assert!(reopened.get_value("uia_model").is_none());
        assert_eq!(
            reopened.get_value("uia_worker_model"),
            Some("w".to_owned()),
            "der Worker-Pin gehört nicht zur UIA-Auswahl"
        );
        Ok(())
    }

    #[test]
    fn write_default_interaction_mode_sets_nested_mode_default() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let config_path = dir.path().join("config.toml");

        let mut writer = harw_config::ConfigWriter::open(&config_path).map_err(ctx("open"))?;
        write_default_interaction_mode(&mut writer, "explore").map_err(ctx("Schreibkern"))?;
        writer.save().map_err(ctx("save"))?;

        let reopened = harw_config::ConfigWriter::open(&config_path).map_err(ctx("reopen"))?;
        assert_eq!(
            reopened.get_value("mode.default"),
            Some("explore".to_owned())
        );
        let content = std::fs::read_to_string(&config_path).map_err(ctx("read back"))?;
        assert!(content.contains("[mode]"), "{content}");
        Ok(())
    }

    #[test]
    fn write_active_agent_sets_and_removes_the_top_level_key() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let config_path = dir.path().join("config.toml");

        let mut writer = harw_config::ConfigWriter::open(&config_path).map_err(ctx("open"))?;
        write_active_agent(&mut writer, Some("root-orchestrator")).map_err(ctx("Schreibkern"))?;
        writer.save().map_err(ctx("save"))?;

        let reopened = harw_config::ConfigWriter::open(&config_path).map_err(ctx("reopen"))?;
        assert_eq!(
            reopened.get_value("active_agent_definition"),
            Some("root-orchestrator".to_owned())
        );

        let mut writer =
            harw_config::ConfigWriter::open(&config_path).map_err(ctx("reopen for removal"))?;
        write_active_agent(&mut writer, None).map_err(ctx("Entfernen"))?;
        writer.save().map_err(ctx("save after removal"))?;

        let reopened =
            harw_config::ConfigWriter::open(&config_path).map_err(ctx("reopen after removal"))?;
        assert_eq!(reopened.get_value("active_agent_definition"), None);
        Ok(())
    }

    #[test]
    fn recording_selection_persistence_records_active_agent() {
        let recorder = RecordingSelectionPersistence::new();
        assert!(recorder.persist_active_agent(Some("planner")).is_none());
        assert!(recorder.persist_active_agent(None).is_none());
        assert_eq!(
            recorder.calls(),
            vec![
                RecordedSelectionPersistCall::ActiveAgent {
                    name: Some("planner".to_owned()),
                },
                RecordedSelectionPersistCall::ActiveAgent { name: None },
            ]
        );
    }
}
