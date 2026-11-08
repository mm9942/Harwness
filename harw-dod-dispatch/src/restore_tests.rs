//! Grenzen beim Wiederherstellen: [`crate::Dispatcher::restore`] übernimmt
//! die Grenzen der **aktiven** Konfiguration (PL-100 WP2.1, Review
//! 4505196829).
//!
//! Kindmodul von `dispatcher`, damit Zustände über einer konstanten Grenze
//! direkt über die privaten Felder von [`crate::DispatchState`] entstehen
//! (ein JSON mit 65537 Aufträgen wäre unnötig teuer).
//!
//! Abgedeckt, je Tabelle des [`crate::DispatchState`]:
//! - bekannte Schlüssel (`max_known_keys`, auch 1000 → 1): genau die
//!   Grenze, die zuletzt gesehenen bleiben, ein gefallener gilt wieder als
//!   neu;
//! - Cgroups des Groupers (`max_tracked_cgroups`): die zuletzt beobachteten
//!   bleiben, Vorfall-Kennungen werden nie doppelt vergeben;
//! - Budget-Eimer (die zuletzt benutzten), Erstsichtungen (die am längsten
//!   etablierten) und Breaker (`breaker.max_partitions`, offene zuerst);
//! - Warteschlange samt zurückgestellter vergebener Aufträge
//!   (`queue_capacity`, kleineres `max_parallel`): die dringendsten bleiben;
//! - konstante Grenzen: genau die Grenze ist gültig, eine mehr wird
//!   abgelehnt, für [`crate::MAX_PENDING_ASKS`],
//!   [`crate::MAX_STARVED_EPISODES`], [`crate::MAX_JOB_MEMBERS`] (wartende
//!   und vergebene Aufträge), [`crate::MAX_ENTRIES_PER_CGROUP`] sowie 64
//!   vergebene und 65536 wartende Aufträge (Höchstwerte von `max_parallel`
//!   und `queue_capacity`).
//!
//! Die Kürzung selbst (Gleichstände, beide Suchseiten) prüfen die
//! Modultests von `prune`, `retriage`, `group`, `budget`, `breaker` und
//! `queue`; hier geht es um die Wirkung über die Fassade.

use super::{MAX_PARALLEL_LIMIT, QUEUE_CAPACITY_LIMIT};
use crate::dispatcher_tests::{
    NOW, digest, enqueued, finding, key, loopback, on_cgroup, roomy, uuid,
};
use crate::job::JobMember;
use crate::test_support::{TestResult, missing};
use crate::*;

// Die Schlüssel einer als Paarliste serialisierten Tabelle unter `pointer`.
fn pair_keys(json: &serde_json::Value, pointer: &str) -> TestResult<Vec<serde_json::Value>> {
    let pairs = json
        .pointer(pointer)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(missing("pair list"))?;
    pairs
        .iter()
        .map(|pair| {
            pair.as_array()
                .and_then(|items| items.first())
                .cloned()
                .ok_or_else(missing("pair key"))
        })
        .collect()
}

// Die Partition eines Testbefunds auf Cgroup `cgroup` als JSON-Wert.
fn partition_json(cgroup: u64) -> TestResult<serde_json::Value> {
    Ok(serde_json::to_value(partition(cgroup))?)
}

// Die Partition eines Testbefunds auf Cgroup `cgroup`.
fn partition(cgroup: u64) -> PartitionKey {
    PartitionKey {
        uid: Some(1000),
        cgroup_kid: Some(cgroup),
        sensor: "probe-bpf-0".to_owned(),
    }
}

// Ob `actions` ein `Grouped` enthält.
fn grouped(actions: &[DispatchAction]) -> bool {
    actions
        .iter()
        .any(|action| matches!(action, DispatchAction::Grouped { .. }))
}

