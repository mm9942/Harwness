//! Gruppierung zu Vorfällen: [`IncidentGrouper`].
//!
//! # Zweck
//! Zwei oder mehr **verschiedene** Regeln auf derselben Cgroup (`cgroup_kid`)
//! innerhalb von 600 s Ereigniszeit bilden einen Vorfall (README §3.3,
//! B11: ein Eingriff erzeugt neue Anomalien, das soll ein Verdict bleiben).
//! Der Grouper führt nur die Mitgliedschaft; welcher Triage-Auftrag zu
//! einem Vorfall gehört, trägt der [`crate::Dispatcher`] über
//! [`IncidentGrouper::bind_job`] ein.
//!
//! # Rolle
//! Ein Vorfall wird von der Rolle seines **schwersten** Befunds triagiert
//! (README §3.3: „gruppiert, Rolle des höchsten Befunds"). Der Rang folgt
//! derselben Ordnung wie der schwerste Befund eines Auftrags: schwerer, dann
//! härter, dann älter; bei vollem Gleichstand bleibt der zuerst gesehene.
//! [`Grouping::Formed`] und [`Grouping::Joined`] nennen die Rolle nach
//! Aufnahme des Befunds.
//!
//! # Fenster
//! - Mitgliedschaft nach **Ereigniszeit** (`observed_at_unix`): zwei Befunde
//!   gehören zusammen, wenn ihr Abstand höchstens das Fenster beträgt
//!   (Grenze eingeschlossen). Ein Vorfall nimmt neue Mitglieder auf, solange
//!   sie höchstens ein Fenster nach dem jüngsten bisherigen **und** höchstens
//!   ein Fenster nach der Bildung des Vorfalls kommen. Die zweite Grenze ist
//!   fest: ein Angreifer kann einen Vorfall nicht mit immer neuen Befunden
//!   beliebig lange offen halten.
//! - Aufräumen nach **Dispatcher-Zeit** (`now_unix`): eine Cgroup, die zwei
//!   Fenster lang nichts gemeldet hat, wird vergessen. Höchstens
//!   `max_cgroups` Cgroups werden geführt (die am längsten ruhige fällt
//!   zuerst, bei Gleichstand die kleinere Kernel-ID; ebenso beim Laden
//!   eines Zustands mit mehr Cgroups), je Cgroup höchstens
//!   [`MAX_ENTRIES_PER_CGROUP`] Einzelbefunde.
//!
//! # Nebenläufigkeit und Fehler
//! `&mut self`, deterministisch, ohne Uhr; keine Fehler außer beim Laden
//! eines Zustands ([`crate::DispatchError::InvalidState`]).
//!
//! # Examples
//! ```
//! use harw_dod_dispatch::{
//!     GroupInput, Grouping, Hardness, IncidentGrouper, Severity, TriageRole, RULE_EGRESS_FLOW,
//!     RULE_EXEC_WRITABLE_PATH,
//! };
//!
//! let mut grouper = IncidentGrouper::new(600, 64);
//! let first = GroupInput {
//!     cgroup_kid: 7,
//!     rule_id: RULE_EGRESS_FLOW,
//!     finding_id: "3f2c9a10-7b1e-4c2d-9e8f-0a1b2c3d4e5f",
//!     role: TriageRole::Egress,
//!     severity: Severity::Medium,
//!     hardness: Hardness::Observed,
//!     observed_at_unix: 1_000,
//! };
//! assert_eq!(grouper.observe(first, 1_000), Grouping::Standalone);
//! let second = GroupInput {
//!     rule_id: RULE_EXEC_WRITABLE_PATH,
//!     finding_id: "4a2c9a10-7b1e-4c2d-9e8f-0a1b2c3d4e5f",
//!     role: TriageRole::Endpoint,
//!     severity: Severity::High,
//!     observed_at_unix: 1_300,
//!     ..first
//! };
//! // Der schwerere Befund bestimmt die Rolle des Vorfalls.
//! assert!(matches!(
//!     grouper.observe(second, 1_300),
//!     Grouping::Formed { role: TriageRole::Endpoint, .. }
//! ));
//! ```

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::DispatchError;
use crate::finding::{Hardness, Severity, is_uuid_v4};
use crate::job::TriageJobId;
use crate::prune::retain_greatest;
use crate::route::{Route, RouteTable, TriageRole};

/// Höchstzahl der Einzelbefunde, die je Cgroup auf einen Partner warten.
pub const MAX_ENTRIES_PER_CGROUP: usize = 32;

/// Kennung eines Vorfalls; vom Grouper fortlaufend ab 1 vergeben.
///
/// # Examples
/// ```
/// use harw_dod_dispatch::IncidentId;
///
/// assert_eq!(IncidentId::from_u64(4).to_string(), "incident-4");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IncidentId(u64);

