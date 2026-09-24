//! Bestätigung eines Plan-Graphen im Freigabefenster (Runde 5, Teil P).
//!
//! # Beschreibung
//! Legt ein Modell über das `plan`-Werkzeug einen Plan an (`plan submit`
//! nach dem Entwurf) oder ändert einen bestätigten Plan wesentlich (Knoten
//! hinzufügen/zerlegen/verdichten/ablösen), fragt die `plan`-Operation die
//! Nutzerin. Die Frage reist als [`PlanUiRequest::ConfirmPlan`] über
//! denselben Kanal wie `plan.exit` und öffnet dasselbe Freigabefenster
//! (`harw_tui::plan_dialog::PlanExitDialog`) in der Variante „Plan
//! bestätigen“:
//!
//! 1. Ja, bestätigen — der Plan wird aktiv bzw. die Änderung übernommen.
//! 2. Nein — mit Rückmeldung an den Agenten.
//!
//! Eine dritte Option „bearbeiten“ gibt es bewusst nicht: der Plan-Graph ist
//! kein Markdown-Dokument, eine im `$EDITOR` geänderte Darstellung ließe
//! sich nicht verlustfrei zurücklesen.
//!
//! Die Operation findet den Kanal als Dienst [`PlanConfirmChannel`] im
//! `OpContext`; die Montage legt ihn **nur** für `EntryKind::Tui` ab. Ohne
//! Kanal (Gateway, One-Shot, Jobs) antwortet [`PlanConfirmChannel`] gar
//! nicht erst — die Operation legt den Plan dann als `proposed` an und
//! meldet das.
//!
//! # Sicherheitsregel
//! Wie überall im Plan-Kanal: **ein Drop ohne Antwort ist nie eine
//! Zustimmung** ([`PlanConfirmOutcome::NoAnswer`]).
//!
//! # Nebenläufigkeit
//! [`PlanConfirmChannel`] ist `Clone + Send + Sync`; die Frage selbst ist
//! `Send` und wird beim Beantworten konsumiert.

use std::fmt;
use std::time::Duration;

use harw_types::cancel::CancelToken;
use tokio::sync::oneshot;

use crate::prompt::{PlanUiRequest, PlanUiSender};

/// Wartezeit auf die Bestätigung eines Plans (wie `plan.exit`).
pub const PLAN_CONFIRM_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Worum es in der Bestätigung geht.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanConfirmKind {
    /// Ein neuer Plan (Entwurf) soll aktiv werden.
    NewPlan,
    /// Ein bestätigter Plan soll wesentlich geändert werden.
    Change,
}

/// Die Entscheidung der Nutzerin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanConfirmDecision {
    /// Option 1: bestätigen.
    Confirm,
    /// Option 2: ablehnen, mit Rückmeldung an den Agenten.
    Reject {
        /// Freitext der Nutzerin (darf leer sein).
        feedback: String,
    },
}

/// Eine Bestätigungsfrage auf dem Weg zum Freigabefenster.
pub struct PlanConfirmPrompt {
    plan_id: String,
    kind: PlanConfirmKind,
    summary: String,
    content: String,
    responder: oneshot::Sender<PlanConfirmDecision>,
}

impl fmt::Debug for PlanConfirmPrompt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlanConfirmPrompt")
            .field("plan_id", &self.plan_id)
            .field("kind", &self.kind)
            .field("bytes", &self.content.len())
            .finish_non_exhaustive()
    }
}

impl PlanConfirmPrompt {
    /// Baut eine Frage plus die wartende Seite.
    ///
    /// # Arguments
    /// - `plan_id`: Bezeichner des Plans (Anzeige).
    /// - `kind`: neuer Plan oder Änderung.
    /// - `summary`: eine Zeile, was bestätigt werden soll.
    /// - `content`: der gerenderte Plan als Markdown (Modelltext, nicht
    ///   vertrauenswürdig — die TUI bereinigt beim Rendern).
    #[must_use]
    pub fn new(
        plan_id: String,
        kind: PlanConfirmKind,
        summary: String,
        content: String,
    ) -> (Self, oneshot::Receiver<PlanConfirmDecision>) {
        let (responder, decision) = oneshot::channel();
        (
            Self {
                plan_id,
                kind,
                summary,
                content,
                responder,
            },
            decision,
        )
    }

    /// Bezeichner des Plans.
    #[must_use]
    pub fn plan_id(&self) -> &str {
        &self.plan_id
    }

    /// Neuer Plan oder Änderung.
    #[must_use]
    pub fn kind(&self) -> PlanConfirmKind {
        self.kind
    }

    /// Eine Zeile, was bestätigt werden soll.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }

    /// Der gerenderte Plan (Markdown).
    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Beantwortet die Frage.
    ///
    /// # Returns
    /// `true`, wenn die Entscheidung die wartende Operation erreicht hat.
    pub fn decide(self, decision: PlanConfirmDecision) -> bool {
        self.responder.send(decision).is_ok()
    }
}

/// Ergebnis einer Bestätigungsfrage aus Sicht der Operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanConfirmOutcome {
    /// Bestätigt.
    Confirmed,
    /// Abgelehnt, mit Rückmeldung.
    Rejected {
        /// Rückmeldung der Nutzerin (getrimmt, darf leer sein).
        feedback: String,
    },
    /// Keine Entscheidung (Fenster geschlossen, Zeitablauf, TUI weg).
    NoAnswer,
    /// Der Turn wurde abgebrochen.
    Cancelled,
}