#[test]
fn test_restore_prunes_known_keys_to_the_active_bound() -> TestResult {
    let wide = DispatchConfig {
        max_known_keys: 1_000,
        ..roomy()
    };
    let mut dispatcher = Dispatcher::new(wide)?;
    // Schlüssel n zuletzt bei NOW + n gesehen.
    for n in 1..=5_u32 {
        let now = NOW.saturating_add(i64::from(n));
        let actions = dispatcher.offer(finding(n, RULE_EGRESS_FLOW, Severity::Medium), now);
        enqueued(&actions).ok_or_else(missing("job"))?;
    }
    let state = dispatcher.snapshot();
    // Unter derselben Grenze ändert sich nichts.
    assert_eq!(Dispatcher::restore(wide, state.clone())?.snapshot(), state);
    // Grenze 3: genau die drei jüngsten.
    let three = DispatchConfig {
        max_known_keys: 3,
        ..wide
    };
    let json = serde_json::to_value(Dispatcher::restore(three, state.clone())?.snapshot())?;
    let known = json
        .pointer("/known/keys")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(missing("known keys"))?;
    assert_eq!(
        known.keys().cloned().collect::<Vec<_>>(),
        [key(3), key(4), key(5)]
    );
    // Konfigurationswechsel 1000 → 1: nur der jüngste.
    let one = DispatchConfig {
        max_known_keys: 1,
        ..wide
    };
    let mut restored = Dispatcher::restore(one, state)?;
    let json = serde_json::to_value(restored.snapshot())?;
    let known = json
        .pointer("/known/keys")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(missing("known keys"))?;
    assert_eq!(known.keys().cloned().collect::<Vec<_>>(), [key(5)]);
    // Der behaltene bleibt ein Duplikat; ein gefallener gilt wieder als neu
    // (eine erneute Triage, nie eine Unterdrückung).
    let later = NOW.saturating_add(10);
    let duplicate = restored.offer(finding(5, RULE_EGRESS_FLOW, Severity::Medium), later);
    assert!(matches!(
        duplicate.as_slice(),
        [DispatchAction::Dropped {
            reason: DropReason::Duplicate,
            ..
        }]
    ));
    let novel = restored.offer(finding(1, RULE_EGRESS_FLOW, Severity::Medium), later);
    assert!(enqueued(&novel).is_some());
    Ok(())
}

#[test]
fn test_restore_prunes_tracked_cgroups_and_keeps_incident_ids_unique() -> TestResult {
    let wide = roomy();
    let mut dispatcher = Dispatcher::new(wide)?;
    // Cgroup 5 bildet bei NOW einen Vorfall …
    dispatcher.offer(on_cgroup(1, RULE_EGRESS_FLOW, Severity::Medium, 5, 0), NOW);
    let formed = dispatcher.offer(
        on_cgroup(2, RULE_EXEC_WRITABLE_PATH, Severity::Medium, 5, 1),
        NOW,
    );
    assert!(grouped(&formed));
    // … danach melden sich die Cgroups 30, 20 und 10 je einmal.
    for (n, cgroup, offset) in [(3_u32, 30_u64, 10_i64), (4, 20, 20), (5, 10, 30)] {
        let actions = dispatcher.offer(
            on_cgroup(n, RULE_EGRESS_FLOW, Severity::Medium, cgroup, offset),
            NOW.saturating_add(offset),
        );
        enqueued(&actions).ok_or_else(missing("job"))?;
    }
    let state = dispatcher.snapshot();
    let later = NOW.saturating_add(40);
    // Unter derselben Grenze nimmt der Vorfall ein weiteres Mitglied auf.
    let mut kept_all = Dispatcher::restore(wide, state.clone())?;
    let joined = kept_all.offer(
        on_cgroup(6, RULE_BASELINE_DEVIATION, Severity::Medium, 5, 40),
        later,
    );
    assert!(joined.contains(&DispatchAction::Grouped {
        incident_id: IncidentId::from_u64(1),
        finding_id: uuid(6),
    }));
    // Grenze 2: die zuletzt beobachteten Cgroups 10 und 20 bleiben.
    let narrow = DispatchConfig {
        max_tracked_cgroups: 2,
        ..wide
    };
    let mut restored = Dispatcher::restore(narrow, state)?;
    let json = serde_json::to_value(restored.snapshot())?;
    assert_eq!(
        pair_keys(&json, "/grouper/cgroups")?,
        [
            serde_json::Value::from(10_u64),
            serde_json::Value::from(20_u64)
        ]
    );
    // Der Vorfall von Cgroup 5 ist mit ihr gefallen: kein Mitglied mehr …
    let alone = restored.offer(
        on_cgroup(6, RULE_BASELINE_DEVIATION, Severity::Medium, 5, 40),
        later,
    );
    assert!(!grouped(&alone));
    assert!(enqueued(&alone).is_some());
    // … ein neuer Vorfall dort bekommt eine neue Kennung.
    let again = restored.offer(
        on_cgroup(7, RULE_EGRESS_FLOW, Severity::Medium, 5, 41),
        later,
    );
    assert!(again.contains(&DispatchAction::Grouped {
        incident_id: IncidentId::from_u64(2),
        finding_id: uuid(7),
    }));
    // Der wartende Auftrag des alten Vorfalls bleibt vergebbar.
    let mut incidents = Vec::new();
    while let Some(job) = restored.next_triage(later) {
        incidents.push(job.incident_id());
        restored.complete(&job.job_id(), TriageOutcome::Verdict, later);
    }
    assert!(incidents.contains(&Some(IncidentId::from_u64(1))));
    Ok(())
}

