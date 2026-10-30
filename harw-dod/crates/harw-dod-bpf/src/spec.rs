//! Beschreibung eines zu ladenden eBPF-Programms: Art, Anknüpfungspunkt, Rumpf.
//!
//! # Verantwortungsbereich
//! [`BpfProgramSpec`] ist reine Beschreibung — sie lädt nichts und erzeugt
//! keinen Bytecode. Der Rumpf ([`BpfProgramSource`]) entsteht mit einer
//! eigenen Toolchain (`clang`/`libbpf` o. ä.); diese Crate lädt ihn, sie
//! schreibt ihn nicht.
//!
//! # Wie der Rumpf hereinkommt — Entscheidung und Folgen
//! [`BpfProgramSource`] hat zwei Varianten:
//!
//! - [`BpfProgramSource::Embedded`]: Bytes, die zur Bauzeit des aufrufenden
//!   Binaries feststehen (typischerweise über `include_bytes!` erzeugt).
//!   **Reproduzierbar:** der geladene Bytecode ist untrennbar mit der
//!   Binary-Version verknüpft, mit der er ausgeliefert wurde — ein
//!   Rollback des Binaries rollt automatisch auch das Programm zurück, und
//!   ein Audit braucht nur das Binary zu prüfen, nicht zusätzlich einen
//!   Pfad auf der Zielmaschine. Kostet: jede Änderung am Bytecode verlangt
//!   einen Neubau des Binaries, und der Bytecode vergrößert dessen Größe.
//! - [`BpfProgramSource::Path`]: ein Dateisystempfad, der erst bei
//!   [`BpfProgramSource::resolve`] gelesen wird. **Leichtgewichtig:** das
//!   Binary bleibt klein, und ein frisch von der Toolchain gebauter
//!   Bytecode lässt sich austauschen, ohne diese Crate neu zu bauen.
//!   Kostet Reproduzierbarkeit: welcher Bytecode tatsächlich geladen wird,
//!   hängt vom Zustand der Zielmaschine zur Laufzeit ab, nicht mehr allein
//!   vom Binary — ein Audit muss zusätzlich den Pfad und seinen Inhalt zum
//!   Beobachtungszeitpunkt kennen.
//!
//! Beide Varianten bleiben verfügbar, weil beide Kosten in unterschiedlichen
//! Betriebsmodi überwiegen: ein fest ausgeliefertes Sicherheitsbinary will in
//! der Regel [`BpfProgramSource::Embedded`]; ein Entwicklungs- oder
//! Testaufbau mit häufig neu gebautem Bytecode profitiert von
//! [`BpfProgramSource::Path`].
//!
//! # Exportierte Typen
//! [`BpfProgramKind`], [`BpfProgramSource`], [`BpfProgramSpec`].
//!
//! # Nebenläufigkeit
//! Reine, unveränderliche Werte: `Send + Sync`. [`BpfProgramSource::resolve`]
//! führt Dateisystem-I/O aus (nur für [`BpfProgramSource::Path`]); sie
//! benötigt keine besondere Berechtigung — das Lesen einer Bytecode-Datei
//! ist kein `bpf()`-Syscall.
//!
//! # Fehler
//! [`crate::error::BpfError::Io`], wenn [`BpfProgramSource::resolve`] eine
//! [`BpfProgramSource::Path`]-Quelle nicht lesen kann.
//!
//! # Examples
//! ```rust
//! use harw_dod_bpf::{BpfProgramKind, BpfProgramSource, BpfProgramSpec};
//! use harw_types::SensorId;
//!
//! let spec = BpfProgramSpec::new(
//!     SensorId::from_str("procmon-0"),
//!     BpfProgramKind::Tracepoint,
//!     "syscalls:sys_enter_execve",
//!     BpfProgramSource::Embedded(std::borrow::Cow::Borrowed(b"\0asm".as_slice())),
//! );
//! assert_eq!(spec.kind, BpfProgramKind::Tracepoint);
//! ```

use std::borrow::Cow;
use std::path::PathBuf;

use harw_types::SensorId;

use crate::error::BpfError;

