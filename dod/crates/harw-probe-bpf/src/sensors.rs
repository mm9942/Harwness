//! Sensor-Aufbau: löst die vier eBPF-Programmobjekte auf und stellt den
//! degradierten Platzhalter-Sensor bereit, wenn ein Sensor nicht geladen
//! werden kann.
//!
//! # Rolle im produktiven Pfad
//! Produktiv lädt diese Sonde ihre Objekte **nicht** über
//! `harw_dod_signals::Sensor`, sondern als v1-Vertragssatz über den
//! gemeinsamen `harw_dod_bpf::RealBpfLoader` (`harw_dod_procmon::procmon_contracts`,
//! `harw_dod_flow::flow_contracts`, geladen von
//! [`crate::real_loader::load_sensor`]) und liest sie über
//! [`crate::collect::drain_wire_once`]. Diese Datei liefert dafür nur zwei
//! Bausteine:
//!
//! - die **Objektauflösung** ([`resolve_procmon_objects`],
//!   [`resolve_flow_objects`]): je Sensor genau zwei geprüfte, bereits
//!   eingelesene Objekte, deren Bytes `main::run` vor der
//!   Landlock-Durchsetzung einliest;
//! - den **degradierten Sensor** ([`UnavailableSensor`]): das einzige
//!   `harw_dod_signals::Sensor`-Objekt dieser Sonde, das die Sammelschleife
//!   über [`crate::collect::run_once`] bedient.
//!
//! # Die geerbte Schnittstellen-Unstimmigkeit (erledigt)
//! `harw-dod-procmon` und `harw-dod-flow` sind unabhängig voneinander
//! gebaut worden (Invariante C7: Geschwistercrates, die einander nicht
//! kennen dürfen). Ursprünglich implementierte nur `harw-dod-procmon`
//! `harw_dod_signals::Sensor` (`ProcmonSensor`), `harw-dod-flow` bot nur
//! `observe()`; diese Sonde glich das mit einem lokalen `FlowSensor`-Adapter
//! aus. Das Urteil dieser Sonde als einziger Konsument war: `harw-dod-procmon`s
//! Form (Trait implementieren, `sensor_suite!` bewusst nicht verwenden,
//! ehrliche Handtests) ist die richtige. `harw-dod-flow` hat das inzwischen
//! übernommen (K72: `harw_dod_flow::FlowSensor` implementiert `Sensor`).
//!
//! Für diese Sonde ist die Frage damit doppelt erledigt: beide Formungscrates
//! bieten heute dieselbe Form, und der produktive Weg führt ohnehin über die
//! v1-Verträge und Wire-Ereignisse, nicht über `Sensor::poll` (siehe beide
//! Crate-Dokus: der `Sensor`/`RawBpfEvent`-Pfad dort ist Fixture- und
//! Übergangskompatibilität). Der lokale Adapter, die alten Einzelprogramm-
//! Bauer und der Anknüpfungspunkt-Alias sind deshalb entfernt.
//!
//! # Genau eine Fähigkeit
//! Beide Sensoren dieser Sonde tragen `harw_dod_cap::Capability::LoadBpfProgram`
//! (Klasse `harw_dod_cap::CapabilityClass::Bpf`) — dieselbe Fähigkeit, die
//! `harw_dod_bpf::REQUIRED_CAPABILITY`, `harw_dod_procmon`s eigene
//! Erwartung und `harw_dod_flow::REQUIRED_CAPABILITY` benennen. Auch
//! [`UnavailableSensor`] trägt sie, damit eine Degradierung dem richtigen
//! Sensor zugeordnet wird. Diese Datei erfindet keine zweite Fähigkeit.
//!
//! # Woher die Programmobjekte kommen
//! `make build-bpf` (bzw. `scripts/build-bpf.sh` → `dod/bpf/Makefile`)
//! übersetzt `dod/bpf/src/*.bpf.c` zu genau vier ELF-Objekten —
//! `exec.bpf.o`, `exit.bpf.o`, `tcp_v4_connect.bpf.o`,
//! `tcp_v6_connect.bpf.o` — und `scripts/install.sh` legt sie unter
//! `$(BPFDIR)` ab (Vorgabe `/usr/local/lib/harw-dod/bpf`). Diese Datei löst
//! je Objekt genau einen Pfad auf ([`resolve_procmon_objects`],
//! [`resolve_flow_objects`]), in dieser Rangfolge:
//!
//! 1. der explizite Kommandozeilenpfad (`--exec-program-path` usw., so wie
//!    `harw-dod-bpf.service` ihn setzt);
//! 2. `$HARW_DOD_BPF_DIR/<name>.bpf.o` ([`BPF_OBJECT_DIR_ENV`]) — für einen
//!    Entwicklungsaufbau, der direkt gegen `BPF_ARTIFACT_DIR` (`dod/bpf`)
//!    läuft;
//! 3. [`DEFAULT_BPF_OBJECT_DIR`]`/<name>.bpf.o`.
//!
//! Der aufgelöste Pfad muss absolut sein, darf kein Symlink sein, muss eine
//! reguläre Datei sein, zwischen 1 Byte und [`MAX_BPF_OBJECT_BYTES`] groß
//! sein und mit der ELF-Kennung beginnen. Die Bytes werden **einmal**, bei
//! der Auflösung, gelesen und als
//! `harw_dod_bpf::BpfProgramSource::Embedded` weitergereicht: Größen- und
//! Formprüfung gelten damit für genau die Bytes, die später geladen werden
//! (kein zweites Lesen, das zwischen Prüfung und Laden eine andere Datei
//! sehen könnte), und nach der Auflösung braucht dieser Prozess keinen
//! Lesezugriff auf das Objektverzeichnis mehr — es ist deshalb nicht Teil
//! des Landlock-Ausschnitts (siehe [`crate::landlock`]).
//!
//! # Degradierung statt leerem Programm
//! Fehlt eines der beiden Objekte eines Sensors (oder verletzt es eine der
//! obigen Prüfungen), wird **kein** leerer Platzhalterrumpf geladen. Der
//! Sensor wird stattdessen als [`UnavailableSensor`] registriert: er loggt
//! beim Aufbau jedes fehlende Objekt mit Pfad und Grund, meldet beim ersten
//! `poll` genau ein `EventKind::SensorDegraded { sensor }` an den Sentinel
//! und liefert danach leere Lesungen (mit [`UNAVAILABLE_IDLE_INTERVAL`]
//! Wartezeit, damit die Sammelschleife nicht leer dreht). Dasselbe gilt,
//! wenn das Laden selbst degradierend scheitert (dann mit leerer
//! Mängelliste, siehe `main::setup_sensor`).
//!
//! # Exportierte Typen
//! [`UNAVAILABLE_IDLE_INTERVAL`], [`BPF_OBJECT_DIR_ENV`],
//! [`DEFAULT_BPF_OBJECT_DIR`], [`MAX_BPF_OBJECT_BYTES`], [`BpfObjectKind`],
//! [`ResolvedBpfObject`], [`ObjectUnavailable`], [`ObjectUnavailableReason`],
//! [`resolve_procmon_objects`], [`resolve_flow_objects`],
//! [`UnavailableSensor`].
//!
//! # Nebenläufigkeit
//! [`UnavailableSensor`] hält nur Griff, Fehlerliste und ein `AtomicBool`
//! (`Send + Sync + Debug`, wie `harw_dod_signals::Sensor` verlangt); `poll`
//! nimmt `&self`, konkurrierende Polls melden die Degradierung trotzdem
//! genau einmal. Die Objektauflösung liest das Dateisystem und die Umgebung
//! einmalig beim Aufbau, nie während eines `poll`.
//!
//! # Fehler
//! [`ObjectUnavailable`] (je fehlendem oder ungültigem Objekt) aus
//! [`resolve_procmon_objects`]/[`resolve_flow_objects`] — kein
//! `crate::error::ProbeError`, weil ein fehlendes Objekt den Sensor
//! degradiert, nicht den Prozess beendet. [`UnavailableSensor::poll`]
//! scheitert nie.
//!
//! # Examples
//! ```rust,ignore
//! use crate::sensors::{resolve_procmon_objects, UnavailableSensor};
//! use harw_types::SensorId;
//!
//! let id = SensorId::from_str("probe-bpf-procmon-0");
//! match resolve_procmon_objects(None, None) {
//!     Ok([exec, exit]) => {
//!         let contracts = harw_dod_procmon::procmon_contracts(
//!             id, exec.into_source(), exit.into_source(), harw_dod_bpf::BpfScope::Host,
//!         );
//!         // → crate::real_loader::load_sensor(&loader, &contracts)
//!     }
//!     Err(missing) => {
//!         let _degraded = UnavailableSensor::new(id, missing);
//!     }
//! }
//! ```