#[test]
fn test_restore_prunes_budget_and_breaker_partitions() -> TestResult {
    let wide = DispatchConfig {
        breaker: BreakerConfig {
            trip_threshold: 2,
            quiet_threshold: 1,
            ..BreakerConfig::default()
        },
        ..roomy()
    };
    let mut dispatcher = Dispatcher::new(wide)?;
    // Cgroup 1 löst bei NOW ihren Breaker aus …
    for n in 1..=3_u32 {
        dispatcher.offer(on_cgroup(n, RULE_EGRESS_FLOW, Severity::Medium, 1, 0), NOW);
    }
    assert!(dispatcher.breakers().is_tripped(&partition(1)));
    // … danach je ein Befund in den Cgroups 2, 3 und 4.
    for (n, cgroup) in [(4_u32, 2_u64), (5, 3), (6, 4)] {
        let now = NOW.saturating_add(i64::try_from(cgroup.saturating_mul(10)).unwrap_or(0));
        let actions = dispatcher.offer(
            on_cgroup(n, RULE_EGRESS_FLOW, Severity::Medium, cgroup, 0),
            now,
        );
        enqueued(&actions).ok_or_else(missing("job"))?;
    }
    let state = dispatcher.snapshot();
    assert_eq!(Dispatcher::restore(wide, state.clone())?.snapshot(), state);
    let narrow = DispatchConfig {
        budget: BudgetConfig {
            max_partitions: 2,
            ..wide.budget
        },
        breaker: BreakerConfig {
            max_partitions: 2,
            ..wide.breaker
        },
        ..wide
    };
    let restored = Dispatcher::restore(narrow, state)?;
    let json = serde_json::to_value(restored.snapshot())?;
    // Budget: die zuletzt benutzten Eimer, aber die am längsten etablierten
    // Erstsichtungen (die Laufzeit verdrängt keine lebende; frische
    // Partitionen vor dem Neustart verdrängen sie auch beim Laden nicht).
    assert_eq!(
        pair_keys(&json, "/budget/partitions")?,
        [partition_json(3)?, partition_json(4)?]
    );
    assert_eq!(
        pair_keys(&json, "/budget/established")?,
        [partition_json(1)?, partition_json(2)?]
    );
    // Breaker: der offene trotz seines Alters, dazu der zuletzt benutzte.
    assert_eq!(
        pair_keys(&json, "/breakers/partitions")?,
        [partition_json(1)?, partition_json(4)?]
    );
    assert!(restored.breakers().is_tripped(&partition(1)));
    Ok(())
}

