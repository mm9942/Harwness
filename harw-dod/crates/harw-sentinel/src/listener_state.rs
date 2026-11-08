//! Persistierte Listener-Historie der Regel `listener-new` (PL-100 WP1.6).
//!
//! # Verantwortungsbereich
//! [`harw_dod_rules::ListenerHistory`] merkt sich, welche Listener dieser
//! Host schon hatte; die Regel `listener-new` liest sie nur. Laden,
//! Lernen und Speichern liegen beim Aufrufer — hier: [`ListenerState`], das
//! [`crate::findings::RuleCycle`] hält. Je Zyklus wertet `RuleCycle` die
//! Regeln mit der Historie **vor** diesem Zyklus aus und ruft danach
//! [`ListenerState::learn`] mit den Ereignissen des Deltas auf
//! (`observe_with_config` mit derselben [`harw_dod_rules::RuleConfig`] wie
//! die Auswertung), am Zyklusende [`ListenerState::persist_if_due`].
//!
//! # Zustand
//! JSON im Format von `ListenerHistory::to_json` in
//! `<home>/`[`DEFAULT_STATE_FILE_NAME`] (`--home`, im Betrieb das
//! Zustandsverzeichnis), Modus [`STATE_FILE_MODE`] (`0600`), atomar ersetzt
//! über die temporäre Geschwisterdatei `.listener-history.json.tmp`
//! (`crate::health_export::write_atomically`, wie der Throttle-Zustand).
//! Die Alias-Prüfung beim Start (`crate::check_export_destinations`) führt
//! beide Namen als Schreibziel. Geschrieben wird
//! - **sofort** am Zyklusende, wenn die Historie in diesem Zyklus
//!   vorbereitet wurde (Lernzyklus) oder einen neuen Schlüssel gelernt hat;
//! - sonst, wenn nur `last_seen` bekannter Listener vorrückte, höchstens alle
//!   [`PERSIST_MIN_INTERVAL`] (der Listener-Sensor meldet jeden offenen
//!   Listener in jedem Zyklus erneut).
//!
//! # Laden: begrenzt, nie fatal, nie still
//! Gelesen werden höchstens `ListenerHistory::MAX_JSON_BYTES + 1` Bytes
//! eines regulären Files: geöffnet mit `O_NOFOLLOW` (nie über einen
//! Symlink, auch keinen zwischen Prüfung und Öffnen eingetauschten), Typ
//! und Größe am geöffneten Deskriptor geprüft
//! (`crate::health_export::read_regular_bounded`). Eine fehlende Datei ist der
//! normale erste Start. Eine unlesbare, zu große, defekte oder fremde Datei
//! führt zu `ListenerHistory::new()` — der folgende Lernzyklus übernimmt
//! dann **jeden** gerade offenen Listener ungeprüft, auch eine eben
//! geöffnete Hintertür. Das geschieht deshalb nie still: der Grund
//! ([`LoadFailure`]) steht als `degraded` im Objekt `listener_history` von
//! `health.json` (für die ganze Laufzeit dieses Prozesses), auf `warn` (nur
//! der Grund, kein Pfad) und als `SensorDegraded`-Ereignis
//! ([`degraded_event`], Kennung [`LISTENER_HISTORY_STATUS_SENSOR_ID`]), das
//! die Regel `sensor-degraded` als Befund meldet.
//!
//! # Nebenläufigkeit
//! Lebt ausschließlich im Sammelthread (`&mut self`); kein interner Lock.
//!
//! # Fehler
//! Keine nach außen. Lade- und Schreibfehler werden gezählt bzw. gemeldet
//! (die erste Schreibstörung auf `warn`, Wiederholungen auf `debug`) und
//! beim nächsten fälligen Zyklus erneut versucht.
//!
//! # Examples
//! ```rust,ignore
//! let mut state = ListenerState::open(Some(home.join(DEFAULT_STATE_FILE_NAME)));
//! if state.load_failure().is_some() {
//!     sentinel.record_external_event(degraded_event(now));
//! }
//! // je Zyklus: auswerten mit `state.history()`, dann lernen und speichern
//! state.learn(&delta.events, now, Some(&rule_config));
//! state.persist_if_due(now);
//! ```

