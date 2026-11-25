//! Public turn input, outcome and approval contracts.

use super::*;

/// Eingabe, mit der ein Turn gestartet wird.
#[derive(Debug, Clone, Default)]
pub struct TurnInput {
    /// Optionaler User-Text, der zu Beginn in den Verlauf gespielt wird.
    pub user_text: Option<String>,
    /// Gespeicherte Bilder, die mit dem User-Text in dieselbe Nachricht gehen
    /// (siehe `harw-media`). Leer = kein Bild.
    pub user_images: Vec<harw_protocol::MediaRef>,
    /// Metadaten, die dem `TurnInputContext` mitgegeben werden.
    pub metadata: serde_json::Value,
    /// Steuerblock (Abbruch, Grenzwerte, Uhr, Zähler) dieses Turns.
    pub control: TurnControl,
    /// Id einer früheren User-Nachricht: der Verlauf wird vor dieser Nachricht abgeschnitten (und
    /// dauerhaft ersetzt), bevor der Turn läuft. So ersetzt "Neu senden" die Nachricht, statt sie zu doppeln.
    pub rewind_before: Option<String>,
}

impl TurnInput {
    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            user_text: Some(text.into()),
            user_images: Vec::new(),
            metadata: serde_json::Value::Null,
            control: TurnControl::new(),
            rewind_before: None,
        }
    }

    /// Schneidet den Verlauf vor der User-Nachricht `item_id` ab, bevor der Turn läuft.
    #[must_use]
    pub fn with_rewind_before(mut self, item_id: Option<String>) -> Self {
        self.rewind_before = item_id;
        self
    }

    /// Hängt gespeicherte Bilder an die User-Nachricht dieses Turns. Die
    /// Bilder müssen im Medienspeicher liegen, den die Laufzeit mit
    /// [`crate::install_media_source`] eingerichtet hat; sonst nennt der
    /// Provider sie im Text als nicht gesendet.
    #[must_use]
    pub fn with_images(
        mut self,
        images: impl IntoIterator<Item = harw_protocol::MediaRef>,
    ) -> Self {
        self.user_images.extend(images);
        self
    }

    /// Replaces the control block of this input.
    ///
    /// # Arguments
    /// - `control` (`TurnControl`): keep a clone to pass to `resume_*` later.
    #[must_use]
    pub fn with_control(mut self, control: TurnControl) -> Self {
        self.control = control;
        self
    }
}

/// Wie ein Turn endete.
#[derive(Debug)]
pub enum TurnOutcome {
    /// Turn vollständig abgeschlossen (keine Tool-Calls mehr).
    Completed,
    /// A child is paused on a handoff and the session waits for its result;
    /// resume via `resume_after_child`.
    ///
    /// Delegation tools (`transfer_to_*`, `agents.delegate`) NEVER produce this
    /// outcome any more: delegation is background-only and a start is a
    /// one-time return (see `background_only_refusal`). The variant is kept
    /// only because persisted sessions, `resume_after_child` and the
    /// host/bridge crates still match on it; it is no longer a delegation mode.
    AwaitingChild {
        child: SessionId,
        call_id: ToolCallId,
        role: String,
    },
    /// Ein Guardrail verlangt eine Nutzer-Entscheidung (`AskUser`). Der Turn
    /// pausiert, bis die Entscheidung über den Channel zurückkommt.
    AwaitingApproval {
        call_id: ToolCallId,
        request: harw_types::ItemId,
    },
    /// Der Turn wurde an einem Prüfpunkt abgebrochen: über den
    /// [`CancelToken`] (`reason` = dessen Grund) oder weil eine
    /// [`TurnLimits`]-Grenze erreicht war (`CancelReason::Budget`). Offene
    /// Tool-Calls haben ein synthetisches Fehlerergebnis; die Session ist `Idle`.
    Cancelled { reason: CancelReason },
    /// Die Modellausgabe wurde abgeschnitten (`StopReason::MaxTokens` oder
    /// `ContextWindowExceeded`). Tool-Calls der Antwort wurden nicht ausgeführt;
    /// die Session ist `Idle`.
    Truncated,
    /// Das Modell hat abgelehnt (`StopReason::Refusal` oder `ContentFilter`).
    /// Tool-Calls der Antwort wurden nicht ausgeführt; die Session ist `Idle`.
    Refused { detail: Option<String> },
    /// Eine angenommene Wiederaufnahme konnte nicht fortgesetzt werden (bzw.
    /// die dauerhafte Freigabe ist abgelaufen/defekt). Die Session ist `Failed`.
    Failed { reason: String },
}

/// Resolution supplied by the user for a paused approval request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalResolution {
    Approve,
    Reject { reason: String },
}

impl From<ReviewDecision> for ApprovalResolution {
    fn from(value: ReviewDecision) -> Self {
        match value {
            ReviewDecision::Approved | ReviewDecision::ApprovedOnce => Self::Approve,
            ReviewDecision::Rejected => Self::Reject {
                reason: "rejected by user".to_owned(),
            },
        }
    }
}

impl ApprovalResolution {
    /// Die eine Ablehnung, die jede Oberfläche liefert, sobald
    /// [`crate::session::PendingApproval::timeout_at`] erreicht ist, ohne
    /// dass eine ausdrückliche Entscheidung eintraf (Interaktionsvertrag
    /// §4.4). Eine gemeinsame Konstruktionsstelle statt einer Kopie der
    /// Begründung je Front-End.
    ///
    /// # Returns
    /// [`Self::Reject`] mit [`crate::session::APPROVAL_TIMEOUT_REASON`].
    #[must_use]
    pub fn timed_out() -> Self {
        Self::Reject {
            reason: crate::session::APPROVAL_TIMEOUT_REASON.to_owned(),
        }
    }
}