use std::borrow::Cow;
use std::ffi::OsStr;
use std::fmt;
use std::io::{ErrorKind, Read as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use harw_dod_bpf::BpfProgramSource;
use harw_dod_cap::{Bound, Capability, ReadScope, SensorError, SensorHandle};
use harw_dod_signals::{EventKind, SecurityEvent, Sensor, SensorReading};
use harw_types::SensorId;
use jiff::Timestamp;

/// Wartezeit eines [`UnavailableSensor`] je leerem `poll`: **200
/// Millisekunden**.
///
/// # Description
/// Derselbe Zahlenwert wie `harw_dod_procmon::DEFAULT_READ_TIMEOUT` und
/// `crate::collect::WIRE_ROUND_BUDGET`: ein degradierter Sensor soll die
/// Sammelschleife genauso takten wie ein realer Sensor, dessen Lesevorgang
/// ohne Ereignis abläuft — ohne diese Wartezeit liefe die Schleife auf einem
/// Host, auf dem alle Objekte fehlen, im Leerlauf mit voller CPU-Last.
pub const UNAVAILABLE_IDLE_INTERVAL: Duration = Duration::from_millis(200);

/// Baut einen leeren, ungebundenen [`harw_dod_cap::ReadScope`].
///
/// # Description
/// [`UnavailableSensor`] liest nie über `handle.scope()`.
/// `harw_dod_cap::SensorHandle::bind` verlangt trotzdem einen `ReadScope`;
/// ein leerer ist dieselbe Wahl, die `harw_dod_procmon`s eigene Beispiele
/// und Tests treffen.
fn empty_scope() -> ReadScope {
    ReadScope::from_roots(Vec::<std::path::PathBuf>::new())
}

/// Umgebungsvariable, die das Objektverzeichnis für Entwicklungsaufbauten
/// überschreibt: `HARW_DOD_BPF_DIR`.
///
/// # Description
/// Nur Rang 2 der Auflösung (siehe Moduldoku, Abschnitt „Woher die
/// Programmobjekte kommen"): ein expliziter Kommandozeilenpfad gewinnt
/// immer. Ein leerer Wert gilt als nicht gesetzt. Typische Verwendung:
/// `HARW_DOD_BPF_DIR=$PWD/bpf` nach `make build-bpf` (dessen Vorgabe für
/// `BPF_ARTIFACT_DIR` genau `dod/bpf` ist).
pub const BPF_OBJECT_DIR_ENV: &str = "HARW_DOD_BPF_DIR";

/// Vorgabe-Objektverzeichnis: `/usr/local/lib/harw-dod/bpf`.
///
/// # Description
/// Entspricht `BPFDIR = $(LIBDIR)/bpf` mit `LIBDIR = $(PREFIX)/lib/harw-dod`
/// und `PREFIX = /usr/local` aus `dod/Makefile` — genau der Ort, an den
/// `scripts/install.sh` die vier Objekte installiert. Ein Paketbau mit
/// anderem `PREFIX` setzt die Pfade ohnehin explizit über die
/// systemd-Unit (`@BPFDIR@`).
pub const DEFAULT_BPF_OBJECT_DIR: &str = "/usr/local/lib/harw-dod/bpf";

/// Größenobergrenze eines einzelnen Objekts: **8 MiB**.
///
/// # Description
/// Die v1-Objekte sind wenige Kilobyte groß; die Grenze liegt weit darüber
/// und verhindert nur, dass ein falsch konfigurierter Pfad (z. B. ein
/// Abbild oder eine Gerätedatei hinter einem Umweg) unbegrenzt in den
/// Speicher dieses privilegierten Prozesses gelesen wird.
pub const MAX_BPF_OBJECT_BYTES: u64 = 8 * 1024 * 1024;

/// Die ersten vier Bytes jeder ELF-Datei.
const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];