/// Die Art eines eBPF-Programms.
///
/// # Description
/// Bestimmt, an welcher Art von Anknüpfungspunkt ein geladener Ladeteil das
/// Programm einhängt. Geschlossen: eine neue Art ist eine bewusste
/// Entscheidung, kein freier String.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BpfProgramKind {
    /// Ein statischer Kernel-Tracepoint (z. B. `syscalls:sys_enter_execve`).
    Tracepoint,
    /// Eine dynamische Sonde auf eine Kernel-Funktion.
    KProbe,
    /// A BTF-typed function-entry hook.  This is used for TCP connect so the
    /// event is produced in the calling task, never from a later socket-state
    /// callback that may run in softirq context.
    FEntry,
    /// Ein Programm, das an einen rohen Socket angehängt wird.
    SocketFilter,
}

/// Woher der Bytecode-Rumpf eines Programms kommt.
///
/// # Description
/// Siehe Moduldoku für die Reproduzierbarkeits-Abwägung zwischen beiden
/// Varianten. Diese Crate erzeugt in keiner Variante selbst Bytecode.
#[derive(Debug, Clone, PartialEq)]
pub enum BpfProgramSource {
    /// Zur Bauzeit feststehende Bytes (typischerweise `include_bytes!`).
    ///
    /// `'static` deckt den `include_bytes!`-Fall ohne Kopie ab;
    /// [`Cow::Owned`] bleibt für Aufrufer verfügbar, die Bytes zur Laufzeit
    /// zusammensetzen (z. B. in Tests).
    Embedded(Cow<'static, [u8]>),
    /// Ein Dateisystempfad, der erst bei [`BpfProgramSource::resolve`]
    /// gelesen wird.
    Path(PathBuf),
}

impl BpfProgramSource {
    /// Löst diese Quelle zu den tatsächlichen Bytecode-Bytes auf.
    ///
    /// # Description
    /// Für [`Self::Embedded`] eine unfehlbare, kopiefreie Ausleihe. Für
    /// [`Self::Path`] eine Dateisystem-Lesung — kein `bpf()`-Syscall, keine
    /// besondere Berechtigung nötig; nur das eigentliche Laden in den
    /// Kernel (außerhalb dieser Crate, siehe `crate`-Moduldoku) verlangt
    /// `CAP_BPF`.
    ///
    /// # Returns
    /// Die Bytecode-Bytes, geliehen für [`Self::Embedded`], neu allokiert
    /// für [`Self::Path`].
    ///
    /// # Errors
    /// - [`BpfError::Io`], wenn [`Self::Path`] nicht gelesen werden kann.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_bpf::BpfProgramSource;
    /// use std::borrow::Cow;
    ///
    /// let source = BpfProgramSource::Embedded(Cow::Borrowed(b"\0asm".as_slice()));
    /// let bytes = source.resolve().expect("embedded bytes are always resolvable");
    /// assert_eq!(bytes.as_ref(), b"\0asm");
    /// ```
    pub fn resolve(&self) -> Result<Cow<'_, [u8]>, BpfError> {
        match self {
            Self::Embedded(bytes) => Ok(Cow::Borrowed(bytes.as_ref())),
            Self::Path(path) => std::fs::read(path).map(Cow::Owned).map_err(BpfError::from),
        }
    }
}

/// Beschreibt ein zu ladendes eBPF-Programm.
///
/// # Description
/// Reine Beschreibung ohne Ladeverhalten. `sensor` verknüpft die
/// entstehenden [`crate::handle::BpfHandle`]-Werte mit dem Sensor, der sie
/// besitzt, damit ein Sentinel eine Degradierung (siehe
/// [`crate::error::BpfError::CapabilityUnavailable`]) dem richtigen Sensor
/// zuordnen kann.
#[derive(Debug, Clone, PartialEq)]
pub struct BpfProgramSpec {
    /// Der Sensor, dem ein aus dieser Beschreibung geladenes Programm dient.
    pub sensor: SensorId,
    /// Die Art des Programms.
    pub kind: BpfProgramKind,
    /// Der Anknüpfungspunkt, dessen Bedeutung von `kind` abhängt — z. B.
    /// `"syscalls:sys_enter_execve"` für [`BpfProgramKind::Tracepoint`],
    /// ein Funktionsname für [`BpfProgramKind::KProbe`], ein
    /// Schnittstellenname für [`BpfProgramKind::SocketFilter`].
    pub attach_point: String,
    /// Woher der Bytecode-Rumpf kommt.
    pub source: BpfProgramSource,
}

