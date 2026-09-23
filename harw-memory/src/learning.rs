//! Self-Learning-Bausteine für das Memory-Subsystem.
//!
//! # Verantwortungsbereich
//! Dieses Modul implementiert drei Bausteine aus `docs/design/memory-v2.md §5`:
//!
//! 1. **Statische Lesson-Regeln** (`LessonRule`, `LESSON_RULES`, `score_correction`) —
//!    bewertet User-Text anhand gewichteter Korrektur-Keywords (DE + EN).
//!
//! 2. **Pattern-Counter** (`PatternCounter`, `PatternObservation`) —
//!    flache `HashMap`-Wrapper mit `observe`, `is_promotable` und `prune`.
//!    Persistenz via JSON in `signals/patterns.json`.
//!
//! 3. **Datei-Persistenz** (`load_or_default`, `save`) —
//!    atomares Schreiben (tmp → rename), liest bestehende Datei oder gibt
//!    `PatternCounter::default()` zurück.
//!
//! # Schlüsseltypen
//! - [`LessonRule`] — ein Keyword + Gewicht.
//! - [`LESSON_RULES`] — statische Tabelle (20 Einträge, DE + EN).
//! - [`PatternCounter`] — zählt wiederholte Muster-Kandidaten.
//! - [`PatternObservation`] — Einzel-Eintrag (`count`, `first_seen`, `last_seen`).
//!
//! # Nebenläufigkeit
//! Alle Funktionen sind rein funktional oder erfordern `&mut self`.
//! Keine eigenen Locks — der Aufrufer ist für Synchronisation verantwortlich.
//!
//! # Fehler
//! - [`crate::error::MemoryError::Io`] — Dateisystem-Fehler.
//! - [`crate::error::MemoryError::Serde`] — JSON-Fehler.
//!
//! # Bindung
//! Implementiert `philosophy.md §16 Invariante 8`:
//! „Memory ist eine Promotion-Pipeline, kein unkontrolliertes Langzeit-Transcript."
//!
//! # Beispiel
//! ```no_run
//! use harw_memory::learning::{score_correction, PatternCounter};
//! use time::OffsetDateTime;
//!
//! let score = score_correction("Das ist falsch, bitte korrigiere das.");
//! assert!(score > 0);
//!
//! let mut counter = PatternCounter::default();
//! let now = OffsetDateTime::now_utc();
//! counter.observe("greeting", now);
//! ```

