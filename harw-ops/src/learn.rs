//! `/learn` — Lernschleife: dauerhafte Erkenntnisse aus der Sitzung als
//! **Vorschläge** ablegen, nie selbst übernehmen.
//!
//! # Die tragende Regel
//! `/learn` **schlägt vor, schreibt nie wirksam.** Keine Codezeile in dieser
//! Datei schreibt in einen Fakt-Speicher (`harw_memory::FactStore`), in ein
//! Live-Skill-Verzeichnis (`<profil>/skills/<name>/`), in ein Agentenprofil
//! oder in ein Kontextprogramm. Geschrieben werden ausschließlich
//! Vorschlagsdateien:
//!
//! - Lern-Vorschläge (Ziel `memory`/`agent`) unter
//!   `<profil>/learn/proposals/<id>.json` ([`LearnProposalStore`]);
//! - Skill-Vorschläge (Ziel `skill`) über die bestehende
//!   [`SkillProposalStore`]-Ablage (`<profil>/skills/.proposals/<id>/`), also
//!   exakt derselbe Weg wie `skills.propose` — übernommen wird ein solcher
//!   Vorschlag nur durch den Menschen mit `/skills accept <id>`. Zusätzlich
//!   hält [`LearnProposalStore`] einen Verweis-Eintrag, damit `/learn list`
//!   alle Vorschläge dieser Schleife an einer Stelle zeigt.
//!
//! `/learn accept <id>` **markiert** einen Lern-Vorschlag nur als angenommen
//! (wie `/context-proposal accept`) und nennt den konkreten Befehl, mit dem
//! der Operator ihn selbst anwendet (`/memory record …`). Die Anwendung ist
//! damit immer ein zweiter, bewusster Operator-Schritt.
//!
//! # Warum Memory-Vorschläge nicht über `/context-proposal` laufen
//! `harw_knowledge::context_proposal::ContextProposal` beschreibt
//! ausschließlich typisierte Änderungen an einem **Kontextprogramm**
//! (`ProposedChange::AddSection`/`ChangeSectionStrength`/…, Pflichtfeld
//! `target_program: DefinitionId`); Freitext ist dort bewusst ausgeschlossen.
//! Ein Fakt wie „Wir committen nie direkt auf main" ist kein solcher
//! Änderungsvorschlag, und einen eigenen Memory-Vorschlagstyp gibt es in
//! `harw-memory`/`harw-knowledge` (noch) nicht. Diese Datei legt deshalb eine
//! schmale, eigene Vorschlagsablage an (JSON je Vorschlag, gleicher
//! Lebenszyklus `pending → accepted|rejected` wie `ContextProposal`). Ein
//! künftiger Memory-Vorschlagstyp in `harw-knowledge` kann sie ablösen.
//!
//! # Datenquelle und Heuristik
//! Bare `/learn` (bzw. `/learn scan`) liest den Verlauf der aktuellen Sitzung
//! über den registrierten `StateStore`
//! ([`harw_core_bridge::OpContextCoreExt::state_store`] +
//! `load_history(ctx.session_id())`, dieselbe Quelle wie `/usage`) und
//! betrachtet **nur Nutzernachrichten** — die Stimme des Menschen ist die
//! Autorität darüber, was gelten soll; Antworten des Modells werden nicht als
//! Lernquelle gewertet. Die Auswahl ist deterministisch (kein Modellaufruf):
//!
//! 1. Nachrichten, die mit `/` beginnen (Befehle), sehr kurze oder sehr lange
//!    Nachrichten (Pastes) werden übersprungen.
//! 2. Jede Nachricht wird in Sätze zerlegt; ein Satz wird Kandidat, wenn er
//!    ein Signalwort trägt (siehe [`classify_sentence`]): explizites Merken
//!    („merk dir", „remember", „ab jetzt" …), Korrektur („nein", „falsch",
//!    „stattdessen" …), Regel („immer", „niemals", „bitte nicht" …),
//!    Entscheidung („wir nehmen", „entschieden" …) oder ein wiederkehrender
//!    Ablauf („jedes Mal wenn", „whenever" … → Skill-Kandidat).
//! 3. Filter „in zwei Wochen noch relevant?": Sätze mit flüchtigen Zeitbezügen
//!    („heute", „vorerst", „right now" …) fallen heraus. Filter „nicht aus
//!    Code/Git rekonstruierbar?": Codeblöcke fallen heraus. Sätze mit
//!    offensichtlichen Geheimnis-Mustern werden nie vorgeschlagen.
//! 4. Ganze Nachrichten, die der Nutzer mindestens zweimal (normalisiert)
//!    gleich geschickt hat, werden als „Wiederholung" vorgeschlagen.
//! 5. Höchstens [`MAX_CANDIDATES`] Kandidaten je Lauf; bereits vorhandene
//!    Vorschläge (gleicher Fingerabdruck aus Ziel + normalisiertem Text,
//!    unabhängig vom Status) werden nicht erneut angelegt.
//!
//! Ist kein `StateStore` registriert, meldet bare `/learn` das ehrlich und
//! verweist auf `/learn note <text> [--target memory|skill|agent]`, das einen
//! Vorschlag aus explizitem Text anlegt.
//!
//! # Subcommands
//! - (bare) / `scan` — Sitzung scannen, Vorschläge anlegen.
//! - `note <text…> [--target memory|skill|agent]` — Vorschlag aus Text
//!   (Standard-Ziel `memory`).
//! - `list [--all]` — offene (bzw. alle) Lern-Vorschläge.
//! - `show <id>` — ein Vorschlag vollständig.
//! - `accept <id>` / `reject <id> [grund…]` — nur für Ziel `memory`/`agent`;
//!   markiert, wendet nie an. Skill-Vorschläge entscheidet `/skills`.
//!
//! # Fläche
//! Nur `Surface::Command` (`channel_reduced`), kein Modell-Werkzeug: derselbe
//! Callback bedient lesende und markierende Subcommands (siehe Regel 4 der
//! `harw-ops`-Moduldoku), und über die eigene Lernschleife entscheidet der
//! Operator, nicht das laufende Modell.
//!
//! # Fehler
//! - [`OpError::InvalidArguments`] — Grammatik, unbekannte Id, falscher
//!   Status, Entscheidung eines Skill-Vorschlags über `/learn`.
//! - [`OpError::Execution`] — Home/Profil nicht auflösbar, Ein-/Ausgabe,
//!   Lesefehler des `StateStore`.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_core::ModelMessage;
use harw_core_bridge::OpContextCoreExt;
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_registry_defaults::skill_proposal_tools::{
    SkillAuthorCeiling, SkillCandidate, SkillProposalListing, SkillProposalStatus,
    SkillProposalStore,
};
use serde::{Deserialize, Serialize};

/// Höchstzahl neuer Kandidaten je `/learn`-Lauf.
pub const MAX_CANDIDATES: usize = 8;

/// Nutzernachrichten unterhalb dieser Länge (Zeichen) tragen kaum Wissen.
const MIN_MESSAGE_CHARS: usize = 12;

/// Nutzernachrichten oberhalb dieser Länge sind meist Pastes (Logs, Code).
const MAX_MESSAGE_CHARS: usize = 4000;

/// Mindest- und Höchstlänge eines Kandidatensatzes (Zeichen).
const MIN_SENTENCE_CHARS: usize = 12;
const MAX_SENTENCE_CHARS: usize = 400;

/// Präfix jeder Lern-Vorschlags-Id.
const ID_PREFIX: &str = "learn-";

/// Präfix der Skill-Namen, die aus `/learn` entstehen.
const SKILL_NAME_PREFIX: &str = "learned-";