/// Eines der vier Objekte, die `make build-bpf` erzeugt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BpfObjectKind {
    /// `exec.bpf.o` — `sched:sched_process_exec`.
    Exec,
    /// `exit.bpf.o` — `sched:sched_process_exit`.
    Exit,
    /// `tcp_v4_connect.bpf.o` — `fentry/tcp_v4_connect`.
    TcpV4Connect,
    /// `tcp_v6_connect.bpf.o` — `fentry/tcp_v6_connect`.
    TcpV6Connect,
}

impl BpfObjectKind {
    /// Der Dateiname, unter dem `scripts/build-bpf.sh` und
    /// `scripts/install.sh` dieses Objekt ablegen.
    ///
    /// # Returns
    /// Einen der vier festen Namen, z. B. `"exec.bpf.o"`.
    #[must_use]
    pub const fn file_name(self) -> &'static str {
        match self {
            Self::Exec => "exec.bpf.o",
            Self::Exit => "exit.bpf.o",
            Self::TcpV4Connect => "tcp_v4_connect.bpf.o",
            Self::TcpV6Connect => "tcp_v6_connect.bpf.o",
        }
    }
}

impl fmt::Display for BpfObjectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.file_name())
    }
}

/// Warum ein Objekt nicht verwendbar ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectUnavailableReason {
    /// Der aufgelöste Pfad ist relativ — abhängig vom Arbeitsverzeichnis
    /// eines privilegierten Prozesses, deshalb abgelehnt.
    RelativePath,
    /// Unter dem Pfad existiert nichts (typisch: `make build-bpf`/`make
    /// install` wurde nicht ausgeführt).
    Missing,
    /// Der Pfad ist ein Symlink, ein Verzeichnis oder eine andere
    /// Nicht-Datei.
    NotRegularFile,
    /// Die Datei ist leer.
    Empty,
    /// Die Datei überschreitet [`MAX_BPF_OBJECT_BYTES`].
    TooLarge {
        /// Die beobachtete Größe in Bytes (bzw. mindestens diese).
        size: u64,
    },
    /// Die Datei beginnt nicht mit der ELF-Kennung.
    NotElf,
    /// Metadaten oder Inhalt ließen sich nicht lesen.
    Unreadable(ErrorKind),
}

impl fmt::Display for ObjectUnavailableReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RelativePath => f.write_str("path is not absolute"),
            Self::Missing => f.write_str("file does not exist (run `make build-bpf` and install)"),
            Self::NotRegularFile => f.write_str("not a regular file (symlinks are rejected)"),
            Self::Empty => f.write_str("file is empty"),
            Self::TooLarge { size } => write!(
                f,
                "file has {size} bytes, more than the limit of {MAX_BPF_OBJECT_BYTES}"
            ),
            Self::NotElf => f.write_str("file is not an ELF object"),
            Self::Unreadable(kind) => write!(f, "file is unreadable: {kind}"),
        }
    }
}

