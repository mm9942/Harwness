//! Reale eBPF-Ladeschicht für diese Sonde.
//!
//! # Verantwortungsbereich
//! Diese Datei ist die einzige Stelle dieser Sonde, die einen
//! [`harw_dod_bpf::RealBpfLoader`] baut, und die einzige, die Objektverträge
//! ([`harw_dod_bpf::BpfObjectContract`]) tatsächlich in den Kernel lädt.
//!
//! # Warum ein konkretes `Arc<RealBpfLoader>` statt `Box<dyn BpfLoader>`
//! Die Trait-Methode `harw_dod_bpf::BpfLoader::load` auf `RealBpfLoader`
//! scheitert bewusst **immer** mit
//! [`harw_dod_bpf::BpfError::InvalidProgramContract`]: ein `BpfProgramSpec`
//! trägt keinen Profil-/Scope-Anteil, der Lader könnte also nicht belegen,
//! dass die Scope-Map vor dem Anheften befüllt wurde. Der Produktionsweg
//! läuft deshalb ausschließlich über die inhärenten Methoden
//! `load_contracts`, `read_wire_events`, `loss_counters` und `unload` — die
//! es nur auf dem konkreten Typ gibt. Das `Arc` erlaubt, denselben Lader
//! (und damit denselben laderweiten Zähler `invalid_wire_events`) zwischen
//! Ladeschritt, Sammelschleife ([`crate::collect`]) und kontrolliertem Stopp
//! zu teilen.
//!
//! # Was [`build_real_loader`] tut
//! Baut einen leeren [`harw_dod_bpf::RealBpfLoader`]. Das Konstruieren führt
//! **keinen** Kernel- oder Berechtigungszugriff aus (siehe
//! `RealBpfLoader::new`) und schlägt deshalb nie fehl; die `Result`-Form
//! bleibt nur, damit Aufrufer sie einheitlich mit `?` behandeln können.
//!
//! # Was [`load_sensor`] tut
//! Lädt alle Objektverträge **eines** Sensors transaktional über
//! `RealBpfLoader::load_contracts`: scheitert ein Objekt, werden alle in
//! diesem Aufruf bereits angehefteten Objekte wieder entfernt, bevor der
//! Fehler zurückkommt. Eine leere Vertragsliste wird abgelehnt.
//!
//! # Exportierte Typen
//! Keine — nur die Funktionen [`build_real_loader`] und [`load_sensor`].
//!
//! # Nebenläufigkeit
//! Zustandslos. `RealBpfLoader` schützt seine Registrierung selbst mit einem
//! `Mutex` und ist `Send + Sync`; das zurückgegebene `Arc` darf frei geteilt
//! werden.
//!
//! # Fehler
//! [`build_real_loader`] ist total. [`load_sensor`] bildet jeden
//! `harw_dod_bpf::BpfError` auf [`ProbeError::BpfLoad`] ab — u. a.
//! `AttachCapabilitiesUnavailable`/`CapabilityUnavailable` (fehlende
//! Berechtigungen) und `InvalidProgramContract` (Vertrag oder Objekt passt
//! nicht, leere Liste).
//!
//! # Examples
//! ```rust,ignore
//! use crate::real_loader::{build_real_loader, load_sensor};
//!
//! let loader = build_real_loader()?;
//! let handles = load_sensor(&loader, &contracts)?;
//! ```

use std::sync::Arc;

use harw_dod_bpf::{BpfHandle, BpfObjectContract, RealBpfLoader};

use crate::error::ProbeError;

/// Baut die reale eBPF-Ladeschicht.
///
/// # Description
/// Siehe Moduldoku: kein Kernel-, kein Berechtigungszugriff.
///
/// # Returns
/// Einen leeren, teilbaren [`harw_dod_bpf::RealBpfLoader`].
///
/// # Errors
/// Keine — diese Funktion ist total; siehe Moduldoku.
pub fn build_real_loader() -> Result<Arc<RealBpfLoader>, ProbeError> {
    Ok(Arc::new(RealBpfLoader::new()))
}

/// Lädt den vollständigen Objektsatz eines Sensors transaktional.
///
/// # Description
/// Delegiert an `RealBpfLoader::load_contracts`. Entweder sind danach alle
/// `contracts` geladen und angeheftet, oder keiner.
///
/// # Arguments
/// - `loader`: die gemeinsame reale Ladeschicht.
/// - `contracts`: die Objektverträge genau eines Sensors, z. B. aus
///   `harw_dod_procmon::procmon_contracts` oder `harw_dod_flow::flow_contracts`.
///
/// # Returns
/// Die Handles in der Reihenfolge von `contracts`.
///
/// # Errors
/// [`ProbeError::BpfLoad`] mit dem zugrunde liegenden
/// `harw_dod_bpf::BpfError`, u. a.:
/// - `InvalidProgramContract`: leere Liste, ungültiger Vertrag oder ein
///   Objekt, das dem v1-Vertrag nicht entspricht.
/// - `AttachCapabilitiesUnavailable`: dem Prozess fehlen die zum Anheften
///   nötigen Fähigkeiten.
/// - `Io`, `ProgramLoadFailed`: Objekt nicht lesbar bzw. `aya`-seitiger
///   Lade-/Anheftfehler.
pub fn load_sensor(
    loader: &RealBpfLoader,
    contracts: &[BpfObjectContract],
) -> Result<Vec<BpfHandle>, ProbeError> {
    loader.load_contracts(contracts).map_err(ProbeError::from)
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use harw_dod_bpf::{BpfError, BpfObjectContract, BpfProgramKind, BpfProgramSource, BpfScope};
    use harw_types::SensorId;

    use super::{build_real_loader, load_sensor};
    use crate::error::ProbeError;
    use crate::test_support::{TestError, TestResult};

    #[test]
    fn test_build_real_loader_succeeds_without_kernel_access() -> TestResult {
        // Kein Kernel-, kein Berechtigungszugriff: das Bauen selbst schlägt
        // nie fehl. Siehe Aufgabenregel „Lade in keinem Test ein echtes
        // eBPF-Programm".
        let loader = build_real_loader().map_err(|err| TestError::Context {
            context: "build_real_loader",
            source: err.to_string(),
        })?;
        assert_eq!(std::sync::Arc::strong_count(&loader), 1);
        Ok(())
    }

    #[test]
    fn test_load_sensor_rejects_an_empty_contract_list() -> TestResult {
        // `load_contracts` prüft die leere Liste vor jedem Kernelzugriff.
        let loader = build_real_loader().map_err(|err| TestError::Context {
            context: "build_real_loader",
            source: err.to_string(),
        })?;
        match load_sensor(&loader, &[]) {
            Err(ProbeError::BpfLoad(BpfError::InvalidProgramContract)) => Ok(()),
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }

    #[test]
    fn test_load_sensor_rejects_an_invalid_contract_before_kernel_access() -> TestResult {
        // Ein unbekannter Programmname scheitert in `BpfObjectContract::validate`,
        // also vor Berechtigungsprüfung und `aya`.
        let loader = build_real_loader().map_err(|err| TestError::Context {
            context: "build_real_loader",
            source: err.to_string(),
        })?;
        let contract = BpfObjectContract::new(
            SensorId::from_str("procmon-0"),
            "not_a_dod_program",
            BpfProgramKind::Tracepoint,
            "sched:sched_process_exec",
            BpfProgramSource::Embedded(Cow::Borrowed(b"ELF")),
            BpfScope::Host,
        );
        match load_sensor(&loader, &[contract]) {
            Err(ProbeError::BpfLoad(BpfError::InvalidProgramContract)) => Ok(()),
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }
}
