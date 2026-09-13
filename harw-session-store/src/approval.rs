//! Durable, actor-bound, single-consumption approval records.
//!
//! Approval requests are stored separately from high-volume transcript JSONL
//! so a response can use a lock-protected read/check/write transition. A
//! request can be resolved exactly once by the same trusted actor recorded at
//! issuance; duplicate callbacks and a different channel peer are rejected.
//!
//! Sowohl Ausstellung als auch Auflösung einer Anfrage syncen neben der
//! Datei auch ihr Elternverzeichnis: eine verlorene Genehmigung ist
//! ärgerlich, eine verlorene Ablehnung, die den vorherigen Zustand
//! zurückfallen lässt, ist ein Sicherheitsproblem.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use fs4::FileExt;
use harw_types::{ApprovalActor, ItemId, ReviewDecision, SessionId, ToolCallId};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::error::{SessionStoreError, SessionStoreResult};

/// Immutable authorization captured before an approval prompt reaches a
/// channel. `actor` must match byte-for-byte when the response is consumed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub request: ItemId,
    pub session: SessionId,
    pub call_id: ToolCallId,
    pub actor: ApprovalActor,
    pub issued_at: Timestamp,
}

/// Terminal, audit-friendly result of consuming an approval request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalResolutionRecord {
    pub request: ItemId,
    pub session: SessionId,
    pub call_id: ToolCallId,
    pub actor: ApprovalActor,
    pub decision: ReviewDecision,
    pub comment: Option<String>,
    pub resolved_at: Timestamp,
}

/// Per-session file store for approval state.
pub struct ApprovalStore {
    root: PathBuf,
}

