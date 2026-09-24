//! Auto-Modus: Schnittstelle zwischen Freigabepolitik und Klassifizierer
//! (Runde 5, Teil E).
//!
//! # Verantwortlichkeit
//! Dieses Modul besitzt das **Vokabular** des Auto-Modus
//! ([`crate::approval_mode::ApprovalMode::Delegated`]), nicht dessen Logik:
//! - [`AutoVerdict`] — die strukturierte Antwort `{decision, category,
//!   reason}` eines Klassifizierers bzw. seines deterministischen Vorfilters.
//! - [`AutoApprovalGate`] — die Naht, über die die eingebaute
//!   Standardpolitik (`harw-registry-defaults`) einen Aufruf, den sie sonst
//!   erfragen müsste, an den Auto-Modus weiterreicht. Die Umsetzung
//!   (Vorfilter, Modellaufruf, Zeitlimit) lebt in `harw-runtime`
//!   (`auto_classifier.rs`); diese Crate kennt weder Modelle noch Prompts.
//! - [`AutoDecisionLog`] — das geteilte Protokoll der letzten
//!   Entscheidungen (`/permissions log`, TUI-Werkzeugzellen) samt
//!   **Sicherheitsdeckel**: nach [`CONSECUTIVE_DENIAL_CAP`] Ablehnungen in
//!   Folge oder [`TOTAL_DENIAL_CAP`] Ablehnungen insgesamt meldet
//!   [`AutoDecisionLog::record`] [`CapStatus::Tripped`], und der Aufrufer
//!   schaltet den Modus auf `ask` zurück. Runde 6, Teil A5: gezählt werden
//!   nur Ablehnungen, die nach der Umwandlung deny → ask übrig bleiben;
//!   dieselbe Aufruf-Signatur zählt binnen [`DENIAL_DEDUP_WINDOW_SECS`]
//!   Sekunden nur einmal ([`AutoDecisionLog::record_with_signature`]).
//! - [`AutoSessionContext`] — das „Ziel der Sitzung" (die letzten
//!   [`RECENT_GOALS_CAPACITY`] Nutzernachrichten, aktiver Plan) und die
//!   letzten Werkzeugaufrufe, die der Klassifizierer als Kontext bekommt.
//!
//! # Sicherheitsregeln (fail-closed)
//! - Ein Urteil, das nicht ausdrücklich `allow` lautet, gibt **nie** frei.
//! - [`AutoVerdict::fallback_ask`] ist die einzige Antwort auf Fehler,
//!   Zeitlimit oder unparsebare Klassifizierer-Antworten.
//! - `ALWAYS_ASK_TOOLS` (`harw-registry-defaults`) erreichen das Gate nie —
//!   die Standardpolitik prüft sie vorher; die Laufzeit prüft sie zusätzlich
//!   selbst (Verteidigung in der Tiefe).
//!
//! # Nebenläufigkeit
//! [`AutoDecisionLog`] und [`AutoSessionContext`] teilen ihren Zustand über
//! `Arc<Mutex<_>>`/`Arc<RwLock<_>>` wie
//! [`crate::approval_mode::ApprovalModeCell`]: Klone sehen denselben
//! Zustand. Ein vergifteter Lock wird über `into_inner` aufgelöst — beide
//! Typen halten nur Protokoll- und Kontextdaten; die sicherheitsrelevante
//! Entscheidung (ob freigegeben wird) trifft nie das Protokoll selbst.
//!
//! # Beispiele
//! ```rust
//! use harw_extension_api::auto_mode::{AutoDecision, AutoVerdict, VerdictSource};
//!
//! let verdict = AutoVerdict::fallback_ask("Zeitlimit überschritten");
//! assert_eq!(verdict.decision, AutoDecision::Ask);
//! assert_eq!(verdict.source, VerdictSource::Fallback);
//! assert!(AutoDecision::parse("ALLOW").is_some());
//! assert!(AutoDecision::parse("vielleicht").is_none());
//! ```

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};

use crate::{ApprovalDecision, ExtFuture, ToolCall};

/// Ablehnungen in Folge, nach denen der Auto-Modus auf `ask` zurückfällt.
pub const CONSECUTIVE_DENIAL_CAP: u32 = 3;

/// Ablehnungen je Sitzung insgesamt, nach denen der Auto-Modus auf `ask`
/// zurückfällt.
pub const TOTAL_DENIAL_CAP: u32 = 20;

/// Runde 6, Teil A5: Zeitfenster (Sekunden), in dem dieselbe
/// Aufruf-Signatur nur einmal als Ablehnung zählt.
pub const DENIAL_DEDUP_WINDOW_SECS: i64 = 60;

/// Runde 6, Teil A2: wie viele der letzten Nutzernachrichten
/// [`AutoSessionContext`] als „Ziel der Sitzung" behält.
pub const RECENT_GOALS_CAPACITY: usize = 3;

/// Wie viele Entscheidungen [`AutoDecisionLog`] höchstens behält.
pub const DECISION_LOG_CAPACITY: usize = 100;

/// Wie viele zuletzt angefragte Werkzeugaufrufe [`AutoSessionContext`]
/// höchstens behält.
pub const RECENT_CALLS_CAPACITY: usize = 16;

