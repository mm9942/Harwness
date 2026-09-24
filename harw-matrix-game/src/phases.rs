//! Rundenstruktur als Zustandsmaschine, JSON-Return-Contracts, Effekt-
//! Validierung und die deterministischen GameMaster-Schritte
//! (matrix-game.md §3, §4, §5.2).
//!
//! Die Funktionen dieses Moduls rufen **kein** Modell auf. Sie nehmen
//! validierte Agenten-Antworten entgegen und erzeugen Journal-Einträge, die
//! über [`GameLog::record`] angewendet werden. Einfügen geschieht immer in
//! kanonischer Sitzreihenfolge — die Fertigstellungsreihenfolge paralleler
//! Child-Aufrufe ändert weder Würfel noch Zustand.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::commitments::{DOMAIN, Salt, commit, sha256_parts, to_hex, verify};
use crate::dice::{self, DiceSystem, Outcome, RollKind};
use crate::error::{MatrixError, MatrixResult};
use crate::scenario::{
    AdjudicationSystem, ArgumentSystem, Ending, LoadedScenario, Rules, Scenario, ScheduledInject,
    TurnOrder, VarVisibility,
};
use crate::state::{
    Audience, AudienceSpec, EntryKind, GameEntry, GameLog, GameState, Ongoing, PlayerId,
    RevealedBy, Seat, SecretRecord, VarValue, WorldVar,
};

// ---------------------------------------------------------------------------
// Phasen-FSM
// ---------------------------------------------------------------------------

/// Phase einer Runde (matrix-game.md §3).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Szenario laden, Weltzustand, Sitze.
    #[default]
    Setup,
    /// Briefing bzw. Lagebild.
    Briefing,
    /// Private Verhandlungen.
    Verhandlung,
    /// Versiegelte Argumente.
    Argumente,
    /// Contras (nur `pros_cons`).
    Gegenargumente,
    /// Umpire-Urteil, Würfe, Erzählung.
    Adjudikation,
    /// Deltas anwenden.
    Veroeffentlichung,
    /// Ongoing, Offenlegungs-Trigger, Snapshot.
    Rundenende,
    /// Optionale Schlussargumente.
    Schlussargumente,
    /// Spielende / After-Action-Review.
    Aar,
}

impl Phase {
    /// Deutsche Anzeige.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Setup => "Setup",
            Self::Briefing => "Briefing",
            Self::Verhandlung => "Verhandlung",
            Self::Argumente => "Argumente",
            Self::Gegenargumente => "Gegenargumente",
            Self::Adjudikation => "Adjudikation",
            Self::Veroeffentlichung => "Veröffentlichung",
            Self::Rundenende => "Rundenende",
            Self::Schlussargumente => "Schlussargumente",
            Self::Aar => "AAR",
        }
    }
}

/// Aus dem Szenario abgeleitete FSM-Parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseConfig {
    /// Rundenzahl.
    pub rounds: u32,
    /// Verhandlungsphase aktiv.
    pub negotiation: bool,
    /// Gegenargument-Phase aktiv (`pros_cons`).
    pub counter_arguments: bool,
    /// Schlussargumente statt fester Rundenzahl.
    pub final_arguments: bool,
}

impl PhaseConfig {
    /// Aus einem Szenario.
    #[must_use]
    pub fn from_scenario(scenario: &Scenario) -> Self {
        let rules = scenario.rules();
        Self {
            rounds: scenario.rounds(),
            negotiation: rules.negotiation.enabled,
            counter_arguments: rules.argument_system == ArgumentSystem::ProsCons,
            final_arguments: rules.ending == Ending::FinalArguments,
        }
    }
}

/// Position in der Rundenstruktur.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundCursor {
    /// Runde (0 = Setup).
    pub round: u32,
    /// Phase.
    pub phase: Phase,
}

impl RoundCursor {
    /// Start (Setup, Runde 0).
    #[must_use]
    pub fn start() -> Self {
        Self {
            round: 0,
            phase: Phase::Setup,
        }
    }

    /// Nächste Position; `None` nach dem AAR.
    #[must_use]
    pub fn next(self, cfg: &PhaseConfig) -> Option<Self> {
        let at = |round: u32, phase: Phase| Some(Self { round, phase });
        match self.phase {
            Phase::Setup => at(1, Phase::Briefing),
            Phase::Briefing if cfg.negotiation => at(self.round, Phase::Verhandlung),
            Phase::Briefing | Phase::Verhandlung => at(self.round, Phase::Argumente),
            Phase::Argumente if cfg.counter_arguments => at(self.round, Phase::Gegenargumente),
            Phase::Argumente | Phase::Gegenargumente => at(self.round, Phase::Adjudikation),
            Phase::Adjudikation => at(self.round, Phase::Veroeffentlichung),
            Phase::Veroeffentlichung => at(self.round, Phase::Rundenende),
            Phase::Rundenende if self.round < cfg.rounds => at(self.round + 1, Phase::Briefing),
            Phase::Rundenende => self.end_early(cfg),
            Phase::Schlussargumente => at(self.round, Phase::Aar),
            Phase::Aar => None,
        }
    }

    /// Facilitator-Befehl „Ende“: direkt zu Schlussargumenten bzw. AAR.
    #[must_use]
    pub fn end_early(self, cfg: &PhaseConfig) -> Option<Self> {
        let phase = if cfg.final_arguments {
            Phase::Schlussargumente
        } else {
            Phase::Aar
        };
        Some(Self {
            round: self.round,
            phase,
        })
    }
}

/// Welcher JSON-Contract von einem Aufruf erwartet wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractKind {
    /// `briefing_ack`.
    BriefingAck,
    /// `negotiation_request`.
    NegotiationRequest,
    /// `negotiation_message`.
    NegotiationMessage,
    /// `player_argument`.
    PlayerArgument,
    /// `counter_argument`.
    CounterArgument,
    /// `umpire_adjudication` (Aufruf A).
    UmpireAdjudication,
    /// `umpire_narration` (Aufruf B).
    UmpireNarration,
    /// Schlussargument (Form wie `player_argument`, genau 3 Gründe).
    FinalArgument,
    /// Spieler-Debrief.
    PlayerDebrief,
    /// Umpire-Synthese im AAR.
    UmpireSynthesis,
}

/// Teilschritt innerhalb einer Phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseStep {
    /// Hauptaufruf der Phase.
    Main,
    /// Verhandlungsaustausch n (1-basiert).
    Exchange(u32),
    /// Umpire-Aufruf B (Erzählung) bzw. Umpire-Urteil nach Schlussargumenten.
    Umpire,
}

/// Ein erwarteter Agenten-Aufruf.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedCall {
    /// Sitz.
    pub seat: Seat,
    /// Contract.
    pub contract: ContractKind,
}

/// Aufrufmenge einer Phase in kanonischer Reihenfolge. Rust-Phasen
/// (Setup, Veröffentlichung, Rundenende) haben keine Aufrufe. In einem
/// Verhandlungsaustausch werden nur Sitze mit offenem Kanal aufgerufen — die
/// Aufrufzahl eines Sitzes hängt nur von seinen eigenen Kanälen ab.
#[must_use]
pub fn expected_calls(
    phase: Phase,
    step: PhaseStep,
    players: &[PlayerId],
    channel_members: &BTreeSet<PlayerId>,
    rules: &Rules,
) -> Vec<ExpectedCall> {
    let each = |contract: ContractKind| -> Vec<ExpectedCall> {
        players
            .iter()
            .map(|p| ExpectedCall {
                seat: Seat::Player(p.clone()),
                contract,
            })
            .collect()
    };
    let umpire = |contract: ContractKind| {
        vec![ExpectedCall {
            seat: Seat::Umpire,
            contract,
        }]
    };
    match (phase, step) {
        (Phase::Briefing, _) => {
            let mut calls = each(ContractKind::BriefingAck);
            calls.extend(umpire(ContractKind::BriefingAck));
            calls
        }
        (Phase::Verhandlung, PhaseStep::Main) if rules.negotiation.enabled => {
            each(ContractKind::NegotiationRequest)
        }
        (Phase::Verhandlung, PhaseStep::Exchange(_)) if rules.negotiation.enabled => players
            .iter()
            .filter(|p| channel_members.contains(*p))
            .map(|p| ExpectedCall {
                seat: Seat::Player(p.clone()),
                contract: ContractKind::NegotiationMessage,
            })
            .collect(),
        (Phase::Argumente, _) => each(ContractKind::PlayerArgument),
        (Phase::Gegenargumente, _) if rules.argument_system == ArgumentSystem::ProsCons => {
            each(ContractKind::CounterArgument)
        }
        (Phase::Adjudikation, PhaseStep::Umpire) => umpire(ContractKind::UmpireNarration),
        (Phase::Adjudikation, _) => umpire(ContractKind::UmpireAdjudication),
        (Phase::Schlussargumente, PhaseStep::Umpire) => umpire(ContractKind::UmpireAdjudication),
        (Phase::Schlussargumente, _) => each(ContractKind::FinalArgument),
        (Phase::Aar, _) => {
            let mut calls = umpire(ContractKind::UmpireSynthesis);
            if rules.debrief_players {
                calls.extend(each(ContractKind::PlayerDebrief));
            }
            calls
        }
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// JSON-Contracts
// ---------------------------------------------------------------------------

/// Zeichenlimit einer Aktion.
pub const MAX_ACTION_CHARS: usize = 600;
/// Zeichenlimit eines Grundes.
pub const MAX_REASON_CHARS: usize = 400;
/// Zeichenlimit privater Notizen und Begründungen.
pub const MAX_NOTE_CHARS: usize = 1_200;
/// Höchstzahl Contras je Argument.
pub const MAX_CONS_PER_ARGUMENT: usize = 3;

/// Entfernt einen Markdown-Codezaun (```json … ```), den Modelle oft trotz
/// Anweisung setzen.
#[must_use]
pub fn strip_fences(raw: &str) -> &str {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        if let Some(inner) = rest.trim_end().strip_suffix("```") {
            return inner.trim();
        }
    }
    trimmed
}

/// Parst eine Agenten-Antwort (genau ein JSON-Objekt, `deny_unknown_fields`).
///
/// # Errors
/// [`MatrixError::Contract`] mit der Parser-Meldung (für den Reprompt).
pub fn parse_contract<T: DeserializeOwned>(raw: &str) -> MatrixResult<T> {
    serde_json::from_str(strip_fences(raw)).map_err(|e| MatrixError::Contract(vec![e.to_string()]))
}

fn default_true() -> bool {
    true
}

/// `briefing_ack`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BriefingAck {
    /// Bestätigung.
    #[serde(default = "default_true")]
    pub ack: bool,
    /// Kurze private Absicht (Audience `Seat(p)`; nur AAR).
    #[serde(default)]
    pub intent: Option<String>,
}

/// Gesprächswunsch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NegotiationOpening {
    /// Adressat (Sitz-ID).
    pub to: String,
    /// Eröffnungsnachricht.
    pub opening: String,
}

/// `negotiation_request`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NegotiationRequest {
    /// Wünsche (leer = keine Gespräche).
    #[serde(default)]
    pub requests: Vec<NegotiationOpening>,
}

/// Nicht bindender Vorschlag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    /// Zusammenfassung.
    pub summary: String,
}

/// Eine Nachricht je offenem Kanal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NegotiationMessage {
    /// Kanal-ID.
    pub channel: String,
    /// Text.
    #[serde(default)]
    pub text: String,
    /// Vorschlag.
    #[serde(default)]
    pub proposal: Option<Proposal>,
    /// Annahme (Zusammenfassung des angenommenen Vorschlags).
    #[serde(default)]
    pub accept: Option<String>,
    /// Ablehnung.
    #[serde(default)]
    pub decline: bool,
}

/// `negotiation_message`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NegotiationMessages {
    /// Nachrichten.
    #[serde(default)]
    pub messages: Vec<NegotiationMessage>,
}

/// `player_argument`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayerArgument {
    /// Aktion („Es passiert …“).
    pub action: String,
    /// Gründe.
    pub pros: Vec<String>,
    /// Geheimes Argument.
    #[serde(default)]
    pub secret: bool,
    /// Zitierte eigene Kanäle.
    #[serde(default)]
    pub cites_negotiation: Vec<String>,
    /// Konfliktgegner (Sitz-ID).
    #[serde(default)]
    pub conflict_target: Option<String>,
    /// Big Project.
    #[serde(default)]
    pub project: Option<String>,
    /// Fail-Chit bei Misserfolg einsetzen.
    #[serde(default)]
    pub use_fail_chit_if_failed: bool,
    /// Private Notiz (Audience `Seat(p)`).
    #[serde(default)]
    pub private_note: Option<String>,
}

