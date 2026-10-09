//! Aktivitätsjournal, Endbericht und Eltern-Kind-Nachrichten (Runde 5, Teil M).
//!
//! # Verantwortungsbereich
//! Ein Kind, das nicht regulär endet (Zeitbudget, Lease-Ablauf, Abbruch,
//! Provider-/Turn-Fehler, Werkzeug-Budget), darf seine Arbeit nicht
//! mitnehmen. Dieses Modul hält dafür drei Dinge, alle am
//! [`ManagedAgentSpawner`] und alle deterministisch, ohne Modellaufruf:
//!
//! 1. **Aktivitätsjournal** ([`ChildJournal`]): fortlaufend im Spawner
//!    mitgeschrieben — Auftrag, Werkzeugaufrufe mit Kurzargumenten und
//!    Ergebnisstatus, geänderte Dateien (aus `fs.write`/`fs.edit` und, soweit
//!    erkennbar, aus Shell-Befehlen), gestartete Enkel samt Ergebnis-Kurzform,
//!    ein- und ausgehende Nachrichten, letzter Assistententext. Gedeckelt auf
//!    [`JOURNAL_MAX_ENTRIES`] Einträge bzw. [`JOURNAL_MAX_BYTES`].
//! 2. **Endbericht** ([`ChildEndReport`]): statt eines nackten Fehlers
//!    bekommt der Elternteil `status: failed|timeout|cancelled`, den Grund,
//!    die Übergabe (falls die Verdichtung aus `child_handoff` möglich war)
//!    und eine Journal-Kurzfassung. Die erste Zeile ist maschinenlesbar
//!    ([`CHILD_END_MARKER`], [`parse_child_end`]); die TUI zeigt daraus den
//!    echten Grund ([`child_end_label`]).
//! 3. **Nachrichten** in beide Richtungen: `agent.message` legt Text in das
//!    begrenzte Postfach eines eigenen, laufenden Kindes, das es an seiner
//!    nächsten Runden-Grenze als „[Nachricht von <rolle>] …" liest;
//!    `parent.message` meldet eine Information (Rate-Limit) oder stellt eine
//!    Frage an den direkten Elternteil und wartet mit Zeitlimit auf die
//!    Antwort.
//!
//! # Sicherheit
//! Nichts hier verleiht Rechte. Nachrichten sind reiner Text zwischen
//! direkt verbundenen Sitzungen; die Eltern-Kind-Bindung prüft der Spawner
//! (Admission-Record), nie ein Modell-Argument. Fremde, unbekannte und
//! beendete Kinder bekommen dieselbe Ablehnung. Jede Nachricht ist auf
//! [`MESSAGE_MAX_BYTES`] gedeckelt, jedes Postfach auf
//! [`MAILBOX_MAX_MESSAGES`].
//!
//! # Nebenläufigkeit
//! [`ChildComms`] ist `Send + Sync`: ein `std::sync::Mutex`, der nie über
//! einen `await` gehalten wird, ein `tokio::sync::Notify` für neue
//! Eltern-Nachrichten und `oneshot`-Kanäle für wartende Fragen.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use harw_extension_api::AgentSpawnError;
use harw_types::SessionId;
use jiff::Timestamp;
use serde_json::{Value, json};
use tokio::sync::{Notify, oneshot};

use crate::child_controller::ManagedAgentSpawner;

// ── Grenzen ──────────────────────────────────────────────────────────────────

/// Name des Werkzeugs Eltern → Kind.
pub const AGENT_MESSAGE_TOOL: &str = "agent.message";

/// Name des Werkzeugs Kind → Eltern.
pub const PARENT_MESSAGE_TOOL: &str = "parent.message";

/// Höchstzahl Journal-Einträge je Kind; ältere fallen heraus (gezählt).
pub const JOURNAL_MAX_ENTRIES: usize = 200;

/// Byte-Deckel der Journal-Einträge je Kind; ältere fallen heraus.
pub const JOURNAL_MAX_BYTES: usize = 64 * 1024;

/// Höchstzahl gemerkter geänderter Dateien je Kind.
pub const JOURNAL_MAX_FILES: usize = 128;

/// Höchstlänge (Zeichen) eines Details in einem Journal-Eintrag.
const ENTRY_DETAIL_MAX_CHARS: usize = 200;

/// Höchstlänge (Zeichen) des Auftrags im Journal.
const TASK_MAX_CHARS: usize = 2_000;

/// Höchstlänge (Bytes) des letzten Assistententexts im Journal.
pub const LAST_TEXT_MAX_BYTES: usize = 4 * 1024;

/// Byte-Deckel der Journal-Kurzfassung im Endbericht.
pub const END_SUMMARY_MAX_BYTES: usize = 6 * 1024;

/// Wie viele Journale beendeter Kinder vorrätig bleiben (`agent.status`,
/// `agent.result {part: "journal"}`); darüber fällt das älteste heraus.
pub const FINISHED_JOURNALS_KEEP: usize = 32;

/// Höchstzahl wartender Nachrichten im Postfach eines Kindes.
pub const MAILBOX_MAX_MESSAGES: usize = 16;

/// Längendeckel einer Nachricht (beide Richtungen).
pub const MESSAGE_MAX_BYTES: usize = 4 * 1024;

/// Mindestabstand zweier `parent.message {kind: "info"}` desselben Kindes.
pub const PARENT_INFO_MIN_INTERVAL: Duration = Duration::from_secs(30);

/// Wie lange `parent.message {kind: "question"}` auf eine Antwort wartet.
pub const PARENT_QUESTION_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Höchstzahl ungelesener Kind-Meldungen je Elternteil (Eingang der Wurzel).
pub const PARENT_INBOX_MAX: usize = 32;

/// Zeitlimit der Übergabe-Verdichtung nach einem nicht regulären Ende.
pub const END_HANDOFF_TIMEOUT: Duration = Duration::from_secs(20);

/// Token-Reserve der End-Verdichtung, wenn das Kind kein Token-Budget hat.
pub const END_HANDOFF_RESERVE_TOKENS: u64 = 16_000;

/// Antwort an ein fragendes Kind, wenn niemand rechtzeitig antwortet.
pub const NO_ANSWER_REPLY: &str = "keine Antwort – arbeite mit einer begründeten Annahme weiter";

/// Anfang der maschinenlesbaren ersten Zeile eines Endberichts.
pub const CHILD_END_MARKER: &str = "[child_end";

// ── Hilfen ───────────────────────────────────────────────────────────────────

/// Einzeiliger Auszug: Zeilenumbrüche werden zu ` ⏎ `, auf `max_chars`
/// Zeichen gekürzt (mit `…`).
fn excerpt(text: &str, max_chars: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = flat.chars().take(max_chars).collect();
    if flat.chars().count() > max_chars {
        out.push('…');
    }
    out
}

/// Kappt `text` auf höchstens `max_bytes` (Zeichengrenze), mit Hinweis.
fn cap_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [{} Bytes ausgelassen]", &text[..end], text.len() - end)
}

/// Kurzform der Argumente eines Werkzeugaufrufs.
fn short_args(tool: &str, args: &Value) -> String {
    let field = |name: &str| args.get(name).and_then(Value::as_str);
    let text = if let Some(path) = field("path") {
        path.to_owned()
    } else if let Some(command) = field("command") {
        command.to_owned()
    } else if let Some(task) = field("task").or_else(|| field("question")) {
        task.to_owned()
    } else if tool.starts_with("transfer_to_") || args.is_null() {
        String::new()
    } else {
        args.to_string()
    };
    excerpt(&text, ENTRY_DETAIL_MAX_CHARS / 2)
}

/// Entfernt umschließende Anführungszeichen eines Shell-Tokens.
fn unquote(token: &str) -> &str {
    token.trim_matches(|c| c == '"' || c == '\'')
}

/// Ob ein Shell-Token als Dateipfad taugt.
fn plausible_path(token: &str) -> bool {
    !token.is_empty()
        && !token.starts_with('-')
        && !token.starts_with('&')
        && !token.starts_with('$')
        && token != "/dev/null"
        && !token.contains(['|', ';', '<', '>', '(', ')', '*', '?'])
}

/// Dateien, die ein Shell-Befehl erkennbar schreibt (best effort):
/// Umleitungen (`>`/`>>`), `tee`, `touch`, `sed -i`, Ziel von `cp`/`mv`,
/// `rm`. Alles andere bleibt unerkannt — das Journal behauptet nichts, was
/// es nicht sieht.
#[must_use]
pub fn shell_written_files(command: &str) -> Vec<String> {
    let mut files = Vec::new();
    for segment in command.split(['\n', ';', '|']).flat_map(|s| s.split("&&")) {
        let tokens: Vec<&str> = segment.split_whitespace().map(unquote).collect();
        for (index, token) in tokens.iter().copied().enumerate() {
            let redirected = token
                .strip_prefix(">>")
                .or_else(|| token.strip_prefix('>'))
                .or_else(|| token.strip_prefix("1>"));
            if let Some(rest) = redirected {
                let target = if rest.is_empty() {
                    tokens.get(index + 1).copied().unwrap_or_default()
                } else {
                    rest
                };
                if plausible_path(target) {
                    files.push(target.to_owned());
                }
            }
        }
        let Some(program) = tokens.first().map(|t| t.rsplit('/').next().unwrap_or(*t)) else {
            continue;
        };
        let operands: Vec<&str> = tokens
            .iter()
            .skip(1)
            .copied()
            .take_while(|t| !t.starts_with('>') && !t.starts_with("2>") && !t.starts_with("1>"))
            .filter(|t| plausible_path(t))
            .collect();
        match program {
            "tee" | "touch" | "rm" => files.extend(operands.iter().map(|t| (*t).to_owned())),
            "cp" | "mv" | "install" => {
                if operands.len() >= 2
                    && let Some(last) = operands.last()
                {
                    files.push((*last).to_owned());
                }
            }
            "sed" if tokens.iter().any(|t| t.starts_with("-i")) => {
                if let Some(last) = operands.last() {
                    files.push((*last).to_owned());
                }
            }
            _ => {}
        }
    }
    files.sort();
    files.dedup();
    files
}

// ── Journal ──────────────────────────────────────────────────────────────────

/// Art einer Nachricht an den Elternteil.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentMessageKind {
    /// Information; geht an den Elternteil, keine Antwort erwartet.
    Info,
    /// Frage; das Kind wartet mit Zeitlimit auf `agent.message`.
    Question,
}