impl IncidentId {
    /// Kennung aus ihrer Zahl.
    ///
    /// # Arguments
    /// - `value` (`u64`): die Zahl.
    ///
    /// # Returns
    /// Die Kennung.
    #[must_use]
    pub const fn from_u64(value: u64) -> Self {
        Self(value)
    }

    /// Die Zahl der Kennung.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl fmt::Display for IncidentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "incident-{}", self.0)
    }
}

/// Ein Befund, wie ihn der Grouper sieht.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupInput<'a> {
    /// Kernel-ID der Cgroup.
    pub cgroup_kid: u64,
    /// Regel-Kennung (Klasse des Befunds).
    pub rule_id: &'a str,
    /// Befund-Kennung.
    pub finding_id: &'a str,
    /// Triage-Rolle der Regel (Route des Befunds).
    pub role: TriageRole,
    /// Wirksame Schwere.
    pub severity: Severity,
    /// Härte des Belegs.
    pub hardness: Hardness,
    /// Ereigniszeit in Unix-Sekunden.
    pub observed_at_unix: i64,
}

impl GroupInput<'_> {
    // Rang und Rolle dieses Befunds.
    fn rank(&self) -> MemberRank {
        MemberRank {
            role: self.role,
            severity: self.severity,
            hardness: self.hardness,
            observed_at_unix: self.observed_at_unix,
        }
    }
}

/// Ergebnis von [`IncidentGrouper::observe`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grouping {
    /// Kein Vorfall; der Befund steht für sich.
    Standalone,
    /// Ein neuer Vorfall aus diesem und den genannten früheren Befunden.
    Formed {
        /// Kennung des neuen Vorfalls.
        incident_id: IncidentId,
        /// Frühere Befunde derselben Cgroup im Fenster.
        earlier_finding_ids: Vec<String>,
        /// Rolle des schwersten Befunds des Vorfalls (siehe Moduldoku).
        role: TriageRole,
    },
    /// Der Befund tritt einem offenen Vorfall bei.
    Joined {
        /// Kennung des Vorfalls.
        incident_id: IncidentId,
        /// Zuletzt gebundener Auftrag des Vorfalls, falls einer existiert.
        job: Option<TriageJobId>,
        /// Höchste Schwere im Vorfall **vor** diesem Befund.
        previous_max_severity: Severity,
        /// Rolle des schwersten Befunds des Vorfalls **mit** diesem Befund.
        role: TriageRole,
    },
}

// Rang eines Befunds im Vorfall und die Rolle seiner Regel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MemberRank {
    // Triage-Rolle der Regel.
    role: TriageRole,
    // Wirksame Schwere.
    severity: Severity,
    // Härte.
    hardness: Hardness,
    // Ereigniszeit.
    observed_at_unix: i64,
}

impl MemberRank {
    // Echt höher als `other`: schwerer, dann härter, dann älter (wie der
    // schwerste Befund eines Auftrags). Gleichstand ist `false`, der zuerst
    // gesehene bleibt.
    fn outranks(&self, other: &Self) -> bool {
        self.severity
            .cmp(&other.severity)
            .then(self.hardness.strength().cmp(&other.hardness.strength()))
            .then(other.observed_at_unix.cmp(&self.observed_at_unix))
            == Ordering::Greater
    }
}

// Ein Einzelbefund, der noch auf einen Partner einer anderen Regel wartet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GroupEntry {
    // Regel-Kennung.
    rule_id: String,
    // Befund-Kennung.
    finding_id: String,
    // Rang und Rolle.
    rank: MemberRank,
}

// Ein offener Vorfall einer Cgroup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OpenIncident {
    // Kennung.
    incident_id: IncidentId,
    // Schwerster Befund aller Mitglieder (Schwere und Rolle des Vorfalls).
    primary: MemberRank,
    // Jüngste Ereigniszeit aller Mitglieder.
    last_observed_at_unix: i64,
    // Feste Obergrenze der Ereigniszeit neuer Mitglieder: Bildung (jüngstes
    // Mitglied bei der Bildung) plus ein Fenster.
    closes_at_unix: i64,
    // Zuletzt gebundener Auftrag.
    job: Option<TriageJobId>,
}

// Zustand einer Cgroup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CgroupWindow {
    // Wartende Einzelbefunde (ohne offenen Vorfall).
    entries: Vec<GroupEntry>,
    // Offener Vorfall.
    incident: Option<OpenIncident>,
    // Letzte Beobachtung in Dispatcher-Zeit.
    touched_unix: i64,
}

// Persistierbarer Inhalt eines `IncidentGrouper` (ohne Konfiguration).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GrouperState {
    // Nächste Vorfall-Nummer.
    next_incident: u64,
    // Cgroups nach Kernel-ID.
    #[serde(with = "crate::serde_map")]
    cgroups: BTreeMap<u64, CgroupWindow>,
}

