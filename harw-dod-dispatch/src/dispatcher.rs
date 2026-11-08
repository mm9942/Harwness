//! Die Fassade: [`Dispatcher`] (README §3, WP2.1).
//!
//! # Zweck
//! Nimmt Befunde entgegen ([`Dispatcher::offer`]), entscheidet über Hinweis,
//! Rückfrage, Gruppierung und Triage und gibt Triage-Aufträge in
//! Prioritätsreihenfolge heraus ([`Dispatcher::next_triage`]), höchstens
//! `max_parallel` gleichzeitig. Ein- und Ausgaben sind reine Werte; was
//! daraus wird (TUI-Zeile, Triage-Kind, `dispatch-starved`-Befund),
//! entscheidet der Aufrufer (DodWatch, WP2.2).
//!
//! # Ablauf von [`Dispatcher::offer`]
//! 1. **Hinweis (I3):** Ist die wirksame Schwere `High` oder `Critical`
//!    (nach der Anhebung für verschleierte Scopes), kommt **immer** zuerst
//!    [`DispatchAction::Notify`], unabhängig von Budget, Breaker,
//!    Warteschlange, Route und Prüfung. Ungültige Kennungen erscheinen
//!    darin nur als feste Marke ([`crate::INVALID_FINDING_ID`],
//!    [`crate::UNKNOWN_RULE`]), nie als fremde Zeichenkette.
//! 2. **Prüfung:** UUIDv4, verifizierte Quelle, bekannte Regel, `dedup_key`,
//!    Sensor, Fingerprint, bei KI-Routen der `record_digest`. Ein Verstoß
//!    ergibt [`DispatchAction::Dropped`] und einen Zähler.
//! 3. **Keine KI** ([`Route::NoAi`]): nur der deterministische Hinweis, auch
//!    unterhalb von `High`. `sensor-degraded` und `spool-degraded` setzen
//!    zusätzlich die Markierung „Sensoren degradiert" für
//!    [`DispatchConfig::degraded_mark_secs`] ([`TriageJob::sensors_degraded`]).
//! 4. **Breaker** der Partition: beim Auslösen ein Trip
//!    ([`StarvedCause::Breaker`]); solange offen, `Dropped(BreakerOpen)`.
//! 5. **Re-Triage:** ein bekannter `dedup_key` ohne echte Änderung ergibt
//!    `Dropped(Duplicate)`.
//! 6. **Loopback-Egress (O9):** ein `egress-flow`-Befund mit `loopback`
//!    bekommt statt Triage [`DispatchAction::AskUser`] plus Hinweis; das
//!    kostet ein Budget-Token. Der Dispatcher merkt sich den geprüften Befund
//!    selbst; die Antwort kommt nur mit der Befund-Kennung über
//!    [`Dispatcher::resolve_ask`], bei „normal behandeln" läuft der gemerkte
//!    Befund den Triage-Weg ab Schritt 7. Fällt eine offene Rückfrage weg
//!    (Grenze [`MAX_PENDING_ASKS`] oder abgelaufen), meldet
//!    [`DispatchAction::AskWithdrawn`] das dem Aufrufer. Andere Regeln (auch
//!    `listener-new`) ignorieren `loopback`.
//! 7. **Gruppierung:** zwei oder mehr Regeln auf derselben Cgroup im Fenster
//!    werden ein Vorfall ([`DispatchAction::Grouped`] je Befund). Wartende
//!    Einzelaufträge der Mitglieder gehen im Vorfall-Auftrag auf
//!    ([`DispatchAction::Merged`] je aufgegangenem Auftrag). Ein späteres
//!    Mitglied geht nur in einen Vorfall-Auftrag auf, der **noch wartet** und
//!    Platz hat: nur dann sieht dessen Verdict den Befund. Ist der Auftrag
//!    schon vergeben, abgeschlossen, voll oder gelöst, bekommt das Mitglied
//!    einen eigenen Folgeauftrag für den Vorfall (mit Budget). Ein Verdict
//!    deckt also nie einen Befund ab, den sein Auftrag nicht trug, und ein
//!    Vorfall nimmt höchstens ein Fenster nach seiner Bildung Mitglieder auf.
//!    **Jeder** Auftrag eines Vorfalls, auch ein Folgeauftrag, bekommt die
//!    Rolle des schwersten Befunds des ganzen Vorfalls (README §3.3; Rang wie
//!    in [`IncidentGrouper`]: schwerer, dann härter, dann älter). Ändert ein
//!    neues Mitglied diese Rolle, übernehmen sie alle noch wartenden Aufträge
//!    des Vorfalls; ein schon vergebener behält seine. Ein Vorfall kann so
//!    mehrere Verdicts haben, je eines je Auftrag, alle von derselben Rolle.
//! 8. **Budget** für jeden neuen Auftrag und jede Rückfrage
//!    ([`PartitionedBudget`]), dann **Warteschlange**. Ein erschöpftes
//!    Budget ergibt `Dropped(BudgetExhausted)`; die erste Abweisung einer
//!    Episode (Partition, uid-Reserve, global oder Reserve) ist ein
//!    Budget-Trip. Eine Verdrängung aus der Warteschlange ergibt
//!    `Dropped(QueueOverflow)` für jedes Mitglied des verdrängten Auftrags,
//!    dessen Schlüssel wieder als neu gelten.
//!
//! # Starved (README §3.3: „Breaker- oder Budget-Trip")
//! Jeder Trip zählt ([`DispatchCounters::breaker_trips`],
//! [`DispatchCounters::budget_trips`]), aber je [`StarvedCause`] und uid der
//! auslösenden Partition erscheint höchstens ein [`DispatchAction::Starved`]
//! je `breaker.quiet_secs`: weitere Trips derselben Ursache und uid innerhalb
//! von `breaker.quiet_secs` nach dem letzten **gemeldeten** werden nur
//! gezählt ([`DispatchCounters::starved_suppressed`]). Unterdrückte Trips
//! verlängern das Fenster nicht; ein anhaltender Sturm meldet sich also je
//! `breaker.quiet_secs` erneut (Erinnerung), und der Trip einer anderen uid
//! wird nie verdeckt. So macht ein Angreifer mit vielen Cgroups aus vielen
//! Trips nicht viele `High`-Befunde `dispatch-starved` (B10). Geführt werden
//! höchstens [`MAX_STARVED_EPISODES`] Paare aus Ursache und uid.
//!
//! # Schlüssel und Verdicts
//! Ein Schlüssel wird beim Einreihen (bzw. bei der Rückfrage) gemerkt und
//! gehört dann zu genau einem Auftrag als Mitglied. Scheitert der Auftrag
//! ([`TriageOutcome::Failed`], [`TriageOutcome::Cancelled`]) oder wird er
//! verdrängt, werden alle seine Mitglieder vergessen; ein erneutes Auftreten
//! wird wieder triagiert.
//!
//! # Invariante 8 (I10, B10)
//! Ein Sturm in einer Partition kostet höchstens deren Eimer und löst ihren
//! Breaker aus; `High`+`Observed`-Befunde anderer Partitionen haben die
//! globale Reserve. Eine einzelne uid nimmt daraus höchstens ihre Quote
//! (`uid_reserve_capacity` plus Nachfüllung) und ein Vorrechts-Token je
//! `established_reserve_interval_secs`, gleich über wie viele Cgroups, ob
//! frisch oder wiederverwendet; der Rest bleibt anderen uids. Opfer
//! **derselben** uid schützt die uid-Quote nicht; für sie gilt das Vorrecht
//! etablierter Partitionen ([`PartitionedBudget`], „Etablierte
//! Partitionen"): eine Partition, die mindestens ein Intervall vor Beginn der
//! Reserve-Episode ein Token bekam, darf das eine Vorrechts-Token ihrer uid
//! nehmen, wenn die Quote leer ist. **Grenze:** Hat ein Angreifer derselben
//! uid selbst etablierte Partitionen, konkurriert er um dieses Token, und ein
//! Opfer, dessen Partition nicht etabliert ist, teilt die Quote mit ihm; dann
//! fehlt dem Opfer die KI-Triage, nicht aber der Hinweis. Jeder `High`-Befund
//! bekommt seinen Hinweis unabhängig davon (I3), und der Trip selbst wird zum
//! `High`-Befund `dispatch-starved`.
//!
//! # Persistenz
//! [`Dispatcher::snapshot`] liefert einen serialisierbaren
//! [`DispatchState`], [`Dispatcher::restore`] prüft ihn (Kennungen, Routen,
//! Rückfragen), berechnet die Dringlichkeit jedes Auftrags neu, löst
//! Vorfall-Bindungen an Aufträge, die es nicht (mehr) gibt, und stellt
//! Aufträge, die beim Schnappschuss in Arbeit waren, wieder in die
//! Warteschlange: ihre Triage-Kinder überleben einen Neustart des Aufrufers
//! nicht.
//!
//! # Nebenläufigkeit
//! `&mut self`, keine inneren Sperren, keine Uhr, kein IO. Genau ein
//! Dispatcher je Host (README §4, WP2.2) ist Sache des Aufrufers.
//!
//! # Fehler
//! Nur Konfiguration und Zustand ([`DispatchError`]); Befunde werden nie
//! mit einem Fehler abgelehnt, sondern verworfen und gezählt.
//!
//! # Examples
//! ```
//! use harw_dod_dispatch::{
//!     DispatchAction, DispatchConfig, DispatchFinding, Dispatcher, FindingKind, Hardness,
//!     Severity, TriageFingerprint, TriageOutcome, RULE_EGRESS_FLOW,
//! };
//!
//! let mut dispatcher = Dispatcher::new(DispatchConfig::default())?;
//! let finding = DispatchFinding {
//!     finding_id: "3f2c9a10-7b1e-4c2d-9e8f-0a1b2c3d4e5f".to_owned(),
//!     rule_id: RULE_EGRESS_FLOW.to_owned(),
//!     severity: Severity::High,
//!     hardness: Hardness::Observed,
//!     kind: FindingKind::RuleTriggered,
//!     dedup_key: "00112233445566778899aabbccddeeff".to_owned(),
//!     cgroup_kid: Some(4242),
//!     uid: Some(1000),
//!     sensor: "probe-bpf-0".to_owned(),
//!     occurrences: 1,
//!     observed_at_unix: 1_700_000_000,
//!     scope_obfuscated: false,
//!     record_digest: Some("ab".repeat(32)),
//!     loopback: false,
//!     verified_source: true,
//!     fingerprint: TriageFingerprint::default(),
//! };
//! let actions = dispatcher.offer(finding.clone(), 1_700_000_001);
//! assert!(matches!(actions.first(), Some(DispatchAction::Notify { .. })));
//! assert!(matches!(actions.get(1), Some(DispatchAction::EnqueuedTriage { .. })));
//!
//! // Derselbe Schlüssel ohne Änderung: nur der Hinweis, keine zweite Triage.
//! let again = dispatcher.offer(finding, 1_700_000_002);
//! assert!(matches!(again.get(1), Some(DispatchAction::Dropped { .. })));
//!
//! if let Some(job) = dispatcher.next_triage(1_700_000_003) {
//!     dispatcher.complete(&job.job_id(), TriageOutcome::Verdict, 1_700_000_090);
//! }
//! assert_eq!(dispatcher.in_flight_len(), 0);
//! # Ok::<(), harw_dod_dispatch::DispatchError>(())
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::breaker::{BreakerConfig, BreakerObservation, BreakerSet, BreakerState};
use crate::budget::{
    BudgetClass, BudgetConfig, BudgetDecision, BudgetScope, BudgetState, PartitionedBudget,
};
use crate::error::DispatchError;
use crate::finding::{
    DispatchFinding, DropReason, INVALID_FINDING_ID, PartitionKey, Severity, UNKNOWN_RULE,
    is_dedup_key, is_record_digest, is_sensor_id, is_uuid_v4,
};
use crate::group::{GroupInput, GrouperState, Grouping, IncidentGrouper, IncidentId};
use crate::job::{JobMember, MAX_JOB_MEMBERS, QueuedTriage, TriageJob, TriageJobId, TriageOutcome};
use crate::queue::{PriorityQueue, PushOutcome, QueueState};
use crate::retriage::{KnownKeys, KnownKeysState, ReTriagePolicy};
use crate::route::{
    RULE_EGRESS_FLOW, RULE_SENSOR_DEGRADED, RULE_SPOOL_DEGRADED, Route, RouteTable, TriageRole,
};