use crate::error::{MemoryError, MemoryResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;

// ─────────────────────────────────────────────────────────
// §1  Statische Lesson-Regeln
// ─────────────────────────────────────────────────────────

/// Eine gewichtete Korrektur-Keyword-Regel.
///
/// # Beschreibung
/// `keyword` wird case-insensitiv als Substring gegen den User-Text geprüft.
/// `weight` gibt an, wie stark die Korrektur-Anzeige gewertet wird (0–100).
///
/// # Design-Referenz
/// `docs/design/memory-v2.md §5`, Tabelle „Learning Signals".
#[derive(Debug, Clone, Copy)]
pub struct LessonRule {
    /// Das Korrektur-Keyword (Kleinbuchstaben, literal).
    pub keyword: &'static str,
    /// Gewicht 0–100; höher = stärkeres Korrektursignal.
    pub weight: u8,
}

/// Statische Tabelle aller Korrektur-Keywords (Deutsch + Englisch).
///
/// # Beschreibung
/// Mindestens 20 Einträge; mehrsprachig. Gewichte spiegeln die semantische
/// Stärke der Korrektur wider (90 = explizite Verneinung, 60 = Präferenz).
///
/// # Design-Referenz
/// `docs/design/memory-v2.md §5`.
pub const LESSON_RULES: &[LessonRule] = &[
    LessonRule {
        keyword: "das ist falsch",
        weight: 90,
    },
    LessonRule {
        keyword: "nein, das ist nicht richtig",
        weight: 90,
    },
    LessonRule {
        keyword: "eigentlich sollte",
        weight: 70,
    },
    LessonRule {
        keyword: "du liegst falsch",
        weight: 90,
    },
    LessonRule {
        keyword: "ich bevorzuge",
        weight: 60,
    },
    LessonRule {
        keyword: "hör auf",
        weight: 80,
    },
    LessonRule {
        keyword: "warum machst du immer",
        weight: 75,
    },
    LessonRule {
        keyword: "ich habe dir gesagt",
        weight: 85,
    },
    LessonRule {
        keyword: "no, that's not right",
        weight: 90,
    },
    LessonRule {
        keyword: "actually, it should be",
        weight: 70,
    },
    LessonRule {
        keyword: "you're wrong about",
        weight: 90,
    },
    LessonRule {
        keyword: "i prefer",
        weight: 60,
    },
    LessonRule {
        keyword: "remember that i always",
        weight: 85,
    },
    LessonRule {
        keyword: "i told you before",
        weight: 85,
    },
    LessonRule {
        keyword: "stop doing",
        weight: 80,
    },
    LessonRule {
        keyword: "why do you keep",
        weight: 75,
    },
    LessonRule {
        keyword: "i like when you",
        weight: 60,
    },
    LessonRule {
        keyword: "always do",
        weight: 65,
    },
    LessonRule {
        keyword: "never do",
        weight: 65,
    },
    LessonRule {
        keyword: "my style is",
        weight: 60,
    },
];

/// Berechnet einen Korrektur-Score für `text` auf Basis der `LESSON_RULES`.
///
/// # Beschreibung
/// Der Text wird in Kleinbuchstaben konvertiert (ASCII-only) und anschließend
/// gegen alle Einträge in `LESSON_RULES` geprüft. Zurückgegeben wird das
/// **Maximum** aller treffsicheren Gewichte, oder `0` bei keinem Treffer.
///
/// # Argumente
/// - `text` (`&str`): rohe User-Nachricht; beliebige UTF-8-Zeichenkette.
///
/// # Rückgabe
/// Maximales Gewicht (0–100). `0` bedeutet: kein Korrektursignal erkannt.
///
/// # Concurrency
/// Rein funktional; thread-safe.
///
/// # Examples
/// ```
/// use harw_memory::learning::score_correction;
///
/// assert_eq!(score_correction("Das ist falsch!"), 90);
/// assert_eq!(score_correction("Wie geht's dir?"), 0);
/// ```
#[must_use]
pub fn score_correction(text: &str) -> u8 {
    let lower = text.to_ascii_lowercase();
    LESSON_RULES
        .iter()
        .filter(|rule| lower.contains(rule.keyword))
        .map(|rule| rule.weight)
        .max()
        .unwrap_or(0)
}

// ─────────────────────────────────────────────────────────
// §2  Pattern-Counter
// ─────────────────────────────────────────────────────────

/// Beobachtungsdaten für einen einzelnen Muster-Schlüssel.
///
/// # Beschreibung
/// Wird von [`PatternCounter`] pro Key gespeichert. `first_seen` und
/// `last_seen` ermöglichen Fenster-basierte Promotion-Checks.
///
/// # Serialisierung
/// RFC 3339 via `time::serde::rfc3339`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternObservation {
    /// Anzahl der Beobachtungen.
    pub count: u32,
    /// Zeitpunkt der ersten Beobachtung (UTC).
    #[serde(with = "time::serde::rfc3339")]
    pub first_seen: OffsetDateTime,
    /// Zeitpunkt der letzten Beobachtung (UTC).
    #[serde(with = "time::serde::rfc3339")]
    pub last_seen: OffsetDateTime,
}

