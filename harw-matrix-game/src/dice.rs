//! Deterministischer RNG und Adjudikationsmathematik (matrix-game.md §4.2, §4.3).
//!
//! Jeder Wurf nutzt einen **abgeleiteten** Seed
//! `sha256("harw-matrix/v1" ‖ master_seed ‖ round ‖ roll_kind ‖ 0 ‖ arg_id ‖ 0 ‖ attempt)`
//! → `ChaCha20Rng::from_seed`. Parallele Ausführung, Retries oder
//! Facilitator-Eingriffe ändern dadurch keine anderen Würfe.
//!
//! 2W6-Pro/Contra: `net = Σ top3(pro) − Σ top3(con) + m`,
//! `target = clamp(7 − net, 3, 11)`, Erfolg bei `2W6 ≥ target` und nicht
//! Pasch 1-1. Alternativ die W100-Leiter `5/15/30/50/70/85/95 %`.

use rand_chacha::ChaCha20Rng;
use rand_chacha::rand_core::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};

use crate::commitments::{DOMAIN, sha256_parts, to_hex};
use crate::error::{MatrixError, MatrixResult};
use crate::scenario::Rules;

/// Basiszielwert (7+ auf 2W6 ≈ 58 %).
pub const BASE_TARGET: i32 = 7;
/// Untere Zielwertschranke (P ≤ 97,2 %).
pub const MIN_TARGET: u8 = 3;
/// Obere Zielwertschranke (P ≥ 8,3 %).
pub const MAX_TARGET: u8 = 11;
/// Höchstgewicht eines Grundes.
pub const MAX_WEIGHT: u8 = 2;
/// Betragsgrenze des Kontext-Modifikators.
pub const MAX_MODIFIER: i32 = 2;
/// Es zählen höchstens so viele Gründe je Seite.
pub const COUNTED_REASONS: usize = 3;
/// Wiederholungsgrenze bei Konflikten.
pub const CONFLICT_CAP: u32 = 10;
/// Zulässige Stufen der W100-Leiter.
pub const LADDER: [u8; 7] = [5, 15, 30, 50, 70, 85, 95];

/// Art eines Wurfs (Teil des abgeleiteten Seeds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollKind {
    /// Normales Argument.
    Argument,
    /// Konfliktpaar.
    Conflict,
    /// Schlussargument.
    FinalArgument,
    /// Zufalls-Inject.
    Inject,
}

impl RollKind {
    /// Stabile Textform (Seed-Bestandteil — nie ändern).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Argument => "argument",
            Self::Conflict => "conflict",
            Self::FinalArgument => "final_argument",
            Self::Inject => "inject",
        }
    }
}

/// Würfelsystem eines Wurfs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiceSystem {
    /// 2W6 gegen Zielwert.
    TwoD6,
    /// W100 gegen Prozentwert (Erfolg bei Wurf ≤ p).
    D100,
}

/// Ergebnis eines Arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Gewürfelt, Erfolg.
    Success,
    /// Gewürfelt, Misserfolg.
    Failure,
    /// `no_roll` bei zwingendem Argument.
    AutoSuccess,
    /// Vom Umpire oder Facilitator verworfen.
    Vetoed,
    /// Kein Argument vorgebracht.
    Forfeited,
}

impl Outcome {
    /// Gilt als Erfolg (Erfolgszweig wird angewendet)?
    #[must_use]
    pub fn is_success(self) -> bool {
        matches!(self, Self::Success | Self::AutoSuccess)
    }
}

/// Nachträglicher Grad für die Erzählung (keine Zusatzeffekte).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grade {
    /// Wurf ≥ Ziel + 3.
    StrongSuccess,
    /// Erfolg.
    Success,
    /// Misserfolg.
    Failure,
    /// Wurf ≤ Ziel − 3.
    StrongFailure,
}

/// Ein journalisierter Wurf.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiceRoll {
    /// Argument-ID.
    pub argument_id: String,
    /// Runde.
    pub round: u32,
    /// Art.
    pub kind: RollKind,
    /// Versuch (0 = erster Wurf, 1 = Fail-Chit-Neuwurf bzw. Konfliktwiederholung).
    pub attempt: u32,
    /// System.
    pub system: DiceSystem,
    /// Augen (2W6: zwei Würfel; W100: ein Wert).
    pub dice: Vec<u8>,
    /// Summe.
    pub total: u16,
    /// Ziel (2W6: Zielwert; W100: Prozent).
    pub target: u8,
    /// Erfolg.
    pub success: bool,
    /// Grad.
    pub grade: Grade,
    /// Abgeleiteter Seed (Hex).
    pub sub_seed_hex: String,
}

