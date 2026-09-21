//! eBPF-Ladeschicht hinter einem Trait — Knoten **AW7-01a**.
//!
//! > **V1-Betriebsvertrag.** Die historischen Erläuterungen zu
//! > `BpfProgramSpec`/`RawBpfEvent` weiter unten bleiben ausschließlich für
//! > Fixture-Kompatibilität erhalten. Ein echter Lader akzeptiert nur
//! > [`BpfObjectContract`] über [`real::RealBpfLoader::load_contract`] oder
//! > `load_contracts`: versionierte C-ELFs, die vier festen Maps, ein vor dem
//! > Attach gefülltes [`BpfScope`] und die v1-Wire-Ereignisse. Insbesondere
//! > ist `sock:inet_sock_set_state` kein Produktionshook mehr; TCP-Connect
//! > hängt per FEntry an `tcp_v4_connect` bzw. `tcp_v6_connect`.
//!
//! # Zweck
//! Diese Crate ist die Fassade zwischen dem Kernel und den drei
//! Sensor-Crates, die eBPF-Programme brauchen: `harw-dod-procmon`
//! (AW7-01b), `harw-dod-flow` (AW7-01c) und ein künftiger Binary-Knoten
//! (AW7-01d). Sie definiert [`BpfLoader`] — ein Ziel, in das Programme
//! geladen und aus dem Ereignisse gelesen werden — sowie die eigenen Typen
//! [`BpfProgramSpec`], [`BpfHandle`], [`event::RawBpfEvent`] und
//! [`error::BpfError`], gegen die alle drei Folgeknoten schreiben.
//!
//! # Welcher Teil Berechtigungen braucht — und welcher nicht
//! Diese Crate trennt zwei Hälften scharf:
//!
//! - **Formungsteil** ([`event`]): reine Funktionen auf `&[u8]`, die rohe
//!   Ereignisbytes deuten — Feldbreiten, Byte-Reihenfolge, Zeichenketten
//!   fester Länge, Nullterminierung. Braucht **nichts**: keinen Kernel,
//!   keine Berechtigung, keine eBPF-Toolchain. Der weitaus größte Teil des
//!   Codes dieser Crate liegt hier, wie beauftragt.
//! - **Ladeteil**: der eigentliche `bpf()`-Syscall, der `CAP_BPF` verlangt.
//!   [`fixture::FixtureBpfLoader`] simuliert seine Schnittstelle vollständig
//!   ohne Kernel, ohne Berechtigung, ohne eBPF-Toolchain, und ist deshalb
//!   Teil der normalen API, nicht hinter `#[cfg(test)]`. [`real::RealBpfLoader`]
//!   ist die echte, `aya`-gestützte Implementierung — siehe Abschnitt
//!   „Der reale Ladeteil: `aya`, nachträglich aufgenommen“ unten und die
//!   ausführliche Recherche in der [`real`]-Moduldoku.
//!
//! Damit laufen [`BpfProgramSpec`], [`BpfHandle`], [`event::RawBpfEvent`],
//! [`error::BpfError`], der gesamte Formungsteil und
//! [`fixture::FixtureBpfLoader`] — also der weit überwiegende Teil dieser
//! Crate — ohne jede erhöhte Berechtigung. Der Bericht der Architektur ist
//! ausdrücklich stolz darauf, dass fünf von acht Stufen ohne
//! Kernel-Berechtigung laufen; diese Crate hält sich an dieses Ziel, statt
//! es zu unterlaufen. [`real::RealBpfLoader`] ist der eine Teil, der
//! `CAP_BPF` voraussetzt — und keine andere erhöhte Fähigkeit.
//!
//! # Warum kein `aya`-Typ nach außen dringt
//! Ein CI-Gate prüft, dass kein `aya::…`-Typ (oder der Typ einer anderen
//! eBPF-Bibliothek) in einer `pub`-Signatur dieser Crate erscheint.
//! `BpfProgramSpec`, `BpfHandle`, `event::RawBpfEvent` und `error::BpfError`
//! sind deshalb eigene Typen dieser Crate, und [`real::RealBpfLoader`] hält
//! ihren gesamten `aya`-Zustand in einem privaten Feld (siehe
//! [`real`]-Moduldoku, Abschnitt „Warum kein `aya`-Typ nach außen dringt“).
//! Der Grund: `aya` ist eine schwere, sich bewegende Abhängigkeit — stünde
//! sie in öffentlichen Signaturen, wanderte jede ihrer Änderungen durch den
//! halben DoD-Teilbaum. Hinter der Fassade ist ein Wechsel ein Austausch.
//!
//! # Der reale Ladeteil: `aya`, nachträglich aufgenommen
//! Eine frühere Fassung dieser Crate verzichtete bewusst auf `aya` — mit
//! zwei Begründungen: `SocketFilter::attach` bräuchte einen bereits offenen,
//! vom Aufrufer besessenen Socket, den eine generische Ladeschicht nicht
//! besitzen darf; und ein neuer, in der damaligen Aufgabenform nicht per
//! `cargo` verifizierbarer Abhängigkeitszuwachs sei im späteren
//! `CAP_BPF`-Teil ein zu großes Risiko. Der erste Punkt ist inzwischen
//! gegenstandslos: `harw-dod-flow` hängt an einem Tracepoint
//! (`sock:inet_sock_set_state`), nicht mehr an `SocketFilter`, und
//! `harw-dod-procmon` ebenso (`sched:sched_process_exec`) — kein heutiger
//! Konsument braucht die Programmart, die den offenen Socket verlangt hätte.
//! Der zweite Punkt bleibt eine Randbedingung dieser Aufgabenform (kein
//! `cargo`-Lauf), aber die zentrale, sequenzielle Verifikation dieses
//! Workspace übernimmt genau die Prüfung, die vorher fehlte.
//!
//! [`real::RealBpfLoader`] ist deshalb jetzt Teil dieser Crate — vollständig
//! implementiert, aber **ungefüttert**: sie lädt ein bereits übersetztes
//! eBPF-ELF-Objekt, erzeugt aber keines. Die [`real`]-Moduldoku enthält die
//! vollständige Recherche (Fassung, `unsafe`-Fläche, `build.rs`-Frage,
//! transitive Crate-Zahl, jeweils mit Quelle) sowie die Begründung, warum
//! die Objekterzeugung selbst — ein `aya-ebpf`/`nightly`/
//! `bpfel-unknown-none`-Übersetzungslauf — ein eigener, künftiger Baustein
//! bleibt und nicht Teil dieser Crate ist.
//!
//! # Abweichung vom Auftrag: keine Abhängigkeit auf `harw-dod-signals`
//! Der Auftrag listete `harw-dod-signals` neben `harw-dod-cap`, `harw-types`
//! und `harw-macros` als Pfadabhängigkeit. Diese Crate liefert jedoch
//! bewusst rohe Bytes ([`event::RawBpfEvent`]), keine
//! `harw_dod_signals::HostSample`/`SecurityEvent`-Werte — genau wie
//! `harw_dod_signals::sensor`'s eigene Moduldoku festhält, dass die
//! Übersetzung roher Beobachtungen in Domänen-Vokabular Sache der
//! Sensor-Crate ist, die sie konsumiert (`harw-dod-procmon`,
//! `harw-dod-flow`), nicht dieser generischen Ladeschicht. Eine Abhängigkeit
//! ohne einen einzigen referenzierten Typ wäre unbegründete Kopplung an eine
//! Crate, die sich ohnehin ändert, sobald `harw-dod-rules` (AW4-03) ihr
//! `Finding<S>`-Typestate anfasst — deshalb fehlt sie hier, statt sie
//! unbenutzt in `Cargo.toml` mitzuführen.
//!
//! # Der Betrieb ohne `CAP_BPF`
//! Ein Host ohne diese Fähigkeit ist ein **dokumentierter, lauffähiger
//! Fall**, kein Fehler: [`BpfLoader::load`] liefert dort
//! [`error::BpfError::CapabilityUnavailable`], und ein Sentinel meldet den
//! betroffenen Sensor als degradiert, statt den Prozess scheitern zu
//! lassen. [`fixture::FixtureBpfLoader::without_capability`] simuliert genau
//! diesen Fall testbar. Die zugehörige Fähigkeit ist
//! [`harw_dod_cap::Capability::LoadBpfProgram`]
//! ([`harw_dod_cap::CapabilityClass::Bpf`]) — siehe [`REQUIRED_CAPABILITY`].
//!
//! # Exportierte Typen
//! [`BpfLoader`], [`BpfProgramKind`], [`BpfProgramSource`],
//! [`BpfProgramSpec`], [`BpfHandle`], [`event::RawBpfEvent`],
//! [`error::BpfError`], [`error::BpfResult`], [`fixture::FixtureBpfLoader`],
//! [`real::RealBpfLoader`], [`REQUIRED_CAPABILITY`].
//!
//! # Nebenläufigkeit
//! [`BpfLoader`] ist `Send + Sync`; ein Sentinel hält Lader hinter `Arc`.
//! Alle Datentypen sind reine Werte ohne innere Veränderlichkeit.
//! [`fixture::FixtureBpfLoader`] kapselt ihre einzige veränderliche
//! Ressource — die Ereigniswarteschlange — hinter `std::sync::Mutex`;
//! [`real::RealBpfLoader`] ebenso ihre Registrierung geladener Programme
//! (siehe [`real`]-Moduldoku für die Nebenläufigkeitsannahme über `aya`s
//! eigene Typen).
//!
//! # Fehler
//! [`error::BpfError`] ist der eine Fehlertyp dieser Crate.
//!
//! # Examples
//! ```rust
//! use harw_dod_bpf::{BpfLoader, BpfProgramKind, BpfProgramSource, BpfProgramSpec};
//! use harw_dod_bpf::fixture::FixtureBpfLoader;
//! use harw_types::SensorId;
//! use std::borrow::Cow;
//! use std::time::Duration;
//!
//! let loader = FixtureBpfLoader::new(Vec::new());
//! let spec = BpfProgramSpec::new(
//!     SensorId::from_str("procmon-0"),
//!     BpfProgramKind::Tracepoint,
//!     "syscalls:sys_enter_execve",
//!     BpfProgramSource::Embedded(Cow::Borrowed(b"\0asm".as_slice())),
//! );
//!
//! let handle = loader.load(&spec).expect("fixture loader with capability always succeeds");
//! let events = loader.read_events(&handle, Duration::from_millis(0)).unwrap();
//! assert!(events.is_empty());
//! ```

