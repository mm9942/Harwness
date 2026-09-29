//! Freigabe-Panel: hervorgehobener Dialog anstelle des Composers.
//!
//! Spec-Quelle: `docs/design/tui-command-contract.md` (Freigabe als Dialog).
//!
//! # Verantwortung
//! Dieses Modul besitzt ausschließlich die Darstellung und Tastaturlogik des
//! Freigabe-Panels — es kennt weder den Freigabe-Kanal (`crate::approval`)
//! noch das Arming-Delay selbst (`classify_armed_approval_key` in `app.rs`).
//! Der Aufrufer (spätere Verdrahtung in `app.rs`/Slice B3) reicht das
//! Ergebnis von `classify_armed_approval_key` als `armed`-Flag durch und
//! wertet die zurückgegebene [`DialogAction`] aus. Ebenso berechnet der
//! Aufrufer die „nicht mehr fragen“-Regel (`harw_extension_api::allow_rules::
//! derive_shell_rule`) **vorher** — dieses Modul rendert die Regel nur, wenn
//! sie als [`ApprovalDialogRequest::remember_rule`] übergeben wird.
//!
//! # Schlüsseltypen
//! - [`ApprovalDialog`] — Widget-Zustand (Argumente, Auswahl, Details,
//!   Freitext-Eingabe). Zeichnet sich selbst in einen `ratatui::Buffer`,
//!   analog zu [`crate::command_popup::CommandPopup`] und
//!   [`crate::choice_dialog::ChoiceDialog`].
//! - [`ApprovalDialogRequest`] — unveränderliche Eingaben für
//!   [`ApprovalDialog::new`].
//! - [`ApprovalChoice`] — die vier möglichen Entscheidungen des Nutzers.
//! - [`DialogAction`] — Ereignis, das `ApprovalDialog::handle_key`
//!   (crate-intern) zurückgibt.
//!
//! # Terminal-Sicherheit
//! Wie `history_cell.rs` (W1-08, Register G-007/G-008): Werkzeugname,
//! Argumente, `cwd`, Risiko, Begründung, Herkunft und die „nicht mehr
//! fragen“-Regel sind nicht vertrauenswürdiger Text (Modell- bzw.
//! Agentenausgabe) und laufen vor dem Rendern durch
//! [`crate::sanitize::sanitize_reveal`]/[`crate::sanitize::sanitize_reveal_inline`]
//! (Argumentwerte, nichts wird verschluckt) bzw.
//! [`crate::sanitize::sanitize_inline`] (einzeilige Zusatzfelder). Die vom
//! Nutzer selbst getippte Freitext-Ablehnung nimmt nur Zeichen an, für die
//! `char::is_control()` falsch ist (siehe `ApprovalDialog::handle_key`).
//!
//! # Nebenläufigkeit
//! Kein interner Zustand wird geteilt; der Aufrufer hält `ApprovalDialog`
//! exklusiv. Keine Locks, keine Threads.
//!
//! # Fehlertypen
//! Keine — alle Operationen sind infallibel.
//!
//! # Beispiele
//! ```ignore
//! use std::time::{Duration, Instant};
//! use harw_tui::approval_dialog::{ApprovalDialog, ApprovalDialogRequest};
//!
//! let call = harw_extension_api::ToolCall {
//!     id: harw_types::ToolCallId::new(),
//!     name: harw_extension_api::ToolName::new("shell.exec"),
//!     arguments: serde_json::json!({ "command": "git status --short" }),
//! };
//! let dialog = ApprovalDialog::new(ApprovalDialogRequest {
//!     call,
//!     cwd: Some("/home/u/project".to_owned()),
//!     justification: None,
//!     risk: None,
//!     origin: None,
//!     remember_rule: Some("git status".to_owned()),
//!     deadline: Instant::now() + Duration::from_secs(1800),
//!     reason_input_enabled: true,
//! });
//! assert!(dialog.desired_height(60) > 0);
//! ```

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders},
};

use harw_extension_api::ToolCall;

use crate::dialog_frame::{self, BodyScroll, DialogContent, PinnedRow};
use crate::history_cell::{
    APPROVAL_COLLAPSED_ARGUMENT_LINES, ApprovalArgument, ApprovalArgumentValue, wrap_plain,
};
/// Kategorie, mit der der Auto-Modus meldet, dass sein Klassifizierer kein
/// Urteil lieferte (`harw_extension_api::AutoVerdict::fallback_ask`).
const CLASSIFIER_UNAVAILABLE: &str = "classifier-unavailable";

/// Rohe Ursache einer leeren Klassifizierer-Antwort, wie sie die
/// Modell-Anbindung meldet (`harw_core::one_shot`).
const EMPTY_RESPONSE_RAW: &str = "model returned an empty text response";

/// R18 F2: liest aus einem Auto-Modus-Grund (`"<Kategorie> – <Grund>"`) die
/// Ursache heraus, wenn der Klassifizierer ausfiel.
///
/// # Beschreibung
/// Vorher stand im Dialog „Auto-Modus: classifier-unavailable –
/// Klassifizierer-Fehler: model returned an empty text response“. Jetzt:
/// Kategorie weg, das Präfix „Klassifizierer-Fehler: “ weg, die rohe
/// englische Meldung der leeren Antwort als „Klassifizierer lieferte leere
/// Antwort“; eine fehlende Ursache wird zu „Klassifizierer nicht erreichbar“.
/// Andere Texte (auch deutsche Gründe mit Modellnamen) bleiben unverändert.
///
/// # Rückgabe
/// `Some(Ursache)` bei der Kategorie `classifier-unavailable`, sonst `None`
/// (dann gilt der Grund unverändert).
fn classifier_unavailable_cause(reason: &str) -> Option<String> {
    let rest = reason.trim().strip_prefix(CLASSIFIER_UNAVAILABLE)?;
    let rest = rest.trim_start();
    let rest = rest
        .strip_prefix('–')
        .or_else(|| rest.strip_prefix('-'))
        .or_else(|| rest.strip_prefix(':'))
        .unwrap_or(rest)
        .trim();
    let cause = rest
        .strip_prefix("Klassifizierer-Fehler:")
        .map_or(rest, str::trim);
    let cause = if cause.is_empty() {
        "Klassifizierer nicht erreichbar".to_owned()
    } else if cause.eq_ignore_ascii_case(EMPTY_RESPONSE_RAW) {
        "Klassifizierer lieferte leere Antwort".to_owned()
    } else {
        cause.replace(EMPTY_RESPONSE_RAW, "leere Antwort")
    };
    Some(cause)
}

// Runde 5, Teil E: Lern-Angebot des Auto-Modus.
use crate::permissions_view::{LearnOfferView, LearnScope};
use crate::sanitize::{sanitize_inline, sanitize_reveal, sanitize_reveal_inline};
use crate::style::{self, Theme};

/// Fußzeilen-Hinweis mit der Tastaturbelegung des Panels.
const FOOTER_HINT: &str = "↑↓ wählen · Enter bestätigen · v Details · Esc = Nein";

/// Varianten des Fußzeilen-Hinweises, von lang nach kurz; gezeigt wird die
/// längste, die in die Innenbreite passt (nie abgeschnitten).
const FOOTER_HINTS: &[&str] = &[
    FOOTER_HINT,
    "↑↓ wählen · Enter ok · v Details · Esc Nein",
    "↑↓ · Enter · v Details · Esc Nein",
    "↑↓ Enter v Esc",
];

/// Endung der verdichteten Vorschauzeile des Hauptarguments.
const DETAILS_MARKER: &str = "… v Details";

/// Hinweis auf `Esc` in der Beschriftung der Ablehnungs-Option.
const REJECT_HINT_PLAIN: &str = "(Esc)";

/// Hinweis auf `Esc` plus `Tab`, wenn eine Begründung eingegeben werden kann.
const REJECT_HINT_WITH_REASON: &str = "(Esc) · Tab: Grund angeben";

/// Text vor dem Freitext-Eingabefeld für die Ablehnungsbegründung.
const REASON_PROMPT: &str = "Grund: ";

/// Schwelle (Sekunden), ab der der Countdown in Warnfarbe erscheint.
const COUNTDOWN_WARNING_THRESHOLD_SECS: u64 = 60;