#[test]
fn test_restore_requeues_in_flight_jobs_within_the_reduced_capacity() -> TestResult {
    let wide = DispatchConfig {
        max_parallel: 4,
        ..roomy()
    };
    let mut dispatcher = Dispatcher::new(wide)?;
    // Sechs neue `Observed`-Aufträge: 1–3 `High`, 4–6 `Medium`.
    for n in 1..=6_u32 {
        let severity = if n <= 3 {
            Severity::High
        } else {
            Severity::Medium
        };
        let actions = dispatcher.offer(finding(n, RULE_EGRESS_FLOW, severity), NOW);
        enqueued(&actions).ok_or_else(missing("job"))?;
    }
    // Vier vergeben (die dringendsten), zwei warten.
    let mut dispatched = Vec::new();
    while let Some(job) = dispatcher.next_triage(NOW) {
        dispatched.push(job.finding_id().to_owned());
    }
    assert_eq!(dispatched, [uuid(1), uuid(2), uuid(3), uuid(4)]);
    assert_eq!(dispatcher.in_flight_len(), 4);
    // Neustart mit `max_parallel` 1 und Platz für drei Aufträge: mehr
    // vergebene als `max_parallel` sind kein Fehler.
    let narrow = DispatchConfig {
        max_parallel: 1,
        queue_capacity: 3,
        ..wide
    };
    let mut restored = Dispatcher::restore(narrow, dispatcher.snapshot())?;
    assert_eq!(restored.in_flight_len(), 0);
    assert_eq!(restored.queue_len(), 3);
    assert_eq!(restored.counters().dropped.queue_overflow, 3);
    // Die dringendsten bleiben, in ihrer Reihenfolge.
    let mut order = Vec::new();
    while let Some(job) = restored.next_triage(NOW) {
        order.push(job.finding_id().to_owned());
        restored.complete(&job.job_id(), TriageOutcome::Verdict, NOW);
    }
    assert_eq!(order, [uuid(1), uuid(2), uuid(3)]);
    // Verdrängte Schlüssel gelten wieder als neu, auch der des eben noch
    // vergebenen Auftrags 4.
    for n in 4..=6_u32 {
        let again = restored.offer(finding(n, RULE_EGRESS_FLOW, Severity::Medium), NOW);
        assert!(enqueued(&again).is_some(), "finding {n}");
    }
    Ok(())
}

#[test]
fn test_restore_accepts_constant_bounds_exactly_and_rejects_one_more() -> TestResult {
    let config = roomy();
    let mut dispatcher = Dispatcher::new(config)?;
    let asked = dispatcher.offer(loopback(1), NOW);
    assert!(asked.contains(&DispatchAction::AskUser {
        finding_id: uuid(1)
    }));
    let json = serde_json::to_value(dispatcher.snapshot())?;
    let template = json
        .pointer("/pending_asks/0")
        .cloned()
        .ok_or_else(missing("pending ask"))?;
    // `count` gültige Rückfragen mit je eigener Kennung.
    let with_asks = |count: usize| -> TestResult<DispatchState> {
        let mut pairs = Vec::new();
        for n in (1_u32..).take(count) {
            let mut pair = template.clone();
            *pair.pointer_mut("/0").ok_or_else(missing("ask id"))? =
                serde_json::Value::from(uuid(n));
            *pair
                .pointer_mut("/1/finding/finding_id")
                .ok_or_else(missing("finding id"))? = serde_json::Value::from(uuid(n));
            pairs.push(pair);
        }
        let mut tampered = json.clone();
        *tampered
            .pointer_mut("/pending_asks")
            .ok_or_else(missing("pending asks"))? = serde_json::Value::Array(pairs);
        Ok(serde_json::from_value(tampered)?)
    };
    let full = Dispatcher::restore(config, with_asks(MAX_PENDING_ASKS)?)?;
    assert_eq!(full.pending_asks_len(), MAX_PENDING_ASKS);
    assert_eq!(
        Dispatcher::restore(config, with_asks(MAX_PENDING_ASKS.saturating_add(1))?).err(),
        Some(DispatchError::InvalidState {
            what: "pending asks"
        })
    );
    // `count` Starved-Episoden mit je eigener uid.
    let with_episodes = |count: usize| -> TestResult<DispatchState> {
        let episodes: Vec<serde_json::Value> = (0_u32..)
            .take(count)
            .map(|uid| serde_json::json!([["breaker", uid], NOW]))
            .collect();
        let mut tampered = json.clone();
        *tampered
            .pointer_mut("/starved_episodes")
            .ok_or_else(missing("starved episodes"))? = serde_json::Value::Array(episodes);
        Ok(serde_json::from_value(tampered)?)
    };
    assert!(Dispatcher::restore(config, with_episodes(MAX_STARVED_EPISODES)?).is_ok());
    assert_eq!(
        Dispatcher::restore(
            config,
            with_episodes(MAX_STARVED_EPISODES.saturating_add(1))?
        )
        .err(),
        Some(DispatchError::InvalidState {
            what: "starved episodes"
        })
    );
    Ok(())
}

