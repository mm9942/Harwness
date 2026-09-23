//! Reale, `aya`-gestützte eBPF-Ladeschicht.
//!
//! # V1 production contract
//! The historical research notes below predate the standalone C object
//! domain and are not the active contract.  Production uses only
//! [`BpfObjectContract`]: one exact program, the four named v1 maps, a
//! populated scope map before attach, and either a sched tracepoint or the
//! target-BTF `FEntry` TCP-connect hook.  Profileless [`BpfLoader::load`]
//! intentionally refuses real attachment; [`RealBpfLoader::load_contracts`]
//! rolls back all links on a partial failure.
//!
//! # Recherche, mit Quellen (Stand dieses Berichts)
//! Vor dieser Datei stand die Entscheidung von `harw-dod-bpf`s eigener
//! `crate`-Moduldoku (Abschnitt „Abweichung vom Auftrag: kein Ladeteil in
//! dieser Lieferung“), **keine** `aya`-Abhängigkeit zu ziehen — mit zwei
//! Begründungen: (1) `SocketFilter::attach` bräuchte einen bereits geöffneten
//! Socket, den eine generische Ladeschicht nicht besitzen darf, und (2) ein
//! neuer, in dieser Sitzung nicht per `cargo` verifizierbarer
//! Abhängigkeitszuwachs sei ausgerechnet im späteren `CAP_BPF`-Teil ein zu
//! großes Risiko. Der Konsument hat (1) inzwischen aufgelöst:
//! `harw-dod-flow` hängt an `sock:inet_sock_set_state` (ein Tracepoint),
//! `harw-dod-procmon` an `sched:sched_process_exec` (ebenfalls ein
//! Tracepoint) — **kein** Sensor dieser beiden Crates braucht mehr
//! `SocketFilter`. Für (2) gilt weiterhin: `cargo` durfte in dieser Aufgabe
//! nicht laufen; die folgende Recherche ersetzt einen `cargo add`/
//! `cargo check`-Lauf durch geprüfte Registry- und Quelltextfakten,
//! dokumentiert hier so vollständig, dass die zentrale, sequenzielle
//! Verifikation dieses Workspace das Ergebnis nachvollziehen und bei einem
//! Fehler genau hier ansetzen kann.
//!
//! - **Fassung:** `aya` **0.14.0**, laut `crates.io`-API
//!   (`GET /api/v1/crates/aya/versions`) der neueste, nicht zurückgezogene
//!   Eintrag zum Berichtszeitpunkt (veröffentlicht 2026-06-24). Nicht
//!   geraten — `~/.cargo/registry/src` enthielt keine gepinnte `aya`-Quelle
//!   in diesem Workspace, die Fassung stammt ausschließlich aus der
//!   Registry-API.
//! - **`unsafe`-Fläche der hier benutzten Teilmenge:** `Ebpf::load`/
//!   `load_file`, `TracePoint::load`/`attach`, `KProbe::load`/`attach`, die
//!   `TryFrom<&mut Program>`-Umwandlungen (Makro `impl_try_from_program!`)
//!   und `RingBuf::next` sind laut gepinnter Quelltextlesung
//!   (`raw.githubusercontent.com/aya-rs/aya/aya-v0.14.0/...`) **sämtlich
//!   sichere Funktionen** — kein `unsafe fn` unter den hier aufgerufenen
//!   Signaturen. `aya/src/lib.rs` selbst trägt kein
//!   `#![forbid(unsafe_code)]`; die Crate benutzt `unsafe` intern (z. B.
//!   `FromRawFd::from_raw_fd`), aber das bleibt hinter sicheren Funktionen
//!   verborgen, die dieses Modul aufruft — `unsafe_code = "forbid"` dieses
//!   Workspace betrifft nur `harw-dod-bpf`s **eigenen** Quelltext, und der
//!   bleibt in dieser Datei vollständig `unsafe`-frei.
//! - **`build.rs`/Systemabhängigkeit:** **keine.** `aya/Cargo.toml`
//!   (gepinnte Fassung 0.14.0, Quelle wie oben) deklariert kein `build.rs`
//!   und keinen `[build-dependencies]`-Abschnitt. Das deckt sich mit `aya`s
//!   eigener Außendarstellung („kein `libbpf`, kein `clang`, nur `libc` für
//!   Syscalls“) — hier nicht geglaubt, sondern an der gepinnten Fassung
//!   nachgelesen. `unsafe_code = "forbid"` und Vertrag **D3** (kein
//!   `build.rs` mit Systemabhängigkeit) sind damit für **diese** Crate kein
//!   Hindernis.
//! - **Transitive Crate-Zahl:** über die `crates.io`-Abhängigkeits-API
//!   rekonstruiert (kein `cargo tree` möglich — Hauptregel dieser Aufgabe).
//!   Direkte Laufzeit-Abhängigkeiten von `aya` 0.14.0: `aya-obj`,
//!   `bitflags`, `hashbrown`, `libc`, `log`, `object`, `once_cell`,
//!   `scopeguard`, `thiserror` (neun; `anyhow`/`nix` sind laut `aya`s
//!   eigener Feature-Dokumentation nur hinter dem hier **nicht** aktivierten
//!   Feature `test-helpers`). `aya-obj` 0.3.0 ergänzt `bytes` (die übrigen
//!   drei seiner Abhängigkeiten — `log`, `object`, `thiserror` — sind bereits
//!   gezählt). `thiserror` 2.0.3 zieht `thiserror-impl`, das wiederum `syn`,
//!   `quote`, `proc-macro2` und `unicode-ident` zieht. `object` 0.39.0 zieht
//!   `memchr` als einzige nicht-optionale Abhängigkeit. **Ergebnis: 17
//!   Crates** in der vollständigen transitiven Hülle, `aya` selbst
//!   mitgezählt (`aya`, `aya-obj`, `bitflags`, `bytes`, `hashbrown`, `libc`,
//!   `log`, `memchr`, `object`, `once_cell`, `proc-macro2`, `quote`,
//!   `scopeguard`, `syn`, `thiserror`, `thiserror-impl`, `unicode-ident`).
//!   Mehrere davon (`libc`, `log`, `once_cell`, `bitflags`, `hashbrown`,
//!   `thiserror`, die `syn`/`quote`/`proc-macro2`-Kette) sind bereits an
//!   anderer Stelle im Workspace vorhanden — der tatsächliche Zuwachs in
//!   `Cargo.lock` dürfte kleiner ausfallen. Diese Zahl ist eine aus
//!   Registry-Metadaten rekonstruierte, keine per `cargo tree` gemessene —
//!   die zentrale Verifikation dieses Workspace hat die Autorität, sie zu
//!   korrigieren.
//! - **`TracePoint::attach` in der gepinnten Fassung, wörtlich gelesen
//!   (nicht docs.rs):**
//!   ```rust,ignore
//!   pub fn load(&mut self) -> Result<(), ProgramError>
//!   pub fn attach(&mut self, category: &str, name: &str) -> Result<TracePointLinkId, ProgramError>
//!   ```
//!   Zwei einfache `&str`-Parameter, kein Tupel, kein `AsFd`, keine
//!   versteckte Falle wie zuletzt bei `rustix::net::recv` — dieses Projekt
//!   hat sich zweimal an einer Ergebnisform verbrannt, die von ihrem Namen
//!   abwich; hier stimmen Name und Form überein.
//!
//! # Woher das ELF-Objekt kommt — der eigentliche Aufwand dieses Knotens
//! `aya::Ebpf::load`/`load_file` **liest** ein bereits übersetztes
//! eBPF-ELF-Objekt; diese Crate **erzeugt** keines. Ein solches Objekt
//! entsteht aus einem `#![no_std]`-Programm, das gegen `aya-ebpf` geschrieben
//! und mit der `nightly`-Toolchain für das Ziel `bpfel-unknown-none` (bzw.
//! `bpfeb-unknown-none` auf Big-Endian-Hosts) übersetzt wird — ein **zweiter,
//! eigenständiger Übersetzungslauf**, mit anderer Toolchain-Kanalwahl
//! (`nightly` statt der Workspace-`rust-toolchain`) und anderem Ziel-Tripel
//! als jeder andere Knoten dieses Workspace. Das ist kein Nebenaspekt,
//! sondern der genau hier zutreffende Befund aus dem Auftrag: „wenn er einen
//! zweiten Übersetzungslauf mit anderer Toolchain bedeutet, ist das ein
//! Befund, keine Nebensache“.
//!
//! Diese Lieferung geht deshalb bewusst **Weg 2**: die Ladeschicht ist
//! vollständig gebaut und nimmt ein **vorhandenes** Objekt entgegen — über
//! [`crate::spec::BpfProgramSource::Embedded`] (zur Bauzeit von
//! `harw-probe-bpf` eingebettet, typischerweise `include_bytes!`) oder
//! [`crate::spec::BpfProgramSource::Path`] (zur Laufzeit von einem
//! Dateisystempfad gelesen). Woher dieses Objekt selbst kommt — der
//! `aya-ebpf`/`nightly`/`bpfel-unknown-none`-Übersetzungslauf — ist
//! **nicht** Teil dieser Crate und **nicht** Teil dieses Knotens: das ist
//! ein eigener, künftiger Baustein (ein `xtask`- oder separates
//! Build-Skript außerhalb dieses Workspace-`Cargo.toml`-Baums, weil ein
//! `build.rs` mit einer `nightly`+`bpfel`-Systemabhängigkeit exakt das wäre,
//! was Vertrag D3 für **diese** Crate verbietet). Bis dieser Baustein
//! existiert, hat kein Aufrufer dieser Ladeschicht ein echtes Objekt zum
//! Laden — [`RealBpfLoader`] ist vollständig, aber ungefüttert.
//!
//! # Der Vertrag zwischen dieser Ladeschicht und dem (künftigen) Objekt
//! Damit [`RealBpfLoader`] ein Objekt ohne zusätzliche Konfiguration laden
//! kann, setzt sie zwei Konventionen voraus, die der künftige
//! `aya-ebpf`-Übersetzungslauf einhalten muss:
//!
//! 1. **Genau ein Programm pro Objekt** — [`RealBpfLoader::load`] nimmt das
//!    erste (einzige) Element von `Ebpf::programs_mut()`. Das passt zu
//!    [`crate::spec::BpfProgramSpec`], die selbst genau eine
//!    [`crate::spec::BpfProgramSource`] pro Beschreibung trägt.
//! 2. **Eine Ringpuffer-Map namens `"EVENTS"`** — [`RealBpfLoader::load`]
//!    ruft `Ebpf::take_map("EVENTS")` und deutet das Ergebnis als
//!    `aya::maps::RingBuf`. Ein Objekt ohne diese Map (oder mit ihr unter
//!    einem anderen Namen oder Maptyp) lässt [`crate::error::BpfError::ProgramLoadFailed`]
//!    entstehen.
//!
//! # Warum kein `aya`-Typ nach außen dringt
//! [`RealBpfLoader`] hält `aya::Ebpf` und `aya::maps::RingBuf<aya::maps::MapData>`
//! ausschließlich in einem privaten Feld ([`LoadedProgram`], nicht `pub`).
//! [`crate::loader::BpfLoader::load`] liefert [`crate::handle::BpfHandle`],
//! [`crate::loader::BpfLoader::read_events`] liefert
//! [`crate::event::RawBpfEvent`] — beides eigene Typen dieser Crate, wie im
//! `crate`-Moduldoku begründet. Jeder `aya`-Fehler wird an der Stelle, an der
//! er entsteht, in [`crate::error::BpfError::ProgramLoadFailed`] übersetzt
//! (inhaltsfrei, kein `aya`-Typ als Feld) statt durchgereicht.
//!
//! # Was der reale Ladeteil nicht tut
//! [`BpfProgramKind::SocketFilter`] bleibt unterstützt in
//! [`crate::spec::BpfProgramSpec`] (geschlossenes Enum, nicht Teil dieses
//! Schreibbereichs), aber [`RealBpfLoader::load`] liefert dafür
//! [`crate::error::BpfError::UnsupportedProgramKind`] — das ursprüngliche
//! Hindernis (`SocketFilter::attach` verlangt `T: AsFd`, also einen bereits
//! offenen, vom Aufrufer besessenen Socket) besteht für diese Programmart
//! unverändert fort, unabhängig davon, dass kein heutiger Konsument sie noch
//! braucht. **Bedingung, unter der das nachgeliefert werden könnte:**
//! [`crate::loader::BpfLoader::load`] (oder eine neue Trait-Methode) müsste
//! einen bereits geöffneten Socket-Deskriptor entgegennehmen können — eine
//! Erweiterung der Trait-Signatur, die dieser Knoten nicht vornimmt, weil sie
//! die beiden bestehenden Sensor-Crates (außerhalb dieses Schreibbereichs)
//! zwingen würde, sich zu ändern, ohne dass ein Konsument das heute braucht.
//!
//! # Die Privilegienklasse
//! [`RealBpfLoader::load`] prüft `CAP_BPF` selbst, **vor** jedem
//! `aya`-Aufruf: [`has_cap_bpf`] liest `/proc/self/status`, sucht die Zeile
//! `CapEff:` und prüft Bit 39 (`CAP_BPF`, Linux ≥ 5.8) in der dort
//! hexadezimal kodierten Fähigkeitsmaske — reines `std`, kein `unsafe`, keine
//! neue Abhängigkeit. Fehlt die Fähigkeit, liefert `load` sofort
//! [`crate::error::BpfError::CapabilityUnavailable`], **bevor** `aya`
//! überhaupt einen `bpf()`-Syscall versucht — derselbe dokumentierte,
//! lauffähige Betriebsfall, den [`crate::fixture::FixtureBpfLoader::without_capability`]
//! bereits simuliert. `CAP_BPF` bleibt damit die **einzige** erhöhte
//! Fähigkeit, die dieser Ladeteil voraussetzt — [`RealBpfLoader`] öffnet
//! selbst keinen Socket, kein Netlink, kein `fanotify`.
//!
//! # Exportierte Typen
//! [`RealBpfLoader`].
//!
//! # Nebenläufigkeit
//! [`RealBpfLoader`] ist `Send + Sync`, wie es [`crate::loader::BpfLoader`]
//! verlangt: die Registrierung geladener Programme steckt hinter einem
//! `std::sync::Mutex` (Muster: [`crate::fixture::FixtureBpfLoader`], inklusive
//! `PoisonError::into_inner` statt eines `panic`s auf einem vergifteten
//! Mutex). Dass `aya::Ebpf` und `aya::maps::RingBuf<aya::maps::MapData>`
//! selbst `Send` sind, ist nicht an dieser Datei nachgewiesen (die gepinnte
//! Quelle zeigt es nicht direkt), sondern aus `aya`s eigener, dokumentierter
//! Nutzung heraus angenommen: die Bibliothek bewirbt sowohl `tokio`- als auch
//! `async-std`-Unterstützung, die beide Objekte routinemäßig über
//! Thread-Grenzen bewegen — eine Bibliothek, deren zentraler Werttyp nicht
//! `Send` wäre, könnte das nicht anbieten. Sollte die zentrale Verifikation
//! dieses Workspace das widerlegen, ist das der erste Ort, an dem
//! nachzusehen ist.
//!
//! # Fehler
//! [`crate::error::BpfError::CapabilityUnavailable`],
//! [`crate::error::BpfError::ProgramLoadFailed`],
//! [`crate::error::BpfError::UnsupportedProgramKind`],
//! [`crate::error::BpfError::UnknownHandle`],
//! [`crate::error::BpfError::MalformedEvent`] (aus [`crate::event::parse_raw_event`]
//! während [`RealBpfLoader::read_events`]).
//!
//! # Examples
//! ```rust
//! use harw_dod_bpf::{BpfLoader, real::RealBpfLoader};
//!
//! // `new()` baut nur die (leere) interne Registrierung — kein Kernel-,
//! // kein Berechtigungszugriff, deshalb ohne Gefahr in einem Doctest
//! // lauffähig (siehe Aufgabenregel „Lade in keinem Test ein echtes
//! // eBPF-Programm").
//! let loader: Box<dyn BpfLoader> = Box::new(RealBpfLoader::new());
//! drop(loader);
//! ```
//
use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use aya::maps::{Array as AyaArray, HashMap as AyaHashMap, Map, MapData, PerCpuArray, RingBuf};
use aya::programs::{FEntry, KProbe, TracePoint};
use aya::{Btf, Ebpf};

