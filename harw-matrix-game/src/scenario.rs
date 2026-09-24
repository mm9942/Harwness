//! Szenario-Format `harwness.matrix-scenario/v1` (matrix-game.md §5.1,
//! wargaming-and-analysis.md §1.9) — Parsing und Validierung beider Modi.
//!
//! - `mode = "classic"` (Default, auch ohne `mode`-Feld): Matrix Game nach
//!   Curry & Price mit genau vier Fraktionen, Tracks, States, Objekten und
//!   Big Projects.
//! - `mode = "business"`: Oriesek-artiges Business-Wargame mit Teams,
//!   Schlüsselfragen, Segmenten, Marktmodell und Injects. Wo sich beide
//!   Entwürfe unterscheiden, gilt `matrix-game.md`: dieselbe Phasen-FSM,
//!   dieselben `[rules]`/`[visibility]`-Tabellen (optional) und dieselbe
//!   Sichtbarkeitslogik.
//!
//! Das Laden liefert ein [`LoadedScenario`] mit Warnungen (nicht blockierend)
//! und dem SHA-256 des Quelltexts (Commitment im `GameCreated`-Event).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::commitments::{DOMAIN, from_hex, sha256_parts, to_hex};
use crate::error::{MatrixError, MatrixResult};
use crate::phases::{EffectContext, EffectOp, validate_effects};
use crate::state::{Audience, AudienceSpec, Ongoing, PlayerId, SecretRecord, VarValue, WorldVar};

/// Schema-Kennung, die jedes Szenario tragen muss.
pub const SCHEMA: &str = "harwness.matrix-scenario/v1";

/// Anzahl der Spieler-Sitze im ersten Schnitt (matrix-game.md §5.1, §10).
pub const PLAYER_SEATS: usize = 4;

/// Höchstzahl Ziele je Kategorie und Fraktion.
pub const MAX_GOALS: usize = 4;

/// Ab dieser Track-Zahl warnt die Validierung (Sabin: einfach halten).
pub const TRACK_WARNING_THRESHOLD: usize = 10;

/// Geschlossener Satz der Business-Aktionsarten (wargaming §1.5).
pub const BUSINESS_ACTION_KINDS: &[&str] = &[
    "price",
    "invest",
    "product",
    "alliance",
    "acquisition",
    "lobby",
    "campaign",
    "free_argument",
];

/// Phasen-Kennungen des Business-Zugs für `injects.at.phase` (wargaming §1.3).
pub const BUSINESS_MOVE_PHASES: &[&str] = &[
    "brief",
    "deliberation",
    "submission",
    "plenary",
    "market_assessment",
    "adjudication",
    "hotwash",
];

// ---------------------------------------------------------------------------
// Gemeinsame Bausteine
// ---------------------------------------------------------------------------

/// Spielmodus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioMode {
    /// Klassisches Matrix Game (Default).
    #[default]
    Classic,
    /// Business-Wargame (wargaming-and-analysis.md §1).
    Business,
}

/// Seed-Angabe im Szenario: Zahl, 64 Hex-Zeichen, beliebiger Text oder leer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SeedSpec {
    /// Ganzzahliger Seed (Business-Beispiel: `seed = 20270101`).
    Number(u64),
    /// Hex-Seed, Freitext oder leer (leer = beim Start ziehen).
    Text(String),
}

impl SeedSpec {
    /// Leitet den 32-Byte-Master-Seed ab; `None` heißt „beim Start ziehen und
    /// als erstes Event journalisieren“.
    #[must_use]
    pub fn master_seed(&self) -> Option<[u8; 32]> {
        match self {
            Self::Number(n) => Some(sha256_parts(&[DOMAIN, b"/seed/u64", &n.to_be_bytes()])),
            Self::Text(text) => {
                let text = text.trim();
                if text.is_empty() {
                    return None;
                }
                if text.len() == 64 {
                    if let Some(bytes) = from_hex(text) {
                        if let Ok(arr) = <[u8; 32]>::try_from(bytes) {
                            return Some(arr);
                        }
                    }
                }
                Some(sha256_parts(&[DOMAIN, b"/seed/text", text.as_bytes()]))
            }
        }
    }
}

/// Sichtbarkeit einer Weltvariablen: `public`, `umpire`, `seat:<id>`,
/// `seats:<id>,<id>` (auch `seats:[a,b]`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum VarVisibility {
    /// Alle Sitze.
    Public,
    /// Nur der Umpire.
    Umpire,
    /// Ein Spieler (Besitzer) und der Umpire.
    Seat(PlayerId),
    /// Mehrere Spieler und der Umpire (sortiert, ohne Duplikate).
    Seats(Vec<PlayerId>),
}

impl VarVisibility {
    /// Parst die Textform.
    ///
    /// # Errors
    /// Beschreibung des Formatfehlers.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        match text {
            "public" => return Ok(Self::Public),
            "umpire" => return Ok(Self::Umpire),
            _ => {}
        }
        if let Some(rest) = text.strip_prefix("seats:") {
            let rest = rest.trim().trim_start_matches('[').trim_end_matches(']');
            let mut ids: Vec<PlayerId> = rest
                .split(',')
                .map(|s| s.trim().trim_matches('"'))
                .filter(|s| !s.is_empty())
                .map(PlayerId::new)
                .collect();
            ids.sort();
            ids.dedup();
            return match ids.len() {
                0 => Err(format!("Sichtbarkeit `{text}` nennt keinen Sitz")),
                1 => Ok(ids.pop().map_or(Self::Umpire, Self::Seat)),
                _ => Ok(Self::Seats(ids)),
            };
        }
        if let Some(rest) = text.strip_prefix("seat:") {
            let id = rest.trim();
            if id.is_empty() {
                return Err(format!("Sichtbarkeit `{text}` nennt keinen Sitz"));
            }
            return Ok(Self::Seat(PlayerId::new(id)));
        }
        Err(format!(
            "unbekannte Sichtbarkeit `{text}` (erlaubt: public, umpire, seat:<id>, seats:<id>,<id>)"
        ))
    }

    /// Ist die Variable verdeckt (nicht öffentlich)?
    #[must_use]
    pub fn is_hidden(&self) -> bool {
        !matches!(self, Self::Public)
    }

    /// Besitzer einer `seat:<id>`-Variablen.
    #[must_use]
    pub fn owner(&self) -> Option<&PlayerId> {
        match self {
            Self::Seat(p) => Some(p),
            _ => None,
        }
    }

    /// Alle referenzierten Spieler.
    #[must_use]
    pub fn referenced_players(&self) -> Vec<&PlayerId> {
        match self {
            Self::Public | Self::Umpire => Vec::new(),
            Self::Seat(p) => vec![p],
            Self::Seats(ps) => ps.iter().collect(),
        }
    }

    /// Journal-Audiences, unter denen Deklaration und Deltas dieser Variablen
    /// verbucht werden. Mehrere Sitze werden auf je einen Eintrag pro Sitz
    /// abgebildet (der erste zusätzlich mit Umpire), weil [`Audience`] keine
    /// beliebigen Sitzmengen kennt.
    #[must_use]
    pub fn audiences(&self) -> Vec<Audience> {
        match self {
            Self::Public => vec![Audience::Public],
            Self::Umpire => vec![Audience::UmpireOnly],
            Self::Seat(p) => vec![Audience::SeatAndUmpire(p.clone())],
            Self::Seats(ps) => ps
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    if i == 0 {
                        Audience::SeatAndUmpire(p.clone())
                    } else {
                        Audience::Seat(p.clone())
                    }
                })
                .collect(),
        }
    }
}

impl fmt::Display for VarVisibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Public => f.write_str("public"),
            Self::Umpire => f.write_str("umpire"),
            Self::Seat(p) => write!(f, "seat:{p}"),
            Self::Seats(ps) => {
                let ids: Vec<&str> = ps.iter().map(PlayerId::as_str).collect();
                write!(f, "seats:{}", ids.join(","))
            }
        }
    }
}

impl TryFrom<String> for VarVisibility {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<VarVisibility> for String {
    fn from(value: VarVisibility) -> Self {
        value.to_string()
    }
}

impl Default for VarVisibility {
    fn default() -> Self {
        Self::Public
    }
}

/// Argumentationssystem (matrix-game.md §1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgumentSystem {
    /// Pro/Contra mit Gegenargument-Phase (Default).
    #[default]
    ProsCons,
    /// Genau drei Gründe, keine Gegenargumente.
    ThreeReasons,
}

/// Einreichungsmodus der Argumente.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgumentMode {
    /// Versiegelt und parallel (Default).
    #[default]
    Simultaneous,
    /// Nacheinander; Spieler n sieht Argumente 1..n−1.
    Sequential,
}

