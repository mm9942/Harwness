//! Sturm-, Budget- und Breaker-Tests der Fassade [`crate::Dispatcher`]
//! (WP2.1, Schritt 2).
//!
//! Abgedeckt:
//! - Invariante 8: 10 000 Befunde in einer Partition bleiben im Budget,
//!   `High`+`Observed`-Befunde anderer Partitionen werden weiter eingereiht;
//! - README §7: ein Angreifer mit 21 Keys je 5 min blockiert keine fremden
//!   `High`+`Observed`-Befunde, in einer Partition wie über viele Cgroups,
//!   mit `Medium`- wie mit `High`-Befunden, und das auch für ein Opfer
//!   **derselben** uid (Einzelnutzer-Desktop), dessen Partition vor dem
//!   Sturm bekannt war (Vorrang etablierter Partitionen);
//! - Invariante 8 bei wiederverwendeten Partitionen: eine uid, die dieselben
//!   24 Cgroups immer wieder benutzt (und damit etabliert), nimmt aus der
//!   Reserve nur ihre Quote und ein Vorrechts-Token; `High`+`Observed` einer
//!   anderen uid wird in jeder Runde eingereiht;
//! - I3 im Sturm: jeder `High`/`Critical` bekommt genau einen Hinweis, und
//!   zwar als erste Aktion, gleich ob er verworfen, gedrosselt oder
//!   eingereiht wird;
//! - ein `Starved` je Trip (Breaker und Budget), neu scharf erst nach voller
//!   Erholung;
//! - Zustands-Rundreise mitten im Sturm: Grenzen und Trips überstehen einen
//!   Neustart, das Verhalten danach ist identisch.
//!
//! Proptest ist nicht im Workspace; die Stürme sind deterministische
//! Schleifen (Knuth-Multiplikationshash für die Mischung).

use crate::dispatcher_tests::{NOW, enqueued, finding, notices, uuid};
use crate::test_support::{TestError, TestResult, missing};
use crate::*;

// Ein gültiger Befund mit frei gewählter Partition und Ereigniszeit.
fn placed(
    n: u32,
    rule: &str,
    severity: Severity,
    uid: u32,
    cgroup: u64,
    now: i64,
) -> DispatchFinding {
    DispatchFinding {
        uid: Some(uid),
        cgroup_kid: Some(cgroup),
        observed_at_unix: now,
        ..finding(n, rule, severity)
    }
}

// Die Partition eines Befunds aus `placed`.
fn partition(uid: u32, cgroup: u64) -> PartitionKey {
    PartitionKey {
        uid: Some(uid),
        cgroup_kid: Some(cgroup),
        sensor: "probe-bpf-0".to_owned(),
    }
}

// Ob `actions` mit genau einem Hinweis der Schwere `severity` beginnt.
fn notified_first(actions: &[DispatchAction], severity: Severity) -> bool {
    notices(actions) == 1
        && matches!(
            actions.first(),
            Some(DispatchAction::Notify { severity: noted, .. }) if *noted == severity
        )
}