/// Der Fragekanal als Dienst für die `plan`-Operation.
///
/// # Beschreibung
/// Dünne Hülle um den [`PlanUiSender`] der TUI-Montage. Als eigener Typ,
/// damit die `ServiceMap` (Schlüssel = Rust-Typ) ihn eindeutig findet und
/// kein anderer Sender versehentlich als Bestätigungskanal gilt.
#[derive(Debug, Clone)]
pub struct PlanConfirmChannel {
    sender: PlanUiSender,
    timeout: Duration,
}

impl PlanConfirmChannel {
    /// Baut den Dienst über dem Sender der TUI-Montage.
    #[must_use]
    pub fn new(sender: PlanUiSender) -> Self {
        Self {
            sender,
            timeout: PLAN_CONFIRM_TIMEOUT,
        }
    }

    /// Setzt eine andere Wartezeit (Tests).
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Fragt die Nutzerin und wartet auf die Entscheidung.
    ///
    /// # Arguments
    /// - `plan_id`, `kind`, `summary`, `content`: siehe [`PlanConfirmPrompt::new`].
    /// - `cancel`: Turn-Abbruch; `None` = nie abgebrochen.
    ///
    /// # Returns
    /// [`PlanConfirmOutcome`]; ein nicht zustellbarer Kanal ist
    /// [`PlanConfirmOutcome::NoAnswer`] — nie eine Zustimmung.
    pub async fn ask(
        &self,
        plan_id: &str,
        kind: PlanConfirmKind,
        summary: &str,
        content: String,
        cancel: Option<&CancelToken>,
    ) -> PlanConfirmOutcome {
        let (prompt, decision) =
            PlanConfirmPrompt::new(plan_id.to_owned(), kind, summary.to_owned(), content);
        if self
            .sender
            .send(PlanUiRequest::ConfirmPlan(prompt))
            .is_err()
        {
            return PlanConfirmOutcome::NoAnswer;
        }
        let cancelled = async {
            match cancel {
                Some(cancel) => cancel.cancelled().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            waited = tokio::time::timeout(self.timeout, decision) => match waited {
                Ok(Ok(PlanConfirmDecision::Confirm)) => PlanConfirmOutcome::Confirmed,
                Ok(Ok(PlanConfirmDecision::Reject { feedback })) => PlanConfirmOutcome::Rejected {
                    feedback: feedback.trim().to_owned(),
                },
                Ok(Err(_)) | Err(_) => PlanConfirmOutcome::NoAnswer,
            },
            () = cancelled => PlanConfirmOutcome::Cancelled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::plan_ui_channel;
    use crate::test_support::{TestError, TestResult};

    #[tokio::test]
    async fn confirmation_reaches_the_waiting_operation() -> TestResult {
        let (sender, mut receiver) = plan_ui_channel();
        let channel = PlanConfirmChannel::new(sender);
        let asking = channel.ask(
            "p-1",
            PlanConfirmKind::NewPlan,
            "Neuer Plan",
            "# Plan p-1".to_owned(),
            None,
        );
        let answering = async {
            match receiver.recv().await {
                Some(PlanUiRequest::ConfirmPlan(prompt)) => {
                    assert_eq!(prompt.plan_id(), "p-1");
                    assert_eq!(prompt.kind(), PlanConfirmKind::NewPlan);
                    assert!(prompt.content().contains("# Plan p-1"));
                    Ok(prompt.decide(PlanConfirmDecision::Confirm))
                }
                _ => Err(TestError::Missing("ConfirmPlan-Frage")),
            }
        };
        let (outcome, delivered) = tokio::join!(asking, answering);
        assert!(delivered?);
        assert_eq!(outcome, PlanConfirmOutcome::Confirmed);
        Ok(())
    }

    #[tokio::test]
    async fn rejection_carries_the_feedback() -> TestResult {
        let (sender, mut receiver) = plan_ui_channel();
        let channel = PlanConfirmChannel::new(sender);
        let asking = channel.ask(
            "p-1",
            PlanConfirmKind::Change,
            "Knoten t-3 hinzufügen",
            "# Plan".to_owned(),
            None,
        );
        let answering = async {
            if let Some(PlanUiRequest::ConfirmPlan(prompt)) = receiver.recv().await {
                prompt.decide(PlanConfirmDecision::Reject {
                    feedback: "  erst Tests schreiben ".to_owned(),
                });
            }
        };
        let (outcome, ()) = tokio::join!(asking, answering);
        assert_eq!(
            outcome,
            PlanConfirmOutcome::Rejected {
                feedback: "erst Tests schreiben".to_owned()
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_dropped_prompt_or_closed_channel_is_never_consent() -> TestResult {
        let (sender, mut receiver) = plan_ui_channel();
        let channel = PlanConfirmChannel::new(sender);
        let asking = channel.ask("p-1", PlanConfirmKind::NewPlan, "x", String::new(), None);
        let dropping = async {
            drop(receiver.recv().await);
        };
        let (outcome, ()) = tokio::join!(asking, dropping);
        assert_eq!(outcome, PlanConfirmOutcome::NoAnswer);

        let (sender, receiver) = plan_ui_channel();
        drop(receiver);
        let closed = PlanConfirmChannel::new(sender)
            .ask("p-1", PlanConfirmKind::NewPlan, "x", String::new(), None)
            .await;
        assert_eq!(closed, PlanConfirmOutcome::NoAnswer);
        Ok(())
    }
}
