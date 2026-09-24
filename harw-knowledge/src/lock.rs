//! Prozessübergreifende Dateisperren für Read-Modify-Write-Zyklen.
//!
//! # Verantwortung
//! Mehrere Prozesse (TUI, Gateway, Job-Worker, zweite TUI) schreiben in
//! denselben Wissensspeicher. `write_atomic` schützt nur vor halb
//! geschriebenen Dateien, nicht vor verlorenen Updates: zwei gleichzeitige
//! Read-Modify-Write-Zyklen (Diary-Append, Kartendatei, Workbench-Manifest)
//! lesen denselben Stand, und der zweite `rename` überschreibt den ersten.
//! [`KnowledgeLock`] serialisiert solche Zyklen über eine beratende
//! (`advisory`) exklusive Sperre auf einer versteckten Nachbardatei
//! (`.<name>.lock`).
//!
//! # Mechanik
//! `fs4` (bereits Workspace-Abhängigkeit, u. a. `harw-session-store`) sperrt
//! über `flock` bzw. `LockFileEx`. Die Sperre hängt an der geöffneten Datei:
//! sie fällt beim `Drop` des Wächters und — wichtig für die
//! Stale-Erkennung — automatisch, wenn der haltende Prozess abstürzt. Eine
//! liegen gebliebene `.lock`-Datei ist darum nie eine hängende Sperre; sie
//! wird bewusst nicht gelöscht (ein Löschen während ein anderer Prozess sie
//! gerade öffnet, würde zwei „exklusive" Halter erzeugen).
//!
//! Weil `flock` an der offenen Dateibeschreibung hängt, schließen sich auch
//! zwei Threads desselben Prozesses gegenseitig aus, sofern sie die Datei
//! getrennt öffnen — das tut [`KnowledgeLock::acquire`] bei jedem Aufruf.
//! Eine Sperre ist **nicht** wiedereintrittsfähig: wer sie hält, darf sie
//! nicht ein zweites Mal anfordern (das endet im Timeout).
//!
//! # Warten
//! Statt blockierend zu warten, versucht [`KnowledgeLock::acquire_with_timeout`]
//! die Sperre in kurzen Abständen erneut und gibt nach der Frist mit
//! [`std::io::ErrorKind::TimedOut`] auf — ein hängender Fremdprozess legt so
//! nie die TUI lahm.
//!
//! Die `.lock`-Dateien enden nicht auf `.md`; `KnowledgeIndex::rebuild`,
//! `diary::gc` und `kanban::board::list_card_records` übergehen sie.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fs4::FileExt;

use crate::error::{KnowledgeError, KnowledgeResult};

/// Vorgabe-Frist, bis [`KnowledgeLock::acquire`] aufgibt.
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// Abstand zwischen zwei Sperrversuchen.
const RETRY_INTERVAL: Duration = Duration::from_millis(10);

/// Gehaltene exklusive Dateisperre; wird beim `Drop` freigegeben.
#[derive(Debug)]
pub struct KnowledgeLock {
    file: File,
    path: PathBuf,
}

impl KnowledgeLock {
    /// Sperrt `lock_path` exklusiv mit [`DEFAULT_LOCK_TIMEOUT`].
    ///
    /// # Fehler
    /// Wie [`Self::acquire_with_timeout`].
    pub fn acquire(lock_path: &Path) -> KnowledgeResult<Self> {
        Self::acquire_with_timeout(lock_path, DEFAULT_LOCK_TIMEOUT)
    }

    /// Sperrt die Nachbar-Sperrdatei von `target` (siehe [`lock_path_for`]).
    ///
    /// # Fehler
    /// Wie [`Self::acquire_with_timeout`].
    pub fn for_target(target: &Path) -> KnowledgeResult<Self> {
        Self::acquire(&lock_path_for(target))
    }

