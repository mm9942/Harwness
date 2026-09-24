//! Prompt-Bausteine für Spieler-, Markt- und Umpire-Agenten
//! (matrix-game.md §6, Business-Marktteam nach wargaming-and-analysis.md §1.2).
//!
//! - [`system_prompt`] beschreibt Rolle, Ziele **nur dieses Sitzes**, Regeln,
//!   Vertraulichkeit und die JSON-Contracts. Quelle ist das Szenario; fremde
//!   geheime Ziele oder private Briefings gelangen nie hinein.
//! - [`turn_prompt`] entsteht **ausschließlich** aus der Projektion
//!   [`SeatView`] (nie aus Journal oder Zustand). Weil die Projektion lokal
//!   nummeriert und ohne Zeitstempel ist, ist der Prompt byte-identisch, ob
//!   andere Sitze privat verhandelt haben oder nicht.
//! - [`repair_prompt`] fordert nach einem Validierungsfehler eine korrigierte
//!   Antwort nach demselben Vertrag an.
//!
//! Alle Texte sind deutsch; weicht `scenario.language` ab, wird die
//! Antwortsprache zusätzlich genannt.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::dice::{Grade, Outcome};
use crate::phases::{ContractKind, ExpectedCall, Phase, Verdict};
use crate::scenario::{
    AdjudicationSystem, ArgumentSystem, BusinessScenario, Ending, LoadedScenario, Scenario,
    ScenarioMode, TeamRole,
};
use crate::state::{Audience, EntryKind, PlayerId, RevealedBy, Seat, VarValue};
use crate::visibility::{SeatView, ViewEntry, Viewer, var_visible_to};

/// Rolle eines Agenten am Tisch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SeatRole {
    /// Spieler-Fraktion bzw. Anbieterteam.
    Player,
    /// Neutraler Schiedsrichter und Erzähler (Control).
    Umpire,
    /// Bewertendes Marktteam (Business-Modus, `market_role = "player"`).
    Market,
}

impl SeatRole {
    /// Leitet die Rolle eines Sitzes aus dem Szenario ab: Umpire-Sitz →
    /// [`SeatRole::Umpire`], Business-Team mit `role = "market"` →
    /// [`SeatRole::Market`], sonst [`SeatRole::Player`].
    #[must_use]
    pub fn for_seat(loaded: &LoadedScenario, seat: &Seat) -> Self {
        match (seat, &loaded.scenario) {
            (Seat::Umpire, _) => Self::Umpire,
            (Seat::Player(p), Scenario::Business(b)) => {
                if b.teams
                    .iter()
                    .any(|t| t.id == p.as_str() && t.role == TeamRole::Market)
                {
                    Self::Market
                } else {
                    Self::Player
                }
            }
            (Seat::Player(_), Scenario::Classic(_)) => Self::Player,
        }
    }

    /// Name der Agentenrolle (`matrix-player` usw.).
    #[must_use]
    pub fn agent_role(self) -> &'static str {
        match self {
            Self::Player => "matrix-player",
            Self::Umpire => "matrix-umpire",
            Self::Market => "matrix-market",
        }
    }
}

/// Serde-Name eines Contracts (`player_argument` …).
#[must_use]
pub fn contract_name(kind: ContractKind) -> &'static str {
    match kind {
        ContractKind::BriefingAck => "briefing_ack",
        ContractKind::NegotiationRequest => "negotiation_request",
        ContractKind::NegotiationMessage => "negotiation_message",
        ContractKind::PlayerArgument => "player_argument",
        ContractKind::CounterArgument => "counter_argument",
        ContractKind::UmpireAdjudication => "umpire_adjudication",
        ContractKind::UmpireNarration => "umpire_narration",
        ContractKind::FinalArgument => "final_argument",
        ContractKind::PlayerDebrief => "player_debrief",
        ContractKind::UmpireSynthesis => "umpire_synthesis",
    }
}

/// Kompakte Schemabeschreibung eines Contracts (ein JSON-Objekt, unbekannte
/// Felder sind unzulässig).
#[must_use]
pub fn contract_schema(kind: ContractKind) -> &'static str {
    match kind {
        ContractKind::BriefingAck => {
            r#"{"ack": true, "intent": "<kurze Absicht für diese Runde; privat, nur für dich und die Auswertung>" | null}"#
        }
        ContractKind::NegotiationRequest => {
            r#"{"requests": [{"to": "<Sitz-ID>", "opening": "<Eröffnungsnachricht>"}]}  — leere Liste = keine Gespräche"#
        }
        ContractKind::NegotiationMessage => {
            r#"{"messages": [{"channel": "<eigene Kanal-ID>", "text": "<Nachricht>", "proposal": {"summary": "<Vorschlag>"} | null, "accept": "<angenommener Vorschlag>" | null, "decline": false}]}  — höchstens eine Nachricht je Kanal"#
        }
        ContractKind::PlayerArgument => {
            r#"{"action": "<eine konkrete Aktion deiner Fraktion>", "pros": ["<Grund>", …], "secret": false, "cites_negotiation": ["<eigene Kanal-ID>"], "conflict_target": "<Sitz-ID>" | null, "project": "<Projekt-ID>" | null, "use_fail_chit_if_failed": false, "private_note": "<privat>" | null}"#
        }
        ContractKind::FinalArgument => {
            r#"{"action": "<wie das Spiel für deine Fraktion ausgeht>", "pros": ["<Grund 1>", "<Grund 2>", "<Grund 3>"], "secret": false, "cites_negotiation": [], "conflict_target": null, "project": null, "use_fail_chit_if_failed": false, "private_note": "<privat>" | null}  — genau 3 Gründe, nie geheim"#
        }
        ContractKind::CounterArgument => {
            r#"{"counters": [{"argument_id": "<ID eines fremden öffentlichen Arguments dieser Runde>", "cons": ["<Contra>", …]}]}  — höchstens 3 Contras je Argument"#
        }
        ContractKind::UmpireAdjudication => {
            r#"{"rulings": [{"argument_id": "<ID>", "verdict": "roll" | "no_roll" | "veto", "pro_weights": [0|1|2, …], "con_weights": {"<Sitz-ID>": [0|1|2, …]}, "umpire_cons": [{"text": "<Contra>", "weight": 0|1|2}], "context_modifier": -2..2, "context_reason": "<Pflicht bei Modifikator ≠ 0>" | null, "probability": <Leiterstufe> | null, "inconsistent_with": "<Argument-ID>" | null, "public_rationale": "<öffentlich>" | null, "private_notes": "<nur Umpire>" | null, "on_success": [<Effekt>], "on_failure": [<Effekt>], "triggers_secret": "<Geheimnis-ID>" | null}], "conflicts": [{"a": "<ID>", "b": "<ID>"}], "standing": ["<Sitz-ID>", …]}
Effekte: {"op": "add", "var": "<Track>", "by": <Schritt>} | {"op": "set", "var": "<Zustand>", "value": "<Wert>"} | {"op": "fact", "text": "<Fakt>", "audience": "public" | "seat:<id>" | "seat+umpire:<id>" | "umpire"} | {"op": "ongoing", "id": "<ID>", "text": "<Beschreibung>", "each_round": [<Effekt>]} | {"op": "stop_ongoing", "id": "<ID>"} | {"op": "project_advance", "id": "<Projekt>"} | {"op": "discover", "object": "<Objekt>"} | {"op": "breach", "object": "<Objekt>"} | {"op": "reveal_secret", "secret_id": "<ID>"}"#
        }
        ContractKind::UmpireNarration => {
            r#"{"narrations": [{"argument_id": "<ID>", "audience": "public" | "seat:<id>" | "seat+umpire:<id>" | "umpire", "text": "<kurze Erzählung>"}], "round_summary": "<öffentliche Rundenzusammenfassung>" | null}"#
        }
        ContractKind::PlayerDebrief => {
            r#"{"wanted": "<was wolltest du>", "happened": "<was ist passiert>", "surprised": "<was hat dich überrascht>", "differently": "<was würdest du anders machen>"}"#
        }
        ContractKind::UmpireSynthesis => {
            r#"{"key_moments": ["<Wendepunkt>", …], "fork_rounds": [<Runde>, …], "plausibility": "<Plausibilitätscheck>" | null, "goal_ratings": [{"faction": "<Sitz-ID>", "goal": "<Ziel wörtlich>", "secret": false, "score": 0..3, "rationale": "<Begründung>"}], "summary": "<Zusammenfassung>" | null}"#
        }
    }
}

