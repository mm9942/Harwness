//! Durable child-lease records for restart-safe orchestrator reconciliation.
//!
//! A child handoff is not merely an in-memory task: the parent/call
//! correlation must survive a process restart so expiry can be delivered once
//! to the correct waiting parent. Active leases are claimed by an atomic
//! rename to `expired`; completed leases are retained as an audit record.
//!
//! Jede Zustandsänderung, die die Einmal-Zustellungsgarantie trägt (Admit,
//! Expiry-Claim, Complete), synct zusätzlich zum Dateiinhalt auch das
//! Elternverzeichnis, damit ihr Verzeichniseintrag einen Stromausfall
//! übersteht — sonst könnte ein Kind nach einem Absturz ein zweites Mal
//! übernommen werden.
//!
//! A-STORE (F-179): `admit` schreibt nicht mehr per `create_new` direkt ins
//! Ziel, sondern atomar über `persist_noclobber` (temp + no-replace-rename);
//! eine leere oder halbe `.active.json` kann nicht mehr entstehen. Defekte
//! Altdateien (undekodierbar, unsicherer `child`, Name ≠ Inhalt) blockieren
//! `active`/`claim_expired` nicht mehr: sie werden nach
//! `<name>.corrupt-<ts>` verschoben und mit `warn!` übersprungen.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{SessionStoreError, SessionStoreResult};
use crate::store::{persist_noclobber, quarantine_file};
use fs4::FileExt;
use harw_observe::TraceContext;
use harw_types::{SessionId, ToolCallId};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildLeaseRecord {
    pub child: SessionId,
    pub parent: SessionId,
    pub handoff_call_id: ToolCallId,
    pub role: String,
    pub depth: u32,
    pub admitted_at: Timestamp,
    pub lease_expires_at: Timestamp,
    /// The trace context under which this child handoff was admitted.
    ///
    /// Optional because `ChildLeaseRecord` is an existing on-disk file
    /// format: leases admitted before trace propagation existed carry no
    /// `trace` field at all, and those `.active.json` / `.expired.json` /
    /// `.completed.json` files must keep deserializing. A `None` here
    /// therefore means "this lease predates trace introduction", not "this
    /// handoff happened without a trace" — a lease admitted after
    /// introduction always carries `Some`. Absent from the JSON entirely
    /// (not `null`) when unset, so a legacy file's byte-for-byte shape is
    /// never rewritten just by being read back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<TraceContext>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildLeaseCompletionRecord {
    pub lease: ChildLeaseRecord,
    pub completed_at: Timestamp,
}

/// File-backed lease state. A store can be reconstructed on startup and
/// scanned before any parent sessions are resumed.
pub struct ChildLeaseStore {
    root: PathBuf,
}

impl ChildLeaseStore {
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.join("child-leases"),
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Persists an admission before the child is made runnable. Child IDs are
    /// globally unique, so an existing terminal record is also a hard error.
    pub fn admit(&self, record: &ChildLeaseRecord) -> SessionStoreResult<()> {
        let path = self.active_path(&record.child)?;
        self.ensure_root()?;
        let expired = self.expired_path(&record.child)?;
        let completed = self.completed_path(&record.child)?;
        if is_regular_file(&expired)? || is_regular_file(&completed)? {
            return Err(SessionStoreError::ChildLeaseAlreadyExists {
                child: record.child.clone(),
            });
        }
        if path_exists(&expired)? || path_exists(&completed)? {
            return Err(non_regular_lease_path_error());
        }
        let bytes = serde_json::to_vec(record)?;
        // F-179: temp + no-replace-rename statt `create_new` + `write_all` ins
        // Ziel; `persist_noclobber` synct Datei und Elternverzeichnis. Das
        // Kernel-seitige „genau ein Gewinner“ bleibt erhalten.
        match persist_noclobber(&path, &bytes) {
            Ok(()) => Ok(()),
            Err(SessionStoreError::PersistTargetExists { .. }) => {
                if is_regular_file(&path)? {
                    Err(SessionStoreError::ChildLeaseAlreadyExists {
                        child: record.child.clone(),
                    })
                } else {
                    Err(non_regular_lease_path_error())
                }
            }
            Err(error) => Err(error),
        }
    }

