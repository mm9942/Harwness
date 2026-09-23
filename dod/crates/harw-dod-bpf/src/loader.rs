//! Der eine Trait, hinter dem jede eBPF-Ladeschicht steckt.
//!
//! # Verantwortungsbereich
//! [`BpfLoader`] ist die gesamte Fassade zum Kernel: laden, anheften, aus
//! dem Ringpuffer lesen. Keine Implementierung dieses Traits gibt einen
//! `aya`-Typ (oder den Typ einer anderen eBPF-Bibliothek) in einer
//! öffentlichen Signatur preis — [`crate::spec::BpfProgramSpec`],
//! [`crate::handle::BpfHandle`], [`crate::event::RawBpfEvent`] und
//! [`crate::error::BpfError`] sind eigene Typen dieser Crate (siehe
//! `crate`-Moduldoku für die Begründung).
//!
//! # Zwei Implementierungen, zwei Berechtigungsstufen
//! - [`crate::fixture::FixtureBpfLoader`]: liefert vorbereitete Ereignisse
//!   aus einer Liste. Braucht **keinen** Kernel, **keine** Berechtigung,
//!   **keine** eBPF-Toolchain — Teil der normalen API dieser Crate, nicht
//!   hinter `#[cfg(test)]`, weil `harw-dod-procmon` (AW7-01b) und
//!   `harw-dod-flow` (AW7-01c) sie für ihre eigenen Tests brauchen.
//! - Ein echter, Linux-spezifischer Lader: **nicht Teil dieser Lieferung**
//!   (siehe `crate`-Moduldoku, Abschnitt „Abweichung"). Er implementiert
//!   diesen Trait genauso und liefert
//!   [`crate::error::BpfError::CapabilityUnavailable`] auf einem Host ohne
//!   `CAP_BPF`, statt zu scheitern.
//!
//! # Exportierte Typen
//! [`BpfLoader`].
//!
//! # Nebenläufigkeit
//! `BpfLoader: Send + Sync`: ein Sentinel hält Lader hinter `Arc` und ruft
//! sie aus einem Sammelthread auf, wie `harw_dod_signals::Sensor` es für
//! Sensoren tut. Beide Methoden nehmen `&self` — eine Implementierung, die
//! veränderlichen Zustand braucht (z. B. eine Zuordnung von Griff zu
//! internem Ladezustand), muss dafür innere Veränderlichkeit
//! (`std::sync::Mutex`, `std::sync::atomic::*`) einsetzen, siehe
//! [`crate::fixture::FixtureBpfLoader`] als Vorbild.
//!
//! # Fehler
//! [`crate::error::BpfError`], insbesondere
//! [`crate::error::BpfError::CapabilityUnavailable`] als dokumentierter,
//! lauffähiger Betriebsfall — kein Absturz.
//!
//! # Examples
//! ```rust
//! use harw_dod_bpf::{BpfHandle, BpfLoader, BpfProgramSpec};
//! use harw_dod_bpf::error::BpfError;
//! use harw_dod_bpf::event::RawBpfEvent;
//! use std::time::Duration;
//!
//! fn load_all(loader: &dyn BpfLoader, specs: &[BpfProgramSpec]) -> Vec<BpfHandle> {
//!     specs.iter().filter_map(|spec| loader.load(spec).ok()).collect()
//! }
//!
//! fn read_all(loader: &dyn BpfLoader, handles: &[BpfHandle]) -> Result<Vec<RawBpfEvent>, BpfError> {
//!     let mut all = Vec::new();
//!     for handle in handles {
//!         all.extend(loader.read_events(handle, Duration::from_millis(0))?);
//!     }
//!     Ok(all)
//! }
//!
//! let loader = harw_dod_bpf::FixtureBpfLoader::new(Vec::new());
//! let handles = load_all(&loader, &[]);
//! assert!(read_all(&loader, &handles).unwrap().is_empty());
//! ```

use std::time::Duration;

use crate::error::BpfError;
use crate::event::RawBpfEvent;
use crate::handle::BpfHandle;
use crate::spec::BpfProgramSpec;