/// Höchstzahl offener Rückfragen an den Nutzer ([`Dispatcher::resolve_ask`]).
///
/// # Description
/// Ist die Grenze erreicht, fällt die älteste offene Rückfrage
/// ([`DispatchAction::AskWithdrawn`]); ihr Schlüssel gilt wieder als neu, ein
/// erneutes Auftreten fragt also erneut.
pub const MAX_PENDING_ASKS: usize = 256;

/// Höchstzahl geführter `Starved`-Episoden (Paare aus [`StarvedCause`] und
/// uid, siehe Moduldoku).
///
/// # Description
/// Ist die Grenze erreicht, fallen zuerst abgelaufene Episoden, dann die
/// älteste laufende; deren nächster Trip meldet sich dann früher. Mehr
/// `Starved` als eines je Ursache, uid und Fenster entsteht so nur für einen
/// Angreifer mit mehr uids, als die Tabelle fasst.
pub const MAX_STARVED_EPISODES: usize = 256;

/// Einstellungen des Dispatchers.
///
/// # Description
/// Vorgaben: `max_parallel` 2, Warteschlange 1024, Gruppierungsfenster
/// 600 s, Reserve 25 % ([`BudgetConfig`]), Breaker 60 Befunde je 60 s mit
/// ruhigem Fenster 300 s ([`BreakerConfig`]), Markierung „Sensoren
/// degradiert" 1800 s. Fehlende Felder nehmen beim Deserialisieren die
/// Vorgabe an.
///
/// # Examples
/// ```
/// use harw_dod_dispatch::DispatchConfig;
///
/// let config = DispatchConfig::default();
/// assert_eq!(config.max_parallel, 2);
/// assert_eq!(config.queue_capacity, 1024);
/// assert!(config.validate().is_ok());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DispatchConfig {
    /// Höchstzahl gleichzeitig vergebener Triage-Aufträge.
    pub max_parallel: usize,
    /// Kapazität der Warteschlange.
    pub queue_capacity: usize,
    /// Gruppierungsfenster in Sekunden Ereigniszeit.
    pub group_window_secs: u32,
    /// Höchstzahl der Cgroups im [`IncidentGrouper`].
    pub max_tracked_cgroups: usize,
    /// Nach so vielen Sekunden ohne Auftreten gilt ein `dedup_key` wieder als
    /// neu.
    pub known_key_ttl_secs: u32,
    /// Höchstzahl gespeicherter `dedup_key`s.
    pub max_known_keys: usize,
    /// So viele Sekunden nach dem letzten `sensor-degraded`- oder
    /// `spool-degraded`-Befund tragen vergebene Aufträge die Markierung
    /// „Sensoren degradiert" ([`TriageJob::sensors_degraded`]).
    pub degraded_mark_secs: u32,
    /// Partitioniertes Budget.
    pub budget: BudgetConfig,
    /// Breaker je Partition.
    pub breaker: BreakerConfig,
}

impl Default for DispatchConfig {
    fn default() -> Self {
        Self {
            max_parallel: 2,
            queue_capacity: 1_024,
            group_window_secs: IncidentGrouper::DEFAULT_WINDOW_SECS,
            max_tracked_cgroups: 4_096,
            known_key_ttl_secs: 86_400,
            max_known_keys: 16_384,
            degraded_mark_secs: 1_800,
            budget: BudgetConfig::default(),
            breaker: BreakerConfig::default(),
        }
    }
}

impl DispatchConfig {
    /// Prüft alle Grenzen.
    ///
    /// # Errors
    /// [`DispatchError::InvalidConfig`] mit dem ersten verletzten Feld:
    /// `max_parallel` 1–64, `queue_capacity` 1–65536, `group_window_secs`
    /// 1–86400, `max_tracked_cgroups` 1–1048576, `known_key_ttl_secs`
    /// 1–2592000, `max_known_keys` 1–4194304, `degraded_mark_secs`
    /// 1–604800, dazu die Budget- und Breaker-Grenzen.
    pub fn validate(&self) -> Result<(), DispatchError> {
        let invalid = |field: &'static str, reason: &'static str| {
            Err(DispatchError::InvalidConfig { field, reason })
        };
        if !(1..=64).contains(&self.max_parallel) {
            return invalid("max_parallel", "must be between 1 and 64");
        }
        if !(1..=65_536).contains(&self.queue_capacity) {
            return invalid("queue_capacity", "must be between 1 and 65536");
        }
        if !(1..=86_400).contains(&self.group_window_secs) {
            return invalid("group_window_secs", "must be between 1 and 86400");
        }
        if !(1..=1 << 20).contains(&self.max_tracked_cgroups) {
            return invalid("max_tracked_cgroups", "must be between 1 and 1048576");
        }
        if !(1..=2_592_000).contains(&self.known_key_ttl_secs) {
            return invalid("known_key_ttl_secs", "must be between 1 and 2592000");
        }
        if !(1..=1 << 22).contains(&self.max_known_keys) {
            return invalid("max_known_keys", "must be between 1 and 4194304");
        }
        if !(1..=604_800).contains(&self.degraded_mark_secs) {
            return invalid("degraded_mark_secs", "must be between 1 and 604800");
        }
        self.budget.validate()?;
        self.breaker.validate()
    }
}

