//! Anhängender JSONL-Telemetrie-Sink mit Größenrotation und
//! Blake3-Prüfsumme.
//!
//! # Verantwortungsbereich
//! Trägt [`FileSink`], den ersten echten Implementierer von
//! [`harw_observe::TelemetrySink`] (Vertrag A.6,
//! `docs/aw-contract-master.md`). Schreibt jeden `record()`-Aufruf als
//! eine JSON-Zeile in eine anhängende Datei unter dem übergebenen
//! Verzeichnis; sobald die aktive Datei `max_bytes` erreicht oder
//! überschreitet, wird sie geschlossen, umbenannt und bekommt eine
//! `.blake3`-Beidatei mit ihrem Digest, bevor eine neue leere aktive
//! Datei entsteht.
//!
//! # Nebenläufigkeit
//! [`FileSink`] ist `Send + Sync + Debug` (Vertrag A.3). Weil
//! `TelemetrySink::record` nur `&self` bekommt, hält `FileSink` seinen
//! gesamten veränderlichen Zustand (offene Datei, aktuelle Größe,
//! Rotationszähler) hinter einem `std::sync::Mutex`. Jeder Aufruf aus
//! jedem Thread nimmt diese eine Sperre für die Dauer des Schreibens bzw.
//! Rotierens.
//!
//! # Fehler
//! [`crate::error::ObserveFileError`] aus `open()`.
//! `TelemetrySink::record` und `::flush` geben `()` zurück
//! (Vertragsentscheidung, siehe [`harw_observe::TelemetrySink`]); interne
//! I/O- oder Serialisierungsfehler während des Schreibens oder Rotierens
//! werden verworfen und in einem eigenen Zähler
//! ([`FileSink::write_error_count`]) erfasst, statt die beobachtete
//! Operation scheitern zu lassen.
//!
//! # Examples
//! ```rust,no_run
//! use harw_observe::{Cardinality, MetricKey, MetricKind, MetricValue, TelemetrySink, Unit};
//! use harw_observe_file::FileSink;
//! use std::path::Path;
//!
//! let sink = FileSink::open(Path::new("/var/lib/harwness/telemetry"), 10 * 1024 * 1024)
//!     .expect("Verzeichnis beschreibbar");
//! const KEY: MetricKey = MetricKey {
//!     name: "jobs_completed_total",
//!     kind: MetricKind::Counter,
//!     unit: Unit::Count,
//!     labels: &[],
//!     cardinality: Cardinality::Single,
//! };
//! sink.record(&KEY, MetricValue::Count(1), &[]);
//! sink.flush();
//! ```

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use harw_observe::{
    Cardinality, FieldName, FieldValue, MetricKey, MetricKind, MetricValue, TelemetrySink, Unit,
};
use serde::Serialize;

use crate::error::ObserveFileError;

const ACTIVE_FILE_NAME: &str = "active.jsonl";
const ROTATED_PREFIX: &str = "rotated-";
const ROTATED_SUFFIX: &str = ".jsonl";
const DIGEST_SUFFIX: &str = ".blake3";

/// Ein anhängender JSONL-Sink unter dem harw-Home, mit Größenrotation.
#[derive(Debug)]
pub struct FileSink {
    dir: PathBuf,
    max_bytes: u64,
    state: Mutex<FileSinkState>,
    write_errors: AtomicU64,
}

/// Der veränderliche Teil von [`FileSink`], hinter einem `Mutex`, weil
/// `TelemetrySink::record` nur `&self` bekommt (Vertrag A.3).
#[derive(Debug)]
struct FileSinkState {
    file: File,
    size: u64,
    next_sequence: u64,
}

/// Eine JSONL-Zeile: ein einzelner `record()`-Aufruf.
///
/// Rein intern — `MetricKey`/`MetricValue`/`FieldValue` aus `harw-observe`
/// tragen bewusst keine Serde-Ableitung (Vertrag A.2/A.1 sind reine
/// Werttypen ohne Wire-Anspruch), daher baut dieser Sink seine eigene,
/// serialisierbare Abbildung.
#[derive(Serialize)]
struct JsonlRecord {
    timestamp: String,
    metric: &'static str,
    kind: &'static str,
    unit: &'static str,
    cardinality: String,
    value: JsonlValue,
    labels: Vec<(String, JsonlValue)>,
}

