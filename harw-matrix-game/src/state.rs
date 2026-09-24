//! Spielzustand, Sitze, Audiences, Journal und Replay
//! (matrix-game.md §2.2, §3, §4.3, §4.4, §9.2).
//!
//! Grundsatz: Der Zustand wird **ausschließlich** über Journal-Einträge
//! verändert ([`GameState::apply`]). Der GameMaster erzeugt Einträge,
//! [`GameLog::record`] wendet sie an und hängt sie an. Ein Replay
//! ([`replay`]) wendet dieselben Einträge ohne Modellaufrufe erneut an und
//! prüft dabei Würfel und `state_hash` jedes Rundenendes.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Write as _;
use std::path::Path;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::commitments::{Commitment, canonical_json, from_hex, sha256_parts, to_hex};
use crate::dice::{self, DiceRoll, Grade, Outcome};
use crate::error::{MatrixError, MatrixResult};
use crate::phases::{ArgumentBody, CounterEntry, EffectOp, Phase, UmpireRuling};
use crate::scenario::{Scenario, ScenarioMode, VarVisibility};

// ---------------------------------------------------------------------------
// Sitze und Audiences
// ---------------------------------------------------------------------------

/// Kennung eines Spieler-Sitzes (= Fraktions- bzw. Team-ID).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PlayerId(pub String);

impl PlayerId {
    /// Neue Kennung.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Textform.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PlayerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Ein Sitz am Tisch. Der Mensch (Facilitator/Beobachter) ist **kein** Sitz
/// und sieht immer alles.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Seat {
    /// Spieler-Agent.
    Player(PlayerId),
    /// Umpire-Agent.
    Umpire,
}

impl Seat {
    /// Spieler-Sitz.
    #[must_use]
    pub fn player(id: impl Into<String>) -> Self {
        Self::Player(PlayerId::new(id))
    }
}

impl fmt::Display for Seat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Player(p) => write!(f, "{p}"),
            Self::Umpire => f.write_str("umpire"),
        }
    }
}

/// Wer einen Journal-Eintrag sehen darf (matrix-game.md §2.2).
///
/// `ObserverOnly` ergänzt die fünf Sitz-Audiences um Einträge, die nur der
/// Mensch sieht (Master-Seed, `state_hash`, Leak-Befunde, verworfene
/// Effekte) — kein Sitz, auch nicht der Umpire.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "of", rename_all = "snake_case")]
pub enum Audience {
    /// Alle Sitze.
    Public,
    /// Genau diese zwei Spieler (normalisiert: a < b).
    Pair(PlayerId, PlayerId),
    /// Nur der Umpire.
    UmpireOnly,
    /// Genau ein Spieler.
    Seat(PlayerId),
    /// Ein Spieler und der Umpire.
    SeatAndUmpire(PlayerId),
    /// Nur der menschliche Facilitator/Beobachter.
    ObserverOnly,
}

impl Audience {
    /// Normalisiertes Paar (`a < b`).
    #[must_use]
    pub fn pair(a: PlayerId, b: PlayerId) -> Self {
        if a <= b { Self::Pair(a, b) } else { Self::Pair(b, a) }
    }

    /// Parst die Textform: `public`/`all`, `umpire`, `observer`, `seat:x`,
    /// `seat+umpire:x`, `pair:a,b`.
    ///
    /// # Errors
    /// Beschreibung des Formatfehlers.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        match text {
            "public" | "all" => return Ok(Self::Public),
            "umpire" => return Ok(Self::UmpireOnly),
            "observer" => return Ok(Self::ObserverOnly),
            _ => {}
        }
        if let Some(id) = text.strip_prefix("seat+umpire:") {
            let id = id.trim();
            if id.is_empty() {
                return Err(format!("Audience `{text}` ohne Sitz"));
            }
            return Ok(Self::SeatAndUmpire(PlayerId::new(id)));
        }
        if let Some(id) = text.strip_prefix("seat:") {
            let id = id.trim();
            if id.is_empty() {
                return Err(format!("Audience `{text}` ohne Sitz"));
            }
            return Ok(Self::Seat(PlayerId::new(id)));
        }
        if let Some(rest) = text.strip_prefix("pair:") {
            let parts: Vec<&str> = rest.split(',').map(str::trim).collect();
            return match parts.as_slice() {
                [a, b] if !a.is_empty() && !b.is_empty() && a != b => {
                    Ok(Self::pair(PlayerId::new(*a), PlayerId::new(*b)))
                }
                _ => Err(format!("Audience `{text}`: pair braucht zwei verschiedene Sitze")),
            };
        }
        Err(format!(
            "unbekannte Audience `{text}` (erlaubt: public, umpire, observer, seat:x, seat+umpire:x, pair:a,b)"
        ))
    }

    /// Öffentlich?
    #[must_use]
    pub fn is_public(&self) -> bool {
        matches!(self, Self::Public)
    }

    /// Alle genannten Spieler.
    #[must_use]
    pub fn players(&self) -> Vec<&PlayerId> {
        match self {
            Self::Public | Self::UmpireOnly | Self::ObserverOnly => Vec::new(),
            Self::Pair(a, b) => vec![a, b],
            Self::Seat(p) | Self::SeatAndUmpire(p) => vec![p],
        }
    }

    /// Mitgliedschaft ohne Konfiguration (Paar-Kanäle ohne Umpire-Zugriff).
    #[must_use]
    pub fn raw_includes(&self, seat: &Seat) -> bool {
        match (self, seat) {
            (Self::Public, _) => true,
            (Self::Pair(a, b), Seat::Player(p)) => p == a || p == b,
            (Self::UmpireOnly | Self::SeatAndUmpire(_), Seat::Umpire) => true,
            (Self::Seat(q) | Self::SeatAndUmpire(q), Seat::Player(p)) => p == q,
            _ => false,
        }
    }

    /// Mitglieder über einer gegebenen Spielerliste.
    #[must_use]
    pub fn members(&self, players: &[PlayerId]) -> BTreeSet<Seat> {
        let mut all: Vec<Seat> = players.iter().cloned().map(Seat::Player).collect();
        all.push(Seat::Umpire);
        all.into_iter().filter(|s| self.raw_includes(s)).collect()
    }

    /// Ist diese Audience höchstens so weit wie `outer`?
    #[must_use]
    pub fn is_within(&self, outer: &Self, players: &[PlayerId]) -> bool {
        if matches!(self, Self::ObserverOnly) {
            return true;
        }
        self.members(players).is_subset(&outer.members(players))
    }
}