use crate::abi::{WireEvent, parse_wire_event};
use crate::contract::{
    BpfObjectContract, EVENTS_MAP_NAME, LOSS_COUNTS_MAP_NAME, REQUIRED_MAP_NAMES, SCOPE_MAP_NAME,
    SEQUENCE_MAP_NAME,
};
use crate::error::BpfError;
use crate::event::{RawBpfEvent, parse_raw_event};
use crate::handle::BpfHandle;
use crate::loader::BpfLoader;
use crate::profile::BpfScope;
use crate::spec::{BpfProgramKind, BpfProgramSpec};
use crate::time::{KernelTimeMapper, TimeConfidence};

/// Name der Ringpuffer-Map, die ein geladenes Objekt tragen muss.
///
/// Siehe Moduldoku, Abschnitt „Der Vertrag zwischen dieser Ladeschicht und
/// dem (künftigen) Objekt“.
/// Bitposition von `CAP_BPF` in der von `/proc/self/status` gemeldeten
/// Fähigkeitsmaske (Linux ≥ 5.8, `include/uapi/linux/capability.h`).
const CAP_BPF_BIT: u32 = 39;
/// `CAP_PERFMON` is needed by the tracing attach path on current kernels.
const CAP_PERFMON_BIT: u32 = 38;

/// Wartezeit zwischen zwei Prüfungen der Ringpuffer-Map in
/// [`RealBpfLoader::read_events`], solange der aufrufer-seitige `timeout`
/// noch nicht verstrichen ist.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Der pro Griff gehaltene, private `aya`-Zustand.
///
/// Trägt `Ebpf` weiter, damit das geladene und angeheftete Programm am Leben
/// bleibt (`aya::Ebpf::drop` löst das Programm — ein fallengelassenes
/// `Ebpf` würde die Anheftung sofort wieder entfernen). Kein Feld dieses
/// Typs verlässt [`RealBpfLoader`] — siehe Moduldoku, Abschnitt „Warum kein
/// `aya`-Typ nach außen dringt“.
struct LoadedProgram {
    /// Gehalten für ihre Nebenwirkung (Programm bleibt geladen/angeheftet),
    /// nie gelesen — daher der führende Unterstrich.
    _ebpf: Ebpf,
    ring_buf: RingBuf<MapData>,
    loss_counts: PerCpuArray<MapData, u64>,
}