use std::io;
use std::path::{Path, PathBuf};

use harw_dod_rules::{ListenerHistory, ListenerHistoryError, ListenerKey, RuleConfig};
use harw_dod_signals::{EventKind, SecurityEvent};
use harw_types::SensorId;
use jiff::{SignedDuration, Timestamp};

use crate::health_export;

/// Dateiname der Listener-Historie unter `--home`.
pub const DEFAULT_STATE_FILE_NAME: &str = "listener-history.json";

/// Modus einer neu angelegten Historie: nur der Sentinel liest und schreibt.
pub const STATE_FILE_MODE: u32 = 0o600;

/// Mindestabstand zwischen zwei Schreibvorgängen, solange nur bekannte
/// Listener erneut gesehen werden.
pub const PERSIST_MIN_INTERVAL: SignedDuration = SignedDuration::from_secs(60);

/// Kennung, unter der der Sentinel eine verworfene Listener-Historie als
/// `SensorDegraded` meldet. **Kein** registrierter Sensor (wie
/// `crate::sandbox::LANDLOCK_STATUS_SENSOR_ID`).
pub const LISTENER_HISTORY_STATUS_SENSOR_ID: &str = "sentinel-listener-history";

/// Warum eine vorhandene Listener-Historie beim Start verworfen wurde.
///
/// # Concurrency
/// Reiner Werttyp, `Copy + Send + Sync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadFailure {
    /// Der Pfad ist kein reguläres File (Symlink, Verzeichnis, …).
    NotRegularFile,
    /// Metadaten oder Inhalt ließen sich nicht lesen.
    Unreadable,
    /// Größer als `ListenerHistory::MAX_JSON_BYTES`.
    TooLarge,
    /// Kein gültiges JSON dieses Formats, fremde Version, zu viele oder
    /// doppelte Einträge.
    Invalid,
}

impl LoadFailure {
    /// Stabiler Bezeichner für `health.json` (`degrade_reason`) und das
    /// Journal.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::NotRegularFile => "state_not_regular_file",
            Self::Unreadable => "state_unreadable",
            Self::TooLarge => "state_too_large",
            Self::Invalid => "state_invalid",
        }
    }
}

/// Was `health.json` über die Listener-Historie meldet (Objekt
/// `listener_history`).
///
/// # Description
/// `degraded`, wenn die gespeicherte Historie beim Start verworfen wurde
/// (für die ganze Laufzeit, siehe Moduldoku) oder der letzte Schreibversuch
/// scheiterte; der Ladegrund hat Vorrang vor `write_failed`.
///
/// # Concurrency
/// Reiner Werttyp, `Copy + Send + Sync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenerHistoryHealth {
    /// Grund, aus dem die gespeicherte Historie beim Start verworfen wurde.
    pub load_failure: Option<LoadFailure>,
    /// Ob der letzte Schreibversuch scheiterte.
    pub write_failing: bool,
    /// Zahl gemerkter Listener-Schlüssel.
    pub keys: u64,
    /// Ob der Lernzyklus stattgefunden hat (`listener-new` meldet erst
    /// danach).
    pub primed: bool,
}

impl ListenerHistoryHealth {
    /// Ob `health.json` die Historie als `degraded` meldet.
    #[must_use]
    pub const fn is_degraded(&self) -> bool {
        self.load_failure.is_some() || self.write_failing
    }

    /// Der gemeldete Grund (`None`, wenn nicht degradiert).
    #[must_use]
    pub const fn degrade_reason(&self) -> Option<&'static str> {
        match self.load_failure {
            Some(failure) => Some(failure.label()),
            None if self.write_failing => Some("write_failed"),
            None => None,
        }
    }
}

