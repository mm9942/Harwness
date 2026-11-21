//! Erneute Beobachtung des Ziels unmittelbar vor dem Eingriff:
//! [`TargetObserver`], [`CgroupFsObserver`], [`StaticTargetObserver`]
//! (PL-100 WP 3.1, Bedrohung B21).
//!
//! # Zweck
//! Kernel-IDs von cgroups werden wiederverwendet. Die Autorisierung bindet
//! deshalb das Tripel aus Pfad, Kernel-ID und `first_seen`
//! (`harw_dod_warden_proto::CgroupTarget`). Bevor der Warden handelt (und
//! bevor der Hold-Timer auftaut), beobachtet er das Ziel erneut: unter dem
//! signierten Pfad muss **jetzt** eine cgroup mit der signierten Kernel-ID
//! liegen. Sonst handelt er nicht (`Denial::TargetChanged`).
//!
//! # Woher die Kernel-ID kommt
//! In cgroup v2 ist die Kernel-ID einer cgroup die ID ihres kernfs-Knotens;
//! auf 64-Bit-Systemen ist das zugleich die Inode-Nummer des
//! cgroup-Verzeichnisses (`kernfs_ino`), dieselbe Zahl, die
//! `name_to_handle_at` und BPF (`bpf_get_current_cgroup_id`) liefern.
//! [`CgroupFsObserver`] liest sie deshalb per `symlink_metadata` als
//! `ino()` des Verzeichnisses unter der cgroup-Wurzel.
//!
//! # `first_seen`
//! `first_seen` ist der Zeitpunkt, zu dem der **Sensor** die cgroup zuerst
//! beobachtet hat. Der Warden hat dafür keine unabhängige Quelle (kernfs
//! führt keine verlässliche Geburtszeit; Inode-Zeiten werden beim Neuanlegen
//! des Inode-Caches neu gesetzt). Der Wert ist MAC-gebunden, der Escalator
//! prüft ihn beim Signieren gegen den Spool (README §3.2 „Kernel-ID beim
//! Signieren erneut prüfen“), und der Warden prüft ihn auf Plausibilität
//! (`first_seen <= issued_at`). Läuft für dieselbe cgroup (Pfad und
//! Kernel-ID) ein Hold, vergleicht der Warden das ganze Tripel samt
//! `first_seen` mit dem des Holds
//! (`VerifiedAuthorization::confirm_target`, siehe `warden.rs`) — die
//! einzige vom aktuellen Auftrag unabhängige Aufzeichnung, die er hat.
//!
//! # Beobachtung ist nicht Eingriff
//! Zwischen dieser Beobachtung und dem Schreiben der Kontrolldatei kann die
//! cgroup ersetzt werden. Der produktive Ausführer prüft die Kernel-ID
//! deshalb noch einmal am geöffneten Verzeichnis und schreibt über genau
//! diesen Deskriptor (`executor.rs`, „Prüfung und Eingriff über denselben
//! Deskriptor“).
//!
//! # Nebenläufigkeit
//! [`CgroupFsObserver`] ist zustandslos; [`StaticTargetObserver`] teilt
//! seine Tabelle über `Arc<Mutex<_>>` zwischen Klonen (Tests).
//!
//! # Fehler
//! [`WardenError::InvalidCgroupId`], [`WardenError::Io`] (z. B. `NotFound`
//! für ein verschwundenes Ziel), [`WardenError::TargetNotCgroup`].

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use harw_dod_warden_proto::CgroupKernelId;
use harw_types::CgroupId;

use crate::error::{WardenError, WardenResult};
use crate::executor::validate_path_segment;

/// Beobachtet die aktuelle Kernel-ID einer cgroup.
pub trait TargetObserver {
    /// Returns the kernel id of the cgroup currently at `path`.
    ///
    /// # Arguments
    /// - `path` (`&CgroupId`): relativer cgroup-Pfad.
    ///
    /// # Errors
    /// - [`WardenError::InvalidCgroupId`]: unsicherer Pfad.
    /// - [`WardenError::Io`]: `NotFound` (Ziel verschwunden) oder ein anderer
    ///   Beobachtungsfehler.
    /// - [`WardenError::TargetNotCgroup`]: kein Verzeichnis oder ID `0`.
    fn kernel_id(&self, path: &CgroupId) -> WardenResult<CgroupKernelId>;
}

