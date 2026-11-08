//! Durchsetzer, Bibliotheksteil (Knoten **AW5-04a**, Ebene **L4**).
//!
//! # Zweck
//! `harw-dod-warden` ist der Bibliotheksteil des einen Prozesses, der
//! Sicherheitsaktionen **tatsächlich ausführt**: cgroups einfrieren und
//! freigeben, Netzzugriff abschneiden, Prozessbäume beenden. Alles andere im
//! DoD-Teilbaum beobachtet, bewertet und schlägt vor; hier wird gehandelt.
//! [`warden::Warden::handle`] ist der Einstiegspunkt für Anfragen: er nimmt
//! eine v2-Anfrage ([`harw_dod_warden_proto::WardenRequest`] mit
//! [`harw_dod_warden_proto::SignedAuthorization`]) entgegen und liefert ein
//! [`warden::WardenOutcome`]; [`warden::Warden::expire_holds`] ist der
//! Hold-Timer, der nur auftaut. Der fälschbare v1-Beleg
//! (`AuthorizationProof`, F-001) wird nirgends mehr angenommen.
//!
//! # PL-100 WP 3.1 (W01 erweitert)
//! - Warden-eigene Policy aus `/etc/harw-dod/warden.toml` ([`config`],
//!   [`policy`], [`trust`]): Aktions-Allowlist, Deny-Liste, `ProofPolicy`,
//!   Escalator-Identität, Schlüsselordner; root-eigen, nicht gruppen- oder
//!   weltschreibbar.
//! - Schlüsselring mit mehreren Key-IDs für die Rotation ([`keys`]),
//!   Schlüsseldateien `0600` root.
//! - Persistenter Zustand im StateDirectory ([`state`]): Nonce-Ledger
//!   ([`nonce_ledger`]), hash-verkettetes Aktions-Ledger ([`action_ledger`]),
//!   Hold-Timer ([`holds`]).
//! - Zielprüfung vor jedem Eingriff ([`observe`]): Kernel-ID unter dem
//!   signierten Pfad; Freeze, Thaw und Kill prüfen sie im Ausführer noch
//!   einmal am geöffneten Verzeichnis und schreiben über denselben
//!   Deskriptor ([`executor`], B21).
//! - Die Deny-Liste schützt auch den User-Manager samt seiner Slices und die
//!   User-Unit `harw-dod-watch.service` ([`policy`]).
//! - Jede Anfrage bekommt eine Antwort ([`warden::WardenOutcome::to_wire`]
//!   ist total).
//!
//! # Abhängigkeitsbudget
//! Das CI-Gate erlaubt dem Warden-Teilbaum höchstens **zwölf** transitive
//! Abhängigkeiten (Proc-Macros und Build-Deps mitgezählt). Diese Crate
//! deklariert die direkten Abhängigkeiten `harw-dod-warden-proto`,
//! `harw-types`, `harw-macros` sowie (PL-100 WP 3.1) `serde`, `serde_json`,
//! `jiff` und `blake3` für die Zustandsdateien und die Hash-Kette — alle
//! vier liegen über `harw-dod-warden-proto` bereits in der Hülle — und
//! `rustix` (`fs`, `process`) für `openat`/`O_NOFOLLOW` und `geteuid`, das
//! das Binary `harw-warden` ohnehin schon mitbringt. Es kommt kein neues
//! Paket in die Warden-Hülle. Für `warden.toml` wird bewusst **keine**
//! `toml`-Crate gezogen (siehe [`config`]).
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
//! # Warum die Autorisierung nachgeprüft wird, statt ihr zu glauben
//! Der Warden läuft privilegiert, an einem systemd-Socket, ohne Netz. Er
//! glaubt der Gegenseite nichts: die Autorisierung kommt ausschließlich
//! durch Deserialisieren empfangener Wire-Bytes herein, und
//! [`warden::Warden::handle`] prüft **jede** Anfrage (MAC, Zeitfenster,
//! Bindung, Stufe, Bestätigung, Präfix, Nonce, eigene Policy, Ziel), bevor
//! irgendetwas ausgeführt wird — siehe `warden.rs`-Moduldoku.
//!
//! # Audit vor jedem Fehlerpfad
//! Siehe `audit.rs`-Moduldoku. Kurzfassung: ein Audit-Eintrag entsteht immer,
//! bevor eine Ausführung versucht wird — nicht erst nach einem Erfolg — und
//! sowohl eine Ablehnung als auch eine fehlgeschlagene Ausführung
//! hinterlassen ihren eigenen Eintrag.
//!
//! # Reversibilität
//! `FreezeCgroup` (über `ThawCgroup`) und `IsolateNetwork` (über
//! `LiftIsolation`) sind reversibel; `KillProcessTree` ist es ausdrücklich
//! **nicht** — siehe `harw_dod_warden_proto::action`-Moduldoku für die
//! Begründung je Aktion. Kill und Isolation verlangen trotzdem beide die
//! signierte Bestätigung `irreversible_ack` (O4).
//! Diese Crate ändert daran nichts; sie führt nur aus, was die Proto-Crate
//! bereits als reversibel/irreversibel klassifiziert.
//!
//! # Was gekapselt ist, und was bewusst nicht gebaut wurde
//! Jede Aktion, die tatsächlich ins System eingreift, ist hinter einem
//! eigenen Trait gekapselt (`executor.rs`):
//! [`executor::CgroupFreezer`], [`executor::CgroupThawer`],
//! [`executor::NetworkIsolator`], [`executor::ProcessTreeKiller`]. Für
//! Freeze, Thaw und Kill liefert diese Crate mit [`executor::CgroupV2Executor`]
//! eine echte Implementierung über cgroup-v2-Kontrolldateien (ohne eigene
//! cgroup-Crate; `rustix::fs::openat` für den Zugriff über den geprüften
//! Verzeichnis-Deskriptor).
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
//! [`warden::Warden`] hält seine vier Ausführer, den Zielbeobachter und sein
//! Audit-Ziel als `Box<dyn Trait + Send + Sync>` — austauschbare,
//! Plugin-artige Konfiguration, kein Leistungspfad — und serialisiert alle
//! Eingriffe über eine Sperre. Die Zustandsdateien bringen eigene `Mutex`
//! mit. [`audit::RecordingAuditSink`] und [`executor::RecordingExecutor`]
//! teilen ihre Aufzeichnung zwischen Klonen (`Arc<Mutex<_>>`). Kein Typ
//! dieser Crate liest die Systemuhr: `now` ist immer Parameter.
//!
//! # Fehler
//! [`error::WardenError`] deckt Ausführungs-, Start- und Zustandsfehler ab
//! (siehe dessen Moduldoku). Nachprüfungsfehler bleiben
//! [`harw_dod_warden_proto::ProofError`] — diese Crate erzeugt keinen
//! eigenen, konkurrierenden Fehlertyp dafür.