/// Signalwörter für explizites Merken.
const EXPLICIT_MARKERS: &[&str] = &[
    "merk dir",
    "merke dir",
    "merk's dir",
    "merks dir",
    "vergiss nicht",
    "nicht vergessen",
    "denk dran",
    "denk daran",
    "denke daran",
    "für die zukunft",
    "in zukunft",
    "künftig",
    "ab jetzt",
    "ab sofort",
    "remember",
    "keep in mind",
    "from now on",
    "going forward",
    "note to self",
];

/// Signalwörter für einen wiederkehrenden Ablauf (Skill-Kandidat).
const WORKFLOW_MARKERS: &[&str] = &[
    "jedes mal wenn",
    "jedes mal, wenn",
    "jedesmal wenn",
    "immer wenn",
    "immer, wenn",
    "every time",
    "each time",
    "whenever",
];

/// Signalwörter für eine Korrektur.
const CORRECTION_MARKERS: &[&str] = &[
    "stattdessen",
    "das ist falsch",
    "so nicht",
    "nicht so,",
    "instead",
    "that's wrong",
    "that is wrong",
];

/// Satzanfänge, die eine Korrektur einleiten.
const CORRECTION_PREFIXES: &[&str] = &["nein", "falsch", "no,", "no.", "wrong"];

/// Signalwörter für eine dauerhafte Regel.
const RULE_MARKERS: &[&str] = &[
    "immer ",
    "niemals",
    "nie wieder",
    "bitte nicht",
    "always ",
    "never ",
    "don't ",
    "do not ",
];

/// Signalwörter für eine Entscheidung.
const DECISION_MARKERS: &[&str] = &[
    "wir nehmen",
    "wir bleiben bei",
    "wir machen es so",
    "entschieden",
    "entscheidung:",
    "we decided",
    "decision:",
    "let's go with",
    "we'll go with",
];

/// Flüchtige Zeitbezüge: in zwei Wochen vermutlich nicht mehr relevant.
const EPHEMERAL_MARKERS: &[&str] = &[
    "heute",
    "morgen",
    "gerade eben",
    "im moment",
    "vorerst",
    "erstmal",
    "für jetzt",
    "diesmal",
    "today",
    "tomorrow",
    "right now",
    "for now",
    "this time",
];

/// Offensichtliche Geheimnis-Muster: solche Sätze werden nie vorgeschlagen.
const SECRET_MARKERS: &[&str] = &[
    "password",
    "passwort",
    "api_key",
    "api-key",
    "apikey",
    "secret",
    "token",
    " sk-",
    "private key",
    "-----begin",
];

// ── Typen ─────────────────────────────────────────────────────────────────────

/// Wohin eine Erkenntnis gehört.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearnTarget {
    /// Ein dauerhafter Fakt, eine Regel oder Entscheidung (Fakt-Speicher).
    Memory,
    /// Ein wiederkehrender Ablauf (Skill-Vorschlag).
    Skill,
    /// Verhaltensregel für ein bestimmtes Agentenprofil.
    Agent,
}

impl LearnTarget {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "memory" => Some(Self::Memory),
            "skill" => Some(Self::Skill),
            "agent" => Some(Self::Agent),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Skill => "skill",
            Self::Agent => "agent",
        }
    }
}

/// Welches Signal einen Kandidaten ausgelöst hat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearnSignal {
    /// „merk dir", „remember", „ab jetzt" …
    Explicit,
    /// „nein", „falsch", „stattdessen" …
    Correction,
    /// „immer", „niemals", „bitte nicht" …
    Rule,
    /// „wir nehmen", „entschieden" …
    Decision,
    /// „jedes Mal wenn", „whenever" …
    Workflow,
    /// Dieselbe Nutzernachricht mehrfach.
    Repetition,
    /// Explizit per `/learn note`.
    Note,
}

impl LearnSignal {
    fn label(self) -> &'static str {
        match self {
            Self::Explicit => "Merken",
            Self::Correction => "Korrektur",
            Self::Rule => "Regel",
            Self::Decision => "Entscheidung",
            Self::Workflow => "Ablauf",
            Self::Repetition => "Wiederholung",
            Self::Note => "Notiz",
        }
    }
}

/// Prüfstatus eines Lern-Vorschlags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearnStatus {
    /// Noch nicht vom Menschen entschieden.
    Pending,
    /// Vom Menschen markiert — **nicht** angewendet.
    Accepted,
    /// Vom Menschen verworfen.
    Rejected,
}

/// Ein Kandidat vor dem Ablegen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LearnCandidate {
    /// Ziel.
    pub target: LearnTarget,
    /// Auslösendes Signal.
    pub signal: LearnSignal,
    /// Der vorgeschlagene Text (ein Satz bzw. die Notiz).
    pub text: String,
}

/// Ein abgelegter Lern-Vorschlag (`<id>.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LearnProposal {
    /// `learn-<fingerabdruck>`; zugleich Dateiname.
    pub id: String,
    /// Ziel.
    pub target: LearnTarget,
    /// Auslösendes Signal.
    pub signal: LearnSignal,
    /// Vorgeschlagener Text.
    pub text: String,
    /// Herkunft: `session:<id>` oder `note`.
    pub source: String,
    /// Anlagezeitpunkt (RFC 3339).
    pub created_at: String,
    /// Prüfstatus.
    pub status: LearnStatus,
    /// Bei Ziel `skill`: Id des Vorschlags in der Skill-Vorschlagsablage.
    #[serde(default)]
    pub skill_proposal_id: Option<String>,
    /// Entscheidungszeitpunkt.
    #[serde(default)]
    pub decided_at: Option<String>,
    /// Ablehnungsgrund.
    #[serde(default)]
    pub reason: Option<String>,
}

// ── Ablage ────────────────────────────────────────────────────────────────────

/// Die Ablage der Lern-Vorschläge (`<profil>/learn/proposals/`).
///
/// # Nebenläufigkeit
/// `Send + Sync`; hält nur einen Pfad, Schreiben ist atomar (Temp + Rename).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LearnProposalStore {
    dir: PathBuf,
}

impl LearnProposalStore {
    /// Ablage im Verzeichnis `dir`.
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Das Ablageverzeichnis.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// Ob ein Vorschlag mit dieser Id existiert.
    #[must_use]
    pub fn exists(&self, id: &str) -> bool {
        self.path(id).is_file()
    }

    /// Schreibt einen Vorschlag atomar.
    ///
    /// # Errors
    /// [`OpError::Execution`] bei Serde- oder Ein-/Ausgabefehlern.
    pub fn save(&self, proposal: &LearnProposal) -> Result<(), OpError> {
        let bytes = serde_json::to_vec_pretty(proposal).map_err(|error| {
            OpError::Execution(format!("Lern-Vorschlag nicht serialisierbar: {error}"))
        })?;
        std::fs::create_dir_all(&self.dir).map_err(|error| {
            OpError::Execution(format!("{} nicht anlegbar: {error}", self.dir.display()))
        })?;
        let target = self.path(&proposal.id);
        let temporary = self.dir.join(format!(".{}.json.tmp", proposal.id));
        std::fs::write(&temporary, bytes).map_err(|error| {
            OpError::Execution(format!("{} nicht schreibbar: {error}", temporary.display()))
        })?;
        std::fs::rename(&temporary, &target).map_err(|error| {
            OpError::Execution(format!("{} nicht schreibbar: {error}", target.display()))
        })
    }