impl fmt::Display for Audience {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Public => f.write_str("public"),
            Self::Pair(a, b) => write!(f, "pair:{a},{b}"),
            Self::UmpireOnly => f.write_str("umpire"),
            Self::Seat(p) => write!(f, "seat:{p}"),
            Self::SeatAndUmpire(p) => write!(f, "seat+umpire:{p}"),
            Self::ObserverOnly => f.write_str("observer"),
        }
    }
}

/// Audience in Textform für TOML/JSON-Contracts (`"public"`, `"seat:x"` …).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AudienceSpec(pub Audience);

impl Default for AudienceSpec {
    fn default() -> Self {
        Self(Audience::Public)
    }
}

impl TryFrom<String> for AudienceSpec {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Audience::parse(&value).map(Self)
    }
}

impl From<AudienceSpec> for String {
    fn from(value: AudienceSpec) -> Self {
        value.0.to_string()
    }
}

// ---------------------------------------------------------------------------
// Welt
// ---------------------------------------------------------------------------

/// Wert einer Weltvariablen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VarValue {
    /// Numerischer Track mit Schranken.
    Track {
        /// Aktueller Wert.
        value: i32,
        /// Untergrenze.
        min: i32,
        /// Obergrenze.
        max: i32,
    },
    /// Diskreter Zustand.
    State {
        /// Aktueller Wert.
        value: String,
        /// Erlaubte Werte.
        values: Vec<String>,
    },
    /// Verdecktes/geschütztes Objekt.
    Object {
        /// Noch verdeckt.
        hidden: bool,
        /// Verbleibende Schutzstufen.
        protection: u8,
        /// Sichtbarkeit nach dem Auffinden.
        visibility_when_found: VarVisibility,
    },
    /// Big Project.
    Project {
        /// Fortschritt.
        progress: u8,
        /// Stufen.
        stages: u8,
    },
    /// Freie Kennzahl (Business-KPIs; nur über das Marktmodell änderbar).
    Number {
        /// Wert.
        value: f64,
    },
}

impl VarValue {
    /// Kurzdarstellung für Protokoll und AAR.
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Self::Track { value, min, max } => format!("{value} [{min}..{max}]"),
            Self::State { value, .. } => value.clone(),
            Self::Object {
                hidden, protection, ..
            } => {
                if *hidden {
                    format!("verdeckt, Schutz {protection}")
                } else {
                    format!("gefunden, Schutz {protection}")
                }
            }
            Self::Project { progress, stages } => format!("{progress}/{stages}"),
            Self::Number { value } => format!("{value}"),
        }
    }
}

/// Weltvariable mit eigener Sichtbarkeit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldVar {
    /// Kennung.
    pub id: String,
    /// Anzeigename.
    pub label: String,
    /// Sichtbarkeit (verdeckte Variablen tragen ihren Besitzer).
    pub visibility: VarVisibility,
    /// Wert.
    pub value: VarValue,
}

impl WorldVar {
    /// Besitzer einer verdeckten `seat:<id>`-Variablen.
    #[must_use]
    pub fn owner(&self) -> Option<&PlayerId> {
        self.visibility.owner()
    }

    /// Verdeckt?
    #[must_use]
    pub fn is_hidden(&self) -> bool {
        self.visibility.is_hidden()
    }
}

/// Erzählfakt der Welt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fact {
    /// Runde.
    pub round: u32,
    /// Text.
    pub text: String,
    /// Audience.
    pub audience: Audience,
}

/// Fortwirkender Effekt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ongoing {
    /// Kennung.
    pub id: String,
    /// Beschreibung.
    pub text: String,
    /// Je Rundenende anzuwendende Ops.
    pub each_round: Vec<EffectOp>,
    /// Audience (die des auslösenden Arguments).
    pub audience: Audience,
    /// Auslösendes Argument.
    pub source: String,
}

/// Geheimes Argument (Buchführung).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRecord {
    /// Kennung (`s1`, `s2` …).
    pub secret_id: String,
    /// Argument-ID.
    pub argument_id: String,
    /// Eigentümer.
    pub owner: PlayerId,
    /// Veröffentlichtes Commitment.
    pub commitment: Commitment,
    /// Runde.
    pub round: u32,
    /// Offengelegt.
    pub revealed: bool,
}

/// Privater Verhandlungskanal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    /// Opake Kennung (`neg-7f3a…`).
    pub id: String,
    /// Mitglieder (normalisiert).
    pub members: [PlayerId; 2],
    /// Eröffnet in Runde.
    pub round: u32,
}