/// Adjudikationsverfahren (matrix-game.md §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AdjudicationSystem {
    /// 2W6 mit Nettopunkten (Default).
    #[default]
    #[serde(rename = "pros_cons_2d6")]
    ProsCons2d6,
    /// Wahrscheinlichkeitsleiter und W100.
    #[serde(rename = "estimative_d100")]
    EstimativeD100,
}

/// Zugreihenfolge / Auflösungspriorität.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOrder {
    /// Feste Reihenfolge aus `seating.order` (Default).
    #[default]
    Fixed,
    /// Führende zuerst (letztes Umpire-`standing`).
    LeaderFirst,
}

/// Spielende.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ending {
    /// Nach fester Rundenzahl (Default).
    #[default]
    FixedRounds,
    /// Schlussargumente mit Knock-out-Würfen.
    FinalArguments,
}

/// Verhandlungsregeln (`[rules.negotiation]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NegotiationRules {
    /// Verhandlungsphase aktiv.
    pub enabled: bool,
    /// Austauschrunden je Verhandlungsphase.
    pub max_exchanges: u32,
    /// Höchstzahl gleichzeitig angefragter Kanäle je Sitz.
    pub max_channels_per_seat: usize,
    /// Zeichenlimit je Nachricht.
    pub max_message_chars: usize,
}

impl Default for NegotiationRules {
    fn default() -> Self {
        Self {
            enabled: true,
            max_exchanges: 2,
            max_channels_per_seat: 2,
            max_message_chars: 800,
        }
    }
}

/// Spielregeln (`[rules]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Rules {
    /// Argumentationssystem.
    pub argument_system: ArgumentSystem,
    /// Einreichungsmodus.
    pub argument_mode: ArgumentMode,
    /// Adjudikationsverfahren.
    pub adjudication: AdjudicationSystem,
    /// Zugreihenfolge.
    pub turn_order: TurnOrder,
    /// Maximaler Track-Schritt je Argument (1, höchstens 2).
    pub max_track_step: i32,
    /// Höchstzahl gleichzeitig laufender Effekte.
    pub max_ongoing: usize,
    /// `no_roll` bei zwingendem Argument zulässig.
    pub allow_auto_success: bool,
    /// Mindest-Netto für `no_roll`.
    pub auto_success_net: i32,
    /// Fail-Chits aktiv.
    pub fail_chits: bool,
    /// Konsens-Stimmungsbild bei strittigen Gründen.
    pub consensus_check: bool,
    /// Geheime Argumente je Sitz und Spiel.
    pub max_secret_arguments_per_seat: u32,
    /// Spielende.
    pub ending: Ending,
    /// Vor-Runde.
    pub prologue: bool,
    /// Nach-Runde.
    pub epilogue: bool,
    /// Spieler-Debrief im AAR.
    pub debrief_players: bool,
    /// Verhandlungsregeln.
    pub negotiation: NegotiationRules,
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            argument_system: ArgumentSystem::default(),
            argument_mode: ArgumentMode::default(),
            adjudication: AdjudicationSystem::default(),
            turn_order: TurnOrder::default(),
            max_track_step: 1,
            max_ongoing: 6,
            allow_auto_success: true,
            auto_success_net: 5,
            fail_chits: false,
            consensus_check: false,
            max_secret_arguments_per_seat: 1,
            ending: Ending::default(),
            prologue: false,
            epilogue: false,
            debrief_players: false,
            negotiation: NegotiationRules::default(),
        }
    }
}

/// Umpire-Zugriff auf Paar-Kanäle (matrix-game.md §2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UmpireNegotiations {
    /// Kein Zugriff (Default).
    #[default]
    None,
    /// Nur zitierte Kanäle für die laufende Adjudikation.
    Cited,
    /// Alle Kanäle.
    Full,
}

/// Leak-Guard-Modus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeakGuardMode {
    /// Reprompt, dann zurückhalten (Default).
    #[default]
    Strict,
    /// Nur kennzeichnen.
    FlagOnly,
}

/// Sichtbarkeitsoptionen (`[visibility]`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VisibilitySettings {
    /// Umpire-Zugriff auf Verhandlungen.
    pub umpire_negotiations: UmpireNegotiations,
    /// Ergebnis geheimer Argumente öffentlich.
    pub secret_outcome_public: bool,
    /// Leak-Guard-Modus.
    pub leak_guard: LeakGuardMode,
}

/// Modellzuordnung (`[models]`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelSettings {
    /// Modell des Umpires.
    pub umpire: Option<String>,
    /// Modell der Spieler.
    pub players: Option<String>,
}

// ---------------------------------------------------------------------------
// Klassischer Modus
// ---------------------------------------------------------------------------

/// Numerischer Track.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackDef {
    /// Kennung.
    pub id: String,
    /// Anzeigename.
    pub label: String,
    /// Untergrenze.
    pub min: i32,
    /// Obergrenze.
    pub max: i32,
    /// Startwert.
    pub start: i32,
    /// Sichtbarkeit.
    #[serde(default)]
    pub visibility: VarVisibility,
}

/// Diskreter Zustand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateDef {
    /// Kennung.
    pub id: String,
    /// Anzeigename.
    pub label: String,
    /// Erlaubte Werte.
    pub values: Vec<String>,
    /// Startwert.
    pub start: String,
    /// Sichtbarkeit.
    #[serde(default)]
    pub visibility: VarVisibility,
}

/// Verdecktes/geschütztes Objekt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectDef {
    /// Kennung.
    pub id: String,
    /// Anzeigename.
    pub label: String,
    /// Zu Beginn verdeckt (dann nur Umpire).
    #[serde(default)]
    pub hidden: bool,
    /// Schutzstufen.
    #[serde(default)]
    pub protection: u8,
    /// Sichtbarkeit nach `discover`.
    #[serde(default)]
    pub visibility_when_found: VarVisibility,
}

/// Big Project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectDef {
    /// Kennung.
    pub id: String,
    /// Anzeigename.
    pub label: String,
    /// Stufen (höchstens 3).
    pub stages: u8,
    /// Fortschritt zu Beginn.
    #[serde(default)]
    pub progress: u8,
    /// Sichtbarkeit.
    #[serde(default)]
    pub visibility: VarVisibility,
}

/// Welt (`[world]`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorldDef {
    /// Öffentliche Lage.
    pub public_situation: String,
    /// Tracks.
    pub tracks: Vec<TrackDef>,
    /// Diskrete Zustände.
    pub states: Vec<StateDef>,
    /// Objekte.
    pub objects: Vec<ObjectDef>,
    /// Big Projects.
    pub projects: Vec<ProjectDef>,
}

/// Ziele einer Fraktion.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Goals {
    /// Öffentliche Ziele.
    pub public: Vec<String>,
    /// Geheime Ziele (nur eigener Sitz und Umpire).
    pub secret: Vec<String>,
}

/// Fraktion (`[[factions]]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Faction {
    /// Kennung (= Sitz-ID).
    pub id: String,
    /// Anzeigename.
    pub name: String,
    /// Ebene (Warnung bei Uneinheitlichkeit).
    #[serde(default)]
    pub level: Option<String>,
    /// Briefing.
    #[serde(default)]
    pub briefing: String,
    /// Ziele.
    #[serde(default)]
    pub goals: Goals,
    /// Machtmittel.
    #[serde(default)]
    pub assets: Vec<String>,
}

/// Sitzordnung (`[seating]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Seating {
    /// Reihenfolge = Zugreihenfolge.
    pub order: Vec<String>,
}

/// Vorbereitetes Facilitator-Ereignis im klassischen Modus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassicInject {
    /// Kennung.
    pub id: String,
    /// Runde, zu deren Beginn es wirkt.
    pub round: u32,
    /// Audience (`public`, `umpire`, `seat:x`, `seat+umpire:x`, `pair:a,b`).
    #[serde(default)]
    pub audience: AudienceSpec,
    /// Text.
    pub text: String,
    /// Effekte.
    #[serde(default)]
    pub effects: Vec<EffectOp>,
    /// Öffentlich mit Urheber „Facilitator“.
    #[serde(default)]
    pub attributed: bool,
}

/// Klassisches Szenario.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassicScenario {
    /// Schema-Kennung.
    pub schema: String,
    /// Modus (hier `classic`).
    #[serde(default)]
    pub mode: ScenarioMode,
    /// Szenario-ID.
    pub id: String,
    /// Titel.
    pub title: String,
    /// Zweck — erste Zeile jedes Prompts.
    pub purpose: String,
    /// Seed.
    #[serde(default)]
    pub seed: Option<SeedSpec>,
    /// Rundenzahl.
    #[serde(default = "default_rounds")]
    pub rounds: u32,
    /// Was eine Runde darstellt.
    #[serde(default)]
    pub round_represents: Option<String>,
    /// Sprache der Prompts.
    #[serde(default)]
    pub language: Option<String>,
    /// Regeln.
    #[serde(default)]
    pub rules: Rules,
    /// Sichtbarkeit.
    #[serde(default)]
    pub visibility: VisibilitySettings,
    /// Modelle.
    #[serde(default)]
    pub models: Option<ModelSettings>,
    /// Welt.
    #[serde(default)]
    pub world: WorldDef,
    /// Fraktionen.
    pub factions: Vec<Faction>,
    /// Sitzordnung.
    pub seating: Seating,
    /// Injects.
    #[serde(default)]
    pub injects: Vec<ClassicInject>,
}