/// Die reale, `aya`-gestützte Implementierung von [`crate::loader::BpfLoader`].
///
/// # Description
/// Siehe Moduldoku für Recherche, Herkunft des ELF-Objekts, den Vertrag
/// zwischen Ladeschicht und Objekt sowie die Privilegienklasse.
pub struct RealBpfLoader {
    loaded: Mutex<HashMap<u64, LoadedProgram>>,
    invalid_wire_events: AtomicU64,
}

/// A wire event accompanied by the loader's measured conversion from kernel
/// monotonic time to realtime.  The raw `ktime_ns` remains available in the
/// inner event for audit and re-mapping after a time discontinuity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimedWireEvent {
    pub event: WireEvent,
    pub observed_at: jiff::Timestamp,
    pub time_confidence: TimeConfidence,
}

/// Counters exposed by every v1 object.  `ringbuf_reserve` failures are
/// counted in-kernel per event kind; a caller compares them with the global
/// event sequence to distinguish a quiet source from a lossy one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BpfLossCounters {
    pub exec: u64,
    pub process_exit: u64,
    pub tcp_connect: u64,
    /// Records rejected by the host ABI parser, including unknown versions,
    /// types, lengths, and impossible timestamp mappings.
    pub invalid_wire_events: u64,
}

impl RealBpfLoader {
    /// Baut eine leere, ungefütterte Ladeschicht.
    ///
    /// # Description
    /// Führt keinen Kernel- oder Berechtigungszugriff aus — nur die interne
    /// Registrierung wird angelegt. Sicher in jedem Kontext aufrufbar,
    /// insbesondere in Tests (siehe Aufgabenregel „Lade in keinem Test ein
    /// echtes eBPF-Programm“).
    ///
    /// # Returns
    /// Eine `RealBpfLoader` ohne geladene Programme.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_bpf::real::RealBpfLoader;
    ///
    /// let _loader = RealBpfLoader::new();
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self {
            loaded: Mutex::new(HashMap::new()),
            invalid_wire_events: AtomicU64::new(0),
        }
    }

    /// Load one selected ELF program after filling the scope map.  This is
    /// intentionally separate from the legacy `BpfLoader::load` method: the
    /// latter has no profile argument and therefore cannot safely attach a
    /// production object.
    pub fn load_contract(&self, contract: &BpfObjectContract) -> Result<BpfHandle, BpfError> {
        contract.validate()?;
        if !has_attach_capabilities() {
            return Err(BpfError::AttachCapabilitiesUnavailable);
        }

        let bytes = contract.source.resolve()?;
        let mut ebpf = Ebpf::load(bytes.as_ref()).map_err(|_| BpfError::ProgramLoadFailed)?;
        if !matches_exact_object_contract(&ebpf, contract) {
            return Err(BpfError::InvalidProgramContract);
        }
        validate_sequence_map(&mut ebpf)?;
        populate_scope_map(&mut ebpf, &contract.scope)?;

        // Selecting by the exact object program name rejects a second,
        // unexpected program instead of attaching whichever iterator entry
        // happens to appear first.
        let program = ebpf
            .program_mut(&contract.program_name)
            .ok_or(BpfError::InvalidProgramContract)?;
        match contract.kind {
            BpfProgramKind::Tracepoint => {
                let (category, name) = split_tracepoint_attach_point(&contract.attach_point)?;
                let tracepoint: &mut TracePoint = program
                    .try_into()
                    .map_err(|_| BpfError::InvalidProgramContract)?;
                tracepoint.load().map_err(|_| BpfError::ProgramLoadFailed)?;
                tracepoint
                    .attach(category, name)
                    .map_err(|_| BpfError::ProgramLoadFailed)?;
            }
            BpfProgramKind::KProbe => {
                let kprobe: &mut KProbe = program
                    .try_into()
                    .map_err(|_| BpfError::InvalidProgramContract)?;
                kprobe.load().map_err(|_| BpfError::ProgramLoadFailed)?;
                kprobe
                    .attach(&contract.attach_point, 0)
                    .map_err(|_| BpfError::ProgramLoadFailed)?;
            }
            BpfProgramKind::FEntry => {
                let btf = Btf::from_sys_fs().map_err(|_| BpfError::InvalidProgramContract)?;
                let fentry: &mut FEntry = program
                    .try_into()
                    .map_err(|_| BpfError::InvalidProgramContract)?;
                fentry
                    .load(&contract.attach_point, &btf)
                    .map_err(|_| BpfError::ProgramLoadFailed)?;
                fentry.attach().map_err(|_| BpfError::ProgramLoadFailed)?;
            }
            BpfProgramKind::SocketFilter => return Err(BpfError::UnsupportedProgramKind),
        }
        let map = ebpf
            .take_map(EVENTS_MAP_NAME)
            .ok_or(BpfError::InvalidProgramContract)?;
        let ring_buf = map
            .try_into()
            .map_err(|_| BpfError::InvalidProgramContract)?;
        let loss_map = ebpf
            .take_map(LOSS_COUNTS_MAP_NAME)
            .ok_or(BpfError::InvalidProgramContract)?;
        let loss_counts = loss_map
            .try_into()
            .map_err(|_| BpfError::InvalidProgramContract)?;
        let handle = BpfHandle::new(
            contract.sensor.clone(),
            contract.kind,
            contract.attach_point.clone(),
        );
        let mut loaded = self
            .loaded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loaded.insert(
            handle.id(),
            LoadedProgram {
                _ebpf: ebpf,
                ring_buf,
                loss_counts,
            },
        );
        Ok(handle)
    }

    /// Attach a profile's complete sensor set as one transaction.  If any
    /// object cannot be loaded, validated, mapped, or attached, every link
    /// already created by this call is dropped before the error is returned.
    /// An empty list is rejected because it cannot establish observation
    /// readiness.
    pub fn load_contracts(
        &self,
        contracts: &[BpfObjectContract],
    ) -> Result<Vec<BpfHandle>, BpfError> {
        if contracts.is_empty() {
            return Err(BpfError::InvalidProgramContract);
        }

        let mut handles = Vec::with_capacity(contracts.len());
        for contract in contracts {
            match self.load_contract(contract) {
                Ok(handle) => handles.push(handle),
                Err(error) => {
                    for handle in &handles {
                        let _ = self.unload(handle);
                    }
                    return Err(error);
                }
            }
        }
        Ok(handles)
    }

    /// Remove one object and its links.  Dropping `Ebpf` is the Aya-owned
    /// detach operation, so a controlled probe stop cannot leave this
    /// loader's BPF attachments behind.
    pub fn unload(&self, handle: &BpfHandle) -> Result<(), BpfError> {
        let removed = self
            .loaded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&handle.id())
            .ok_or(BpfError::UnknownHandle)?;
        drop(removed);
        Ok(())
    }

    /// Decode v1 wire events and apply the supplied clock relation.  The
    /// mapper is injected so the probe can resample it around suspend or a
    /// detected realtime step; no `ktime` value is ever treated as epoch time.
    pub fn read_wire_events(
        &self,
        handle: &BpfHandle,
        timeout: Duration,
        mapper: KernelTimeMapper,
    ) -> Result<Vec<TimedWireEvent>, BpfError> {
        let deadline = Instant::now() + timeout;
        loop {
            let events = {
                let mut loaded = self
                    .loaded
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let entry = loaded
                    .get_mut(&handle.id())
                    .ok_or(BpfError::UnknownHandle)?;
                let mut events = Vec::new();
                while let Some(item) = entry.ring_buf.next() {
                    let event = match parse_wire_event(&item) {
                        Ok(event) => event,
                        Err(_) => {
                            self.invalid_wire_events.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                    };
                    let Some(observed_at) = mapper.map(event.ktime_ns) else {
                        self.invalid_wire_events.fetch_add(1, Ordering::Relaxed);
                        continue;
                    };
                    events.push(TimedWireEvent {
                        event,
                        observed_at,
                        time_confidence: mapper.confidence(),
                    });
                }
                events
            };
            if !events.is_empty() || Instant::now() >= deadline {
                return Ok(events);
            }
            std::thread::sleep(POLL_INTERVAL.min(timeout));
        }
    }

    /// Return the loss counters from the same loaded object as `handle`.
    /// Per-CPU values are summed with saturation: counter wrap must never
    /// make a known loss look smaller.
    pub fn loss_counters(&self, handle: &BpfHandle) -> Result<BpfLossCounters, BpfError> {
        let loaded = self
            .loaded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = loaded.get(&handle.id()).ok_or(BpfError::UnknownHandle)?;
        Ok(BpfLossCounters {
            exec: sum_loss_slot(&entry.loss_counts, 1)?,
            process_exit: sum_loss_slot(&entry.loss_counts, 2)?,
            tcp_connect: sum_loss_slot(&entry.loss_counts, 3)?,
            invalid_wire_events: self.invalid_wire_events.load(Ordering::Relaxed),
        })
    }
}