#[test]
fn test_storm_of_ten_thousand_in_one_partition_stays_in_budget_and_spares_others() -> TestResult {
    let config = DispatchConfig::default();
    let mut dispatcher = Dispatcher::new(config)?;
    let attacker = partition(1001, 66);
    let mut attacker_enqueued = 0_u64;
    let mut starved = Vec::new();
    let mut victims = 0_u64;
    let mut last = NOW;
    for n in 1..=10_000_u32 {
        // Zwei Befunde je Sekunde, jeder mit neuem Schlüssel.
        let now = NOW.saturating_add(i64::from(n.checked_div(2).unwrap_or(0)));
        last = now;
        let storm = placed(n, RULE_EGRESS_FLOW, Severity::Medium, 1001, 66, now);
        for action in dispatcher.offer(storm, now) {
            match action {
                DispatchAction::EnqueuedTriage { .. } => {
                    attacker_enqueued = attacker_enqueued.saturating_add(1);
                }
                DispatchAction::Starved { partition, cause } => starved.push((partition, cause)),
                DispatchAction::Dropped {
                    reason: DropReason::BudgetExhausted | DropReason::BreakerOpen,
                    ..
                } => {}
                other => return Err(TestError::Unexpected(format!("storm {n}: {other:?}"))),
            }
        }
        // Alle 100 Sturmbefunde ein High+Observed einer anderen Partition.
        if n.checked_rem(100) == Some(0) {
            victims = victims.saturating_add(1);
            let v = n.saturating_add(20_000);
            let victim = placed(
                v,
                RULE_EXEC_WRITABLE_PATH,
                Severity::High,
                1000,
                u64::from(v),
                now,
            );
            let actions = dispatcher.offer(victim, now);
            if actions.len() != 2
                || !notified_first(&actions, Severity::High)
                || enqueued(&actions).is_none()
            {
                return Err(TestError::Unexpected(format!("victim {v}: {actions:?}")));
            }
        }
    }
    let budget = config.budget;
    let elapsed = u64::try_from(last.saturating_sub(NOW)).unwrap_or(0);
    let refilled = elapsed
        .saturating_mul(u64::from(budget.partition_refill_per_hour))
        .checked_div(3_600)
        .unwrap_or(0);
    let bound = u64::from(budget.partition_capacity).saturating_add(refilled);
    assert!(attacker_enqueued <= bound, "{attacker_enqueued} > {bound}");
    // Der Breaker öffnet, bevor die Partition nachgefüllt ist.
    assert_eq!(attacker_enqueued, u64::from(budget.partition_capacity));
    assert_eq!(
        starved,
        [
            (attacker.clone(), StarvedCause::PartitionBudget),
            (attacker.clone(), StarvedCause::Breaker),
        ]
    );
    assert!(dispatcher.breakers().is_tripped(&attacker));
    assert_eq!(dispatcher.budget().partition_tokens(&attacker), Some(0));
    let counters = dispatcher.counters();
    assert_eq!(victims, 100);
    assert_eq!(counters.breaker_trips, 1);
    assert_eq!(counters.budget_trips, 1);
    assert_eq!(counters.enqueued, attacker_enqueued.saturating_add(victims));
    assert_eq!(
        counters
            .dropped
            .budget_exhausted
            .saturating_add(counters.dropped.breaker_open)
            .saturating_add(attacker_enqueued),
        10_000
    );
    assert_eq!(counters.dropped.queue_overflow, 0);
    assert_eq!(
        u64::try_from(dispatcher.queue_len()).unwrap_or(u64::MAX),
        counters.enqueued
    );
    Ok(())
}

// Wie der Angreifer seine Schlüssel verteilt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spread {
    // Alle Schlüssel in einer Partition.
    OnePartition,
    // Jeder Schlüssel in einer neuen Cgroup derselben uid.
    CgroupPerKey,
}

// Vier Stunden lang 21 neue Schlüssel je 5 min von uid 1001; nach jedem
// Schwall (ungünstigster Zeitpunkt) ein `High`+`Observed`-Befund des Opfers
// `victim_uid` in Cgroup 500, der eingereiht werden muss. Liefert die
// Ursachen aller `Starved`.
fn attacker_with_21_keys(
    severity: Severity,
    spread: Spread,
    victim_uid: u32,
) -> TestResult<Vec<StarvedCause>> {
    let mut dispatcher = Dispatcher::new(DispatchConfig::default())?;
    let mut causes = Vec::new();
    let mut n = 0_u32;
    for round in 0..48_u32 {
        let now = NOW.saturating_add(i64::from(round.saturating_mul(300)));
        for _ in 0..21 {
            n = n.saturating_add(1);
            let cgroup = match spread {
                Spread::OnePartition => 66,
                Spread::CgroupPerKey => 100_000_u64.saturating_add(u64::from(n)),
            };
            let storm = placed(n, RULE_EGRESS_FLOW, severity, 1001, cgroup, now);
            for action in dispatcher.offer(storm, now) {
                if let DispatchAction::Starved { cause, .. } = action {
                    causes.push(cause);
                }
            }
        }
        let v = round.saturating_add(50_000);
        let victim = placed(
            v,
            RULE_EXEC_WRITABLE_PATH,
            Severity::High,
            victim_uid,
            500,
            now,
        );
        let actions = dispatcher.offer(victim, now);
        if !notified_first(&actions, Severity::High) || enqueued(&actions).is_none() {
            return Err(TestError::Unexpected(format!(
                "{severity:?} {spread:?} uid {victim_uid} round {round}: {actions:?}"
            )));
        }
    }
    let counters = dispatcher.counters();
    let case = format!("{severity:?} {spread:?} uid {victim_uid}");
    // 21 je 5 min bleiben unter dem Breaker: allein das Budget begrenzt.
    assert_eq!(counters.breaker_trips, 0, "{case}");
    assert!(counters.dropped.budget_exhausted > 0, "{case}");
    assert_eq!(counters.dropped.queue_overflow, 0, "{case}");
    assert_eq!(counters.starved_suppressed, 0, "{case}");
    Ok(causes)
}