    /// Lists unclaimed active leases. A completion record wins over a stale
    /// active file left by a crash between durable completion and cleanup.
    pub fn active(&self) -> SessionStoreResult<Vec<ChildLeaseRecord>> {
        self.records_with_suffix(".active.json")
    }

    /// Atomically claims every active lease whose deadline has elapsed. A
    /// second process or post-restart scan sees the `.expired.json` record and
    /// cannot deliver the same expiry a second time.
    pub fn claim_expired(&self, now: Timestamp) -> SessionStoreResult<Vec<ChildLeaseRecord>> {
        self.ensure_root()?;
        let lock = self.lock()?;
        let result = (|| {
            let mut claimed = Vec::new();
            for entry in std::fs::read_dir(&self.root)? {
                let entry = entry?;
                let path = entry.path();
                if !has_suffix(&path, ".active.json") || !is_regular_file(&path)? {
                    continue;
                }
                let Some(lease) = self.load_scanned(&path, "active")? else {
                    continue;
                };
                if is_regular_file(&self.completed_path(&lease.child)?)?
                    || now < lease.lease_expires_at
                {
                    continue;
                }
                let expired = self.expired_path(&lease.child)?;
                if path_exists(&expired)? {
                    continue;
                }
                match std::fs::rename(&path, expired) {
                    Ok(()) => claimed.push(lease),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(SessionStoreError::Io(error)),
                }
            }
            if !claimed.is_empty() {
                // Der rename() aktiv -> expired ist der Übergang, auf dem die
                // Einmal-Zustellung beruht. Ohne Sync des Elternverzeichnisses
                // könnte er nach einem Absturz zurückfallen: der Eintrag
                // erschiene wieder als aktiv, obwohl die Expiry bereits an
                // den Parent ausgeliefert wurde — derselbe Datensatz würde
                // ein zweites Mal beansprucht. Ein Sync pro Aufruf genügt,
                // weil alle betroffenen Dateien im selben Verzeichnis
                // (`self.root`) liegen.
                sync_parent_directory(&self.root)?;
            }
            Ok(claimed)
        })();
        unlock(lock, result)
    }

