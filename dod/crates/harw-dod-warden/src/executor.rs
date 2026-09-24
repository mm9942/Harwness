//! Die Ausführungs-Traits: [`CgroupFreezer`], [`CgroupReleaser`],
//! [`NetworkIsolator`], [`ProcessTreeKiller`] — je einer pro
//! [`harw_dod_warden_proto::WardenAction`]-Variante.
//!
//! # Verantwortungsbereich
//! Jede Aktion, die tatsächlich ins System eingreift, ist hinter einem
//! eigenen, einmethodigen Trait gekapselt (Brief: „Kapsle jede hinter einen
//! eigenen Trait"). [`crate::warden::Warden`] hält für jede Aktion genau
//! eine `Box<dyn Trait>`-Instanz und ruft nie direkt cgroup- oder
//! Netz-Mechanik auf. Das erlaubt zwei unabhängige Dinge:
//!
//! - **Tests ohne echte Wirkung**: [`RecordingExecutor`] implementiert alle
//!   vier Traits, friert nichts ein, tötet nichts, schneidet kein Netz ab —
//!   sie zeichnet nur auf, welche Methode mit welcher cgroup aufgerufen
//!   wurde. Kein Test dieser Crate erzeugt ein echtes Einfrieren, Töten oder
//!   Netzabschneiden (Brief).
//! - **Ein Austausch der Netz-Mechanik ohne Umschreibung**: siehe Abschnitt
//!   „Warum `NetworkIsolator` keine echte Implementierung hat" unten.
//!
//! # Was `CgroupV2Executor` wirklich tut
//! [`CgroupV2Executor`] implementiert [`CgroupFreezer`], [`CgroupReleaser`]
//! und [`ProcessTreeKiller`] über einfache Dateisystemschreibzugriffe auf
//! die cgroup-v2-Kontrolldateien (`cgroup.freeze`, `cgroup.kill`) unterhalb
//! eines konfigurierbaren Wurzelverzeichnisses (Produktionscode:
//! `/sys/fs/cgroup`; Tests: ein `tempfile`-Verzeichnis). Das ist reiner
//! `std::fs`-Code ohne zusätzliche Abhängigkeit — kein `unsafe`, kein
//! externer Crate für cgroup-Zugriff nötig, weil die cgroup-v2-Schnittstelle
//! selbst schon eine Textdatei ist.
//!
//! **Zusätzliche, selbst auferlegte Prüfung:** Bevor eine `CgroupId` in
//! einen Pfad eingebettet wird, weist [`validate_path_segment`] Werte mit
//! `/`, `..` oder einem eingebetteten NUL-Byte zurück
//! ([`crate::error::WardenError::InvalidCgroupId`]). `harw-types` garantiert
//! für `CgroupId` nur „nicht leer" — keine Pfadsicherheit (siehe
//! `harw-types/src/ids.rs`, `CgroupId`-Moduldoku zu Kernel-ID-Wiederverwendung,
//! die aber die *Zeichen* der ID nicht einschränkt). Der Warden glaubt der
//! Gegenseite nichts (Crate-Moduldoku) — das gilt auch für Feldwerte
//! innerhalb einer bereits nachgeprüften und zulässigen Aktion.
//!
//! # Warum `NetworkIsolator` keine echte Implementierung hat
//! `harw-dod-netpolicy` (AW3-02) hat die Nahtstelle bereits richtig
//! geschnitten (`NetBackend::apply(&self, plan: &NetPlan)`) und verweist die
//! echte Mechanik ausdrücklich auf diesen Knoten (AW5-04a). Diese Crate
//! prüfte deshalb, ob `rustables` — die im Plan (`docs/design/build-history.md`)
//! genannte Kandidaten-Crate für die echte Netlink-Mechanik — in das
//! Abhängigkeitsbudget passt:
//!
//! - `rustables` taucht in `Cargo.lock` an keiner Stelle im Workspace auf
//!   (geprüft: `grep -n '^name = "rustables"' Cargo.lock` liefert nichts) —
//!   es wäre eine vollständig neue Abhängigkeitskette, nicht die
//!   Wiederverwendung einer bereits vorhandenen.
//! - Das Abhängigkeitsbudget dieser Crate ist bereits durch die mandatierte
//!   Grundausstattung von `harw-dod-warden-proto` selbst gesprengt (siehe
//!   `lib.rs`-Moduldoku, Abschnitt „Abhängigkeitsbudget") — jede zusätzliche
//!   Kette verschärft einen bereits dokumentierten Befund, statt einen neuen
//!   zu rechtfertigen.
//!
//! **Entscheidung: kein echtes Netz-Backend in diesem Knoten.**
//! [`NetworkIsolator`] besteht nur aus dem Trait plus
//! [`RecordingExecutor`]. Ein künftiges echtes Backend implementiert
//! [`NetworkIsolator`] in einem eigenen Crate (genau wie
//! `harw_dod_netpolicy::NetBackend` es für `NetPlan` vorsieht) — das ist ein
//! Austausch der Implementierung, keine Umschreibung dieses Traits: die
//! Signatur `fn isolate(&self, cgroup: &CgroupId) -> WardenResult<()>`
//! enthält kein backend-spezifisches Detail (kein Netlink-Handle, kein
//! `nft`-Prozess-Handle), genau wie `NetBackend::apply` keines enthält.