/// Was der Aufrufer nach [`Dispatcher::offer`] tun soll.
///
/// # Description
/// Alle Zeichenketten sind geprüfte Kennungen oder feste Marken; keine
/// stammt ungeprüft aus dem Befund. `sensor` in [`PartitionKey`] ist eine
/// geprüfte Sensor-Kennung.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum DispatchAction {
    /// Deterministischer Hinweis (TUI-Systemzeile, Gateway, Mission Control).
    Notify {
        /// Befund-Kennung oder [`crate::INVALID_FINDING_ID`].
        finding_id: String,
        /// Wirksame Schwere.
        severity: Severity,
        /// Kanonische Regel oder [`crate::UNKNOWN_RULE`].
        rule_id: String,
    },
    /// Ein Triage-Auftrag wurde eingereiht.
    EnqueuedTriage {
        /// Kennung des Auftrags.
        job_id: TriageJobId,
        /// Rolle zum Zeitpunkt des Einreihens (maßgeblich ist
        /// [`TriageJob::role`] bei der Vergabe).
        role: TriageRole,
    },
    /// Rückfrage an den Nutzer im TUI-Chat (O9): erlauben / als Low / normal.
    /// Die Antwort geht an [`Dispatcher::resolve_ask`].
    AskUser {
        /// Befund-Kennung.
        finding_id: String,
    },
    /// Eine offene Rückfrage ist weggefallen (Grenze [`MAX_PENDING_ASKS`]
    /// oder nach `known_key_ttl_secs` unbeantwortet); der Aufrufer nimmt sie
    /// aus der TUI. Eine Antwort darauf zählt nur noch als unbekannt, ein
    /// erneutes Auftreten des Befunds fragt erneut.
    AskWithdrawn {
        /// Befund-Kennung der früheren [`DispatchAction::AskUser`].
        finding_id: String,
    },
    /// Der Befund geht nicht (erneut) an ein Modell.
    Dropped {
        /// Befund-Kennung oder [`crate::INVALID_FINDING_ID`].
        finding_id: String,
        /// Grund.
        reason: DropReason,
    },
    /// Breaker- oder Budget-Trip (höchstens einmal je Ursache, uid und
    /// `breaker.quiet_secs`, siehe Moduldoku); der Aufrufer meldet einen
    /// `High`-Befund `dispatch-starved` ([`crate::RULE_DISPATCH_STARVED`]).
    Starved {
        /// Partition des Befunds, der den ersten Trip der Episode ausgelöst
        /// hat. Bei [`StarvedCause::GlobalBudget`] und
        /// [`StarvedCause::ReserveExhausted`] ist sie nicht zwingend der
        /// Verursacher, sondern die erste Partition ohne Triage.
        partition: PartitionKey,
        /// Was ausgelöst hat.
        cause: StarvedCause,
    },
    /// Der Befund gehört zu einem Vorfall.
    Grouped {
        /// Kennung des Vorfalls.
        incident_id: IncidentId,
        /// Befund-Kennung.
        finding_id: String,
    },
    /// Ein wartender Auftrag ist in einem Vorfall-Auftrag aufgegangen:
    /// `from` wird nie vergeben oder abgeschlossen, seine Befunde laufen in
    /// `into` weiter (direkt nach dem `EnqueuedTriage` von `into`).
    Merged {
        /// Der aufgegangene, früher mit `EnqueuedTriage` gemeldete Auftrag.
        from: TriageJobId,
        /// Der Vorfall-Auftrag.
        into: TriageJobId,
    },
}

/// Was einen [`DispatchAction::Starved`] ausgelöst hat.
///
/// # Examples
/// ```
/// use harw_dod_dispatch::{BudgetScope, StarvedCause};
///
/// assert_eq!(StarvedCause::from(BudgetScope::Global), StarvedCause::GlobalBudget);
/// assert_eq!(StarvedCause::from(BudgetScope::Reserve), StarvedCause::ReserveExhausted);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StarvedCause {
    /// Der Breaker der Partition hat ausgelöst.
    Breaker,
    /// Der Budget-Eimer der Partition ist leer.
    PartitionBudget,
    /// Die uid des Befunds hat ihren Anteil an der Reserve aufgebraucht.
    UidReserve,
    /// Das globale Budget ist erschöpft (für `Standard`: bis auf die
    /// Reserve).
    GlobalBudget,
    /// Die Reserve für `High`+`Observed` ist erschöpft (eigene Episode,
    /// unabhängig von [`StarvedCause::GlobalBudget`]).
    ReserveExhausted,
}

impl From<BudgetScope> for StarvedCause {
    fn from(scope: BudgetScope) -> Self {
        match scope {
            BudgetScope::Partition => Self::PartitionBudget,
            BudgetScope::UidReserve => Self::UidReserve,
            BudgetScope::Global => Self::GlobalBudget,
            BudgetScope::Reserve => Self::ReserveExhausted,
        }
    }
}

/// Antwort des Nutzers auf eine [`DispatchAction::AskUser`]-Rückfrage (O9).
///
/// # Description
/// Was aus „erlauben" und „als Low" dauerhaft wird (signiertes Label bzw.
/// Allowlist-Vorschlag), entscheidet der Aufrufer; der Dispatcher sorgt nur
/// dafür, dass danach keine (bzw. bei „normal" genau eine) Triage folgt.
///
/// # Examples
/// ```
/// use harw_dod_dispatch::AskAnswer;
///
/// assert_ne!(AskAnswer::Allow, AskAnswer::TreatNormally);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AskAnswer {
    /// „Erlauben": keine Triage; der Schlüssel bleibt bekannt.
    Allow,
    /// „Als Low behandeln": keine Triage; der Schlüssel bleibt bekannt.
    TreatAsLow,
    /// „Normal behandeln": der Befund geht den normalen Triage-Weg
    /// (Gruppierung, Budget, Warteschlange).
    TreatNormally,
}

/// Verwurfszähler je [`DropReason`].
///
/// # Examples
/// ```
/// use harw_dod_dispatch::{DropCounters, DropReason};
///
/// let counters = DropCounters::default();
/// assert_eq!(counters.get(DropReason::Duplicate), 0);
/// assert_eq!(counters.total(), 0);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DropCounters {
    /// [`DropReason::InvalidFindingId`].
    pub invalid_finding_id: u64,
    /// [`DropReason::UnverifiedSource`].
    pub unverified_source: u64,
    /// [`DropReason::UnknownRule`].
    pub unknown_rule: u64,
    /// [`DropReason::InvalidDedupKey`].
    pub invalid_dedup_key: u64,
    /// [`DropReason::InvalidSensor`].
    pub invalid_sensor: u64,
    /// [`DropReason::InvalidFingerprint`].
    pub invalid_fingerprint: u64,
    /// [`DropReason::MissingRecordDigest`].
    pub missing_record_digest: u64,
    /// [`DropReason::InvalidRecordDigest`].
    pub invalid_record_digest: u64,
    /// [`DropReason::Duplicate`].
    pub duplicate: u64,
    /// [`DropReason::BreakerOpen`].
    pub breaker_open: u64,
    /// [`DropReason::BudgetExhausted`].
    pub budget_exhausted: u64,
    /// [`DropReason::QueueOverflow`].
    pub queue_overflow: u64,
}

impl DropCounters {
    /// Zählerstand eines Grundes.
    ///
    /// # Arguments
    /// - `reason` (`DropReason`): der Grund.
    ///
    /// # Returns
    /// Die Zahl der Verwürfe mit diesem Grund.
    #[must_use]
    pub fn get(&self, reason: DropReason) -> u64 {
        match reason {
            DropReason::InvalidFindingId => self.invalid_finding_id,
            DropReason::UnverifiedSource => self.unverified_source,
            DropReason::UnknownRule => self.unknown_rule,
            DropReason::InvalidDedupKey => self.invalid_dedup_key,
            DropReason::InvalidSensor => self.invalid_sensor,
            DropReason::InvalidFingerprint => self.invalid_fingerprint,
            DropReason::MissingRecordDigest => self.missing_record_digest,
            DropReason::InvalidRecordDigest => self.invalid_record_digest,
            DropReason::Duplicate => self.duplicate,
            DropReason::BreakerOpen => self.breaker_open,
            DropReason::BudgetExhausted => self.budget_exhausted,
            DropReason::QueueOverflow => self.queue_overflow,
        }
    }

    /// Summe aller Verwürfe (sättigend).
    #[must_use]
    pub fn total(&self) -> u64 {
        DropReason::ALL
            .iter()
            .fold(0_u64, |sum, reason| sum.saturating_add(self.get(*reason)))
    }

    // Erhöht den Zähler eines Grundes.
    fn bump(&mut self, reason: DropReason) {
        let counter = match reason {
            DropReason::InvalidFindingId => &mut self.invalid_finding_id,
            DropReason::UnverifiedSource => &mut self.unverified_source,
            DropReason::UnknownRule => &mut self.unknown_rule,
            DropReason::InvalidDedupKey => &mut self.invalid_dedup_key,
            DropReason::InvalidSensor => &mut self.invalid_sensor,
            DropReason::InvalidFingerprint => &mut self.invalid_fingerprint,
            DropReason::MissingRecordDigest => &mut self.missing_record_digest,
            DropReason::InvalidRecordDigest => &mut self.invalid_record_digest,
            DropReason::Duplicate => &mut self.duplicate,
            DropReason::BreakerOpen => &mut self.breaker_open,
            DropReason::BudgetExhausted => &mut self.budget_exhausted,
            DropReason::QueueOverflow => &mut self.queue_overflow,
        };
        *counter = counter.saturating_add(1);
    }
}