    /// Liest einen Vorschlag.
    ///
    /// # Errors
    /// [`OpError::InvalidArguments`] bei unzulässiger oder unbekannter Id,
    /// [`OpError::Execution`] bei Lese-/Parsefehlern.
    pub fn load(&self, id: &str) -> Result<LearnProposal, OpError> {
        if !is_valid_learn_id(id) {
            return Err(OpError::InvalidArguments(format!(
                "'{id}' ist keine gültige Lern-Vorschlags-Id (learn-<hex>)"
            )));
        }
        let path = self.path(id);
        let source = match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Err(OpError::InvalidArguments(format!(
                    "kein Lern-Vorschlag '{id}'"
                )));
            }
            Err(error) => {
                return Err(OpError::Execution(format!(
                    "{} nicht lesbar: {error}",
                    path.display()
                )));
            }
        };
        let proposal: LearnProposal = serde_json::from_str(&source).map_err(|error| {
            OpError::Execution(format!(
                "{} ist kein gültiges JSON: {error}",
                path.display()
            ))
        })?;
        if proposal.id != id {
            return Err(OpError::Execution(format!(
                "{} gehört nicht zum Vorschlag '{id}'",
                path.display()
            )));
        }
        Ok(proposal)
    }

    /// Alle lesbaren Vorschläge, sortiert nach Anlagezeit, dann Id.
    ///
    /// # Errors
    /// [`OpError::Execution`], wenn das Verzeichnis existiert, aber nicht
    /// lesbar ist. Einzelne unlesbare Dateien werden übersprungen (geloggt).
    pub fn list(&self) -> Result<Vec<LearnProposal>, OpError> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(OpError::Execution(format!(
                    "{} nicht lesbar: {error}",
                    self.dir.display()
                )));
            }
        };
        let mut proposals = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".json") else {
                continue;
            };
            if !is_valid_learn_id(id) {
                continue;
            }
            match self.load(id) {
                Ok(proposal) => proposals.push(proposal),
                Err(error) => tracing::warn!(id, %error, "learn.proposal_unreadable"),
            }
        }
        // Nach geparster Zeit sortieren, nicht nach dem Text: RFC-3339-
        // Zeichenketten mit unterschiedlich vielen Nachkommastellen ordnen
        // sich lexikografisch nicht chronologisch.
        proposals.sort_by(|left, right| {
            let time =
                |proposal: &LearnProposal| proposal.created_at.parse::<jiff::Timestamp>().ok();
            time(left)
                .cmp(&time(right))
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(proposals)
    }
}

fn is_valid_learn_id(id: &str) -> bool {
    id.strip_prefix(ID_PREFIX).is_some_and(|rest| {
        !rest.is_empty()
            && rest.len() <= 32
            && rest
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    })
}

// ── Argumente ─────────────────────────────────────────────────────────────────

/// Argument-Container für `/learn`: rohe Tokens.
#[derive(Debug, Default, serde::Deserialize)]
pub struct LearnArgs {
    /// Alle Tokens nach `/learn`.
    #[serde(default)]
    pub tokens: Vec<String>,
}

impl harw_operations::FromRawArgs for LearnArgs {
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        Ok(Self {
            tokens: tokens.to_vec(),
        })
    }
}

// ── Operation ─────────────────────────────────────────────────────────────────

/// Führt `/learn` aus.
///
/// # Fehler
/// Siehe Moduldoku.
#[operation(
    name = "learn",
    summary = "Lernschleife: dauerhafte Erkenntnisse der Sitzung als Vorschläge ablegen (scan, note, list, show, accept, reject). Übernimmt nie selbst.",
    domain = "knowledge",
    permission = "operator",
    command(path = "/learn", visibility = "channel_reduced")
)]
async fn learn(ctx: &OpContext, args: LearnArgs) -> Result<OpOutput, OpError> {
    let tokens = args.tokens;
    let tail = tokens.get(1..).unwrap_or_default();
    match tokens.first().map(String::as_str) {
        None | Some("scan") => scan(ctx).await,
        Some("note") => {
            let learn_store = learn_store(ctx)?;
            note(&learn_store, &|| skill_store(ctx), tail)
        }
        Some("list") => {
            let learn_store = learn_store(ctx)?;
            let all = tail.iter().any(|token| token == "--all");
            let skills = skill_store(ctx).ok();
            render_list(&learn_store, skills.as_deref(), all)
        }
        Some("show") => render_show(&*learn_store(ctx)?, tail),
        Some("accept") => decide(&*learn_store(ctx)?, tail, true),
        Some("reject") => decide(&*learn_store(ctx)?, tail, false),
        Some(other) => Err(OpError::InvalidArguments(format!(
            "unbekannter /learn-Subcommand: {other} (scan, note, list, show, accept, reject)"
        ))),
    }
}

/// Meldung, wenn kein Sitzungsverlauf erreichbar ist.
const NO_HISTORY_MESSAGE: &str = "Kein Sitzungsverlauf erreichbar (kein StateStore registriert) — \
     /learn kann die Sitzung nicht scannen. Lege Erkenntnisse explizit an mit: \
     /learn note <text> [--target memory|skill|agent]";

async fn scan(ctx: &OpContext) -> Result<OpOutput, OpError> {
    let Some(state_store) = ctx.state_store() else {
        return Ok(OpOutput::from(NO_HISTORY_MESSAGE.to_owned()));
    };
    let history = state_store
        .load_history(ctx.session_id())
        .await
        .map_err(|error| OpError::Execution(format!("Sitzungsverlauf nicht lesbar: {error}")))?;
    let user_messages: Vec<String> = history
        .to_model_messages()
        .into_iter()
        .filter_map(|message| match message {
            ModelMessage::User { text } => Some(text),
            _ => None,
        })
        .collect();
    let candidates = extract_candidates(&user_messages);
    let learn_store = learn_store(ctx)?;
    let source = format!("session:{}", ctx.session_id());
    let report = file_candidates(&learn_store, &|| skill_store(ctx), &candidates, &source);
    Ok(OpOutput::from(render_scan_report(
        user_messages.len(),
        &report,
    )))
}

// ── Heuristik ─────────────────────────────────────────────────────────────────

/// Zieht deterministisch Lern-Kandidaten aus Nutzernachrichten (siehe
/// Moduldoku, „Datenquelle und Heuristik"). Reihenfolge: Signal-Sätze in
/// Nachrichtenreihenfolge, danach Wiederholungen; höchstens
/// [`MAX_CANDIDATES`], ohne Duplikate.
#[must_use]
pub fn extract_candidates(user_messages: &[String]) -> Vec<LearnCandidate> {
    let mut candidates: Vec<LearnCandidate> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut push = |candidate: LearnCandidate, candidates: &mut Vec<LearnCandidate>| {
        let key = normalize(&candidate.text);
        if !seen.contains(&key) {
            seen.push(key);
            candidates.push(candidate);
        }
    };

    let eligible: Vec<&str> = user_messages
        .iter()
        .map(|message| message.trim())
        .filter(|message| is_eligible_message(message))
        .collect();

    for message in &eligible {
        for sentence in split_sentences(message) {
            if let Some((target, signal)) = classify_sentence(&sentence) {
                push(
                    LearnCandidate {
                        target,
                        signal,
                        text: sentence,
                    },
                    &mut candidates,
                );
            }
        }
    }

    // Wiederholungen: dieselbe (normalisierte) Nachricht mindestens zweimal.
    let mut counted: Vec<(String, &str, usize)> = Vec::new();
    for message in &eligible {
        let key = normalize(message);
        match counted.iter_mut().find(|(existing, _, _)| *existing == key) {
            Some(entry) => entry.2 += 1,
            None => counted.push((key, *message, 1)),
        }
    }
    for (_, message, count) in counted {
        if count >= 2 && passes_filters(message) && message.chars().count() <= MAX_SENTENCE_CHARS {
            push(
                LearnCandidate {
                    target: LearnTarget::Memory,
                    signal: LearnSignal::Repetition,
                    text: message.to_owned(),
                },
                &mut candidates,
            );
        }
    }

    candidates.truncate(MAX_CANDIDATES);
    candidates
}