impl ApprovalStore {
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.join("approvals"),
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Durably issues a request. Reusing an existing request id is rejected;
    /// callers must create a fresh `ItemId` rather than silently overwriting
    /// authority for an old prompt.
    pub fn issue(&self, record: &ApprovalRecord) -> SessionStoreResult<()> {
        let path = self.pending_path(&record.session, &record.request)?;
        let parent = path.parent().ok_or_else(|| {
            SessionStoreError::Io(std::io::Error::other("approval path has no parent"))
        })?;
        std::fs::create_dir_all(parent)?;
        let bytes = serde_json::to_vec(record)?;
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(SessionStoreError::ApprovalAlreadyExists {
                    session: record.session.clone(),
                    request: record.request.clone(),
                });
            }
            Err(error) => return Err(SessionStoreError::Io(error)),
        };
        file.write_all(&bytes)?;
        file.sync_all()?;
        // Das Elternverzeichnis wird gesynct, weil `create_new` nur die neue
        // Datei selbst sichert, nicht ihren Verzeichniseintrag. Bliebe dieser
        // Eintrag nach einem Stromausfall im Cache stehen, könnte ein
        // zweiter `issue()`-Aufruf für dieselbe Request-ID unbemerkt eine
        // zweite Autorisierung mit abweichendem Actor anlegen, statt korrekt
        // mit `ApprovalAlreadyExists` abgewiesen zu werden.
        sync_parent_directory(parent)?;
        Ok(())
    }

    /// Atomically consumes a pending request. The lock covers checking the
    /// pending record, verifying actor identity, detecting a prior decision,
    /// and persisting the terminal resolution.
    pub fn resolve(
        &self,
        session: &SessionId,
        request: &ItemId,
        actor: &ApprovalActor,
        decision: ReviewDecision,
        comment: Option<String>,
        resolved_at: Timestamp,
    ) -> SessionStoreResult<ApprovalResolutionRecord> {
        let pending_path = self.pending_path(session, request)?;
        let resolved_path = self.resolved_path(session, request)?;
        let directory = pending_path.parent().ok_or_else(|| {
            SessionStoreError::Io(std::io::Error::other("approval path has no parent"))
        })?;
        std::fs::create_dir_all(directory)?;
        let lock = self.lock_session(session)?;

        let outcome = (|| -> SessionStoreResult<ApprovalResolutionRecord> {
            if path_is_symlink(&resolved_path)? {
                return Err(SessionStoreError::ApprovalNotFound {
                    session: session.clone(),
                    request: request.clone(),
                });
            }
            if resolved_path.exists() {
                return Err(SessionStoreError::ApprovalAlreadyResolved {
                    request: request.clone(),
                });
            }
            let bytes = match read_pending(&pending_path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Err(SessionStoreError::ApprovalNotFound {
                        session: session.clone(),
                        request: request.clone(),
                    });
                }
                Err(error) => return Err(SessionStoreError::Io(error)),
            };
            let record: ApprovalRecord = serde_json::from_slice(&bytes)?;
            if record.session != *session || record.request != *request {
                return Err(SessionStoreError::ApprovalNotFound {
                    session: session.clone(),
                    request: request.clone(),
                });
            }
            if &record.actor != actor {
                return Err(SessionStoreError::ApprovalActorMismatch {
                    request: request.clone(),
                });
            }

            let resolution = ApprovalResolutionRecord {
                request: request.clone(),
                session: session.clone(),
                call_id: record.call_id,
                actor: actor.clone(),
                decision,
                comment,
                resolved_at,
            };
            self.persist_resolution(&resolved_path, &resolution)?;
            Ok(resolution)
        })();
        let unlock = FileExt::unlock(&lock).map_err(SessionStoreError::Io);
        match (outcome, unlock) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(resolution), Ok(())) => Ok(resolution),
        }
    }

    pub fn pending(
        &self,
        session: &SessionId,
        request: &ItemId,
    ) -> SessionStoreResult<ApprovalRecord> {
        let path = self.pending_path(session, request)?;
        let bytes = read_pending(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                SessionStoreError::ApprovalNotFound {
                    session: session.clone(),
                    request: request.clone(),
                }
            } else {
                SessionStoreError::Io(error)
            }
        })?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    fn persist_resolution(
        &self,
        path: &Path,
        resolution: &ApprovalResolutionRecord,
    ) -> SessionStoreResult<()> {
        let parent = path.parent().ok_or_else(|| {
            SessionStoreError::Io(std::io::Error::other("resolution path has no parent"))
        })?;
        if path_is_symlink(path)? {
            return Err(SessionStoreError::ApprovalNotFound {
                session: resolution.session.clone(),
                request: resolution.request.clone(),
            });
        }
        let mut temp = NamedTempFile::new_in(parent)?;
        serde_json::to_writer(temp.as_file_mut(), resolution)?;
        temp.as_file().sync_all()?;
        temp.persist(path)
            .map_err(|error| SessionStoreError::Io(error.error))?;
        // Das Elternverzeichnis wird gesynct, weil der wirksame
        // Zustandswechsel (pending -> resolved) erst mit dem
        // Verzeichniseintrag des `persist()` sichtbar wird, nicht mit dem
        // Dateiinhalt. Fällt dieser Eintrag nach einem Absturz zurück,
        // erscheint die Entscheidung — Zusage oder Ablehnung — nie
        // getroffen; eine verlorene Ablehnung ist hier ein
        // Sicherheitsproblem, kein bloßer Datenverlust.
        sync_parent_directory(parent)?;
        Ok(())
    }

    fn lock_session(&self, session: &SessionId) -> SessionStoreResult<File> {
        let path = self.session_dir(session)?.join(".lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        FileExt::try_lock(&file).map_err(|error| match error {
            fs4::TryLockError::WouldBlock => SessionStoreError::LockContended {
                session: session.clone(),
            },
            fs4::TryLockError::Error(error) => SessionStoreError::Io(error),
        })?;
        Ok(file)
    }

    fn pending_path(&self, session: &SessionId, request: &ItemId) -> SessionStoreResult<PathBuf> {
        Ok(self
            .session_dir(session)?
            .join(safe_component(request.as_str())?)
            .with_extension("pending.json"))
    }

    fn resolved_path(&self, session: &SessionId, request: &ItemId) -> SessionStoreResult<PathBuf> {
        Ok(self
            .session_dir(session)?
            .join(safe_component(request.as_str())?)
            .with_extension("resolved.json"))
    }

    fn session_dir(&self, session: &SessionId) -> SessionStoreResult<PathBuf> {
        Ok(self.root.join(safe_component(session.as_str())?))
    }
}

/// Synct das Elternverzeichnis einer soeben angelegten oder ersetzten
/// Approval-Datei. Ein `sync_all()` auf der Datei sichert nur ihren Inhalt;
/// der Verzeichniseintrag, der sie überhaupt auffindbar macht, liegt im
/// Verzeichnis-Inode und muss separat gesynct werden — sonst kann eine
/// vollständig geschriebene Datei nach einem Stromausfall trotzdem nicht
/// existieren. Ein Verzeichnis wird zum Lesen geöffnet (`File::open`), nicht
/// zum Schreiben; `sync_all()` erfasst dabei genau den Verzeichniseintrag.
/// Ein Sync-Fehler wird propagiert statt verschluckt, exakt wie in
/// `store.rs::sync_parent_directory`.
// Die eine Fassung liegt in `crate::durability`; sie stand vorher in fünf
// Dateien byte-gleich. Warum ein Eltern-fsync nötig ist, steht dort.
use crate::durability::sync_parent_directory;