/// Inhalt eines Arguments ohne private Notiz — Gegenstand des Commitments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgumentBody {
    /// Aktion.
    pub action: String,
    /// Gründe.
    pub pros: Vec<String>,
    /// Zitierte Kanäle.
    #[serde(default)]
    pub cites_negotiation: Vec<String>,
    /// Konfliktgegner.
    #[serde(default)]
    pub conflict_target: Option<String>,
    /// Big Project.
    #[serde(default)]
    pub project: Option<String>,
    /// Fail-Chit-Einsatz.
    #[serde(default)]
    pub use_fail_chit_if_failed: bool,
}

impl PlayerArgument {
    /// Zerlegt in Inhalt, Geheim-Flag und private Notiz.
    #[must_use]
    pub fn split(&self) -> (ArgumentBody, bool, Option<String>) {
        (
            ArgumentBody {
                action: self.action.clone(),
                pros: self.pros.clone(),
                cites_negotiation: self.cites_negotiation.clone(),
                conflict_target: self.conflict_target.clone(),
                project: self.project.clone(),
                use_fail_chit_if_failed: self.use_fail_chit_if_failed,
            },
            self.secret,
            self.private_note.clone(),
        )
    }
}

/// Contras gegen ein Argument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CounterEntry {
    /// Argument-ID.
    pub argument_id: String,
    /// Contras (max. 3).
    #[serde(default)]
    pub cons: Vec<String>,
}

/// `counter_argument`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CounterArgument {
    /// Contras je Argument.
    #[serde(default)]
    pub counters: Vec<CounterEntry>,
}

/// Urteil des Umpires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Würfeln.
    Roll,
    /// Kein Wurf (nur bei Netto ≥ `auto_success_net`).
    NoRoll,
    /// Zurückweisen (einmaliger Reprompt des Spielers).
    Veto,
}

/// Vom Umpire selbst gelieferter Contra (geheime Argumente).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UmpireCon {
    /// Text.
    pub text: String,
    /// Gewicht 0/1/2.
    pub weight: u8,
}

/// Urteil zu einem Argument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UmpireRuling {
    /// Argument-ID.
    pub argument_id: String,
    /// Urteil.
    pub verdict: Verdict,
    /// Gewichte der Pros (Reihenfolge der Einreichung).
    #[serde(default)]
    pub pro_weights: Vec<u8>,
    /// Gewichte der Contras je Fraktion.
    #[serde(default)]
    pub con_weights: BTreeMap<String, Vec<u8>>,
    /// Umpire-Contras (nur geheime Argumente).
    #[serde(default)]
    pub umpire_cons: Vec<UmpireCon>,
    /// Kontext-Modifikator −2..=2.
    #[serde(default)]
    pub context_modifier: i32,
    /// Pflichtbegründung bei Modifikator ≠ 0.
    #[serde(default)]
    pub context_reason: Option<String>,
    /// Leiterstufe (nur `estimative_d100`).
    #[serde(default)]
    pub probability: Option<u8>,
    /// Inkonsistent mit früherem Argument.
    #[serde(default)]
    pub inconsistent_with: Option<String>,
    /// Öffentliche Begründung (bei geheimen Argumenten `null`).
    #[serde(default)]
    pub public_rationale: Option<String>,
    /// Private Notizen (nur Umpire).
    #[serde(default)]
    pub private_notes: Option<String>,
    /// Effekte bei Erfolg (vor dem Wurf festgelegt).
    #[serde(default)]
    pub on_success: Vec<EffectOp>,
    /// Effekte bei Misserfolg (vor dem Wurf festgelegt).
    #[serde(default)]
    pub on_failure: Vec<EffectOp>,
    /// Löst Offenlegung eines Geheimnisses aus.
    #[serde(default)]
    pub triggers_secret: Option<String>,
}

/// Konfliktpaar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConflictPair {
    /// Erstes Argument.
    pub a: String,
    /// Zweites Argument.
    pub b: String,
}

/// `umpire_adjudication` (Aufruf A).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UmpireAdjudication {
    /// Urteile.
    pub rulings: Vec<UmpireRuling>,
    /// Konflikte.
    #[serde(default)]
    pub conflicts: Vec<ConflictPair>,
    /// Einschätzung der Führenden.
    #[serde(default)]
    pub standing: Vec<String>,
}

/// Eine Erzählung.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Narration {
    /// Argument-ID.
    pub argument_id: String,
    /// Audience (nicht weiter als die des Arguments).
    pub audience: AudienceSpec,
    /// Text.
    pub text: String,
}

/// `umpire_narration` (Aufruf B).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UmpireNarration {
    /// Erzählungen.
    #[serde(default)]
    pub narrations: Vec<Narration>,
    /// Öffentliche Rundenzusammenfassung.
    #[serde(default)]
    pub round_summary: Option<String>,
}

/// Spieler-Debrief (AAR §8.2 Punkt 7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayerDebrief {
    /// Was wolltest du?
    pub wanted: String,
    /// Was ist passiert?
    pub happened: String,
    /// Was hat dich überrascht?
    pub surprised: String,
    /// Was würdest du anders machen?
    pub differently: String,
}

/// Umpire-Synthese im AAR.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UmpireSynthesis {
    /// Zwei bis drei Wendepunkte.
    #[serde(default)]
    pub key_moments: Vec<String>,
    /// Vorgeschlagene Fork-Runden.
    #[serde(default)]
    pub fork_rounds: Vec<u32>,
    /// Plausibilitätscheck (Sabins Validierungsfragen).
    #[serde(default)]
    pub plausibility: Option<String>,
    /// Zielbewertungen.
    #[serde(default)]
    pub goal_ratings: Vec<crate::aar::GoalRating>,
    /// Zusammenfassung.
    #[serde(default)]
    pub summary: Option<String>,
}

/// Typisierte Ausgabe eines Phasenaufrufs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhaseOutput {
    /// Briefing-Bestätigung.
    BriefingAck(BriefingAck),
    /// Gesprächswünsche.
    NegotiationRequest(NegotiationRequest),
    /// Kanalnachrichten.
    NegotiationMessages(NegotiationMessages),
    /// Argument (auch Schlussargument).
    PlayerArgument(PlayerArgument),
    /// Contras.
    CounterArgument(CounterArgument),
    /// Urteile.
    UmpireAdjudication(UmpireAdjudication),
    /// Erzählung.
    UmpireNarration(UmpireNarration),
    /// Debrief.
    PlayerDebrief(PlayerDebrief),
    /// Synthese.
    UmpireSynthesis(UmpireSynthesis),
}

impl PhaseOutput {
    /// Parst die Rohantwort gemäß Contract.
    ///
    /// # Errors
    /// [`MatrixError::Contract`].
    pub fn parse(kind: ContractKind, raw: &str) -> MatrixResult<Self> {
        Ok(match kind {
            ContractKind::BriefingAck => Self::BriefingAck(parse_contract(raw)?),
            ContractKind::NegotiationRequest => Self::NegotiationRequest(parse_contract(raw)?),
            ContractKind::NegotiationMessage => Self::NegotiationMessages(parse_contract(raw)?),
            ContractKind::PlayerArgument | ContractKind::FinalArgument => {
                Self::PlayerArgument(parse_contract(raw)?)
            }
            ContractKind::CounterArgument => Self::CounterArgument(parse_contract(raw)?),
            ContractKind::UmpireAdjudication => Self::UmpireAdjudication(parse_contract(raw)?),
            ContractKind::UmpireNarration => Self::UmpireNarration(parse_contract(raw)?),
            ContractKind::PlayerDebrief => Self::PlayerDebrief(parse_contract(raw)?),
            ContractKind::UmpireSynthesis => Self::UmpireSynthesis(parse_contract(raw)?),
        })
    }
}

/// Ein offengelegtes Argument der laufenden Runde (GameMaster-Sicht).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundArgument {
    /// ID (`r2-a1`).
    pub id: String,
    /// Runde.
    pub round: u32,
    /// Sitz.
    pub seat: PlayerId,
    /// Inhalt.
    pub body: ArgumentBody,
    /// Geheimnis-ID bei geheimen Argumenten.
    pub secret_id: Option<String>,
    /// Contras je Spieler (Einreichungsreihenfolge).
    pub counters: BTreeMap<PlayerId, Vec<String>>,
}

impl RoundArgument {
    /// Geheim?
    #[must_use]
    pub fn is_secret(&self) -> bool {
        self.secret_id.is_some()
    }

    /// Audience des Arguments.
    #[must_use]
    pub fn audience(&self) -> Audience {
        if self.is_secret() {
            Audience::SeatAndUmpire(self.seat.clone())
        } else {
            Audience::Public
        }
    }
}

fn blank(text: Option<&str>) -> bool {
    text.is_none_or(|t| t.trim().is_empty())
}

fn check_len(text: &str, max: usize, label: &str, errors: &mut Vec<String>) {
    let n = text.chars().count();
    if n > max {
        errors.push(format!("{label}: {n} Zeichen (max. {max})"));
    }
}

fn contract_result(errors: Vec<String>) -> MatrixResult<()> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(MatrixError::Contract(errors))
    }
}

/// Validiert ein `player_argument` bzw. Schlussargument.
///
/// # Errors
/// [`MatrixError::Contract`] mit allen Befunden.
pub fn validate_player_argument(
    argument: &PlayerArgument,
    player: &PlayerId,
    state: &GameState,
    rules: &Rules,
    final_argument: bool,
) -> MatrixResult<()> {
    let mut errors = Vec::new();
    if argument.action.trim().is_empty() {
        errors.push("action ist leer".to_owned());
    }
    check_len(&argument.action, MAX_ACTION_CHARS, "action", &mut errors);
    let n = argument.pros.len();
    if final_argument || rules.argument_system == ArgumentSystem::ThreeReasons {
        if n != 3 {
            errors.push(format!("genau 3 pros erforderlich, erhalten {n}"));
        }
    } else if !(1..=5).contains(&n) {
        errors.push(format!("1–5 pros erforderlich, erhalten {n}"));
    }
    for (i, pro) in argument.pros.iter().enumerate() {
        if pro.trim().is_empty() {
            errors.push(format!("pros[{i}] ist leer"));
        }
        check_len(pro, MAX_REASON_CHARS, &format!("pros[{i}]"), &mut errors);
    }
    if argument.secret {
        if final_argument {
            errors.push("Schlussargumente können nicht geheim sein".to_owned());
        }
        if state.secrets_used_by(player) >= rules.max_secret_arguments_per_seat {
            errors.push(format!(
                "Kontingent geheimer Argumente erschöpft (max. {})",
                rules.max_secret_arguments_per_seat
            ));
        }
    }
    for channel in &argument.cites_negotiation {
        let own = state
            .channels
            .get(channel)
            .is_some_and(|c| c.has_member(player));
        if !own {
            // Existenz fremder Kanäle wird nicht bestätigt.
            errors.push(format!(
                "cites_negotiation: `{channel}` ist kein eigener Kanal"
            ));
        }
    }
    if let Some(target) = &argument.conflict_target {
        if target == player.as_str() || !state.players.iter().any(|p| p.as_str() == target) {
            errors.push(format!("conflict_target `{target}` ist kein anderer Sitz"));
        }
    }
    if let Some(project) = &argument.project {
        let visible = state.vars.get(project).is_some_and(|v| {
            matches!(v.value, VarValue::Project { .. })
                && crate::visibility::var_visible_to(&v.visibility, &Seat::Player(player.clone()))
        });
        if !visible {
            errors.push(format!("project `{project}` unbekannt"));
        }
    }
    if argument.use_fail_chit_if_failed && !rules.fail_chits {
        errors.push("Fail-Chits sind in diesem Szenario deaktiviert".to_owned());
    }
    if let Some(note) = &argument.private_note {
        check_len(note, MAX_NOTE_CHARS, "private_note", &mut errors);
    }
    contract_result(errors)
}

/// Validiert `counter_argument`: nur fremde, öffentliche Argumente der Runde,
/// je höchstens drei Contras.
///
/// # Errors
/// [`MatrixError::Contract`].
pub fn validate_counter_argument(
    counter: &CounterArgument,
    player: &PlayerId,
    args: &[RoundArgument],
) -> MatrixResult<()> {
    let mut errors = Vec::new();
    let mut seen = BTreeSet::new();
    for (i, entry) in counter.counters.iter().enumerate() {
        let label = format!("counters[{i}] ({})", entry.argument_id);
        match args
            .iter()
            .find(|a| a.id == entry.argument_id && !a.is_secret())
        {
            None => errors.push(format!(
                "{label}: unbekanntes oder nicht öffentliches Argument"
            )),
            Some(a) if &a.seat == player => errors.push(format!("{label}: eigenes Argument")),
            Some(_) => {}
        }
        if !seen.insert(entry.argument_id.as_str()) {
            errors.push(format!("{label}: doppelt"));
        }
        if entry.cons.len() > MAX_CONS_PER_ARGUMENT {
            errors.push(format!("{label}: mehr als {MAX_CONS_PER_ARGUMENT} Contras"));
        }
        for (j, con) in entry.cons.iter().enumerate() {
            if con.trim().is_empty() {
                errors.push(format!("{label}: cons[{j}] leer"));
            }
            check_len(
                con,
                MAX_REASON_CHARS,
                &format!("{label}: cons[{j}]"),
                &mut errors,
            );
        }
    }
    contract_result(errors)
}

