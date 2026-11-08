//! Partitioniertes Triage-Budget: [`PartitionedBudget`] (I10, B10).
//!
//! # Zweck
//! Jeder neue Triage-Auftrag (und jede Rückfrage an den Nutzer) kostet ein
//! Token aus **zwei** Eimern:
//! - dem Eimer seiner Partition `(uid, cgroup_kid, sensor)`
//!   ([`crate::PartitionKey`]): Kapazität und Nachfüllrate je Stunde;
//! - dem globalen Eimer: Kapazität und Nachfüllrate je Stunde.
//!
//! # Reserve
//! Ein reservierter Anteil des globalen Eimers (Vorgabe 25 %) ist nur für
//! `High`/`Critical`-Befunde mit Härte `Observed` nutzbar
//! ([`BudgetClass::HighObserved`]). Ein Sturm in einer Partition leert so
//! höchstens deren eigenen Eimer und nie die Reserve; Befunde anderer
//! Partitionen bleiben triagierbar (Invariante 8).
//!
//! Wer in die Reserve greift, zahlt zusätzlich ein Token aus dem
//! Reserve-Eimer **seiner uid** (`uid_reserve_capacity`,
//! `uid_reserve_refill_per_hour`). Cgroups kann ein Angreifer beliebig viele
//! anlegen, uids nicht: ohne diese Stufe könnte er mit `High`+`Observed`-
//! Befunden über viele Cgroups die ganze Reserve leeren. So bleibt der Rest
//! der Reserve für `High`+`Observed`-Befunde **anderer** uids. Außerhalb der
//! Reserve (globaler Eimer über der Reserve-Grenze) greift die uid-Stufe
//! nicht; im Normalbetrieb ändert sie also nichts.
//!
//! # Etablierte Partitionen (gleiche uid)
//! Auf einem Einzelnutzer-Desktop teilen Angreifer und Opfer meist eine uid;
//! die uid-Quote allein schützt das Opfer dann nicht. Eine **Reserve-Episode**
//! beginnt beim ersten Aufruf, der den globalen Eimer an der Reserve-Grenze
//! sieht, und endet beim ersten, der ihn wieder darüber sieht. Als Sichtung
//! einer Partition zählt nur ein **bewilligter** Aufruf (abgewiesene nie).
//! Eine Partition ist in der Episode **etabliert**, wenn ihre erste Sichtung
//! mindestens `established_reserve_interval_secs` (und mindestens eine
//! Sekunde) vor Beginn der Episode liegt und ihre letzte höchstens
//! [`ESTABLISHED_TTL_SECS`] zurück.
//!
//! Ist die uid-Quote leer, darf eine etablierte Partition ein Reserve-Token
//! **außerhalb** der Quote nehmen (Vorrecht), wenn in den letzten
//! `established_reserve_interval_secs` weder sie noch **ihre uid** eines so
//! genommen hat: je uid also höchstens ein Vorrechts-Token je Intervall, gleich
//! wie viele Cgroups die uid hat. Partitionen, die erst kurz vor oder während
//! der Episode auftauchen (frisch angelegte Scopes des Angreifers), begrenzt
//! allein die uid-Quote.
//!
//! Die Erstsichtungen liegen getrennt von den Eimern (die verdrängt werden
//! dürfen), höchstens `max_partitions` Stück; ist die Tabelle voll, fallen nur
//! Einträge, die [`ESTABLISHED_TTL_SECS`] lang ruhig waren, und sonst wird die
//! neue Partition nicht eingetragen (ein Sturm neuer Scopes verdrängt keine
//! etablierte Partition). Die Vorrechte je uid sind ebenso auf
//! `max_partitions` Einträge begrenzt; ist die Tabelle voll mit Einträgen,
//! deren Intervall noch läuft, gibt es kein Vorrecht (nie ein unbegrenztes).
//!
//! **Grenzen:**
//! - Eine uid nimmt aus der Reserve höchstens ihre Quote
//!   (`uid_reserve_capacity` plus Nachfüllung) und ein Vorrechts-Token je
//!   Intervall, auch mit beliebig vielen etablierten Partitionen. Der Rest der
//!   Reserve bleibt `High`+`Observed`-Befunden **anderer** uids.
//! - Ein Opfer **derselben** uid schützt das Vorrecht nur gegen frische
//!   Scopes. Hat der Angreifer selbst etablierte Partitionen (bewilligte
//!   Befunde mindestens ein Intervall vor der Episode), konkurriert er mit dem
//!   Opfer um das eine Vorrechts-Token je Intervall; fragt er zuerst, bekommt
//!   das Opfer keine KI-Triage mehr. Ebenso teilt eine Opfer-Partition, die
//!   nicht etabliert ist, die uid-Quote mit dem Angreifer. Der Hinweis (I3)
//!   und der `dispatch-starved`-Befund bleiben in allen Fällen.
//!
//! # Trip
//! [`PartitionedBudget::try_consume`] meldet eine Abweisung mit ihrer Ebene
//! ([`BudgetScope`]) und `tripped == true` nur für die **erste** Abweisung
//! einer Episode dieser Ebene. Die Episoden und ihre Hysterese:
//! - Partition und uid-Reserve: bis der betroffene Eimer wieder voll ist;
//! - global ([`BudgetScope::Global`], `Standard` an der Reserve-Grenze): bis
//!   der globale Eimer wieder voll ist;
//! - Reserve ([`BudgetScope::Reserve`], `High`+`Observed` findet kein Token
//!   mehr): bis der globale Eimer wieder mindestens die Reserve (und
//!   mindestens ein Token) hält. Diese Episode ist von der globalen getrennt,
//!   damit ein früherer Trip durch `Standard`-Befunde die echte Erschöpfung
//!   der Reserve nicht verschluckt.
//!
//! Der Dispatcher macht aus einem Trip ein
//! [`crate::DispatchAction::Starved`] (README §3.3: „Breaker- oder
//! Budget-Trip").
//!
//! # Rechnung
//! Ganzzahlig in Bruchteilen eines Tokens ([`TOKEN_UNITS`] = 3 600 000 je
//! Token). Eine Nachfüllrate von `n` Tokens je Stunde ist damit genau
//! `n * 1000` Einheiten je Sekunde: es wird nie gerundet, auch nicht bei
//! kleinen Raten und häufigen Aufrufen.
//! Die Zeit kommt als Unix-Sekunden herein; geht sie rückwärts, wird nichts
//! nachgefüllt und der Bezugspunkt zurückgesetzt. Ein neuer Eimer startet
//! voll.
//!
//! # Speicher
//! Höchstens `max_partitions` Partitions- und ebenso viele uid-Eimer. Ein
//! voller Eimer ist gleichwertig zu keinem Eimer und fällt zuerst; danach
//! der am längsten unbenutzte. Dieselben Grenzen gelten beim Laden eines
//! Zustands ([`crate::Dispatcher::restore`]) für alle vier Tabellen (Eimer,
//! uid-Eimer, Erstsichtungen, Vorrechte je uid), auch wenn er unter einer
//! größeren Grenze geschrieben wurde: was darüber liegt, fällt
//! deterministisch, bevor das Budget benutzt wird, bei Gleichstand der
//! kleinere Schlüssel. Eimer wie oben, nur ohne Uhr. Erstsichtungen und
//! Vorrechte verdrängt die Laufzeit nie lebend; beim Laden fallen sie
//! fail-closed nach eigener Regel: Erstsichtungen, die relativ zur jüngsten
//! Sichtung der Tabelle abgelaufen sind, dann die zuletzt aufgenommenen (die
//! am längsten etablierten bleiben); Vorrechte die ältesten (solange die
//! behaltenen laufen, gibt es kein neues).
//!
//! # Nebenläufigkeit und Fehler
//! `&mut self`, deterministisch; Fehler nur beim Laden eines Zustands.
//!
//! # Examples
//! ```
//! use harw_dod_dispatch::{
//!     BudgetClass, BudgetConfig, BudgetDecision, BudgetScope, PartitionKey, PartitionedBudget,
//! };
//!
//! let config = BudgetConfig { partition_capacity: 1, ..BudgetConfig::default() };
//! let mut budget = PartitionedBudget::new(config);
//! let key = PartitionKey { uid: Some(1000), cgroup_kid: Some(7), sensor: "probe-bpf-0".to_owned() };
//! assert_eq!(budget.try_consume(&key, BudgetClass::Standard, 0), BudgetDecision::Granted);
//! assert_eq!(
//!     budget.try_consume(&key, BudgetClass::Standard, 1),
//!     BudgetDecision::Refused { scope: BudgetScope::Partition, tripped: true }
//! );
//! // Dieselbe Episode: kein zweiter Trip.
//! assert_eq!(
//!     budget.try_consume(&key, BudgetClass::Standard, 2),
//!     BudgetDecision::Refused { scope: BudgetScope::Partition, tripped: false }
//! );
//! ```

use std::cmp::Reverse;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::DispatchError;
use crate::finding::{Hardness, PartitionKey, Severity, is_sensor_id};
use crate::prune::retain_greatest;

/// Ein Token in internen Einheiten (durch 3600 teilbar, siehe Moduldoku).
pub const TOKEN_UNITS: u64 = 3_600_000;

/// Nach so vielen Sekunden ohne Aufruf gilt eine etablierte Partition wieder
/// als neu (Kernel-IDs von Cgroups werden wiederverwendet); 7 Tage.
pub const ESTABLISHED_TTL_SECS: i64 = 604_800;

// Einheiten je Sekunde für eine Rate von einem Token je Stunde.
const UNITS_PER_SECOND_PER_HOURLY_TOKEN: u128 = 1_000;