impl Channel {
    /// Ist `player` Mitglied?
    #[must_use]
    pub fn has_member(&self, player: &PlayerId) -> bool {
        self.members.contains(player)
    }
}

/// Wer eine Offenlegung ausgelöst hat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", content = "detail", rename_all = "snake_case")]
pub enum RevealedBy {
    /// Bedingung `reveal_when` (Umpire `triggers_secret`).
    Trigger(String),
    /// Eigentümer.
    Owner,
    /// Facilitator-Befehl.
    Facilitator,
    /// Spielende/AAR.
    GameEnd,
    /// Effekt-Op `reveal_secret`.
    Effect(String),
}

// ---------------------------------------------------------------------------
// Journal-Einträge
// ---------------------------------------------------------------------------

/// Art eines Journal-Eintrags.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EntryKind {
    /// Erstes Event: Szenario-Hash und Master-Seed (nur Beobachter).
    GameCreated {
        /// Szenario-ID.
        scenario_id: String,
        /// SHA-256 des Szenario-Quelltexts.
        scenario_hash: String,
        /// Master-Seed (Hex).
        master_seed_hex: String,
        /// Sitzreihenfolge.
        players: Vec<PlayerId>,
    },
    /// Deklaration (oder Neu-Deklaration nach Sichtbarkeitswechsel) einer
    /// Weltvariablen.
    VarDeclared {
        /// Variable.
        var: WorldVar,
    },
    /// Öffentliches Fraktions-Briefing.
    FactionBriefing {
        /// Fraktion.
        faction: PlayerId,
        /// Name.
        name: String,
        /// Briefing.
        briefing: String,
        /// Öffentliche Ziele.
        public_goals: Vec<String>,
        /// Machtmittel.
        assets: Vec<String>,
    },
    /// Geheime Ziele / private Zusatzinformation.
    SecretBriefing {
        /// Fraktion.
        faction: PlayerId,
        /// Geheime Ziele.
        secret_goals: Vec<String>,
        /// Private Zusatzinformation.
        private_brief: Option<String>,
    },
    /// Phasenwechsel.
    PhaseEntered {
        /// Neue Phase.
        phase: Phase,
    },
    /// Briefing bestätigt (mit privater Absicht).
    Briefed {
        /// Sitz.
        seat: Seat,
        /// Absicht (nur AAR).
        intent: Option<String>,
    },
    /// Paar-Kanal eröffnet.
    ChannelOpened {
        /// Opake Kennung.
        channel: String,
        /// Mitglieder.
        members: [PlayerId; 2],
        /// Initiator.
        initiator: PlayerId,
        /// Eröffnungsnachricht.
        opening: String,
    },
    /// Nachricht in einem Paar-Kanal.
    NegotiationPosted {
        /// Kanal.
        channel: String,
        /// Absender.
        from: PlayerId,
        /// Text.
        text: String,
        /// Vorschlag.
        proposal: Option<String>,
        /// Annahme eines Vorschlags.
        accept: Option<String>,
        /// Ablehnung.
        decline: bool,
    },
    /// Verhandlungsphase beendet (öffentlich, ohne Zahl/Teilnehmer).
    NegotiationClosed,
    /// Versiegelte Einreichung.
    ArgumentSealed {
        /// Argument-ID.
        argument_id: String,
        /// Sitz.
        seat: PlayerId,
        /// Commitment auf den Inhalt.
        commitment: Commitment,
    },
    /// Inhalt eines Arguments (öffentlich bzw. Sitz+Umpire bei geheimen).
    ArgumentRevealed {
        /// Argument-ID.
        argument_id: String,
        /// Sitz.
        seat: PlayerId,
        /// Inhalt.
        argument: ArgumentBody,
    },
    /// Öffentliche Ankündigung eines geheimen Arguments (Rust-Template).
    SecretArgumentAnnounced {
        /// Argument-ID.
        argument_id: String,
        /// Sitz.
        seat: PlayerId,
        /// Geheimnis-ID.
        secret_id: String,
        /// Commitment.
        commitment: Commitment,
        /// Vorlagentext.
        text: String,
    },
    /// Private Notiz eines Spielers (nur Sitz; AAR).
    PrivateNote {
        /// Argument-ID.
        argument_id: String,
        /// Sitz.
        seat: PlayerId,
        /// Text.
        text: String,
    },
    /// Gegenargumente eines Spielers.
    CountersSubmitted {
        /// Sitz.
        seat: PlayerId,
        /// Contras je Argument.
        counters: Vec<CounterEntry>,
    },
    /// Sitz passt in einer Phase.
    Forfeit {
        /// Sitz.
        seat: PlayerId,
        /// Phase.
        phase: Phase,
        /// Vorlagentext.
        text: String,
    },
    /// Vollständiges Umpire-Urteil (nur Umpire).
    Adjudicated {
        /// Argument-ID.
        argument_id: String,
        /// Urteil.
        ruling: UmpireRuling,
        /// Netto.
        net: i32,
        /// Zielwert (2W6).
        target: Option<u8>,
        /// Erfolgswahrscheinlichkeit in Prozent.
        probability_pct: u8,
    },
    /// Öffentlicher Teil des Urteils (Gewichte sind öffentlich).
    RulingPublished {
        /// Argument-ID.
        argument_id: String,
        /// Gewichte der Pros.
        pro_weights: Vec<u8>,
        /// Gewichte der Contras je Fraktion.
        con_weights: BTreeMap<String, Vec<u8>>,
        /// Kontext-Modifikator.
        context_modifier: i32,
        /// Netto.
        net: i32,
        /// Zielwert.
        target: Option<u8>,
        /// Erfolgswahrscheinlichkeit in Prozent.
        probability_pct: u8,
        /// Öffentliche Begründung.
        rationale: Option<String>,
    },
    /// Würfelwurf.
    DiceRolled {
        /// Wurf.
        roll: DiceRoll,
    },
    /// Ergebnis eines Arguments.
    ArgumentResolved {
        /// Argument-ID.
        argument_id: String,
        /// Sitz.
        seat: PlayerId,
        /// Ergebnis.
        outcome: Outcome,
        /// Grad.
        grade: Option<Grade>,
        /// Änderung des Fail-Chit-Bestands.
        fail_chit_delta: i32,
    },
    /// Erzählung (Umpire-Prosa).
    Narrated {
        /// Argument (None = Rundenzusammenfassung).
        argument_id: Option<String>,
        /// Text.
        text: String,
    },
    /// Wertänderung einer Weltvariablen.
    WorldDelta {
        /// Variable.
        var: String,
        /// Alter Wert.
        from: VarValue,
        /// Neuer Wert.
        to: VarValue,
        /// Ursache (Argument-, Inject- oder Ongoing-ID).
        cause: String,
    },
    /// Neuer Erzählfakt.
    FactAdded {
        /// Text.
        text: String,
    },
    /// Fortwirkender Effekt gestartet.
    OngoingStarted {
        /// Effekt.
        ongoing: Ongoing,
    },
    /// Fortwirkender Effekt beendet.
    OngoingStopped {
        /// Kennung.
        id: String,
    },
    /// Ungültige Effekt-Op verworfen (Facilitator).
    EffectRejected {
        /// Ursache.
        argument_id: String,
        /// Index im Zweig.
        op_index: usize,
        /// Grund.
        reason: String,
    },
    /// Offenlegung eines geheimen Arguments mit Salt.
    SecretRevealed {
        /// Geheimnis-ID.
        secret_id: String,
        /// Argument-ID.
        argument_id: String,
        /// Eigentümer.
        seat: PlayerId,
        /// Inhalt.
        content: ArgumentBody,
        /// Salt (Hex).
        salt_hex: String,
        /// Commitment.
        commitment: Commitment,
        /// Auslöser.
        by: RevealedBy,
    },
    /// Inject wirksam geworden.
    InjectApplied {
        /// Inject-ID.
        inject_id: String,
        /// Text.
        text: String,
        /// Mit Urheber.
        attributed: bool,
    },
    /// Leak-Verdacht (nur Facilitator).
    LeakSuspect {
        /// Quelle (z. B. `umpire:r2-a1:public_rationale`).
        source: String,
        /// Zurückgehaltener Text.
        text: String,
        /// Befunde.
        findings: Vec<String>,
    },
    /// Umpire-Einschätzung der Führenden.
    StandingSet {
        /// Reihenfolge.
        order: Vec<PlayerId>,
    },
    /// Rundenende mit Zustands-Hash (nur Beobachter).
    RoundClosed {
        /// SHA-256 über den kanonischen Zustand.
        state_hash: String,
    },
    /// Facilitator-Eingriff.
    FacilitatorNote {
        /// Befehl.
        command: String,
        /// Details.
        detail: String,
    },
    /// Spielende.
    GameEnded {
        /// Grund.
        reason: String,
    },
}

