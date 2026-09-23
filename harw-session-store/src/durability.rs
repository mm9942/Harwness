//! Dauerhaftigkeit: die eine Stelle, an der ein Verzeichniseintrag auf die
//! Platte gezwungen wird.
//!
//! # Verantwortungsbereich
//! Dieses Modul besitzt genau eine Funktion — [`sync_parent_directory`] — und
//! existiert, weil sie vorher in **fünf** Dateien byte-gleich stand
//! (`store.rs`, `child_lease.rs`, `approval.rs`, `job_store.rs`, `freeze.rs`).
//! Eine Dauerhaftigkeitszusage in fünf Kopien driftet; dieselbe Lehre wie bei
//! der Punktgrenzen-Regel in `harw-sandbox`, die aus demselben Grund
//! zusammengelegt wurde.
//!
//! # Warum es diese Funktion überhaupt gibt
//! Ein `sync_all()` auf einer Datei sichert **ihren Inhalt**. Es sichert
//! **nicht den Verzeichniseintrag**, der sie auffindbar macht. Nach einem
//! Stromausfall kann eine vollständig geschriebene Datei auf der Platte
//! liegen und **trotzdem nicht existieren**, weil ihr Eintrag im
//! Verzeichnis-Cache stand.
//!
//! Für einen Zwischenspeicher wäre das hinnehmbar. Für die Stores dieser
//! Crate nicht:
//!
//! - `ChildLeaseStore` trägt die Einmal-Zustellungsgarantie. Der Übergang von
//!   `.active.json` nach `.completed.json` ist ein `rename`; fällt dessen
//!   Verzeichniseintrag nach einem Absturz zurück, wird derselbe Datensatz
//!   ein zweites Mal beansprucht.
//! - `ApprovalStore` trägt Genehmigungen. Eine verlorene **Ablehnung**, die
//!   den vorherigen Zustand zurückfallen lässt, ist ein Sicherheitsproblem.
//! - `JobStore` trägt Jobs, deren Verlust stillen Stillstand bedeutet.
//! - `FreezeStore` trägt Einfrierungen, deren Verlust eine Abwehr aufhebt.
//!
//! **Wer eine dieser Zeilen für überflüssig hält und entfernt, entfernt die
//! Zusage, nicht den Aufwand.**
//!
//! # Nebenläufigkeit
//! Die Funktion hält keinen Zustand und ist aus mehreren Threads parallel
//! aufrufbar. Sie ersetzt keine Sperre: sie sagt nur, dass ein bereits
//! vollzogener Verzeichniswechsel dauerhaft ist.
//!
//! # Fehler
//! [`SessionStoreError::Io`], wenn das Verzeichnis nicht geöffnet oder nicht
//! synchronisiert werden kann. Der Fehler wird **nie verschluckt** — ein
//! stillschweigend fehlgeschlagenes `fsync` ist von einem erfolgreichen nicht
//! zu unterscheiden, und genau diese Verwechslung soll die Funktion
//! verhindern.

use std::fs::File;
use std::path::Path;

use crate::error::{SessionStoreError, SessionStoreResult};

/// Erzwingt, dass die Verzeichniseinträge in `parent` auf der Platte stehen.
///
/// # Description
/// Öffnet das Verzeichnis **lesend** und ruft `sync_all()` darauf auf. Der
/// Aufruf gehört **hinter** den wirksamen Punkt der Änderung — hinter
/// `persist`, `rename` oder `create_new` —, nie davor: vorher gibt es noch
/// keinen Eintrag zu sichern.
///
/// Ein Verzeichnis wird zum Lesen geöffnet, nicht zum Schreiben;
/// `OpenOptions::write(true)` scheitert auf einem Verzeichnis mit `EISDIR`.
///
/// # Arguments
/// - `parent` (`&Path`): das Verzeichnis, dessen Einträge dauerhaft werden
///   sollen. Üblicherweise das Ergebnis von `Path::parent` — der Aufrufer
///   behandelt dessen `None`-Fall selbst, statt hier zu raten.
///
/// # Returns
/// `Ok(())`, wenn das Verzeichnis synchronisiert wurde.
///
/// # Errors
/// [`SessionStoreError::Io`] beim Öffnen oder Synchronisieren. Der Aufrufer
/// lässt den Fehler den ganzen Vorgang scheitern, statt ihn zu verwerfen.
///
/// # Concurrency
/// Zustandslos, aus mehreren Threads parallel aufrufbar.
///
/// # Examples
/// ```rust,no_run
/// # use std::path::Path;
/// # fn demo(record: &Path) -> harw_session_store::SessionStoreResult<()> {
/// // ... die Datei wurde bereits geschrieben und per `persist` eingehängt ...
/// let parent = record.parent().ok_or_else(|| {
///     harw_session_store::SessionStoreError::Io(std::io::Error::new(
///         std::io::ErrorKind::InvalidInput,
///         "record path has no parent directory",
///     ))
/// })?;
/// # let _ = parent;
/// # Ok(())
/// # }
/// ```
pub(crate) fn sync_parent_directory(parent: &Path) -> SessionStoreResult<()> {
    let directory = File::open(parent).map_err(SessionStoreError::Io)?;
    directory.sync_all().map_err(SessionStoreError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

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

    #[test]
    fn sync_parent_directory_succeeds_on_an_existing_directory() -> TestResult {
        let temp = tempfile::tempdir()?;
        assert!(sync_parent_directory(temp.path()).is_ok());
        Ok(())
    }
}
