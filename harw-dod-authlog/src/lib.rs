//! Anmeldeereignisse mit `auid`: wer hat sich wann erfolgreich oder
//! erfolglos angemeldet, und unter welcher Anmelde-UID (Contract-Master,
//! Knoten AW2-15).
//!
//! # Verantwortungsbereich
//! Diese Crate liest Anmeldeereignisse aus möglichen Quellen eines
//! Linux-Hosts und stellt sie hinter einem gemeinsamen Trait,
//! [`AuthBackend`], bereit:
//!
//! - **Journal-Backend** (`harw_dod_cap::Capability::ReadJournal`) — **nicht
//!   gebaut**, siehe Abschnitt „Journal-Backend" unten.
//! - **Audit-Backend** ([`AuditBackend`],
//!   `harw_dod_cap::Capability::ReadAuditNetlink`) — liest über die gelandete
//!   Zugriffsschicht `harw-dod-netlink`.
//! - [`FixtureAuthBackend`] — eine deterministische Attrappe, Teil der
//!   normalen API (nicht hinter `#[cfg(test)]`), weil spätere Knoten
//!   (Regeln, Sentinel) sie für ihre eigenen Tests brauchen.
//! - [`AuthlogSensor`] — bindet ein `Box<dyn AuthBackend>` an
//!   `harw_dod_signals::Sensor`, damit `harw-dod-sentinel` (das
//!   ausschließlich `Box<dyn Sensor>`/`Arc<dyn Sensor>` sammelt) diese Crate
//!   überhaupt abrufen kann — siehe `sensor`-Moduldokumentation für die
//!   volle Begründung, insbesondere die `since`/Rückschaufenster-Frage.
//!
//! # Die dokumentierte Ausnahme von „genau eine Quelle je Crate"
//! Jede andere Sensor-Crate des AW0-Ausbauprogramms liest genau eine Quelle
//! (eine `/proc`- oder `/sys`-Datei, einen Netlink-Socket). Diese Crate ist
//! die einzige dokumentierte Ausnahme: sie hält **zwei** Backends hinter
//! einem gemeinsamen Trait. Das ist zulässig, weil beide Backends
//! *dieselbe* Frage — wer hat sich wann erfolgreich oder erfolglos
//! angemeldet? — aus zwei verschiedenen Quellen beantworten, und ein Host im
//! Feld mal die eine, mal die andere Quelle anbietet (ein systemd-Host mit
//! laufendem Journal, oder ein Host, auf dem nur das Kernel-Audit-Subsystem
//! verfügbar ist). Zwei Crates dafür wären zwei Namen für dieselbe Sache —
//! reiner Aufwand ohne Erkenntnisgewinn für einen Konsumenten, der ohnehin
//! nur wissen will, wer sich angemeldet hat.
//!
//! Die Ausnahme bleibt eng: **jedes Backend für sich hält genau eine
//! Fähigkeit** (`AuthBackend::capability` liefert nie mehr als eine
//! `harw_dod_cap::Capability` pro Implementierung). Die Rechtematrix aus
//! Contract-Master Abschnitt F bleibt damit exakt — ein Binary, das nur
//! `Capability::ReadAuditNetlink` gewährt bekommt, kann kein
//! Journal-Backend instanziieren, weil es keins gibt, und umgekehrt bekäme
//! ein reines Journal-Backend nie Zugriff auf den Netlink-Socket. Diese
//! Crate ist die einzige im Programm mit dieser Ausnahme; wer sie hier
//! liest, soll wissen, warum sie hier zulässig ist und nirgendwo sonst.
//!
//! # Journal-Backend: warum es hier nicht existiert
//! Der Auftrag verlangt, das Journal-Backend nur zu bauen, wenn es ohne
//! Subprozess und ohne schwere neue Abhängigkeit geht. Beides trifft hier
//! nicht zu:
//!
//! - **Kein dateibasierter Weg ohne Subprozess.** `journalctl -o json`
//!   liefert zeilenweises JSON, aber nur über einen **Subprozessaufruf** —
//!   das würde die Berechtigungsklasse dieser Crate ändern (von reinem
//!   Dateizugriff zu Prozessausführung) und ist ausdrücklich untersagt.
//! - **Die Journal-Dateien selbst sind kein Text- oder Zeilenformat.**
//!   `/var/log/journal/**/*.journal` ist ein binäres, indiziertes Format
//!   (Hash-Tabellen für Feldsuche, pro Feld optional LZ4/XZ/ZSTD-komprimierte
//!   Nutzlast, `mmap`-basierter wahlfreier Zugriff). Keine der Funktionen
//!   aus `harw-dod-readfs` (`read_to_string`, `read_lines`, `parse_i64`,
//!   `read_key_values`) — allesamt text- bzw. zeilenorientiert — kann dieses
//!   Format lesen; es gibt keinen „einfachen dateibasierten Weg", der ohne
//!   einen Journal-Parser auskäme.
//! - **Ein Parser dafür wäre die schwere Abhängigkeit, vor der der Auftrag
//!   warnt.** Eine vollständige, korrekte Journal-Parser-Implementierung
//!   (Header, Hash-Tabellen, Objekt-Kette, Kompressions-Codecs) von Hand zu
//!   schreiben wäre für den Umfang dieser Crate unverhältnismäßig; eine
//!   fertige Crate dafür einzuziehen bräuchte eine eigene, explizite
//!   Rechtfertigung, die dieser Knoten nicht vorwegnehmen soll.
//!
//! Ergebnis gemäß Auftrag: **kein Journal-Backend.** Diese Crate liefert
//! stattdessen den Trait, das Audit-Backend und [`FixtureAuthBackend`] — ein
//! nach Auftrag zulässiges Ergebnis. Ein künftiger Knoten kann das
//! Journal-Backend nachreichen, sobald entweder ein dateibasierter
//! Journal-Parser als bewusst gewählte, begründete Abhängigkeit vorliegt,
//! oder die Berechtigungsklasse „Subprozess" für eine eigene Crate
//! akzeptiert wird.
//!
//! # Der Punkt der ganzen Crate: `auid`, nicht `uid`
//! `harw_dod_signals::Actor::uid` ist die aktuelle Ausführungs-UID und
//! wechselt mit jedem `sudo`. `harw_dod_signals::Actor::auid` — die
//! Anmelde-UID — ist der Wert, den `sudo` **nicht** verändert, und damit die
//! einzige Kennung, die beantwortet, wer eine Kette wirklich angestoßen hat.
//! Ohne sie sähe jede `sudo`-Aktion aus wie „root war's".
//!
//! Der Kernel-Sentinelwert `4294967295` (`u32::MAX`, `-1` als vorzeichenloses
//! 32-Bit-Wort) bedeutet „keine Anmelde-UID gesetzt" und muss als `None`
//! erscheinen. `harw_dod_netlink::parse_record` behandelt diesen Sonderfall
//! bereits für das Audit-Backend — [`AuditBackend`] wiederholt die Prüfung
//! **nicht** noch einmal, sondern übernimmt `AuditFields::auid` unverändert
//! (siehe dortige Tests zur Verifikation). `auid=0` ist der davon zu
//! unterscheidende echte Fall: root, der sich tatsächlich angemeldet hat,
//! und bleibt `Some(0)`.
//!
//! # Der Inhalt ist heikel
//! Anmeldeereignisse enthalten Benutzernamen, Hostnamen, Terminalnamen und
//! teils fehlgeschlagene Passworteingaben mit Tippfehlern, die dem Passwort
//! ähneln. **Keiner dieser Inhalte darf in eine Fehlermeldung oder ein
//! Metriklabel gelangen.** [`AuthlogError`] trägt deshalb — wie
//! `harw_dod_cap::SensorError`, das es unverändert durchreicht — keinen
//! Feldwert aus einem gelesenen Record; ein kaputter Record ergibt
//! `harw_dod_cap::SensorError::MalformedSource` („unerwartete Form"), nie
//! den Recordinhalt selbst.
//!
//! # Exportierte Typen
//! [`AuthBackend`], [`AuthRecord`], [`AuditBackend`], [`FixtureAuthBackend`],
//! [`AuthlogSensor`], [`sensor::DEFAULT_LOOKBACK`], [`AuthlogError`],
//! [`AuthlogResult`].
//!
//! # Nebenläufigkeit
//! `AuthBackend: Send + Sync` — ein Sentinel darf ein Backend aus einem
//! beliebigen Thread pollen. Alle Typen dieser Crate sind reine Werte oder
//! zustandslose Aufrufe auf eine injizierte Quelle; [`AuditBackend`] und
//! [`AuthlogSensor`] halten keine innere Veränderlichkeit über den Aufruf
//! hinaus.
//!
//! # Fehler
//! [`AuthlogError`] ist der einzige Fehlertyp dieser Crate — inhaltsfrei,
//! siehe dessen Moduldokumentation. [`AuthlogSensor::poll`] entpackt daraus
//! `harw_dod_cap::SensorError`, wie vom `Sensor`-Trait verlangt.
//!
//! # Examples
//! ```rust
//! use harw_dod_authlog::{AuditBackend, AuthBackend};
//! use harw_dod_netlink::{FixtureAuditSource, RawRecord};
//!
//! let source = FixtureAuditSource::new([RawRecord::new(
//!     "type=USER_LOGIN msg=audit(1699999999.000:1): auid=4294967295 uid=0 success=yes",
//! )]);
//! let backend = AuditBackend::new(Box::new(source));
//!
//! let events = backend
//!     .read_events(jiff::Timestamp::UNIX_EPOCH)
//!     .expect("Fixture liefert wohlgeformte Records");
//! // Der Sentinelwert 4294967295 heißt "keine Anmelde-UID gesetzt".
//! assert_eq!(events[0].actor.auid, None);
//! ```

pub mod audit_backend;
pub mod backend;
pub mod error;
pub mod fixture_backend;
pub mod record;
pub mod sensor;

pub use audit_backend::AuditBackend;
pub use backend::AuthBackend;
pub use error::{AuthlogError, AuthlogResult};
pub use fixture_backend::FixtureAuthBackend;
pub use record::AuthRecord;
pub use sensor::AuthlogSensor;