fn final_instruction(kind: ContractKind) -> String {
    format!(
        "Antworte ausschließlich mit JSON nach Vertrag `{}`.",
        contract_name(kind)
    )
}

// ---------------------------------------------------------------------------
// System-Prompts
// ---------------------------------------------------------------------------

const CONFIDENTIALITY: &str = "\
## Vertraulichkeit (strikt)
- Öffentliche Felder (`action`, `pros`, `cons`, `opening` gegenüber Dritten, `public_rationale`, öffentliche Erzählungen, `round_summary`) dürfen niemals private Informationen enthalten: keine geheimen Ziele, keine Inhalte privater Kanäle, keine verdeckten Werte, keine geheimen Argumente — weder wörtlich noch sinngemäß noch als Andeutung.
- Private Inhalte gehören ausschließlich in die dafür vorgesehenen privaten Felder (`private_note`, `intent`, `private_notes`) bzw. in den eigenen Kanal.
- Legst du als Spieler bewusst etwas aus einem privaten Kanal offen, ist das ein Spielzug; vermerke es dann in `private_note`.
- Der GameMaster prüft öffentliche Texte automatisch auf Überschneidungen mit geschützten Inhalten und hält Verdachtsfälle zurück.";

fn language_hint(scenario: &Scenario, out: &mut String) {
    let lang = scenario.language();
    if !lang.eq_ignore_ascii_case("de") {
        let _ = writeln!(
            out,
            "Freitexte in deinen Antworten schreibst du in der Sprache `{lang}`; JSON-Schlüssel bleiben unverändert.\n"
        );
    }
}

fn bullet_list(items: &[String], out: &mut String) {
    for item in items {
        let _ = writeln!(out, "- {item}");
    }
}

fn rules_section(scenario: &Scenario, role: SeatRole, out: &mut String) {
    let rules = scenario.rules();
    out.push_str("## Regeln\n");
    let _ = writeln!(
        out,
        "- Das Spiel dauert {} Runden bzw. Züge. Jede Runde: Briefing{}, versiegelte Argumente{}, Adjudikation durch den Umpire, Veröffentlichung.",
        scenario.rounds(),
        if rules.negotiation.enabled {
            ", private Verhandlungen"
        } else {
            ""
        },
        if rules.argument_system == ArgumentSystem::ProsCons {
            ", Gegenargumente"
        } else {
            ""
        }
    );
    if let Scenario::Classic(c) = scenario {
        if let Some(span) = &c.round_represents {
            let _ = writeln!(out, "- Eine Runde entspricht {span}.");
        }
    }
    match rules.argument_system {
        ArgumentSystem::ProsCons => out.push_str(
            "- Argument: eine konkrete Aktion („Es geschieht …“) mit 1–5 Gründen (`pros`). Danach dürfen alle anderen Fraktionen Contras (`cons`) gegen fremde öffentliche Argumente einreichen, höchstens 3 je Argument.\n",
        ),
        ArgumentSystem::ThreeReasons => out.push_str(
            "- Argument: eine konkrete Aktion mit genau 3 Gründen (`pros`); es gibt keine Gegenargument-Phase.\n",
        ),
    }
    match rules.adjudication {
        AdjudicationSystem::ProsCons2d6 => out.push_str(
            "- Auflösung mit 2W6: Der Umpire gewichtet jeden Grund und jedes Contra mit 0, 1 oder 2 und setzt ggf. einen begründeten Kontext-Modifikator von −2 bis +2. Netto = Summe Pro − Summe Contra + Modifikator. Zielwert = 7 − Netto (begrenzt auf 3 bis 11). Erfolg bei Augensumme ≥ Zielwert; ein Pasch 1-1 scheitert immer. Ab Zielwert + 3 gilt ein starker Erfolg, ab Zielwert − 3 ein schwerer Misserfolg.\n",
        ),
        AdjudicationSystem::EstimativeD100 => out.push_str(
            "- Auflösung mit W100: Der Umpire ordnet jedes Argument einer Wahrscheinlichkeitsstufe zu (5, 15, 30, 50, 70, 85, 95 %); Erfolg bei Wurf ≤ Stufe.\n",
        ),
    }
    if rules.allow_auto_success {
        let _ = writeln!(
            out,
            "- Ab Netto {} kann der Umpire ein zwingendes Argument ohne Wurf gelingen lassen.",
            rules.auto_success_net
        );
    }
    if rules.fail_chits {
        out.push_str(
            "- Fail-Chits: Ein gescheitertes Argument bringt einen Fail-Chit; mit `use_fail_chit_if_failed` darfst du einen vorhandenen Chit für einen Neuwurf einsetzen.\n",
        );
    }
    let _ = writeln!(
        out,
        "- Effekte eines Arguments legt der Umpire vor dem Wurf fest; Tracks bewegen sich je Effekt um höchstens {} Stufe(n), höchstens {} fortwirkende Effekte gleichzeitig.",
        rules.max_track_step, rules.max_ongoing
    );
    if rules.max_secret_arguments_per_seat > 0 {
        let _ = writeln!(
            out,
            "- Geheime Argumente (`secret: true`): höchstens {} pro Sitz und Spiel, nur für konkrete Vorbereitungen, die über mehrere Züge verborgen bleiben müssen. Öffentlich erscheint nur eine neutrale Ankündigung mit Prüfsumme; den Inhalt sehen nur du und der Umpire, bis das Geheimnis offengelegt wird.",
            rules.max_secret_arguments_per_seat
        );
    } else {
        out.push_str("- Geheime Argumente sind in diesem Szenario nicht erlaubt.\n");
    }
    if rules.negotiation.enabled {
        let n = &rules.negotiation;
        let _ = writeln!(
            out,
            "- Verhandlungen: je Runde höchstens {} Gesprächswünsche pro Sitz, bis zu {} Austausche, Nachrichten bis {} Zeichen. Ein Kanal verbindet genau zwei Sitze; niemand sonst erfährt Inhalt, Teilnehmer oder auch nur, dass verhandelt wurde{}. Absprachen sind nicht bindend.",
            n.max_channels_per_seat,
            n.max_exchanges,
            n.max_message_chars,
            match role {
                SeatRole::Umpire => ", sofern du nicht ausdrücklich Einblick erhältst",
                _ => "",
            }
        );
    }
    if rules.ending == Ending::FinalArguments {
        out.push_str(
            "- Zum Spielende gibt jede Fraktion ein Schlussargument mit genau 3 Gründen ab.\n",
        );
    }
    out.push('\n');
}

fn contracts_section(kinds: &[ContractKind], out: &mut String) {
    out.push_str("## Antwortformat\nJede Antwort ist genau ein JSON-Objekt ohne Codezaun, ohne Text davor oder danach und ohne Metakommentare über Spiel, Modell oder Harness. Unbekannte Felder sind unzulässig. Welcher Vertrag gilt, steht am Ende jedes Auftrags.\n");
    for kind in kinds {
        let _ = writeln!(
            out,
            "- `{}`: {}",
            contract_name(*kind),
            contract_schema(*kind)
        );
    }
    out.push('\n');
}

fn player_contracts(scenario: &Scenario) -> Vec<ContractKind> {
    let rules = scenario.rules();
    let mut kinds = vec![ContractKind::BriefingAck];
    if rules.negotiation.enabled {
        kinds.push(ContractKind::NegotiationRequest);
        kinds.push(ContractKind::NegotiationMessage);
    }
    kinds.push(ContractKind::PlayerArgument);
    if rules.argument_system == ArgumentSystem::ProsCons {
        kinds.push(ContractKind::CounterArgument);
    }
    if rules.ending == Ending::FinalArguments {
        kinds.push(ContractKind::FinalArgument);
    }
    if rules.debrief_players {
        kinds.push(ContractKind::PlayerDebrief);
    }
    kinds
}