// Abstand zweier Ereigniszeiten höchstens `window` Sekunden.
fn within(a: i64, b: i64, window: u64) -> bool {
    a.abs_diff(b) <= window
}

/// Führt Vorfälle je Cgroup (siehe Moduldoku).
///
/// # Examples
/// Siehe Moduldoku.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncidentGrouper {
    // Fensterbreite in Sekunden.
    window_secs: u64,
    // Höchstzahl geführter Cgroups.
    max_cgroups: usize,
    // Nächste Vorfall-Nummer.
    next_incident: u64,
    // Zeit des letzten Aufräumens; je Sekunde höchstens ein Durchlauf.
    last_prune_unix: Option<i64>,
    // Cgroups nach Kernel-ID.
    cgroups: BTreeMap<u64, CgroupWindow>,
}

impl IncidentGrouper {
    /// Vorgabe der Fensterbreite: 10 Minuten (README §3.3).
    pub const DEFAULT_WINDOW_SECS: u32 = 600;

    /// Leerer Grouper.
    ///
    /// # Arguments
    /// - `window_secs` (`u32`): Fensterbreite in Sekunden.
    /// - `max_cgroups` (`usize`): Höchstzahl geführter Cgroups.
    ///
    /// # Returns
    /// Den Grouper ohne Vorfälle.
    #[must_use]
    pub fn new(window_secs: u32, max_cgroups: usize) -> Self {
        Self {
            window_secs: u64::from(window_secs),
            max_cgroups,
            next_incident: 1,
            last_prune_unix: None,
            cgroups: BTreeMap::new(),
        }
    }

    /// Anzahl der aktuell geführten Cgroups.
    #[must_use]
    pub fn tracked_cgroups(&self) -> usize {
        self.cgroups.len()
    }