    /// Sperrt `lock_path` exklusiv; legt Datei und Elternverzeichnis bei
    /// Bedarf an und versucht es bis `timeout` erneut.
    ///
    /// # Fehler
    /// - [`KnowledgeError::Io`] (`InvalidInput`), wenn `lock_path` ein
    ///   symbolischer Link ist (eine Sperre folgt nie einem Link).
    /// - [`KnowledgeError::Io`] (`TimedOut`), wenn ein anderer Halter die
    ///   Sperre länger als `timeout` hält.
    /// - [`KnowledgeError::Io`] bei Anlege-/Öffnungs-/Sperrfehlern.
    pub fn acquire_with_timeout(lock_path: &Path, timeout: Duration) -> KnowledgeResult<Self> {
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::symlink_metadata(lock_path).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(KnowledgeError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("knowledge lock is a symlink: {}", lock_path.display()),
            )));
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        let deadline = Instant::now() + timeout;
        loop {
            // Vollqualifiziert: `std::fs::File` hat eigene `try_lock`-Methoden.
            match FileExt::try_lock(&file) {
                Ok(()) => {
                    return Ok(Self {
                        file,
                        path: lock_path.to_path_buf(),
                    });
                }
                Err(fs4::TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return Err(KnowledgeError::Io(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            format!("knowledge lock busy: {}", lock_path.display()),
                        )));
                    }
                    std::thread::sleep(RETRY_INTERVAL);
                }
                Err(fs4::TryLockError::Error(error)) => return Err(KnowledgeError::Io(error)),
            }
        }
    }

    /// Pfad der gesperrten Datei.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for KnowledgeLock {
    fn drop(&mut self) {
        // Schlägt das Entsperren fehl, gibt spätestens das Schließen der
        // Datei (direkt danach) die Sperre frei.
        let _ = FileExt::unlock(&self.file);
    }
}

/// Die versteckte Nachbar-Sperrdatei eines Ziels: `<dir>/.<name>.lock`.
#[must_use]
pub fn lock_path_for(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "knowledge".to_owned());
    let lock_name = format!(".{name}.lock");
    match target.parent() {
        Some(parent) => parent.join(lock_name),
        None => PathBuf::from(lock_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    fn temporary_root(label: &str) -> TestResult<PathBuf> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(crate::test_support::ctx("system clock is after epoch"))?
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "harw-knowledge-lock-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root)?;
        Ok(root)
    }

    #[test]
    fn lock_path_is_a_hidden_sibling() {
        assert_eq!(
            lock_path_for(Path::new("/k/diary/a/2026-01-01.md")),
            PathBuf::from("/k/diary/a/.2026-01-01.md.lock")
        );
    }

    #[test]
    fn a_held_lock_times_out_a_second_holder_and_frees_on_drop() -> TestResult {
        let root = temporary_root("contended")?;
        let target = root.join("nested/card.md");
        let first = KnowledgeLock::for_target(&target)?;
        assert!(first.path().ends_with(".card.md.lock"));
        match KnowledgeLock::acquire_with_timeout(first.path(), Duration::from_millis(30)) {
            Err(KnowledgeError::Io(error)) if error.kind() == std::io::ErrorKind::TimedOut => {}
            other => {
                return Err(crate::test_support::TestError::Unexpected(format!(
                    "second holder must time out, got {other:?}"
                )));
            }
        }
        drop(first);
        let again = KnowledgeLock::acquire_with_timeout(
            &lock_path_for(&target),
            Duration::from_millis(30),
        )?;
        drop(again);
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn threads_serialize_their_read_modify_write() -> TestResult {
        let root = temporary_root("threads")?;
        let counter = root.join("counter.txt");
        std::fs::write(&counter, "0")?;
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let counter = counter.clone();
                std::thread::spawn(move || -> KnowledgeResult<()> {
                    for _ in 0..10 {
                        let _lock = KnowledgeLock::for_target(&counter)?;
                        let value: u32 = std::fs::read_to_string(&counter)?
                            .trim()
                            .parse()
                            .unwrap_or(0);
                        std::fs::write(&counter, (value + 1).to_string())?;
                    }
                    Ok(())
                })
            })
            .collect();
        for handle in handles {
            handle.join().map_err(|_| {
                crate::test_support::TestError::Unexpected("thread panicked".to_owned())
            })??;
        }
        assert_eq!(std::fs::read_to_string(&counter)?.trim(), "80");
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }
}