fn seat_table(scenario: &Scenario, me: Option<&PlayerId>, out: &mut String) {
    out.push_str("## Sitze am Tisch\n");
    for f in scenario.factions() {
        let marker = if me == Some(&f.id) { " (du)" } else { "" };
        let _ = writeln!(out, "- `{}`: {}{marker}", f.id, f.name);
    }
    out.push('\n');
}

fn business_context(b: &BusinessScenario, out: &mut String) {
    if !b.game.key_questions.is_empty() {
        out.push_str("## Schlüsselfragen des Spiels\n");
        for q in &b.game.key_questions {
            let _ = writeln!(out, "- {}: {}", q.id, q.text);
        }
        out.push('\n');
    }
    if !b.game.move_labels.is_empty() {
        out.push_str("## Züge\n");
        for (i, label) in b.game.move_labels.iter().enumerate() {
            let _ = writeln!(out, "- Zug {}: {label}", i + 1);
        }
        out.push('\n');
    }
    if !b.business.segments.is_empty() {
        out.push_str("## Marktsegmente\n");
        for s in &b.business.segments {
            let _ = writeln!(out, "- `{}`: {}", s.id, s.name);
        }
        out.push('\n');
    }
}

fn own_brief(scenario: &Scenario, player: &PlayerId, out: &mut String) {
    let Some(f) = scenario.factions().into_iter().find(|f| &f.id == player) else {
        let _ = writeln!(
            out,
            "## Deine Fraktion\nSitz `{player}` (keine weiteren Angaben im Szenario).\n"
        );
        return;
    };
    out.push_str("## Deine Fraktion\n");
    let level = f
        .level
        .as_deref()
        .map(|l| format!(", Ebene: {l}"))
        .unwrap_or_default();
    let _ = writeln!(out, "- Name: {} (Sitz `{}`{level})", f.name, f.id);
    if !f.briefing.trim().is_empty() {
        let _ = writeln!(out, "- Lage und Auftrag: {}", f.briefing.trim());
    }
    if !f.public_goals.is_empty() {
        out.push_str("- Öffentliche Ziele (allen bekannt):\n");
        for g in &f.public_goals {
            let _ = writeln!(out, "  - {g}");
        }
    }
    if !f.assets.is_empty() {
        let _ = writeln!(out, "- Machtmittel: {}", f.assets.join(", "));
    }
    if let Scenario::Business(b) = scenario {
        if let Some(team) = b.teams.iter().find(|t| t.id == player.as_str()) {
            if let Some(variant) = &team.strategy_variant {
                let _ = writeln!(out, "- Zu spielende Strategievariante: {variant}");
            }
            if let Some(scale) = &team.assessment_scale {
                if let [lo, hi] = scale.as_slice() {
                    let _ = writeln!(out, "- Bewertungsskala: {lo} bis {hi}");
                }
            }
        }
    }
    if !f.secret_goals.is_empty() || f.private_brief.is_some() {
        out.push_str("\n## Nur für dich (vertraulich)\n");
        if !f.secret_goals.is_empty() {
            out.push_str("Geheime Ziele — niemals öffentlich nennen oder andeuten:\n");
            bullet_list(&f.secret_goals, out);
        }
        if let Some(brief) = &f.private_brief {
            let _ = writeln!(out, "Privates Briefing (Gamebook): {brief}");
        }
    }
    out.push('\n');
}

fn player_system(loaded: &LoadedScenario, player: &PlayerId, market: bool) -> String {
    let scenario = &loaded.scenario;
    let name = scenario.display_name(player);
    let mut out = String::new();
    language_hint(scenario, &mut out);
    out.push_str("# Rolle\n");
    if market {
        let _ = writeln!(
            out,
            "Du bist das Marktteam „{name}“ (Sitz `{player}`) im Business-Wargame „{}“. Zweck: {}\n\
             Du bist Bewerter, nicht Konkurrent: Du vertrittst die Kundschaft der Segmente und beurteilst die Angebote der Anbieterteams ehrlich und nachvollziehbar. Du bluffst nicht, täuschst nicht und verfolgst keine eigene Marktstrategie. Deine Argumente beschreiben plausible Kundenreaktionen und Kaufentscheidungen; deine Contras benennen Schwächen von Angeboten aus Kundensicht.\n",
            scenario.title(),
            scenario.purpose().trim()
        );
    } else {
        let _ = writeln!(
            out,
            "Du spielst die Fraktion „{name}“ (Sitz `{player}`) im Matrix Game „{}“. Zweck des Spiels: {}\n\
             Du bist Akteur — nicht Erzähler und nicht Schiedsrichter. Handle im Sinne deiner Fraktion und auf ihrer Ebene.\n",
            scenario.title(),
            scenario.purpose().trim()
        );
    }
    if let Scenario::Business(b) = scenario {
        if let Some(team) = b.teams.iter().find(|t| t.id == player.as_str()) {
            match team.role {
                TeamRole::Company => out.push_str(
                    "Als Unternehmensteam spielst du den aktuellen Strategieplan deines Hauses so realistisch wie möglich — mit seinen Stärken und Schwächen.\n\n",
                ),
                TeamRole::Competitor => out.push_str(
                    "Als Wettbewerberteam gilt: Sei der Gegner. Denke wie dieses Unternehmen, handle legal und plausibel, aber entschlossen.\n\n",
                ),
                TeamRole::Market => {}
            }
            if let Some(directive) = &team.directive {
                let _ = writeln!(out, "Leitlinie: {}\n", directive.trim());
            }
        }
        business_context(b, &mut out);
    }
    own_brief(scenario, player, &mut out);
    seat_table(scenario, Some(player), &mut out);

    out.push_str("## Was du weißt\nDu kennst ausschließlich, was in deinem Lagebild und deinem Protokoll steht. Andere Sitze haben eigene, teils geheime Ziele und können privat miteinander sprechen, ohne dass du davon erfährst. Erfinde kein Wissen über fremde geheime Ziele, Absprachen oder verdeckte Werte.\n\n");

    out.push_str("## Spielweise\n");
    out.push_str("- Argumentieren: eine konkrete Aktion je Zug; wenige starke Gründe statt vieler schwacher. Baue auf bereits Geschehenem auf, statt erfolgreiche Ereignisse einfach umzukehren. Große Vorhaben zerlegst du in Schritte.\n");
    if scenario.rules().argument_system == ArgumentSystem::ProsCons {
        out.push_str("- Gegenargumente: sachliche Gründe, warum ein fremdes Argument scheitern könnte; keine Wiederholungen, keine Polemik.\n");
    }
    if scenario.rules().negotiation.enabled {
        if market {
            let contact = match scenario {
                Scenario::Business(b) => b
                    .channels
                    .as_ref()
                    .and_then(|c| c.market_contact.clone())
                    .unwrap_or_else(|| "sales_call_only".to_owned()),
                Scenario::Classic(_) => "sales_call_only".to_owned(),
            };
            let _ = writeln!(
                out,
                "- Kanäle: Als Marktteam führst du keine eigenen Verhandlungen mit Anbieterteams. Zulässig sind nur Kundengespräche (Kontaktregel `{contact}`), die Control mitliest; stelle im Zweifel keine Gesprächswünsche."
            );
        } else {
            out.push_str("- Verhandeln: Absprachen sind nicht bindend; du darfst bluffen und Absprachen brechen. Was in einem privaten Kanal steht, bleibt privat, solange du es nicht bewusst offenlegst — tust du es, ist das ein Spielzug.\n");
        }
    }
    out.push('\n');

    rules_section(
        scenario,
        if market {
            SeatRole::Market
        } else {
            SeatRole::Player
        },
        &mut out,
    );
    out.push_str(CONFIDENTIALITY);
    out.push_str("\n\n");
    contracts_section(&player_contracts(scenario), &mut out);
    out
}