/// Die Listener-Historie samt Speicherort und Schreibzustand.
///
/// # Description
/// Siehe Moduldoku: begrenztes Laden, Lernen nach jeder Auswertung,
/// atomares Speichern (sofort nach neuem Wissen, sonst gedrosselt).
///
/// # Concurrency
/// `&mut self` für jede Änderung; lebt im Sammelthread.
#[derive(Debug)]
pub struct ListenerState {
    // Die Historie, die die Regeln lesen.
    history: ListenerHistory,
    // Zustandsdatei; `None` hält nur im Speicher.
    path: Option<PathBuf>,
    // Warum eine vorhandene Datei beim Start verworfen wurde.
    load_failure: Option<LoadFailure>,
    // Seit dem letzten Schreiben geändert.
    dirty: bool,
    // Seit dem letzten Schreiben neues Wissen (Lernzyklus, neuer Schlüssel).
    urgent: bool,
    // Zeitpunkt des letzten Schreibversuchs.
    last_persist: Option<Timestamp>,
    // Der letzte Schreibversuch scheiterte (begrenzt die Warnungen).
    failing: bool,
}

impl ListenerState {
    /// Lädt die Historie von `path` (siehe Moduldoku, „Laden").
    ///
    /// # Arguments
    /// - `path` (`Option<PathBuf>`): die Zustandsdatei (im Betrieb
    ///   `<home>/`[`DEFAULT_STATE_FILE_NAME`]); `None` hält nur im Speicher.
    ///
    /// # Returns
    /// Den Zustand: die geladene Historie, sonst `ListenerHistory::new()` —
    /// nach einem Ladefehler mit [`Self::load_failure`] gesetzt und einer
    /// Warnung (nur der Grund).
    #[must_use]
    pub fn open(path: Option<PathBuf>) -> Self {
        let (history, load_failure) = match path.as_deref().map(read_history) {
            None | Some(Ok(None)) => {
                tracing::debug!(
                    "no listener history yet; the first cycle learns the open listeners"
                );
                (ListenerHistory::new(), None)
            }
            Some(Ok(Some(history))) => {
                tracing::info!(
                    keys = history.len(),
                    primed = history.is_primed(),
                    "listener history restored"
                );
                (history, None)
            }
            Some(Err(failure)) => {
                tracing::warn!(
                    reason = failure.label(),
                    "listener history unusable; starting a new learning cycle that accepts \
                     every currently open listener unchecked"
                );
                (ListenerHistory::new(), Some(failure))
            }
        };
        Self {
            history,
            path,
            load_failure,
            dirty: false,
            urgent: false,
            last_persist: None,
            failing: false,
        }
    }

    /// Die Historie für den `RuleContext` dieses Zyklus.
    #[must_use]
    pub const fn history(&self) -> &ListenerHistory {
        &self.history
    }

    /// Warum eine vorhandene Historie beim Start verworfen wurde.
    ///
    /// # Returns
    /// `None` nach erfolgreichem Laden oder ohne Datei.
    #[must_use]
    pub const fn load_failure(&self) -> Option<LoadFailure> {
        self.load_failure
    }

    /// Der Stand für `health.json`.
    #[must_use]
    pub fn health(&self) -> ListenerHistoryHealth {
        ListenerHistoryHealth {
            load_failure: self.load_failure,
            write_failing: self.failing,
            keys: u64::try_from(self.history.len()).unwrap_or(u64::MAX),
            primed: self.history.is_primed(),
        }
    }

