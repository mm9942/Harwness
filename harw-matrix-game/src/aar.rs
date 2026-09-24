//! After-Action-Review als Markdown (matrix-game.md §8.2).
//!
//! Das AAR entsteht rein deterministisch aus Szenario, Journal und
//! Endzustand; die Umpire-Synthese (Schlüsselmomente, Zielbewertungen,
//! Plausibilitätscheck) und Spieler-Debriefs werden als bereits validierte
//! Contracts hineingereicht. Offenlegung heißt hier: alle Audiences werden
//! sichtbar — geheime Argumente samt Commitment-Prüfung, alle privaten
//! Kanäle, alle privaten Notizen.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::commitments::{Commitment, Salt, verify};
use crate::dice::{DiceRoll, Grade, Outcome};
use crate::error::MatrixResult;
pub use crate::lessons::{AarSummary, compare_runs, summarize_run};
use crate::lessons::{design_lessons, render_design_lessons};
use crate::phases::{ArgumentBody, PlayerDebrief, UmpireRuling, UmpireSynthesis};
use crate::precedents::later_matches;
use crate::scenario::{LoadedScenario, RED_CELL_KEY, Scenario};
use crate::state::{EntryKind, GameState, Journal, PlayerId, VarValue};

/// Höchstwert einer Zielbewertung.
pub const MAX_GOAL_SCORE: u8 = 3;

/// Bewertung eines Ziels (0–3 mit Begründung).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalRating {
    /// Fraktion.
    pub faction: String,
    /// Ziel (wörtlich wie im Szenario).
    pub goal: String,
    /// Geheimes Ziel.
    #[serde(default)]
    pub secret: bool,
    /// Bewertung 0–3.
    pub score: u8,
    /// Begründung.
    pub rationale: String,
}

/// Prüft Zielbewertungen gegen das Szenario.
#[must_use]
pub fn validate_goal_ratings(ratings: &[GoalRating], scenario: &Scenario) -> Vec<String> {
    let factions = scenario.factions();
    let mut errors = Vec::new();
    for (i, r) in ratings.iter().enumerate() {
        let label = format!("goal_ratings[{i}]");
        match factions.iter().find(|f| f.id.as_str() == r.faction) {
            None => errors.push(format!("{label}: unbekannte Fraktion `{}`", r.faction)),
            Some(f) => {
                let goals = if r.secret {
                    &f.secret_goals
                } else {
                    &f.public_goals
                };
                if !goals.contains(&r.goal) {
                    errors.push(format!(
                        "{label}: Ziel `{}` gehört nicht zu `{}`",
                        r.goal, r.faction
                    ));
                }
            }
        }
        if r.score > MAX_GOAL_SCORE {
            errors.push(format!("{label}: score {} > {MAX_GOAL_SCORE}", r.score));
        }
        if r.rationale.trim().is_empty() {
            errors.push(format!("{label}: Begründung fehlt"));
        }
    }
    errors
}

/// Ergebnis der Commitment-Prüfung eines geheimen Arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisclosureCheck {
    /// Geheimnis-ID.
    pub secret_id: String,
    /// Argument-ID.
    pub argument_id: String,
    /// Eigentümer.
    pub seat: PlayerId,
    /// Öffentlich angekündigtes Commitment.
    pub announced: Commitment,
    /// Im Spiel offengelegt (sonst erst im AAR).
    pub revealed_in_game: bool,
    /// `sha256(canonical(content) ‖ salt) == announced`.
    pub verified: bool,
    /// Inhalt.
    pub content: Option<ArgumentBody>,
}

/// Prüft alle geheimen Argumente gegen ihre angekündigten Commitments.
///
/// # Errors
/// [`crate::MatrixError::Json`] bei nicht serialisierbarem Inhalt.
pub fn disclosure_checks(
    journal: &Journal,
    state: &GameState,
) -> MatrixResult<Vec<DisclosureCheck>> {
    let master = state.master_seed().ok();
    let mut checks = Vec::new();
    for entry in journal.entries() {
        let EntryKind::SecretArgumentAnnounced {
            argument_id,
            seat,
            secret_id,
            commitment,
            ..
        } = &entry.kind
        else {
            continue;
        };
        let revealed = journal.entries().find_map(|e| match &e.kind {
            EntryKind::SecretRevealed {
                secret_id: sid,
                content,
                salt_hex,
                commitment: revealed_commitment,
                ..
            } if sid == secret_id => Some((
                content.clone(),
                salt_hex.clone(),
                revealed_commitment.clone(),
            )),
            _ => None,
        });
        let check = if let Some((content, salt_hex, revealed_commitment)) = revealed {
            let verified = match Salt::from_hex(&salt_hex) {
                Some(salt) => {
                    verify(&content, &salt, commitment)? && &revealed_commitment == commitment
                }
                None => false,
            };
            DisclosureCheck {
                secret_id: secret_id.clone(),
                argument_id: argument_id.clone(),
                seat: seat.clone(),
                announced: commitment.clone(),
                revealed_in_game: true,
                verified,
                content: Some(content),
            }
        } else {
            let body = journal.entries().find_map(|e| match &e.kind {
                EntryKind::ArgumentRevealed {
                    argument_id: aid,
                    argument,
                    ..
                } if aid == argument_id => Some(argument.clone()),
                _ => None,
            });
            let verified = match (&body, master) {
                (Some(b), Some(m)) => verify(b, &Salt::derive(&m, argument_id), commitment)?,
                _ => false,
            };
            DisclosureCheck {
                secret_id: secret_id.clone(),
                argument_id: argument_id.clone(),
                seat: seat.clone(),
                announced: commitment.clone(),
                revealed_in_game: false,
                verified,
                content: body,
            }
        };
        checks.push(check);
    }
    Ok(checks)
}