fn umpire_system(loaded: &LoadedScenario) -> String {
    let scenario = &loaded.scenario;
    let mut out = String::new();
    language_hint(scenario, &mut out);
    let _ = writeln!(
        out,
        "# Rolle\nDu bist der neutrale Schiedsrichter (Umpire) und Erzähler im Matrix Game „{}“. Zweck des Spiels: {}\n\
         Dein Ziel ist eine glaubwürdige, zusammenhängende Erzählung — nicht ein Sieger. Du bevorzugst keine Fraktion.\n",
        scenario.title(),
        scenario.purpose().trim()
    );
    if let Scenario::Business(b) = scenario {
        out.push_str("Im Business-Modus bist du Control: Das Marktmodell des GameMasters liefert einen Vorschlag, keine Wahrheit. Du spielst alle nicht besetzten Stakeholder und wählst Ereignisse nur aus dem Katalog des Szenarios — du erfindest keine neuen Weltfakten.\n\n");
        business_context(b, &mut out);
        if !b.business.stakeholders.is_empty() {
            out.push_str("## Von dir gespielte Stakeholder\n");
            for s in &b.business.stakeholders {
                let _ = writeln!(out, "- `{}` {}: {}", s.id, s.display, s.profile.trim());
            }
            out.push('\n');
        }
        if scenario.mode() == ScenarioMode::Business
            && b.teams.iter().any(|t| t.role == TeamRole::Market)
        {
            out.push_str("Das Marktteam ist Bewerter, nicht Konkurrent; seine Einschätzungen fließen als Kundensicht in deine Urteile ein.\n\n");
        }
    }
    seat_table(scenario, None, &mut out);
    out.push_str("## Was du weißt\nDu kennst, was in deinem Lagebild und Protokoll steht, einschließlich verdeckter Werte und geheimer Ziele, die dir zugänglich sind. Private Verhandlungen siehst du nur, wenn sie dir ausdrücklich gezeigt werden.\n\n");
    out.push_str("## Urteilen\n\
- Prüfe jeden Grund auf Plausibilität, Lage und Präzedenz. Gewichte: 0 = irrelevant oder unplausibel, 1 = trägt, 2 = stark und entscheidend.\n\
- Einen Kontext-Modifikator setzt du nur mit Begründung und nur für Faktoren, die kein Spieler genannt hat.\n\
- Nimm nicht vorweg, was die Würfel entscheiden sollen, und ersetze fehlendes Urteil nicht durch Würfel.\n\
- Vetos: Triviale, unrealistische oder spielbrechende Argumente („wir greifen an und gewinnen“) weist du mit öffentlicher Begründung zurück oder bewertest sie als riskant.\n\
- Effekte für Erfolg und Misserfolg legst du vor dem Wurf fest — klein und innerhalb der Schranken; Misserfolge erklärst du aus den Contras.\n\
- Konsistenz: Achte auf frühere erfolgreiche Argumente, laufende Effekte und Projekte; melde Widersprüche (`inconsistent_with`), statt sie still zu übergehen.\n\
- Geheime Argumente: `umpire_cons` statt `con_weights`, `public_rationale` bleibt `null`.\n\
- Erzählen: kurz und konkret, Ergebnis gemäß Würfelgrad, keine neuen Zustandsänderungen.\n\
- Spielende: Du wechselst in die Rolle des Seminarleiters — Muster, Wendepunkte, Alternativen und Bezug zum Zweck.\n\n");
    rules_section(scenario, SeatRole::Umpire, &mut out);
    out.push_str(CONFIDENTIALITY);
    out.push_str("\n- Was du nur als Umpire oder aus geheimen Argumenten weißt, darf in öffentlichen Texten weder zitiert noch angedeutet werden. Öffentliche Begründungen stützen sich nur auf öffentlich Bekanntes. Private Verhandlungen erwähnst du nie — nicht einmal, dass sie stattfanden.\n\n");
    contracts_section(
        &[
            ContractKind::BriefingAck,
            ContractKind::UmpireAdjudication,
            ContractKind::UmpireNarration,
            ContractKind::UmpireSynthesis,
        ],
        &mut out,
    );
    out
}

/// System-Prompt eines Sitzes.
///
/// Für [`SeatRole::Player`] und [`SeatRole::Market`] muss `seat` ein
/// Spieler-Sitz sein; nur dessen eigene Ziele und privates Briefing werden
/// aufgenommen. Fehlt der Sitz, entsteht ein allgemeiner Prompt ohne
/// Fraktionsdaten. Für [`SeatRole::Umpire`] wird `seat` ignoriert.
#[must_use]
pub fn system_prompt(role: SeatRole, loaded: &LoadedScenario, seat: Option<&Seat>) -> String {
    let player = match seat {
        Some(Seat::Player(p)) => Some(p.clone()),
        _ => None,
    };
    match (role, player) {
        (SeatRole::Umpire, _) => umpire_system(loaded),
        (SeatRole::Player, Some(p)) => player_system(loaded, &p, false),
        (SeatRole::Market, Some(p)) => player_system(loaded, &p, true),
        (SeatRole::Player | SeatRole::Market, None) => player_system(
            loaded,
            &PlayerId::new("unbekannt"),
            role == SeatRole::Market,
        ),
    }
}

// ---------------------------------------------------------------------------
// Turn-Prompts (nur aus der Projektion)
// ---------------------------------------------------------------------------

fn me_of(view: &SeatView, call: &ExpectedCall) -> Seat {
    match view.viewer() {
        Viewer::Seat(seat) => seat.clone(),
        Viewer::Observer => call.seat.clone(),
    }
}

fn is_me(me: &Seat, p: &PlayerId) -> bool {
    matches!(me, Seat::Player(q) if q == p)
}

fn who(me: &Seat, p: &PlayerId) -> String {
    if is_me(me, p) {
        format!("{p} (du)")
    } else {
        p.to_string()
    }
}

fn audience_tag(audience: &Audience, me: &Seat) -> String {
    match audience {
        Audience::Public => "öffentlich".to_owned(),
        Audience::Pair(a, b) => format!("privat {a}⇄{b}"),
        Audience::UmpireOnly => "nur Umpire".to_owned(),
        Audience::Seat(p) if is_me(me, p) => "nur du".to_owned(),
        Audience::Seat(p) => format!("nur {p}"),
        Audience::SeatAndUmpire(p) if is_me(me, p) => "du + Umpire".to_owned(),
        Audience::SeatAndUmpire(p) => format!("{p} + Umpire"),
        Audience::ObserverOnly => "Beobachter".to_owned(),
    }
}

fn outcome_label(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Success => "Erfolg",
        Outcome::Failure => "Misserfolg",
        Outcome::AutoSuccess => "Erfolg ohne Wurf",
        Outcome::Vetoed => "verworfen",
        Outcome::Forfeited => "kein Argument",
    }
}

fn grade_label(grade: Grade) -> &'static str {
    match grade {
        Grade::StrongSuccess => "strong_success",
        Grade::Success => "success",
        Grade::Failure => "failure",
        Grade::StrongFailure => "strong_failure",
    }
}

fn verdict_label(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Roll => "roll",
        Verdict::NoRoll => "no_roll",
        Verdict::Veto => "veto",
    }
}

fn numbered(items: &[String]) -> String {
    items
        .iter()
        .enumerate()
        .map(|(i, t)| format!("{}) {t}", i + 1))
        .collect::<Vec<_>>()
        .join(" ")
}

fn argument_line(
    id: &str,
    seat: &PlayerId,
    body: &crate::phases::ArgumentBody,
    me: &Seat,
) -> String {
    let mut line = format!(
        "Argument {id} von {}: {} | Gründe: {}",
        who(me, seat),
        body.action,
        numbered(&body.pros)
    );
    if !body.cites_negotiation.is_empty() {
        let _ = write!(
            line,
            " | zitiert Kanal {}",
            body.cites_negotiation.join(", ")
        );
    }
    if let Some(t) = &body.conflict_target {
        let _ = write!(line, " | Konflikt mit {t}");
    }
    if let Some(p) = &body.project {
        let _ = write!(line, " | Projekt {p}");
    }
    if body.use_fail_chit_if_failed {
        line.push_str(" | setzt bei Misserfolg einen Fail-Chit ein");
    }
    line
}