use harw_types::CgroupId;

use crate::error::{WardenError, WardenResult};

/// Friert eine cgroup ein (Aktion [`harw_dod_warden_proto::WardenAction::FreezeCgroup`]).
pub trait CgroupFreezer {
    /// Friert die angegebene cgroup ein.
    ///
    /// # Description
    /// Wird von [`crate::warden::Warden::handle`] ausschließlich nach
    /// erfolgreicher Beleg-Nachprüfung aufgerufen.
    ///
    /// # Arguments
    /// - `cgroup` (`&CgroupId`): die einzufrierende cgroup.
    ///
    /// # Returns
    /// `Ok(())` bei Erfolg.
    ///
    /// # Errors
    /// - [`WardenError::InvalidCgroupId`] / [`WardenError::Io`]: siehe
    ///   jeweilige Implementierung.
    fn freeze(&self, cgroup: &CgroupId) -> WardenResult<()>;
}

/// Hebt eine bestehende Eindämmung auf (Aktion
/// [`harw_dod_warden_proto::WardenAction::ReleaseCgroup`]).
pub trait CgroupReleaser {
    /// Gibt die angegebene cgroup frei (hebt Freeze oder Netzisolation auf).
    ///
    /// # Arguments
    /// - `cgroup` (`&CgroupId`): die freizugebende cgroup.
    ///
    /// # Returns
    /// `Ok(())` bei Erfolg.
    ///
    /// # Errors
    /// - [`WardenError::InvalidCgroupId`] / [`WardenError::Io`]: siehe
    ///   jeweilige Implementierung.
    fn release(&self, cgroup: &CgroupId) -> WardenResult<()>;
}

/// Schneidet den Netzzugriff einer cgroup ab (Aktion
/// [`harw_dod_warden_proto::WardenAction::IsolateNetwork`]).
///
/// # Description
/// Siehe Moduldoku, Abschnitt „Warum `NetworkIsolator` keine echte
/// Implementierung hat" — diese Crate liefert nur [`RecordingExecutor`].
pub trait NetworkIsolator {
    /// Isoliert die angegebene cgroup vom Netz.
    ///
    /// # Arguments
    /// - `cgroup` (`&CgroupId`): die zu isolierende cgroup.
    ///
    /// # Returns
    /// `Ok(())` bei Erfolg.
    ///
    /// # Errors
    /// Implementierungsabhängig.
    fn isolate(&self, cgroup: &CgroupId) -> WardenResult<()>;

    /// Hebt eine zuvor gesetzte Netz-Isolation wieder auf (idempotent).
    /// Standard: nichts zu tun (Isolatoren ohne dauerhaften Zustand).
    ///
    /// # Errors
    /// Implementierungsabhängig.
    fn release_isolation(&self, _cgroup: &CgroupId) -> WardenResult<()> {
        Ok(())
    }
}