fn sum_loss_slot(loss_counts: &PerCpuArray<MapData, u64>, slot: u32) -> Result<u64, BpfError> {
    let values = loss_counts
        .get(&slot, 0)
        .map_err(|_| BpfError::ProgramLoadFailed)?;
    Ok(values.iter().copied().fold(0u64, u64::saturating_add))
}

/// Check names *and map kinds* before writing profile state or attaching a
/// hook.  This is intentionally stricter than merely looking up `EVENTS`:
/// an ELF with an extra map or program is not a DoD v1 object.
fn matches_exact_object_contract(ebpf: &Ebpf, contract: &BpfObjectContract) -> bool {
    let program_names: BTreeSet<_> = ebpf.programs().map(|(name, _)| name).collect();
    if program_names.len() != 1 || !program_names.contains(contract.program_name.as_str()) {
        return false;
    }

    let map_names: BTreeSet<_> = ebpf.maps().map(|(name, _)| name).collect();
    let expected_names: BTreeSet<_> = REQUIRED_MAP_NAMES.into_iter().collect();
    if map_names != expected_names {
        return false;
    }

    ebpf.maps().all(|(name, map)| {
        matches!(
            (name, map),
            (EVENTS_MAP_NAME, Map::RingBuf(_))
                | (SCOPE_MAP_NAME, Map::HashMap(_))
                | (LOSS_COUNTS_MAP_NAME, Map::PerCpuArray(_))
                | (SEQUENCE_MAP_NAME, Map::Array(_))
        )
    })
}