fn default_rounds() -> u32 {
    6
}

// ---------------------------------------------------------------------------
// Business-Modus
// ---------------------------------------------------------------------------

/// Schlüsselfrage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyQuestion {
    /// Kennung.
    pub id: String,
    /// Frage.
    pub text: String,
}

/// `[game]` im Business-Modus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusinessGame {
    /// Anzahl Züge (= Runden der Phasen-FSM).
    pub moves: u32,
    /// Simulierte Zeit je Zug.
    #[serde(default)]
    pub move_labels: Vec<String>,
    /// Max. Agenten-Turns je Phase und Team.
    #[serde(default)]
    pub phase_deadline_turns: Option<u32>,
    /// Max. Kanalnachrichten je Zug.
    #[serde(default)]
    pub max_channel_messages_per_move: Option<u32>,
    /// Schlüsselfragen (Pflicht, mindestens eine).
    #[serde(default)]
    pub key_questions: Vec<KeyQuestion>,
}

/// Business-Variante.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BusinessVariant {
    /// Strategietest (Default).
    #[default]
    StrategyTest,
    /// Angriff/Gegenstrategie.
    AttackCounter,
    /// Krisenreaktion.
    CrisisResponse,
}

/// Belegung der Marktrolle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketRole {
    /// Markt ist ein Spieler-Sitz (Default).
    #[default]
    Player,
    /// Markt als White-Cell-Subagent des GameMasters.
    WhiteCell,
}

/// Korrekturgrenzen des Control-Urteils.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OverrideBounds {
    /// Anteil in Prozentpunkten.
    pub share_pp: Option<f64>,
    /// Kosten in Prozent.
    pub cost_pct: Option<f64>,
}

/// Marktmodell.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MarketModel {
    /// Modellart (z. B. `logit_share`).
    pub kind: String,
    /// Logit-Steilheit.
    pub beta: f64,
    /// Gewichte der Score-Komponenten.
    pub weights: BTreeMap<String, f64>,
    /// Sättigung des Vertriebsinvests.
    pub sales_invest_saturation_meur: Option<f64>,
    /// Ausgewiesene KPIs.
    pub kpis: Vec<String>,
}

/// Marktsegment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Segment {
    /// Kennung.
    pub id: String,
    /// Anzeigename.
    pub name: String,
    /// Größe je Zug.
    #[serde(default)]
    pub size_meur: Vec<f64>,
    /// Preissensitivität.
    #[serde(default)]
    pub price_sensitivity: Option<f64>,
}

/// Deklarative Geschäftsregel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusinessRule {
    /// Kennung.
    pub id: String,
    /// Prädikat.
    pub when: String,
    /// Wirkung.
    pub effect: String,
}

/// Vom GameMaster gespielter Stakeholder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stakeholder {
    /// Kennung.
    pub id: String,
    /// Anzeigename.
    pub display: String,
    /// `gamemaster` oder Team-ID.
    #[serde(default = "default_played_by")]
    pub played_by: String,
    /// Profil.
    #[serde(default)]
    pub profile: String,
}

fn default_played_by() -> String {
    "gamemaster".to_owned()
}

/// Team-Audience: `"all"` oder Liste von Team-IDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TeamAudience {
    /// Schlüsselwort (nur `all`).
    Keyword(String),
    /// Explizite Team-Liste.
    Teams(Vec<String>),
}

impl TeamAudience {
    /// Journal-Audiences; Teamlisten werden je Team als `SeatAndUmpire`
    /// verbucht (Control liest mit).
    #[must_use]
    pub fn audiences(&self) -> Vec<Audience> {
        match self {
            Self::Keyword(_) => vec![Audience::Public],
            Self::Teams(ids) => ids
                .iter()
                .map(|id| Audience::SeatAndUmpire(PlayerId::new(id.as_str())))
                .collect(),
        }
    }

    fn check(&self, teams: &BTreeSet<&str>, context: &str, errors: &mut Vec<String>) {
        match self {
            Self::Keyword(k) if k == "all" => {}
            Self::Keyword(k) => errors.push(format!(
                "{context}: audience `{k}` unbekannt (erlaubt: \"all\" oder Team-Liste)"
            )),
            Self::Teams(ids) => {
                if ids.is_empty() {
                    errors.push(format!("{context}: leere Team-Liste"));
                }
                for id in ids {
                    if !teams.contains(id.as_str()) {
                        errors.push(format!("{context}: unbekanntes Team `{id}`"));
                    }
                }
            }
        }
    }
}

/// Offenlegungsregel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisclosureRule {
    /// Auslösendes GM-Ereignis.
    pub on: String,
    /// Empfänger.
    pub audience: TeamAudience,
    /// Verzögerung in Phasen.
    #[serde(default)]
    pub delay_phases: u32,
    /// Meldungsvorlage.
    #[serde(default)]
    pub template: Option<String>,
}

/// `[business]`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BusinessSection {
    /// Variante.
    pub variant: BusinessVariant,
    /// Marktrolle.
    pub market_role: MarketRole,
    /// Währung.
    pub currency: Option<String>,
    /// Korrekturgrenzen.
    pub override_bounds: Option<OverrideBounds>,
    /// Erlaubte Aktionsarten.
    pub action_kinds: Vec<String>,
    /// Marktmodell.
    pub market_model: Option<MarketModel>,
    /// Segmente.
    pub segments: Vec<Segment>,
    /// Regeln.
    pub rules: Vec<BusinessRule>,
    /// Stakeholder.
    pub stakeholders: Vec<Stakeholder>,
    /// Offenlegungsregeln.
    pub disclosure: Vec<DisclosureRule>,
}

/// Teamrolle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamRole {
    /// Unternehmensteam („Blue“).
    Company,
    /// Wettbewerber („Red“).
    Competitor,
    /// Marktteam (Bewerter).
    Market,
}

/// Startwerte eines Teams.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TeamStart {
    /// Kasse.
    pub cash: Option<f64>,
    /// Anteile je Segment.
    pub share: BTreeMap<String, f64>,
    /// Kapazität.
    pub capacity: Option<f64>,
}

/// Team (`[[teams]]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Team {
    /// Kennung (= Sitz-ID).
    pub id: String,
    /// Rolle.
    pub role: TeamRole,
    /// Anzeigename.
    pub display: String,
    /// Agentenprofil.
    #[serde(default)]
    pub agent: Option<String>,
    /// Gamebook-Pfad (teamprivat).
    #[serde(default)]
    pub strategy_brief: Option<String>,
    /// Auftrag (z. B. „Be the enemy“).
    #[serde(default)]
    pub directive: Option<String>,
    /// Alternative Strategie des Unternehmensteams.
    #[serde(default)]
    pub strategy_variant: Option<String>,
    /// Startwerte.
    #[serde(default)]
    pub start: Option<TeamStart>,
    /// Bewertungsskala des Marktteams.
    #[serde(default)]
    pub assessment_scale: Option<Vec<i64>>,
}

/// `[channels]`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChannelsDef {
    /// Paarkanäle.
    pub pairwise: Option<String>,
    /// Mitleser.
    pub observer: Option<String>,
    /// Marktkontakt.
    pub market_contact: Option<String>,
}

/// Inject-Art.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectKind {
    /// Fester Zug/Phase.
    Scheduled,
    /// Prädikat über den Weltzustand.
    Conditional,
    /// Seed-deterministischer Wurf mit `p`.
    Random,
    /// Adjudikator-Vorschlag, vom GM bestätigt.
    ControlDiscretion,
}

/// Zeitpunkt eines Business-Injects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InjectAt {
    /// Zug (1-basiert).
    #[serde(rename = "move")]
    pub move_number: u32,
    /// Phase des Zugs.
    pub phase: String,
}

/// Business-Inject.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusinessInject {
    /// Kennung.
    pub id: String,
    /// Art.
    pub kind: InjectKind,
    /// Zeitpunkt.
    #[serde(default)]
    pub at: Option<InjectAt>,
    /// Prädikat.
    #[serde(default)]
    pub when: Option<String>,
    /// Wahrscheinlichkeit.
    #[serde(default)]
    pub p: Option<f64>,
    /// Empfänger.
    pub audience: TeamAudience,
    /// Text.
    pub text: String,
    /// Effekte (optional).
    #[serde(default)]
    pub effects: Vec<EffectOp>,
}