/// Validiert `negotiation_request`.
///
/// # Errors
/// [`MatrixError::Contract`].
pub fn validate_negotiation_request(
    request: &NegotiationRequest,
    player: &PlayerId,
    players: &[PlayerId],
    rules: &Rules,
) -> MatrixResult<()> {
    let mut errors = Vec::new();
    let n = &rules.negotiation;
    if !n.enabled && !request.requests.is_empty() {
        errors.push("Verhandlungen sind deaktiviert".to_owned());
    }
    if request.requests.len() > n.max_channels_per_seat {
        errors.push(format!(
            "höchstens {} Gesprächswünsche erlaubt",
            n.max_channels_per_seat
        ));
    }
    let mut seen = BTreeSet::new();
    for (i, r) in request.requests.iter().enumerate() {
        if r.to == player.as_str() {
            errors.push(format!("requests[{i}]: Gespräch mit sich selbst"));
        } else if !players.iter().any(|p| p.as_str() == r.to) {
            errors.push(format!("requests[{i}]: unbekannter Sitz `{}`", r.to));
        }
        if !seen.insert(r.to.as_str()) {
            errors.push(format!("requests[{i}]: `{}` doppelt", r.to));
        }
        if r.opening.trim().is_empty() {
            errors.push(format!("requests[{i}]: opening leer"));
        }
        check_len(
            &r.opening,
            n.max_message_chars,
            &format!("requests[{i}].opening"),
            &mut errors,
        );
    }
    contract_result(errors)
}

/// Validiert `negotiation_message`: nur eigene Kanäle, je Kanal eine Nachricht.
///
/// # Errors
/// [`MatrixError::Contract`].
pub fn validate_negotiation_messages(
    messages: &NegotiationMessages,
    player: &PlayerId,
    state: &GameState,
    rules: &Rules,
) -> MatrixResult<()> {
    let mut errors = Vec::new();
    let mut seen = BTreeSet::new();
    for (i, m) in messages.messages.iter().enumerate() {
        let own = state
            .channels
            .get(&m.channel)
            .is_some_and(|c| c.has_member(player));
        if !own {
            errors.push(format!(
                "messages[{i}]: `{}` ist kein eigener Kanal",
                m.channel
            ));
        }
        if !seen.insert(m.channel.as_str()) {
            errors.push(format!(
                "messages[{i}]: mehrere Nachrichten für `{}`",
                m.channel
            ));
        }
        if m.text.trim().is_empty() && !m.decline && m.accept.is_none() {
            errors.push(format!("messages[{i}]: text leer"));
        }
        check_len(
            &m.text,
            rules.negotiation.max_message_chars,
            &format!("messages[{i}].text"),
            &mut errors,
        );
    }
    contract_result(errors)
}

fn validate_ruling(
    ruling: &UmpireRuling,
    arg: &RoundArgument,
    state: &GameState,
    rules: &Rules,
    label: &str,
    errors: &mut Vec<String>,
) {
    if ruling.pro_weights.len() != arg.body.pros.len() {
        errors.push(format!(
            "{label}: {} pro_weights für {} pros",
            ruling.pro_weights.len(),
            arg.body.pros.len()
        ));
    }
    if ruling.context_modifier != 0 && blank(ruling.context_reason.as_deref()) {
        errors.push(format!(
            "{label}: context_reason ist bei Modifikator ≠ 0 Pflicht"
        ));
    }
    let cons: Vec<u8> = if arg.is_secret() {
        if !ruling.con_weights.is_empty() {
            errors.push(format!(
                "{label}: geheimes Argument — con_weights müssen leer sein, umpire_cons verwenden"
            ));
        }
        if ruling.public_rationale.is_some() {
            errors.push(format!(
                "{label}: geheimes Argument — public_rationale muss null sein"
            ));
        }
        if ruling.verdict == Verdict::Veto && blank(ruling.private_notes.as_deref()) {
            errors.push(format!(
                "{label}: Veto eines geheimen Arguments braucht private_notes"
            ));
        }
        for (i, c) in ruling.umpire_cons.iter().enumerate() {
            if c.text.trim().is_empty() {
                errors.push(format!("{label}: umpire_cons[{i}] leer"));
            }
        }
        ruling.umpire_cons.iter().map(|c| c.weight).collect()
    } else {
        if !ruling.umpire_cons.is_empty() {
            errors.push(format!("{label}: umpire_cons nur für geheime Argumente"));
        }
        for (faction, weights) in &ruling.con_weights {
            match arg.counters.get(&PlayerId::new(faction.as_str())) {
                None => errors.push(format!("{label}: keine Contras von `{faction}`")),
                Some(cons) if cons.len() != weights.len() => errors.push(format!(
                    "{label}: {} Gewichte für {} Contras von `{faction}`",
                    weights.len(),
                    cons.len()
                )),
                Some(_) => {}
            }
        }
        for (player, cons) in &arg.counters {
            if !cons.is_empty() && !ruling.con_weights.contains_key(player.as_str()) {
                errors.push(format!(
                    "{label}: Gewichte für Contras von `{player}` fehlen"
                ));
            }
        }
        if ruling.verdict == Verdict::Veto && blank(ruling.public_rationale.as_deref()) {
            errors.push(format!("{label}: Veto erfordert public_rationale"));
        }
        ruling.con_weights.values().flatten().copied().collect()
    };
    match dice::net_value(&ruling.pro_weights, &cons, ruling.context_modifier) {
        Ok(net) => {
            if ruling.verdict == Verdict::NoRoll && !dice::auto_success_allowed(net, rules) {
                errors.push(format!(
                    "{label}: no_roll erfordert Netto ≥ {} und allow_auto_success (Netto {net})",
                    rules.auto_success_net
                ));
            }
        }
        Err(MatrixError::Contract(list)) => {
            errors.extend(list.into_iter().map(|e| format!("{label}: {e}")));
        }
        Err(other) => errors.push(format!("{label}: {other}")),
    }
    match rules.adjudication {
        AdjudicationSystem::EstimativeD100 => {
            if !ruling
                .probability
                .is_some_and(|p| dice::LADDER.contains(&p))
            {
                errors.push(format!(
                    "{label}: probability muss eine Leiterstufe sein ({:?})",
                    dice::LADDER
                ));
            }
        }
        AdjudicationSystem::ProsCons2d6 => {
            if ruling.probability.is_some() {
                errors.push(format!("{label}: probability nur bei estimative_d100"));
            }
        }
    }
    if ruling.inconsistent_with.as_deref() == Some(arg.id.as_str()) {
        errors.push(format!(
            "{label}: inconsistent_with verweist auf sich selbst"
        ));
    }
    if let Some(secret) = &ruling.triggers_secret {
        if !state.secrets.get(secret).is_some_and(|s| !s.revealed) {
            errors.push(format!(
                "{label}: triggers_secret `{secret}` unbekannt oder offen"
            ));
        }
    }
    if let Some(text) = &ruling.public_rationale {
        check_len(
            text,
            MAX_NOTE_CHARS,
            &format!("{label}: public_rationale"),
            errors,
        );
    }
    let owner = arg.is_secret().then(|| arg.seat.clone());
    let ctx = EffectContext::for_state(state, rules, arg.audience(), owner);
    for v in validate_effects(&ctx, &ruling.on_success) {
        errors.push(format!("{label}: on_success[{}]: {}", v.index, v.reason));
    }
    for v in validate_effects(&ctx, &ruling.on_failure) {
        errors.push(format!("{label}: on_failure[{}]: {}", v.index, v.reason));
    }
}

/// Validiert `umpire_adjudication` gegen die Argumente der Runde.
///
/// # Errors
/// [`MatrixError::Contract`] mit allen Befunden (Grundlage des Reprompts).
pub fn validate_umpire_adjudication(
    adjudication: &UmpireAdjudication,
    args: &[RoundArgument],
    state: &GameState,
    rules: &Rules,
) -> MatrixResult<()> {
    let mut errors = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for (i, ruling) in adjudication.rulings.iter().enumerate() {
        let label = format!("rulings[{i}] ({})", ruling.argument_id);
        let Some(arg) = args.iter().find(|a| a.id == ruling.argument_id) else {
            errors.push(format!("{label}: unbekanntes Argument"));
            continue;
        };
        if !seen.insert(ruling.argument_id.as_str()) {
            errors.push(format!("{label}: doppelt"));
        }
        validate_ruling(ruling, arg, state, rules, &label, &mut errors);
    }
    for arg in args {
        if !seen.contains(arg.id.as_str()) {
            errors.push(format!("Urteil für `{}` fehlt", arg.id));
        }
    }
    for (i, c) in adjudication.conflicts.iter().enumerate() {
        let ok = |id: &str| {
            args.iter().any(|a| a.id == id && !a.is_secret())
                && adjudication
                    .rulings
                    .iter()
                    .any(|r| r.argument_id == id && r.verdict == Verdict::Roll)
        };
        if c.a == c.b || !ok(&c.a) || !ok(&c.b) {
            errors.push(format!(
                "conflicts[{i}]: `{}`/`{}` müssen zwei verschiedene öffentliche Argumente mit verdict roll sein",
                c.a, c.b
            ));
        }
    }
    let mut standing_seen = BTreeSet::new();
    for s in &adjudication.standing {
        if !state.players.iter().any(|p| p.as_str() == s) {
            errors.push(format!("standing: unbekannter Sitz `{s}`"));
        }
        if !standing_seen.insert(s.as_str()) {
            errors.push(format!("standing: `{s}` doppelt"));
        }
    }
    contract_result(errors)
}

/// Validiert `umpire_narration`: Audience nie weiter als die des Arguments.
///
/// # Errors
/// [`MatrixError::Contract`].
pub fn validate_umpire_narration(
    narration: &UmpireNarration,
    args: &[RoundArgument],
    players: &[PlayerId],
) -> MatrixResult<()> {
    let mut errors = Vec::new();
    for (i, n) in narration.narrations.iter().enumerate() {
        let label = format!("narrations[{i}] ({})", n.argument_id);
        match args.iter().find(|a| a.id == n.argument_id) {
            None => errors.push(format!("{label}: unbekanntes Argument")),
            Some(a) => {
                if !n.audience.0.is_within(&a.audience(), players) {
                    errors.push(format!(
                        "{label}: Audience `{}` weiter als die des Arguments",
                        n.audience.0
                    ));
                }
            }
        }
        if n.text.trim().is_empty() {
            errors.push(format!("{label}: text leer"));
        }
        check_len(&n.text, MAX_NOTE_CHARS, &label, &mut errors);
    }
    contract_result(errors)
}

// ---------------------------------------------------------------------------
// Effekte
// ---------------------------------------------------------------------------

/// Effekt-Operation (vom Umpire vorgeschlagen, von Rust validiert, §4.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum EffectOp {
    /// Track verschieben.
    Add {
        /// Variable.
        var: String,
        /// Schritt (|by| ≤ `max_track_step`).
        by: i32,
    },
    /// Diskreten Zustand setzen.
    Set {
        /// Variable.
        var: String,
        /// Neuer Wert (aus `values`).
        value: String,
    },
    /// Erzählfakt.
    Fact {
        /// Text.
        text: String,
        /// Audience (⊆ Audience des Arguments).
        #[serde(default)]
        audience: AudienceSpec,
    },
    /// Fortwirkender Effekt.
    Ongoing {
        /// Kennung.
        id: String,
        /// Beschreibung.
        text: String,
        /// Je Rundenende anzuwendende Ops.
        #[serde(default)]
        each_round: Vec<EffectOp>,
    },
    /// Fortwirkenden Effekt beenden.
    StopOngoing {
        /// Kennung.
        id: String,
    },
    /// Big Project +1 Stufe.
    ProjectAdvance {
        /// Projekt.
        id: String,
    },
    /// Verdecktes Objekt finden.
    Discover {
        /// Objekt.
        object: String,
    },
    /// Schutzstufe überwinden.
    Breach {
        /// Objekt.
        object: String,
    },
    /// Offenlegung eines Geheimnisses auslösen.
    RevealSecret {
        /// Geheimnis-ID.
        secret_id: String,
    },
}

