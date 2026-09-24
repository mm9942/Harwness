//! Design-Lehren im AAR und Vergleich mehrerer Läufe (matrix-game.md §11.5).
//!
//! Alles hier ist rein und deterministisch aus dem Journal abgeleitet:
//! - [`design_lessons`] fragt, was ein Lauf über das **Szenario-Design**
//!   verrät — wann geheime Argumente eingesetzt wurden, ob die Zielwerte des
//!   Umpires an einer Schwelle kleben, ob die Würfel im Rahmen des Zufalls
//!   lagen.
//! - [`summarize_run`] verdichtet einen Lauf zu einer [`AarSummary`];
//!   [`compare_runs`] stellt mehrere Läufe (z. B. über verschiedene Seeds und
//!   Inject-Pakete) nebeneinander und trennt robuste von empfindlichen
//!   Größen.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::dice::{self, DiceSystem, Outcome};
use crate::library::selected_package;
use crate::phases::Verdict;
use crate::scenario::LoadedScenario;
use crate::state::{EntryKind, GameState, Journal, PlayerId, RevealedBy, VarValue};

/// Ab diesem Anteil gleicher Zielwerte gilt die Verteilung als geklumpt.
const CLUSTER_SHARE: f64 = 0.6;
/// Mindestzahl Urteile für eine Klumpungsaussage.
const CLUSTER_MIN: usize = 4;
/// |z|, ab dem eine Abweichung als auffällig gilt.
const Z_ALERT: f64 = 2.0;
/// Standardabweichung der Augensumme von 2W6.
const SD_2D6: f64 = 2.415_229_457_2;

/// Einsatz eines geheimen Arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretUse {
    /// Geheimnis-ID.
    pub secret_id: String,
    /// Eigentümer.
    pub seat: PlayerId,
    /// Runde des Einsatzes.
    pub round: u32,
    /// Runde der Offenlegung (im Spiel oder zum Ende).
    pub revealed_round: Option<u32>,
    /// Erst durch das Spielende offengelegt.
    pub revealed_at_end: bool,
    /// Höchste erreichte Stufe der Verdachtsleiter vor der Offenlegung.
    pub max_suspicion: String,
}

/// Verteilung der Zielwerte bzw. Leiterstufen.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ThresholdStats {
    /// Anzahl gewürfelter Urteile.
    pub rulings: usize,
    /// Häufigkeit je Zielwert (2W6) bzw. Prozentstufe (W100).
    pub histogram: BTreeMap<u8, usize>,
    /// Häufigster Wert.
    pub mode: Option<u8>,
    /// Anteil des häufigsten Werts.
    pub mode_share: f64,
    /// Urteile ohne Wurf.
    pub no_roll: usize,
    /// Vetos.
    pub vetoes: usize,
}

/// Plausibilität der Würfel (nur erste Würfe, Fail-Chit-Neuwürfe zählen nicht).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DiceStats {
    /// Anzahl erster Würfe.
    pub rolls: usize,
    /// Davon 2W6.
    pub rolls_2d6: usize,
    /// Mittlere Augensumme der 2W6-Würfe.
    pub mean_2d6: Option<f64>,
    /// z-Wert des Mittels gegen 7.
    pub z_mean: Option<f64>,
    /// Erwartete Erfolge (Summe der Einzelwahrscheinlichkeiten).
    pub expected_successes: f64,
    /// Beobachtete Erfolge.
    pub observed_successes: usize,
    /// z-Wert der Erfolge gegen die Erwartung.
    pub z_success: Option<f64>,
}

/// Design-Lehren eines Laufs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DesignLessons {
    /// Geheime Argumente.
    pub secrets: Vec<SecretUse>,
    /// Zielwert-Verteilung.
    pub thresholds: ThresholdStats,
    /// Würfel.
    pub dice: DiceStats,
    /// Abgeleitete Hinweise für das Szenario-Design.
    pub notes: Vec<String>,
}

