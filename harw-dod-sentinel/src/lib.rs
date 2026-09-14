//! Die Sammelstelle: hält Sensoren, ruft `poll`, führt ihren
//! Gesundheitszustand und puffert, was sie liefern (Knoten AW2-18,
//! Vertrag `docs/aw-contract-master.md`).
//!
//! # Die härteste Auflage: keine Parselogik
//! **Diese Crate enthält keine einzige Zeile, die eine Quelle deutet.**
//! Kein `/proc`-Format, kein sysfs-Wert, kein Audit-Record. Sie kennt nur
//! `dyn` [`harw_dod_signals::Sensor`] und ruft
//! [`harw_dod_signals::Sensor::poll`] auf; alles, was ein Abruf liefert
//! ([`harw_dod_signals::SensorReading`]), wird unverändert durchgereicht,
//! gepuffert oder in ein Zustandsübergangsurteil (`Ok`/`Err` +
//! `Permanence`) umgesetzt — nie inhaltlich inspiziert.
//!
//! **Warum das so hart durchgesetzt wird:** elf Sensor-Crates haben je
//! genau eine Quelle und je genau eine Fähigkeit (Vertrag Abschnitt F/G).
//! Zöge diese Sammelstelle auch nur eine Parselogik an sich — und sei es nur
//! ein einziger `if line.starts_with(...)` —, hätte sie zwei Quellen, und
//! die Behauptung „genau eine Quelle je Crate" wäre unwahr, ohne dass es
//! irgendeinem einzelnen Konsumenten auffiele. Ein CI-Gate erzwingt die
//! strukturelle Hälfte dieser Regel direkt: `harw-dod-sentinel` hängt in
//! `Cargo.toml` auf keine der elf Sensor-Crates ab, nur auf
//! [`harw_dod_cap`] (Zugriffsvokabular) und [`harw_dod_signals`]
//! (Datenvokabular). Eine Abhängigkeit auf eine Sensor-Crate wäre bereits
//! der erste Schritt zu einer zweiten Quelle in dieser Crate.
//!
//! # Der Sentinel ist unprivilegiert
//! `Sentinel` hält **keine eigene Fähigkeit**. Jede [`harw_dod_cap::Capability`]
//! sitzt in dem Sensor, der sie beim Aufbau seines
//! [`harw_dod_cap::SensorHandle`] gebunden hat — die Sammelstelle ruft nur
//! `poll()` auf ein bereits gebundenes `dyn Sensor`-Trait-Objekt und liest
//! nirgends selbst aus `/proc`, `/sys`, einem Netlink-Socket oder einer
//! Datei. Das ist der Grund, warum das Binary, das diese Crate zu einem
//! laufenden Prozess macht (Knoten AW2-19), das erste vollständig
//! unprivilegierte Binary des gesamten Ausbauprogramms sein kann: es muss
//! keine einzige Capability besitzen, die nicht schon in einem der
//! übergebenen Sensoren steckt.
//!
//! # Der Degradationsautomat (Übersicht)
//! Je Sensor ein [`health::SensorHealth`] — `Bound` (liefert), `Retrying`
//! (vorübergehender Fehler) oder `Degraded` (aufgegeben, wird nicht mehr
//! abgerufen). Siehe [`health`]-Moduldoku für das vollständige
//! Zustandsdiagramm, die Begründung für „`Permanent` überspringt jeden
//! Zwischenversuch" und die ausführlich begründete Entscheidung, dass es
//! **innerhalb eines laufenden Prozesses keinen Rückweg aus `Degraded`
//! gibt** — nur ein Neustart des Sentinel-Prozesses baut einen Sensor
//! wieder `Bound` auf.
//!
//! **Der wichtigste Teil dieses Automaten:** ein Übergang nach `Degraded`
//! erzeugt ein [`harw_dod_signals::SecurityEvent`] mit
//! [`harw_dod_signals::EventKind::SensorDegraded`]. Ein Sensor, der still
//! ausfällt, ist schlimmer als keiner — man verlässt sich auf ihn und
//! bekommt Ruhe statt Meldungen, wo doch eine fehlende Meldung genau dann
//! am gefährlichsten ist, wenn sie am meisten gebraucht würde. Die
//! Abmeldung eines Sensors muss deshalb selbst eine Meldung sein, keine
//! stille Zustandsänderung.
//!
//! # Der Ringpuffer (Übersicht)
//! [`buffer::EvidenceBuffer`] hält die jüngsten
//! [`harw_dod_signals::HostSample`]s und [`harw_dod_signals::SecurityEvent`]s
//! in zwei getrennten, fest dimensionierten Ringen, die beim Erreichen
//! ihrer Kapazität den jeweils ältesten Eintrag überschreiben. Die Größe
//! ist eine **Sicherheitsentscheidung**, keine bloße Speicherfrage — siehe
//! `buffer`-Moduldoku für die vollständige Begründung (zu klein: ein
//! Angreifer verdrängt seine Spuren durch Lärm; zu groß: unbegrenzter
//! Speicherbedarf) und die gewählten, konfigurierbaren Voreinstellungen.
//! [`buffer::EvidenceBuffer::freeze`] friert den aktuellen Inhalt als
//! zitierfähigen [`harw_dod_signals::SecurityEvidence`]-Beleg ein — über den
//! dort bereits gelandeten Konstruktor, der den Digest selbst bildet;
//! diese Crate berechnet ihn nicht zweitrangig noch einmal selbst.
//!
//! **Zwei Schreibwege führen in diesen Puffer, nicht einer:**
//! [`Sentinel::poll_all`] für Ereignisse, die diese Crate selbst aus einem
//! `Sensor::poll`-Aufruf erzeugt, und [`Sentinel::record_external_event`]
//! für ein [`harw_dod_signals::SecurityEvent`], das anderswo bereits
//! vollständig entstanden ist — ein privilegierter Sondenprozess, der es
//! über einen `SOCK_SEQPACKET`-Socket an das Sammel-Binary schickt (Vertrag
//! Entscheidung Nr. 3), oder das Binary selbst bei einer eigenen
//! Degradation (z. B. eine nicht durchsetzbare Landlock-Selbstbeschränkung).
//! Beide Wege teilen sich denselben Ring und dasselbe stille
//! Verdrängungsverhalten bei Kapazitätsüberlauf — siehe `sentinel`-Moduldoku,
//! Abschnitt "Zwei Schreibwege in den Puffer", für die vollständige
//! Begründung, warum es zwei sind und warum sie sich nur in ihrer Herkunft
//! unterscheiden dürfen, nicht in ihrem Verhalten.
//!
//! # Modulübersicht
//! - [`error`]: [`error::SentinelError`], der einzige Fehlertyp dieser
//!   Crate.
//! - [`health`]: [`health::SensorHealth`], [`health::DegradeReason`],
//!   [`health::RetryPolicy`] — der Degradationsautomat.
//! - [`buffer`]: [`buffer::EvidenceBuffer`] — die zwei Ringe und ihr
//!   `freeze()`.
//! - [`metrics`]: die in [`harw_observe`] deklarierten Kennzahlen dieser
//!   Crate (Abrufe je Sensor, Fehler nach `Permanence`, degradierte
//!   Sensoren, Pufferauslastung).
//! - [`spool`]: [`spool::FindingSpool`] — die dateibasierte Übergabe
//!   eingefrorener Befunde (`harw_dod_rules::finding::FindingRecord`) an
//!   Triage und Escalator (W3/C-FIND): atomar geschrieben (`0640`),
//!   symlinkfest gelesen, größen- und eintragsbegrenzt, Cursor in
//!   Schreibreihenfolge. Das ist **keine** Parselogik im Sinne der Auflage
//!   oben: gelesen werden ausschließlich Records, die diese Crate selbst im
//!   eigenen Format geschrieben hat, nie eine Sensorquelle.
//! - [`Sentinel`], [`SentinelConfig`]: die Sammelstelle selbst, die die
//!   drei Bausteine oben zusammenführt — mit zwei Schreibwegen in ihren
//!   Puffer ([`Sentinel::poll_all`] für Sensor-Abrufe,
//!   [`Sentinel::record_external_event`] für andernorts bereits entstandene
//!   Ereignisse).
//!
//! # Nebenläufigkeit
//! Jeder Datentyp dieser Crate ist `Send + Sync`, weil jedes seiner Felder
//! es ist — kein internes Locking, keine geteilten Ressourcen außer dem in
//! [`Sentinel`] gehaltenen `Arc<dyn TelemetrySink>` und den `Arc<dyn
//! Sensor>`-Trait-Objekten selbst, beide bereits vertraglich `Send + Sync`
//! (siehe `sentinel`-Moduldoku für die Begründung, warum diese Crate
//! trotzdem mit einem einzigen Sammelthread entworfen ist).
//!
//! # Fehler
//! [`error::SentinelError`] — ein einziger Fehlerpfad der Sammelstelle
//! ([`Sentinel::freeze`]/[`buffer::EvidenceBuffer::freeze`]). Der Spool hat
//! seinen eigenen, von der Sammelstelle getrennten Fehlertyp
//! [`spool::SpoolError`] (Dateisystem, Größen-/Symlinkschutz). Ein
//! fehlgeschlagener Sensor-Abruf ist **kein** `Err` dieser Crate, sondern
//! ein Zustandsübergang im Automaten (siehe [`health`]-Moduldoku).
//!
//! # Examples
//! ```rust
//! use harw_dod_sentinel::{Sentinel, SentinelConfig};
//! use harw_dod_signals::{Sensor, SensorReading};
//! use harw_observe::NullSink;
//! use harw_types::SensorId;
//! use std::sync::Arc;
//!
//! #[derive(Debug)]
//! struct AlwaysEmpty {
//!     handle: harw_dod_cap::SensorHandle<harw_dod_cap::Bound>,
//! }
//!
//! impl Sensor for AlwaysEmpty {
//!     fn handle(&self) -> &harw_dod_cap::SensorHandle<harw_dod_cap::Bound> {
//!         &self.handle
//!     }
//!
//!     fn poll(&self, _now: jiff::Timestamp) -> Result<SensorReading, harw_dod_cap::SensorError> {
//!         Ok(SensorReading::default())
//!     }
//! }
//!
//! let scope = harw_dod_cap::ReadScope::from_roots([std::path::PathBuf::from("/proc/stat")]);
//! let handle = harw_dod_cap::SensorHandle::new(
//!     SensorId::from_str("proc-stat-0"),
//!     harw_dod_cap::Capability::ReadProcStat,
//! )
//! .bind(scope);
//! let sensor: Arc<dyn Sensor> = Arc::new(AlwaysEmpty { handle });
//!
//! let mut sentinel = Sentinel::new(vec![sensor], Arc::new(NullSink), SentinelConfig::default());
//! let reading = sentinel.poll_all(jiff::Timestamp::UNIX_EPOCH);
//! assert!(reading.samples.is_empty());
//!
//! let evidence = sentinel
//!     .freeze(jiff::Timestamp::UNIX_EPOCH)
//!     .expect("evidence capture is infallible for these field types");
//! assert!(evidence.events.is_empty());
//! ```
//!
//! # Stand
//! Gerüst aus Knoten AW0-00 (Workspace-Fundament). Der Inhalt entstand in
//! Knoten **AW2-18**; Ebene **L4** im Zielgraphen.

pub mod buffer;
pub mod error;
pub mod health;
pub mod metrics;
pub mod sentinel;
pub mod spool;

pub use buffer::EvidenceBuffer;
pub use error::{SentinelError, SentinelResult};
pub use health::{DegradeReason, RetryPolicy, SensorHealth};
pub use sentinel::{Sentinel, SentinelConfig};
pub use spool::{
    FindingSpool, SpoolCursor, SpoolEntry, SpoolError, SpoolId, SpoolLimits, SpoolResult,
};