impl ParentMessageKind {
    /// Stabiles Label (`info`, `question`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Question => "question",
        }
    }

    /// Parst das Modell-Argument `kind` (Vorgabe `info`).
    ///
    /// # Errors
    /// Eine Meldung für das Modell bei einem unbekannten Wert.
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim) {
            None | Some("" | "info") => Ok(Self::Info),
            Some("question") => Ok(Self::Question),
            Some(other) => Err(format!(
                "`kind` muss \"info\" oder \"question\" sein, nicht \"{other}\""
            )),
        }
    }
}

/// Inhalt eines Journal-Eintrags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalEntryKind {
    /// Ein abgeschlossener Werkzeugaufruf.
    Tool {
        /// Werkzeugname.
        name: String,
        /// Kurzform der Argumente.
        args: String,
        /// Ob der Aufruf erfolgreich war.
        ok: bool,
        /// Kurzform des Ergebnisses bzw. der Fehlermeldung.
        result: String,
    },
    /// Ein gestarteter bzw. beendeter Enkel (Kind dieses Kindes).
    Grandchild {
        /// Kind-ID des Enkels.
        child: String,
        /// Seine Rolle.
        role: String,
        /// `None` beim Start, sonst Status und Ergebnis-Kurzform.
        outcome: Option<String>,
    },
    /// Eine Nachricht des Elternteils (`agent.message`).
    MessageIn {
        /// Absenderrolle.
        from: String,
        /// Kurzform des Textes.
        text: String,
    },
    /// Eine Nachricht an den Elternteil (`parent.message`).
    MessageOut {
        /// `info` oder `question`.
        kind: ParentMessageKind,
        /// Kurzform des Textes.
        text: String,
    },
    /// Sonstiger Vermerk (z. B. das Ende).
    Note(String),
}

/// Ein Eintrag im Aktivitätsjournal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntry {
    /// Laufende Nummer (ab 1, auch über verworfene Einträge hinweg).
    pub seq: u64,
    /// Zeitpunkt.
    pub at: Timestamp,
    /// Inhalt.
    pub kind: JournalEntryKind,
}

impl JournalEntry {
    /// Eine Zeile für Modell und Anzeige.
    #[must_use]
    pub fn line(&self) -> String {
        let body = match &self.kind {
            JournalEntryKind::Tool {
                name,
                args,
                ok,
                result,
            } => {
                let status = if *ok { "ok" } else { "Fehler" };
                if result.is_empty() {
                    format!("{name}({args}) → {status}")
                } else {
                    format!("{name}({args}) → {status}: {result}")
                }
            }
            JournalEntryKind::Grandchild {
                child,
                role,
                outcome: None,
            } => format!("startet {role} ({child})"),
            JournalEntryKind::Grandchild {
                child,
                role,
                outcome: Some(outcome),
            } => format!("{role} ({child}) beendet: {outcome}"),
            JournalEntryKind::MessageIn { from, text } => format!("Nachricht von {from}: {text}"),
            JournalEntryKind::MessageOut { kind, text } => {
                format!("an Elternteil ({}): {text}", kind.as_str())
            }
            JournalEntryKind::Note(text) => text.clone(),
        };
        format!("#{} {}", self.seq, body)
    }

    /// Byte-Gewicht für den Deckel.
    fn weight(&self) -> usize {
        self.line().len()
    }

    /// Als JSON für das Modell.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({ "seq": self.seq, "at": self.at.to_string(), "text": self.line() })
    }
}

/// Wie ein Kind nicht regulär endete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildEndStatus {
    /// Fehler (Provider/Turn, Werkzeug-Budget, unzulässige Pause …).
    Failed,
    /// Zeitbudget oder Lease abgelaufen.
    Timeout,
    /// Abgebrochen (Nutzerin, Elternteil, Freigabe).
    Cancelled,
}

impl ChildEndStatus {
    /// Stabiles Label (`failed`, `timeout`, `cancelled`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Failed => "failed",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parst das stabile Label.
    #[must_use]
    pub fn parse(label: &str) -> Option<Self> {
        match label {
            "failed" => Some(Self::Failed),
            "timeout" => Some(Self::Timeout),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    /// Deutsches Anzeigewort.
    #[must_use]
    pub fn label_de(self) -> &'static str {
        match self {
            Self::Failed => "gescheitert",
            Self::Timeout | Self::Cancelled => "abgebrochen",
        }
    }
}

/// Die Ursache eines nicht regulären Endes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildEndCause {
    /// Das Zeitbudget (`max_wall_time_ms`) lief ab.
    WallTime {
        /// Grenze in ms.
        limit_ms: u64,
        /// Verbrauch in ms.
        used_ms: u64,
    },
    /// Die Lease lief ab (kein Lebenszeichen innerhalb der Lease-Dauer).
    LeaseExpired,
    /// Das Werkzeug-Budget ist erschöpft.
    ToolBudget {
        /// Grenze.
        limit: u64,
        /// Verbrauch.
        used: u64,
    },
    /// Abgebrochen (Nutzerin, Elternteil, Herunterfahren).
    Cancelled {
        /// Grund in Kurzform.
        reason: String,
    },
    /// Freigegeben, während der Lauf noch lief.
    Released,
    /// Fehler des Turns (Provider, Modell, Werkzeugkette).
    TurnError(String),
    /// Terminales, nicht erfolgreiches Turn-Ergebnis (abgeschnitten,
    /// abgelehnt, gescheitert).
    Outcome(String),
    /// Der Turn wurde an einer Turn-Grenze oder von einem Turn-Wächter
    /// beendet (`CancelReason::Budget`); der Text nennt die konkrete Grenze
    /// mit Wert und Verbrauch bzw. den Wächter (siehe
    /// [`crate::turn_loop::TurnControl::stop_detail`]).
    TurnStopped(String),
    /// Das Kind pausierte, obwohl sein Lebenszyklus das verbietet.
    PauseForbidden(String),
}

/// Formatiert eine Dauer in ms knapp auf Deutsch („15 min", „90 s").
fn duration_de(ms: u64) -> String {
    let secs = ms / 1000;
    if secs >= 60 && secs % 60 == 0 {
        format!("{} min", secs / 60)
    } else if secs >= 60 {
        format!("{} min {} s", secs / 60, secs % 60)
    } else if secs > 0 {
        format!("{secs} s")
    } else {
        format!("{ms} ms")
    }
}

impl ChildEndCause {
    /// Der Endstatus dieser Ursache.
    #[must_use]
    pub fn status(&self) -> ChildEndStatus {
        match self {
            Self::WallTime { .. } | Self::LeaseExpired => ChildEndStatus::Timeout,
            Self::Cancelled { .. } | Self::Released => ChildEndStatus::Cancelled,
            Self::ToolBudget { .. }
            | Self::TurnError(_)
            | Self::Outcome(_)
            | Self::TurnStopped(_)
            | Self::PauseForbidden(_) => ChildEndStatus::Failed,
        }
    }

    /// Der Grund in lesbarer Kurzform.
    #[must_use]
    pub fn reason_de(&self) -> String {
        match self {
            Self::WallTime { limit_ms, .. } => {
                format!("Zeitbudget {} erreicht", duration_de(*limit_ms))
            }
            Self::LeaseExpired => {
                "Lease abgelaufen (kein Lebenszeichen innerhalb der Lease-Dauer)".to_owned()
            }
            Self::ToolBudget { limit, used } => {
                format!("Werkzeug-Budget erschöpft ({used} von {limit} Aufrufen)")
            }
            Self::Cancelled { reason } => format!("abgebrochen ({reason})"),
            Self::Released => "freigegeben, während der Lauf noch lief".to_owned(),
            Self::TurnError(message) => format!("Fehler: {}", excerpt(message, 240)),
            Self::Outcome(message) => excerpt(message, 240),
            Self::TurnStopped(detail) => {
                format!("Turn vorzeitig beendet: {}", excerpt(detail, 220))
            }
            Self::PauseForbidden(message) => {
                format!("unzulässige Pause: {}", excerpt(message, 200))
            }
        }
    }

    /// Ob eine Übergabe-Verdichtung versucht wird: bei Budget-Enden (Zeit,
    /// Werkzeuge) und — seit Runde 7, Teil A3 — auch nach einem Provider-/
    /// Turn-Fehler (ein Timeout oder 5xx ist oft vorübergehend; der kurze
    /// Aufruf mit eigenem Zeitlimit [`END_HANDOFF_TIMEOUT`] rettet die
    /// bisherige Arbeit, scheitert er, bleiben Journal und letzter
    /// Assistententext). Nicht bei Abbruch (die Nutzerin will stoppen) und
    /// nicht bei Lease-Ablauf (die Sitzung ist bereits verworfen).
    ///
    /// Ausnahme: ein Provider-Rate-Limit (HTTP 429, [`Self::is_rate_limited`])
    /// — die Verdichtung liefe gegen dasselbe Limit und verlängerte das Ende
    /// nur um ihr Zeitlimit (Export 429: 61 s + 20 s).
    ///
    /// Runde 9, Teil E2: auch ein Ende an einer Turn-Grenze oder durch einen
    /// Turn-Wächter ([`Self::TurnStopped`]) wird verdichtet, damit
    /// `continue_from` mit einer Übergabe statt von vorn beginnt (Export:
    /// `uia-latex-writer` dreimal ohne Verdichtung). Ausgenommen ist das
    /// Token-Budget ([`crate::turn_loop::TOKEN_BUDGET_STOP_PREFIX`]): dort
    /// verdichtet der Budget-Pfad aus seiner Reserve — sonst doppelt.
    #[must_use]
    pub fn allows_compaction(&self) -> bool {
        match self {
            Self::WallTime { .. } | Self::ToolBudget { .. } => true,
            Self::TurnError(_) => !self.is_rate_limited(),
            Self::TurnStopped(detail) => !detail
                .trim_start()
                .starts_with(crate::turn_loop::TOKEN_BUDGET_STOP_PREFIX),
            _ => false,
        }
    }

    /// Ob der Turn an einem Provider-Rate-Limit (HTTP 429) scheiterte.
    #[must_use]
    pub fn is_rate_limited(&self) -> bool {
        match self {
            Self::TurnError(message) => {
                message.contains("rate limited by provider") || message.contains("HTTP 429")
            }
            _ => false,
        }
    }

