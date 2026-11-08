//! Landlock-Selbstbeschränkung beim Start — hart, nicht degradierend, und
//! ohne eine einzige erlaubte Dateisystemwurzel.
//!
//! # Die Landlock-Asymmetrie
//! `harw-sentinel` (`src/sandbox.rs`) **degradiert** bei fehlender
//! Landlock-Unterstützung nur, weil es keine erhöhte Fähigkeit hält. Diese
//! Sonde läuft mit `CAP_AUDIT_READ`; Landlock ist hier die Schranke, die
//! ihren Dateisystemzugriff gegen ihr volles Berechtigungsvermögen
//! einschränkt. [`enforce_no_fs_access`] liefert deshalb `Err`, sobald der
//! Ausgang irgendetwas außer vollständiger Durchsetzung ist, und `main`
//! behandelt das als harten Startfehler — dieselbe Entscheidung, die
//! `harw-probe-bpf` und `harw-probe-fs` bereits treffen (Entscheidung Nr. 4
//! des Architekturberichts: jede privilegierte Sonde bricht hart ab).
//!
//! # Warum keine Wurzel
//! Anders als `harw-probe-fs` (fanotify auf konfigurierten Verzeichnissen)
//! und `harw-probe-bpf` (BTF, tracefs, `/proc/self`) braucht diese Sonde nach
//! dem Start **keinen** Pfad mehr: der Audit-Socket ist geöffnet und
//! gebunden, der Sentinel-Socket verbunden, `tracing` schreibt auf den
//! bereits offenen stderr-Deskriptor, und `jiff::Timestamp::now` liest die
//! Uhr per Systemaufruf, nicht aus einer Datei. Die Regel behandelt deshalb
//! alle Dateisystemrechte der ABI-Stufe V1 (`AccessFs::from_all`: lesen,
//! schreiben, ausführen, anlegen, löschen) und erlaubt **keine** Wurzel —
//! die engste Regel, die Landlock kennt.
//!
//! # Nur `FullyEnforced` zählt als Erfolg
//! `PartiallyEnforced` und `NotEnforced` gelten wie bei den anderen Sonden
//! als „Landlock nicht verfügbar". `CompatLevel::BestEffort` sorgt nur dafür,
//! dass ein älterer Kernel einen Status statt eines harten Fehlers aus der
//! `landlock`-Crate liefert; entschieden wird danach hier.
//!
//! # Ohne `unsafe`
//! Jeder hier aufgerufene Baustein der `landlock`-Crate (`Ruleset`,
//! `RulesetCreated`, `restrict_self`) ist eine sichere Funktion. Gegen die
//! API in `landlock-0.4.7` geschrieben, aber **nie gegen einen echten Kernel
//! ausgeführt**.
//!
//! # Exportierte Typen
//! Keine — die Funktion [`enforce_no_fs_access`].
//!
//! # Nebenläufigkeit
//! Muss aus dem Hauptthread aufgerufen werden, bevor ein weiterer Thread
//! entsteht — eine Landlock-Regel gilt für alle danach entstehenden Threads.
//!
//! # Fehler
//! [`crate::error::ProbeError::LandlockUnavailable`].
//!
//! # Examples
//! ```rust,ignore
//! use crate::landlock::enforce_no_fs_access;
//!
//! enforce_no_fs_access()?;
//! # Ok::<(), crate::error::ProbeError>(())
//! ```

use landlock::{
    ABI, Access, AccessFs, BitFlags, CompatLevel, Compatible, RestrictionStatus, Ruleset,
    RulesetAttr, RulesetStatus,
};

use crate::error::ProbeError;

/// Die ABI-Stufe, deren Rechte die Regel behandelt.
const LANDLOCK_ABI: ABI = ABI::V1;

/// Die Rechte, die die Regel behandelt — und damit, ohne erlaubte Wurzel,
/// verbietet.
///
/// # Arguments
/// - `abi` (`landlock::ABI`): die ABI-Stufe.
///
/// # Returns
/// `AccessFs::from_all(abi)`: Lese- und Schreibrechte zusammen.
fn handled_access(abi: ABI) -> BitFlags<AccessFs> {
    AccessFs::from_all(abi)
}

/// Verbietet diesem Prozess jeden Dateisystemzugriff über Landlock.
///
/// # Description
/// Baut ein `landlock::Ruleset` mit `CompatLevel::BestEffort`, behandelt
/// [`handled_access`] der ABI-Stufe V1, fügt **keine** Regel hinzu und ruft
/// `restrict_self()` auf. Nur [`RulesetStatus::FullyEnforced`] gilt als
/// Erfolg.
///
/// # Returns
/// `Ok(())` ausschließlich bei `RulesetStatus::FullyEnforced`.
///
/// # Errors
/// [`ProbeError::LandlockUnavailable`] bei jedem Aufbaufehler
/// (`handle_access`, `create`, `restrict_self`) und bei jedem Ausgang außer
/// `FullyEnforced`.
pub fn enforce_no_fs_access() -> Result<(), ProbeError> {
    let ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(handled_access(LANDLOCK_ABI))
        .map_err(|error| {
            tracing::error!(error = %error, "landlock handle_access failed; refusing to start");
            ProbeError::LandlockUnavailable
        })?;

    let created = ruleset.create().map_err(|error| {
        tracing::error!(error = %error, "landlock ruleset create failed; refusing to start");
        ProbeError::LandlockUnavailable
    })?;

    let status: RestrictionStatus = created.restrict_self().map_err(|error| {
        tracing::error!(error = %error, "landlock restrict_self failed; refusing to start");
        ProbeError::LandlockUnavailable
    })?;

    if status.ruleset == RulesetStatus::FullyEnforced {
        tracing::info!("landlock denies all file system access");
        Ok(())
    } else {
        tracing::error!(
            status = ?status.ruleset,
            "landlock did not fully enforce the empty file system scope; refusing to start"
        );
        Err(ProbeError::LandlockUnavailable)
    }
}

// Kein Test ruft `enforce_no_fs_access` auf (echtes `restrict_self` würde
// den Testprozess selbst einsperren) — dieselbe Disziplin wie bei
// `harw-probe-bpf::landlock`. Geprüft wird nur die reine Rechtemenge.
#[cfg(test)]
mod tests {
    use landlock::{ABI, AccessFs};

    use super::{LANDLOCK_ABI, handled_access};

    #[test]
    fn test_handled_access_covers_read_and_write_rights() {
        let rights = handled_access(ABI::V1);
        assert_eq!(
            rights,
            AccessFs::from_read(ABI::V1) | AccessFs::from_write(ABI::V1)
        );
        assert!(rights.contains(AccessFs::ReadFile));
        assert!(rights.contains(AccessFs::ReadDir));
        assert!(rights.contains(AccessFs::Execute));
        assert!(rights.contains(AccessFs::WriteFile));
        assert!(rights.contains(AccessFs::MakeReg));
    }

    #[test]
    fn test_the_ruleset_uses_abi_v1() {
        assert_eq!(LANDLOCK_ABI, ABI::V1);
    }
}