/// Ein nicht verwendbares Objekt: welches, wo gesucht, und warum.
///
/// # Description
/// Trägt den Pfad, weil er vom Betreiber bzw. vom Paket stammt (kein
/// Hostgeheimnis) und ohne ihn eine Degradierungsmeldung nicht behebbar
/// wäre. Nie Dateiinhalt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectUnavailable {
    /// Das betroffene Objekt.
    pub object: BpfObjectKind,
    /// Der aufgelöste Pfad, unter dem gesucht wurde.
    pub path: PathBuf,
    /// Der Grund.
    pub reason: ObjectUnavailableReason,
}

impl fmt::Display for ObjectUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "bpf object {} at '{}' is unavailable: {}",
            self.object,
            self.path.display(),
            self.reason
        )
    }
}

impl std::error::Error for ObjectUnavailable {}

/// Ein erfolgreich aufgelöstes und gelesenes Objekt.
///
/// # Description
/// `source` ist immer `harw_dod_bpf::BpfProgramSource::Embedded` mit den
/// bereits geprüften Bytes — siehe Moduldoku, warum nicht `Path`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedBpfObject {
    /// Welches Objekt.
    pub object: BpfObjectKind,
    /// Woher es gelesen wurde (nur für Protokoll und Audit).
    pub path: PathBuf,
    /// Die geprüften Objektbytes.
    pub source: BpfProgramSource,
}

impl ResolvedBpfObject {
    /// Gibt die Programmquelle für `harw_dod_bpf::BpfObjectContract`/
    /// `BpfProgramSpec` heraus.
    #[must_use]
    pub fn into_source(self) -> BpfProgramSource {
        self.source
    }
}

/// Bestimmt den Pfad eines Objekts nach der Rangfolge der Moduldoku.
///
/// # Arguments
/// - `object` (`BpfObjectKind`): das gesuchte Objekt.
/// - `explicit` (`Option<&Path>`): der Kommandozeilenpfad, falls gesetzt.
/// - `env_dir` (`Option<&OsStr>`): der Wert von [`BPF_OBJECT_DIR_ENV`],
///   injiziert, damit Tests die Prozessumgebung nicht verändern müssen.
///
/// # Returns
/// Den zu prüfenden Pfad; noch keine Dateisystemprüfung.
fn bpf_object_path(
    object: BpfObjectKind,
    explicit: Option<&Path>,
    env_dir: Option<&OsStr>,
) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_path_buf();
    }
    match env_dir {
        Some(dir) if !dir.is_empty() => Path::new(dir).join(object.file_name()),
        _ => Path::new(DEFAULT_BPF_OBJECT_DIR).join(object.file_name()),
    }
}

/// Prüft eine beobachtete Größe gegen die Grenzen `1..=MAX_BPF_OBJECT_BYTES`.
fn size_violation(size: u64) -> Option<ObjectUnavailableReason> {
    if size == 0 {
        Some(ObjectUnavailableReason::Empty)
    } else if size > MAX_BPF_OBJECT_BYTES {
        Some(ObjectUnavailableReason::TooLarge { size })
    } else {
        None
    }
}

/// Bildet einen I/O-Fehler auf einen Grund ab.
fn io_reason(err: &std::io::Error) -> ObjectUnavailableReason {
    if err.kind() == ErrorKind::NotFound {
        ObjectUnavailableReason::Missing
    } else {
        ObjectUnavailableReason::Unreadable(err.kind())
    }
}

/// Prüft und liest genau ein Objekt.
///
/// # Description
/// Reihenfolge: absolut? → `symlink_metadata` (kein Symlink, reguläre
/// Datei, Größe) → öffnen → Metadaten des **geöffneten** Deskriptors erneut
/// prüfen → höchstens `MAX_BPF_OBJECT_BYTES + 1` Bytes lesen → Größe der
/// tatsächlich gelesenen Bytes und ELF-Kennung prüfen. Die erste Prüfung vor
/// dem Öffnen verhindert, dass ein FIFO oder Gerät geöffnet wird; die
/// begrenzte Lesung macht die Grenze unabhängig von einer zwischenzeitlich
/// gewachsenen Datei.
///
/// # Errors
/// [`ObjectUnavailable`] mit dem ersten verletzten Grund.
fn resolve_bpf_object(
    object: BpfObjectKind,
    path: PathBuf,
) -> Result<ResolvedBpfObject, ObjectUnavailable> {
    let unavailable = |reason: ObjectUnavailableReason| ObjectUnavailable {
        object,
        path: path.clone(),
        reason,
    };

    if !path.is_absolute() {
        return Err(unavailable(ObjectUnavailableReason::RelativePath));
    }
    let link_meta = std::fs::symlink_metadata(&path).map_err(|err| unavailable(io_reason(&err)))?;
    if !link_meta.file_type().is_file() {
        return Err(unavailable(ObjectUnavailableReason::NotRegularFile));
    }
    if let Some(reason) = size_violation(link_meta.len()) {
        return Err(unavailable(reason));
    }

    let file = std::fs::File::open(&path).map_err(|err| unavailable(io_reason(&err)))?;
    let meta = file
        .metadata()
        .map_err(|err| unavailable(io_reason(&err)))?;
    if !meta.is_file() {
        return Err(unavailable(ObjectUnavailableReason::NotRegularFile));
    }
    if let Some(reason) = size_violation(meta.len()) {
        return Err(unavailable(reason));
    }

    // Blockweises, begrenztes Einlesen (höchstens ein Byte über der
    // Obergrenze, damit `size_violation` Überlänge erkennt). Bewusst kein
    // Einlesen „bis zum Streamende“ über `std::io::Read`: der
    // `push_only_guard` verbietet diesen Bezeichner in der ganzen Sonde.
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    let mut limited = file.take(MAX_BPF_OBJECT_BYTES.saturating_add(1));
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        match limited.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(chunk.get(..n).unwrap_or_default()),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(unavailable(io_reason(&err))),
        }
    }
    if let Some(reason) = size_violation(u64::try_from(bytes.len()).unwrap_or(u64::MAX)) {
        return Err(unavailable(reason));
    }
    if !bytes.starts_with(&ELF_MAGIC) {
        return Err(unavailable(ObjectUnavailableReason::NotElf));
    }

    Ok(ResolvedBpfObject {
        object,
        path,
        source: BpfProgramSource::Embedded(Cow::Owned(bytes)),
    })
}