#![forbid(unsafe_code)]

pub mod action_ledger;
pub mod audit;
pub mod config;
pub mod error;
pub mod executor;
mod fsutil;
mod hex;
pub mod holds;
pub mod keys;
pub mod nonce_ledger;
pub mod observe;
pub mod policy;
pub mod state;
pub mod trust;
pub mod warden;

pub use action_ledger::{ActionLedger, LedgerEntry, LedgerEvent, LedgerOrigin, LedgerRecord};
pub use audit::{AuditEvent, AuditSink, RecordingAuditSink};
pub use config::{EscalatorIdentity, WardenConfig, load_warden_config, parse_warden_config};
pub use error::{WardenError, WardenResult};
pub use executor::{
    CgroupFreezer, CgroupThawer, CgroupV2Executor, NetworkIsolator, ProcessTreeKiller,
    RecordedCall, RecordingExecutor,
};
pub use fsutil::STATE_RENAME_SYSCALL;
pub use holds::{HoldEntry, HoldStore};
pub use keys::load_key_ring;
pub use nonce_ledger::FileNonceLedger;
pub use observe::{CgroupFsObserver, StaticTargetObserver, TargetObserver};
pub use policy::{ActionAllowlist, DenyList, DenyPattern, PolicyViolation, WardenPolicy};
pub use state::WardenState;
pub use trust::{FileSecrecy, TrustRules};
pub use warden::{HoldSweep, Warden, WardenOutcome, WardenParts};

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