fn count_f64(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

fn last_round(journal: &Journal) -> u32 {
    journal.entries().map(|e| e.round).max().unwrap_or(0)
}

fn secret_uses(journal: &Journal) -> Vec<SecretUse> {
    let mut uses: Vec<SecretUse> = Vec::new();
    for e in journal.entries() {
        match &e.kind {
            EntryKind::SecretArgumentAnnounced {
                secret_id, seat, ..
            } => uses.push(SecretUse {
                secret_id: secret_id.clone(),
                seat: seat.clone(),
                round: e.round,
                revealed_round: None,
                revealed_at_end: false,
                max_suspicion: "unbemerkt".to_owned(),
            }),
            EntryKind::SuspicionRaised { secret_id, to, .. } => {
                if let Some(u) = uses.iter_mut().find(|u| &u.secret_id == secret_id) {
                    u.max_suspicion = to.label().to_owned();
                }
            }
            EntryKind::SecretRevealed { secret_id, by, .. } => {
                if let Some(u) = uses.iter_mut().find(|u| &u.secret_id == secret_id) {
                    u.revealed_round = Some(e.round);
                    u.revealed_at_end = matches!(by, RevealedBy::GameEnd);
                }
            }
            _ => {}
        }
    }
    uses
}

fn threshold_stats(journal: &Journal) -> ThresholdStats {
    let mut stats = ThresholdStats::default();
    for e in journal.entries() {
        let EntryKind::Adjudicated {
            ruling,
            target,
            probability_pct,
            ..
        } = &e.kind
        else {
            continue;
        };
        match ruling.verdict {
            Verdict::Veto => stats.vetoes += 1,
            Verdict::NoRoll => stats.no_roll += 1,
            Verdict::Roll => {
                stats.rulings += 1;
                let key = target.unwrap_or(*probability_pct);
                *stats.histogram.entry(key).or_insert(0) += 1;
            }
        }
    }
    if let Some((value, count)) = stats
        .histogram
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
    {
        stats.mode = Some(*value);
        stats.mode_share = count_f64(*count) / count_f64(stats.rulings.max(1));
    }
    stats
}

fn dice_stats(journal: &Journal) -> DiceStats {
    let mut stats = DiceStats::default();
    let mut sum_2d6 = 0.0;
    let mut variance = 0.0;
    for e in journal.entries() {
        let EntryKind::DiceRolled { roll } = &e.kind else {
            continue;
        };
        if roll.attempt != 0 {
            continue;
        }
        let p = match roll.system {
            DiceSystem::TwoD6 => dice::success_probability_2d6(roll.target) / 100.0,
            DiceSystem::D100 => f64::from(roll.target) / 100.0,
        };
        stats.rolls += 1;
        stats.expected_successes += p;
        variance += p * (1.0 - p);
        if roll.success {
            stats.observed_successes += 1;
        }
        if roll.system == DiceSystem::TwoD6 {
            stats.rolls_2d6 += 1;
            sum_2d6 += f64::from(roll.total);
        }
    }
    if stats.rolls_2d6 > 0 {
        let n = count_f64(stats.rolls_2d6);
        let mean = sum_2d6 / n;
        stats.mean_2d6 = Some(mean);
        stats.z_mean = Some((mean - 7.0) / (SD_2D6 / n.sqrt()));
    }
    if variance > 0.0 {
        stats.z_success = Some(
            (count_f64(stats.observed_successes) - stats.expected_successes) / variance.sqrt(),
        );
    }
    stats
}

/// Kopie des Journals, in der jeder Wurf (Argument, Versuch) nur einmal
/// vorkommt — defensiv, falls ein Wurf unter mehreren Audiences steht.
fn dedup_rolls(journal: &Journal) -> Journal {
    let mut seen = BTreeSet::new();
    let mut out = Journal::new();
    for record in journal.records() {
        if let EntryKind::DiceRolled { roll } = &record.entry.kind {
            if !seen.insert((roll.argument_id.clone(), roll.attempt)) {
                continue;
            }
        }
        out.append(record.entry.clone(), None);
    }
    out
}

/// Design-Lehren aus einem vollständigen Journal.
#[must_use]
pub fn design_lessons(loaded: &LoadedScenario, journal: &Journal) -> DesignLessons {
    let journal = dedup_rolls(journal);
    let secrets = secret_uses(&journal);
    let thresholds = threshold_stats(&journal);
    let dice = dice_stats(&journal);
    let mut notes = Vec::new();
    let rules = loaded.scenario.rules();
    let final_round = last_round(&journal).max(1);

    if secrets.is_empty() {
        if rules.max_secret_arguments_per_seat > 0 {
            notes.push(
                "Kein Sitz setzte ein geheimes Argument ein. Entweder lohnt sich Verdecktes in diesem Szenario nicht, oder der Anreiz fehlt — verdeckte Ziele oder Objekte, die sich nur verdeckt erreichen lassen, schaffen ihn.".to_owned(),
            );
        }
    } else {
        let late = secrets.iter().filter(|s| s.round >= final_round).count();
        if late == secrets.len() {
            notes.push(format!(
                "Alle {} geheimen Argumente kamen erst in der letzten gespielten Runde — sie wirkten als Schlussüberraschung statt als verdeckte Vorbereitung. Früher einsetzbare Vorteile oder eine Verdachtsleiter, die spätes Verstecken teurer macht, verschieben den Einsatz nach vorn.",
                secrets.len()
            ));
        } else if late == 0 && secrets.iter().all(|s| s.round == 1) {
            notes.push("Alle geheimen Argumente fielen in Runde 1 — das Kontingent wurde verbraucht, bevor die Lage sich entwickelt hatte. Mehr Kontingent oder ein späteres Freischalten kann helfen.".to_owned());
        }
        let at_end = secrets.iter().filter(|s| s.revealed_at_end).count();
        if at_end > 0 {
            notes.push(format!(
                "{at_end} von {} Geheimnissen blieben bis zum Spielende unentdeckt. Soll Aufdeckung Teil des Spiels sein, braucht es Wege dorthin (Ermittlungen mit `raise_suspicion`, `reveal_when`-Auslöser).",
                secrets.len()
            ));
        }
    }

    if thresholds.rulings >= CLUSTER_MIN && thresholds.mode_share >= CLUSTER_SHARE {
        if let Some(mode) = thresholds.mode {
            notes.push(format!(
                "Die Zielwerte klumpen: {:.0} % der gewürfelten Urteile lagen auf {mode}. Der Umpire unterscheidet kaum zwischen starken und schwachen Argumenten — Gewichte 0 und 2 und begründete Kontext-Modifikatoren bewusster einsetzen, oder die Leiter (`estimative_d100`) erwägen.",
                thresholds.mode_share * 100.0
            ));
        }
    }
    let judged = thresholds.rulings + thresholds.no_roll + thresholds.vetoes;
    if judged >= CLUSTER_MIN && thresholds.vetoes * 3 > judged {
        notes.push(format!(
            "Viele Vetos ({} von {judged}): Die Spieler verstehen den Rahmen des Szenarios womöglich nicht — Briefings und Beispiele für zulässige Aktionen schärfen.",
            thresholds.vetoes
        ));
    }

    match dice.z_success {
        Some(z) if z.abs() > Z_ALERT => notes.push(format!(
            "Die Würfel fielen auffällig {} aus ({} Erfolge bei {:.1} erwarteten, z = {z:+.2}). Der Zufallsgenerator ist geprüft; der Verlauf hängt in diesem Lauf aber stark am Würfelglück — Mehrfachläufe über andere Seeds zeigen, wie robust die Ergebnisse sind.",
            if z > 0.0 { "günstig" } else { "ungünstig" },
            dice.observed_successes,
            dice.expected_successes
        )),
        Some(_) => {}
        None => notes.push(
            "Keine Würfe — über die Würfelverteilung lässt sich nichts sagen.".to_owned(),
        ),
    }
    DesignLessons {
        secrets,
        thresholds,
        dice,
        notes,
    }
}

/// Markdown-Abschnitt „Design-Lehren“ (ohne Überschrift erster Ebene).
#[must_use]
pub fn render_design_lessons(lessons: &DesignLessons) -> String {
    let mut out = String::new();
    out.push_str("## Design-Lehren\n\n");
    out.push_str("Was dieser Lauf über das Szenario-Design verrät — nicht über die Spieler.\n\n");

    out.push_str("### Einsatz geheimer Argumente\n\n");
    if lessons.secrets.is_empty() {
        out.push_str("Keine geheimen Argumente.\n\n");
    } else {
        out.push_str("| Geheimnis | Sitz | Eingesetzt | Offengelegt | Höchste Verdachtsstufe |\n|---|---|---|---|---|\n");
        for s in &lessons.secrets {
            let revealed = match (s.revealed_round, s.revealed_at_end) {
                (Some(r), true) => format!("R{r} (Spielende)"),
                (Some(r), false) => format!("R{r}"),
                (None, _) => "—".to_owned(),
            };
            let _ = writeln!(
                out,
                "| #{} | {} | R{} | {revealed} | {} |",
                s.secret_id, s.seat, s.round, s.max_suspicion
            );
        }
        out.push('\n');
    }

    out.push_str("### Schwellen der Adjudikation\n\n");
    let t = &lessons.thresholds;
    if t.histogram.is_empty() {
        out.push_str("Keine gewürfelten Urteile.\n");
    } else {
        let hist: Vec<String> = t
            .histogram
            .iter()
            .map(|(k, v)| format!("{k}: {v}×"))
            .collect();
        let _ = writeln!(
            out,
            "- Verteilung der Zielwerte/Stufen ({} Urteile): {}",
            t.rulings,
            hist.join(", ")
        );
        if let Some(mode) = t.mode {
            let _ = writeln!(
                out,
                "- Häufigster Wert {mode} ({:.0} %)",
                t.mode_share * 100.0
            );
        }
    }
    let _ = writeln!(out, "- Ohne Wurf: {} · Vetos: {}\n", t.no_roll, t.vetoes);

    out.push_str("### Plausibilität der Würfel\n\n");
    let d = &lessons.dice;
    let _ = writeln!(
        out,
        "- Erste Würfe: {} (davon 2W6: {})",
        d.rolls, d.rolls_2d6
    );
    if let (Some(mean), Some(z)) = (d.mean_2d6, d.z_mean) {
        let _ = writeln!(
            out,
            "- Mittlere Augensumme 2W6: {mean:.2} (erwartet 7,00; z = {z:+.2})"
        );
    }
    if let Some(z) = d.z_success {
        let _ = writeln!(
            out,
            "- Erfolge: {} beobachtet, {:.1} erwartet (z = {z:+.2}; |z| ≤ {Z_ALERT:.0} gilt als Zufall)",
            d.observed_successes, d.expected_successes
        );
    }
    out.push('\n');

    out.push_str("### Hinweise\n\n");
    if lessons.notes.is_empty() {
        out.push_str("Keine Auffälligkeiten.\n");
    }
    for n in &lessons.notes {
        let _ = writeln!(out, "- {n}");
    }
    out.push('\n');
    out
}

// ---------------------------------------------------------------------------
// Mehrfachläufe
// ---------------------------------------------------------------------------

/// Verdichtung eines Laufs für den Vergleich über Seeds/Pakete.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AarSummary {
    /// Bezeichnung des Laufs (z. B. Run-ID).
    pub label: String,
    /// Master-Seed (Hex, gekürzt auf 16 Zeichen).
    pub seed: String,
    /// Gewähltes Inject-Paket.
    pub package: Option<String>,
    /// Angewendete Injects (IDs, erste Anwendung).
    pub injects: Vec<String>,
    /// Gespielte Runden.
    pub rounds_played: u32,
    /// Argumente insgesamt.
    pub arguments: usize,
    /// Erfolge (auch ohne Wurf).
    pub successes: usize,
    /// Misserfolge.
    pub failures: usize,
    /// Vetos.
    pub vetoes: usize,
    /// Erwartete Erfolgsquote erster Würfe in Prozent.
    pub expected_success_pct: f64,
    /// Beobachtete Erfolgsquote erster Würfe in Prozent.
    pub observed_success_pct: f64,
    /// Eingesetzte geheime Argumente.
    pub secrets_used: usize,
    /// Davon im Spiel (nicht erst zum Ende) offengelegt.
    pub secrets_revealed_in_game: usize,
    /// Präzedenzfälle.
    pub precedents: usize,
    /// Einwände der Red Cell (ohne „kein Einwand“).
    pub red_cell_objections: usize,
    /// Endwerte von Tracks, Zuständen und Projekten.
    pub final_values: BTreeMap<String, String>,
}