impl EntryKind {
    /// Alle Freitexte des Eintrags (Leak-Scanner).
    #[must_use]
    pub fn texts(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        match self {
            Self::FactionBriefing {
                briefing,
                public_goals,
                assets,
                ..
            } => {
                out.push(briefing);
                out.extend(public_goals.iter().map(String::as_str));
                out.extend(assets.iter().map(String::as_str));
            }
            Self::SecretBriefing {
                secret_goals,
                private_brief,
                ..
            } => {
                out.extend(secret_goals.iter().map(String::as_str));
                out.extend(private_brief.as_deref());
            }
            Self::Briefed { intent, .. } => out.extend(intent.as_deref()),
            Self::ChannelOpened { opening, .. } => out.push(opening),
            Self::NegotiationPosted {
                text,
                proposal,
                accept,
                ..
            } => {
                out.push(text);
                out.extend(proposal.as_deref());
                out.extend(accept.as_deref());
            }
            Self::ArgumentRevealed { argument, .. } | Self::SecretRevealed { content: argument, .. } => {
                out.push(&argument.action);
                out.extend(argument.pros.iter().map(String::as_str));
            }
            Self::PrivateNote { text, .. }
            | Self::Narrated { text, .. }
            | Self::FactAdded { text }
            | Self::InjectApplied { text, .. } => out.push(text),
            Self::CountersSubmitted { counters, .. } => {
                for c in counters {
                    out.extend(c.cons.iter().map(String::as_str));
                }
            }
            Self::Adjudicated { ruling, .. } => {
                // `public_rationale` wird über `RulingPublished` ohnehin
                // öffentlich und zählt deshalb nicht als geschützter Text.
                out.extend(ruling.private_notes.as_deref());
                out.extend(ruling.context_reason.as_deref());
                out.extend(ruling.umpire_cons.iter().map(|c| c.text.as_str()));
            }
            Self::RulingPublished { rationale, .. } => out.extend(rationale.as_deref()),
            Self::OngoingStarted { ongoing } => out.push(&ongoing.text),
            _ => {}
        }
        out
    }

    /// Kanal-ID bei Kanal-Einträgen.
    #[must_use]
    pub fn channel_id(&self) -> Option<&str> {
        match self {
            Self::ChannelOpened { channel, .. } | Self::NegotiationPosted { channel, .. } => {
                Some(channel)
            }
            _ => None,
        }
    }
}

