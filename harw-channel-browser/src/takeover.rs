/// Ownership state for outbound activity in a browser-backed conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TakeoverState {
    AutomationActive,
    SharedDraft,
    HumanActive,
    Paused,
    Escalated,
}

/// A fact or policy action that changes conversation ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TakeoverEvent {
    HumanActivityDetected,
    EnterSharedDraft,
    Pause,
    ExplicitResume,
    Escalate,
}

/// Whether the connector may send without a human action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutonomousSend {
    Allowed,
    Blocked,
}

impl TakeoverState {
    #[must_use]
    pub const fn autonomous_send(self) -> AutonomousSend {
        match self {
            Self::AutomationActive => AutonomousSend::Allowed,
            Self::SharedDraft | Self::HumanActive | Self::Paused | Self::Escalated => {
                AutonomousSend::Blocked
            }
        }
    }

    #[must_use]
    pub const fn transition(self, event: TakeoverEvent) -> Self {
        match (self, event) {
            (_, TakeoverEvent::ExplicitResume) => Self::AutomationActive,
            (_, TakeoverEvent::Escalate) => Self::Escalated,
            (Self::Escalated, _) => Self::Escalated,
            (_, TakeoverEvent::HumanActivityDetected) => Self::HumanActive,
            (_, TakeoverEvent::EnterSharedDraft) => Self::SharedDraft,
            (_, TakeoverEvent::Pause) => Self::Paused,
        }
    }
}