/// Produktiver Beobachter über die cgroup-v2-Hierarchie.
///
/// # Examples
/// ```rust
/// use harw_dod_warden::observe::{CgroupFsObserver, TargetObserver};
/// use harw_types::CgroupId;
///
/// let Ok(root) = tempfile::tempdir() else { return };
/// let Ok(()) = std::fs::create_dir(root.path().join("job-1")) else { return };
/// let Ok(path) = CgroupId::try_from_str("job-1") else { return };
/// let observer = CgroupFsObserver::new(root.path());
/// assert!(observer.kernel_id(&path).is_ok());
/// ```
#[derive(Debug, Clone)]
pub struct CgroupFsObserver {
    root: PathBuf,
}

impl CgroupFsObserver {
    /// Builds an observer below `root` (production: `/sys/fs/cgroup`).
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl TargetObserver for CgroupFsObserver {
    fn kernel_id(&self, path: &CgroupId) -> WardenResult<CgroupKernelId> {
        let relative = validate_path_segment(path)?;
        let metadata = std::fs::symlink_metadata(self.root.join(relative))?;
        if !metadata.file_type().is_dir() {
            return Err(WardenError::TargetNotCgroup);
        }
        CgroupKernelId::new(metadata.ino()).map_err(|_| WardenError::TargetNotCgroup)
    }
}

/// Fester, veränderbarer Beobachter für Tests (kein Dateisystem).
///
/// # Description
/// Klone teilen dieselbe Tabelle; ein Test kann dem Warden einen Klon geben
/// und danach eine cgroup „verschwinden“ lassen oder ihre Kernel-ID ändern.
///
/// # Examples
/// ```rust
/// use harw_dod_warden::observe::{StaticTargetObserver, TargetObserver};
/// use harw_dod_warden_proto::CgroupKernelId;
/// use harw_types::CgroupId;
///
/// let (Ok(path), Ok(id)) = (CgroupId::try_from_str("harw.slice/job-1"), CgroupKernelId::new(9))
/// else {
///     return;
/// };
/// let observer = StaticTargetObserver::new();
/// observer.set(&path, id);
/// assert_eq!(observer.kernel_id(&path).ok(), Some(id));
/// observer.remove(&path);
/// assert!(observer.kernel_id(&path).is_err());
/// ```
#[derive(Debug, Clone, Default)]
pub struct StaticTargetObserver {
    ids: Arc<Mutex<HashMap<String, CgroupKernelId>>>,
}

impl StaticTargetObserver {
    /// Creates an empty observer (every path is "not found").
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the kernel id observed at `path`.
    pub fn set(&self, path: &CgroupId, id: CgroupKernelId) {
        self.ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(path.as_str().to_owned(), id);
    }

    /// Makes `path` disappear.
    pub fn remove(&self, path: &CgroupId) {
        self.ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(path.as_str());
    }
}

impl TargetObserver for StaticTargetObserver {
    fn kernel_id(&self, path: &CgroupId) -> WardenResult<CgroupKernelId> {
        self.ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(path.as_str())
            .copied()
            .ok_or_else(|| WardenError::Io(std::io::Error::from(std::io::ErrorKind::NotFound)))
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt as _;

    use super::{CgroupFsObserver, TargetObserver};
    use crate::error::WardenError;
    use crate::test_support::{TestResult, ctx};
    use harw_types::CgroupId;

    #[test]
    fn test_fs_observer_reports_directory_inode_as_kernel_id() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let job = root.path().join("harw.slice").join("job-1");
        std::fs::create_dir_all(&job).map_err(ctx("mkdir"))?;
        let inode = std::fs::metadata(job).map_err(ctx("metadata"))?.ino();
        let observer = CgroupFsObserver::new(root.path());
        let path = CgroupId::try_from_str("harw.slice/job-1").map_err(ctx("cgroup"))?;
        assert_eq!(
            observer.kernel_id(&path).map_err(ctx("observe"))?.get(),
            inode
        );
        Ok(())
    }

    #[test]
    fn test_fs_observer_reports_missing_and_non_directory_targets() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::fs::write(root.path().join("file"), b"x").map_err(ctx("write"))?;
        let observer = CgroupFsObserver::new(root.path());
        let missing = CgroupId::try_from_str("gone").map_err(ctx("cgroup"))?;
        let file = CgroupId::try_from_str("file").map_err(ctx("cgroup"))?;
        let traversal = CgroupId::try_from_str("../etc").map_err(ctx("cgroup"))?;
        assert!(
            observer
                .kernel_id(&missing)
                .is_err_and(|e| e.is_target_gone())
        );
        assert!(matches!(
            observer.kernel_id(&file),
            Err(WardenError::TargetNotCgroup)
        ));
        assert!(matches!(
            observer.kernel_id(&traversal),
            Err(WardenError::InvalidCgroupId)
        ));
        Ok(())
    }
}
