//! Durchsetzer, Bibliotheksteil (Knoten **AW5-04a**, Ebene **L4**).
//!
//! # Zweck
//! `harw-dod-warden` ist der Bibliotheksteil des einen Prozesses, der
//! Sicherheitsaktionen **tatsächlich ausführt**: cgroups einfrieren und
//! freigeben, Netzzugriff abschneiden, Prozessbäume beenden. Alles andere im
//! DoD-Teilbaum beobachtet, bewertet und schlägt vor; hier wird gehandelt.
//! [`warden::Warden::handle`] ist der einzige Einstiegspunkt: er nimmt eine
//! [`harw_dod_warden_proto::WardenActionRequest`] entgegen und liefert ein
//! [`warden::WardenOutcome`].
//!
//! # Abhängigkeitsbudget
//! Das CI-Gate erlaubt dem Warden-Teilbaum höchstens **zwölf** transitive
//! Abhängigkeiten (Proc-Macros und Build-Deps mitgezählt). Diese Crate
//! deklariert genau drei direkte Abhängigkeiten:
//! `harw-dod-warden-proto`, `harw-types`, `harw-macros`.
//!
//! **Gezählte Zahl und wie gezählt wurde:** `harw-dod-warden-proto`
//! (AW5-02, gelandet) hat in seiner eigenen Moduldoku (`lib.rs`, Abschnitt
//! „Prüfung 2") bereits dokumentiert, dass seine mandatierte Grundausstattung
//! (`harw-types`, `harw-macros`, `serde`, `serde_json`, `jiff`) bei
//! vollständiger, blattgenauer transitiver Zählung über `Cargo.lock`
//! (**ohne** Dev-Dependencies der beteiligten Crates) auf **65** eindeutige
//! Crates kommt — weit über der Obergrenze von zwölf. Diese Crate fügt zu
//! dieser bereits gesprengten Grundausstattung **kein einziges neues
//! Blatt-Crate** hinzu: `harw-types` und `harw-macros` sind in der
//! `harw-dod-warden-proto`-Kette bereits enthalten, und diese Crate selbst
//! zieht `serde`/`serde_json`/`jiff` nicht direkt ein (kein eigener
//! Wire-Code — Serialisierung bleibt vollständig Sache von
//! `harw-dod-warden-proto`). Die einzige zusätzliche Prüfung, die dieser
//! Knoten selbst vorgenommen hat, betraf `rustables` für die
//! Netzisolations-Ausführung (siehe `executor.rs`-Moduldoku, Abschnitt
//! „Warum `NetworkIsolator` keine echte Implementierung hat"): `rustables`
//! taucht in `Cargo.lock` an keiner Stelle auf (`grep -n '^name =
//! "rustables"' Cargo.lock` liefert nichts) — es wurde **nicht**
//! hinzugefügt, gerade weil das Budget bereits gesprengt ist.
//!
//! **Befund, nicht Stillschweigen:** Das eigentliche Gate
//! (`xtask/src/gate_privileges.rs`) existierte laut `harw-dod-warden-proto`s
//! eigener Dokumentation zum Zeitpunkt von dessen Knoten noch nicht als
//! lauffähiger Code. Diese Crate hält sich an dieselbe Disziplin — sie
//! vermeidet jede vermeidbare zusätzliche Abhängigkeit —, kann die
//! geerbte Grundausstattung von `harw-dod-warden-proto` aber nicht rückgängig
//! machen, ohne diesen Knoten neu zu bauen. Ob die Obergrenze von zwölf mit
//! der geerbten Kette überhaupt erreichbar ist, ist ein Befund für das
//! Gate/den Contract-Master, kein Ergebnis, das dieser Knoten stillschweigend
//! herstellen konnte.
//!
//! # Warum der Beleg nachgeprüft wird, statt ihm zu glauben
//! Der Warden läuft privilegiert, an einem systemd-Socket, ohne Netz. Er
//! glaubt der Gegenseite nichts: [`harw_dod_warden_proto::AuthorizationProof`]
//! kommt ausschließlich durch Deserialisieren empfangener Wire-Bytes herein
//! (nie durch einen Aufruf von `AuthorizationProof::new`, den diese Crate
//! nicht anbietet), und [`warden::Warden::handle`] ruft
//! [`harw_dod_warden_proto::AuthorizationProof::verify`] auf **jede**
//! Anfrage an, bevor irgendetwas ausgeführt wird — siehe `warden.rs`-Moduldoku
//! für die vollständige Begründung jeder Prüfung, einschließlich der K43-Falle
//! (ein Vergleichswert, der aus derselben Quelle wie der geprüfte Wert
//! stammt) und wie dieser Knoten sie vermeidet.
//!
//! # Audit vor jedem Fehlerpfad
//! Siehe `audit.rs`-Moduldoku. Kurzfassung: ein Audit-Eintrag entsteht immer,
//! bevor eine Ausführung versucht wird — nicht erst nach einem Erfolg — und
//! sowohl eine Ablehnung als auch eine fehlgeschlagene Ausführung
//! hinterlassen ihren eigenen Eintrag.
//!
//! # Reversibilität
//! `FreezeCgroup`, `ReleaseCgroup` und `IsolateNetwork` sind reversibel;
//! `KillProcessTree` ist es ausdrücklich **nicht** — siehe
//! `harw_dod_warden_proto::action`-Moduldoku für die Begründung je Aktion.
//! Diese Crate ändert daran nichts; sie führt nur aus, was die Proto-Crate
//! bereits als reversibel/irreversibel klassifiziert.
//!
//! # Was gekapselt ist, und was bewusst nicht gebaut wurde
//! Jede Aktion, die tatsächlich ins System eingreift, ist hinter einem
//! eigenen Trait gekapselt (`executor.rs`):
//! [`executor::CgroupFreezer`], [`executor::CgroupReleaser`],
//! [`executor::NetworkIsolator`], [`executor::ProcessTreeKiller`]. Für die
//! ersten drei liefert diese Crate mit [`executor::CgroupV2Executor`] eine
//! echte, dependency-freie Implementierung über cgroup-v2-Kontrolldateien.
//! Für [`executor::NetworkIsolator`] liefert diese Crate **keine** echte
//! Implementierung — siehe `executor.rs`-Moduldoku, Abschnitt „Warum
//! `NetworkIsolator` keine echte Implementierung hat", für die
//! `rustables`-Prüfung und die Begründung. Kein Test dieser Crate erzeugt ein
//! echtes Einfrieren, Töten oder Netzabschneiden — [`executor::RecordingExecutor`]
//! ist die einzige in Tests verwendete Implementierung aller vier Traits.
//!
//! # Inhaltsfreie Antworten
//! Was der Warden nach außen meldet ([`warden::WardenOutcome::to_wire`] →
//! [`harw_dod_warden_proto::WardenResponse`]), trägt nie mehr als
//! [`harw_dod_warden_proto::Denial`] oder einen
//! [`harw_dod_warden_proto::WardenActionAudit`] — beide bereits von
//! `harw-dod-warden-proto` inhaltlich begrenzt. Was diese Crate **intern**
//! über [`audit::AuditSink`] aufzeichnet, verlässt den Vertrauensbereich
//! dagegen nicht und darf deshalb Inhalt tragen (siehe `audit.rs`-Moduldoku).
//!
//! # Nebenläufigkeit
//! [`warden::Warden`] hält seine vier Ausführer und sein Audit-Ziel als
//! `Box<dyn Trait + Send + Sync>` — austauschbare, Plugin-artige
//! Konfiguration, kein Leistungspfad. [`audit::RecordingAuditSink`] und
//! [`executor::RecordingExecutor`] sichern ihren inneren Zustand jeweils
//! über einen `Mutex` ab und sind damit sicher aus mehreren Threads
//! gleichzeitig aufrufbar. Kein Typ dieser Crate liest die Systemuhr.
//!
//! # Fehler
//! [`error::WardenError`] deckt Ausführungsfehler ab (siehe dessen
//! Moduldoku). Nachprüfungsfehler bleiben
//! [`harw_dod_warden_proto::WardenProtoError`] — diese Crate erzeugt keinen
//! eigenen, konkurrierenden Fehlertyp dafür.

#![forbid(unsafe_code)]

pub mod audit;
pub mod error;
pub mod executor;
pub mod warden;

pub use audit::{AuditEvent, AuditSink, RecordingAuditSink};
pub use error::{WardenError, WardenResult};
pub use executor::{
    CgroupFreezer, CgroupReleaser, CgroupV2Executor, NetworkIsolator, ProcessTreeKiller,
    RecordedCall, RecordingExecutor,
};
pub use warden::{Warden, WardenOutcome};