/// Abgeleiteter Seed eines Wurfs.
#[must_use]
pub fn sub_seed(
    master_seed: &[u8; 32],
    round: u32,
    kind: RollKind,
    argument_id: &str,
    attempt: u32,
) -> [u8; 32] {
    let round_bytes = round.to_be_bytes();
    let attempt_bytes = attempt.to_be_bytes();
    sha256_parts(&[
        DOMAIN,
        master_seed,
        &round_bytes,
        kind.as_str().as_bytes(),
        &[0u8],
        argument_id.as_bytes(),
        &[0u8],
        &attempt_bytes,
    ])
}

/// Gleichverteilter Würfel `1..=sides` per Verwerfungsmethode (kein Bias).
fn die(rng: &mut ChaCha20Rng, sides: u32) -> u8 {
    let sides = sides.max(1);
    let zone = u32::MAX - (u32::MAX % sides);
    loop {
        let value = rng.next_u32();
        if value < zone {
            return u8::try_from(value % sides).unwrap_or(0).saturating_add(1);
        }
    }
}

/// Zwei W6 aus einem Seed.
#[must_use]
pub fn roll_2d6(seed: [u8; 32]) -> [u8; 2] {
    let mut rng = ChaCha20Rng::from_seed(seed);
    let first = die(&mut rng, 6);
    let second = die(&mut rng, 6);
    [first, second]
}

/// Ein W100 aus einem Seed.
#[must_use]
pub fn roll_d100(seed: [u8; 32]) -> u8 {
    let mut rng = ChaCha20Rng::from_seed(seed);
    die(&mut rng, 100)
}

/// Summe der höchstens drei höchsten Gewichte.
#[must_use]
pub fn top3_sum(weights: &[u8]) -> i32 {
    let mut sorted: Vec<u8> = weights.to_vec();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    sorted
        .iter()
        .take(COUNTED_REASONS)
        .map(|w| i32::from(*w))
        .sum()
}

/// Netto aus Gewichten und Modifikator.
///
/// # Errors
/// [`MatrixError::Contract`] bei Gewicht ∉ {0,1,2} oder Modifikator ∉ [−2, 2].
pub fn net_value(pro_weights: &[u8], con_weights: &[u8], modifier: i32) -> MatrixResult<i32> {
    let mut errors = Vec::new();
    for (i, w) in pro_weights.iter().enumerate() {
        if *w > MAX_WEIGHT {
            errors.push(format!("pro_weights[{i}] = {w} (erlaubt 0, 1, 2)"));
        }
    }
    for (i, w) in con_weights.iter().enumerate() {
        if *w > MAX_WEIGHT {
            errors.push(format!("con_weights[{i}] = {w} (erlaubt 0, 1, 2)"));
        }
    }
    if !(-MAX_MODIFIER..=MAX_MODIFIER).contains(&modifier) {
        errors.push(format!("context_modifier = {modifier} (erlaubt −2..=2)"));
    }
    if !errors.is_empty() {
        return Err(MatrixError::Contract(errors));
    }
    Ok(top3_sum(pro_weights) - top3_sum(con_weights) + modifier)
}

/// Zielwert `clamp(7 − net, 3, 11)`.
#[must_use]
pub fn target_for(net: i32) -> u8 {
    let target = (BASE_TARGET - net).clamp(i32::from(MIN_TARGET), i32::from(MAX_TARGET));
    u8::try_from(target).unwrap_or(MAX_TARGET)
}

/// Erfolg auf 2W6: Summe ≥ Ziel und kein Pasch 1-1.
#[must_use]
pub fn is_success_2d6(dice: [u8; 2], target: u8) -> bool {
    let [a, b] = dice;
    let sum = u16::from(a) + u16::from(b);
    sum >= u16::from(target) && !(a == 1 && b == 1)
}

