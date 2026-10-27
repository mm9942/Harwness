//! Durabler Marker für nicht verbuchte Job-Usage (Review P1 zu #151).
//!
//! # Description
//! Der Job-Worker persistiert die verbrauchte Usage eines laufenden Jobs über
//! `JobStore::record_usage`. Schlägt dieser Schreibzugriff aus einem anderen
//! Grund als einem Lease-Verlust fehl (Platte voll, Dateisystemfehler, …), ist
//! der Verbrauch real, aber nicht im Job-Datensatz: Stirbt der Worker danach,
//! lädt der Reclaim die ältere `job.usage` und gäbe dasselbe Budget ein
//! zweites Mal frei (`INTENT_CYCLE_MAX_ATTEMPTS > 1`).
//!
//! Dieses Modul legt für genau diesen Fall einen eigenen, kleinen Marker neben
//! dem Job-Store ab (`<job-root>/usage-unresolved/<work-id>.json`). Der
//! nächste Claim verrechnet den Marker zuerst mit der gespeicherten Usage und
//! entfernt ihn erst danach; lässt er sich nicht lesen oder verrechnen, endet
//! der Job fehlgeschlagen statt mit frischem Budget weiterzulaufen.
//!
//! # Concurrency
//! Kein Dateilock: der Schreiber ist der Lease-Halter des Jobs, und innerhalb
//! eines Prozesses serialisiert der Flush-Gate des Ledgers die Zugriffe. Jede
//! einzelne Datei wird atomar ersetzt (temp + rename + fsync des
//! Verzeichnisses), ein Absturz hinterlässt also nie eine halbe Marker-Datei.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use harw_job_runtime::BudgetUsage;
use serde::{Deserialize, Serialize};

/// Verzeichnisname unterhalb der Job-Store-Wurzel (`JobStore::root()`).
pub(crate) const USAGE_UNRESOLVED_DIR: &str = "usage-unresolved";

/// Aktuelle Schemaversion der Marker-Datei.
const SCHEMA_VERSION: u32 = 1;

/// Obergrenze für [`UnresolvedUsage::reason`] in Zeichen.
pub(crate) const MAX_REASON_CHARS: usize = 200;

/// Ersatztext, wenn nach dem Begrenzen kein Grund übrig bleibt.
const UNSPECIFIED_REASON: &str = "unspecified";

/// Endung einer Marker-Datei.
const MARKER_EXTENSION: &str = "json";

/// Endung einer unfertigen Temp-Datei.
const TEMP_SUFFIX: &str = ".tmp";

/// Dateimodus neuer Marker-Dateien (nur Eigentümer), wie der Job-Store.
#[cfg(unix)]
const FILE_MODE: u32 = 0o600;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Verbrauchte, aber nicht im Job-Datensatz festgeschriebene Usage eines Jobs.
///
/// # Description
/// `usage` ist ein absoluter Stand (Gesamtverbrauch des Jobs), kein Delta; so
/// lässt er sich mit der gespeicherten Usage per Feld-Maximum verrechnen
/// ([`UsageMarkerStore::merged`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UnresolvedUsage {
    /// Schemaversion der Datei; derzeit `1`.
    pub schema_version: u32,
    /// Job, zu dem der Verbrauch gehört.
    pub work_id: String,
    /// Lease-Epoche des Workers, der den Verbrauch gemeldet hat.
    pub epoch: u64,
    /// Absoluter Verbrauchsstand zum Zeitpunkt des fehlgeschlagenen Schreibens.
    pub usage: BudgetUsage,
    /// Zeitpunkt, zu dem der Marker geschrieben wurde.
    pub recorded_at: jiff::Timestamp,
    /// Kurzer Grund des fehlgeschlagenen Schreibens (begrenzt, keine Secrets).
    pub reason: String,
}

