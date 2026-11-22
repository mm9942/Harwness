use std::collections::{HashMap, HashSet};

use harw_mobile_core::{EntryKind, SessionView, ToolState};
use harw_protocol::items::{ContentPart, ToolCallItem, ToolCallResult, TurnItem, UserMessageItem};
use harw_protocol::session_wire::{
    AttachAck, ClientCaps, Cursor, FrameEnvelope, HostedState, SessionSummary,
};
use harw_protocol::{ApprovalRequest, SessionEvent, SessionFrame, TurnEvent};
use harw_types::{ApprovalId, ItemId, ReviewDecision, RiskLevel, SessionId, TurnId};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::mission::Mission;
use crate::{
    ApprovalDecision, ApprovalRisk, MobileApproval, MobileError, MobileSession, MobileSessionState,
    MobileToolState, SessionSnapshot, TranscriptEntry, TranscriptKind,
};

const MAX_LIVE_TEXT_BYTES: usize = 1024 * 1024;

#[derive(Default)]
pub(crate) struct SessionState {
    view: SessionView,
    mission: Mission,
    pending: HashMap<ApprovalId, ApprovalRequest>,
    deciding: HashSet<ApprovalId>,
    partial: Option<PartialText>,
    running_turn: Option<TurnId>,
    head: Option<Cursor>,
    replay_target: Option<Cursor>,
    host_epoch: Option<u64>,
    granted: ClientCaps,
    ready: bool,
    readonly: bool,
    closed: bool,
    last_sync_millis: Option<i64>,
    detail: Option<String>,
    server_error: Option<String>,
    pub worker: Option<SessionWorker>,
}

pub(crate) struct SessionWorker {
    pub stop: CancellationToken,
    pub task: JoinHandle<()>,
}

struct PartialText {
    turn_id: TurnId,
    text: String,
    first_unseen_live: u32,
}

pub(crate) enum FrameAction {
    Continue,
    Reattach(Option<Cursor>),
    RefreshTranscript,
    Revoked,
    Draining(u64),
    Closed,
}

impl SessionState {
    pub fn resume(&mut self, host_epoch: u64) -> Option<Cursor> {
        if self.host_epoch != Some(host_epoch) {
            self.reset_view();
        }
        self.host_epoch = Some(host_epoch);
        self.head
    }

    pub fn begin_attach(&mut self, detail: &str) {
        self.ready = false;
        self.granted = ClientCaps::NONE;
        self.detail = Some(detail.into());
    }

    pub fn attached(&mut self, ack: &AttachAck, fresh: bool) {
        if fresh {
            // Stored message IDs can differ from their live IDs. Rebuild the
            // authoritative tail as one transcript, including user prompts.
            let failure = self
                .server_error
                .take()
                .or_else(|| self.view.last_failure().map(str::to_owned));
            self.reset_view();
            self.server_error = failure;
        }
        self.granted = ack.granted;
        self.replay_target = Some(ack.head);
        self.pending.clear();
        self.deciding.clear();
        self.mission.root_waiting(false);
        self.partial = None;
        self.closed = ack.session.state == HostedState::Closed;
        self.readonly = ack.session.state == HostedState::Interrupted || self.closed;
        if matches!(
            ack.session.state,
            HostedState::Idle
                | HostedState::Failed
                | HostedState::Closed
                | HostedState::Interrupted
        ) {
            self.running_turn = None;
        }
    }

    pub fn offline(&mut self, detail: &str) {
        self.begin_attach(detail);
        self.deciding.clear();
        if let Some(worker) = self.worker.take() {
            worker.task.abort();
        }
    }

    pub fn worker_ended(&mut self, detail: &str) {
        self.begin_attach(detail);
        self.worker = None;
    }

    fn reset_view(&mut self) {
        self.view = SessionView::new();
        self.mission.clear();
        self.head = None;
        self.pending.clear();
        self.deciding.clear();
        self.partial = None;
        self.running_turn = None;
        self.last_sync_millis = None;
        self.server_error = None;
    }