/// Zähler des Dispatchers (für Doctor, Mission Control und Metriken).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DispatchCounters {
    /// Verwürfe je Grund.
    pub dropped: DropCounters,
    /// Ausgegebene Hinweise.
    pub notified: u64,
    /// Eingereihte Aufträge.
    pub enqueued: u64,
    /// [`DispatchAction::Grouped`]-Meldungen.
    pub grouped: u64,
    /// Rückfragen an den Nutzer.
    pub asked_user: u64,
    /// Breaker-Auslösungen.
    pub breaker_trips: u64,
    /// Budget-Trips (erste Abweisung einer Episode).
    pub budget_trips: u64,
    /// Vergebene Aufträge.
    pub dispatched: u64,
    /// Aufträge mit Verdict.
    pub completed: u64,
    /// Fehlgeschlagene oder abgebrochene Aufträge.
    pub failed: u64,
    /// [`Dispatcher::complete`] für einen unbekannten Auftrag.
    pub unknown_completions: u64,
    /// Trips, deren `Starved` unterdrückt wurde (gleiche Ursache und uid,
    /// laufendes Fenster).
    pub starved_suppressed: u64,
    /// [`DispatchAction::Merged`]-Meldungen.
    pub merged: u64,
    /// Beantwortete Rückfragen.
    pub asks_answered: u64,
    /// [`Dispatcher::resolve_ask`] ohne passende offene Rückfrage.
    pub unknown_ask_answers: u64,
    /// [`DispatchAction::AskWithdrawn`]-Meldungen.
    pub asks_withdrawn: u64,
}

// Eine offene Rückfrage an den Nutzer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PendingAsk {
    // Der gefragte, geprüfte Befund; „normal behandeln" triagiert genau ihn,
    // nicht das, was der Aufrufer bei der Antwort mitgibt.
    finding: DispatchFinding,
    // Zeitpunkt der Rückfrage.
    asked_at_unix: i64,
}

// Ein vergebener Auftrag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct InFlight {
    // Der Auftrag.
    job: QueuedTriage,
    // Zeitpunkt der Vergabe.
    dispatched_at_unix: i64,
}

/// Persistierbarer Zustand eines [`Dispatcher`].
///
/// # Description
/// Undurchsichtig bis auf Version und Zähler; serialisierbar mit jedem
/// Serde-Format. [`Dispatcher::restore`] prüft ihn vollständig, ein
/// manipulierter Zustand kann also keine ungeprüfte Zeichenkette in ein
/// Template bringen.
///
/// # Examples
/// ```
/// use harw_dod_dispatch::{DispatchConfig, DispatchState, Dispatcher};
///
/// let state = Dispatcher::new(DispatchConfig::default())?.snapshot();
/// assert_eq!(state.version(), DispatchState::VERSION);
/// let restored = Dispatcher::restore(DispatchConfig::default(), state)?;
/// assert_eq!(restored.queue_len(), 0);
/// # Ok::<(), harw_dod_dispatch::DispatchError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchState {
    // Formatversion.
    version: u32,
    // Nächste Auftragsnummer.
    next_job: u64,
    // Warteschlange.
    queue: QueueState<QueuedTriage>,
    // Vergebene Aufträge.
    in_flight: Vec<InFlight>,
    // Bekannte Schlüssel.
    known: KnownKeysState,
    // Vorfälle.
    grouper: GrouperState,
    // Budget.
    budget: BudgetState,
    // Breaker.
    breakers: BreakerState,
    // Zähler.
    counters: DispatchCounters,
    // Offene Rückfragen nach Befund-Kennung.
    #[serde(with = "crate::serde_map")]
    pending_asks: BTreeMap<String, PendingAsk>,
    // Letzter gemeldeter Trip je Ursache und uid (Starved-Episoden).
    #[serde(with = "crate::serde_map")]
    starved_episodes: BTreeMap<StarvedEpisodeKey, i64>,
    // Ende der Markierung „Sensoren degradiert".
    degraded_until_unix: Option<i64>,
}

// Schlüssel einer Starved-Episode: Ursache und uid der auslösenden Partition.
type StarvedEpisodeKey = (StarvedCause, Option<u32>);

impl DispatchState {
    /// Formatversion, die diese Crate schreibt und liest.
    pub const VERSION: u32 = 1;

    /// Formatversion dieses Zustands.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Zähler zum Zeitpunkt des Schnappschusses.
    #[must_use]
    pub fn counters(&self) -> &DispatchCounters {
        &self.counters
    }
}

/// Der Dispatcher (siehe Moduldoku).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dispatcher {
    // Einstellungen.
    config: DispatchConfig,
    // Routing-Tabelle.
    routes: RouteTable,
    // Re-Triage-Regel.
    policy: ReTriagePolicy,
    // Wartende Aufträge.
    queue: PriorityQueue<QueuedTriage>,
    // Vergebene Aufträge.
    in_flight: Vec<InFlight>,
    // Bekannte Schlüssel.
    known: KnownKeys,
    // Vorfälle.
    grouper: IncidentGrouper,
    // Budget.
    budget: PartitionedBudget,
    // Breaker.
    breakers: BreakerSet,
    // Zähler.
    counters: DispatchCounters,
    // Nächste Auftragsnummer (ab 1).
    next_job: u64,
    // Offene Rückfragen nach Befund-Kennung (höchstens `MAX_PENDING_ASKS`).
    pending_asks: BTreeMap<String, PendingAsk>,
    // Letzter gemeldeter Trip je Ursache und uid (höchstens
    // `MAX_STARVED_EPISODES`).
    starved_episodes: BTreeMap<StarvedEpisodeKey, i64>,
    // Ende der Markierung „Sensoren degradiert".
    degraded_until_unix: Option<i64>,
}

// Ein geprüfter Befund auf einer KI-Route.
struct Admitted<'a> {
    // Der Befund.
    finding: &'a DispatchFinding,
    // Seine Partition.
    partition: PartitionKey,
    // Wirksame Schwere.
    severity: Severity,
    // Zuständige Rolle.
    role: TriageRole,
    // Erstes Auftreten des Schlüssels.
    novel: bool,
    // Erneute Triage eines bekannten Schlüssels.
    retriage: bool,
}

impl Dispatcher {
    /// Neuer Dispatcher ohne Zustand.
    ///
    /// # Arguments
    /// - `config` (`DispatchConfig`): Einstellungen.
    ///
    /// # Errors
    /// [`DispatchError::InvalidConfig`] aus [`DispatchConfig::validate`].
    pub fn new(config: DispatchConfig) -> Result<Self, DispatchError> {
        config.validate()?;
        Ok(Self {
            config,
            routes: RouteTable::new(),
            policy: ReTriagePolicy::new(),
            queue: PriorityQueue::new(config.queue_capacity),
            in_flight: Vec::new(),
            known: KnownKeys::new(config.known_key_ttl_secs, config.max_known_keys),
            grouper: IncidentGrouper::new(config.group_window_secs, config.max_tracked_cgroups),
            budget: PartitionedBudget::new(config.budget),
            breakers: BreakerSet::new(config.breaker),
            counters: DispatchCounters::default(),
            next_job: 1,
            pending_asks: BTreeMap::new(),
            starved_episodes: BTreeMap::new(),
            degraded_until_unix: None,
        })
    }

    /// Die Einstellungen.
    #[must_use]
    pub fn config(&self) -> &DispatchConfig {
        &self.config
    }

    /// Die Zähler.
    #[must_use]
    pub fn counters(&self) -> &DispatchCounters {
        &self.counters
    }