impl EffectOp {
    fn name(&self) -> &'static str {
        match self {
            Self::Add { .. } => "add",
            Self::Set { .. } => "set",
            Self::Fact { .. } => "fact",
            Self::Ongoing { .. } => "ongoing",
            Self::StopOngoing { .. } => "stop_ongoing",
            Self::ProjectAdvance { .. } => "project_advance",
            Self::Discover { .. } => "discover",
            Self::Breach { .. } => "breach",
            Self::RevealSecret { .. } => "reveal_secret",
        }
    }
}

/// Kontext der Effekt-Validierung.
#[derive(Debug, Clone)]
pub struct EffectContext<'a> {
    /// Weltvariablen.
    pub vars: &'a BTreeMap<String, WorldVar>,
    /// Laufende Effekte.
    pub ongoing: &'a BTreeMap<String, Ongoing>,
    /// Geheimnisse.
    pub secrets: &'a BTreeMap<String, SecretRecord>,
    /// Spieler.
    pub players: &'a [PlayerId],
    /// Regeln.
    pub rules: &'a Rules,
    /// Audience des auslösenden Arguments/Injects.
    pub argument_audience: Audience,
    /// Eigentümer bei geheimen Argumenten.
    pub secret_owner: Option<PlayerId>,
}

impl<'a> EffectContext<'a> {
    /// Kontext über dem aktuellen Zustand.
    #[must_use]
    pub fn for_state(
        state: &'a GameState,
        rules: &'a Rules,
        argument_audience: Audience,
        secret_owner: Option<PlayerId>,
    ) -> Self {
        Self {
            vars: &state.vars,
            ongoing: &state.ongoing,
            secrets: &state.secrets,
            players: &state.players,
            rules,
            argument_audience,
            secret_owner,
        }
    }
}

/// Verletzung einer Effekt-Regel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectViolation {
    /// Index im Zweig.
    pub index: usize,
    /// Grund.
    pub reason: String,
}

/// Ergebnis der Effektplanung.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EffectPlan {
    /// Anzuwendende Journal-Einträge.
    pub entries: Vec<GameEntry>,
    /// Verworfene Ops.
    pub violations: Vec<EffectViolation>,
    /// Auszulösende Offenlegungen.
    pub reveals: Vec<String>,
}

struct Scratch {
    vars: BTreeMap<String, WorldVar>,
    ongoing_ids: BTreeSet<String>,
    steps: BTreeMap<String, i32>,
    advanced: BTreeSet<String>,
    reveals: Vec<String>,
}

fn secret_guard(
    ctx: &EffectContext<'_>,
    var: &WorldVar,
    visibility: &VarVisibility,
) -> Result<(), String> {
    if let Some(owner) = &ctx.secret_owner {
        let ok = match visibility {
            VarVisibility::Umpire => true,
            VarVisibility::Seat(p) => p == owner,
            _ => false,
        };
        if !ok {
            return Err(format!(
                "geheimes Argument darf `{}` (Sichtbarkeit {visibility}) vor der Offenlegung nicht ändern",
                var.id
            ));
        }
    }
    Ok(())
}

fn delta_entries(round: u32, before: &WorldVar, to: &VarValue, cause: &str) -> Vec<GameEntry> {
    before
        .visibility
        .audiences()
        .into_iter()
        .map(|audience| {
            GameEntry::new(
                round,
                audience,
                EntryKind::WorldDelta {
                    var: before.id.clone(),
                    from: before.value.clone(),
                    to: to.clone(),
                    cause: cause.to_owned(),
                },
            )
        })
        .collect()
}

impl Scratch {
    fn new(ctx: &EffectContext<'_>) -> Self {
        Self {
            vars: ctx.vars.clone(),
            ongoing_ids: ctx.ongoing.keys().cloned().collect(),
            steps: BTreeMap::new(),
            advanced: BTreeSet::new(),
            reveals: Vec::new(),
        }
    }

    fn var(&self, id: &str) -> Result<WorldVar, String> {
        self.vars
            .get(id)
            .cloned()
            .ok_or_else(|| format!("unbekannte Variable `{id}`"))
    }

    fn change(
        &mut self,
        round: u32,
        before: &WorldVar,
        to: VarValue,
        cause: &str,
        out: &mut Vec<GameEntry>,
    ) {
        out.extend(delta_entries(round, before, &to, cause));
        if let Some(v) = self.vars.get_mut(&before.id) {
            v.value = to;
        }
    }

    #[allow(clippy::too_many_lines)]
    fn apply_op(
        &mut self,
        ctx: &EffectContext<'_>,
        round: u32,
        op: &EffectOp,
        cause: &str,
        nested: bool,
        out: &mut Vec<GameEntry>,
    ) -> Result<(), String> {
        match op {
            EffectOp::Add { var, by } => {
                let current = self.var(var)?;
                let VarValue::Track { value, min, max } = current.value else {
                    return Err(format!("`{var}` ist kein Track"));
                };
                if *by == 0 {
                    return Err(format!("add auf `{var}` mit by = 0"));
                }
                secret_guard(ctx, &current, &current.visibility)?;
                let total = self.steps.get(var).copied().unwrap_or(0) + by;
                if total.abs() > ctx.rules.max_track_step {
                    return Err(format!(
                        "|Δ {var}| = {} überschreitet max_track_step {}",
                        total.abs(),
                        ctx.rules.max_track_step
                    ));
                }
                self.steps.insert(var.clone(), total);
                let new_value = (value + by).clamp(min, max);
                if new_value != value {
                    self.change(
                        round,
                        &current,
                        VarValue::Track {
                            value: new_value,
                            min,
                            max,
                        },
                        cause,
                        out,
                    );
                }
            }
            EffectOp::Set { var, value } => {
                let current = self.var(var)?;
                let VarValue::State { value: old, values } = &current.value else {
                    return Err(format!("`{var}` ist kein State"));
                };
                if !values.contains(value) {
                    return Err(format!("`{value}` ist kein erlaubter Wert von `{var}`"));
                }
                secret_guard(ctx, &current, &current.visibility)?;
                if old != value {
                    let to = VarValue::State {
                        value: value.clone(),
                        values: values.clone(),
                    };
                    self.change(round, &current, to, cause, out);
                }
            }
            EffectOp::Fact { text, audience } => {
                if text.trim().is_empty() {
                    return Err("fact ohne Text".to_owned());
                }
                if !audience.0.is_within(&ctx.argument_audience, ctx.players) {
                    return Err(format!(
                        "fact-Audience `{}` ist weiter als die des Arguments (`{}`)",
                        audience.0, ctx.argument_audience
                    ));
                }
                out.push(GameEntry::new(
                    round,
                    audience.0.clone(),
                    EntryKind::FactAdded { text: text.clone() },
                ));
            }
            EffectOp::Ongoing {
                id,
                text,
                each_round,
            } => {
                if nested {
                    return Err("ongoing darf nicht verschachtelt werden".to_owned());
                }
                if self.ongoing_ids.contains(id) {
                    return Err(format!("ongoing `{id}` läuft bereits"));
                }
                if self.ongoing_ids.len() >= ctx.rules.max_ongoing {
                    return Err(format!(
                        "höchstens {} laufende Effekte",
                        ctx.rules.max_ongoing
                    ));
                }
                if text.trim().is_empty() {
                    return Err(format!("ongoing `{id}` ohne Text"));
                }
                let mut probe = Scratch {
                    vars: self.vars.clone(),
                    ongoing_ids: self.ongoing_ids.clone(),
                    steps: BTreeMap::new(),
                    advanced: BTreeSet::new(),
                    reveals: Vec::new(),
                };
                let mut sink = Vec::new();
                for (i, inner) in each_round.iter().enumerate() {
                    if matches!(
                        inner,
                        EffectOp::Ongoing { .. }
                            | EffectOp::StopOngoing { .. }
                            | EffectOp::RevealSecret { .. }
                    ) {
                        return Err(format!("each_round[{i}]: `{}` nicht erlaubt", inner.name()));
                    }
                    probe
                        .apply_op(ctx, round, inner, cause, true, &mut sink)
                        .map_err(|e| format!("each_round[{i}]: {e}"))?;
                }
                self.ongoing_ids.insert(id.clone());
                out.push(GameEntry::new(
                    round,
                    ctx.argument_audience.clone(),
                    EntryKind::OngoingStarted {
                        ongoing: Ongoing {
                            id: id.clone(),
                            text: text.clone(),
                            each_round: each_round.clone(),
                            audience: ctx.argument_audience.clone(),
                            source: cause.to_owned(),
                        },
                    },
                ));
            }
            EffectOp::StopOngoing { id } => {
                let audience = ctx
                    .ongoing
                    .get(id)
                    .map_or_else(|| ctx.argument_audience.clone(), |o| o.audience.clone());
                if ctx.secret_owner.is_some() && audience.is_public() {
                    return Err(format!(
                        "geheimes Argument darf öffentliches ongoing `{id}` nicht beenden"
                    ));
                }
                if !self.ongoing_ids.remove(id) {
                    return Err(format!("ongoing `{id}` läuft nicht"));
                }
                out.push(GameEntry::new(
                    round,
                    audience,
                    EntryKind::OngoingStopped { id: id.clone() },
                ));
            }
            EffectOp::ProjectAdvance { id } => {
                let current = self.var(id)?;
                let VarValue::Project { progress, stages } = current.value else {
                    return Err(format!("`{id}` ist kein Projekt"));
                };
                secret_guard(ctx, &current, &current.visibility)?;
                if self.advanced.contains(id) {
                    return Err(format!("`{id}`: höchstens +1 Stufe je Argument"));
                }
                if progress >= stages {
                    return Err(format!("`{id}` ist bereits abgeschlossen"));
                }
                self.advanced.insert(id.clone());
                self.change(
                    round,
                    &current,
                    VarValue::Project {
                        progress: progress + 1,
                        stages,
                    },
                    cause,
                    out,
                );
            }
            EffectOp::Discover { object } => {
                let current = self.var(object)?;
                let VarValue::Object {
                    hidden,
                    protection,
                    visibility_when_found,
                } = current.value.clone()
                else {
                    return Err(format!("`{object}` ist kein Objekt"));
                };
                if !hidden {
                    return Err(format!("`{object}` ist bereits gefunden"));
                }
                secret_guard(ctx, &current, &visibility_when_found)?;
                let to = VarValue::Object {
                    hidden: false,
                    protection,
                    visibility_when_found: visibility_when_found.clone(),
                };
                out.extend(delta_entries(round, &current, &to, cause));
                let found = WorldVar {
                    id: current.id.clone(),
                    label: current.label.clone(),
                    visibility: visibility_when_found,
                    value: to,
                };
                for audience in found.visibility.audiences() {
                    out.push(GameEntry::new(
                        round,
                        audience,
                        EntryKind::VarDeclared { var: found.clone() },
                    ));
                }
                self.vars.insert(found.id.clone(), found);
            }
            EffectOp::Breach { object } => {
                let current = self.var(object)?;
                let VarValue::Object {
                    hidden,
                    protection,
                    visibility_when_found,
                } = current.value.clone()
                else {
                    return Err(format!("`{object}` ist kein Objekt"));
                };
                if hidden {
                    return Err(format!(
                        "`{object}` ist verdeckt — erst discover, dann breach"
                    ));
                }
                if protection == 0 {
                    return Err(format!("`{object}` hat keine Schutzstufe mehr"));
                }
                secret_guard(ctx, &current, &current.visibility)?;
                self.change(
                    round,
                    &current,
                    VarValue::Object {
                        hidden,
                        protection: protection - 1,
                        visibility_when_found,
                    },
                    cause,
                    out,
                );
            }
            EffectOp::RevealSecret { secret_id } => {
                if nested {
                    return Err("reveal_secret nicht in each_round".to_owned());
                }
                if !ctx.secrets.get(secret_id).is_some_and(|s| !s.revealed) {
                    return Err(format!(
                        "Geheimnis `{secret_id}` unbekannt oder bereits offen"
                    ));
                }
                if !self.reveals.contains(secret_id) {
                    self.reveals.push(secret_id.clone());
                }
            }
        }
        Ok(())
    }
}

/// Plant einen Effekt-Zweig: gültige Ops werden zu Journal-Einträgen,
/// ungültige zu [`EffectViolation`]s (sie werden verworfen, nicht teilweise
/// angewendet). Tracks werden auf `[min, max]` geklemmt.
#[must_use]
pub fn plan_effects(
    ctx: &EffectContext<'_>,
    round: u32,
    ops: &[EffectOp],
    cause: &str,
) -> EffectPlan {
    let mut scratch = Scratch::new(ctx);
    let mut plan = EffectPlan::default();
    for (index, op) in ops.iter().enumerate() {
        let mut out = Vec::new();
        match scratch.apply_op(ctx, round, op, cause, false, &mut out) {
            Ok(()) => plan.entries.extend(out),
            Err(reason) => plan.violations.push(EffectViolation { index, reason }),
        }
    }
    plan.reveals = scratch.reveals;
    plan
}