fn is_eligible_message(message: &str) -> bool {
    let chars = message.chars().count();
    !message.starts_with('/') && (MIN_MESSAGE_CHARS..=MAX_MESSAGE_CHARS).contains(&chars)
}

/// Ordnet einen Satz einem Ziel und Signal zu, oder `None`, wenn er kein
/// Signal trägt oder einen Filter nicht besteht.
#[must_use]
pub fn classify_sentence(sentence: &str) -> Option<(LearnTarget, LearnSignal)> {
    let chars = sentence.chars().count();
    if !(MIN_SENTENCE_CHARS..=MAX_SENTENCE_CHARS).contains(&chars) || !passes_filters(sentence) {
        return None;
    }
    let lower = sentence.to_lowercase();
    let has = |markers: &[&str]| markers.iter().any(|marker| lower.contains(marker));
    if has(WORKFLOW_MARKERS) {
        return Some((LearnTarget::Skill, LearnSignal::Workflow));
    }
    if has(EXPLICIT_MARKERS) {
        return Some((LearnTarget::Memory, LearnSignal::Explicit));
    }
    if has(CORRECTION_MARKERS)
        || CORRECTION_PREFIXES
            .iter()
            .any(|prefix| lower.starts_with(prefix))
    {
        return Some((LearnTarget::Memory, LearnSignal::Correction));
    }
    if has(DECISION_MARKERS) {
        return Some((LearnTarget::Memory, LearnSignal::Decision));
    }
    if has(RULE_MARKERS) {
        return Some((LearnTarget::Memory, LearnSignal::Rule));
    }
    None
}

/// Filter „in zwei Wochen noch relevant?", „nicht aus Code rekonstruierbar?"
/// und „kein Geheimnis".
fn passes_filters(text: &str) -> bool {
    let lower = text.to_lowercase();
    let contains_any = |markers: &[&str]| markers.iter().any(|marker| lower.contains(marker));
    !contains_any(EPHEMERAL_MARKERS) && !contains_any(SECRET_MARKERS) && !lower.contains("```")
}

/// Zerlegt eine Nachricht in Sätze (Schnitt an Zeilenumbruch sowie an `.`,
/// `!`, `?` vor Leerraum oder Ende). Liefert getrimmte, nicht-leere Sätze.
fn split_sentences(message: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();
    let mut chars = message.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\n' {
            flush_sentence(&mut current, &mut sentences);
            continue;
        }
        current.push(ch);
        if matches!(ch, '.' | '!' | '?') && chars.peek().is_none_or(|next| next.is_whitespace()) {
            flush_sentence(&mut current, &mut sentences);
        }
    }
    flush_sentence(&mut current, &mut sentences);
    sentences
}

fn flush_sentence(current: &mut String, sentences: &mut Vec<String>) {
    let trimmed = current
        .trim()
        .trim_start_matches(['-', '*', '•'])
        .trim()
        .to_owned();
    if !trimmed.is_empty() {
        sentences.push(trimmed);
    }
    current.clear();
}

/// Kleinbuchstaben, nur alphanumerische Zeichen, Leerraum zusammengefasst.
fn normalize(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|ch| if ch.is_alphanumeric() { ch } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Stabile Id aus Ziel + normalisiertem Text (FNV-1a, 64 Bit).
fn proposal_id(target: LearnTarget, text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in target
        .as_str()
        .bytes()
        .chain(std::iter::once(b'|'))
        .chain(normalize(text).bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{ID_PREFIX}{hash:016x}")
}

// ── Ablegen ───────────────────────────────────────────────────────────────────

/// Ergebnis des Ablegens mehrerer Kandidaten.
#[derive(Debug, Default)]
struct FileReport {
    created: Vec<LearnProposal>,
    known: Vec<LearnProposal>,
    failed: Vec<(LearnCandidate, String)>,
}

fn file_candidates(
    learn_store: &LearnProposalStore,
    skills: &dyn Fn() -> Result<Arc<SkillProposalStore>, OpError>,
    candidates: &[LearnCandidate],
    source: &str,
) -> FileReport {
    let mut report = FileReport::default();
    for candidate in candidates {
        match file_candidate(learn_store, skills, candidate, source) {
            Ok(Filed::New(proposal)) => report.created.push(proposal),
            Ok(Filed::Known(proposal)) => report.known.push(proposal),
            Err(error) => report.failed.push((candidate.clone(), error.to_string())),
        }
    }
    report
}

enum Filed {
    New(LearnProposal),
    Known(LearnProposal),
}

/// Legt genau einen Kandidaten als Vorschlag ab. Schreibt ausschließlich in
/// die Vorschlagsablagen (siehe Moduldoku, „Die tragende Regel").
fn file_candidate(
    learn_store: &LearnProposalStore,
    skills: &dyn Fn() -> Result<Arc<SkillProposalStore>, OpError>,
    candidate: &LearnCandidate,
    source: &str,
) -> Result<Filed, OpError> {
    let id = proposal_id(candidate.target, &candidate.text);
    if learn_store.exists(&id) {
        return learn_store.load(&id).map(Filed::Known);
    }
    let skill_proposal_id = match candidate.target {
        LearnTarget::Skill => Some(propose_skill(&*skills()?, &candidate.text)?),
        LearnTarget::Memory | LearnTarget::Agent => None,
    };
    let proposal = LearnProposal {
        id,
        target: candidate.target,
        signal: candidate.signal,
        text: candidate.text.clone(),
        source: source.to_owned(),
        created_at: jiff::Timestamp::now().to_string(),
        status: LearnStatus::Pending,
        skill_proposal_id,
        decided_at: None,
        reason: None,
    };
    learn_store.save(&proposal)?;
    Ok(Filed::New(proposal))
}

/// Legt einen Vorschlag aus einer anderen Vorschlagsquelle ab (etwa einen
/// angenommenen Traum-Vorschlag, `source = "dream:<work-id>/<p-id>"`) —
/// derselbe Weg wie `/learn note`, schreibt also nur in die
/// Vorschlagsablagen und übernimmt nie selbst.
///
/// # Rückgabe
/// Den Vorschlag und `true`, wenn er neu angelegt wurde (`false`: gab es
/// schon).
///
/// # Errors
/// [`OpError::InvalidArguments`] bei leerem Text oder Geheimnis-Muster,
/// sonst wie das Ablegen selbst.
pub(crate) fn file_external_proposal(
    learn_store: &LearnProposalStore,
    skills: &dyn Fn() -> Result<Arc<SkillProposalStore>, OpError>,
    target: LearnTarget,
    text: &str,
    source: &str,
) -> Result<(LearnProposal, bool), OpError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(OpError::InvalidArguments("Vorschlagstext fehlt".to_owned()));
    }
    let lower = text.to_lowercase();
    if SECRET_MARKERS.iter().any(|marker| lower.contains(marker)) {
        return Err(OpError::InvalidArguments(
            "Der Text enthält ein Geheimnis-Muster und wird nicht als Vorschlag abgelegt"
                .to_owned(),
        ));
    }
    let candidate = LearnCandidate {
        target,
        signal: LearnSignal::Note,
        text: text.to_owned(),
    };
    match file_candidate(learn_store, skills, &candidate, source)? {
        Filed::New(proposal) => Ok((proposal, true)),
        Filed::Known(proposal) => Ok((proposal, false)),
    }
}