/// Journal-Eintrag: Art plus Audience. Kein Zeitstempel, keine Sequenz —
/// beides steht nur im [`JournalRecord`] und gelangt nie in eine Projektion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameEntry {
    /// Runde (0 = Setup).
    pub round: u32,
    /// Audience.
    pub audience: Audience,
    /// Zugehöriges Geheimnis: nach dessen Offenlegung für alle sichtbar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_id: Option<String>,
    /// In-Game-Offenlegung durch diesen Spieler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disclosed_by: Option<PlayerId>,
    /// Art.
    pub kind: EntryKind,
}

impl GameEntry {
    /// Neuer Eintrag.
    #[must_use]
    pub fn new(round: u32, audience: Audience, kind: EntryKind) -> Self {
        Self {
            round,
            audience,
            secret_id: None,
            disclosed_by: None,
            kind,
        }
    }

    /// Bindet den Eintrag an ein Geheimnis.
    #[must_use]
    pub fn with_secret(mut self, secret_id: Option<String>) -> Self {
        self.secret_id = secret_id;
        self
    }

    /// Markiert eine In-Game-Offenlegung.
    #[must_use]
    pub fn disclosed_by(mut self, player: PlayerId) -> Self {
        self.disclosed_by = Some(player);
        self
    }
}

// ---------------------------------------------------------------------------
// Journal
// ---------------------------------------------------------------------------

/// Eine Journalzeile (JSONL).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalRecord {
    /// Laufende Nummer (1-basiert, nur für das Journal selbst).
    pub seq: u64,
    /// Aufzeichnungszeit (optional; beim Replay ignoriert).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Timestamp>,
    /// Eintrag.
    pub entry: GameEntry,
}

/// Append-only Journal.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Journal {
    records: Vec<JournalRecord>,
}

impl Journal {
    /// Leeres Journal.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Hängt einen Eintrag an; Rückgabe ist die Sequenznummer.
    pub fn append(&mut self, entry: GameEntry, at: Option<Timestamp>) -> u64 {
        let seq = u64::try_from(self.records.len()).unwrap_or(u64::MAX).saturating_add(1);
        self.records.push(JournalRecord { seq, at, entry });
        seq
    }

    /// Alle Zeilen.
    #[must_use]
    pub fn records(&self) -> &[JournalRecord] {
        &self.records
    }

    /// Alle Einträge.
    pub fn entries(&self) -> impl Iterator<Item = &GameEntry> {
        self.records.iter().map(|r| &r.entry)
    }

    /// Anzahl Zeilen.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Leer?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// JSONL-Serialisierung (eine Zeile je Record, abschließender Umbruch).
    ///
    /// # Errors
    /// [`MatrixError::Json`].
    pub fn to_jsonl(&self) -> MatrixResult<String> {
        let mut out = String::new();
        for record in &self.records {
            out.push_str(&serde_json::to_string(record)?);
            out.push('\n');
        }
        Ok(out)
    }

    /// Liest JSONL; prüft lückenlose Sequenznummern.
    ///
    /// # Errors
    /// [`MatrixError::Journal`] mit Zeilennummer.
    pub fn from_jsonl(text: &str) -> MatrixResult<Self> {
        let mut journal = Self::new();
        for (index, line) in text.lines().enumerate() {
            let line_no = index + 1;
            if line.trim().is_empty() {
                continue;
            }
            let record: JournalRecord =
                serde_json::from_str(line).map_err(|e| MatrixError::Journal {
                    line: line_no,
                    reason: e.to_string(),
                })?;
            let expected = u64::try_from(journal.records.len())
                .unwrap_or(u64::MAX)
                .saturating_add(1);
            if record.seq != expected {
                return Err(MatrixError::Journal {
                    line: line_no,
                    reason: format!("seq {} statt {expected}", record.seq),
                });
            }
            journal.records.push(record);
        }
        Ok(journal)
    }

    /// Hängt eine Zeile an eine JSONL-Datei an (legt sie bei Bedarf an).
    ///
    /// # Errors
    /// [`MatrixError::Io`] / [`MatrixError::Json`].
    pub fn append_record_to_file(path: &Path, record: &JournalRecord) -> MatrixResult<()> {
        let line = serde_json::to_string(record)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        writeln!(file, "{line}")?;
        file.flush()?;
        Ok(())
    }

    /// Lädt eine JSONL-Datei.
    ///
    /// # Errors
    /// [`MatrixError::Io`] / [`MatrixError::Journal`].
    pub fn load_file(path: &Path) -> MatrixResult<Self> {
        let text = std::fs::read_to_string(path)?;
        Self::from_jsonl(&text)
    }

    /// Präfix bis einschließlich `RoundClosed` der Runde `round` (Fork).
    ///
    /// # Errors
    /// [`MatrixError::State`], wenn die Runde nie geschlossen wurde.
    pub fn fork_at_round_end(&self, round: u32) -> MatrixResult<Self> {
        let cut = self
            .records
            .iter()
            .position(|r| {
                r.entry.round == round && matches!(r.entry.kind, EntryKind::RoundClosed { .. })
            })
            .ok_or_else(|| MatrixError::State(format!("Runde {round} wurde nie geschlossen")))?;
        Ok(Self {
            records: self.records.iter().take(cut + 1).cloned().collect(),
        })
    }
}

// ---------------------------------------------------------------------------
// Zustand
// ---------------------------------------------------------------------------

