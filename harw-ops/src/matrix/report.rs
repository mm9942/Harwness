//! Paper-tauglicher Abschlussbericht eines Matrix-Game-Laufs (`report.md`,
//! Runde 7, Teil M4).
//!
//! # Verantwortungsbereich
//! [`build_report`] erzeugt aus Szenario, Journal, Endzustand, Umpire-Synthese
//! und Spieler-Debriefs ein Markdown-Dokument, das die UIA ohne Nacharbeit an
//! `uia-latex-writer` (Vorlage `business-paper`) weitergeben kann:
//!
//! 1. Kopf (Szenario, Lauf, Seed, Status),
//! 2. Zweck und Schlüsselfragen,
//! 3. Akteure (öffentliche Ziele, Machtmittel),
//! 4. Rundenprotokoll (Argumente, Gegenargumente, Urteile, Würfe,
//!    Entscheidungen, Weltänderungen, Erzählung),
//! 5. Marktanteile (Business-Modus: Start → Ende je Team und Segment),
//! 6. Ergebnis (Spielende, Einschätzung, Bilanz je Akteur),
//! 7. Nachbetrachtung nach vier Fragen (geplant / geschehen / warum / Lehren),
//! 8. Empfehlungen und offene Fragen,
//! 9. Methodik (Würfel ausschließlich aus der Engine, Seed, Replay).
//!
//! # Sichtbarkeit
//! Der Bericht ist die Sicht der Spielleitung (Beobachter): er enthält auch
//! offengelegte Geheimnisse, aber keine Beobachter-Vermerke des Runners
//! (`FacilitatorNote`) außer Geschäftsregeln und Konfliktwürfen.
//!
//! # Nebenläufigkeit
//! Reine Funktion über einem ruhenden [`MatrixRun`].

use std::collections::BTreeMap;
use std::fmt::Write as _;

use harw_matrix_game::dice::Outcome;
use harw_matrix_game::scenario::Scenario;
use harw_matrix_game::state::{EntryKind, GameEntry, PlayerId, VarValue};

use super::runner::{MatrixRun, entry_text};

/// Baut den Markdown-Bericht eines (in der Regel beendeten) Laufs.
///
/// # Argumente
/// - `run`: der Lauf.
///
/// # Rückgabe
/// Das vollständige Markdown-Dokument.
#[must_use]
pub fn build_report(run: &MatrixRun) -> String {
    let scenario = &run.loaded().scenario;
    let log = run.log();
    let mut out = String::new();

    let _ = writeln!(out, "# Matrix-Game-Bericht: {}\n", scenario.title());
    out.push_str("| Feld | Wert |\n|---|---|\n");
    let _ = writeln!(out, "| Szenario | `{}` |", scenario.id());
    let _ = writeln!(out, "| Modus | {:?} |", scenario.mode());
    let _ = writeln!(out, "| Lauf | `{}` |", run.run_id());
    let _ = writeln!(out, "| Seed (Präfix) | `{}` |", run.seed_prefix());
    let _ = writeln!(
        out,
        "| Runden | {} von {} |",
        log.state.round,
        scenario.rounds()
    );
    let _ = writeln!(out, "| Status | {} |\n", run.status().as_str());

    purpose_section(scenario, &mut out);
    actors_section(scenario, &mut out);
    protocol_section(scenario, log.journal.entries(), &mut out);
    market_section(run, &mut out);
    result_section(run, &mut out);
    four_questions_section(run, &mut out);
    recommendations_section(run, &mut out);

    out.push_str("## Methodik\n\n");
    out.push_str(
        "- Alle Würfe stammen ausschließlich aus der deterministischen Engine \
         (abgeleitete Seeds je Runde und Argument); kein Modell würfelt. Jeder \
         Wurf steht mit Augen, Ziel und abgeleitetem Seed im Journal.\n",
    );
    out.push_str(
        "- Jeder Sitz ist ein eigener Kind-Agent und sieht nur seine Projektion \
         des Journals (Regeln, eigenes Briefing, öffentliche Lage, an ihn \
         adressierte Nachrichten).\n",
    );
    out.push_str(
        "- Der Lauf ist per Replay aus `journal.jsonl` ohne Modellaufrufe \
         reproduzierbar (Würfel und Zustands-Hashes werden nachgerechnet).\n",
    );
    if matches!(scenario, Scenario::Business(_)) {
        out.push_str(
            "- Marktanteile berechnet das Logit-Marktmodell der Engine \
             (Rundenerfolg, Preisposition, Vertriebsinvest mit Sättigung, \
             Produktfit); Geschäftsregeln sperren Aktionsarten deterministisch.\n",
        );
    }
    out
}

