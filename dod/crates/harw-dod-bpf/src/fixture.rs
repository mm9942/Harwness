//! `FixtureBpfLoader`: ein `BpfLoader`, der aus einer Liste vorbereiteter
//! Ereignisse liefert — kein Kernel, keine Berechtigung, keine
//! eBPF-Toolchain.
//!
//! # Verantwortungsbereich
//! Diese Datei ist Teil der **normalen** API dieser Crate, nicht hinter
//! `#[cfg(test)]`: `harw-dod-procmon` (AW7-01b) und `harw-dod-flow`
//! (AW7-01c) sind eigene Crates und brauchen [`FixtureBpfLoader`] als echte
//! Abhängigkeit für ihre eigenen Tests, nicht nur für die Tests dieser
//! Crate.
//!
//! # Zwei Betriebsmodi
//! - [`FixtureBpfLoader::new`]: [`crate::loader::BpfLoader::load`] gelingt
//!   immer und liefert die konfigurierten Ereignisse einmalig über
//!   [`crate::loader::BpfLoader::read_events`] — genau der Fall eines Hosts
//!   **mit** der nötigen Fähigkeit.
//! - [`FixtureBpfLoader::without_capability`] — [`crate::loader::BpfLoader::load`]
//!   liefert immer [`crate::error::BpfError::CapabilityUnavailable`] — genau
//!   der dokumentierte, lauffähige Fall eines Hosts **ohne** `CAP_BPF`
//!   (siehe `crate`-Moduldoku).
//!
//! # Grenzen dieser Fixture
//! Ein Griff trägt keine Beziehung zu einer bestimmten Teilmenge der
//! konfigurierten Ereignisse — alle über [`FixtureBpfLoader::new`] geladenen
//! Griffe teilen sich dieselbe Warteschlange. Das genügt dem in
//! `harw_dod_signals::sensor` festgehaltenen Grundsatz, dass eine
//! Sensor-Crate genau eine Quelle liest: wer mehrere Programme mit
//! unterschiedlichen erwarteten Ereignissen gegeneinander testen will,
//! braucht mehrere `FixtureBpfLoader`-Instanzen, keine gemeinsame.
//!
//! # Exportierte Typen
//! [`FixtureBpfLoader`].
//!
//! # Nebenläufigkeit
//! `Send + Sync`: die Warteschlange steckt hinter einem `std::sync::Mutex`,
//! weil [`crate::loader::BpfLoader::read_events`] nur `&self` bekommt (siehe
//! Trait-Vertrag) und die Warteschlange beim Lesen verändert (Drain). Ein
//! vergifteter Mutex (nach einem Panic in einem anderen Thread, während der
//! die Sperre gehalten wurde) blockiert diese Fixture nicht: sie holt den
//! inneren Zustand über `PoisonError::into_inner` zurück, statt zu
//! `panic`en — ein Test-Doppelgänger, der selbst zur Fehlerquelle wird,
//! wäre schlechter als einer, der Vergiftung schlicht ignoriert.
//!
//! # Fehler
//! [`crate::error::BpfError::CapabilityUnavailable`] aus
//! [`FixtureBpfLoader::without_capability`]; [`FixtureBpfLoader::read_events`]
//! schlägt nie fehl.
//!
//! # Examples
//! ```rust
//! use harw_dod_bpf::{BpfLoader, BpfProgramKind, BpfProgramSource, BpfProgramSpec};
//! use harw_dod_bpf::event::RawBpfEvent;
//! use harw_dod_bpf::fixture::FixtureBpfLoader;
//! use harw_types::SensorId;
//! use std::borrow::Cow;
//! use std::time::Duration;
//!
//! let event = RawBpfEvent {
//!     pid: 42,
//!     comm: "sshd".to_owned(),
//!     observed_at: jiff::Timestamp::UNIX_EPOCH,
//!     payload: vec![],
//! };
//! let loader = FixtureBpfLoader::new(vec![event.clone()]);
//!
//! let spec = BpfProgramSpec::new(
//!     SensorId::from_str("procmon-0"),
//!     BpfProgramKind::Tracepoint,
//!     "syscalls:sys_enter_execve",
//!     BpfProgramSource::Embedded(Cow::Borrowed(b"\0asm".as_slice())),
//! );
//! let handle = loader.load(&spec).expect("fixture loader with capability always succeeds");
//! let events = loader.read_events(&handle, Duration::from_millis(0)).unwrap();
//! assert_eq!(events, vec![event]);
//! ```

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use crate::error::BpfError;
use crate::event::RawBpfEvent;
use crate::handle::BpfHandle;
use crate::loader::BpfLoader;
use crate::spec::BpfProgramSpec;

