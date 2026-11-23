use std::collections::{HashMap, HashSet};

use harw_mobile_core::{EntryKind, SessionView, ToolState};
use harw_protocol::items::{ContentPart, ToolCallItem, ToolCallResult, TurnItem, UserMessageItem};
use harw_protocol::session_wire::{
    AttachAck, ClientCaps, Cursor, FrameEnvelope, HostedState, MAX_VISIBLE_REASONING_BYTES,
    SessionSummary,
};
use harw_protocol::{ApprovalRequest, SessionEvent, SessionFrame, TurnEvent};
use harw_types::{ApprovalId, ItemId, ReviewDecision, RiskLevel, SessionId, TurnId};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::mission::Mission;
use crate::{
    ApprovalDecision, ApprovalRisk, MobileAgentState, MobileApproval, MobileError, MobileSession,
    MobileSessionState, MobileToolState, SessionSnapshot, TranscriptEntry, TranscriptKind,
};

const MAX_LIVE_TEXT_BYTES: usize = 1024 * 1024;

#[derive(Default)]
pub(crate) struct SessionState {
    view: SessionView,
    mission: Mission,
    ring: MissionRing,
    replay: Option<RingReplay>,
    pending: HashMap<ApprovalId, ApprovalRequest>,
    deciding: HashSet<ApprovalId>,
    partial: Option<PartialText>,
    running_turn: Option<TurnId>,
    live_reasoning: Option<(TurnId, String)>,
    head: Option<Cursor>,
    replay_target: Option<Cursor>,
    /// Nach einem "Neu senden" (rewind_before) erwartet: der nächste *live* gemeldete Verlaufsersatz lädt den Chat neu.
    expect_history_replace: bool,
    host_epoch: Option<u64>,
    granted: ClientCaps,
    ready: bool,
    readonly: bool,
    closed: bool,
    last_sync_millis: Option<i64>,
    detail: Option<String>,
    server_error: Option<String>,
    /// The host restart interrupted the last turn (shown as a short hint, not as an error).
    restart_hint: bool,
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

/// Reasoning deltas the host coalesces into one envelope: one agent (the
/// root is the empty id) in one turn.
type ReasoningStream = (String, TurnId);

/// Live-ring position of the mission view in the current generation.
///
/// A non-fresh re-attach replays the ring from position 0 and the compact
/// profiles merge consecutive reasoning deltas, so the replay repeats frames
/// the mission already applied, and one envelope can carry an applied prefix
/// followed by new text. The transcript cursor cannot tell: the head frame
/// itself passes `cursor == head`, and a merged envelope carries the cursor of
/// its last part.
#[derive(Default)]
struct MissionRing {
    mark: Option<RingMark>,
    /// The mission saw the ring's root `TurnStarted`, so `reasoning` counts
    /// every reasoning byte the ring holds up to `mark`.
    from_start: bool,
    /// Reasoning bytes applied per stream since that `TurnStarted`.
    reasoning: HashMap<ReasoningStream, usize>,
}

/// Newest frame applied to the mission view.
#[derive(Clone, Copy)]
struct RingMark {
    generation: u32,
    live: u32,
    /// The frame at `live` was a ring frame (a turn or child event), not a
    /// side frame stamped with the next free ring index.
    ring: bool,
}

impl MissionRing {
    /// Highest ring index of `generation` the mission has applied.
    fn seen_upto(&self, generation: u32) -> Option<u32> {
        let mark = self.mark.filter(|mark| mark.generation == generation)?;
        if mark.ring {
            Some(mark.live)
        } else {
            mark.live.checked_sub(1)
        }
    }

    fn advance(&mut self, cursor: Cursor, ring: bool) {
        if let Some(mark) = self
            .mark
            .as_mut()
            .filter(|mark| mark.generation == cursor.generation)
        {
            if cursor.live > mark.live {
                mark.live = cursor.live;
                mark.ring = ring;
            } else if cursor.live == mark.live {
                mark.ring |= ring;
            }
            return;
        }
        // A new generation starts a new ring.
        *self = Self {
            mark: Some(RingMark {
                generation: cursor.generation,
                live: cursor.live,
                ring,
            }),
            ..Self::default()
        };
    }