/// Eingaben des AAR-Builders.
#[derive(Debug, Clone, Copy)]
pub struct AarInput<'a> {
    /// Szenario.
    pub loaded: &'a LoadedScenario,
    /// Vollständiges Journal.
    pub journal: &'a Journal,
    /// Endzustand.
    pub state: &'a GameState,
    /// Umpire-Synthese (validiert).
    pub synthesis: Option<&'a UmpireSynthesis>,
    /// Spieler-Debriefs.
    pub debriefs: &'a BTreeMap<PlayerId, PlayerDebrief>,
    /// Modellangabe für den Kopf.
    pub models: Option<&'a str>,
}

#[derive(Debug, Clone)]
struct ArgInfo {
    id: String,
    round: u32,
    seat: PlayerId,
    body: ArgumentBody,
    secret_id: Option<String>,
    counters: Vec<(PlayerId, Vec<String>)>,
    ruling: Option<(UmpireRuling, i32, Option<u8>, u8)>,
    rolls: Vec<DiceRoll>,
    outcome: Option<(Outcome, Option<Grade>)>,
    narrations: Vec<String>,
    rejected: Vec<String>,
    red_cell_assumption: Option<String>,
}

fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\n', '\r'], " ")
}

fn push_line(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

fn outcome_label(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Success => "Erfolg",
        Outcome::Failure => "Misserfolg",
        Outcome::AutoSuccess => "Erfolg ohne Wurf",
        Outcome::Vetoed => "Veto",
        Outcome::Forfeited => "kein Argument",
    }
}

fn grade_label(grade: Grade) -> &'static str {
    match grade {
        Grade::StrongSuccess => "deutlicher Erfolg",
        Grade::Success => "Erfolg",
        Grade::Failure => "Misserfolg",
        Grade::StrongFailure => "deutlicher Misserfolg",
    }
}

fn track_value(value: &VarValue) -> Option<String> {
    match value {
        VarValue::Track { value, .. } => Some(value.to_string()),
        VarValue::Project { progress, stages } => Some(format!("{progress}/{stages}")),
        VarValue::State { value, .. } => Some(value.clone()),
        VarValue::Number { value } => Some(format!("{value}")),
        VarValue::Object { .. } => None,
    }
}

fn collect_arguments(journal: &Journal) -> Vec<ArgInfo> {
    let mut args: Vec<ArgInfo> = Vec::new();
    let index = |args: &Vec<ArgInfo>, id: &str| args.iter().position(|a| a.id == id);
    for entry in journal.entries() {
        match &entry.kind {
            EntryKind::ArgumentRevealed {
                argument_id,
                seat,
                argument,
            } => args.push(ArgInfo {
                id: argument_id.clone(),
                round: entry.round,
                seat: seat.clone(),
                body: argument.clone(),
                secret_id: entry.secret_id.clone(),
                counters: Vec::new(),
                ruling: None,
                rolls: Vec::new(),
                outcome: None,
                narrations: Vec::new(),
                rejected: Vec::new(),
                red_cell_assumption: None,
            }),
            EntryKind::RedCellObjection {
                target: Some(target),
                assumption,
                cons,
            } => {
                if let Some(a) = index(&args, target).and_then(|i| args.get_mut(i)) {
                    a.counters.push((PlayerId::new(RED_CELL_KEY), cons.clone()));
                    a.red_cell_assumption.clone_from(assumption);
                }
            }
            EntryKind::CountersSubmitted { seat, counters } => {
                for c in counters.iter().filter(|c| !c.cons.is_empty()) {
                    if let Some(i) = index(&args, &c.argument_id) {
                        if let Some(a) = args.get_mut(i) {
                            a.counters.push((seat.clone(), c.cons.clone()));
                        }
                    }
                }
            }
            EntryKind::Adjudicated {
                argument_id,
                ruling,
                net,
                target,
                probability_pct,
            } => {
                if let Some(a) = index(&args, argument_id).and_then(|i| args.get_mut(i)) {
                    a.ruling = Some((ruling.clone(), *net, *target, *probability_pct));
                }
            }
            EntryKind::DiceRolled { roll } => {
                if let Some(a) = index(&args, &roll.argument_id).and_then(|i| args.get_mut(i)) {
                    if !a.rolls.contains(roll) {
                        a.rolls.push(roll.clone());
                    }
                }
            }
            EntryKind::ArgumentResolved {
                argument_id,
                outcome,
                grade,
                ..
            } => {
                if let Some(a) = index(&args, argument_id).and_then(|i| args.get_mut(i)) {
                    a.outcome = Some((*outcome, *grade));
                }
            }
            EntryKind::Narrated {
                argument_id: Some(argument_id),
                text,
            } => {
                if let Some(a) = index(&args, argument_id).and_then(|i| args.get_mut(i)) {
                    a.narrations.push(text.clone());
                }
            }
            EntryKind::EffectRejected {
                argument_id,
                op_index,
                reason,
            } => {
                if let Some(a) = index(&args, argument_id).and_then(|i| args.get_mut(i)) {
                    a.rejected.push(format!("Op {op_index}: {reason}"));
                }
            }
            _ => {}
        }
    }
    args
}