/// Die Entscheidung, die der Nutzer im Freigabe-Panel getroffen hat.
///
/// # Beschreibung
/// Entspricht den vier Optionen aus Plan Schritt 3. Der Aufrufer übersetzt
/// dies in eine [`harw_core::ApprovalResolution`] (Approve/Reject) und, bei
/// [`Self::ApproveAndRemember`]/[`Self::ApproveAndAutoMode`], in
/// zusätzliche Seiteneffekte (Regel speichern, Modus wechseln) — dieses
/// Modul selbst löst keinen dieser Effekte aus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalChoice {
    /// Option 1: einmalig freigeben.
    Approve,
    /// Option 2 (nur sichtbar, wenn eine Regel ableitbar ist): freigeben und
    /// die genannte Regel dauerhaft als „nicht mehr fragen“ hinterlegen.
    ApproveAndRemember(String),
    /// Option 3: freigeben und in den auto-Modus wechseln.
    ApproveAndAutoMode,
    /// Runde 5, Teil E: freigeben und künftig erlauben (Lern-Angebot ab der
    /// dritten gleichartigen Freigabe), mit gewähltem Scope. Der Aufrufer
    /// legt die Regel an (Sitzung) bzw. schreibt sie über
    /// `/permissions allow … --project` (Projekt).
    ApproveAndLearn {
        /// Das angenommene Angebot.
        offer: LearnOfferView,
        /// Sitzung oder Projekt.
        scope: LearnScope,
    },
    /// Option 4: ablehnen, optional mit einer vom Nutzer getippten Begründung.
    Reject {
        /// `Some(text)`, wenn der Nutzer über `Tab` eine Begründung eingegeben
        /// und mit `Enter` abgeschickt hat; sonst `None`.
        reason: Option<String>,
    },
}

/// Ereignis, das `ApprovalDialog::handle_key` zurückgibt.
///
/// # Beschreibung
/// - `Stay`: Panel bleibt offen, keine Entscheidung.
/// - `Decided(choice)`: der Nutzer hat entschieden; der Aufrufer schließt
///   das Panel und löst die Entscheidung ein.
/// - `ToggleDetails`: `v` wurde gedrückt; der interne Aufklapp-Zustand hat
///   bereits gewechselt (siehe [`ApprovalDialog::desired_height`] für die
///   neue Höhe), der Aufrufer muss nur neu rendern/die Fläche neu bemessen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogAction {
    /// Panel bleibt offen, keine Entscheidung getroffen.
    Stay,
    /// Der Nutzer hat entschieden.
    Decided(ApprovalChoice),
    /// Der Aufklapp-Zustand der Argumente wurde umgeschaltet.
    ToggleDetails,
    /// Der Körper (Befehl, Info-Zeilen) wurde gescrollt (`Strg+↑↓`); die
    /// Auswahl ist unverändert. Nur neu zeichnen — die Taste darf nicht
    /// weitergereicht werden.
    Scrolled,
}

/// Eingaben für [`ApprovalDialog::new`].
///
/// # Beschreibung
/// Trägt bewusst den vollständigen [`ToolCall`] statt getrennter
/// `tool_name`/`arguments`-Felder: `harw-tui` führt `serde_json` nicht als
/// eigene Abhängigkeit (nur transitiv über `harw-extension-api`/
/// `harw-tools`), und `ToolCall` ist bereits der Typ, den
/// [`crate::history_cell::ApprovalArgument::from_call`] für genau diesen
/// Zweck entgegennimmt — die Zerlegung in Schlüssel/Wert-Paare bleibt damit
/// über einen zentralen Ort definiert, statt zweimal (hier und in
/// `history_cell.rs`) leicht abweichend nachgebaut zu werden.
#[derive(Debug, Clone)]
pub struct ApprovalDialogRequest {
    /// Der vom Kern festgehaltene Werkzeugaufruf (Name + Argumente).
    pub call: ToolCall,
    /// Arbeitsverzeichnis des Aufrufs, falls bekannt.
    pub cwd: Option<String>,
    /// Vom Agenten mitgelieferte Begründung für den Aufruf.
    pub justification: Option<String>,
    /// Risikoeinschätzung als kurzer Text (z. B. „hoch — löscht Dateien“).
    pub risk: Option<String>,
    /// Herkunftsangabe, wenn der Aufruf von einem Kind-/Unteragenten stammt
    /// (z. B. `"von executor"`).
    pub origin: Option<String>,
    /// Vom Aufrufer über `harw_extension_api::allow_rules::derive_shell_rule`
    /// abgeleitete Regel-Vorschlag; `None`, wenn keine sichere Regel
    /// ableitbar ist (Option 2 entfällt dann, siehe [`ApprovalDialog`]).
    pub remember_rule: Option<String>,
    /// Zeitpunkt, zu dem die Freigabefrage automatisch als Ablehnung gilt.
    /// Ein [`Instant`] statt einer Restsekundenzahl, damit der Countdown bei
    /// jedem Rendern gegen die tatsächlich verstrichene Zeit neu berechnet
    /// wird, statt einen einmal übergebenen Wert veralten zu lassen.
    pub deadline: Instant,
    /// `true`, wenn der Aufrufer eine getippte Ablehnungsbegründung erlaubt
    /// (`Tab`-Hinweis und Freitexteingabe erscheinen dann in Option 4).
    pub reason_input_enabled: bool,
}

/// Eine der vier Optionen, unabhängig von ihrer sichtbaren Nummer.
///
/// # Beschreibung
/// Die sichtbare Nummer ergibt sich allein aus der Position in
/// [`ApprovalDialog::visible_options`] — [`OptionKind::Remember`] fehlt in
/// dieser Liste, wenn keine Regel vorgeschlagen wurde, und alle folgenden
/// Optionen rücken automatisch nach (Plan Schritt 3: „Renumber correctly
/// when option 2 is absent“).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OptionKind {
    /// „Ja“.
    Approve,
    /// „Ja, und nicht mehr fragen für: …“.
    Remember,
    /// „Ja, und in den auto-Modus wechseln“.
    AutoMode,
    /// Runde 5, Teil E: „Ja, und künftig erlauben: … (nur diese Sitzung)“.
    LearnSession,
    /// Runde 5, Teil E: „Ja, und künftig erlauben: … (dauerhaft im Projekt)“.
    LearnProject,
    /// „Nein“.
    Reject,
}

/// Stil-Kategorie einer gerenderten Zeile; die tatsächliche Farbe entsteht
/// erst in [`ApprovalDialog::render`] aus dem übergebenen [`Theme`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowStyle {
    /// Countdown-Zeile; `true` = unter der Warnschwelle.
    Countdown { warn: bool },
    /// Hervorgehobenes Hauptargument (Befehl/Pfad).
    Primary,
    /// Gedimmte Zusatzinformation (cwd, Risiko, Begründung, Herkunft,
    /// eingeklappte übrige Argumente).
    Dim,
    /// Optionszeile; `true` = aktuell markiert.
    Option { selected: bool },
    /// Freitext-Eingabezeile für die Ablehnungsbegründung.
    ReasonInput,
    /// Unformatierte Zeile (Leerzeile, Fußzeile).
    Plain,
}

impl RowStyle {
    /// Löst diese Kategorie in einen konkreten [`Style`] auf.
    fn resolve(self, theme: Theme) -> Style {
        match self {
            RowStyle::Countdown { warn: true } => Style::default()
                .fg(style::warning_color(theme))
                .add_modifier(Modifier::BOLD),
            RowStyle::Countdown { warn: false } => style::dim_style(theme),
            RowStyle::Primary => Style::default().add_modifier(Modifier::BOLD),
            RowStyle::Dim => style::dim_style(theme),
            RowStyle::Option { selected: true } => style::selected_style(theme),
            RowStyle::Option { selected: false } => Style::default(),
            RowStyle::ReasonInput => Style::default(),
            RowStyle::Plain => Style::default(),
        }
    }
}

