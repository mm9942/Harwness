//! fanotify-Dateisystemwächter, Bibliotheksteil.
//!
//! # Zwei Hälften, zwei Berechtigungsregime
//! Der ursprüngliche Plan hatte Bibliothek und Binary in einem Knoten. Das
//! war falsch geschnitten: diese Crate ist der Teil, der **ohne**
//! Berechtigungen testbar ist; das Binary `harw-probe-fs` (Knoten AW4-02b)
//! trägt `CAP_SYS_ADMIN`, den Empfangspfad für rohe Kernel-Ereignisse und die
//! `Landlock`-Bindung. Zwei Testregime, zwei Reviewer — die Trennung sorgt
//! dafür, dass der prüfbare Teil groß und der unprüfbare Teil klein bleibt.
//!
//! Innerhalb dieser Crate gilt dieselbe Trennung noch einmal, in Miniatur:
//! - **Formungsteil — vollständig testbar** ([`mask`], [`fdpath`],
//!   [`loginuid`], [`shape`]): reine Funktionen auf Bytes und Zahlen. Rohe
//!   fanotify-Ereignisse → `harw_dod_signals::SecurityEvent`. Kein
//!   Betriebssystemaufruf außer dem privilegienlosen loginuid-Lesezugriff
//!   über [`harw_dod_readfs`].
//! - **Bindungsteil — klein und hinter einem Trait** ([`raw`]): die
//!   Schnittstelle zu einer Quelle roher Ereignisse. Nur eine
//!   Implementierung lebt hier: [`raw::FixtureFsEventSource`], die aus einer
//!   vorab konfigurierten Liste liefert.
//!
//! # Stand der fanotify-Bindung: gebaut — im Binary-Knoten, nicht hier
//! Diese Crate liefert weiterhin **keine** echte fanotify-Bindung selbst.
//! Grund unverändert: die für den Workspace vorgesehene `rustix`-Fassung
//! 1.1.4 (siehe `[workspace.dependencies]`) implementiert
//! `fanotify_init`/`fanotify_mark` **nicht** — beide Funktionen stehen in
//! `rustix`s eigener `not_implemented!`-Liste (`src/not_implemented.rs`),
//! ohne auch nur eine unsichere Hülle. Ein roher `libc`-Syscall über
//! `unsafe` bleibt versperrt (`[workspace.lints.rust] unsafe_code =
//! "forbid"`, nicht `"deny"`: nicht per `#[allow]` überstimmbar).
//!
//! Der Binary-Knoten `harw-probe-fs` (AW4-02b) hat jedoch eine zweite,
//! sichere Bindung recherchiert und eingebaut: `nix::sys::fanotify`, seit
//! Fassung 0.28.0 im Baum, mit E/A-Sicherheit seit 0.30.0 — siehe
//! `harw-probe-fs/src/source.rs`-Moduldoku für Quellen, Fassungen und die
//! volle Begründung. Diese Crate liefert weiterhin genau das, was ihr
//! Zuschnitt vorsieht: [`raw::FsEventSource`], den vollständigen
//! Formungsteil und [`raw::FixtureFsEventSource`] — die Bindung selbst
//! bleibt bewusst im privilegierten Binary-Knoten, getrennt vom
//! privilegienlos testbaren Formungsteil (siehe Abschnitt „Zwei Hälften,
//! zwei Berechtigungsregime" oben).
//!
//! # Push-only: kein Empfangspfad ist ausdrückbar
//! Der spätere Wächter hat keinen Empfangspfad — er sendet Ereignisse und
//! nimmt keine Anweisungen entgegen. Ein Prozess mit `CAP_SYS_ADMIN`, der
//! Nachrichten annimmt, ist ein Angriffsziel; einer, der nur sendet, ist
//! keins. Diese Eigenschaft ist hier keine Konvention, sondern eine
//! Eigenschaft der API selbst: [`raw::FsEventSource`] hat genau **eine**
//! Methode (`read_events`), und diese liefert ausschließlich Daten heraus.
//! Es gibt kein `handle_command`, kein `set_watch_from`, keine Methode, die
//! einen Parameter entgegennähme, der den Zustand des Wächters von außen
//! verändern könnte. Ein Aufrufer kann diese API nicht dazu benutzen, dem
//! Wächter irgendetwas mitzuteilen — nur, um von ihm zu lesen.
//!
//! # Die loginuid-Falle
//! [`loginuid::resolve_loginuid`] liest `/proc/<pid>/loginuid` — die
//! Anmelde-UID, den einen Wert, den ein `sudo`-Aufruf **nicht** verändert,
//! und damit die einzige Kennung, die beantwortet, wer eine Kette wirklich
//! angestoßen hat. Ein `loginuid`-Wert von `4294967295` (`-1` als `u32`)
//! bedeutet „nicht gesetzt" und muss als [`None`] erscheinen — **nicht** als
//! `Some(4294967295)`, was wie eine gültige, aber absurd hohe UID aussähe
//! und deshalb nie aus Versehen auffiele. Siehe [`loginuid`]-Moduldoku für
//! die volle Begründung; der entsprechende Test ist der wichtigste dieser
//! Crate.
//!
//! # Verantwortungsbereich
//! - [`raw`]: [`raw::RawFsEvent`], [`raw::FsEventSource`],
//!   [`raw::FixtureFsEventSource`].
//! - [`mask`]: Deutung roher Ereignismasken zu `EventKind`.
//! - [`fdpath`]: Deutung eines aufgelösten Dateideskriptor-Ziels als Pfad.
//! - [`loginuid`]: Auflösung der loginuid über [`harw_dod_readfs`].
//! - [`shape`]: [`shape::shape_event`] — der eine Formungs-Einstiegspunkt,
//!   der die vorigen drei Module verkettet.
//! - [`sensor`]: [`sensor::FsMonSensor`] — bindet Quelle, Bereich und Formung
//!   zu einem `harw_dod_signals::Sensor` mit
//!   [`harw_dod_cap::Capability::WatchFilesystem`] zusammen. Genau diese eine
//!   Fähigkeit; keine Abhängigkeit auf eine andere Sensor-Crate.
//! - [`error`]: [`error::FsMonError`], die einzige Fehlermenge dieser Crate.
//!
//! # Exportierte Typen
//! [`raw::RawFsEvent`], [`raw::FsEventSource`], [`raw::FixtureFsEventSource`],
//! [`sensor::FsMonSensor`], [`error::FsMonError`], [`error::FsMonResult`].
//!
//! # Nebenläufigkeit
//! Jede öffentliche Funktion ist zustandslos und `Send + Sync` ohne innere
//! Veränderlichkeit; siehe die jeweilige Modul-Dokumentation für Details.
//! [`raw::FsEventSource`] verlangt `Send + Sync`, damit eine künftige
//! Bindung hinter `Arc` gehalten und aus einem Sammelthread aufgerufen
//! werden kann.
//!
//! # Fehler
//! [`error::FsMonError`] — inhaltsfrei, siehe dessen Moduldoku. Die
//! loginuid-Auflösung ist bewusst fehlerfrei ([`Option`]-basiert), siehe
//! [`loginuid`]-Moduldoku.
//!
//! # Examples
//! ```rust
//! use std::path::Path;
//! use harw_dod_cap::ReadScope;
//! use harw_dod_fsmon::raw::{FixtureFsEventSource, FsEventSource, RawFsEvent};
//! use harw_dod_fsmon::shape::shape_event;
//! use harw_types::SensorId;
//! use std::time::Duration;
//!
//! let source = FixtureFsEventSource::new(vec![RawFsEvent {
//!     mask: 0x08, // FAN_CLOSE_WRITE
//!     pid: 4242,
//!     uid: 1000,
//!     fd_target: "/srv/data/report.csv".to_owned(),
//! }]);
//! let raw_events = source.read_events(Duration::from_millis(1)).expect("Fixture liefert immer");
//!
//! let scope = ReadScope::from_roots([Path::new("/srv/data").to_path_buf()]);
//! let sensor = SensorId::from_str("fsmon-0");
//!
//! let event = shape_event(&raw_events[0], &scope, Path::new("/proc"), &sensor, jiff::Timestamp::UNIX_EPOCH)
//!     .expect("Schreibmaske muss formbar sein")
//!     .expect("Pfad liegt im Bereich");
//! assert!(matches!(event.kind, harw_dod_signals::EventKind::FileWrite { .. }));
//! ```

pub mod error;
pub mod fdpath;
pub mod loginuid;
pub mod mask;
pub mod raw;
pub mod sensor;
pub mod shape;

pub use error::{FsMonError, FsMonResult};
pub use raw::{FixtureFsEventSource, FsEventSource, RawFsEvent};
pub use sensor::FsMonSensor;
pub use shape::shape_event;

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