/// Manifest eines aus `/learn` vorgeschlagenen Skills.
#[derive(Serialize)]
struct LearnedSkillManifest {
    name: String,
    description: String,
    instructions_file: &'static str,
}

/// Legt einen Skill-Vorschlag über die bestehende Ablage an (aktiviert nie).
fn propose_skill(store: &SkillProposalStore, text: &str) -> Result<String, OpError> {
    let name = learned_skill_name(text);
    let manifest = LearnedSkillManifest {
        name: name.clone(),
        description: format!(
            "Gelernter Ablauf (aus /learn): {}",
            truncate_chars(text, 160)
        ),
        instructions_file: "instructions.md",
    };
    let skill_toml = toml::to_string(&manifest)
        .map_err(|error| OpError::Execution(format!("Skill-Manifest nicht erzeugbar: {error}")))?;
    let instructions = format!(
        "# {name}\n\n\
         > Aus der Lernschleife (`/learn`) vorgeschlagen. Vor dem Übernehmen \
         prüfen, schärfen und um konkrete Schritte ergänzen.\n\n\
         ## Wann anwenden\n\n\
         Wenn die folgende, vom Nutzer benannte Situation eintritt.\n\n\
         ## Gelernter Ablauf\n\n\
         {text}\n"
    );
    let candidate = SkillCandidate {
        skill_toml,
        instructions,
        ..SkillCandidate::default()
    };
    // Der Vorschlag deklariert weder Werkzeuge noch MCPs; die Decke ist
    // deshalb leer und das Delta leer. Übernommen wird er nur über
    // `/skills accept` bzw. das freigabepflichtige `skills.commit_proposal`.
    let ceiling = SkillAuthorCeiling {
        role: harw_agent_dsl::roles::AgentRoleId::UserInterface,
        tools: std::collections::BTreeSet::new(),
        mcps: std::collections::BTreeSet::new(),
    };
    store
        .propose(&candidate, &ceiling)
        .map(|meta| meta.proposal_id)
        .map_err(|error| OpError::Execution(format!("Skill-Vorschlag abgelehnt: {error}")))
}

fn learned_skill_name(text: &str) -> String {
    let words: Vec<String> = harw_memory::slugify(text)
        .split('-')
        .filter(|word| !word.is_empty())
        .take(5)
        .map(str::to_owned)
        .collect();
    let slug = if words.is_empty() {
        "ablauf".to_owned()
    } else {
        words.join("-")
    };
    let mut name = format!("{SKILL_NAME_PREFIX}{slug}");
    name.truncate(64);
    name.trim_end_matches('-').to_owned()
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let mut cut: String = text.chars().take(max).collect();
        cut.push('…');
        cut
    }
}

// ── /learn note ───────────────────────────────────────────────────────────────

fn note(
    learn_store: &LearnProposalStore,
    skills: &dyn Fn() -> Result<Arc<SkillProposalStore>, OpError>,
    tail: &[String],
) -> Result<OpOutput, OpError> {
    const USAGE: &str = "/learn note <text…> [--target memory|skill|agent]";
    let mut target = LearnTarget::Memory;
    let mut words: Vec<&str> = Vec::new();
    let mut index = 0;
    while index < tail.len() {
        let token = tail[index].as_str();
        let value = if token == "--target" {
            index += 1;
            Some(tail.get(index).map(String::as_str).ok_or_else(|| {
                OpError::InvalidArguments(format!("--target braucht einen Wert ({USAGE})"))
            })?)
        } else {
            token.strip_prefix("--target=")
        };
        match value {
            Some(value) => {
                target = LearnTarget::parse(value).ok_or_else(|| {
                    OpError::InvalidArguments(format!(
                        "unbekanntes Ziel '{value}' (memory, skill, agent)"
                    ))
                })?;
            }
            None => words.push(token),
        }
        index += 1;
    }
    let text = words.join(" ").trim().to_owned();
    if text.is_empty() {
        return Err(OpError::InvalidArguments(format!("Text fehlt ({USAGE})")));
    }
    let lower = text.to_lowercase();
    if SECRET_MARKERS.iter().any(|marker| lower.contains(marker)) {
        return Err(OpError::InvalidArguments(
            "Der Text enthält ein Geheimnis-Muster und wird nicht als Vorschlag abgelegt"
                .to_owned(),
        ));
    }
    let candidate = LearnCandidate {
        target,
        signal: LearnSignal::Note,
        text,
    };
    match file_candidate(learn_store, skills, &candidate, "note")? {
        Filed::New(proposal) => Ok(OpOutput::from(format!(
            "Vorschlag angelegt (nichts wurde übernommen):\n{}",
            render_entry(&proposal)
        ))),
        Filed::Known(proposal) => Ok(OpOutput::from(format!(
            "Diesen Vorschlag gibt es bereits:\n{}",
            render_entry(&proposal)
        ))),
    }
}

// ── Darstellung ───────────────────────────────────────────────────────────────

/// Wie der Operator einen Vorschlag annimmt oder verwirft.
fn how_to_decide(proposal: &LearnProposal) -> String {
    match (&proposal.target, &proposal.skill_proposal_id) {
        (LearnTarget::Skill, Some(skill_id)) => format!(
            "prüfen: /skills review {skill_id} · annehmen: /skills accept {skill_id} · \
             ablehnen: /skills reject {skill_id}"
        ),
        _ => format!(
            "annehmen: /learn accept {id} · ablehnen: /learn reject {id}",
            id = proposal.id
        ),
    }
}

fn render_entry(proposal: &LearnProposal) -> String {
    format!(
        "· {} [{} · {}] „{}“\n    {}\n",
        proposal.id,
        proposal.target.as_str(),
        proposal.signal.label(),
        truncate_chars(&proposal.text, 160),
        how_to_decide(proposal)
    )
}