impl UnresolvedUsage {
    /// Baut einen Marker mit aktueller Schemaversion und Zeitstempel.
    ///
    /// # Description
    /// `reason` wird auf die erste Zeile ohne Steuerzeichen und höchstens
    /// [`MAX_REASON_CHARS`] Zeichen begrenzt; ein leerer Rest wird zu
    /// `"unspecified"`.
    pub(crate) fn new(work_id: &str, epoch: u64, usage: BudgetUsage, reason: &str) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            work_id: work_id.to_owned(),
            epoch,
            usage,
            recorded_at: jiff::Timestamp::now(),
            reason: bounded_reason(reason),
        }
    }
}

/// Dateibasierter Speicher der Marker unter `<job-root>/usage-unresolved/`.
#[derive(Debug, Clone)]
pub(crate) struct UsageMarkerStore {
    dir: PathBuf,
}

impl UsageMarkerStore {
    /// Marker-Speicher unterhalb der Job-Store-Wurzel `job_root`
    /// (`JobStore::root()`); legt noch nichts auf der Platte an.
    pub(crate) fn in_job_root(job_root: &Path) -> Self {
        Self {
            dir: job_root.join(USAGE_UNRESOLVED_DIR),
        }
    }

    /// Schreibt den Marker atomar (temp + rename + fsync des Verzeichnisses);
    /// überschreibt nur mit größerer oder gleicher Usage (max pro Feld).
    ///
    /// # Description
    /// Existiert bereits ein Marker für dieselbe Arbeits-ID, wird dessen
    /// Usage feldweise mit der neuen vereinigt ([`Self::merged`]); die
    /// gespeicherte Usage wird also nie kleiner. Trägt der vorhandene Marker
    /// eine höhere Epoche, behält er Epoche, Grund und Zeitstempel (ein
    /// verspäteter Schreiber einer alten Lease darf die Herkunft nicht
    /// zurückdrehen). Der Grund wird vor dem Schreiben begrenzt.
    ///
    /// # Errors
    /// Unsichere Arbeits-ID, unbekannte Schemaversion, ein vorhandener, aber
    /// unlesbarer Marker (er wird nicht überschrieben — der nächste Claim
    /// schlägt an ihm ohnehin geschlossen fehl) oder jeder I/O-Fehler.
    pub(crate) fn write(&self, marker: &UnresolvedUsage) -> Result<(), String> {
        let path = self.path_for(&marker.work_id)?;
        if marker.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "usage marker: unsupported schema version {} (expected {SCHEMA_VERSION})",
                marker.schema_version
            ));
        }
        let mut next = marker.clone();
        next.reason = bounded_reason(&marker.reason);
        if let Some(existing) = self.read(&marker.work_id)? {
            next.usage = Self::merged(&existing.usage, &next.usage);
            if existing.epoch > next.epoch {
                next.epoch = existing.epoch;
                next.reason = existing.reason;
                next.recorded_at = existing.recorded_at;
            }
        }
        let bytes = serde_json::to_vec(&next)
            .map_err(|error| format!("usage marker: encoding failed: {error}"))?;
        std::fs::create_dir_all(&self.dir).map_err(|error| {
            format!(
                "usage marker: cannot create '{}': {error}",
                self.dir.display()
            )
        })?;
        // Das frisch angelegte Verzeichnis selbst muss im Elternverzeichnis
        // dauerhaft sein, sonst kann ein Absturz den ganzen Marker verlieren.
        if let Some(parent) = self.dir.parent() {
            sync_dir(parent)?;
        }
        self.write_atomic(&path, &marker.work_id, &bytes)
    }

    /// Liest den Marker zu `work_id`.
    ///
    /// # Returns
    /// `Ok(None)`, wenn kein Marker existiert; sonst der Marker.
    ///
    /// # Errors
    /// Fail-closed: unsichere Arbeits-ID, ein Symlink oder Nicht-Datei an der
    /// Stelle des Markers, nicht lesbare Datei, ungültiges JSON, unbekannte
    /// Felder oder Schemaversion, oder eine Marker-Datei, die zu einer anderen
    /// Arbeits-ID gehört. Ein kaputter Marker darf nie wie "kein Marker"
    /// aussehen, sonst bekäme der Job frisches Budget.
    pub(crate) fn read(&self, work_id: &str) -> Result<Option<UnresolvedUsage>, String> {
        let path = self.path_for(work_id)?;
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "usage marker for '{work_id}' must not be a symbolic link"
                ));
            }
            Ok(metadata) if !metadata.is_file() => {
                return Err(format!(
                    "usage marker for '{work_id}' is not a regular file"
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "usage marker for '{work_id}' is not accessible: {error}"
                ));
            }
        }
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "usage marker for '{work_id}' cannot be read: {error}"
                ));
            }
        };
        let marker: UnresolvedUsage = serde_json::from_slice(&bytes)
            .map_err(|error| format!("usage marker for '{work_id}' is malformed: {error}"))?;
        if marker.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "usage marker for '{work_id}' has unsupported schema version {}",
                marker.schema_version
            ));
        }
        if marker.work_id != work_id {
            return Err(format!(
                "usage marker for '{work_id}' belongs to another job"
            ));
        }
        Ok(Some(marker))
    }

    /// Entfernt den Marker zu `work_id`; idempotent (fehlender Marker ist `Ok`).
    ///
    /// # Errors
    /// Unsichere Arbeits-ID oder ein I/O-Fehler beim Löschen bzw. beim
    /// fsync des Verzeichnisses. Ein verbliebener Marker ist unschädlich: er
    /// wird beim nächsten Claim erneut per Maximum verrechnet.
    pub(crate) fn remove(&self, work_id: &str) -> Result<(), String> {
        let path = self.path_for(work_id)?;
        match std::fs::remove_file(&path) {
            Ok(()) => sync_dir(&self.dir),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "usage marker for '{work_id}' cannot be removed: {error}"
            )),
        }
    }

    /// Feldweises Maximum zweier Usages.
    #[must_use]
    pub(crate) fn merged(a: &BudgetUsage, b: &BudgetUsage) -> BudgetUsage {
        BudgetUsage {
            tokens: a.tokens.max(b.tokens),
            wall: a.wall.max(b.wall),
            tool_calls: a.tool_calls.max(b.tool_calls),
        }
    }

    // Pfad der Marker-Datei; die Arbeits-ID muss ein sicherer einzelner
    // Pfadbestandteil sein (dieselbe Regel wie die Datensätze des Job-Stores).
    fn path_for(&self, work_id: &str) -> Result<PathBuf, String> {
        harw_job_store::validate_id(work_id).map_err(|error| format!("usage marker: {error}"))?;
        Ok(self.dir.join(format!("{work_id}.{MARKER_EXTENSION}")))
    }

    // temp + fsync + rename + fsync des Verzeichnisses; die Temp-Datei wird bei
    // jedem Fehler wieder entfernt.
    fn write_atomic(&self, path: &Path, work_id: &str, bytes: &[u8]) -> Result<(), String> {
        let temp = self.dir.join(temp_name(work_id));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        restrict_mode(&mut options);
        let mut file = options.open(&temp).map_err(|error| {
            format!(
                "usage marker: cannot create temp file '{}': {error}",
                temp.display()
            )
        })?;
        let written = file.write_all(bytes).and_then(|()| file.sync_all());
        drop(file);
        if let Err(error) = written {
            remove_quietly(&temp);
            return Err(format!("usage marker: cannot write temp file: {error}"));
        }
        if let Err(error) = std::fs::rename(&temp, path) {
            remove_quietly(&temp);
            return Err(format!("usage marker: cannot publish marker: {error}"));
        }
        sync_dir(&self.dir)
    }
}