    /// Wie ein Vorgänger mit dieser Ursache endete — für den Auftragstext
    /// einer Fortsetzung ([`crate::child_handoff::continuation_task`]).
    #[must_use]
    pub fn predecessor_end(&self) -> crate::child_handoff::PredecessorEnd {
        use crate::child_handoff::PredecessorEnd;
        match self {
            Self::WallTime { .. } | Self::ToolBudget { .. } => PredecessorEnd::BudgetExhausted,
            Self::Cancelled { .. } | Self::Released => PredecessorEnd::Cancelled,
            Self::TurnStopped(detail) => PredecessorEnd::Stopped {
                reason: excerpt(detail, 220),
            },
            Self::LeaseExpired
            | Self::TurnError(_)
            | Self::Outcome(_)
            | Self::PauseForbidden(_) => PredecessorEnd::Failed {
                reason: self.reason_de(),
            },
        }
    }

    /// Ob der Elternteil mit `continue_from` fortsetzen darf (alles außer
    /// einem gewollten Abbruch).
    #[must_use]
    pub fn allows_continuation(&self) -> bool {
        !matches!(self, Self::Cancelled { .. } | Self::Released)
    }
}

/// Der Bericht eines nicht regulär beendeten Kindes an seinen Elternteil.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildEndReport {
    /// Das Kind.
    pub child: SessionId,
    /// Sein Elternteil.
    pub parent: SessionId,
    /// Seine Rolle.
    pub role: String,
    /// Endstatus.
    pub status: ChildEndStatus,
    /// Grund in Kurzform.
    pub reason: String,
    /// Die Übergabe (Verdichtung), falls möglich.
    pub handoff: Option<String>,
    /// Warum es keine Verdichtung gibt (falls keine).
    pub handoff_note: Option<String>,
    /// Journal-Kurzfassung (gedeckelt auf [`END_SUMMARY_MAX_BYTES`]).
    pub journal_summary: String,
    /// Zahl der Journal-Schritte.
    pub steps: u64,
    /// Geänderte Dateien (soweit erkennbar).
    pub files: Vec<String>,
    /// Ob `continue_from` möglich ist.
    pub continuation: bool,
}

impl ChildEndReport {
    /// Die maschinenlesbare erste Zeile.
    #[must_use]
    pub fn header(&self) -> String {
        format!(
            "{CHILD_END_MARKER} status={} handoff={}] {}",
            self.status.as_str(),
            if self.handoff.is_some() { "yes" } else { "no" },
            self.reason
        )
    }

    /// Die Übergabe für eine Fortsetzung (`continue_from`, Runde 9, E2).
    ///
    /// # Returns
    /// Kopfzeile mit dem konkreten Endgrund, die geänderten Dateien und dann
    /// die Verdichtung — ohne Verdichtung die Journal-Kurzfassung (sie nennt
    /// die Dateien selbst).
    #[must_use]
    pub fn continuation_text(&self) -> String {
        let header = self.header();
        match &self.handoff {
            Some(handoff) => {
                let files = if self.files.is_empty() {
                    "Geänderte Dateien: keine erkannt.".to_owned()
                } else {
                    format!(
                        "Geänderte Dateien ({}): {}",
                        self.files.len(),
                        self.files.join(", ")
                    )
                };
                format!("{header}\n{files}\n\n{handoff}")
            }
            None => format!("{header}\n\n{}", self.journal_summary),
        }
    }

    /// Der Text für das Modell des Elternteils.
    #[must_use]
    pub fn to_parent_text(&self) -> String {
        let child = self.child.as_str();
        let mut text = self.header();
        text.push_str(&format!(
            "\nKind-Agent {} ({child}) endete nicht regulär ({}). Die bisherige Arbeit ist \
             erhalten:\n- Schritte: {} — das vollständige Aktivitätsjournal liefert agent.result \
             {{\"child_id\": \"{child}\", \"part\": \"journal\"}}.\n",
            self.role,
            self.status.as_str(),
            self.steps
        ));
        if self.files.is_empty() {
            text.push_str("- Geänderte Dateien: keine erkannt.\n");
        } else {
            text.push_str(&format!(
                "- Geänderte Dateien ({}): {}\n",
                self.files.len(),
                self.files.join(", ")
            ));
        }
        match (&self.handoff, &self.handoff_note) {
            (Some(_), _) => text.push_str("- Übergabe: verdichtet, siehe unten.\n"),
            (None, Some(note)) => text.push_str(&format!(
                "- Übergabe: keine Verdichtung ({note}); das Journal ist die Übergabe.\n"
            )),
            (None, None) => {
                text.push_str("- Übergabe: keine Verdichtung; das Journal ist die Übergabe.\n");
            }
        }
        if self.continuation {
            text.push_str(&format!(
                "- Fortsetzen mit derselben Rolle: transfer_to_{} {{\"task\": …, \
                 \"continue_from\": \"{child}\"}}.\n",
                self.role
            ));
        }
        text.push_str(
            "Prüfe den Stand anhand von Journal und Dateien, statt neu anzufangen oder zu raten.\n",
        );
        text.push_str("\n--- Aktivitätsjournal (Kurzfassung) ---\n");
        text.push_str(&self.journal_summary);
        if let Some(handoff) = &self.handoff {
            text.push_str("\n--- Übergabe ---\n");
            text.push_str(handoff);
        }
        text
    }

    /// Strukturierte Form (`OpOutput::data`).
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "child_id": self.child.as_str(),
            "role": self.role,
            "status": self.status.as_str(),
            "reason": self.reason,
            "handoff_available": self.handoff.is_some(),
            "steps": self.steps,
            "files": self.files,
            "continuation": self.continuation,
        })
    }
}

/// Die geparste erste Zeile eines Endberichts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildEndHeader {
    /// Endstatus.
    pub status: ChildEndStatus,
    /// Grund.
    pub reason: String,
    /// Ob eine Übergabe vorliegt.
    pub handoff: bool,
}

/// Findet und parst die Kopfzeile eines Endberichts irgendwo in `text`.
///
/// # Returns
/// `None`, wenn `text` keinen (gültigen) Endbericht enthält.
#[must_use]
pub fn parse_child_end(text: &str) -> Option<ChildEndHeader> {
    let start = text.find(CHILD_END_MARKER)?;
    let line = text[start..].lines().next()?;
    let inner = line.strip_prefix(CHILD_END_MARKER)?;
    let (fields, reason) = inner.split_once(']')?;
    let mut status = None;
    let mut handoff = false;
    for field in fields.split_whitespace() {
        match field.split_once('=') {
            Some(("status", value)) => status = ChildEndStatus::parse(value),
            Some(("handoff", value)) => handoff = value == "yes",
            _ => {}
        }
    }
    Some(ChildEndHeader {
        status: status?,
        reason: reason.trim().to_owned(),
        handoff,
    })
}

/// Die Anzeige in der Werkzeugzelle des Elternteils, z. B.
/// „abgebrochen: Zeitbudget 15 min erreicht · Übergabe verfügbar".
#[must_use]
pub fn child_end_label(header: &ChildEndHeader) -> String {
    format!(
        "{}: {} · {}",
        header.status.label_de(),
        header.reason,
        if header.handoff {
            "Übergabe verfügbar"
        } else {
            "Journal verfügbar"
        }
    )
}

/// Das Aktivitätsjournal eines Kindes.
#[derive(Debug, Clone)]
pub struct ChildJournal {
    /// Das Kind.
    pub child: SessionId,
    /// Sein Elternteil.
    pub parent: SessionId,
    /// Seine Rolle.
    pub role: String,
    /// Der Auftrag (gekürzt).
    pub task: Option<String>,
    /// Startzeitpunkt.
    pub started_at: Timestamp,
    entries: VecDeque<JournalEntry>,
    bytes: usize,
    next_seq: u64,
    dropped: u64,
    files: BTreeSet<String>,
    files_dropped: u64,
    last_assistant: Option<String>,
    running: bool,
    end: Option<ChildEndReport>,
}

impl ChildJournal {
    /// Legt ein leeres, laufendes Journal an.
    #[must_use]
    pub fn new(child: SessionId, parent: SessionId, role: &str, task: Option<&str>) -> Self {
        Self {
            child,
            parent,
            role: role.to_owned(),
            task: task
                .map(str::trim)
                .filter(|task| !task.is_empty())
                .map(|task| {
                    let mut short: String = task.chars().take(TASK_MAX_CHARS).collect();
                    if task.chars().count() > TASK_MAX_CHARS {
                        short.push('…');
                    }
                    short
                }),
            started_at: Timestamp::now(),
            entries: VecDeque::new(),
            bytes: 0,
            next_seq: 0,
            dropped: 0,
            files: BTreeSet::new(),
            files_dropped: 0,
            last_assistant: None,
            running: true,
            end: None,
        }
    }

    /// Hängt einen Eintrag an und hält die Deckel ein.
    pub fn push(&mut self, kind: JournalEntryKind) {
        self.next_seq = self.next_seq.saturating_add(1);
        let entry = JournalEntry {
            seq: self.next_seq,
            at: Timestamp::now(),
            kind,
        };
        self.bytes = self.bytes.saturating_add(entry.weight());
        self.entries.push_back(entry);
        while self.entries.len() > JOURNAL_MAX_ENTRIES
            || (self.bytes > JOURNAL_MAX_BYTES && self.entries.len() > 1)
        {
            let Some(old) = self.entries.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(old.weight());
            self.dropped = self.dropped.saturating_add(1);
        }
    }

    /// Merkt eine geänderte Datei (gedeckelt auf [`JOURNAL_MAX_FILES`]).
    pub fn note_file(&mut self, path: &str) {
        let path = excerpt(path, ENTRY_DETAIL_MAX_CHARS);
        if path.is_empty() || self.files.contains(&path) {
            return;
        }
        if self.files.len() >= JOURNAL_MAX_FILES {
            self.files_dropped = self.files_dropped.saturating_add(1);
            return;
        }
        self.files.insert(path);
    }

    /// Verbucht einen abgeschlossenen Werkzeugaufruf.
    pub fn record_tool(&mut self, name: &str, args: &Value, ok: bool, output: &str) {
        if ok {
            match name {
                "fs.write" | "fs.edit" => {
                    if let Some(path) = args.get("path").and_then(Value::as_str) {
                        self.note_file(path);
                    }
                }
                "shell.exec" | "host.sudo_exec" => {
                    if let Some(command) = args.get("command").and_then(Value::as_str) {
                        for path in shell_written_files(command) {
                            self.note_file(&format!("{path} (shell)"));
                        }
                    }
                }
                _ => {}
            }
        }
        self.push(JournalEntryKind::Tool {
            name: name.to_owned(),
            args: short_args(name, args),
            ok,
            result: excerpt(output, ENTRY_DETAIL_MAX_CHARS / 2),
        });
    }