/// Ein `BpfLoader`, der aus einer Liste vorbereiteter Ereignisse liefert.
///
/// # Description
/// Siehe Moduldoku für die beiden Betriebsmodi und ihre Grenzen.
#[derive(Debug)]
pub struct FixtureBpfLoader {
    capability_available: bool,
    events: Mutex<VecDeque<RawBpfEvent>>,
}

impl FixtureBpfLoader {
    /// Baut eine Fixture, die Programme erfolgreich lädt.
    ///
    /// # Arguments
    /// - `events` (`Vec<RawBpfEvent>`): die Ereignisse, die
    ///   [`crate::loader::BpfLoader::read_events`] beim ersten Aufruf
    ///   (drainend, siehe [`Self::read_events`]) zurückgibt.
    ///
    /// # Returns
    /// Eine `FixtureBpfLoader`, deren `load` immer gelingt.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_bpf::fixture::FixtureBpfLoader;
    ///
    /// let _loader = FixtureBpfLoader::new(Vec::new());
    /// ```
    #[must_use]
    pub fn new(events: Vec<RawBpfEvent>) -> Self {
        Self {
            capability_available: true,
            events: Mutex::new(events.into()),
        }
    }

    /// Baut eine Fixture, die den Betriebsfall „keine Berechtigung" simuliert.
    ///
    /// # Description
    /// Jeder Aufruf von [`crate::loader::BpfLoader::load`] liefert
    /// [`BpfError::CapabilityUnavailable`] — genau das Verhalten, das ein
    /// Sentinel auf einem Host ohne `CAP_BPF` sehen muss, um den Sensor als
    /// degradiert zu melden statt zu scheitern.
    ///
    /// # Returns
    /// Eine `FixtureBpfLoader`, deren `load` immer
    /// [`BpfError::CapabilityUnavailable`] liefert.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_bpf::{BpfLoader, BpfProgramKind, BpfProgramSource, BpfProgramSpec};
    /// use harw_dod_bpf::error::BpfError;
    /// use harw_dod_bpf::fixture::FixtureBpfLoader;
    /// use harw_types::SensorId;
    /// use std::borrow::Cow;
    ///
    /// let loader = FixtureBpfLoader::without_capability();
    /// let spec = BpfProgramSpec::new(
    ///     SensorId::from_str("flow-0"),
    ///     BpfProgramKind::SocketFilter,
    ///     "eth0",
    ///     BpfProgramSource::Embedded(Cow::Borrowed(b"\0asm".as_slice())),
    /// );
    /// assert!(matches!(loader.load(&spec), Err(BpfError::CapabilityUnavailable)));
    /// ```
    #[must_use]
    pub fn without_capability() -> Self {
        Self {
            capability_available: false,
            events: Mutex::new(VecDeque::new()),
        }
    }
}

impl BpfLoader for FixtureBpfLoader {
    /// Siehe [`crate::loader::BpfLoader::load`].
    ///
    /// # Errors
    /// [`BpfError::CapabilityUnavailable`], wenn diese Fixture über
    /// [`Self::without_capability`] gebaut wurde.
    fn load(&self, program: &BpfProgramSpec) -> Result<BpfHandle, BpfError> {
        if !self.capability_available {
            return Err(BpfError::CapabilityUnavailable);
        }
        Ok(BpfHandle::new(
            program.sensor.clone(),
            program.kind,
            program.attach_point.clone(),
        ))
    }