fn write_argument(out: &mut String, scenario: &Scenario, a: &ArgInfo) {
    let kind = a
        .secret_id
        .as_ref()
        .map_or_else(|| "öffentlich".to_owned(), |s| format!("geheim #{s}"));
    push_line(
        out,
        &format!(
            "#### {} — {} ({kind})",
            a.id,
            scenario.display_name(&a.seat)
        ),
    );
    push_line(out, "");
    push_line(out, &format!("**Aktion:** {}", a.body.action));
    push_line(out, "");
    let ruling = a.ruling.as_ref().map(|r| &r.0);
    for (i, pro) in a.body.pros.iter().enumerate() {
        let w = ruling
            .and_then(|r| r.pro_weights.get(i))
            .map_or_else(|| "–".to_owned(), ToString::to_string);
        push_line(out, &format!("- Pro {}: {pro} (Gewicht {w})", i + 1));
    }
    if let Some(assumption) = &a.red_cell_assumption {
        push_line(
            out,
            &format!("- Red Cell — angegriffene Kernannahme: {assumption}"),
        );
    }
    for (seat, cons) in &a.counters {
        for (j, con) in cons.iter().enumerate() {
            let w = ruling
                .and_then(|r| r.con_weights.get(seat.as_str()))
                .and_then(|ws| ws.get(j))
                .map_or_else(|| "–".to_owned(), ToString::to_string);
            push_line(out, &format!("- Contra ({seat}): {con} (Gewicht {w})"));
        }
    }
    if let Some((r, net, target, pct)) = &a.ruling {
        for con in &r.umpire_cons {
            push_line(
                out,
                &format!("- Contra (Umpire): {} (Gewicht {})", con.text, con.weight),
            );
        }
        if r.context_modifier != 0 {
            push_line(
                out,
                &format!(
                    "- Kontext-Modifikator {:+}: {}",
                    r.context_modifier,
                    r.context_reason.as_deref().unwrap_or("—")
                ),
            );
        }
        let target_text =
            target.map_or_else(|| format!("{pct} %"), |t| format!("Ziel {t}+ ({pct} %)"));
        push_line(out, &format!("- Netto {net:+} → {target_text}"));
        if let Some(rationale) = &r.public_rationale {
            push_line(out, &format!("- Umpire (öffentlich): {rationale}"));
        }
        if let Some(notes) = &r.private_notes {
            push_line(out, &format!("- Umpire (intern): {notes}"));
        }
        if let Some(other) = &r.inconsistent_with {
            push_line(out, &format!("- Inkonsistent mit `{other}`"));
        }
    }
    for roll in &a.rolls {
        let dice: Vec<String> = roll.dice.iter().map(ToString::to_string).collect();
        push_line(
            out,
            &format!(
                "- Wurf (Versuch {}): {} = {} gegen {} → {} [seed {}]",
                roll.attempt,
                dice.join("+"),
                roll.total,
                roll.target,
                if roll.success { "Erfolg" } else { "Misserfolg" },
                roll.sub_seed_hex.get(..12).unwrap_or(&roll.sub_seed_hex)
            ),
        );
    }
    if let Some((outcome, grade)) = a.outcome {
        let grade = grade.map_or_else(String::new, |g| format!(" ({})", grade_label(g)));
        push_line(
            out,
            &format!("- **Ergebnis:** {}{grade}", outcome_label(outcome)),
        );
    }
    for n in &a.narrations {
        push_line(out, &format!("- Erzählung: {n}"));
    }
    for r in &a.rejected {
        push_line(out, &format!("- Verworfener Effekt: {r}"));
    }
    push_line(out, "");
}