/// `[debrief]`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DebriefDef {
    /// Hotwash nach jedem Zug.
    pub hotwash_each_move: bool,
    /// Bias-Checkliste.
    pub bias_checklist: bool,
    /// Gegenstrategie-Taxonomie.
    pub counter_approach_taxonomy: bool,
    /// Indikatoren exportieren.
    pub export_indicators: bool,
    /// Endex-Freigabe.
    pub endex_release: Option<String>,
}

/// Business-Szenario.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusinessScenario {
    /// Schema-Kennung.
    pub schema: String,
    /// Modus (hier `business`).
    pub mode: ScenarioMode,
    /// Szenario-ID.
    pub id: String,
    /// Titel.
    pub title: String,
    /// Zweck (optional; sonst Titel).
    #[serde(default)]
    pub purpose: Option<String>,
    /// Seed.
    #[serde(default)]
    pub seed: Option<SeedSpec>,
    /// Sprache.
    #[serde(default)]
    pub language: Option<String>,
    /// Zugstruktur und Schlüsselfragen.
    pub game: BusinessGame,
    /// Business-Parameter.
    #[serde(default)]
    pub business: BusinessSection,
    /// Teams (genau 4 Sitze).
    pub teams: Vec<Team>,
    /// Kanäle.
    #[serde(default)]
    pub channels: Option<ChannelsDef>,
    /// Injects.
    #[serde(default)]
    pub injects: Vec<BusinessInject>,
    /// Debriefing.
    #[serde(default)]
    pub debrief: Option<DebriefDef>,
    /// Matrix-Game-Regeln (gelten auch hier, matrix-game.md hat Vorrang).
    #[serde(default)]
    pub rules: Rules,
    /// Sichtbarkeit.
    #[serde(default)]
    pub visibility: VisibilitySettings,
    /// Modelle.
    #[serde(default)]
    pub models: Option<ModelSettings>,
}

// ---------------------------------------------------------------------------
// Einheitliche Sicht
// ---------------------------------------------------------------------------

/// Geladenes Szenario eines der beiden Modi.
#[derive(Debug, Clone, PartialEq)]
pub enum Scenario {
    /// Klassischer Modus.
    Classic(Box<ClassicScenario>),
    /// Business-Modus.
    Business(Box<BusinessScenario>),
}

/// Fraktion/Team in modusunabhängiger Form (Briefing-Quelle).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactionInfo {
    /// Sitz-ID.
    pub id: PlayerId,
    /// Anzeigename.
    pub name: String,
    /// Ebene.
    pub level: Option<String>,
    /// Öffentliches Briefing.
    pub briefing: String,
    /// Öffentliche Ziele.
    pub public_goals: Vec<String>,
    /// Geheime Ziele.
    pub secret_goals: Vec<String>,
    /// Machtmittel.
    pub assets: Vec<String>,
    /// Private Zusatzinformation (Gamebook-Verweis).
    pub private_brief: Option<String>,
}

/// Für eine Runde fälliges Inject in modusunabhängiger Form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledInject {
    /// Kennung.
    pub id: String,
    /// Runde.
    pub round: u32,
    /// Empfänger-Audiences.
    pub audiences: Vec<Audience>,
    /// Text.
    pub text: String,
    /// Effekte.
    pub effects: Vec<EffectOp>,
    /// Mit Urheber.
    pub attributed: bool,
}

