//! Opaker Griff auf ein geladenes eBPF-Programm.
//!
//! # Verantwortungsbereich
//! [`BpfHandle`] trägt keinen `aya`-Zustand und keinen Dateideskriptor —
//! nur die Angaben, die ein Aufrufer braucht, um ein geladenes Programm
//! wiederzuerkennen und einem Sensor zuzuordnen. Eine echte
//! [`crate::loader::BpfLoader`]-Implementierung (Ladeteil, außerhalb dieser
//! Lieferung — siehe `crate`-Moduldoku) hält ihren eigenen, `aya`-typisierten
//! Zustand privat und reicht nach außen ausschließlich `BpfHandle` weiter.
//!
//! # Exportierte Typen
//! [`BpfHandle`].
//!
//! # Nebenläufigkeit
//! Reiner, unveränderlicher Wert: `Send + Sync`. Die Vergabe der `id` läuft
//! über einen prozessweiten `AtomicU64`-Zähler, damit zwei
//! [`crate::loader::BpfLoader`]-Implementierungen — etwa eine
//! [`crate::fixture::FixtureBpfLoader`] und ein echter Ladeteil in
//! verschiedenen Crates — niemals kollidierende Griffe vergeben, auch wenn
//! `new` gleichzeitig aus mehreren Threads aufgerufen wird.
//!
//! # Fehler
//! Keine — [`BpfHandle::new`] ist total.
//!
//! # Examples
//! ```rust
//! use harw_dod_bpf::{BpfHandle, BpfProgramKind};
//! use harw_types::SensorId;
//!
//! let handle = BpfHandle::new(
//!     SensorId::from_str("procmon-0"),
//!     BpfProgramKind::Tracepoint,
//!     "syscalls:sys_enter_execve",
//! );
//! assert_eq!(handle.kind(), BpfProgramKind::Tracepoint);
//! assert_eq!(handle.attach_point(), "syscalls:sys_enter_execve");
//! ```

use std::sync::atomic::{AtomicU64, Ordering};

use harw_types::SensorId;

use crate::spec::BpfProgramKind;

// Prozessweiter Zähler statt eines Felds auf jeder `BpfLoader`-Implementierung:
// `BpfHandle::new` ist `pub`, weil auch eine Loader-Implementierung außerhalb
// dieser Crate (der künftige Ladeteil in AW7-01d) gültige Griffe erzeugen
// muss. Ein gemeinsamer, prozessweiter Zähler garantiert eindeutige IDs über
// alle Implementierungen hinweg, ohne dass jede Implementierung ihre eigene
// Zählung koordinieren müsste.
static NEXT_HANDLE_ID: AtomicU64 = AtomicU64::new(1);

/// Ein Griff auf ein geladenes eBPF-Programm.
///
/// # Description
/// Trägt keinen `aya`-Typ und keinen Dateideskriptor — nur Kennung, Sensor,
/// Art und Anknüpfungspunkt. Jede [`crate::loader::BpfLoader`]-Implementierung
/// (auch [`crate::fixture::FixtureBpfLoader`]) erzeugt ihre Griffe über
/// [`Self::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BpfHandle {
    id: u64,
    sensor: SensorId,
    kind: BpfProgramKind,
    attach_point: String,
}

impl BpfHandle {
    /// Erzeugt einen neuen, prozessweit eindeutigen Griff.
    ///
    /// # Arguments
    /// - `sensor` (`harw_types::SensorId`): der Sensor, dem dieses Programm
    ///   dient.
    /// - `kind` (`BpfProgramKind`): die Art des geladenen Programms.
    /// - `attach_point` (`impl Into<String>`): der Anknüpfungspunkt, an dem
    ///   das Programm eingehängt wurde.
    ///
    /// # Returns
    /// Einen `BpfHandle` mit einer neuen, noch nie vergebenen `id`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_bpf::{BpfHandle, BpfProgramKind};
    /// use harw_types::SensorId;
    ///
    /// let a = BpfHandle::new(SensorId::from_str("s"), BpfProgramKind::KProbe, "do_sys_open");
    /// let b = BpfHandle::new(SensorId::from_str("s"), BpfProgramKind::KProbe, "do_sys_open");
    /// assert_ne!(a.id(), b.id());
    /// ```
    #[must_use]
    pub fn new(sensor: SensorId, kind: BpfProgramKind, attach_point: impl Into<String>) -> Self {
        let id = NEXT_HANDLE_ID.fetch_add(1, Ordering::Relaxed);
        Self {
            id,
            sensor,
            kind,
            attach_point: attach_point.into(),
        }
    }

    /// Die prozessweit eindeutige Kennung dieses Griffs.
    ///
    /// # Returns
    /// Ein `u64`, das von keinem anderen `BpfHandle` in diesem Prozess
    /// wiederverwendet wird.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Der Sensor, dem dieses Programm dient.
    ///
    /// # Returns
    /// Referenz auf die bei [`Self::new`] übergebene `SensorId`.
    #[must_use]
    pub fn sensor(&self) -> &SensorId {
        &self.sensor
    }

    /// Die Art des geladenen Programms.
    ///
    /// # Returns
    /// Die bei [`Self::new`] übergebene [`BpfProgramKind`].
    #[must_use]
    pub fn kind(&self) -> BpfProgramKind {
        self.kind
    }

    /// Der Anknüpfungspunkt, an dem das Programm eingehängt wurde.
    ///
    /// # Returns
    /// Der bei [`Self::new`] übergebene Anknüpfungspunkt als `&str`.
    #[must_use]
    pub fn attach_point(&self) -> &str {
        &self.attach_point
    }
}

#[cfg(test)]
mod tests {
    use super::BpfHandle;
    use crate::spec::BpfProgramKind;
    use harw_types::SensorId;

    #[test]
    fn test_new_stores_sensor_kind_and_attach_point() {
        let handle = BpfHandle::new(
            SensorId::from_str("procmon-0"),
            BpfProgramKind::Tracepoint,
            "syscalls:sys_enter_execve",
        );
        assert_eq!(handle.sensor(), &SensorId::from_str("procmon-0"));
        assert_eq!(handle.kind(), BpfProgramKind::Tracepoint);
        assert_eq!(handle.attach_point(), "syscalls:sys_enter_execve");
    }

    #[test]
    fn test_new_assigns_distinct_ids_across_calls() {
        let a = BpfHandle::new(SensorId::from_str("s"), BpfProgramKind::KProbe, "do_sys_open");
        let b = BpfHandle::new(SensorId::from_str("s"), BpfProgramKind::KProbe, "do_sys_open");
        assert_ne!(a.id(), b.id());
    }

    #[test]
    fn test_new_assigns_distinct_ids_under_concurrent_construction() {
        let handles: Vec<BpfHandle> = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        BpfHandle::new(SensorId::from_str("s"), BpfProgramKind::SocketFilter, "eth0")
                    })
                })
                .collect();
            threads
                .into_iter()
                .map(|t| t.join().expect("handle-construction thread must not panic"))
                .collect()
        });

        let mut ids: Vec<u64> = handles.iter().map(BpfHandle::id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), handles.len(), "all concurrently created ids must be distinct");
    }
}