    /// Lernt die Listener eines ausgewerteten Zyklus.
    ///
    /// # Description
    /// Ruft `ListenerHistory::observe_with_config(events, now, config)` auf
    /// und merkt sich, ob gespeichert werden muss: sofort nach dem
    /// Lernzyklus oder einem neuen Schlüssel, gedrosselt, wenn nur bekannte
    /// Listener erneut gesehen wurden.
    ///
    /// # Arguments
    /// - `events` (`&[SecurityEvent]`): die Ereignisse des gerade
    ///   ausgewerteten Deltas (nie der ganze Ring).
    /// - `now` (`Timestamp`): die injizierte Zeit des Zyklus.
    /// - `config` (`Option<&RuleConfig>`): **dieselbe** Konfiguration wie
    ///   `rule_config` der Auswertung.
    pub fn learn(&mut self, events: &[SecurityEvent], now: Timestamp, config: Option<&RuleConfig>) {
        let was_primed = self.history.is_primed();
        let mut listeners = 0_u64;
        let mut new_keys = 0_u64;
        for key in events
            .iter()
            .filter_map(|event| ListenerKey::from_event(event, config))
        {
            listeners = listeners.saturating_add(1);
            if !self.history.contains(&key) {
                new_keys = new_keys.saturating_add(1);
            }
        }
        self.history.observe_with_config(events, now, config);
        if !was_primed || new_keys > 0 {
            self.urgent = true;
        }
        if !was_primed || listeners > 0 {
            self.dirty = true;
        }
        tracing::debug!(
            listeners,
            new_keys,
            keys = self.history.len(),
            "listener history learned this cycle's listeners"
        );
    }

    /// Schreibt die Historie, wenn sie geändert ist und fällig wird.
    ///
    /// # Description
    /// Fällig sofort nach neuem Wissen, sonst wenn seit dem letzten
    /// Schreibversuch [`PERSIST_MIN_INTERVAL`] verging oder die Uhr
    /// zurücksprang. Ohne Zustandsdatei ein No-Op. Ein Fehlschlag wird
    /// gemeldet (erstmals `warn`, ohne Pfad) und beim nächsten fälligen
    /// Zyklus wiederholt; ein dringender Schreibvorgang (neues Wissen) bleibt
    /// dabei dringend, wird also schon im nächsten Zyklus wiederholt statt
    /// erst nach [`PERSIST_MIN_INTERVAL`] -- sonst lernte ein Neustart in
    /// diesem Fenster den Listener erneut und meldete ihn wieder als neu.
    ///
    /// # Arguments
    /// - `now` (`Timestamp`): die Zeit des Zyklus.
    ///
    /// # Returns
    /// `true`, wenn die Historie geschrieben wurde.
    pub fn persist_if_due(&mut self, now: Timestamp) -> bool {
        if !self.dirty {
            return false;
        }
        let Some(path) = self.path.as_deref() else {
            return false;
        };
        let interval_elapsed = self.last_persist.is_none_or(|last| {
            let since = now.duration_since(last);
            since.is_negative() || since >= PERSIST_MIN_INTERVAL
        });
        if !self.urgent && !interval_elapsed {
            return false;
        }
        let result = self
            .history
            .to_json()
            .map_err(io::Error::other)
            .and_then(|bytes| write_history(path, &bytes));
        self.last_persist = Some(now);
        match result {
            Ok(()) => {
                // Nur ein gelungener Schreibvorgang erledigt die Dringlichkeit.
                self.urgent = false;
                self.dirty = false;
                if self.failing {
                    tracing::info!("listener history writable again");
                }
                self.failing = false;
                true
            }
            Err(error) => {
                if self.failing {
                    tracing::debug!(error = %error, "listener history still not writable");
                } else {
                    tracing::warn!(
                        error = %error,
                        "listener history not writable; a restart relearns every open listener"
                    );
                }
                self.failing = true;
                false
            }
        }
    }
}

/// Baut das `SensorDegraded`-Ereignis, das eine beim Start verworfene
/// Listener-Historie meldet (siehe Moduldoku).
///
/// # Arguments
/// - `now` (`Timestamp`): Zeitpunkt des Ladeversuchs.
///
/// # Returns
/// Ein Ereignis ohne Akteur mit `EventKind::SensorDegraded { sensor:
/// SensorId::from_str(`[`LISTENER_HISTORY_STATUS_SENSOR_ID`]`) }`.
#[must_use]
pub fn degraded_event(now: Timestamp) -> SecurityEvent {
    let sensor = SensorId::from_str(LISTENER_HISTORY_STATUS_SENSOR_ID);
    SecurityEvent {
        sensor: sensor.clone(),
        observed_at: now,
        actor: None,
        kind: EventKind::SensorDegraded { sensor },
    }
}