/// Serialisierbare Abbildung eines [`harw_observe::FieldValue`] oder
/// [`harw_observe::MetricValue`], rein intern für [`JsonlRecord`].
#[derive(Serialize)]
#[serde(untagged)]
enum JsonlValue {
    I64(i64),
    U64(u64),
    F64(f64),
    Bool(bool),
    Str(String),
}

impl FileSink {
    /// Öffnet oder legt den Sink unter `dir` an.
    ///
    /// # Description
    /// Legt `dir` an, falls es fehlt, und öffnet die aktive Datei
    /// (`active.jsonl`) anhängend — vorhandener Inhalt aus einem früheren
    /// Prozess bleibt erhalten. Vorhandene rotierte Dateien im Verzeichnis
    /// bestimmen die nächste Rotationsnummer, damit ein Neustart keine
    /// Dateien überschreibt.
    ///
    /// # Arguments
    /// - `dir` (`&Path`): Zielverzeichnis für aktive und rotierte Dateien.
    /// - `max_bytes` (`u64`): Größenschwelle, ab der die aktive Datei nach
    ///   einem Schreibvorgang rotiert wird. Ein Wert von `0` wird als `1`
    ///   behandelt, damit jeder Schreibvorgang sofort rotiert statt
    ///   endlos zu wachsen.
    ///
    /// # Returns
    /// Der geöffnete `FileSink`, bereit für `record()`.
    ///
    /// # Errors
    /// [`ObserveFileError::Open`], wenn das Verzeichnis nicht angelegt
    /// oder die aktive Datei nicht geöffnet bzw. ihre Metadaten nicht
    /// gelesen werden können.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_observe_file::FileSink;
    /// use std::path::Path;
    /// let sink = FileSink::open(Path::new("/tmp/harw-telemetry"), 1_048_576).unwrap();
    /// ```
    pub fn open(dir: &Path, max_bytes: u64) -> Result<Self, ObserveFileError> {
        fs::create_dir_all(dir).map_err(|source| ObserveFileError::Open {
            path: dir.display().to_string(),
            source,
        })?;

        let next_sequence = next_rotation_sequence(dir);

        let active_path = dir.join(ACTIVE_FILE_NAME);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&active_path)
            .map_err(|source| ObserveFileError::Open {
                path: active_path.display().to_string(),
                source,
            })?;
        let size = file
            .metadata()
            .map_err(|source| ObserveFileError::Open {
                path: active_path.display().to_string(),
                source,
            })?
            .len();