/// Vollständiger Spielzustand (nur der GameMaster und der Beobachter sehen
/// ihn; Prompts entstehen ausschließlich aus [`crate::visibility::SeatView`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameState {
    /// Szenario-ID.
    pub scenario_id: String,
    /// Modus.
    pub mode: ScenarioMode,
    /// Spieler in Sitzreihenfolge.
    pub players: Vec<PlayerId>,
    /// Aktuelle Runde.
    pub round: u32,
    /// Aktuelle Phase.
    pub phase: Phase,
    /// Master-Seed (Hex), gesetzt durch `GameCreated`.
    pub master_seed_hex: Option<String>,
    /// Weltvariablen.
    pub vars: BTreeMap<String, WorldVar>,
    /// Erzählfakten.
    pub facts: Vec<Fact>,
    /// Laufende Effekte.
    pub ongoing: BTreeMap<String, Ongoing>,
    /// Paar-Kanäle.
    pub channels: BTreeMap<String, Channel>,
    /// Geheime Argumente.
    pub secrets: BTreeMap<String, SecretRecord>,
    /// Verbrauchte geheime Argumente je Sitz.
    pub secrets_used: BTreeMap<PlayerId, u32>,
    /// Fail-Chits je Sitz.
    pub fail_chits: BTreeMap<PlayerId, u32>,
    /// Letzte Umpire-Einschätzung.
    pub standing: Vec<PlayerId>,
    /// Ergebnisse aller Argumente.
    pub outcomes: BTreeMap<String, Outcome>,
    /// Spiel beendet.
    pub ended: bool,
}

impl GameState {
    /// Leerer Zustand vor `GameCreated`.
    #[must_use]
    pub fn new(scenario_id: impl Into<String>, mode: ScenarioMode, players: Vec<PlayerId>) -> Self {
        Self {
            scenario_id: scenario_id.into(),
            mode,
            players,
            round: 0,
            phase: Phase::Setup,
            master_seed_hex: None,
            vars: BTreeMap::new(),
            facts: Vec::new(),
            ongoing: BTreeMap::new(),
            channels: BTreeMap::new(),
            secrets: BTreeMap::new(),
            secrets_used: BTreeMap::new(),
            fail_chits: BTreeMap::new(),
            standing: Vec::new(),
            outcomes: BTreeMap::new(),
            ended: false,
        }
    }

    /// Leerer Zustand für ein Szenario (Variablen kommen über `VarDeclared`).
    #[must_use]
    pub fn from_scenario(scenario: &Scenario) -> Self {
        Self::new(scenario.id(), scenario.mode(), scenario.seat_order())
    }

    /// Master-Seed.
    ///
    /// # Errors
    /// [`MatrixError::State`], solange `GameCreated` fehlt.
    pub fn master_seed(&self) -> MatrixResult<[u8; 32]> {
        let hex = self
            .master_seed_hex
            .as_deref()
            .ok_or_else(|| MatrixError::State("Master-Seed fehlt (kein GameCreated)".to_owned()))?;
        from_hex(hex)
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
            .ok_or_else(|| MatrixError::State("Master-Seed ist kein 32-Byte-Hex".to_owned()))
    }

    /// SHA-256 über den kanonischen Zustand.
    ///
    /// # Errors
    /// [`MatrixError::Json`].
    pub fn state_hash(&self) -> MatrixResult<String> {
        let canonical = canonical_json(self)?;
        Ok(to_hex(&sha256_parts(&[canonical.as_bytes()])))
    }

    /// Verbleibende Fail-Chits eines Sitzes.
    #[must_use]
    pub fn fail_chits_of(&self, player: &PlayerId) -> u32 {
        self.fail_chits.get(player).copied().unwrap_or(0)
    }

    /// Verbrauchte geheime Argumente eines Sitzes.
    #[must_use]
    pub fn secrets_used_by(&self, player: &PlayerId) -> u32 {
        self.secrets_used.get(player).copied().unwrap_or(0)
    }

