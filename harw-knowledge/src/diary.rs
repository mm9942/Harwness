//! Typed diary entries; persistence stays in [`KnowledgeStore`](crate::KnowledgeStore).

use jiff::Timestamp;

use crate::visibility::{AgentId, VisibilityScope};

/// Why a diary entry was appended rather than edited in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiaryTrigger {
    EndOfSession,
    Compaction,
    DreamReflection,
    Manual,
}

impl DiaryTrigger {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::EndOfSession => "end-of-session",
            Self::Compaction => "compaction",
            Self::DreamReflection => "dream-reflection",
            Self::Manual => "manual",
        }
    }
}

/// One immutable append in a per-agent, per-day diary file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiaryEntry {
    pub agent_id: AgentId,
    pub recorded_at: Timestamp,
    pub trigger: DiaryTrigger,
    pub visibility: VisibilityScope,
    pub body: String,
}

impl DiaryEntry {
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "### {} — {}\n\n{}\n",
            self.recorded_at.strftime("%H:%M:%S"),
            self.trigger.label(),
            self.body
        )
    }
}