/// `SEQUENCE` is not consumed by host code, but its key/value width is part
/// of the C/Rust ABI.  Check it while the object is still unattached.
fn validate_sequence_map(ebpf: &mut Ebpf) -> Result<(), BpfError> {
    let map = ebpf
        .map_mut(SEQUENCE_MAP_NAME)
        .ok_or(BpfError::InvalidProgramContract)?;
    let sequence: AyaArray<_, u64> =
        AyaArray::try_from(map).map_err(|_| BpfError::InvalidProgramContract)?;
    if sequence.len() != 1 {
        return Err(BpfError::InvalidProgramContract);
    }
    Ok(())
}

impl Default for RealBpfLoader {
    /// Wie [`Self::new`].
    fn default() -> Self {
        Self::new()
    }
}

/// Prüft Bit [`CAP_BPF_BIT`] in einer hexadezimal kodierten
/// Fähigkeitsmaske.
///
/// # Description
/// Reine Funktion — keine Dateisystem- oder Kernel-Interaktion — damit
/// [`has_cap_bpf`]s Deutungslogik ohne `/proc` testbar bleibt, im Geiste des
/// gesamten Formungsteils dieser Crate (siehe [`crate::event`]-Moduldoku).
/// Eine nicht als Hexzahl lesbare Maske gilt als „Fähigkeit fehlt“, nie als
/// Fehler — dieselbe Haltung wie [`has_cap_bpf`] gegenüber einem fehlenden
/// oder unlesbaren `/proc/self/status`.
///
/// # Arguments
/// - `cap_eff_hex` (`&str`): der Wert hinter `CapEff:` in
///   `/proc/self/status`, ohne das Präfix.
///
/// # Returns
/// `true`, wenn Bit [`CAP_BPF_BIT`] gesetzt ist; `false` sonst, auch bei
/// unlesbarer Eingabe.
fn capability_bit_set(cap_eff_hex: &str, bit: u32) -> bool {
    u64::from_str_radix(cap_eff_hex.trim(), 16)
        .map(|mask| mask & (1u64 << bit) != 0)
        .unwrap_or(false)
}