    /// Setzt den letzten Assistententext (gedeckelt).
    pub fn set_last_assistant(&mut self, text: &str) {
        let text = text.trim();
        if !text.is_empty() {
            self.last_assistant = Some(cap_bytes(text, LAST_TEXT_MAX_BYTES));
        }
    }

    /// Zahl aller bisherigen Schritte (auch verworfener).
    #[must_use]
    pub fn steps(&self) -> u64 {
        self.next_seq
    }

    /// Zahl der wegen des Deckels verworfenen ältesten Einträge.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Die vorhandenen Einträge, älteste zuerst.
    pub fn entries(&self) -> impl Iterator<Item = &JournalEntry> {
        self.entries.iter()
    }

    /// Die letzten `n` Einträge, älteste zuerst.
    #[must_use]
    pub fn last(&self, n: usize) -> Vec<JournalEntry> {
        let skip = self.entries.len().saturating_sub(n);
        self.entries.iter().skip(skip).cloned().collect()
    }

    /// Byte-Summe der Einträge (für den Deckel).
    #[must_use]
    pub fn entry_bytes(&self) -> usize {
        self.bytes
    }

    /// Geänderte Dateien.
    #[must_use]
    pub fn files(&self) -> Vec<String> {
        self.files.iter().cloned().collect()
    }

    /// Der letzte Assistententext.
    #[must_use]
    pub fn last_assistant(&self) -> Option<&str> {
        self.last_assistant.as_deref()
    }

    /// Ob das Kind noch läuft (admittiert ist).
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Der Endbericht, falls das Kind nicht regulär endete.
    #[must_use]
    pub fn end(&self) -> Option<&ChildEndReport> {
        self.end.as_ref()
    }

    /// Der aktuelle bzw. letzte Schritt.
    #[must_use]
    pub fn current_step(&self) -> Option<String> {
        self.entries.back().map(JournalEntry::line)
    }

    // `with_reason`: der Grund nur im vollständigen Journal; die Kurzfassung
    // steckt im Endbericht, dessen Kopfzeile den Grund schon trägt.
    fn status_line(&self, with_reason: bool) -> String {
        match (&self.end, self.running) {
            (Some(end), _) if with_reason => format!("{} ({})", end.status.as_str(), end.reason),
            (Some(end), _) => end.status.as_str().to_owned(),
            (None, true) => "läuft".to_owned(),
            (None, false) => "beendet".to_owned(),
        }
    }

    fn head_lines(&self, with_reason: bool) -> String {
        let mut text = format!(
            "Aktivitätsjournal von {} ({}) · Status: {} · {} Schritte",
            self.role,
            self.child,
            self.status_line(with_reason),
            self.steps()
        );
        if self.dropped > 0 {
            text.push_str(&format!(" (die ältesten {} verworfen)", self.dropped));
        }
        text.push('\n');
        if let Some(task) = &self.task {
            text.push_str(&format!("Auftrag: {}\n", excerpt(task, 400)));
        }
        if !self.files.is_empty() {
            text.push_str(&format!(
                "Geänderte Dateien ({}{}): {}\n",
                self.files.len(),
                if self.files_dropped > 0 { "+" } else { "" },
                self.files.iter().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
        text
    }

    /// Das ganze Journal als Text (für `agent.result {part: "journal"}`).
    #[must_use]
    pub fn render_full(&self) -> String {
        let mut text = self.head_lines(true);
        if let Some(task) = &self.task {
            text.push_str(&format!("\nVollständiger Auftrag:\n{task}\n"));
        }
        text.push_str("\nSchritte:\n");
        for entry in &self.entries {
            text.push_str(&entry.line());
            text.push('\n');
        }
        if let Some(last) = &self.last_assistant {
            text.push_str(&format!("\nLetzter Assistententext:\n{last}\n"));
        }
        // Der Endgrund steht bereits in der Kopfzeile (`Status: … (Grund)`);
        // kein zweiter `[child_end …]`-Block mit demselben Text.
        text
    }

    /// Kurzfassung mit den jüngsten Schritten, höchstens `max_bytes`.
    #[must_use]
    pub fn summary(&self, max_bytes: usize) -> String {
        let head = self.head_lines(false);
        let last = self
            .last_assistant
            .as_deref()
            .map(|text| format!("Letzter Assistententext: {}\n", excerpt(text, 600)))
            .unwrap_or_default();
        let mut budget = max_bytes.saturating_sub(head.len() + last.len());
        let mut lines: Vec<String> = Vec::new();
        for entry in self.entries.iter().rev() {
            let line = entry.line();
            if line.len() + 1 > budget {
                break;
            }
            budget -= line.len() + 1;
            lines.push(line);
        }
        lines.reverse();
        let omitted = self.entries.len() - lines.len();
        let mut text = head;
        if omitted > 0 {
            text.push_str(&format!(
                "… {omitted} ältere Schritte im vollständigen Journal\n"
            ));
        }
        for line in lines {
            text.push_str(&line);
            text.push('\n');
        }
        text.push_str(&last);
        cap_bytes(&text, max_bytes)
    }
}

// ── Nachrichten ──────────────────────────────────────────────────────────────

/// Eine Meldung eines Kindes an seinen Elternteil.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentMessage {
    /// Das meldende Kind.
    pub child: SessionId,
    /// Sein Elternteil (Empfänger).
    pub parent: SessionId,
    /// Rolle des Kindes.
    pub role: String,
    /// `info` oder `question`.
    pub kind: ParentMessageKind,
    /// Der Text (≤ [`MESSAGE_MAX_BYTES`]).
    pub text: String,
    /// Zeitpunkt.
    pub at: Timestamp,
}

impl ParentMessage {
    /// Der Text für das Modell des Elternteils (Wurzel).
    #[must_use]
    pub fn to_model_text(&self) -> String {
        match self.kind {
            ParentMessageKind::Info => format!(
                "[Nachricht von {} ({})] {}",
                self.role, self.child, self.text
            ),
            ParentMessageKind::Question => format!(
                "[Frage von {} ({})] {}\nDas Kind wartet auf eine Antwort: agent.message \
                 {{\"child_id\": \"{}\", \"text\": …}}. Deine nächste Nachricht an dieses Kind \
                 gilt als Antwort auf genau diese Frage — frage bei Bedarf zuerst die Nutzerin und \
                 gib ihre Antwort wörtlich weiter, keine anderen Hinweise vorher. Ohne Antwort \
                 arbeitet es nach {} min mit einer begründeten Annahme weiter.",
                self.role,
                self.child,
                self.text,
                self.child,
                PARENT_QUESTION_TIMEOUT.as_secs() / 60
            ),
        }
    }

    /// Die Anzeigezeile im Verlauf („root-orchestrator › …").
    #[must_use]
    pub fn display_line(&self) -> String {
        let prefix = match self.kind {
            ParentMessageKind::Info => "",
            ParentMessageKind::Question => "Frage: ",
        };
        format!("{} › {prefix}{}", self.role, excerpt(&self.text, 300))
    }
}

/// Wie eine Nachricht an ein Kind zugestellt wurde.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageDelivery {
    /// Beantwortet eine wartende Frage des Kindes (sofort).
    AnsweredQuestion,
    /// Im Postfach; das Kind liest sie an seiner nächsten Runden-Grenze.
    Queued,
}

impl MessageDelivery {
    /// Stabiles Label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AnsweredQuestion => "answered_question",
            Self::Queued => "queued",
        }
    }
}

/// Prüft und normalisiert einen Nachrichtentext.
///
/// # Errors
/// Meldung für das Modell bei leerem oder zu langem Text.
pub fn validate_message(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("`text` darf nicht leer sein".to_owned());
    }
    if text.len() > MESSAGE_MAX_BYTES {
        return Err(format!(
            "`text` ist {} Bytes lang; erlaubt sind höchstens {} Bytes (4 KiB)",
            text.len(),
            MESSAGE_MAX_BYTES
        ));
    }
    Ok(text.to_owned())
}

#[derive(Debug)]
struct PendingQuestion {
    id: u64,
    reply: oneshot::Sender<String>,
    /// Runde 9, E3: seit wann die Frage offen ist (die Wartezeit zählt nicht
    /// zum Zeitbudget des Kindes, [`ChildComms::question_wait`]).
    asked_at: Instant,
    /// Runde 9, E3: Auszug der Frage (für den Zustellhinweis an den
    /// Elternteil, [`ChildComms::pending_question_excerpt`]).
    excerpt: String,
}

#[derive(Debug, Default)]
struct CommsState {
    journals: BTreeMap<String, ChildJournal>,
    finished: VecDeque<String>,
    mailboxes: BTreeMap<String, VecDeque<String>>,
    questions: BTreeMap<String, PendingQuestion>,
    inbox: BTreeMap<String, VecDeque<ParentMessage>>,
    last_info: BTreeMap<String, Instant>,
    next_question: u64,
    /// Runde 9, E3: bereits abgeschlossene Wartezeit je Kind auf Antworten
    /// des Elternteils.
    question_waited: BTreeMap<String, Duration>,
}

impl CommsState {
    /// Entfernt die offene Frage von `child` und verbucht ihre Wartezeit.
    fn take_question(&mut self, child: &str) -> Option<PendingQuestion> {
        let question = self.questions.remove(child)?;
        let waited = question.asked_at.elapsed();
        let total = self.question_waited.entry(child.to_owned()).or_default();
        *total = total.saturating_add(waited);
        Some(question)
    }
}

/// Register für Journale, Endberichte und Nachrichten eines Spawners.
#[derive(Debug, Default)]
pub struct ChildComms {
    state: Mutex<CommsState>,
    inbox_wake: Notify,
}

impl ChildComms {
    fn state(&self) -> MutexGuard<'_, CommsState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    // ── Journal ──

    /// Legt das Journal eines frisch admittierten Kindes an und vermerkt
    /// den Start beim Elternteil, falls dieser selbst ein Journal hat.
    pub fn open_journal(
        &self,
        child: &SessionId,
        parent: &SessionId,
        role: &str,
        task: Option<&str>,
    ) {
        let mut state = self.state();
        state.finished.retain(|id| id != child.as_str());
        state.journals.insert(
            child.as_str().to_owned(),
            ChildJournal::new(child.clone(), parent.clone(), role, task),
        );
        if let Some(parent_journal) = state.journals.get_mut(parent.as_str()) {
            parent_journal.push(JournalEntryKind::Grandchild {
                child: child.as_str().to_owned(),
                role: role.to_owned(),
                outcome: None,
            });
        }
    }

