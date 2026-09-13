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

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use fs4::FileExt;
use harw_observe::TraceContext;
use harw_types::{SessionId, ToolCallId};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::error::{SessionStoreError, SessionStoreResult};

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
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if is_regular_file(&path)? {
                    return Err(SessionStoreError::ChildLeaseAlreadyExists {
                        child: record.child.clone(),
                    });
                }
                return Err(non_regular_lease_path_error());
            }
            Err(error) => return Err(SessionStoreError::Io(error)),
        };
        file.write_all(&bytes)?;
        file.sync_all()?;
        // Das Elternverzeichnis wird gesynct, weil `create_new` nur die neue
        // Datei selbst sichert, nicht ihren Verzeichniseintrag. Bliebe dieser
        // Eintrag nach einem Stromausfall im Cache stehen, könnte ein
        // zweiter `admit()`-Aufruf für dasselbe Kind unbemerkt ein zweites
        // Mal durchgehen — genau die doppelte Übernahme, die die
        // Einmal-Zustellungsgarantie ausschließen soll.
        sync_parent_directory(&self.root)?;
        Ok(())
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
                let lease = read_lease(&path)?;
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
            let lease = read_lease(&path)?;
            if !is_regular_file(&self.completed_path(&lease.child)?)? {
                records.push(lease);
            }
        }
        records.sort_by(|left, right| left.child.as_str().cmp(right.child.as_str()));
        Ok(records)
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