/// Anfang jedes Ablehnungstextes des Auto-Modus.
///
/// # Beschreibung
/// Die Standardpolitik liefert eine Ablehnung als
/// `ApprovalDecision::Deny(text)`; der Kern macht daraus das Werkzeugergebnis
/// `denied: <text>`. Die TUI erkennt eine Auto-Ablehnung an diesem Präfix.
pub const AUTO_DENIAL_PREFIX: &str = "Vom Auto-Modus abgelehnt";

/// Die Entscheidung eines Auto-Modus-Urteils.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutoDecision {
    /// Ohne Rückfrage ausführen.
    Allow,
    /// Die Person fragen (bzw. im Kind ohne Pausenrecht: mit Grund ablehnen).
    Ask,
    /// Ohne Rückfrage ablehnen; der Agent bekommt den Grund als Werkzeugfehler.
    ///
    /// Runde 6, Teil A1: das Gate wandelt ein `deny` des Klassifizierers in
    /// `ask` um, wo jemand gefragt werden kann (Wurzel, Kind mit
    /// Freigabe-Kanal); ein hartes `Deny` bleibt nur, wo niemand fragen kann.
    Deny,
}

impl AutoDecision {
    /// Der kanonische Kurzname (`allow`, `ask`, `deny`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        }
    }

    /// Liest eine Entscheidung aus Text.
    ///
    /// # Beschreibung
    /// Groß-/Kleinschreibung und umgebender Leerraum sind egal. Jeder andere
    /// Wert liefert `None` — der Aufrufer muss das als „ask" behandeln, nie
    /// als „allow".
    ///
    /// # Arguments
    /// - `text` (`&str`): der zu lesende Wert.
    ///
    /// # Rückgabe
    /// Die Entscheidung oder `None`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "allow" => Some(Self::Allow),
            "ask" => Some(Self::Ask),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }
}

/// Woher ein [`AutoVerdict`] stammt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerdictSource {
    /// Der deterministische Vorfilter hat zugeschlagen (nie `allow`).
    Prefilter,
    /// Der Modell-Klassifizierer hat geantwortet.
    Classifier,
    /// Fehler, Zeitlimit, unparsebare Antwort oder kein Modell: immer `ask`.
    Fallback,
    /// Der Sicherheitsdeckel ist ausgelöst; der Modus fällt auf `ask` zurück.
    SafetyCap,
}

impl VerdictSource {
    /// Kurzname für Protokoll und Audit.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prefilter => "prefilter",
            Self::Classifier => "classifier",
            Self::Fallback => "fallback",
            Self::SafetyCap => "safety-cap",
        }
    }
}

/// Das strukturierte Urteil des Auto-Modus über einen Werkzeugaufruf.
///
/// # Beschreibung
/// Entspricht der Klassifizierer-Ausgabe `{decision, category, reason}`
/// plus der Herkunft ([`VerdictSource`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoVerdict {
    /// Die Entscheidung.
    pub decision: AutoDecision,
    /// Kurze Kategorie (z. B. `"workspace-edit"`, `"credential-path"`).
    pub category: String,
    /// Einzeilige Begründung für Person und Agent.
    pub reason: String,
    /// Woher das Urteil stammt.
    pub source: VerdictSource,
    /// Runde 6, Teil A1: `true`, wenn dieses `ask` aus einem `deny` des
    /// Klassifizierers bzw. Vorfilters entstanden ist, weil die Person
    /// gefragt werden kann. Zählt nicht für den Sicherheitsdeckel.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub escalated: bool,
}

impl AutoVerdict {
    /// Baut ein Urteil.
    ///
    /// # Arguments
    /// - `decision` ([`AutoDecision`]), `category`/`reason` (`impl Into<String>`),
    ///   `source` ([`VerdictSource`]).
    #[must_use]
    pub fn new(
        decision: AutoDecision,
        category: impl Into<String>,
        reason: impl Into<String>,
        source: VerdictSource,
    ) -> Self {
        Self {
            decision,
            category: category.into(),
            reason: reason.into(),
            source,
            escalated: false,
        }
    }

    /// Runde 6, Teil A1: macht aus einem `deny` eine Rückfrage an die Person.
    ///
    /// # Beschreibung
    /// Kategorie, Grund und Herkunft bleiben erhalten, damit der
    /// Freigabedialog sie als „Auto-Modus: <Kategorie> – <Grund>" zeigen
    /// kann; [`Self::escalated`] wird gesetzt. Jede andere Entscheidung
    /// bleibt unverändert (ein `allow` wird nie berührt).
    ///
    /// # Rückgabe
    /// Das umgewandelte Urteil.
    #[must_use]
    pub fn escalate_to_ask(mut self) -> Self {
        if self.decision == AutoDecision::Deny {
            self.decision = AutoDecision::Ask;
            self.escalated = true;
        }
        self
    }