    /// Verbucht einen Werkzeugaufruf im Journal von `child`.
    pub fn record_tool_outcome(
        &self,
        child: &SessionId,
        name: &str,
        args: &Value,
        ok: bool,
        output: &str,
    ) {
        if let Some(journal) = self.state().journals.get_mut(child.as_str()) {
            journal.record_tool(name, args, ok, output);
        }
    }

    /// Vermerkt das Ende eines Enkels im Journal seines Elternteils
    /// (`parent`); No-op ohne Journal (z. B. die Wurzel).
    pub fn record_grandchild_end(
        &self,
        parent: &SessionId,
        child: &SessionId,
        role: &str,
        outcome: &str,
    ) {
        if let Some(journal) = self.state().journals.get_mut(parent.as_str()) {
            journal.push(JournalEntryKind::Grandchild {
                child: child.as_str().to_owned(),
                role: role.to_owned(),
                outcome: Some(excerpt(outcome, ENTRY_DETAIL_MAX_CHARS)),
            });
        }
    }

    /// Setzt den letzten Assistententext von `child`.
    pub fn set_last_assistant(&self, child: &SessionId, text: &str) {
        if let Some(journal) = self.state().journals.get_mut(child.as_str()) {
            journal.set_last_assistant(text);
        }
    }

    /// Hinterlegt den Endbericht (überschreibt einen älteren) und vermerkt
    /// das Ende im Journal.
    ///
    /// # Returns
    /// Den Bericht; `None`, wenn es kein Journal gibt.
    pub fn finalize_end(
        &self,
        child: &SessionId,
        cause: &ChildEndCause,
        handoff: Option<String>,
        handoff_note: Option<String>,
    ) -> Option<ChildEndReport> {
        let mut state = self.state();
        let journal = state.journals.get_mut(child.as_str())?;
        // Nur der Status: der Grund steht in der Kopfzeile des Berichts bzw.
        // des vollständigen Journals (sonst erschiene er drei- bis viermal).
        journal.push(JournalEntryKind::Note(format!(
            "Ende: {}",
            cause.status().as_str()
        )));
        let mut report = ChildEndReport {
            child: journal.child.clone(),
            parent: journal.parent.clone(),
            role: journal.role.clone(),
            status: cause.status(),
            reason: cause.reason_de(),
            handoff,
            handoff_note,
            journal_summary: String::new(),
            steps: journal.steps(),
            files: journal.files(),
            continuation: false,
        };
        // Erst das Ende hinterlegen, dann die Kurzfassung bilden — sonst
        // meldet sie noch „Status: läuft" (Export 429).
        journal.end = Some(report.clone());
        report.journal_summary = journal.summary(END_SUMMARY_MAX_BYTES);
        journal.end = Some(report.clone());
        Some(report)
    }

    /// Markiert, dass `continue_from` für `child` möglich ist.
    pub fn mark_continuation(&self, child: &SessionId) -> Option<ChildEndReport> {
        let mut state = self.state();
        let end = state.journals.get_mut(child.as_str())?.end.as_mut()?;
        end.continuation = true;
        Some(end.clone())
    }

    /// Verwirft einen Endbericht (z. B. wenn das Token-Budget regulär mit
    /// einer Übergabe endete).
    pub fn clear_end(&self, child: &SessionId) {
        if let Some(journal) = self.state().journals.get_mut(child.as_str()) {
            journal.end = None;
        }
    }

    /// Der Endbericht von `child`, falls vorhanden.
    #[must_use]
    pub fn end_report(&self, child: &SessionId) -> Option<ChildEndReport> {
        self.state()
            .journals
            .get(child.as_str())
            .and_then(|journal| journal.end.clone())
    }

    /// Schließt das Journal eines freigegebenen Kindes: Postfach und
    /// wartende Frage fallen weg, das Journal bleibt für
    /// [`FINISHED_JOURNALS_KEEP`] beendete Kinder vorrätig.
    pub fn close_journal(&self, child: &SessionId) {
        let mut state = self.state();
        state.mailboxes.remove(child.as_str());
        state.questions.remove(child.as_str());
        state.question_waited.remove(child.as_str());
        state.last_info.remove(child.as_str());
        let Some(journal) = state.journals.get_mut(child.as_str()) else {
            return;
        };
        if !journal.running {
            return;
        }
        journal.running = false;
        state.finished.push_back(child.as_str().to_owned());
        while state.finished.len() > FINISHED_JOURNALS_KEEP {
            if let Some(evicted) = state.finished.pop_front() {
                state.journals.remove(&evicted);
            }
        }
    }

    /// Kopie des Journals von `child`.
    #[must_use]
    pub fn journal(&self, child: &SessionId) -> Option<ChildJournal> {
        self.state().journals.get(child.as_str()).cloned()
    }

    /// Kopie des Journals, nur wenn `caller` der Elternteil ist.
    #[must_use]
    pub fn journal_for(&self, caller: &SessionId, child: &str) -> Option<ChildJournal> {
        self.state()
            .journals
            .get(child)
            .filter(|journal| &journal.parent == caller)
            .cloned()
    }

    /// Alle Journale, deren Elternteil `caller` ist (laufende zuerst).
    #[must_use]
    pub fn journals_for(&self, caller: &SessionId) -> Vec<ChildJournal> {
        let mut journals: Vec<ChildJournal> = self
            .state()
            .journals
            .values()
            .filter(|journal| &journal.parent == caller)
            .cloned()
            .collect();
        journals.sort_by_key(|journal| (!journal.running, journal.started_at));
        journals
    }

    // ── Eltern → Kind ──

    /// Stellt eine (bereits geprüfte) Nachricht an `child` zu: beantwortet
    /// eine wartende Frage sofort, sonst ins Postfach.
    ///
    /// # Errors
    /// Meldung, wenn das Postfach voll ist.
    pub fn deliver_to_child(
        &self,
        child: &SessionId,
        from_role: &str,
        text: &str,
    ) -> Result<MessageDelivery, String> {
        let mut state = self.state();
        if let Some(journal) = state.journals.get_mut(child.as_str()) {
            journal.push(JournalEntryKind::MessageIn {
                from: from_role.to_owned(),
                text: excerpt(text, ENTRY_DETAIL_MAX_CHARS),
            });
        }
        if let Some(question) = state.take_question(child.as_str()) {
            match question.reply.send(text.to_owned()) {
                Ok(()) => return Ok(MessageDelivery::AnsweredQuestion),
                // Der Wartende ist schon weg (Zeitlimit): ins Postfach.
                Err(_) => tracing::debug!(child = %child, "child_comms.question_gone"),
            }
        }
        let mailbox = state
            .mailboxes
            .entry(child.as_str().to_owned())
            .or_default();
        if mailbox.len() >= MAILBOX_MAX_MESSAGES {
            return Err(format!(
                "das Postfach des Kindes ist voll ({MAILBOX_MAX_MESSAGES} ungelesene \
                 Nachrichten); warte, bis es sie an seiner nächsten Runden-Grenze gelesen hat"
            ));
        }
        mailbox.push_back(format!("[Nachricht von {from_role}] {text}"));
        Ok(MessageDelivery::Queued)
    }

    /// Plan R9, Teil F: legt eine Systemnotiz (z. B. ein Job-Ereignis) in
    /// das Postfach eines **laufenden** Kindes; sie erreicht das Modell an
    /// seiner nächsten Runden-Grenze wie eine Elternnachricht
    /// ([`Self::take_inbound`]).
    ///
    /// # Beschreibung
    /// Anders als [`Self::deliver_to_child`] beantwortet die Notiz nie eine
    /// wartende Frage und wird unverändert (ohne Absenderpräfix) abgelegt.
    /// Ist das Postfach voll, fällt die älteste Nachricht heraus.
    ///
    /// # Returns
    /// `true`, wenn das Kind läuft und die Notiz abgelegt wurde; `false` für
    /// ein beendetes oder unbekanntes Kind (der Aufrufer leitet dann an den
    /// Elternteil weiter).
    pub fn deliver_note_to_running_child(&self, child: &SessionId, text: &str) -> bool {
        let mut guard = self.state();
        let state = &mut *guard;
        let Some(journal) = state
            .journals
            .get_mut(child.as_str())
            .filter(|journal| journal.running)
        else {
            return false;
        };
        journal.push(JournalEntryKind::Note(excerpt(
            text,
            ENTRY_DETAIL_MAX_CHARS,
        )));
        let mailbox = state
            .mailboxes
            .entry(child.as_str().to_owned())
            .or_default();
        if mailbox.len() >= MAILBOX_MAX_MESSAGES {
            mailbox.pop_front();
        }
        mailbox.push_back(text.to_owned());
        true
    }

    /// Plan R9, Teil F: der direkte Elternteil von `child`, solange dessen
    /// Journal noch vorrätig ist (laufende und die zuletzt beendeten Kinder).
    #[must_use]
    pub fn parent_of(&self, child: &SessionId) -> Option<SessionId> {
        self.state()
            .journals
            .get(child.as_str())
            .map(|journal| journal.parent.clone())
    }

    /// Entnimmt die wartenden Nachrichten von `child` (Runden-Grenze).
    #[must_use]
    pub fn take_inbound(&self, child: &SessionId) -> Vec<String> {
        self.state()
            .mailboxes
            .remove(child.as_str())
            .map(Vec::from)
            .unwrap_or_default()
    }

    /// Zahl ungelesener Nachrichten im Postfach von `child`.
    #[must_use]
    pub fn pending_inbound(&self, child: &SessionId) -> usize {
        self.state()
            .mailboxes
            .get(child.as_str())
            .map_or(0, VecDeque::len)
    }

    /// Ob `child` gerade auf eine Antwort wartet.
    #[must_use]
    pub fn has_pending_question(&self, child: &SessionId) -> bool {
        self.state().questions.contains_key(child.as_str())
    }

    /// Runde 9, E3: Auszug der offenen Frage von `child`, falls eine wartet.
    #[must_use]
    pub fn pending_question_excerpt(&self, child: &SessionId) -> Option<String> {
        self.state()
            .questions
            .get(child.as_str())
            .map(|question| question.excerpt.clone())
    }