/// Beendet den Prozessbaum einer cgroup (Aktion
/// [`harw_dod_warden_proto::WardenAction::KillProcessTree`]). **Nicht
/// reversibel** (siehe `harw_dod_warden_proto::action`-Moduldoku).
pub trait ProcessTreeKiller {
    /// Beendet den Prozessbaum der angegebenen cgroup.
    ///
    /// # Arguments
    /// - `cgroup` (`&CgroupId`): die cgroup, deren Prozessbaum beendet wird.
    ///
    /// # Returns
    /// `Ok(())` bei Erfolg.
    ///
    /// # Errors
    /// - [`WardenError::InvalidCgroupId`] / [`WardenError::Io`]: siehe
    ///   jeweilige Implementierung.
    fn kill(&self, cgroup: &CgroupId) -> WardenResult<()>;
}

/// Weist eine cgroup-Kennung als sicheres Dateisystem-Pfadsegment aus.
///
/// # Description
/// Siehe Moduldoku, Abschnitt „Was `CgroupV2Executor` wirklich tut". Lehnt
/// jede Kennung ab, die einen Verzeichniswechsel ermöglichen würde.
///
/// # Errors
/// - [`WardenError::InvalidCgroupId`]: die Kennung enthält `/`, `..` oder
///   ein NUL-Byte.
fn validate_path_segment(cgroup: &CgroupId) -> WardenResult<&str> {
    let value = cgroup.as_str();
    if value.contains('/') || value.contains("..") || value.contains('\0') {
        return Err(WardenError::InvalidCgroupId);
    }
    Ok(value)
}

/// Echte cgroup-v2-Ausführung über Dateisystemschreibzugriffe.
///
/// # Description
/// Siehe Moduldoku, Abschnitt „Was `CgroupV2Executor` wirklich tut". Kein
/// Test dieser Crate lässt diesen Typ auf ein echtes `/sys/fs/cgroup`
/// zugreifen — Tests übergeben ein `tempfile`-Verzeichnis als `root`.
pub struct CgroupV2Executor {
    root: std::path::PathBuf,
}

impl CgroupV2Executor {
    /// Baut einen Ausführer, der unterhalb von `root` schreibt.
    ///
    /// # Arguments
    /// - `root` (`impl Into<PathBuf>`): das cgroup-v2-Wurzelverzeichnis
    ///   (Produktion: `/sys/fs/cgroup`; Tests: ein Tempdir).
    ///
    /// # Returns
    /// Einen neuen [`CgroupV2Executor`].
    #[must_use]
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn control_file(&self, cgroup: &CgroupId, file_name: &str) -> WardenResult<std::path::PathBuf> {
        let segment = validate_path_segment(cgroup)?;
        Ok(self.root.join(segment).join(file_name))
    }

    fn write_control_file(
        &self,
        cgroup: &CgroupId,
        file_name: &str,
        value: &str,
    ) -> WardenResult<()> {
        let path = self.control_file(cgroup, file_name)?;
        std::fs::write(path, value)?;
        Ok(())
    }
}

impl CgroupFreezer for CgroupV2Executor {
    fn freeze(&self, cgroup: &CgroupId) -> WardenResult<()> {
        self.write_control_file(cgroup, "cgroup.freeze", "1")
    }
}

impl CgroupReleaser for CgroupV2Executor {
    fn release(&self, cgroup: &CgroupId) -> WardenResult<()> {
        self.write_control_file(cgroup, "cgroup.freeze", "0")
    }
}

impl ProcessTreeKiller for CgroupV2Executor {
    fn kill(&self, cgroup: &CgroupId) -> WardenResult<()> {
        self.write_control_file(cgroup, "cgroup.kill", "1")
    }
}

/// Ein einzelner aufgezeichneter Aufruf an [`RecordingExecutor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordedCall {
    /// [`CgroupFreezer::freeze`] wurde für diese cgroup aufgerufen.
    Freeze(CgroupId),
    /// [`CgroupReleaser::release`] wurde für diese cgroup aufgerufen.
    Release(CgroupId),
    /// [`NetworkIsolator::isolate`] wurde für diese cgroup aufgerufen.
    Isolate(CgroupId),
    /// [`ProcessTreeKiller::kill`] wurde für diese cgroup aufgerufen.
    Kill(CgroupId),
}