// `count` gültige Mitglieder nach dem Muster `template`, je mit eigener
// Kennung, eigenem Schlüssel und eigenem Digest (Nummern ab 100).
fn members_like(template: &JobMember, count: usize) -> Vec<JobMember> {
    (100_u32..)
        .take(count)
        .map(|n| JobMember {
            finding_id: uuid(n),
            dedup_key: key(n),
            record_digest: digest(n),
            ..template.clone()
        })
        .collect()
}

// Ein Zustand mit vergebenem Auftrag 1 und wartendem Auftrag 2 (je ein
// `egress-flow`-Mitglied ohne Cgroup, gleich dringlich).
fn one_in_flight_one_queued(config: DispatchConfig) -> TestResult<DispatchState> {
    let mut dispatcher = Dispatcher::new(config)?;
    for n in 1..=2_u32 {
        let actions = dispatcher.offer(finding(n, RULE_EGRESS_FLOW, Severity::Medium), NOW);
        enqueued(&actions).ok_or_else(missing("job"))?;
    }
    let dispatched = dispatcher
        .next_triage(NOW)
        .ok_or_else(missing("in-flight job"))?;
    assert_eq!(dispatched.job_id(), TriageJobId::from_u64(1));
    Ok(dispatcher.snapshot())
}

#[test]
fn test_restore_accepts_the_max_parallel_limit_in_flight_and_rejects_one_more() -> TestResult {
    let config = roomy();
    let state = one_in_flight_one_queued(config)?;
    let template = state
        .in_flight
        .first()
        .cloned()
        .ok_or_else(missing("in-flight job"))?;
    // `count` vergebene Aufträge (Nummern ab 3) neben dem wartenden
    // Auftrag 2.
    let with_in_flight = |count: usize| -> DispatchState {
        let mut tampered = state.clone();
        tampered.in_flight = (3_u64..)
            .take(count)
            .map(|id| {
                let mut flight = template.clone();
                flight.job.job_id = TriageJobId::from_u64(id);
                flight
            })
            .collect();
        tampered.next_job = u64::try_from(count).map_or(u64::MAX, |n| n.saturating_add(3));
        tampered
    };
    // 64 vergebene (Höchstwert von `max_parallel`) sind gültig, auch über
    // `max_parallel` 2, und kommen alle hinter den wartenden zurück.
    let mut restored = Dispatcher::restore(config, with_in_flight(MAX_PARALLEL_LIMIT))?;
    assert_eq!(restored.in_flight_len(), 0);
    assert_eq!(restored.queue_len(), MAX_PARALLEL_LIMIT.saturating_add(1));
    let first = restored
        .next_triage(NOW)
        .ok_or_else(missing("queued job"))?;
    assert_eq!(first.job_id(), TriageJobId::from_u64(2));
    // Unter kleinerer Kapazität bleiben bei gleicher Dringlichkeit die
    // zuerst eingereihten (FIFO); jeder andere zählt als Überlauf.
    let tight = DispatchConfig {
        queue_capacity: 10,
        ..config
    };
    let mut restored = Dispatcher::restore(tight, with_in_flight(MAX_PARALLEL_LIMIT))?;
    assert_eq!(restored.queue_len(), 10);
    let surplus = MAX_PARALLEL_LIMIT.saturating_add(1).saturating_sub(10);
    assert_eq!(
        restored.counters().dropped.queue_overflow,
        u64::try_from(surplus).unwrap_or(u64::MAX)
    );
    let mut order = Vec::new();
    while let Some(job) = restored.next_triage(NOW) {
        order.push(job.job_id().as_u64());
        restored.complete(&job.job_id(), TriageOutcome::Verdict, NOW);
    }
    assert_eq!(order, (2..=11_u64).collect::<Vec<_>>());
    // 65 vergebene kann keine gültige Konfiguration schreiben.
    assert_eq!(
        Dispatcher::restore(config, with_in_flight(MAX_PARALLEL_LIMIT.saturating_add(1))).err(),
        Some(DispatchError::InvalidState {
            what: "triage jobs"
        })
    );
    Ok(())
}