/// Baut das AAR-Markdown.
///
/// # Errors
/// [`crate::MatrixError::Json`] aus der Commitment-Prüfung.
#[allow(clippy::too_many_lines)]
pub fn build_aar(input: &AarInput<'_>) -> MatrixResult<String> {
    let scenario = &input.loaded.scenario;
    let journal = input.journal;
    let state = input.state;
    let mut out = String::new();

    // 1. Kopf
    push_line(
        &mut out,
        &format!("# After-Action-Review: {}", scenario.title()),
    );
    push_line(&mut out, "");
    push_line(&mut out, "| Feld | Wert |");
    push_line(&mut out, "|---|---|");
    push_line(&mut out, &format!("| Szenario | `{}` |", scenario.id()));
    push_line(&mut out, &format!("| Modus | {:?} |", scenario.mode()));
    push_line(
        &mut out,
        &format!("| Zweck | {} |", cell(scenario.purpose())),
    );
    let seed = state.master_seed_hex.as_deref().unwrap_or("—");
    push_line(
        &mut out,
        &format!("| Seed | `{}…` |", seed.get(..16).unwrap_or(seed)),
    );
    push_line(
        &mut out,
        &format!(
            "| Szenario-Hash | `{}…` |",
            input.loaded.source_hash.get(..16).unwrap_or("")
        ),
    );
    push_line(
        &mut out,
        &format!("| Runden | {} von {} |", state.round, scenario.rounds()),
    );
    push_line(
        &mut out,
        &format!(
            "| Modelle | {} |",
            cell(input.models.unwrap_or("Harness-Default"))
        ),
    );
    push_line(&mut out, "");
    push_line(
        &mut out,
        "> Hinweis: Ein Matrix Game sagt die Zukunft nicht voraus. Sein Wert liegt in der \
         Teilnahme und in den Einsichten, die es erzeugt — dieses Dokument hält Einsichten fest, \
         keine Prognose.",
    );
    push_line(&mut out, "");
    if let Scenario::Business(b) = scenario {
        push_line(&mut out, "## Schlüsselfragen");
        push_line(&mut out, "");
        for q in &b.game.key_questions {
            push_line(&mut out, &format!("- **{}**: {}", q.id, q.text));
        }
        push_line(&mut out, "");
    }

    // 2. Endzustand + Verlauf
    push_line(&mut out, "## Endzustand");
    push_line(&mut out, "");
    push_line(&mut out, "| Variable | Wert | Sichtbarkeit |");
    push_line(&mut out, "|---|---|---|");
    for var in state.vars.values() {
        push_line(
            &mut out,
            &format!(
                "| {} (`{}`) | {} | {} |",
                cell(&var.label),
                var.id,
                cell(&var.value.display()),
                var.visibility
            ),
        );
    }
    push_line(&mut out, "");
    let mut current: BTreeMap<String, VarValue> = BTreeMap::new();
    let mut start: BTreeMap<String, String> = BTreeMap::new();
    let mut snapshots: Vec<(u32, BTreeMap<String, String>)> = Vec::new();
    for entry in journal.entries() {
        match &entry.kind {
            EntryKind::VarDeclared { var } => {
                current.insert(var.id.clone(), var.value.clone());
                if entry.round == 0 {
                    if let Some(v) = track_value(&var.value) {
                        start.entry(var.id.clone()).or_insert(v);
                    }
                }
            }
            EntryKind::WorldDelta { var, to, .. } => {
                current.insert(var.clone(), to.clone());
            }
            EntryKind::RoundClosed { .. } => {
                let snap = current
                    .iter()
                    .filter_map(|(k, v)| track_value(v).map(|s| (k.clone(), s)))
                    .collect();
                snapshots.push((entry.round, snap));
            }
            _ => {}
        }
    }
    if !snapshots.is_empty() {
        push_line(&mut out, "### Verlauf je Runde");
        push_line(&mut out, "");
        let mut header = String::from("| Variable | Start |");
        let mut sep = String::from("|---|---|");
        for (round, _) in &snapshots {
            header.push_str(&format!(" R{round} |"));
            sep.push_str("---|");
        }
        push_line(&mut out, &header);
        push_line(&mut out, &sep);
        for (id, first) in &start {
            let mut row = format!("| `{id}` | {} |", cell(first));
            for (_, snap) in &snapshots {
                row.push_str(&format!(
                    " {} |",
                    cell(snap.get(id).map_or("—", String::as_str))
                ));
            }
            push_line(&mut out, &row);
        }
        push_line(&mut out, "");
    }

    // 3. Zeitleiste
    push_line(&mut out, "## Zeitleiste");
    push_line(&mut out, "");
    let args = collect_arguments(journal);
    let mut rounds: BTreeSet<u32> = args.iter().map(|a| a.round).collect();
    let mut round_notes: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for entry in journal.entries() {
        let note = match &entry.kind {
            EntryKind::Forfeit { text, .. } => Some(text.clone()),
            EntryKind::InjectApplied {
                inject_id, text, ..
            } => Some(format!("Inject `{inject_id}`: {text}")),
            EntryKind::Narrated {
                argument_id: None,
                text,
            } => Some(format!("Zusammenfassung: {text}")),
            _ => None,
        };
        if let Some(note) = note {
            let list = round_notes.entry(entry.round).or_default();
            if !list.contains(&note) {
                list.push(note);
            }
            rounds.insert(entry.round);
        }
    }
    for round in rounds {
        push_line(&mut out, &format!("### Runde {round}"));
        push_line(&mut out, "");
        for note in round_notes.get(&round).into_iter().flatten() {
            push_line(&mut out, &format!("- {note}"));
        }
        if round_notes.contains_key(&round) {
            push_line(&mut out, "");
        }
        for a in args.iter().filter(|a| a.round == round) {
            write_argument(&mut out, scenario, a);
        }
    }

    // 4. Offenlegung
    push_line(&mut out, "## Offenlegung");
    push_line(&mut out, "");
    push_line(&mut out, "### Geheime Argumente");
    push_line(&mut out, "");
    let checks = disclosure_checks(journal, state)?;
    if checks.is_empty() {
        push_line(&mut out, "Keine geheimen Argumente.");
    } else {
        push_line(
            &mut out,
            "| Geheimnis | Argument | Sitz | Commitment | Offengelegt | Prüfung |",
        );
        push_line(&mut out, "|---|---|---|---|---|---|");
        for c in &checks {
            push_line(
                &mut out,
                &format!(
                    "| #{} | {} | {} | `{}…` | {} | {} |",
                    c.secret_id,
                    c.argument_id,
                    c.seat,
                    c.announced.short(),
                    if c.revealed_in_game {
                        "im Spiel"
                    } else {
                        "erst im AAR"
                    },
                    if c.verified {
                        "Commitment bestätigt"
                    } else {
                        "Commitment NICHT bestätigt"
                    }
                ),
            );
        }
    }
    push_line(&mut out, "");
    let steps: Vec<String> = journal
        .entries()
        .filter_map(|e| match &e.kind {
            EntryKind::SuspicionRaised {
                secret_id,
                from,
                to,
                cause,
                ..
            } => Some(format!(
                "- r{} #{secret_id}: {} → {} (ausgelöst durch `{cause}`)",
                e.round,
                from.label(),
                to.label()
            )),
            _ => None,
        })
        .collect();
    if !steps.is_empty() {
        push_line(&mut out, "### Verdachtsleiter");
        push_line(&mut out, "");
        for line in &steps {
            push_line(&mut out, line);
        }
        push_line(&mut out, "");
    }

    push_line(&mut out, "### Private Kanäle");
    push_line(&mut out, "");
    let mut channel_order: Vec<String> = Vec::new();
    let mut channel_lines: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut channel_heads: BTreeMap<String, String> = BTreeMap::new();
    for entry in journal.entries() {
        match &entry.kind {
            EntryKind::ChannelOpened {
                channel,
                members,
                initiator,
                opening,
            } => {
                if !channel_order.contains(channel) {
                    channel_order.push(channel.clone());
                }
                let [a, b] = members;
                channel_heads.insert(
                    channel.clone(),
                    format!("{a} ⇄ {b}, eröffnet in Runde {}", entry.round),
                );
                channel_lines
                    .entry(channel.clone())
                    .or_default()
                    .push(format!("- r{} {initiator}: {opening}", entry.round));
            }
            EntryKind::NegotiationPosted {
                channel,
                from,
                text,
                proposal,
                accept,
                decline,
            } => {
                let mut line = format!("- r{} {from}: {text}", entry.round);
                if let Some(p) = proposal {
                    line.push_str(&format!(" — Vorschlag: {p}"));
                }
                if let Some(a) = accept {
                    line.push_str(&format!(" — angenommen: {a}"));
                }
                if *decline {
                    line.push_str(" — abgelehnt");
                }
                channel_lines.entry(channel.clone()).or_default().push(line);
            }
            _ => {}
        }
    }
    if channel_order.is_empty() {
        push_line(&mut out, "Keine privaten Verhandlungen.");
        push_line(&mut out, "");
    }
    for channel in &channel_order {
        let head = channel_heads.get(channel).map_or("", String::as_str);
        push_line(&mut out, &format!("#### `{channel}` ({head})"));
        push_line(&mut out, "");
        for line in channel_lines.get(channel).into_iter().flatten() {
            push_line(&mut out, line);
        }
        push_line(&mut out, "");
    }

    push_line(&mut out, "### Private Notizen, Absichten und geheime Ziele");
    push_line(&mut out, "");
    let mut any_private = false;
    for entry in journal.entries() {
        let line = match &entry.kind {
            EntryKind::PrivateNote {
                argument_id,
                seat,
                text,
            } => Some(format!(
                "- r{} {seat} zu {argument_id}: {text}",
                entry.round
            )),
            EntryKind::Briefed {
                seat,
                intent: Some(intent),
            } => Some(format!("- r{} Absicht {seat}: {intent}", entry.round)),
            EntryKind::BehaviorBriefing { faction, profile } => {
                let mut parts = vec![format!(
                    "Risiko {} ({:.2})",
                    profile.risk_label(),
                    profile.risk
                )];
                if profile.loss_framing {
                    parts.push("Verlustrahmung".to_owned());
                }
                if let Some(anchor) = &profile.anchor {
                    parts.push(format!("Bezugspunkt: {anchor}"));
                }
                if !profile.rules.is_empty() {
                    parts.push(format!("Regeln: {}", profile.rules.join("; ")));
                }
                parts.push(format!("rote Linien: {}", profile.red_lines.join("; ")));
                Some(format!(
                    "- {faction} Verhaltensprofil: {}",
                    parts.join(" — ")
                ))
            }
            EntryKind::SecretBriefing {
                faction,
                secret_goals,
                private_brief,
            } => {
                let mut parts = Vec::new();
                if !secret_goals.is_empty() {
                    parts.push(format!("geheime Ziele: {}", secret_goals.join("; ")));
                }
                if let Some(b) = private_brief {
                    parts.push(format!("privates Briefing: {b}"));
                }
                Some(format!("- {faction}: {}", parts.join(" — ")))
            }
            _ => None,
        };
        if let Some(line) = line {
            any_private = true;
            push_line(&mut out, &line);
        }
    }
    if !any_private {
        push_line(&mut out, "Keine.");
    }
    push_line(&mut out, "");

    // 4b. Präzedenzregister und Red Cell
    push_line(&mut out, "## Präzedenzregister");
    push_line(&mut out, "");
    let precedents = later_matches(journal);
    if precedents.is_empty() {
        push_line(&mut out, "Keine Präzedenzfälle markiert.");
    } else {
        push_line(
            &mut out,
            "| Fall | Runde | Argument | Maßstab | Schlagworte | Netto / % | Später einschlägig für |",
        );
        push_line(&mut out, "|---|---|---|---|---|---|---|");
        for r in &precedents {
            let p = &r.precedent;
            let later = if r.arguments.is_empty() {
                "—".to_owned()
            } else {
                r.arguments.join(", ")
            };
            push_line(
                &mut out,
                &format!(
                    "| {} | {} | {} | {} | {} | {:+} / {} % | {later} |",
                    p.id,
                    p.round,
                    p.argument_id,
                    cell(&p.principle),
                    cell(&p.tags.join(", ")),
                    p.net,
                    p.probability_pct
                ),
            );
        }
    }
    push_line(&mut out, "");
    let red_cell: Vec<String> = journal
        .entries()
        .filter_map(|e| match &e.kind {
            EntryKind::RedCellObjection {
                target: Some(t),
                assumption,
                cons,
            } => Some(format!(
                "- r{} gegen {t}: Kernannahme „{}“ — {} Contra(s)",
                e.round,
                assumption.as_deref().unwrap_or("—"),
                cons.len()
            )),
            EntryKind::RedCellObjection { target: None, .. } => {
                Some(format!("- r{} kein Einwand", e.round))
            }
            _ => None,
        })
        .collect();
    if !red_cell.is_empty() {
        push_line(&mut out, "## Red Cell");
        push_line(&mut out, "");
        for line in &red_cell {
            push_line(&mut out, line);
        }
        push_line(&mut out, "");
    }

    // 5. Zielerreichung
    push_line(&mut out, "## Zielerreichung");
    push_line(&mut out, "");
    let ratings: &[GoalRating] = input
        .synthesis
        .map_or(&[][..], |s| s.goal_ratings.as_slice());
    push_line(
        &mut out,
        "| Fraktion | Ziel | Art | Bewertung (0–3) | Begründung |",
    );
    push_line(&mut out, "|---|---|---|---|---|");
    for faction in scenario.factions() {
        let goals = faction
            .public_goals
            .iter()
            .map(|g| (g, false))
            .chain(faction.secret_goals.iter().map(|g| (g, true)));
        for (goal, secret) in goals {
            let rating = ratings.iter().find(|r| {
                r.faction == faction.id.as_str() && &r.goal == goal && r.secret == secret
            });
            let (score, rationale) = rating.map_or_else(
                || ("nicht bewertet".to_owned(), "—".to_owned()),
                |r| (r.score.to_string(), r.rationale.clone()),
            );
            push_line(
                &mut out,
                &format!(
                    "| {} | {} | {} | {score} | {} |",
                    cell(&faction.name),
                    cell(goal),
                    if secret { "geheim" } else { "öffentlich" },
                    cell(&rationale)
                ),
            );
        }
    }
    push_line(&mut out, "");

    // 6. Schlüsselmomente & Alternativen
    push_line(&mut out, "## Schlüsselmomente & Alternativen");
    push_line(&mut out, "");
    match input.synthesis {
        Some(s) if !s.key_moments.is_empty() || !s.fork_rounds.is_empty() => {
            for m in &s.key_moments {
                push_line(&mut out, &format!("- {m}"));
            }
            if !s.fork_rounds.is_empty() {
                let rounds: Vec<String> = s.fork_rounds.iter().map(ToString::to_string).collect();
                push_line(
                    &mut out,
                    &format!("- Vorgeschlagene Fork-Runden: {}", rounds.join(", ")),
                );
            }
            if let Some(summary) = &s.summary {
                push_line(&mut out, "");
                push_line(&mut out, summary);
            }
        }
        _ => push_line(&mut out, "— (keine Umpire-Synthese geliefert)"),
    }
    push_line(&mut out, "");

    // 7. Spieler-Debrief
    if !input.debriefs.is_empty() {
        push_line(&mut out, "## Spieler-Debrief");
        push_line(&mut out, "");
        for player in &state.players {
            let Some(d) = input.debriefs.get(player) else {
                continue;
            };
            push_line(&mut out, &format!("### {}", scenario.display_name(player)));
            push_line(&mut out, "");
            push_line(&mut out, &format!("- Wollte: {}", d.wanted));
            push_line(&mut out, &format!("- Passiert: {}", d.happened));
            push_line(&mut out, &format!("- Überrascht: {}", d.surprised));
            push_line(&mut out, &format!("- Anders machen: {}", d.differently));
            push_line(&mut out, "");
        }
    }

    // 8. Plausibilitätscheck und Facilitator-Eingriffe
    push_line(&mut out, "## Plausibilitätscheck");
    push_line(&mut out, "");
    push_line(
        &mut out,
        "Leitfragen (nach Sabin): Ergibt das Verhalten der Akteure ungefähr einen bekannten \
         oder glaubwürdigen Verlauf? Haben rational handelnde Spieler zumindest manchmal die \
         realistischen Strategien gewählt?",
    );
    push_line(&mut out, "");
    push_line(
        &mut out,
        input
            .synthesis
            .and_then(|s| s.plausibility.as_deref())
            .unwrap_or("— (keine Einschätzung geliefert)"),
    );
    push_line(&mut out, "");
    out.push_str(&render_design_lessons(&design_lessons(
        input.loaded,
        journal,
    )));
    push_line(&mut out, "## Facilitator-Eingriffe und Leak-Guard");
    push_line(&mut out, "");
    let mut interventions = Vec::new();
    let mut seen_injects = BTreeSet::new();
    for entry in journal.entries() {
        match &entry.kind {
            EntryKind::InjectApplied {
                inject_id, text, ..
            } if seen_injects.insert(inject_id.clone()) => {
                interventions.push(format!("- r{} Inject `{inject_id}`: {text}", entry.round));
            }
            EntryKind::FacilitatorNote { command, detail } => {
                interventions.push(format!("- r{} {command}: {detail}", entry.round));
            }
            EntryKind::InjectPackageSelected { package_id, plan } => {
                let picks: Vec<String> = plan
                    .iter()
                    .map(|p| format!("`{}` (R{})", p.inject_id, p.round))
                    .collect();
                interventions.push(format!(
                    "- Inject-Paket `{package_id}` (Seed-Auswahl): {}",
                    if picks.is_empty() {
                        "keine Ziehung".to_owned()
                    } else {
                        picks.join(", ")
                    }
                ));
            }
            EntryKind::LeakSuspect {
                source, findings, ..
            } => {
                interventions.push(format!(
                    "- r{} Leak-Verdacht ({source}): {}",
                    entry.round,
                    findings.join("; ")
                ));
            }
            EntryKind::EffectRejected {
                argument_id,
                op_index,
                reason,
            } => {
                interventions.push(format!(
                    "- r{} Effekt verworfen ({argument_id}, Op {op_index}): {reason}",
                    entry.round
                ));
            }
            _ => {}
        }
    }
    if interventions.is_empty() {
        push_line(&mut out, "Keine.");
    }
    for line in interventions {
        push_line(&mut out, &line);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{CLOUD, NORD_REPLY, scripted_game};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn synthesis() -> UmpireSynthesis {
        UmpireSynthesis {
            key_moments: vec!["Die Gilde baut ihr Schmuggelnetz verdeckt aus.".to_owned()],
            fork_rounds: vec![1],
            plausibility: Some("Plausibel; das Nordreich verhält sich wie erwartet.".to_owned()),
            goal_ratings: vec![GoalRating {
                faction: "rat".to_owned(),
                goal: "Kontrolle über die Anlage behalten".to_owned(),
                secret: false,
                score: 3,
                rationale: "Die Anlage blieb beim Rat.".to_owned(),
            }],
            summary: None,
        }
    }

    #[test]
    fn aar_contains_all_sections_and_disclosures() -> TestResult {
        let (loaded, log) = scripted_game(&[21u8; 32], &["rat", "gilde", "nord", "mission"])?;
        let synthesis = synthesis();
        assert!(validate_goal_ratings(&synthesis.goal_ratings, &loaded.scenario).is_empty());
        let mut debriefs = BTreeMap::new();
        debriefs.insert(
            PlayerId::new("rat"),
            PlayerDebrief {
                wanted: "Ordnung".to_owned(),
                happened: "Polizeischutz".to_owned(),
                surprised: "Nordreich".to_owned(),
                differently: "früher verhandeln".to_owned(),
            },
        );
        let md = build_aar(&AarInput {
            loaded: &loaded,
            journal: &log.journal,
            state: &log.state,
            synthesis: Some(&synthesis),
            debriefs: &debriefs,
            models: Some("test-model"),
        })?;
        for heading in [
            "# After-Action-Review: Wasserkrise auf den Karst-Inseln",
            "## Endzustand",
            "### Verlauf je Runde",
            "## Zeitleiste",
            "### Runde 1",
            "#### r1-a1",
            "## Offenlegung",
            "### Geheime Argumente",
            "### Private Kanäle",
            "## Zielerreichung",
            "## Schlüsselmomente & Alternativen",
            "## Spieler-Debrief",
            "## Plausibilitätscheck",
            "## Facilitator-Eingriffe und Leak-Guard",
        ] {
            assert!(md.contains(heading), "fehlt: {heading}\n{md}");
        }
        assert!(md.contains("keine Prognose"));
        assert!(md.contains("Commitment bestätigt"));
        assert!(!md.contains("NICHT bestätigt"));
        assert!(md.contains(NORD_REPLY), "private Kanäle werden offengelegt");
        assert!(
            md.contains("Nur für uns"),
            "private Notizen werden offengelegt"
        );
        assert!(
            md.contains("smuggling_net"),
            "verdeckte Tracks im Endzustand"
        );
        assert!(md.contains("| 3 | Die Anlage blieb beim Rat. |"));
        assert!(md.contains("nicht bewertet"));
        assert!(
            md.contains("bringt kein Argument vor"),
            "Forfeit-Text der Mission"
        );
        Ok(())
    }

    #[test]
    fn disclosure_check_detects_forged_content() -> TestResult {
        let (_, log) = scripted_game(&[22u8; 32], &["rat", "gilde", "nord", "mission"])?;
        let checks = disclosure_checks(&log.journal, &log.state)?;
        assert_eq!(checks.len(), 1);
        assert!(checks.iter().all(|c| c.verified && c.revealed_in_game));
        // Fälschung: Inhalt in der Offenlegung verändert
        let text = log.journal.to_jsonl()?;
        let needle = "Die Gilde schleust nachts zusätzliche Tanker am Zoll vorbei.";
        let last = text.rfind(needle).ok_or("Offenlegung fehlt")?;
        let forged = format!(
            "{}Die Gilde spendet Wasser an die Armen.{}",
            &text[..last],
            &text[last + needle.len()..]
        );
        let journal = Journal::from_jsonl(&forged)?;
        let checks = disclosure_checks(&journal, &log.state)?;
        assert!(checks.iter().any(|c| !c.verified));
        Ok(())
    }

    #[test]
    fn goal_rating_validation() -> TestResult {
        let loaded = crate::scenario::load_scenario(crate::test_support::KARST)?;
        let bad = vec![
            GoalRating {
                faction: "piraten".to_owned(),
                goal: "x".to_owned(),
                secret: false,
                score: 1,
                rationale: "r".to_owned(),
            },
            GoalRating {
                faction: "rat".to_owned(),
                goal: "Kontrolle über die Anlage behalten".to_owned(),
                secret: true,
                score: 4,
                rationale: String::new(),
            },
        ];
        assert_eq!(validate_goal_ratings(&bad, &loaded.scenario).len(), 4);
        Ok(())
    }

    #[test]
    fn business_aar_lists_key_questions() -> TestResult {
        let loaded = crate::scenario::load_scenario(CLOUD)?;
        let log = crate::phases::open_game(&loaded, &[1u8; 32], None)?;
        let md = build_aar(&AarInput {
            loaded: &loaded,
            journal: &log.journal,
            state: &log.state,
            synthesis: None,
            debriefs: &BTreeMap::new(),
            models: None,
        })?;
        assert!(md.contains("## Schlüsselfragen"));
        assert!(md.contains("KQ1"));
        assert!(md.contains("company.cash"));
        Ok(())
    }
}