/// Aufzeichnende Testimplementierung aller vier Ausführungs-Traits.
///
/// # Description
/// Friert nichts ein, tötet nichts, schneidet kein Netz ab — zeichnet nur
/// auf, welche Methode mit welcher cgroup aufgerufen wurde. Kann optional
/// so konfiguriert werden, dass sie fehlschlägt (`with_failure`), um den
/// Ausführungsfehler-Audit-Pfad zu testen, ohne einen echten Fehler
/// (z. B. eine gesperrte Datei) herbeiführen zu müssen.
#[derive(Debug, Default)]
pub struct RecordingExecutor {
    calls: std::sync::Mutex<Vec<RecordedCall>>,
    fail: bool,
}

impl RecordingExecutor {
    /// Baut einen Ausführer, der jeden Aufruf aufzeichnet und `Ok(())`
    /// zurückgibt.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Baut einen Ausführer, der jeden Aufruf aufzeichnet, aber
    /// [`WardenError::Io`] zurückgibt — für den
    /// Ausführungsfehler-Audit-Pfad.
    #[must_use]
    pub fn new_failing() -> Self {
        Self {
            calls: std::sync::Mutex::new(Vec::new()),
            fail: true,
        }
    }

    /// Gibt alle bisher aufgezeichneten Aufrufe zurück, in Aufrufreihenfolge.
    ///
    /// # Panics
    /// Wenn der interne Mutex vergiftet ist — für eine reine
    /// Testimplementierung akzeptabel.
    #[must_use]
    pub fn calls(&self) -> Vec<RecordedCall> {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn record(&self, call: RecordedCall) -> WardenResult<()> {
        let mut calls = self
            .calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        calls.push(call);
        if self.fail {
            return Err(WardenError::Io(std::io::Error::other(
                "aufgezeichneter Testfehler",
            )));
        }
        Ok(())
    }
}

impl CgroupFreezer for RecordingExecutor {
    fn freeze(&self, cgroup: &CgroupId) -> WardenResult<()> {
        self.record(RecordedCall::Freeze(cgroup.clone()))
    }
}

impl CgroupReleaser for RecordingExecutor {
    fn release(&self, cgroup: &CgroupId) -> WardenResult<()> {
        self.record(RecordedCall::Release(cgroup.clone()))
    }
}

impl NetworkIsolator for RecordingExecutor {
    fn isolate(&self, cgroup: &CgroupId) -> WardenResult<()> {
        self.record(RecordedCall::Isolate(cgroup.clone()))
    }
}

impl ProcessTreeKiller for RecordingExecutor {
    fn kill(&self, cgroup: &CgroupId) -> WardenResult<()> {
        self.record(RecordedCall::Kill(cgroup.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CgroupFreezer, CgroupReleaser, CgroupV2Executor, NetworkIsolator, ProcessTreeKiller,
        RecordedCall, RecordingExecutor, validate_path_segment,
    };
    use crate::error::WardenError;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_types::CgroupId;

    fn cgroup(id: &str) -> TestResult<CgroupId> {
        CgroupId::try_from_str(id).map_err(ctx("non-empty id"))
    }

    // -- validate_path_segment -------------------------------------------------

    #[test]
    fn test_validate_path_segment_accepts_plain_id() -> TestResult {
        let cg = cgroup("cgroup-1")?;
        assert_eq!(
            validate_path_segment(&cg).map_err(ctx("validate path segment"))?,
            "cgroup-1"
        );
        Ok(())
    }

    #[test]
    fn test_validate_path_segment_rejects_slash() -> TestResult {
        let cg = cgroup("a/b")?;
        let Err(err) = validate_path_segment(&cg) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, WardenError::InvalidCgroupId));
        Ok(())
    }