#[test]
fn test_restore_accepts_the_queue_capacity_limit_and_rejects_one_more() -> TestResult {
    let config = DispatchConfig {
        queue_capacity: QUEUE_CAPACITY_LIMIT,
        ..roomy()
    };
    let state = one_in_flight_one_queued(config)?;
    let template = state
        .queue
        .entries
        .first()
        .cloned()
        .ok_or_else(missing("queued job"))?;
    // `count` wartende Aufträge (Nummern ab 2) neben dem vergebenen
    // Auftrag 1.
    let with_queued = |count: usize| -> DispatchState {
        let mut tampered = state.clone();
        tampered.queue.entries = (2_u64..)
            .take(count)
            .map(|id| {
                let mut entry = template.clone();
                entry.item.job_id = TriageJobId::from_u64(id);
                entry.seq = id;
                entry
            })
            .collect();
        tampered.next_job = u64::try_from(count).map_or(u64::MAX, |n| n.saturating_add(2));
        tampered
    };
    {
        // 65536 wartende (Höchstwert von `queue_capacity`) sind gültig; der
        // zurückgestellte Auftrag 1 passt nicht mehr und zählt als Überlauf.
        let restored = Dispatcher::restore(config, with_queued(QUEUE_CAPACITY_LIMIT))?;
        assert_eq!(restored.queue_len(), QUEUE_CAPACITY_LIMIT);
        assert_eq!(restored.in_flight_len(), 0);
        assert_eq!(restored.counters().dropped.queue_overflow, 1);
    }
    // 65537 kann keine gültige Konfiguration schreiben; abgelehnt, bevor
    // ein Auftrag geprüft oder sortiert wird.
    assert_eq!(
        Dispatcher::restore(config, with_queued(QUEUE_CAPACITY_LIMIT.saturating_add(1))).err(),
        Some(DispatchError::InvalidState {
            what: "triage jobs"
        })
    );
    Ok(())
}