/// Freigabe-Panel: ersetzt den Composer, solange eine Freigabe offen ist.
///
/// # Beschreibung
/// Hält Titel-relevante Daten ([`ApprovalDialogRequest`]), den aus dem
/// [`ToolCall`] gelösten Argumentsatz (über
/// [`ApprovalArgument::from_call`], damit die Darstellung mit der
/// bestehenden Freigabe-Zelle übereinstimmt), die aktuelle Auswahl, den
/// Aufklapp-Zustand der übrigen Argumente sowie den Zustand der optionalen
/// Freitext-Eingabe.
///
/// # Nebenläufigkeit
/// Nicht thread-sicher; exklusiver `&mut`-Zugriff des Aufrufers erwartet
/// (analog [`crate::command_popup::CommandPopup`]).
#[derive(Debug)]
pub struct ApprovalDialog {
    /// Name des zur Freigabe anstehenden Werkzeugs (bestimmt den Titel und
    /// das bevorzugte Hauptargument).
    tool_name: String,
    /// Aus dem `ToolCall` gelöste Argumente; `None`, wenn die Argumente kein
    /// JSON-Objekt sind (dann bleibt der Argumentblock leer).
    arguments: Option<Vec<ApprovalArgument>>,
    /// Arbeitsverzeichnis des Aufrufs, falls bekannt.
    cwd: Option<String>,
    /// Vom Agenten mitgelieferte Begründung.
    justification: Option<String>,
    /// Risikoeinschätzung.
    risk: Option<String>,
    /// Herkunftsangabe (Kind-/Unteragent).
    origin: Option<String>,
    /// Regel-Vorschlag für „nicht mehr fragen“; steuert, ob Option 2
    /// erscheint.
    remember_rule: Option<String>,
    /// Runde 5, Teil E: Lern-Angebot; steuert, ob die beiden
    /// „künftig erlauben“-Optionen erscheinen.
    learn_offer: Option<LearnOfferView>,
    /// Runde 5 (Integration O): nur einmalige Zustimmung anbieten — blendet
    /// „Ja, und in den auto-Modus wechseln“ aus (Kind-Freigaben).
    once_only: bool,
    /// Runde 6, Teil A1: warum der Auto-Modus fragt („<Kategorie> –
    /// <Grund>“); erscheint als Zeile „Auto-Modus: …“.
    auto_reason: Option<String>,
    /// Zeitpunkt, zu dem die Frage automatisch als Ablehnung gilt.
    deadline: Instant,
    /// Ob die Freitext-Ablehnungsbegründung angeboten wird.
    reason_input_enabled: bool,
    /// Index der aktuell markierten Option in [`Self::visible_options`].
    selected: usize,
    /// `true`, wenn die übrigen Argumente vollständig angezeigt werden.
    expanded: bool,
    /// `true`, während die Freitext-Eingabe aktiv ist (nach `Tab`).
    reason_editing: bool,
    /// Bisher getippter Text der Freitext-Eingabe.
    reason_text: String,
    /// Scroll-Zustand des Körpers (Befehl, Info-Zeilen, übrige Argumente).
    body_scroll: BodyScroll,
}

impl ApprovalDialog {
    /// Baut ein neues Freigabe-Panel mit der ersten Option markiert und
    /// eingeklappten übrigen Argumenten.
    ///
    /// # Argumente
    /// - `request` ([`ApprovalDialogRequest`]): siehe Feldbeschreibungen dort.
    ///
    /// # Rückgabe
    /// Ein einsatzbereites `ApprovalDialog`.
    #[must_use]
    pub fn new(request: ApprovalDialogRequest) -> Self {
        let tool_name = request.call.name.as_str().to_owned();
        let arguments = ApprovalArgument::from_call(&request.call);
        Self {
            tool_name,
            arguments,
            cwd: request.cwd,
            justification: request.justification,
            risk: request.risk,
            origin: request.origin,
            remember_rule: request.remember_rule,
            learn_offer: None,
            once_only: false,
            auto_reason: None,
            deadline: request.deadline,
            reason_input_enabled: request.reason_input_enabled,
            selected: 0,
            expanded: false,
            reason_editing: false,
            reason_text: String::new(),
            body_scroll: BodyScroll::default(),
        }
    }

    /// Runde 5, Teil E: hängt das Lern-Angebot an („Ja, und künftig
    /// erlauben: `<muster>`“, je eine Option für Sitzung und Projekt).
    ///
    /// # Argumente
    /// - `offer` ([`LearnOfferView`]): das Angebot aus
    ///   `harw_runtime::AutoModeHandle::learning_offer`.
    ///
    /// # Rückgabe
    /// Das Panel mit den beiden zusätzlichen Optionen.
    #[must_use]
    pub fn with_learning_offer(mut self, offer: LearnOfferView) -> Self {
        self.learn_offer = Some(offer);
        self
    }

    /// Runde 6, Teil A1: hängt den Grund an, aus dem der Auto-Modus fragt.
    ///
    /// # Argumente
    /// - `reason` (`Option<String>`): `"<Kategorie> – <Grund>"` aus
    ///   `crate::permissions_view::auto_ask_reason_for`; `None` lässt das
    ///   Panel unverändert.
    ///
    /// # Rückgabe
    /// Das Panel mit der Zeile „Auto-Modus: …“.
    #[must_use]
    pub fn with_auto_reason(mut self, reason: Option<String>) -> Self {
        self.auto_reason = reason.filter(|reason| !reason.trim().is_empty());
        self
    }

    /// Runde 5 (Integration O): bietet nur „Ja“ und „Nein“ an — für
    /// Freigaben, die ausschließlich einmalig gelten (Kind-Agenten).
    ///
    /// # Rückgabe
    /// Das Panel ohne die Option „Ja, und in den auto-Modus wechseln“.
    #[must_use]
    pub fn once_only(mut self) -> Self {
        self.once_only = true;
        self
    }

    /// Gibt die aktuell sichtbaren Optionen in Anzeigereihenfolge zurück.
    ///
    /// [`OptionKind::Remember`] fehlt, wenn kein Regel-Vorschlag vorliegt —
    /// alle folgenden Optionen rücken dadurch automatisch eine Nummer nach.
    fn visible_options(&self) -> Vec<OptionKind> {
        let mut options = vec![OptionKind::Approve];
        if self.remember_rule.is_some() {
            options.push(OptionKind::Remember);
        }
        // Runde 5, Teil E: Lern-Angebot mit Scope-Wahl.
        if self.learn_offer.is_some() {
            options.push(OptionKind::LearnSession);
            options.push(OptionKind::LearnProject);
        }
        if !self.once_only {
            options.push(OptionKind::AutoMode);
        }
        options.push(OptionKind::Reject);
        options
    }

    /// Übersetzt eine [`OptionKind`] in die zugehörige [`ApprovalChoice`].
    fn choice_for(&self, kind: OptionKind) -> ApprovalChoice {
        match kind {
            OptionKind::Approve => ApprovalChoice::Approve,
            OptionKind::Remember => {
                ApprovalChoice::ApproveAndRemember(self.remember_rule.clone().unwrap_or_default())
            }
            OptionKind::AutoMode => ApprovalChoice::ApproveAndAutoMode,
            OptionKind::LearnSession | OptionKind::LearnProject => match &self.learn_offer {
                Some(offer) => ApprovalChoice::ApproveAndLearn {
                    offer: offer.clone(),
                    scope: if kind == OptionKind::LearnProject {
                        LearnScope::Project
                    } else {
                        LearnScope::Session
                    },
                },
                // Nur sichtbar mit Angebot; ohne bleibt es eine einfache Freigabe.
                None => ApprovalChoice::Approve,
            },
            OptionKind::Reject => ApprovalChoice::Reject { reason: None },
        }
    }

    /// Baut die Beschriftung einer Option (ohne Nummer/Marker).
    fn option_label(&self, kind: OptionKind) -> String {
        match kind {
            OptionKind::Approve => "Ja".to_owned(),
            OptionKind::Remember => {
                let rule = self.remember_rule.as_deref().unwrap_or_default();
                format!("Ja, und nicht mehr fragen für: {}", sanitize_inline(rule))
            }
            OptionKind::AutoMode => "Ja, und in den auto-Modus wechseln".to_owned(),
            OptionKind::LearnSession => self
                .learn_offer
                .as_ref()
                .map(|offer| offer.option_label(LearnScope::Session))
                .unwrap_or_default(),
            OptionKind::LearnProject => self
                .learn_offer
                .as_ref()
                .map(|offer| offer.option_label(LearnScope::Project))
                .unwrap_or_default(),
            OptionKind::Reject => {
                let hint = if self.reason_input_enabled {
                    REJECT_HINT_WITH_REASON
                } else {
                    REJECT_HINT_PLAIN
                };
                format!("Nein {hint}")
            }
        }
    }