fn path_is_symlink(path: &Path) -> SessionStoreResult<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_symlink()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(SessionStoreError::Io(error)),
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn read_pending(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::os::unix::fs::OpenOptionsExt;

    // O_NOFOLLOW prevents a pending record from being redirected between the
    // metadata check and the read. Other Unix targets use the metadata check
    // in the fallback below.
    const O_NOFOLLOW: i32 = 0o400_000;
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(O_NOFOLLOW);
    let file = options.open(path).map_err(|error| {
        if error.raw_os_error() == Some(40) {
            std::io::Error::from(std::io::ErrorKind::NotFound)
        } else {
            error
        }
    })?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(std::io::Error::from(std::io::ErrorKind::NotFound));
    }
    let mut bytes = Vec::new();
    file.take(u64::MAX).read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn read_pending(path: &Path) -> std::io::Result<Vec<u8>> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(std::io::Error::from(std::io::ErrorKind::NotFound));
    }
    std::fs::read(path)
}

fn safe_component(value: &str) -> SessionStoreResult<&str> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(SessionStoreError::UnsafeApprovalPath(value.to_owned()));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(name: &str) -> ApprovalActor {
        ApprovalActor::Operator {
            id: name.to_owned(),
        }
    }

    fn record() -> ApprovalRecord {
        ApprovalRecord {
            request: ItemId::from_str("approval-1"),
            session: SessionId::from_str("session-1"),
            call_id: ToolCallId::from_str("call-1"),
            actor: actor("alice"),
            issued_at: Timestamp::now(),
        }
    }

    #[test]
    fn resolution_is_durable_actor_bound_and_single_use() {
        let temp = tempfile::tempdir().unwrap();
        let store = ApprovalStore::new(temp.path());
        let request = record();
        store.issue(&request).unwrap();
        assert_eq!(
            store.pending(&request.session, &request.request).unwrap(),
            request
        );

        let mismatch = store
            .resolve(
                &request.session,
                &request.request,
                &actor("mallory"),
                ReviewDecision::Approved,
                None,
                Timestamp::now(),
            )
            .unwrap_err();
        assert!(matches!(
            mismatch,
            SessionStoreError::ApprovalActorMismatch { .. }
        ));

        let resolution = store
            .resolve(
                &request.session,
                &request.request,
                &request.actor,
                ReviewDecision::ApprovedOnce,
                Some("bounded exception".to_owned()),
                Timestamp::now(),
            )
            .unwrap();
        assert_eq!(resolution.call_id, request.call_id);
        assert_eq!(resolution.decision, ReviewDecision::ApprovedOnce);

        let replay = store
            .resolve(
                &request.session,
                &request.request,
                &request.actor,
                ReviewDecision::Approved,
                None,
                Timestamp::now(),
            )
            .unwrap_err();
        assert!(matches!(
            replay,
            SessionStoreError::ApprovalAlreadyResolved { .. }
        ));
    }

    #[test]
    fn unsafe_ids_cannot_escape_approval_root() {
        let temp = tempfile::tempdir().unwrap();
        let store = ApprovalStore::new(temp.path());
        let mut request = record();
        request.request = ItemId::from_str("../escape");
        assert!(matches!(
            store.issue(&request),
            Err(SessionStoreError::UnsafeApprovalPath(_))
        ));
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
    fn symlinked_pending_record_is_not_authority() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let store = ApprovalStore::new(temp.path());
        let request = record();
        let session_dir = store.session_dir(&request.session).unwrap();
        std::fs::create_dir_all(&session_dir).unwrap();
        let external = temp.path().join("external-pending.json");
        std::fs::write(&external, serde_json::to_vec(&request).unwrap()).unwrap();
        symlink(&external, session_dir.join("approval-1.pending.json")).unwrap();

        assert!(matches!(
            store.pending(&request.session, &request.request),
            Err(SessionStoreError::ApprovalNotFound { .. })
        ));
        assert!(matches!(
            store.resolve(
                &request.session,
                &request.request,
                &request.actor,
                ReviewDecision::Approved,
                None,
                Timestamp::now(),
            ),
            Err(SessionStoreError::ApprovalNotFound { .. })
        ));
        assert_eq!(
            std::fs::read(&external).unwrap(),
            serde_json::to_vec(&request).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_resolved_record_is_not_authority_or_write_target() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let store = ApprovalStore::new(temp.path());
        let request = record();
        store.issue(&request).unwrap();
        let session_dir = store.session_dir(&request.session).unwrap();
        let external = temp.path().join("external-resolved.json");
        std::fs::write(&external, b"sentinel").unwrap();
        symlink(&external, session_dir.join("approval-1.resolved.json")).unwrap();

        assert!(matches!(
            store.resolve(
                &request.session,
                &request.request,
                &request.actor,
                ReviewDecision::Approved,
                None,
                Timestamp::now(),
            ),
            Err(SessionStoreError::ApprovalNotFound { .. })
        ));
        assert_eq!(std::fs::read(&external).unwrap(), b"sentinel");
    }
}