fn cap_bpf_bit_set(cap_eff_hex: &str) -> bool {
    capability_bit_set(cap_eff_hex, CAP_BPF_BIT)
}

/// Prüft, ob der laufende Prozess `CAP_BPF` in seiner effektiven
/// Fähigkeitsmenge trägt.
///
/// # Description
/// Liest `/proc/self/status`, sucht die `CapEff:`-Zeile und delegiert die
/// Deutung an [`cap_bpf_bit_set`]. Reines `std`, kein `unsafe`, keine neue
/// Abhängigkeit — siehe Moduldoku, Abschnitt „Die Privilegienklasse“.
///
/// # Returns
/// `true`, wenn `CAP_BPF` effektiv vorhanden ist; `false` bei fehlender
/// Fähigkeit, fehlender oder unlesbarer `/proc/self/status`, oder fehlender
/// `CapEff:`-Zeile — nie ein Fehler, nie ein Panic.
fn has_cap_bpf() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("CapEff:"))
                .map(str::to_owned)
        })
        .is_some_and(|hex| cap_bpf_bit_set(&hex))
}

fn has_attach_capabilities() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("CapEff:"))
                .map(str::to_owned)
        })
        .is_some_and(|hex| has_cap_bpf() && capability_bit_set(&hex, CAP_PERFMON_BIT))
}