    /// Account an applied frame; `replayed` is the stream's replayed total.
    fn account(&mut self, frame: &SessionFrame, replayed: Option<usize>) {
        match frame {
            SessionFrame::Turn(TurnEvent::TurnStarted { .. }) => {
                self.from_start = true;
                self.reasoning.clear();
            }
            SessionFrame::Snapshot { .. } => {
                self.from_start = false;
                self.reasoning.clear();
            }
            _ => {
                if let Some((stream, text)) = reasoning_of(frame) {
                    let seen = self.reasoning.entry(stream).or_default();
                    *seen = match replayed {
                        Some(total) => (*seen).max(total),
                        None => seen.saturating_add(text.len()),
                    };
                }
            }
        }
    }
}

/// Ring replay of a non-fresh attach, until the stream is ready.
struct RingReplay {
    /// Host head at attach: replayed ring frames carry its generation and
    /// durable position and a live index below its live index.
    head: Cursor,
    /// Live index of the previous replayed ring envelope.
    last: Option<u32>,
    /// Reasoning bytes replayed per stream.
    reasoning: HashMap<ReasoningStream, usize>,
}

/// How one frame reaches the mission view.
enum MissionStep {
    /// Apply and move the ring mark.
    Live,
    /// Apply without moving the mark: durable replay and approvals of an
    /// attach precede the ring replay and carry a later cursor.
    Side,
    /// Replayed ring envelope with unseen content; drop its first `skip`
    /// reasoning bytes, which the mission already holds.
    Replayed { skip: usize },
    /// Already applied, or stale.
    Seen,
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
        self.mission.set_root_state(root_state(&ack.session.state));
        self.replay = (!fresh && self.ring.mark.is_some()).then(|| RingReplay {
            head: ack.head,
            last: None,
            reasoning: HashMap::new(),
        });
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
            self.live_reasoning = None;
        }
    }

    pub fn offline(&mut self, detail: &str) {
        self.begin_attach(detail);
        self.deciding.clear();
        if let Some(worker) = self.worker.take() {
            worker.task.abort();
        }
    }

    /// Keep the durable cursor and the visible mission while the same worker reattaches.
    pub fn recover_stream(&mut self) -> Option<Cursor> {
        self.begin_attach("Reconnecting to the session stream");
        self.head
    }

    pub fn worker_ended(&mut self, detail: &str) {
        self.begin_attach(detail);
        self.worker = None;
    }

    fn reset_view(&mut self) {
        self.view = SessionView::new();
        self.mission.clear();
        self.ring = MissionRing::default();
        self.replay = None;
        self.head = None;
        self.pending.clear();
        self.deciding.clear();
        self.partial = None;
        self.running_turn = None;
        self.live_reasoning = None;
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
            SessionFrame::HistoryReplaced { .. } if self.expect_history_replace => {
                // Nur der durch unser Neu-Senden ausgelöste, live gemeldete Ersatz lädt neu; beim Wiederholen alter
                // Verlaufsersätze (Replay beim Anhängen, Verdichtung) wird er ignoriert, sonst entstünde eine Schleife.
                self.expect_history_replace = false;
                self.reset_view();
                self.begin_attach("Refreshing transcript after your edit");
                return FrameAction::Reattach(None);
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
        let stale = self.head.is_some_and(|head| cursor < head);
        let mission = self.mission_step(envelope, stale);
        self.apply_mission(envelope, mission);
        if stale {
            return FrameAction::Continue;
        }
        self.head = Some(cursor);
        self.observe_transcript(envelope);
        self.last_sync_millis = Some(jiff::Timestamp::now().as_millisecond());

        match &envelope.frame {
            SessionFrame::Presence { .. }
                if self
                    .replay_target
                    .is_some_and(|head| !head.is_durably_ahead_of(&cursor)) =>
            {
                self.ready = true;
                self.replay = None;
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
                reasoning_text,
                ..
            } => {
                self.running_turn = Some(turn_id.clone());
                self.live_reasoning = None;
                if let Some(text) = reasoning_text {
                    self.append_visible_reasoning(turn_id, text);
                }
                self.partial = Some(PartialText {
                    turn_id: turn_id.clone(),
                    text: assistant_text.clone(),
                    first_unseen_live: cursor.live,
                });
            }
            SessionFrame::Turn(TurnEvent::TurnStarted { turn_id, .. }) => {
                self.running_turn = Some(turn_id.clone());
                if self.live_reasoning.as_ref().is_some_and(|(id, _)| id != turn_id) {
                    self.live_reasoning = None;
                }
                self.server_error = None;
                self.restart_hint = false;
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
                self.live_reasoning = None;
                self.partial = None;
                if self.ready {
                    return self.refresh_transcript();
                }
            }
            SessionFrame::Turn(TurnEvent::ItemAdded {
                turn_id,
                item: TurnItem::AssistantMessage(_),
            }) => {
                self.partial = None;
                self.clear_visible_reasoning(turn_id);
            }
            SessionFrame::Turn(TurnEvent::ItemAdded {
                turn_id,
                item: TurnItem::Reasoning(_),
            }) => self.clear_visible_reasoning(turn_id),
            SessionFrame::Session(SessionEvent::SessionClosed { .. }) => {
                self.closed = true;
                self.running_turn = None;
                return FrameAction::Closed;
            }
            SessionFrame::Session(SessionEvent::SessionError { message, .. })
                if message.starts_with("session was interrupted by a host restart") =>
            {
                self.restart_hint = true;
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

    /// Decide how `envelope` reaches the mission view; during a ring replay
    /// this also counts the replayed reasoning bytes.
    fn mission_step(&mut self, envelope: &FrameEnvelope, stale: bool) -> MissionStep {
        let cursor = envelope.cursor;
        let Some(replay) = self.replay.as_mut() else {
            return if stale {
                MissionStep::Seen
            } else {
                MissionStep::Live
            };
        };
        let replayed = is_ring_frame(envelope)
            && cursor.generation == replay.head.generation
            && cursor.durable == replay.head.durable
            && cursor.live < replay.head.live;
        if !replayed {
            return if stale {
                MissionStep::Seen
            } else {
                MissionStep::Side
            };
        }
        let previous = replay.last.replace(cursor.live);
        let reasoning = reasoning_of(&envelope.frame);
        let before = reasoning.as_ref().map_or(0, |(stream, text)| {
            let total = replay.reasoning.entry(stream.clone()).or_default();
            let before = *total;
            *total = before.saturating_add(text.len());
            before
        });
        let Some(seen) = self.ring.seen_upto(cursor.generation) else {
            return MissionStep::Replayed { skip: 0 };
        };
        if cursor.live <= seen {
            return MissionStep::Seen;
        }
        if previous.is_some_and(|previous| previous >= seen) {
            return MissionStep::Replayed { skip: 0 };
        }
        // The envelope starts at or before the mark: only a merged reasoning
        // envelope holds parts on both sides.
        let skip = match reasoning {
            Some((stream, text)) if self.ring.from_start => self
                .ring
                .reasoning
                .get(&stream)
                .map_or(0, |seen| seen.saturating_sub(before))
                .min(text.len()),
            // The mission began mid-turn from a snapshot, so the applied
            // prefix has no known length: drop the envelope, never repeat it.
            Some((_, text)) => text.len(),
            None => 0,
        };
        MissionStep::Replayed { skip }
    }

    fn apply_mission(&mut self, envelope: &FrameEnvelope, step: MissionStep) {
        let frame = &envelope.frame;
        match step {
            MissionStep::Seen => {}
            MissionStep::Side => self.observe_mission(frame),
            MissionStep::Live => {
                self.ring.advance(envelope.cursor, is_ring_frame(envelope));
                self.ring.account(frame, None);
                self.observe_mission(frame);
            }
            MissionStep::Replayed { skip } => {
                self.ring.advance(envelope.cursor, true);
                let Some((stream, text)) = reasoning_of(frame) else {
                    self.ring.account(frame, None);
                    self.observe_mission(frame);
                    return;
                };
                let replayed = self
                    .replay
                    .as_ref()
                    .and_then(|replay| replay.reasoning.get(&stream).copied());
                self.ring.account(frame, replayed);
                let rest = &text[text.ceil_char_boundary(skip)..];
                if !rest.is_empty() {
                    self.observe_mission(&with_reasoning(frame, rest));
                }
            }
        }
    }

    fn observe_mission(&mut self, frame: &SessionFrame) {
        match frame {
            SessionFrame::Turn(event) => {
                if let TurnEvent::ReasoningDelta { turn_id, text } = event {
                    self.append_visible_reasoning(turn_id, text);
                }
                self.mission.root_event(event);
            }
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
            } => self.mission.job(work_id, kind, state, summary.as_deref()),
            _ => {}
        }
    }

    fn append_visible_reasoning(&mut self, turn_id: &TurnId, delta: &str) {
        if delta.is_empty() {
            return;
        }
        let (id, text) = self.live_reasoning.get_or_insert_with(|| (turn_id.clone(), String::new()));
        if &*id != turn_id {
            *id = turn_id.clone();
            text.clear();
        }
        text.push_str(delta);
        if text.len() > MAX_VISIBLE_REASONING_BYTES {
            let keep_from = text.ceil_char_boundary(text.len() - MAX_VISIBLE_REASONING_BYTES);
            text.drain(..keep_from);
        }
    }

    fn clear_visible_reasoning(&mut self, turn_id: &TurnId) {
        if self.live_reasoning.as_ref().is_some_and(|(id, _)| id == turn_id) {
            self.live_reasoning = None;
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

    /// Merkt, dass ein Neu-Senden (rewind_before) lief und der Verlaufsersatz erwartet wird.
    pub fn expect_history_replace(&mut self) {
        self.expect_history_replace = true;
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

    /// The session was resumed after a host restart: writable again.
    pub fn mark_resumed(&mut self) {
        self.readonly = false;
        self.restart_hint = true;
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
            live_reasoning: self.live_reasoning.as_ref()
                .filter(|(id, _)| self.running_turn_id() == Some(id))
                .map(|(_, text)| text.clone()),
            approvals: approvals.into_iter().map(project_approval).collect(),
            last_failure: self
                .server_error
                .clone()
                .or_else(|| self.view.last_failure().map(str::to_owned)),
            last_sync_millis: self.last_sync_millis,
            detail: self.detail.clone().or_else(|| {
                self.restart_hint.then(|| {
                    "Unterbrochen durch Neustart – die letzte Antwort fehlt, du kannst weiterschreiben"
                        .to_owned()
                })
            }),
            agents: self.mission.agents(),
            jobs: self.mission.jobs(),
            reasoning: self.mission.reasoning(),
            finished: self.mission.finished(),
        }
    }
}

/// Turn and child events sit in the live ring while a turn runs; durable
/// replay frames are items at live index 0.
fn is_ring_frame(envelope: &FrameEnvelope) -> bool {
    match &envelope.frame {
        SessionFrame::Turn(TurnEvent::ItemAdded { .. }) => envelope.cursor.live > 0,
        SessionFrame::Turn(_) | SessionFrame::Child { .. } => true,
        _ => false,
    }
}

fn reasoning_of(frame: &SessionFrame) -> Option<(ReasoningStream, &str)> {
    match frame {
        SessionFrame::Turn(TurnEvent::ReasoningDelta { turn_id, text }) => {
            Some(((String::new(), turn_id.clone()), text.as_str()))
        }
        SessionFrame::Child {
            agent,
            event: TurnEvent::ReasoningDelta { turn_id, text },
            ..
        } => Some(((agent.to_string(), turn_id.clone()), text.as_str())),
        _ => None,
    }
}

/// `frame`, a reasoning delta, with its text replaced by `rest`.
fn with_reasoning(frame: &SessionFrame, rest: &str) -> SessionFrame {
    let mut frame = frame.clone();
    if let SessionFrame::Turn(TurnEvent::ReasoningDelta { text, .. })
    | SessionFrame::Child {
        event: TurnEvent::ReasoningDelta { text, .. },
        ..
    } = &mut frame
    {
        *text = rest.to_owned();
    }
    frame
}

/// Root agent state that a session state implies at attach.
fn root_state(state: &HostedState) -> MobileAgentState {
    match state {
        HostedState::Running | HostedState::Queued { .. } | HostedState::WaitingForChild => {
            MobileAgentState::Running
        }
        HostedState::WaitingForApproval => MobileAgentState::Waiting,
        HostedState::Idle | HostedState::Closed => MobileAgentState::Done,
        HostedState::Failed | HostedState::Interrupted => MobileAgentState::Failed,
    }
}

fn project_entry(entry: &harw_mobile_core::Entry) -> TranscriptEntry {
    let (kind, text, tool_state) = match &entry.kind {
        EntryKind::User(text) => (TranscriptKind::User, text, None),
        EntryKind::Assistant(text) => (TranscriptKind::Assistant, text, None),
        EntryKind::Reasoning(text) => (TranscriptKind::Reasoning, text, None),
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
        images: entry
            .media
            .iter()
            .filter_map(|media| serde_json::to_string(media).ok())
            .collect(),
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

#[cfg(test)]
mod tests {
    use harw_protocol::session_wire::{
        AttachAck, ClientCaps, Cursor, FrameEnvelope, HostedState, SessionSummary,
    };
    use harw_protocol::{SessionFrame, TurnEvent};
    use harw_types::{SessionId, ThreadId, ToolCallId, TurnId};

    use super::SessionState;
    use crate::{MobileAgentState, ReasoningKind};

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    const GENERATION: u32 = 1;
    const DURABLE: u64 = 4;

    fn at(live: u32) -> Cursor {
        Cursor {
            generation: GENERATION,
            durable: DURABLE,
            live,
        }
    }

    fn ack(state: HostedState, head: Cursor) -> AttachAck {
        AttachAck {
            session: SessionSummary {
                session_id: SessionId::from_str("s1"),
                title: None,
                tenant: None,
                state,
                attached: 1,
                updated_at: jiff::Timestamp::UNIX_EPOCH,
                model: None,
            },
            granted: ClientCaps::NONE,
            head,
            replay_from: head,
            host_epoch: 1,
        }
    }

    fn feed(state: &mut SessionState, live: u32, frame: SessionFrame) {
        state.observe(&FrameEnvelope {
            session_id: SessionId::from_str("s1"),
            cursor: at(live),
            frame,
        });
    }

    fn presence() -> SessionFrame {
        SessionFrame::Presence {
            attached: Vec::new(),
        }
    }

    fn turn(event: TurnEvent) -> SessionFrame {
        SessionFrame::Turn(event)
    }

    fn started() -> SessionFrame {
        turn(TurnEvent::TurnStarted {
            turn_id: TurnId::from_str("t1"),
            thread_id: ThreadId::from_str("th1"),
        })
    }

    fn reasoning(text: &str) -> SessionFrame {
        turn(TurnEvent::ReasoningDelta {
            turn_id: TurnId::from_str("t1"),
            text: text.into(),
        })
    }

    fn tool_call(call: &str) -> SessionFrame {
        turn(TurnEvent::ToolCallRequested {
            turn_id: TurnId::from_str("t1"),
            call_id: ToolCallId::from_str(call),
            tool_name: "shell".into(),
            arguments: serde_json::Value::Null,
        })
    }

    fn job(work_id: &str) -> SessionFrame {
        SessionFrame::Job {
            work_id: work_id.into(),
            kind: "build".into(),
            state: "running".into(),
            summary: None,
        }
    }

    /// Thinking text, number of tool-start lines and root tool calls.
    fn mission_view(state: &SessionState) -> TestResult<(String, usize, u32)> {
        let lines = state.mission.reasoning();
        let thinking = lines
            .iter()
            .filter(|line| line.kind == ReasoningKind::Thinking)
            .map(|line| line.text.as_str())
            .collect();
        let starts = lines
            .iter()
            .filter(|line| line.kind == ReasoningKind::ToolStart)
            .count();
        let root = state
            .mission
            .agents()
            .into_iter()
            .next()
            .ok_or("root agent missing")?;
        Ok((thinking, starts, root.tool_calls))
    }

    fn reattach(state: &mut SessionState, head: Cursor) {
        state.begin_attach("Reconnecting");
        state.attached(&ack(HostedState::Running, head), false);
    }

    #[test]
    fn reattach_replay_trims_merged_reasoning_and_skips_head_frame() -> TestResult {
        let mut state = SessionState::default();
        state.attached(&ack(HostedState::Running, at(0)), true);
        feed(&mut state, 0, presence());
        feed(&mut state, 0, started());
        feed(&mut state, 1, tool_call("c1"));
        feed(&mut state, 2, reasoning("Hello "));
        assert_eq!(mission_view(&state)?, ("Hello ".into(), 1, 1));

        // Ring position 3 ("world") arrived while the phone was away; the
        // compact replay merges it with the already applied position 2.
        reattach(&mut state, at(4));
        feed(&mut state, 0, started());
        feed(&mut state, 1, tool_call("c1"));
        feed(&mut state, 3, reasoning("Hello world"));
        feed(&mut state, 4, presence());
        assert_eq!(mission_view(&state)?, ("Hello world".into(), 1, 1));
        Ok(())
    }

    #[test]
    fn reattach_replay_skips_equal_cursor_head_but_live_side_frames_apply() -> TestResult {
        let mut state = SessionState::default();
        state.attached(&ack(HostedState::Running, at(0)), true);
        feed(&mut state, 0, presence());
        feed(&mut state, 0, started());
        feed(&mut state, 1, reasoning("Plan"));
        feed(&mut state, 2, tool_call("c1"));

        reattach(&mut state, at(3));
        feed(&mut state, 0, started());
        feed(&mut state, 1, reasoning("Plan"));
        feed(&mut state, 2, tool_call("c1"));
        feed(&mut state, 3, presence());
        assert_eq!(mission_view(&state)?, ("Plan".into(), 1, 1));

        // After the replay, frames that share a cursor are all new.
        feed(&mut state, 3, job("w1"));
        feed(&mut state, 3, job("w2"));
        feed(&mut state, 3, tool_call("c2"));
        assert_eq!(state.mission.jobs().len(), 2);
        assert_eq!(mission_view(&state)?, ("Plan".into(), 2, 2));
        Ok(())
    }

    #[test]
    fn attach_sets_root_state_from_session_state() -> TestResult {
        let cases = [
            (HostedState::Idle, MobileAgentState::Done),
            (HostedState::Closed, MobileAgentState::Done),
            (HostedState::Running, MobileAgentState::Running),
            (HostedState::Queued { depth: 1 }, MobileAgentState::Running),
            (HostedState::WaitingForApproval, MobileAgentState::Waiting),
            (HostedState::Failed, MobileAgentState::Failed),
            (HostedState::Interrupted, MobileAgentState::Failed),
        ];
        for (hosted, expected) in cases {
            let mut state = SessionState::default();
            state.attached(&ack(hosted, at(0)), true);
            let root = state
                .mission
                .agents()
                .into_iter()
                .next()
                .ok_or("root agent missing")?;
            assert_eq!(root.state, expected);
        }
        Ok(())
    }
}