#[test]
fn test_attacker_with_21_keys_per_5_min_never_blocks_foreign_high_observed() -> TestResult {
    // Je Angriff genau ein Budget-Trip, an der Ebene, die ihn begrenzt.
    let cases = [
        (
            Severity::Medium,
            Spread::OnePartition,
            StarvedCause::PartitionBudget,
        ),
        (
            Severity::High,
            Spread::OnePartition,
            StarvedCause::PartitionBudget,
        ),
        (
            Severity::Medium,
            Spread::CgroupPerKey,
            StarvedCause::GlobalBudget,
        ),
        (
            Severity::High,
            Spread::CgroupPerKey,
            StarvedCause::UidReserve,
        ),
    ];
    // uid 1000: ein anderer Nutzer; uid 1001: dieselbe uid wie der Angreifer.
    for victim_uid in [1000, 1001] {
        for (severity, spread, cause) in cases {
            assert_eq!(
                attacker_with_21_keys(severity, spread, victim_uid)?,
                [cause],
                "{severity:?} {spread:?} uid {victim_uid}"
            );
        }
    }
    Ok(())
}

#[test]
fn test_reused_partitions_of_one_uid_never_starve_another_uids_high_observed() -> TestResult {
    // Vorgaben: Reserve 12 Tokens, uid-Quote 6, Vorrechts-Intervall 300 s.
    let config = DispatchConfig::default();
    let budget = config.budget;
    let reserve = u64::from(budget.global_capacity)
        .saturating_mul(u64::from(budget.reserved_share_percent))
        .checked_div(100)
        .ok_or_else(missing("reserve"))?;
    // Aus der Reserve nimmt die uid höchstens ihre Quote und ein
    // Vorrechts-Token.
    let floor = reserve
        .saturating_sub(u64::from(budget.uid_reserve_capacity))
        .saturating_sub(1);
    let mut dispatcher = Dispatcher::new(config)?;
    let mut n = 0_u32;
    // Zwei Stunden: ab der zweiten Runde sind die 24 Cgroups etabliert.
    for round in 0..24_u32 {
        let start = NOW.saturating_add(i64::from(round.saturating_mul(300)));
        // Uid 1001 bietet in denselben 24 Cgroups in zwei aufeinanderfolgenden
        // Sekunden je einen `High`+`Observed`-Befund an.
        for second in 0..2_i64 {
            let now = start.saturating_add(second);
            for cgroup in 1..=24_u64 {
                n = n.saturating_add(1);
                let storm = placed(n, RULE_EGRESS_FLOW, Severity::High, 1001, cgroup, now);
                let actions = dispatcher.offer(storm, now);
                if !notified_first(&actions, Severity::High) {
                    return Err(TestError::Unexpected(format!(
                        "round {round} storm {n}: {actions:?}"
                    )));
                }
            }
        }
        let left = dispatcher.budget().global_tokens();
        assert!(left >= floor, "round {round}: {left} < {floor}");
        // Eine Sekunde später ein `High`+`Observed`-Befund von uid 1000 in
        // einer frischen Cgroup: er muss eingereiht werden.
        let v = round.saturating_add(90_000);
        let now = start.saturating_add(2);
        let victim = placed(
            v,
            RULE_EXEC_WRITABLE_PATH,
            Severity::High,
            1000,
            u64::from(v),
            now,
        );
        let actions = dispatcher.offer(victim, now);
        if actions.len() != 2
            || !notified_first(&actions, Severity::High)
            || enqueued(&actions).is_none()
        {
            return Err(TestError::Unexpected(format!(
                "round {round} victim: {actions:?}"
            )));
        }
    }
    let counters = dispatcher.counters();
    assert!(counters.dropped.budget_exhausted > 0);
    assert_eq!(counters.dropped.queue_overflow, 0);
    assert_eq!(counters.breaker_trips, 0);
    Ok(())
}