/// Nur die Verletzungen eines Zweigs (für Contract-Validierung/Reprompt).
#[must_use]
pub fn validate_effects(ctx: &EffectContext<'_>, ops: &[EffectOp]) -> Vec<EffectViolation> {
    plan_effects(ctx, 0, ops, "validate").violations
}

// ---------------------------------------------------------------------------
// Versiegelte Einreichung
// ---------------------------------------------------------------------------

/// Sammelbox der versiegelten Argumente einer Runde. Eine zweite Einreichung
/// desselben Sitzes wird abgewiesen (keine Nachbesserung, §9.3 Nr. 14).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArgumentBox {
    round: u32,
    sealed: BTreeMap<PlayerId, PlayerArgument>,
    forfeited: BTreeSet<PlayerId>,
}

impl ArgumentBox {
    /// Leere Box.
    #[must_use]
    pub fn new(round: u32) -> Self {
        Self {
            round,
            sealed: BTreeMap::new(),
            forfeited: BTreeSet::new(),
        }
    }

    /// Runde.
    #[must_use]
    pub fn round(&self) -> u32 {
        self.round
    }

    /// Hat der Sitz schon eingereicht oder gepasst?
    #[must_use]
    pub fn has_submitted(&self, player: &PlayerId) -> bool {
        self.sealed.contains_key(player) || self.forfeited.contains(player)
    }

    /// Versiegelt ein (bereits validiertes) Argument.
    ///
    /// # Errors
    /// [`MatrixError::Phase`] bei zweiter Einreichung.
    pub fn seal(&mut self, player: PlayerId, argument: PlayerArgument) -> MatrixResult<()> {
        if self.has_submitted(&player) {
            return Err(MatrixError::Phase(format!(
                "`{player}` hat in Runde {} bereits eingereicht (keine Nachbesserung)",
                self.round
            )));
        }
        self.sealed.insert(player, argument);
        Ok(())
    }

    /// Sitz passt (zweites Scheitern der Validierung).
    ///
    /// # Errors
    /// [`MatrixError::Phase`] bei zweiter Einreichung.
    pub fn forfeit(&mut self, player: PlayerId) -> MatrixResult<()> {
        if self.has_submitted(&player) {
            return Err(MatrixError::Phase(format!(
                "`{player}` hat in Runde {} bereits eingereicht",
                self.round
            )));
        }
        self.forfeited.insert(player);
        Ok(())
    }

    /// Liegen alle Einreichungen (oder `Forfeit`) vor?
    #[must_use]
    pub fn is_complete(&self, players: &[PlayerId]) -> bool {
        players.iter().all(|p| self.has_submitted(p))
    }

    fn take_ordered(mut self, order: &[PlayerId]) -> Vec<(PlayerId, Option<PlayerArgument>)> {
        order
            .iter()
            .map(|p| (p.clone(), self.sealed.remove(p)))
            .collect()
    }
}

/// Auflösungsreihenfolge: fest (`seating.order`) oder Führende zuerst.
#[must_use]
pub fn resolution_order(state: &GameState, rules: &Rules) -> Vec<PlayerId> {
    match rules.turn_order {
        TurnOrder::Fixed => state.players.clone(),
        TurnOrder::LeaderFirst => {
            let mut order: Vec<PlayerId> = state
                .standing
                .iter()
                .filter(|p| state.players.contains(p))
                .cloned()
                .collect();
            for p in &state.players {
                if !order.contains(p) {
                    order.push(p.clone());
                }
            }
            order
        }
    }
}

// ---------------------------------------------------------------------------
// Deterministische GameMaster-Schritte
// ---------------------------------------------------------------------------

/// Setup: `GameCreated` (Master-Seed, Szenario-Hash), Variablen-Deklarationen
/// je Sichtbarkeit, öffentliche Lage, Briefings, geheime Ziele.
///
/// # Errors
/// Zustandsfehler aus [`GameLog::record`].
pub fn open_game(
    loaded: &LoadedScenario,
    master_seed: &[u8; 32],
    at: Option<Timestamp>,
) -> MatrixResult<GameLog> {
    let scenario = &loaded.scenario;
    let mut log = GameLog::new(GameState::from_scenario(scenario));
    let mut entries = vec![GameEntry::new(
        0,
        Audience::ObserverOnly,
        EntryKind::GameCreated {
            scenario_id: scenario.id().to_owned(),
            scenario_hash: loaded.source_hash.clone(),
            master_seed_hex: to_hex(master_seed),
            players: scenario.seat_order(),
        },
    )];
    for var in scenario.initial_vars() {
        for audience in var.visibility.audiences() {
            entries.push(GameEntry::new(
                0,
                audience,
                EntryKind::VarDeclared { var: var.clone() },
            ));
        }
    }
    let situation = scenario.public_situation();
    if !situation.trim().is_empty() {
        entries.push(GameEntry::new(
            0,
            Audience::Public,
            EntryKind::FactAdded {
                text: situation.trim().to_owned(),
            },
        ));
    }
    for faction in scenario.factions() {
        entries.push(GameEntry::new(
            0,
            Audience::Public,
            EntryKind::FactionBriefing {
                faction: faction.id.clone(),
                name: faction.name.clone(),
                briefing: faction.briefing.clone(),
                public_goals: faction.public_goals.clone(),
                assets: faction.assets.clone(),
            },
        ));
        if !faction.secret_goals.is_empty() || faction.private_brief.is_some() {
            entries.push(GameEntry::new(
                0,
                Audience::SeatAndUmpire(faction.id.clone()),
                EntryKind::SecretBriefing {
                    faction: faction.id.clone(),
                    secret_goals: faction.secret_goals.clone(),
                    private_brief: faction.private_brief.clone(),
                },
            ));
        }
    }
    entries.push(GameEntry::new(
        0,
        Audience::Public,
        EntryKind::PhaseEntered {
            phase: Phase::Setup,
        },
    ));
    log.record_all(entries, at)?;
    Ok(log)
}

/// Journalisiert einen Phasenwechsel.
///
/// # Errors
/// Zustandsfehler.
pub fn enter_phase(
    log: &mut GameLog,
    cursor: RoundCursor,
    at: Option<Timestamp>,
) -> MatrixResult<()> {
    log.record(
        GameEntry::new(
            cursor.round,
            Audience::Public,
            EntryKind::PhaseEntered {
                phase: cursor.phase,
            },
        ),
        at,
    )?;
    Ok(())
}

/// Journalisiert eine Briefing-Bestätigung (Audience `Seat(p)` bzw. `UmpireOnly`).
///
/// # Errors
/// Zustandsfehler.
pub fn record_briefing(
    log: &mut GameLog,
    seat: &Seat,
    ack: &BriefingAck,
    at: Option<Timestamp>,
) -> MatrixResult<()> {
    let audience = match seat {
        Seat::Player(p) => Audience::Seat(p.clone()),
        Seat::Umpire => Audience::UmpireOnly,
    };
    log.record(
        GameEntry::new(
            log.state.round,
            audience,
            EntryKind::Briefed {
                seat: seat.clone(),
                intent: ack.intent.clone(),
            },
        ),
        at,
    )?;
    Ok(())
}

/// Opake Kanal-ID; ohne Master-Seed nicht aus Sitznamen ableitbar.
#[must_use]
pub fn channel_id(master_seed: &[u8; 32], round: u32, a: &PlayerId, b: &PlayerId) -> String {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let digest = sha256_parts(&[
        DOMAIN,
        b"/channel",
        master_seed,
        &round.to_be_bytes(),
        lo.as_str().as_bytes(),
        &[0u8],
        hi.as_str().as_bytes(),
    ]);
    let hex = to_hex(&digest);
    format!("neg-{}", hex.get(..8).unwrap_or(&hex))
}

/// Öffnet Kanäle aus den (validierten) Gesprächswünschen in Sitzreihenfolge.
/// Fragen beide Seiten einander an, entsteht ein Kanal; die zweite Eröffnung
/// wird als Nachricht gepostet. Rückgabe: geöffnete Kanal-IDs.
///
/// # Errors
/// Zustandsfehler.
pub fn open_channels(
    log: &mut GameLog,
    requests: &BTreeMap<PlayerId, NegotiationRequest>,
    at: Option<Timestamp>,
) -> MatrixResult<Vec<String>> {
    let master = log.state.master_seed()?;
    let round = log.state.round;
    let players = log.state.players.clone();
    let mut opened = Vec::new();
    for player in &players {
        let Some(request) = requests.get(player) else {
            continue;
        };
        for r in &request.requests {
            let to = PlayerId::new(r.to.as_str());
            if &to == player || !players.contains(&to) {
                continue;
            }
            let id = channel_id(&master, round, player, &to);
            let audience = Audience::pair(player.clone(), to.clone());
            let kind = if log.state.channels.contains_key(&id) {
                EntryKind::NegotiationPosted {
                    channel: id.clone(),
                    from: player.clone(),
                    text: r.opening.clone(),
                    proposal: None,
                    accept: None,
                    decline: false,
                }
            } else {
                let members = if player <= &to {
                    [player.clone(), to.clone()]
                } else {
                    [to.clone(), player.clone()]
                };
                opened.push(id.clone());
                EntryKind::ChannelOpened {
                    channel: id.clone(),
                    members,
                    initiator: player.clone(),
                    opening: r.opening.clone(),
                }
            };
            log.record(GameEntry::new(round, audience, kind), at)?;
        }
    }
    Ok(opened)
}

/// Postet die (validierten) Kanalnachrichten eines Sitzes.
///
/// # Errors
/// [`MatrixError::State`] bei fremdem Kanal.
pub fn post_messages(
    log: &mut GameLog,
    player: &PlayerId,
    messages: &NegotiationMessages,
    at: Option<Timestamp>,
) -> MatrixResult<()> {
    let round = log.state.round;
    for m in &messages.messages {
        let channel = log
            .state
            .channels
            .get(&m.channel)
            .filter(|c| c.has_member(player))
            .cloned()
            .ok_or_else(|| {
                MatrixError::State(format!("`{}` ist kein Kanal von `{player}`", m.channel))
            })?;
        let [a, b] = channel.members;
        log.record(
            GameEntry::new(
                round,
                Audience::pair(a, b),
                EntryKind::NegotiationPosted {
                    channel: m.channel.clone(),
                    from: player.clone(),
                    text: m.text.clone(),
                    proposal: m.proposal.as_ref().map(|p| p.summary.clone()),
                    accept: m.accept.clone(),
                    decline: m.decline,
                },
            ),
            at,
        )?;
    }
    Ok(())
}

/// Schließt die Verhandlungsphase (öffentlich nur diese Zeile — ohne Zahl,
/// Dauer oder Teilnehmer).
///
/// # Errors
/// Zustandsfehler.
pub fn close_negotiation(log: &mut GameLog, at: Option<Timestamp>) -> MatrixResult<()> {
    let round = log.state.round;
    log.record(
        GameEntry::new(round, Audience::Public, EntryKind::NegotiationClosed),
        at,
    )?;
    Ok(())
}