/// Liest die gespeicherte Historie begrenzt.
///
/// # Returns
/// `Ok(None)`, wenn es die Datei nicht gibt.
///
/// # Errors
/// [`LoadFailure`] für jedes andere Ergebnis als eine gültige Historie.
fn read_history(path: &Path) -> Result<Option<ListenerHistory>, LoadFailure> {
    let limit = u64::try_from(ListenerHistory::MAX_JSON_BYTES).unwrap_or(u64::MAX);
    // `O_NOFOLLOW`, Typ und Größe am geöffneten Deskriptor (PL-100 Review
    // Runde 1): kein Symlink wird je gefolgt, auch kein zwischen Prüfung
    // und Öffnen eingetauschter.
    let bytes = match health_export::read_regular_bounded(path, limit) {
        Ok(health_export::RegularRead::Missing) => return Ok(None),
        Ok(health_export::RegularRead::NotRegularFile) => {
            return Err(LoadFailure::NotRegularFile);
        }
        Ok(health_export::RegularRead::TooLarge) => return Err(LoadFailure::TooLarge),
        Ok(health_export::RegularRead::Bytes(bytes)) => bytes,
        Err(error) => {
            tracing::debug!(error = %error, "listener history unreadable");
            return Err(LoadFailure::Unreadable);
        }
    };
    // `from_json` lehnt mehr als `MAX_JSON_BYTES` zusätzlich vor dem Parsen ab.
    match ListenerHistory::from_json(&bytes) {
        Ok(history) => Ok(Some(history)),
        Err(ListenerHistoryError::TooLarge) => Err(LoadFailure::TooLarge),
        Err(error) => {
            // Inhaltsfrei (Kategorie, Zeile, Spalte), siehe `ListenerHistoryError`.
            tracing::debug!(error = %error, "listener history rejected");
            Err(LoadFailure::Invalid)
        }
    }
}

