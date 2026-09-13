//! Typed board, lane, and card state. Lifecycle execution remains in
//! `harw-job-runtime`; these types are its auditable knowledge projection.

use serde::{Deserialize, Serialize};

use harw_job_runtime::WorkId;

use crate::visibility::{AgentId, AgentRoleRef, VisibilityScope};

id_newtype!(BoardId);
id_newtype!(LaneId);
id_newtype!(CardId);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LaneKind {
    Status(CardState),
    Worker { agent_role: AgentRoleRef },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardState {
    Triage,
    Todo,
    Ready,
    Running,
    Blocked { reason_kind: BlockKind },
    Done,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Dependency,
    NeedsInput,
    Capability,
    Transient,
    ReviewRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Board {
    pub id: BoardId,
    pub name: String,
    pub lanes: Vec<LaneId>,
    pub visibility: VisibilityScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lane {
    pub id: LaneId,
    pub board_id: BoardId,
    pub kind: LaneKind,
    pub bound_worker: Option<AgentId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    pub id: CardId,
    pub lane_id: LaneId,
    pub title: String,
    pub body: String,
    pub work_id: Option<WorkId>,
    pub state: CardState,
    pub parents: Vec<CardId>,
    pub assignee: Option<AgentId>,
    pub tags: Vec<String>,
    pub visibility: VisibilityScope,
}

impl Card {
    /// A card with unresolved parent work must not enter the ready queue.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self.state, CardState::Done | CardState::Archived)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_cards_are_done_or_archived_only() {
        let card = Card {
            id: CardId::new("card-1"),
            lane_id: LaneId::new("done"),
            title: "verify".to_owned(),
            body: String::new(),
            work_id: None,
            state: CardState::Done,
            parents: Vec::new(),
            assignee: None,
            tags: Vec::new(),
            visibility: VisibilityScope::SelfOnly,
        };
        assert!(card.is_terminal());
    }
}