    /// Marks a child terminal after its result was durably delivered to its
    /// parent. The completion file is written first; recovery therefore never
    /// reclaims a lease if the process crashes before source cleanup.
    pub fn complete(
        &self,
        child: &SessionId,
        completed_at: Timestamp,
    ) -> SessionStoreResult<ChildLeaseCompletionRecord> {
        self.ensure_root()?;
        let lock = self.lock()?;
        let result = (|| {
            let completed = self.completed_path(child)?;
            if is_regular_file(&completed)? {
                return Err(SessionStoreError::ChildLeaseAlreadyCompleted {
                    child: child.clone(),
                });
            }
            if path_exists(&completed)? {
                return Err(non_regular_lease_path_error());
            }
            let active = self.active_path(child)?;
            let expired = self.expired_path(child)?;
            let source = if is_regular_file(&active)? {
                active
            } else if is_regular_file(&expired)? {
                expired
            } else {
                return Err(SessionStoreError::ChildLeaseNotFound {
                    child: child.clone(),
                });
            };
            let lease = read_lease(&source)?;
            let completion = ChildLeaseCompletionRecord {
                lease,
                completed_at,
            };
            persist_json(&completed, &completion)?;
            // Bewusst kein Sync des Elternverzeichnisses nach dieser
            // Löschung: die Completion-Datei ist an dieser Stelle bereits
            // durabel und gewinnt laut `records_with_suffix` gegenüber einem
            // liegen gebliebenen active/expired-Datensatz. Übersteht der
            // Verzeichniseintrag dieser Löschung einen Absturz nicht, bleibt
            // höchstens eine verwaiste Quelldatei zurück — kein
            // Korrektheitsproblem, nur Aufräumbedarf.
            match std::fs::remove_file(source) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(SessionStoreError::Io(error)),
            }
            Ok(completion)
        })();
        unlock(lock, result)
    }

    /// Verlängert eine noch nicht abgelaufene aktive Lease durabel auf
    /// `max(aktueller Wert, now + lease_seconds)`.
    ///
    /// # Beschreibung
    /// Welle FANIN-K. Liest die aktuelle `.active.json`, berechnet die neue
    /// Fälligkeit und schreibt den Datensatz nur bei tatsächlicher
    /// Verlängerung zurück: Inhalt in eine Geschwister-Temp-Datei, `sync_all`,
    /// dann ein **ersetzendes** `rename` über die vorhandene aktive Datei.
    /// Im Unterschied zu [`Self::admit`]/[`Self::complete`], die über
    /// [`persist_noclobber`] nie ein vorhandenes Ziel überschreiben dürfen,
    /// MUSS diese Schreiboperation das bestehende Ziel ersetzen — dafür
    /// nutzt sie `NamedTempFile::persist` (dieselbe zugrunde liegende
    /// Bibliothek, nur ohne dessen No-Replace-Verhalten) statt
    /// [`persist_noclobber`]. Das Elternverzeichnis wird nicht erneut
    /// gesynct: der Verzeichniseintrag selbst ändert sich nicht, nur der
    /// Inode-Inhalt hinter ihm — ein Stromausfall zeigt danach höchstens die
    /// alte oder die neue Fälligkeit, nie eine leere Datei.
    ///
    /// # Arguments
    /// - `child` (`&SessionId`): das zu verlängernde Kind.
    /// - `lease_seconds` (`i64`): die konfigurierte Lease-Dauer.
    /// - `now` (`Timestamp`): Referenzzeitpunkt.
    ///
    /// # Returns
    /// `Ok(())` immer bei erfolgreichem Abschluss — auch wenn keine
    /// Verlängerung nötig war (unbekanntes Kind, bereits abgelaufene Lease,
    /// oder die berechnete neue Fälligkeit liegt nicht nach der
    /// eingetragenen). Idempotent, damit ein Aufrufer eine verspätete
    /// Verlängerung nicht als Fehler behandeln muss.
    ///
    /// # Errors
    /// [`SessionStoreError::Io`] bei Lese-/Schreib-/Sync-Fehlern (inklusive
    /// eines kontendierten Lease-Locks), [`SessionStoreError::Serde`] bei
    /// einem undekodierbaren aktiven Datensatz.
    ///
    /// # Concurrency
    /// Nimmt denselben Verzeichnis-Lock wie [`Self::claim_expired`]/
    /// [`Self::complete`]; von beliebig vielen Threads/Prozessen aufrufbar,
    /// serialisiert über diesen Lock.
    /// Reattaches a durable child lease after the owning WorkId has been
    /// claimed by a recovery worker.
    ///
    /// Unlike [`Self::renew`], this trusted recovery seam may move an
    /// already-expired record back to active. It never creates a lease from
    /// nothing and never revives a completed child. Before writing it verifies
    /// the immutable correlation fields against `expected`; the caller may
    /// choose a fresh expiry but cannot substitute another child/parent/tool
    /// handoff/role/depth/trace.
    ///
    /// The WorkId fence lives in JobStore and is intentionally not duplicated
    /// here; callers MUST hold that job lease before invoking this method.
    pub fn recover(
        &self,
        expected: &ChildLeaseRecord,
    ) -> SessionStoreResult<()> {
        self.ensure_root()?;
        let lock = self.lock()?;
        let result = (|| {
            let completed = self.completed_path(&expected.child)?;
            if is_regular_file(&completed)? {
                return Err(SessionStoreError::ChildLeaseAlreadyCompleted {
                    child: expected.child.clone(),
                });
            }
            if path_exists(&completed)? {
                return Err(non_regular_lease_path_error());
            }

            let active = self.active_path(&expected.child)?;
            let expired = self.expired_path(&expected.child)?;
            let (source, was_expired) = if is_regular_file(&active)? {
                (active.clone(), false)
            } else if is_regular_file(&expired)? {
                (expired.clone(), true)
            } else {
                return Err(SessionStoreError::ChildLeaseNotFound {
                    child: expected.child.clone(),
                });
            };
            let stored = read_lease(&source)?;
            let mismatch = if stored.child != expected.child {
                Some("child")
            } else if stored.parent != expected.parent {
                Some("parent")
            } else if stored.handoff_call_id != expected.handoff_call_id {
                Some("handoff_call_id")
            } else if stored.role != expected.role {
                Some("role")
            } else if stored.depth != expected.depth {
                Some("depth")
            } else if stored.trace != expected.trace {
                Some("trace")
            } else {
                None
            };
            if let Some(field) = mismatch {
                return Err(SessionStoreError::ChildLeaseRecoveryMismatch {
                    child: expected.child.clone(),
                    detail: format!("{field} does not match the persisted admission"),
                });
            }

            let mut recovered = stored;
            recovered.lease_expires_at = expected.lease_expires_at;
            let bytes = serde_json::to_vec(&recovered)?;
            let parent = active.parent().ok_or_else(|| {
                SessionStoreError::Io(std::io::Error::other("child lease path has no parent"))
            })?;
            let mut temp = NamedTempFile::new_in(parent).map_err(SessionStoreError::Io)?;
            temp.write_all(&bytes).map_err(SessionStoreError::Io)?;
            temp.as_file().sync_all().map_err(SessionStoreError::Io)?;
            temp.persist(&active)
                .map_err(|error| SessionStoreError::Io(error.error))?;
            if was_expired && expired.exists() {
                std::fs::remove_file(&expired)?;
                File::open(parent)?.sync_all()?;
            }
            Ok(())
        })();
        unlock(lock, result)
    }

    pub fn renew(
        &self,
        child: &SessionId,
        lease_seconds: i64,
        now: Timestamp,
    ) -> SessionStoreResult<()> {
        self.ensure_root()?;
        let lock = self.lock()?;
        let result = (|| {
            let active = self.active_path(child)?;
            if !is_regular_file(&active)? {
                // Unbekannt oder bereits terminal: keine aktive Lease zum
                // Verlängern, kein Fehler.
                return Ok(());
            }
            let mut record = read_lease(&active)?;
            if now >= record.lease_expires_at {
                // Bereits abgelaufen: kein Wiederbeleben durch eine späte
                // Verlängerung.
                return Ok(());
            }
            let Ok(candidate) = now.checked_add(SignedDuration::from_secs(lease_seconds)) else {
                return Ok(());
            };
            if candidate <= record.lease_expires_at {
                return Ok(());
            }
            record.lease_expires_at = candidate;
            let bytes = serde_json::to_vec(&record)?;
            let parent = active.parent().ok_or_else(|| {
                SessionStoreError::Io(std::io::Error::other("child lease path has no parent"))
            })?;
            let mut temp = NamedTempFile::new_in(parent).map_err(SessionStoreError::Io)?;
            temp.write_all(&bytes).map_err(SessionStoreError::Io)?;
            temp.as_file().sync_all().map_err(SessionStoreError::Io)?;
            temp.persist(&active)
                .map_err(|error| SessionStoreError::Io(error.error))?;
            Ok(())
        })();
        unlock(lock, result)
    }

    fn records_with_suffix(&self, suffix: &str) -> SessionStoreResult<Vec<ChildLeaseRecord>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut records = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if !has_suffix(&path, suffix) || !is_regular_file(&path)? {
                continue;
            }
            let state = suffix
                .strip_prefix('.')
                .and_then(|rest| rest.strip_suffix(".json"))
                .unwrap_or(suffix);
            let Some(lease) = self.load_scanned(&path, state)? else {
                continue;
            };
            if !is_regular_file(&self.completed_path(&lease.child)?)? {
                records.push(lease);
            }
        }
        records.sort_by(|left, right| left.child.as_str().cmp(right.child.as_str()));
        Ok(records)
    }

    // Liest einen beim Scan gefundenen Datensatz. Defekte Dateien (undekodierbar,
    // unsicherer `child`, Dateiname passt nicht zum Inhalt) werden in Quarantäne
    // verschoben und als `None` übersprungen, statt den Scan abzubrechen.
    fn load_scanned(
        &self,
        path: &Path,
        state: &str,
    ) -> SessionStoreResult<Option<ChildLeaseRecord>> {
        let detail = match read_lease(path) {
            Ok(lease) => match self.path(&lease.child, state) {
                Ok(expected) if expected == path => return Ok(Some(lease)),
                Ok(_) => "lease file name does not match its child".to_owned(),
                Err(error) => error.to_string(),
            },
            Err(SessionStoreError::Serde(error)) => error.to_string(),
            Err(SessionStoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        match quarantine_file(path) {
            Ok(Some(quarantine)) => tracing::warn!(
                path = %path.display(),
                quarantine = %quarantine.display(),
                detail = %detail,
                "corrupt child lease record quarantined"
            ),
            Ok(None) => {}
            Err(error) => tracing::warn!(
                path = %path.display(),
                error = %error,
                detail = %detail,
                "corrupt child lease record could not be quarantined; skipped"
            ),
        }
        Ok(None)
    }

    fn ensure_root(&self) -> SessionStoreResult<()> {
        std::fs::create_dir_all(&self.root)?;
        Ok(())
    }

    fn lock(&self) -> SessionStoreResult<File> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join(".lock"))?;
        FileExt::try_lock(&file).map_err(|error| match error {
            fs4::TryLockError::WouldBlock => SessionStoreError::ChildLeaseLockContended,
            fs4::TryLockError::Error(error) => SessionStoreError::Io(error),
        })?;
        Ok(file)
    }

    fn active_path(&self, child: &SessionId) -> SessionStoreResult<PathBuf> {
        self.path(child, "active")
    }

    fn expired_path(&self, child: &SessionId) -> SessionStoreResult<PathBuf> {
        self.path(child, "expired")
    }

    fn completed_path(&self, child: &SessionId) -> SessionStoreResult<PathBuf> {
        self.path(child, "completed")
    }

    fn path(&self, child: &SessionId, state: &str) -> SessionStoreResult<PathBuf> {
        Ok(self
            .root
            .join(safe_component(child.as_str())?)
            .with_extension(format!("{state}.json")))
    }
}