fn render_kind(kind: &EntryKind, me: &Seat) -> String {
    match kind {
        EntryKind::GameCreated { .. } => "Spiel eröffnet.".to_owned(),
        EntryKind::VarDeclared { var } => {
            format!(
                "Größe {} (`{}`) = {}",
                var.label,
                var.id,
                var.value.display()
            )
        }
        EntryKind::FactionBriefing {
            faction,
            name,
            briefing,
            public_goals,
            assets,
        } => {
            let mut s = format!("Fraktion {name} (`{faction}`): {}", briefing.trim());
            if !public_goals.is_empty() {
                let _ = write!(s, " | öffentliche Ziele: {}", public_goals.join("; "));
            }
            if !assets.is_empty() {
                let _ = write!(s, " | Mittel: {}", assets.join(", "));
            }
            s
        }
        EntryKind::SecretBriefing {
            faction,
            secret_goals,
            private_brief,
        } => {
            let mut s = format!("Vertrauliches Briefing {}", who(me, faction));
            if !secret_goals.is_empty() {
                let _ = write!(s, " | geheime Ziele: {}", secret_goals.join("; "));
            }
            if let Some(b) = private_brief {
                let _ = write!(s, " | privates Briefing: {b}");
            }
            s
        }
        EntryKind::PhaseEntered { phase } => format!("Phase: {}", phase.label()),
        EntryKind::Briefed { seat, intent } => {
            let seat = match seat {
                Seat::Player(p) => who(me, p),
                Seat::Umpire => "Umpire".to_owned(),
            };
            match intent {
                Some(i) => format!("{seat} bestätigt das Briefing; Absicht: {i}"),
                None => format!("{seat} bestätigt das Briefing."),
            }
        }
        EntryKind::ChannelOpened {
            channel,
            members,
            initiator,
            opening,
        } => format!(
            "Kanal {channel} ({}⇄{}) eröffnet von {}: „{opening}“",
            members[0],
            members[1],
            who(me, initiator)
        ),
        EntryKind::NegotiationPosted {
            channel,
            from,
            text,
            proposal,
            accept,
            decline,
        } => {
            let mut s = format!("[{channel}] {}: „{text}“", who(me, from));
            if let Some(p) = proposal {
                let _ = write!(s, " | Vorschlag: {p}");
            }
            if let Some(a) = accept {
                let _ = write!(s, " | nimmt an: {a}");
            }
            if *decline {
                s.push_str(" | lehnt ab");
            }
            s
        }
        EntryKind::NegotiationClosed => "Die Verhandlungsphase ist beendet.".to_owned(),
        EntryKind::ArgumentSealed {
            argument_id,
            seat,
            commitment,
        } => format!(
            "{} hat {argument_id} versiegelt eingereicht (sha {}).",
            who(me, seat),
            commitment.short()
        ),
        EntryKind::ArgumentRevealed {
            argument_id,
            seat,
            argument,
        } => argument_line(argument_id, seat, argument, me),
        EntryKind::SecretArgumentAnnounced {
            argument_id,
            secret_id,
            commitment,
            text,
            ..
        } => format!(
            "{text} ({argument_id}, Geheimnis {secret_id}, sha {})",
            commitment.short()
        ),
        EntryKind::PrivateNote {
            argument_id, text, ..
        } => format!("Private Notiz zu {argument_id}: {text}"),
        EntryKind::CountersSubmitted { seat, counters } => {
            let parts: Vec<String> = counters
                .iter()
                .filter(|c| !c.cons.is_empty())
                .map(|c| format!("gegen {}: {}", c.argument_id, numbered(&c.cons)))
                .collect();
            if parts.is_empty() {
                format!("{} bringt keine Contras vor.", who(me, seat))
            } else {
                format!("Contras von {}: {}", who(me, seat), parts.join(" | "))
            }
        }
        EntryKind::Forfeit { seat, phase, text } => {
            format!("{} passt ({}): {text}", who(me, seat), phase.label())
        }
        EntryKind::Adjudicated {
            argument_id,
            ruling,
            net,
            target,
            probability_pct,
        } => {
            let mut s = format!(
                "Internes Urteil {argument_id}: {}, Netto {net}, Ziel {}, {probability_pct} %",
                verdict_label(ruling.verdict),
                target.map_or_else(|| "–".to_owned(), |t| t.to_string())
            );
            if let Some(n) = &ruling.private_notes {
                let _ = write!(s, " | Notizen: {n}");
            }
            s
        }
        EntryKind::RulingPublished {
            argument_id,
            pro_weights,
            con_weights,
            context_modifier,
            net,
            target,
            probability_pct,
            rationale,
        } => {
            let cons: Vec<String> = con_weights
                .iter()
                .map(|(k, v)| format!("{k} {v:?}"))
                .collect();
            let mut s = format!(
                "Urteil {argument_id}: Pro {pro_weights:?}, Contra {{{}}}, Modifikator {context_modifier:+}, Netto {net}, Ziel {}, {probability_pct} %",
                cons.join(", "),
                target.map_or_else(|| "–".to_owned(), |t| t.to_string())
            );
            if let Some(r) = rationale {
                let _ = write!(s, " | Begründung: {r}");
            }
            s
        }
        EntryKind::DiceRolled { roll } => format!(
            "Wurf {} (Versuch {}): {:?} = {} gegen {} → {} ({})",
            roll.argument_id,
            roll.attempt + 1,
            roll.dice,
            roll.total,
            roll.target,
            if roll.success { "Erfolg" } else { "Misserfolg" },
            grade_label(roll.grade)
        ),
        EntryKind::ArgumentResolved {
            argument_id,
            seat,
            outcome,
            grade,
            ..
        } => {
            let grade = grade
                .map(|g| format!(" ({})", grade_label(g)))
                .unwrap_or_default();
            format!(
                "Ergebnis {argument_id} ({}): {}{grade}",
                who(me, seat),
                outcome_label(*outcome)
            )
        }
        EntryKind::Narrated { argument_id, text } => match argument_id {
            Some(id) => format!("Erzählung zu {id}: {text}"),
            None => format!("Rundenzusammenfassung: {text}"),
        },
        EntryKind::WorldDelta { var, from, to, .. } => {
            format!("`{var}`: {} → {}", from.display(), to.display())
        }
        EntryKind::FactAdded { text } => format!("Lage: {}", text.trim()),
        EntryKind::OngoingStarted { ongoing } => {
            format!("Fortwirkender Effekt `{}`: {}", ongoing.id, ongoing.text)
        }
        EntryKind::OngoingStopped { id } => format!("Fortwirkender Effekt `{id}` endet."),
        EntryKind::EffectRejected { argument_id, .. } => {
            format!("Ein Effekt zu {argument_id} wurde verworfen.")
        }
        EntryKind::SecretRevealed {
            secret_id,
            argument_id,
            seat,
            content,
            by,
            ..
        } => {
            let by = match by {
                RevealedBy::Trigger(t) => format!("Auslöser {t}"),
                RevealedBy::Owner => "durch den Eigentümer".to_owned(),
                RevealedBy::Facilitator => "durch den Facilitator".to_owned(),
                RevealedBy::GameEnd => "zum Spielende".to_owned(),
                RevealedBy::Effect(e) => format!("durch Effekt {e}"),
            };
            format!(
                "Geheimnis {secret_id} offengelegt ({by}): {}",
                argument_line(argument_id, seat, content, me)
            )
        }
        EntryKind::InjectApplied { text, .. } => format!("Ereignis: {}", text.trim()),
        EntryKind::LeakSuspect { .. } => "Interner Prüfvermerk.".to_owned(),
        EntryKind::StandingSet { order } => {
            let order: Vec<String> = order.iter().map(|p| who(me, p)).collect();
            format!("Einschätzung der Führenden: {}", order.join(" > "))
        }
        EntryKind::RoundClosed { .. } => "Runde abgeschlossen.".to_owned(),
        EntryKind::FacilitatorNote { command, detail } => {
            format!("Facilitator ({command}): {detail}")
        }
        EntryKind::GameEnded { reason } => format!("Spielende: {reason}"),
    }
}

fn render_entry(entry: &ViewEntry, me: &Seat) -> String {
    let mut line = format!(
        "#{} R{} [{}] {}",
        entry.n,
        entry.round,
        audience_tag(&entry.audience, me),
        render_kind(&entry.kind, me)
    );
    if let Some(p) = &entry.disclosed_by {
        let _ = write!(line, " (offengelegt von {})", who(me, p));
    }
    line
}