    /// Anzahl wartender Aufträge.
    #[must_use]
    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }

    /// Anzahl vergebener, noch nicht abgeschlossener Aufträge.
    #[must_use]
    pub fn in_flight_len(&self) -> usize {
        self.in_flight.len()
    }

    /// Das Budget (nur lesend, z. B. für Doctor und Mission Control).
    #[must_use]
    pub fn budget(&self) -> &PartitionedBudget {
        &self.budget
    }

    /// Die Breaker (nur lesend, z. B. für Doctor und Mission Control).
    #[must_use]
    pub fn breakers(&self) -> &BreakerSet {
        &self.breakers
    }

    /// Anzahl offener Rückfragen an den Nutzer.
    #[must_use]
    pub fn pending_asks_len(&self) -> usize {
        self.pending_asks.len()
    }

    /// Ob die Markierung „Sensoren degradiert" gilt.
    ///
    /// # Arguments
    /// - `now_unix` (`i64`): aktuelle Zeit in Unix-Sekunden.
    ///
    /// # Returns
    /// `true` bis einschließlich `degraded_mark_secs` nach dem letzten
    /// gültigen `sensor-degraded`- oder `spool-degraded`-Befund.
    #[must_use]
    pub fn sensors_degraded(&self, now_unix: i64) -> bool {
        self.degraded_until_unix
            .is_some_and(|until| now_unix <= until)
    }

    /// Zeitpunkt, zu dem ein Auftrag vergeben wurde.
    ///
    /// # Description
    /// Ab hier läuft das Zeitbudget des Triage-Kinds; die Wartezeit in der
    /// Warteschlange zählt nicht mit (README §4, WP2.2).
    ///
    /// # Arguments
    /// - `job` (`&TriageJobId`): der Auftrag.
    ///
    /// # Returns
    /// `None`, wenn der Auftrag nicht vergeben ist.
    #[must_use]
    pub fn dispatched_at(&self, job: &TriageJobId) -> Option<i64> {
        self.in_flight
            .iter()
            .find(|flight| flight.job.job_id == *job)
            .map(|flight| flight.dispatched_at_unix)
    }

    /// Nimmt einen Befund entgegen (Ablauf siehe Moduldoku).
    ///
    /// # Arguments
    /// - `finding` (`DispatchFinding`): der Befund (untrusted).
    /// - `now_unix` (`i64`): aktuelle Zeit in Unix-Sekunden.
    ///
    /// # Returns
    /// Die Aktionen in dieser Reihenfolge: ein etwaiger
    /// [`DispatchAction::Notify`] zuerst, dann etwaige
    /// [`DispatchAction::Starved`], dann etwaige
    /// [`DispatchAction::AskWithdrawn`] (vor einer neuen Rückfrage mit
    /// derselben Kennung), dann alles Übrige.
    pub fn offer(&mut self, finding: DispatchFinding, now_unix: i64) -> Vec<DispatchAction> {
        self.known.prune(now_unix);
        let mut withdrawn = self.prune_asks(now_unix);
        let mut actions = Vec::new();
        self.offer_into(&mut actions, &mut withdrawn, &finding, now_unix);
        self.withdraw_asks(&mut actions, withdrawn);
        actions
    }

    // Ablauf von `offer` (siehe Moduldoku); weggefallene Rückfragen landen in
    // `withdrawn` und werden von `offer` eingeordnet.
    fn offer_into(
        &mut self,
        actions: &mut Vec<DispatchAction>,
        withdrawn: &mut Vec<String>,
        finding: &DispatchFinding,
        now_unix: i64,
    ) {
        let severity = finding.effective_severity();
        let route = self.routes.lookup(&finding.rule_id);
        let shown_id = if is_uuid_v4(&finding.finding_id) {
            finding.finding_id.clone()
        } else {
            INVALID_FINDING_ID.to_owned()
        };
        let shown_rule = self
            .routes
            .canonical_rule_id(&finding.rule_id)
            .unwrap_or(UNKNOWN_RULE);
        let mut notified = false;
        if severity.requires_notice() {
            self.notify(actions, &shown_id, severity, shown_rule);
            notified = true;
        }
        let route = match admit(finding, route) {
            Ok(route) => route,
            Err(reason) => {
                self.drop_finding(actions, shown_id, reason);
                return;
            }
        };
        let role = match route {
            Route::NoAi => {
                if !notified {
                    self.notify(actions, &shown_id, severity, shown_rule);
                }
                if shown_rule == RULE_SENSOR_DEGRADED || shown_rule == RULE_SPOOL_DEGRADED {
                    self.mark_degraded(now_unix);
                }
                return;
            }
            Route::Triage { role, .. } => role,
        };
        let partition = finding.partition();
        match self.breakers.observe(&partition, now_unix) {
            BreakerObservation::Closed => {}
            BreakerObservation::Tripped => {
                self.counters.breaker_trips = self.counters.breaker_trips.saturating_add(1);
                self.report_starved(actions, partition, StarvedCause::Breaker, now_unix);
                self.drop_finding(actions, shown_id, DropReason::BreakerOpen);
                return;
            }
            BreakerObservation::Open => {
                self.drop_finding(actions, shown_id, DropReason::BreakerOpen);
                return;
            }
        }
        // `None` = bekannter Schlüssel ohne echte Änderung.
        let freshness = match self.known.get(&finding.dedup_key) {
            None => Some((true, false)),
            Some(stored) => self
                .policy
                .is_material_change(stored, finding.hardness, &finding.fingerprint)
                .then_some((false, true)),
        };
        let Some((novel, retriage)) = freshness else {
            self.known.touch(&finding.dedup_key, now_unix);
            self.drop_finding(actions, shown_id, DropReason::Duplicate);
            return;
        };
        let admitted = Admitted {
            finding,
            partition,
            severity,
            role,
            novel,
            retriage,
        };
        // O9 gilt nur für Loopback-Egress, nicht für `listener-new`.
        if finding.loopback && shown_rule == RULE_EGRESS_FLOW {
            self.ask_user(actions, withdrawn, &admitted, notified, now_unix);
        } else {
            self.route_to_triage(actions, &admitted, now_unix);
        }
    }

    /// Gibt den dringendsten wartenden Auftrag heraus.
    ///
    /// # Arguments
    /// - `now_unix` (`i64`): aktuelle Zeit in Unix-Sekunden.
    ///
    /// # Returns
    /// `None`, wenn schon `max_parallel` Aufträge vergeben sind oder nichts
    /// wartet.
    pub fn next_triage(&mut self, now_unix: i64) -> Option<TriageJob> {
        if self.in_flight.len() >= self.config.max_parallel {
            return None;
        }
        let degraded = self.sensors_degraded(now_unix);
        while let Some(queued) = self.queue.pop() {
            if let Some(job) = queued.to_job(&self.routes, degraded) {
                self.in_flight.push(InFlight {
                    job: queued,
                    dispatched_at_unix: now_unix,
                });
                self.counters.dispatched = self.counters.dispatched.saturating_add(1);
                return Some(job);
            }
            // Ohne Mitglied oder Route nicht vergebbar (nach `restore`
            // ausgeschlossen); die Schlüssel werden wieder neu.
            self.forget_members(&queued);
        }
        None
    }

    /// Schließt einen vergebenen Auftrag ab.
    ///
    /// # Description
    /// Der Auftrag wird in jedem Fall von seinem Vorfall gelöst: ein weiteres
    /// Mitglied bekommt dann einen neuen Auftrag. Bei
    /// [`TriageOutcome::Failed`] und [`TriageOutcome::Cancelled`] werden
    /// zusätzlich die Schlüssel aller Mitglieder vergessen: ein erneutes
    /// Auftreten wird wieder triagiert. Ein unbekannter Auftrag wird nur
    /// gezählt.
    ///
    /// # Arguments
    /// - `job` (`&TriageJobId`): der Auftrag.
    /// - `outcome` (`TriageOutcome`): sein Ausgang.
    /// - `now_unix` (`i64`): aktuelle Zeit in Unix-Sekunden.
    pub fn complete(&mut self, job: &TriageJobId, outcome: TriageOutcome, now_unix: i64) {
        let (done, rest): (Vec<InFlight>, Vec<InFlight>) = std::mem::take(&mut self.in_flight)
            .into_iter()
            .partition(|flight| flight.job.job_id == *job);
        self.in_flight = rest;
        if done.is_empty() {
            self.counters.unknown_completions = self.counters.unknown_completions.saturating_add(1);
            return;
        }
        for flight in done {
            self.grouper.unbind_job(flight.job.job_id);
            match outcome {
                TriageOutcome::Verdict => {
                    self.counters.completed = self.counters.completed.saturating_add(1);
                    for member in &flight.job.members {
                        self.known.touch(&member.dedup_key, now_unix);
                    }
                }
                TriageOutcome::Failed | TriageOutcome::Cancelled => {
                    self.counters.failed = self.counters.failed.saturating_add(1);
                    self.forget_members(&flight.job);
                }
            }
        }
    }

    /// Nimmt die Antwort des Nutzers auf eine Rückfrage entgegen (O9).
    ///
    /// # Description
    /// Nur eine offene Rückfrage lässt sich beantworten, und nur über die
    /// Kennung, unter der sie gestellt wurde; sonst wird nur
    /// [`DispatchCounters::unknown_ask_answers`] gezählt. Jede Rückfrage
    /// lässt sich genau einmal beantworten. Maßgeblich ist der Befund, den
    /// der Dispatcher bei der Rückfrage gemerkt hat (Digest, Schwere, Härte,
    /// Partition, Verschleierung), nie ein vom Aufrufer neu gelieferter.
    /// - [`AskAnswer::Allow`], [`AskAnswer::TreatAsLow`]: keine Triage, der
    ///   Schlüssel bleibt bekannt (dieselbe Frage kommt nicht wieder).
    /// - [`AskAnswer::TreatNormally`]: der gemerkte Befund wird erneut
    ///   geprüft und geht den Triage-Weg (Gruppierung, Budget, Warteschlange)
    ///   wie ein neuer Schlüssel; der Breaker hat ihn schon bei
    ///   [`Dispatcher::offer`] gezählt. Den Hinweis gab es ebenfalls schon
    ///   dort.
    ///
    /// # Arguments
    /// - `finding_id` (`&str`): Kennung aus der [`DispatchAction::AskUser`]
    ///   (untrusted; nur Schlüssel der Suche).
    /// - `answer` (`AskAnswer`): die Antwort.
    /// - `now_unix` (`i64`): aktuelle Zeit in Unix-Sekunden.
    ///
    /// # Returns
    /// Etwaige [`DispatchAction::AskWithdrawn`] für inzwischen abgelaufene
    /// Rückfragen (nach etwaigen `Starved`), dazu bei
    /// [`AskAnswer::TreatNormally`] die Aktionen des Triage-Wegs
    /// (`EnqueuedTriage`, `Grouped`, `Merged`, `Starved`, `Dropped`).
    pub fn resolve_ask(
        &mut self,
        finding_id: &str,
        answer: AskAnswer,
        now_unix: i64,
    ) -> Vec<DispatchAction> {
        self.known.prune(now_unix);
        let withdrawn = self.prune_asks(now_unix);
        let mut actions = Vec::new();
        match self.pending_asks.remove(finding_id) {
            Some(ask) => {
                self.counters.asks_answered = self.counters.asks_answered.saturating_add(1);
                self.answer_ask(&mut actions, &ask.finding, answer, now_unix);
            }
            None => {
                self.counters.unknown_ask_answers =
                    self.counters.unknown_ask_answers.saturating_add(1);
            }
        }
        self.withdraw_asks(&mut actions, withdrawn);
        actions
    }

    // Wendet eine Antwort auf den bei der Rückfrage gemerkten Befund an.
    fn answer_ask(
        &mut self,
        actions: &mut Vec<DispatchAction>,
        finding: &DispatchFinding,
        answer: AskAnswer,
        now_unix: i64,
    ) {
        match answer {
            AskAnswer::Allow | AskAnswer::TreatAsLow => {
                self.known.touch(&finding.dedup_key, now_unix);
            }
            AskAnswer::TreatNormally => {
                self.known.forget(&finding.dedup_key);
                let role = match admit(finding, self.routes.lookup(&finding.rule_id)) {
                    Ok(Route::Triage { role, .. }) => role,
                    // Ausgeschlossen: gefragt wird nur auf der Egress-Route.
                    Ok(Route::NoAi) => return,
                    Err(reason) => {
                        // Ausgeschlossen (gemerkt wird nur ein geprüfter
                        // Befund, `restore` prüft erneut); die Kennung ist
                        // dann eine geprüfte UUIDv4.
                        self.drop_finding(actions, finding.finding_id.clone(), reason);
                        return;
                    }
                };
                let admitted = Admitted {
                    finding,
                    partition: finding.partition(),
                    severity: finding.effective_severity(),
                    role,
                    novel: true,
                    retriage: false,
                };
                self.route_to_triage(actions, &admitted, now_unix);
            }
        }
    }

    /// Schnappschuss des gesamten Zustands.
    ///
    /// # Returns
    /// Einen serialisierbaren [`DispatchState`].
    #[must_use]
    pub fn snapshot(&self) -> DispatchState {
        DispatchState {
            version: DispatchState::VERSION,
            next_job: self.next_job,
            queue: self.queue.export(),
            in_flight: self.in_flight.clone(),
            known: self.known.export(),
            grouper: self.grouper.export(),
            budget: self.budget.export(),
            breakers: self.breakers.export(),
            counters: self.counters.clone(),
            pending_asks: self.pending_asks.clone(),
            starved_episodes: self.starved_episodes.clone(),
            degraded_until_unix: self.degraded_until_unix,
        }
    }

    /// Stellt einen Dispatcher aus einem Schnappschuss wieder her.
    ///
    /// # Description
    /// Die Einstellungen kommen aus `config`, nicht aus dem Zustand. Jeder
    /// Auftrag, jeder Schlüssel, jede Rückfrage und jede Partition wird gegen
    /// die Grammatiken und die Routing-Tabelle geprüft; die Dringlichkeit
    /// jedes Auftrags wird aus seinen Mitgliedern neu berechnet, nie
    /// übernommen. Aufträge, die beim Schnappschuss in Arbeit waren, kommen
    /// zurück in die Warteschlange; was dort nicht mehr passt, wird wie ein
    /// Überlauf verdrängt. Eine Vorfall-Bindung an einen Auftrag, den es
    /// danach nicht gibt oder der zu einem anderen Vorfall gehört, wird
    /// gelöst (das nächste Mitglied bekommt einen neuen Auftrag).
    ///
    /// # Arguments
    /// - `config` (`DispatchConfig`): Einstellungen.
    /// - `state` (`DispatchState`): ein Schnappschuss.
    ///
    /// # Errors
    /// - [`DispatchError::InvalidConfig`] aus [`DispatchConfig::validate`];
    /// - [`DispatchError::UnsupportedStateVersion`] bei fremder Version;
    /// - [`DispatchError::InvalidState`] bei ungültigem Inhalt.
    pub fn restore(config: DispatchConfig, state: DispatchState) -> Result<Self, DispatchError> {
        config.validate()?;
        let DispatchState {
            version,
            next_job,
            mut queue,
            in_flight,
            known,
            grouper,
            budget,
            breakers,
            counters,
            pending_asks,
            starved_episodes,
            degraded_until_unix,
        } = state;
        if version != DispatchState::VERSION {
            return Err(DispatchError::UnsupportedStateVersion {
                found: version,
                supported: DispatchState::VERSION,
            });
        }
        if next_job < 1 {
            return Err(DispatchError::InvalidState { what: "next job" });
        }
        let routes = RouteTable::new();
        let grouper = IncidentGrouper::import(
            config.group_window_secs,
            config.max_tracked_cgroups,
            grouper,
            &routes,
        )?;
        let next_incident = grouper.next_incident();
        let jobs_ok = queue
            .entries
            .iter()
            .map(|entry| &entry.item)
            .chain(in_flight.iter().map(|flight| &flight.job))
            .all(|job| job.is_well_formed(&routes, next_job, next_incident));
        if !jobs_ok {
            return Err(DispatchError::InvalidState {
                what: "triage jobs",
            });
        }
        let mut job_ids: Vec<u64> = queue
            .entries
            .iter()
            .map(|entry| entry.item.job_id.as_u64())
            .chain(in_flight.iter().map(|flight| flight.job.job_id.as_u64()))
            .collect();
        let total = job_ids.len();
        job_ids.sort_unstable();
        job_ids.dedup();
        if job_ids.len() != total {
            return Err(DispatchError::InvalidState {
                what: "duplicate job ids",
            });
        }
        // Die Dringlichkeit folgt allein aus dem Auftrag; eine gespeicherte
        // (womöglich manipulierte) Priorität zählt nicht.
        for entry in &mut queue.entries {
            entry.priority = entry.item.priority().ok_or(DispatchError::InvalidState {
                what: "triage jobs",
            })?;
        }
        // Jede Rückfrage trägt einen Befund, der die Prüfung von `offer`
        // besteht und wirklich gefragt worden wäre (Loopback-Egress).
        let asks_ok = pending_asks.len() <= MAX_PENDING_ASKS
            && pending_asks.iter().all(|(finding_id, ask)| {
                let finding = &ask.finding;
                *finding_id == finding.finding_id
                    && finding.loopback
                    && routes.canonical_rule_id(&finding.rule_id) == Some(RULE_EGRESS_FLOW)
                    && admit(finding, routes.lookup(&finding.rule_id)).is_ok_and(Route::is_ai)
            });
        if !asks_ok {
            return Err(DispatchError::InvalidState {
                what: "pending asks",
            });
        }
        if starved_episodes.len() > MAX_STARVED_EPISODES {
            return Err(DispatchError::InvalidState {
                what: "starved episodes",
            });
        }
        let (queue, overflow) = PriorityQueue::import(config.queue_capacity, queue);
        let mut dispatcher = Self {
            config,
            routes,
            policy: ReTriagePolicy::new(),
            queue,
            in_flight: Vec::new(),
            known: KnownKeys::import(config.known_key_ttl_secs, config.max_known_keys, known)?,
            grouper,
            budget: PartitionedBudget::import(config.budget, budget)?,
            breakers: BreakerSet::import(config.breaker, breakers)?,
            counters,
            next_job,
            pending_asks,
            starved_episodes,
            degraded_until_unix,
        };
        // Beim Wiederherstellen gibt es keinen Empfänger für Aktionen; die
        // Zähler laufen trotzdem.
        let mut unreported = Vec::new();
        for job in overflow {
            dispatcher.report_overflow(&mut unreported, &job);
        }
        for flight in in_flight {
            // Eine Verdrängung zählt wie ein Überlauf.
            dispatcher.push_queued(&mut unreported, flight.job);
        }
        // Nach dem Neustart ist jeder lebende Auftrag in der Warteschlange.
        let live: Vec<(TriageJobId, Option<IncidentId>)> = dispatcher
            .queue
            .iter()
            .map(|job| (job.job_id, job.incident_id))
            .collect();
        dispatcher.grouper.retain_bindings(|incident, job| {
            live.iter()
                .any(|(id, bound)| *id == job && *bound == Some(incident))
        });
        Ok(dispatcher)
    }

    // Hinweis ausgeben und zählen.
    fn notify(
        &mut self,
        actions: &mut Vec<DispatchAction>,
        finding_id: &str,
        severity: Severity,
        rule_id: &str,
    ) {
        self.counters.notified = self.counters.notified.saturating_add(1);
        actions.push(DispatchAction::Notify {
            finding_id: finding_id.to_owned(),
            severity,
            rule_id: rule_id.to_owned(),
        });
    }

    // Verwurf ausgeben und zählen.
    fn drop_finding(
        &mut self,
        actions: &mut Vec<DispatchAction>,
        finding_id: String,
        reason: DropReason,
    ) {
        self.counters.dropped.bump(reason);
        actions.push(DispatchAction::Dropped { finding_id, reason });
    }

    // Vorfall-Meldung ausgeben und zählen.
    fn grouped(
        &mut self,
        actions: &mut Vec<DispatchAction>,
        incident_id: IncidentId,
        finding_id: &str,
    ) {
        self.counters.grouped = self.counters.grouped.saturating_add(1);
        actions.push(DispatchAction::Grouped {
            incident_id,
            finding_id: finding_id.to_owned(),
        });
    }

    // Loopback-Egress (O9): Rückfrage statt Triage, mit Hinweis. Eine dafür
    // verdrängte Rückfrage landet in `withdrawn`.
    fn ask_user(
        &mut self,
        actions: &mut Vec<DispatchAction>,
        withdrawn: &mut Vec<String>,
        admitted: &Admitted<'_>,
        notified: bool,
        now_unix: i64,
    ) {
        let finding = admitted.finding;
        if !self.charge_budget(actions, admitted, now_unix) {
            return;
        }
        // Erst verdrängen (das vergisst den alten Schlüssel), dann merken.
        if let Some(evicted) = self.track_ask(finding, now_unix) {
            withdrawn.push(evicted);
        }
        self.remember(finding, now_unix);
        if !notified {
            let rule_id = self
                .routes
                .canonical_rule_id(&finding.rule_id)
                .unwrap_or(UNKNOWN_RULE);
            self.notify(actions, &finding.finding_id, admitted.severity, rule_id);
        }
        self.counters.asked_user = self.counters.asked_user.saturating_add(1);
        actions.push(DispatchAction::AskUser {
            finding_id: finding.finding_id.clone(),
        });
    }

    // Gruppierung und Einreihen eines geprüften Befunds.
    fn route_to_triage(
        &mut self,
        actions: &mut Vec<DispatchAction>,
        admitted: &Admitted<'_>,
        now_unix: i64,
    ) {
        let finding = admitted.finding;
        let Some(member) = JobMember::from_finding(finding) else {
            // Durch `admit` ausgeschlossen: KI-Routen haben einen Digest.
            self.drop_finding(
                actions,
                finding.finding_id.clone(),
                DropReason::MissingRecordDigest,
            );
            return;
        };
        let grouping = match finding.cgroup_kid {
            Some(cgroup_kid) => self.grouper.observe(
                GroupInput {
                    cgroup_kid,
                    rule_id: &finding.rule_id,
                    finding_id: &finding.finding_id,
                    role: admitted.role,
                    severity: admitted.severity,
                    hardness: finding.hardness,
                    observed_at_unix: finding.observed_at_unix,
                },
                now_unix,
            ),
            None => Grouping::Standalone,
        };
        match grouping {
            Grouping::Standalone => {
                self.enqueue_new(actions, admitted, member, None, now_unix);
            }
            Grouping::Formed {
                incident_id,
                earlier_finding_ids,
                role,
            } => {
                for earlier in &earlier_finding_ids {
                    self.grouped(actions, incident_id, earlier);
                }
                self.grouped(actions, incident_id, &finding.finding_id);
                // Nur Einzelaufträge früherer Mitglieder gehen auf, nie der
                // Auftrag eines anderen Vorfalls.
                let taken = self.queue.remove_where(|queued| {
                    queued.incident_id.is_none()
                        && queued
                            .members
                            .iter()
                            .any(|existing| earlier_finding_ids.contains(&existing.finding_id))
                });
                let incident = (incident_id, role);
                if taken.is_empty() {
                    self.enqueue_new(actions, admitted, member, Some(incident), now_unix);
                } else {
                    self.merge_into_incident(actions, admitted, member, incident, taken, now_unix);
                }
            }
            Grouping::Joined {
                incident_id,
                job,
                role,
                ..
            } => {
                self.grouped(actions, incident_id, &finding.finding_id);
                let incident = (incident_id, role);
                // Die Rolle des schwersten Befunds kann sich mit diesem
                // Mitglied geändert haben.
                self.sync_incident_role(actions, incident);
                // Nur ein noch wartender Vorfall-Auftrag mit Platz nimmt den
                // Befund auf; sein Verdict sieht ihn dann. Ist der Auftrag
                // vergeben, abgeschlossen, voll oder gelöst, bekommt der
                // Befund einen eigenen Folgeauftrag für den Vorfall (mit
                // Budget und der Rolle des Vorfalls).
                let waiting = job.and_then(|job_id| {
                    self.queue
                        .remove_where(|queued| queued.job_id == job_id && queued.has_room())
                        .into_iter()
                        .next()
                });
                let Some(mut queued) = waiting else {
                    self.enqueue_new(actions, admitted, member, Some(incident), now_unix);
                    return;
                };
                match queued.add_member(member) {
                    None => {
                        queued.novel |= admitted.novel;
                        queued.retriage |= admitted.retriage;
                        self.remember(finding, now_unix);
                        // Der Platz wurde eben frei; ein Überlauf ist
                        // ausgeschlossen.
                        self.push_queued(actions, queued);
                    }
                    Some(member) => {
                        // Durch `has_room` ausgeschlossen: der Auftrag bleibt
                        // unverändert, der Befund bekommt einen eigenen.
                        self.push_queued(actions, queued);
                        self.enqueue_new(actions, admitted, member, Some(incident), now_unix);
                    }
                }
            }
        }
    }

    // Setzt die Rolle aller wartenden Aufträge eines Vorfalls auf dessen
    // aktuelle Rolle (README §3.3: „Rolle des höchsten Befunds"). Vergebene
    // Aufträge behalten ihre. Die Dringlichkeit hängt nicht an der Rolle;
    // die Aufträge kommen also unverändert zurück.
    fn sync_incident_role(
        &mut self,
        actions: &mut Vec<DispatchAction>,
        (incident_id, role): (IncidentId, TriageRole),
    ) {
        let stale = self.queue.remove_where(|queued| {
            queued.incident_id == Some(incident_id) && queued.incident_role != Some(role)
        });
        for mut queued in stale {
            queued.incident_role = Some(role);
            // Der Platz wurde eben frei; ein Überlauf ist ausgeschlossen.
            self.push_queued(actions, queued);
        }
    }

    // Neuer Auftrag (Budget, Warteschlange, Vorfall-Bindung). Ein
    // Vorfall-Auftrag trägt die Rolle des Vorfalls.
    fn enqueue_new(
        &mut self,
        actions: &mut Vec<DispatchAction>,
        admitted: &Admitted<'_>,
        member: JobMember,
        incident: Option<(IncidentId, TriageRole)>,
        now_unix: i64,
    ) {
        let finding = admitted.finding;
        if !self.charge_budget(actions, admitted, now_unix) {
            return;
        }
        let job_id = self.allocate_job_id();
        let queued = QueuedTriage {
            job_id,
            incident_id: incident.map(|(incident_id, _)| incident_id),
            incident_role: incident.map(|(_, role)| role),
            members: vec![member],
            novel: admitted.novel,
            retriage: admitted.retriage,
        };
        if self.push_queued(actions, queued) {
            self.remember(finding, now_unix);
            self.counters.enqueued = self.counters.enqueued.saturating_add(1);
            actions.push(DispatchAction::EnqueuedTriage {
                job_id,
                role: incident.map_or(admitted.role, |(_, role)| role),
            });
            if let (Some((incident_id, _)), Some(cgroup_kid)) = (incident, finding.cgroup_kid) {
                self.grouper.bind_job(cgroup_kid, incident_id, job_id);
            }
        }
    }

    // Verbraucht ein Budget-Token für einen Auftrag oder eine Rückfrage.
    // Bei Abweisung: `Dropped(BudgetExhausted)` und, beim ersten Abweisen
    // einer Episode, ein `Starved` mit der Ursache. `true` = bewilligt.
    fn charge_budget(
        &mut self,
        actions: &mut Vec<DispatchAction>,
        admitted: &Admitted<'_>,
        now_unix: i64,
    ) -> bool {
        let class = BudgetClass::of(admitted.severity, admitted.finding.hardness);
        match self
            .budget
            .try_consume(&admitted.partition, class, now_unix)
        {
            BudgetDecision::Granted => true,
            BudgetDecision::Refused { scope, tripped } => {
                if tripped {
                    self.counters.budget_trips = self.counters.budget_trips.saturating_add(1);
                    self.report_starved(
                        actions,
                        admitted.partition.clone(),
                        StarvedCause::from(scope),
                        now_unix,
                    );
                }
                self.drop_finding(
                    actions,
                    admitted.finding.finding_id.clone(),
                    DropReason::BudgetExhausted,
                );
                false
            }
        }
    }

    // Führt wartende Einzelaufträge früherer Mitglieder und den neuen Befund
    // zu einem Vorfall-Auftrag zusammen (ohne neues Budget: es werden nicht
    // mehr Aufträge). Jeder aufgegangene Auftrag wird mit `Merged` gemeldet.
    // Ein Auftrag, der nicht mehr hineinpasst, bleibt für sich bestehen. Der
    // Vorfall-Auftrag trägt die Rolle des Vorfalls.
    fn merge_into_incident(
        &mut self,
        actions: &mut Vec<DispatchAction>,
        admitted: &Admitted<'_>,
        member: JobMember,
        incident: (IncidentId, TriageRole),
        taken: Vec<QueuedTriage>,
        now_unix: i64,
    ) {
        let finding = admitted.finding;
        let (incident_id, role) = incident;
        let job_id = self.allocate_job_id();
        let mut merged = QueuedTriage {
            job_id,
            incident_id: Some(incident_id),
            incident_role: Some(role),
            members: Vec::new(),
            novel: admitted.novel,
            retriage: admitted.retriage,
        };
        let mut merged_from = Vec::new();
        let mut left_over = Vec::new();
        for queued in taken {
            // Ein Platz bleibt für den neuen Befund frei.
            let fits = merged.members.len().saturating_add(queued.members.len()) < MAX_JOB_MEMBERS;
            if fits {
                merged.novel |= queued.novel;
                merged.retriage |= queued.retriage;
                merged_from.push(queued.job_id);
                merged.members.extend(queued.members);
            } else {
                left_over.push(queued);
            }
        }
        for queued in left_over {
            // Ihr Platz wurde eben frei; sie bleiben eigene Aufträge.
            self.push_queued(actions, queued);
        }
        let rejected = merged.add_member(member);
        if rejected.is_none() {
            self.remember(finding, now_unix);
        }
        if self.push_queued(actions, merged) {
            self.counters.enqueued = self.counters.enqueued.saturating_add(1);
            actions.push(DispatchAction::EnqueuedTriage { job_id, role });
            for from in merged_from {
                self.counters.merged = self.counters.merged.saturating_add(1);
                actions.push(DispatchAction::Merged { from, into: job_id });
            }
            if let Some(cgroup_kid) = finding.cgroup_kid {
                self.grouper.bind_job(cgroup_kid, incident_id, job_id);
            }
        }
        if let Some(member) = rejected {
            // Durch den freigehaltenen Platz ausgeschlossen; der Befund
            // bekommt dann einen eigenen Auftrag für den Vorfall.
            self.enqueue_new(actions, admitted, member, Some(incident), now_unix);
        }
    }

    // Reiht einen Auftrag ein; `true`, wenn er in der Warteschlange steht.
    fn push_queued(&mut self, actions: &mut Vec<DispatchAction>, queued: QueuedTriage) -> bool {
        let Some(priority) = queued.priority() else {
            return false;
        };
        match self.queue.push(priority, queued) {
            PushOutcome::Queued => true,
            PushOutcome::Evicted(evicted) => {
                self.report_overflow(actions, &evicted);
                true
            }
            PushOutcome::Rejected(rejected) => {
                self.report_overflow(actions, &rejected);
                false
            }
        }
    }

    // Überlauf: jedes Mitglied verworfen, Schlüssel und Vorfall gelöst.
    fn report_overflow(&mut self, actions: &mut Vec<DispatchAction>, job: &QueuedTriage) {
        for member in &job.members {
            self.drop_finding(
                actions,
                member.finding_id.clone(),
                DropReason::QueueOverflow,
            );
        }
        self.forget_members(job);
    }

    // Vergisst die Schlüssel eines Auftrags und löst ihn vom Vorfall.
    fn forget_members(&mut self, job: &QueuedTriage) {
        for member in &job.members {
            self.known.forget(&member.dedup_key);
        }
        self.grouper.unbind_job(job.job_id);
    }

    // Merkt den Schlüssel eines triagierten oder abgedeckten Befunds.
    fn remember(&mut self, finding: &DispatchFinding, now_unix: i64) {
        self.known.remember(
            &finding.dedup_key,
            &finding.fingerprint,
            finding.hardness,
            now_unix,
        );
    }

    // Vergibt die nächste Auftragsnummer.
    fn allocate_job_id(&mut self) -> TriageJobId {
        let job_id = TriageJobId::from_u64(self.next_job);
        self.next_job = self.next_job.saturating_add(1);
        job_id
    }

    // Meldet einen Trip: höchstens ein `Starved` je Ursache und uid der
    // Partition innerhalb von `breaker.quiet_secs` nach dem letzten
    // **gemeldeten**. Unterdrückte Trips verschieben das Fenster nicht; geht
    // die Zeit rückwärts, wird gemeldet.
    fn report_starved(
        &mut self,
        actions: &mut Vec<DispatchAction>,
        partition: PartitionKey,
        cause: StarvedCause,
        now_unix: i64,
    ) {
        let quiet = i64::from(self.config.breaker.quiet_secs);
        let live = |last: i64| (0..quiet).contains(&now_unix.saturating_sub(last));
        let key: StarvedEpisodeKey = (cause, partition.uid);
        if self
            .starved_episodes
            .get(&key)
            .is_some_and(|last| live(*last))
        {
            self.counters.starved_suppressed = self.counters.starved_suppressed.saturating_add(1);
            return;
        }
        if !self.starved_episodes.contains_key(&key)
            && self.starved_episodes.len() >= MAX_STARVED_EPISODES
        {
            self.starved_episodes.retain(|_, last| live(*last));
            if self.starved_episodes.len() >= MAX_STARVED_EPISODES {
                // Voll mit laufenden Episoden: die älteste fällt.
                let oldest = self
                    .starved_episodes
                    .iter()
                    .min_by_key(|(_, last)| **last)
                    .map(|(oldest, _)| *oldest);
                if let Some(oldest) = oldest {
                    self.starved_episodes.remove(&oldest);
                }
            }
        }
        self.starved_episodes.insert(key, now_unix);
        starved(actions, partition, cause);
    }

    // Merkt eine offene Rückfrage samt geprüftem Befund. Ist die Grenze
    // erreicht, fällt die älteste; ihr Schlüssel gilt wieder als neu, ihre
    // Kennung kommt zurück (für `AskWithdrawn`).
    fn track_ask(&mut self, finding: &DispatchFinding, now_unix: i64) -> Option<String> {
        let mut evicted = None;
        if !self.pending_asks.contains_key(&finding.finding_id)
            && self.pending_asks.len() >= MAX_PENDING_ASKS
        {
            let oldest = self
                .pending_asks
                .iter()
                .min_by_key(|(_, ask)| ask.asked_at_unix)
                .map(|(id, _)| id.clone());
            if let Some(id) = oldest {
                if let Some(ask) = self.pending_asks.remove(&id) {
                    self.known.forget(&ask.finding.dedup_key);
                }
                evicted = Some(id);
            }
        }
        self.pending_asks.insert(
            finding.finding_id.clone(),
            PendingAsk {
                finding: finding.clone(),
                asked_at_unix: now_unix,
            },
        );
        evicted
    }

    // Vergisst Rückfragen, die länger als `known_key_ttl_secs` offen sind;
    // ihr Schlüssel gilt wieder als neu, ein erneutes Auftreten fragt erneut.
    // Liefert ihre Kennungen (für `AskWithdrawn`).
    fn prune_asks(&mut self, now_unix: i64) -> Vec<String> {
        let ttl = i64::from(self.config.known_key_ttl_secs);
        let expired: Vec<String> = self
            .pending_asks
            .iter()
            .filter(|(_, ask)| now_unix.saturating_sub(ask.asked_at_unix) > ttl)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            if let Some(ask) = self.pending_asks.remove(id) {
                self.known.forget(&ask.finding.dedup_key);
            }
        }
        expired
    }

    // Meldet weggefallene Rückfragen direkt hinter den führenden Hinweisen
    // und `Starved`-Meldungen, also vor einer neuen Rückfrage mit derselben
    // Kennung. Die Kennungen sind die geprüften UUIDv4 der Rückfragen.
    fn withdraw_asks(&mut self, actions: &mut Vec<DispatchAction>, withdrawn: Vec<String>) {
        let position = actions
            .iter()
            .take_while(|action| {
                matches!(
                    action,
                    DispatchAction::Notify { .. } | DispatchAction::Starved { .. }
                )
            })
            .count();
        for (offset, finding_id) in withdrawn.into_iter().enumerate() {
            self.counters.asks_withdrawn = self.counters.asks_withdrawn.saturating_add(1);
            actions.insert(
                position.saturating_add(offset),
                DispatchAction::AskWithdrawn { finding_id },
            );
        }
    }

    // Verlängert die Markierung „Sensoren degradiert".
    fn mark_degraded(&mut self, now_unix: i64) {
        let until = now_unix.saturating_add(i64::from(self.config.degraded_mark_secs));
        self.degraded_until_unix = Some(
            self.degraded_until_unix
                .map_or(until, |current| current.max(until)),
        );
    }
}