fn persist_json<T: Serialize>(path: &Path, value: &T) -> SessionStoreResult<()> {
    let parent = path.parent().ok_or_else(|| {
        SessionStoreError::Io(std::io::Error::other("child lease path has no parent"))
    })?;
    let mut temp = NamedTempFile::new_in(parent)?;
    serde_json::to_writer(temp.as_file_mut(), value)?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(path)
        .map_err(|error| SessionStoreError::Io(error.error))?;
    // Das Elternverzeichnis wird gesynct, weil erst dieser Verzeichnis-
    // eintrag die Completion-Datei sichtbar macht, nicht ihr Inhalt.
    // `records_with_suffix` behandelt eine vorhandene Completion als
    // Autorität über aktive/expired-Datensätze; fiele der Eintrag nach
    // einem Absturz zurück, erschiene ein bereits abgeschlossenes Kind
    // wieder als offen und könnte erneut beansprucht werden.
    sync_parent_directory(parent)?;
    Ok(())
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
    use harw_types::SessionId;

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    fn lease(child: &str, expires_in_seconds: i64) -> ChildLeaseRecord {
        let admitted_at = Timestamp::now();
        ChildLeaseRecord {
            child: SessionId::from_str(child),
            parent: SessionId::from_str("parent-1"),
            handoff_call_id: ToolCallId::from_str("call-1"),
            role: "worker".to_owned(),
            depth: 1,
            admitted_at,
            lease_expires_at: admitted_at
                .checked_add(jiff::SignedDuration::from_secs(expires_in_seconds))
                .unwrap(),
            trace: None,
        }
    }

    fn sample_trace() -> TraceContext {
        TraceContext {
            trace_id: "a".repeat(32),
            span_id: "b".repeat(16),
            parent_span_id: None,
        }
    }

    #[test]
    fn lease_record_with_trace_context_roundtrips_through_serde() {
        let original = ChildLeaseRecord {
            trace: Some(sample_trace()),
            ..lease("child-traced", 60)
        };

        let json = serde_json::to_string(&original).unwrap();
        let decoded: ChildLeaseRecord = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, original);
    }

    #[test]
    fn lease_record_without_trace_context_roundtrips_through_serde() {
        let original = lease("child-untraced", 60);
        assert_eq!(original.trace, None);

        let json = serde_json::to_string(&original).unwrap();
        let decoded: ChildLeaseRecord = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, original);
        assert_eq!(decoded.trace, None);
    }

    #[test]
    fn a_lease_record_without_trace_omits_the_field_from_its_json() {
        let json = serde_json::to_string(&lease("child-untraced", 60)).unwrap();

        assert!(
            !json.contains("\"trace\""),
            "a None trace must be absent, not serialized as `\"trace\":null`: {json}"
        );
    }

    /// The most important test in this module: a `ChildLeaseRecord` written
    /// to disk before trace propagation existed has exactly this shape — no
    /// `trace` key anywhere. The literal below is hand-written, not derived
    /// from `lease(..)`, so it independently pins the pre-trace file format
    /// rather than testing today's serializer against itself.
    #[test]
    fn a_pre_trace_lease_file_still_deserializes_with_no_trace() {
        let legacy = r#"{
            "child": "child-legacy",
            "parent": "parent-legacy",
            "handoff_call_id": "call-legacy",
            "role": "worker",
            "depth": 1,
            "admitted_at": "2024-01-01T00:00:00Z",
            "lease_expires_at": "2024-01-01T01:00:00Z"
        }"#;

        let decoded: ChildLeaseRecord = serde_json::from_str(legacy)
            .expect("a pre-trace ChildLeaseRecord file must still deserialize");

        assert_eq!(decoded.trace, None);
        assert_eq!(decoded.child, SessionId::from_str("child-legacy"));
        assert_eq!(decoded.depth, 1);
    }

    #[test]
    fn admitted_lease_with_trace_context_roundtrips_through_the_real_store_path() {
        let temp = tempfile::tempdir().unwrap();
        let store = ChildLeaseStore::new(temp.path());
        let admitted = ChildLeaseRecord {
            trace: Some(sample_trace()),
            ..lease("child-store-traced", 60)
        };

        store.admit(&admitted).unwrap();

        assert_eq!(store.active().unwrap(), vec![admitted]);
    }

    #[test]
    fn admitted_lease_without_trace_context_roundtrips_through_the_real_store_path() {
        let temp = tempfile::tempdir().unwrap();
        let store = ChildLeaseStore::new(temp.path());
        let admitted = lease("child-store-untraced", 60);
        assert_eq!(admitted.trace, None);

        store.admit(&admitted).unwrap();

        let active = store.active().unwrap();
        assert_eq!(active, vec![admitted]);
        assert_eq!(active[0].trace, None);
    }

    #[test]
    fn expiry_claim_is_durable_and_single_delivery() {
        let temp = tempfile::tempdir().unwrap();
        let store = ChildLeaseStore::new(temp.path());
        let expired = lease("child-expired", -1);
        let active = lease("child-active", 60);
        store.admit(&expired).unwrap();
        store.admit(&active).unwrap();

        let first = store.claim_expired(Timestamp::now()).unwrap();
        assert_eq!(first, vec![expired.clone()]);
        assert!(store.claim_expired(Timestamp::now()).unwrap().is_empty());
        assert_eq!(store.active().unwrap(), vec![active]);
    }

    #[test]
    fn completion_prevents_later_recovery_delivery_and_is_auditable() {
        let temp = tempfile::tempdir().unwrap();
        let store = ChildLeaseStore::new(temp.path());
        let record = lease("child-1", -1);
        store.admit(&record).unwrap();
        let completion = store.complete(&record.child, Timestamp::now()).unwrap();
        assert_eq!(completion.lease, record);
        assert!(store.claim_expired(Timestamp::now()).unwrap().is_empty());
        assert!(matches!(
            store.complete(&completion.lease.child, Timestamp::now()),
            Err(SessionStoreError::ChildLeaseAlreadyCompleted { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn list_and_expiry_ignore_symlinked_active_leases() {
        let temp = tempfile::tempdir().unwrap();
        let store = ChildLeaseStore::new(temp.path());
        std::fs::create_dir_all(store.root()).unwrap();
        let linked_record = lease("child-linked", -1);
        let target = temp.path().join("outside-active.json");
        std::fs::write(&target, serde_json::to_vec(&linked_record).unwrap()).unwrap();
        let link = store.root().join("child-linked.active.json");
        symlink(&target, &link).unwrap();

        assert!(store.active().unwrap().is_empty());
        assert!(store.claim_expired(Timestamp::now()).unwrap().is_empty());
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn expiry_does_not_replace_a_symlinked_terminal_record() {
        let temp = tempfile::tempdir().unwrap();
        let store = ChildLeaseStore::new(temp.path());
        let record = lease("child-expired", -1);
        store.admit(&record).unwrap();
        let target = temp.path().join("outside-expired.json");
        std::fs::write(&target, b"untrusted").unwrap();
        let expired = store.expired_path(&record.child).unwrap();
        symlink(&target, &expired).unwrap();

        assert!(store.claim_expired(Timestamp::now()).unwrap().is_empty());
        assert!(std::fs::symlink_metadata(&expired)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read(&target).unwrap(), b"untrusted");
        assert!(store.active_path(&record.child).unwrap().exists());
    }

    #[cfg(unix)]
    #[test]
    fn completion_does_not_read_symlinked_lease_records() {
        let temp = tempfile::tempdir().unwrap();
        let store = ChildLeaseStore::new(temp.path());
        std::fs::create_dir_all(store.root()).unwrap();
        let record = lease("child-linked", -1);
        let target = temp.path().join("outside-active.json");
        std::fs::write(&target, serde_json::to_vec(&record).unwrap()).unwrap();
        let active = store.active_path(&record.child).unwrap();
        symlink(&target, &active).unwrap();

        assert!(matches!(
            store.complete(&record.child, Timestamp::now()),
            Err(SessionStoreError::ChildLeaseNotFound { .. })
        ));
        assert!(std::fs::symlink_metadata(&active)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(target.exists());
    }

    #[test]
    fn sync_parent_directory_reports_a_missing_directory_without_panicking() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("does-not-exist");

        assert!(matches!(
            sync_parent_directory(&missing),
            Err(SessionStoreError::Io(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_completion_record_neither_suppresses_nor_is_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let store = ChildLeaseStore::new(temp.path());
        let record = lease("child-completion-link", 60);
        store.admit(&record).unwrap();
        let target = temp.path().join("outside-completed.json");
        std::fs::write(&target, b"untrusted").unwrap();
        let completed = store.completed_path(&record.child).unwrap();
        symlink(&target, &completed).unwrap();

        assert_eq!(store.active().unwrap(), vec![record.clone()]);
        assert!(matches!(
            store.complete(&record.child, Timestamp::now()),
            Err(SessionStoreError::Io(_))
        ));
        assert!(std::fs::symlink_metadata(&completed)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read(&target).unwrap(), b"untrusted");
        assert!(store.active_path(&record.child).unwrap().exists());
    }
}
