//! Daten-Vokabular für Host-Sicherheitsbeobachtung: Messwerte, Ereignisse,
//! Belege und der eine `Sensor`-Trait, an dem elf Sensor-Crates gleichzeitig
//! hängen (Contract-Master `docs/aw-contract-master.md` §G, Knoten AW0-07).
//!
//! # Verantwortungsbereich
//! Diese Crate deklariert Vokabular; sie liest keine Quelle selbst und
//! enthält keine Parselogik. Sie besitzt:
//! - [`HostSample`] (Modul [`sample`]) — den langweiligen, häufigen
//!   Messwertstrom.
//! - [`Actor`], [`AuthOutcome`], [`EventKind`], [`SecurityEvent`] (Modul
//!   [`event`]) — den seltenen, wichtigen Ereignisstrom.
//! - [`Hardness`], [`Severity`], [`SecurityEvidence`] (Modul [`evidence`]) —
//!   den zitierfähigen Beleg über eingefrorene Beobachtungen.
//! - [`SensorReading`] und `trait` [`Sensor`] (Modul [`sensor`]) — die
//!   Erzeugungsseite: eine Quelle für Hostbeobachtungen.
//! - [`SecurityVerdict`], [`VerdictClassification`], [`SuggestedResponse`]
//!   (Modul [`verdict`], AW6-02) — der Verdict-Vertrag
//!   `harwness.security-verdict/v1`: wie ein Sicherheitsagent sein Urteil
//!   über einen geprüften Befund zurückgibt, an einen Belegdigest gebunden
//!   und ohne jeden Weg zu einer autorisierten Aktion. Siehe
//!   [`verdict`]-Moduldoku für die vollständige Begründung.
//! - [`error::SignalsError`] (Modul [`error`]) — den einen Fehlertyp dieser
//!   Crate.
//!
//! # Warum `trait Sensor` hier liegt und nicht in `harw-dod-cap`
//! Der Arbeitsplan sah den Trait in `harw-dod-cap` vor. Das geht nicht: der
//! Trait muss [`HostSample`] und [`SecurityEvent`] nennen, und die liegen
//! hier. `harw-dod-signals` hängt seinerseits an `harw-dod-cap`
//! (`ReadScope`, `SensorHandle`, `Capability`, `SensorError`) — der Trait
//! dort hinzulegen hieße einen Zyklus zu bauen. Also: `harw-dod-cap` besitzt
//! das *Zugriffs*vokabular, `harw-dod-signals` das *Daten*vokabular und den
//! Trait, der Daten erzeugt (Contract-Master §F, Abweichungsvermerk).
//!
//! # Wo `Finding<S>` lebt — nicht hier
//! Der Arbeitsplan sah `Finding<S>` mit `pub(crate)`-Konstruktoren in dieser
//! Crate vor. Von hier aus wären die Konstruktoren für die elf
//! Sensor-Crates **unerreichbar** — das Typestate-Muster funktioniert nur,
//! wenn Typ- und Übergangsbesitzer dieselbe Crate sind. Contract-Master
//! §G.1 korrigiert das verbindlich: `Finding<S>` und alle drei
//! Typestate-Übergänge leben in `harw-dod-rules` (Knoten AW4-03). Wer
//! `Finding` sucht, sucht dort — diese Crate exportiert und definiert
//! **kein** `Finding`.
//!
//! # Exportierte Typen
//! [`HostSample`], [`Actor`], [`AuthOutcome`], [`EventKind`],
//! [`SecurityEvent`], [`Hardness`], [`Severity`], [`SecurityEvidence`],
//! [`SensorReading`], [`Sensor`], [`SecurityVerdict`],
//! [`VerdictClassification`], [`SuggestedResponse`], [`parse_verdict`],
//! [`validate_verdict`], [`parse_and_validate_verdict`],
//! [`error::SignalsError`], [`error::SignalsResult`].
//!
//! # Nebenläufigkeit
//! Alle Datentypen sind reine, unveränderliche Werte: `Send + Sync`
//! automatisch, kein internes Locking. `Sensor: Send + Sync` ausdrücklich
//! (siehe Moduldoku [`sensor`]), weil der Sentinel Sensoren hinter `Arc`
//! hält und aus einem Sammelthread aufruft.
//!
//! # Fehler
//! [`error::SignalsError`] — Digestbildung in [`SecurityEvidence::capture`]
//! sowie Parsen/Versionsprüfung/Validierung/Bindung eines
//! [`SecurityVerdict`] (siehe [`verdict`]-Moduldoku). Sensorfehler selbst
//! laufen über [`harw_dod_cap::SensorError`], nicht über diesen Typ (siehe
//! [`error`]-Moduldoku für die Abgrenzung).
//!
//! # Examples
//! ```rust
//! use std::borrow::Cow;
//!
//! use harw_dod_signals::{HostSample, SecurityEvidence};
//! use harw_types::SensorId;
//!
//! let sample = HostSample {
//!     sensor: SensorId::from_str("thermal-0"),
//!     observed_at: jiff::Timestamp::UNIX_EPOCH,
//!     // `Cow`, nicht `&'static str`: ein `&'static str` in einem
//!     // serde-Typ ergibt `impl Deserialize<'static>` -- der Wert wäre nur
//!     // aus einer statischen Quelle lesbar, nie aus einem zur Laufzeit
//!     // gelesenen Puffer (K31).
//!     metric: Cow::Borrowed("temperature_celsius"),
//!     value: 42.5,
//! };
//! let evidence = SecurityEvidence::capture(vec![sample], vec![], jiff::Timestamp::UNIX_EPOCH)
//!     .expect("capture succeeds for well-formed samples");
//! assert_eq!(evidence.samples.len(), 1);
//! ```

pub mod error;
pub mod event;
pub mod evidence;
pub mod sample;
pub mod sensor;
pub mod verdict;

pub use error::{SignalsError, SignalsResult};
pub use event::{DriftSeverity, Actor, AuthOutcome, EventKind, SecurityEvent};
pub use evidence::{Hardness, SecurityEvidence, Severity};
pub use sample::HostSample;
pub use sensor::{Sensor, SensorReading};
pub use verdict::{
    SecurityVerdict, SuggestedResponse, VerdictClassification, parse_and_validate_verdict,
    parse_verdict, validate_verdict,
};
