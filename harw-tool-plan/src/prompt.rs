//! Fragekanal zwischen den Plan-Werkzeugen und der TUI (Runde 5, Teil F).
//!
//! # Beschreibung
//! `plan.exit`, `plan.enter` und `ask_user` brauchen eine anwesende Person.
//! Jede Frage reist als [`PlanUiRequest`] über einen `mpsc`-Kanal zur TUI
//! (`harw_tui::plan_dialog`/`harw_tui::ask_user_dialog`) und trägt die
//! Antwortseite als `oneshot`. Muster wie `host.sudo_exec`
//! (`harw_tool_shell::sudo`): **ein Drop ohne Antwort ist eine Ablehnung**
//! bzw. ein Abbruch — nie eine Zustimmung.
//!
//! Den Kanal baut die Montage (`harw-runtime`) ausschließlich für
//! `EntryKind::Tui`. Jeder andere Einstieg hat keinen Sender; die Werkzeuge
//! antworten dann fail-closed mit einer klaren Meldung.
//!
//! # Nebenläufigkeit
//! Die Fragen sind `Send`, nicht `Sync`, und werden beim Beantworten
//! konsumiert. [`PlanUiSender`] ist ein `mpsc::UnboundedSender` (`Clone`).

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::ask_user::AskQuestion;

/// Sendeseite des Plan-Fragekanals.
pub type PlanUiSender = mpsc::UnboundedSender<PlanUiRequest>;

/// Empfängerseite des Plan-Fragekanals (pollt die TUI).
pub type PlanUiReceiver = mpsc::UnboundedReceiver<PlanUiRequest>;

/// Baut einen frischen Plan-Fragekanal.
#[must_use]
pub fn plan_ui_channel() -> (PlanUiSender, PlanUiReceiver) {
    mpsc::unbounded_channel()
}

/// Eine Frage an die TUI.
#[derive(Debug)]
pub enum PlanUiRequest {
    /// `ask_user`: Auswahlfenster mit 1–4 Fragen.
    AskUser(AskUserPrompt),
    /// `plan.exit`: Freigabefenster für einen fertigen Plan.
    ExitPlan(PlanExitPrompt),
    /// `plan.enter`: Vorschlag, in den Plan-Modus zu wechseln.
    EnterPlan(PlanEnterPrompt),
    /// Runde 5, Teil P: `plan`-Operation — einen Plan-Graphen bestätigen
    /// (neuer Plan oder wesentliche Änderung), siehe [`crate::confirm`].
    ConfirmPlan(crate::confirm::PlanConfirmPrompt),
}

// ── ask_user ─────────────────────────────────────────────────────────────────

/// Die Antwort auf **eine** Frage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionAnswer {
    /// Der Fragetext (zur Zuordnung im Werkzeugergebnis).
    pub question: String,
    /// Die gewählten Optionen (Labels), bei Einzelauswahl höchstens eine.
    pub selected: Vec<String>,
    /// Freitext aus „Andere“, falls gewählt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other: Option<String>,
}

/// Die vollständige Antwort der Nutzerin auf ein `ask_user`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskUserAnswer {
    /// Eine Antwort je Frage, in Fragereihenfolge.
    pub answers: Vec<QuestionAnswer>,
}

/// Eine `ask_user`-Frage auf dem Weg zum Auswahlfenster.
pub struct AskUserPrompt {
    questions: Vec<AskQuestion>,
    responder: oneshot::Sender<AskUserAnswer>,
}

impl fmt::Debug for AskUserPrompt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AskUserPrompt")
            .field("questions", &self.questions)
            .finish_non_exhaustive()
    }
}

impl AskUserPrompt {
    /// Baut eine Frage plus die wartende Seite.
    #[must_use]
    pub fn new(questions: Vec<AskQuestion>) -> (Self, oneshot::Receiver<AskUserAnswer>) {
        let (responder, answer) = oneshot::channel();
        (
            Self {
                questions,
                responder,
            },
            answer,
        )
    }

    /// Die (bereits geprüften) Fragen.
    #[must_use]
    pub fn questions(&self) -> &[AskQuestion] {
        &self.questions
    }

    /// Beantwortet die Frage.
    ///
    /// # Returns
    /// `true`, wenn die Antwort das wartende Werkzeug erreicht hat.
    pub fn answer(self, answer: AskUserAnswer) -> bool {
        self.responder.send(answer).is_ok()
    }

    /// Bricht die Frage ab (Esc, Turn-Ende). Das Werkzeug meldet dem Modell
    /// dann, dass keine Antwort kam.
    pub fn cancel(self) {
        drop(self);
    }
}

// ── plan.exit ────────────────────────────────────────────────────────────────