impl Scenario {
    /// Szenario-ID.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Classic(s) => &s.id,
            Self::Business(s) => &s.id,
        }
    }

    /// Titel.
    #[must_use]
    pub fn title(&self) -> &str {
        match self {
            Self::Classic(s) => &s.title,
            Self::Business(s) => &s.title,
        }
    }

    /// Modus.
    #[must_use]
    pub fn mode(&self) -> ScenarioMode {
        match self {
            Self::Classic(_) => ScenarioMode::Classic,
            Self::Business(_) => ScenarioMode::Business,
        }
    }

    /// Zweck (Business ohne `purpose`: Titel).
    #[must_use]
    pub fn purpose(&self) -> &str {
        match self {
            Self::Classic(s) => &s.purpose,
            Self::Business(s) => s.purpose.as_deref().unwrap_or(&s.title),
        }
    }

    /// Sprache (Default `de`).
    #[must_use]
    pub fn language(&self) -> &str {
        match self {
            Self::Classic(s) => s.language.as_deref().unwrap_or("de"),
            Self::Business(s) => s.language.as_deref().unwrap_or("de"),
        }
    }

    /// Rundenzahl (Business: Züge).
    #[must_use]
    pub fn rounds(&self) -> u32 {
        match self {
            Self::Classic(s) => s.rounds,
            Self::Business(s) => s.game.moves,
        }
    }

    /// Seed-Angabe.
    #[must_use]
    pub fn seed(&self) -> Option<&SeedSpec> {
        match self {
            Self::Classic(s) => s.seed.as_ref(),
            Self::Business(s) => s.seed.as_ref(),
        }
    }

    /// Regeln.
    #[must_use]
    pub fn rules(&self) -> &Rules {
        match self {
            Self::Classic(s) => &s.rules,
            Self::Business(s) => &s.rules,
        }
    }

    /// Sichtbarkeitsoptionen.
    #[must_use]
    pub fn visibility(&self) -> &VisibilitySettings {
        match self {
            Self::Classic(s) => &s.visibility,
            Self::Business(s) => &s.visibility,
        }
    }

    /// Öffentliche Lage (Business: Schlüsselfragen als Lage).
    #[must_use]
    pub fn public_situation(&self) -> String {
        match self {
            Self::Classic(s) => s.world.public_situation.clone(),
            Self::Business(s) => s
                .game
                .key_questions
                .iter()
                .map(|q| format!("{}: {}", q.id, q.text))
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    /// Sitzreihenfolge (= Zugreihenfolge / Auflösungspriorität).
    #[must_use]
    pub fn seat_order(&self) -> Vec<PlayerId> {
        match self {
            Self::Classic(s) => s
                .seating
                .order
                .iter()
                .map(|id| PlayerId::new(id.as_str()))
                .collect(),
            Self::Business(s) => s
                .teams
                .iter()
                .map(|t| PlayerId::new(t.id.as_str()))
                .collect(),
        }
    }

    /// Fraktionen in Sitzreihenfolge.
    #[must_use]
    pub fn factions(&self) -> Vec<FactionInfo> {
        match self {
            Self::Classic(s) => {
                let mut out = Vec::new();
                for id in &s.seating.order {
                    if let Some(f) = s.factions.iter().find(|f| &f.id == id) {
                        out.push(FactionInfo {
                            id: PlayerId::new(f.id.as_str()),
                            name: f.name.clone(),
                            level: f.level.clone(),
                            briefing: f.briefing.clone(),
                            public_goals: f.goals.public.clone(),
                            secret_goals: f.goals.secret.clone(),
                            assets: f.assets.clone(),
                            private_brief: None,
                        });
                    }
                }
                out
            }
            Self::Business(s) => s
                .teams
                .iter()
                .map(|t| FactionInfo {
                    id: PlayerId::new(t.id.as_str()),
                    name: t.display.clone(),
                    level: Some(format!("{:?}", t.role).to_lowercase()),
                    briefing: t.directive.clone().unwrap_or_default(),
                    public_goals: Vec::new(),
                    secret_goals: Vec::new(),
                    assets: Vec::new(),
                    private_brief: t.strategy_brief.clone(),
                })
                .collect(),
        }
    }

    /// Anzeigename eines Sitzes.
    #[must_use]
    pub fn display_name(&self, player: &PlayerId) -> String {
        self.factions()
            .into_iter()
            .find(|f| &f.id == player)
            .map_or_else(|| player.to_string(), |f| f.name)
    }

    /// Anfangszustand aller Weltvariablen.
    #[must_use]
    pub fn initial_vars(&self) -> Vec<WorldVar> {
        match self {
            Self::Classic(s) => classic_vars(&s.world),
            Self::Business(s) => business_vars(s),
        }
    }

    /// Für `round` fällige, deterministisch planbare Injects
    /// (klassisch: `round`; Business: `scheduled` mit `at.move == round`).
    #[must_use]
    pub fn injects_for_round(&self, round: u32) -> Vec<ScheduledInject> {
        match self {
            Self::Classic(s) => s
                .injects
                .iter()
                .filter(|i| i.round == round)
                .map(|i| ScheduledInject {
                    id: i.id.clone(),
                    round,
                    audiences: vec![i.audience.0.clone()],
                    text: i.text.clone(),
                    effects: i.effects.clone(),
                    attributed: i.attributed,
                })
                .collect(),
            Self::Business(s) => s
                .injects
                .iter()
                .filter(|i| {
                    i.kind == InjectKind::Scheduled
                        && i.at.as_ref().is_some_and(|at| at.move_number == round)
                })
                .map(|i| ScheduledInject {
                    id: i.id.clone(),
                    round,
                    audiences: i.audience.audiences(),
                    text: i.text.clone(),
                    effects: i.effects.clone(),
                    attributed: false,
                })
                .collect(),
        }
    }

    /// Validiert das Szenario; Rückgabe sind Warnungen.
    ///
    /// # Errors
    /// [`MatrixError::ScenarioInvalid`] mit allen Befunden.
    pub fn validate(&self) -> MatrixResult<Vec<String>> {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        match self {
            Self::Classic(s) => validate_classic(s, &mut errors, &mut warnings),
            Self::Business(s) => validate_business(s, &mut errors, &mut warnings),
        }
        validate_rules(self.rules(), &mut errors);
        self.validate_injects_effects(&mut errors);
        if errors.is_empty() {
            Ok(warnings)
        } else {
            Err(MatrixError::ScenarioInvalid(errors))
        }
    }

    fn validate_injects_effects(&self, errors: &mut Vec<String>) {
        let vars: BTreeMap<String, WorldVar> = self
            .initial_vars()
            .into_iter()
            .map(|v| (v.id.clone(), v))
            .collect();
        let ongoing: BTreeMap<String, Ongoing> = BTreeMap::new();
        let secrets: BTreeMap<String, SecretRecord> = BTreeMap::new();
        let players = self.seat_order();
        for round in 1..=self.rounds() {
            for inject in self.injects_for_round(round) {
                for audience in &inject.audiences {
                    let ctx = EffectContext {
                        vars: &vars,
                        ongoing: &ongoing,
                        secrets: &secrets,
                        players: &players,
                        rules: self.rules(),
                        argument_audience: audience.clone(),
                        secret_owner: None,
                    };
                    for violation in validate_effects(&ctx, &inject.effects) {
                        errors.push(format!(
                            "inject `{}` effects[{}]: {}",
                            inject.id, violation.index, violation.reason
                        ));
                    }
                }
            }
        }
    }
}

fn classic_vars(world: &WorldDef) -> Vec<WorldVar> {
    let mut out = Vec::new();
    for t in &world.tracks {
        out.push(WorldVar {
            id: t.id.clone(),
            label: t.label.clone(),
            visibility: t.visibility.clone(),
            value: VarValue::Track {
                value: t.start,
                min: t.min,
                max: t.max,
            },
        });
    }
    for s in &world.states {
        out.push(WorldVar {
            id: s.id.clone(),
            label: s.label.clone(),
            visibility: s.visibility.clone(),
            value: VarValue::State {
                value: s.start.clone(),
                values: s.values.clone(),
            },
        });
    }
    for o in &world.objects {
        let visibility = if o.hidden {
            VarVisibility::Umpire
        } else {
            o.visibility_when_found.clone()
        };
        out.push(WorldVar {
            id: o.id.clone(),
            label: o.label.clone(),
            visibility,
            value: VarValue::Object {
                hidden: o.hidden,
                protection: o.protection,
                visibility_when_found: o.visibility_when_found.clone(),
            },
        });
    }
    for p in &world.projects {
        out.push(WorldVar {
            id: p.id.clone(),
            label: p.label.clone(),
            visibility: p.visibility.clone(),
            value: VarValue::Project {
                progress: p.progress,
                stages: p.stages,
            },
        });
    }
    out
}

fn business_vars(s: &BusinessScenario) -> Vec<WorldVar> {
    let mut out = Vec::new();
    for team in &s.teams {
        let Some(start) = &team.start else { continue };
        let owner = PlayerId::new(team.id.as_str());
        if let Some(cash) = start.cash {
            out.push(WorldVar {
                id: format!("{}.cash", team.id),
                label: format!("Kasse {}", team.display),
                visibility: VarVisibility::Seat(owner.clone()),
                value: VarValue::Number { value: cash },
            });
        }
        if let Some(capacity) = start.capacity {
            out.push(WorldVar {
                id: format!("{}.capacity", team.id),
                label: format!("Kapazität {}", team.display),
                visibility: VarVisibility::Seat(owner.clone()),
                value: VarValue::Number { value: capacity },
            });
        }
        for (segment, share) in &start.share {
            out.push(WorldVar {
                id: format!("{}.share.{segment}", team.id),
                label: format!("Anteil {} ({segment})", team.display),
                visibility: VarVisibility::Public,
                value: VarValue::Number { value: *share },
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Laden
// ---------------------------------------------------------------------------

/// Geladenes, validiertes Szenario.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedScenario {
    /// Szenario.
    pub scenario: Scenario,
    /// Nicht blockierende Warnungen.
    pub warnings: Vec<String>,
    /// SHA-256 des TOML-Quelltexts (Hex).
    pub source_hash: String,
}

/// Parst und validiert ein Szenario aus TOML.
///
/// # Errors
/// [`MatrixError::ScenarioParse`] bei Syntax-/Strukturfehlern,
/// [`MatrixError::ScenarioInvalid`] bei Regelverletzungen.
pub fn load_scenario(source: &str) -> MatrixResult<LoadedScenario> {
    let table: toml::Table =
        toml::from_str(source).map_err(|e| MatrixError::ScenarioParse(e.to_string()))?;
    let schema = table
        .get("schema")
        .and_then(toml::Value::as_str)
        .unwrap_or_default();
    if schema != SCHEMA {
        return Err(MatrixError::ScenarioParse(format!(
            "schema `{schema}` statt `{SCHEMA}`"
        )));
    }
    let mode = match table.get("mode").and_then(toml::Value::as_str) {
        None | Some("classic") => ScenarioMode::Classic,
        Some("business") => ScenarioMode::Business,
        Some(other) => {
            return Err(MatrixError::ScenarioParse(format!(
                "mode `{other}` unbekannt (erlaubt: classic, business)"
            )));
        }
    };
    let scenario = match mode {
        ScenarioMode::Classic => Scenario::Classic(Box::new(
            toml::from_str::<ClassicScenario>(source)
                .map_err(|e| MatrixError::ScenarioParse(e.to_string()))?,
        )),
        ScenarioMode::Business => Scenario::Business(Box::new(
            toml::from_str::<BusinessScenario>(source)
                .map_err(|e| MatrixError::ScenarioParse(e.to_string()))?,
        )),
    };
    let warnings = scenario.validate()?;
    Ok(LoadedScenario {
        scenario,
        warnings,
        source_hash: to_hex(&sha256_parts(&[source.as_bytes()])),
    })
}

// ---------------------------------------------------------------------------
// Validierung
// ---------------------------------------------------------------------------

fn check_id(id: &str, context: &str, errors: &mut Vec<String>) {
    if id.trim().is_empty() {
        errors.push(format!("{context}: leere ID"));
    } else if id.contains([':', ',', ' ', '[', ']']) {
        errors.push(format!(
            "{context}: ID `{id}` enthält unzulässige Zeichen (: , Leerzeichen [ ])"
        ));
    }
}

fn check_visibility(
    vis: &VarVisibility,
    players: &BTreeSet<&str>,
    context: &str,
    errors: &mut Vec<String>,
) {
    for p in vis.referenced_players() {
        if !players.contains(p.as_str()) {
            errors.push(format!(
                "{context}: Sichtbarkeit verweist auf unbekannten Sitz `{p}`"
            ));
        }
    }
}

fn validate_rules(rules: &Rules, errors: &mut Vec<String>) {
    if !(1..=2).contains(&rules.max_track_step) {
        errors.push(format!(
            "rules.max_track_step = {} (erlaubt 1..=2)",
            rules.max_track_step
        ));
    }
    if !(0..=8).contains(&rules.auto_success_net) {
        errors.push(format!(
            "rules.auto_success_net = {} (erlaubt 0..=8)",
            rules.auto_success_net
        ));
    }
    if rules.negotiation.enabled {
        if rules.negotiation.max_exchanges == 0 {
            errors.push("rules.negotiation.max_exchanges muss ≥ 1 sein".to_owned());
        }
        if rules.negotiation.max_message_chars == 0 {
            errors.push("rules.negotiation.max_message_chars muss ≥ 1 sein".to_owned());
        }
        if rules.negotiation.max_channels_per_seat == 0 {
            errors.push("rules.negotiation.max_channels_per_seat muss ≥ 1 sein".to_owned());
        }
    }
}

fn validate_classic(s: &ClassicScenario, errors: &mut Vec<String>, warnings: &mut Vec<String>) {
    if s.mode != ScenarioMode::Classic {
        errors.push("mode muss `classic` sein".to_owned());
    }
    check_id(&s.id, "id", errors);
    if s.title.trim().is_empty() {
        errors.push("title ist leer".to_owned());
    }
    if s.purpose.trim().is_empty() {
        errors.push("purpose ist leer (Pflichtfeld, erste Zeile jedes Prompts)".to_owned());
    }
    if s.rounds == 0 {
        errors.push("rounds muss ≥ 1 sein".to_owned());
    } else if !(6..=8).contains(&s.rounds) {
        warnings.push(format!(
            "rounds = {} — empfohlen sind 6–8 (Curry & Price)",
            s.rounds
        ));
    }

    // Fraktionen
    if s.factions.len() != PLAYER_SEATS {
        errors.push(format!(
            "genau {PLAYER_SEATS} Fraktionen erforderlich, gefunden {}",
            s.factions.len()
        ));
    }
    let mut faction_ids: BTreeSet<&str> = BTreeSet::new();
    for f in &s.factions {
        check_id(&f.id, "factions.id", errors);
        if !faction_ids.insert(f.id.as_str()) {
            errors.push(format!("Fraktion `{}` doppelt", f.id));
        }
        if f.name.trim().is_empty() {
            errors.push(format!("Fraktion `{}`: name leer", f.id));
        }
        if f.goals.public.len() > MAX_GOALS {
            errors.push(format!(
                "Fraktion `{}`: mehr als {MAX_GOALS} öffentliche Ziele",
                f.id
            ));
        }
        if f.goals.secret.len() > MAX_GOALS {
            errors.push(format!(
                "Fraktion `{}`: mehr als {MAX_GOALS} geheime Ziele",
                f.id
            ));
        }
    }
    let levels: BTreeSet<&str> = s
        .factions
        .iter()
        .filter_map(|f| f.level.as_deref())
        .collect();
    if levels.len() > 1 {
        warnings.push(format!(
            "uneinheitliche Fraktionsebenen ({}) — Rollen sollten auf ähnlicher Ebene operieren",
            levels.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }

    // Sitzordnung
    let order: BTreeSet<&str> = s.seating.order.iter().map(String::as_str).collect();
    if order.len() != s.seating.order.len() {
        errors.push("seating.order enthält Duplikate".to_owned());
    }
    if order != faction_ids {
        errors.push("seating.order muss genau die Fraktions-IDs enthalten".to_owned());
    }

    // Welt
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut add_var = |id: &str, errors: &mut Vec<String>| {
        check_id(id, "world", errors);
        if !seen.insert(id.to_owned()) {
            errors.push(format!("Weltvariable `{id}` doppelt"));
        }
    };
    for t in &s.world.tracks {
        add_var(&t.id, errors);
        if t.min >= t.max {
            errors.push(format!(
                "Track `{}`: min ({}) muss < max ({}) sein",
                t.id, t.min, t.max
            ));
        }
        if t.start < t.min || t.start > t.max {
            errors.push(format!(
                "Track `{}`: start {} außerhalb [{}, {}]",
                t.id, t.start, t.min, t.max
            ));
        }
        check_visibility(
            &t.visibility,
            &faction_ids,
            &format!("Track `{}`", t.id),
            errors,
        );
    }
    if s.world.tracks.len() > TRACK_WARNING_THRESHOLD {
        warnings.push(format!(
            "{} Tracks — mehr als {TRACK_WARNING_THRESHOLD} machen das Spiel schwer lesbar",
            s.world.tracks.len()
        ));
    }
    for st in &s.world.states {
        add_var(&st.id, errors);
        if st.values.is_empty() {
            errors.push(format!("State `{}`: values leer", st.id));
        }
        let distinct: BTreeSet<&String> = st.values.iter().collect();
        if distinct.len() != st.values.len() {
            errors.push(format!("State `{}`: values nicht eindeutig", st.id));
        }
        if !st.values.contains(&st.start) {
            errors.push(format!(
                "State `{}`: start `{}` nicht in values",
                st.id, st.start
            ));
        }
        check_visibility(
            &st.visibility,
            &faction_ids,
            &format!("State `{}`", st.id),
            errors,
        );
    }
    for o in &s.world.objects {
        add_var(&o.id, errors);
        check_visibility(
            &o.visibility_when_found,
            &faction_ids,
            &format!("Objekt `{}`", o.id),
            errors,
        );
    }
    for p in &s.world.projects {
        add_var(&p.id, errors);
        if !(1..=3).contains(&p.stages) {
            errors.push(format!(
                "Projekt `{}`: stages {} (erlaubt 1..=3)",
                p.id, p.stages
            ));
        }
        if p.progress > p.stages {
            errors.push(format!("Projekt `{}`: progress > stages", p.id));
        }
        check_visibility(
            &p.visibility,
            &faction_ids,
            &format!("Projekt `{}`", p.id),
            errors,
        );
    }
    // Injects
    let mut inject_ids: BTreeSet<&str> = BTreeSet::new();
    for i in &s.injects {
        if !inject_ids.insert(i.id.as_str()) {
            errors.push(format!("Inject `{}` doppelt", i.id));
        }
        if i.round == 0 || i.round > s.rounds {
            errors.push(format!(
                "Inject `{}`: round {} außerhalb 1..={}",
                i.id, i.round, s.rounds
            ));
        }
        if i.text.trim().is_empty() {
            errors.push(format!("Inject `{}`: text leer", i.id));
        }
        for p in i.audience.0.players() {
            if !faction_ids.contains(p.as_str()) {
                errors.push(format!("Inject `{}`: unbekannter Sitz `{p}`", i.id));
            }
        }
    }
}

/// Grobe Syntaxprüfung eines Prädikats (`when`): nicht leer, Klammern und
/// Anführungszeichen ausgeglichen. Die Auswertung selbst folgt mit dem
/// Marktmodell (offene Entscheidung, siehe Crate-Doku).
#[must_use]
pub fn predicate_syntax_ok(expr: &str) -> bool {
    if expr.trim().is_empty() {
        return false;
    }
    let mut depth: i32 = 0;
    let mut quote: Option<char> = None;
    for c in expr.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => quote = Some(c),
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth < 0 {
                        return false;
                    }
                }
                _ => {}
            },
        }
    }
    depth == 0 && quote.is_none()
}

fn validate_business(s: &BusinessScenario, errors: &mut Vec<String>, warnings: &mut Vec<String>) {
    if s.mode != ScenarioMode::Business {
        errors.push("mode muss `business` sein".to_owned());
    }
    check_id(&s.id, "id", errors);
    if s.title.trim().is_empty() {
        errors.push("title ist leer".to_owned());
    }
    let moves = s.game.moves;
    if moves == 0 {
        errors.push("game.moves muss ≥ 1 sein".to_owned());
    }
    if !s.game.move_labels.is_empty()
        && usize::try_from(moves).ok() != Some(s.game.move_labels.len())
    {
        errors.push(format!(
            "game.move_labels hat {} Einträge, erwartet {moves}",
            s.game.move_labels.len()
        ));
    }

    // Schlüsselfragen
    if s.game.key_questions.is_empty() {
        errors.push("game.key_questions: mindestens eine Schlüsselfrage erforderlich".to_owned());
    }
    let mut kq_ids = BTreeSet::new();
    for q in &s.game.key_questions {
        if !kq_ids.insert(q.id.as_str()) {
            errors.push(format!("Schlüsselfrage `{}` doppelt", q.id));
        }
        if q.text.trim().is_empty() {
            errors.push(format!("Schlüsselfrage `{}`: text leer", q.id));
        }
    }

    // Teams
    if s.teams.len() != PLAYER_SEATS {
        errors.push(format!(
            "genau {PLAYER_SEATS} Teams (Spieler-Sitze) erforderlich, gefunden {}",
            s.teams.len()
        ));
    }
    let mut team_ids: BTreeSet<&str> = BTreeSet::new();
    for t in &s.teams {
        check_id(&t.id, "teams.id", errors);
        if !team_ids.insert(t.id.as_str()) {
            errors.push(format!("Team `{}` doppelt", t.id));
        }
        if t.display.trim().is_empty() {
            errors.push(format!("Team `{}`: display leer", t.id));
        }
        if let Some(scale) = &t.assessment_scale {
            let ok = scale.len() == 2 && scale.first() < scale.get(1);
            if !ok {
                errors.push(format!(
                    "Team `{}`: assessment_scale muss [min, max] sein",
                    t.id
                ));
            }
        }
    }
    let companies = s
        .teams
        .iter()
        .filter(|t| t.role == TeamRole::Company)
        .count();
    if companies != 1 {
        errors.push(format!(
            "genau ein Team mit role = \"company\" erforderlich, gefunden {companies}"
        ));
    }
    let markets = s
        .teams
        .iter()
        .filter(|t| t.role == TeamRole::Market)
        .count();
    match s.business.market_role {
        MarketRole::Player if markets != 1 => errors.push(format!(
            "market_role = \"player\" erfordert genau ein Team mit role = \"market\", gefunden {markets}"
        )),
        MarketRole::WhiteCell if markets != 0 => errors.push(format!(
            "market_role = \"white_cell\" erlaubt kein Team mit role = \"market\", gefunden {markets}"
        )),
        _ => {}
    }

    // Segmente und Startanteile
    let mut segment_ids: BTreeSet<&str> = BTreeSet::new();
    for seg in &s.business.segments {
        check_id(&seg.id, "business.segments.id", errors);
        if !segment_ids.insert(seg.id.as_str()) {
            errors.push(format!("Segment `{}` doppelt", seg.id));
        }
        if usize::try_from(moves).ok() != Some(seg.size_meur.len()) {
            errors.push(format!(
                "Segment `{}`: size_meur hat {} Einträge, erwartet {moves} (einer je Zug)",
                seg.id,
                seg.size_meur.len()
            ));
        }
        if seg.size_meur.iter().any(|v| !v.is_finite() || *v < 0.0) {
            errors.push(format!("Segment `{}`: size_meur muss ≥ 0 sein", seg.id));
        }
    }
    let mut share_sums: BTreeMap<&str, f64> = BTreeMap::new();
    for t in &s.teams {
        let Some(start) = &t.start else { continue };
        for (seg, share) in &start.share {
            if !segment_ids.contains(seg.as_str()) {
                errors.push(format!(
                    "Team `{}`: Startanteil für unbekanntes Segment `{seg}`",
                    t.id
                ));
            }
            if !share.is_finite() || *share < 0.0 {
                errors.push(format!(
                    "Team `{}`: Startanteil `{seg}` muss ≥ 0 sein",
                    t.id
                ));
            }
            *share_sums.entry(seg.as_str()).or_insert(0.0) += *share;
        }
        if start.cash.is_some_and(|c| !c.is_finite()) {
            errors.push(format!("Team `{}`: cash ungültig", t.id));
        }
    }
    for (seg, sum) in share_sums {
        if sum > 1.0 + 1e-9 {
            errors.push(format!(
                "Segment `{seg}`: Summe der Startanteile {sum:.3} > 1"
            ));
        }
    }

    // Aktionsarten, Regeln, Modell
    for kind in &s.business.action_kinds {
        if !BUSINESS_ACTION_KINDS.contains(&kind.as_str()) {
            errors.push(format!(
                "business.action_kinds: unbekannte Aktionsart `{kind}`"
            ));
        }
    }
    let mut rule_ids = BTreeSet::new();
    for r in &s.business.rules {
        if !rule_ids.insert(r.id.as_str()) {
            errors.push(format!("business.rules `{}` doppelt", r.id));
        }
        if !predicate_syntax_ok(&r.when) {
            errors.push(format!("business.rules `{}`: when nicht parsebar", r.id));
        }
        if r.effect.trim().is_empty() {
            errors.push(format!("business.rules `{}`: effect leer", r.id));
        }
    }
    if let Some(model) = &s.business.market_model {
        if model.weights.values().any(|w| !w.is_finite() || *w < 0.0) {
            errors.push("business.market_model.weights müssen ≥ 0 sein".to_owned());
        }
        if !model.beta.is_finite() {
            errors.push("business.market_model.beta ungültig".to_owned());
        }
    }

    // Stakeholder
    let mut stakeholder_ids = BTreeSet::new();
    for st in &s.business.stakeholders {
        if !stakeholder_ids.insert(st.id.as_str()) {
            errors.push(format!("Stakeholder `{}` doppelt", st.id));
        }
        if team_ids.contains(st.id.as_str()) {
            errors.push(format!("Stakeholder `{}` kollidiert mit Team-ID", st.id));
        }
        if st.played_by != "gamemaster" && !team_ids.contains(st.played_by.as_str()) {
            errors.push(format!(
                "Stakeholder `{}`: played_by `{}` unbekannt",
                st.id, st.played_by
            ));
        }
    }

    // Offenlegung
    for (i, d) in s.business.disclosure.iter().enumerate() {
        d.audience
            .check(&team_ids, &format!("business.disclosure[{i}]"), errors);
        if d.on.trim().is_empty() {
            errors.push(format!("business.disclosure[{i}]: on leer"));
        }
    }

    // Injects
    let mut inject_ids = BTreeSet::new();
    for i in &s.injects {
        let ctx = format!("Inject `{}`", i.id);
        if !inject_ids.insert(i.id.as_str()) {
            errors.push(format!("{ctx} doppelt"));
        }
        if i.text.trim().is_empty() {
            errors.push(format!("{ctx}: text leer"));
        }
        i.audience.check(&team_ids, &ctx, errors);
        if let Some(at) = &i.at {
            if at.move_number == 0 || at.move_number > moves {
                errors.push(format!(
                    "{ctx}: at.move {} außerhalb 1..={moves}",
                    at.move_number
                ));
            }
            if !BUSINESS_MOVE_PHASES.contains(&at.phase.as_str()) {
                errors.push(format!("{ctx}: at.phase `{}` unbekannt", at.phase));
            }
        }
        if let Some(when) = &i.when {
            if !predicate_syntax_ok(when) {
                errors.push(format!("{ctx}: when nicht parsebar"));
            }
        }
        match i.kind {
            InjectKind::Scheduled if i.at.is_none() => {
                errors.push(format!("{ctx}: scheduled erfordert at"));
            }
            InjectKind::Conditional if i.when.is_none() => {
                errors.push(format!("{ctx}: conditional erfordert when"));
            }
            InjectKind::Random => {
                if i.at.is_none() {
                    errors.push(format!("{ctx}: random erfordert at"));
                }
                match i.p {
                    Some(p) if (0.0..=1.0).contains(&p) => {}
                    _ => errors.push(format!("{ctx}: random erfordert p in [0, 1]")),
                }
            }
            _ => {}
        }
    }

    if s.debrief.is_none() {
        warnings.push(
            "kein [debrief]-Abschnitt — Debriefing ist der wichtigste Teil des Spiels".to_owned(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    use crate::test_support::{CLOUD, KARST};

    #[test]
    fn karst_example_loads() -> TestResult {
        let loaded = load_scenario(KARST)?;
        assert_eq!(loaded.scenario.mode(), ScenarioMode::Classic);
        assert_eq!(loaded.scenario.id(), "karst-wasserkrise");
        assert_eq!(loaded.scenario.rounds(), 6);
        assert_eq!(
            loaded.scenario.seat_order(),
            vec![
                PlayerId::new("rat"),
                PlayerId::new("gilde"),
                PlayerId::new("nord"),
                PlayerId::new("mission")
            ]
        );
        assert!(loaded.scenario.rules().fail_chits);
        assert!(loaded.scenario.rules().epilogue);
        assert_eq!(loaded.scenario.initial_vars().len(), 7 + 1 + 1 + 1);
        // uneinheitliche Ebenen sind nur eine Warnung
        assert!(loaded.warnings.iter().any(|w| w.contains("Ebenen")));
        assert_eq!(loaded.source_hash.len(), 64);
        assert_eq!(loaded.scenario.injects_for_round(3).len(), 1);
        Ok(())
    }

    #[test]
    fn karst_hidden_vars_have_owners() -> TestResult {
        let loaded = load_scenario(KARST)?;
        let vars = loaded.scenario.initial_vars();
        let smuggling = vars
            .iter()
            .find(|v| v.id == "smuggling_net")
            .ok_or("smuggling_net fehlt")?;
        assert_eq!(smuggling.visibility.owner(), Some(&PlayerId::new("gilde")));
        let reservoir = vars
            .iter()
            .find(|v| v.id == "reservoir_altmark")
            .ok_or("reservoir fehlt")?;
        assert_eq!(reservoir.visibility, VarVisibility::Umpire);
        Ok(())
    }

    #[test]
    fn business_example_loads() -> TestResult {
        let loaded = load_scenario(CLOUD)?;
        assert_eq!(loaded.scenario.mode(), ScenarioMode::Business);
        assert_eq!(loaded.scenario.rounds(), 3);
        assert_eq!(loaded.scenario.seat_order().len(), 4);
        let Scenario::Business(b) = &loaded.scenario else {
            return Err("kein Business-Szenario".into());
        };
        assert_eq!(b.game.key_questions.len(), 3);
        assert_eq!(b.business.segments.len(), 2);
        assert_eq!(b.injects.len(), 4);
        assert_eq!(loaded.scenario.injects_for_round(2).len(), 1);
        // Kasse ist teamprivat, Anteile sind öffentlich
        let vars = loaded.scenario.initial_vars();
        let cash = vars
            .iter()
            .find(|v| v.id == "company.cash")
            .ok_or("cash fehlt")?;
        assert_eq!(
            cash.visibility,
            VarVisibility::Seat(PlayerId::new("company"))
        );
        let share = vars
            .iter()
            .find(|v| v.id == "company.share.sme")
            .ok_or("share fehlt")?;
        assert_eq!(share.visibility, VarVisibility::Public);
        Ok(())
    }

    #[test]
    fn integer_and_hex_seeds() {
        let a = SeedSpec::Number(20_270_101).master_seed();
        assert!(a.is_some());
        assert_eq!(a, SeedSpec::Number(20_270_101).master_seed());
        assert_ne!(a, SeedSpec::Number(20_270_102).master_seed());
        assert_eq!(SeedSpec::Text(String::new()).master_seed(), None);
        let hex = "00".repeat(31) + "01";
        let mut expected = [0u8; 32];
        expected[31] = 1;
        assert_eq!(SeedSpec::Text(hex).master_seed(), Some(expected));
    }

    fn classic_with(replace: &str, with: &str) -> Result<String, Box<dyn std::error::Error>> {
        if !KARST.contains(replace) {
            return Err(format!("Muster `{replace}` fehlt").into());
        }
        Ok(KARST.replacen(replace, with, 1))
    }

    fn invalid_errors(src: &str) -> Vec<String> {
        match load_scenario(src) {
            Err(MatrixError::ScenarioInvalid(errs)) => errs,
            other => vec![format!("UNERWARTET: {other:?}")],
        }
    }

    #[test]
    fn rejects_wrong_schema_and_mode() -> TestResult {
        let src = classic_with(
            "schema = \"harwness.matrix-scenario/v1\"",
            "schema = \"x/v2\"",
        )?;
        assert!(matches!(
            load_scenario(&src),
            Err(MatrixError::ScenarioParse(_))
        ));
        let src = classic_with(
            "schema = \"harwness.matrix-scenario/v1\"",
            "schema = \"harwness.matrix-scenario/v1\"\nmode = \"chess\"",
        )?;
        assert!(matches!(
            load_scenario(&src),
            Err(MatrixError::ScenarioParse(_))
        ));
        Ok(())
    }

    #[test]
    fn rejects_unknown_fields() -> TestResult {
        let src = classic_with("rounds = 6", "rounds = 6\nbogus = 1")?;
        assert!(matches!(
            load_scenario(&src),
            Err(MatrixError::ScenarioParse(_))
        ));
        Ok(())
    }

    #[test]
    fn rejects_start_out_of_range() -> TestResult {
        let src = classic_with(
            "min = -3, max = 3, start = 0,  visibility = \"public\"",
            "min = -3, max = 3, start = 9,  visibility = \"public\"",
        )?;
        let errs = invalid_errors(&src);
        assert!(errs.iter().any(|e| e.contains("außerhalb")), "{errs:?}");
        Ok(())
    }

    #[test]
    fn rejects_unknown_visibility_seat() -> TestResult {
        let src = classic_with(
            "visibility = \"seat:gilde\"",
            "visibility = \"seat:piraten\"",
        )?;
        let errs = invalid_errors(&src);
        assert!(errs.iter().any(|e| e.contains("piraten")), "{errs:?}");
        Ok(())
    }

    #[test]
    fn rejects_empty_purpose_and_big_project() -> TestResult {
        let src = classic_with("stages = 3, progress = 0", "stages = 4, progress = 0")?;
        let errs = invalid_errors(&src);
        assert!(errs.iter().any(|e| e.contains("stages")), "{errs:?}");
        let start = KARST.find("purpose = ").ok_or("purpose fehlt")?;
        let end = KARST[start..].find('\n').ok_or("Zeilenende fehlt")? + start;
        let src = format!("{}purpose = \"\"{}", &KARST[..start], &KARST[end..]);
        let errs = invalid_errors(&src);
        assert!(errs.iter().any(|e| e.contains("purpose")), "{errs:?}");
        Ok(())
    }

    #[test]
    fn rejects_too_many_goals_and_bad_inject_effect() -> TestResult {
        let src = classic_with(
            "goals.public = [\"Als Helfer in der Not wahrgenommen werden\"]",
            "goals.public = [\"a\", \"b\", \"c\", \"d\", \"e\"]",
        )?;
        let errs = invalid_errors(&src);
        assert!(
            errs.iter().any(|e| e.contains("öffentliche Ziele")),
            "{errs:?}"
        );
        let src = classic_with(
            "effects = [{ op = \"add\", var = \"water\", by = -1 }]",
            "effects = [{ op = \"add\", var = \"wasser\", by = -1 }]",
        )?;
        let errs = invalid_errors(&src);
        assert!(errs.iter().any(|e| e.contains("hitzewelle")), "{errs:?}");
        let src = classic_with(
            "effects = [{ op = \"add\", var = \"water\", by = -1 }]",
            "effects = [{ op = \"add\", var = \"water\", by = -2 }]",
        )?;
        let errs = invalid_errors(&src);
        assert!(
            errs.iter().any(|e| e.contains("max_track_step")),
            "{errs:?}"
        );
        Ok(())
    }

    #[test]
    fn rejects_three_factions() -> TestResult {
        let start = KARST
            .find("[[factions]]\nid = \"mission\"")
            .ok_or("mission fehlt")?;
        let end = KARST.find("[seating]").ok_or("seating fehlt")?;
        let src = format!("{}{}", &KARST[..start], &KARST[end..]).replace(
            "order = [\"rat\", \"gilde\", \"nord\", \"mission\"]",
            "order = [\"rat\", \"gilde\", \"nord\"]",
        );
        let errs = invalid_errors(&src);
        assert!(
            errs.iter().any(|e| e.contains("genau 4 Fraktionen")),
            "{errs:?}"
        );
        Ok(())
    }

    fn business_with(replace: &str, with: &str) -> Result<String, Box<dyn std::error::Error>> {
        if !CLOUD.contains(replace) {
            return Err(format!("Muster `{replace}` fehlt").into());
        }
        Ok(CLOUD.replacen(replace, with, 1))
    }

    #[test]
    fn business_requires_key_questions_and_company() -> TestResult {
        let start = CLOUD.find("[[game.key_questions]]").ok_or("kq fehlt")?;
        let end = CLOUD.find("[business]").ok_or("business fehlt")?;
        let src = format!("{}{}", &CLOUD[..start], &CLOUD[end..]);
        let errs = invalid_errors(&src);
        assert!(
            errs.iter().any(|e| e.contains("Schlüsselfrage")),
            "{errs:?}"
        );

        let src = business_with("role = \"company\"", "role = \"competitor\"")?;
        let errs = invalid_errors(&src);
        assert!(errs.iter().any(|e| e.contains("company")), "{errs:?}");
        Ok(())
    }

    #[test]
    fn business_market_role_consistency() -> TestResult {
        let src = business_with("market_role = \"player\"", "market_role = \"white_cell\"")?;
        let errs = invalid_errors(&src);
        assert!(errs.iter().any(|e| e.contains("white_cell")), "{errs:?}");
        Ok(())
    }

    #[test]
    fn business_share_sum_and_segment_sizes() -> TestResult {
        let src = business_with("share = { sme = 0.05,", "share = { sme = 0.55,")?;
        let errs = invalid_errors(&src);
        assert!(
            errs.iter().any(|e| e.contains("Summe der Startanteile")),
            "{errs:?}"
        );
        let src = business_with("size_meur = [800, 950, 1300]", "size_meur = [800, 950]")?;
        let errs = invalid_errors(&src);
        assert!(errs.iter().any(|e| e.contains("size_meur")), "{errs:?}");
        Ok(())
    }

    #[test]
    fn business_inject_rules() -> TestResult {
        let src = business_with("p = 0.25", "p = 1.5")?;
        let errs = invalid_errors(&src);
        assert!(errs.iter().any(|e| e.contains("INJ-3")), "{errs:?}");
        let src = business_with(
            "audience = [\"competitor_b\"]",
            "audience = [\"competitor_z\"]",
        )?;
        let errs = invalid_errors(&src);
        assert!(errs.iter().any(|e| e.contains("competitor_z")), "{errs:?}");
        let src = business_with("when = \"cash < 0\"", "when = \"cash < (0\"")?;
        let errs = invalid_errors(&src);
        assert!(errs.iter().any(|e| e.contains("cash_floor")), "{errs:?}");
        Ok(())
    }

    #[test]
    fn predicate_checker() {
        assert!(predicate_syntax_ok("share('a','sme') > 0.25"));
        assert!(predicate_syntax_ok("x > 1 && (y < 2)"));
        assert!(!predicate_syntax_ok(""));
        assert!(!predicate_syntax_ok("share('a > 1"));
        assert!(!predicate_syntax_ok("(a > 1"));
        assert!(!predicate_syntax_ok("a > 1)"));
    }

    #[test]
    fn var_visibility_roundtrip() -> TestResult {
        for text in ["public", "umpire", "seat:gilde", "seats:gilde,nord"] {
            let v = VarVisibility::parse(text)?;
            assert_eq!(v.to_string(), text);
        }
        assert_eq!(
            VarVisibility::parse("seats:[nord, gilde]")?,
            VarVisibility::Seats(vec![PlayerId::new("gilde"), PlayerId::new("nord")])
        );
        assert_eq!(
            VarVisibility::parse("seats:gilde")?,
            VarVisibility::Seat(PlayerId::new("gilde"))
        );
        assert!(VarVisibility::parse("seat:").is_err());
        assert!(VarVisibility::parse("everyone").is_err());
        Ok(())
    }
}