    #[test]
    fn test_validate_path_segment_rejects_parent_traversal() -> TestResult {
        let cg = cgroup("../etc")?;
        let Err(err) = validate_path_segment(&cg) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, WardenError::InvalidCgroupId));
        Ok(())
    }

    #[test]
    fn test_validate_path_segment_rejects_embedded_nul() -> TestResult {
        let cg = cgroup("a\0b")?;
        let Err(err) = validate_path_segment(&cg) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, WardenError::InvalidCgroupId));
        Ok(())
    }

    // -- CgroupV2Executor: keine echte Wirkung außerhalb eines Tempdirs -------

    #[test]
    fn test_cgroup_v2_executor_freeze_writes_control_file() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::fs::create_dir(dir.path().join("cgroup-1")).map_err(ctx("create cgroup dir"))?;
        let executor = CgroupV2Executor::new(dir.path());

        let cg = cgroup("cgroup-1")?;
        executor.freeze(&cg).map_err(ctx("freeze succeeds"))?;

        let written = std::fs::read_to_string(dir.path().join("cgroup-1").join("cgroup.freeze"))
            .map_err(ctx("control file written"))?;
        assert_eq!(written, "1");
        Ok(())
    }

    #[test]
    fn test_cgroup_v2_executor_release_writes_zero() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::fs::create_dir(dir.path().join("cgroup-1")).map_err(ctx("create cgroup dir"))?;
        let executor = CgroupV2Executor::new(dir.path());

        let cg = cgroup("cgroup-1")?;
        executor.release(&cg).map_err(ctx("release succeeds"))?;

        let written = std::fs::read_to_string(dir.path().join("cgroup-1").join("cgroup.freeze"))
            .map_err(ctx("control file written"))?;
        assert_eq!(written, "0");
        Ok(())
    }

    #[test]
    fn test_cgroup_v2_executor_kill_writes_control_file() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::fs::create_dir(dir.path().join("cgroup-1")).map_err(ctx("create cgroup dir"))?;
        let executor = CgroupV2Executor::new(dir.path());

        let cg = cgroup("cgroup-1")?;
        executor.kill(&cg).map_err(ctx("kill succeeds"))?;

        let written = std::fs::read_to_string(dir.path().join("cgroup-1").join("cgroup.kill"))
            .map_err(ctx("control file written"))?;
        assert_eq!(written, "1");
        Ok(())
    }

    #[test]
    fn test_cgroup_v2_executor_rejects_traversal_before_touching_filesystem() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let executor = CgroupV2Executor::new(dir.path());

        let cg = cgroup("../escape")?;
        let Err(err) = executor.freeze(&cg) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, WardenError::InvalidCgroupId));
        Ok(())
    }

    #[test]
    fn test_cgroup_v2_executor_reports_io_error_for_missing_directory() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let executor = CgroupV2Executor::new(dir.path());

        let cg = cgroup("does-not-exist")?;
        let Err(err) = executor.freeze(&cg) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, WardenError::Io(_)));
        Ok(())
    }

    // -- RecordingExecutor: jede Aktion über ihren Trait, nie echte Wirkung ---

    #[test]
    fn test_recording_executor_records_freeze_via_trait() -> TestResult {
        let executor = RecordingExecutor::new();
        let cg = cgroup("cgroup-1")?;
        CgroupFreezer::freeze(&executor, &cg).map_err(ctx("records, does not act"))?;
        assert_eq!(executor.calls(), vec![RecordedCall::Freeze(cg)]);
        Ok(())
    }

    #[test]
    fn test_recording_executor_records_release_via_trait() -> TestResult {
        let executor = RecordingExecutor::new();
        let cg = cgroup("cgroup-1")?;
        CgroupReleaser::release(&executor, &cg).map_err(ctx("records, does not act"))?;
        assert_eq!(executor.calls(), vec![RecordedCall::Release(cg)]);
        Ok(())
    }

    #[test]
    fn test_recording_executor_records_isolate_via_trait() -> TestResult {
        let executor = RecordingExecutor::new();
        let cg = cgroup("cgroup-1")?;
        NetworkIsolator::isolate(&executor, &cg).map_err(ctx("records, does not act"))?;
        assert_eq!(executor.calls(), vec![RecordedCall::Isolate(cg)]);
        Ok(())
    }

    #[test]
    fn test_recording_executor_records_kill_via_trait() -> TestResult {
        let executor = RecordingExecutor::new();
        let cg = cgroup("cgroup-1")?;
        ProcessTreeKiller::kill(&executor, &cg).map_err(ctx("records, does not act"))?;
        assert_eq!(executor.calls(), vec![RecordedCall::Kill(cg)]);
        Ok(())
    }

    #[test]
    fn test_failing_recording_executor_still_records_before_returning_err() -> TestResult {
        let executor = RecordingExecutor::new_failing();
        let cg = cgroup("cgroup-1")?;
        let Err(err) = CgroupFreezer::freeze(&executor, &cg) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, WardenError::Io(_)));
        assert_eq!(executor.calls(), vec![RecordedCall::Freeze(cg)]);
        Ok(())
    }
}