fn render_scan_report(message_count: usize, report: &FileReport) -> String {
    let mut buf = format!(
        "/learn: {} Nutzernachricht(en) geprüft, {} neue(r) Vorschlag/Vorschläge. \
         Nichts wurde übernommen — jede Übernahme ist eine Operator-Entscheidung.\n",
        message_count,
        report.created.len()
    );
    if report.created.is_empty() && report.known.is_empty() && report.failed.is_empty() {
        buf.push_str(
            "Keine dauerhaften Erkenntnisse erkannt. Explizit: /learn note <text> \
             [--target memory|skill|agent]\n",
        );
    }
    for proposal in &report.created {
        buf.push_str(&render_entry(proposal));
    }
    if !report.known.is_empty() {
        buf.push_str(&format!(
            "{} bereits vorhanden (nicht erneut angelegt): {}\n",
            report.known.len(),
            report
                .known
                .iter()
                .map(|proposal| proposal.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for (candidate, error) in &report.failed {
        buf.push_str(&format!(
            "! nicht abgelegt [{}] „{}“: {error}\n",
            candidate.target.as_str(),
            truncate_chars(&candidate.text, 80)
        ));
    }
    buf
}

/// Status eines Skill-Vorschlags aus der Skill-Ablage, falls lesbar.
fn skill_status(skills: Option<&SkillProposalStore>, skill_id: &str) -> Option<String> {
    let listings = skills?.list().ok()?;
    listings.into_iter().find_map(|listing| match listing {
        SkillProposalListing::Proposal { meta, expired } if meta.proposal_id == skill_id => {
            Some(match (meta.status, expired) {
                (SkillProposalStatus::PendingReview, true) => "abgelaufen".to_owned(),
                (status, _) => status.as_str().to_owned(),
            })
        }
        _ => None,
    })
}

/// Ob ein Vorschlag noch offen ist — bei Skill-Vorschlägen maßgeblich der
/// Status in der Skill-Ablage.
fn is_open(proposal: &LearnProposal, skills: Option<&SkillProposalStore>) -> bool {
    match (&proposal.target, &proposal.skill_proposal_id) {
        (LearnTarget::Skill, Some(skill_id)) => skill_status(skills, skill_id)
            .is_none_or(|status| status == SkillProposalStatus::PendingReview.as_str()),
        _ => proposal.status == LearnStatus::Pending,
    }
}

fn status_label(proposal: &LearnProposal, skills: Option<&SkillProposalStore>) -> String {
    match (&proposal.target, &proposal.skill_proposal_id) {
        (LearnTarget::Skill, Some(skill_id)) => {
            skill_status(skills, skill_id).unwrap_or_else(|| "unbekannt".to_owned())
        }
        _ => format!("{:?}", proposal.status).to_lowercase(),
    }
}

fn render_list(
    learn_store: &LearnProposalStore,
    skills: Option<&SkillProposalStore>,
    all: bool,
) -> Result<OpOutput, OpError> {
    let proposals: Vec<LearnProposal> = learn_store
        .list()?
        .into_iter()
        .filter(|proposal| all || is_open(proposal, skills))
        .collect();
    if proposals.is_empty() {
        return Ok(OpOutput::from(if all {
            "Keine Lern-Vorschläge.".to_owned()
        } else {
            "Keine offenen Lern-Vorschläge. (/learn list --all zeigt auch entschiedene)".to_owned()
        }));
    }
    let mut buf = format!(
        "{} {}Lern-Vorschlag/Vorschläge:\n",
        proposals.len(),
        if all { "" } else { "offene(r) " }
    );
    for proposal in &proposals {
        if all {
            buf.push_str(&format!("  ({})\n", status_label(proposal, skills)));
        }
        buf.push_str(&render_entry(proposal));
    }
    Ok(OpOutput::from(buf))
}

fn render_show(learn_store: &LearnProposalStore, tail: &[String]) -> Result<OpOutput, OpError> {
    let id = tail
        .first()
        .ok_or_else(|| OpError::InvalidArguments("/learn show <id>".to_owned()))?;
    let proposal = learn_store.load(id)?;
    let mut buf = format!(
        "Vorschlag: {}\nZiel: {}\nSignal: {}\nStatus: {:?}\nHerkunft: {}\nErzeugt: {}\n",
        proposal.id,
        proposal.target.as_str(),
        proposal.signal.label(),
        proposal.status,
        proposal.source,
        proposal.created_at,
    );
    if let Some(skill_id) = &proposal.skill_proposal_id {
        buf.push_str(&format!("Skill-Vorschlag: {skill_id}\n"));
    }
    if let Some(decided_at) = &proposal.decided_at {
        buf.push_str(&format!("Entschieden: {decided_at}\n"));
    }
    if let Some(reason) = &proposal.reason {
        buf.push_str(&format!("Grund: {reason}\n"));
    }
    buf.push_str(&format!("Text:\n{}\n", proposal.text));
    buf.push_str(&format!("{}\n", how_to_decide(&proposal)));
    Ok(OpOutput::from(buf))
}

/// Markiert einen `memory`/`agent`-Vorschlag. Schreibt ausschließlich die
/// eine Vorschlagsdatei — nie einen Fakt, ein Profil oder einen Skill.
fn decide(
    learn_store: &LearnProposalStore,
    tail: &[String],
    accept: bool,
) -> Result<OpOutput, OpError> {
    let verb = if accept { "accept" } else { "reject" };
    let id = tail
        .first()
        .ok_or_else(|| OpError::InvalidArguments(format!("/learn {verb} <id>")))?;
    let mut proposal = learn_store.load(id)?;
    if let (LearnTarget::Skill, Some(skill_id)) = (&proposal.target, &proposal.skill_proposal_id) {
        return Err(OpError::InvalidArguments(format!(
            "{id} ist ein Skill-Vorschlag — entschieden wird er über /skills {verb} {skill_id}"
        )));
    }
    if proposal.status != LearnStatus::Pending {
        return Err(OpError::InvalidArguments(format!(
            "{id} ist bereits entschieden ({:?})",
            proposal.status
        )));
    }
    proposal.status = if accept {
        LearnStatus::Accepted
    } else {
        LearnStatus::Rejected
    };
    proposal.decided_at = Some(jiff::Timestamp::now().to_string());
    if !accept {
        let reason = tail.get(1..).unwrap_or_default().join(" ");
        proposal.reason = (!reason.trim().is_empty()).then(|| reason.trim().to_owned());
    }
    learn_store.save(&proposal)?;
    if !accept {
        return Ok(OpOutput::from(format!(
            "{id} abgelehnt{}.",
            proposal
                .reason
                .as_deref()
                .map(|reason| format!(": {reason}"))
                .unwrap_or_default()
        )));
    }
    let apply = match proposal.target {
        LearnTarget::Memory => format!(
            "Anwenden (eigener Schritt): /memory record {} [--project|--global]",
            proposal.text
        ),
        LearnTarget::Agent => "Anwenden (eigener Schritt): Agentenprofil über den \
                               Agentendefinitions-Vorschlagsweg oder von Hand anpassen."
            .to_owned(),
        LearnTarget::Skill => String::new(),
    };
    Ok(OpOutput::from(format!(
        "{id} als angenommen markiert. Es wurde nichts übernommen.\n{apply}"
    )))
}

// ── Dienste ───────────────────────────────────────────────────────────────────

fn profile_dir() -> Result<PathBuf, OpError> {
    let home = harw_home::home_dir()
        .map_err(|error| OpError::Execution(format!("HARW_HOME nicht auflösbar: {error}")))?;
    let profile = harw_home::active_profile_name(&home);
    harw_home::profile_dir(&home, &profile)
        .map_err(|error| OpError::Execution(format!("Profil '{profile}' nicht auflösbar: {error}")))
}

/// Injizierte Lern-Ablage oder `<HARW_HOME>/profiles/<aktiv>/learn/proposals`.
pub(crate) fn learn_store(ctx: &OpContext) -> Result<Arc<LearnProposalStore>, OpError> {
    if let Some(store) = ctx.service::<Arc<LearnProposalStore>>() {
        return Ok(Arc::clone(store));
    }
    Ok(Arc::new(LearnProposalStore::new(
        profile_dir()?.join("learn").join("proposals"),
    )))
}

/// Injizierte Skill-Vorschlagsablage oder die des aktiven Profils (wie
/// `/skills`).
pub(crate) fn skill_store(ctx: &OpContext) -> Result<Arc<SkillProposalStore>, OpError> {
    if let Some(store) = ctx.service::<Arc<SkillProposalStore>>() {
        return Ok(Arc::clone(store));
    }
    Ok(Arc::new(SkillProposalStore::new(
        profile_dir()?.join("skills"),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_core::state_store::StateStore;
    use harw_core::{ConversationHistory, InMemoryStateStore};
    use harw_operations::context::ServiceMap;
    use harw_operations::operation::{CommandVisibility, Surface};
    use harw_operations::{FromRawArgs, Operation};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};

    struct Fixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        learn: Arc<LearnProposalStore>,
        skills: Arc<SkillProposalStore>,
        session: SessionId,
    }

    impl Fixture {
        fn new() -> TestResult<Self> {
            let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let root = temp.path().to_path_buf();
            Ok(Self {
                learn: Arc::new(LearnProposalStore::new(root.join("learn/proposals"))),
                skills: Arc::new(SkillProposalStore::new(root.join("skills"))),
                session: SessionId::new(),
                root,
                _temp: temp,
            })
        }

        fn context(&self, state_store: Option<Arc<dyn StateStore>>) -> TestResult<OpContext> {
            std::fs::create_dir_all(self.root.join("ws")).map_err(ctx("create workspace"))?;
            let registry = WorkspaceRegistry::build(
                &self.root,
                [WorkspaceRegistration {
                    tenant: TenantId::from_str("test-tenant"),
                    workspace: WorkspaceId::from_str("ws"),
                    root: PathBuf::from("ws"),
                }],
            )
            .map_err(ctx("build workspace registry"))?;
            let binding = registry
                .resolve(
                    &TenantId::from_str("test-tenant"),
                    &WorkspaceId::from_str("ws"),
                )
                .map_err(ctx("resolve workspace"))?;
            let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());
            let mut services = ServiceMap::new();
            services.insert(Arc::clone(&self.learn));
            services.insert(Arc::clone(&self.skills));
            if let Some(store) = state_store {
                services.insert(store);
            }
            Ok(OpContext::new(
                self.session.clone(),
                TurnId::new(),
                sandbox,
                services,
            ))
        }

        async fn context_with_history(&self, user_messages: &[&str]) -> TestResult<OpContext> {
            let store = InMemoryStateStore::new();
            let mut history = ConversationHistory::new();
            for message in user_messages {
                history.push_user_text(*message);
                history.push_assistant_text("Verstanden.", None);
            }
            store
                .save_history(&self.session, &history)
                .await
                .map_err(ctx("save history"))?;
            self.context(Some(Arc::new(store) as Arc<dyn StateStore>))
        }
    }

    fn walk_files(root: &Path) -> std::collections::BTreeSet<PathBuf> {
        let mut files = std::collections::BTreeSet::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    files.insert(path);
                }
            }
        }
        files
    }

    async fn run(op_ctx: &OpContext, tokens: &[&str]) -> Result<OpOutput, OpError> {
        let args = LearnArgs::from_raw_args(&toks(tokens))?;
        super::learn(op_ctx, args).await
    }

    #[test]
    fn learn_operation_is_command_only_and_channel_reduced() {
        let meta = super::LearnOperation.meta();
        assert_eq!(meta.name, "learn");
        assert!(
            !meta
                .surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::ModelTool { .. })),
            "/learn darf kein Modell-Werkzeug sein"
        );
        assert!(meta.surfaces.iter().any(|surface| matches!(
            surface,
            Surface::Command {
                path: "/learn",
                visibility: CommandVisibility::ChannelReduced,
            }
        )));
    }

    #[test]
    fn classify_recognizes_the_signal_families() {
        let cases = [
            (
                "Merk dir: Wir committen nie direkt auf main.",
                Some((LearnTarget::Memory, LearnSignal::Explicit)),
            ),
            (
                "Nein, die Tests laufen mit make test statt cargo.",
                Some((LearnTarget::Memory, LearnSignal::Correction)),
            ),
            (
                "Wir nehmen jiff für alle Zeitstempel.",
                Some((LearnTarget::Memory, LearnSignal::Decision)),
            ),
            (
                "Kommentare bitte immer auf Deutsch schreiben.",
                Some((LearnTarget::Memory, LearnSignal::Rule)),
            ),
            (
                "Jedes Mal wenn du ein Release baust, erst das Changelog prüfen.",
                Some((LearnTarget::Skill, LearnSignal::Workflow)),
            ),
            ("Wie spät ist es eigentlich?", None),
        ];
        for (sentence, expected) in cases {
            assert_eq!(classify_sentence(sentence), expected, "{sentence}");
        }
    }

    #[test]
    fn classify_filters_ephemeral_code_and_secrets() {
        assert_eq!(
            classify_sentence("Merk dir, heute deployen wir nicht."),
            None
        );
        assert_eq!(
            classify_sentence("Merk dir das Passwort hunter2 für den Server."),
            None
        );
        assert_eq!(
            classify_sentence("Remember ```let x = 1;``` in the loop"),
            None
        );
    }

    #[test]
    fn extract_finds_sentences_and_repetitions_without_duplicates() {
        let messages = vec![
            "/status".to_owned(),
            "Bitte baue das Feature. Merk dir: Wir committen nie direkt auf main.".to_owned(),
            "Lies zuerst die README-Datei durch".to_owned(),
            "Lies zuerst die README-Datei durch!".to_owned(),
            "Merk dir: Wir committen nie direkt auf main.".to_owned(),
        ];
        let candidates = extract_candidates(&messages);
        assert_eq!(candidates.len(), 2, "{candidates:?}");
        assert_eq!(candidates[0].signal, LearnSignal::Explicit);
        assert_eq!(
            candidates[0].text,
            "Merk dir: Wir committen nie direkt auf main."
        );
        assert_eq!(candidates[1].signal, LearnSignal::Repetition);
        assert_eq!(candidates[1].text, "Lies zuerst die README-Datei durch");
    }

    #[test]
    fn extract_caps_the_number_of_candidates() {
        let messages: Vec<String> = (0..20)
            .map(|index| format!("Merk dir Regel Nummer {index} für das Projekt."))
            .collect();
        assert_eq!(extract_candidates(&messages).len(), MAX_CANDIDATES);
    }

    #[test]
    fn proposal_ids_are_stable_and_valid() {
        let first = proposal_id(LearnTarget::Memory, "Wir committen nie auf main.");
        let second = proposal_id(LearnTarget::Memory, "wir  committen nie auf MAIN");
        let other = proposal_id(LearnTarget::Agent, "Wir committen nie auf main.");
        assert_eq!(first, second);
        assert_ne!(first, other);
        assert!(is_valid_learn_id(&first));
        assert!(!is_valid_learn_id("learn-../../etc"));
        assert!(!is_valid_learn_id("context-proposal/x"));
    }

    #[tokio::test]
    async fn bare_learn_without_state_store_points_to_note() -> TestResult {
        let fixture = Fixture::new()?;
        let op_ctx = fixture.context(None)?;
        let output = run(&op_ctx, &[]).await.map_err(ctx("bare learn"))?;
        assert!(output.text.contains("/learn note"));
        assert!(fixture.learn.list().map_err(ctx("list"))?.is_empty());
        Ok(())
    }

    /// Kernbeweis: ein Scan legt nur Vorschlagsdateien an — kein Fakt, kein
    /// Live-Skill-Verzeichnis, sonst nichts.
    #[tokio::test]
    async fn scan_creates_only_proposals_and_never_commits() -> TestResult {
        let fixture = Fixture::new()?;
        let op_ctx = fixture
            .context_with_history(&[
                "Merk dir: Wir committen nie direkt auf main.",
                "Jedes Mal wenn du ein Release baust, erst das Changelog prüfen.",
                "Wie spät ist es?",
            ])
            .await?;

        let output = run(&op_ctx, &[]).await.map_err(ctx("scan"))?;
        assert!(output.text.contains("2 neue"), "{}", output.text);
        assert!(output.text.contains("Nichts wurde übernommen"));
        assert!(output.text.contains("/learn accept learn-"));
        assert!(output.text.contains("/skills accept "));

        let files = walk_files(&fixture.root);
        for file in &files {
            let relative = file.strip_prefix(&fixture.root).map_err(ctx("relative"))?;
            let text = relative.to_string_lossy();
            assert!(
                text.starts_with("learn/proposals/")
                    || text.starts_with("skills/.proposals/")
                    || text.starts_with("ws/"),
                "unerwartete Datei außerhalb der Vorschlagsablagen: {text}"
            );
        }
        let proposals = fixture.learn.list().map_err(ctx("list"))?;
        assert_eq!(proposals.len(), 2);
        assert!(
            proposals
                .iter()
                .all(|proposal| proposal.status == LearnStatus::Pending)
        );
        let skill = proposals
            .iter()
            .find(|proposal| proposal.target == LearnTarget::Skill)
            .ok_or(TestError::Missing("skill proposal"))?;
        let skill_id = skill
            .skill_proposal_id
            .as_deref()
            .ok_or(TestError::Missing("skill proposal id"))?;
        let loaded = fixture
            .skills
            .load(skill_id)
            .map_err(ctx("load skill proposal"))?;
        assert_eq!(loaded.meta.status, SkillProposalStatus::PendingReview);
        assert!(loaded.meta.name.starts_with("learned-"));
        assert!(loaded.instructions.contains("Changelog"));

        // Ein zweiter Lauf legt nichts erneut an.
        let again = run(&op_ctx, &["scan"]).await.map_err(ctx("second scan"))?;
        assert!(again.text.contains("0 neue"), "{}", again.text);
        assert!(again.text.contains("2 bereits vorhanden"));
        assert_eq!(walk_files(&fixture.root), files);
        Ok(())
    }

    #[tokio::test]
    async fn note_creates_memory_skill_and_agent_proposals() -> TestResult {
        let fixture = Fixture::new()?;
        let op_ctx = fixture.context(None)?;

        let memory = run(
            &op_ctx,
            &["note", "Tests", "laufen", "über", "make", "test"],
        )
        .await
        .map_err(ctx("note memory"))?;
        assert!(memory.text.contains("[memory · Notiz]"), "{}", memory.text);

        let skill = run(
            &op_ctx,
            &[
                "note",
                "--target",
                "skill",
                "Release:",
                "Changelog,",
                "Tag,",
                "Push",
            ],
        )
        .await
        .map_err(ctx("note skill"))?;
        assert!(skill.text.contains("/skills accept "), "{}", skill.text);

        let agent = run(
            &op_ctx,
            &[
                "note",
                "Reviewer",
                "prüft",
                "zuerst",
                "Tests",
                "--target=agent",
            ],
        )
        .await
        .map_err(ctx("note agent"))?;
        assert!(agent.text.contains("[agent · Notiz]"), "{}", agent.text);

        let duplicate = run(&op_ctx, &["note", "Tests laufen über make test"])
            .await
            .map_err(ctx("duplicate note"))?;
        assert!(duplicate.text.contains("bereits"), "{}", duplicate.text);

        assert!(
            !fixture
                .root
                .join("skills/learned-release-changelog-tag-push")
                .exists()
        );
        assert_eq!(fixture.learn.list().map_err(ctx("list"))?.len(), 3);
        Ok(())
    }

    #[tokio::test]
    async fn note_rejects_bad_input() -> TestResult {
        let fixture = Fixture::new()?;
        let op_ctx = fixture.context(None)?;
        for tokens in [
            vec!["note"],
            vec!["note", "text", "--target"],
            vec!["note", "text", "--target", "palace"],
            vec!["note", "Das", "Passwort", "ist", "geheim"],
        ] {
            let result = run(&op_ctx, &tokens).await;
            assert!(
                matches!(result, Err(OpError::InvalidArguments(_))),
                "{tokens:?} → {result:?}"
            );
        }
        assert!(fixture.learn.list().map_err(ctx("list"))?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn list_accept_and_reject_only_mark() -> TestResult {
        let fixture = Fixture::new()?;
        let op_ctx = fixture.context(None)?;
        run(
            &op_ctx,
            &["note", "Wir", "nutzen", "jiff", "für", "Zeitstempel"],
        )
        .await
        .map_err(ctx("note one"))?;
        run(
            &op_ctx,
            &["note", "Kommentare", "auf", "Deutsch", "schreiben"],
        )
        .await
        .map_err(ctx("note two"))?;
        let proposals = fixture.learn.list().map_err(ctx("list"))?;
        let id_of = |needle: &str| {
            proposals
                .iter()
                .find(|proposal| proposal.text.contains(needle))
                .map(|proposal| proposal.id.clone())
                .ok_or(TestError::Missing("proposal by text"))
        };
        let first = id_of("jiff")?;
        let second = id_of("Deutsch")?;

        let listed = run(&op_ctx, &["list"]).await.map_err(ctx("list op"))?;
        assert!(listed.text.contains("2 offene"), "{}", listed.text);

        let before = walk_files(&fixture.root);
        let accepted = run(&op_ctx, &["accept", first.as_str()])
            .await
            .map_err(ctx("accept"))?;
        assert!(
            accepted.text.contains("nichts übernommen"),
            "{}",
            accepted.text
        );
        assert!(accepted.text.contains("/memory record Wir nutzen jiff"));
        let rejected = run(&op_ctx, &["reject", second.as_str(), "zu", "vage"])
            .await
            .map_err(ctx("reject"))?;
        assert!(rejected.text.contains("abgelehnt: zu vage"));
        assert_eq!(walk_files(&fixture.root), before, "nur Statusänderungen");

        let again = run(&op_ctx, &["accept", first.as_str()]).await;
        assert!(matches!(again, Err(OpError::InvalidArguments(_))));
        let open = run(&op_ctx, &["list"]).await.map_err(ctx("list open"))?;
        assert!(open.text.contains("Keine offenen"), "{}", open.text);
        let all = run(&op_ctx, &["list", "--all"])
            .await
            .map_err(ctx("list all"))?;
        assert!(all.text.contains("(accepted)") && all.text.contains("(rejected)"));
        let shown = run(&op_ctx, &["show", second.as_str()])
            .await
            .map_err(ctx("show"))?;
        assert!(shown.text.contains("Grund: zu vage"));
        Ok(())
    }

    #[tokio::test]
    async fn skill_proposals_are_decided_via_skills_only() -> TestResult {
        let fixture = Fixture::new()?;
        let op_ctx = fixture.context(None)?;
        run(
            &op_ctx,
            &[
                "note",
                "--target",
                "skill",
                "Vor",
                "jedem",
                "Release",
                "Changelog",
                "prüfen",
            ],
        )
        .await
        .map_err(ctx("note skill"))?;
        let proposal = fixture
            .learn
            .list()
            .map_err(ctx("list"))?
            .into_iter()
            .next()
            .ok_or(TestError::Missing("proposal"))?;
        let result = run(&op_ctx, &["accept", proposal.id.as_str()]).await;
        let Err(OpError::InvalidArguments(message)) = result else {
            return Err(TestError::Unexpected(format!("{result:?}")));
        };
        assert!(message.contains("/skills accept"));
        Ok(())
    }

    #[tokio::test]
    async fn unknown_subcommand_and_ids_are_invalid() -> TestResult {
        let fixture = Fixture::new()?;
        let op_ctx = fixture.context(None)?;
        for tokens in [
            vec!["bogus"],
            vec!["show"],
            vec!["show", "learn-0000000000000000"],
            vec!["accept", "../escape"],
        ] {
            let result = run(&op_ctx, &tokens).await;
            assert!(
                matches!(result, Err(OpError::InvalidArguments(_))),
                "{tokens:?} → {result:?}"
            );
        }
        Ok(())
    }
}