fn purpose_section(scenario: &Scenario, out: &mut String) {
    out.push_str("## Zweck und Schlüsselfragen\n\n");
    let _ = writeln!(out, "{}\n", scenario.purpose().trim());
    let situation = scenario.public_situation();
    if !situation.trim().is_empty() {
        out.push_str("**Ausgangslage bzw. Schlüsselfragen:**\n\n");
        for line in situation.lines().filter(|l| !l.trim().is_empty()) {
            let _ = writeln!(out, "- {}", line.trim());
        }
        out.push('\n');
    }
}

fn actors_section(scenario: &Scenario, out: &mut String) {
    out.push_str("## Akteure\n\n");
    for faction in scenario.factions() {
        let level = faction
            .level
            .as_deref()
            .map(|l| format!(", Ebene {l}"))
            .unwrap_or_default();
        let _ = writeln!(out, "### {} (`{}`{level})\n", faction.name, faction.id);
        if !faction.briefing.trim().is_empty() {
            let _ = writeln!(out, "{}\n", faction.briefing.trim());
        }
        if !faction.public_goals.is_empty() {
            out.push_str("Öffentliche Ziele:\n");
            for goal in &faction.public_goals {
                let _ = writeln!(out, "- {goal}");
            }
            out.push('\n');
        }
        if !faction.assets.is_empty() {
            let _ = writeln!(out, "Machtmittel: {}\n", faction.assets.join(", "));
        }
    }
}

/// Gehört ein Eintrag ins Rundenprotokoll?
fn protocol_entry(entry: &GameEntry) -> Option<&'static str> {
    match &entry.kind {
        EntryKind::InjectApplied { .. } => Some("Ereignis"),
        EntryKind::Briefed { .. } => Some("Absicht"),
        EntryKind::ChannelOpened { .. } | EntryKind::NegotiationPosted { .. } => {
            Some("Verhandlung")
        }
        EntryKind::ArgumentRevealed { .. } => Some("Argument"),
        EntryKind::SecretRevealed { .. } => Some("Offengelegt"),
        EntryKind::CountersSubmitted { .. } => Some("Gegenargumente"),
        EntryKind::RedCellObjection { .. } => Some("Red Cell"),
        EntryKind::Adjudicated { .. } => Some("Urteil"),
        EntryKind::DiceRolled { .. } => Some("Würfel"),
        EntryKind::ArgumentResolved { .. } => Some("Entscheidung"),
        EntryKind::Forfeit { .. } => Some("Verzicht"),
        EntryKind::WorldDelta { .. } => Some("Weltänderung"),
        EntryKind::FactAdded { .. } => Some("Fakt"),
        EntryKind::Narrated { .. } => Some("Erzählung"),
        EntryKind::StandingSet { .. } => Some("Einschätzung"),
        EntryKind::FacilitatorNote { command, .. }
            if command == harw_matrix_game::business::RULE_NOTE_COMMAND
                || command == "conflict" =>
        {
            Some("Regel")
        }
        _ => None,
    }
}

fn protocol_section<'a>(
    scenario: &Scenario,
    entries: impl Iterator<Item = &'a GameEntry>,
    out: &mut String,
) {
    out.push_str("## Rundenprotokoll\n\n");
    let mut rounds: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    let mut seen_deltas: Vec<(u32, String)> = Vec::new();
    for entry in entries {
        let Some(label) = protocol_entry(entry) else {
            continue;
        };
        // Weltänderungen erscheinen je Audience einmal im Journal; im
        // Bericht genügt eine Zeile.
        if let EntryKind::WorldDelta { var, to, .. } = &entry.kind {
            let key = format!("{var}={}", to.display());
            if seen_deltas.contains(&(entry.round, key.clone())) {
                continue;
            }
            seen_deltas.push((entry.round, key));
        }
        rounds.entry(entry.round).or_default().push(format!(
            "- **{label}:** {}",
            entry_text(&entry.kind, scenario).replace('\n', " ")
        ));
    }
    if rounds.is_empty() {
        out.push_str("Keine Spielzüge protokolliert.\n\n");
        return;
    }
    for (round, lines) in rounds {
        if round == 0 {
            out.push_str("### Vorbereitung\n\n");
        } else {
            let _ = writeln!(out, "### Runde {round}\n");
        }
        for line in lines {
            let _ = writeln!(out, "{line}");
        }
        out.push('\n');
    }
}

fn number(value: &VarValue) -> Option<f64> {
    match value {
        VarValue::Number { value } => Some(*value),
        _ => None,
    }
}