/// Aktuelle Runde und Phase laut Projektion (letzter Phasenwechsel).
fn cursor_of(view: &SeatView) -> (u32, Phase) {
    view.entries()
        .iter()
        .rev()
        .find_map(|e| match &e.kind {
            EntryKind::PhaseEntered { phase } => Some((e.round, *phase)),
            _ => None,
        })
        .unwrap_or((0, Phase::Setup))
}

/// Sichtbare Sitze aus den öffentlichen Briefings (ID → Name).
fn seats_of(view: &SeatView) -> BTreeMap<PlayerId, String> {
    let mut seats = BTreeMap::new();
    for e in view.entries() {
        if let EntryKind::FactionBriefing { faction, name, .. } = &e.kind {
            seats.insert(faction.clone(), name.clone());
        }
    }
    seats
}

/// Eigene Kanäle mit Partner und Eröffnungsrunde.
fn channels_of(view: &SeatView, me: &Seat) -> Vec<(String, String, u32)> {
    view.entries()
        .iter()
        .filter_map(|e| match &e.kind {
            EntryKind::ChannelOpened {
                channel, members, ..
            } => {
                let partner = match me {
                    Seat::Player(p) if &members[0] == p => members[1].to_string(),
                    Seat::Player(p) if &members[1] == p => members[0].to_string(),
                    _ => format!("{}⇄{}", members[0], members[1]),
                };
                Some((channel.clone(), partner, e.round))
            }
            _ => None,
        })
        .collect()
}

fn var_scope(visibility: &crate::scenario::VarVisibility, me: &Seat) -> &'static str {
    use crate::scenario::VarVisibility as V;
    match visibility {
        V::Public => "öffentlich",
        V::Umpire => "nur Umpire",
        V::Seat(_) | V::Seats(_) if matches!(me, Seat::Umpire) => "verdeckt",
        V::Seat(_) => "nur du",
        V::Seats(_) => "vertraulich",
    }
}

fn situation_section(view: &SeatView, me: &Seat, out: &mut String) {
    out.push_str("## Lagebild\n### Weltzustand\n");
    if view.world().is_empty() {
        out.push_str("- (keine Größen bekannt)\n");
    }
    for var in view.world().values() {
        // Doppelte Absicherung: die Projektion enthält nur Sichtbares.
        if !var_visible_to(&var.visibility, me) {
            continue;
        }
        let _ = writeln!(
            out,
            "- {} (`{}`): {} [{}]",
            var.label,
            var.id,
            var.value.display(),
            var_scope(&var.visibility, me)
        );
    }
    let situation: Vec<&str> = view
        .entries()
        .iter()
        .filter_map(|e| match &e.kind {
            EntryKind::FactAdded { text } if e.round == 0 => Some(text.trim()),
            _ => None,
        })
        .collect();
    if !situation.is_empty() {
        out.push_str("### Ausgangslage\n");
        for s in situation {
            let _ = writeln!(out, "{s}");
        }
    }
    out.push_str("### Fraktionen\n");
    for e in view.entries() {
        if matches!(
            e.kind,
            EntryKind::FactionBriefing { .. } | EntryKind::SecretBriefing { .. }
        ) {
            let _ = writeln!(
                out,
                "- [{}] {}",
                audience_tag(&e.audience, me),
                render_kind(&e.kind, me)
            );
        }
    }
    let mut ongoing: BTreeMap<String, String> = BTreeMap::new();
    for e in view.entries() {
        match &e.kind {
            EntryKind::OngoingStarted { ongoing: o } => {
                ongoing.insert(o.id.clone(), o.text.clone());
            }
            EntryKind::OngoingStopped { id } => {
                ongoing.remove(id);
            }
            _ => {}
        }
    }
    if !ongoing.is_empty() {
        out.push_str("### Laufende Effekte\n");
        for (id, text) in &ongoing {
            let _ = writeln!(out, "- `{id}`: {text}");
        }
    }
    let projects: Vec<String> = view
        .world()
        .values()
        .filter(|v| matches!(v.value, VarValue::Project { .. }))
        .map(|v| format!("`{}` {} ({})", v.id, v.label, v.value.display()))
        .collect();
    if !projects.is_empty() {
        let _ = writeln!(out, "### Vorhaben\n- {}", projects.join("\n- "));
    }
    if matches!(me, Seat::Player(_)) {
        let channels = channels_of(view, me);
        if !channels.is_empty() {
            out.push_str("### Deine privaten Kanäle\n");
            for (id, partner, round) in channels {
                let _ = writeln!(out, "- {id} mit {partner} (seit Runde {round})");
            }
        }
    }
    out.push('\n');
}

fn round_arguments(view: &SeatView, round: u32) -> Vec<(String, PlayerId, bool)> {
    let secret: BTreeSet<&str> = view
        .entries()
        .iter()
        .filter_map(|e| match &e.kind {
            EntryKind::SecretArgumentAnnounced { argument_id, .. } if e.round == round => {
                Some(argument_id.as_str())
            }
            _ => None,
        })
        .collect();
    let mut out: Vec<(String, PlayerId, bool)> = Vec::new();
    for e in view.entries() {
        if e.round != round {
            continue;
        }
        let (id, seat) = match &e.kind {
            EntryKind::ArgumentRevealed {
                argument_id, seat, ..
            }
            | EntryKind::SecretArgumentAnnounced {
                argument_id, seat, ..
            } => (argument_id, seat),
            _ => continue,
        };
        if out.iter().any(|(i, _, _)| i == id) {
            continue;
        }
        let is_secret = secret.contains(id.as_str()) || !e.audience.is_public();
        out.push((id.clone(), seat.clone(), is_secret));
    }
    out
}

