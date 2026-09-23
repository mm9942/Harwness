//! Netlink-Zugriff hinter einem Trait: das Audit-Backend für `harw-dod-authlog`.
//!
//! # Verantwortungsbereich
//! Diese Crate füllt eine Lücke im ursprünglichen Ausbauprogramm: die
//! Architektur nennt sie, der Arbeitsplan hatte keinen Knoten dafür. Ohne
//! sie hat `harw-dod-authlog` (Knoten AW2-15) nur ein Backend statt zwei —
//! das systemd-Journal, aber nicht das Audit-Subsystem des Kernels.
//!
//! Die Crate hat zwei streng getrennte Teile, und die Trennung ist der Punkt
//! der Aufgabe:
//!
//! - **Formungsteil** ([`record`], sowie das crate-interne Modul `frame` zur
//!   Netlink-Rahmenzerlegung): Audit-Records parsen, Felder extrahieren,
//!   Typen bilden. Reine Funktionen auf Bytes und Zeichenketten. **Braucht
//!   keine Berechtigung und keinen Kernel** — der weitaus größte Teil dieser
//!   Crate läuft in jeder CI, in jedem Entwicklungscontainer, ohne Root.
//! - **Bindungsteil** ([`socket`], nur `#[cfg(target_os = "linux")]`): der
//!   eigentliche `AUDIT`-Netlink-Socket. Klein, hinter [`AuditSource`]
//!   verborgen, in einer eigenen Datei. **Braucht `CAP_AUDIT_READ`
//!   (oder root).**
//!
//! [`FixtureAuditSource`] ist die dritte Implementierung von [`AuditSource`]
//! neben [`socket::NetlinkAuditSource`] — keine Wegwerfattrappe, sondern
//! Teil der normalen API, weil `harw-dod-authlog` sie für seine eigenen
//! Tests braucht.
//!
//! # Warum `auid`, nicht `uid`
//! `uid` ist die aktuelle Ausführungs-UID und wechselt mit jedem
//! Rechtewechsel (`sudo`, Setuid-Programme, …). `auid` — die Anmelde-UID —
//! ist der Wert, den keiner dieser Mechanismen verändert, und damit die
//! einzige Kennung, die beantwortet, wer eine Aktion wirklich ausgelöst hat.
//! Der Kernel-Sentinelwert `4294967295` (`u32::MAX`, `-1` als vorzeichenloses
//! 32-Bit-Wort) bedeutet „keine Anmelde-UID gesetzt" und muss als `None`
//! erscheinen, nicht als diese Zahl — siehe [`AuditFields::auid`] für die
//! ausführliche Begründung; das ist die klassische Fehlerquelle beim
//! Auswerten von `auid`.
//!
//! # Exportierte Typen
//! [`AuditSource`], [`FixtureAuditSource`], [`RawRecord`], [`AuditFields`],
//! die freie Funktion [`parse_record`], sowie [`error::NetlinkError`] /
//! `error::NetlinkResult`. [`socket::NetlinkAuditSource`] ist bewusst
//! **nicht** an der Crate-Wurzel re-exportiert — sie existiert nur unter
//! `#[cfg(target_os = "linux")]`, ein Re-Export an der Wurzel würde diese
//! Bedingung für Aufrufer verschleiern, die stattdessen über den vollen Pfad
//! `harw_dod_netlink::socket::NetlinkAuditSource` sehen, dass es sich um den
//! plattformgebundenen Bindungsteil handelt.
//!
//! # Nebenläufigkeit
//! [`AuditSource`] verlangt `Send + Sync` von jeder Implementierung. Der
//! Formungsteil ([`parse_record`], [`RawRecord`], [`AuditFields`]) besteht
//! aus reinen Werttypen und zustandslosen freien Funktionen ohne innere
//! Veränderlichkeit. Siehe [`socket`] für das Nebenläufigkeitsmodell des
//! Bindungsteils.
//!
//! # Fehler
//! [`error::NetlinkError`] ist der einzige Fehlertyp dieser Crate —
//! inhaltsfrei, siehe dortige Moduldokumentation.
//!
//! # Examples
//! ```rust
//! use harw_dod_netlink::{AuditSource, FixtureAuditSource, RawRecord, parse_record};
//! use std::time::Duration;
//!
//! let source = FixtureAuditSource::new([RawRecord::new(
//!     "type=SYSCALL msg=audit(1699999999.0:1): auid=4294967295 uid=0 success=yes",
//! )]);
//!
//! let records = source.read_records(Duration::from_secs(1))?;
//! let fields = parse_record(&records[0])?;
//!
//! // Der Sentinelwert 4294967295 heißt "keine Anmelde-UID gesetzt".
//! assert_eq!(fields.auid, None);
//! assert_eq!(fields.record_type, "SYSCALL");
//! # Ok::<(), harw_dod_netlink::error::NetlinkError>(())
//! ```

pub mod error;
mod frame;
pub mod record;
#[cfg(target_os = "linux")]
pub mod socket;
pub mod source;
#[cfg(test)]
mod test_support;

pub use error::NetlinkError;
pub use record::{AuditFields, RawRecord, parse_record};
pub use source::{AuditSource, FixtureAuditSource};