    /// Runde 6, Teil A1: der Grund für den Freigabedialog.
    ///
    /// # Rückgabe
    /// `"<Kategorie> – <Grund>"` (die Oberfläche stellt „Auto-Modus: "
    /// voran und bereinigt Steuerzeichen).
    #[must_use]
    pub fn dialog_reason(&self) -> String {
        format!("{} – {}", self.category.trim(), self.reason.trim())
    }

    /// Das Urteil für Fehler, Zeitlimit, unparsebare Antwort oder fehlendes
    /// Modell: **immer** `ask`, nie `allow`.
    ///
    /// # Arguments
    /// - `reason` (`impl Into<String>`): was schiefging.
    #[must_use]
    pub fn fallback_ask(reason: impl Into<String>) -> Self {
        Self::new(
            AutoDecision::Ask,
            "classifier-unavailable",
            reason,
            VerdictSource::Fallback,
        )
    }

    /// Der Ablehnungstext, den Agent und TUI sehen.
    ///
    /// # Rückgabe
    /// `"Vom Auto-Modus abgelehnt · <Kategorie>: <Grund>"` (siehe
    /// [`AUTO_DENIAL_PREFIX`]).
    #[must_use]
    pub fn denial_text(&self) -> String {
        format!(
            "{AUTO_DENIAL_PREFIX} · {}: {}",
            self.category.trim(),
            self.reason.trim()
        )
    }

    /// Übersetzt das Urteil in eine [`ApprovalDecision`].
    ///
    /// # Beschreibung
    /// `allow` → `Allow`, `ask` → `AskUser`, `deny` → `Deny(`[`Self::denial_text`]`)`.
    ///
    /// Runde 6, Teil A1: `ApprovalDecision::AskUser` trägt nur eine
    /// Anfrage-Id (Vertrag mit `harw-core`). Den Kontext der Rückfrage
    /// (Kategorie, Grund) liest die Oberfläche über
    /// [`AutoDecisionLog::verdict_for`] mit der Id des Werkzeugaufrufs — das
    /// Gate protokolliert das Urteil **nach** der Umwandlung deny → ask.
    #[must_use]
    pub fn into_approval(self) -> ApprovalDecision {
        match self.decision {
            AutoDecision::Allow => ApprovalDecision::Allow,
            AutoDecision::Ask => ApprovalDecision::AskUser(Default::default()),
            AutoDecision::Deny => ApprovalDecision::Deny(self.denial_text()),
        }
    }
}

/// Die Naht zwischen eingebauter Standardpolitik und Auto-Modus.
///
/// # Beschreibung
/// Die Standardpolitik ruft [`Self::decide`] **nur** im Modus `auto`, **nur**
/// für Aufrufe, die weder in `AUTO_APPROVED_TOOLS` noch in `ALWAYS_ASK_TOOLS`
/// stehen und keine Allow-/Deny-Regel treffen — also genau dort, wo sie
/// sonst `AskUser` liefern würde. Ein Gate kann damit nie mehr erlauben als
/// „diese eine Rückfrage entfällt".
///
/// # Vertrag
/// Die Zukunft muss in jedem Fall mit einem Urteil enden; Fehler werden als
/// [`AutoVerdict::fallback_ask`] gemeldet, nie als Panik.
pub trait AutoApprovalGate: Send + Sync + std::fmt::Debug {
    /// Urteilt über einen Aufruf.
    ///
    /// # Arguments
    /// - `call` (`&ToolCall`): der angefragte Aufruf.
    ///
    /// # Rückgabe
    /// Das Urteil.
    fn decide<'a>(&'a self, call: &'a ToolCall) -> ExtFuture<'a, AutoVerdict>;
}

/// Ergebnis von [`AutoDecisionLog::record`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapStatus {
    /// Der Deckel ist nicht erreicht.
    Ok,
    /// Diese Entscheidung hat den Deckel ausgelöst: der Aufrufer muss den
    /// Modus auf `ask` zurückstellen.
    Tripped,
}

/// Runde 6, Teil A5: welche Grenze des Sicherheitsdeckels gegriffen hat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapLimit {
    /// [`CONSECUTIVE_DENIAL_CAP`] Ablehnungen in Folge.
    Consecutive,
    /// [`TOTAL_DENIAL_CAP`] Ablehnungen in dieser Sitzung.
    Total,
}

impl CapLimit {
    /// Die Grenze als Satzteil („3 Ablehnungen in Folge" bzw. „20
    /// Ablehnungen in dieser Sitzung").
    #[must_use]
    pub fn describe(self) -> String {
        match self {
            Self::Consecutive => format!("{CONSECUTIVE_DENIAL_CAP} Ablehnungen in Folge"),
            Self::Total => format!("{TOTAL_DENIAL_CAP} Ablehnungen in dieser Sitzung"),
        }
    }
}

/// Ein Eintrag im [`AutoDecisionLog`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoLogEntry {
    /// Zeitpunkt der Entscheidung.
    pub at: jiff::Timestamp,
    /// Id des Werkzeugaufrufs (für die Werkzeugzelle der TUI).
    pub call_id: String,
    /// Werkzeugname.
    pub tool: String,
    /// Kurze, bereits von Geheimnissen bereinigte Argument-Zusammenfassung.
    pub summary: String,
    /// Das Urteil.
    pub verdict: AutoVerdict,
}