/// Ersetzt die Historie atomar (Modus [`STATE_FILE_MODE`]).
fn write_history(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = health_export::temporary_path(path).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "listener history path has no file name",
        )
    })?;
    health_export::write_atomically(path, &temporary, bytes, STATE_FILE_MODE)
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_dod_signals::Actor;

    fn at(secs: i64) -> TestResult<Timestamp> {
        Timestamp::from_second(secs).map_err(ctx("timestamp"))
    }

    fn listener(port: u16, observed_at: Timestamp) -> SecurityEvent {
        SecurityEvent {
            sensor: SensorId::from_str("listener-0"),
            observed_at,
            actor: Some(Actor {
                uid: 1000,
                auid: None,
                cgroup: None,
                process: None,
            }),
            kind: EventKind::ListenerOpened { port },
        }
    }

    fn key(port: u16) -> TestResult<ListenerKey> {
        ListenerKey::from_event(&listener(port, Timestamp::UNIX_EPOCH), None)
            .ok_or(TestError::Missing("listener key"))
    }

    fn state_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join(DEFAULT_STATE_FILE_NAME)
    }

    #[test]
    fn test_missing_state_is_a_clean_first_start() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let state = ListenerState::open(Some(state_path(&dir)));
        assert_eq!(state.load_failure(), None);
        assert!(!state.history().is_primed());
        let health = state.health();
        assert!(!health.is_degraded());
        assert_eq!(health.degrade_reason(), None);
        assert_eq!((health.keys, health.primed), (0, false));
        Ok(())
    }

    /// Lernzyklus, sofortiges Speichern, Neustart: die Historie kommt
    /// vorbereitet und mit dem gelernten Schlüssel zurück.
    #[test]
    fn test_learning_cycle_is_persisted_at_once_and_survives_a_restart() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut state = ListenerState::open(Some(state_path(&dir)));
        state.learn(&[listener(8080, at(10)?)], at(10)?, None);
        assert!(
            state.persist_if_due(at(10)?),
            "the learning cycle is urgent"
        );
        assert!(!state.persist_if_due(at(11)?), "nothing changed since");

        let restarted = ListenerState::open(Some(state_path(&dir)));
        assert_eq!(restarted.load_failure(), None);
        assert!(restarted.history().is_primed());
        assert_eq!(restarted.history().primed_at(), Some(at(10)?));
        assert!(restarted.history().contains(&key(8080)?));
        assert_eq!(restarted.health().keys, 1);
        Ok(())
    }

    /// Auch ein Lernzyklus ohne Listener wird gespeichert: sonst lernte ein
    /// Neustart den ersten echten Listener still.
    #[test]
    fn test_an_empty_learning_cycle_is_persisted_too() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut state = ListenerState::open(Some(state_path(&dir)));
        state.learn(&[], at(10)?, None);
        assert!(state.persist_if_due(at(10)?));
        let restarted = ListenerState::open(Some(state_path(&dir)));
        assert!(restarted.history().is_primed());
        assert!(restarted.history().is_empty());
        Ok(())
    }

    /// Ein neuer Schlüssel wird sofort gespeichert; bloßes Wiedersehen
    /// bekannter Listener höchstens je `PERSIST_MIN_INTERVAL`.
    #[test]
    fn test_known_listeners_persist_throttled_and_new_keys_at_once() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut state = ListenerState::open(Some(state_path(&dir)));
        state.learn(&[listener(22, at(0)?)], at(0)?, None);
        assert!(state.persist_if_due(at(0)?));

        state.learn(&[listener(22, at(5)?)], at(5)?, None);
        assert!(!state.persist_if_due(at(5)?), "only last_seen moved");
        state.learn(&[listener(22, at(30)?)], at(30)?, None);
        assert!(!state.persist_if_due(at(30)?));
        state.learn(&[listener(22, at(60)?)], at(60)?, None);
        assert!(state.persist_if_due(at(60)?), "the interval elapsed");

        state.learn(
            &[listener(22, at(61)?), listener(4444, at(61)?)],
            at(61)?,
            None,
        );
        assert!(
            state.persist_if_due(at(61)?),
            "a new key is written at once"
        );
        let restarted = ListenerState::open(Some(state_path(&dir)));
        assert!(restarted.history().contains(&key(4444)?));
        Ok(())
    }

    /// Ein defekter Zustand führt zu einem neuen Lernzyklus, nie zum
    /// Abbruch, und ist sichtbar (`degraded` mit Grund).
    #[test]
    fn test_corrupt_state_starts_new_and_reports_degraded() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::fs::write(state_path(&dir), b"{\"version\": 1, \"primed_at\"")
            .map_err(ctx("write corrupt state"))?;
        let state = ListenerState::open(Some(state_path(&dir)));
        assert_eq!(state.load_failure(), Some(LoadFailure::Invalid));
        assert!(!state.history().is_primed(), "a fresh history");
        let health = state.health();
        assert!(health.is_degraded());
        assert_eq!(health.degrade_reason(), Some("state_invalid"));

        std::fs::write(
            state_path(&dir),
            b"{\"version\":99,\"primed_at\":null,\"entries\":[]}",
        )
        .map_err(ctx("write foreign version"))?;
        let foreign = ListenerState::open(Some(state_path(&dir)));
        assert_eq!(foreign.load_failure(), Some(LoadFailure::Invalid));
        Ok(())
    }

    /// Eine übergroße Datei wird abgelehnt, ohne sie ganz zu lesen.
    #[test]
    fn test_oversized_state_is_refused_as_too_large() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let size = u64::try_from(ListenerHistory::MAX_JSON_BYTES)
            .map_err(ctx("size bound"))?
            .saturating_add(1);
        let file = File::create(state_path(&dir)).map_err(ctx("create state"))?;
        file.set_len(size).map_err(ctx("grow state"))?;
        let state = ListenerState::open(Some(state_path(&dir)));
        assert_eq!(state.load_failure(), Some(LoadFailure::TooLarge));
        assert_eq!(state.health().degrade_reason(), Some("state_too_large"));
        Ok(())
    }

    /// Ein Symlink an Stelle der Historie wird nie verfolgt; eine neue
    /// Historie entsteht als privates File (`0600`).
    #[cfg(unix)]
    #[test]
    fn test_symlinked_state_is_refused_and_a_written_state_is_private() -> TestResult {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let target = dir.path().join("elsewhere.json");
        std::fs::write(&target, b"{}").map_err(ctx("write target"))?;
        std::os::unix::fs::symlink(&target, state_path(&dir)).map_err(ctx("symlink"))?;
        let mut state = ListenerState::open(Some(state_path(&dir)));
        assert_eq!(state.load_failure(), Some(LoadFailure::NotRegularFile));
        state.learn(&[], at(0)?, None);
        assert!(!state.persist_if_due(at(0)?), "the symlink is not replaced");
        assert!(state.health().write_failing);
        assert_eq!(
            state.health().degrade_reason(),
            Some("state_not_regular_file"),
            "the load failure keeps precedence"
        );

        let other = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut fresh = ListenerState::open(Some(state_path(&other)));
        fresh.learn(&[], at(0)?, None);
        assert!(fresh.persist_if_due(at(0)?));
        let mode = std::fs::metadata(state_path(&other))
            .map_err(ctx("state metadata"))?
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, STATE_FILE_MODE);
        Ok(())
    }

    /// Ein Schreibfehler macht die Historie `degraded` (`write_failed`),
    /// ein späterer Erfolg nimmt das zurück.
    #[test]
    fn test_write_failure_is_reported_and_recovers() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("missing-dir").join(DEFAULT_STATE_FILE_NAME);
        let mut state = ListenerState::open(Some(path.clone()));
        state.learn(&[listener(22, at(0)?)], at(0)?, None);
        assert!(!state.persist_if_due(at(0)?));
        assert_eq!(state.health().degrade_reason(), Some("write_failed"));

        std::fs::create_dir(dir.path().join("missing-dir")).map_err(ctx("create dir"))?;
        // Review R2: der gescheiterte dringende Schreibvorgang bleibt
        // dringend und wird im nächsten Zyklus wiederholt, nicht erst nach
        // `PERSIST_MIN_INTERVAL`.
        assert!(
            state.persist_if_due(at(1)?),
            "urgent write retried next cycle"
        );
        assert_eq!(state.health().degrade_reason(), None);
        assert!(path.is_file(), "the history reached the state file");
        Ok(())
    }

    #[test]
    fn test_degraded_event_names_the_listener_history() -> TestResult {
        let event = degraded_event(at(7)?);
        assert_eq!(event.sensor.as_str(), LISTENER_HISTORY_STATUS_SENSOR_ID);
        assert_eq!(event.observed_at, at(7)?);
        assert!(event.actor.is_none());
        let EventKind::SensorDegraded { sensor } = &event.kind else {
            return Err(TestError::Unexpected("expected SensorDegraded".to_owned()));
        };
        assert_eq!(sensor.as_str(), LISTENER_HISTORY_STATUS_SENSOR_ID);
        Ok(())
    }

    #[test]
    fn test_load_failure_labels_are_stable() {
        assert_eq!(
            LoadFailure::NotRegularFile.label(),
            "state_not_regular_file"
        );
        assert_eq!(LoadFailure::Unreadable.label(), "state_unreadable");
        assert_eq!(LoadFailure::TooLarge.label(), "state_too_large");
        assert_eq!(LoadFailure::Invalid.label(), "state_invalid");
    }
}