    /// Verbleibende Zeit bis zum automatischen Ablehnen.
    ///
    /// # Rückgabe
    /// `Duration::ZERO`, wenn die Frist bereits verstrichen ist — der
    /// Countdown zeigt dann `noch 0:00` statt zu unterlaufen.
    fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    /// Verarbeitet einen Tastendruck und gibt eine [`DialogAction`] zurück.
    ///
    /// # Beschreibung
    /// **Arming-Vertrag**: Ist `armed == false`, liefert diese Methode für
    /// **jede** Taste `DialogAction::Stay` — sie wertet die Taste nicht
    /// einmal aus. Der Aufrufer bestimmt `armed` über
    /// `classify_armed_approval_key` (`app.rs`) und ist allein dafür
    /// zuständig, dass die erste kurze Zeitspanne nach dem Erscheinen des
    /// Panels keine Taste wirken lässt (Schutz vor versehentlicher
    /// Übernahme eines im Terminalpuffer wartenden Tastendrucks). Dieses
    /// Modul kennt die Dauer der Sperre nicht und darf sie nicht selbst
    /// durchsetzen — es verlässt sich vollständig auf den übergebenen Wert.
    ///
    /// Ist eine Freitext-Eingabe aktiv (nach `Tab`), gehen alle Tasten außer
    /// `Enter`/`Esc`/`Backspace` als Zeichen in den Eingabepuffer ein;
    /// `y`/`n`/`v`/Ziffern lösen währenddessen **keine** Auswahl aus.
    ///
    /// Tasten außerhalb der Eingabe:
    /// - `Up`/`Down`: Auswahl bewegen (an den Rändern begrenzt).
    /// - `1`-`9`: Auswahl direkt auf die entsprechende (1-basierte) Option
    ///   setzen, sofern sie existiert; wählt noch nicht aus — dafür `Enter`.
    /// - `Enter`: aktuell markierte Option entscheiden.
    /// - `y`/`Y`: sofort [`ApprovalChoice::Approve`] (Kurzwahl für Option 1).
    /// - `n`/`N`/`Esc`: sofort [`ApprovalChoice::Reject`] ohne Begründung
    ///   (Kurzwahl für Option 4).
    /// - `v`/`V`: Aufklapp-Zustand der übrigen Argumente umschalten.
    /// - `Tab`: Freitext-Eingabe öffnen, sofern
    ///   [`ApprovalDialogRequest::reason_input_enabled`] gesetzt war.
    ///
    /// # Argumente
    /// - `key` (`KeyEvent`): das eingegangene Crossterm-Tastenereignis.
    /// - `armed` (`bool`): `true`, wenn das Arming-Delay des Aufrufers
    ///   bereits abgelaufen ist.
    ///
    /// # Rückgabe
    /// Siehe [`DialogAction`].
    ///
    /// # Sichtbarkeit
    /// `pub(crate)`: der Parameter ist ein Crossterm-Typ (Fremdcrate vor 1.0),
    /// der nicht in der öffentlichen API stehen soll; einziger Aufrufer ist
    /// der Event-Loop in `app.rs` (samt Untermodul `app/child_approvals.rs`).
    pub(crate) fn handle_key(&mut self, key: KeyEvent, armed: bool) -> DialogAction {
        if !armed {
            return DialogAction::Stay;
        }
        // `Strg+↑↓` scrollt den Körper und verändert nie die Auswahl.
        if self.body_scroll.handle_key(&key) {
            return DialogAction::Scrolled;
        }
        if self.reason_editing {
            return self.handle_reason_key(key);
        }

        let options = self.visible_options();
        match key.code {
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                DialogAction::Stay
            }
            KeyCode::Down => {
                if !options.is_empty() {
                    self.selected = (self.selected + 1).min(options.len() - 1);
                }
                DialogAction::Stay
            }
            KeyCode::Char(c @ '1'..='9') => {
                let zero_based = (c as usize) - ('1' as usize);
                if zero_based < options.len() {
                    self.selected = zero_based;
                }
                DialogAction::Stay
            }
            KeyCode::Enter => {
                let kind = options
                    .get(self.selected)
                    .copied()
                    .unwrap_or(OptionKind::Reject);
                DialogAction::Decided(self.choice_for(kind))
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                DialogAction::Decided(ApprovalChoice::Approve)
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                DialogAction::Decided(ApprovalChoice::Reject { reason: None })
            }
            KeyCode::Char('v') | KeyCode::Char('V') => {
                self.expanded = !self.expanded;
                self.body_scroll.reset();
                DialogAction::ToggleDetails
            }
            KeyCode::Tab if self.reason_input_enabled => {
                self.reason_editing = true;
                self.reason_text.clear();
                DialogAction::Stay
            }
            _ => DialogAction::Stay,
        }
    }

    /// Verarbeitet einen Tastendruck, während die Freitext-Eingabe aktiv ist.
    ///
    /// Wird nur von [`Self::handle_key`] gerufen, nachdem `armed` bereits
    /// geprüft wurde.
    fn handle_reason_key(&mut self, key: KeyEvent) -> DialogAction {
        match key.code {
            KeyCode::Enter => {
                let reason = std::mem::take(&mut self.reason_text);
                self.reason_editing = false;
                DialogAction::Decided(ApprovalChoice::Reject {
                    reason: Some(reason),
                })
            }
            KeyCode::Esc => {
                // „Esc leaves the input“: nur die Eingabe verlassen, nicht
                // ablehnen — die Ablehnung bleibt `n`/Esc außerhalb der
                // Eingabe vorbehalten.
                self.reason_editing = false;
                self.reason_text.clear();
                DialogAction::Stay
            }
            KeyCode::Backspace => {
                self.reason_text.pop();
                DialogAction::Stay
            }
            KeyCode::Char(c) if !c.is_control() => {
                self.reason_text.push(c);
                DialogAction::Stay
            }
            _ => DialogAction::Stay,
        }
    }

    /// Berechnet die Höhe (in Zeilen, inklusive Rahmen), die [`Self::render`]
    /// bei der gegebenen Breite benötigt, damit nichts scrollen muss.
    ///
    /// # Beschreibung
    /// Der Aufrufer nutzt dies, um die Fläche zu bemessen, die den Composer
    /// ersetzt (Plan Schritt 3), und kappt sie auf die verfügbare Höhe; ist
    /// die Fläche kleiner, verdichtet [`Self::render`] das Hauptargument und
    /// lässt den Körper scrollen (siehe [`crate::dialog_frame`]). Wächst mit
    /// ausgeklappten Argumenten (`v`) und mit aktiver Freitext-Eingabe.
    ///
    /// # Argumente
    /// - `width` (`u16`): Gesamtbreite in Spalten (inklusive Rahmen).
    ///
    /// # Rückgabe
    /// Benötigte Gesamthöhe in Zeilen (inklusive oberem und unterem Rahmen).
    #[must_use]
    pub fn desired_height(&self, width: u16) -> u16 {
        let inner_width = width.saturating_sub(2).max(1);
        self.content(inner_width, false, Theme::Dark)
            .desired_height(width)
    }

    /// Scrollt den Körper per Mausrad.
    ///
    /// Die App leitet das Mausrad über [`Self::scroll_body`]
    /// (`app/scroll_routing.rs`), daher rufen nur Tests diese Methode.
    ///
    /// # Rückgabe
    /// `true`, wenn neu gezeichnet werden soll.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn scroll_wheel(&self, kind: crossterm::event::MouseEventKind) -> bool {
        self.body_scroll.handle_wheel(kind)
    }

    /// Scrollt den Körper um `lines` Zeilen (Scroll-Aktionen der
    /// Tastenbelegung); ändert nie die Auswahl.
    ///
    /// # Rückgabe
    /// Immer `true` (neu zeichnen).
    pub fn scroll_body(&self, up: bool, lines: usize) -> bool {
        if up {
            self.body_scroll.scroll_up(lines);
        } else {
            self.body_scroll.scroll_down(lines);
        }
        true
    }

    /// Aktueller Scroll-Abstand des Körpers (für Tests und Diagnose).
    #[must_use]
    pub fn body_offset(&self) -> usize {
        self.body_scroll.offset()
    }

    /// Zeichnet das Panel (Rahmen, Titel, Inhalt) in den angegebenen
    /// `Buffer`-Bereich.
    ///
    /// # Beschreibung
    /// Rahmen und Titel in der Warnfarbe des Themes, der Countdown rechts im
    /// oberen Rahmen. Optionen, Freitext-Eingabe und Hinweiszeile sind
    /// angeheftet und immer vollständig sichtbar; reicht `area` nicht, wird
    /// zuerst das Hauptargument auf eine Vorschauzeile „… v Details“
    /// verdichtet, dann scrollt der Körper (Info-Zeilen) innerhalb des
    /// Panels (`Strg+↑↓`, Mausrad), dann entfallen Leerzeilen. Gezeichnet
    /// wird nie außerhalb von `area ∩ buf.area`.
    ///
    /// # Argumente
    /// - `area` (`Rect`): der Zeichenbereich im Terminal-Buffer.
    /// - `buf` (`&mut Buffer`): der ratatui-Buffer, in den geschrieben wird.
    /// - `theme` (`&Theme`): aktives Farbschema.
    ///
    /// # Nebenläufigkeit
    /// Rein synchron; kein Locking erforderlich.
    ///
    /// # Sichtbarkeit
    /// `pub(crate)` statt `pub`: [`Theme`] ist crate-privat (`style`-Modul ist
    /// `pub(crate)`, siehe `lib.rs`), eine `pub`-Methode mit `&Theme`-Parameter
    /// wäre von außerhalb der Crate ohnehin nicht aufrufbar gewesen — externer
    /// Code kann keinen `Theme`-Wert konstruieren. Rendering ist damit
    /// konsequent als interne Implementierung markiert, konsistent mit
    /// `history_cell`/`style` (beide `pub(crate)`); nur der Zustand
    /// (`ApprovalDialog` selbst, `desired_height`, `scroll_body`,
    /// `body_offset`) bleibt Teil der öffentlichen Fläche des Crates;
    /// `handle_key`/`scroll_wheel` sind ebenfalls `pub(crate)`, weil sie
    /// Crossterm-Typen nehmen.
    pub(crate) fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let theme = *theme;
        let border_color = style::warning_color(theme);
        let title_style = Style::default()
            .fg(border_color)
            .add_modifier(Modifier::BOLD);
        let title = self.title_text();
        let title_width = unicode_width::UnicodeWidthStr::width(title.as_str());
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(Span::styled(title, title_style));
        let area = area.intersection(buf.area);
        let inner = block.inner(area);
        let mut content = self.content(inner.width.max(1), false, theme);
        if !self.expanded && !content.fits(inner) {
            content = self.content(inner.width.max(1), true, theme);
        }
        dialog_frame::render_dialog(
            block,
            area,
            buf,
            &content,
            &self.body_scroll,
            style::dim_style(theme),
        );
        let remaining = self.remaining();
        let countdown_style = RowStyle::Countdown {
            warn: remaining.as_secs() < COUNTDOWN_WARNING_THRESHOLD_SECS,
        }
        .resolve(theme);
        dialog_frame::render_top_right(
            area,
            buf,
            &format!(" {} ", format_countdown(remaining)),
            title_width,
            countdown_style,
        );
    }

    /// Wählt den Titel anhand des Werkzeugnamens (Plan Schritt 3).
    fn title_text(&self) -> String {
        match self.tool_name.as_str() {
            "shell.exec" => " Befehl ausführen? ".to_owned(),
            "fs.write" | "fs.patch" => " Datei schreiben? ".to_owned(),
            // Runde 5, Teil H: `fs.edit` (Teil D) bekommt einen eigenen Titel.
            "fs.edit" => " Datei bearbeiten? ".to_owned(),
            _ => " Werkzeug freigeben? ".to_owned(),
        }
    }

    /// Baut Körper und angeheftete Zeilen (ohne Rahmen).
    ///
    /// Gemeinsam von [`Self::desired_height`] und [`Self::render`] genutzt,
    /// damit beide nie auseinanderlaufen.
    ///
    /// # Argumente
    /// - `width`: Innenbreite.
    /// - `compact_primary`: Hauptargument als eine Vorschauzeile
    ///   („… v Details“), wenn der Platz nicht reicht.
    fn content(&self, width: u16, compact_primary: bool, theme: Theme) -> DialogContent {
        let mut body: Vec<Line<'static>> = Vec::new();
        let primary_style = RowStyle::Primary.resolve(theme);
        let dim = RowStyle::Dim.resolve(theme);
        let primary = self.primary_argument_strings(width);
        if compact_primary && primary.len() > 1 {
            body.push(Line::styled(
                compact_preview(&primary.join(" "), width),
                primary_style,
            ));
        } else {
            for line in primary {
                body.push(Line::styled(line, primary_style));
            }
        }

        for (label, value) in self.info_fields() {
            let text = format!("{label}: {}", sanitize_inline(&value));
            for line in wrapped_strings(&text, width) {
                body.push(Line::styled(line, dim));
            }
        }

        let rest = self.other_argument_strings(width);
        if !rest.is_empty() {
            body.push(Line::default());
            for line in rest {
                body.push(Line::styled(line, dim));
            }
        }

        let mut pinned = vec![PinnedRow::padding()];
        for (idx, kind) in self.visible_options().into_iter().enumerate() {
            let selected = idx == self.selected;
            let marker = if selected { "❯ " } else { "  " };
            let text = format!("{marker}{}. {}", idx + 1, self.option_label(kind));
            let row_style = RowStyle::Option { selected }.resolve(theme);
            for line in wrapped_strings(&text, width) {
                pinned.push(PinnedRow::content(Line::styled(line, row_style)));
            }
        }

        if self.reason_editing {
            let text = format!("{REASON_PROMPT}{}", self.reason_text);
            for line in wrapped_strings(&text, width) {
                pinned.push(PinnedRow::content(Line::styled(
                    line,
                    RowStyle::ReasonInput.resolve(theme),
                )));
            }
        }

        pinned.push(PinnedRow::padding());
        let hint = dialog_frame::pick_fitting(FOOTER_HINTS, width);
        for line in wrapped_strings(hint, width) {
            pinned.push(PinnedRow::content(Line::styled(
                line,
                RowStyle::Plain.resolve(theme),
            )));
        }

        DialogContent { body, pinned }
    }

    /// Zusatzfelder (Herkunft, Auto-Modus-Grund, cwd, Risiko, Begründung) in
    /// Anzeigereihenfolge, jeweils mit Beschriftung; fehlende Felder werden
    /// ausgelassen.
    fn info_fields(&self) -> Vec<(&'static str, String)> {
        let mut fields = Vec::new();
        if let Some(origin) = &self.origin {
            // Runde 5, Teil O: Kind-Freigaben nennen ihren Absender als
            // „angefragt von: <rolle> (<pfad im baum>)“.
            fields.push(("angefragt von", origin.clone()));
        }
        // Runde 6, Teil A1: der Grund des Auto-Modus steht vor cwd/Risiko.
        // R18 F2: fiel der Klassifizierer aus, steht dort in Klartext, dass
        // der Auto-Modus nicht entscheiden konnte und warum.
        if let Some(reason) = &self.auto_reason {
            match classifier_unavailable_cause(reason) {
                Some(cause) => fields.push((
                    "Auto-Modus nicht verfügbar",
                    format!("{cause} – bitte selbst entscheiden"),
                )),
                None => fields.push(("Auto-Modus", reason.clone())),
            }
        }
        if let Some(cwd) = &self.cwd {
            fields.push(("cwd", cwd.clone()));
        }
        if let Some(risk) = &self.risk {
            fields.push(("Risiko", risk.clone()));
        }
        if let Some(justification) = &self.justification {
            fields.push(("Begründung", justification.clone()));
        }
        fields
    }

    /// Zeilen des Hauptarguments (Befehl/Pfad), niemals eingeklappt.
    fn primary_argument_strings(&self, width: u16) -> Vec<String> {
        let Some(arguments) = &self.arguments else {
            return Vec::new();
        };
        let Some(argument) =
            primary_argument_index(&self.tool_name, arguments).and_then(|i| arguments.get(i))
        else {
            return Vec::new();
        };
        argument_display_strings(argument, width)
    }

    /// Zeilen der übrigen Argumente (alles außer dem Hauptargument),
    /// eingeklappt auf [`APPROVAL_COLLAPSED_ARGUMENT_LINES`], solange
    /// [`Self::expanded`] `false` ist.
    fn other_argument_strings(&self, width: u16) -> Vec<String> {
        let Some(arguments) = &self.arguments else {
            return Vec::new();
        };
        let primary = primary_argument_index(&self.tool_name, arguments);
        let mut rest = Vec::new();
        for (index, argument) in arguments.iter().enumerate() {
            if Some(index) == primary {
                continue;
            }
            rest.extend(argument_display_strings(argument, width));
        }
        if !self.expanded && rest.len() > APPROVAL_COLLAPSED_ARGUMENT_LINES {
            let hidden = rest.len() - APPROVAL_COLLAPSED_ARGUMENT_LINES;
            rest.truncate(APPROVAL_COLLAPSED_ARGUMENT_LINES);
            rest.extend(wrapped_strings(
                &format!("… {hidden} weitere Zeilen ausgeblendet — [v] vollständig anzeigen"),
                width,
            ));
        }
        rest
    }
}