/// Innerer Zustand des [`AutoDecisionLog`].
#[derive(Debug, Default)]
struct LogState {
    /// Die letzten Entscheidungen, älteste zuerst.
    entries: VecDeque<AutoLogEntry>,
    /// Ablehnungen seit der letzten Freigabe.
    consecutive_denials: u32,
    /// Ablehnungen seit Sitzungsbeginn bzw. seit dem letzten Rücksetzen.
    total_denials: u32,
    /// Ob der Deckel ausgelöst ist (bis [`AutoDecisionLog::reset_cap`]).
    tripped: bool,
    /// Runde 6, Teil A5: welche Grenze zuletzt gegriffen hat.
    tripped_by: Option<CapLimit>,
    /// Runde 6, Teil A5: zuletzt **gezählte** Ablehnung je Signatur
    /// (nur ein Hash, nie Argumente — kein Geheimnis im Speicher).
    counted_signatures: VecDeque<(u64, jiff::Timestamp)>,
    /// Ein noch nicht angezeigter Hinweis für die Oberfläche.
    pending_notice: Option<String>,
}

/// Geteiltes Protokoll der Auto-Modus-Entscheidungen samt Sicherheitsdeckel.
///
/// # Beschreibung
/// Klone teilen denselben Zustand. Gezählt werden nur **Ablehnungen**
/// (`deny`): jede Ablehnung erhöht beide Zähler, jede Freigabe (`allow`)
/// setzt den Folgezähler zurück, ein `ask` lässt beide unverändert — auch
/// ein aus `deny` umgewandeltes `ask` (Runde 6, Teil A5). Mit Signatur
/// ([`Self::record_with_signature`]) zählt dieselbe Signatur binnen
/// [`DENIAL_DEDUP_WINDOW_SECS`] Sekunden nur einmal.
#[derive(Clone, Default)]
pub struct AutoDecisionLog(Arc<Mutex<LogState>>);

impl AutoDecisionLog {
    /// Erzeugt ein leeres Protokoll.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Führt `f` unter der Sperre aus; ein vergifteter Lock wird aufgelöst.
    fn with_state<T>(&self, f: impl FnOnce(&mut LogState) -> T) -> T {
        match self.0.lock() {
            Ok(mut guard) => f(&mut guard),
            Err(poisoned) => f(&mut poisoned.into_inner()),
        }
    }

    /// Hält eine Entscheidung fest und prüft den Sicherheitsdeckel.
    ///
    /// # Arguments
    /// - `entry` ([`AutoLogEntry`]): die Entscheidung.
    ///
    /// # Rückgabe
    /// [`CapStatus::Tripped`] genau bei der Entscheidung, die den Deckel
    /// erstmals erreicht ([`CONSECUTIVE_DENIAL_CAP`] in Folge oder
    /// [`TOTAL_DENIAL_CAP`] insgesamt); sonst [`CapStatus::Ok`].
    pub fn record(&self, entry: AutoLogEntry) -> CapStatus {
        self.record_inner(entry, None)
    }

    /// Runde 6, Teil A5: wie [`Self::record`], aber eine Ablehnung mit
    /// derselben `signature` zählt binnen [`DENIAL_DEDUP_WINDOW_SECS`]
    /// Sekunden (gemessen an [`AutoLogEntry::at`]) nur einmal.
    ///
    /// # Beschreibung
    /// Das Fenster gleitet bewusst **nicht**: eine Wiederholung innerhalb
    /// des Fensters verlängert es nicht. Ein Agent, der denselben Aufruf in
    /// Schleife wiederholt, erreicht den Deckel dadurch trotzdem (einmal je
    /// Fenster), nur nicht in Sekunden. Protokolliert wird jede Entscheidung.
    ///
    /// # Arguments
    /// - `entry` ([`AutoLogEntry`]): die Entscheidung.
    /// - `signature` (`u64`): Hash aus Werkzeug und normalisierten
    ///   Argumenten (vom Aufrufer gebildet).
    ///
    /// # Rückgabe
    /// Wie [`Self::record`].
    pub fn record_with_signature(&self, entry: AutoLogEntry, signature: u64) -> CapStatus {
        self.record_inner(entry, Some(signature))
    }

    fn record_inner(&self, entry: AutoLogEntry, signature: Option<u64>) -> CapStatus {
        self.with_state(|state| {
            match entry.verdict.decision {
                AutoDecision::Deny => {
                    if state.should_count(signature, entry.at) {
                        state.consecutive_denials = state.consecutive_denials.saturating_add(1);
                        state.total_denials = state.total_denials.saturating_add(1);
                    }
                }
                AutoDecision::Allow => state.consecutive_denials = 0,
                AutoDecision::Ask => {}
            }
            let last_denial = (entry.verdict.decision == AutoDecision::Deny).then(|| entry.clone());
            state.entries.push_back(entry);
            while state.entries.len() > DECISION_LOG_CAPACITY {
                state.entries.pop_front();
            }
            let limit = if state.consecutive_denials >= CONSECUTIVE_DENIAL_CAP {
                Some(CapLimit::Consecutive)
            } else if state.total_denials >= TOTAL_DENIAL_CAP {
                Some(CapLimit::Total)
            } else {
                None
            };
            match limit {
                Some(limit) if !state.tripped => {
                    state.tripped = true;
                    state.tripped_by = Some(limit);
                    state.pending_notice = Some(cap_notice(limit, last_denial.as_ref()));
                    CapStatus::Tripped
                }
                _ => CapStatus::Ok,
            }
        })
    }