#[test]
fn test_restore_accepts_max_job_members_and_rejects_one_more() -> TestResult {
    let config = roomy();
    let state = one_in_flight_one_queued(config)?;
    let template = state
        .queue
        .entries
        .first()
        .and_then(|entry| entry.item.members.first())
        .cloned()
        .ok_or_else(missing("job member"))?;
    // Der wartende Auftrag 2 mit `count` Mitgliedern.
    let with_queued_members = |count: usize| -> TestResult<DispatchState> {
        let mut tampered = state.clone();
        let entry = tampered
            .queue
            .entries
            .first_mut()
            .ok_or_else(missing("queued job"))?;
        entry.item.members = members_like(&template, count);
        Ok(tampered)
    };
    // Der vergebene Auftrag 1 mit `count` Mitgliedern.
    let with_in_flight_members = |count: usize| -> TestResult<DispatchState> {
        let mut tampered = state.clone();
        let flight = tampered
            .in_flight
            .first_mut()
            .ok_or_else(missing("in-flight job"))?;
        flight.job.members = members_like(&template, count);
        Ok(tampered)
    };
    let invalid = Some(DispatchError::InvalidState {
        what: "triage jobs",
    });
    let one_more = MAX_JOB_MEMBERS.saturating_add(1);
    // Wartend: genau die Grenze ist gültig, eine mehr nicht.
    let mut restored = Dispatcher::restore(config, with_queued_members(MAX_JOB_MEMBERS)?)?;
    let job = restored
        .next_triage(NOW)
        .ok_or_else(missing("queued job"))?;
    assert_eq!(job.job_id(), TriageJobId::from_u64(2));
    assert_eq!(job.member_finding_ids().len(), MAX_JOB_MEMBERS);
    assert_eq!(
        Dispatcher::restore(config, with_queued_members(one_more)?).err(),
        invalid
    );
    // Vergeben ebenso; der zurückgestellte Auftrag behält alle Mitglieder.
    let mut restored = Dispatcher::restore(config, with_in_flight_members(MAX_JOB_MEMBERS)?)?;
    let mut sizes = Vec::new();
    while let Some(job) = restored.next_triage(NOW) {
        sizes.push((job.job_id().as_u64(), job.member_finding_ids().len()));
        restored.complete(&job.job_id(), TriageOutcome::Verdict, NOW);
    }
    assert_eq!(sizes, [(2, 1), (1, MAX_JOB_MEMBERS)]);
    assert_eq!(
        Dispatcher::restore(config, with_in_flight_members(one_more)?).err(),
        invalid
    );
    Ok(())
}

#[test]
fn test_restore_accepts_max_entries_per_cgroup_and_rejects_one_more() -> TestResult {
    let config = roomy();
    let mut dispatcher = Dispatcher::new(config)?;
    // Ein Einzelbefund wartet in Cgroup 7 auf einen Partner.
    let actions = dispatcher.offer(on_cgroup(1, RULE_EGRESS_FLOW, Severity::Medium, 7, 0), NOW);
    enqueued(&actions).ok_or_else(missing("job"))?;
    let json = serde_json::to_value(dispatcher.snapshot())?;
    let template = json
        .pointer("/grouper/cgroups/0/1/entries/0")
        .cloned()
        .ok_or_else(missing("waiting entry"))?;
    // `count` wartende Befunde in Cgroup 7 mit je eigener Kennung.
    let with_entries = |count: usize| -> TestResult<DispatchState> {
        let mut entries = Vec::new();
        for n in (100_u32..).take(count) {
            let mut entry = template.clone();
            *entry
                .pointer_mut("/finding_id")
                .ok_or_else(missing("finding id"))? = serde_json::Value::from(uuid(n));
            entries.push(entry);
        }
        let mut tampered = json.clone();
        *tampered
            .pointer_mut("/grouper/cgroups/0/1/entries")
            .ok_or_else(missing("waiting entries"))? = serde_json::Value::Array(entries);
        Ok(serde_json::from_value(tampered)?)
    };
    // Genau die Grenze ist gültig und bleibt unverändert …
    let full = with_entries(MAX_ENTRIES_PER_CGROUP)?;
    assert_eq!(Dispatcher::restore(config, full.clone())?.snapshot(), full);
    // … eine mehr kann die Laufzeit nicht schreiben.
    assert_eq!(
        Dispatcher::restore(
            config,
            with_entries(MAX_ENTRIES_PER_CGROUP.saturating_add(1))?
        )
        .err(),
        Some(DispatchError::InvalidState {
            what: "incident grouper"
        })
    );
    Ok(())
}