    pub fn observe(&mut self, envelope: &FrameEnvelope) -> FrameAction {
        let cursor = envelope.cursor;
        match &envelope.frame {
            SessionFrame::Revoked => return FrameAction::Revoked,
            SessionFrame::HostDraining { retry_after_ms } => {
                return FrameAction::Draining(*retry_after_ms);
            }
            SessionFrame::Resync { .. } => {
                self.reset_view();
                self.begin_attach("Refreshing transcript after a server history change");
                return FrameAction::Reattach(None);
            }
            SessionFrame::Lagged { resume_from } => {
                // The bridge may have applied fewer frames than the receiver
                // queued before detecting lag. Resume no later than our view.
                let from = self
                    .head
                    .map_or(*resume_from, |head| head.min(*resume_from));
                self.begin_attach("Catching up with the server");
                return FrameAction::Reattach(Some(from));
            }
            _ => {}
        }

        if let SessionFrame::Turn(TurnEvent::AssistantDelta { turn_id, text }) = &envelope.frame {
            self.append_delta(cursor, turn_id, text);
        }
        if self.head.is_some_and(|head| cursor < head) {
            return FrameAction::Continue;
        }
        self.head = Some(cursor);
        self.observe_mission(&envelope.frame);
        self.observe_transcript(envelope);
        self.last_sync_millis = Some(jiff::Timestamp::now().as_millisecond());

        match &envelope.frame {
            SessionFrame::Presence { .. }
                if self
                    .replay_target
                    .is_some_and(|head| !head.is_durably_ahead_of(&cursor)) =>
            {
                self.ready = true;
                self.detail = match (self.closed, self.readonly) {
                    (true, _) => Some("Session closed".into()),
                    (_, true) => Some("Host interrupted this session".into()),
                    _ => None,
                };
            }
            SessionFrame::ApprovalRequested(request) => {
                self.pending.insert(request.id.clone(), request.clone());
                self.mission.root_waiting(true);
                if self.ready {
                    return self.refresh_transcript();
                }
            }
            SessionFrame::ApprovalResolved { request_id, .. } => {
                self.finish_decision(request_id, true);
                self.mission.root_waiting(!self.pending.is_empty());
            }
            SessionFrame::Snapshot {
                turn_id,
                assistant_text,
                ..
            } => {
                self.running_turn = Some(turn_id.clone());
                self.partial = Some(PartialText {
                    turn_id: turn_id.clone(),
                    text: assistant_text.clone(),
                    first_unseen_live: cursor.live,
                });
            }
            SessionFrame::Turn(TurnEvent::TurnStarted { turn_id, .. }) => {
                self.running_turn = Some(turn_id.clone());
                self.server_error = None;
                if self
                    .partial
                    .as_ref()
                    .is_some_and(|partial| &partial.turn_id != turn_id)
                {
                    self.partial = None;
                }
            }
            SessionFrame::Turn(
                TurnEvent::TurnCompleted { .. }
                | TurnEvent::TurnAborted { .. }
                | TurnEvent::TurnFailed { .. },
            ) => {
                self.running_turn = None;
                self.partial = None;
                if self.ready {
                    return self.refresh_transcript();
                }
            }
            SessionFrame::Turn(TurnEvent::ItemAdded {
                item: TurnItem::AssistantMessage(_),
                ..
            }) => self.partial = None,
            SessionFrame::Session(SessionEvent::SessionClosed { .. }) => {
                self.closed = true;
                self.running_turn = None;
                return FrameAction::Closed;
            }
            SessionFrame::Session(SessionEvent::SessionError { message, .. })
            | SessionFrame::Session(SessionEvent::SessionFailed {
                reason: message, ..
            }) => {
                self.server_error = Some(message.clone());
            }
            _ => {}
        }
        FrameAction::Continue
    }

    fn refresh_transcript(&mut self) -> FrameAction {
        self.begin_attach("Synchronizing the session transcript");
        FrameAction::RefreshTranscript
    }

    fn observe_mission(&mut self, frame: &SessionFrame) {
        match frame {
            SessionFrame::Turn(event) => self.mission.root_event(event),
            SessionFrame::Child {
                agent,
                parent,
                role,
                event,
            } => self.mission.child_event(
                agent.as_str(),
                parent.as_ref().map(SessionId::as_str),
                role,
                event,
            ),
            SessionFrame::Job {
                work_id,
                kind,
                state,
                summary,
            } => self
                .mission
                .job(work_id, kind, state, summary.as_deref()),
            _ => {}
        }
    }