#[test]
fn test_every_high_or_critical_is_notified_first_throughout_a_storm() -> TestResult {
    // Winzige Warteschlange, kleines Budget, empfindlicher Breaker.
    let config = DispatchConfig {
        queue_capacity: 4,
        max_parallel: 1,
        budget: BudgetConfig {
            partition_capacity: 2,
            global_capacity: 8,
            global_refill_per_hour: 60,
            uid_reserve_capacity: 1,
            ..BudgetConfig::default()
        },
        breaker: BreakerConfig {
            trip_threshold: 5,
            quiet_threshold: 1,
            ..BreakerConfig::default()
        },
        ..DispatchConfig::default()
    };
    let mut dispatcher = Dispatcher::new(config)?;
    let severities = [
        Severity::Info,
        Severity::Low,
        Severity::Medium,
        Severity::High,
        Severity::Critical,
    ];
    let hardnesses = [Hardness::Observed, Hardness::Correlated, Hardness::Inferred];
    let mut high_offers = 0_u64;
    let mut shown_total = 0_u64;
    for n in 0..10_000_u32 {
        // Erst dicht (20 je Sekunde), dann dünn (einer je 15 s), damit
        // Breaker schließen und Budgets sich erholen.
        let now = if n < 2_000 {
            NOW.saturating_add(i64::from(n.checked_div(20).unwrap_or(0)))
        } else {
            NOW.saturating_add(100)
                .saturating_add(i64::from(n.saturating_sub(2_000).saturating_mul(15)))
        };
        let mix = n.wrapping_mul(2_654_435_761);
        let pick = |shift: u32, modulus: u32| {
            let bits = mix.checked_shr(shift).unwrap_or(0);
            usize::try_from(bits.checked_rem(modulus).unwrap_or(0)).unwrap_or(0)
        };
        let rule = ALL_RULE_IDS
            .get(pick(3, 10))
            .copied()
            .ok_or_else(missing("rule"))?;
        let severity = severities
            .get(pick(8, 5))
            .copied()
            .ok_or_else(missing("severity"))?;
        let hardness = hardnesses
            .get(pick(12, 3))
            .copied()
            .ok_or_else(missing("hardness"))?;
        let uid = 1000_u32.saturating_add(u32::try_from(pick(16, 3)).unwrap_or(0));
        let cgroup = u64::try_from(pick(20, 7)).unwrap_or(0);
        // 400 Schlüssel: Wiederholungen ergeben Duplikate und Re-Triage.
        let key_n = n.checked_rem(400).unwrap_or(0).saturating_add(1);
        let mut input = DispatchFinding {
            finding_id: uuid(n.saturating_add(1)),
            hardness,
            ..placed(key_n, rule, severity, uid, cgroup, now)
        };
        match pick(24, 12) {
            0 => input.finding_id = "not-a-uuid".to_owned(),
            1 => input.verified_source = false,
            2 => input.record_digest = None,
            3 => input.scope_obfuscated = true,
            4 => input.loopback = true,
            5 => input.rule_id = "no-such-rule".to_owned(),
            6 => input.dedup_key = "XYZ".to_owned(),
            7 => input.sensor = "bad sensor".to_owned(),
            8 => input.fingerprint.exe_digest = Some("zz".to_owned()),
            9 => input.record_digest = Some("AB".repeat(32)),
            _ => {}
        }
        let effective = input.effective_severity();
        let actions = dispatcher.offer(input, now);
        let shown = notices(&actions);
        shown_total = shown_total.saturating_add(u64::try_from(shown).unwrap_or(u64::MAX));
        if effective.requires_notice() {
            high_offers = high_offers.saturating_add(1);
            if !notified_first(&actions, effective) {
                return Err(TestError::Unexpected(format!("I3 {n}: {actions:?}")));
            }
        } else if shown > 1
            || (shown == 1 && !matches!(actions.first(), Some(DispatchAction::Notify { .. })))
        {
            return Err(TestError::Unexpected(format!("notice {n}: {actions:?}")));
        }
        let starved = actions
            .iter()
            .filter(|action| matches!(action, DispatchAction::Starved { .. }))
            .count();
        assert!(starved <= 1, "{n}: {actions:?}");
        // Ab und zu einen Auftrag vergeben und abschließen.
        if n.checked_rem(17) == Some(0)
            && let Some(job) = dispatcher.next_triage(now)
        {
            let outcome = if n.checked_rem(2) == Some(0) {
                TriageOutcome::Verdict
            } else {
                TriageOutcome::Failed
            };
            dispatcher.complete(&job.job_id(), outcome, now);
        }
    }
    let counters = dispatcher.counters();
    assert_eq!(counters.notified, shown_total);
    assert!(high_offers > 3_000, "{high_offers}");
    // Die Prüfgründe, Breaker und Budget wurden im Sturm wirklich begangen.
    for reason in [
        DropReason::InvalidFindingId,
        DropReason::UnverifiedSource,
        DropReason::UnknownRule,
        DropReason::InvalidDedupKey,
        DropReason::InvalidSensor,
        DropReason::InvalidFingerprint,
        DropReason::MissingRecordDigest,
        DropReason::InvalidRecordDigest,
        DropReason::BreakerOpen,
        DropReason::BudgetExhausted,
    ] {
        assert!(counters.dropped.get(reason) > 0, "{reason:?}");
    }
    assert!(counters.breaker_trips > 0);
    assert!(counters.budget_trips > 0);
    Ok(())
}