    /// Runde 9, E3: wie lange `child` insgesamt auf Antworten des
    /// Elternteils gewartet hat bzw. gerade wartet. Diese Zeit zählt nicht
    /// zum Zeitbudget des Kindes: ein fragendes Kind bleibt am Leben, bis
    /// die Frage beantwortet, abgebrochen oder abgelaufen ist.
    #[must_use]
    pub fn question_wait(&self, child: &SessionId) -> Duration {
        let state = self.state();
        let done = state
            .question_waited
            .get(child.as_str())
            .copied()
            .unwrap_or_default();
        let open = state
            .questions
            .get(child.as_str())
            .map(|question| question.asked_at.elapsed())
            .unwrap_or_default();
        done.saturating_add(open)
    }

    // ── Kind → Eltern ──

    /// Legt eine Meldung für den Elternteil ab: hat er ein Journal (ein
    /// laufendes Kind), landet sie in dessen Postfach, sonst im Eingang der
    /// Wurzel ([`Self::take_parent_messages`]).
    fn route_to_parent(state: &mut CommsState, message: ParentMessage) -> bool {
        if state
            .journals
            .get(message.parent.as_str())
            .is_some_and(|journal| journal.running)
        {
            let mailbox = state
                .mailboxes
                .entry(message.parent.as_str().to_owned())
                .or_default();
            if mailbox.len() >= MAILBOX_MAX_MESSAGES {
                mailbox.pop_front();
            }
            mailbox.push_back(message.to_model_text());
            return false;
        }
        let inbox = state
            .inbox
            .entry(message.parent.as_str().to_owned())
            .or_default();
        if inbox.len() >= PARENT_INBOX_MAX {
            inbox.pop_front();
        }
        inbox.push_back(message);
        true
    }

    /// `parent.message {kind: "info"}`: Rate-Limit je Kind
    /// ([`PARENT_INFO_MIN_INTERVAL`]).
    ///
    /// # Errors
    /// Meldung für das Modell, wenn das Rate-Limit greift.
    pub fn post_info(
        &self,
        child: &SessionId,
        parent: &SessionId,
        role: &str,
        text: &str,
        now: Instant,
    ) -> Result<(), String> {
        let mut state = self.state();
        if let Some(last) = state.last_info.get(child.as_str()) {
            let since = now.saturating_duration_since(*last);
            if since < PARENT_INFO_MIN_INTERVAL {
                return Err(format!(
                    "Rate-Limit: höchstens eine Info-Nachricht je {} s — noch {} s warten; \
                     fasse Zwischenstände zusammen",
                    PARENT_INFO_MIN_INTERVAL.as_secs(),
                    (PARENT_INFO_MIN_INTERVAL - since).as_secs().max(1)
                ));
            }
        }
        state.last_info.insert(child.as_str().to_owned(), now);
        if let Some(journal) = state.journals.get_mut(child.as_str()) {
            journal.push(JournalEntryKind::MessageOut {
                kind: ParentMessageKind::Info,
                text: excerpt(text, ENTRY_DETAIL_MAX_CHARS),
            });
        }
        let to_root = Self::route_to_parent(
            &mut state,
            ParentMessage {
                child: child.clone(),
                parent: parent.clone(),
                role: role.to_owned(),
                kind: ParentMessageKind::Info,
                text: text.to_owned(),
                at: Timestamp::now(),
            },
        );
        drop(state);
        if to_root {
            self.inbox_wake.notify_waiters();
        }
        Ok(())
    }

    /// `parent.message {kind: "question"}`: legt die Frage ab und liefert
    /// den Empfänger der Antwort.
    ///
    /// # Errors
    /// Meldung für das Modell, wenn schon eine Frage offen ist.
    pub fn ask_parent(
        &self,
        child: &SessionId,
        parent: &SessionId,
        role: &str,
        text: &str,
    ) -> Result<(u64, oneshot::Receiver<String>), String> {
        let mut state = self.state();
        if state.questions.contains_key(child.as_str()) {
            return Err("es ist bereits eine Frage an den Elternteil offen".to_owned());
        }
        state.next_question = state.next_question.saturating_add(1);
        let id = state.next_question;
        let (reply, receiver) = oneshot::channel();
        state.questions.insert(
            child.as_str().to_owned(),
            PendingQuestion {
                id,
                reply,
                asked_at: Instant::now(),
                excerpt: excerpt(text, 160),
            },
        );
        if let Some(journal) = state.journals.get_mut(child.as_str()) {
            journal.push(JournalEntryKind::MessageOut {
                kind: ParentMessageKind::Question,
                text: excerpt(text, ENTRY_DETAIL_MAX_CHARS),
            });
        }
        let to_root = Self::route_to_parent(
            &mut state,
            ParentMessage {
                child: child.clone(),
                parent: parent.clone(),
                role: role.to_owned(),
                kind: ParentMessageKind::Question,
                text: text.to_owned(),
                at: Timestamp::now(),
            },
        );
        drop(state);
        if to_root {
            self.inbox_wake.notify_waiters();
        }
        Ok((id, receiver))
    }

    /// Entfernt die offene Frage `id` von `child` (Zeitablauf).
    pub fn drop_question(&self, child: &SessionId, id: u64) {
        let mut state = self.state();
        if state
            .questions
            .get(child.as_str())
            .is_some_and(|question| question.id == id)
        {
            let _ = state.take_question(child.as_str());
            if let Some(journal) = state.journals.get_mut(child.as_str()) {
                journal.push(JournalEntryKind::Note(
                    "Frage an den Elternteil ohne Antwort (Zeitlimit)".to_owned(),
                ));
            }
        }
    }

    /// Nimmt alle ungelesenen Kind-Meldungen an `parent` (Wurzel).
    #[must_use]
    pub fn take_parent_messages(&self, parent: &SessionId) -> Vec<ParentMessage> {
        self.state()
            .inbox
            .remove(parent.as_str())
            .map(Vec::from)
            .unwrap_or_default()
    }

    /// Ob für `parent` Kind-Meldungen warten.
    #[must_use]
    pub fn has_parent_messages(&self, parent: &SessionId) -> bool {
        self.state()
            .inbox
            .get(parent.as_str())
            .is_some_and(|inbox| !inbox.is_empty())
    }

    /// Wartet, bis eine neue Meldung an eine Wurzel abgelegt wurde.
    pub async fn parent_messages_notified(&self) {
        self.inbox_wake.notified().await;
    }
}

// ── Anbindung an den Spawner ──────────────────────────────────────────────────

/// Immer dieselbe Ablehnung für fremde, unbekannte und beendete Kinder.
fn not_own_running_child(child: &str) -> AgentSpawnError {
    AgentSpawnError {
        kind: Default::default(),
        message: format!("kein eigenes, laufendes Kind mit der ID {child}"),
    }
}

impl ManagedAgentSpawner {
    /// Das Register für Journale, Endberichte und Nachrichten (Teil M).
    #[must_use]
    pub fn child_comms(&self) -> &Arc<ChildComms> {
        &self.comms
    }

    /// Der Endbericht eines nicht regulär beendeten Kindes.
    #[must_use]
    pub fn child_end_report(&self, child: &SessionId) -> Option<ChildEndReport> {
        self.comms.end_report(child)
    }

    /// Das Journal eines eigenen (laufenden oder beendeten) Kindes.
    #[must_use]
    pub fn child_journal_for(&self, caller: &SessionId, child: &str) -> Option<ChildJournal> {
        self.comms.journal_for(caller, child)
    }

    /// Die Rolle einer Sitzung für Nachrichten-Absender: ihr Rollenname,
    /// falls sie ein admittiertes Kind ist, sonst `uia` (die Wurzel).
    fn message_sender_role(&self, session: &SessionId) -> String {
        self.child_record(session)
            .map_or_else(|| "uia".to_owned(), |record| record.role)
    }

    /// `agent.message`: schickt `text` an ein eigenes, laufendes Kind.
    ///
    /// # Beschreibung
    /// Wartet das Kind auf eine Antwort (`parent.message {kind:
    /// "question"}`), beantwortet der Text die Frage sofort; sonst liest das
    /// Kind ihn an seiner nächsten Runden-Grenze als
    /// „[Nachricht von <rolle>] …". Verleiht keine Rechte.
    ///
    /// # Errors
    /// [`AgentSpawnError`] mit immer derselben Meldung für fremde,
    /// unbekannte oder beendete Kinder; sonst bei leerem/zu langem Text oder
    /// vollem Postfach.
    pub fn send_message_to_child(
        &self,
        caller: &SessionId,
        child_id: &str,
        text: &str,
    ) -> Result<MessageDelivery, AgentSpawnError> {
        let child = SessionId::try_from_str(child_id.to_owned())
            .map_err(|_| not_own_running_child(child_id))?;
        let record = self
            .child_record(&child)
            .filter(|record| &record.parent == caller)
            .ok_or_else(|| not_own_running_child(child_id))?;
        if record.status.is_terminal() {
            return Err(not_own_running_child(child_id));
        }
        let text = validate_message(text).map_err(AgentSpawnError::new)?;
        let from = self.message_sender_role(caller);
        let delivery = self
            .comms
            .deliver_to_child(&child, &from, &text)
            .map_err(AgentSpawnError::new)?;
        tracing::info!(
            child = %child,
            parent = %caller,
            delivery = delivery.as_str(),
            bytes = text.len(),
            "child_comms.message_to_child"
        );
        Ok(delivery)
    }

    /// `parent.message {kind: "info"}` von `child` an seinen direkten
    /// Elternteil.
    ///
    /// # Errors
    /// [`AgentSpawnError`], wenn `child` kein admittiertes Kind ist, bei
    /// leerem/zu langem Text oder wenn das Rate-Limit greift.
    pub fn post_info_to_parent(
        &self,
        child: &SessionId,
        text: &str,
    ) -> Result<SessionId, AgentSpawnError> {
        let record = self.child_record(child).ok_or_else(|| AgentSpawnError {
            kind: Default::default(),
            message: "parent.message: diese Sitzung hat keinen Elternteil".to_owned(),
        })?;
        let text = validate_message(text).map_err(AgentSpawnError::new)?;
        self.comms
            .post_info(child, &record.parent, &record.role, &text, Instant::now())
            .map_err(AgentSpawnError::new)?;
        tracing::info!(child = %child, parent = %record.parent, "child_comms.info_to_parent");
        Ok(record.parent)
    }