pub mod abi;
pub mod contract;
pub mod error;
pub mod event;
pub mod fixture;
pub mod handle;
pub mod loader;
pub mod real;
pub mod profile;
pub mod spec;
pub mod time;

pub use error::BpfError;
pub use abi::{parse_wire_event, TaskIdentity, WireEvent, WireEventType, WIRE_HEADER_LEN_V1, WIRE_VERSION_V1};
pub use contract::{
    BpfObjectContract, EVENTS_MAP_NAME, EXEC_ATTACH_POINT, EXEC_PROGRAM_NAME,
    EXIT_ATTACH_POINT, EXIT_PROGRAM_NAME, LOSS_COUNTS_MAP_NAME,
    REQUIRED_MAP_NAMES, SEQUENCE_MAP_NAME, SCOPE_MAP_NAME,
    TCP_V4_CONNECT_ATTACH_POINT, TCP_V4_CONNECT_PROGRAM_NAME,
    TCP_V6_CONNECT_ATTACH_POINT, TCP_V6_CONNECT_PROGRAM_NAME,
};
pub use event::RawBpfEvent;
pub use fixture::FixtureBpfLoader;
pub use handle::BpfHandle;
pub use loader::BpfLoader;
pub use real::{BpfLossCounters, RealBpfLoader, TimedWireEvent};
pub use profile::{BpfScope, ResolvedCgroup, MAX_SCOPE_CGROUP_IDS};
pub use spec::{BpfProgramKind, BpfProgramSource, BpfProgramSpec};
pub use time::{KernelTimeMapper, TimeConfidence};