impl BpfProgramSpec {
    /// Baut eine Programmbeschreibung.
    ///
    /// # Arguments
    /// - `sensor` (`harw_types::SensorId`): der besitzende Sensor.
    /// - `kind` (`BpfProgramKind`): die Art des Programms.
    /// - `attach_point` (`impl Into<String>`): der Anknüpfungspunkt.
    /// - `source` (`BpfProgramSource`): woher der Rumpf kommt.
    ///
    /// # Returns
    /// Eine neue `BpfProgramSpec` mit exakt den übergebenen Werten.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_bpf::{BpfProgramKind, BpfProgramSource, BpfProgramSpec};
    /// use harw_types::SensorId;
    /// use std::borrow::Cow;
    ///
    /// let spec = BpfProgramSpec::new(
    ///     SensorId::from_str("flow-0"),
    ///     BpfProgramKind::SocketFilter,
    ///     "eth0",
    ///     BpfProgramSource::Embedded(Cow::Borrowed(b"\0asm".as_slice())),
    /// );
    /// assert_eq!(spec.attach_point, "eth0");
    /// ```
    #[must_use]
    pub fn new(
        sensor: SensorId,
        kind: BpfProgramKind,
        attach_point: impl Into<String>,
        source: BpfProgramSource,
    ) -> Self {
        Self {
            sensor,
            kind,
            attach_point: attach_point.into(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BpfProgramKind, BpfProgramSource, BpfProgramSpec};
    use crate::error::BpfError;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_types::SensorId;
    use std::borrow::Cow;

    fn sample_spec(source: BpfProgramSource) -> BpfProgramSpec {
        BpfProgramSpec::new(
            SensorId::from_str("procmon-0"),
            BpfProgramKind::Tracepoint,
            "syscalls:sys_enter_execve",
            source,
        )
    }

    #[test]
    fn test_new_stores_all_fields_exactly() {
        let spec = sample_spec(BpfProgramSource::Embedded(Cow::Borrowed(
            b"\0asm".as_slice(),
        )));
        assert_eq!(spec.sensor, SensorId::from_str("procmon-0"));
        assert_eq!(spec.kind, BpfProgramKind::Tracepoint);
        assert_eq!(spec.attach_point, "syscalls:sys_enter_execve");
    }

    #[test]
    fn test_resolve_embedded_returns_the_same_bytes_without_copying() -> TestResult {
        let source = BpfProgramSource::Embedded(Cow::Borrowed(b"\0asm".as_slice()));
        let resolved = source
            .resolve()
            .map_err(ctx("embedded source always resolves"))?;
        assert_eq!(resolved.as_ref(), b"\0asm");
        assert!(matches!(resolved, Cow::Borrowed(_)));
        Ok(())
    }

    #[test]
    fn test_resolve_path_reads_file_contents() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir for program body"))?;
        let file_path = dir.path().join("program.bpf.o");
        std::fs::write(&file_path, b"bytecode-bytes").map_err(ctx("write fixture program body"))?;

        let source = BpfProgramSource::Path(file_path);
        let resolved = source.resolve().map_err(ctx("existing file resolves"))?;
        assert_eq!(resolved.as_ref(), b"bytecode-bytes");
        Ok(())
    }

    #[test]
    fn test_resolve_path_missing_file_returns_io_error() -> TestResult {
        let source =
            BpfProgramSource::Path(std::path::PathBuf::from("/nonexistent/does-not-exist.o"));
        let Err(err) = source.resolve() else {
            return Err(TestError::Unexpected("missing file must fail".to_owned()));
        };
        assert!(matches!(err, BpfError::Io(_)));
        Ok(())
    }
}