// Erste Zeile, ohne Steuerzeichen, höchstens `MAX_REASON_CHARS` Zeichen.
fn bounded_reason(reason: &str) -> String {
    let first_line = reason.lines().next().unwrap_or("");
    let bounded: String = first_line
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_REASON_CHARS)
        .collect();
    if bounded.trim().is_empty() {
        UNSPECIFIED_REASON.to_owned()
    } else {
        bounded
    }
}

// `.<id>.<pid>-<nanos>-<zähler>.tmp`: beginnt mit einem Punkt und endet nicht
// auf `.json`, ist also nie ein lesbarer Marker.
fn temp_name(work_id: &str) -> String {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let stamp = jiff::Timestamp::now().as_nanosecond();
    format!(
        ".{work_id}.{}-{stamp}-{counter}{TEMP_SUFFIX}",
        std::process::id()
    )
}

#[cfg(unix)]
fn restrict_mode(options: &mut std::fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(FILE_MODE);
}

#[cfg(not(unix))]
fn restrict_mode(_options: &mut std::fs::OpenOptions) {}

fn remove_quietly(path: &Path) {
    if let Err(error) = std::fs::remove_file(path) {
        tracing::debug!(path = %path.display(), error = %error, "usage marker temp file not removed");
    }
}

// fsync des Verzeichnisses, damit Anlegen, Umbenennen und Löschen eines
// Eintrags einen Absturz überstehen. Windows kennt kein fsync auf
// Verzeichnisse; dort bleibt es bei der atomaren Ersetzung.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<(), String> {
    std::fs::File::open(dir)
        .and_then(|handle| handle.sync_all())
        .map_err(|error| {
            format!(
                "usage marker: cannot sync directory '{}': {error}",
                dir.display()
            )
        })
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx, some_or};
    use jiff::SignedDuration;

    fn temp_dir() -> TestResult<tempfile::TempDir> {
        tempfile::tempdir().map_err(ctx("temp directory"))
    }

    fn usage(tokens: u64, tool_calls: u32) -> BudgetUsage {
        BudgetUsage {
            tokens,
            wall: SignedDuration::from_secs(5),
            tool_calls,
        }
    }

    fn marker(work_id: &str, epoch: u64, tokens: u64, tool_calls: u32) -> UnresolvedUsage {
        UnresolvedUsage::new(
            work_id,
            epoch,
            usage(tokens, tool_calls),
            "store: disk full",
        )
    }

    fn marker_file(root: &Path, work_id: &str) -> PathBuf {
        root.join(USAGE_UNRESOLVED_DIR)
            .join(format!("{work_id}.json"))
    }

    fn expect_err<T: std::fmt::Debug>(result: Result<T, String>, what: &str) -> TestResult<String> {
        match result {
            Err(message) => Ok(message),
            Ok(value) => Err(TestError::Unexpected(format!(
                "{what}: expected an error, got {value:?}"
            ))),
        }
    }

    #[test]
    fn test_write_and_read_round_trip() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        let written = marker("job-1", 3, 1_234, 2);
        store.write(&written).map_err(TestError::Unexpected)?;
        let read = some_or(
            store.read("job-1").map_err(TestError::Unexpected)?,
            "marker present after write",
        )?;
        if read != written {
            return Err(TestError::Unexpected(format!(
                "round trip changed the marker: {written:?} -> {read:?}"
            )));
        }
        // Nur die Marker-Datei liegt im Verzeichnis, keine Temp-Reste.
        let names: Vec<String> = std::fs::read_dir(root.path().join(USAGE_UNRESOLVED_DIR))
            .map_err(ctx("read marker dir"))?
            .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .collect::<Result<Vec<String>, std::io::Error>>()
            .map_err(ctx("marker dir entry"))?;
        if names != ["job-1.json".to_owned()] {
            return Err(TestError::Unexpected(format!(
                "unexpected directory content: {names:?}"
            )));
        }
        Ok(())
    }

    #[test]
    fn test_read_of_a_missing_marker_is_none() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        // Weder Verzeichnis noch Datei vorhanden.
        if store
            .read("job-1")
            .map_err(TestError::Unexpected)?
            .is_some()
        {
            return Err(TestError::Unexpected(
                "a marker appeared out of nowhere".to_owned(),
            ));
        }
        // Verzeichnis vorhanden, Datei nicht.
        store
            .write(&marker("job-2", 1, 1, 0))
            .map_err(TestError::Unexpected)?;
        if store
            .read("job-1")
            .map_err(TestError::Unexpected)?
            .is_some()
        {
            return Err(TestError::Unexpected(
                "a marker of another job was returned".to_owned(),
            ));
        }
        Ok(())
    }

    #[test]
    fn test_remove_is_idempotent() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        // Ohne Verzeichnis und ohne Datei.
        store.remove("job-1").map_err(TestError::Unexpected)?;
        store
            .write(&marker("job-1", 1, 10, 0))
            .map_err(TestError::Unexpected)?;
        store.remove("job-1").map_err(TestError::Unexpected)?;
        store.remove("job-1").map_err(TestError::Unexpected)?;
        if store
            .read("job-1")
            .map_err(TestError::Unexpected)?
            .is_some()
        {
            return Err(TestError::Unexpected(
                "the marker survived its removal".to_owned(),
            ));
        }
        Ok(())
    }

    #[test]
    fn test_malformed_json_fails_closed() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        std::fs::create_dir_all(root.path().join(USAGE_UNRESOLVED_DIR))
            .map_err(ctx("create marker dir"))?;
        let path = marker_file(root.path(), "job-1");
        std::fs::write(&path, b"{ not json").map_err(ctx("write garbage"))?;
        expect_err(store.read("job-1"), "garbage marker")?;
        // Ein Schreibversuch überschreibt die kaputte Datei nicht.
        expect_err(
            store.write(&marker("job-1", 1, 5, 0)),
            "write over a garbage marker",
        )?;
        let after = std::fs::read(&path).map_err(ctx("re-read garbage"))?;
        if after != b"{ not json" {
            return Err(TestError::Unexpected(
                "the unreadable marker was overwritten".to_owned(),
            ));
        }
        Ok(())
    }

    #[test]
    fn test_unknown_fields_schema_and_foreign_job_fail_closed() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        store
            .write(&marker("job-1", 1, 5, 0))
            .map_err(TestError::Unexpected)?;
        let path = marker_file(root.path(), "job-1");
        let text = std::fs::read_to_string(&path).map_err(ctx("read marker"))?;

        // Unbekanntes Feld (deny_unknown_fields).
        let with_extra = text.replacen('{', "{\"surprise\":true,", 1);
        std::fs::write(&path, with_extra).map_err(ctx("write extra field"))?;
        expect_err(store.read("job-1"), "unknown field")?;

        // Unbekannte Schemaversion.
        let future_schema = text.replacen("\"schema_version\":1", "\"schema_version\":2", 1);
        std::fs::write(&path, future_schema).map_err(ctx("write future schema"))?;
        expect_err(store.read("job-1"), "future schema")?;

        // Marker einer anderen Arbeits-ID unter diesem Dateinamen.
        let foreign = text.replacen("\"work_id\":\"job-1\"", "\"work_id\":\"job-2\"", 1);
        std::fs::write(&path, foreign).map_err(ctx("write foreign marker"))?;
        expect_err(store.read("job-1"), "foreign marker")?;
        Ok(())
    }

    #[test]
    fn test_write_keeps_the_larger_usage_per_field() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        store
            .write(&marker("job-1", 1, 100, 1))
            .map_err(TestError::Unexpected)?;
        // Weniger Tokens, mehr Tool-Aufrufe: jedes Feld behält sein Maximum.
        store
            .write(&marker("job-1", 1, 50, 5))
            .map_err(TestError::Unexpected)?;
        let read = some_or(
            store.read("job-1").map_err(TestError::Unexpected)?,
            "marker present",
        )?;
        if read.usage.tokens != 100 || read.usage.tool_calls != 5 {
            return Err(TestError::Unexpected(format!(
                "field-wise maximum expected (100 tokens, 5 tool calls), got {:?}",
                read.usage
            )));
        }
        // Größere Usage ersetzt die kleinere.
        store
            .write(&marker("job-1", 1, 400, 5))
            .map_err(TestError::Unexpected)?;
        let read = some_or(
            store.read("job-1").map_err(TestError::Unexpected)?,
            "marker present",
        )?;
        if read.usage.tokens != 400 {
            return Err(TestError::Unexpected(format!(
                "larger usage was not kept: {:?}",
                read.usage
            )));
        }
        Ok(())
    }

    #[test]
    fn test_older_epoch_does_not_rewind_the_origin() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        store
            .write(&marker("job-1", 7, 10, 0))
            .map_err(TestError::Unexpected)?;
        store
            .write(&marker("job-1", 6, 99, 0))
            .map_err(TestError::Unexpected)?;
        let read = some_or(
            store.read("job-1").map_err(TestError::Unexpected)?,
            "marker present",
        )?;
        if read.epoch != 7 || read.usage.tokens != 99 {
            return Err(TestError::Unexpected(format!(
                "expected epoch 7 with the merged 99 tokens, got {read:?}"
            )));
        }
        Ok(())
    }

    #[test]
    fn test_merged_is_the_field_wise_maximum() {
        let merged = UsageMarkerStore::merged(
            &BudgetUsage {
                tokens: 10,
                wall: SignedDuration::from_secs(9),
                tool_calls: 1,
            },
            &BudgetUsage {
                tokens: 3,
                wall: SignedDuration::from_secs(20),
                tool_calls: 4,
            },
        );
        assert_eq!(
            merged,
            BudgetUsage {
                tokens: 10,
                wall: SignedDuration::from_secs(20),
                tool_calls: 4,
            }
        );
    }

    #[test]
    fn test_reason_is_bounded_to_one_short_line() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        let long = format!(
            "{}\nsecond line with details",
            "x".repeat(MAX_REASON_CHARS * 3)
        );
        let mut raw = marker("job-1", 1, 1, 0);
        // Direkt gebaut, am Konstruktor vorbei: `write` begrenzt trotzdem.
        raw.reason = long;
        store.write(&raw).map_err(TestError::Unexpected)?;
        let read = some_or(
            store.read("job-1").map_err(TestError::Unexpected)?,
            "marker present",
        )?;
        if read.reason.chars().count() != MAX_REASON_CHARS || read.reason.contains('\n') {
            return Err(TestError::Unexpected(format!(
                "reason not bounded: {} chars",
                read.reason.chars().count()
            )));
        }
        if UnresolvedUsage::new("job-1", 1, usage(1, 0), "\n\t ").reason != UNSPECIFIED_REASON {
            return Err(TestError::Unexpected(
                "an empty reason must become 'unspecified'".to_owned(),
            ));
        }
        Ok(())
    }

    #[test]
    fn test_unsafe_work_ids_are_rejected() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        for id in ["", "../escape", "a/b", "a\\b", "a.b", "job\n1", "job 1"] {
            expect_err(store.read(id), "read with unsafe id")?;
            expect_err(store.remove(id), "remove with unsafe id")?;
            expect_err(store.write(&marker(id, 1, 1, 0)), "write with unsafe id")?;
        }
        // Nichts ist außerhalb des Marker-Verzeichnisses entstanden.
        let entries = std::fs::read_dir(root.path())
            .map_err(ctx("read job root"))?
            .count();
        if entries != 0 {
            return Err(TestError::Unexpected(format!(
                "{entries} entries were created for rejected ids"
            )));
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_symlinked_marker_fails_closed() -> TestResult {
        let root = temp_dir()?;
        let store = UsageMarkerStore::in_job_root(root.path());
        store
            .write(&marker("job-2", 1, 5, 0))
            .map_err(TestError::Unexpected)?;
        std::os::unix::fs::symlink(
            marker_file(root.path(), "job-2"),
            marker_file(root.path(), "job-1"),
        )
        .map_err(ctx("create symlink"))?;
        expect_err(store.read("job-1"), "symlinked marker")?;
        Ok(())
    }
}