/// Grenzen des Budgets.
///
/// # Description
/// Vorgaben: 6 Tokens je Partition (12 je Stunde nach), global 48 (120 je
/// Stunde nach), davon 25 % Reserve für `High`+`Observed`, je uid höchstens
/// 6 Tokens aus der Reserve (12 je Stunde nach), je uid höchstens ein
/// Reserve-Token je 300 s außerhalb der uid-Quote (nur für Partitionen, die
/// mindestens 300 s vor der Reserve-Episode ein Token bekamen), höchstens
/// 4096 Partitionen. Fehlende Felder nehmen beim Deserialisieren die Vorgabe
/// an.
///
/// # Examples
/// ```
/// use harw_dod_dispatch::BudgetConfig;
///
/// let config = BudgetConfig::default();
/// assert_eq!(config.reserved_share_percent, 25);
/// assert_eq!(config.uid_reserve_capacity, 6);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BudgetConfig {
    /// Tokens je Partition (Eimergröße).
    pub partition_capacity: u32,
    /// Nachgefüllte Tokens je Partition und Stunde.
    pub partition_refill_per_hour: u32,
    /// Tokens im globalen Eimer.
    pub global_capacity: u32,
    /// Nachgefüllte Tokens im globalen Eimer je Stunde.
    pub global_refill_per_hour: u32,
    /// Anteil des globalen Eimers in Prozent, den nur
    /// [`BudgetClass::HighObserved`] nutzen darf.
    pub reserved_share_percent: u8,
    /// Tokens, die eine einzelne uid höchstens am Stück aus der Reserve
    /// ziehen kann (Eimergröße der uid-Reserve).
    pub uid_reserve_capacity: u32,
    /// Nachgefüllte Tokens der uid-Reserve je uid und Stunde.
    pub uid_reserve_refill_per_hour: u32,
    /// Intervall des Vorrechts etablierter Partitionen in Sekunden (siehe
    /// Moduldoku): so lange muss die erste Sichtung vor der Reserve-Episode
    /// liegen, und höchstens ein Reserve-Token außerhalb der uid-Quote je
    /// Partition **und** je uid in diesem Abstand; `0` schaltet das ab.
    pub established_reserve_interval_secs: u32,
    /// Höchstzahl geführter Partitionen (und ebenso uid-Reserve-Eimer,
    /// Erstsichtungen und Vorrechte je uid).
    pub max_partitions: usize,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            partition_capacity: 6,
            partition_refill_per_hour: 12,
            global_capacity: 48,
            global_refill_per_hour: 120,
            reserved_share_percent: 25,
            uid_reserve_capacity: 6,
            uid_reserve_refill_per_hour: 12,
            established_reserve_interval_secs: 300,
            max_partitions: 4_096,
        }
    }
}

// Grenzen einer Eimer-Familie (Partitionen oder uid-Reserven).
#[derive(Debug, Clone, Copy)]
struct BucketLimits {
    // Eimergröße in Tokens.
    capacity: u32,
    // Nachfüllung in Tokens je Stunde.
    per_hour: u32,
    // Höchstzahl der Eimer.
    max_buckets: usize,
}

impl BudgetConfig {
    // Prüft die Grenzen (von `DispatchConfig::validate` aufgerufen).
    pub(crate) fn validate(&self) -> Result<(), DispatchError> {
        let invalid = |field: &'static str, reason: &'static str| {
            Err(DispatchError::InvalidConfig { field, reason })
        };
        if self.partition_capacity == 0 {
            return invalid("budget.partition_capacity", "must be at least 1");
        }
        if self.global_capacity == 0 {
            return invalid("budget.global_capacity", "must be at least 1");
        }
        if self.reserved_share_percent > 100 {
            return invalid("budget.reserved_share_percent", "must be at most 100");
        }
        if self.uid_reserve_capacity == 0 {
            return invalid("budget.uid_reserve_capacity", "must be at least 1");
        }
        if self.established_reserve_interval_secs > 86_400 {
            return invalid(
                "budget.established_reserve_interval_secs",
                "must be at most 86400",
            );
        }
        if self.max_partitions == 0 || self.max_partitions > 1 << 20 {
            return invalid("budget.max_partitions", "must be between 1 and 1048576");
        }
        Ok(())
    }

    // Reserve in Einheiten.
    fn reserve_units(&self) -> u64 {
        let units = u128::from(self.global_capacity)
            .saturating_mul(u128::from(TOKEN_UNITS))
            .saturating_mul(u128::from(self.reserved_share_percent))
            .checked_div(100)
            .unwrap_or(0);
        u64::try_from(units).unwrap_or(u64::MAX)
    }

    // Grenzen der Partitions-Eimer.
    fn partition_limits(&self) -> BucketLimits {
        BucketLimits {
            capacity: self.partition_capacity,
            per_hour: self.partition_refill_per_hour,
            max_buckets: self.max_partitions,
        }
    }

    // Grenzen der uid-Reserve-Eimer.
    fn uid_reserve_limits(&self) -> BucketLimits {
        BucketLimits {
            capacity: self.uid_reserve_capacity,
            per_hour: self.uid_reserve_refill_per_hour,
            max_buckets: self.max_partitions,
        }
    }
}

/// Budgetklasse eines Befunds.
///
/// # Examples
/// ```
/// use harw_dod_dispatch::{BudgetClass, Hardness, Severity};
///
/// assert_eq!(BudgetClass::of(Severity::Critical, Hardness::Observed), BudgetClass::HighObserved);
/// assert_eq!(BudgetClass::of(Severity::High, Hardness::Inferred), BudgetClass::Standard);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BudgetClass {
    /// `High` oder `Critical` mit Härte `Observed`: darf die Reserve nutzen.
    HighObserved,
    /// Alles andere.
    Standard,
}

impl BudgetClass {
    /// Klasse aus Schwere und Härte.
    ///
    /// # Arguments
    /// - `severity` (`Severity`): wirksame Schwere.
    /// - `hardness` (`Hardness`): Härte.
    ///
    /// # Returns
    /// [`BudgetClass::HighObserved`] für `High`/`Critical` + `Observed`.
    #[must_use]
    pub const fn of(severity: Severity, hardness: Hardness) -> Self {
        if severity.requires_notice() && matches!(hardness, Hardness::Observed) {
            Self::HighObserved
        } else {
            Self::Standard
        }
    }
}

/// Ebene, an der das Budget einen Auftrag abgewiesen hat.
///
/// # Examples
/// ```
/// use harw_dod_dispatch::BudgetScope;
///
/// assert_ne!(BudgetScope::Partition, BudgetScope::Global);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BudgetScope {
    /// Der Eimer der Partition ist leer.
    Partition,
    /// Der Befund müsste in die Reserve greifen, der Reserve-Eimer seiner
    /// uid ist leer.
    UidReserve,
    /// Der globale Eimer reicht für die Klasse nicht: für
    /// [`BudgetClass::Standard`] ist nur noch die Reserve übrig; für
    /// [`BudgetClass::HighObserved`] nur, wenn keine Reserve konfiguriert
    /// ist und der Eimer leer ist.
    Global,
    /// Die Reserve selbst ist erschöpft: ein [`BudgetClass::HighObserved`]
    /// findet kein Token mehr im globalen Eimer.
    Reserve,
}

/// Ergebnis von [`PartitionedBudget::try_consume`].
///
/// # Examples
/// ```
/// use harw_dod_dispatch::{BudgetDecision, BudgetScope};
///
/// assert!(BudgetDecision::Granted.is_granted());
/// assert!(!BudgetDecision::Refused { scope: BudgetScope::Global, tripped: false }.is_granted());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[must_use]
pub enum BudgetDecision {
    /// Ein Token wurde verbraucht.
    Granted,
    /// Nichts wurde verbraucht.
    Refused {
        /// Die Ebene, die nicht gereicht hat.
        scope: BudgetScope,
        /// Erste Abweisung dieser Ebene seit sie zuletzt voll war (ein Trip).
        tripped: bool,
    },
}

impl BudgetDecision {
    /// Ob ein Token verbraucht wurde.
    #[must_use]
    pub const fn is_granted(self) -> bool {
        matches!(self, Self::Granted)
    }
}

// Ein Token-Eimer in Einheiten.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TokenBucket {
    // Inhalt in Einheiten.
    units: u64,
    // Bezugszeit der letzten Nachfüllung (= letzte Benutzung); `None` =
    // noch nie benutzt.
    last_refill_unix: Option<i64>,
    // Seit der letzten Abweisung noch nicht wieder voll (Trip-Hysterese).
    starved: bool,
}

impl TokenBucket {
    // Ein voller Eimer.
    fn full(capacity: u32) -> Self {
        Self {
            units: capacity_units(capacity),
            last_refill_unix: None,
            starved: false,
        }
    }

    // Inhalt nach dem Nachfüllen bis `now_unix`, ohne den Eimer zu ändern.
    fn projected_units(&self, capacity: u32, per_hour: u32, now_unix: i64) -> u64 {
        let capacity = capacity_units(capacity);
        let current = self.units.min(capacity);
        let Some(last) = self.last_refill_unix else {
            return current;
        };
        if now_unix <= last {
            // Rückwärtslaufende Zeit: nichts gutschreiben.
            return current;
        }
        let elapsed = u128::from(now_unix.abs_diff(last));
        let added = elapsed
            .saturating_mul(u128::from(per_hour))
            .saturating_mul(UNITS_PER_SECOND_PER_HOURLY_TOKEN);
        let added = u64::try_from(added).unwrap_or(u64::MAX);
        current.saturating_add(added).min(capacity)
    }

    // Füllt anhand der vergangenen Zeit nach und setzt den Bezugspunkt auf
    // `now_unix` (auch rückwärts). Ein voller Eimer beendet eine Episode.
    fn refill(&mut self, capacity: u32, per_hour: u32, now_unix: i64) {
        self.units = self.projected_units(capacity, per_hour, now_unix);
        self.last_refill_unix = Some(now_unix);
        if self.units >= capacity_units(capacity) {
            self.starved = false;
        }
    }

    // Mindestens ein ganzes Token.
    fn has_token(&self) -> bool {
        self.units >= TOKEN_UNITS
    }

    // Verbraucht ein Token (vorher mit `has_token` geprüft).
    fn take_token(&mut self) {
        self.units = self.units.saturating_sub(TOKEN_UNITS);
    }

    // Weist ab; `tripped` nur bei der ersten Abweisung einer Episode.
    fn refuse(&mut self, scope: BudgetScope) -> BudgetDecision {
        let tripped = !self.starved;
        self.starved = true;
        BudgetDecision::Refused { scope, tripped }
    }