fn market_section(run: &MatrixRun, out: &mut String) {
    if !matches!(run.loaded().scenario, Scenario::Business(_)) {
        return;
    }
    let log = run.log();
    let mut start: BTreeMap<String, f64> = BTreeMap::new();
    for entry in log.journal.entries() {
        if let EntryKind::VarDeclared { var } = &entry.kind {
            if let (true, Some(v)) = (var.id.contains(".share."), number(&var.value)) {
                start.entry(var.id.clone()).or_insert(v);
            }
        }
    }
    if start.is_empty() {
        return;
    }
    out.push_str(
        "## Marktanteile\n\n| Team | Segment | Start | Ende | Δ (pp) |\n|---|---|---|---|---|\n",
    );
    for (id, from) in &start {
        let Some((team, segment)) = id.split_once(".share.") else {
            continue;
        };
        let to = log
            .state
            .vars
            .get(id)
            .and_then(|v| number(&v.value))
            .unwrap_or(*from);
        let name = run.loaded().scenario.display_name(&PlayerId::new(team));
        let _ = writeln!(
            out,
            "| {name} | {segment} | {:.1} % | {:.1} % | {:+.1} |",
            from * 100.0,
            to * 100.0,
            (to - from) * 100.0
        );
    }
    out.push('\n');
}

fn result_section(run: &MatrixRun, out: &mut String) {
    let log = run.log();
    let scenario = &run.loaded().scenario;
    out.push_str("## Ergebnis\n\n");
    let reason = log.journal.entries().find_map(|e| match &e.kind {
        EntryKind::GameEnded { reason } => Some(reason.clone()),
        _ => None,
    });
    match reason {
        Some(reason) => {
            let _ = writeln!(out, "Spielende: {reason}\n");
        }
        None => out.push_str("Das Spiel ist noch nicht beendet (Zwischenstand).\n\n"),
    }
    if !log.state.standing.is_empty() {
        let names: Vec<String> = log
            .state
            .standing
            .iter()
            .map(|p| scenario.display_name(p))
            .collect();
        let _ = writeln!(
            out,
            "Letzte Einschätzung der Spielleitung: {}\n",
            names.join(" > ")
        );
    }
    out.push_str("| Akteur | Erfolge | Misserfolge | Verworfen/gepasst |\n|---|---|---|---|\n");
    let mut tally: BTreeMap<PlayerId, (u32, u32, u32)> = BTreeMap::new();
    for entry in log.journal.entries() {
        if let EntryKind::ArgumentResolved { seat, outcome, .. } = &entry.kind {
            let slot = tally.entry(seat.clone()).or_default();
            match outcome {
                Outcome::Success | Outcome::AutoSuccess => slot.0 += 1,
                Outcome::Failure => slot.1 += 1,
                Outcome::Vetoed | Outcome::Forfeited => slot.2 += 1,
            }
        }
    }
    for player in &log.state.players {
        let (won, lost, other) = tally.get(player).copied().unwrap_or_default();
        let _ = writeln!(
            out,
            "| {} | {won} | {lost} | {other} |",
            scenario.display_name(player)
        );
    }
    out.push('\n');
    let public_vars: Vec<String> = log
        .state
        .vars
        .values()
        .filter(|v| !v.is_hidden() && !v.id.contains(".share."))
        .map(|v| format!("- {}: {}", v.label, v.value.display()))
        .collect();
    if !public_vars.is_empty() {
        out.push_str("Öffentlicher Endzustand:\n\n");
        for line in public_vars {
            let _ = writeln!(out, "{line}");
        }
        out.push('\n');
    }
}

fn four_questions_section(run: &MatrixRun, out: &mut String) {
    let log = run.log();
    let scenario = &run.loaded().scenario;
    out.push_str("## Nachbetrachtung (geplant · geschehen · warum · Lehren)\n\n");
    for player in &log.state.players {
        let name = scenario.display_name(player);
        let debrief = run.debriefs().get(player);
        let intents: Vec<String> = log
            .journal
            .entries()
            .filter_map(|e| match &e.kind {
                EntryKind::Briefed {
                    seat,
                    intent: Some(intent),
                    ..
                } if super::runner::seat_key(seat) == player.as_str() => {
                    Some(format!("Runde {}: {intent}", e.round))
                }
                _ => None,
            })
            .collect();
        let _ = writeln!(out, "### {name}\n");
        let planned = debrief
            .map(|d| d.wanted.clone())
            .or_else(|| (!intents.is_empty()).then(|| intents.join("; ")))
            .unwrap_or_else(|| "keine Angabe".to_owned());
        let _ = writeln!(out, "- **Geplant:** {planned}");
        let happened = debrief
            .map(|d| d.happened.clone())
            .unwrap_or_else(|| "siehe Rundenprotokoll".to_owned());
        let _ = writeln!(out, "- **Geschehen:** {happened}");
        let why = run
            .synthesis()
            .and_then(|s| {
                s.goal_ratings
                    .iter()
                    .filter(|g| g.faction == player.as_str())
                    .map(|g| format!("{} ({}/3): {}", g.goal, g.score, g.rationale))
                    .reduce(|a, b| format!("{a}; {b}"))
            })
            .unwrap_or_else(|| "siehe Urteile und Würfe im Rundenprotokoll".to_owned());
        let _ = writeln!(out, "- **Warum:** {why}");
        let lessons = debrief
            .map(|d| format!("{} — Überraschung: {}", d.differently, d.surprised))
            .unwrap_or_else(|| "kein Debrief".to_owned());
        let _ = writeln!(out, "- **Lehren:** {lessons}\n");
    }
}