/// Install all profile entries before retrieving/loading/attaching a program.
/// The BPF objects use key `0` as an explicit host-profile marker; an empty
/// cgroup map is never interpreted as host observation.
fn populate_scope_map(ebpf: &mut Ebpf, scope: &BpfScope) -> Result<(), BpfError> {
    let map = ebpf
        .map_mut(SCOPE_MAP_NAME)
        .ok_or(BpfError::InvalidProgramContract)?;
    let mut ids: AyaHashMap<&mut MapData, u64, u8> =
        AyaHashMap::try_from(map).map_err(|_| BpfError::InvalidProgramContract)?;
    match scope {
        BpfScope::Host => ids
            .insert(0u64, 1u8, 0)
            .map_err(|_| BpfError::InvalidProgramContract),
        BpfScope::Cgroups { groups, .. } => {
            for group in groups {
                ids.insert(group.id, 1u8, 0)
                    .map_err(|_| BpfError::InvalidProgramContract)?;
            }
            Ok(())
        }
    }
}

/// Zerlegt einen Tracepoint-Anknüpfungspunkt in Kategorie und Name.
///
/// # Description
/// [`crate::spec::BpfProgramSpec::attach_point`] kodiert einen Tracepoint
/// als `"kategorie:name"` (z. B. `"sched:sched_process_exec"`).  TCP uses
/// the separate FEntry branch and therefore never reaches this helper.
///
/// # Arguments
/// - `attach_point` (`&str`): der volle Anknüpfungspunkt.
///
/// # Returns
/// `(kategorie, name)`, beide ohne den trennenden Doppelpunkt.
///
/// # Errors
/// - [`BpfError::ProgramLoadFailed`], wenn `attach_point` keinen
///   Doppelpunkt enthält.
fn split_tracepoint_attach_point(attach_point: &str) -> Result<(&str, &str), BpfError> {
    attach_point
        .split_once(':')
        .ok_or(BpfError::ProgramLoadFailed)
}

impl BpfLoader for RealBpfLoader {
    /// Siehe [`crate::loader::BpfLoader::load`].
    ///
    /// # Description
    /// Prüft zuerst [`has_cap_bpf`]; ohne die Fähigkeit endet dieser Aufruf
    /// sofort mit [`BpfError::CapabilityUnavailable`], ohne einen einzigen
    /// `aya`-Aufruf zu versuchen. Danach: `program.source.resolve()`,
    /// `Ebpf::load`, das erste (einzige) Programm aus dem Objekt nehmen, es
    /// je nach `program.kind` als `TracePoint` oder `KProbe` laden und
    /// anheften ([`BpfProgramKind::SocketFilter`] liefert
    /// [`BpfError::UnsupportedProgramKind`]), dann die `"EVENTS"`-Ringpuffer-
    /// Map nehmen und den gesamten Zustand unter dem neuen
    /// [`crate::handle::BpfHandle`] registrieren.
    ///
    /// # Errors
    /// - [`BpfError::CapabilityUnavailable`]: kein `CAP_BPF`.
    /// - [`BpfError::Io`]: [`crate::spec::BpfProgramSource::Path`] nicht
    ///   lesbar.
    /// - [`BpfError::UnsupportedProgramKind`]: `program.kind` ist
    ///   [`BpfProgramKind::SocketFilter`].
    /// - [`BpfError::ProgramLoadFailed`]: jeder andere `aya`-seitige
    ///   Fehlschlag (Objekt lässt sich nicht laden, kein Programm im
    ///   Objekt, Laden/Anheften scheitert, `"EVENTS"`-Map fehlt oder hat
    ///   den falschen Maptyp).
    fn load(&self, program: &BpfProgramSpec) -> Result<BpfHandle, BpfError> {
        let _ = program;
        // A profileless caller cannot prove map setup occurred before attach.
        // Refuse rather than preserving the prior broad-capture behavior.
        Err(BpfError::InvalidProgramContract)
    }