/// Journalisiert alle Siegel in Auflösungsreihenfolge und legt danach die
/// Argumente offen (öffentlich bzw. `SeatAndUmpire` bei geheimen, mit
/// Rust-Template-Ankündigung und Commitment).
///
/// # Errors
/// [`MatrixError::Phase`], wenn noch Einreichungen fehlen.
pub fn reveal_round(
    log: &mut GameLog,
    scenario: &Scenario,
    submissions: ArgumentBox,
    at: Option<Timestamp>,
) -> MatrixResult<Vec<RoundArgument>> {
    let master = log.state.master_seed()?;
    let round = submissions.round();
    let order = resolution_order(&log.state, scenario.rules());
    if !submissions.is_complete(&order) {
        return Err(MatrixError::Phase(format!(
            "Runde {round}: noch nicht alle Argumente eingegangen"
        )));
    }
    let mut entries = Vec::new();
    let mut prepared = Vec::new();
    for (index, (player, argument)) in submissions.take_ordered(&order).into_iter().enumerate() {
        let id = format!("r{round}-a{}", index + 1);
        match argument {
            None => prepared.push((id, player, None)),
            Some(argument) => {
                let (body, secret, note) = argument.split();
                let commitment = commit(&body, &Salt::derive(&master, &id))?;
                entries.push(GameEntry::new(
                    round,
                    Audience::Public,
                    EntryKind::ArgumentSealed {
                        argument_id: id.clone(),
                        seat: player.clone(),
                        commitment: commitment.clone(),
                    },
                ));
                prepared.push((id, player, Some((body, secret, note, commitment))));
            }
        }
    }
    let mut next_secret = log.state.secrets.len() + 1;
    let mut args = Vec::new();
    for (id, player, data) in prepared {
        let name = scenario.display_name(&player);
        let Some((body, secret, note, commitment)) = data else {
            entries.push(GameEntry::new(
                round,
                Audience::Public,
                EntryKind::Forfeit {
                    seat: player.clone(),
                    phase: Phase::Argumente,
                    text: format!("{name} bringt kein Argument vor."),
                },
            ));
            entries.push(GameEntry::new(
                round,
                Audience::Public,
                EntryKind::ArgumentResolved {
                    argument_id: id,
                    seat: player,
                    outcome: Outcome::Forfeited,
                    grade: None,
                    fail_chit_delta: 0,
                },
            ));
            continue;
        };
        let secret_id = secret.then(|| {
            let sid = format!("s{next_secret}");
            next_secret += 1;
            sid
        });
        if let Some(sid) = &secret_id {
            entries.push(GameEntry::new(
                round,
                Audience::Public,
                EntryKind::SecretArgumentAnnounced {
                    argument_id: id.clone(),
                    seat: player.clone(),
                    secret_id: sid.clone(),
                    commitment,
                    text: format!("{name} bringt ein geheimes Argument vor (#{sid})."),
                },
            ));
        }
        let audience = if secret_id.is_some() {
            Audience::SeatAndUmpire(player.clone())
        } else {
            Audience::Public
        };
        entries.push(
            GameEntry::new(
                round,
                audience,
                EntryKind::ArgumentRevealed {
                    argument_id: id.clone(),
                    seat: player.clone(),
                    argument: body.clone(),
                },
            )
            .with_secret(secret_id.clone()),
        );
        if let Some(text) = note.filter(|n| !n.trim().is_empty()) {
            entries.push(GameEntry::new(
                round,
                Audience::Seat(player.clone()),
                EntryKind::PrivateNote {
                    argument_id: id.clone(),
                    seat: player.clone(),
                    text,
                },
            ));
        }
        args.push(RoundArgument {
            id,
            round,
            seat: player,
            body,
            secret_id,
            counters: BTreeMap::new(),
        });
    }
    log.record_all(entries, at)?;
    Ok(args)
}

/// Journalisiert (validierte) Contras eines Sitzes und hängt sie an die
/// Argumente. Aufruf in Sitzreihenfolge.
///
/// # Errors
/// Contract- oder Zustandsfehler.
pub fn submit_counters(
    log: &mut GameLog,
    args: &mut [RoundArgument],
    player: &PlayerId,
    counter: &CounterArgument,
    at: Option<Timestamp>,
) -> MatrixResult<()> {
    validate_counter_argument(counter, player, args)?;
    let round = log.state.round;
    log.record(
        GameEntry::new(
            round,
            Audience::Public,
            EntryKind::CountersSubmitted {
                seat: player.clone(),
                counters: counter.counters.clone(),
            },
        ),
        at,
    )?;
    for entry in &counter.counters {
        if entry.cons.is_empty() {
            continue;
        }
        if let Some(arg) = args.iter_mut().find(|a| a.id == entry.argument_id) {
            arg.counters.insert(player.clone(), entry.cons.clone());
        }
    }
    Ok(())
}

/// Rust-Teil der Adjudikation eines Arguments: Netto → Zielwert → Wurf
/// (seeded, mit Fail-Chit) → Ergebnis → vorab festgelegter Effekt-Zweig →
/// ausgelöste Offenlegungen. Das Urteil muss vorher mit
/// [`validate_umpire_adjudication`] geprüft sein.
///
/// # Errors
/// Contract-, Zustands- oder Commitment-Fehler.
pub fn resolve_argument(
    log: &mut GameLog,
    scenario: &Scenario,
    arg: &RoundArgument,
    ruling: &UmpireRuling,
    at: Option<Timestamp>,
) -> MatrixResult<Outcome> {
    if ruling.argument_id != arg.id {
        return Err(MatrixError::Contract(vec![format!(
            "Urteil `{}` passt nicht zu Argument `{}`",
            ruling.argument_id, arg.id
        )]));
    }
    let rules = scenario.rules();
    let master = log.state.master_seed()?;
    let round = arg.round;
    let audience = arg.audience();
    let secret = arg.secret_id.clone();
    let result_audience = if arg.is_secret() && scenario.visibility().secret_outcome_public {
        Audience::Public
    } else {
        audience.clone()
    };
    let cons: Vec<u8> = if arg.is_secret() {
        ruling.umpire_cons.iter().map(|c| c.weight).collect()
    } else {
        ruling.con_weights.values().flatten().copied().collect()
    };
    let net = dice::net_value(&ruling.pro_weights, &cons, ruling.context_modifier)?;
    let (system, target, probability_pct) = match rules.adjudication {
        AdjudicationSystem::ProsCons2d6 => {
            let t = dice::target_for(net);
            (DiceSystem::TwoD6, t, dice::probability_pct_2d6(t))
        }
        AdjudicationSystem::EstimativeD100 => {
            let p = ruling
                .probability
                .filter(|p| dice::LADDER.contains(p))
                .ok_or_else(|| {
                    MatrixError::Contract(vec![
                        "probability fehlt oder liegt nicht auf der Leiter".to_owned(),
                    ])
                })?;
            (DiceSystem::D100, p, p)
        }
    };
    let target_opt = (system == DiceSystem::TwoD6).then_some(target);
    let mut entries = vec![GameEntry::new(
        round,
        Audience::UmpireOnly,
        EntryKind::Adjudicated {
            argument_id: arg.id.clone(),
            ruling: ruling.clone(),
            net,
            target: target_opt,
            probability_pct,
        },
    )];
    if !arg.is_secret() {
        entries.push(GameEntry::new(
            round,
            Audience::Public,
            EntryKind::RulingPublished {
                argument_id: arg.id.clone(),
                pro_weights: ruling.pro_weights.clone(),
                con_weights: ruling.con_weights.clone(),
                context_modifier: ruling.context_modifier,
                net,
                target: target_opt,
                probability_pct,
                rationale: ruling.public_rationale.clone(),
            },
        ));
    }
    let (outcome, grade, fail_chit_delta) = match ruling.verdict {
        Verdict::Veto => (Outcome::Vetoed, None, 0),
        Verdict::NoRoll if dice::auto_success_allowed(net, rules) => {
            (Outcome::AutoSuccess, None, 0)
        }
        Verdict::NoRoll | Verdict::Roll => {
            let chit_available = rules.fail_chits
                && arg.body.use_fail_chit_if_failed
                && log.state.fail_chits_of(&arg.seat) > 0;
            let resolved = dice::resolve_roll(
                &master,
                round,
                RollKind::Argument,
                &arg.id,
                system,
                target,
                chit_available,
            );
            for roll in &resolved.rolls {
                entries.push(
                    GameEntry::new(
                        round,
                        result_audience.clone(),
                        EntryKind::DiceRolled { roll: roll.clone() },
                    )
                    .with_secret(secret.clone()),
                );
            }
            let mut delta = 0;
            if resolved.used_fail_chit {
                delta -= 1;
            }
            if !resolved.success && rules.fail_chits {
                delta += 1;
            }
            let outcome = if resolved.success {
                Outcome::Success
            } else {
                Outcome::Failure
            };
            (outcome, Some(resolved.grade), delta)
        }
    };
    entries.push(
        GameEntry::new(
            round,
            result_audience,
            EntryKind::ArgumentResolved {
                argument_id: arg.id.clone(),
                seat: arg.seat.clone(),
                outcome,
                grade,
                fail_chit_delta,
            },
        )
        .with_secret(secret),
    );
    log.record_all(entries, at)?;

    let branch: &[EffectOp] = match outcome {
        Outcome::Success | Outcome::AutoSuccess => &ruling.on_success,
        Outcome::Failure => &ruling.on_failure,
        Outcome::Vetoed | Outcome::Forfeited => &[],
    };
    let plan = {
        let owner = arg.is_secret().then(|| arg.seat.clone());
        let ctx = EffectContext::for_state(&log.state, rules, audience, owner);
        plan_effects(&ctx, round, branch, &arg.id)
    };
    let mut effect_entries = plan.entries;
    for v in &plan.violations {
        effect_entries.push(GameEntry::new(
            round,
            Audience::ObserverOnly,
            EntryKind::EffectRejected {
                argument_id: arg.id.clone(),
                op_index: v.index,
                reason: v.reason.clone(),
            },
        ));
    }
    log.record_all(effect_entries, at)?;

    for secret_id in &plan.reveals {
        reveal_secret(log, secret_id, RevealedBy::Effect(arg.id.clone()), at)?;
    }
    if let Some(secret_id) = &ruling.triggers_secret {
        reveal_secret(log, secret_id, RevealedBy::Trigger(arg.id.clone()), at)?;
    }
    Ok(outcome)
}

/// Legt ein geheimes Argument offen: Inhalt, Salt und Commitment werden
/// öffentlich — jeder kann den Hash prüfen. Bereits offene Geheimnisse
/// werden ignoriert.
///
/// # Errors
/// [`MatrixError::State`] (unbekannt) oder [`MatrixError::Commitment`]
/// (Inhalt passt nicht zum veröffentlichten Commitment).
pub fn reveal_secret(
    log: &mut GameLog,
    secret_id: &str,
    by: RevealedBy,
    at: Option<Timestamp>,
) -> MatrixResult<()> {
    let record = log
        .state
        .secrets
        .get(secret_id)
        .cloned()
        .ok_or_else(|| MatrixError::State(format!("Geheimnis `{secret_id}` unbekannt")))?;
    if record.revealed {
        return Ok(());
    }
    let body = log
        .journal
        .entries()
        .find_map(|e| match &e.kind {
            EntryKind::ArgumentRevealed {
                argument_id,
                argument,
                ..
            } if argument_id == &record.argument_id => Some(argument.clone()),
            _ => None,
        })
        .ok_or_else(|| {
            MatrixError::State(format!(
                "Inhalt von `{}` fehlt im Journal",
                record.argument_id
            ))
        })?;
    let master = log.state.master_seed()?;
    let salt = Salt::derive(&master, &record.argument_id);
    if !verify(&body, &salt, &record.commitment)? {
        return Err(MatrixError::Commitment(format!(
            "Inhalt von `{secret_id}` passt nicht zum Commitment {}",
            record.commitment
        )));
    }
    let round = log.state.round;
    log.record(
        GameEntry::new(
            round,
            Audience::Public,
            EntryKind::SecretRevealed {
                secret_id: record.secret_id.clone(),
                argument_id: record.argument_id.clone(),
                seat: record.owner.clone(),
                content: body,
                salt_hex: salt.to_hex(),
                commitment: record.commitment.clone(),
                by,
            },
        ),
        at,
    )?;
    Ok(())
}

/// Journalisiert eine (validierte, leak-geprüfte) Erzählung.
///
/// # Errors
/// Contract- oder Zustandsfehler.
pub fn record_narration(
    log: &mut GameLog,
    args: &[RoundArgument],
    narration: &UmpireNarration,
    at: Option<Timestamp>,
) -> MatrixResult<()> {
    let players = log.state.players.clone();
    validate_umpire_narration(narration, args, &players)?;
    let round = log.state.round;
    for n in &narration.narrations {
        let secret = args
            .iter()
            .find(|a| a.id == n.argument_id)
            .and_then(|a| a.secret_id.clone());
        log.record(
            GameEntry::new(
                round,
                n.audience.0.clone(),
                EntryKind::Narrated {
                    argument_id: Some(n.argument_id.clone()),
                    text: n.text.clone(),
                },
            )
            .with_secret(secret),
            at,
        )?;
    }
    if let Some(summary) = narration
        .round_summary
        .as_ref()
        .filter(|s| !s.trim().is_empty())
    {
        log.record(
            GameEntry::new(
                round,
                Audience::Public,
                EntryKind::Narrated {
                    argument_id: None,
                    text: summary.clone(),
                },
            ),
            at,
        )?;
    }
    Ok(())
}

/// Übernimmt die Umpire-Einschätzung `standing` (nur Umpire sichtbar).
///
/// # Errors
/// Zustandsfehler.
pub fn record_standing(
    log: &mut GameLog,
    standing: &[String],
    at: Option<Timestamp>,
) -> MatrixResult<()> {
    if standing.is_empty() {
        return Ok(());
    }
    let order = standing.iter().map(|s| PlayerId::new(s.as_str())).collect();
    let round = log.state.round;
    log.record(
        GameEntry::new(
            round,
            Audience::UmpireOnly,
            EntryKind::StandingSet { order },
        ),
        at,
    )?;
    Ok(())
}