fn unlock<T>(lock: File, result: SessionStoreResult<T>) -> SessionStoreResult<T> {
    let unlock = FileExt::unlock(&lock).map_err(SessionStoreError::Io);
    match (result, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

// No-Clobber-Persist über den crate-weiten Helfer (`store::persist_noclobber`):
// temp-Datei, `sync_all`, no-replace-rename, Eltern-fsync. Das Elternverzeichnis
// wird gesynct, weil erst dieser Verzeichniseintrag die Completion-Datei
// sichtbar macht; `records_with_suffix` behandelt eine vorhandene Completion als
// Autorität über aktive/expired-Datensätze.
fn persist_json<T: Serialize>(path: &Path, value: &T) -> SessionStoreResult<()> {
    let bytes = serde_json::to_vec(value)?;
    persist_noclobber(path, &bytes)
}

/// Synct das Elternverzeichnis einer soeben angelegten, ersetzten oder
/// umbenannten Lease-Datei. Ein `sync_all()` auf der Datei sichert nur ihren
/// Inhalt; der Verzeichniseintrag, der sie überhaupt auffindbar macht, liegt
/// im Verzeichnis-Inode und muss separat gesynct werden — sonst kann eine
/// vollständig geschriebene Datei nach einem Stromausfall trotzdem nicht
/// existieren. Ein Verzeichnis wird zum Lesen geöffnet (`File::open`), nicht
/// zum Schreiben; `sync_all()` erfasst dabei genau den Verzeichniseintrag.
/// Ein Sync-Fehler wird propagiert statt verschluckt, exakt wie in
/// `store.rs::sync_parent_directory`.
// Die eine Fassung liegt in `crate::durability`; sie stand vorher in fünf
// Dateien byte-gleich. Warum ein Eltern-fsync nötig ist, steht dort.
use crate::durability::sync_parent_directory;

fn has_suffix(path: &Path, suffix: &str) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(suffix))
}

