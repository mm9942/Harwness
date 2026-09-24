//! Inject-Bibliothek mit Varianz-Paketen (matrix-game.md §11.5).
//!
//! Ein Szenario kann unter `[[inject_packages]]` benannte Sätze möglicher
//! Ereignisse hinterlegen. Ein Lauf wählt genau ein Paket (vorgegeben oder
//! seed-deterministisch) und zieht daraus höchstens drei Injects — nie zwei
//! in derselben Runde und nie in einer Runde, in der schon ein festes
//! Szenario-Inject wirkt. Die Auswahl hängt nur vom Master-Seed und vom
//! Szenario ab, wird als [`EntryKind::InjectPackageSelected`] (nur
//! Beobachter) journalisiert und beim Replay nachgerechnet. Mehrere Läufe
//! über verschiedene Seeds ergeben so unterschiedliche, aber jeweils
//! reproduzierbare Verläufe (Vergleich: [`crate::lessons::compare_runs`]).

use jiff::Timestamp;
use rand_chacha::ChaCha20Rng;
use rand_chacha::rand_core::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};

use crate::commitments::{DOMAIN, sha256_parts};
use crate::error::{MatrixError, MatrixResult};
use crate::scenario::{MAX_PACKAGE_INJECTS, Scenario, ScheduledInject};
use crate::state::{Audience, EntryKind, GameEntry, GameLog, Journal};

/// Ein gezogenes Inject mit seiner Runde.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedInject {
    /// Inject-ID aus dem Paket.
    pub inject_id: String,
    /// Runde, zu deren Beginn es wirkt.
    pub round: u32,
}

/// Auswahl eines Laufs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InjectPackagePlan {
    /// Paket.
    pub package_id: String,
    /// Gezogene Injects, nach Runde sortiert.
    pub injects: Vec<PlannedInject>,
}

/// Gleichverteilter Index `0..n` per Verwerfungsmethode (kein Bias).
fn uniform(rng: &mut ChaCha20Rng, n: usize) -> usize {
    let n32 = u32::try_from(n.max(1)).unwrap_or(u32::MAX);
    let zone = u32::MAX - (u32::MAX % n32);
    loop {
        let value = rng.next_u32();
        if value < zone {
            return usize::try_from(value % n32).unwrap_or(0);
        }
    }
}

/// Berechnet die Auswahl eines Laufs, ohne etwas zu journalisieren.
/// `package = None` wählt das Paket seed-deterministisch. `Ok(None)`, wenn
/// das Szenario keine Bibliothek hat.
///
/// # Errors
/// [`MatrixError::State`], wenn ein genanntes Paket nicht existiert.
pub fn plan_inject_package(
    scenario: &Scenario,
    master_seed: &[u8; 32],
    package: Option<&str>,
) -> MatrixResult<Option<InjectPackagePlan>> {
    let packages = scenario.inject_packages();
    if packages.is_empty() {
        return match package {
            None => Ok(None),
            Some(id) => Err(MatrixError::State(format!(
                "Inject-Paket `{id}` unbekannt (Szenario hat keine Bibliothek)"
            ))),
        };
    }
    let chosen = match package {
        Some(id) => packages
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| MatrixError::State(format!("Inject-Paket `{id}` unbekannt")))?,
        None => {
            let mut rng = ChaCha20Rng::from_seed(sha256_parts(&[
                DOMAIN,
                b"/inject-package/choice",
                master_seed,
            ]));
            let index = uniform(&mut rng, packages.len());
            packages
                .get(index)
                .ok_or_else(|| MatrixError::State("Paketwahl außerhalb der Liste".to_owned()))?
        }
    };
    let mut rng = ChaCha20Rng::from_seed(sha256_parts(&[
        DOMAIN,
        b"/inject-package/draw",
        master_seed,
        chosen.id.as_bytes(),
    ]));
    let rounds = scenario.rounds();
    let mut used: Vec<u32> = (1..=rounds)
        .filter(|r| !scenario.injects_for_round(*r).is_empty())
        .collect();
    // Fisher-Yates über die Kandidaten.
    let mut order: Vec<usize> = (0..chosen.injects.len()).collect();
    for i in (1..order.len()).rev() {
        let j = uniform(&mut rng, i + 1);
        order.swap(i, j);
    }
    let limit = chosen.max_injects.min(MAX_PACKAGE_INJECTS);
    let mut picked = Vec::new();
    for index in order {
        if picked.len() >= limit {
            break;
        }
        let Some(inject) = chosen.injects.get(index) else {
            continue;
        };
        let earliest = inject.earliest.unwrap_or(1).max(1);
        let latest = inject.latest.unwrap_or(rounds).min(rounds);
        let free: Vec<u32> = (earliest..=latest).filter(|r| !used.contains(r)).collect();
        if free.is_empty() {
            continue;
        }
        let round = free
            .get(uniform(&mut rng, free.len()))
            .copied()
            .unwrap_or(earliest);
        used.push(round);
        picked.push(PlannedInject {
            inject_id: inject.id.clone(),
            round,
        });
    }
    picked.sort_by(|a, b| a.round.cmp(&b.round).then(a.inject_id.cmp(&b.inject_id)));
    Ok(Some(InjectPackagePlan {
        package_id: chosen.id.clone(),
        injects: picked,
    }))
}

/// Journalisierte Auswahl (falls vorhanden).
#[must_use]
pub fn selected_package(journal: &Journal) -> Option<InjectPackagePlan> {
    journal.entries().find_map(|e| match &e.kind {
        EntryKind::InjectPackageSelected { package_id, plan } => Some(InjectPackagePlan {
            package_id: package_id.clone(),
            injects: plan.clone(),
        }),
        _ => None,
    })
}