fn task_section(view: &SeatView, call: &ExpectedCall, me: &Seat, out: &mut String) {
    let (round, phase) = cursor_of(view);
    let seats = seats_of(view);
    let others: Vec<String> = seats
        .iter()
        .filter(|(p, _)| !is_me(me, p))
        .map(|(p, n)| format!("`{p}` ({n})"))
        .collect();
    let kind = call.contract;
    let _ = writeln!(
        out,
        "## Auftrag — Runde {round}, Phase {} (Vertrag `{}`)",
        phase.label(),
        contract_name(kind)
    );
    match kind {
        ContractKind::BriefingAck => out.push_str(
            "Bestätige das Lagebild. Formuliere in `intent` knapp deine Absicht für diese Runde; sie bleibt privat.\n",
        ),
        ContractKind::NegotiationRequest => {
            out.push_str("Wähle, mit wem du in dieser Runde privat sprechen willst, und formuliere je Gesprächswunsch eine Eröffnung. Eine leere Liste bedeutet: keine Gespräche.\n");
            let _ = writeln!(out, "Mögliche Gesprächspartner: {}", others.join(", "));
        }
        ContractKind::NegotiationMessage => {
            let channels: Vec<(String, String, u32)> = channels_of(view, me)
                .into_iter()
                .filter(|(_, _, r)| *r == round)
                .collect();
            out.push_str("Antworte in deinen offenen Kanälen dieser Runde (höchstens eine Nachricht je Kanal). Du kannst Vorschläge machen, annehmen oder ablehnen.\n");
            if channels.is_empty() {
                out.push_str("Offene Kanäle: keine — antworte mit leerer `messages`-Liste.\n");
            } else {
                let list: Vec<String> = channels
                    .iter()
                    .map(|(id, partner, _)| format!("`{id}` mit {partner}"))
                    .collect();
                let _ = writeln!(out, "Offene Kanäle: {}", list.join(", "));
            }
        }
        ContractKind::PlayerArgument | ContractKind::FinalArgument => {
            if kind == ContractKind::FinalArgument {
                out.push_str("Gib dein Schlussargument ab: wie das Spiel für deine Fraktion ausgeht, mit genau 3 Gründen.\n");
            } else {
                out.push_str("Reiche dein Argument für diese Runde ein: eine konkrete Aktion deiner Fraktion mit Gründen. Die Einreichung ist versiegelt, bis alle abgegeben haben.\n");
            }
            let _ = writeln!(out, "Mögliche Konfliktgegner (`conflict_target`): {}", others.join(", "));
            let channels: Vec<String> = channels_of(view, me)
                .into_iter()
                .map(|(id, partner, _)| format!("`{id}` mit {partner}"))
                .collect();
            if channels.is_empty() {
                out.push_str("Zitierbare eigene Kanäle (`cites_negotiation`): keine\n");
            } else {
                let _ = writeln!(
                    out,
                    "Zitierbare eigene Kanäle (`cites_negotiation`): {}",
                    channels.join(", ")
                );
            }
            let projects: Vec<String> = view
                .world()
                .values()
                .filter(|v| matches!(v.value, VarValue::Project { .. }))
                .map(|v| format!("`{}`", v.id))
                .collect();
            if !projects.is_empty() {
                let _ = writeln!(out, "Vorhaben (`project`): {}", projects.join(", "));
            }
            if let Seat::Player(p) = me {
                let used = view
                    .entries()
                    .iter()
                    .filter(|e| {
                        matches!(&e.kind, EntryKind::SecretArgumentAnnounced { seat, .. } if seat == p)
                    })
                    .count();
                let _ = writeln!(out, "Bisher eingesetzte geheime Argumente: {used}");
            }
        }
        ContractKind::CounterArgument => {
            let targets: Vec<String> = round_arguments(view, round)
                .into_iter()
                .filter(|(_, seat, secret)| !secret && !is_me(me, seat))
                .map(|(id, seat, _)| format!("`{id}` ({seat})"))
                .collect();
            out.push_str("Nenne sachliche Gründe, warum fremde Argumente dieser Runde scheitern könnten (höchstens 3 je Argument). Lass Argumente aus, gegen die du nichts vorbringen willst.\n");
            if targets.is_empty() {
                out.push_str("Angreifbare Argumente: keine — antworte mit leerer `counters`-Liste.\n");
            } else {
                let _ = writeln!(out, "Angreifbare Argumente: {}", targets.join(", "));
            }
        }
        ContractKind::UmpireAdjudication => {
            let args = round_arguments(view, round);
            out.push_str("Beurteile jedes Argument dieser Runde (Aufruf A, vor dem Wurf): Gewichte, Kontext-Modifikator, Urteil und Effekte für Erfolg und Misserfolg. Setze zusätzlich `standing` (Führende zuerst).\n");
            if args.is_empty() {
                out.push_str("Argumente: keine\n");
            } else {
                let list: Vec<String> = args
                    .iter()
                    .map(|(id, seat, secret)| {
                        if *secret {
                            format!("`{id}` ({seat}, geheim)")
                        } else {
                            format!("`{id}` ({seat})")
                        }
                    })
                    .collect();
                let _ = writeln!(out, "Argumente: {}", list.join(", "));
            }
            let ids: Vec<String> = seats.keys().map(|p| format!("`{p}`")).collect();
            let _ = writeln!(out, "Sitz-IDs (`con_weights`, `standing`): {}", ids.join(", "));
            let vars: Vec<String> = view.world().keys().map(|v| format!("`{v}`")).collect();
            if !vars.is_empty() {
                let _ = writeln!(out, "Weltgrößen für Effekte: {}", vars.join(", "));
            }
        }
        ContractKind::UmpireNarration => {
            out.push_str("Erzähle die Ergebnisse dieser Runde (Aufruf B, nach dem Wurf): kurz, konkret, gemäß Ergebnis und Grad, ohne neue Zustandsänderungen. Die Audience einer Erzählung ist nie weiter als die des Arguments; geheime Argumente erzählst du nur `seat+umpire:<id>`.\n");
            let results: Vec<String> = view
                .entries()
                .iter()
                .filter(|e| e.round == round)
                .filter_map(|e| match &e.kind {
                    EntryKind::ArgumentResolved {
                        argument_id,
                        outcome,
                        grade,
                        ..
                    } => Some(format!(
                        "`{argument_id}` [{}] {}{}",
                        audience_tag(&e.audience, me),
                        outcome_label(*outcome),
                        grade.map(|g| format!(" ({})", grade_label(g))).unwrap_or_default()
                    )),
                    _ => None,
                })
                .collect();
            if results.is_empty() {
                out.push_str("Ergebnisse: keine\n");
            } else {
                let _ = writeln!(out, "Ergebnisse: {}", results.join(", "));
            }
        }
        ContractKind::PlayerDebrief => out.push_str(
            "Das Spiel ist beendet. Reflektiere knapp aus Sicht deiner Fraktion: was du wolltest, was geschah, was dich überrascht hat und was du anders machen würdest.\n",
        ),
        ContractKind::UmpireSynthesis => {
            out.push_str("Das Spiel ist beendet. Du bist jetzt Seminarleiter: Benenne zwei bis drei Wendepunkte, schlage Runden für alternative Verläufe vor, prüfe die Plausibilität und bewerte die Zielerreichung je Fraktion (0–3, Ziele wörtlich).\n");
            let ids: Vec<String> = seats.keys().map(|p| format!("`{p}`")).collect();
            let _ = writeln!(out, "Fraktionen: {}", ids.join(", "));
        }
    }
    let _ = writeln!(out, "Schema: {}\n", contract_schema(kind));
    out.push_str(&final_instruction(kind));
}

/// User-Prompt eines Aufrufs — ausschließlich aus der Projektion `view`.
///
/// `since` ist die lokale Nummer des letzten Eintrags, den der Sitz bereits
/// gesehen hat (0 = alles); gesendet wird nur das Delta
/// ([`SeatView::entries_since`]). Mit `situation_report` wird zusätzlich ein
/// vollständiges Lagebild (Weltzustand, Fraktionen, laufende Effekte, eigene
/// Kanäle) vorangestellt. Der Prompt endet immer mit
/// „Antworte ausschließlich mit JSON nach Vertrag `…`.“
#[must_use]
pub fn turn_prompt(
    view: &SeatView,
    since: usize,
    call: &ExpectedCall,
    situation_report: bool,
) -> String {
    let me = me_of(view, call);
    let (round, phase) = cursor_of(view);
    let mut out = String::new();
    let _ = writeln!(out, "# Runde {round} — Phase {}\n", phase.label());
    if situation_report {
        situation_section(view, &me, &mut out);
    }
    let since = u32::try_from(since).unwrap_or(u32::MAX);
    let delta = view.entries_since(since);
    if since == 0 {
        out.push_str("## Protokoll (vollständig, soweit für dich sichtbar)\n");
    } else {
        out.push_str("## Neu seit deinem letzten Zug\n");
    }
    if delta.is_empty() {
        out.push_str("- (nichts Neues)\n");
    }
    for entry in delta {
        let _ = writeln!(out, "- {}", render_entry(entry, &me));
    }
    out.push('\n');
    task_section(view, call, &me, &mut out);
    out
}