/// Ein Ziel, in das eBPF-Programme geladen und aus dem Ereignisse gelesen werden.
///
/// # Description
/// Objektsicher (`dyn BpfLoader`): ein Sentinel hält heterogene Lader in
/// einer gemeinsamen Sammlung, genau wie `harw_dod_signals::Sensor` es für
/// Sensoren erlaubt.
///
/// # Errors
/// [`BpfError`] — insbesondere [`BpfError::CapabilityUnavailable`] als
/// dokumentierter, lauffähiger Fall eines Hosts ohne die nötige Fähigkeit.
///
/// # Concurrency
/// `Send + Sync`, beide Methoden nehmen `&self`. Siehe Moduldoku.
pub trait BpfLoader: Send + Sync {
    /// Lädt ein Programm und heftet es an seinen Anknüpfungspunkt.
    ///
    /// # Arguments
    /// - `program` (`&BpfProgramSpec`): Art, Anknüpfungspunkt und
    ///   Rumpfquelle des zu ladenden Programms.
    ///
    /// # Returns
    /// Einen [`BpfHandle`], über den [`Self::read_events`] die von diesem
    /// Programm erzeugten Ereignisse liest.
    ///
    /// # Errors
    /// - [`BpfError::CapabilityUnavailable`], wenn der Host die zum Laden
    ///   nötige Fähigkeit nicht anbietet (dokumentierter, lauffähiger Fall,
    ///   kein Absturz).
    /// - [`BpfError::Io`], wenn eine dateibasierte Rumpfquelle nicht
    ///   gelesen werden kann.
    fn load(&self, program: &BpfProgramSpec) -> Result<BpfHandle, BpfError>;

    /// Liest die nächsten Ereignisse aus dem Ringpuffer.
    ///
    /// # Arguments
    /// - `handle` (`&BpfHandle`): der Griff eines zuvor über [`Self::load`]
    ///   geladenen Programms.
    /// - `timeout` (`std::time::Duration`): wie lange auf mindestens ein
    ///   Ereignis gewartet werden darf, bevor mit einer leeren Liste
    ///   zurückgekehrt wird.
    ///
    /// # Returns
    /// Die seit dem letzten Aufruf neu eingetroffenen Ereignisse. Kann leer
    /// sein, wenn innerhalb von `timeout` keines eintraf.
    ///
    /// # Errors
    /// - [`BpfError::MalformedEvent`], wenn ein gelesener Puffer nicht die
    ///   erwartete Form hat.
    fn read_events(
        &self,
        handle: &BpfHandle,
        timeout: Duration,
    ) -> Result<Vec<RawBpfEvent>, BpfError>;
}

#[cfg(test)]
mod tests {
    use super::BpfLoader;
    use crate::error::BpfError;
    use crate::event::RawBpfEvent;
    use crate::fixture::FixtureBpfLoader;
    use crate::handle::BpfHandle;
    use crate::spec::{BpfProgramKind, BpfProgramSource, BpfProgramSpec};
    use crate::test_support::{TestResult, ctx};
    use harw_types::SensorId;
    use std::borrow::Cow;
    use std::time::Duration;

    fn sample_spec() -> BpfProgramSpec {
        BpfProgramSpec::new(
            SensorId::from_str("procmon-0"),
            BpfProgramKind::Tracepoint,
            "syscalls:sys_enter_execve",
            BpfProgramSource::Embedded(Cow::Borrowed(b"\0asm".as_slice())),
        )
    }

    #[test]
    fn test_boxed_loader_is_object_safe_and_usable_through_dyn() -> TestResult {
        let loader: Box<dyn BpfLoader> = Box::new(FixtureBpfLoader::new(Vec::new()));
        let handle = loader
            .load(&sample_spec())
            .map_err(ctx("fixture loader always succeeds"))?;
        let events = loader
            .read_events(&handle, Duration::from_millis(0))
            .map_err(ctx("fixture loader read_events never fails"))?;
        assert!(events.is_empty());
        Ok(())
    }

    #[test]
    fn test_boxed_loaders_can_be_held_in_a_heterogeneous_collection() {
        let loaders: Vec<Box<dyn BpfLoader>> = vec![
            Box::new(FixtureBpfLoader::new(Vec::new())),
            Box::new(FixtureBpfLoader::without_capability()),
        ];
        assert_eq!(loaders.len(), 2);

        let results: Vec<Result<BpfHandle, BpfError>> = loaders
            .iter()
            .map(|loader| loader.load(&sample_spec()))
            .collect();
        assert!(results[0].is_ok());
        assert!(matches!(results[1], Err(BpfError::CapabilityUnavailable)));
    }

    #[test]
    fn test_read_all_helper_collects_events_across_handles() -> TestResult {
        let event = RawBpfEvent {
            pid: 1,
            comm: "init".to_owned(),
            observed_at: jiff::Timestamp::UNIX_EPOCH,
            payload: vec![],
        };
        let loader = FixtureBpfLoader::new(vec![event.clone()]);
        let handle = loader
            .load(&sample_spec())
            .map_err(ctx("fixture loader always succeeds"))?;

        let events = loader
            .read_events(&handle, Duration::from_millis(0))
            .map_err(ctx("fixture loader read_events never fails"))?;
        assert_eq!(events, vec![event]);
        Ok(())
    }
}