/// Löst die beiden Objekte eines Sensors auf — beide oder keins.
///
/// # Description
/// Ein Sensor mit nur einem seiner zwei Objekte (z. B. Verbindungen nur
/// über IPv4) wäre eine stille Teilabdeckung; deshalb liefert diese
/// Funktion nur dann Objekte, wenn **beide** verwendbar sind, und sonst
/// **jeden** gefundenen Mangel, nicht nur den ersten.
fn resolve_object_pair(
    objects: [(BpfObjectKind, Option<&Path>); 2],
    env_dir: Option<&OsStr>,
) -> Result<[ResolvedBpfObject; 2], Vec<ObjectUnavailable>> {
    let [(first_kind, first_explicit), (second_kind, second_explicit)] = objects;
    let first = resolve_bpf_object(
        first_kind,
        bpf_object_path(first_kind, first_explicit, env_dir),
    );
    let second = resolve_bpf_object(
        second_kind,
        bpf_object_path(second_kind, second_explicit, env_dir),
    );
    match (first, second) {
        (Ok(first), Ok(second)) => {
            for resolved in [&first, &second] {
                tracing::info!(
                    object = %resolved.object,
                    path = %resolved.path.display(),
                    "bpf object resolved"
                );
            }
            Ok([first, second])
        }
        (first, second) => Err(first.err().into_iter().chain(second.err()).collect()),
    }
}

/// Löst die Objekte des Prozess-Sensors auf: `[exec, exit]`.
///
/// # Arguments
/// - `exec` (`Option<&Path>`): `--exec-program-path`, falls gesetzt.
/// - `exit` (`Option<&Path>`): `--exit-program-path`, falls gesetzt.
///
/// # Returns
/// `[exec, exit]` in dieser Reihenfolge — passend zu
/// `harw_dod_procmon::procmon_contracts(sensor, exec_source, exit_source, scope)`.
///
/// # Errors
/// Jeder Mangel beider Objekte als [`ObjectUnavailable`]; der Aufrufer
/// registriert dann [`UnavailableSensor`].
pub fn resolve_procmon_objects(
    exec: Option<&Path>,
    exit: Option<&Path>,
) -> Result<[ResolvedBpfObject; 2], Vec<ObjectUnavailable>> {
    let env_dir = std::env::var_os(BPF_OBJECT_DIR_ENV);
    resolve_object_pair(
        [(BpfObjectKind::Exec, exec), (BpfObjectKind::Exit, exit)],
        env_dir.as_deref(),
    )
}

/// Löst die Objekte des Verbindungs-Sensors auf: `[tcp_v4, tcp_v6]`.
///
/// # Arguments
/// - `tcp_v4` (`Option<&Path>`): `--tcp-v4-program-path`, falls gesetzt.
/// - `tcp_v6` (`Option<&Path>`): `--tcp-v6-program-path`, falls gesetzt.
///
/// # Returns
/// `[tcp_v4, tcp_v6]` in dieser Reihenfolge — passend zu
/// `harw_dod_flow::flow_contracts(sensor, ipv4_source, ipv6_source, scope)`.
///
/// # Errors
/// Siehe [`resolve_procmon_objects`].
pub fn resolve_flow_objects(
    tcp_v4: Option<&Path>,
    tcp_v6: Option<&Path>,
) -> Result<[ResolvedBpfObject; 2], Vec<ObjectUnavailable>> {
    let env_dir = std::env::var_os(BPF_OBJECT_DIR_ENV);
    resolve_object_pair(
        [
            (BpfObjectKind::TcpV4Connect, tcp_v4),
            (BpfObjectKind::TcpV6Connect, tcp_v6),
        ],
        env_dir.as_deref(),
    )
}

