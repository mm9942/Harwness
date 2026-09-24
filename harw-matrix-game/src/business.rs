//! Business-Modus: Geschäftsregeln (`[[business.rules]]`) und Marktmodell
//! (`[business.market_model]`, `kind = "logit_share"`) —
//! `docs/design/wargaming-and-analysis.md` §1.6 (Runde 7, Teil M5).
//!
//! # Verantwortungsbereich
//! - [`evaluate_predicate`]: kleiner, deterministischer Ausdrucksauswerter für
//!   `when`-Prädikate (`cash < 0`, `share(segment) > 0.40 && action.kind ==
//!   'acquisition'`, `share('competitor_a','sme') > 0.25`).
//! - [`RuleEffect`] / [`rule_hits`]: welche Regeln für ein Argument eines
//!   Teams greifen; [`restriction_for`] liefert die Sperre einer Aktionsart
//!   (`restrict_actions:[invest,acquisition]`), die die Adjudikation als
//!   Veto verbucht.
//! - [`action_kind`]: Aktionsart eines Arguments — ausdrücklich über das
//!   Präfix `[art]` (so fordert es der Business-Prompt), sonst über
//!   Stichwörter.
//! - [`apply_market_model`]: Logit-Marktanteile je Segment nach der
//!   Adjudikation einer Runde, journalisiert als `WorldDelta` mit Ursache
//!   [`MARKET_MODEL_CAUSE`].
//!
//! # Marktmodell
//! Je Segment und Team entsteht ein Score aus vier Komponenten im Bereich
//! −1..1 (Gewichte aus `weights`):
//! - `market_score`: Variable `<team>.market_score[.<segment>]` (Skala
//!   0..10, 5 = neutral); fehlt sie, der Rundenerfolg des Teams
//!   (Erfolg +1, Misserfolg −1, Veto/Verzicht −0,5; Mittelwert).
//! - `price_position`: Variable `<team>.price_position[.<segment>]` (0..10),
//!   gewichtet zusätzlich mit der Preissensitivität des Segments.
//! - `sales_invest`: Variable `<team>.sales_invest` (MEUR) mit abnehmendem
//!   Grenznutzen `1 − exp(−x / Sättigung)`.
//! - `product_fit`: Variable `<team>.product_fit[.<segment>]` (0..10).
//!
//! Neuer Anteil: `alt · exp(β · Score)` (Anker mindestens 0,01, damit ein
//! Team ohne Anteil einsteigen kann), normiert auf die bisherige Summe der
//! Team-Anteile des Segments — gleiche Scores lassen die Anteile also
//! unverändert, der Rest des Markts (nicht modellierte Anbieter) bleibt
//! konstant.
//!
//! # Determinismus
//! Alle Funktionen sind rein bzw. schreiben nur über [`GameLog::record`];
//! die Ergebnisse stehen im Journal und werden beim Replay nur angewendet.
//!
//! # Grenzen
//! `share(segment)` ohne Anführungszeichen wertet das **größte** Segment
//! des Teams aus (existenzielle Lesart). Unbekannte Bezeichner machen ein
//! Prädikat unauswertbar; die Regel greift dann nicht ([`RuleCheck::errors`]
//! nennt den Grund).

use std::collections::BTreeMap;

use jiff::Timestamp;

use crate::dice::Outcome;
use crate::error::MatrixResult;
use crate::scenario::{BusinessScenario, MarketModel, Scenario};
use crate::state::{Audience, EntryKind, GameEntry, GameLog, GameState, PlayerId, VarValue};

/// Ursache der Marktmodell-Deltas im Journal.
pub const MARKET_MODEL_CAUSE: &str = "market_model";

/// Befehlskennung der Regel-Vermerke im Journal (`FacilitatorNote`).
pub const RULE_NOTE_COMMAND: &str = "business_rule";

/// Vorgabe der Sättigung, wenn das Szenario keine nennt (MEUR).
const DEFAULT_SATURATION_MEUR: f64 = 10.0;

/// Kleinster Anker eines Anteils im Logit-Modell.
const SHARE_ANCHOR: f64 = 0.01;

/// Rundung der Anteile (vier Nachkommastellen).
const SHARE_SCALE: f64 = 10_000.0;

// ---------------------------------------------------------------------------
// Ausdrücke
// ---------------------------------------------------------------------------