/// Wählt das Hauptargument, das direkt hinter dem Werkzeugnamen steht.
///
/// Eigene, kleine Kopie von `history_cell::approval_primary_index` (dort
/// modul-privat und daher von hier aus nicht aufrufbar) — identische Logik,
/// damit die Darstellung übereinstimmt.
///
/// # Rückgabe
/// Index in `arguments`, falls ein passender Schlüssel vorhanden ist.
fn primary_argument_index(tool_name: &str, arguments: &[ApprovalArgument]) -> Option<usize> {
    let preferred: &[&str] = match tool_name {
        "fs.write" => &["path"],
        // Runde 5, Teil H: Schlüsselargumente von `fs.edit` (Teil D).
        "fs.edit" => &["path", "old_string", "new_string"],
        "shell.exec" => &["command"],
        _ => &["command", "path"],
    };
    preferred
        .iter()
        .find_map(|key| arguments.iter().position(|argument| argument.key == *key))
}

/// Setzt eine einzeilige Zeichenkette in Anführungszeichen (`"`/`\` escaped).
fn quote_text(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Rendert ein einzelnes Argument (Schlüssel + Wert) als Textzeilen,
/// terminal-sicher über [`sanitize_reveal`]/[`sanitize_reveal_inline`].
///
/// Mehrzeilige Textwerte bekommen eine Randmarke `│ ` je Zeile, damit eine
/// Werteszeile nie eine eigene Options- oder Fußzeile vortäuschen kann.
fn argument_display_strings(argument: &ApprovalArgument, width: u16) -> Vec<String> {
    let key = sanitize_reveal_inline(&argument.key);
    match &argument.value {
        ApprovalArgumentValue::Text(text) => {
            let value = sanitize_reveal(text);
            if value.contains('\n') {
                let mut lines = wrapped_strings(&format!("{key}:"), width);
                for part in value.split('\n') {
                    lines.extend(wrapped_strings(&format!("│ {part}"), width));
                }
                lines
            } else {
                wrapped_strings(&format!("{key}: {}", quote_text(&value)), width)
            }
        }
        ApprovalArgumentValue::Json(json) => {
            wrapped_strings(&format!("{key}: {}", sanitize_reveal_inline(json)), width)
        }
    }
}

/// Bricht `text` über [`wrap_plain`] um und extrahiert die reinen
/// Zeicheninhalte (ohne Stil) als `String`s.
fn wrapped_strings(text: &str, width: u16) -> Vec<String> {
    wrap_plain(text, width)
        .into_iter()
        .map(|line| {
            line.spans
                .into_iter()
                .map(|s| s.content.into_owned())
                .collect()
        })
        .collect()
}

/// Formatiert die verbleibende Zeit als `"noch M:SS"` (z. B. `"noch 4:52"`).
fn format_countdown(remaining: Duration) -> String {
    let total_secs = remaining.as_secs();
    format!("noch {}:{:02}", total_secs / 60, total_secs % 60)
}

/// Einzeilige Vorschau des Hauptarguments, die mit „… v Details“ endet
/// und genau in `width` Spalten passt.
fn compact_preview(text: &str, width: u16) -> String {
    use unicode_width::UnicodeWidthChar;
    let marker_width = unicode_width::UnicodeWidthStr::width(DETAILS_MARKER);
    let budget = usize::from(width).saturating_sub(marker_width + 1);
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let ch = if ch == '\n' { ' ' } else { ch };
        let w = ch.width().unwrap_or(0);
        if used + w > budget {
            break;
        }
        used += w;
        out.push(ch);
    }
    if usize::from(width) > marker_width {
        out.push(' ');
        out.push_str(DETAILS_MARKER);
    } else {
        out = "…".to_owned();
    }
    out
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use crossterm::event::KeyModifiers;
    use ratatui::layout::Rect;

    /// Baut einen `ToolCall` mit den gegebenen Argumenten.
    fn tool_call(tool: &str, arguments: harw_tools::serde_json::Value) -> ToolCall {
        ToolCall {
            id: harw_types::ToolCallId::new(),
            name: harw_extension_api::ToolName::new(tool),
            arguments,
        }
    }

    /// Baut ein Panel mit `shell.exec` und einem einzelnen `command`-Argument,
    /// einer Restlaufzeit von 300s und optionalem Regel-Vorschlag.
    fn shell_dialog(remember_rule: Option<&str>) -> ApprovalDialog {
        ApprovalDialog::new(ApprovalDialogRequest {
            call: tool_call(
                "shell.exec",
                harw_tools::serde_json::json!({ "command": "git status --short" }),
            ),
            cwd: Some("/home/u/project".to_owned()),
            justification: None,
            risk: None,
            origin: None,
            remember_rule: remember_rule.map(str::to_owned),
            deadline: Instant::now() + Duration::from_secs(300),
            reason_input_enabled: true,
        })
    }

    fn make_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn buffer_to_string(buf: &Buffer) -> String {
        let area = buf.area();
        let mut out = String::new();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn render_dialog(dialog: &ApprovalDialog, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        dialog.render(area, &mut buf, &Theme::Dark);
        buffer_to_string(&buf)
    }

    // ── Optionsliste und Nummerierung ───────────────────────────────────

    #[test]
    fn test_options_with_remember_rule_are_numbered_one_to_four() {
        let dialog = shell_dialog(Some("git status"));
        let rendered = render_dialog(&dialog, 70, 20);
        assert!(rendered.contains("1. Ja"), "{rendered}");
        assert!(
            rendered.contains("2. Ja, und nicht mehr fragen für: git status"),
            "{rendered}"
        );
        assert!(
            rendered.contains("3. Ja, und in den auto-Modus wechseln"),
            "{rendered}"
        );
        assert!(rendered.contains("4. Nein"), "{rendered}");
    }

    /// Runde 5 (Integration O): Kind-Freigaben bieten nur „Ja“ und „Nein“.
    #[test]
    fn test_once_only_hides_the_auto_mode_option() {
        let dialog = shell_dialog(None).once_only();
        assert_eq!(
            dialog.visible_options(),
            vec![OptionKind::Approve, OptionKind::Reject]
        );
        let rendered = render_dialog(&dialog, 70, 20);
        assert!(!rendered.contains("auto-Modus"), "{rendered}");
        assert!(rendered.contains("2. Nein"), "{rendered}");
    }

    #[test]
    fn test_options_without_remember_rule_are_renumbered() {
        let dialog = shell_dialog(None);
        let rendered = render_dialog(&dialog, 70, 20);
        assert!(rendered.contains("1. Ja"), "{rendered}");
        assert!(!rendered.contains("nicht mehr fragen"), "{rendered}");
        assert!(
            rendered.contains("2. Ja, und in den auto-Modus wechseln"),
            "{rendered}"
        );
        assert!(rendered.contains("3. Nein"), "{rendered}");
        assert!(!rendered.contains("4."), "{rendered}");
    }

    // ── Auswahl per Ziffer/Pfeil, Enter entscheidet ─────────────────────

    #[test]
    fn test_digit_and_arrow_selection_move_selected_index() {
        let mut dialog = shell_dialog(None); // Optionen: Ja / AutoMode / Nein
        assert_eq!(dialog.selected, 0);

        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Down), true),
            DialogAction::Stay
        );
        assert_eq!(dialog.selected, 1);

        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Char('3')), true),
            DialogAction::Stay
        );
        assert_eq!(dialog.selected, 2);

        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Up), true),
            DialogAction::Stay
        );
        assert_eq!(dialog.selected, 1);
    }

    #[test]
    fn test_enter_returns_choice_for_currently_selected_option() {
        let mut dialog = shell_dialog(Some("git status"));
        // Option 3 (AutoMode) über Ziffer markieren, dann mit Enter bestätigen.
        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Char('3')), true),
            DialogAction::Stay
        );
        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Enter), true),
            DialogAction::Decided(ApprovalChoice::ApproveAndAutoMode)
        );
    }

    #[test]
    fn test_enter_returns_remember_choice_with_rule_text() {
        let mut dialog = shell_dialog(Some("git status"));
        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Char('2')), true),
            DialogAction::Stay
        );
        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Enter), true),
            DialogAction::Decided(ApprovalChoice::ApproveAndRemember("git status".to_owned()))
        );
    }

    // ── n/Esc → Reject, y → Approve ─────────────────────────────────────

    #[test]
    fn test_n_and_esc_reject_without_reason() {
        let mut dialog = shell_dialog(None);
        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Char('n')), true),
            DialogAction::Decided(ApprovalChoice::Reject { reason: None })
        );
        let mut dialog = shell_dialog(None);
        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Esc), true),
            DialogAction::Decided(ApprovalChoice::Reject { reason: None })
        );
    }

    #[test]
    fn test_y_approves_immediately() {
        let mut dialog = shell_dialog(None);
        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Char('y')), true),
            DialogAction::Decided(ApprovalChoice::Approve)
        );
    }

    // ── v → ToggleDetails ────────────────────────────────────────────────

    #[test]
    fn test_v_toggles_details() {
        let mut dialog = shell_dialog(None);
        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Char('v')), true),
            DialogAction::ToggleDetails
        );
        assert!(dialog.expanded);
        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Char('v')), true),
            DialogAction::ToggleDetails
        );
        assert!(!dialog.expanded);
    }

    // ── Arming-Vertrag ───────────────────────────────────────────────────

    #[test]
    fn test_unarmed_keys_never_decide() {
        let mut dialog = shell_dialog(Some("git status"));
        for code in [
            KeyCode::Enter,
            KeyCode::Char('y'),
            KeyCode::Char('n'),
            KeyCode::Esc,
            KeyCode::Char('1'),
            KeyCode::Char('v'),
            KeyCode::Tab,
        ] {
            assert_eq!(dialog.handle_key(make_key(code), false), DialogAction::Stay);
        }
        // Kein Tastendruck darf trotz vieler Versuche eine Entscheidung
        // ausgelöst oder auch nur die Auswahl bewegt haben.
        assert_eq!(dialog.selected, 0);
        assert!(!dialog.expanded);
        assert!(!dialog.reason_editing);
    }

    // ── Countdown ────────────────────────────────────────────────────────

    #[test]
    fn test_format_countdown_formats_five_minutes() {
        assert_eq!(format_countdown(Duration::from_secs(300)), "noch 5:00");
        assert_eq!(format_countdown(Duration::from_secs(292)), "noch 4:52");
        assert_eq!(format_countdown(Duration::from_secs(5)), "noch 0:05");
    }

    #[test]
    fn test_countdown_appears_in_render() {
        let dialog = ApprovalDialog::new(ApprovalDialogRequest {
            call: tool_call(
                "shell.exec",
                harw_tools::serde_json::json!({ "command": "ls" }),
            ),
            cwd: None,
            justification: None,
            risk: None,
            origin: None,
            remember_rule: None,
            deadline: Instant::now() + Duration::from_secs(300),
            reason_input_enabled: false,
        });
        let rendered = render_dialog(&dialog, 70, 20);
        assert!(
            rendered.contains("noch 4:5") || rendered.contains("noch 5:00"),
            "{rendered}"
        );
    }

    // ── Remote-OCR-Hinweis ───────────────────────────────────────────────

    #[test]
    fn test_remote_ocr_risk_names_host_in_render() {
        let call = tool_call(
            "doc.read_pdf",
            harw_tools::serde_json::json!({ "path": "scan.pdf" }),
        );
        let target = harw_registry_defaults::RemoteOcrTarget {
            host: "api.mistral.ai".to_owned(),
            approval: harw_registry_defaults::RemoteOcrApproval::Ask,
        };
        let risk = harw_registry_defaults::remote_ocr_approval_notice_with(&call, Some(&target));
        assert!(risk.is_some());
        let dialog = ApprovalDialog::new(ApprovalDialogRequest {
            call,
            cwd: None,
            justification: None,
            risk,
            origin: None,
            remember_rule: None,
            deadline: Instant::now() + Duration::from_secs(300),
            reason_input_enabled: false,
        });
        let rendered = render_dialog(&dialog, 100, 30);
        assert!(rendered.contains("api.mistral.ai"), "{rendered}");
        assert!(rendered.contains("remote OCR"), "{rendered}");
    }

    // ── Terminal-Sicherheit ──────────────────────────────────────────────

    #[test]
    fn test_hostile_argument_text_is_sanitized() {
        let dialog = ApprovalDialog::new(ApprovalDialogRequest {
            call: tool_call(
                "shell.exec",
                harw_tools::serde_json::json!({
                    "command": "printf '\u{1b}]52;c;ZXZpbA==\u{07}'\nrm -rf /",
                }),
            ),
            cwd: None,
            justification: None,
            risk: None,
            origin: None,
            remember_rule: None,
            deadline: Instant::now() + Duration::from_secs(300),
            reason_input_enabled: false,
        });
        let rendered = render_dialog(&dialog, 70, 20);
        assert!(!rendered.contains('\u{1b}'), "{rendered}");
        assert!(
            rendered.contains("⟨U+001B⟩") || rendered.contains("⟨ESC⟩"),
            "{rendered}"
        );
        // Der Befehl bleibt trotzdem lesbar (nichts wird verschluckt).
        assert!(rendered.contains("rm -rf /"), "{rendered}");
    }

    // ── desired_height wächst mit ausgeklappten Details ─────────────────

    #[test]
    fn test_desired_height_grows_when_expanded() {
        let mut arguments = harw_tools::serde_json::Map::new();
        arguments.insert(
            "command".to_owned(),
            harw_tools::serde_json::Value::String("do-something".to_owned()),
        );
        for i in 0..12 {
            arguments.insert(
                format!("arg_{i}"),
                harw_tools::serde_json::Value::String(format!("wert-nummer-{i}")),
            );
        }
        let mut dialog = ApprovalDialog::new(ApprovalDialogRequest {
            call: tool_call(
                "shell.exec",
                harw_tools::serde_json::Value::Object(arguments),
            ),
            cwd: None,
            justification: None,
            risk: None,
            origin: None,
            remember_rule: None,
            deadline: Instant::now() + Duration::from_secs(300),
            reason_input_enabled: false,
        });

        let collapsed_height = dialog.desired_height(70);
        dialog.handle_key(make_key(KeyCode::Char('v')), true);
        let expanded_height = dialog.desired_height(70);
        assert!(
            expanded_height > collapsed_height,
            "{expanded_height} <= {collapsed_height}"
        );
    }

    // ── Freitext-Ablehnung ───────────────────────────────────────────────

    #[test]
    fn test_reason_input_captures_text_and_enter_rejects_with_reason() {
        let mut dialog = shell_dialog(None);
        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Tab), true),
            DialogAction::Stay
        );
        assert!(dialog.reason_editing);

        for c in "zu riskant".chars() {
            assert_eq!(
                dialog.handle_key(make_key(KeyCode::Char(c)), true),
                DialogAction::Stay
            );
        }
        assert_eq!(dialog.reason_text, "zu riskant");

        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Enter), true),
            DialogAction::Decided(ApprovalChoice::Reject {
                reason: Some("zu riskant".to_owned())
            })
        );
    }

    #[test]
    fn test_reason_input_backspace_and_esc_leaves_without_deciding() {
        let mut dialog = shell_dialog(None);
        dialog.handle_key(make_key(KeyCode::Tab), true);
        dialog.handle_key(make_key(KeyCode::Char('x')), true);
        assert_eq!(dialog.reason_text, "x");
        dialog.handle_key(make_key(KeyCode::Backspace), true);
        assert_eq!(dialog.reason_text, "");

        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Esc), true),
            DialogAction::Stay
        );
        assert!(!dialog.reason_editing);
    }

    /// Runde 6, Teil A1: fragt der Auto-Modus statt abzulehnen, steht sein
    /// Grund als Zeile „Auto-Modus: <Kategorie> – <Grund>“ im Dialog.
    #[test]
    fn test_auto_mode_reason_is_shown_as_a_line() {
        let dialog = shell_dialog(None).with_auto_reason(Some(
            "exfiltration – verschiebt export.md nach ~".to_owned(),
        ));
        let rendered = render_dialog(&dialog, 90, 24);
        assert!(
            rendered.contains("Auto-Modus: exfiltration – verschiebt export.md nach ~"),
            "{rendered}"
        );
        let plain = render_dialog(&shell_dialog(None).with_auto_reason(None), 90, 24);
        assert!(!plain.contains("Auto-Modus:"), "{plain}");
    }

    /// TUI-06 (R18 F2): fiel der Klassifizierer aus, nennt der Dialog das in
    /// Klartext statt der Kategorie `classifier-unavailable`.
    #[test]
    fn test_classifier_fallback_reason_is_readable() {
        let dialog = shell_dialog(None).with_auto_reason(Some(
            "classifier-unavailable – Klassifizierer-Fehler: model returned an empty text response"
                .to_owned(),
        ));
        let rendered = render_dialog(&dialog, 120, 24);
        assert!(
            rendered.contains(
                "Auto-Modus nicht verfügbar: Klassifizierer lieferte leere Antwort – bitte \
                 selbst entscheiden"
            ),
            "{rendered}"
        );
        assert!(!rendered.contains("classifier-unavailable"), "{rendered}");

        assert_eq!(
            classifier_unavailable_cause(
                "classifier-unavailable – Klassifizierer-Modell m lieferte zweimal eine leere \
                 Antwort; kein Ersatzmodell konfiguriert"
            )
            .as_deref(),
            Some(
                "Klassifizierer-Modell m lieferte zweimal eine leere Antwort; kein \
                 Ersatzmodell konfiguriert"
            )
        );
        assert_eq!(
            classifier_unavailable_cause("classifier-unavailable – ").as_deref(),
            Some("Klassifizierer nicht erreichbar")
        );
        assert_eq!(
            classifier_unavailable_cause("shell – unbekannter Befehl"),
            None
        );
    }

    #[test]
    fn test_reason_input_not_offered_when_disabled() {
        let dialog = ApprovalDialog::new(ApprovalDialogRequest {
            call: tool_call(
                "shell.exec",
                harw_tools::serde_json::json!({ "command": "ls" }),
            ),
            cwd: None,
            justification: None,
            risk: None,
            origin: None,
            remember_rule: None,
            deadline: Instant::now() + Duration::from_secs(300),
            reason_input_enabled: false,
        });
        let rendered = render_dialog(&dialog, 70, 20);
        assert!(!rendered.contains("Tab: Grund angeben"), "{rendered}");
    }

    // ── Titel je Werkzeug ────────────────────────────────────────────────

    #[test]
    fn test_title_depends_on_tool_name() {
        let shell = shell_dialog(None);
        assert!(render_dialog(&shell, 70, 20).contains("Befehl ausführen?"));

        let write = ApprovalDialog::new(ApprovalDialogRequest {
            call: tool_call(
                "fs.write",
                harw_tools::serde_json::json!({ "path": "/tmp/x" }),
            ),
            cwd: None,
            justification: None,
            risk: None,
            origin: None,
            remember_rule: None,
            deadline: Instant::now() + Duration::from_secs(300),
            reason_input_enabled: false,
        });
        assert!(render_dialog(&write, 70, 20).contains("Datei schreiben?"));

        // Runde 5, Teil H: `fs.edit` fragt „Datei bearbeiten?“ und zeigt
        // Pfad, alten und neuen Text.
        let edit = ApprovalDialog::new(ApprovalDialogRequest {
            call: tool_call(
                "fs.edit",
                harw_tools::serde_json::json!({
                    "path": "src/lib.rs",
                    "old_string": "alt",
                    "new_string": "neu"
                }),
            ),
            cwd: None,
            justification: None,
            risk: None,
            origin: None,
            remember_rule: None,
            deadline: Instant::now() + Duration::from_secs(300),
            reason_input_enabled: false,
        });
        let rendered = render_dialog(&edit, 70, 24);
        assert!(rendered.contains("Datei bearbeiten?"), "{rendered}");
        assert!(rendered.contains("src/lib.rs"), "{rendered}");
        assert!(rendered.contains("old_string"), "{rendered}");
        assert!(rendered.contains("new_string"), "{rendered}");

        let other = ApprovalDialog::new(ApprovalDialogRequest {
            call: tool_call("mcp.custom", harw_tools::serde_json::json!({})),
            cwd: None,
            justification: None,
            risk: None,
            origin: None,
            remember_rule: None,
            deadline: Instant::now() + Duration::from_secs(300),
            reason_input_enabled: false,
        });
        assert!(render_dialog(&other, 70, 20).contains("Werkzeug freigeben?"));
    }

    // ── Runde 5, Teil E: Lern-Angebot ────────────────────────────────────────

    fn learn_view() -> LearnOfferView {
        LearnOfferView {
            tool: "shell.exec".to_owned(),
            pattern: Some("git status".to_owned()),
            display: "shell.exec git status *".to_owned(),
        }
    }

    #[test]
    fn test_learning_offer_adds_session_and_project_options() {
        let without = shell_dialog(None);
        assert_eq!(without.visible_options().len(), 3);

        let with = shell_dialog(None).with_learning_offer(learn_view());
        assert_eq!(
            with.visible_options(),
            vec![
                OptionKind::Approve,
                OptionKind::LearnSession,
                OptionKind::LearnProject,
                OptionKind::AutoMode,
                OptionKind::Reject,
            ]
        );
        let rendered = render_dialog(&with, 90, 24);
        assert!(
            rendered.contains("künftig erlauben: shell.exec git status *"),
            "{rendered}"
        );
    }

    #[test]
    fn test_learning_offer_choices_carry_the_scope() {
        let mut dialog = shell_dialog(None).with_learning_offer(learn_view());
        dialog.handle_key(make_key(KeyCode::Char('2')), true);
        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Enter), true),
            DialogAction::Decided(ApprovalChoice::ApproveAndLearn {
                offer: learn_view(),
                scope: LearnScope::Session,
            })
        );

        let mut dialog = shell_dialog(None).with_learning_offer(learn_view());
        dialog.handle_key(make_key(KeyCode::Char('3')), true);
        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Enter), true),
            DialogAction::Decided(ApprovalChoice::ApproveAndLearn {
                offer: learn_view(),
                scope: LearnScope::Project,
            })
        );
    }

    // ── Kleine Fenster: Optionen angeheftet, Körper scrollt ─────────────

    /// Ein Dialog mit langem Befehl und allen Info-Zeilen (wie im
    /// Screenshot: cwd, angefragt von, Auto-Modus-Grund, Risiko).
    fn crowded_dialog() -> ApprovalDialog {
        let command = (0..12)
            .map(|i| format!("schritt-{i} --mit-langem-argument"))
            .collect::<Vec<_>>()
            .join(" && ");
        ApprovalDialog::new(ApprovalDialogRequest {
            call: tool_call(
                "shell.exec",
                harw_tools::serde_json::json!({ "command": command }),
            ),
            cwd: Some("/home/u/ein/ziemlich/langes/projekt/verzeichnis".to_owned()),
            justification: Some("baut und prüft das Projekt".to_owned()),
            risk: Some("mittel — schreibt in target/".to_owned()),
            origin: Some("uia-worker (uia › root-orchestrator › uia-worker)".to_owned()),
            remember_rule: None,
            deadline: Instant::now() + Duration::from_secs(300),
            reason_input_enabled: true,
        })
        .with_auto_reason(Some("shell – unbekannter Befehl".to_owned()))
    }

    fn rows_of(rendered: &str) -> Vec<&str> {
        rendered.lines().collect()
    }

    #[test]
    fn test_small_panels_keep_options_and_hint_fully_visible() {
        let dialog = crowded_dialog();
        for (width, height) in [(40u16, 10u16), (60, 12), (80, 14), (120, 14)] {
            let rendered = render_dialog(&dialog, width, height);
            assert!(rendered.contains("1. Ja"), "{width}x{height}: {rendered}");
            assert!(
                rendered.contains("Nein (Esc)"),
                "{width}x{height}: {rendered}"
            );
            let inner = width - 2;
            let hint = dialog_frame::pick_fitting(FOOTER_HINTS, inner);
            assert!(
                rows_of(&rendered).iter().any(|row| row.contains(hint)),
                "{width}x{height}: Hinweis {hint:?} fehlt: {rendered}"
            );
            // Die erste Zeile verdichtet den Befehl („… v Details“).
            assert!(
                rendered.contains(DETAILS_MARKER),
                "{width}x{height}: {rendered}"
            );
            // Nichts ragt über den Rahmen hinaus.
            for row in rows_of(&rendered) {
                assert_eq!(row.chars().count(), usize::from(width), "{row}");
            }
        }
    }

    #[test]
    fn test_body_scrolls_by_ctrl_arrows_without_changing_the_selection() {
        let mut dialog = crowded_dialog();
        dialog.handle_key(make_key(KeyCode::Down), true);
        assert_eq!(dialog.selected, 1);
        let before = render_dialog(&dialog, 60, 12);
        assert_eq!(dialog.body_offset(), 0);
        let ctrl_down = KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL);
        assert_eq!(dialog.handle_key(ctrl_down, true), DialogAction::Scrolled);
        assert_eq!(dialog.handle_key(ctrl_down, true), DialogAction::Scrolled);
        assert_eq!(dialog.selected, 1, "Scrollen ändert die Auswahl nicht");
        assert_eq!(dialog.body_offset(), 2);
        let after = render_dialog(&dialog, 60, 12);
        assert_ne!(before, after, "der Körper hat sich bewegt");
        assert!(after.contains("1. Ja"), "{after}");
        assert!(after.contains("❯ 2."), "Auswahl bleibt sichtbar: {after}");
        let ctrl_up = KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL);
        assert_eq!(dialog.handle_key(ctrl_up, true), DialogAction::Scrolled);
        assert_eq!(dialog.body_offset(), 1);
        assert_eq!(dialog.selected, 1);
    }

    #[test]
    fn test_mouse_wheel_scrolls_the_body_and_options_stay() {
        let dialog = crowded_dialog();
        let top = render_dialog(&dialog, 60, 12);
        assert!(dialog.scroll_wheel(crossterm::event::MouseEventKind::ScrollDown));
        assert_eq!(dialog.body_offset(), dialog_frame::WHEEL_LINES);
        let scrolled = render_dialog(&dialog, 60, 12);
        assert_ne!(top, scrolled);
        for needle in ["1. Ja", "Nein (Esc)"] {
            assert!(scrolled.contains(needle), "{needle}: {scrolled}");
        }
    }

    #[test]
    fn test_expanded_details_scroll_to_the_full_command() {
        let mut dialog = crowded_dialog();
        dialog.handle_key(make_key(KeyCode::Char('v')), true);
        let rendered = render_dialog(&dialog, 60, 12);
        assert!(!rendered.contains(DETAILS_MARKER), "{rendered}");
        // Bis ans Ende scrollen: der letzte Befehlsteil wird erreichbar.
        for _ in 0..40 {
            dialog.scroll_wheel(crossterm::event::MouseEventKind::ScrollDown);
            let _ = render_dialog(&dialog, 60, 12);
        }
        let end = render_dialog(&dialog, 60, 12);
        assert!(end.contains("Begründung"), "{end}");
        assert!(end.contains("1. Ja"), "{end}");
    }

    #[test]
    fn test_compact_preview_fits_the_width() {
        let preview = compact_preview("git status --short && cargo build", 20);
        assert!(preview.ends_with(DETAILS_MARKER), "{preview}");
        assert!(unicode_width::UnicodeWidthStr::width(preview.as_str()) <= 20);
    }
}
