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
//!
//! # C-APPR (W3): Serveruhr, TTL, Leser
//! - [`ApprovalStore::resolve`] nimmt keine Client-Zeit mehr entgegen, sondern
//!   eine [`Clock`] (F-122); `resolved_at` ist immer `clock.now()`.
//! - Jede Anfrage lebt höchstens [`ApprovalStore::ttl`] ab `issued_at`
//!   (Default [`DEFAULT_APPROVAL_TTL`]). Eine abgelaufene Anfrage ist nicht
//!   mehr auflösbar ([`SessionStoreError::ApprovalExpired`]) und erscheint
//!   nicht in [`ApprovalStore::pending_all`].
//! - [`ApprovalStore::resolution`] ist der Leser für pausierte Turns
//!   (TUI-Polling, G-011): fehlend → `None`, defekt → `Err` (fail-closed).
//! - [`ApprovalStore::pending_all`] listet offene Anfragen über alle Sessions;
//!   defekte Einträge werden mit `tracing::warn!` übersprungen.
//!
//! `ApprovalRecord::actor` ist der bei Ausstellung gebundene *Beantworter*,
//! nicht der Anfragende. Eine Selbstgenehmigungsprüfung (Anfragender ==
//! Beantworter) ist ohne neues Feld nicht möglich (Folgearbeit).
//!
//! # Nebenläufigkeit
//! `ApprovalStore` ist zustandslos bis auf Pfad und TTL (`Send + Sync`).
//! `resolve` serialisiert über einen `fs4`-Dateilock je Session;
//! `resolution` und `pending_all` lesen ohne Lock, weil `.resolved.json`
//! ausschließlich per atomarem `persist()` erscheint.
//!
//! # Examples
//! ```rust,no_run
//! use harw_session_store::ApprovalStore;
//! use harw_types::SystemClock;
//!
//! let store = ApprovalStore::new(std::path::Path::new("/tmp/harw-home"));
//! let open = store.pending_all(50, &SystemClock)?;
//! assert!(open.len() <= 50);
//! # Ok::<(), harw_session_store::SessionStoreError>(())
//! ```

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use fs4::FileExt;
#[cfg(unix)]
use harw_fsutil::OpenMode;
use harw_types::{ApprovalActor, Clock, ItemId, ReviewDecision, SessionId, TenantId, ToolCallId};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::error::{SessionStoreError, SessionStoreResult};

/// Default lifetime of an approval request, measured from `issued_at`.
///
/// # Description
/// 30 Minuten (Orchestrator-Entscheidung C-APPR). Danach ist die Anfrage
/// nicht mehr auflösbar; der pausierte Turn muss neu fragen.
pub const DEFAULT_APPROVAL_TTL: SignedDuration = SignedDuration::from_mins(30);

// Dateinamen-Suffixe für den Scan in `pending_all`; müssen zu den
// `with_extension`-Werten in `pending_path`/`resolved_path` passen.
const PENDING_SUFFIX: &str = ".pending.json";
const RESOLVED_SUFFIX: &str = ".resolved.json";

/// Immutable authorization captured before an approval prompt reaches a
/// channel. `actor` must match byte-for-byte when the response is consumed.
///
/// `tenant` ist der Mandant der auslösenden Sitzung (bzw. des durablen Jobs),
/// falls bekannt. Mandantengebundene Leser filtern damit nach der Regel von
/// `OpContext::tenant_admits` (ein Datensatz ohne Mandant ist für einen
/// Aufrufer mit Scope unsichtbar). Das Feld fehlt in der JSON-Form, solange
/// es `None` ist — Altdatensätze laden unverändert und ein neu geschriebener
/// Datensatz ohne Mandant ist byte-gleich mit dem alten Format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub request: ItemId,
    pub session: SessionId,
    pub call_id: ToolCallId,
    pub actor: ApprovalActor,
    pub issued_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<TenantId>,
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
    /// Mandant der aufgelösten Anfrage, übernommen aus [`ApprovalRecord::tenant`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<TenantId>,
}

/// Per-session file store for approval state.
///
/// # Concurrency
/// `Send + Sync`; see the module docs for the locking model.
#[derive(Debug, Clone)]
pub struct ApprovalStore {
    root: PathBuf,
    ttl: SignedDuration,
}