    // Kappt den Inhalt auf die Kapazität (beim Laden).
    fn clamped(mut self, capacity: u32) -> Self {
        self.units = self.units.min(capacity_units(capacity));
        self
    }

    // Rang beim Kürzen einer Tabelle beim Laden (größer bleibt): ein nicht
    // voller Eimer vor einem vollen (der gleichwertig zu keinem ist), dann
    // der zuletzt benutzte (`None` = nie benutzt, zuerst verdrängt).
    fn retention_rank(&self, capacity: u32) -> (bool, Option<i64>) {
        (self.units < capacity_units(capacity), self.last_refill_unix)
    }
}

// Kapazität in Einheiten.
fn capacity_units(capacity: u32) -> u64 {
    u64::from(capacity).saturating_mul(TOKEN_UNITS)
}

// Holt den Eimer zu `key` (neu: voll), füllt ihn bis `now_unix` nach und
// gibt ihn zurück. Vorher wird bei Bedarf Platz gemacht.
fn bucket_at<'a, K: Ord + Clone>(
    buckets: &'a mut BTreeMap<K, TokenBucket>,
    key: &K,
    limits: BucketLimits,
    now_unix: i64,
) -> &'a mut TokenBucket {
    if !buckets.contains_key(key) {
        make_room(buckets, limits, now_unix);
    }
    let bucket = buckets
        .entry(key.clone())
        .or_insert_with(|| TokenBucket::full(limits.capacity));
    bucket.refill(limits.capacity, limits.per_hour, now_unix);
    bucket
}

// Macht Platz für einen neuen Eimer: (rechnerisch) volle zuerst, dann der
// am längsten unbenutzte. Volle Eimer werden dabei nicht verändert, damit
// die Reihenfolge „am längsten unbenutzt" erhalten bleibt.
fn make_room<K: Ord + Clone>(
    buckets: &mut BTreeMap<K, TokenBucket>,
    limits: BucketLimits,
    now_unix: i64,
) {
    if buckets.len() < limits.max_buckets {
        return;
    }
    let full = capacity_units(limits.capacity);
    buckets.retain(|_, bucket| {
        bucket.projected_units(limits.capacity, limits.per_hour, now_unix) < full
    });
    while buckets.len() >= limits.max_buckets {
        let oldest = buckets
            .iter()
            .min_by_key(|(_, bucket)| bucket.last_refill_unix)
            .map(|(key, _)| key.clone());
        match oldest {
            Some(key) => {
                buckets.remove(&key);
            }
            None => break,
        }
    }
}

// Erstsichtung einer Partition (siehe Moduldoku, „Etablierte Partitionen").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EstablishedPartition {
    // Erster bewilligter Aufruf (nach dem letzten Ablauf).
    first_seen_unix: i64,
    // Letzter bewilligter Aufruf.
    last_seen_unix: i64,
    // Letztes Reserve-Token außerhalb der uid-Quote.
    last_privileged_unix: Option<i64>,
}

impl EstablishedPartition {
    // Neu gesehen zu `now_unix`.
    fn seen_at(now_unix: i64) -> Self {
        Self {
            first_seen_unix: now_unix,
            last_seen_unix: now_unix,
            last_privileged_unix: None,
        }
    }

    // Etabliert für eine Reserve-Episode ab `since_unix`: erste Sichtung
    // mindestens `min_age_secs` (und mindestens eine Sekunde) vor deren
    // Beginn, letzte höchstens `ESTABLISHED_TTL_SECS` vor `now_unix`.
    fn counts_for(&self, since_unix: i64, now_unix: i64, min_age_secs: u32) -> bool {
        let min_age = i64::from(min_age_secs.max(1));
        now_unix.saturating_sub(self.last_seen_unix) <= ESTABLISHED_TTL_SECS
            && since_unix.saturating_sub(self.first_seen_unix) >= min_age
    }

    // Rang beim Kürzen der Tabelle beim Laden (größer bleibt). Zur Laufzeit
    // verdrängt `note_partition` keinen lebenden Eintrag (eine volle Tabelle
    // nimmt nichts Neues auf, nur abgelaufene fallen); es gilt also die
    // Reihenfolge der Aufnahme. Ohne Uhr dient `newest_unix`, die jüngste
    // Sichtung der Tabelle, als Untergrenze der Zeit beim Schnappschuss: ein
    // relativ zu ihr abgelaufener Eintrag war es sicher schon und fällt
    // zuerst; unter den übrigen bleibt die frühere Erstsichtung.
    fn retention_rank(&self, newest_unix: i64) -> (bool, Reverse<i64>) {
        (
            newest_unix.saturating_sub(self.last_seen_unix) <= ESTABLISHED_TTL_SECS,
            Reverse(self.first_seen_unix),
        )
    }
}

// Vorrecht etablierter Partitionen (siehe Moduldoku): `true`, wenn die
// Partition in der laufenden Reserve-Episode etabliert ist und in den letzten
// `established_reserve_interval_secs` weder sie noch ihre uid ein
// Reserve-Token außerhalb der Quote genommen hat. Dann ist die Vergabe bei
// beiden vermerkt. Freie Funktion, damit der Aufrufer den Partitions- und
// den Quoten-Eimer derweil geliehen halten kann.
fn take_privilege(
    config: &BudgetConfig,
    established: &mut BTreeMap<PartitionKey, EstablishedPartition>,
    uid_privileged: &mut BTreeMap<Option<u32>, i64>,
    since_unix: Option<i64>,
    partition: &PartitionKey,
    now_unix: i64,
) -> bool {
    let interval_secs = config.established_reserve_interval_secs;
    if interval_secs == 0 {
        return false;
    }
    let (Some(since), Some(entry)) = (since_unix, established.get_mut(partition)) else {
        return false;
    };
    if !entry.counts_for(since, now_unix, interval_secs) {
        return false;
    }
    let interval = i64::from(interval_secs);
    // Abgelaufen (oder rückwärtslaufende Zeit): gleichwertig zu keinem Eintrag.
    let due = |last: i64| now_unix < last || now_unix.saturating_sub(last) >= interval;
    if !entry.last_privileged_unix.is_none_or(due) {
        return false;
    }
    match uid_privileged.get(&partition.uid).copied() {
        Some(last) if !due(last) => return false,
        Some(_) => {}
        None => {
            if uid_privileged.len() >= config.max_partitions {
                uid_privileged.retain(|_, last| !due(*last));
            }
            if uid_privileged.len() >= config.max_partitions {
                // Voll mit laufenden Intervallen: lieber kein Vorrecht als ein
                // unvermerktes.
                return false;
            }
        }
    }
    entry.last_privileged_unix = Some(now_unix);
    uid_privileged.insert(partition.uid, now_unix);
    true
}

// Persistierbarer Inhalt eines `PartitionedBudget` (ohne Konfiguration).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BudgetState {
    // Globaler Eimer.
    global: TokenBucket,
    // Eimer je Partition.
    #[serde(with = "crate::serde_map")]
    partitions: BTreeMap<PartitionKey, TokenBucket>,
    // Reserve-Eimer je uid.
    #[serde(with = "crate::serde_map")]
    uid_reserve: BTreeMap<Option<u32>, TokenBucket>,
    // Seit der letzten Reserve-Erschöpfung noch nicht wieder aufgefüllt.
    reserve_starved: bool,
    // Beginn der laufenden Reserve-Episode.
    reserve_since_unix: Option<i64>,
    // Erstsichtungen je Partition.
    #[serde(with = "crate::serde_map")]
    established: BTreeMap<PartitionKey, EstablishedPartition>,
    // Letztes Reserve-Token außerhalb der Quote je uid.
    #[serde(with = "crate::serde_map")]
    uid_privileged: BTreeMap<Option<u32>, i64>,
}

/// Partitioniertes Budget (siehe Moduldoku).
///
/// # Examples
/// Siehe Moduldoku.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionedBudget {
    // Grenzen.
    config: BudgetConfig,
    // Globaler Eimer.
    global: TokenBucket,
    // Eimer je Partition.
    partitions: BTreeMap<PartitionKey, TokenBucket>,
    // Reserve-Eimer je uid.
    uid_reserve: BTreeMap<Option<u32>, TokenBucket>,
    // Seit der letzten Reserve-Erschöpfung noch nicht wieder aufgefüllt.
    reserve_starved: bool,
    // Beginn der laufenden Reserve-Episode.
    reserve_since_unix: Option<i64>,
    // Erstsichtungen je Partition (getrennt von den verdrängbaren Eimern).
    established: BTreeMap<PartitionKey, EstablishedPartition>,
    // Letztes Reserve-Token außerhalb der Quote je uid (höchstens
    // `max_partitions` Einträge).
    uid_privileged: BTreeMap<Option<u32>, i64>,
}

impl PartitionedBudget {
    /// Budget mit vollen Eimern.
    ///
    /// # Arguments
    /// - `config` (`BudgetConfig`): Grenzen (vorher mit
    ///   [`crate::DispatchConfig::validate`] geprüft).
    ///
    /// # Returns
    /// Das Budget.
    #[must_use]
    pub fn new(config: BudgetConfig) -> Self {
        Self {
            config,
            global: TokenBucket::full(config.global_capacity),
            partitions: BTreeMap::new(),
            uid_reserve: BTreeMap::new(),
            reserve_starved: false,
            reserve_since_unix: None,
            established: BTreeMap::new(),
            uid_privileged: BTreeMap::new(),
        }
    }