    /// Alle behaltenen Entscheidungen, älteste zuerst.
    #[must_use]
    pub fn entries(&self) -> Vec<AutoLogEntry> {
        self.with_state(|state| state.entries.iter().cloned().collect())
    }

    /// Die letzten `n` Entscheidungen, älteste zuerst.
    #[must_use]
    pub fn recent(&self, n: usize) -> Vec<AutoLogEntry> {
        self.with_state(|state| {
            let skip = state.entries.len().saturating_sub(n);
            state.entries.iter().skip(skip).cloned().collect()
        })
    }

    /// Das Urteil zu einem Werkzeugaufruf, falls protokolliert.
    ///
    /// # Arguments
    /// - `call_id` (`&str`): die Id des Aufrufs.
    #[must_use]
    pub fn verdict_for(&self, call_id: &str) -> Option<AutoVerdict> {
        self.with_state(|state| {
            state
                .entries
                .iter()
                .rev()
                .find(|entry| entry.call_id == call_id)
                .map(|entry| entry.verdict.clone())
        })
    }

    /// `(Ablehnungen in Folge, Ablehnungen insgesamt)`.
    #[must_use]
    pub fn denial_counters(&self) -> (u32, u32) {
        self.with_state(|state| (state.consecutive_denials, state.total_denials))
    }

    /// Ob der Sicherheitsdeckel ausgelöst ist.
    #[must_use]
    pub fn is_tripped(&self) -> bool {
        self.with_state(|state| state.tripped)
    }

    /// Runde 6, Teil A5: welche Grenze den Deckel ausgelöst hat (`None`,
    /// solange er nicht ausgelöst ist).
    #[must_use]
    pub fn tripped_by(&self) -> Option<CapLimit> {
        self.with_state(|state| state.tripped.then_some(state.tripped_by).flatten())
    }

    /// Setzt Deckel und Zähler zurück (die Person hat `auto` wieder gewählt).
    pub fn reset_cap(&self) {
        self.with_state(|state| {
            state.tripped = false;
            state.tripped_by = None;
            state.consecutive_denials = 0;
            state.total_denials = 0;
            state.counted_signatures.clear();
            state.pending_notice = None;
        });
    }

    /// Entnimmt den noch nicht angezeigten Hinweis (höchstens einmal).
    #[must_use]
    pub fn take_notice(&self) -> Option<String> {
        self.with_state(|state| state.pending_notice.take())
    }
}

impl std::fmt::Debug for AutoDecisionLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (consecutive, total) = self.denial_counters();
        f.debug_struct("AutoDecisionLog")
            .field("entries", &self.entries().len())
            .field("consecutive_denials", &consecutive)
            .field("total_denials", &total)
            .field("tripped", &self.is_tripped())
            .finish()
    }
}

impl LogState {
    /// Runde 6, Teil A5: ob eine Ablehnung mit `signature` zum Zeitpunkt
    /// `at` zählt; merkt sich gezählte Signaturen (nicht gleitend).
    fn should_count(&mut self, signature: Option<u64>, at: jiff::Timestamp) -> bool {
        let Some(signature) = signature else {
            return true;
        };
        let window = jiff::SignedDuration::from_secs(DENIAL_DEDUP_WINDOW_SECS);
        // Abgelaufene Einträge verwerfen (Zeit rückwärts: behalten, damit
        // eine verstellte Uhr nicht mehr zählen lässt als vorgesehen).
        self.counted_signatures
            .retain(|(_, counted_at)| at.duration_since(*counted_at) < window);
        if self
            .counted_signatures
            .iter()
            .any(|(existing, _)| *existing == signature)
        {
            return false;
        }
        self.counted_signatures.push_back((signature, at));
        while self.counted_signatures.len() > DECISION_LOG_CAPACITY {
            self.counted_signatures.pop_front();
        }
        true
    }
}

/// Der Hinweis beim Auslösen des Deckels (sinngemäß wie in Claude Code).
///
/// # Beschreibung
/// Runde 6, Teil A5: nennt genau die Grenze, die gegriffen hat, und — falls
/// bekannt — die letzte Ablehnung mit Werkzeug, Kategorie und Grund
/// (bereits bereinigt; Grund einzeilig gekürzt).
fn cap_notice(limit: CapLimit, last_denial: Option<&AutoLogEntry>) -> String {
    let last = last_denial
        .map(|entry| {
            let reason: String = entry
                .verdict
                .reason
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .take(200)
                .collect();
            format!(
                " Letzte Ablehnung: {} · {}: {}.",
                entry.tool,
                entry.verdict.category.trim(),
                reason.trim()
            )
        })
        .unwrap_or_default();
    format!(
        "Auto-Modus pausiert: {} erreicht.{last} \
         Der Freigabemodus steht wieder auf „ask“ — jeder weitere Aufruf wird erfragt. \
         Mit `/permissions mode auto` bzw. Umschalt+Tab lässt sich der Auto-Modus wieder einschalten.",
        limit.describe()
    )
}