        Ok(Self {
            dir: dir.to_path_buf(),
            max_bytes: max_bytes.max(1),
            state: Mutex::new(FileSinkState {
                file,
                size,
                next_sequence,
            }),
            write_errors: AtomicU64::new(0),
        })
    }

    /// Anzahl der intern behandelten Schreib-, Serialisierungs- oder
    /// Rotationsfehler seit `open()`.
    ///
    /// # Returns
    /// Die kumulierte Fehlerzahl (Vertrag A.3, zweite Festlegung:
    /// `record`/`flush` scheitern nie sichtbar, zählen aber intern).
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_observe_file::FileSink;
    /// use std::path::Path;
    /// let sink = FileSink::open(Path::new("/tmp/harw-telemetry-2"), 1_048_576).unwrap();
    /// assert_eq!(sink.write_error_count(), 0);
    /// ```
    #[must_use]
    pub fn write_error_count(&self) -> u64 {
        self.write_errors.load(Ordering::Relaxed)
    }

    /// Schreibt `line` an die aktive Datei an und rotiert bei Bedarf.
    fn write_record(&self, line: &[u8]) -> Result<(), ObserveFileError> {
        let mut state = self.lock_state();
        state
            .file
            .write_all(line)
            .map_err(|source| ObserveFileError::Open {
                path: self.dir.join(ACTIVE_FILE_NAME).display().to_string(),
                source,
            })?;
        state.size += line.len() as u64;
        if state.size >= self.max_bytes {
            self.rotate_locked(&mut state)?;
        }
        Ok(())
    }

    /// Schließt die aktive Datei, benennt sie um, schreibt Digest und
    /// `.blake3`-Beidatei, und öffnet eine neue leere aktive Datei.
    fn rotate_locked(&self, state: &mut FileSinkState) -> Result<(), ObserveFileError> {
        let active_path = self.dir.join(ACTIVE_FILE_NAME);
        state
            .file
            .flush()
            .map_err(|source| ObserveFileError::Rotate {
                path: active_path.display().to_string(),
                source,
            })?;

        let sequence = state.next_sequence;
        state.next_sequence += 1;
        let rotated_name = format!("{ROTATED_PREFIX}{sequence:010}{ROTATED_SUFFIX}");
        let rotated_path = self.dir.join(&rotated_name);

        fs::rename(&active_path, &rotated_path).map_err(|source| ObserveFileError::Rotate {
            path: active_path.display().to_string(),
            source,
        })?;

        let contents = fs::read(&rotated_path).map_err(|source| ObserveFileError::Rotate {
            path: rotated_path.display().to_string(),
            source,
        })?;
        let digest = blake3::hash(&contents);
        let digest_path = PathBuf::from(format!("{}{DIGEST_SUFFIX}", rotated_path.display()));
        fs::write(&digest_path, digest.to_hex().as_bytes()).map_err(|source| {
            ObserveFileError::Rotate {
                path: digest_path.display().to_string(),
                source,
            }
        })?;

        let new_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&active_path)
            .map_err(|source| ObserveFileError::Rotate {
                path: active_path.display().to_string(),
                source,
            })?;
        state.file = new_file;
        state.size = 0;
        Ok(())
    }

    /// Nimmt die interne Sperre; bei Vergiftung (ein anderer Thread ist
    /// unter Halten der Sperre panisch geworden) wird der zuletzt bekannte
    /// Zustand trotzdem übernommen, damit ein einzelner Panik-Aufrufer
    /// nicht jeden künftigen `record()`-Aufruf blockiert.
    fn lock_state(&self) -> std::sync::MutexGuard<'_, FileSinkState> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl TelemetrySink for FileSink {
    fn record(&self, key: &MetricKey, value: MetricValue, labels: &[(FieldName, FieldValue)]) {
        let record = build_jsonl_record(key, value, labels);
        let Ok(mut line) = serde_json::to_vec(&record) else {
            self.write_errors.fetch_add(1, Ordering::Relaxed);
            return;
        };
        line.push(b'\n');

        if self.write_record(&line).is_err() {
            self.write_errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn flush(&self) {
        // Kein `mut`: `File::sync_all` nimmt `&self`, der Zustand wird hier
        // nur gelesen.
        let state = self.lock_state();
        if state.file.sync_all().is_err() {
            self.write_errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn name(&self) -> &'static str {
        "file"
    }
}

/// Baut die serialisierbare Zeile für einen `record()`-Aufruf.
///
/// Liest die Systemuhr über `jiff::Timestamp::now()`: `record()` ist eine
/// I/O-tragende Sink-Methode ohne injizierbaren `now`-Parameter (die
/// Trait-Signatur ist in Vertrag A.3 eingefroren), keine "reine Funktion"
/// im Sinn der Zeit-Doktrin im Kopfteil von `docs/aw-contract-master.md`.
fn build_jsonl_record(
    key: &MetricKey,
    value: MetricValue,
    labels: &[(FieldName, FieldValue)],
) -> JsonlRecord {
    JsonlRecord {
        timestamp: jiff::Timestamp::now().to_string(),
        metric: key.name,
        kind: metric_kind_str(key.kind),
        unit: unit_str(key.unit),
        cardinality: cardinality_string(key.cardinality),
        value: metric_value_to_jsonl(value),
        labels: labels
            .iter()
            .map(|(name, value)| (name.as_str().to_owned(), field_value_to_jsonl(value)))
            .collect(),
    }
}

fn metric_kind_str(kind: MetricKind) -> &'static str {
    match kind {
        MetricKind::Counter => "counter",
        MetricKind::Gauge => "gauge",
        MetricKind::Histogram => "histogram",
    }
}

fn unit_str(unit: Unit) -> &'static str {
    match unit {
        Unit::Count => "count",
        Unit::Bytes => "bytes",
        Unit::Seconds => "seconds",
        Unit::Ratio => "ratio",
        Unit::Tokens => "tokens",
        Unit::Celsius => "celsius",
    }
}