    /// Siehe [`crate::loader::BpfLoader::read_events`].
    ///
    /// # Description
    /// Entleert die konfigurierte Warteschlange vollständig und gibt genau
    /// die entnommenen Ereignisse zurück — ein zweiter Aufruf ohne
    /// zwischenzeitliches Nachfüllen liefert eine leere Liste, wie ein
    /// echter Ringpuffer, der bereits leergelesen wurde. `handle` und
    /// `timeout` werden ignoriert: diese Fixture wartet nie und
    /// unterscheidet nicht zwischen Griffen (siehe Moduldoku, „Grenzen
    /// dieser Fixture").
    ///
    /// # Errors
    /// Keine — schlägt nie fehl.
    fn read_events(
        &self,
        _handle: &BpfHandle,
        _timeout: Duration,
    ) -> Result<Vec<RawBpfEvent>, BpfError> {
        let mut queue = self
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(queue.drain(..).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::FixtureBpfLoader;
    use crate::error::BpfError;
    use crate::event::RawBpfEvent;
    use crate::loader::BpfLoader;
    use crate::spec::{BpfProgramKind, BpfProgramSource, BpfProgramSpec};
    use crate::test_support::{TestError, TestResult, ctx};
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

    fn sample_event(pid: u32) -> RawBpfEvent {
        RawBpfEvent {
            pid,
            comm: "sshd".to_owned(),
            observed_at: jiff::Timestamp::UNIX_EPOCH,
            payload: vec![],
        }
    }

    #[test]
    fn test_load_with_capability_succeeds_and_carries_spec_fields() -> TestResult {
        let loader = FixtureBpfLoader::new(Vec::new());
        let spec = sample_spec();
        let handle = loader
            .load(&spec)
            .map_err(ctx("fixture loader with capability succeeds"))?;
        assert_eq!(handle.sensor(), &spec.sensor);
        assert_eq!(handle.kind(), spec.kind);
        assert_eq!(handle.attach_point(), spec.attach_point);
        Ok(())
    }

    #[test]
    fn test_load_without_capability_returns_capability_unavailable() -> TestResult {
        let loader = FixtureBpfLoader::without_capability();
        let result = loader.load(&sample_spec());
        let Err(err) = result else {
            return Err(TestError::Unexpected("must fail without capability".into()));
        };
        assert!(matches!(err, BpfError::CapabilityUnavailable));
        Ok(())
    }

    #[test]
    fn test_read_events_returns_exactly_the_configured_events() -> TestResult {
        let events = vec![sample_event(1), sample_event(2)];
        let loader = FixtureBpfLoader::new(events.clone());
        let handle = loader
            .load(&sample_spec())
            .map_err(ctx("fixture loader with capability succeeds"))?;

        let read_back = loader
            .read_events(&handle, Duration::from_millis(0))
            .map_err(ctx("fixture read_events never fails"))?;
        assert_eq!(read_back, events);
        Ok(())
    }

    #[test]
    fn test_read_events_drains_the_queue_so_a_second_call_is_empty() -> TestResult {
        let loader = FixtureBpfLoader::new(vec![sample_event(1)]);
        let handle = loader
            .load(&sample_spec())
            .map_err(ctx("fixture loader with capability succeeds"))?;

        let first = loader
            .read_events(&handle, Duration::from_millis(0))
            .map_err(ctx("read_events"))?;
        assert_eq!(first.len(), 1);

        let second = loader
            .read_events(&handle, Duration::from_millis(0))
            .map_err(ctx("read_events"))?;
        assert!(second.is_empty());
        Ok(())
    }

    #[test]
    fn test_without_capability_read_events_is_always_empty() -> TestResult {
        let loader = FixtureBpfLoader::without_capability();
        // `read_events` selbst kennt keine Berechtigungsprüfung — nur `load`
        // meldet `CapabilityUnavailable`. Ein Aufrufer, der `read_events`
        // ohne vorheriges erfolgreiches `load` aufruft, bekommt schlicht die
        // (hier leere) konfigurierte Warteschlange.
        let handle = crate::handle::BpfHandle::new(
            SensorId::from_str("procmon-0"),
            BpfProgramKind::Tracepoint,
            "syscalls:sys_enter_execve",
        );
        let events = loader
            .read_events(&handle, Duration::from_millis(0))
            .map_err(ctx("read_events"))?;
        assert!(events.is_empty());
        Ok(())
    }
}