    fn observe_transcript(&mut self, envelope: &FrameEnvelope) {
        let SessionFrame::Turn(event) = &envelope.frame else {
            return;
        };
        let projected = match event {
            TurnEvent::ToolCallRequested {
                turn_id,
                call_id,
                tool_name,
                ..
            } => TurnEvent::ItemAdded {
                turn_id: turn_id.clone(),
                item: TurnItem::ToolCall(ToolCallItem {
                    id: ItemId::from_str(call_id.as_str()),
                    call_id: call_id.clone(),
                    tool_name: tool_name.clone(),
                    arguments: serde_json::Value::Null,
                }),
            },
            TurnEvent::ItemAdded {
                turn_id,
                item: TurnItem::ToolResult(result),
            } => TurnEvent::ToolCallCompleted {
                turn_id: turn_id.clone(),
                call_id: result.call_id.clone(),
                duration_ms: result.duration_ms,
                result: if result.result.is_success() {
                    ToolCallResult::success(serde_json::Value::Null)
                } else {
                    ToolCallResult::error("Tool failed")
                },
                placement: None,
            },
            _ => {
                self.view.apply(envelope);
                return;
            }
        };
        self.view.apply(&FrameEnvelope {
            session_id: envelope.session_id.clone(),
            cursor: envelope.cursor,
            frame: SessionFrame::Turn(projected),
        });
    }

    pub fn echo_prompt(
        &mut self,
        id: &SessionId,
        displayed: Cursor,
        client_id: &str,
        text: String,
    ) {
        let Some(head) = self.head else { return };
        if self.ready
            && head.generation == displayed.generation
            && head.durable == displayed.durable
        {
            self.view.apply(&FrameEnvelope {
                session_id: id.clone(),
                cursor: head,
                frame: SessionFrame::Turn(TurnEvent::ItemAdded {
                    turn_id: TurnId::from_str("mobile-submit"),
                    item: TurnItem::UserMessage(UserMessageItem {
                        id: ItemId::from_str(format!("mobile-{client_id}")),
                        content: vec![ContentPart::Text { text }],
                    }),
                }),
            });
        }
    }

    fn append_delta(&mut self, cursor: Cursor, turn_id: &TurnId, text: &str) {
        if self
            .head
            .is_some_and(|head| head.is_durably_ahead_of(&cursor))
        {
            return;
        }
        if self
            .partial
            .as_ref()
            .is_none_or(|partial| &partial.turn_id != turn_id)
        {
            if self.head.is_some_and(|head| cursor < head) && self.ready {
                return;
            }
            self.partial = Some(PartialText {
                turn_id: turn_id.clone(),
                text: String::new(),
                first_unseen_live: 0,
            });
        }
        if let Some(partial) = &mut self.partial
            && cursor.live >= partial.first_unseen_live
        {
            self.running_turn = Some(turn_id.clone());
            partial.first_unseen_live = cursor.live.saturating_add(1);
            partial.text.push_str(text);
            if partial.text.len() > MAX_LIVE_TEXT_BYTES {
                let keep_from = partial
                    .text
                    .ceil_char_boundary(partial.text.len() - MAX_LIVE_TEXT_BYTES);
                partial.text.drain(..keep_from);
            }
        }
    }

    pub fn running_turn_id(&self) -> Option<&TurnId> {
        self.running_turn.as_ref()
    }

    pub fn can_submit(&self) -> bool {
        self.ready && self.granted.steer && !self.readonly
    }

    pub fn can_approve(&self) -> bool {
        self.ready && self.granted.approve && !self.closed
    }

    pub fn require_live(&self) -> Result<(), MobileError> {
        if self.ready {
            Ok(())
        } else {
            Err(MobileError::Disconnected)
        }
    }

    pub fn current_submission_head(
        &self,
        displayed: Cursor,
    ) -> Result<Result<Cursor, Cursor>, MobileError> {
        self.require_live()?;
        if !self.can_submit() {
            return Err(MobileError::Denied {
                detail: "This session does not allow submissions".into(),
            });
        }
        let head = self.head.ok_or(MobileError::Disconnected)?;
        Ok(
            if (head.generation, head.durable) == (displayed.generation, displayed.durable) {
                Ok(displayed)
            } else {
                Err(head)
            },
        )
    }

    pub fn begin_decision(
        &mut self,
        id: &ApprovalId,
        decision: ReviewDecision,
    ) -> Result<(), MobileError> {
        self.require_live()?;
        if !self.can_approve() {
            return Err(MobileError::Denied {
                detail: "This device is not allowed to resolve approvals".into(),
            });
        }
        let valid = self.pending.get(id).is_some_and(|request| {
            request.timeout_at > jiff::Timestamp::now() && request.decisions.contains(&decision)
        });
        if !valid || !self.deciding.insert(id.clone()) {
            return Err(MobileError::Denied {
                detail: "Approval is no longer pending or the decision is unavailable".into(),
            });
        }
        Ok(())
    }