fn cardinality_string(cardinality: Cardinality) -> String {
    match cardinality {
        Cardinality::Bounded(limit) => format!("bounded:{limit}"),
        Cardinality::Single => "single".to_owned(),
    }
}

fn metric_value_to_jsonl(value: MetricValue) -> JsonlValue {
    match value {
        MetricValue::Count(v) => JsonlValue::U64(v),
        MetricValue::Gauge(v) => JsonlValue::F64(v),
        MetricValue::Observation(v) => JsonlValue::F64(v),
    }
}

fn field_value_to_jsonl(value: &FieldValue) -> JsonlValue {
    match value {
        FieldValue::Str(s) => JsonlValue::Str((*s).to_owned()),
        FieldValue::Owned(s) => JsonlValue::Str(s.to_owned()),
        FieldValue::I64(v) => JsonlValue::I64(*v),
        FieldValue::U64(v) => JsonlValue::U64(*v),
        FieldValue::F64(v) => JsonlValue::F64(*v),
        FieldValue::Bool(v) => JsonlValue::Bool(*v),
    }
}

/// Bestimmt die nächste freie Rotationsnummer, indem das Verzeichnis nach
/// vorhandenen `rotated-*.jsonl`-Dateien durchsucht wird — ein Neustart
/// über demselben Verzeichnis darf keine bestehende Datei überschreiben.
fn next_rotation_sequence(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut max_seen: Option<u64> = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(digits) = name
            .strip_prefix(ROTATED_PREFIX)
            .and_then(|rest| rest.strip_suffix(ROTATED_SUFFIX))
        else {
            continue;
        };
        if let Ok(seq) = digits.parse::<u64>() {
            max_seen = Some(max_seen.map_or(seq, |m| m.max(seq)));
        }
    }
    max_seen.map_or(0, |m| m + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use tempfile::tempdir;

    fn key(name: &'static str) -> MetricKey {
        MetricKey {
            name,
            kind: MetricKind::Counter,
            unit: Unit::Count,
            labels: &[],
            cardinality: Cardinality::Single,
        }
    }

    #[test]
    fn test_open_creates_directory_and_active_file() -> TestResult {
        let dir = tempdir().map_err(ctx("Temp-Verzeichnis anlegen"))?;
        let target = dir.path().join("nested");
        let sink = FileSink::open(&target, 1_048_576).map_err(ctx("FileSink öffnen"))?;
        assert!(target.join(ACTIVE_FILE_NAME).exists());
        assert_eq!(sink.name(), "file");
        Ok(())
    }

    #[test]
    fn test_record_appends_readable_jsonl_line() -> TestResult {
        let dir = tempdir().map_err(ctx("Temp-Verzeichnis anlegen"))?;
        let sink = FileSink::open(dir.path(), 1_048_576).map_err(ctx("FileSink öffnen"))?;
        sink.record(
            &key("jobs_total"),
            MetricValue::Count(3),
            &[(harw_observe::field!("host"), FieldValue::Str("a"))],
        );
        sink.flush();

        let contents = fs::read_to_string(dir.path().join(ACTIVE_FILE_NAME))
            .map_err(ctx("aktive Datei lesen"))?;
        let line = contents
            .lines()
            .next()
            .ok_or(crate::test_support::TestError::Missing("erste Zeile"))?;
        let parsed: serde_json::Value =
            serde_json::from_str(line).map_err(ctx("Zeile als JSON parsen"))?;
        assert_eq!(parsed["metric"], "jobs_total");
        assert_eq!(parsed["value"], 3);
        assert_eq!(parsed["labels"][0][0], "host");
        assert_eq!(parsed["labels"][0][1], "a");
        Ok(())
    }

    #[test]
    fn test_record_rotates_after_max_bytes_and_writes_digest_sidecar() -> TestResult {
        let dir = tempdir().map_err(ctx("Temp-Verzeichnis anlegen"))?;
        let sink = FileSink::open(dir.path(), 1).map_err(ctx("FileSink öffnen"))?;
        sink.record(&key("a"), MetricValue::Count(1), &[]);

        let rotated: Vec<_> = fs::read_dir(dir.path())
            .map_err(ctx("Verzeichnis lesen"))?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(ROTATED_PREFIX) && name.ends_with(ROTATED_SUFFIX))
            .collect();
        assert_eq!(
            rotated.len(),
            1,
            "expected exactly one rotated file, got {rotated:?}"
        );

        let rotated_path = dir.path().join(&rotated[0]);
        let digest_path = dir.path().join(format!("{}{DIGEST_SUFFIX}", rotated[0]));
        assert!(digest_path.exists());

        let expected_digest =
            blake3::hash(&fs::read(&rotated_path).map_err(ctx("rotierte Datei lesen"))?)
                .to_hex()
                .to_string();
        let stored_digest = fs::read_to_string(&digest_path).map_err(ctx("Digest-Datei lesen"))?;
        assert_eq!(stored_digest, expected_digest);

        let active_len = fs::metadata(dir.path().join(ACTIVE_FILE_NAME))
            .map_err(ctx("aktive Datei-Metadaten lesen"))?
            .len();
        assert_eq!(
            active_len, 0,
            "active file must be empty right after rotation"
        );
        Ok(())
    }

    #[test]
    fn test_open_resumes_sequence_after_restart() -> TestResult {
        let dir = tempdir().map_err(ctx("Temp-Verzeichnis anlegen"))?;
        {
            let sink = FileSink::open(dir.path(), 1).map_err(ctx("FileSink öffnen"))?;
            sink.record(&key("a"), MetricValue::Count(1), &[]);
        }
        let sink2 = FileSink::open(dir.path(), 1).map_err(ctx("FileSink erneut öffnen"))?;
        sink2.record(&key("b"), MetricValue::Count(2), &[]);

        let mut sequences: Vec<u64> = fs::read_dir(dir.path())
            .map_err(ctx("Verzeichnis lesen"))?
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.strip_prefix(ROTATED_PREFIX)
                    .and_then(|rest| rest.strip_suffix(ROTATED_SUFFIX))
                    .and_then(|digits| digits.parse::<u64>().ok())
            })
            .collect();
        sequences.sort_unstable();
        assert_eq!(
            sequences,
            vec![0, 1],
            "sequence numbers must not collide across restarts"
        );
        Ok(())
    }

    #[test]
    fn test_write_error_count_starts_at_zero_and_stays_zero_on_success() -> TestResult {
        let dir = tempdir().map_err(ctx("Temp-Verzeichnis anlegen"))?;
        let sink = FileSink::open(dir.path(), 1_048_576).map_err(ctx("FileSink öffnen"))?;
        assert_eq!(sink.write_error_count(), 0);
        sink.record(&key("x"), MetricValue::Count(1), &[]);
        assert_eq!(sink.write_error_count(), 0);
        Ok(())
    }

    #[test]
    fn test_name_is_file() -> TestResult {
        let dir = tempdir().map_err(ctx("Temp-Verzeichnis anlegen"))?;
        let sink = FileSink::open(dir.path(), 1_048_576).map_err(ctx("FileSink öffnen"))?;
        assert_eq!(sink.name(), "file");
        Ok(())
    }

    #[test]
    fn test_file_sink_is_send_sync_debug() {
        fn assert_bounds<T: Send + Sync + std::fmt::Debug>() {}
        assert_bounds::<FileSink>();
    }
}