    /// Siehe [`crate::loader::BpfLoader::read_events`].
    ///
    /// # Description
    /// Pollt die registrierte Ringpuffer-Map in Schritten von
    /// [`POLL_INTERVAL`], bis entweder mindestens ein Ereignis vorliegt oder
    /// `timeout` verstrichen ist. Jeder gelesene Puffer wird sofort über
    /// [`crate::event::parse_raw_event`] gedeutet.
    ///
    /// # Errors
    /// - [`BpfError::UnknownHandle`], wenn `handle` nicht (mehr) registriert
    ///   ist.
    /// - [`BpfError::MalformedEvent`], wenn ein gelesener Puffer nicht die
    ///   erwartete Form hat.
    fn read_events(
        &self,
        handle: &BpfHandle,
        timeout: Duration,
    ) -> Result<Vec<RawBpfEvent>, BpfError> {
        let deadline = Instant::now() + timeout;
        loop {
            let events = {
                let mut loaded = self
                    .loaded
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let entry = loaded
                    .get_mut(&handle.id())
                    .ok_or(BpfError::UnknownHandle)?;
                let mut events = Vec::new();
                while let Some(item) = entry.ring_buf.next() {
                    events.push(parse_raw_event(&item)?);
                }
                events
            };
            if !events.is_empty() || Instant::now() >= deadline {
                return Ok(events);
            }
            std::thread::sleep(POLL_INTERVAL.min(timeout));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CAP_BPF_BIT, RealBpfLoader, cap_bpf_bit_set, has_cap_bpf, split_tracepoint_attach_point,
    };
    use crate::error::BpfError;
    use crate::loader::BpfLoader;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_new_boxed_as_dyn_bpf_loader_compiles_and_holds_no_programs() {
        // Kein Kernel-, kein Berechtigungszugriff: `new()` legt nur die
        // (leere) Registrierung an. Siehe Aufgabenregel „Lade in keinem
        // Test ein echtes eBPF-Programm“.
        let loader: Box<dyn BpfLoader> = Box::new(RealBpfLoader::new());
        drop(loader);
    }

    #[test]
    fn test_default_matches_new() {
        let _loader = RealBpfLoader::default();
    }

    #[test]
    fn test_cap_bpf_bit_set_true_when_bit_39_is_set() {
        // Nur Bit 39 gesetzt: 1u64 << 39 in Hex.
        let mask = 1u64 << CAP_BPF_BIT;
        assert!(cap_bpf_bit_set(&format!("{mask:x}")));
    }

    #[test]
    fn test_cap_bpf_bit_set_false_when_bit_39_is_clear() {
        // Alle Bits außer Bit 39 gesetzt.
        let mask = !(1u64 << CAP_BPF_BIT);
        assert!(!cap_bpf_bit_set(&format!("{mask:x}")));
    }

    #[test]
    fn test_cap_bpf_bit_set_false_for_unparseable_hex() {
        assert!(!cap_bpf_bit_set("not-hex"));
    }

    #[test]
    fn test_cap_bpf_bit_set_trims_whitespace() {
        let mask = 1u64 << CAP_BPF_BIT;
        assert!(cap_bpf_bit_set(&format!("   {mask:x}\t")));
    }

    #[test]
    fn test_has_cap_bpf_never_panics_regardless_of_host_state() {
        // Umgebungsabhängig (root/CAP_BPF vs. nicht) — dieser Test prüft nur,
        // dass die Funktion total ist, nicht welchen Wert sie liefert.
        let _ = has_cap_bpf();
    }

    #[test]
    fn test_split_tracepoint_attach_point_splits_on_first_colon() -> TestResult {
        let (category, name) = split_tracepoint_attach_point("sched:sched_process_exec")
            .map_err(ctx("split_tracepoint_attach_point"))?;
        assert_eq!(category, "sched");
        assert_eq!(name, "sched_process_exec");
        Ok(())
    }

    #[test]
    fn test_split_tracepoint_attach_point_without_colon_returns_program_load_failed() -> TestResult
    {
        let Err(err) = split_tracepoint_attach_point("no-colon-here") else {
            return Err(TestError::Unexpected("must fail without a colon".into()));
        };
        assert!(matches!(err, BpfError::ProgramLoadFailed));
        Ok(())
    }
}