/// Innerer Zustand des [`AutoSessionContext`].
#[derive(Debug, Default)]
struct ContextState {
    /// Runde 6, Teil A2: die letzten [`RECENT_GOALS_CAPACITY`]
    /// Nutzernachrichten, älteste zuerst (Ringpuffer).
    goals: VecDeque<String>,
    /// Aktiver Plan (Kurzfassung).
    plan: Option<String>,
    /// Zuletzt angefragte Werkzeugaufrufe (bereinigte Kurzfassungen).
    recent_calls: VecDeque<String>,
}

/// Geteilter Sitzungskontext für den Klassifizierer.
///
/// # Beschreibung
/// Die Oberfläche setzt das Ziel ([`Self::set_goal`]) bei jeder abgeschickten
/// Nutzernachricht — Runde 6, Teil A2: als Ringpuffer der letzten
/// [`RECENT_GOALS_CAPACITY`] Nachrichten, damit ein kurzes „ja, mach" den
/// eigentlichen Auftrag nicht verdrängt —, optional den aktiven Plan
/// ([`Self::set_plan`]); das Gate
/// hängt jeden beurteilten Aufruf an ([`Self::push_recent_call`]). Klone
/// teilen denselben Zustand.
#[derive(Clone, Default)]
pub struct AutoSessionContext(Arc<RwLock<ContextState>>);

impl AutoSessionContext {
    /// Erzeugt einen leeren Kontext.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn write<T>(&self, f: impl FnOnce(&mut ContextState) -> T) -> T {
        match self.0.write() {
            Ok(mut guard) => f(&mut guard),
            Err(poisoned) => f(&mut poisoned.into_inner()),
        }
    }

    fn read<T>(&self, f: impl FnOnce(&ContextState) -> T) -> T {
        match self.0.read() {
            Ok(guard) => f(&guard),
            Err(poisoned) => f(&poisoned.into_inner()),
        }
    }

    /// Hängt eine Nutzernachricht an das Ziel der Sitzung an.
    ///
    /// # Beschreibung
    /// Runde 6, Teil A2: Ringpuffer der letzten [`RECENT_GOALS_CAPACITY`]
    /// Nachrichten; eine leere Nachricht wird ignoriert.
    pub fn set_goal(&self, goal: impl Into<String>) {
        let goal = goal.into();
        if goal.trim().is_empty() {
            return;
        }
        self.write(|state| {
            state.goals.push_back(goal);
            while state.goals.len() > RECENT_GOALS_CAPACITY {
                state.goals.pop_front();
            }
        });
    }

    /// Runde 6, Teil A2: die letzten Nutzernachrichten, älteste zuerst.
    #[must_use]
    pub fn recent_goals(&self) -> Vec<String> {
        self.read(|state| state.goals.iter().cloned().collect())
    }

    /// Setzt den aktiven Plan (oder entfernt ihn mit `None`).
    pub fn set_plan(&self, plan: Option<String>) {
        self.write(|state| state.plan = plan);
    }

    /// Die letzte Nutzernachricht.
    #[must_use]
    pub fn goal(&self) -> Option<String> {
        self.read(|state| state.goals.back().cloned())
    }

    /// Der aktive Plan.
    #[must_use]
    pub fn plan(&self) -> Option<String> {
        self.read(|state| state.plan.clone())
    }

    /// Hängt einen beurteilten Aufruf an (höchstens
    /// [`RECENT_CALLS_CAPACITY`] bleiben).
    pub fn push_recent_call(&self, summary: impl Into<String>) {
        let summary = summary.into();
        self.write(|state| {
            state.recent_calls.push_back(summary);
            while state.recent_calls.len() > RECENT_CALLS_CAPACITY {
                state.recent_calls.pop_front();
            }
        });
    }

    /// Die letzten `n` Aufrufe, älteste zuerst.
    #[must_use]
    pub fn recent_calls(&self, n: usize) -> Vec<String> {
        self.read(|state| {
            let skip = state.recent_calls.len().saturating_sub(n);
            state.recent_calls.iter().skip(skip).cloned().collect()
        })
    }
}