/// Ein Sensor, dessen Programmobjekte fehlen: meldet sich als nicht
/// verfügbar, statt ein leeres Programm zu laden.
///
/// # Description
/// Trägt dieselbe Kennung und dieselbe Fähigkeit
/// (`Capability::LoadBpfProgram`) wie der Sensor, den er vertritt, damit
/// die Degradierung beim Sentinel dem richtigen Sensor zugeordnet wird.
/// Siehe Moduldoku, Abschnitt „Degradierung statt leerem Programm".
///
/// Der **erste** `poll` liefert genau ein
/// `EventKind::SensorDegraded { sensor }` — so erreicht die Degradierung
/// den Sentinel über den generischen `Sensor`-Zweig der Sammelschleife
/// ([`crate::collect::run_once`], aufgerufen von `main::collect_forever`),
/// ohne dass diese einen dauerhaften Sensorfehler als Prozessende deuten
/// muss. Jeder weitere `poll` wartet [`UNAVAILABLE_IDLE_INTERVAL`] und
/// liefert eine leere Lesung — genau wie ein realer Sensor, dessen
/// Lesevorgang ohne Ereignis abläuft.
#[derive(Debug)]
pub struct UnavailableSensor {
    handle: SensorHandle<Bound>,
    missing: Vec<ObjectUnavailable>,
    reported: AtomicBool,
    idle: Duration,
}

impl UnavailableSensor {
    /// Baut den Platzhalter-Sensor und protokolliert jeden Mangel einmal
    /// als Warnung.
    ///
    /// # Arguments
    /// - `sensor_id` (`harw_types::SensorId`): die Kennung des vertretenen
    ///   Sensors.
    /// - `missing` (`Vec<ObjectUnavailable>`): die Mängel aus
    ///   [`resolve_procmon_objects`]/[`resolve_flow_objects`].
    ///
    /// # Returns
    /// Einen Sensor, der seine Degradierung beim ersten `poll` meldet.
    #[must_use]
    pub fn new(sensor_id: SensorId, missing: Vec<ObjectUnavailable>) -> Self {
        Self::with_idle(sensor_id, missing, UNAVAILABLE_IDLE_INTERVAL)
    }

    /// Wie [`Self::new`], mit eigener Wartezeit je leerem `poll`.
    fn with_idle(sensor_id: SensorId, missing: Vec<ObjectUnavailable>, idle: Duration) -> Self {
        for item in &missing {
            tracing::warn!(
                sensor = %sensor_id,
                object = %item.object,
                path = %item.path.display(),
                reason = %item.reason,
                "bpf sensor unavailable: required object missing or invalid"
            );
        }
        let handle = SensorHandle::new(sensor_id, Capability::LoadBpfProgram).bind(empty_scope());
        Self {
            handle,
            missing,
            reported: AtomicBool::new(false),
            idle,
        }
    }

    /// Die Mängel, derentwegen dieser Sensor nicht verfügbar ist.
    #[cfg(test)]
    #[must_use]
    pub fn missing(&self) -> &[ObjectUnavailable] {
        &self.missing
    }
}

impl Sensor for UnavailableSensor {
    /// Der gebundene Griff dieses Sensors.
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    /// Meldet die Degradierung einmal, danach leere Lesungen.
    ///
    /// # Description
    /// Erster Aufruf: ein `SecurityEvent` mit
    /// `EventKind::SensorDegraded { sensor }`, `observed_at = now`, ohne
    /// Akteur. Jeder weitere Aufruf: `idle` warten, leere Lesung.
    ///
    /// # Errors
    /// Keine.
    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        if self.reported.swap(true, Ordering::AcqRel) {
            std::thread::sleep(self.idle);
            return Ok(SensorReading {
                samples: Vec::new(),
                events: Vec::new(),
            });
        }
        tracing::warn!(
            sensor = %self.handle.id(),
            missing_objects = self.missing.len(),
            "reporting bpf sensor as degraded"
        );
        Ok(SensorReading {
            samples: Vec::new(),
            events: vec![SecurityEvent {
                sensor: self.handle.id().clone(),
                observed_at: now,
                actor: None,
                kind: EventKind::SensorDegraded {
                    sensor: self.handle.id().clone(),
                },
            }],
        })
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use harw_dod_bpf::BpfProgramSource;
    use harw_dod_cap::Capability;
    use harw_dod_signals::{EventKind, Sensor};
    use harw_types::SensorId;
    use jiff::Timestamp;

    use super::{
        BpfObjectKind, DEFAULT_BPF_OBJECT_DIR, ELF_MAGIC, MAX_BPF_OBJECT_BYTES,
        ObjectUnavailableReason, UnavailableSensor, bpf_object_path, resolve_bpf_object,
        resolve_object_pair,
    };
    use crate::test_support::{TestError, TestResult, ctx};

    /// Schreibt ein minimales „Objekt" (ELF-Kennung plus Füllbytes).
    fn write_elf(dir: &Path, name: &str) -> TestResult<PathBuf> {
        let path = dir.join(name);
        let mut bytes = ELF_MAGIC.to_vec();
        bytes.extend_from_slice(b"-rest-of-object");
        std::fs::write(&path, &bytes).map_err(ctx("write fixture object"))?;
        Ok(path)
    }

    fn expect_reason(
        object: BpfObjectKind,
        path: PathBuf,
        expected: ObjectUnavailableReason,
    ) -> TestResult {
        match resolve_bpf_object(object, path) {
            Ok(_) => Err(TestError::Unexpected(format!(
                "expected {expected:?}, got a resolved object"
            ))),
            Err(err) if err.reason == expected => Ok(()),
            Err(err) => Err(TestError::Unexpected(format!(
                "expected {expected:?}, got {:?}",
                err.reason
            ))),
        }
    }