    /// Versucht, ein Token für einen Auftrag zu verbrauchen.
    ///
    /// # Description
    /// Gelingt nur, wenn
    /// 1. die Partition mindestens ein Token hat,
    /// 2. der globale Eimer nach Abzug noch die Reserve hält
    ///    ([`BudgetClass::Standard`]) bzw. überhaupt ein Token hat
    ///    ([`BudgetClass::HighObserved`]), und
    /// 3. bei einem Griff in die Reserve der Reserve-Eimer der uid ein Token hat
    ///    oder sonst die Partition etabliert ist und weder sie noch ihre uid
    ///    im laufenden Intervall ein Vorrechts-Token genommen hat.
    ///
    /// Sonst wird nichts verbraucht und die erste verletzte Ebene gemeldet;
    /// `tripped` ist nur bei der ersten Abweisung einer Episode gesetzt
    /// (siehe Moduldoku). Nur ein bewilligter Aufruf zählt als Sichtung der
    /// Partition.
    ///
    /// # Arguments
    /// - `partition` (`&PartitionKey`): Partition des Befunds.
    /// - `class` (`BudgetClass`): Klasse des Befunds.
    /// - `now_unix` (`i64`): aktuelle Zeit in Unix-Sekunden.
    ///
    /// # Returns
    /// [`BudgetDecision::Granted`] oder [`BudgetDecision::Refused`].
    pub fn try_consume(
        &mut self,
        partition: &PartitionKey,
        class: BudgetClass,
        now_unix: i64,
    ) -> BudgetDecision {
        let config = self.config;
        self.global.refill(
            config.global_capacity,
            config.global_refill_per_hour,
            now_unix,
        );
        let reserve_units = config.reserve_units();
        let above_reserve = self.global.units >= reserve_units.saturating_add(TOKEN_UNITS);
        if above_reserve {
            self.reserve_since_unix = None;
        } else if self.reserve_since_unix.is_none() {
            self.reserve_since_unix = Some(now_unix);
        }
        // Reserve wieder voll (und mindestens ein Token): neue Episode. Die
        // Bedingung schließt die Abweisung unten aus (dort fehlt ein Token).
        if self.global.units >= reserve_units.max(TOKEN_UNITS) {
            self.reserve_starved = false;
        }
        let bucket = bucket_at(
            &mut self.partitions,
            partition,
            config.partition_limits(),
            now_unix,
        );
        if !bucket.has_token() {
            return bucket.refuse(BudgetScope::Partition);
        }
        let into_reserve = match class {
            BudgetClass::Standard if !above_reserve => {
                return self.global.refuse(BudgetScope::Global);
            }
            BudgetClass::Standard => false,
            BudgetClass::HighObserved if above_reserve => false,
            BudgetClass::HighObserved if self.global.has_token() => true,
            BudgetClass::HighObserved if reserve_units == 0 => {
                return self.global.refuse(BudgetScope::Global);
            }
            BudgetClass::HighObserved => {
                let tripped = !self.reserve_starved;
                self.reserve_starved = true;
                return BudgetDecision::Refused {
                    scope: BudgetScope::Reserve,
                    tripped,
                };
            }
        };
        if into_reserve {
            // Erst die uid-Quote, dann (wenn sie leer ist) das Vorrecht: so
            // bleibt das eine Vorrechts-Token je Intervall für den Fall, dass
            // frische Scopes derselben uid die Quote geleert haben.
            let quota = bucket_at(
                &mut self.uid_reserve,
                &partition.uid,
                config.uid_reserve_limits(),
                now_unix,
            );
            if quota.has_token() {
                quota.take_token();
            } else if !take_privilege(
                &config,
                &mut self.established,
                &mut self.uid_privileged,
                self.reserve_since_unix,
                partition,
                now_unix,
            ) {
                return quota.refuse(BudgetScope::UidReserve);
            }
        }
        bucket.take_token();
        self.global.take_token();
        self.note_partition(partition, now_unix);
        BudgetDecision::Granted
    }

    /// Ob eine Partition für die laufende Reserve-Episode als etabliert gilt.
    ///
    /// # Arguments
    /// - `partition` (`&PartitionKey`): die Partition.
    /// - `now_unix` (`i64`): aktuelle Zeit in Unix-Sekunden.
    ///
    /// # Returns
    /// `true`, wenn eine Reserve-Episode läuft, die erste bewilligte Sichtung
    /// der Partition mindestens `established_reserve_interval_secs` (und
    /// mindestens eine Sekunde) vor deren Beginn liegt und die letzte
    /// höchstens [`ESTABLISHED_TTL_SECS`] vor `now_unix` (siehe Moduldoku);
    /// sonst `false`.
    #[must_use]
    pub fn is_established(&self, partition: &PartitionKey, now_unix: i64) -> bool {
        match (self.established.get(partition), self.reserve_since_unix) {
            (Some(entry), Some(since)) => entry.counts_for(
                since,
                now_unix,
                self.config.established_reserve_interval_secs,
            ),
            _ => false,
        }
    }

    // Vermerkt eine Sichtung (nur nach einer Bewilligung); nach
    // `ESTABLISHED_TTL_SECS` Ruhe gilt die Partition wieder als neu. Ist die
    // Tabelle voll, fallen nur abgelaufene Einträge; reicht das nicht, wird
    // die neue Partition nicht eingetragen.
    fn note_partition(&mut self, partition: &PartitionKey, now_unix: i64) {
        if let Some(entry) = self.established.get_mut(partition) {
            if now_unix.saturating_sub(entry.last_seen_unix) > ESTABLISHED_TTL_SECS {
                *entry = EstablishedPartition::seen_at(now_unix);
            } else {
                entry.last_seen_unix = now_unix;
            }
            return;
        }
        let limit = self.config.max_partitions;
        if self.established.len() >= limit {
            self.established.retain(|_, entry| {
                now_unix.saturating_sub(entry.last_seen_unix) <= ESTABLISHED_TTL_SECS
            });
        }
        if self.established.len() < limit {
            self.established
                .insert(partition.clone(), EstablishedPartition::seen_at(now_unix));
        }
    }

    /// Ganze Tokens im globalen Eimer (Stand der letzten Nachfüllung).
    #[must_use]
    pub fn global_tokens(&self) -> u64 {
        self.global.units.checked_div(TOKEN_UNITS).unwrap_or(0)
    }

    /// Ganze Tokens einer Partition (Stand der letzten Nachfüllung).
    ///
    /// # Arguments
    /// - `partition` (`&PartitionKey`): die Partition.
    ///
    /// # Returns
    /// `None` für eine Partition ohne Eimer (gilt als voll).
    #[must_use]
    pub fn partition_tokens(&self, partition: &PartitionKey) -> Option<u64> {
        self.partitions
            .get(partition)
            .map(|bucket| bucket.units.checked_div(TOKEN_UNITS).unwrap_or(0))
    }

    /// Ganze Tokens im Reserve-Eimer einer uid (Stand der letzten
    /// Nachfüllung).
    ///
    /// # Arguments
    /// - `uid` (`Option<u32>`): die uid (`None` = Befunde ohne uid).
    ///
    /// # Returns
    /// `None`, solange die uid nie in die Reserve gegriffen hat (gilt als
    /// voll).
    #[must_use]
    pub fn uid_reserve_tokens(&self, uid: Option<u32>) -> Option<u64> {
        self.uid_reserve
            .get(&uid)
            .map(|bucket| bucket.units.checked_div(TOKEN_UNITS).unwrap_or(0))
    }

    // Persistierbarer Inhalt.
    pub(crate) fn export(&self) -> BudgetState {
        BudgetState {
            global: self.global,
            partitions: self.partitions.clone(),
            uid_reserve: self.uid_reserve.clone(),
            reserve_starved: self.reserve_starved,
            reserve_since_unix: self.reserve_since_unix,
            established: self.established.clone(),
            uid_privileged: self.uid_privileged.clone(),
        }
    }