/// Grad eines 2W6-Wurfs.
#[must_use]
pub fn grade_2d6(sum: u16, target: u8, success: bool) -> Grade {
    let sum = i32::from(sum);
    let target = i32::from(target);
    if success {
        if sum >= target + 3 {
            Grade::StrongSuccess
        } else {
            Grade::Success
        }
    } else if sum <= target - 3 {
        Grade::StrongFailure
    } else {
        Grade::Failure
    }
}

/// Exakte Erfolgswahrscheinlichkeit (Prozent) für einen 2W6-Zielwert.
#[must_use]
pub fn success_probability_2d6(target: u8) -> f64 {
    let mut hits = 0u32;
    for a in 1..=6u8 {
        for b in 1..=6u8 {
            if is_success_2d6([a, b], target) {
                hits += 1;
            }
        }
    }
    f64::from(hits) * 100.0 / 36.0
}

/// Gerundete Erfolgswahrscheinlichkeit (Prozent) für einen Zielwert.
#[must_use]
pub fn probability_pct_2d6(target: u8) -> u8 {
    let mut hits = 0u32;
    for a in 1..=6u8 {
        for b in 1..=6u8 {
            if is_success_2d6([a, b], target) {
                hits += 1;
            }
        }
    }
    // (hits * 100 + 18) / 36 == Rundung auf ganze Prozent
    u8::try_from((hits * 100 + 18) / 36).unwrap_or(100)
}

/// Ist `no_roll` bei diesem Netto zulässig?
#[must_use]
pub fn auto_success_allowed(net: i32, rules: &Rules) -> bool {
    rules.allow_auto_success && net >= rules.auto_success_net
}

/// Verbale Stufe der W100-Leiter.
#[must_use]
pub fn ladder_label(probability: u8) -> Option<&'static str> {
    match probability {
        5 => Some("nahezu ausgeschlossen"),
        15 => Some("sehr unwahrscheinlich"),
        30 => Some("unwahrscheinlich"),
        50 => Some("offen"),
        70 => Some("wahrscheinlich"),
        85 => Some("sehr wahrscheinlich"),
        95 => Some("nahezu sicher"),
        _ => None,
    }
}

/// Einzelwurf eines Arguments (2W6 gegen `target` bzw. W100 gegen `target` %).
#[must_use]
pub fn roll_once(
    master_seed: &[u8; 32],
    round: u32,
    kind: RollKind,
    argument_id: &str,
    attempt: u32,
    system: DiceSystem,
    target: u8,
) -> DiceRoll {
    let seed = sub_seed(master_seed, round, kind, argument_id, attempt);
    let (dice, total, success, grade) = match system {
        DiceSystem::TwoD6 => {
            let dice = roll_2d6(seed);
            let [a, b] = dice;
            let total = u16::from(a) + u16::from(b);
            let success = is_success_2d6(dice, target);
            (
                vec![a, b],
                total,
                success,
                grade_2d6(total, target, success),
            )
        }
        DiceSystem::D100 => {
            let value = roll_d100(seed);
            let success = value <= target;
            let grade = if success {
                Grade::Success
            } else {
                Grade::Failure
            };
            (vec![value], u16::from(value), success, grade)
        }
    };
    DiceRoll {
        argument_id: argument_id.to_owned(),
        round,
        kind,
        attempt,
        system,
        dice,
        total,
        target,
        success,
        grade,
        sub_seed_hex: to_hex(&seed),
    }
}

/// Replay-Prüfung: ergibt die Neuberechnung exakt denselben Wurf?
#[must_use]
pub fn verify_roll(master_seed: &[u8; 32], roll: &DiceRoll) -> bool {
    roll_once(
        master_seed,
        roll.round,
        roll.kind,
        &roll.argument_id,
        roll.attempt,
        roll.system,
        roll.target,
    ) == *roll
}

/// Aufgelöster Wurf eines Arguments inklusive optionalem Fail-Chit-Neuwurf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRoll {
    /// Alle Würfe (1 oder 2).
    pub rolls: Vec<DiceRoll>,
    /// Ergebnis des zählenden (letzten) Wurfs.
    pub success: bool,
    /// Grad des zählenden Wurfs.
    pub grade: Grade,
    /// Fail-Chit eingesetzt.
    pub used_fail_chit: bool,
}