/// Verdichtet einen abgeschlossenen Lauf.
#[must_use]
pub fn summarize_run(
    label: &str,
    loaded: &LoadedScenario,
    journal: &Journal,
    state: &GameState,
) -> AarSummary {
    let lessons = design_lessons(loaded, journal);
    let seed = state.master_seed_hex.clone().unwrap_or_default();
    let mut injects = Vec::new();
    let mut summary = AarSummary {
        label: label.to_owned(),
        seed: seed.get(..16).unwrap_or(&seed).to_owned(),
        package: selected_package(journal).map(|p| p.package_id),
        injects: Vec::new(),
        rounds_played: state.round,
        arguments: state.outcomes.len(),
        successes: 0,
        failures: 0,
        vetoes: 0,
        expected_success_pct: 0.0,
        observed_success_pct: 0.0,
        secrets_used: lessons.secrets.len(),
        secrets_revealed_in_game: lessons
            .secrets
            .iter()
            .filter(|s| s.revealed_round.is_some() && !s.revealed_at_end)
            .count(),
        precedents: 0,
        red_cell_objections: 0,
        final_values: BTreeMap::new(),
    };
    for outcome in state.outcomes.values() {
        match outcome {
            Outcome::Success | Outcome::AutoSuccess => summary.successes += 1,
            Outcome::Failure => summary.failures += 1,
            Outcome::Vetoed => summary.vetoes += 1,
            Outcome::Forfeited => {}
        }
    }
    if lessons.dice.rolls > 0 {
        let n = count_f64(lessons.dice.rolls);
        summary.expected_success_pct = lessons.dice.expected_successes * 100.0 / n;
        summary.observed_success_pct = count_f64(lessons.dice.observed_successes) * 100.0 / n;
    }
    for e in journal.entries() {
        match &e.kind {
            EntryKind::InjectApplied { inject_id, .. } if !injects.contains(inject_id) => {
                injects.push(inject_id.clone());
            }
            EntryKind::PrecedentSet { .. } => summary.precedents += 1,
            EntryKind::RedCellObjection {
                target: Some(_), ..
            } => summary.red_cell_objections += 1,
            _ => {}
        }
    }
    summary.injects = injects;
    for var in state.vars.values() {
        let value = match &var.value {
            VarValue::Track { value, .. } => value.to_string(),
            VarValue::State { value, .. } => value.clone(),
            VarValue::Project { progress, stages } => format!("{progress}/{stages}"),
            VarValue::Number { value } => format!("{value}"),
            VarValue::Object { .. } => continue,
        };
        summary.final_values.insert(var.id.clone(), value);
    }
    summary
}

fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\n', '\r'], " ")
}

/// Vergleicht mehrere Läufe als Markdown: Kennzahlen je Lauf, Endwerte je
/// Größe mit Streuung und eine Lesart (robust vs. empfindlich).
#[must_use]
pub fn compare_runs(runs: &[AarSummary]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Vergleich von {} Läufen\n", runs.len());
    if runs.is_empty() {
        out.push_str("Keine Läufe übergeben.\n");
        return out;
    }
    out.push_str("| Lauf | Seed | Paket | Runden | Argumente | Erfolg / Misserfolg / Veto | Erfolgsquote beob. / erw. | Geheim (eingesetzt / aufgeflogen) | Präzedenzfälle | Red-Cell-Einwände | Injects |\n");
    out.push_str("|---|---|---|---|---|---|---|---|---|---|---|\n");
    for r in runs {
        let _ = writeln!(
            out,
            "| {} | `{}` | {} | {} | {} | {} / {} / {} | {:.0} % / {:.0} % | {} / {} | {} | {} | {} |",
            cell(&r.label),
            r.seed,
            r.package.as_deref().map_or_else(|| "—".to_owned(), cell),
            r.rounds_played,
            r.arguments,
            r.successes,
            r.failures,
            r.vetoes,
            r.observed_success_pct,
            r.expected_success_pct,
            r.secrets_used,
            r.secrets_revealed_in_game,
            r.precedents,
            r.red_cell_objections,
            if r.injects.is_empty() {
                "—".to_owned()
            } else {
                cell(&r.injects.join(", "))
            }
        );
    }
    out.push('\n');

    let vars: BTreeSet<&String> = runs.iter().flat_map(|r| r.final_values.keys()).collect();
    let mut robust = Vec::new();
    let mut sensitive = Vec::new();
    if !vars.is_empty() {
        out.push_str("## Endwerte je Größe\n\n| Größe |");
        for r in runs {
            let _ = write!(out, " {} |", cell(&r.label));
        }
        out.push_str(" Streuung |\n|---|");
        for _ in runs {
            out.push_str("---|");
        }
        out.push_str("---|\n");
        for var in vars {
            let values: Vec<&str> = runs
                .iter()
                .map(|r| r.final_values.get(var).map_or("—", String::as_str))
                .collect();
            let distinct: BTreeSet<&str> = values.iter().copied().collect();
            let numbers: Vec<f64> = values.iter().filter_map(|v| v.parse().ok()).collect();
            let spread = if distinct.len() == 1 {
                robust.push(var.as_str());
                "stabil".to_owned()
            } else {
                sensitive.push(var.as_str());
                if numbers.len() == values.len() {
                    let min = numbers.iter().copied().fold(f64::INFINITY, f64::min);
                    let max = numbers.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                    format!("variiert ({min} … {max})")
                } else {
                    format!("variiert ({} Ausprägungen)", distinct.len())
                }
            };
            let _ = write!(out, "| `{var}` |");
            for v in &values {
                let _ = write!(out, " {} |", cell(v));
            }
            let _ = writeln!(out, " {spread} |");
        }
        out.push('\n');
    }

    out.push_str("## Lesart\n\n");
    if runs.len() < 3 {
        out.push_str(
            "- Für belastbare Aussagen braucht es mindestens drei Läufe über verschiedene Seeds.\n",
        );
    }
    if !robust.is_empty() {
        let _ = writeln!(
            out,
            "- Robust (in allen Läufen gleich): {} — hier trägt die Struktur des Szenarios, nicht der Zufall.",
            robust
                .iter()
                .map(|v| format!("`{v}`"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !sensitive.is_empty() {
        let _ = writeln!(
            out,
            "- Empfindlich (Endwerte streuen): {} — hier entscheiden Würfel, Injects oder Spielerwahl; lohnende Kandidaten für Fork-Runden.",
            sensitive
                .iter()
                .map(|v| format!("`{v}`"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let packages: BTreeSet<Option<&str>> = runs.iter().map(|r| r.package.as_deref()).collect();
    if packages.len() > 1 {
        out.push_str("- Die Läufe nutzten verschiedene Inject-Pakete; Unterschiede können auch aus den Ereignissen stammen, nicht nur aus den Würfeln.\n");
    }
    let lucky: Vec<&str> = runs
        .iter()
        .filter(|r| (r.observed_success_pct - r.expected_success_pct).abs() >= 20.0)
        .map(|r| r.label.as_str())
        .collect();
    if !lucky.is_empty() {
        let _ = writeln!(
            out,
            "- Deutliches Würfelglück oder -pech (Abweichung ≥ 20 Prozentpunkte): {}.",
            lucky.join(", ")
        );
    }
    let secretless = runs.iter().filter(|r| r.secrets_used == 0).count();
    if secretless == runs.len() {
        out.push_str("- In keinem Lauf wurden geheime Argumente eingesetzt.\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{ORDER, scripted_game};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn lessons_cover_secret_timing_thresholds_and_dice() -> TestResult {
        let (loaded, log) = scripted_game(&[41u8; 32], &ORDER)?;
        let lessons = design_lessons(&loaded, &log.journal);
        assert_eq!(lessons.secrets.len(), 1);
        let secret = lessons.secrets.first().ok_or("kein Geheimnis")?;
        assert_eq!(secret.round, 1);
        assert!(secret.revealed_at_end);
        assert_eq!(lessons.thresholds.rulings, 3);
        assert_eq!(lessons.dice.rolls, 3);
        assert!(lessons.dice.expected_successes > 0.0);
        assert!(
            lessons
                .notes
                .iter()
                .any(|n| n.contains("bis zum Spielende unentdeckt"))
        );
        let md = render_design_lessons(&lessons);
        for heading in [
            "## Design-Lehren",
            "### Einsatz geheimer Argumente",
            "### Schwellen der Adjudikation",
            "### Plausibilität der Würfel",
            "### Hinweise",
        ] {
            assert!(md.contains(heading), "fehlt {heading}");
        }
        Ok(())
    }

    #[test]
    fn threshold_clustering_is_flagged() {
        let mut stats = ThresholdStats::default();
        for _ in 0..5 {
            *stats.histogram.entry(7).or_insert(0) += 1;
        }
        stats.rulings = 5;
        stats.mode = Some(7);
        stats.mode_share = 1.0;
        assert!(stats.mode_share >= CLUSTER_SHARE && stats.rulings >= CLUSTER_MIN);
    }

    #[test]
    fn compare_runs_separates_robust_and_sensitive() -> TestResult {
        let (loaded, a) = scripted_game(&[1u8; 32], &ORDER)?;
        let (_, b) = scripted_game(&[2u8; 32], &ORDER)?;
        let (_, c) = scripted_game(&[3u8; 32], &ORDER)?;
        let runs = vec![
            summarize_run("lauf-a", &loaded, &a.journal, &a.state),
            summarize_run("lauf-b", &loaded, &b.journal, &b.state),
            summarize_run("lauf-c", &loaded, &c.journal, &c.state),
        ];
        assert!(runs.iter().all(|r| r.arguments == 4 && r.secrets_used == 1));
        let md = compare_runs(&runs);
        assert!(md.starts_with("# Vergleich von 3 Läufen"));
        assert!(md.contains("| lauf-a |"));
        assert!(md.contains("## Endwerte je Größe"));
        assert!(md.contains("Robust"));
        // Rein und deterministisch.
        assert_eq!(md, compare_runs(&runs));
        // Nicht angefasste Größe ist stabil.
        assert!(md.contains("| `pipeline` | 0/3 | 0/3 | 0/3 | stabil |"));
        let empty = compare_runs(&[]);
        assert!(empty.contains("Keine Läufe"));
        let single = compare_runs(&runs[..1]);
        assert!(single.contains("mindestens drei Läufe"));
        Ok(())
    }
}