/// Wert eines ausgewerteten Ausdrucks.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Zahl.
    Num(f64),
    /// Zeichenkette.
    Str(String),
    /// Wahrheitswert.
    Bool(bool),
}

/// Kontext der Prädikat-Auswertung: Zustand, Team und optional die
/// Aktionsart des geprüften Arguments.
#[derive(Debug, Clone)]
pub struct PredicateContext<'a> {
    /// Spielzustand.
    pub state: &'a GameState,
    /// Team, für das unqualifizierte Namen (`cash`, `share(segment)`) gelten.
    pub team: Option<&'a PlayerId>,
    /// Aktionsart des Arguments (`action.kind`).
    pub action_kind: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Num(f64),
    Str(String),
    Ident(String),
    Op(&'static str),
    LParen,
    RParen,
    Comma,
}

fn tokenize(expr: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = expr.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let next = chars.get(i + 1).copied();
        let two = |a: char, b: char| c == a && next == Some(b);
        let op: Option<(&'static str, usize)> = if two('&', '&') {
            Some(("&&", 2))
        } else if two('|', '|') {
            Some(("||", 2))
        } else if two('=', '=') {
            Some(("==", 2))
        } else if two('!', '=') {
            Some(("!=", 2))
        } else if two('<', '=') {
            Some(("<=", 2))
        } else if two('>', '=') {
            Some((">=", 2))
        } else if c == '<' {
            Some(("<", 1))
        } else if c == '>' {
            Some((">", 1))
        } else if c == '!' {
            Some(("!", 1))
        } else {
            // `-` gilt nur als Vorzeichen einer Zahl (Zweig unten).
            None
        };
        if let Some((op, len)) = op {
            tokens.push(Token::Op(op));
            i += len;
            continue;
        }
        match c {
            '(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            ',' => {
                tokens.push(Token::Comma);
                i += 1;
            }
            '\'' | '"' => {
                let quote = c;
                let mut text = String::new();
                i += 1;
                loop {
                    match chars.get(i) {
                        Some(&ch) if ch == quote => {
                            i += 1;
                            break;
                        }
                        Some(&ch) => {
                            text.push(ch);
                            i += 1;
                        }
                        None => return Err(format!("offene Zeichenkette in `{expr}`")),
                    }
                }
                tokens.push(Token::Str(text));
            }
            c if c.is_ascii_digit() || c == '-' || c == '.' => {
                let start = i;
                i += 1;
                while chars
                    .get(i)
                    .is_some_and(|ch| ch.is_ascii_digit() || *ch == '.')
                {
                    i += 1;
                }
                let text: String = chars.get(start..i).unwrap_or_default().iter().collect();
                let value = text
                    .parse::<f64>()
                    .map_err(|_| format!("`{text}` ist keine Zahl"))?;
                tokens.push(Token::Num(value));
            }
            c if c.is_alphanumeric() || c == '_' => {
                let start = i;
                while chars
                    .get(i)
                    .is_some_and(|ch| ch.is_alphanumeric() || *ch == '_' || *ch == '.')
                {
                    i += 1;
                }
                tokens.push(Token::Ident(
                    chars.get(start..i).unwrap_or_default().iter().collect(),
                ));
            }
            other => return Err(format!("unerwartetes Zeichen `{other}` in `{expr}`")),
        }
    }
    Ok(tokens)
}

struct Parser<'t, 'c> {
    tokens: &'t [Token],
    pos: usize,
    ctx: &'c PredicateContext<'c>,
}