/// Flacher Zähler für wiederholt auftretende Muster-Kandidaten.
///
/// # Beschreibung
/// Speichert pro Key einen [`PatternObservation`]-Eintrag. Persistierbar via
/// JSON (`signals/patterns.json`). Wird von `FileMemoryStore::maintain()`
/// gelesen und nach erfolgreicher Promotion gespeichert.
///
/// # Design-Referenz
/// `docs/design/memory-v2.md §5`.
///
/// # Examples
/// ```
/// use harw_memory::learning::PatternCounter;
/// use time::OffsetDateTime;
///
/// let mut c = PatternCounter::default();
/// let now = OffsetDateTime::now_utc();
/// c.observe("greet", now);
/// c.observe("greet", now);
/// c.observe("greet", now);
/// assert!(c.is_promotable("greet", now, 7, 3));
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PatternCounter {
    /// Beobachtungs-Map: Key → Häufigkeitsdaten.
    pub observations: HashMap<String, PatternObservation>,
}

impl PatternCounter {
    /// Registriert eine Beobachtung für `key` zum Zeitpunkt `now`.
    ///
    /// # Beschreibung
    /// Falls `key` noch nicht bekannt ist, wird ein neuer Eintrag angelegt
    /// (`count = 1`, `first_seen = last_seen = now`). Andernfalls wird `count`
    /// inkrementiert und `last_seen` aktualisiert.
    ///
    /// # Argumente
    /// - `key` (`&str`): stabiler Bezeichner des Musters.
    /// - `now` ([`OffsetDateTime`]): aktueller Zeitstempel (UTC).
    ///
    /// # Concurrency
    /// Erfordert `&mut self`; der Aufrufer ist für Synchronisation verantwortlich.
    pub fn observe(&mut self, key: &str, now: OffsetDateTime) {
        let entry = self
            .observations
            .entry(key.to_owned())
            .or_insert(PatternObservation {
                count: 0,
                first_seen: now,
                last_seen: now,
            });
        entry.count += 1;
        entry.last_seen = now;
    }

    /// Prüft, ob `key` die Promotion-Kriterien erfüllt.
    ///
    /// # Beschreibung
    /// Liefert `true` genau dann, wenn:
    /// - Eine Beobachtung für `key` existiert,
    /// - `count >= threshold`,
    /// - Die Spanne `last_seen − first_seen <= window_days` Tage beträgt
    ///   (d.h. alle Beobachtungen liegen innerhalb des Zeitfensters).
    ///
    /// # Argumente
    /// - `key` (`&str`): Muster-Bezeichner.
    /// - `now` ([`OffsetDateTime`]): aktueller Zeitstempel (ungenutzt, für API-Symmetrie).
    /// - `window_days` (`i64`): maximale Fensterbreite in Tagen. Default laut Design: 7.
    /// - `threshold` (`u32`): Mindestzählwert. Default laut Design: 3.
    ///
    /// # Rückgabe
    /// `true` wenn promotable, `false` sonst.
    #[must_use]
    pub fn is_promotable(
        &self,
        key: &str,
        _now: OffsetDateTime,
        window_days: i64,
        threshold: u32,
    ) -> bool {
        match self.observations.get(key) {
            None => false,
            Some(obs) => {
                if obs.count < threshold {
                    return false;
                }
                let span = obs.last_seen - obs.first_seen;
                span <= time::Duration::days(window_days)
            }
        }
    }

    /// Entfernt Einträge, deren `last_seen` älter als `max_age_days` Tage ist.
    ///
    /// # Beschreibung
    /// Vergleicht `now − obs.last_seen` mit `max_age_days`. Einträge, die
    /// länger als `max_age_days` nicht aktualisiert wurden, werden gelöscht.
    ///
    /// # Argumente
    /// - `now` ([`OffsetDateTime`]): aktueller Zeitstempel (UTC).
    /// - `max_age_days` (`i64`): maximales Alter in Tagen.
    pub fn prune(&mut self, now: OffsetDateTime, max_age_days: i64) {
        self.observations.retain(|_, obs| {
            let age = now - obs.last_seen;
            age <= time::Duration::days(max_age_days)
        });
    }
}