/// Würfelt ein Argument; bei Misserfolg und verfügbarem Fail-Chit genau ein
/// Neuwurf (Versuch 1), der zweite Wurf zählt.
#[must_use]
pub fn resolve_roll(
    master_seed: &[u8; 32],
    round: u32,
    kind: RollKind,
    argument_id: &str,
    system: DiceSystem,
    target: u8,
    fail_chit_available: bool,
) -> ResolvedRoll {
    let first = roll_once(master_seed, round, kind, argument_id, 0, system, target);
    if first.success || !fail_chit_available {
        return ResolvedRoll {
            success: first.success,
            grade: first.grade,
            rolls: vec![first],
            used_fail_chit: false,
        };
    }
    let second = roll_once(master_seed, round, kind, argument_id, 1, system, target);
    ResolvedRoll {
        success: second.success,
        grade: second.grade,
        rolls: vec![first, second],
        used_fail_chit: true,
    }
}

/// Ergebnis eines Konfliktpaars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictOutcome {
    /// Sieger-Argument.
    pub winner: String,
    /// Unterlegenes Argument.
    pub loser: String,
    /// Alle Würfe (paarweise je Versuch).
    pub rolls: Vec<DiceRoll>,
    /// Anzahl Versuche.
    pub attempts: u32,
    /// |Differenz der Würfe| im entscheidenden Versuch.
    pub margin: u16,
    /// Entscheidung erst über die Wiederholungsgrenze.
    pub decided_by_cap: bool,
}