// Fügt ein `Starved` direkt hinter den führenden Hinweisen ein, damit die
// Reihenfolge „Hinweis, dann Starved, dann alles Übrige" auch gilt, wenn
// schon `Grouped`-Meldungen vorliegen.
fn starved(actions: &mut Vec<DispatchAction>, partition: PartitionKey, cause: StarvedCause) {
    let position = actions
        .iter()
        .take_while(|action| matches!(action, DispatchAction::Notify { .. }))
        .count();
    actions.insert(position, DispatchAction::Starved { partition, cause });
}

// Prüft einen Befund in fester Reihenfolge (siehe `DropReason::ALL`).
fn admit(finding: &DispatchFinding, route: Option<Route>) -> Result<Route, DropReason> {
    if !is_uuid_v4(&finding.finding_id) {
        return Err(DropReason::InvalidFindingId);
    }
    if !finding.verified_source {
        return Err(DropReason::UnverifiedSource);
    }
    let Some(route) = route else {
        return Err(DropReason::UnknownRule);
    };
    if !is_dedup_key(&finding.dedup_key) {
        return Err(DropReason::InvalidDedupKey);
    }
    if !is_sensor_id(&finding.sensor) {
        return Err(DropReason::InvalidSensor);
    }
    if finding
        .fingerprint
        .exe_digest
        .as_deref()
        .is_some_and(|digest| !is_record_digest(digest))
    {
        return Err(DropReason::InvalidFingerprint);
    }
    if route.is_ai() {
        match finding.record_digest.as_deref() {
            None => return Err(DropReason::MissingRecordDigest),
            Some(digest) if !is_record_digest(digest) => {
                return Err(DropReason::InvalidRecordDigest);
            }
            Some(_) => {}
        }
    }
    Ok(route)
}