/// Wendet ein Inject an (Text je Audience, dann validierte Effekte).
///
/// # Errors
/// Zustandsfehler.
pub fn apply_inject(
    log: &mut GameLog,
    rules: &Rules,
    inject: &ScheduledInject,
    at: Option<Timestamp>,
) -> MatrixResult<()> {
    let round = log.state.round;
    let mut entries: Vec<GameEntry> = inject
        .audiences
        .iter()
        .map(|audience| {
            GameEntry::new(
                round,
                audience.clone(),
                EntryKind::InjectApplied {
                    inject_id: inject.id.clone(),
                    text: inject.text.clone(),
                    attributed: inject.attributed,
                },
            )
        })
        .collect();
    let widest = if inject.audiences.iter().any(Audience::is_public) {
        Audience::Public
    } else {
        inject
            .audiences
            .first()
            .cloned()
            .unwrap_or(Audience::UmpireOnly)
    };
    let plan = {
        let ctx = EffectContext::for_state(&log.state, rules, widest, None);
        plan_effects(&ctx, round, &inject.effects, &inject.id)
    };
    entries.extend(plan.entries);
    for v in plan.violations {
        entries.push(GameEntry::new(
            round,
            Audience::ObserverOnly,
            EntryKind::EffectRejected {
                argument_id: inject.id.clone(),
                op_index: v.index,
                reason: v.reason,
            },
        ));
    }
    log.record_all(entries, at)
}

/// Rundenende: laufende Effekte anwenden, dann `RoundClosed{state_hash}`.
/// Rückgabe ist der Zustands-Hash.
///
/// # Errors
/// Zustandsfehler.
pub fn close_round(
    log: &mut GameLog,
    rules: &Rules,
    at: Option<Timestamp>,
) -> MatrixResult<String> {
    let round = log.state.round;
    let ongoing: Vec<Ongoing> = log.state.ongoing.values().cloned().collect();
    for o in ongoing {
        let plan = {
            let ctx = EffectContext::for_state(&log.state, rules, o.audience.clone(), None);
            plan_effects(&ctx, round, &o.each_round, &format!("ongoing:{}", o.id))
        };
        let mut entries = plan.entries;
        for v in plan.violations {
            entries.push(GameEntry::new(
                round,
                Audience::ObserverOnly,
                EntryKind::EffectRejected {
                    argument_id: format!("ongoing:{}", o.id),
                    op_index: v.index,
                    reason: v.reason,
                },
            ));
        }
        log.record_all(entries, at)?;
    }
    let hash = log.state.state_hash()?;
    log.record(
        GameEntry::new(
            round,
            Audience::ObserverOnly,
            EntryKind::RoundClosed {
                state_hash: hash.clone(),
            },
        ),
        at,
    )?;
    Ok(hash)
}