    #[test]
    fn test_object_file_names_match_build_bpf_outputs() {
        // Dieselben vier Namen wie `scripts/build-bpf.sh`/`scripts/install.sh`.
        assert_eq!(BpfObjectKind::Exec.file_name(), "exec.bpf.o");
        assert_eq!(BpfObjectKind::Exit.file_name(), "exit.bpf.o");
        assert_eq!(
            BpfObjectKind::TcpV4Connect.file_name(),
            "tcp_v4_connect.bpf.o"
        );
        assert_eq!(
            BpfObjectKind::TcpV6Connect.file_name(),
            "tcp_v6_connect.bpf.o"
        );
    }

    #[test]
    fn test_object_path_prefers_explicit_then_env_then_default() {
        let explicit = Path::new("/opt/explicit/exec.o");
        let env = OsStr::new("/srv/dod/bpf");
        assert_eq!(
            bpf_object_path(BpfObjectKind::Exec, Some(explicit), Some(env)),
            PathBuf::from("/opt/explicit/exec.o")
        );
        assert_eq!(
            bpf_object_path(BpfObjectKind::Exit, None, Some(env)),
            PathBuf::from("/srv/dod/bpf/exit.bpf.o")
        );
        assert_eq!(
            bpf_object_path(BpfObjectKind::TcpV4Connect, None, None),
            Path::new(DEFAULT_BPF_OBJECT_DIR).join("tcp_v4_connect.bpf.o")
        );
    }

    #[test]
    fn test_object_path_ignores_an_empty_env_dir() {
        assert_eq!(
            bpf_object_path(BpfObjectKind::TcpV6Connect, None, Some(OsStr::new(""))),
            Path::new(DEFAULT_BPF_OBJECT_DIR).join("tcp_v6_connect.bpf.o")
        );
    }

    #[test]
    fn test_default_object_dir_matches_the_makefile_install_prefix() {
        // `PREFIX ?= /usr/local`, `LIBDIR ?= $(PREFIX)/lib/harw-dod`,
        // `BPFDIR ?= $(LIBDIR)/bpf`.
        assert_eq!(DEFAULT_BPF_OBJECT_DIR, "/usr/local/lib/harw-dod/bpf");
    }