/// Die Fähigkeit, die ein echter (Nicht-Fixture-)Lader zum Laden braucht.
///
/// # Description
/// Diese Crate setzt die Fähigkeit nicht selbst durch — das bleibt Sache
/// einer echten [`BpfLoader`]-Implementierung und des Sentinels, der sie
/// registriert. Diese Konstante benennt sie nur, damit kein Aufrufer sie
/// sich selbst ausdenken oder aus dem Kontext erraten muss.
///
/// # Examples
/// ```rust
/// use harw_dod_bpf::REQUIRED_CAPABILITY;
/// use harw_dod_cap::CapabilityClass;
///
/// assert_eq!(REQUIRED_CAPABILITY.class(), CapabilityClass::Bpf);
/// ```
pub const REQUIRED_CAPABILITY: harw_dod_cap::Capability = harw_dod_cap::Capability::LoadBpfProgram;

#[cfg(test)]
mod tests {
    use super::REQUIRED_CAPABILITY;

    #[test]
    fn test_required_capability_is_load_bpf_program_in_the_bpf_class() {
        assert_eq!(REQUIRED_CAPABILITY, harw_dod_cap::Capability::LoadBpfProgram);
        assert_eq!(REQUIRED_CAPABILITY.class(), harw_dod_cap::CapabilityClass::Bpf);
    }

    /// Hält die (jetzt umgekehrte) Entscheidung strukturell fest: diese
    /// Crate zieht `aya` — siehe Moduldoku, Abschnitt „Der reale Ladeteil:
    /// `aya`, nachträglich aufgenommen“, und die vollständige Recherche in
    /// der [`crate::real`]-Moduldoku. Das CI-Gate „kein `aya`-Typ in einer
    /// `pub`-Signatur“ bleibt davon unberührt — es prüft die Signaturen
    /// selbst, nicht die Abwesenheit der Abhängigkeit, und
    /// [`crate::real::RealBpfLoader`] hält jeden `aya`-Typ in einem
    /// privaten Feld.
    #[test]
    fn test_cargo_toml_declares_the_verified_aya_dependency() {
        let manifest = include_str!("../Cargo.toml");
        assert!(
            manifest.contains("aya = \"0.14.0\""),
            "harw-dod-bpf soll laut real-Moduldoku aya 0.14.0 ziehen (Version per crates.io-API verifiziert, nicht geraten)"
        );
    }
}