// ─────────────────────────────────────────────────────────
// §3  Datei-Persistenz
// ─────────────────────────────────────────────────────────

/// Lädt den `PatternCounter` aus `root/signals/patterns.json`.
///
/// # Beschreibung
/// Falls die Datei nicht existiert, wird `PatternCounter::default()` zurückgegeben.
/// Alle anderen Fehler (Lesefehler, JSON-Parse-Fehler) werden als `MemoryError`
/// propagiert.
///
/// # Argumente
/// - `root` (`&std::path::Path`): Wurzelpfad des Memory-Stores.
///
/// # Rückgabe
/// `Ok(PatternCounter)` — entweder deserialisiert oder leer (Default).
///
/// # Errors
/// - [`MemoryError::Io`][]: Datei vorhanden, aber nicht lesbar.
/// - [`MemoryError::Serde`][]: JSON-Deserialiserungsfehler.
///
/// # Examples
/// ```no_run
/// use harw_memory::learning::load_or_default;
/// use std::path::Path;
///
/// let counter = load_or_default(Path::new("/tmp/harw-mem")).unwrap();
/// ```
pub fn load_or_default(root: &Path) -> MemoryResult<PatternCounter> {
    let path = root.join("signals").join("patterns.json");
    reject_symlink(&path).map_err(|source| MemoryError::Io {
        path: path.clone(),
        source,
    })?;
    match std::fs::read(&path) {
        Ok(bytes) => {
            let counter = serde_json::from_slice(&bytes).map_err(|e| MemoryError::Serde {
                context: "load PatternCounter",
                source: e,
            })?;
            Ok(counter)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(PatternCounter::default()),
        Err(e) => Err(MemoryError::Io { path, source: e }),
    }
}

/// Speichert `counter` atomar nach `root/signals/patterns.json`.
///
/// # Beschreibung
/// Schreibt zunächst in eine zufällig benannte, exklusiv angelegte temporäre
/// Datei im selben Verzeichnis und benennt diese dann atomar um. Damit bleibt
/// die bestehende Datei bis zur erfolgreichen Fertigstellung erhalten, ohne
/// vorhersehbare temporäre Dateien oder Symlinks zu folgen.
///
/// # Argumente
/// - `counter` (`&PatternCounter`): zu speichernder Zustand.
/// - `root` (`&std::path::Path`): Wurzelpfad des Memory-Stores.
///
/// # Errors
/// - [`MemoryError::Serde`][]: JSON-Serialisierungsfehler.
/// - [`MemoryError::Io`][]: Schreib- oder Umbenennungsfehler.
///
/// # Examples
/// ```no_run
/// use harw_memory::learning::{PatternCounter, save};
/// use std::path::Path;
///
/// let counter = PatternCounter::default();
/// save(&counter, Path::new("/tmp/harw-mem")).unwrap();
/// ```
pub fn save(counter: &PatternCounter, root: &Path) -> MemoryResult<()> {
    let signals_dir = root.join("signals");
    let target = signals_dir.join("patterns.json");

    let bytes = serde_json::to_vec_pretty(counter).map_err(|e| MemoryError::Serde {
        context: "save PatternCounter",
        source: e,
    })?;

    reject_symlink(&signals_dir).map_err(|source| MemoryError::Io {
        path: signals_dir.clone(),
        source,
    })?;
    reject_symlink(&target).map_err(|source| MemoryError::Io {
        path: target.clone(),
        source,
    })?;

    let (mut file, tmp) = create_temp_file(&signals_dir).map_err(|source| MemoryError::Io {
        path: signals_dir.clone(),
        source,
    })?;
    file.write_all(&bytes).map_err(|source| MemoryError::Io {
        path: tmp.clone(),
        source,
    })?;
    file.sync_all().map_err(|source| MemoryError::Io {
        path: tmp.clone(),
        source,
    })?;
    drop(file);

    std::fs::rename(&tmp, &target).map_err(|e| MemoryError::Io {
        path: target,
        source: e,
    })?;

    // Persist the directory entry update where the platform exposes directory
    // syncing; the file itself was already synced before the rename.
    #[cfg(unix)]
    File::open(&signals_dir)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| MemoryError::Io {
            path: signals_dir,
            source,
        })?;

    Ok(())
}

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn create_temp_file(directory: &Path) -> std::io::Result<(File, PathBuf)> {
    for _ in 0..16 {
        let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let path = directory.join(format!(
            ".patterns.json.tmp-{}-{timestamp}-{counter}",
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        match options.open(&path) {
            Ok(file) => return Ok((file, path)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique temporary PatternCounter file",
    ))
}

fn reject_symlink(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing to follow a symlink",
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

// ─────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use time::Duration;

    // Hilfsfunktion: OffsetDateTime N Tage vor `now`
    fn days_ago(now: OffsetDateTime, days: i64) -> OffsetDateTime {
        now - Duration::days(days)
    }

    // ── score_correction ────────────────────────────────

    #[test]
    fn score_correction_matches_de() {
        let score = score_correction("Das ist falsch, bitte korrigiere das!");
        assert_eq!(
            score, 90,
            "deutscher Korrektur-Trigger muss weight 90 zurückgeben"
        );
    }

    #[test]
    fn score_correction_matches_en() {
        let score = score_correction("You're wrong about this approach entirely.");
        assert_eq!(
            score, 90,
            "englischer Korrektur-Trigger muss weight 90 zurückgeben"
        );
    }

    #[test]
    fn score_correction_no_match_zero() {
        let score = score_correction("Ich denke, das könnte interessant sein.");
        assert_eq!(score, 0, "neutraler Text muss 0 zurückgeben");
    }

    #[test]
    fn score_correction_case_insensitive() {
        // "DAS IST FALSCH" muss genauso erkannt werden wie Kleinbuchstaben
        let score = score_correction("DAS IST FALSCH!");
        assert_eq!(score, 90, "Groß-/Kleinschreibung darf keine Rolle spielen");
    }

    // ── PatternCounter ──────────────────────────────────

    #[test]
    fn pattern_counter_increments() {
        let mut c = PatternCounter::default();
        let now = OffsetDateTime::now_utc();
        c.observe("key", now);
        c.observe("key", now);
        c.observe("key", now);
        assert_eq!(c.observations["key"].count, 3);
    }

    #[test]
    fn pattern_counter_is_promotable_at_threshold() {
        let mut c = PatternCounter::default();
        let now = OffsetDateTime::now_utc();
        // Drei Beobachtungen innerhalb von 3 Tagen → Fenster 7d, Schwelle 3
        let t0 = days_ago(now, 3);
        c.observe("key", t0);
        c.observe("key", days_ago(now, 2));
        c.observe("key", days_ago(now, 1));
        assert!(
            c.is_promotable("key", now, 7, 3),
            "count=3 within 3d window ≤ 7d → promotable"
        );
    }

    #[test]
    fn pattern_counter_not_promotable_outside_window() {
        let mut c = PatternCounter::default();
        let now = OffsetDateTime::now_utc();
        // Drei Beobachtungen über 10 Tage → Fenster 7d → nicht promotable
        c.observe("key", days_ago(now, 10));
        c.observe("key", days_ago(now, 5));
        c.observe("key", days_ago(now, 1));
        assert!(
            !c.is_promotable("key", now, 7, 3),
            "span=10d > window=7d → not promotable"
        );
    }

    #[test]
    fn pattern_counter_prune_removes_old() {
        let mut c = PatternCounter::default();
        let now = OffsetDateTime::now_utc();
        // Eintrag mit last_seen vor 31 Tagen → max_age_days=30 → wird gelöscht
        let old = days_ago(now, 31);
        c.observations.insert(
            "old_key".to_owned(),
            PatternObservation {
                count: 5,
                first_seen: old,
                last_seen: old,
            },
        );
        // Eintrag mit last_seen von gestern → bleibt erhalten
        let recent = days_ago(now, 1);
        c.observations.insert(
            "recent_key".to_owned(),
            PatternObservation {
                count: 2,
                first_seen: recent,
                last_seen: recent,
            },
        );
        c.prune(now, 30);
        assert!(
            !c.observations.contains_key("old_key"),
            "alter Eintrag muss entfernt werden"
        );
        assert!(
            c.observations.contains_key("recent_key"),
            "neuer Eintrag muss bleiben"
        );
    }

    // ── save / load Roundtrip ────────────────────────────

    #[test]
    fn save_and_load_roundtrip() -> TestResult {
        // Temporäres Verzeichnis anlegen
        let root = std::env::temp_dir().join(format!(
            "harw-learning-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("signals")).map_err(ctx("signals dir anlegen"))?;

        let now = OffsetDateTime::now_utc();
        let mut counter = PatternCounter::default();
        counter.observe("alpha", now);
        counter.observe("alpha", now);
        counter.observe("beta", now);

        save(&counter, &root).map_err(ctx("save must succeed"))?;
        let loaded = load_or_default(&root).map_err(ctx("load must succeed"))?;

        assert_eq!(
            loaded.observations["alpha"].count, counter.observations["alpha"].count,
            "alpha count must round-trip"
        );
        assert_eq!(
            loaded.observations["beta"].count, counter.observations["beta"].count,
            "beta count must round-trip"
        );

        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn save_rejects_symlinked_target_without_touching_external_file() -> TestResult {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "harw-learning-symlink-target-{}-{}",
            std::process::id(),
            TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let signals = root.join("signals");
        let outside = root.join("outside.json");
        std::fs::create_dir_all(&signals).map_err(ctx("signals dir anlegen"))?;
        std::fs::write(&outside, b"external state").map_err(ctx("outside-Datei schreiben"))?;
        symlink(&outside, signals.join("patterns.json")).map_err(ctx("symlink anlegen"))?;

        let Err(error) = save(&PatternCounter::default(), &root) else {
            return Err(TestError::Unexpected(
                "save must reject a symlinked target".to_owned(),
            ));
        };

        assert!(matches!(error, MemoryError::Io { .. }));
        assert_eq!(
            std::fs::read(&outside).map_err(ctx("outside-Datei lesen"))?,
            b"external state"
        );
        assert!(
            std::fs::symlink_metadata(signals.join("patterns.json"))
                .map_err(ctx("symlink-Metadaten lesen"))?
                .file_type()
                .is_symlink()
        );
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn save_does_not_follow_predictable_temp_symlink() -> TestResult {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "harw-learning-symlink-temp-{}-{}",
            std::process::id(),
            TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let signals = root.join("signals");
        let outside = root.join("outside.json");
        std::fs::create_dir_all(&signals).map_err(ctx("signals dir anlegen"))?;
        std::fs::write(&outside, b"external state").map_err(ctx("outside-Datei schreiben"))?;
        symlink(&outside, signals.join("patterns.json.tmp")).map_err(ctx("symlink anlegen"))?;

        save(&PatternCounter::default(), &root).map_err(ctx("save must succeed"))?;

        assert_eq!(
            std::fs::read(&outside).map_err(ctx("outside-Datei lesen"))?,
            b"external state"
        );
        assert_eq!(
            load_or_default(&root)
                .map_err(ctx("load must succeed"))?
                .observations
                .len(),
            0
        );
        assert!(
            std::fs::symlink_metadata(signals.join("patterns.json.tmp"))
                .map_err(ctx("symlink-Metadaten lesen"))?
                .file_type()
                .is_symlink()
        );
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }
}