// Ein `Medium`-Befund mit neuem Schlüssel in Partition `(1001, 66)`.
fn noisy_offer(dispatcher: &mut Dispatcher, n: u32, now: i64) -> Vec<DispatchAction> {
    dispatcher.offer(
        placed(n, RULE_EGRESS_FLOW, Severity::Medium, 1001, 66, now),
        now,
    )
}

#[test]
fn test_budget_trip_is_reported_once_until_the_partition_is_full_again() -> TestResult {
    let config = DispatchConfig {
        budget: BudgetConfig {
            partition_capacity: 2,
            partition_refill_per_hour: 12,
            global_capacity: 1_000,
            ..BudgetConfig::default()
        },
        ..DispatchConfig::default()
    };
    let mut dispatcher = Dispatcher::new(config)?;
    let trip = |n: u32| {
        vec![
            DispatchAction::Starved {
                partition: partition(1001, 66),
                cause: StarvedCause::PartitionBudget,
            },
            DispatchAction::Dropped {
                finding_id: uuid(n),
                reason: DropReason::BudgetExhausted,
            },
        ]
    };
    let exhausted = |n: u32| {
        vec![DispatchAction::Dropped {
            finding_id: uuid(n),
            reason: DropReason::BudgetExhausted,
        }]
    };
    assert!(enqueued(&noisy_offer(&mut dispatcher, 1, NOW)).is_some());
    assert!(enqueued(&noisy_offer(&mut dispatcher, 2, NOW)).is_some());
    assert_eq!(noisy_offer(&mut dispatcher, 3, NOW), trip(3));
    assert_eq!(noisy_offer(&mut dispatcher, 4, NOW), exhausted(4));
    // Ein Token nach 300 s: kein neuer Trip, solange der Eimer nie voll war.
    let later = NOW.saturating_add(300);
    assert!(enqueued(&noisy_offer(&mut dispatcher, 5, later)).is_some());
    assert_eq!(noisy_offer(&mut dispatcher, 6, later), exhausted(6));
    // Nach 600 s Ruhe voll: die nächste Erschöpfung ist ein neuer Trip.
    let rested = later.saturating_add(600);
    assert!(enqueued(&noisy_offer(&mut dispatcher, 7, rested)).is_some());
    assert!(enqueued(&noisy_offer(&mut dispatcher, 8, rested)).is_some());
    assert_eq!(noisy_offer(&mut dispatcher, 9, rested), trip(9));
    assert_eq!(dispatcher.counters().budget_trips, 2);
    assert_eq!(dispatcher.counters().dropped.budget_exhausted, 4);
    assert_eq!(dispatcher.counters().breaker_trips, 0);
    Ok(())
}