    /// Ordnet einen Befund ein.
    ///
    /// # Description
    /// 1. Ein offener Vorfall der Cgroup, dessen jüngstes Mitglied höchstens
    ///    ein Fenster entfernt ist und dessen feste Obergrenze (Bildung plus
    ///    ein Fenster) der Befund nicht überschreitet, nimmt ihn auf
    ///    ([`Grouping::Joined`]); ist der Befund schwerer als der bisher
    ///    schwerste, geht die Rolle des Vorfalls auf ihn über. Sonst ist der
    ///    Vorfall geschlossen.
    /// 2. Sonst bildet der Befund mit den wartenden Einzelbefunden im Fenster
    ///    einen neuen Vorfall, wenn mindestens einer eine **andere** Regel hat
    ///    ([`Grouping::Formed`]).
    /// 3. Sonst wartet er als Einzelbefund ([`Grouping::Standalone`]).
    ///
    /// # Arguments
    /// - `input` (`GroupInput`): der Befund.
    /// - `now_unix` (`i64`): Dispatcher-Zeit für das Aufräumen.
    ///
    /// # Returns
    /// Die Einordnung.
    pub fn observe(&mut self, input: GroupInput<'_>, now_unix: i64) -> Grouping {
        self.prune(now_unix);
        if !self.cgroups.contains_key(&input.cgroup_kid) {
            self.make_room();
        }
        let window = self.window_secs;
        let slot = self
            .cgroups
            .entry(input.cgroup_kid)
            .or_insert_with(|| CgroupWindow {
                entries: Vec::new(),
                incident: None,
                touched_unix: now_unix,
            });
        slot.touched_unix = now_unix;
        if slot.incident.as_ref().is_some_and(|incident| {
            !within(
                incident.last_observed_at_unix,
                input.observed_at_unix,
                window,
            ) || input.observed_at_unix > incident.closes_at_unix
        }) {
            slot.incident = None;
        }
        if let Some(incident) = slot.incident.as_mut() {
            let previous_max_severity = incident.primary.severity;
            let rank = input.rank();
            if rank.outranks(&incident.primary) {
                incident.primary = rank;
            }
            incident.last_observed_at_unix =
                incident.last_observed_at_unix.max(input.observed_at_unix);
            return Grouping::Joined {
                incident_id: incident.incident_id,
                job: incident.job,
                previous_max_severity,
                role: incident.primary.role,
            };
        }
        slot.entries
            .retain(|entry| within(entry.rank.observed_at_unix, input.observed_at_unix, window));
        if slot
            .entries
            .iter()
            .any(|entry| entry.rule_id != input.rule_id)
        {
            let incident_id = IncidentId(self.next_incident);
            self.next_incident = self.next_incident.saturating_add(1);
            // Die wartenden Befunde in Aufnahmereihenfolge, dann dieser: bei
            // Gleichstand bleibt der zuerst gesehene der schwerste.
            let mut primary: Option<MemberRank> = None;
            let mut last_observed_at_unix = input.observed_at_unix;
            let mut earlier_finding_ids = Vec::with_capacity(slot.entries.len());
            for entry in slot.entries.drain(..) {
                if primary.is_none_or(|current| entry.rank.outranks(&current)) {
                    primary = Some(entry.rank);
                }
                last_observed_at_unix = last_observed_at_unix.max(entry.rank.observed_at_unix);
                earlier_finding_ids.push(entry.finding_id);
            }
            let rank = input.rank();
            let primary = match primary {
                Some(current) if !rank.outranks(&current) => current,
                _ => rank,
            };
            let window_i64 = i64::try_from(window).unwrap_or(i64::MAX);
            slot.incident = Some(OpenIncident {
                incident_id,
                primary,
                last_observed_at_unix,
                closes_at_unix: last_observed_at_unix.saturating_add(window_i64),
                job: None,
            });
            return Grouping::Formed {
                incident_id,
                earlier_finding_ids,
                role: primary.role,
            };
        }
        slot.entries.push(GroupEntry {
            rule_id: input.rule_id.to_owned(),
            finding_id: input.finding_id.to_owned(),
            rank: input.rank(),
        });
        while slot.entries.len() > MAX_ENTRIES_PER_CGROUP {
            let oldest = slot
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.rank.observed_at_unix)
                .map(|(position, _)| position);
            match oldest {
                Some(position) if position < slot.entries.len() => {
                    slot.entries.remove(position);
                }
                _ => break,
            }
        }
        Grouping::Standalone
    }

    /// Bindet einen Triage-Auftrag an einen offenen Vorfall.
    ///
    /// # Arguments
    /// - `cgroup_kid` (`u64`): Cgroup des Vorfalls.
    /// - `incident_id` (`IncidentId`): der Vorfall.
    /// - `job` (`TriageJobId`): der Auftrag.
    pub fn bind_job(&mut self, cgroup_kid: u64, incident_id: IncidentId, job: TriageJobId) {
        if let Some(slot) = self.cgroups.get_mut(&cgroup_kid)
            && let Some(incident) = slot.incident.as_mut()
            && incident.incident_id == incident_id
        {
            incident.job = Some(job);
        }
    }

    /// Löst einen Auftrag von seinem Vorfall (verdrängt oder fehlgeschlagen),
    /// damit das nächste Mitglied einen neuen Auftrag bekommt.
    ///
    /// # Arguments
    /// - `job` (`TriageJobId`): der Auftrag.
    pub fn unbind_job(&mut self, job: TriageJobId) {
        for slot in self.cgroups.values_mut() {
            if let Some(incident) = slot.incident.as_mut()
                && incident.job == Some(job)
            {
                incident.job = None;
            }
        }
    }

    // Löst jede Bindung, für die `keep(Vorfall, Auftrag)` `false` liefert
    // (Prüfung beim Laden: nur Bindungen an existierende Aufträge desselben
    // Vorfalls bleiben).
    pub(crate) fn retain_bindings<F>(&mut self, mut keep: F)
    where
        F: FnMut(IncidentId, TriageJobId) -> bool,
    {
        for slot in self.cgroups.values_mut() {
            if let Some(incident) = slot.incident.as_mut()
                && let Some(job) = incident.job
                && !keep(incident.incident_id, job)
            {
                incident.job = None;
            }
        }
    }

    // Nächste Vorfall-Nummer (Prüfung beim Laden).
    pub(crate) fn next_incident(&self) -> u64 {
        self.next_incident
    }

    // Vergisst Cgroups, die zwei Fenster lang ruhig waren.
    fn prune(&mut self, now_unix: i64) {
        if self.last_prune_unix == Some(now_unix) {
            return;
        }
        self.last_prune_unix = Some(now_unix);
        let retention = i64::try_from(self.window_secs.saturating_mul(2)).unwrap_or(i64::MAX);
        self.cgroups
            .retain(|_, slot| now_unix.saturating_sub(slot.touched_unix) <= retention);
    }

    // Macht Platz für eine neue Cgroup (die am längsten ruhige fällt).
    fn make_room(&mut self) {
        while self.cgroups.len() >= self.max_cgroups {
            let quietest = self
                .cgroups
                .iter()
                .min_by_key(|(_, slot)| slot.touched_unix)
                .map(|(kid, _)| *kid);
            match quietest {
                Some(kid) => {
                    self.cgroups.remove(&kid);
                }
                None => break,
            }
        }
    }

    // Persistierbarer Inhalt.
    pub(crate) fn export(&self) -> GrouperState {
        GrouperState {
            next_incident: self.next_incident,
            cgroups: self.cgroups.clone(),
        }
    }

    // Baut den Grouper aus einem Zustand: prüft jede Cgroup (auch die, die
    // danach fällt) und kürzt dann auf `max_cgroups` (Grenze der aktiven
    // Konfiguration). Es bleiben die zuletzt beobachteten, bei Gleichstand die
    // größere Kernel-ID (dieselbe Ordnung wie `make_room`). Eine gefallene
    // Cgroup ist wie eine verdrängte: ihr offener Vorfall nimmt keine
    // Mitglieder mehr auf, schon eingereihte Aufträge behalten ihre
    // Vorfall-Kennung, und `next_incident` bleibt, eine Kennung wird also nie
    // doppelt vergeben. Mehr als `MAX_ENTRIES_PER_CGROUP` wartende Befunde
    // kann es zur Laufzeit nicht geben; das bleibt eine Ablehnung.
    pub(crate) fn import(
        window_secs: u32,
        max_cgroups: usize,
        state: GrouperState,
        routes: &RouteTable,
    ) -> Result<Self, DispatchError> {
        let invalid = DispatchError::InvalidState {
            what: "incident grouper",
        };
        let GrouperState {
            next_incident,
            mut cgroups,
        } = state;
        if next_incident < 1 {
            return Err(invalid);
        }
        for slot in cgroups.values() {
            // Jeder wartende Befund hat eine KI-Route, und seine Rolle ist die
            // seiner Regel.
            let entries_ok = slot.entries.len() <= MAX_ENTRIES_PER_CGROUP
                && slot.entries.iter().all(|entry| {
                    is_uuid_v4(&entry.finding_id)
                        && matches!(
                            routes.lookup(&entry.rule_id),
                            Some(Route::Triage { role, .. }) if role == entry.rank.role
                        )
                });
            let incident_ok = slot.incident.as_ref().is_none_or(|incident| {
                incident.incident_id.as_u64() >= 1 && incident.incident_id.as_u64() < next_incident
            });
            if !entries_ok || !incident_ok {
                return Err(invalid);
            }
        }
        retain_greatest(&mut cgroups, max_cgroups, |slot| slot.touched_unix);
        Ok(Self {
            window_secs: u64::from(window_secs),
            max_cgroups,
            next_incident,
            last_prune_unix: None,
            cgroups,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route::{
        RULE_BASELINE_DEVIATION, RULE_EGRESS_FLOW, RULE_EXEC_WRITABLE_PATH, RULE_PERSISTENCE_WRITE,
        RULE_STRUCTURE_DRIFT,
    };
    use crate::test_support::{TestError, TestResult, missing};

    fn id(n: u32) -> String {
        format!("{n:08x}-1a2b-4c3d-8e4f-5a6b7c8d9e0f")
    }

    // Rolle einer Regel laut Tabelle (Testregeln haben alle eine KI-Route).
    fn role_of(rule: &str) -> TriageRole {
        match RouteTable::new().lookup(rule) {
            Some(Route::Triage { role, .. }) => role,
            _ => TriageRole::Baseline,
        }
    }

    // Ein `Medium`+`Observed`-Befund.
    fn input<'a>(kid: u64, rule: &'a str, finding: &'a str, at: i64) -> GroupInput<'a> {
        GroupInput {
            cgroup_kid: kid,
            rule_id: rule,
            finding_id: finding,
            role: role_of(rule),
            severity: Severity::Medium,
            hardness: Hardness::Observed,
            observed_at_unix: at,
        }
    }

    #[test]
    fn test_two_rule_classes_within_the_window_form_an_incident() -> TestResult {
        let mut grouper = IncidentGrouper::new(600, 16);
        let (a, b, c) = (id(1), id(2), id(3));
        assert_eq!(
            grouper.observe(input(7, RULE_EGRESS_FLOW, &a, 1_000), 1_000),
            Grouping::Standalone
        );
        let formed = grouper.observe(input(7, RULE_EXEC_WRITABLE_PATH, &b, 1_600), 1_600);
        let (incident_id, earlier_finding_ids, role) = match formed {
            Grouping::Formed {
                incident_id,
                earlier_finding_ids,
                role,
            } => (incident_id, earlier_finding_ids, role),
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        };
        assert_eq!(earlier_finding_ids, [a]);
        // Gleich schwer und hart: der ältere Befund bestimmt die Rolle.
        assert_eq!(role, TriageRole::Egress);
        let joined = grouper.observe(input(7, RULE_BASELINE_DEVIATION, &c, 2_100), 2_100);
        assert_eq!(
            joined,
            Grouping::Joined {
                incident_id,
                job: None,
                previous_max_severity: Severity::Medium,
                role: TriageRole::Egress,
            }
        );
        Ok(())
    }

    // Ein Befund mit frei gewählter Schwere und Härte.
    fn ranked<'a>(
        kid: u64,
        rule: &'a str,
        finding: &'a str,
        severity: Severity,
        hardness: Hardness,
        at: i64,
    ) -> GroupInput<'a> {
        GroupInput {
            severity,
            hardness,
            ..input(kid, rule, finding, at)
        }
    }

    // Die Rolle des Vorfalls nach der Einordnung, falls es einen gibt.
    fn role_after(grouping: Grouping) -> Option<TriageRole> {
        match grouping {
            Grouping::Formed { role, .. } | Grouping::Joined { role, .. } => Some(role),
            Grouping::Standalone => None,
        }
    }

    #[test]
    fn test_incident_role_follows_the_most_severe_member() -> TestResult {
        use crate::finding::Hardness::{Correlated, Inferred, Observed};
        let mut grouper = IncidentGrouper::new(600, 16);
        let ids: Vec<String> = (1..=8).map(id).collect();
        let at = |n: usize| ids.get(n).map(String::as_str).unwrap_or_default();
        let first = ranked(8, RULE_EGRESS_FLOW, at(0), Severity::Medium, Observed, 0);
        assert_eq!(role_after(grouper.observe(first, 0)), None);
        // Schwerer: die Endpunkt-Regel bestimmt die Rolle.
        let exec = ranked(
            8,
            RULE_EXEC_WRITABLE_PATH,
            at(1),
            Severity::High,
            Observed,
            10,
        );
        assert_eq!(
            role_after(grouper.observe(exec, 10)),
            Some(TriageRole::Endpoint)
        );
        // Leichter: unverändert (auch für einen Folgeauftrag).
        let low = ranked(
            8,
            RULE_BASELINE_DEVIATION,
            at(2),
            Severity::Low,
            Observed,
            20,
        );
        assert!(matches!(
            grouper.observe(low, 20),
            Grouping::Joined {
                previous_max_severity: Severity::High,
                role: TriageRole::Endpoint,
                ..
            }
        ));
        // Gleich schwer, aber weniger hart: unverändert.
        let drift = ranked(8, RULE_STRUCTURE_DRIFT, at(3), Severity::High, Inferred, 25);
        assert_eq!(
            role_after(grouper.observe(drift, 25)),
            Some(TriageRole::Endpoint)
        );
        // Schwerer: die Egress-Regel übernimmt.
        let critical = ranked(
            8,
            RULE_EGRESS_FLOW,
            at(4),
            Severity::Critical,
            Correlated,
            30,
        );
        assert_eq!(
            role_after(grouper.observe(critical, 30)),
            Some(TriageRole::Egress)
        );
        // Gleich schwer, gleich hart, jünger: der ältere bleibt.
        let later = ranked(
            8,
            RULE_PERSISTENCE_WRITE,
            at(5),
            Severity::Critical,
            Correlated,
            40,
        );
        assert_eq!(
            role_after(grouper.observe(later, 40)),
            Some(TriageRole::Egress)
        );
        // Bei der Bildung zählt die Härte vor dem Alter.
        let soft = ranked(9, RULE_STRUCTURE_DRIFT, at(6), Severity::High, Inferred, 0);
        assert_eq!(role_after(grouper.observe(soft, 40)), None);
        let hard = ranked(
            9,
            RULE_PERSISTENCE_WRITE,
            at(7),
            Severity::High,
            Observed,
            5,
        );
        assert_eq!(
            role_after(grouper.observe(hard, 40)),
            Some(TriageRole::Endpoint)
        );
        Ok(())
    }

    #[test]
    fn test_window_boundary_is_inclusive() -> TestResult {
        let mut grouper = IncidentGrouper::new(600, 16);
        let (a, b, c, d) = (id(1), id(2), id(3), id(4));
        assert_eq!(
            grouper.observe(input(1, RULE_EGRESS_FLOW, &a, 0), 0),
            Grouping::Standalone
        );
        assert_eq!(
            grouper.observe(input(1, RULE_EXEC_WRITABLE_PATH, &b, 601), 601),
            Grouping::Standalone
        );
        assert!(matches!(
            grouper.observe(input(2, RULE_EGRESS_FLOW, &c, 0), 601),
            Grouping::Standalone
        ));
        assert!(matches!(
            grouper.observe(input(2, RULE_EXEC_WRITABLE_PATH, &d, 600), 602),
            Grouping::Formed { .. }
        ));
        Ok(())
    }

    #[test]
    fn test_same_rule_or_other_cgroup_never_groups() -> TestResult {
        let mut grouper = IncidentGrouper::new(600, 16);
        let ids: Vec<String> = (1..=4).map(id).collect();
        let mut results = Vec::new();
        for (n, finding) in ids.iter().enumerate() {
            let at = i64::try_from(n).unwrap_or(0);
            results.push(grouper.observe(input(9, RULE_EGRESS_FLOW, finding, at), at));
        }
        let other = id(5);
        results.push(grouper.observe(input(10, RULE_EXEC_WRITABLE_PATH, &other, 3), 3));
        assert!(
            results
                .iter()
                .all(|grouping| *grouping == Grouping::Standalone)
        );
        Ok(())
    }

    #[test]
    fn test_incident_expires_after_a_quiet_window() -> TestResult {
        let mut grouper = IncidentGrouper::new(600, 16);
        let (a, b, c, d) = (id(1), id(2), id(3), id(4));
        grouper.observe(input(3, RULE_EGRESS_FLOW, &a, 0), 0);
        assert!(matches!(
            grouper.observe(input(3, RULE_EXEC_WRITABLE_PATH, &b, 10), 10),
            Grouping::Formed { .. }
        ));
        // 611 s nach dem jüngsten Mitglied: der Vorfall ist geschlossen.
        assert_eq!(
            grouper.observe(input(3, RULE_EGRESS_FLOW, &c, 621), 621),
            Grouping::Standalone
        );
        assert!(matches!(
            grouper.observe(input(3, RULE_BASELINE_DEVIATION, &d, 700), 700),
            Grouping::Formed { .. }
        ));
        Ok(())
    }

    #[test]
    fn test_bind_and_unbind_job() -> TestResult {
        let mut grouper = IncidentGrouper::new(600, 16);
        let (a, b, c, d) = (id(1), id(2), id(3), id(4));
        grouper.observe(input(4, RULE_EGRESS_FLOW, &a, 0), 0);
        let Grouping::Formed { incident_id, .. } =
            grouper.observe(input(4, RULE_EXEC_WRITABLE_PATH, &b, 1), 1)
        else {
            return Err(TestError::Unexpected("no incident".to_owned()));
        };
        let job = TriageJobId::from_u64(12);
        grouper.bind_job(4, incident_id, job);
        assert!(matches!(
            grouper.observe(input(4, RULE_EGRESS_FLOW, &c, 2), 2),
            Grouping::Joined { job: Some(bound), .. } if bound == job
        ));
        grouper.unbind_job(job);
        assert!(matches!(
            grouper.observe(input(4, RULE_EGRESS_FLOW, &d, 3), 3),
            Grouping::Joined { job: None, .. }
        ));
        Ok(())
    }

    #[test]
    fn test_joiners_cannot_keep_an_incident_open_forever() -> TestResult {
        let mut grouper = IncidentGrouper::new(600, 16);
        let ids: Vec<String> = (1..=6).map(id).collect();
        let at = |n: usize| ids.get(n).map(String::as_str).unwrap_or_default();
        grouper.observe(input(5, RULE_EGRESS_FLOW, at(0), 0), 0);
        let Grouping::Formed { incident_id, .. } =
            grouper.observe(input(5, RULE_EXEC_WRITABLE_PATH, at(1), 10), 10)
        else {
            return Err(TestError::Unexpected("no incident".to_owned()));
        };
        // Bildung bei 10: Mitglieder bis einschließlich 610.
        assert!(matches!(
            grouper.observe(input(5, RULE_EGRESS_FLOW, at(2), 500), 500),
            Grouping::Joined { incident_id: joined, .. } if joined == incident_id
        ));
        assert!(matches!(
            grouper.observe(input(5, RULE_EGRESS_FLOW, at(3), 610), 610),
            Grouping::Joined { .. }
        ));
        // 390 s nach dem jüngsten Mitglied, aber nach der festen Grenze:
        // der Vorfall ist zu, der Befund steht für sich.
        assert_eq!(
            grouper.observe(input(5, RULE_EGRESS_FLOW, at(4), 1_000), 1_000),
            Grouping::Standalone
        );
        // Ein neuer Vorfall braucht wieder zwei Regeln.
        assert!(matches!(
            grouper.observe(input(5, RULE_BASELINE_DEVIATION, at(5), 1_010), 1_010),
            Grouping::Formed { incident_id: next, .. } if next != incident_id
        ));
        Ok(())
    }

    #[test]
    fn test_retain_bindings_clears_rejected_bindings() -> TestResult {
        let mut grouper = IncidentGrouper::new(600, 16);
        let (a, b, c, d) = (id(1), id(2), id(3), id(4));
        grouper.observe(input(6, RULE_EGRESS_FLOW, &a, 0), 0);
        let Grouping::Formed { incident_id, .. } =
            grouper.observe(input(6, RULE_EXEC_WRITABLE_PATH, &b, 1), 1)
        else {
            return Err(TestError::Unexpected("no incident".to_owned()));
        };
        let job = TriageJobId::from_u64(3);
        grouper.bind_job(6, incident_id, job);
        grouper.retain_bindings(|incident, bound| incident == incident_id && bound == job);
        assert!(matches!(
            grouper.observe(input(6, RULE_EGRESS_FLOW, &c, 2), 2),
            Grouping::Joined { job: Some(bound), .. } if bound == job
        ));
        grouper.retain_bindings(|_, _| false);
        assert!(matches!(
            grouper.observe(input(6, RULE_EGRESS_FLOW, &d, 3), 3),
            Grouping::Joined { job: None, .. }
        ));
        Ok(())
    }

    #[test]
    fn test_tracked_cgroups_are_bounded_and_pruned() -> TestResult {
        let mut grouper = IncidentGrouper::new(600, 2);
        let ids: Vec<String> = (1..=3).map(id).collect();
        for (kid, finding) in (1_u64..).zip(&ids) {
            grouper.observe(input(kid, RULE_EGRESS_FLOW, finding, 0), 0);
        }
        assert_eq!(grouper.tracked_cgroups(), 2);
        let late = id(9);
        grouper.observe(input(99, RULE_EGRESS_FLOW, &late, 5_000), 5_000);
        assert_eq!(grouper.tracked_cgroups(), 1);
        Ok(())
    }

    // Die geführten Kernel-IDs in aufsteigender Reihenfolge.
    fn kids(grouper: &IncidentGrouper) -> Vec<u64> {
        grouper.cgroups.keys().copied().collect()
    }

    #[test]
    fn test_import_prunes_to_the_active_bound_keeping_the_most_recent() -> TestResult {
        let routes = RouteTable::new();
        let mut grouper = IncidentGrouper::new(600, 64);
        let ids: Vec<String> = (1..=6).map(id).collect();
        // Cgroup 10 bildet zuerst einen Vorfall (Zeit 0 und 1) …
        let first = ids.first().ok_or_else(missing("id 1"))?;
        let second = ids.get(1).ok_or_else(missing("id 2"))?;
        grouper.observe(input(10, RULE_EGRESS_FLOW, first, 0), 0);
        let Grouping::Formed { incident_id, .. } =
            grouper.observe(input(10, RULE_EXEC_WRITABLE_PATH, second, 1), 1)
        else {
            return Err(TestError::Unexpected("no incident".to_owned()));
        };
        // … danach melden sich die Cgroups 50, 40, 30, 20 in dieser Reihenfolge.
        for ((kid, at), finding) in [(50_u64, 10_i64), (40, 20), (30, 30), (20, 40)]
            .into_iter()
            .zip(ids.iter().skip(2))
        {
            grouper.observe(input(kid, RULE_EGRESS_FLOW, finding, at), at);
        }
        assert_eq!(grouper.tracked_cgroups(), 5);
        // Unverändert unter derselben Grenze.
        let same = IncidentGrouper::import(600, 64, grouper.export(), &routes)?;
        assert_eq!(same.export(), grouper.export());
        // Grenze 2: die zuletzt beobachteten (20 bei 40, 30 bei 30).
        let two = IncidentGrouper::import(600, 2, grouper.export(), &routes)?;
        assert_eq!(kids(&two), [20, 30]);
        // Grenze 1: der Vorfall von Cgroup 10 ist mit ihr gefallen; die
        // nächste Vorfall-Kennung bleibt, eine Kennung kommt nie doppelt.
        let mut one = IncidentGrouper::import(600, 1, grouper.export(), &routes)?;
        assert_eq!(kids(&one), [20]);
        assert_eq!(one.next_incident(), grouper.next_incident());
        let (c, d) = (id(7), id(8));
        assert_eq!(
            one.observe(input(10, RULE_BASELINE_DEVIATION, &c, 50), 50),
            Grouping::Standalone
        );
        assert!(matches!(
            one.observe(input(10, RULE_EGRESS_FLOW, &d, 51), 51),
            Grouping::Formed { incident_id: next, .. } if next != incident_id
        ));
        Ok(())
    }

    #[test]
    fn test_import_breaks_ties_by_the_greater_kernel_id() -> TestResult {
        let routes = RouteTable::new();
        let mut grouper = IncidentGrouper::new(600, 64);
        let ids: Vec<String> = (1..=4).map(id).collect();
        for (kid, finding) in [7_u64, 3, 9, 1].into_iter().zip(&ids) {
            grouper.observe(input(kid, RULE_EGRESS_FLOW, finding, 5), 5);
        }
        let two = IncidentGrouper::import(600, 2, grouper.export(), &routes)?;
        assert_eq!(kids(&two), [7, 9]);
        Ok(())
    }

    #[test]
    fn test_import_rejects_more_waiting_entries_than_the_constant() -> TestResult {
        let routes = RouteTable::new();
        let mut grouper = IncidentGrouper::new(600, 64);
        let first = id(1);
        grouper.observe(input(4, RULE_EGRESS_FLOW, &first, 0), 0);
        let mut state = grouper.export();
        let slot = state.cgroups.get_mut(&4).ok_or_else(missing("slot"))?;
        let entry = slot.entries.first().cloned().ok_or_else(missing("entry"))?;
        slot.entries = vec![entry; MAX_ENTRIES_PER_CGROUP.saturating_add(1)];
        assert_eq!(
            IncidentGrouper::import(600, 64, state, &routes).err(),
            Some(DispatchError::InvalidState {
                what: "incident grouper"
            })
        );
        Ok(())
    }
}