    /// `parent.message {kind: "question"}`: fragt den direkten Elternteil
    /// und wartet höchstens `timeout` auf `agent.message`.
    ///
    /// # Returns
    /// Die Antwort, oder [`NO_ANSWER_REPLY`] nach Zeitablauf bzw. wenn der
    /// Elternteil nicht mehr antworten kann.
    ///
    /// # Errors
    /// [`AgentSpawnError`], wenn `child` kein admittiertes Kind ist, bei
    /// leerem/zu langem Text oder wenn schon eine Frage offen ist.
    ///
    /// # Concurrency
    /// Hält keinen Lock über das Warten.
    pub async fn ask_parent(
        &self,
        child: &SessionId,
        text: &str,
        timeout: Duration,
    ) -> Result<(String, bool), AgentSpawnError> {
        let record = self.child_record(child).ok_or_else(|| AgentSpawnError {
            kind: Default::default(),
            message: "parent.message: diese Sitzung hat keinen Elternteil".to_owned(),
        })?;
        let text = validate_message(text).map_err(AgentSpawnError::new)?;
        let (id, receiver) = self
            .comms
            .ask_parent(child, &record.parent, &record.role, &text)
            .map_err(AgentSpawnError::new)?;
        tracing::info!(child = %child, parent = %record.parent, "child_comms.question_to_parent");
        let cancel = self.child_cancel_token(child);
        let wait = tokio::time::timeout(timeout, receiver);
        let answer = match cancel {
            Some(token) => tokio::select! {
                biased;
                () = token.cancelled() => None,
                result = wait => result.ok().and_then(Result::ok),
            },
            None => wait.await.ok().and_then(Result::ok),
        };
        match answer {
            Some(answer) => Ok((answer, true)),
            None => {
                self.comms.drop_question(child, id);
                Ok((NO_ANSWER_REPLY.to_owned(), false))
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn ids() -> (SessionId, SessionId) {
        (SessionId::new(), SessionId::new())
    }

    #[test]
    fn journal_is_capped_by_entries_and_bytes() {
        let (child, parent) = ids();
        let mut journal = ChildJournal::new(child, parent, "root-orchestrator", Some("Baue X"));
        for index in 0..(JOURNAL_MAX_ENTRIES * 3) {
            journal.record_tool(
                "fs.read",
                &json!({ "path": format!("src/datei_{index}.rs") }),
                true,
                &"x".repeat(10_000),
            );
        }
        assert_eq!(journal.steps(), (JOURNAL_MAX_ENTRIES * 3) as u64);
        assert!(journal.entries().count() <= JOURNAL_MAX_ENTRIES);
        assert!(journal.entry_bytes() <= JOURNAL_MAX_BYTES);
        assert!(journal.dropped() >= (JOURNAL_MAX_ENTRIES * 2) as u64);
        // Die jüngsten Schritte bleiben.
        let last = journal.last(1);
        assert!(
            last[0]
                .line()
                .contains(&format!("datei_{}", JOURNAL_MAX_ENTRIES * 3 - 1))
        );
        // Auch sehr lange Einzelergebnisse sprengen den Byte-Deckel nicht.
        assert!(journal.render_full().len() < JOURNAL_MAX_BYTES + 16 * 1024);
    }

    #[test]
    fn journal_notes_written_files_from_fs_and_shell() {
        let (child, parent) = ids();
        let mut journal = ChildJournal::new(child, parent, "executor", None);
        journal.record_tool("fs.write", &json!({ "path": "a.rs" }), true, "ok");
        journal.record_tool("fs.edit", &json!({ "path": "b.rs" }), true, "ok");
        journal.record_tool("fs.edit", &json!({ "path": "failed.rs" }), false, "nope");
        journal.record_tool(
            "shell.exec",
            &json!({ "command": "echo hi > out.txt && sed -i 's/a/b/' c.rs" }),
            true,
            "",
        );
        let files = journal.files();
        assert!(files.contains(&"a.rs".to_owned()));
        assert!(files.contains(&"b.rs".to_owned()));
        assert!(files.contains(&"out.txt (shell)".to_owned()));
        assert!(files.contains(&"c.rs (shell)".to_owned()));
        assert!(!files.iter().any(|file| file.contains("failed.rs")));
    }

    #[test]
    fn shell_detection_ignores_dev_null_and_reads() {
        assert_eq!(
            shell_written_files("cat a.rs 2>/dev/null"),
            Vec::<String>::new()
        );
        assert_eq!(shell_written_files("git status"), Vec::<String>::new());
        assert_eq!(shell_written_files("cp a b/c"), vec!["b/c".to_owned()]);
        assert_eq!(
            shell_written_files("ls | tee log.txt"),
            vec!["log.txt".to_owned()]
        );
    }

    #[test]
    fn end_report_header_round_trips_and_labels_the_real_reason() -> TestResult {
        let comms = ChildComms::default();
        let (child, parent) = ids();
        comms.open_journal(&child, &parent, "root-orchestrator", Some("Umsetzen"));
        comms.record_tool_outcome(&child, "fs.write", &json!({ "path": "x.rs" }), true, "ok");
        let cause = ChildEndCause::WallTime {
            limit_ms: 900_000,
            used_ms: 1_224_814,
        };
        let report = comms
            .finalize_end(&child, &cause, Some("Übergabe".to_owned()), None)
            .ok_or(TestError::Missing("Endbericht"))?;
        let text = report.to_parent_text();
        let header = parse_child_end(&text).ok_or(TestError::Missing("Kopfzeile"))?;
        assert_eq!(header.status, ChildEndStatus::Timeout);
        assert!(header.handoff);
        assert_eq!(
            child_end_label(&header),
            "abgebrochen: Zeitbudget 15 min erreicht · Übergabe verfügbar"
        );
        assert!(text.contains("x.rs"));
        assert!(text.contains("\"part\": \"journal\""));
        // Ohne Übergabe nennt das Label das Journal.
        let failed = parse_child_end("vorher\n[child_end status=failed handoff=no] Fehler: 529")
            .ok_or(TestError::Missing("Kopfzeile ohne Übergabe"))?;
        assert_eq!(
            child_end_label(&failed),
            "gescheitert: Fehler: 529 · Journal verfügbar"
        );
        assert!(parse_child_end("kein Bericht").is_none());
        Ok(())
    }

    /// Export 429: die Kurzfassung im Endbericht meldete „Status: läuft",
    /// und der Fehlertext stand drei- bis viermal im Bericht.
    #[test]
    fn end_report_summary_shows_final_status_and_names_the_error_once() -> TestResult {
        let comms = ChildComms::default();
        let (child, parent) = ids();
        comms.open_journal(&child, &parent, "matrix-game-master", Some("Spiel"));
        comms.record_tool_outcome(
            &child,
            "fs.read",
            &json!({ "path": "README.md" }),
            true,
            "ok",
        );
        let error = "rate limited by provider — retry after 30s: provider returned HTTP 429 \
                     (request_id: req_1): rate_limit_error: Error";
        let cause = ChildEndCause::TurnError(error.to_owned());
        let report = comms
            .finalize_end(&child, &cause, None, Some("Provider-Rate-Limit".to_owned()))
            .ok_or(TestError::Missing("Endbericht"))?;
        assert!(
            report.journal_summary.contains("Status: failed ·"),
            "{}",
            report.journal_summary
        );
        assert!(!report.journal_summary.contains("läuft"));
        let text = report.to_parent_text();
        assert_eq!(
            text.matches("rate limited by provider").count(),
            1,
            "{text}"
        );
        // Das vollständige Journal nennt den Grund ebenfalls genau einmal.
        let journal = comms.journal(&child).ok_or(TestError::Missing("Journal"))?;
        let full = journal.render_full();
        assert!(
            full.contains("Status: failed (Fehler: rate limited"),
            "{full}"
        );
        assert_eq!(
            full.matches("rate limited by provider").count(),
            1,
            "{full}"
        );
        Ok(())
    }

    /// Statt „Token-Budget oder Turn-Wächter" nennt der Grund die konkrete
    /// Grenze bzw. den Wächter.
    #[test]
    fn turn_stopped_names_the_concrete_limit() {
        let cause = ChildEndCause::TurnStopped(
            "Rundenlimit des Turns erreicht (40/40 Modellrunden)".into(),
        );
        assert_eq!(cause.status(), ChildEndStatus::Failed);
        assert_eq!(
            cause.reason_de(),
            "Turn vorzeitig beendet: Rundenlimit des Turns erreicht (40/40 Modellrunden)"
        );
        // Runde 9, E2: Turn-Grenzen und Wächter werden verdichtet …
        assert!(cause.allows_compaction());
        assert!(cause.allows_continuation());
        assert!(
            ChildEndCause::TurnStopped("Turn-Wächter (no_progress_rounds): 4 Runden".into())
                .allows_compaction()
        );
        // … das Token-Budget nicht (das verdichtet der Budget-Pfad selbst).
        let budget = ChildEndCause::TurnStopped(format!(
            "{} (150 von 100 neuen Tokens; Cache-Lesungen zählen nicht)",
            crate::turn_loop::TOKEN_BUDGET_STOP_PREFIX
        ));
        assert!(!budget.allows_compaction());
        assert!(budget.reason_de().contains("150 von 100"));
        assert!(matches!(
            cause.predecessor_end(),
            crate::child_handoff::PredecessorEnd::Stopped { .. }
        ));
    }

    /// Runde 9, E2: die Übergabe an eine Fortsetzung nennt Endgrund und
    /// geänderte Dateien, auch wenn eine Verdichtung vorliegt.
    #[test]
    fn continuation_text_carries_reason_files_and_handoff() {
        let cause = ChildEndCause::TurnStopped("Turn-Wächter (no_progress_rounds): 4".into());
        let mut report = ChildEndReport {
            child: SessionId::new(),
            parent: SessionId::new(),
            role: "uia-latex-writer".to_owned(),
            status: cause.status(),
            reason: cause.reason_de(),
            handoff: Some("## Auftrag\nLayout".to_owned()),
            handoff_note: None,
            journal_summary: "Journal …".to_owned(),
            steps: 19,
            files: vec!["paper.tex".to_owned()],
            continuation: true,
        };
        let text = report.continuation_text();
        assert!(text.contains("Turn-Wächter (no_progress_rounds)"), "{text}");
        assert!(text.contains("Geänderte Dateien (1): paper.tex"), "{text}");
        assert!(text.ends_with("## Auftrag\nLayout"), "{text}");
        report.handoff = None;
        let text = report.continuation_text();
        assert!(text.ends_with("Journal …"), "{text}");
        assert!(text.contains("handoff=no"), "{text}");
    }

    #[test]
    fn rate_limited_turn_errors_skip_the_compaction() {
        let cause = ChildEndCause::TurnError(
            "rate limited by provider — retry after 30s: provider returned HTTP 429".to_owned(),
        );
        assert!(cause.is_rate_limited());
        assert!(!cause.allows_compaction());
        assert!(cause.allows_continuation());
        let openai = ChildEndCause::TurnError(
            "transient provider error: provider returned HTTP 429: requests: slow down".to_owned(),
        );
        assert!(openai.is_rate_limited());
        assert!(!ChildEndCause::TurnError("provider 529".to_owned()).is_rate_limited());
    }

    #[test]
    fn budget_ends_and_turn_errors_are_compacted() {
        assert!(
            ChildEndCause::WallTime {
                limit_ms: 1,
                used_ms: 2
            }
            .allows_compaction()
        );
        assert!(ChildEndCause::ToolBudget { limit: 1, used: 2 }.allows_compaction());
        // Runde 7, Teil A3: auch nach einem Providerfehler.
        assert!(ChildEndCause::TurnError("provider 529".to_owned()).allows_compaction());
        assert!(
            !ChildEndCause::Cancelled {
                reason: "User".to_owned()
            }
            .allows_compaction()
        );
        assert!(!ChildEndCause::LeaseExpired.allows_compaction());
        assert_eq!(
            ChildEndCause::LeaseExpired.status(),
            ChildEndStatus::Timeout
        );
    }

    #[test]
    fn message_length_is_capped() {
        assert!(validate_message("  ").is_err());
        assert!(validate_message(&"x".repeat(MESSAGE_MAX_BYTES + 1)).is_err());
        assert!(validate_message(&"x".repeat(MESSAGE_MAX_BYTES)).is_ok());
    }

    #[test]
    fn inbound_messages_are_queued_bounded_and_taken_once() {
        let comms = ChildComms::default();
        let (child, parent) = ids();
        comms.open_journal(&child, &parent, "root-orchestrator", None);
        for index in 0..MAILBOX_MAX_MESSAGES {
            assert_eq!(
                comms.deliver_to_child(&child, "uia", &format!("Hinweis {index}")),
                Ok(MessageDelivery::Queued)
            );
        }
        assert!(comms.deliver_to_child(&child, "uia", "zu viel").is_err());
        let taken = comms.take_inbound(&child);
        assert_eq!(taken.len(), MAILBOX_MAX_MESSAGES);
        assert_eq!(taken[0], "[Nachricht von uia] Hinweis 0");
        assert!(comms.take_inbound(&child).is_empty());
    }

    /// Plan R9, Teil F: Systemnotizen (Job-Ereignisse) erreichen nur
    /// laufende Kinder, ohne Präfix und ohne eine offene Frage zu
    /// beantworten; nach dem Ende meldet die Zustellung `false`.
    #[test]
    fn system_notes_reach_only_running_children() {
        let comms = ChildComms::default();
        let (child, parent) = ids();
        assert!(!comms.deliver_note_to_running_child(&child, "[job x] started"));
        comms.open_journal(&child, &parent, "executor", None);
        assert_eq!(comms.parent_of(&child), Some(parent.clone()));
        // Der Empfänger der Antwort bleibt bis zum Testende am Leben.
        let asked = comms.ask_parent(&child, &parent, "executor", "Frage?");
        assert!(asked.is_ok(), "Frage an den Elternteil");
        assert!(comms.deliver_note_to_running_child(&child, "[job x] finished"));
        assert!(
            comms.has_pending_question(&child),
            "Notiz beantwortet keine Frage"
        );
        assert_eq!(
            comms.take_inbound(&child),
            vec!["[job x] finished".to_owned()]
        );
        comms.close_journal(&child);
        assert!(!comms.deliver_note_to_running_child(&child, "[job x] late"));
        assert!(comms.take_inbound(&child).is_empty());
    }

    #[test]
    fn info_is_rate_limited_and_reaches_the_root_inbox() {
        let comms = ChildComms::default();
        let (child, root) = ids();
        comms.open_journal(&child, &root, "root-orchestrator", None);
        let now = Instant::now();
        assert!(
            comms
                .post_info(&child, &root, "root-orchestrator", "Zwischenstand", now)
                .is_ok()
        );
        let second = comms.post_info(
            &child,
            &root,
            "root-orchestrator",
            "gleich noch einer",
            now + Duration::from_secs(5),
        );
        assert!(second.is_err_and(|message| message.contains("Rate-Limit")));
        assert!(
            comms
                .post_info(
                    &child,
                    &root,
                    "root-orchestrator",
                    "später",
                    now + PARENT_INFO_MIN_INTERVAL
                )
                .is_ok()
        );
        let messages = comms.take_parent_messages(&root);
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[0].display_line(),
            "root-orchestrator › Zwischenstand"
        );
        assert!(
            messages[0]
                .to_model_text()
                .starts_with("[Nachricht von root-orchestrator")
        );
    }

    #[test]
    fn info_to_a_running_orchestrator_parent_lands_in_its_mailbox() {
        let comms = ChildComms::default();
        let (orchestrator, root) = ids();
        let worker = SessionId::new();
        comms.open_journal(&orchestrator, &root, "coding-orchestrator", None);
        comms.open_journal(&worker, &orchestrator, "executor", None);
        assert!(
            comms
                .post_info(
                    &worker,
                    &orchestrator,
                    "executor",
                    "Tests grün",
                    Instant::now()
                )
                .is_ok()
        );
        assert!(comms.take_parent_messages(&root).is_empty());
        let inbound = comms.take_inbound(&orchestrator);
        assert_eq!(inbound.len(), 1);
        assert!(inbound[0].contains("Tests grün"));
        // Der Start des Enkels steht im Journal des Orchestrators.
        let journal = comms.journal(&orchestrator);
        assert!(journal.is_some_and(|journal| {
            journal
                .entries()
                .any(|entry| entry.line().contains("startet executor"))
        }));
    }

    #[tokio::test]
    async fn a_question_is_answered_by_the_next_message() -> TestResult {
        let comms = ChildComms::default();
        let (child, root) = ids();
        comms.open_journal(&child, &root, "root-orchestrator", None);
        let (_id, receiver) = comms
            .ask_parent(&child, &root, "root-orchestrator", "Tabelle A oder B?")
            .map_err(TestError::Unexpected)?;
        assert!(comms.has_pending_question(&child));
        assert!(
            comms
                .ask_parent(&child, &root, "root-orchestrator", "noch eine")
                .is_err()
        );
        assert_eq!(
            comms.deliver_to_child(&child, "uia", "B"),
            Ok(MessageDelivery::AnsweredQuestion)
        );
        let answer = receiver.await.map_err(|_| TestError::Missing("Antwort"))?;
        assert_eq!(answer, "B");
        assert!(
            comms.take_inbound(&child).is_empty(),
            "Antwort geht nicht ins Postfach"
        );
        Ok(())
    }

    /// Runde 9, E3: die Wartezeit auf eine Antwort wird gezählt (sie ruht
    /// im Zeitbudget des Kindes), der Auszug der Frage ist abrufbar, und die
    /// Frage an die Wurzel sagt, dass die nächste Nachricht die Antwort ist.
    #[tokio::test]
    async fn a_pending_question_counts_its_wait_and_names_the_routing() -> TestResult {
        let comms = ChildComms::default();
        let (child, root) = ids();
        comms.open_journal(&child, &root, "matrix-game-master", None);
        assert_eq!(comms.question_wait(&child), Duration::ZERO);
        assert_eq!(comms.pending_question_excerpt(&child), None);
        let (_id, receiver) = comms
            .ask_parent(&child, &root, "matrix-game-master", "Szenario freigeben?")
            .map_err(TestError::Unexpected)?;
        assert_eq!(
            comms.pending_question_excerpt(&child).as_deref(),
            Some("Szenario freigeben?")
        );
        let message = comms
            .take_parent_messages(&root)
            .pop()
            .ok_or(TestError::Missing("Frage im Eingang der Wurzel"))?;
        let text = message.to_model_text();
        assert!(
            text.contains("gilt als Antwort auf genau diese Frage"),
            "{text}"
        );
        assert!(text.contains("wörtlich"), "{text}");
        tokio::time::sleep(Duration::from_millis(20)).await;
        let open = comms.question_wait(&child);
        assert!(open >= Duration::from_millis(20), "{open:?}");
        assert_eq!(
            comms.deliver_to_child(&child, "uia", "freigegeben"),
            Ok(MessageDelivery::AnsweredQuestion)
        );
        assert_eq!(
            receiver.await.map_err(|_| TestError::Missing("Antwort"))?,
            "freigegeben"
        );
        // Beantwortet: die Wartezeit bleibt verbucht, wächst aber nicht mehr.
        let done = comms.question_wait(&child);
        assert!(done >= open, "{done:?} < {open:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(comms.question_wait(&child), done);
        assert!(!comms.has_pending_question(&child));
        Ok(())
    }

    #[test]
    fn closing_keeps_a_bounded_number_of_finished_journals() {
        let comms = ChildComms::default();
        let parent = SessionId::new();
        let mut children = Vec::new();
        for _ in 0..(FINISHED_JOURNALS_KEEP + 3) {
            let child = SessionId::new();
            comms.open_journal(&child, &parent, "explorer", None);
            let _ = comms.deliver_to_child(&child, "uia", "x");
            comms.close_journal(&child);
            assert!(
                comms.take_inbound(&child).is_empty(),
                "Postfach fällt beim Ende weg"
            );
            children.push(child);
        }
        assert!(comms.journal(&children[0]).is_none());
        assert!(
            comms
                .journal(&children[children.len() - 1])
                .is_some_and(|journal| !journal.is_running())
        );
        assert_eq!(comms.journals_for(&parent).len(), FINISHED_JOURNALS_KEEP);
    }

    #[test]
    fn a_foreign_caller_never_sees_a_journal() {
        let comms = ChildComms::default();
        let (child, parent) = ids();
        comms.open_journal(&child, &parent, "explorer", None);
        assert!(
            comms
                .journal_for(&SessionId::new(), child.as_str())
                .is_none()
        );
        assert!(comms.journal_for(&parent, child.as_str()).is_some());
    }
}