    pub fn finish_decision(&mut self, id: &ApprovalId, remove: bool) {
        self.deciding.remove(id);
        if remove {
            self.pending.remove(id);
        }
    }

    pub fn snapshot(&self, session: &SessionId, connected: bool) -> SessionSnapshot {
        let online = connected && self.ready;
        let running_turn_id = self.running_turn_id().map(ToString::to_string);
        let now = jiff::Timestamp::now();
        let mut approvals: Vec<_> = self
            .pending
            .values()
            .filter(|request| request.timeout_at > now && !self.deciding.contains(&request.id))
            .collect();
        approvals.sort_by_key(|request| (request.requested_at, request.id.as_str()));
        SessionSnapshot {
            session_id: session.to_string(),
            head: self.head.map(Into::into),
            online,
            can_submit: online && self.can_submit(),
            can_approve: online && self.can_approve(),
            can_interrupt: online && self.can_submit() && running_turn_id.is_some(),
            running_turn_id,
            transcript: self.view.transcript().iter().map(project_entry).collect(),
            live_text: self.partial.as_ref().map(|partial| partial.text.clone()),
            approvals: approvals.into_iter().map(project_approval).collect(),
            last_failure: self
                .server_error
                .clone()
                .or_else(|| self.view.last_failure().map(str::to_owned)),
            last_sync_millis: self.last_sync_millis,
            detail: self.detail.clone(),
            agents: self.mission.agents(),
            jobs: self.mission.jobs(),
            reasoning: self.mission.reasoning(),
            finished: self.mission.finished(),
        }
    }
}

fn project_entry(entry: &harw_mobile_core::Entry) -> TranscriptEntry {
    let (kind, text, tool_state) = match &entry.kind {
        EntryKind::User(text) => (TranscriptKind::User, text, None),
        EntryKind::Assistant(text) => (TranscriptKind::Assistant, text, None),
        EntryKind::Error(text) => (TranscriptKind::Error, text, None),
        EntryKind::Tool { name, state } => {
            let state = match state {
                ToolState::Running => MobileToolState::Running,
                ToolState::Done => MobileToolState::Done,
                ToolState::Failed => MobileToolState::Failed,
            };
            (TranscriptKind::Tool, name, Some(state))
        }
    };
    TranscriptEntry {
        id: entry.key().into(),
        kind,
        text: text.clone(),
        tool_state,
    }
}

fn project_approval(request: &ApprovalRequest) -> MobileApproval {
    MobileApproval {
        id: request.id.to_string(),
        summary: request.summary.clone(),
        risk: match request.risk {
            RiskLevel::Low => ApprovalRisk::Low,
            RiskLevel::Medium => ApprovalRisk::Medium,
            RiskLevel::High => ApprovalRisk::High,
            RiskLevel::Critical => ApprovalRisk::Critical,
        },
        requested_at: request.requested_at.to_string(),
        timeout_at: request.timeout_at.to_string(),
        decisions: request
            .decisions
            .iter()
            .map(|decision| (*decision).into())
            .collect(),
    }
}

impl From<ReviewDecision> for ApprovalDecision {
    fn from(decision: ReviewDecision) -> Self {
        match decision {
            ReviewDecision::Approved => Self::Approved,
            ReviewDecision::Rejected => Self::Rejected,
            ReviewDecision::ApprovedOnce => Self::ApprovedOnce,
        }
    }
}

impl From<ApprovalDecision> for ReviewDecision {
    fn from(decision: ApprovalDecision) -> Self {
        match decision {
            ApprovalDecision::Approved => Self::Approved,
            ApprovalDecision::Rejected => Self::Rejected,
            ApprovalDecision::ApprovedOnce => Self::ApprovedOnce,
        }
    }
}

impl From<SessionSummary> for MobileSession {
    fn from(summary: SessionSummary) -> Self {
        Self {
            session_id: summary.session_id.to_string(),
            title: summary.title,
            state: match summary.state {
                HostedState::Idle => MobileSessionState::Idle,
                HostedState::Running => MobileSessionState::Running,
                HostedState::WaitingForApproval => MobileSessionState::WaitingForApproval,
                HostedState::WaitingForChild => MobileSessionState::WaitingForChild,
                HostedState::Queued { .. } => MobileSessionState::Queued,
                HostedState::Interrupted => MobileSessionState::Interrupted,
                HostedState::Failed => MobileSessionState::Failed,
                HostedState::Closed => MobileSessionState::Closed,
            },
            attached: summary.attached,
            updated_at: summary.updated_at.to_string(),
            model: summary.model,
        }
    }
}