impl Parser<'_, '_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn bump(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.pos).cloned();
        self.pos += 1;
        token
    }

    fn eat_op(&mut self, op: &str) -> bool {
        if matches!(self.peek(), Some(Token::Op(o)) if *o == op) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn or(&mut self) -> Result<Value, String> {
        let mut left = self.and()?;
        while self.eat_op("||") {
            let right = self.and()?;
            left = Value::Bool(truthy(&left)? || truthy(&right)?);
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Value, String> {
        let mut left = self.cmp()?;
        while self.eat_op("&&") {
            let right = self.cmp()?;
            left = Value::Bool(truthy(&left)? && truthy(&right)?);
        }
        Ok(left)
    }

    fn cmp(&mut self) -> Result<Value, String> {
        let left = self.unary()?;
        let op = match self.peek() {
            Some(Token::Op(op)) if matches!(*op, "<" | "<=" | ">" | ">=" | "==" | "!=") => *op,
            _ => return Ok(left),
        };
        self.pos += 1;
        let right = self.unary()?;
        compare(&left, op, &right).map(Value::Bool)
    }

    fn unary(&mut self) -> Result<Value, String> {
        if self.eat_op("!") {
            let inner = self.unary()?;
            return Ok(Value::Bool(!truthy(&inner)?));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Value, String> {
        match self.bump() {
            Some(Token::Num(n)) => Ok(Value::Num(n)),
            Some(Token::Str(s)) => Ok(Value::Str(s)),
            Some(Token::LParen) => {
                let inner = self.or()?;
                match self.bump() {
                    Some(Token::RParen) => Ok(inner),
                    _ => Err("`)` erwartet".to_owned()),
                }
            }
            Some(Token::Ident(name)) => {
                if matches!(self.peek(), Some(Token::LParen)) {
                    self.pos += 1;
                    let mut args = Vec::new();
                    if matches!(self.peek(), Some(Token::RParen)) {
                        self.pos += 1;
                    } else {
                        loop {
                            args.push(self.call_arg()?);
                            match self.bump() {
                                Some(Token::Comma) => {}
                                Some(Token::RParen) => break,
                                _ => return Err(format!("`,` oder `)` in `{name}(…)` erwartet")),
                            }
                        }
                    }
                    self.call(&name, &args)
                } else {
                    self.ident(&name)
                }
            }
            other => Err(format!("Ausdruck erwartet, gefunden: {other:?}")),
        }
    }

    /// Funktionsargument: Zeichenkette, Zahl oder nackter Bezeichner (als
    /// Platzhalter, z. B. `segment`).
    fn call_arg(&mut self) -> Result<CallArg, String> {
        match self.bump() {
            Some(Token::Str(s)) => Ok(CallArg::Literal(s)),
            Some(Token::Ident(_)) => Ok(CallArg::Placeholder),
            Some(Token::Num(n)) => Ok(CallArg::Literal(n.to_string())),
            other => Err(format!("Funktionsargument erwartet, gefunden: {other:?}")),
        }
    }

    fn call(&self, name: &str, args: &[CallArg]) -> Result<Value, String> {
        match (name, args) {
            ("share", [CallArg::Literal(team), CallArg::Literal(segment)]) => {
                number_var(self.ctx.state, &format!("{team}.share.{segment}")).map(Value::Num)
            }
            ("share", [CallArg::Literal(segment)]) => {
                let team = self.team()?;
                number_var(self.ctx.state, &format!("{team}.share.{segment}")).map(Value::Num)
            }
            ("share", [CallArg::Placeholder]) => {
                let team = self.team()?;
                let prefix = format!("{team}.share.");
                self.ctx
                    .state
                    .vars
                    .iter()
                    .filter(|(id, _)| id.starts_with(&prefix))
                    .filter_map(|(_, var)| numeric(&var.value))
                    .fold(None, |best: Option<f64>, v| {
                        Some(best.map_or(v, |b| b.max(v)))
                    })
                    .map(Value::Num)
                    .ok_or_else(|| format!("Team `{team}` hat keine Marktanteile"))
            }
            ("var", [CallArg::Literal(id)]) => var_value(self.ctx.state, id),
            _ => Err(format!(
                "unbekannte Funktion `{name}` mit {} Argument(en)",
                args.len()
            )),
        }
    }

    fn team(&self) -> Result<&PlayerId, String> {
        self.ctx
            .team
            .ok_or_else(|| "kein Team im Kontext (unqualifizierter Name)".to_owned())
    }

    fn ident(&self, name: &str) -> Result<Value, String> {
        match name {
            "true" => return Ok(Value::Bool(true)),
            "false" => return Ok(Value::Bool(false)),
            "action.kind" => {
                return Ok(Value::Str(
                    self.ctx.action_kind.unwrap_or_default().to_owned(),
                ));
            }
            "round" | "move" => return Ok(Value::Num(f64::from(self.ctx.state.round))),
            _ => {}
        }
        if let Some(team) = self.ctx.team {
            let own = format!("{team}.{name}");
            if self.ctx.state.vars.contains_key(&own) {
                return var_value(self.ctx.state, &own);
            }
        }
        var_value(self.ctx.state, name)
    }
}

#[derive(Debug, Clone)]
enum CallArg {
    Literal(String),
    /// Bezeichner ohne Wert (z. B. `seg`) – steht für „jedes Segment“.
    Placeholder,
}

fn numeric(value: &VarValue) -> Option<f64> {
    match value {
        VarValue::Number { value } => Some(*value),
        VarValue::Track { value, .. } => Some(f64::from(*value)),
        VarValue::Project { progress, .. } => Some(f64::from(*progress)),
        VarValue::State { .. } | VarValue::Object { .. } => None,
    }
}

fn number_var(state: &GameState, id: &str) -> Result<f64, String> {
    state
        .vars
        .get(id)
        .and_then(|var| numeric(&var.value))
        .ok_or_else(|| format!("unbekannte oder nicht-numerische Variable `{id}`"))
}

fn var_value(state: &GameState, id: &str) -> Result<Value, String> {
    let var = state
        .vars
        .get(id)
        .ok_or_else(|| format!("unbekannter Bezeichner `{id}`"))?;
    Ok(match &var.value {
        VarValue::State { value, .. } => Value::Str(value.clone()),
        other => Value::Num(numeric(other).unwrap_or_default()),
    })
}

fn truthy(value: &Value) -> Result<bool, String> {
    match value {
        Value::Bool(b) => Ok(*b),
        other => Err(format!("Wahrheitswert erwartet, gefunden: {other:?}")),
    }
}

fn compare(left: &Value, op: &str, right: &Value) -> Result<bool, String> {
    match (left, right) {
        (Value::Num(a), Value::Num(b)) => Ok(match op {
            "<" => a < b,
            "<=" => a <= b,
            ">" => a > b,
            ">=" => a >= b,
            "==" => (a - b).abs() < f64::EPSILON,
            _ => (a - b).abs() >= f64::EPSILON,
        }),
        (Value::Str(a), Value::Str(b)) => match op {
            "==" => Ok(a == b),
            "!=" => Ok(a != b),
            _ => Err(format!("`{op}` ist für Zeichenketten nicht definiert")),
        },
        (Value::Bool(a), Value::Bool(b)) => match op {
            "==" => Ok(a == b),
            "!=" => Ok(a != b),
            _ => Err(format!("`{op}` ist für Wahrheitswerte nicht definiert")),
        },
        _ => Err(format!("`{op}` vergleicht unverträgliche Werte")),
    }
}

/// Wertet ein `when`-Prädikat aus.
///
/// # Argumente
/// - `expr`: Prädikat (`cash < 0`, `share('a','sme') > 0.25` …).
/// - `ctx`: Zustand, Team und Aktionsart.
///
/// # Rückgabe
/// `Ok(true)`, wenn das Prädikat gilt.
///
/// # Errors
/// Deutsche Beschreibung bei Syntaxfehlern, unbekannten Bezeichnern oder
/// unverträglichen Vergleichen.
pub fn evaluate_predicate(expr: &str, ctx: &PredicateContext<'_>) -> Result<bool, String> {
    let tokens = tokenize(expr)?;
    let mut parser = Parser {
        tokens: &tokens,
        pos: 0,
        ctx,
    };
    let value = parser.or()?;
    if parser.pos != tokens.len() {
        return Err(format!("überzählige Zeichen in `{expr}`"));
    }
    truthy(&value)
}

// ---------------------------------------------------------------------------
// Regeln
// ---------------------------------------------------------------------------

/// Wirkung einer Geschäftsregel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleEffect {
    /// `restrict_actions:[a,b]` — diese Aktionsarten sind gesperrt.
    RestrictActions(Vec<String>),
    /// `require_stakeholder:<id>` — die Aktion braucht die Zustimmung eines
    /// vom GameMaster gespielten Stakeholders.
    RequireStakeholder(String),
    /// Unbekannte Wirkung (nur Vermerk).
    Other(String),
}

impl RuleEffect {
    /// Parst die Textform aus dem Szenario.
    #[must_use]
    pub fn parse(effect: &str) -> Self {
        let effect = effect.trim();
        if let Some(rest) = effect.strip_prefix("restrict_actions:") {
            let kinds = rest
                .trim()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split(',')
                .map(|k| k.trim().trim_matches(['\'', '"']).to_owned())
                .filter(|k| !k.is_empty())
                .collect();
            return Self::RestrictActions(kinds);
        }
        if let Some(rest) = effect.strip_prefix("require_stakeholder:") {
            return Self::RequireStakeholder(rest.trim().to_owned());
        }
        Self::Other(effect.to_owned())
    }
}

/// Ein Treffer einer Geschäftsregel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleHit {
    /// Regel-ID.
    pub rule_id: String,
    /// Wirkung.
    pub effect: RuleEffect,
}

/// Ergebnis der Regelprüfung für ein Argument.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleCheck {
    /// Greifende Regeln.
    pub hits: Vec<RuleHit>,
    /// Nicht auswertbare Regeln (`(id, grund)`).
    pub errors: Vec<(String, String)>,
}

/// Prüft alle Geschäftsregeln für ein Argument von `team`.
///
/// # Rückgabe
/// Leeres Ergebnis außerhalb des Business-Modus.
#[must_use]
pub fn rule_hits(
    state: &GameState,
    scenario: &Scenario,
    team: &PlayerId,
    action_kind: Option<&str>,
) -> RuleCheck {
    let Scenario::Business(b) = scenario else {
        return RuleCheck::default();
    };
    let ctx = PredicateContext {
        state,
        team: Some(team),
        action_kind,
    };
    let mut check = RuleCheck::default();
    for rule in &b.business.rules {
        match evaluate_predicate(&rule.when, &ctx) {
            Ok(true) => check.hits.push(RuleHit {
                rule_id: rule.id.clone(),
                effect: RuleEffect::parse(&rule.effect),
            }),
            Ok(false) => {}
            Err(error) => check.errors.push((rule.id.clone(), error)),
        }
    }
    check
}

/// Stichwörter je Aktionsart (klein geschrieben, Teilwort-Treffer).
const KIND_KEYWORDS: &[(&str, &[&str])] = &[
    (
        "acquisition",
        &[
            "acquisition",
            "übernahme",
            "akquisition",
            "übernimmt",
            "kauft",
        ],
    ),
    ("invest", &["invest"]),
    ("price", &["preis", "price", "rabatt"]),
    (
        "alliance",
        &["allianz", "alliance", "partnerschaft", "kooperation"],
    ),
    ("product", &["produkt", "product", "launch"]),
    ("lobby", &["lobby"]),
    ("campaign", &["kampagne", "campaign", "werbung"]),
];

/// Aktionsart eines Arguments im Business-Modus.
///
/// # Beschreibung
/// Zuerst das ausdrückliche Präfix `[art]` (muss in `kinds` stehen), sonst
/// das erste Stichwort aus [`KIND_KEYWORDS`], dessen Art in `kinds` steht.
///
/// # Rückgabe
/// `None`, wenn keine Art erkennbar ist.
#[must_use]
pub fn action_kind(action: &str, kinds: &[String]) -> Option<String> {
    let trimmed = action.trim_start();
    if let Some(rest) = trimmed.strip_prefix('[') {
        if let Some((kind, _)) = rest.split_once(']') {
            let kind = kind.trim().to_lowercase();
            if kinds.contains(&kind) {
                return Some(kind);
            }
        }
    }
    let lower = action.to_lowercase();
    KIND_KEYWORDS
        .iter()
        .filter(|(kind, _)| kinds.iter().any(|k| k == kind))
        .find(|(_, words)| words.iter().any(|w| lower.contains(w)))
        .map(|(kind, _)| (*kind).to_owned())
}

/// Sperrgrund für ein Argument (erste `restrict_actions`-Regel, die seine
/// Aktionsart sperrt).
///
/// # Rückgabe
/// `Some((regel, art))` bei einer Sperre, sonst `None` (auch außerhalb des
/// Business-Modus).
#[must_use]
pub fn restriction_for(
    state: &GameState,
    scenario: &Scenario,
    team: &PlayerId,
    action: &str,
) -> Option<(String, String)> {
    let Scenario::Business(b) = scenario else {
        return None;
    };
    let kind = action_kind(action, &b.business.action_kinds)?;
    let check = rule_hits(state, scenario, team, Some(&kind));
    check.hits.into_iter().find_map(|hit| match hit.effect {
        RuleEffect::RestrictActions(kinds) if kinds.contains(&kind) => {
            Some((hit.rule_id, kind.clone()))
        }
        _ => None,
    })
}

/// Journal-Vermerke der Regelprüfung eines Arguments: Stakeholder-Pflichten
/// (`require_stakeholder`) für den Umpire.
///
/// # Beschreibung
/// Nicht auswertbare Regeln (etwa `cash < 0` für ein Team ohne Kasse, z. B.
/// das Marktteam) greifen schlicht nicht und erzeugen keinen Vermerk — sonst
/// stünde bei jedem Argument eines solchen Teams dieselbe Meldung im
/// Journal. [`rule_hits`] liefert sie für Diagnosen weiterhin.
#[must_use]
pub fn rule_notes(
    state: &GameState,
    scenario: &Scenario,
    team: &PlayerId,
    action: &str,
    argument_id: &str,
    round: u32,
) -> Vec<GameEntry> {
    let Scenario::Business(b) = scenario else {
        return Vec::new();
    };
    let kind = action_kind(action, &b.business.action_kinds);
    let check = rule_hits(state, scenario, team, kind.as_deref());
    check
        .hits
        .iter()
        .filter_map(|hit| match &hit.effect {
            RuleEffect::RequireStakeholder(stakeholder) => Some(GameEntry::new(
                round,
                Audience::UmpireOnly,
                EntryKind::FacilitatorNote {
                    command: RULE_NOTE_COMMAND.to_owned(),
                    detail: format!(
                        "Regel `{}`: Argument `{argument_id}` ({team}) braucht die Zustimmung von `{stakeholder}`",
                        hit.rule_id
                    ),
                },
            )),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Marktmodell
// ---------------------------------------------------------------------------

/// Eine Anteilsänderung des Marktmodells.
#[derive(Debug, Clone, PartialEq)]
pub struct ShareChange {
    /// Team.
    pub team: String,
    /// Segment.
    pub segment: String,
    /// Alter Anteil.
    pub from: f64,
    /// Neuer Anteil.
    pub to: f64,
}

fn normalized_0_10(value: f64) -> f64 {
    ((value - 5.0) / 5.0).clamp(-1.0, 1.0)
}

fn component(state: &GameState, team: &str, name: &str, segment: &str) -> Option<f64> {
    number_var(state, &format!("{team}.{name}.{segment}"))
        .or_else(|_| number_var(state, &format!("{team}.{name}")))
        .ok()
}

/// Rundenerfolg eines Teams aus den `ArgumentResolved`-Einträgen der Runde
/// (−1..1; ohne Argument 0).
fn round_success(log: &GameLog, team: &str, round: u32) -> f64 {
    let mut sum = 0.0;
    let mut n = 0u32;
    for entry in log.journal.entries() {
        if entry.round != round {
            continue;
        }
        if let EntryKind::ArgumentResolved { seat, outcome, .. } = &entry.kind {
            if seat.as_str() != team {
                continue;
            }
            sum += match outcome {
                Outcome::Success | Outcome::AutoSuccess => 1.0,
                Outcome::Failure => -1.0,
                Outcome::Vetoed | Outcome::Forfeited => -0.5,
            };
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { sum / f64::from(n) }
}

fn team_score(
    log: &GameLog,
    b: &BusinessScenario,
    model: &MarketModel,
    team: &str,
    segment: &str,
    round: u32,
) -> f64 {
    let state = &log.state;
    let weight = |key: &str| model.weights.get(key).copied().unwrap_or(0.0);
    let market = component(state, team, "market_score", segment)
        .map_or_else(|| round_success(log, team, round), normalized_0_10);
    let sensitivity = b
        .business
        .segments
        .iter()
        .find(|s| s.id == segment)
        .and_then(|s| s.price_sensitivity)
        .unwrap_or(1.0);
    let price = component(state, team, "price_position", segment).map_or(0.0, normalized_0_10);
    let saturation = model
        .sales_invest_saturation_meur
        .filter(|s| *s > 0.0)
        .unwrap_or(DEFAULT_SATURATION_MEUR);
    let sales = component(state, team, "sales_invest", segment)
        .map_or(0.0, |x| 1.0 - (-(x.max(0.0)) / saturation).exp());
    let fit = component(state, team, "product_fit", segment).map_or(0.0, normalized_0_10);
    weight("market_score") * market
        + weight("price_position") * sensitivity * price
        + weight("sales_invest") * sales
        + weight("product_fit") * fit
}

/// Berechnet die neuen Anteile einer Runde, ohne sie zu verbuchen.
///
/// # Rückgabe
/// Alle Änderungen ≥ 0,0001; leer außerhalb des Business-Modus oder ohne
/// `logit_share`-Modell.
#[must_use]
pub fn market_shares(log: &GameLog, scenario: &Scenario, round: u32) -> Vec<ShareChange> {
    let Scenario::Business(b) = scenario else {
        return Vec::new();
    };
    let Some(model) = b
        .business
        .market_model
        .as_ref()
        .filter(|m| m.kind == "logit_share")
    else {
        return Vec::new();
    };
    // Segment → [(Team, alter Anteil)].
    let mut segments: BTreeMap<String, Vec<(String, f64)>> = BTreeMap::new();
    for (id, var) in &log.state.vars {
        let Some((team, segment)) = id.split_once(".share.") else {
            continue;
        };
        let Some(value) = numeric(&var.value) else {
            continue;
        };
        segments
            .entry(segment.to_owned())
            .or_default()
            .push((team.to_owned(), value));
    }
    let mut changes = Vec::new();
    for (segment, teams) in segments {
        let total: f64 = teams.iter().map(|(_, s)| s).sum();
        if teams.len() < 2 || total <= 0.0 {
            continue;
        }
        let attractions: Vec<f64> = teams
            .iter()
            .map(|(team, share)| {
                let score = team_score(log, b, model, team, &segment, round);
                share.max(SHARE_ANCHOR) * (model.beta * score).exp()
            })
            .collect();
        let sum: f64 = attractions.iter().sum();
        if !sum.is_finite() || sum <= 0.0 {
            continue;
        }
        for ((team, from), attraction) in teams.iter().zip(&attractions) {
            let to = (total * attraction / sum * SHARE_SCALE).round() / SHARE_SCALE;
            if (to - from).abs() >= 1.0 / SHARE_SCALE {
                changes.push(ShareChange {
                    team: team.clone(),
                    segment: segment.clone(),
                    from: *from,
                    to,
                });
            }
        }
    }
    changes
}

/// Wendet das Marktmodell für `round` an und journalisiert jede Änderung als
/// `WorldDelta` (Ursache [`MARKET_MODEL_CAUSE`]).
///
/// # Rückgabe
/// Die verbuchten Änderungen.
///
/// # Errors
/// Zustandsfehler aus [`GameLog::record`].
pub fn apply_market_model(
    log: &mut GameLog,
    scenario: &Scenario,
    round: u32,
    at: Option<Timestamp>,
) -> MatrixResult<Vec<ShareChange>> {
    let changes = market_shares(log, scenario, round);
    let mut entries = Vec::new();
    for change in &changes {
        let id = format!("{}.share.{}", change.team, change.segment);
        let Some(var) = log.state.vars.get(&id) else {
            continue;
        };
        for audience in var.visibility.audiences() {
            entries.push(GameEntry::new(
                round,
                audience,
                EntryKind::WorldDelta {
                    var: id.clone(),
                    from: var.value.clone(),
                    to: VarValue::Number { value: change.to },
                    cause: MARKET_MODEL_CAUSE.to_owned(),
                },
            ));
        }
    }
    log.record_all(entries, at)?;
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phases::open_game;
    use crate::scenario::load_scenario;
    use crate::test_support::CLOUD;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn cloud_log() -> Result<(crate::scenario::LoadedScenario, GameLog), Box<dyn std::error::Error>>
    {
        let loaded = load_scenario(CLOUD)?;
        let log = open_game(&loaded, &[7u8; 32], None)?;
        Ok((loaded, log))
    }

    fn set_number(log: &mut GameLog, id: &str, value: f64) -> TestResult {
        let from = log
            .state
            .vars
            .get(id)
            .map(|v| v.value.clone())
            .ok_or("Variable fehlt")?;
        log.record(
            GameEntry::new(
                1,
                Audience::ObserverOnly,
                EntryKind::WorldDelta {
                    var: id.to_owned(),
                    from,
                    to: VarValue::Number { value },
                    cause: "test".to_owned(),
                },
            ),
            None,
        )?;
        Ok(())
    }

    #[test]
    fn predicates_cover_the_scenario_forms() -> TestResult {
        let (_, log) = cloud_log()?;
        let company = PlayerId::new("company");
        let ctx = PredicateContext {
            state: &log.state,
            team: Some(&company),
            action_kind: Some("acquisition"),
        };
        assert!(!evaluate_predicate("cash < 0", &ctx)?);
        assert!(evaluate_predicate(
            "cash > 10 && action.kind == 'acquisition'",
            &ctx
        )?);
        assert!(!evaluate_predicate(
            "share(segment) > 0.40 && action.kind == 'acquisition'",
            &ctx
        )?);
        assert!(evaluate_predicate("share(segment) > 0.30", &ctx)?);
        assert!(evaluate_predicate(
            "share('competitor_a','sme') < 0.25",
            &ctx
        )?);
        assert!(evaluate_predicate("!(cash < 0) || false", &ctx)?);
        assert!(evaluate_predicate("gibt_es_nicht > 1", &ctx).is_err());
        assert!(evaluate_predicate("cash <", &ctx).is_err());
        Ok(())
    }

    #[test]
    fn cash_floor_restricts_invest_and_acquisition() -> TestResult {
        let (loaded, mut log) = cloud_log()?;
        let company = PlayerId::new("company");
        let scenario = &loaded.scenario;
        assert_eq!(
            restriction_for(
                &log.state,
                scenario,
                &company,
                "[invest] 12 MEUR in den Vertrieb"
            ),
            None
        );
        set_number(&mut log, "company.cash", -5.0)?;
        assert_eq!(
            restriction_for(
                &log.state,
                scenario,
                &company,
                "[invest] 12 MEUR in den Vertrieb"
            ),
            Some(("cash_floor".to_owned(), "invest".to_owned()))
        );
        assert_eq!(
            restriction_for(
                &log.state,
                scenario,
                &company,
                "Wir kaufen ValueHost (Übernahme)"
            ),
            Some(("cash_floor".to_owned(), "acquisition".to_owned()))
        );
        assert_eq!(
            restriction_for(&log.state, scenario, &company, "[price] KMU-Preise −10 %"),
            None
        );
        Ok(())
    }

    #[test]
    fn antitrust_rule_notes_the_regulator() -> TestResult {
        let (loaded, mut log) = cloud_log()?;
        set_number(&mut log, "company.share.sme", 0.45)?;
        let company = PlayerId::new("company");
        let notes = rule_notes(
            &log.state,
            &loaded.scenario,
            &company,
            "[acquisition] Übernahme eines Systemhauses",
            "r1-a1",
            1,
        );
        assert!(notes.iter().any(|n| matches!(
            &n.kind,
            EntryKind::FacilitatorNote { detail, .. } if detail.contains("regulator")
        )));
        Ok(())
    }

    #[test]
    fn action_kind_prefers_the_explicit_prefix() {
        let kinds: Vec<String> = ["price", "invest", "acquisition"]
            .iter()
            .map(|k| (*k).to_owned())
            .collect();
        assert_eq!(
            action_kind("[price] Investition in Preise", &kinds).as_deref(),
            Some("price")
        );
        assert_eq!(
            action_kind("Wir investieren 5 MEUR", &kinds).as_deref(),
            Some("invest")
        );
        assert_eq!(action_kind("Wir warten ab", &kinds), None);
    }

    #[test]
    fn market_model_moves_shares_toward_the_successful_team() -> TestResult {
        let (loaded, mut log) = cloud_log()?;
        let before_company = number_var(&log.state, "company.share.sme")?;
        let before_total: f64 = ["company", "competitor_a", "competitor_b"]
            .iter()
            .map(|t| number_var(&log.state, &format!("{t}.share.sme")))
            .sum::<Result<f64, String>>()?;
        log.record(
            GameEntry::new(
                1,
                Audience::Public,
                EntryKind::ArgumentResolved {
                    argument_id: "r1-a1".to_owned(),
                    seat: PlayerId::new("company"),
                    outcome: Outcome::Success,
                    grade: None,
                    fail_chit_delta: 0,
                },
            ),
            None,
        )?;
        let changes = apply_market_model(&mut log, &loaded.scenario, 1, None)?;
        assert!(!changes.is_empty(), "Marktmodell muss Anteile verschieben");
        let after_company = number_var(&log.state, "company.share.sme")?;
        assert!(
            after_company > before_company,
            "{before_company} → {after_company}"
        );
        let after_total: f64 = ["company", "competitor_a", "competitor_b"]
            .iter()
            .map(|t| number_var(&log.state, &format!("{t}.share.sme")))
            .sum::<Result<f64, String>>()?;
        assert!(
            (after_total - before_total).abs() < 0.001,
            "Summe bleibt erhalten"
        );
        assert!(log.journal.entries().any(|e| matches!(
            &e.kind,
            EntryKind::WorldDelta { cause, .. } if cause == MARKET_MODEL_CAUSE
        )));
        Ok(())
    }

    #[test]
    fn equal_scores_keep_shares() -> TestResult {
        let (loaded, log) = cloud_log()?;
        assert!(market_shares(&log, &loaded.scenario, 1).is_empty());
        Ok(())
    }
}