#[test]
fn test_state_round_trip_mid_storm_keeps_limits_trips_and_behaviour() -> TestResult {
    let config = DispatchConfig::default();
    let mut original = Dispatcher::new(config)?;
    let mut n = 0_u32;
    // Partition 66 öffnet ihren Breaker (und leert ihren Eimer) …
    for _ in 0..80 {
        n = n.saturating_add(1);
        noisy_offer(&mut original, n, NOW);
    }
    // … und uid 1001 greift über viele Cgroups bis in ihre Reserve.
    for cgroup in 0..60_u64 {
        n = n.saturating_add(1);
        original.offer(
            placed(
                n,
                RULE_EXEC_WRITABLE_PATH,
                Severity::High,
                1001,
                cgroup.saturating_add(1_000),
                NOW,
            ),
            NOW,
        );
    }
    assert!(original.breakers().is_tripped(&partition(1001, 66)));
    assert_eq!(original.counters().breaker_trips, 1);
    // Partitions-Eimer von 66 und die Reserve-Quote von uid 1001.
    assert_eq!(original.counters().budget_trips, 2);
    assert_eq!(original.budget().uid_reserve_tokens(Some(1001)), Some(0));
    assert_eq!(original.in_flight_len(), 0);

    let bytes = original.snapshot().to_json()?;
    let mut restored = Dispatcher::restore(config, DispatchState::from_json_bounded(&bytes)?)?;
    assert_eq!(restored.snapshot(), original.snapshot());
    assert!(restored.breakers().is_tripped(&partition(1001, 66)));

    // Dieselben Eingaben an beide: identische Aktionen, kein zweiter Trip.
    for step in 1..=40_u32 {
        let now = NOW.saturating_add(i64::from(step.saturating_mul(3)));
        n = n.saturating_add(1);
        let input = if step.checked_rem(2) == Some(0) {
            placed(n, RULE_EGRESS_FLOW, Severity::Medium, 1001, 66, now)
        } else {
            placed(
                n,
                RULE_EXEC_WRITABLE_PATH,
                Severity::High,
                1001,
                u64::from(step).saturating_add(2_000),
                now,
            )
        };
        let expected = original.offer(input.clone(), now);
        let actual = restored.offer(input, now);
        assert_eq!(actual, expected, "step {step}");
        assert!(
            !actual
                .iter()
                .any(|action| matches!(action, DispatchAction::Starved { .. })),
            "step {step}: {actual:?}"
        );
    }
    // Auch die Vergabe ist identisch.
    let later = NOW.saturating_add(200);
    while let Some(job) = original.next_triage(later) {
        let twin = restored
            .next_triage(later)
            .ok_or_else(missing("restored job"))?;
        assert_eq!(twin, job);
        original.complete(&job.job_id(), TriageOutcome::Verdict, later);
        restored.complete(&twin.job_id(), TriageOutcome::Verdict, later);
    }
    assert!(restored.next_triage(later).is_none());
    assert_eq!(restored.snapshot(), original.snapshot());
    assert_eq!(restored.counters().breaker_trips, 1);
    assert_eq!(restored.counters().budget_trips, 2);
    Ok(())
}