/// Konflikt (matrix-game.md §4.2): beide würfeln, bis genau einer Erfolg hat
/// (höchstens [`CONFLICT_CAP`] Versuche); danach gewinnt die größere Marge
/// `Wurf − Ziel`, bei Gleichstand der in Auflösungsreihenfolge frühere
/// (`first`).
#[must_use]
pub fn resolve_conflict(
    master_seed: &[u8; 32],
    round: u32,
    first: (&str, u8),
    second: (&str, u8),
) -> ConflictOutcome {
    let (id_a, target_a) = first;
    let (id_b, target_b) = second;
    let mut rolls = Vec::new();
    let mut last: Option<(DiceRoll, DiceRoll)> = None;
    for attempt in 0..CONFLICT_CAP {
        let a = roll_once(
            master_seed,
            round,
            RollKind::Conflict,
            id_a,
            attempt,
            DiceSystem::TwoD6,
            target_a,
        );
        let b = roll_once(
            master_seed,
            round,
            RollKind::Conflict,
            id_b,
            attempt,
            DiceSystem::TwoD6,
            target_b,
        );
        rolls.push(a.clone());
        rolls.push(b.clone());
        if a.success != b.success {
            let (winner, loser) = if a.success {
                (id_a, id_b)
            } else {
                (id_b, id_a)
            };
            return ConflictOutcome {
                winner: winner.to_owned(),
                loser: loser.to_owned(),
                margin: a.total.abs_diff(b.total),
                rolls,
                attempts: attempt + 1,
                decided_by_cap: false,
            };
        }
        last = Some((a, b));
    }
    let (margin_a, margin_b, margin) = match &last {
        Some((a, b)) => (
            i32::from(a.total) - i32::from(target_a),
            i32::from(b.total) - i32::from(target_b),
            a.total.abs_diff(b.total),
        ),
        None => (0, 0, 0),
    };
    let (winner, loser) = if margin_b > margin_a {
        (id_b, id_a)
    } else {
        (id_a, id_b)
    };
    ConflictOutcome {
        winner: winner.to_owned(),
        loser: loser.to_owned(),
        rolls,
        attempts: CONFLICT_CAP,
        margin,
        decided_by_cap: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn master(n: u64) -> [u8; 32] {
        sha256_parts(&[b"test-master", &n.to_be_bytes()])
    }

    #[test]
    fn target_is_clamped_for_all_weight_combinations() -> TestResult {
        for net in -8..=8 {
            let t = target_for(net);
            assert!((MIN_TARGET..=MAX_TARGET).contains(&t), "net {net} → {t}");
        }
        assert_eq!(target_for(0), 7);
        assert_eq!(target_for(4), 3);
        assert_eq!(target_for(8), 3);
        assert_eq!(target_for(-4), 11);
        assert_eq!(target_for(-8), 11);
        // exhaustiv über gültige Gewichte
        for p in 0..=2u8 {
            for c in 0..=2u8 {
                for m in -2..=2 {
                    let net = net_value(&[p, p, p], &[c, c], m)?;
                    assert!((MIN_TARGET..=MAX_TARGET).contains(&target_for(net)));
                }
            }
        }
        Ok(())
    }

    #[test]
    fn design_doc_example_net() -> TestResult {
        // pro [2,1,1], con rat [2] + mission [1], m = −1 → 4 − 3 − 1 = 0 → 7+
        let net = net_value(&[2, 1, 1], &[2, 1], -1)?;
        assert_eq!(net, 0);
        assert_eq!(target_for(net), 7);
        Ok(())
    }

    #[test]
    fn only_top_three_reasons_count() -> TestResult {
        assert_eq!(top3_sum(&[1, 2, 0, 2, 2, 1]), 6);
        assert_eq!(net_value(&[2, 2, 2, 2, 2], &[], 0)?, 6);
        assert_eq!(net_value(&[1], &[2, 2, 2, 2], 0)?, -5);
        Ok(())
    }

    #[test]
    fn invalid_weights_and_modifier_are_rejected() {
        assert!(matches!(
            net_value(&[3], &[], 0),
            Err(MatrixError::Contract(_))
        ));
        assert!(matches!(
            net_value(&[1], &[5], 0),
            Err(MatrixError::Contract(_))
        ));
        assert!(matches!(
            net_value(&[1], &[], 3),
            Err(MatrixError::Contract(_))
        ));
        assert!(matches!(
            net_value(&[1], &[], -3),
            Err(MatrixError::Contract(_))
        ));
    }

    #[test]
    fn double_one_always_fails() {
        for target in MIN_TARGET..=MAX_TARGET {
            assert!(!is_success_2d6([1, 1], target));
        }
        assert!(is_success_2d6([1, 2], 3));
        assert!(is_success_2d6([6, 6], MAX_TARGET));
    }

    #[test]
    fn probability_table_matches_design() {
        let expected = [
            (3, 97.2),
            (4, 91.7),
            (5, 83.3),
            (6, 72.2),
            (7, 58.3),
            (8, 41.7),
            (9, 27.8),
            (10, 16.7),
            (11, 8.3),
        ];
        for (target, pct) in expected {
            let p = success_probability_2d6(target);
            assert!((p - pct).abs() < 0.06, "target {target}: {p} vs {pct}");
        }
        assert_eq!(probability_pct_2d6(7), 58);
        assert_eq!(probability_pct_2d6(3), 97);
    }

    #[test]
    fn auto_success_threshold() {
        let rules = Rules::default();
        assert!(auto_success_allowed(5, &rules));
        assert!(!auto_success_allowed(4, &rules));
        let off = Rules {
            allow_auto_success: false,
            ..Rules::default()
        };
        assert!(!auto_success_allowed(8, &off));
    }

    #[test]
    fn grades() {
        assert_eq!(grade_2d6(10, 7, true), Grade::StrongSuccess);
        assert_eq!(grade_2d6(9, 7, true), Grade::Success);
        assert_eq!(grade_2d6(6, 7, false), Grade::Failure);
        assert_eq!(grade_2d6(4, 7, false), Grade::StrongFailure);
    }

    #[test]
    fn rolls_are_deterministic_and_order_independent() {
        let m = master(1);
        let ids = ["r1-a1", "r1-a2", "r1-a3", "r1-a4"];
        let forward: Vec<DiceRoll> = ids
            .iter()
            .map(|id| roll_once(&m, 1, RollKind::Argument, id, 0, DiceSystem::TwoD6, 7))
            .collect();
        let mut backward: Vec<DiceRoll> = ids
            .iter()
            .rev()
            .map(|id| roll_once(&m, 1, RollKind::Argument, id, 0, DiceSystem::TwoD6, 7))
            .collect();
        backward.reverse();
        assert_eq!(forward, backward);
        for roll in &forward {
            assert!(verify_roll(&m, roll));
            assert!(roll.dice.iter().all(|d| (1..=6).contains(d)));
        }
    }

    #[test]
    fn seed_components_all_matter() {
        let m = master(2);
        let base = sub_seed(&m, 1, RollKind::Argument, "r1-a1", 0);
        assert_ne!(
            base,
            sub_seed(&master(3), 1, RollKind::Argument, "r1-a1", 0)
        );
        assert_ne!(base, sub_seed(&m, 2, RollKind::Argument, "r1-a1", 0));
        assert_ne!(base, sub_seed(&m, 1, RollKind::Conflict, "r1-a1", 0));
        assert_ne!(base, sub_seed(&m, 1, RollKind::Argument, "r1-a2", 0));
        assert_ne!(base, sub_seed(&m, 1, RollKind::Argument, "r1-a1", 1));
        // Trennbytes verhindern Verschiebungskollisionen
        assert_ne!(
            sub_seed(&m, 1, RollKind::Argument, "ab", 0),
            sub_seed(&m, 1, RollKind::Argument, "a", 0)
        );
    }

    #[test]
    fn tampered_roll_fails_verification() {
        let m = master(4);
        let mut roll = roll_once(&m, 3, RollKind::Argument, "r3-a2", 0, DiceSystem::TwoD6, 7);
        assert!(verify_roll(&m, &roll));
        roll.success = !roll.success;
        assert!(!verify_roll(&m, &roll));
    }

    #[test]
    fn distribution_matches_table_within_one_percent() {
        const N: u32 = 100_000;
        let mut counts = [0u32; 13];
        let mut double_ones = 0u32;
        for i in 0..N {
            let seed = sub_seed(&master(u64::from(i)), 1, RollKind::Argument, "dist", 0);
            let [a, b] = roll_2d6(seed);
            if a == 1 && b == 1 {
                double_ones += 1;
            }
            counts[usize::from(a + b)] += 1;
        }
        for target in MIN_TARGET..=MAX_TARGET {
            let hits: u32 = (usize::from(target)..=12).map(|s| counts[s]).sum();
            let observed = f64::from(hits) * 100.0 / f64::from(N);
            let expected = success_probability_2d6(target);
            assert!(
                (observed - expected).abs() <= 1.0,
                "target {target}: beobachtet {observed:.2} %, erwartet {expected:.2} %"
            );
        }
        let p_double_one = f64::from(double_ones) * 100.0 / f64::from(N);
        assert!((p_double_one - 100.0 / 36.0).abs() <= 1.0);
    }

    #[test]
    fn d100_is_in_range_and_ladder_is_closed() {
        for i in 0..2_000u64 {
            let v = roll_d100(master(i));
            assert!((1..=100).contains(&v));
        }
        for p in LADDER {
            assert!(ladder_label(p).is_some());
        }
        assert!(ladder_label(42).is_none());
    }

    #[test]
    fn fail_chit_rerolls_exactly_once() -> TestResult {
        // Suche einen Seed mit Misserfolg im ersten Wurf.
        for i in 0..500u64 {
            let m = master(1_000 + i);
            let first = roll_once(&m, 1, RollKind::Argument, "r1-a1", 0, DiceSystem::TwoD6, 11);
            if first.success {
                continue;
            }
            let without = resolve_roll(
                &m,
                1,
                RollKind::Argument,
                "r1-a1",
                DiceSystem::TwoD6,
                11,
                false,
            );
            assert_eq!(without.rolls.len(), 1);
            assert!(!without.used_fail_chit);
            let with = resolve_roll(
                &m,
                1,
                RollKind::Argument,
                "r1-a1",
                DiceSystem::TwoD6,
                11,
                true,
            );
            assert_eq!(with.rolls.len(), 2);
            assert!(with.used_fail_chit);
            assert_eq!(with.rolls.get(1).map(|r| r.attempt), Some(1));
            assert_eq!(with.success, with.rolls.get(1).is_some_and(|r| r.success));
            return Ok(());
        }
        Err("kein Misserfolg in 500 Seeds gefunden".into())
    }

    #[test]
    fn conflicts_resolve_to_exactly_one_winner() {
        for i in 0..200u64 {
            let m = master(5_000 + i);
            let out = resolve_conflict(&m, 2, ("r2-a1", 7), ("r2-a3", 7));
            assert_ne!(out.winner, out.loser);
            assert!(out.attempts >= 1 && out.attempts <= CONFLICT_CAP);
            if !out.decided_by_cap {
                let n = out.rolls.len();
                let a = out.rolls.get(n - 2).map(|r| r.success);
                let b = out.rolls.get(n - 1).map(|r| r.success);
                assert_ne!(a, b);
            }
            assert_eq!(out, resolve_conflict(&m, 2, ("r2-a1", 7), ("r2-a3", 7)));
        }
    }
}