/// Wählt und journalisiert das Paket des Laufs (einmal, direkt nach
/// [`crate::phases::open_game`]). `Ok(None)` ohne Bibliothek.
///
/// # Errors
/// [`MatrixError::Phase`], wenn schon gewählt wurde; Fehler aus
/// [`plan_inject_package`].
pub fn record_inject_package(
    log: &mut GameLog,
    scenario: &Scenario,
    package: Option<&str>,
    at: Option<Timestamp>,
) -> MatrixResult<Option<InjectPackagePlan>> {
    if selected_package(&log.journal).is_some() {
        return Err(MatrixError::Phase(
            "Inject-Paket wurde für diesen Lauf bereits gewählt".to_owned(),
        ));
    }
    let master = log.state.master_seed()?;
    let Some(plan) = plan_inject_package(scenario, &master, package)? else {
        return Ok(None);
    };
    log.record(
        GameEntry::new(
            0,
            Audience::ObserverOnly,
            EntryKind::InjectPackageSelected {
                package_id: plan.package_id.clone(),
                plan: plan.injects.clone(),
            },
        ),
        at,
    )?;
    Ok(Some(plan))
}

/// Für `round` fällige Injects aus der journalisierten Paketauswahl — zu
/// Rundenbeginn zusätzlich zu [`Scenario::injects_for_round`] anzuwenden.
#[must_use]
pub fn package_injects_for_round(
    scenario: &Scenario,
    journal: &Journal,
    round: u32,
) -> Vec<ScheduledInject> {
    let Some(plan) = selected_package(journal) else {
        return Vec::new();
    };
    let Some(package) = scenario
        .inject_packages()
        .iter()
        .find(|p| p.id == plan.package_id)
    else {
        return Vec::new();
    };
    plan.injects
        .iter()
        .filter(|p| p.round == round)
        .filter_map(|p| package.injects.iter().find(|i| i.id == p.inject_id))
        .map(|i| ScheduledInject {
            id: i.id.clone(),
            round,
            audiences: vec![i.audience.0.clone()],
            text: i.text.clone(),
            effects: i.effects.clone(),
            attributed: i.attributed,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phases::open_game;
    use crate::scenario::load_scenario;
    use crate::state::replay;
    use crate::test_support::{Fixture, KARST, karst_extended};

    #[test]
    fn selection_is_deterministic_bounded_and_spread() -> Fixture<()> {
        let loaded = load_scenario(&karst_extended()?)?;
        let scenario = &loaded.scenario;
        let blocked: Vec<u32> = (1..=scenario.rounds())
            .filter(|r| !scenario.injects_for_round(*r).is_empty())
            .collect();
        let mut packages_seen = std::collections::BTreeSet::new();
        for seed in 0u8..40 {
            let master = [seed; 32];
            let a = plan_inject_package(scenario, &master, None)?.ok_or("kein Plan")?;
            let b = plan_inject_package(scenario, &master, None)?.ok_or("kein Plan")?;
            assert_eq!(a, b, "gleicher Seed, gleiche Auswahl");
            packages_seen.insert(a.package_id.clone());
            assert!(a.injects.len() <= MAX_PACKAGE_INJECTS);
            let mut rounds: Vec<u32> = a.injects.iter().map(|p| p.round).collect();
            rounds.dedup();
            assert_eq!(rounds.len(), a.injects.len(), "zwei Injects in einer Runde");
            for p in &a.injects {
                assert!(!blocked.contains(&p.round), "Kollision mit festem Inject");
            }
        }
        assert_eq!(packages_seen.len(), 2, "beide Pakete kommen vor");
        // Explizite Wahl und unbekanntes Paket
        let fixed = plan_inject_package(scenario, &[1u8; 32], Some("unruhen"))?;
        assert_eq!(fixed.map(|p| p.package_id).as_deref(), Some("unruhen"));
        assert!(plan_inject_package(scenario, &[1u8; 32], Some("gibtsnicht")).is_err());
        // Ohne Bibliothek: kein Plan
        let plain = load_scenario(KARST)?;
        assert_eq!(
            plan_inject_package(&plain.scenario, &[1u8; 32], None)?,
            None
        );
        Ok(())
    }

    #[test]
    fn recorded_selection_replays_and_detects_tampering() -> Fixture<()> {
        let loaded = load_scenario(&karst_extended()?)?;
        let mut log = open_game(&loaded, &[9u8; 32], None)?;
        let plan = record_inject_package(&mut log, &loaded.scenario, Some("sturm"), None)?
            .ok_or("kein Plan")?;
        assert!(record_inject_package(&mut log, &loaded.scenario, None, None).is_err());
        let due: usize = (1..=loaded.scenario.rounds())
            .map(|r| package_injects_for_round(&loaded.scenario, &log.journal, r).len())
            .sum();
        assert_eq!(due, plan.injects.len());
        replay(&loaded.scenario, &log.journal)?;

        // Manipulierte Runde fällt beim Replay auf.
        let text = log.journal.to_jsonl()?;
        let first = plan.injects.first().ok_or("leerer Plan")?;
        let needle = format!(
            "\"inject_id\":\"{}\",\"round\":{}",
            first.inject_id, first.round
        );
        let forged = text.replacen(
            &needle,
            &format!("\"inject_id\":\"{}\",\"round\":99", first.inject_id),
            1,
        );
        assert_ne!(forged, text, "Muster nicht gefunden");
        let journal = Journal::from_jsonl(&forged)?;
        assert!(matches!(
            replay(&loaded.scenario, &journal),
            Err(MatrixError::Replay(_))
        ));
        Ok(())
    }
}