/// Spielende: alle noch verdeckten geheimen Argumente offenlegen (spätestens
/// im AAR), dann `GameEnded`.
///
/// # Errors
/// Zustands- oder Commitment-Fehler.
pub fn end_game(log: &mut GameLog, reason: &str, at: Option<Timestamp>) -> MatrixResult<()> {
    let open: Vec<String> = log
        .state
        .secrets
        .values()
        .filter(|s| !s.revealed)
        .map(|s| s.secret_id.clone())
        .collect();
    for secret_id in open {
        reveal_secret(log, &secret_id, RevealedBy::GameEnd, at)?;
    }
    let round = log.state.round;
    log.record(
        GameEntry::new(
            round,
            Audience::Public,
            EntryKind::GameEnded {
                reason: reason.to_owned(),
            },
        ),
        at,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::load_scenario;
    use crate::state::replay;
    use crate::test_support::{KARST, scripted_game, scripted_submissions};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn cfg(negotiation: bool, counters: bool, final_arguments: bool) -> PhaseConfig {
        PhaseConfig {
            rounds: 2,
            negotiation,
            counter_arguments: counters,
            final_arguments,
        }
    }

    fn walk(cfg: &PhaseConfig) -> Vec<(u32, Phase)> {
        let mut out = Vec::new();
        let mut cursor = Some(RoundCursor::start());
        while let Some(c) = cursor {
            out.push((c.round, c.phase));
            cursor = c.next(cfg);
        }
        out
    }

    #[test]
    fn fsm_full_round_structure() {
        use Phase::*;
        let steps = walk(&cfg(true, true, false));
        assert_eq!(
            steps,
            vec![
                (0, Setup),
                (1, Briefing),
                (1, Verhandlung),
                (1, Argumente),
                (1, Gegenargumente),
                (1, Adjudikation),
                (1, Veroeffentlichung),
                (1, Rundenende),
                (2, Briefing),
                (2, Verhandlung),
                (2, Argumente),
                (2, Gegenargumente),
                (2, Adjudikation),
                (2, Veroeffentlichung),
                (2, Rundenende),
                (2, Aar),
            ]
        );
    }

    #[test]
    fn fsm_skips_optional_phases() {
        let steps = walk(&cfg(false, false, true));
        assert!(!steps.iter().any(|(_, p)| *p == Phase::Verhandlung));
        assert!(!steps.iter().any(|(_, p)| *p == Phase::Gegenargumente));
        assert_eq!(
            steps.iter().rev().nth(1).map(|s| s.1),
            Some(Phase::Schlussargumente)
        );
        assert_eq!(steps.last().map(|s| s.1), Some(Phase::Aar));
    }

    #[test]
    fn fsm_early_end() {
        let c = RoundCursor {
            round: 1,
            phase: Phase::Argumente,
        };
        assert_eq!(
            c.end_early(&cfg(true, true, false)).map(|c| c.phase),
            Some(Phase::Aar)
        );
        assert_eq!(
            c.end_early(&cfg(true, true, true)).map(|c| c.phase),
            Some(Phase::Schlussargumente)
        );
    }

    #[test]
    fn expected_calls_per_phase() {
        let players: Vec<PlayerId> = ["a", "b", "c", "d"]
            .into_iter()
            .map(PlayerId::new)
            .collect();
        let rules = Rules::default();
        let members: BTreeSet<PlayerId> = [PlayerId::new("b"), PlayerId::new("d")]
            .into_iter()
            .collect();
        assert_eq!(
            expected_calls(Phase::Briefing, PhaseStep::Main, &players, &members, &rules).len(),
            5
        );
        let exchange = expected_calls(
            Phase::Verhandlung,
            PhaseStep::Exchange(1),
            &players,
            &members,
            &rules,
        );
        assert_eq!(
            exchange.iter().map(|c| c.seat.clone()).collect::<Vec<_>>(),
            vec![Seat::player("b"), Seat::player("d")]
        );
        assert!(
            expected_calls(
                Phase::Rundenende,
                PhaseStep::Main,
                &players,
                &members,
                &rules
            )
            .is_empty()
        );
        assert_eq!(
            expected_calls(
                Phase::Adjudikation,
                PhaseStep::Main,
                &players,
                &members,
                &rules
            ),
            vec![ExpectedCall {
                seat: Seat::Umpire,
                contract: ContractKind::UmpireAdjudication
            }]
        );
        let three = Rules {
            argument_system: ArgumentSystem::ThreeReasons,
            ..Rules::default()
        };
        assert!(
            expected_calls(
                Phase::Gegenargumente,
                PhaseStep::Main,
                &players,
                &members,
                &three
            )
            .is_empty()
        );
    }

    #[test]
    fn contracts_parse_design_examples() -> TestResult {
        let arg: PlayerArgument = parse_contract(
            r#"{
  "action": "Die Gilde übernimmt den Betrieb der Entsalzungsanlage im Auftrag des Rates.",
  "pros": ["Die Gilde stellt die einzigen Techniker mit Hafenkran-Zugang.",
           "Der Rat hat kein Budget für Überstunden.",
           "Die Hafenarbeiter streiken sonst."],
  "secret": false,
  "cites_negotiation": [],
  "conflict_target": null,
  "project": null,
  "use_fail_chit_if_failed": false,
  "private_note": "Erster Schritt zur Kontrolle der Anlage."
}"#,
        )?;
        assert_eq!(arg.pros.len(), 3);
        let counter: CounterArgument = parse_contract(
            r#"{"counters": [{"argument_id": "r2-a1", "cons": ["a", "b"]}, {"argument_id": "r2-a3", "cons": []}]}"#,
        )?;
        assert_eq!(counter.counters.len(), 2);
        let msg: NegotiationMessages = parse_contract(
            r#"{"messages": [{"channel": "neg-7f3a", "text": "Gegen 2 Tanker",
                "proposal": {"summary": "2 Tanker/Woche"}, "accept": null, "decline": false}]}"#,
        )?;
        assert_eq!(msg.messages.len(), 1);
        let req: NegotiationRequest =
            parse_contract(r#"{ "requests": [ { "to": "nord", "opening": "Reden wir." } ] }"#)?;
        assert_eq!(req.requests.len(), 1);
        let adj: UmpireAdjudication = parse_contract(
            r#"```json
{
  "rulings": [
    {
      "argument_id": "r2-a1",
      "verdict": "roll",
      "pro_weights": [2, 1, 1],
      "con_weights": { "rat": [2], "mission": [1] },
      "umpire_cons": [],
      "context_modifier": -1,
      "context_reason": "Eine Regierung gibt kritische Infrastruktur selten freiwillig ab.",
      "inconsistent_with": null,
      "public_rationale": "Technisch plausibel.",
      "private_notes": "intern",
      "on_success": [ { "op": "set", "var": "plant_control", "value": "gilde" },
                      { "op": "add", "var": "rat_support", "by": -1 } ],
      "on_failure": [ { "op": "fact", "text": "Der Rat lehnt ab.", "audience": "public" } ],
      "triggers_secret": null
    }
  ],
  "conflicts": [],
  "standing": ["gilde", "rat", "nord", "mission"]
}
```"#,
        )?;
        assert_eq!(adj.rulings.len(), 1);
        let narration: UmpireNarration = parse_contract(
            r#"{"narrations": [{"argument_id": "r2-a1", "audience": "public", "text": "x"}], "round_summary": "y"}"#,
        )?;
        assert_eq!(narration.narrations.len(), 1);
        Ok(())
    }

    #[test]
    fn contracts_deny_unknown_fields() {
        assert!(
            parse_contract::<PlayerArgument>(r#"{"action":"x","pros":["a"],"extra":1}"#).is_err()
        );
        assert!(
            parse_contract::<EffectOp>(r#"{"op":"add","var":"x","by":1,"sneaky":true}"#).is_err()
        );
        assert!(parse_contract::<EffectOp>(r#"{"op":"teleport","var":"x"}"#).is_err());
        assert!(
            parse_contract::<Narration>(r#"{"argument_id":"a","audience":"everyone","text":"x"}"#)
                .is_err()
        );
    }

    #[test]
    fn strip_fences_variants() {
        assert_eq!(strip_fences("```json\n{}\n```"), "{}");
        assert_eq!(strip_fences("```\n{\"a\":1}\n```  "), "{\"a\":1}");
        assert_eq!(strip_fences("  {}  "), "{}");
    }

    fn fresh_log() -> Result<(LoadedScenario, GameLog), Box<dyn std::error::Error>> {
        let loaded = load_scenario(KARST)?;
        let log = open_game(&loaded, &[9u8; 32], None)?;
        Ok((loaded, log))
    }

    fn ctx_for<'a>(log: &'a GameLog, rules: &'a Rules, owner: Option<&str>) -> EffectContext<'a> {
        let audience = owner.map_or(Audience::Public, |o| {
            Audience::SeatAndUmpire(PlayerId::new(o))
        });
        EffectContext::for_state(&log.state, rules, audience, owner.map(PlayerId::new))
    }

    #[test]
    fn effect_validation_rules() -> TestResult {
        let (loaded, log) = fresh_log()?;
        let rules = loaded.scenario.rules();
        let ctx = ctx_for(&log, rules, None);
        let reasons = |ops: Vec<EffectOp>| -> Vec<String> {
            validate_effects(&ctx, &ops)
                .into_iter()
                .map(|v| v.reason)
                .collect()
        };
        assert!(
            reasons(vec![EffectOp::Add {
                var: "water".into(),
                by: 1
            }])
            .is_empty()
        );
        assert!(
            !reasons(vec![EffectOp::Add {
                var: "wasser".into(),
                by: 1
            }])
            .is_empty()
        );
        assert!(
            !reasons(vec![EffectOp::Add {
                var: "water".into(),
                by: 2
            }])
            .is_empty()
        );
        assert!(
            !reasons(vec![EffectOp::Add {
                var: "water".into(),
                by: 0
            }])
            .is_empty()
        );
        // Summe je Argument zählt
        let r = reasons(vec![
            EffectOp::Add {
                var: "water".into(),
                by: 1,
            },
            EffectOp::Add {
                var: "water".into(),
                by: 1,
            },
        ]);
        assert_eq!(r.len(), 1);
        assert!(
            !reasons(vec![EffectOp::Add {
                var: "plant_control".into(),
                by: 1
            }])
            .is_empty()
        );
        assert!(
            reasons(vec![EffectOp::Set {
                var: "plant_control".into(),
                value: "gilde".into()
            }])
            .is_empty()
        );
        assert!(
            !reasons(vec![EffectOp::Set {
                var: "plant_control".into(),
                value: "piraten".into()
            }])
            .is_empty()
        );
        // Big Project: +1 je Argument
        let r = reasons(vec![
            EffectOp::ProjectAdvance {
                id: "pipeline".into(),
            },
            EffectOp::ProjectAdvance {
                id: "pipeline".into(),
            },
        ]);
        assert_eq!(r.len(), 1);
        // erst discover, dann breach
        assert!(
            !reasons(vec![EffectOp::Breach {
                object: "reservoir_altmark".into()
            }])
            .is_empty()
        );
        assert!(
            reasons(vec![
                EffectOp::Discover {
                    object: "reservoir_altmark".into()
                },
                EffectOp::Breach {
                    object: "reservoir_altmark".into()
                },
            ])
            .is_empty()
        );
        // ongoing: keine Verschachtelung, gültige inneren Ops
        assert!(
            reasons(vec![EffectOp::Ongoing {
                id: "streik".into(),
                text: "Hafenstreik".into(),
                each_round: vec![EffectOp::Add {
                    var: "stability".into(),
                    by: -1
                }],
            }])
            .is_empty()
        );
        assert!(
            !reasons(vec![EffectOp::Ongoing {
                id: "x".into(),
                text: "x".into(),
                each_round: vec![EffectOp::Add {
                    var: "nix".into(),
                    by: -1
                }],
            }])
            .is_empty()
        );
        assert!(!reasons(vec![EffectOp::StopOngoing { id: "nie".into() }]).is_empty());
        assert!(
            !reasons(vec![EffectOp::RevealSecret {
                secret_id: "s9".into()
            }])
            .is_empty()
        );
        Ok(())
    }

    #[test]
    fn effects_clamp_to_bounds() -> TestResult {
        let (loaded, log) = fresh_log()?;
        let rules = loaded.scenario.rules();
        // stability startet bei 0, max 3; mehrfach +1 über getrennte Pläne
        let mut state = log.state.clone();
        for _ in 0..5 {
            let plan = {
                let ctx = EffectContext::for_state(&state, rules, Audience::Public, None);
                plan_effects(
                    &ctx,
                    1,
                    &[EffectOp::Add {
                        var: "stability".into(),
                        by: 1,
                    }],
                    "t",
                )
            };
            assert!(plan.violations.is_empty());
            for e in &plan.entries {
                state.apply(e)?;
            }
        }
        let v = state.vars.get("stability").ok_or("stability fehlt")?;
        assert_eq!(
            v.value,
            VarValue::Track {
                value: 3,
                min: -3,
                max: 3
            }
        );
        Ok(())
    }

    #[test]
    fn secret_arguments_cannot_touch_public_vars() -> TestResult {
        let (loaded, log) = fresh_log()?;
        let rules = loaded.scenario.rules();
        let ctx = ctx_for(&log, rules, Some("gilde"));
        assert!(
            validate_effects(
                &ctx,
                &[EffectOp::Add {
                    var: "smuggling_net".into(),
                    by: 1
                }]
            )
            .is_empty()
        );
        assert!(
            validate_effects(
                &ctx,
                &[EffectOp::Add {
                    var: "plant_sabotage_risk".into(),
                    by: 1
                }]
            )
            .is_empty()
        );
        assert!(
            !validate_effects(
                &ctx,
                &[EffectOp::Add {
                    var: "water".into(),
                    by: 1
                }]
            )
            .is_empty()
        );
        assert!(
            !validate_effects(
                &ctx,
                &[EffectOp::Add {
                    var: "north_agents".into(),
                    by: 1
                }]
            )
            .is_empty()
        );
        // Fakten nicht weiter als das Argument
        assert!(
            !validate_effects(
                &ctx,
                &[EffectOp::Fact {
                    text: "x".into(),
                    audience: AudienceSpec(Audience::Public)
                }]
            )
            .is_empty()
        );
        assert!(
            validate_effects(
                &ctx,
                &[EffectOp::Fact {
                    text: "x".into(),
                    audience: AudienceSpec(Audience::Seat(PlayerId::new("gilde")))
                }]
            )
            .is_empty()
        );
        Ok(())
    }

    #[test]
    fn player_argument_validation() -> TestResult {
        let (loaded, log) = fresh_log()?;
        let rules = loaded.scenario.rules();
        let gilde = PlayerId::new("gilde");
        let ok = PlayerArgument {
            action: "Die Gilde handelt.".into(),
            pros: vec!["Grund".into()],
            secret: false,
            cites_negotiation: vec![],
            conflict_target: Some("rat".into()),
            project: Some("pipeline".into()),
            use_fail_chit_if_failed: true,
            private_note: None,
        };
        validate_player_argument(&ok, &gilde, &log.state, rules, false)?;
        let mut bad = ok.clone();
        bad.pros = vec![];
        bad.conflict_target = Some("gilde".into());
        bad.cites_negotiation = vec!["neg-deadbeef".into()];
        bad.project = Some("reservoir_altmark".into());
        let Err(MatrixError::Contract(errs)) =
            validate_player_argument(&bad, &gilde, &log.state, rules, false)
        else {
            return Err("erwartete Contract-Fehler".into());
        };
        assert_eq!(errs.len(), 4, "{errs:?}");
        // Schlussargument: genau 3 Gründe
        assert!(validate_player_argument(&ok, &gilde, &log.state, rules, true).is_err());
        Ok(())
    }

    #[test]
    fn no_resubmission_after_seal() -> TestResult {
        let mut sealed = ArgumentBox::new(1);
        let arg = PlayerArgument {
            action: "x".into(),
            pros: vec!["y".into()],
            secret: false,
            cites_negotiation: vec![],
            conflict_target: None,
            project: None,
            use_fail_chit_if_failed: false,
            private_note: None,
        };
        sealed.seal(PlayerId::new("rat"), arg.clone())?;
        assert!(matches!(
            sealed.seal(PlayerId::new("rat"), arg),
            Err(MatrixError::Phase(_))
        ));
        assert!(matches!(
            sealed.forfeit(PlayerId::new("rat")),
            Err(MatrixError::Phase(_))
        ));
        sealed.forfeit(PlayerId::new("nord"))?;
        assert!(!sealed.is_complete(&[PlayerId::new("rat"), PlayerId::new("gilde")]));
        Ok(())
    }

    #[test]
    fn reveal_requires_all_submissions() -> TestResult {
        let (loaded, mut log) = fresh_log()?;
        enter_phase(
            &mut log,
            RoundCursor {
                round: 1,
                phase: Phase::Argumente,
            },
            None,
        )?;
        let mut sealed = ArgumentBox::new(1);
        sealed.forfeit(PlayerId::new("rat"))?;
        assert!(matches!(
            reveal_round(&mut log, &loaded.scenario, sealed, None),
            Err(MatrixError::Phase(_))
        ));
        Ok(())
    }

    #[test]
    fn scripted_game_replays_identically() -> TestResult {
        let (loaded, log) = scripted_game(&[3u8; 32], &["rat", "gilde", "nord", "mission"])?;
        let replayed = replay(&loaded.scenario, &log.journal)?;
        assert_eq!(replayed, log.state);
        assert_eq!(replayed.state_hash()?, log.state.state_hash()?);
        // Roundtrip über JSONL
        let text = log.journal.to_jsonl()?;
        let back = crate::state::Journal::from_jsonl(&text)?;
        assert_eq!(replay(&loaded.scenario, &back)?, log.state);
        Ok(())
    }

    #[test]
    fn completion_order_does_not_change_journal() -> TestResult {
        let (_, a) = scripted_game(&[4u8; 32], &["rat", "gilde", "nord", "mission"])?;
        let (_, b) = scripted_game(&[4u8; 32], &["mission", "nord", "gilde", "rat"])?;
        assert_eq!(a.journal, b.journal);
        assert_eq!(a.state, b.state);
        Ok(())
    }

    #[test]
    fn replay_detects_tampered_dice_and_state() -> TestResult {
        let (loaded, log) = scripted_game(&[5u8; 32], &["rat", "gilde", "nord", "mission"])?;
        let mut text = log.journal.to_jsonl()?;
        // Würfel manipulieren: "success":true <-> false im ersten DiceRolled
        let pos = text.find("\"success\":").ok_or("kein Wurf")?;
        let tail = &text[pos..];
        let replaced = if tail.starts_with("\"success\":true") {
            tail.replacen("\"success\":true", "\"success\":false", 1)
        } else {
            tail.replacen("\"success\":false", "\"success\":true", 1)
        };
        text = format!("{}{}", &text[..pos], replaced);
        let tampered = crate::state::Journal::from_jsonl(&text)?;
        assert!(matches!(
            replay(&loaded.scenario, &tampered),
            Err(MatrixError::Replay(_))
        ));

        // Verdeckten Weltwert manipulieren, den kein Argument berührt →
        // state_hash am Rundenende weicht ab.
        let text = log.journal.to_jsonl()?;
        let needle = "\"id\":\"north_agents\",\"label\":\"Nordreich-Agenten im Hafen\",\"visibility\":\"seat:nord\",\"value\":{\"kind\":\"track\",\"value\":1";
        if !text.contains(needle) {
            return Err("Deklaration von north_agents nicht gefunden".into());
        }
        let changed = text.replacen(needle, &needle.replace("\"value\":1", "\"value\":2"), 1);
        let tampered = crate::state::Journal::from_jsonl(&changed)?;
        assert!(matches!(
            replay(&loaded.scenario, &tampered),
            Err(MatrixError::Replay(_))
        ));
        Ok(())
    }

    #[test]
    fn fork_replays_prefix_identically() -> TestResult {
        let (loaded, log) = scripted_game(&[6u8; 32], &["rat", "gilde", "nord", "mission"])?;
        let fork = log.journal.fork_at_round_end(1)?;
        let forked_state = replay(&loaded.scenario, &fork)?;
        assert_eq!(forked_state.round, 1);
        // Der Präfix ist exakt der Anfang des Originals.
        assert_eq!(&log.journal.records()[..fork.len()], fork.records());
        Ok(())
    }

    #[test]
    fn secret_argument_reveal_verifies_commitment() -> TestResult {
        let (_, log) = scripted_game(&[7u8; 32], &["rat", "gilde", "nord", "mission"])?;
        let revealed = log.journal.entries().find_map(|e| match &e.kind {
            EntryKind::SecretRevealed {
                content,
                salt_hex,
                commitment,
                ..
            } => Some((content.clone(), salt_hex.clone(), commitment.clone())),
            _ => None,
        });
        let (content, salt_hex, commitment) = revealed.ok_or("keine Offenlegung")?;
        let salt = Salt::from_hex(&salt_hex).ok_or("Salt ungültig")?;
        assert!(verify(&content, &salt, &commitment)?);
        // Siegel-Commitment == Ankündigungs-Commitment
        let sealed = log.journal.entries().find_map(|e| match &e.kind {
            EntryKind::ArgumentSealed {
                argument_id,
                commitment,
                ..
            } if argument_id == "r1-a2" => Some(commitment.clone()),
            _ => None,
        });
        assert_eq!(sealed, Some(commitment));
        Ok(())
    }

    #[test]
    fn secret_limit_is_enforced() -> TestResult {
        let (loaded, log) = scripted_game(&[8u8; 32], &["rat", "gilde", "nord", "mission"])?;
        let gilde = PlayerId::new("gilde");
        assert_eq!(log.state.secrets_used_by(&gilde), 1);
        let mut subs = scripted_submissions();
        let arg = subs.remove(&gilde).ok_or("gilde fehlt")?;
        assert!(arg.secret);
        assert!(
            validate_player_argument(&arg, &gilde, &log.state, loaded.scenario.rules(), false)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn dice_follow_adjudication_in_journal() -> TestResult {
        let (_, log) = scripted_game(&[10u8; 32], &["rat", "gilde", "nord", "mission"])?;
        let mut adjudicated: BTreeSet<String> = BTreeSet::new();
        for e in log.journal.entries() {
            match &e.kind {
                EntryKind::Adjudicated { argument_id, .. } => {
                    adjudicated.insert(argument_id.clone());
                }
                EntryKind::DiceRolled { roll } => {
                    assert!(adjudicated.contains(&roll.argument_id), "Wurf vor Urteil");
                }
                _ => {}
            }
        }
        Ok(())
    }

    #[test]
    fn channel_ids_are_opaque_and_symmetric() {
        let m = [1u8; 32];
        let a = PlayerId::new("rat");
        let b = PlayerId::new("nord");
        let id = channel_id(&m, 1, &a, &b);
        assert_eq!(id, channel_id(&m, 1, &b, &a));
        assert!(id.starts_with("neg-"));
        assert!(!id.contains("rat") && !id.contains("nord"));
        assert_ne!(id, channel_id(&[2u8; 32], 1, &a, &b));
    }
}