impl ApprovalStore {
    /// Creates a store under `<root>/approvals` with [`DEFAULT_APPROVAL_TTL`].
    ///
    /// # Arguments
    /// - `root` (`&Path`): harness home; no file is opened yet.
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self::with_ttl(root, DEFAULT_APPROVAL_TTL)
    }

    /// Creates a store under `<root>/approvals` with an explicit request TTL.
    ///
    /// # Description
    /// `ttl` wird gegen `issued_at` jeder Anfrage geprüft. Ein TTL `<= 0`
    /// lässt jede Anfrage sofort ablaufen (fail-closed), statt sie ewig
    /// gültig zu machen.
    ///
    /// # Arguments
    /// - `root` (`&Path`): harness home.
    /// - `ttl` (`SignedDuration`): lifetime of a request from `issued_at`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_session_store::ApprovalStore;
    ///
    /// let store = ApprovalStore::with_ttl(
    ///     std::path::Path::new("/tmp/harw-home"),
    ///     jiff::SignedDuration::from_mins(5),
    /// );
    /// assert_eq!(store.ttl(), jiff::SignedDuration::from_mins(5));
    /// ```
    #[must_use]
    pub fn with_ttl(root: &Path, ttl: SignedDuration) -> Self {
        Self {
            root: root.join("approvals"),
            ttl,
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the configured request TTL.
    #[must_use]
    pub fn ttl(&self) -> SignedDuration {
        self.ttl
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

    /// Atomically consumes a pending request using the server clock.
    ///
    /// # Description
    /// Der Lock deckt ab: Prüfung des Pending-Datensatzes, Actor-Identität,
    /// TTL gegen `clock.now()`, Erkennung einer früheren Entscheidung und das
    /// durable Schreiben der Auflösung. `resolved_at` ist immer
    /// `clock.now()` — nie ein vom Client gelieferter Wert (F-122).
    /// Reihenfolge der Prüfungen: bereits aufgelöst → nicht gefunden →
    /// Actor → TTL, damit ein fremder Actor nichts über den TTL-Zustand
    /// erfährt.
    ///
    /// # Arguments
    /// - `session`, `request`: Schlüssel der Anfrage.
    /// - `decision` (`ReviewDecision`), `comment` (`Option<String>`): Entscheidung.
    /// - `actor` (`&ApprovalActor`): vertrauenswürdig ermittelter Beantworter.
    /// - `clock` (`&dyn Clock`): Serveruhr.
    ///
    /// # Errors
    /// - [`SessionStoreError::ApprovalAlreadyResolved`]: bereits entschieden.
    /// - [`SessionStoreError::ApprovalNotFound`]: keine (vertrauenswürdige) Anfrage.
    /// - [`SessionStoreError::ApprovalActorMismatch`]: anderer Actor als gebunden.
    /// - [`SessionStoreError::ApprovalExpired`]: TTL abgelaufen.
    /// - [`SessionStoreError::LockContended`], `Io`, `Serde`.
    ///
    /// # Concurrency
    /// Nicht-blockierender Dateilock je Session; paralleler Aufruf liefert
    /// `LockContended`.
    pub fn resolve(
        &self,
        session: &SessionId,
        request: &ItemId,
        decision: ReviewDecision,
        comment: Option<String>,
        actor: &ApprovalActor,
        clock: &dyn Clock,
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
            let now = clock.now();
            if let Some(expires_at) = self.expiry_if_expired(&record, now) {
                return Err(SessionStoreError::ApprovalExpired {
                    request: request.clone(),
                    issued_at: record.issued_at,
                    expires_at,
                });
            }

            let resolution = ApprovalResolutionRecord {
                request: request.clone(),
                session: session.clone(),
                call_id: record.call_id,
                actor: actor.clone(),
                decision,
                comment,
                resolved_at: now,
                tenant: record.tenant,
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

    /// Reads the terminal resolution of a request, if one exists.
    ///
    /// # Description
    /// Leser für pausierte Turns (TUI-Polling). Liest ohne Lock, symlinkfest
    /// (`O_NOFOLLOW`). Fail-closed: ein Symlink, eine Nicht-Datei, nicht
    /// dekodierbarer Inhalt oder ein Datensatz mit fremdem
    /// Session-/Request-Schlüssel ist ein Fehler, nie `None`.
    ///
    /// # Returns
    /// `Ok(None)` wenn keine `.resolved.json` existiert, sonst den Datensatz.
    ///
    /// # Errors
    /// - [`SessionStoreError::ApprovalCorrupt`]: Datei vorhanden, aber nicht vertrauenswürdig.
    /// - [`SessionStoreError::UnsafeApprovalPath`]: unsichere ID.
    /// - [`SessionStoreError::Io`]: sonstiger Lesefehler.
    ///
    /// # Concurrency
    /// Lock-frei; `.resolved.json` erscheint nur per atomarem Rename.
    pub fn resolution(
        &self,
        session: &SessionId,
        request: &ItemId,
    ) -> SessionStoreResult<Option<ApprovalResolutionRecord>> {
        let path = self.resolved_path(session, request)?;
        let corrupt = |detail: String| SessionStoreError::ApprovalCorrupt {
            session: session.clone(),
            request: request.clone(),
            detail,
        };
        if path_is_symlink(&path)? {
            return Err(corrupt("resolution file is a symlink".to_owned()));
        }
        let bytes = match read_pending(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // `read_pending` meldet auch Symlink (ELOOP) und Nicht-Datei als
                // NotFound; nur ein wirklich fehlender Eintrag ist `None`.
                return match std::fs::symlink_metadata(&path) {
                    Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(other) => Err(SessionStoreError::Io(other)),
                    Ok(_) => Err(corrupt("resolution entry is not a regular file".to_owned())),
                };
            }
            Err(error) => return Err(SessionStoreError::Io(error)),
        };
        let record: ApprovalResolutionRecord =
            serde_json::from_slice(&bytes).map_err(|error| corrupt(error.to_string()))?;
        if record.session != *session || record.request != *request {
            return Err(corrupt(format!(
                "resolution is keyed to session '{}' request '{}'",
                record.session, record.request
            )));
        }
        Ok(Some(record))
    }

    /// Lists open, unexpired requests across all sessions.
    ///
    /// # Description
    /// Durchläuft `<root>/approvals/<session>/*.pending.json`. Offen heißt:
    /// keine `.resolved.json` (auch kein Symlink an dieser Stelle) und
    /// `clock.now()` vor Ablauf der TTL. Defekte Einträge (unsichere Namen,
    /// Symlinks, nicht dekodierbar, Schlüssel passt nicht zum Pfad,
    /// Lesefehler einzelner Einträge) werden mit `tracing::warn!`
    /// übersprungen statt die Liste abzubrechen. Ergebnis aufsteigend nach
    /// `issued_at` (Gleichstand: Session, dann Request), auf `limit` gekappt.
    ///
    /// # Arguments
    /// - `limit` (`usize`): Höchstzahl der Einträge; `0` liefert eine leere Liste.
    /// - `clock` (`&dyn Clock`): Serveruhr für die TTL-Prüfung.
    ///
    /// # Errors
    /// - [`SessionStoreError::Io`]: nur wenn das Wurzelverzeichnis selbst nicht
    ///   lesbar ist (ein fehlendes Wurzelverzeichnis liefert eine leere Liste).
    ///
    /// # Concurrency
    /// Lock-freier Schnappschuss; ein parallel aufgelöster Eintrag kann noch
    /// erscheinen — die Autorität bleibt `resolve`.
    pub fn pending_all(
        &self,
        limit: usize,
        clock: &dyn Clock,
    ) -> SessionStoreResult<Vec<ApprovalRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let sessions = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(SessionStoreError::Io(error)),
        };
        let now = clock.now();
        let mut open = Vec::new();
        for entry in sessions {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    tracing::warn!(
                        root = %self.root.display(),
                        error = %error,
                        "approval session entry unreadable; skipped"
                    );
                    continue;
                }
            };
            let dir = entry.path();
            let Some(session) = entry
                .file_name()
                .to_str()
                .and_then(|name| safe_component(name).ok().map(SessionId::from_str))
            else {
                tracing::warn!(
                    path = %dir.display(),
                    "approval session directory name is unsafe; skipped"
                );
                continue;
            };
            match std::fs::symlink_metadata(&dir) {
                Ok(metadata) if metadata.file_type().is_dir() => {}
                Ok(_) => {
                    tracing::warn!(
                        path = %dir.display(),
                        "approval session entry is not a real directory; skipped"
                    );
                    continue;
                }
                Err(error) => {
                    tracing::warn!(
                        path = %dir.display(),
                        error = %error,
                        "approval session directory unreadable; skipped"
                    );
                    continue;
                }
            }
            self.collect_open_in_session(&dir, &session, now, &mut open);
        }
        open.sort_by(|left, right| {
            left.issued_at
                .cmp(&right.issued_at)
                .then_with(|| left.session.as_str().cmp(right.session.as_str()))
                .then_with(|| left.request.as_str().cmp(right.request.as_str()))
        });
        open.truncate(limit);
        Ok(open)
    }

    // Sammelt offene, nicht abgelaufene Anfragen einer Session; alles Defekte
    // wird mit `warn!` übersprungen.
    fn collect_open_in_session(
        &self,
        dir: &Path,
        session: &SessionId,
        now: Timestamp,
        open: &mut Vec<ApprovalRecord>,
    ) {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                tracing::warn!(
                    path = %dir.display(),
                    error = %error,
                    "approval session directory unreadable; skipped"
                );
                return;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    tracing::warn!(
                        path = %dir.display(),
                        error = %error,
                        "approval entry unreadable; skipped"
                    );
                    continue;
                }
            };
            let file_name = entry.file_name();
            let Some(stem) = file_name
                .to_str()
                .and_then(|name| name.strip_suffix(PENDING_SUFFIX))
            else {
                // `.lock`, `.resolved.json`, Tempdateien: kein Pending-Eintrag.
                continue;
            };
            let path = entry.path();
            if safe_component(stem).is_err() {
                tracing::warn!(
                    path = %path.display(),
                    "approval request file name is unsafe; skipped"
                );
                continue;
            }
            let request = ItemId::from_str(stem);
            let resolved = dir.join(format!("{stem}{RESOLVED_SUFFIX}"));
            match std::fs::symlink_metadata(&resolved) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Ok(_) => continue,
                Err(error) => {
                    tracing::warn!(
                        path = %resolved.display(),
                        error = %error,
                        "approval resolution state unreadable; skipped"
                    );
                    continue;
                }
            }
            let bytes = match read_pending(&path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %error,
                        "approval request file unreadable or not a regular file; skipped"
                    );
                    continue;
                }
            };
            let record: ApprovalRecord = match serde_json::from_slice(&bytes) {
                Ok(record) => record,
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %error,
                        "approval request file is corrupt; skipped"
                    );
                    continue;
                }
            };
            if record.session != *session || record.request != request {
                tracing::warn!(
                    path = %path.display(),
                    record_session = %record.session,
                    record_request = %record.request,
                    "approval request is keyed to another path; skipped"
                );
                continue;
            }
            if self.expiry_if_expired(&record, now).is_some() {
                continue;
            }
            open.push(record);
        }
    }

    // `Some(expires_at)` wenn die Anfrage zu `now` abgelaufen ist. Grenze
    // inklusiv (`now >= expires_at`); Überlauf von `issued_at + ttl` gilt
    // fail-closed als abgelaufen (`Timestamp::MAX`).
    fn expiry_if_expired(&self, record: &ApprovalRecord, now: Timestamp) -> Option<Timestamp> {
        match record.issued_at.checked_add(self.ttl) {
            Ok(expires_at) => (now >= expires_at).then_some(expires_at),
            Err(_overflow) => Some(Timestamp::MAX),
        }
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
        // F-006: `lock_session` folgte zuvor Symlinks vollständig (kein
        // `O_NOFOLLOW`, keine Vorab-Prüfung). Jetzt symlinkfest über
        // `harw_fsutil::open_nofollow`.
        let file = open_lock_file_without_following_symlinks(&path)?;
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

/// Öffnet den Session-Lock ohne dem letzten Pfadglied als Symlink zu folgen.
///
/// F-006: `lock_session` folgte zuvor Symlinks vollständig — es gab weder
/// eine Vorab-Prüfung noch `O_NOFOLLOW`. Auf Unix läuft das Öffnen jetzt über
/// `harw_fsutil::open_nofollow` (plattformkorrektes `O_NOFOLLOW` über
/// `rustix::fs::OFlags::NOFOLLOW`).
fn open_lock_file_without_following_symlinks(path: &Path) -> std::io::Result<File> {
    #[cfg(unix)]
    {
        harw_fsutil::open_nofollow(
            path,
            OpenMode {
                read: true,
                write: true,
                create: true,
                create_new: false,
                truncate: false,
                append: false,
                mode: 0o666,
            },
        )
    }
    #[cfg(not(unix))]
    {
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
    }
}

/// Liest eine ausstehende Genehmigung, ohne dem letzten Pfadglied als
/// Symlink zu folgen.
///
/// F-006: das vormals architekturabhängig falsche `O_NOFOLLOW` (nur auf
/// Linux/Android verwendet, andere Unix-Zielsysteme verließen sich allein
/// auf die TOCTOU-anfällige `symlink_metadata`-Prüfung) wurde durch
/// `harw_fsutil::open_nofollow` ersetzt, das auf jedem Unix-Zielsystem die
/// korrekte Konstante verwendet.
#[cfg(unix)]
fn read_pending(path: &Path) -> std::io::Result<Vec<u8>> {
    let file = harw_fsutil::open_nofollow(path, OpenMode::read_only()).map_err(|error| {
        if harw_fsutil::is_symlink_loop(&error) {
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

#[cfg(not(unix))]
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
    use crate::test_support::{TestError, TestResult, ctx};

    // Feste Serveruhr für deterministische TTL-/Zeitstempel-Tests.
    struct FixedClock(Timestamp);

    impl Clock for FixedClock {
        fn now(&self) -> Timestamp {
            self.0
        }
    }

    // Ausstellungszeitpunkt aller Test-Datensätze.
    const ISSUED: Timestamp = Timestamp::constant(1_700_000_000, 0);

    fn at_offset_mins(mins: i64) -> TestResult<FixedClock> {
        Ok(FixedClock(
            ISSUED
                .checked_add(SignedDuration::from_mins(mins))
                .map_err(ctx("ISSUED + mins offset"))?,
        ))
    }

    fn actor(name: &str) -> ApprovalActor {
        ApprovalActor::Operator {
            id: name.to_owned(),
        }
    }

    fn record() -> ApprovalRecord {
        record_for("session-1", "approval-1", ISSUED)
    }

    fn record_for(session: &str, request: &str, issued_at: Timestamp) -> ApprovalRecord {
        ApprovalRecord {
            request: ItemId::from_str(request),
            session: SessionId::from_str(session),
            call_id: ToolCallId::from_str("call-1"),
            actor: actor("alice"),
            issued_at,
            tenant: None,
        }
    }

    #[test]
    fn resolution_is_durable_actor_bound_and_single_use() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = ApprovalStore::new(temp.path());
        let request = record();
        let clock = at_offset_mins(1)?;
        store.issue(&request)?;
        assert_eq!(store.pending(&request.session, &request.request)?, request);

        assert!(matches!(
            store.resolve(
                &request.session,
                &request.request,
                ReviewDecision::Approved,
                None,
                &actor("mallory"),
                &clock,
            ),
            Err(SessionStoreError::ApprovalActorMismatch { .. })
        ));

        let resolution = store.resolve(
            &request.session,
            &request.request,
            ReviewDecision::ApprovedOnce,
            Some("bounded exception".to_owned()),
            &request.actor,
            &clock,
        )?;
        assert_eq!(resolution.call_id, request.call_id);
        assert_eq!(resolution.decision, ReviewDecision::ApprovedOnce);

        assert!(matches!(
            store.resolve(
                &request.session,
                &request.request,
                ReviewDecision::Approved,
                None,
                &request.actor,
                &clock,
            ),
            Err(SessionStoreError::ApprovalAlreadyResolved { .. })
        ));
        Ok(())
    }

    /// Altdatensätze ohne `tenant` laden als `tenant: None`, und ein Datensatz
    /// ohne Mandant serialisiert byte-gleich zum alten Format.
    #[test]
    fn test_legacy_record_without_tenant_loads_and_reserializes_byte_identical() -> TestResult {
        // Exakt die Feldfolge des Formats vor Einführung von `tenant`.
        #[derive(Serialize)]
        struct LegacyApprovalRecord {
            request: ItemId,
            session: SessionId,
            call_id: ToolCallId,
            actor: ApprovalActor,
            issued_at: Timestamp,
        }
        let legacy = serde_json::to_vec(&LegacyApprovalRecord {
            request: ItemId::from_str("approval-1"),
            session: SessionId::from_str("session-1"),
            call_id: ToolCallId::from_str("call-1"),
            actor: actor("alice"),
            issued_at: ISSUED,
        })?;
        let loaded: ApprovalRecord = serde_json::from_slice(&legacy)?;
        assert_eq!(loaded, record());
        assert_eq!(loaded.tenant, None);
        assert_eq!(serde_json::to_vec(&loaded)?, legacy);

        // Auch ein über den Speicher ausgestellter Altdatensatz ist lesbar.
        let temp = tempfile::tempdir()?;
        let store = ApprovalStore::new(temp.path());
        let session_dir = store.session_dir(&loaded.session)?;
        std::fs::create_dir_all(&session_dir)?;
        std::fs::write(session_dir.join("approval-1.pending.json"), &legacy)?;
        assert_eq!(store.pending(&loaded.session, &loaded.request)?, loaded);
        assert_eq!(
            store.pending_all(10, &at_offset_mins(1)?)?,
            vec![loaded.clone()]
        );

        let resolution = store.resolve(
            &loaded.session,
            &loaded.request,
            ReviewDecision::Approved,
            None,
            &loaded.actor,
            &at_offset_mins(1)?,
        )?;
        assert_eq!(resolution.tenant, None);
        let resolution_json = serde_json::to_value(&resolution)?;
        assert!(resolution_json.get("tenant").is_none());
        Ok(())
    }

    /// Ein gesetzter Mandant wird persistiert und in die Auflösung übernommen.
    #[test]
    fn test_tenant_is_persisted_and_carried_into_resolution() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = ApprovalStore::new(temp.path());
        let mut request = record();
        request.tenant = Some(TenantId::from_str("tenant-a"));
        store.issue(&request)?;

        let pending = store.pending(&request.session, &request.request)?;
        assert_eq!(pending.tenant, Some(TenantId::from_str("tenant-a")));
        let resolution = store.resolve(
            &request.session,
            &request.request,
            ReviewDecision::Approved,
            None,
            &request.actor,
            &at_offset_mins(1)?,
        )?;
        assert_eq!(resolution.tenant, Some(TenantId::from_str("tenant-a")));
        let durable = store
            .resolution(&request.session, &request.request)?
            .ok_or(TestError::Missing("resolution"))?;
        assert_eq!(durable.tenant, Some(TenantId::from_str("tenant-a")));
        Ok(())
    }

    #[test]
    fn test_resolve_uses_server_clock_for_resolved_at() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = ApprovalStore::new(temp.path());
        let request = record();
        store.issue(&request)?;
        let clock = at_offset_mins(7)?;

        let resolution = store.resolve(
            &request.session,
            &request.request,
            ReviewDecision::Rejected,
            None,
            &request.actor,
            &clock,
        )?;

        assert_eq!(resolution.resolved_at, clock.now());
        let durable = store
            .resolution(&request.session, &request.request)?
            .ok_or(TestError::Missing("resolution"))?;
        assert_eq!(durable, resolution);
        assert_eq!(durable.resolved_at, clock.now());
        Ok(())
    }

    #[test]
    fn test_resolve_rejects_expired_request_and_writes_nothing() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = ApprovalStore::new(temp.path());
        let request = record();
        store.issue(&request)?;

        // Grenze inklusiv: genau `issued_at + TTL` ist bereits abgelaufen.
        let clock = at_offset_mins(30)?;
        let result = store.resolve(
            &request.session,
            &request.request,
            ReviewDecision::Approved,
            None,
            &request.actor,
            &clock,
        );
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "expected resolve to reject the expired request".to_owned(),
            ));
        };

        match error {
            SessionStoreError::ApprovalExpired {
                request: expired,
                issued_at,
                expires_at,
            } => {
                assert_eq!(expired, request.request);
                assert_eq!(issued_at, ISSUED);
                assert_eq!(expires_at, clock.now());
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected ApprovalExpired, got {other:?}"
                )));
            }
        }
        assert_eq!(store.resolution(&request.session, &request.request)?, None);
        Ok(())
    }

    #[test]
    fn test_resolve_accepts_request_just_before_ttl() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = ApprovalStore::new(temp.path());
        let request = record();
        store.issue(&request)?;
        let clock = FixedClock(
            ISSUED
                .checked_add(DEFAULT_APPROVAL_TTL - SignedDuration::from_secs(1))
                .map_err(ctx("ISSUED + TTL - 1s"))?,
        );

        let resolution = store.resolve(
            &request.session,
            &request.request,
            ReviewDecision::Approved,
            None,
            &request.actor,
            &clock,
        )?;
        assert_eq!(resolution.decision, ReviewDecision::Approved);
        Ok(())
    }

    #[test]
    fn test_with_ttl_applies_custom_ttl_and_new_uses_default() -> TestResult {
        let temp = tempfile::tempdir()?;
        assert_eq!(ApprovalStore::new(temp.path()).ttl(), DEFAULT_APPROVAL_TTL);
        assert_eq!(DEFAULT_APPROVAL_TTL, SignedDuration::from_mins(30));

        let store = ApprovalStore::with_ttl(temp.path(), SignedDuration::from_mins(5));
        assert_eq!(store.ttl(), SignedDuration::from_mins(5));
        let request = record();
        store.issue(&request)?;
        assert!(matches!(
            store.resolve(
                &request.session,
                &request.request,
                ReviewDecision::Approved,
                None,
                &request.actor,
                &at_offset_mins(6)?,
            ),
            Err(SessionStoreError::ApprovalExpired { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_resolution_missing_returns_none() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = ApprovalStore::new(temp.path());
        let request = record();
        assert_eq!(store.resolution(&request.session, &request.request)?, None);
        store.issue(&request)?;
        assert_eq!(store.resolution(&request.session, &request.request)?, None);
        Ok(())
    }

    #[test]
    fn test_resolution_corrupt_file_is_error() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = ApprovalStore::new(temp.path());
        let request = record();
        store.issue(&request)?;
        let path = store.resolved_path(&request.session, &request.request)?;
        std::fs::write(&path, b"{ not json")?;

        assert!(matches!(
            store.resolution(&request.session, &request.request),
            Err(SessionStoreError::ApprovalCorrupt { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_resolution_record_keyed_to_other_request_is_error() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = ApprovalStore::new(temp.path());
        let request = record();
        let foreign = ApprovalResolutionRecord {
            request: ItemId::from_str("approval-other"),
            session: request.session.clone(),
            call_id: request.call_id.clone(),
            actor: request.actor.clone(),
            decision: ReviewDecision::Approved,
            comment: None,
            resolved_at: ISSUED,
            tenant: None,
        };
        let path = store.resolved_path(&request.session, &request.request)?;
        std::fs::create_dir_all(path.parent().ok_or(TestError::Missing("path parent"))?)?;
        std::fs::write(&path, serde_json::to_vec(&foreign)?)?;

        assert!(matches!(
            store.resolution(&request.session, &request.request),
            Err(SessionStoreError::ApprovalCorrupt { .. })
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_resolution_symlink_is_error_not_none() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir()?;
        let store = ApprovalStore::new(temp.path());
        let request = record();
        store.issue(&request)?;
        let external = temp.path().join("external-resolved.json");
        std::fs::write(&external, b"sentinel")?;
        let path = store.resolved_path(&request.session, &request.request)?;
        symlink(&external, &path)?;

        assert!(matches!(
            store.resolution(&request.session, &request.request),
            Err(SessionStoreError::ApprovalCorrupt { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_pending_all_sorts_by_issued_at_and_applies_limit() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = ApprovalStore::new(temp.path());
        let t = |mins: i64| -> TestResult<Timestamp> {
            ISSUED
                .checked_add(SignedDuration::from_mins(mins))
                .map_err(ctx("ISSUED + mins offset"))
        };
        let late = record_for("session-b", "approval-late", t(3)?);
        let early = record_for("session-a", "approval-early", t(1)?);
        let middle = record_for("session-b", "approval-middle", t(2)?);
        for item in [&late, &early, &middle] {
            store.issue(item)?;
        }
        let clock = at_offset_mins(4)?;

        let all = store.pending_all(10, &clock)?;
        assert_eq!(all, vec![early.clone(), middle.clone(), late]);

        let limited = store.pending_all(2, &clock)?;
        assert_eq!(limited, vec![early, middle]);

        assert!(store.pending_all(0, &clock)?.is_empty());
        Ok(())
    }

    #[test]
    fn test_pending_all_excludes_resolved_and_expired() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = ApprovalStore::new(temp.path());
        let old = record_for("session-1", "approval-old", ISSUED);
        let fresh_at = ISSUED
            .checked_add(SignedDuration::from_mins(20))
            .map_err(ctx("ISSUED + 20min"))?;
        let resolved = record_for("session-1", "approval-done", fresh_at);
        let open = record_for("session-2", "approval-open", fresh_at);
        for item in [&old, &resolved, &open] {
            store.issue(item)?;
        }
        let clock = at_offset_mins(35)?;
        store.resolve(
            &resolved.session,
            &resolved.request,
            ReviewDecision::Approved,
            None,
            &resolved.actor,
            &clock,
        )?;

        assert_eq!(store.pending_all(10, &clock)?, vec![open]);
        Ok(())
    }

    #[test]
    fn test_pending_all_skips_corrupt_entries() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = ApprovalStore::new(temp.path());
        let good = record_for("session-1", "approval-good", ISSUED);
        store.issue(&good)?;
        let session_dir = store.session_dir(&good.session)?;
        std::fs::write(session_dir.join("approval-broken.pending.json"), b"garbage")?;
        // Datensatz, dessen Inhalt auf einen anderen Request zeigt.
        let misfiled = record_for("session-1", "approval-elsewhere", ISSUED);
        std::fs::write(
            session_dir.join("approval-misfiled.pending.json"),
            serde_json::to_vec(&misfiled)?,
        )?;
        // Unsicherer Session-Verzeichnisname.
        std::fs::create_dir_all(store.root().join("bad.name"))?;

        assert_eq!(store.pending_all(10, &at_offset_mins(1)?)?, vec![good]);
        Ok(())
    }

    #[test]
    fn test_pending_all_without_root_is_empty() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = ApprovalStore::new(temp.path());
        assert!(store.pending_all(5, &at_offset_mins(1)?)?.is_empty());
        Ok(())
    }

    #[test]
    fn unsafe_ids_cannot_escape_approval_root() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = ApprovalStore::new(temp.path());
        let mut request = record();
        request.request = ItemId::from_str("../escape");
        assert!(matches!(
            store.issue(&request),
            Err(SessionStoreError::UnsafeApprovalPath(_))
        ));
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
    fn symlinked_pending_record_is_not_authority() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir()?;
        let store = ApprovalStore::new(temp.path());
        let request = record();
        let session_dir = store.session_dir(&request.session)?;
        std::fs::create_dir_all(&session_dir)?;
        let external = temp.path().join("external-pending.json");
        std::fs::write(&external, serde_json::to_vec(&request)?)?;
        symlink(&external, session_dir.join("approval-1.pending.json"))?;

        assert!(matches!(
            store.pending(&request.session, &request.request),
            Err(SessionStoreError::ApprovalNotFound { .. })
        ));
        assert!(matches!(
            store.resolve(
                &request.session,
                &request.request,
                ReviewDecision::Approved,
                None,
                &request.actor,
                &at_offset_mins(1)?,
            ),
            Err(SessionStoreError::ApprovalNotFound { .. })
        ));
        // Ein Symlink ist auch in der Übersicht keine offene Anfrage.
        assert!(store.pending_all(10, &at_offset_mins(1)?)?.is_empty());
        assert_eq!(std::fs::read(&external)?, serde_json::to_vec(&request)?);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_resolved_record_is_not_authority_or_write_target() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir()?;
        let store = ApprovalStore::new(temp.path());
        let request = record();
        store.issue(&request)?;
        let session_dir = store.session_dir(&request.session)?;
        let external = temp.path().join("external-resolved.json");
        std::fs::write(&external, b"sentinel")?;
        symlink(&external, session_dir.join("approval-1.resolved.json"))?;

        assert!(matches!(
            store.resolve(
                &request.session,
                &request.request,
                ReviewDecision::Approved,
                None,
                &request.actor,
                &at_offset_mins(1)?,
            ),
            Err(SessionStoreError::ApprovalNotFound { .. })
        ));
        assert_eq!(std::fs::read(&external)?, b"sentinel");
        Ok(())
    }

    /// F-006 regression: `lock_session` used to open its `.lock` sidecar with
    /// no symlink protection at all (`OpenOptions` without `O_NOFOLLOW`), so a
    /// planted symlink there was opened, created and locked through to
    /// whatever it pointed at. `open_lock_file_without_following_symlinks`
    /// now goes through `harw_fsutil::open_nofollow`, which must refuse the
    /// last path component being a symlink before `resolve` ever gets a lock.
    #[cfg(unix)]
    #[test]
    fn symlinked_session_lock_is_rejected_without_opening_its_target() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir()?;
        let store = ApprovalStore::new(temp.path());
        let request = record();
        store.issue(&request)?;
        let session_dir = store.session_dir(&request.session)?;
        let lock_path = session_dir.join(".lock");
        let external = temp.path().join("external.lock");
        std::fs::write(&external, b"outside lock target")?;
        symlink(&external, &lock_path)?;

        let result = store.resolve(
            &request.session,
            &request.request,
            ReviewDecision::Approved,
            None,
            &request.actor,
            &at_offset_mins(1)?,
        );

        assert!(
            matches!(result, Err(SessionStoreError::Io(_))),
            "expected a symlink rejection, got {result:?}"
        );
        assert_eq!(
            std::fs::read(&external)?,
            b"outside lock target",
            "the external lock target must not have been opened, let alone written"
        );
        Ok(())
    }
}