    /// Wendet einen Journal-Eintrag an — der einzige Weg, den Zustand zu
    /// ändern.
    ///
    /// # Errors
    /// [`MatrixError::State`], wenn der Eintrag nicht zum Zustand passt.
    pub fn apply(&mut self, entry: &GameEntry) -> MatrixResult<()> {
        match &entry.kind {
            EntryKind::GameCreated {
                master_seed_hex,
                players,
                ..
            } => {
                if !self.players.is_empty() && &self.players != players {
                    return Err(MatrixError::State(
                        "GameCreated: Sitzreihenfolge weicht vom Szenario ab".to_owned(),
                    ));
                }
                self.players.clone_from(players);
                self.master_seed_hex = Some(master_seed_hex.clone());
            }
            EntryKind::VarDeclared { var } => {
                self.vars.insert(var.id.clone(), var.clone());
            }
            EntryKind::PhaseEntered { phase } => {
                self.phase = *phase;
                self.round = entry.round;
            }
            EntryKind::ChannelOpened {
                channel, members, ..
            } => {
                self.channels.entry(channel.clone()).or_insert_with(|| Channel {
                    id: channel.clone(),
                    members: members.clone(),
                    round: entry.round,
                });
            }
            EntryKind::SecretArgumentAnnounced {
                argument_id,
                seat,
                secret_id,
                commitment,
                ..
            } => {
                if self.secrets.contains_key(secret_id) {
                    return Err(MatrixError::State(format!("Geheimnis `{secret_id}` doppelt")));
                }
                self.secrets.insert(
                    secret_id.clone(),
                    SecretRecord {
                        secret_id: secret_id.clone(),
                        argument_id: argument_id.clone(),
                        owner: seat.clone(),
                        commitment: commitment.clone(),
                        round: entry.round,
                        revealed: false,
                    },
                );
                *self.secrets_used.entry(seat.clone()).or_insert(0) += 1;
            }
            EntryKind::ArgumentResolved {
                argument_id,
                seat,
                outcome,
                fail_chit_delta,
                ..
            } => {
                self.outcomes.insert(argument_id.clone(), *outcome);
                let chits = self.fail_chits.entry(seat.clone()).or_insert(0);
                if *fail_chit_delta < 0 {
                    *chits = chits.saturating_sub(fail_chit_delta.unsigned_abs());
                } else {
                    *chits = chits.saturating_add(fail_chit_delta.unsigned_abs());
                }
            }
            EntryKind::WorldDelta { var, from, to, .. } => {
                let current = self
                    .vars
                    .get_mut(var)
                    .ok_or_else(|| MatrixError::State(format!("WorldDelta: unbekannte Variable `{var}`")))?;
                if current.value != *from && current.value != *to {
                    return Err(MatrixError::State(format!(
                        "WorldDelta `{var}`: Ausgangswert passt nicht ({} statt {})",
                        current.value.display(),
                        from.display()
                    )));
                }
                current.value = to.clone();
            }
            EntryKind::FactAdded { text } => self.facts.push(Fact {
                round: entry.round,
                text: text.clone(),
                audience: entry.audience.clone(),
            }),
            EntryKind::OngoingStarted { ongoing } => {
                self.ongoing.insert(ongoing.id.clone(), ongoing.clone());
            }
            EntryKind::OngoingStopped { id } => {
                if self.ongoing.remove(id).is_none() {
                    return Err(MatrixError::State(format!("OngoingStopped: `{id}` läuft nicht")));
                }
            }
            EntryKind::SecretRevealed { secret_id, .. } => {
                let record = self.secrets.get_mut(secret_id).ok_or_else(|| {
                    MatrixError::State(format!("SecretRevealed: unbekanntes Geheimnis `{secret_id}`"))
                })?;
                record.revealed = true;
            }
            EntryKind::StandingSet { order } => self.standing.clone_from(order),
            EntryKind::GameEnded { .. } => self.ended = true,
            _ => {}
        }
        Ok(())
    }
}

/// Zustand plus Journal: jeder Eintrag wird angewendet **und** angehängt.
#[derive(Debug, Clone, PartialEq)]
pub struct GameLog {
    /// Zustand.
    pub state: GameState,
    /// Journal.
    pub journal: Journal,
}

impl GameLog {
    /// Neues Log.
    #[must_use]
    pub fn new(state: GameState) -> Self {
        Self {
            state,
            journal: Journal::new(),
        }
    }

    /// Wendet an und hängt an.
    ///
    /// # Errors
    /// Fehler aus [`GameState::apply`]; dann bleibt das Journal unverändert.
    pub fn record(&mut self, entry: GameEntry, at: Option<Timestamp>) -> MatrixResult<u64> {
        self.state.apply(&entry)?;
        Ok(self.journal.append(entry, at))
    }

    /// Mehrere Einträge in Reihenfolge.
    ///
    /// # Errors
    /// Erster Fehler aus [`GameLog::record`].
    pub fn record_all(&mut self, entries: Vec<GameEntry>, at: Option<Timestamp>) -> MatrixResult<()> {
        for entry in entries {
            self.record(entry, at)?;
        }
        Ok(())
    }
}