fn path_exists(path: &Path) -> SessionStoreResult<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(SessionStoreError::Io(error)),
    }
}

fn non_regular_lease_path_error() -> SessionStoreError {
    SessionStoreError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "child lease path is occupied by a non-regular file",
    ))
}

fn is_regular_file(path: &Path) -> SessionStoreResult<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(SessionStoreError::Io(error)),
    }
}

fn read_lease(path: &Path) -> SessionStoreResult<ChildLeaseRecord> {
    if !is_regular_file(path)? {
        return Err(SessionStoreError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "child lease record is not a regular file",
        )));
    }
    let bytes = std::fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn safe_component(value: &str) -> SessionStoreResult<&str> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(SessionStoreError::UnsafeChildLeasePath(value.to_owned()));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use harw_types::SessionId;

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    fn lease(child: &str, expires_in_seconds: i64) -> TestResult<ChildLeaseRecord> {
        let admitted_at = Timestamp::now();
        Ok(ChildLeaseRecord {
            child: SessionId::from_str(child),
            parent: SessionId::from_str("parent-1"),
            handoff_call_id: ToolCallId::from_str("call-1"),
            role: "worker".to_owned(),
            depth: 1,
            admitted_at,
            lease_expires_at: admitted_at
                .checked_add(jiff::SignedDuration::from_secs(expires_in_seconds))
                .map_err(ctx("lease: admitted_at + expires_in_seconds"))?,
            trace: None,
        })
    }

    fn sample_trace() -> TraceContext {
        TraceContext {
            trace_id: "a".repeat(32),
            span_id: "b".repeat(16),
            parent_span_id: None,
        }
    }

    #[test]
    fn lease_record_with_trace_context_roundtrips_through_serde() -> TestResult {
        let original = ChildLeaseRecord {
            trace: Some(sample_trace()),
            ..lease("child-traced", 60)?
        };

        let json = serde_json::to_string(&original)?;
        let decoded: ChildLeaseRecord = serde_json::from_str(&json)?;

        assert_eq!(decoded, original);
        Ok(())
    }

    #[test]
    fn lease_record_without_trace_context_roundtrips_through_serde() -> TestResult {
        let original = lease("child-untraced", 60)?;
        assert_eq!(original.trace, None);

        let json = serde_json::to_string(&original)?;
        let decoded: ChildLeaseRecord = serde_json::from_str(&json)?;

        assert_eq!(decoded, original);
        assert_eq!(decoded.trace, None);
        Ok(())
    }

    #[test]
    fn a_lease_record_without_trace_omits_the_field_from_its_json() -> TestResult {
        let json = serde_json::to_string(&lease("child-untraced", 60)?)?;

        assert!(
            !json.contains("\"trace\""),
            "a None trace must be absent, not serialized as `\"trace\":null`: {json}"
        );
        Ok(())
    }

    /// The most important test in this module: a `ChildLeaseRecord` written
    /// to disk before trace propagation existed has exactly this shape — no
    /// `trace` key anywhere. The literal below is hand-written, not derived
    /// from `lease(..)`, so it independently pins the pre-trace file format
    /// rather than testing today's serializer against itself.
    #[test]
    fn a_pre_trace_lease_file_still_deserializes_with_no_trace() -> TestResult {
        let legacy = r#"{
            "child": "child-legacy",
            "parent": "parent-legacy",
            "handoff_call_id": "call-legacy",
            "role": "worker",
            "depth": 1,
            "admitted_at": "2024-01-01T00:00:00Z",
            "lease_expires_at": "2024-01-01T01:00:00Z"
        }"#;

        let decoded: ChildLeaseRecord = serde_json::from_str(legacy).map_err(ctx(
            "a pre-trace ChildLeaseRecord file must still deserialize",
        ))?;

        assert_eq!(decoded.trace, None);
        assert_eq!(decoded.child, SessionId::from_str("child-legacy"));
        assert_eq!(decoded.depth, 1);
        Ok(())
    }

    #[test]
    fn admitted_lease_with_trace_context_roundtrips_through_the_real_store_path() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = ChildLeaseStore::new(temp.path());
        let admitted = ChildLeaseRecord {
            trace: Some(sample_trace()),
            ..lease("child-store-traced", 60)?
        };

        store.admit(&admitted)?;

        assert_eq!(store.active()?, vec![admitted]);
        Ok(())
    }

    #[test]
    fn admitted_lease_without_trace_context_roundtrips_through_the_real_store_path() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = ChildLeaseStore::new(temp.path());
        let admitted = lease("child-store-untraced", 60)?;
        assert_eq!(admitted.trace, None);

        store.admit(&admitted)?;

        let active = store.active()?;
        assert_eq!(active, vec![admitted]);
        assert_eq!(active[0].trace, None);
        Ok(())
    }

    #[test]
    fn expiry_claim_is_durable_and_single_delivery() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = ChildLeaseStore::new(temp.path());
        let expired = lease("child-expired", -1)?;
        let active = lease("child-active", 60)?;
        store.admit(&expired)?;
        store.admit(&active)?;

        let first = store.claim_expired(Timestamp::now())?;
        assert_eq!(first, vec![expired.clone()]);
        assert!(store.claim_expired(Timestamp::now())?.is_empty());
        assert_eq!(store.active()?, vec![active]);
        Ok(())
    }

    #[test]
    fn completion_prevents_later_recovery_delivery_and_is_auditable() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = ChildLeaseStore::new(temp.path());
        let record = lease("child-1", -1)?;
        store.admit(&record)?;
        let completion = store.complete(&record.child, Timestamp::now())?;
        assert_eq!(completion.lease, record);
        assert!(store.claim_expired(Timestamp::now())?.is_empty());
        assert!(matches!(
            store.complete(&completion.lease.child, Timestamp::now()),
            Err(SessionStoreError::ChildLeaseAlreadyCompleted { .. })
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn list_and_expiry_ignore_symlinked_active_leases() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = ChildLeaseStore::new(temp.path());
        std::fs::create_dir_all(store.root())?;
        let linked_record = lease("child-linked", -1)?;
        let target = temp.path().join("outside-active.json");
        std::fs::write(&target, serde_json::to_vec(&linked_record)?)?;
        let link = store.root().join("child-linked.active.json");
        symlink(&target, &link)?;

        assert!(store.active()?.is_empty());
        assert!(store.claim_expired(Timestamp::now())?.is_empty());
        assert!(std::fs::symlink_metadata(&link)?.file_type().is_symlink());
        assert!(target.exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn expiry_does_not_replace_a_symlinked_terminal_record() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = ChildLeaseStore::new(temp.path());
        let record = lease("child-expired", -1)?;
        store.admit(&record)?;
        let target = temp.path().join("outside-expired.json");
        std::fs::write(&target, b"untrusted")?;
        let expired = store.expired_path(&record.child)?;
        symlink(&target, &expired)?;

        assert!(store.claim_expired(Timestamp::now())?.is_empty());
        assert!(
            std::fs::symlink_metadata(&expired)?
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&target)?, b"untrusted");
        assert!(store.active_path(&record.child)?.exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn completion_does_not_read_symlinked_lease_records() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = ChildLeaseStore::new(temp.path());
        std::fs::create_dir_all(store.root())?;
        let record = lease("child-linked", -1)?;
        let target = temp.path().join("outside-active.json");
        std::fs::write(&target, serde_json::to_vec(&record)?)?;
        let active = store.active_path(&record.child)?;
        symlink(&target, &active)?;

        assert!(matches!(
            store.complete(&record.child, Timestamp::now()),
            Err(SessionStoreError::ChildLeaseNotFound { .. })
        ));
        assert!(std::fs::symlink_metadata(&active)?.file_type().is_symlink());
        assert!(target.exists());
        Ok(())
    }

    #[test]
    fn sync_parent_directory_reports_a_missing_directory_without_panicking() -> TestResult {
        let temp = tempfile::tempdir()?;
        let missing = temp.path().join("does-not-exist");

        assert!(matches!(
            sync_parent_directory(&missing),
            Err(SessionStoreError::Io(_))
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_completion_record_neither_suppresses_nor_is_replaced() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = ChildLeaseStore::new(temp.path());
        let record = lease("child-completion-link", 60)?;
        store.admit(&record)?;
        let target = temp.path().join("outside-completed.json");
        std::fs::write(&target, b"untrusted")?;
        let completed = store.completed_path(&record.child)?;
        symlink(&target, &completed)?;

        assert_eq!(store.active()?, vec![record.clone()]);
        assert!(matches!(
            store.complete(&record.child, Timestamp::now()),
            Err(SessionStoreError::Io(_))
        ));
        assert!(
            std::fs::symlink_metadata(&completed)?
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&target)?, b"untrusted");
        assert!(store.active_path(&record.child)?.exists());
        Ok(())
    }
}