/// Korrektur-Prompt nach einer ungültigen Antwort.
#[must_use]
pub fn repair_prompt(error: &str, contract: ContractKind) -> String {
    format!(
        "Deine letzte Antwort war nach Vertrag `{name}` ungültig:\n{error}\n\n\
         Korrigiere genau diese Punkte und sende die vollständige Antwort erneut — ein einziges JSON-Objekt, ohne Codezaun und ohne Erläuterungen. Private Informationen bleiben in privaten Feldern.\n\
         Schema: {schema}\n\n{last}",
        name = contract_name(contract),
        error = error.trim(),
        schema = contract_schema(contract),
        last = final_instruction(contract)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::load_scenario;
    use crate::test_support::{
        CLOUD, Fixture, KARST, NORD_REPLY, RAT_OPENING, scripted_game, scripted_game_with,
    };
    use crate::visibility::{VisibilityCfg, project};

    const ORDER: [&str; 4] = ["rat", "gilde", "nord", "mission"];

    const ALL_CONTRACTS: [ContractKind; 10] = [
        ContractKind::BriefingAck,
        ContractKind::NegotiationRequest,
        ContractKind::NegotiationMessage,
        ContractKind::PlayerArgument,
        ContractKind::CounterArgument,
        ContractKind::UmpireAdjudication,
        ContractKind::UmpireNarration,
        ContractKind::FinalArgument,
        ContractKind::PlayerDebrief,
        ContractKind::UmpireSynthesis,
    ];

    fn call(seat: Seat, contract: ContractKind) -> ExpectedCall {
        ExpectedCall { seat, contract }
    }

    fn view_for(loaded: &LoadedScenario, log: &crate::state::GameLog, seat: &Seat) -> SeatView {
        let cfg = VisibilityCfg::from_settings(loaded.scenario.visibility());
        project(&log.journal, seat, &cfg)
    }

    #[test]
    fn turn_prompt_is_unaffected_by_foreign_negotiation() -> Fixture<()> {
        let (loaded, with) = scripted_game_with(&[7u8; 32], &ORDER, true)?;
        let (_, without) = scripted_game_with(&[7u8; 32], &ORDER, false)?;
        let gilde = Seat::player("gilde");
        let v_with = view_for(&loaded, &with, &gilde);
        let v_without = view_for(&loaded, &without, &gilde);
        let own_secrets: Vec<String> = loaded
            .scenario
            .factions()
            .into_iter()
            .filter(|f| f.id.as_str() != "gilde")
            .flat_map(|f| f.secret_goals)
            .collect();
        assert!(
            !own_secrets.is_empty(),
            "Fixture braucht fremde Geheimziele"
        );

        for contract in ALL_CONTRACTS {
            for (since, report) in [(0usize, true), (0, false), (5, true), (12, false)] {
                let c = call(gilde.clone(), contract);
                let a = turn_prompt(&v_with, since, &c, report);
                let b = turn_prompt(&v_without, since, &c, report);
                assert_eq!(a, b, "Prompt hängt von fremder Verhandlung ab");
                for marker in [
                    RAT_OPENING,
                    NORD_REPLY,
                    "neg-",
                    "Tankschiffe gegen Landerecht",
                ] {
                    assert!(!a.contains(marker), "fremder Marker `{marker}` im Prompt");
                }
                for secret in &own_secrets {
                    assert!(!a.contains(secret.as_str()), "fremdes Geheimziel im Prompt");
                }
                for hidden in ["Nordreich-Agenten im Hafen", "Sabotagerisiko Anlage"] {
                    assert!(!a.contains(hidden), "verdeckte Größe `{hidden}` im Prompt");
                }
            }
        }

        // Gegenprobe: Die Beteiligten sehen ihre Verhandlung.
        let nord = Seat::player("nord");
        let nord_prompt = turn_prompt(
            &view_for(&loaded, &with, &nord),
            0,
            &call(nord.clone(), ContractKind::PlayerArgument),
            true,
        );
        assert!(nord_prompt.contains(NORD_REPLY));
        assert!(nord_prompt.contains(RAT_OPENING));

        // Auch der System-Prompt enthält keine fremden Geheimnisse.
        let sys = system_prompt(SeatRole::Player, &loaded, Some(&gilde));
        for secret in &own_secrets {
            assert!(!sys.contains(secret.as_str()));
        }
        Ok(())
    }

    #[test]
    fn prompts_name_the_expected_contract() -> Fixture<()> {
        let (loaded, log) = scripted_game(&[3u8; 32], &ORDER)?;
        for (seat, kinds) in [
            (
                Seat::player("rat"),
                vec![
                    ContractKind::BriefingAck,
                    ContractKind::NegotiationRequest,
                    ContractKind::NegotiationMessage,
                    ContractKind::PlayerArgument,
                    ContractKind::CounterArgument,
                    ContractKind::FinalArgument,
                    ContractKind::PlayerDebrief,
                ],
            ),
            (
                Seat::Umpire,
                vec![
                    ContractKind::BriefingAck,
                    ContractKind::UmpireAdjudication,
                    ContractKind::UmpireNarration,
                    ContractKind::UmpireSynthesis,
                ],
            ),
        ] {
            let view = view_for(&loaded, &log, &seat);
            for kind in kinds {
                let prompt = turn_prompt(&view, 3, &call(seat.clone(), kind), false);
                let name = contract_name(kind);
                let expected = format!("Antworte ausschließlich mit JSON nach Vertrag `{name}`.");
                assert!(prompt.ends_with(&expected), "{name}: {prompt}");
                assert!(prompt.contains(contract_schema(kind)));
                for other in ALL_CONTRACTS.iter().filter(|k| **k != kind) {
                    assert!(
                        !prompt.contains(&format!("nach Vertrag `{}`", contract_name(*other))),
                        "falscher Vertrag im Prompt"
                    );
                }
                let repair = repair_prompt("pros: 0 Einträge", kind);
                assert!(repair.contains("pros: 0 Einträge"));
                assert!(repair.ends_with(&expected));
            }
        }
        // Serde-Namen stimmen mit ContractKind überein.
        for kind in ALL_CONTRACTS {
            let json = serde_json::to_string(&kind)?;
            assert_eq!(json.trim_matches('"'), contract_name(kind));
        }
        Ok(())
    }

    #[test]
    fn umpire_system_prompt_differs_from_player() -> Fixture<()> {
        let loaded = load_scenario(KARST)?;
        let player = system_prompt(SeatRole::Player, &loaded, Some(&Seat::player("rat")));
        let umpire = system_prompt(SeatRole::Umpire, &loaded, Some(&Seat::Umpire));
        assert_ne!(player, umpire);
        assert!(umpire.contains("Schiedsrichter"));
        assert!(umpire.contains("`umpire_adjudication`"));
        assert!(!umpire.contains("Du spielst die Fraktion"));
        assert!(player.contains("Du spielst die Fraktion „Inselrat“"));
        assert!(player.contains("`player_argument`"));
        assert!(!player.contains("`umpire_adjudication`"));
        assert!(player.contains("Die Gilde als Schuldige der Krise dastehen lassen"));
        assert!(player.contains("Vertraulichkeit"));
        assert!(player.contains("2W6"));

        let business = load_scenario(CLOUD)?;
        let market = system_prompt(SeatRole::Market, &business, Some(&Seat::player("market")));
        assert_eq!(
            SeatRole::for_seat(&business, &Seat::player("market")),
            SeatRole::Market
        );
        assert_eq!(
            SeatRole::for_seat(&business, &Seat::player("company")),
            SeatRole::Player
        );
        assert!(market.contains("Bewerter, nicht Konkurrent"));
        assert!(market.contains("gamebook/market-panel.md"));
        assert!(!market.contains("gamebook/nordcloud.md"));
        assert!(market.contains("KQ1"));
        let competitor = system_prompt(
            SeatRole::Player,
            &business,
            Some(&Seat::player("competitor_a")),
        );
        assert!(competitor.contains("Be the enemy"));
        assert!(!competitor.contains("gamebook/market-panel.md"));
        assert_ne!(market, competitor);
        Ok(())
    }

    #[test]
    fn situation_report_lists_only_visible_world_vars() -> Fixture<()> {
        let (loaded, log) = scripted_game(&[5u8; 32], &ORDER)?;
        let gilde = Seat::player("gilde");
        let view = view_for(&loaded, &log, &gilde);
        let c = call(gilde, ContractKind::PlayerArgument);
        let n = view.entries().len();

        let report = turn_prompt(&view, n, &c, true);
        assert!(report.contains("## Lagebild"));
        assert!(report.contains("Öffentliche Ordnung (`stability`)"));
        assert!(report.contains("Schmuggelnetz der Gilde (`smuggling_net`)"));
        assert!(!report.contains("north_agents"));
        assert!(!report.contains("plant_sabotage_risk"));
        assert!(!report.contains("reservoir_altmark"));

        let plain = turn_prompt(&view, n, &c, false);
        assert!(!plain.contains("## Lagebild"));
        assert!(plain.contains("(nichts Neues)"));

        let umpire = view_for(&loaded, &log, &Seat::Umpire);
        let u = turn_prompt(
            &umpire,
            umpire.entries().len(),
            &call(Seat::Umpire, ContractKind::UmpireSynthesis),
            true,
        );
        for id in [
            "smuggling_net",
            "north_agents",
            "plant_sabotage_risk",
            "stability",
        ] {
            assert!(
                u.contains(&format!("(`{id}`)")),
                "Umpire sieht `{id}` nicht"
            );
        }
        Ok(())
    }
}
