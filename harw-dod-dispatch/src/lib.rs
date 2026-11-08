//! `harw-dod-dispatch`: reiner Dispatch von DoD-Befunden an Triage-Rollen
//! (PL-100 WP2.1).
//!
//! # Zweck
//! Zwischen dem lesenden DoD-Client und den Triage-Kindern entscheidet diese
//! Crate, **was** mit einem Befund geschieht: deterministischer Hinweis,
//! Rückfrage an den Nutzer, Gruppierung zu einem Vorfall, KI-Triage oder
//! Verwurf mit Zähler. Sie entscheidet nie über einen Eingriff; Verdicts
//! annotieren nur (I1, I3).
//!
//! # Bausteine
//! - [`RouteTable`]: die vollständige Routing-Tabelle (README §3.3) samt den
//!   Regel-Kennungen [`ALL_RULE_IDS`];
//! - [`PartitionedBudget`]: Token-Eimer je `(uid, cgroup_kid, sensor)` plus
//!   globale Grenze mit Reserve für `High`+`Observed`, die keine einzelne
//!   uid allein leeren kann (I10), auch nicht über viele etablierte
//!   Partitionen (ein Vorrechts-Token je uid und Intervall); ein Budget-Trip
//!   meldet sich einmal je Episode;
//! - [`PriorityQueue`]: beschränkt, Neuheit vor Härte vor Schwere vor Alter;
//! - [`BreakerSet`]/[`BreakerPartition`]: Hysterese je Partition, ein Trip je
//!   Auslösung;
//! - [`IncidentGrouper`]: ≥ 2 Regeln auf derselben Cgroup in 600 s, mit fester
//!   Obergrenze der Laufzeit eines Vorfalls; jeder Auftrag eines Vorfalls hat
//!   die Rolle des schwersten Befunds;
//! - [`ReTriagePolicy`]: erneute Triage nur bei echter Änderung;
//! - [`Dispatcher`]: die Fassade mit `offer`, `next_triage`, `complete`,
//!   `resolve_ask`, `snapshot` und `restore`; höchstens ein
//!   [`DispatchAction::Starved`] je Ursache, uid und Fenster, und jede
//!   weggefallene Rückfrage als [`DispatchAction::AskWithdrawn`].
//!
//! # Ring A
//! Rein: kein IO, kein Lesen der Uhr (die Zeit kommt als Unix-Sekunden
//! herein), kein async, keine Threads, keine Panik. Einzige Abhängigkeit
//! ist `serde`, damit der Aufrufer den [`DispatchState`] persistieren kann.
//! Schicht `F` in `xtask/arch-policy.toml`.
//!
//! # Untrusted Text (I4)
//! Triage-Templates ([`TriageJob::template`]) und Hinweise enthalten nur
//! geprüfte Kennungen (UUIDv4, bekannte Regel, 64-Hex-Digest) und
//! Enum-Namen. Sensortext erreicht sie nie; ungültige Kennungen erscheinen
//! als feste Marken ([`INVALID_FINDING_ID`], [`UNKNOWN_RULE`]).
//!
//! # Nebenläufigkeit
//! Alle Typen sind `Send + Sync`; Zustandsänderung nur über `&mut self`.
//!
//! # Fehler
//! [`DispatchError`] für Konfiguration und Zustand; Befunde werden nie mit
//! einem Fehler abgelehnt.
//!
//! # Examples
//! ```
//! use harw_dod_dispatch::{
//!     DispatchAction, DispatchConfig, DispatchFinding, Dispatcher, FindingKind, Hardness,
//!     Severity, TriageFingerprint, RULE_SENSOR_DEGRADED,
//! };
//!
//! let mut dispatcher = Dispatcher::new(DispatchConfig::default())?;
//! // Ein degradierter Sensor geht nie an ein Modell: nur der Hinweis.
//! let finding = DispatchFinding {
//!     finding_id: "3f2c9a10-7b1e-4c2d-9e8f-0a1b2c3d4e5f".to_owned(),
//!     rule_id: RULE_SENSOR_DEGRADED.to_owned(),
//!     severity: Severity::Medium,
//!     hardness: Hardness::Observed,
//!     kind: FindingKind::Anomaly,
//!     dedup_key: "00112233445566778899aabbccddeeff".to_owned(),
//!     cgroup_kid: None,
//!     uid: None,
//!     sensor: "thermal-0".to_owned(),
//!     occurrences: 3,
//!     observed_at_unix: 1_700_000_000,
//!     scope_obfuscated: false,
//!     record_digest: None,
//!     loopback: false,
//!     verified_source: true,
//!     fingerprint: TriageFingerprint::default(),
//! };
//! let actions = dispatcher.offer(finding, 1_700_000_001);
//! assert_eq!(actions.len(), 1);
//! assert!(matches!(actions.first(), Some(DispatchAction::Notify { .. })));
//! assert!(dispatcher.next_triage(1_700_000_002).is_none());
//! # Ok::<(), harw_dod_dispatch::DispatchError>(())
//! ```
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod breaker;
mod budget;
mod dispatcher;
mod error;
mod finding;
mod group;
mod job;
mod queue;
mod retriage;
mod route;
mod serde_map;

pub use breaker::{BreakerConfig, BreakerObservation, BreakerPartition, BreakerSet};
pub use budget::{
    BudgetClass, BudgetConfig, BudgetDecision, BudgetScope, ESTABLISHED_TTL_SECS,
    PartitionedBudget, TOKEN_UNITS,
};
pub use dispatcher::{
    AskAnswer, DispatchAction, DispatchConfig, DispatchCounters, DispatchState, Dispatcher,
    DropCounters, MAX_PENDING_ASKS, MAX_STARVED_EPISODES, StarvedCause,
};
pub use error::DispatchError;
pub use finding::{
    DispatchFinding, DropReason, FindingKind, Hardness, INVALID_FINDING_ID, MAX_SENSOR_ID_LEN,
    PartitionKey, Severity, TriageFingerprint, UNKNOWN_RULE, is_dedup_key, is_record_digest,
    is_sensor_id, is_uuid_v4,
};
pub use group::{GroupInput, Grouping, IncidentGrouper, IncidentId, MAX_ENTRIES_PER_CGROUP};
pub use job::{MAX_JOB_MEMBERS, TEMPLATE_HEADER, TriageJob, TriageJobId, TriageOutcome};
pub use queue::{PriorityQueue, PushOutcome, TriagePriority};
pub use retriage::{KnownFinding, MaterialChange, ReTriagePolicy};
pub use route::{
    ALL_RULE_IDS, RULE_ADVISORY_CORRELATE, RULE_BASELINE_DEVIATION, RULE_DISPATCH_STARVED,
    RULE_EGRESS_FLOW, RULE_EXEC_WRITABLE_PATH, RULE_LISTENER_NEW, RULE_PERSISTENCE_WRITE,
    RULE_SENSOR_DEGRADED, RULE_SPOOL_DEGRADED, RULE_STRUCTURE_DRIFT, Route, RouteTable, TriageRole,
};

#[cfg(test)]
mod dispatcher_tests;
#[cfg(test)]
mod storm_tests;
#[cfg(test)]
mod test_support;