    // Baut das Budget aus einem Zustand: ungültige Sensor-Kennungen werden
    // abgelehnt (auch in Einträgen, die danach fallen), jede der vier
    // Tabellen wird auf `max_partitions` gekürzt (Grenze der aktiven
    // Konfiguration, siehe `crate::prune`), Inhalte über der Kapazität werden
    // gekappt. Was bleibt, je Tabelle (bei Gleichstand der größere
    // Schlüssel):
    // - Partitions- und uid-Reserve-Eimer: erst fallen volle Eimer
    //   (gleichwertig zu keinem, verlustfrei), dann die am längsten
    //   unbenutzten, wie in `make_room`. Ohne Uhr zählt nur, was schon beim
    //   Schnappschuss voll war. Ein gefallener Eimer startet beim nächsten
    //   Aufruf voll; das kostet höchstens dessen Kapazität, der globale Eimer
    //   und die Reserve bleiben unberührt.
    // - Erstsichtungen: die Laufzeit verdrängt keine lebende (sie nimmt bei
    //   voller Tabelle keine neue auf), es gilt die Reihenfolge der
    //   Aufnahme. Erst fallen Einträge, die relativ zur jüngsten Sichtung der
    //   Tabelle abgelaufen sind, dann die zuletzt aufgenommenen
    //   (`EstablishedPartition::retention_rank`). Es bleiben die am längsten
    //   etablierten; frische Partitionen eines Sturms vor dem Neustart
    //   verdrängen sie nicht. Ein gefallener Eintrag verliert nur sein
    //   Vorrecht (fail-closed); die volle Tabelle nimmt neue Partitionen wie
    //   zur Laufzeit erst nach Ablauf eines Eintrags.
    // - Vorrechte je uid: die Laufzeit verdrängt keines, dessen Intervall
    //   läuft (`take_privilege` gibt bei voller Tabelle keines). Es bleiben
    //   die jüngsten: jeder gefallene ist älter als jeder behaltene, sein
    //   Intervall endet also nicht später; solange die behaltenen laufen,
    //   ist die Tabelle voll und `take_privilege` gibt kein Vorrecht
    //   (fail-closed). Nur bei rückwärtslaufender Zeit kann eine gefallene
    //   uid ein Vorrechts-Token früher bekommen.
    pub(crate) fn import(config: BudgetConfig, state: BudgetState) -> Result<Self, DispatchError> {
        let BudgetState {
            global,
            mut partitions,
            mut uid_reserve,
            reserve_starved,
            reserve_since_unix,
            mut established,
            mut uid_privileged,
        } = state;
        let sensors_ok = partitions
            .keys()
            .chain(established.keys())
            .all(|key| is_sensor_id(&key.sensor));
        if !sensors_ok {
            return Err(DispatchError::InvalidState { what: "budget" });
        }
        let limit = config.max_partitions;
        retain_greatest(&mut partitions, limit, |bucket| {
            bucket.retention_rank(config.partition_capacity)
        });
        for bucket in partitions.values_mut() {
            *bucket = bucket.clamped(config.partition_capacity);
        }
        retain_greatest(&mut uid_reserve, limit, |bucket| {
            bucket.retention_rank(config.uid_reserve_capacity)
        });
        for bucket in uid_reserve.values_mut() {
            *bucket = bucket.clamped(config.uid_reserve_capacity);
        }
        // Ein Durchlauf ohne Speicher; nur bei leerer Tabelle `None`, dann
        // wird nichts gekürzt und der Rang nie gefragt.
        let newest_unix = established
            .values()
            .map(|entry| entry.last_seen_unix)
            .max()
            .unwrap_or(i64::MIN);
        retain_greatest(&mut established, limit, |entry| {
            entry.retention_rank(newest_unix)
        });
        retain_greatest(&mut uid_privileged, limit, |last| *last);
        Ok(Self {
            config,
            global: global.clamped(config.global_capacity),
            partitions,
            uid_reserve,
            reserve_starved,
            reserve_since_unix,
            established,
            uid_privileged,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    fn key_for(uid: u32, cgroup: u64) -> PartitionKey {
        PartitionKey {
            uid: Some(uid),
            cgroup_kid: Some(cgroup),
            sensor: "probe-bpf-0".to_owned(),
        }
    }

    fn key(cgroup: u64) -> PartitionKey {
        key_for(1000, cgroup)
    }

    fn refused(scope: BudgetScope, tripped: bool) -> BudgetDecision {
        BudgetDecision::Refused { scope, tripped }
    }

    const GRANTED: BudgetDecision = BudgetDecision::Granted;

    // Kurzform: ein `Standard`-Auftrag in Cgroup `cgroup` von uid 1000.
    fn standard(budget: &mut PartitionedBudget, cgroup: u64, now: i64) -> BudgetDecision {
        budget.try_consume(&key(cgroup), BudgetClass::Standard, now)
    }

    // Kurzform: ein `High`+`Observed`-Auftrag in Cgroup `cgroup` von uid 1000.
    fn high(budget: &mut PartitionedBudget, cgroup: u64, now: i64) -> BudgetDecision {
        budget.try_consume(&key(cgroup), BudgetClass::HighObserved, now)
    }

    // Kurzform: ein Auftrag der Klasse `class` von `uid` zur Zeit 0.
    fn as_uid(
        budget: &mut PartitionedBudget,
        uid: u32,
        cgroup: u64,
        class: BudgetClass,
    ) -> BudgetDecision {
        budget.try_consume(&key_for(uid, cgroup), class, 0)
    }

    #[test]
    fn test_partition_bucket_empties_and_refills() -> TestResult {
        let config = BudgetConfig {
            partition_capacity: 2,
            partition_refill_per_hour: 12,
            ..BudgetConfig::default()
        };
        let mut budget = PartitionedBudget::new(config);
        assert_eq!(standard(&mut budget, 1, 0), GRANTED);
        assert_eq!(standard(&mut budget, 1, 0), GRANTED);
        assert_ne!(standard(&mut budget, 1, 0), GRANTED);
        // 12 je Stunde = ein Token je 300 s.
        assert_ne!(standard(&mut budget, 1, 299), GRANTED);
        assert_eq!(standard(&mut budget, 1, 300), GRANTED);
        // Eine andere Partition hat ihren eigenen, vollen Eimer.
        assert_eq!(standard(&mut budget, 2, 300), GRANTED);
        Ok(())
    }

    #[test]
    fn test_reserve_is_only_for_high_observed() -> TestResult {
        let config = BudgetConfig {
            partition_capacity: 100,
            global_capacity: 4,
            global_refill_per_hour: 0,
            reserved_share_percent: 50,
            ..BudgetConfig::default()
        };
        let mut budget = PartitionedBudget::new(config);
        assert_eq!(standard(&mut budget, 1, 0), GRANTED);
        assert_eq!(standard(&mut budget, 1, 0), GRANTED);
        // Zwei Tokens übrig = die Reserve.
        assert_eq!(
            standard(&mut budget, 2, 0),
            refused(BudgetScope::Global, true)
        );
        assert_eq!(high(&mut budget, 2, 0), GRANTED);
        assert_eq!(high(&mut budget, 3, 0), GRANTED);
        // Die erschöpfte Reserve ist eine eigene Episode: ein Trip, obwohl
        // `Standard` die globale Episode schon ausgelöst hat.
        assert_eq!(high(&mut budget, 4, 0), refused(BudgetScope::Reserve, true));
        assert_eq!(
            high(&mut budget, 5, 0),
            refused(BudgetScope::Reserve, false)
        );
        assert_eq!(budget.global_tokens(), 0);
        assert_eq!(budget.uid_reserve_tokens(Some(1000)), Some(4));
        Ok(())
    }

    #[test]
    fn test_reserve_trip_is_rearmed_once_the_reserve_is_full_again() -> TestResult {
        // 3600 je Stunde = ein Token je Sekunde; Reserve 50 % von 4 = 2.
        let config = BudgetConfig {
            partition_capacity: 100,
            global_capacity: 4,
            global_refill_per_hour: 3_600,
            reserved_share_percent: 50,
            uid_reserve_capacity: 100,
            ..BudgetConfig::default()
        };
        let mut budget = PartitionedBudget::new(config);
        let decisions: Vec<BudgetDecision> =
            (0..6).map(|cgroup| high(&mut budget, cgroup, 0)).collect();
        assert_eq!(
            decisions,
            [
                GRANTED,
                GRANTED,
                GRANTED,
                GRANTED,
                refused(BudgetScope::Reserve, true),
                refused(BudgetScope::Reserve, false),
            ]
        );
        // Ein Token später: bewilligt, die Reserve ist aber nicht wieder voll.
        assert_eq!(high(&mut budget, 10, 1), GRANTED);
        assert_eq!(
            high(&mut budget, 11, 1),
            refused(BudgetScope::Reserve, false)
        );
        // Zwei Sekunden Ruhe: die Reserve (2 Tokens) ist wieder voll.
        assert_eq!(high(&mut budget, 12, 3), GRANTED);
        assert_eq!(high(&mut budget, 13, 3), GRANTED);
        assert_eq!(
            high(&mut budget, 14, 3),
            refused(BudgetScope::Reserve, true)
        );
        Ok(())
    }

    #[test]
    fn test_established_partition_reaches_the_reserve_outside_the_uid_quota() -> TestResult {
        // Reserve 50 % von 10 = 5 Tokens, je uid 1 Token, keine Nachfüllung.
        let config = BudgetConfig {
            partition_capacity: 100,
            global_capacity: 10,
            global_refill_per_hour: 0,
            reserved_share_percent: 50,
            uid_reserve_capacity: 1,
            uid_reserve_refill_per_hour: 0,
            established_reserve_interval_secs: 300,
            ..BudgetConfig::default()
        };
        let mut budget = PartitionedBudget::new(config);
        let victim = key(500);
        // Zwei Partitionen des Opfers (uid 1000) hatten lange vor dem Sturm
        // einen Befund.
        assert_eq!(high(&mut budget, 500, 0), GRANTED);
        assert_eq!(high(&mut budget, 501, 0), GRANTED);
        assert!(!budget.is_established(&victim, 0));
        // Der Angreifer derselben uid leert alles über der Reserve und seine
        // uid-Quote mit frisch angelegten Cgroups.
        let storm: Vec<BudgetDecision> = (1..=7)
            .map(|cgroup| high(&mut budget, cgroup, 300))
            .collect();
        assert_eq!(
            storm
                .iter()
                .filter(|decision| decision.is_granted())
                .count(),
            4
        );
        assert_eq!(storm.get(4), Some(&refused(BudgetScope::UidReserve, true)));
        assert!(budget.is_established(&victim, 300));
        assert!(!budget.is_established(&key(1), 300));
        // Das Opfer nimmt ein Token außerhalb der Quote …
        assert_eq!(high(&mut budget, 500, 310), GRANTED);
        // … aber nur eines je uid und Intervall, auch über mehrere etablierte
        // Partitionen …
        assert_eq!(
            high(&mut budget, 501, 320),
            refused(BudgetScope::UidReserve, false)
        );
        assert_eq!(
            high(&mut budget, 500, 609),
            refused(BudgetScope::UidReserve, false)
        );
        // … und nach dem Intervall wieder eines.
        assert_eq!(high(&mut budget, 500, 610), GRANTED);
        assert_eq!(
            high(&mut budget, 501, 611),
            refused(BudgetScope::UidReserve, false)
        );
        assert_eq!(budget.global_tokens(), 2);
        // Abgeschaltet: auch die etablierte Partition braucht die Quote.
        let mut off = PartitionedBudget::new(BudgetConfig {
            established_reserve_interval_secs: 0,
            ..config
        });
        assert_eq!(high(&mut off, 500, 0), GRANTED);
        for cgroup in 1..=7 {
            let _decision = high(&mut off, cgroup, 300);
        }
        assert_eq!(
            high(&mut off, 500, 310),
            refused(BudgetScope::UidReserve, false)
        );
        Ok(())
    }

    #[test]
    fn test_partitions_first_seen_with_the_episode_are_not_established() -> TestResult {
        let config = BudgetConfig {
            partition_capacity: 100,
            global_capacity: 4,
            global_refill_per_hour: 0,
            reserved_share_percent: 50,
            ..BudgetConfig::default()
        };
        let mut budget = PartitionedBudget::new(config);
        assert_eq!(high(&mut budget, 1, 7), GRANTED);
        assert_eq!(high(&mut budget, 2, 7), GRANTED);
        // Die Episode beginnt in derselben Sekunde, in der beide auftauchten.
        assert_eq!(high(&mut budget, 2, 7), GRANTED);
        assert!(!budget.is_established(&key(1), 7));
        assert!(!budget.is_established(&key(2), 7));
        assert_eq!(budget.uid_reserve_tokens(Some(1000)), Some(5));
        Ok(())
    }

    #[test]
    fn test_only_granted_calls_establish_a_partition() -> TestResult {
        // 3600 je Stunde = ein Token je Sekunde; Reserve 50 % von 4 = 2;
        // je uid ein Token Quote ohne Nachfüllung; Intervall 10 s.
        let config = BudgetConfig {
            partition_capacity: 100,
            global_capacity: 4,
            global_refill_per_hour: 3_600,
            reserved_share_percent: 50,
            uid_reserve_capacity: 1,
            uid_reserve_refill_per_hour: 0,
            established_reserve_interval_secs: 10,
            ..BudgetConfig::default()
        };
        let mut budget = PartitionedBudget::new(config);
        // Episode 1: zwei Tokens über der Reserve, eines aus der Quote; die
        // Partition 4 wird nur abgewiesen.
        assert_eq!(high(&mut budget, 1, 0), GRANTED);
        assert_eq!(high(&mut budget, 2, 0), GRANTED);
        assert_eq!(high(&mut budget, 3, 0), GRANTED);
        assert_eq!(
            high(&mut budget, 4, 0),
            refused(BudgetScope::UidReserve, true)
        );
        // Der Eimer erholt sich über die Reserve: die Episode endet.
        assert_eq!(high(&mut budget, 10, 3), GRANTED);
        // Episode 2 ab t=20: die nur abgewiesene Partition 4 ist nicht
        // etabliert …
        assert_eq!(high(&mut budget, 11, 20), GRANTED);
        assert_eq!(high(&mut budget, 12, 20), GRANTED);
        assert_eq!(
            high(&mut budget, 4, 20),
            refused(BudgetScope::UidReserve, false)
        );
        assert!(!budget.is_established(&key(4), 20));
        // … die vor 20 s bewilligte Partition 1 schon.
        assert!(budget.is_established(&key(1), 20));
        assert_eq!(high(&mut budget, 1, 20), GRANTED);
        Ok(())
    }

    // Uid 1001 bietet in den Cgroups 1..=24 je einen `High`+`Observed`-Befund
    // an; gezählt werden die Bewilligungen.
    fn attack_24(budget: &mut PartitionedBudget, now: i64) -> usize {
        (1..=24_u64)
            .filter(|cgroup| {
                budget
                    .try_consume(&key_for(1001, *cgroup), BudgetClass::HighObserved, now)
                    .is_granted()
            })
            .count()
    }

    #[test]
    fn test_one_uid_never_drains_the_reserve_however_many_partitions_it_reuses() -> TestResult {
        // Vorgaben: global 48, Reserve 12, uid-Quote 6, Intervall 300 s.
        let config = BudgetConfig::default();
        let reserve = config
            .reserve_units()
            .checked_div(TOKEN_UNITS)
            .ok_or_else(crate::test_support::missing("reserve"))?;
        let quota = u64::from(config.uid_reserve_capacity);
        let victim = key_for(1000, 500);

        // a) Dieselben 24 Cgroups eine Sekunde vor der Episode (Befund des
        //    Reviews): nicht etabliert, nur die uid-Quote greift.
        let mut quick = PartitionedBudget::new(config);
        assert_eq!(attack_24(&mut quick, 0), 24);
        assert_eq!(attack_24(&mut quick, 1), 12 + 6);
        assert_eq!(quick.global_tokens(), reserve.saturating_sub(quota));
        assert_eq!(
            quick.try_consume(&victim, BudgetClass::HighObserved, 2),
            GRANTED
        );

        // b) Dieselben 24 Cgroups lange vorher etabliert: über die Quote
        //    hinaus nur ein Vorrechts-Token für die ganze uid.
        let mut prepared = PartitionedBudget::new(config);
        assert_eq!(attack_24(&mut prepared, 0), 24);
        assert_eq!(attack_24(&mut prepared, 3_600), 24);
        assert_eq!(attack_24(&mut prepared, 3_601), 12 + 6 + 1);
        assert_eq!(
            prepared.global_tokens(),
            reserve.saturating_sub(quota).saturating_sub(1)
        );
        assert_eq!(prepared.uid_reserve_tokens(Some(1001)), Some(0));
        assert_eq!(attack_24(&mut prepared, 3_602), 0);
        // Der Vorrechts-Stand überlebt die Rundreise.
        let json = serde_json::to_string(&prepared.export())?;
        let mut restored = PartitionedBudget::import(config, serde_json::from_str(&json)?)?;
        assert_eq!(restored, prepared);
        assert!(!restored.uid_privileged.is_empty());
        assert_eq!(attack_24(&mut restored, 3_603), 0);
        // Ein `High`+`Observed`-Befund einer anderen uid kommt durch.
        assert_eq!(
            restored.try_consume(&victim, BudgetClass::HighObserved, 3_604),
            GRANTED
        );
        Ok(())
    }

    #[test]
    fn test_privilege_is_gated_per_partition_and_uid_and_fails_closed_when_full() -> TestResult {
        // Intervall 10 s, Platz für die Vorrechte zweier uids.
        let config = BudgetConfig {
            established_reserve_interval_secs: 10,
            max_partitions: 2,
            ..BudgetConfig::default()
        };
        let mut established = BTreeMap::new();
        for uid in 1..=3 {
            established.insert(key_for(uid, 1), EstablishedPartition::seen_at(0));
        }
        established.insert(key_for(1, 2), EstablishedPartition::seen_at(0));
        let mut uid_privileged = BTreeMap::new();
        // Eine Episode ab t=20: alle Partitionen sind etabliert.
        let mut take = |uid: u32, cgroup: u64, now: i64| {
            take_privilege(
                &config,
                &mut established,
                &mut uid_privileged,
                Some(20),
                &key_for(uid, cgroup),
                now,
            )
        };
        assert!(take(1, 1, 20));
        assert!(take(2, 1, 20));
        // Dieselbe uid über eine andere Partition: nicht vor Ablauf.
        assert!(!take(1, 2, 25));
        // Dieselbe Partition: ebenso.
        assert!(!take(1, 1, 25));
        // Voll mit laufenden Intervallen: kein Vorrecht für eine dritte uid.
        assert!(!take(3, 1, 21));
        // Nach dem Intervall fallen abgelaufene Einträge.
        assert!(take(3, 1, 30));
        assert!(take(1, 2, 31));
        assert_eq!(uid_privileged.len(), 2);
        // Ohne Episode oder abgeschaltet: nie.
        let mut entries = BTreeMap::from([(key_for(4, 1), EstablishedPartition::seen_at(0))]);
        let mut uids = BTreeMap::new();
        let partition = key_for(4, 1);
        assert!(!take_privilege(
            &config,
            &mut entries,
            &mut uids,
            None,
            &partition,
            50
        ));
        let off = BudgetConfig {
            established_reserve_interval_secs: 0,
            ..config
        };
        assert!(!take_privilege(
            &off,
            &mut entries,
            &mut uids,
            Some(20),
            &partition,
            50
        ));
        assert!(uids.is_empty());
        Ok(())
    }

    #[test]
    fn test_established_table_is_bounded_and_never_evicts_live_entries() -> TestResult {
        let config = BudgetConfig {
            max_partitions: 3,
            ..BudgetConfig::default()
        };
        let mut budget = PartitionedBudget::new(config);
        for cgroup in 0..10 {
            let _decision = standard(&mut budget, cgroup, 0);
        }
        assert_eq!(budget.established.len(), 3);
        assert!(budget.established.contains_key(&key(0)));
        assert!(!budget.established.contains_key(&key(9)));
        // Nach Ablauf der Ruhezeit macht die volle Tabelle Platz.
        let later = ESTABLISHED_TTL_SECS.saturating_add(1);
        let _decision = standard(&mut budget, 9, later);
        assert!(budget.established.contains_key(&key(9)));
        assert_eq!(budget.established.len(), 1);
        Ok(())
    }

    #[test]
    fn test_random_sequences_keep_every_bound() -> TestResult {
        // Deterministische Pseudo-Zufallsfolge (kein proptest im Workspace).
        let config = BudgetConfig {
            partition_capacity: 3,
            partition_refill_per_hour: 60,
            global_capacity: 12,
            global_refill_per_hour: 240,
            reserved_share_percent: 25,
            uid_reserve_capacity: 2,
            uid_reserve_refill_per_hour: 30,
            max_partitions: 16,
            ..BudgetConfig::default()
        };
        let reserve = config.reserve_units();
        let mut budget = PartitionedBudget::new(config);
        let mut now = 0_i64;
        let mut state = 0x2545_f491_u32;
        for _ in 0..5_000 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let pick = |shift: u32, modulus: u32| {
                state
                    .checked_shr(shift)
                    .unwrap_or(0)
                    .checked_rem(modulus)
                    .unwrap_or(0)
            };
            now = now.saturating_add(i64::from(pick(4, 7)));
            let class = if pick(8, 3) == 0 {
                BudgetClass::HighObserved
            } else {
                BudgetClass::Standard
            };
            let partition = key_for(
                1000_u32.saturating_add(pick(12, 3)),
                u64::from(pick(16, 24)),
            );
            let before = budget.global.units;
            let decision = budget.try_consume(&partition, class, now);
            assert!(budget.global.units <= capacity_units(config.global_capacity));
            assert!(budget.partitions.len() <= config.max_partitions);
            assert!(budget.established.len() <= config.max_partitions);
            assert!(budget.uid_privileged.len() <= config.max_partitions);
            if decision.is_granted() && class == BudgetClass::Standard {
                // `Standard` greift nie in die Reserve.
                assert!(
                    budget.global.units >= reserve,
                    "{before} -> {}",
                    budget.global.units
                );
            }
            if !decision.is_granted() {
                // Eine Abweisung verbraucht nichts (die Nachfüllung bleibt).
                assert!(budget.global.units >= before.min(capacity_units(config.global_capacity)));
            }
        }
        Ok(())
    }

    #[test]
    fn test_fractional_reserve_floor_is_never_rounded_down() -> TestResult {
        // 33 % von 10 Tokens = 3,3 Tokens Reserve: `Standard` braucht 4,3
        // (abgerundet wären es 4 und damit ein Token mehr).
        let config = BudgetConfig {
            partition_capacity: 100,
            global_capacity: 10,
            global_refill_per_hour: 0,
            reserved_share_percent: 33,
            ..BudgetConfig::default()
        };
        let mut budget = PartitionedBudget::new(config);
        let granted = (0..10)
            .filter(|cgroup| standard(&mut budget, *cgroup, 0).is_granted())
            .count();
        assert_eq!(granted, 6);
        assert_eq!(budget.global_tokens(), 4);
        Ok(())
    }

    #[test]
    fn test_uid_reserve_quota_protects_other_uids() -> TestResult {
        let config = BudgetConfig {
            partition_capacity: 100,
            global_capacity: 10,
            global_refill_per_hour: 0,
            reserved_share_percent: 50,
            uid_reserve_capacity: 2,
            uid_reserve_refill_per_hour: 0,
            ..BudgetConfig::default()
        };
        let mut budget = PartitionedBudget::new(config);
        // Der Angreifer (uid 1001) streut über viele Cgroups.
        let attacker: Vec<BudgetDecision> = (0..9)
            .map(|cgroup| as_uid(&mut budget, 1001, cgroup, BudgetClass::HighObserved))
            .collect();
        let granted = attacker
            .iter()
            .filter(|decision| decision.is_granted())
            .count();
        // Fünf Tokens über der Reserve plus zwei aus seiner uid-Reserve.
        assert_eq!(granted, 7);
        assert_eq!(
            attacker.get(7),
            Some(&refused(BudgetScope::UidReserve, true))
        );
        assert_eq!(
            attacker.get(8),
            Some(&refused(BudgetScope::UidReserve, false))
        );
        assert_eq!(budget.global_tokens(), 3);
        // Eine andere uid greift weiter in die Reserve, bis ihre Quote leer ist.
        let high_observed = BudgetClass::HighObserved;
        assert_eq!(as_uid(&mut budget, 1000, 50, high_observed), GRANTED);
        assert_eq!(as_uid(&mut budget, 1000, 51, high_observed), GRANTED);
        assert_eq!(
            as_uid(&mut budget, 1000, 52, high_observed),
            refused(BudgetScope::UidReserve, true)
        );
        assert_eq!(budget.global_tokens(), 1);
        // `Standard` kommt nie an die Reserve.
        assert_eq!(
            as_uid(&mut budget, 1002, 60, BudgetClass::Standard),
            refused(BudgetScope::Global, true)
        );
        Ok(())
    }

    #[test]
    fn test_trip_is_reported_once_per_episode_and_rearmed_when_full() -> TestResult {
        let config = BudgetConfig {
            partition_capacity: 2,
            partition_refill_per_hour: 12,
            ..BudgetConfig::default()
        };
        let mut budget = PartitionedBudget::new(config);
        let partition = key(1);
        let mut at = |now: i64| budget.try_consume(&partition, BudgetClass::Standard, now);
        assert!(at(0).is_granted());
        assert!(at(0).is_granted());
        assert_eq!(at(0), refused(BudgetScope::Partition, true));
        assert_eq!(at(1), refused(BudgetScope::Partition, false));
        // Ein Token nach 300 s: bewilligt, aber nicht voll: dieselbe Episode.
        assert!(at(300).is_granted());
        assert_eq!(at(300), refused(BudgetScope::Partition, false));
        // Nach 600 s Ruhe wieder voll: neue Episode, neuer Trip.
        assert!(at(900).is_granted());
        assert!(at(900).is_granted());
        assert_eq!(at(900), refused(BudgetScope::Partition, true));
        Ok(())
    }

    #[test]
    fn test_global_trip_is_rearmed_after_a_full_refill() -> TestResult {
        // 3600 je Stunde = ein Token je Sekunde; Reserve 25 % von 4 = 1.
        let config = BudgetConfig {
            partition_capacity: 100,
            global_capacity: 4,
            global_refill_per_hour: 3_600,
            ..BudgetConfig::default()
        };
        let mut budget = PartitionedBudget::new(config);
        let decisions: Vec<BudgetDecision> = (0..5)
            .map(|cgroup| budget.try_consume(&key(cgroup), BudgetClass::Standard, 10))
            .collect();
        assert_eq!(
            decisions,
            [
                BudgetDecision::Granted,
                BudgetDecision::Granted,
                BudgetDecision::Granted,
                refused(BudgetScope::Global, true),
                refused(BudgetScope::Global, false),
            ]
        );
        // Ein Token später: bewilligt, die Episode läuft weiter.
        assert_eq!(standard(&mut budget, 1, 11), GRANTED);
        assert_eq!(
            budget.try_consume(&key(1), BudgetClass::Standard, 11),
            refused(BudgetScope::Global, false)
        );
        // Drei Sekunden Ruhe: voll, neue Episode.
        for _ in 0..3 {
            assert_eq!(standard(&mut budget, 2, 14), GRANTED);
        }
        assert_eq!(
            budget.try_consume(&key(2), BudgetClass::Standard, 14),
            refused(BudgetScope::Global, true)
        );
        Ok(())
    }

    #[test]
    fn test_refill_is_exact_for_odd_rates() -> TestResult {
        // 7 je Stunde: ein Token nach 3600/7 = 514,29 s, also ab 515 s.
        let config = BudgetConfig {
            partition_capacity: 1,
            partition_refill_per_hour: 7,
            global_capacity: 1_000,
            ..BudgetConfig::default()
        };
        let mut stepping = PartitionedBudget::new(config);
        let mut jumping = PartitionedBudget::new(config);
        assert_eq!(standard(&mut stepping, 1, 0), GRANTED);
        assert_eq!(standard(&mut jumping, 1, 0), GRANTED);
        let first_grant = (1..=600_i64).find(|now| {
            stepping
                .try_consume(&key(1), BudgetClass::Standard, *now)
                .is_granted()
        });
        assert_eq!(first_grant, Some(515));
        assert_ne!(standard(&mut jumping, 1, 514), GRANTED);
        assert_eq!(standard(&mut jumping, 1, 515), GRANTED);
        Ok(())
    }

    #[test]
    fn test_global_never_exceeds_its_capacity() -> TestResult {
        let mut budget = PartitionedBudget::new(BudgetConfig::default());
        assert_eq!(standard(&mut budget, 1, 0), GRANTED);
        assert_eq!(standard(&mut budget, 2, 1_000_000_000), GRANTED);
        assert_eq!(budget.global_tokens(), 47);
        Ok(())
    }

    #[test]
    fn test_backwards_clock_grants_nothing() -> TestResult {
        let config = BudgetConfig {
            partition_capacity: 1,
            partition_refill_per_hour: 3_600,
            ..BudgetConfig::default()
        };
        let mut budget = PartitionedBudget::new(config);
        assert_eq!(standard(&mut budget, 1, 1_000), GRANTED);
        assert_ne!(standard(&mut budget, 1, 10), GRANTED);
        assert_eq!(standard(&mut budget, 1, 11), GRANTED);
        Ok(())
    }

    #[test]
    fn test_partition_count_is_bounded() -> TestResult {
        let config = BudgetConfig {
            max_partitions: 3,
            ..BudgetConfig::default()
        };
        let mut budget = PartitionedBudget::new(config);
        for cgroup in 0..10 {
            assert!(
                budget
                    .try_consume(&key(cgroup), BudgetClass::HighObserved, 0)
                    .is_granted()
            );
        }
        assert!(budget.partitions.len() <= 3);
        Ok(())
    }

    #[test]
    fn test_eviction_keeps_the_recently_used_bucket() -> TestResult {
        // Ohne Nachfüllung bleibt jeder benutzte Eimer unvoll; verdrängt
        // wird der am längsten unbenutzte, nicht der kleinste Schlüssel.
        let config = BudgetConfig {
            partition_capacity: 3,
            partition_refill_per_hour: 0,
            max_partitions: 2,
            ..BudgetConfig::default()
        };
        let mut budget = PartitionedBudget::new(config);
        assert_eq!(standard(&mut budget, 1, 0), GRANTED);
        assert_eq!(standard(&mut budget, 2, 1), GRANTED);
        assert_eq!(standard(&mut budget, 1, 2), GRANTED);
        assert_eq!(standard(&mut budget, 3, 3), GRANTED);
        assert_eq!(budget.partition_tokens(&key(1)), Some(1));
        assert_eq!(budget.partition_tokens(&key(2)), None);
        assert_eq!(budget.partition_tokens(&key(3)), Some(2));
        Ok(())
    }

    #[test]
    fn test_full_buckets_are_evicted_before_used_ones() -> TestResult {
        let config = BudgetConfig {
            partition_capacity: 1,
            partition_refill_per_hour: 3_600,
            max_partitions: 2,
            ..BudgetConfig::default()
        };
        let mut budget = PartitionedBudget::new(config);
        // Partition 1 ist bei t=5 längst wieder voll, Partition 2 nicht.
        assert_eq!(standard(&mut budget, 1, 0), GRANTED);
        assert_eq!(standard(&mut budget, 2, 5), GRANTED);
        assert_eq!(standard(&mut budget, 3, 5), GRANTED);
        assert_eq!(budget.partition_tokens(&key(1)), None);
        assert_eq!(budget.partition_tokens(&key(2)), Some(0));
        Ok(())
    }

    #[test]
    fn test_state_round_trip_preserves_every_decision() -> TestResult {
        let config = BudgetConfig {
            partition_capacity: 2,
            global_capacity: 8,
            global_refill_per_hour: 60,
            reserved_share_percent: 50,
            uid_reserve_capacity: 2,
            established_reserve_interval_secs: 20,
            ..BudgetConfig::default()
        };
        let mut original = PartitionedBudget::new(config);
        let script = |budget: &mut PartitionedBudget, start: i64| -> Vec<BudgetDecision> {
            (0..24_u64)
                .map(|n| {
                    let class = if n.checked_rem(3) == Some(0) {
                        BudgetClass::Standard
                    } else {
                        BudgetClass::HighObserved
                    };
                    let parity = u32::try_from(n.checked_rem(2).unwrap_or(0)).unwrap_or(0);
                    let uid = 1000_u32.saturating_add(parity);
                    let cgroup = n.checked_rem(5).unwrap_or(0);
                    let now = start.saturating_add(i64::try_from(n.saturating_mul(7)).unwrap_or(0));
                    budget.try_consume(&key_for(uid, cgroup), class, now)
                })
                .collect()
        };
        let before = script(&mut original, 0);
        assert!(before.iter().any(|decision| !decision.is_granted()));
        let json = serde_json::to_string(&original.export())?;
        let state: BudgetState = serde_json::from_str(&json)?;
        assert_eq!(state, original.export());
        let mut restored = PartitionedBudget::import(config, state)?;
        assert_eq!(restored, original);
        assert_eq!(script(&mut restored, 400), script(&mut original, 400));
        assert_eq!(restored.export(), original.export());
        Ok(())
    }

    #[test]
    fn test_import_clamps_to_capacity_and_rejects_bad_sensors() -> TestResult {
        let large = BudgetConfig {
            partition_capacity: 50,
            global_capacity: 500,
            uid_reserve_capacity: 50,
            reserved_share_percent: 100,
            ..BudgetConfig::default()
        };
        let mut budget = PartitionedBudget::new(large);
        assert_eq!(high(&mut budget, 1, 0), GRANTED);
        let small = BudgetConfig::default();
        let restored = PartitionedBudget::import(small, budget.export())?;
        assert_eq!(restored.global_tokens(), 48);
        assert_eq!(restored.partition_tokens(&key(1)), Some(6));
        assert_eq!(restored.uid_reserve_tokens(Some(1000)), Some(6));
        let evil = PartitionKey {
            uid: None,
            cgroup_kid: None,
            sensor: "evil sensor".to_owned(),
        };
        let mut tampered = budget.export();
        tampered
            .partitions
            .insert(evil.clone(), TokenBucket::full(1));
        assert_eq!(
            PartitionedBudget::import(small, tampered).err(),
            Some(DispatchError::InvalidState { what: "budget" })
        );
        let mut tampered = budget.export();
        tampered
            .established
            .insert(evil, EstablishedPartition::seen_at(0));
        assert_eq!(
            PartitionedBudget::import(small, tampered).err(),
            Some(DispatchError::InvalidState { what: "budget" })
        );
        Ok(())
    }

    // Die Schlüssel einer Tabelle in aufsteigender Reihenfolge.
    fn keys_of<K: Clone, V>(map: &BTreeMap<K, V>) -> Vec<K> {
        map.keys().cloned().collect()
    }

    #[test]
    fn test_import_prunes_every_table_to_the_active_bound() -> TestResult {
        // Ohne Nachfüllung bleibt jeder benutzte Eimer unvoll.
        let wide = BudgetConfig {
            partition_capacity: 3,
            partition_refill_per_hour: 0,
            ..BudgetConfig::default()
        };
        let mut budget = PartitionedBudget::new(wide);
        for cgroup in 1..=6_u64 {
            let now = i64::try_from(cgroup.saturating_mul(10)).unwrap_or(0);
            assert_eq!(standard(&mut budget, cgroup, now), GRANTED);
        }
        let mut state = budget.export();
        // uid-Eimer und Vorrechte je uid direkt (der Weg in die Reserve ist
        // oben getestet): uid n zuletzt bei `at`.
        for (uid, at) in [(1_u32, 40_i64), (2, 10), (3, 30), (4, 20)] {
            state.uid_reserve.insert(
                Some(uid),
                TokenBucket {
                    units: 0,
                    last_refill_unix: Some(at),
                    starved: true,
                },
            );
            state.uid_privileged.insert(Some(uid), at);
        }
        // Erstsichtungen: 1 und 2 am längsten etabliert, 5 und 6 zuletzt
        // gesehen, aber zuletzt aufgenommen (alle leben).
        let first_sightings = [
            (1_u64, 1_i64, 15_i64),
            (2, 2, 25),
            (5, 50, 90),
            (6, 60, 100),
        ];
        for (cgroup, first, last) in first_sightings {
            state.established.insert(
                key(cgroup),
                EstablishedPartition {
                    first_seen_unix: first,
                    last_seen_unix: last,
                    last_privileged_unix: None,
                },
            );
        }
        // Unverändert unter derselben Grenze.
        let same = PartitionedBudget::import(wide, state.clone())?;
        assert_eq!(same.export(), state);
        // Konfigurationswechsel 4096 → 2: Eimer und Vorrechte die zuletzt
        // benutzten, Erstsichtungen die zuerst aufgenommenen (die Laufzeit
        // verdrängt keine lebende).
        let narrow = BudgetConfig {
            max_partitions: 2,
            ..wide
        };
        let restored = PartitionedBudget::import(narrow, state.clone())?;
        assert_eq!(keys_of(&restored.partitions), [key(5), key(6)]);
        assert_eq!(keys_of(&restored.established), [key(1), key(2)]);
        assert_eq!(
            restored.established.get(&key(1)),
            state.established.get(&key(1))
        );
        assert_eq!(keys_of(&restored.uid_reserve), [Some(1), Some(3)]);
        assert_eq!(keys_of(&restored.uid_privileged), [Some(1), Some(3)]);
        assert_eq!(restored.partition_tokens(&key(6)), Some(2));
        Ok(())
    }

    #[test]
    fn test_import_keeps_the_earliest_admitted_live_first_sightings() -> TestResult {
        let wide = BudgetConfig::default();
        let mut state = PartitionedBudget::new(wide).export();
        let late = ESTABLISHED_TTL_SECS.saturating_add(1_000);
        // Partition 1 ist am frühesten gesehen, aber relativ zur jüngsten
        // Sichtung (`late`) abgelaufen; 2 bis 5 leben, 3 und 5 sind gleich
        // lange etabliert.
        let first_sightings = [
            (1_u64, 0_i64, 5_i64),
            (2, 100, late),
            (3, 200, late.saturating_sub(100)),
            (4, 300, late),
            (5, 200, late),
        ];
        for (cgroup, first, last) in first_sightings {
            state.established.insert(
                key(cgroup),
                EstablishedPartition {
                    first_seen_unix: first,
                    last_seen_unix: last,
                    last_privileged_unix: None,
                },
            );
        }
        let bounded = |max_partitions: usize| BudgetConfig {
            max_partitions,
            ..wide
        };
        // Grenze 3: die abgelaufene fällt trotz frühester Sichtung, dann die
        // zuletzt aufgenommene (4); beim Gleichstand von 3 und 5 bleibt die
        // größere vor der kleineren.
        let three = PartitionedBudget::import(bounded(3), state.clone())?;
        assert_eq!(keys_of(&three.established), [key(2), key(3), key(5)]);
        let two = PartitionedBudget::import(bounded(2), state.clone())?;
        assert_eq!(keys_of(&two.established), [key(2), key(5)]);
        // Grenze 1: die am längsten etablierte; solange sie lebt, nimmt die
        // volle Tabelle wie zur Laufzeit keine frische Partition auf.
        let mut one = PartitionedBudget::import(bounded(1), state)?;
        assert_eq!(keys_of(&one.established), [key(2)]);
        one.note_partition(&key(9), late);
        assert_eq!(keys_of(&one.established), [key(2)]);
        Ok(())
    }

    #[test]
    fn test_import_drops_full_buckets_before_used_ones() -> TestResult {
        let wide = BudgetConfig {
            partition_capacity: 3,
            partition_refill_per_hour: 0,
            ..BudgetConfig::default()
        };
        let mut budget = PartitionedBudget::new(wide);
        assert_eq!(standard(&mut budget, 1, 10), GRANTED);
        assert_eq!(standard(&mut budget, 2, 20), GRANTED);
        let mut state = budget.export();
        // Partition 3: voll (gleichwertig zu keinem Eimer), aber zuletzt
        // benutzt; Partition 4: unvoll, aber nie benutzt.
        state.partitions.insert(
            key(3),
            TokenBucket {
                units: capacity_units(3),
                last_refill_unix: Some(1_000),
                starved: false,
            },
        );
        state.partitions.insert(
            key(4),
            TokenBucket {
                units: TOKEN_UNITS,
                last_refill_unix: None,
                starved: false,
            },
        );
        let three = BudgetConfig {
            max_partitions: 3,
            ..wide
        };
        let restored = PartitionedBudget::import(three, state.clone())?;
        assert_eq!(keys_of(&restored.partitions), [key(1), key(2), key(4)]);
        let two = BudgetConfig {
            max_partitions: 2,
            ..wide
        };
        let restored = PartitionedBudget::import(two, state)?;
        assert_eq!(keys_of(&restored.partitions), [key(1), key(2)]);
        Ok(())
    }

    #[test]
    fn test_pruned_privileges_stay_fail_closed() -> TestResult {
        let wide = BudgetConfig {
            established_reserve_interval_secs: 10,
            ..BudgetConfig::default()
        };
        let mut state = PartitionedBudget::new(wide).export();
        for (uid, at) in [(1_u32, 20_i64), (2, 21), (3, 22), (4, 23)] {
            state.uid_privileged.insert(Some(uid), at);
        }
        let narrow = BudgetConfig {
            max_partitions: 2,
            ..wide
        };
        let mut restored = PartitionedBudget::import(narrow, state)?;
        assert_eq!(keys_of(&restored.uid_privileged), [Some(3), Some(4)]);
        // uid 1 ist gefallen; ihr Intervall (ab 20) liefe bis 30.
        let mut established = BTreeMap::from([(key_for(1, 1), EstablishedPartition::seen_at(0))]);
        let mut take = |now: i64| {
            take_privilege(
                &narrow,
                &mut established,
                &mut restored.uid_privileged,
                Some(20),
                &key_for(1, 1),
                now,
            )
        };
        // Solange die behaltenen, jüngeren Intervalle laufen, ist die Tabelle
        // voll: kein Vorrecht.
        assert!(!take(25));
        // Ab 32 ist das älteste behaltene (22) abgelaufen, das gefallene (20)
        // also erst recht.
        assert!(take(32));
        Ok(())
    }
}