impl std::fmt::Debug for AutoSessionContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AutoSessionContext")
            .field("has_goal", &self.goal().is_some())
            .field("has_plan", &self.plan().is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn entry(decision: AutoDecision) -> AutoLogEntry {
        AutoLogEntry {
            at: jiff::Timestamp::UNIX_EPOCH,
            call_id: "call-1".to_owned(),
            tool: "shell.exec".to_owned(),
            summary: "cargo test".to_owned(),
            verdict: AutoVerdict::new(decision, "test", "grund", VerdictSource::Classifier),
        }
    }

    #[test]
    fn decision_parse_accepts_known_values_only() {
        assert_eq!(AutoDecision::parse(" Allow "), Some(AutoDecision::Allow));
        assert_eq!(AutoDecision::parse("ASK"), Some(AutoDecision::Ask));
        assert_eq!(AutoDecision::parse("deny"), Some(AutoDecision::Deny));
        assert_eq!(AutoDecision::parse("yes"), None);
        assert_eq!(AutoDecision::parse(""), None);
    }

    #[test]
    fn fallback_is_always_ask() {
        let verdict = AutoVerdict::fallback_ask("kaputt");
        assert_eq!(verdict.decision, AutoDecision::Ask);
        assert!(matches!(
            verdict.into_approval(),
            ApprovalDecision::AskUser(_)
        ));
    }

    #[test]
    fn deny_maps_to_deny_with_prefixed_reason() -> TestResult {
        let verdict = AutoVerdict::new(
            AutoDecision::Deny,
            "credential-path",
            "schreibt ~/.ssh",
            VerdictSource::Classifier,
        );
        match verdict.into_approval() {
            ApprovalDecision::Deny(text) => {
                assert!(text.starts_with(AUTO_DENIAL_PREFIX), "{text}");
                assert!(text.contains("credential-path"), "{text}");
                Ok(())
            }
            other => Err(TestError::Unexpected(format!(
                "erwartet Deny, war {other:?}"
            ))),
        }
    }

    #[test]
    fn three_denials_in_a_row_trip_the_cap_once() {
        let log = AutoDecisionLog::new();
        assert_eq!(log.record(entry(AutoDecision::Deny)), CapStatus::Ok);
        assert_eq!(log.record(entry(AutoDecision::Deny)), CapStatus::Ok);
        assert_eq!(log.record(entry(AutoDecision::Deny)), CapStatus::Tripped);
        assert!(log.is_tripped());
        assert!(log.take_notice().is_some());
        assert!(log.take_notice().is_none(), "Hinweis nur einmal");
        assert_eq!(
            log.record(entry(AutoDecision::Deny)),
            CapStatus::Ok,
            "ein bereits ausgelöster Deckel meldet sich nicht erneut"
        );
    }

    #[test]
    fn an_allow_resets_the_consecutive_counter_but_not_the_total() {
        let log = AutoDecisionLog::new();
        log.record(entry(AutoDecision::Deny));
        log.record(entry(AutoDecision::Deny));
        log.record(entry(AutoDecision::Allow));
        log.record(entry(AutoDecision::Ask));
        assert_eq!(log.denial_counters(), (0, 2));
        assert_eq!(log.record(entry(AutoDecision::Deny)), CapStatus::Ok);
        assert!(!log.is_tripped());
    }

    #[test]
    fn twenty_denials_in_total_trip_the_cap() {
        let log = AutoDecisionLog::new();
        let mut tripped = 0;
        for _ in 0..(TOTAL_DENIAL_CAP - 1) {
            if log.record(entry(AutoDecision::Deny)) == CapStatus::Tripped {
                tripped += 1;
            }
            log.record(entry(AutoDecision::Allow));
        }
        assert_eq!(tripped, 0);
        assert_eq!(log.record(entry(AutoDecision::Deny)), CapStatus::Tripped);
    }

    #[test]
    fn reset_cap_clears_counters() {
        let log = AutoDecisionLog::new();
        for _ in 0..3 {
            log.record(entry(AutoDecision::Deny));
        }
        log.reset_cap();
        assert!(!log.is_tripped());
        assert_eq!(log.denial_counters(), (0, 0));
    }

    #[test]
    fn log_is_bounded_and_finds_verdicts_by_call_id() {
        let log = AutoDecisionLog::new();
        for index in 0..(DECISION_LOG_CAPACITY + 5) {
            let mut item = entry(AutoDecision::Allow);
            item.call_id = format!("call-{index}");
            log.record(item);
        }
        assert_eq!(log.entries().len(), DECISION_LOG_CAPACITY);
        assert!(log.verdict_for("call-0").is_none());
        assert!(
            log.verdict_for(&format!("call-{}", DECISION_LOG_CAPACITY + 4))
                .is_some()
        );
        assert_eq!(log.recent(3).len(), 3);
    }

    #[test]
    fn session_context_keeps_goal_plan_and_recent_calls() {
        let context = AutoSessionContext::new();
        context.set_goal("Tests reparieren");
        context.set_plan(Some("1. cargo test".to_owned()));
        for index in 0..(RECENT_CALLS_CAPACITY + 2) {
            context.push_recent_call(format!("aufruf {index}"));
        }
        assert_eq!(context.goal().as_deref(), Some("Tests reparieren"));
        assert_eq!(context.plan().as_deref(), Some("1. cargo test"));
        assert_eq!(context.recent_calls(100).len(), RECENT_CALLS_CAPACITY);
        assert_eq!(context.recent_calls(2).len(), 2);
        // Runde 6, Teil A2: eine leere Nachricht verdrängt das Ziel nicht.
        context.set_goal("   ");
        assert_eq!(context.goal().as_deref(), Some("Tests reparieren"));
    }

    // ── Runde 6, Teil A ─────────────────────────────────────────────────

    fn entry_at(decision: AutoDecision, secs: i64) -> TestResult<AutoLogEntry> {
        let mut item = entry(decision);
        item.at = jiff::Timestamp::from_second(secs)
            .map_err(|error| TestError::Unexpected(format!("Zeitstempel: {error}")))?;
        Ok(item)
    }

    /// Runde 6, Teil A2: ein kurzes „ja, mach" verdrängt den eigentlichen
    /// Auftrag nicht; es bleiben die letzten drei Nachrichten.
    #[test]
    fn session_goal_is_a_ring_buffer_of_the_last_three_messages() {
        let context = AutoSessionContext::new();
        for message in ["alt", "Verschieb export.md nach ~", "ja, mach", "bitte"] {
            context.set_goal(message);
        }
        assert_eq!(
            context.recent_goals(),
            vec![
                "Verschieb export.md nach ~".to_owned(),
                "ja, mach".to_owned(),
                "bitte".to_owned()
            ]
        );
        assert_eq!(context.goal().as_deref(), Some("bitte"));
        assert_eq!(RECENT_GOALS_CAPACITY, 3);
    }

    /// Runde 6, Teil A1: `deny` → `ask` behält Kategorie und Grund; ein
    /// `allow` bleibt unberührt.
    #[test]
    fn escalate_to_ask_keeps_category_and_reason() {
        let verdict = AutoVerdict::new(
            AutoDecision::Deny,
            "exfiltration",
            "verschiebt nach ~",
            VerdictSource::Classifier,
        )
        .escalate_to_ask();
        assert_eq!(verdict.decision, AutoDecision::Ask);
        assert!(verdict.escalated);
        assert_eq!(verdict.dialog_reason(), "exfiltration – verschiebt nach ~");
        assert!(matches!(
            verdict.into_approval(),
            ApprovalDecision::AskUser(_)
        ));
        let allow = AutoVerdict::new(AutoDecision::Allow, "c", "r", VerdictSource::Classifier)
            .escalate_to_ask();
        assert_eq!(allow.decision, AutoDecision::Allow);
        assert!(!allow.escalated);
    }

    /// Runde 6, Teil A5: ein umgewandeltes `ask` zählt nicht als Ablehnung.
    #[test]
    fn escalated_asks_never_count_towards_the_cap() {
        let log = AutoDecisionLog::new();
        for _ in 0..10 {
            let mut item = entry(AutoDecision::Deny);
            item.verdict = item.verdict.escalate_to_ask();
            assert_eq!(log.record(item), CapStatus::Ok);
        }
        assert_eq!(log.denial_counters(), (0, 0));
        assert!(!log.is_tripped());
    }

    /// Runde 6, Teil A5: dieselbe Signatur zählt binnen 60 s nur einmal,
    /// danach wieder; andere Signaturen zählen sofort.
    #[test]
    fn same_signature_within_the_window_counts_once() -> TestResult {
        let log = AutoDecisionLog::new();
        for secs in [0, 10, 30, 59] {
            assert_eq!(
                log.record_with_signature(entry_at(AutoDecision::Deny, secs)?, 7),
                CapStatus::Ok
            );
        }
        assert_eq!(log.denial_counters(), (1, 1));
        assert_eq!(
            log.entries().len(),
            4,
            "protokolliert wird jede Entscheidung"
        );
        log.record_with_signature(entry_at(AutoDecision::Deny, DENIAL_DEDUP_WINDOW_SECS)?, 7);
        assert_eq!(log.denial_counters(), (2, 2), "nach 60 s zählt sie wieder");
        let status = log.record_with_signature(entry_at(AutoDecision::Deny, 61)?, 8);
        assert_eq!(status, CapStatus::Tripped, "andere Signatur zählt sofort");
        assert_eq!(log.tripped_by(), Some(CapLimit::Consecutive));
        Ok(())
    }

    /// Runde 6, Teil A5: der Hinweis nennt die Grenze, die gegriffen hat,
    /// und die letzte Ablehnung mit Grund.
    #[test]
    fn cap_notice_names_the_limit_that_tripped() -> TestResult {
        let log = AutoDecisionLog::new();
        for _ in 0..CONSECUTIVE_DENIAL_CAP {
            log.record(entry(AutoDecision::Deny));
        }
        let notice = log.take_notice().ok_or(TestError::Missing("Hinweis"))?;
        assert!(notice.contains("3 Ablehnungen in Folge"), "{notice}");
        assert!(!notice.contains("insgesamt"), "{notice}");
        assert!(
            notice.contains("Letzte Ablehnung: shell.exec · test: grund"),
            "{notice}"
        );

        let log = AutoDecisionLog::new();
        for _ in 0..TOTAL_DENIAL_CAP {
            log.record(entry(AutoDecision::Deny));
            log.record(entry(AutoDecision::Allow));
        }
        assert_eq!(log.tripped_by(), Some(CapLimit::Total));
        let notice = log.take_notice().ok_or(TestError::Missing("Hinweis"))?;
        assert!(
            notice.contains("20 Ablehnungen in dieser Sitzung"),
            "{notice}"
        );
        assert!(!notice.contains("in Folge"), "{notice}");
        log.reset_cap();
        assert_eq!(log.tripped_by(), None);
        Ok(())
    }
}