/// Replay ohne Modellaufrufe: wendet das Journal auf einen frischen Zustand
/// an, prüft jeden Wurf (Neuberechnung aus abgeleitetem Seed) und jeden
/// `state_hash` eines Rundenendes.
///
/// # Errors
/// [`MatrixError::Replay`] bei Abweichung, [`MatrixError::State`] bei
/// inkonsistentem Journal.
pub fn replay(scenario: &Scenario, journal: &Journal) -> MatrixResult<GameState> {
    let mut state = GameState::from_scenario(scenario);
    for record in journal.records() {
        match &record.entry.kind {
            EntryKind::GameCreated { scenario_id, .. } if scenario_id != scenario.id() => {
                return Err(MatrixError::Replay(format!(
                    "Journal gehört zu `{scenario_id}`, nicht zu `{}`",
                    scenario.id()
                )));
            }
            EntryKind::DiceRolled { roll } => {
                let master = state.master_seed()?;
                if !dice::verify_roll(&master, roll) {
                    return Err(MatrixError::Replay(format!(
                        "Wurf {} (Versuch {}) in Zeile {} weicht ab",
                        roll.argument_id, roll.attempt, record.seq
                    )));
                }
            }
            EntryKind::RoundClosed { state_hash } => {
                let actual = state.state_hash()?;
                if &actual != state_hash {
                    return Err(MatrixError::Replay(format!(
                        "state_hash Runde {} weicht ab ({actual} statt {state_hash})",
                        record.entry.round
                    )));
                }
            }
            _ => {}
        }
        state.apply(&record.entry)?;
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn audience_pair_is_normalized() {
        let a = Audience::pair(PlayerId::new("nord"), PlayerId::new("gilde"));
        assert_eq!(a, Audience::Pair(PlayerId::new("gilde"), PlayerId::new("nord")));
    }

    #[test]
    fn audience_parse_roundtrip() -> TestResult {
        for text in [
            "public",
            "umpire",
            "observer",
            "seat:rat",
            "seat+umpire:rat",
            "pair:gilde,nord",
        ] {
            assert_eq!(Audience::parse(text)?.to_string(), text);
        }
        assert_eq!(Audience::parse("all")?, Audience::Public);
        assert_eq!(Audience::parse("pair:nord,gilde")?.to_string(), "pair:gilde,nord");
        assert!(Audience::parse("pair:rat,rat").is_err());
        assert!(Audience::parse("pair:rat").is_err());
        assert!(Audience::parse("seat:").is_err());
        Ok(())
    }

    #[test]
    fn audience_subset() {
        let players: Vec<PlayerId> = ["a", "b", "c"].into_iter().map(PlayerId::new).collect();
        let a = PlayerId::new("a");
        let b = PlayerId::new("b");
        assert!(Audience::Seat(a.clone()).is_within(&Audience::Public, &players));
        assert!(Audience::Seat(a.clone()).is_within(&Audience::SeatAndUmpire(a.clone()), &players));
        assert!(!Audience::Public.is_within(&Audience::SeatAndUmpire(a.clone()), &players));
        assert!(!Audience::Seat(b.clone()).is_within(&Audience::SeatAndUmpire(a.clone()), &players));
        assert!(Audience::UmpireOnly.is_within(&Audience::SeatAndUmpire(a), &players));
        assert!(!Audience::pair(PlayerId::new("a"), b).is_within(&Audience::UmpireOnly, &players));
    }

    #[test]
    fn audience_serde_roundtrip() -> TestResult {
        for aud in [
            Audience::Public,
            Audience::pair(PlayerId::new("x"), PlayerId::new("y")),
            Audience::UmpireOnly,
            Audience::Seat(PlayerId::new("x")),
            Audience::SeatAndUmpire(PlayerId::new("x")),
            Audience::ObserverOnly,
        ] {
            let json = serde_json::to_string(&aud)?;
            let back: Audience = serde_json::from_str(&json)?;
            assert_eq!(back, aud);
        }
        Ok(())
    }

    fn sample_journal() -> Journal {
        let mut j = Journal::new();
        j.append(
            GameEntry::new(0, Audience::Public, EntryKind::PhaseEntered { phase: Phase::Setup }),
            None,
        );
        j.append(
            GameEntry::new(
                1,
                Audience::pair(PlayerId::new("a"), PlayerId::new("b")),
                EntryKind::NegotiationPosted {
                    channel: "neg-1".to_owned(),
                    from: PlayerId::new("a"),
                    text: "Hallo".to_owned(),
                    proposal: None,
                    accept: None,
                    decline: false,
                },
            ),
            Some(Timestamp::UNIX_EPOCH),
        );
        j
    }

    #[test]
    fn journal_jsonl_roundtrip() -> TestResult {
        let j = sample_journal();
        let text = j.to_jsonl()?;
        assert_eq!(text.lines().count(), 2);
        let back = Journal::from_jsonl(&text)?;
        assert_eq!(back, j);
        Ok(())
    }

    #[test]
    fn journal_rejects_gaps_and_garbage() -> TestResult {
        let j = sample_journal();
        let text = j.to_jsonl()?;
        let second = text.lines().nth(1).ok_or("Zeile fehlt")?;
        assert!(matches!(
            Journal::from_jsonl(second),
            Err(MatrixError::Journal { line: 1, .. })
        ));
        assert!(matches!(
            Journal::from_jsonl("{kaputt"),
            Err(MatrixError::Journal { line: 1, .. })
        ));
        Ok(())
    }

    #[test]
    fn journal_file_append_and_load() -> TestResult {
        let dir = std::env::temp_dir().join(format!(
            "harw-matrix-journal-{}-{}",
            std::process::id(),
            Timestamp::now().as_nanosecond()
        ));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("journal.jsonl");
        let j = sample_journal();
        for record in j.records() {
            Journal::append_record_to_file(&path, record)?;
        }
        let loaded = Journal::load_file(&path)?;
        assert_eq!(loaded, j);
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn world_delta_must_match_current_value() -> TestResult {
        let mut state = GameState::new("x", ScenarioMode::Classic, vec![PlayerId::new("a")]);
        state.apply(&GameEntry::new(
            0,
            Audience::Public,
            EntryKind::VarDeclared {
                var: WorldVar {
                    id: "t".to_owned(),
                    label: "T".to_owned(),
                    visibility: VarVisibility::Public,
                    value: VarValue::Track {
                        value: 0,
                        min: -3,
                        max: 3,
                    },
                },
            },
        ))?;
        let delta = |from: i32, to: i32| {
            GameEntry::new(
                1,
                Audience::Public,
                EntryKind::WorldDelta {
                    var: "t".to_owned(),
                    from: VarValue::Track {
                        value: from,
                        min: -3,
                        max: 3,
                    },
                    to: VarValue::Track {
                        value: to,
                        min: -3,
                        max: 3,
                    },
                    cause: "test".to_owned(),
                },
            )
        };
        state.apply(&delta(0, 1))?;
        // idempotent (Mehrfach-Audience derselben Änderung)
        state.apply(&delta(0, 1))?;
        assert!(matches!(state.apply(&delta(2, 3)), Err(MatrixError::State(_))));
        Ok(())
    }

    #[test]
    fn state_hash_is_stable_and_sensitive() -> TestResult {
        let a = GameState::new("x", ScenarioMode::Classic, vec![PlayerId::new("a")]);
        let mut b = a.clone();
        assert_eq!(a.state_hash()?, b.state_hash()?);
        b.round = 2;
        assert_ne!(a.state_hash()?, b.state_hash()?);
        Ok(())
    }
}
