//! Der persistente Zustand des Wardens im StateDirectory: [`WardenState`]
//! (PL-100 WP 3.1, Bedrohung B24).
//!
//! # Zweck
//! Bündelt die vier Zustandsdateien unter `/var/lib/harw-warden`
//! (systemd `StateDirectory=harw-warden`, Modus `0700`):
//!
//! - [`FileNonceLedger`] (`nonces.v2.log`): Replay-Schutz über Neustarts;
//! - [`ActionLedger`] (`actions.v2.log`): hash-verkettetes Aktions-Ledger;
//! - [`HoldStore`] (`holds.v2.json`): laufende Hold-Timer;
//! - [`ContainmentStore`] (`containment.v1.json`): Eindämmungen mit ihrem
//!   Marker für harw und `freeze_was_set` (`containment.rs`); beim Öffnen
//!   liest er die Boot-ID (`/proc/sys/kernel/random/boot_id`), um Datensätze
//!   früherer Boots zu erkennen.
//!
//! [`WardenState::open`] muss **vor** der Landlock-Beschränkung laufen
//! oder die Landlock-Regel muss das StateDirectory lesend und schreibend
//! freigeben (`harw-warden`, `landlock.rs`, tut beides). Die Boot-ID gibt es
//! nur vor Landlock (`/proc` liegt nicht im Laufzeitbereich); ohne sie gilt
//! kein gespeicherter Eindämmungsdatensatz als aus diesem Boot (fail closed,
//! `containment.rs`).
//!
//! # Prüfungen
//! Das Verzeichnis muss existieren (systemd legt es an; der Warden legt es
//! nicht selbst an), ein echtes Verzeichnis sein (kein Symlink), dem
//! effektiven Nutzer des Prozesses gehören (systemd: `User=harw-warden`)
//! und darf keine Gruppen- oder Weltrechte haben. Ein fremdes Verzeichnis
//! (etwa ein liegengebliebenes `/var/lib/harw-warden` einer früheren
//! Installation oder ein falsches `--state-dir`) wird abgelehnt. Die
//! Zustandsdateien darin werden ohne Symlink-Folgen geöffnet (`O_NOFOLLOW`,
//! `fsutil.rs`). Jede beschädigte Datei verhindert den Start.
//!
//! # Nebenläufigkeit
//! Jede Komponente bringt ihren eigenen `Mutex` mit; [`WardenState`] ist
//! `Send + Sync`.
//!
//! # Examples
//! ```rust
//! use std::os::unix::fs::PermissionsExt as _;
//!
//! use harw_dod_warden::state::WardenState;
//!
//! // Wie systemd mit `StateDirectoryMode=0700`: nur der Eigentümer.
//! let Ok(dir) = tempfile::Builder::new()
//!     .permissions(std::fs::Permissions::from_mode(0o700))
//!     .tempdir()
//! else {
//!     return;
//! };
//! assert!(WardenState::open(dir.path(), jiff::Timestamp::UNIX_EPOCH).is_ok());
//! ```

use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use jiff::Timestamp;

use crate::action_ledger::ActionLedger;
use crate::containment::ContainmentStore;
use crate::error::{WardenError, WardenResult};
use crate::holds::HoldStore;
use crate::nonce_ledger::{DEFAULT_NONCE_CAPACITY, FileNonceLedger};

/// Vorgabepfad des StateDirectory.
pub const DEFAULT_STATE_DIR: &str = "/var/lib/harw-warden";

// Keinerlei Gruppen- oder Weltrechte auf dem StateDirectory.
const GROUP_OR_WORLD_ANY: u32 = 0o077;

/// Die vier Zustandsdateien des Wardens.
#[derive(Debug)]
pub struct WardenState {
    nonces: FileNonceLedger,
    actions: ActionLedger,
    holds: HoldStore,
    containment: ContainmentStore,
}