/// Die Entscheidung der Nutzerin im Freigabefenster von `plan.exit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanExitDecision {
    /// Option 1: umsetzen im Auto-Modus — Modus `work`, Freigabe `auto`
    /// (`ApprovalMode::Delegated`).
    ImplementAuto,
    /// Option 2: umsetzen und Änderungen einzeln freigeben — Modus `work`,
    /// Freigabe `ask` (`ApprovalMode::AlwaysAsk`).
    ImplementAsk,
    /// Option 3: weiter planen, mit Rückmeldung an den Agenten.
    KeepPlanning {
        /// Freitext der Nutzerin (darf leer sein).
        feedback: String,
    },
}

/// Eine `plan.exit`-Frage auf dem Weg zum Freigabefenster.
pub struct PlanExitPrompt {
    slug: String,
    path: PathBuf,
    display_path: String,
    content: String,
    responder: oneshot::Sender<PlanExitDecision>,
}

impl fmt::Debug for PlanExitPrompt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlanExitPrompt")
            .field("slug", &self.slug)
            .field("display_path", &self.display_path)
            .field("bytes", &self.content.len())
            .finish_non_exhaustive()
    }
}

impl PlanExitPrompt {
    /// Baut eine Frage plus die wartende Seite.
    #[must_use]
    pub fn new(
        slug: String,
        path: PathBuf,
        content: String,
    ) -> (Self, oneshot::Receiver<PlanExitDecision>) {
        let (responder, decision) = oneshot::channel();
        let display_path = crate::plan_file::display_path(&slug);
        (
            Self {
                slug,
                path,
                display_path,
                content,
                responder,
            },
            decision,
        )
    }

    /// Der Slug des Plans.
    #[must_use]
    pub fn slug(&self) -> &str {
        &self.slug
    }

    /// Absoluter Pfad der Plan-Datei.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Anzeigepfad (`.harw/plans/<slug>.md`).
    #[must_use]
    pub fn display_path(&self) -> &str {
        &self.display_path
    }

    /// Der Planinhalt (Markdown, nicht vertrauenswürdig).
    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }

    /// `true`, wenn der Plan leer ist (nur Leerraum) — das Fenster warnt.
    #[must_use]
    pub fn is_empty_plan(&self) -> bool {
        self.content.trim().is_empty()
    }

    /// Beantwortet die Frage.
    ///
    /// # Returns
    /// `true`, wenn die Entscheidung das wartende Werkzeug erreicht hat.
    pub fn decide(self, decision: PlanExitDecision) -> bool {
        self.responder.send(decision).is_ok()
    }
}

// ── plan.enter ───────────────────────────────────────────────────────────────

/// Eine `plan.enter`-Frage: der Agent schlägt den Plan-Modus vor.
pub struct PlanEnterPrompt {
    reason: String,
    responder: oneshot::Sender<bool>,
}

impl fmt::Debug for PlanEnterPrompt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlanEnterPrompt")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

impl PlanEnterPrompt {
    /// Baut eine Frage plus die wartende Seite.
    #[must_use]
    pub fn new(reason: String) -> (Self, oneshot::Receiver<bool>) {
        let (responder, answer) = oneshot::channel();
        (Self { reason, responder }, answer)
    }

    /// Der Grund laut Modell (Anzeige).
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// Beantwortet die Frage (`true` = in den Plan-Modus wechseln).
    ///
    /// # Returns
    /// `true`, wenn die Antwort das wartende Werkzeug erreicht hat.
    pub fn answer(self, accept: bool) -> bool {
        self.responder.send(accept).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    #[tokio::test]
    async fn dropping_a_prompt_never_counts_as_consent() -> TestResult {
        let (prompt, answer) = PlanEnterPrompt::new("groß".to_owned());
        drop(prompt);
        assert!(answer.await.is_err(), "Drop ist keine Zustimmung");

        let (prompt, decision) =
            PlanExitPrompt::new("p".to_owned(), PathBuf::from("/x/p.md"), "x".to_owned());
        drop(prompt);
        assert!(decision.await.is_err());

        let (prompt, answer) = AskUserPrompt::new(Vec::new());
        prompt.cancel();
        assert!(answer.await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn answers_reach_the_waiting_side() -> TestResult {
        let (prompt, decision) =
            PlanExitPrompt::new("p".to_owned(), PathBuf::from("/x/p.md"), " \n".to_owned());
        assert!(prompt.is_empty_plan());
        assert_eq!(prompt.display_path(), ".harw/plans/p.md");
        assert!(prompt.decide(PlanExitDecision::ImplementAsk));
        let received = decision
            .await
            .map_err(|_| TestError::Missing("Entscheidung"))?;
        assert_eq!(received, PlanExitDecision::ImplementAsk);
        Ok(())
    }
}