fn recommendations_section(run: &MatrixRun, out: &mut String) {
    out.push_str("## Empfehlungen\n\n");
    match run.synthesis() {
        Some(synthesis) => {
            if let Some(summary) = &synthesis.summary {
                let _ = writeln!(out, "{}\n", summary.trim());
            }
            for moment in &synthesis.key_moments {
                let _ = writeln!(out, "- Wendepunkt prüfen: {moment}");
            }
            if !synthesis.fork_rounds.is_empty() {
                let rounds: Vec<String> =
                    synthesis.fork_rounds.iter().map(u32::to_string).collect();
                let _ = writeln!(
                    out,
                    "- Alternativen durchspielen: Fork ab Runde {}",
                    rounds.join(", ")
                );
            }
            out.push('\n');
        }
        None => out.push_str(
            "Keine Umpire-Synthese verfügbar; Empfehlungen aus dem Rundenprotokoll ableiten.\n\n",
        ),
    }
    for (player, debrief) in run.debriefs() {
        let _ = writeln!(
            out,
            "- {}: {}",
            run.loaded().scenario.display_name(player),
            debrief.differently
        );
    }
    out.push_str("\n## Offene Fragen\n\n");
    let mut any = false;
    if let Some(plausibility) = run.synthesis().and_then(|s| s.plausibility.as_deref()) {
        let _ = writeln!(out, "- Plausibilität: {plausibility}");
        any = true;
    }
    let forfeits = run
        .log()
        .journal
        .entries()
        .filter(|e| matches!(e.kind, EntryKind::Forfeit { .. }))
        .count();
    if forfeits > 0 {
        let _ = writeln!(
            out,
            "- {forfeits} Zug/Züge wurden gepasst (Fehler, Zeitlimit oder ungültige Antwort) — Ergebnis entsprechend vorsichtig lesen."
        );
        any = true;
    }
    let leaks = run
        .log()
        .journal
        .entries()
        .filter(|e| matches!(e.kind, EntryKind::LeakSuspect { .. }))
        .count();
    if leaks > 0 {
        let _ = writeln!(
            out,
            "- {leaks} Leak-Verdacht/-Verdachte des Guards — betroffene Texte wurden zurückgehalten."
        );
        any = true;
    }
    if !any {
        out.push_str("- Keine offenen Punkte aus dem Lauf; Schlüsselfragen oben gegen das Ergebnis prüfen.\n");
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matrix::runner::{NullDriver, master_seed_for};
    use harw_matrix_game::scenario::load_scenario;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const CLOUD: &str = include_str!("../../../harw-matrix-game/scenarios/cloud-sme-2027.toml");

    #[tokio::test]
    async fn report_covers_the_paper_sections() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let loaded = load_scenario(CLOUD)?;
        let seed = master_seed_for(&loaded, Some(3));
        let mut run = MatrixRun::start(
            loaded,
            CLOUD.to_owned(),
            None,
            seed,
            "report-test".to_owned(),
            Some(tmp.path().join("lauf")),
            false,
            None,
        )?;
        run.request_end()?;
        let mut driver = NullDriver;
        run.step_with(&mut driver).await?;
        let path = run.report_path().ok_or("report.md fehlt")?.to_path_buf();
        let text = std::fs::read_to_string(&path)?;
        for heading in [
            "# Matrix-Game-Bericht",
            "## Zweck und Schlüsselfragen",
            "## Akteure",
            "## Rundenprotokoll",
            "## Marktanteile",
            "## Ergebnis",
            "## Nachbetrachtung",
            "## Empfehlungen",
            "## Offene Fragen",
            "## Methodik",
        ] {
            assert!(text.contains(heading), "{heading} fehlt");
        }
        assert!(text.contains("Spielende"));
        assert_eq!(build_report(&run), text);
        Ok(())
    }
}