impl WardenState {
    /// Opens all state files in `dir` with the default nonce capacity.
    ///
    /// # Arguments
    /// - `dir` (`&Path`): das StateDirectory.
    /// - `now` (`Timestamp`): aktuelle Zeit (verwirft unter Druck abgelaufene
    ///   Nonces, siehe `nonce_ledger.rs`).
    ///
    /// # Errors
    /// - [`WardenError::UntrustedFile`]: kein Verzeichnis, nicht im Besitz
    ///   des effektiven Nutzers oder Gruppen-/Weltrechte gesetzt.
    /// - alles aus [`FileNonceLedger::open`], [`ActionLedger::open`],
    ///   [`HoldStore::open`] und [`ContainmentStore::open`].
    pub fn open(dir: &Path, now: Timestamp) -> WardenResult<Self> {
        Self::open_with_capacity(dir, now, DEFAULT_NONCE_CAPACITY)
    }

    /// Like [`Self::open`] with an explicit nonce capacity.
    ///
    /// # Errors
    /// Wie [`Self::open`].
    pub fn open_with_capacity(
        dir: &Path,
        now: Timestamp,
        nonce_capacity: usize,
    ) -> WardenResult<Self> {
        let metadata = std::fs::symlink_metadata(dir)?;
        if !metadata.file_type().is_dir() {
            return Err(WardenError::UntrustedFile {
                what: "state directory",
                reason: "directory is not a plain directory",
            });
        }
        if metadata.uid() != rustix::process::geteuid().as_raw() {
            return Err(WardenError::UntrustedFile {
                what: "state directory",
                reason: "owner is not the warden's effective user",
            });
        }
        if metadata.mode() & GROUP_OR_WORLD_ANY != 0 {
            return Err(WardenError::UntrustedFile {
                what: "state directory",
                reason: "mode grants group or world access",
            });
        }
        Ok(Self {
            nonces: FileNonceLedger::open(dir, now, nonce_capacity)?,
            actions: ActionLedger::open(dir)?,
            holds: HoldStore::open(dir)?,
            containment: ContainmentStore::open(dir)?,
        })
    }

    /// Returns the persistent nonce ledger.
    #[must_use]
    pub fn nonces(&self) -> &FileNonceLedger {
        &self.nonces
    }

    /// Returns the hash-chained action ledger.
    #[must_use]
    pub fn actions(&self) -> &ActionLedger {
        &self.actions
    }

    /// Returns the hold timer store.
    #[must_use]
    pub fn holds(&self) -> &HoldStore {
        &self.holds
    }

    /// Returns the containment store (marker bookkeeping, `freeze_was_set`).
    #[must_use]
    pub fn containment(&self) -> &ContainmentStore {
        &self.containment
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::WardenState;
    use crate::error::WardenError;
    use crate::test_support::{TestResult, ctx, private_tempdir};

    #[test]
    fn test_state_directory_must_be_private() -> TestResult {
        let dir = private_tempdir()?;
        let now = jiff::Timestamp::UNIX_EPOCH;
        assert!(WardenState::open(dir.path(), now).is_ok());
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o750))
            .map_err(ctx("chmod"))?;
        assert!(matches!(
            WardenState::open(dir.path(), now),
            Err(WardenError::UntrustedFile { .. })
        ));
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .map_err(ctx("chmod back"))?;
        let missing = dir.path().join("missing");
        assert!(matches!(
            WardenState::open(&missing, now),
            Err(WardenError::Io(_))
        ));
        Ok(())
    }

    #[test]
    fn test_symlinked_state_directory_is_refused() -> TestResult {
        let dir = private_tempdir()?;
        let real = dir.path().join("real");
        std::fs::create_dir(&real).map_err(ctx("mkdir"))?;
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700))
            .map_err(ctx("chmod"))?;
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).map_err(ctx("symlink"))?;
        assert!(matches!(
            WardenState::open(&link, jiff::Timestamp::UNIX_EPOCH),
            Err(WardenError::UntrustedFile { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_symlinked_state_file_is_refused() -> TestResult {
        let dir = private_tempdir()?;
        let elsewhere = private_tempdir()?;
        let target = elsewhere.path().join("holds.v2.json");
        std::fs::write(&target, b"{\"version\":2,\"holds\":[]}").map_err(ctx("write"))?;
        std::os::unix::fs::symlink(&target, dir.path().join(crate::holds::HOLDS_FILE))
            .map_err(ctx("symlink"))?;
        assert!(matches!(
            WardenState::open(dir.path(), jiff::Timestamp::UNIX_EPOCH),
            Err(WardenError::Io(_))
        ));
        Ok(())
    }
}