    #[test]
    fn test_resolve_reads_a_valid_object_into_embedded_bytes() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = write_elf(dir.path(), "exec.bpf.o")?;
        let resolved = resolve_bpf_object(BpfObjectKind::Exec, path.clone())
            .map_err(ctx("valid object resolves"))?;
        assert_eq!(resolved.object, BpfObjectKind::Exec);
        assert_eq!(resolved.path, path);
        let BpfProgramSource::Embedded(bytes) = resolved.into_source() else {
            return Err(TestError::Unexpected(
                "resolved objects are always embedded".into(),
            ));
        };
        assert!(bytes.starts_with(&ELF_MAGIC));
        assert!(!bytes.is_empty());
        Ok(())
    }

    #[test]
    fn test_resolve_missing_object_is_missing() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        expect_reason(
            BpfObjectKind::Exec,
            dir.path().join("exec.bpf.o"),
            ObjectUnavailableReason::Missing,
        )
    }

    #[test]
    fn test_resolve_relative_path_is_rejected() -> TestResult {
        expect_reason(
            BpfObjectKind::Exec,
            PathBuf::from("bpf/exec.bpf.o"),
            ObjectUnavailableReason::RelativePath,
        )
    }

    #[test]
    fn test_resolve_directory_is_not_a_regular_file() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("exec.bpf.o");
        std::fs::create_dir(&path).map_err(ctx("create directory in place of object"))?;
        expect_reason(
            BpfObjectKind::Exec,
            path,
            ObjectUnavailableReason::NotRegularFile,
        )
    }

    #[test]
    fn test_resolve_symlink_is_not_a_regular_file() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let target = write_elf(dir.path(), "real.bpf.o")?;
        let link = dir.path().join("exec.bpf.o");
        std::os::unix::fs::symlink(&target, &link).map_err(ctx("create symlink"))?;
        expect_reason(
            BpfObjectKind::Exec,
            link,
            ObjectUnavailableReason::NotRegularFile,
        )
    }

    #[test]
    fn test_resolve_empty_file_is_empty() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("exit.bpf.o");
        std::fs::write(&path, b"").map_err(ctx("write empty object"))?;
        expect_reason(BpfObjectKind::Exit, path, ObjectUnavailableReason::Empty)
    }

    #[test]
    fn test_resolve_oversized_file_is_too_large() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("exit.bpf.o");
        let file = std::fs::File::create(&path).map_err(ctx("create object"))?;
        // Sparse: belegt keinen echten Speicher.
        file.set_len(MAX_BPF_OBJECT_BYTES + 1)
            .map_err(ctx("extend object past the limit"))?;
        expect_reason(
            BpfObjectKind::Exit,
            path,
            ObjectUnavailableReason::TooLarge {
                size: MAX_BPF_OBJECT_BYTES + 1,
            },
        )
    }

    #[test]
    fn test_resolve_non_elf_file_is_rejected() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("exec.bpf.o");
        std::fs::write(&path, b"not an object").map_err(ctx("write non-ELF object"))?;
        expect_reason(BpfObjectKind::Exec, path, ObjectUnavailableReason::NotElf)
    }

    #[test]
    fn test_resolve_pair_from_env_dir_returns_both_in_order() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        write_elf(dir.path(), "tcp_v4_connect.bpf.o")?;
        write_elf(dir.path(), "tcp_v6_connect.bpf.o")?;
        let [v4, v6] = resolve_object_pair(
            [
                (BpfObjectKind::TcpV4Connect, None),
                (BpfObjectKind::TcpV6Connect, None),
            ],
            Some(dir.path().as_os_str()),
        )
        .map_err(|missing| TestError::Unexpected(format!("{missing:?}")))?;
        assert_eq!(v4.object, BpfObjectKind::TcpV4Connect);
        assert_eq!(v6.object, BpfObjectKind::TcpV6Connect);
        assert_eq!(v4.path, dir.path().join("tcp_v4_connect.bpf.o"));
        Ok(())
    }

    #[test]
    fn test_resolve_pair_explicit_path_overrides_env_dir() -> TestResult {
        let env_dir = tempfile::tempdir().map_err(ctx("env tempdir"))?;
        let other = tempfile::tempdir().map_err(ctx("explicit tempdir"))?;
        let explicit = write_elf(other.path(), "custom-exec.o")?;
        write_elf(env_dir.path(), "exit.bpf.o")?;
        let [exec, exit] = resolve_object_pair(
            [
                (BpfObjectKind::Exec, Some(explicit.as_path())),
                (BpfObjectKind::Exit, None),
            ],
            Some(env_dir.path().as_os_str()),
        )
        .map_err(|missing| TestError::Unexpected(format!("{missing:?}")))?;
        assert_eq!(exec.path, explicit);
        assert_eq!(exit.path, env_dir.path().join("exit.bpf.o"));
        Ok(())
    }

    #[test]
    fn test_resolve_pair_with_one_missing_object_reports_only_that_one() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        write_elf(dir.path(), "exec.bpf.o")?;
        let Err(missing) = resolve_object_pair(
            [(BpfObjectKind::Exec, None), (BpfObjectKind::Exit, None)],
            Some(dir.path().as_os_str()),
        ) else {
            return Err(TestError::Unexpected(
                "a sensor with one missing object must not resolve".into(),
            ));
        };
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].object, BpfObjectKind::Exit);
        assert_eq!(missing[0].reason, ObjectUnavailableReason::Missing);
        Ok(())
    }

    #[test]
    fn test_resolve_pair_reports_every_missing_object() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let Err(missing) = resolve_object_pair(
            [(BpfObjectKind::Exec, None), (BpfObjectKind::Exit, None)],
            Some(dir.path().as_os_str()),
        ) else {
            return Err(TestError::Unexpected(
                "an empty directory must not resolve".into(),
            ));
        };
        assert_eq!(missing.len(), 2);
        let text = missing[1].to_string();
        assert!(text.contains("exit.bpf.o"));
        assert!(text.contains("make build-bpf"));
        Ok(())
    }

    #[test]
    fn test_unavailable_sensor_reports_degradation_once_under_its_own_id() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let Err(missing) = resolve_object_pair(
            [
                (BpfObjectKind::TcpV4Connect, None),
                (BpfObjectKind::TcpV6Connect, None),
            ],
            Some(dir.path().as_os_str()),
        ) else {
            return Err(TestError::Unexpected(
                "an empty directory must not resolve".into(),
            ));
        };
        let id = SensorId::from_str("probe-bpf-flow-0");
        // Keine Wartezeit, damit der zweite `poll` den Test nicht bremst.
        let sensor = UnavailableSensor::with_idle(id.clone(), missing, Duration::ZERO);
        assert_eq!(sensor.missing().len(), 2);
        assert_eq!(sensor.handle().id(), &id);
        assert_eq!(sensor.handle().capability(), Capability::LoadBpfProgram);

        let first = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("an unavailable sensor never fails to poll"))?;
        assert!(first.samples.is_empty());
        assert_eq!(first.events.len(), 1);
        let event = first
            .events
            .first()
            .ok_or(TestError::Missing("the degradation event"))?;
        assert_eq!(event.sensor, id);
        assert_eq!(event.observed_at, Timestamp::UNIX_EPOCH);
        assert!(event.actor.is_none());
        assert!(matches!(
            &event.kind,
            EventKind::SensorDegraded { sensor } if sensor == &id
        ));

        let second = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("an unavailable sensor never fails to poll"))?;
        assert!(second.events.is_empty());
        assert!(second.samples.is_empty());
        Ok(())
    }

    #[test]
    fn test_unavailable_sensor_without_missing_objects_still_reports_degradation() -> TestResult {
        // Degradierender Ladefehler: `main::setup_sensor` übergibt eine leere
        // Mängelliste — die Meldung an den Sentinel erfolgt trotzdem.
        let id = SensorId::from_str("probe-bpf-procmon-0");
        let sensor = UnavailableSensor::with_idle(id.clone(), Vec::new(), Duration::ZERO);
        assert!(sensor.missing().is_empty());
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("an unavailable sensor never fails to poll"))?;
        assert_eq!(reading.events.len(), 1);
        Ok(())
    }
}
