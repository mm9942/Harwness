//! Thin attach view over a [`harw_protocol::SessionPort`] (W00 W06, S09).
//!
//! The view works on any port (local UDS, node transport, in-process), so
//! it never learns which transport is behind it. It is a line-oriented
//! view, not the full-screen TUI: turn events are rendered through the
//! existing history cells ([`crate::history_cell`]), plain input lines are
//! submitted as turns, and slash commands steer the session.
//!
//! Commands: `/approve [ID]`, `/once [ID]`, `/reject [ID]`, `/interrupt`,
//! `/help`, `/detach` (also `/quit`, `/q`, EOF).
//!
//! Live token deltas are not rendered; a finished item is printed when the
//! host reports it (replay and live therefore look the same).

use std::collections::VecDeque;
use std::io::Write;
use std::sync::Arc;

use harw_protocol::session_wire::{
    ApprovalRespondParams, AttachParams, CreateParams, DEFAULT_TAIL_ITEMS, InterruptParams,
    RespondResult, SubmitParams, SubmitResult,
};
use harw_protocol::{
    ApprovalKind, ApprovalRequest, ContentPart, Cursor, FrameEnvelope, FrameSource, PortError,
    SessionEvent, SessionFrame, SessionPort, StreamProfile, ToolCallResult, TurnEvent, TurnItem,
};
use harw_types::{ApprovalId, ReviewDecision, SessionId};
use ratatui::text::Line;
use tokio::sync::mpsc;

use crate::history_cell::{
    AssistantHistoryCell, HistoryCell, ReasoningHistoryCell, UserHistoryCell,
};
use crate::style::Theme;

/// Render width when the terminal size is unknown.
const RENDER_WIDTH: u16 = 100;
/// Bound on consecutive re-attaches (resync/lagged) without any new frame.
const MAX_REATTACH: u32 = 8;

/// Failures of the attach view.
#[derive(Debug)]
#[non_exhaustive]
pub enum AttachError {
    /// Skeleton stub: not implemented yet.
    NotImplemented(&'static str),
    /// The session port failed.
    Port(String),
    /// The caller's authority was revoked.
    Revoked,
    /// No session matches the given id or prefix.
    NoMatch(String),
    /// More than one session matches; the list is in the message.
    Ambiguous(String),
    /// The host is shutting down.
    HostDraining {
        /// Suggested reconnect delay.
        retry_after_ms: u64,
    },
    /// The frame stream ended without a detach.
    StreamEnded,
    /// Writing to the output failed.
    Output(String),
}

impl std::fmt::Display for AttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotImplemented(what) => write!(f, "not implemented: {what}"),
            Self::Port(detail) => write!(f, "session port: {detail}"),
            Self::Revoked => f.write_str("session access revoked"),
            Self::NoMatch(id) => write!(f, "no session matches `{id}`"),
            Self::Ambiguous(list) => write!(f, "several sessions match; choose one: {list}"),
            Self::HostDraining { retry_after_ms } => {
                write!(f, "host is draining; retry after {retry_after_ms} ms")
            }
            Self::StreamEnded => f.write_str("session stream ended"),
            Self::Output(detail) => write!(f, "output: {detail}"),
        }
    }
}

impl std::error::Error for AttachError {}

impl From<PortError> for AttachError {
    fn from(error: PortError) -> Self {
        match error {
            PortError::Revoked => Self::Revoked,
            other => Self::Port(other.to_string()),
        }
    }
}

impl From<std::io::Error> for AttachError {
    fn from(error: std::io::Error) -> Self {
        Self::Output(error.to_string())
    }
}

/// Resolve a session id or unique prefix against the host's session list.
///
/// An exact id always wins over prefix matches.
///
/// # Errors
/// [`AttachError::NoMatch`] / [`AttachError::Ambiguous`] or the port error.
pub async fn resolve_session(
    port: &dyn SessionPort,
    query: &str,
) -> Result<SessionId, AttachError> {
    let sessions = port.list().await?;
    let mut matches: Vec<&SessionId> = Vec::new();
    for summary in &sessions {
        if summary.session_id.as_str() == query {
            return Ok(summary.session_id.clone());
        }
        if summary.session_id.as_str().starts_with(query) {
            matches.push(&summary.session_id);
        }
    }
    match matches.as_slice() {
        [] => Err(AttachError::NoMatch(query.to_owned())),
        [one] => Ok((*one).clone()),
        many => Err(AttachError::Ambiguous(
            many.iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        )),
    }
}

/// Attach to `session` (or pick one when `None`) and run the interactive
/// view on stdin/stdout until detach.
///
/// With `None`: exactly one hosted session is attached; none creates a new
/// one; several are listed and refused ([`AttachError::Ambiguous`]).
///
/// # Errors
/// See [`AttachError`].
pub async fn run_attach(
    port: Arc<dyn SessionPort>,
    session: Option<SessionId>,
) -> Result<(), AttachError> {
    let (tx, rx) = mpsc::unbounded_channel();
    // Blocking stdin reads live on their own thread; the view only sees lines.
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut line = String::new();
        loop {
            line.clear();
            match stdin.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx
                        .send(line.trim_end_matches(['\r', '\n']).to_owned())
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
    let mut out = std::io::stdout();
    attach_loop(port, session, rx, &mut out).await
}

fn render_cell(cell: &dyn HistoryCell, out: &mut dyn Write) -> Result<(), AttachError> {
    for line in cell.display_lines(RENDER_WIDTH, Theme::Dark) {
        writeln!(out, "{}", plain(&line))?;
    }
    Ok(())
}

fn plain(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn text_of(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } => text.clone(),
            ContentPart::ImageUrl { .. } | ContentPart::Media { .. } => "[image]".to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_turn(
    event: &TurnEvent,
    origin: Option<&str>,
    out: &mut dyn Write,
) -> Result<(), AttachError> {
    let tag = origin.map(|role| format!("[{role}] ")).unwrap_or_default();
    match event {
        TurnEvent::ItemAdded { item, .. } => match item {
            TurnItem::UserMessage(m) => render_cell(
                &UserHistoryCell {
                    text: text_of(&m.content),
                },
                out,
            )?,
            TurnItem::AssistantMessage(m) => {
                render_cell(
                    &AssistantHistoryCell {
                        source: text_of(&m.content),
                    },
                    out,
                )?;
            }
            TurnItem::Reasoning(r) => {
                let mut cell = ReasoningHistoryCell::new(r.summary_text.join(" "));
                if let Some(role) = origin {
                    cell = cell.with_origin(role);
                }
                render_cell(&cell, out)?;
            }
            TurnItem::ToolCall(call) => {
                writeln!(
                    out,
                    "{tag}* tool {}",
                    crate::sanitize::sanitize_inline(&call.tool_name)
                )?;
            }
            TurnItem::ToolResult(result) => {
                let status = match &result.result {
                    ToolCallResult::Success { .. } => "ok".to_owned(),
                    ToolCallResult::Error { message } => {
                        format!("error: {}", crate::sanitize::sanitize_inline(message))
                    }
                };
                writeln!(out, "{tag}  -> {status} ({} ms)", result.duration_ms)?;
            }
            TurnItem::Error(e) => {
                writeln!(
                    out,
                    "{tag}! {}",
                    crate::sanitize::sanitize_inline(&e.message)
                )?;
            }
        },
        TurnEvent::TurnFailed { reason, .. } => {
            writeln!(
                out,
                "{tag}! turn failed: {}",
                crate::sanitize::sanitize_inline(reason)
            )?;
        }
        TurnEvent::TurnAborted { .. } => writeln!(out, "{tag}! turn interrupted")?,
        _ => {}
    }
    Ok(())
}

fn render_approval(request: &ApprovalRequest, out: &mut dyn Write) -> Result<(), AttachError> {
    let what = match &request.kind {
        ApprovalKind::Exec { command, cwd, .. } => format!("exec `{}` in {cwd}", command.join(" ")),
        ApprovalKind::Patch { changes, .. } => format!("patch of {} file(s)", changes.len()),
        ApprovalKind::DynamicTool { tool_name, .. } => format!("tool {tool_name}"),
    };
    writeln!(
        out,
        "? approval {} requires a decision: {what}\n  {}\n  /approve  /once  /reject",
        request.id.as_str(),
        crate::sanitize::sanitize_inline(&request.summary)
    )?;
    Ok(())
}

/// Outcome of handling one frame.
enum Flow {
    Continue,
    /// Re-attach from this cursor (resync / lagged).
    Reattach(Cursor),
}

struct View<'a> {
    port: Arc<dyn SessionPort>,
    session: SessionId,
    head: Cursor,
    can_steer: bool,
    can_approve: bool,
    pending: VecDeque<ApprovalId>,
    counter: u64,
    out: &'a mut dyn Write,
}

impl View<'_> {
    fn on_frame(&mut self, env: &FrameEnvelope) -> Result<Flow, AttachError> {
        if env.session_id != self.session {
            return Ok(Flow::Continue);
        }
        if env.cursor.is_durably_ahead_of(&self.head) {
            self.head = env.cursor;
        }
        match &env.frame {
            SessionFrame::Turn(event) => render_turn(event, None, self.out)?,
            SessionFrame::Child { role, event, .. } => {
                render_turn(event, Some(role.as_str()), self.out)?;
            }
            SessionFrame::Session(SessionEvent::SessionError { message, .. }) => {
                writeln!(
                    self.out,
                    "! session error: {}",
                    crate::sanitize::sanitize_inline(message)
                )?;
            }
            SessionFrame::Session(SessionEvent::SessionClosed { reason, .. }) => {
                writeln!(
                    self.out,
                    "session closed{}",
                    reason
                        .as_deref()
                        .map(|r| format!(": {r}"))
                        .unwrap_or_default()
                )?;
            }
            SessionFrame::ApprovalRequested(request) => {
                if !self.pending.contains(&request.id) {
                    self.pending.push_back(request.id.clone());
                }
                render_approval(request, self.out)?;
            }
            SessionFrame::ApprovalResolved {
                request_id,
                decision,
                by,
            } => {
                self.pending.retain(|id| id != request_id);
                writeln!(
                    self.out,
                    "approval {} resolved ({decision:?}) by {by}",
                    request_id.as_str()
                )?;
            }
            SessionFrame::Snapshot { assistant_text, .. } => {
                render_cell(
                    &AssistantHistoryCell {
                        source: assistant_text.clone(),
                    },
                    self.out,
                )?;
            }
            SessionFrame::Resync { reason, head } => {
                writeln!(self.out, "-- resync ({reason}); reloading view")?;
                return Ok(Flow::Reattach(*head));
            }
            SessionFrame::Lagged { resume_from } => {
                writeln!(self.out, "-- fell behind; resuming")?;
                return Ok(Flow::Reattach(*resume_from));
            }
            SessionFrame::HostDraining { retry_after_ms } => {
                return Err(AttachError::HostDraining {
                    retry_after_ms: *retry_after_ms,
                });
            }
            SessionFrame::Revoked => return Err(AttachError::Revoked),
            _ => {}
        }
        Ok(Flow::Continue)
    }

    fn next_msg_id(&mut self) -> String {
        self.counter += 1;
        format!("attach-{}-{}", std::process::id(), self.counter)
    }

    /// Handle one input line. `Ok(true)` means detach.
    async fn on_input(&mut self, line: &str) -> Result<bool, AttachError> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(false);
        }
        if let Some(command) = line.strip_prefix('/') {
            let mut words = command.split_whitespace();
            let name = words.next().unwrap_or_default();
            let arg = words.next();
            return match name {
                "detach" | "quit" | "q" => Ok(true),
                "help" => {
                    writeln!(
                        self.out,
                        "/approve [ID]  /once [ID]  /reject [ID]  /interrupt  /detach; any other line is sent as a turn"
                    )?;
                    Ok(false)
                }
                "interrupt" => {
                    if !self.can_steer {
                        writeln!(self.out, "! not allowed (observe-only)")?;
                        return Ok(false);
                    }
                    let result = self
                        .port
                        .interrupt(InterruptParams {
                            session_id: self.session.clone(),
                            turn_id: None,
                        })
                        .await;
                    self.report(result)?;
                    Ok(false)
                }
                "approve" => self
                    .decide(arg, ReviewDecision::Approved)
                    .await
                    .map(|()| false),
                "once" => self
                    .decide(arg, ReviewDecision::ApprovedOnce)
                    .await
                    .map(|()| false),
                "reject" => self
                    .decide(arg, ReviewDecision::Rejected)
                    .await
                    .map(|()| false),
                other => {
                    writeln!(self.out, "! unknown command /{other} (try /help)")?;
                    Ok(false)
                }
            };
        }
        if !self.can_steer {
            writeln!(self.out, "! not allowed: this attachment is observe-only")?;
            return Ok(false);
        }
        let params = SubmitParams {
            session_id: self.session.clone(),
            text: line.to_owned(),
            expect_head: self.head,
            client_msg_id: self.next_msg_id(),
            force: false,
        };
        match self.port.submit(params).await {
            Ok(SubmitResult::Accepted { position }) if position > 0 => {
                writeln!(self.out, "-- queued at position {position}")?;
            }
            Ok(SubmitResult::Accepted { .. }) => {}
            Ok(SubmitResult::Stale { head }) => {
                self.head = head;
                writeln!(
                    self.out,
                    "! session moved on; review the new output and send again"
                )?;
            }
            Ok(SubmitResult::QueueFull) => writeln!(self.out, "! queue full; try again later")?,
            Ok(SubmitResult::Denied { reason }) => writeln!(self.out, "! denied: {reason}")?,
            Err(error) => return Err(error.into()),
        }
        Ok(false)
    }

    async fn decide(
        &mut self,
        arg: Option<&str>,
        decision: ReviewDecision,
    ) -> Result<(), AttachError> {
        if !self.can_approve {
            writeln!(self.out, "! not allowed: no approval capability")?;
            return Ok(());
        }
        let id = match arg {
            Some(text) => self
                .pending
                .iter()
                .find(|id| id.as_str().starts_with(text))
                .cloned(),
            None => self.pending.front().cloned(),
        };
        let Some(id) = id else {
            writeln!(self.out, "! no matching pending approval")?;
            return Ok(());
        };
        match self
            .port
            .respond(ApprovalRespondParams {
                request_id: id.clone(),
                decision,
                reason: None,
            })
            .await
        {
            Ok(RespondResult::Resolved) => {
                self.pending.retain(|p| p != &id);
            }
            Ok(RespondResult::AlreadyResolved { by }) => {
                self.pending.retain(|p| p != &id);
                writeln!(self.out, "-- already resolved by {by}")?;
            }
            Ok(RespondResult::Expired) => {
                self.pending.retain(|p| p != &id);
                writeln!(self.out, "-- approval expired")?;
            }
            Ok(RespondResult::Denied { reason }) => writeln!(self.out, "! denied: {reason}")?,
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    fn report(&mut self, result: Result<(), PortError>) -> Result<(), AttachError> {
        match result {
            Ok(()) => Ok(()),
            Err(PortError::Denied(reason)) => {
                writeln!(self.out, "! denied: {reason}")?;
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }
}

/// Transport- and terminal-independent core of [`run_attach`]: input lines
/// arrive on `input`, output goes to `out`.
///
/// # Errors
/// See [`AttachError`].
pub async fn attach_loop(
    port: Arc<dyn SessionPort>,
    session: Option<SessionId>,
    mut input: mpsc::UnboundedReceiver<String>,
    out: &mut dyn Write,
) -> Result<(), AttachError> {
    let session_id = match session {
        Some(id) => id,
        None => pick_session(port.as_ref(), out).await?,
    };
    let (ack, frames) = port
        .attach(AttachParams {
            session_id: session_id.clone(),
            from: None,
            profile: StreamProfile::Full,
            tail_items: DEFAULT_TAIL_ITEMS,
        })
        .await?;
    writeln!(
        out,
        "attached to {} ({}); /help for commands",
        session_id.as_str(),
        ack.session.title.as_deref().unwrap_or("untitled")
    )?;
    let mut view = View {
        port: Arc::clone(&port),
        session: session_id.clone(),
        head: ack.head,
        can_steer: ack.granted.steer,
        can_approve: ack.granted.approve,
        pending: VecDeque::new(),
        counter: 0,
        out,
    };
    let result = pump(&mut view, frames, &mut input).await;
    // Best effort: the attachment may already be gone.
    let _ = port.detach(session_id).await;
    result
}

/// What woke the pump.
enum Event {
    Frame(Box<Result<Option<FrameEnvelope>, PortError>>),
    Line(Option<String>),
}

/// Drive frames and input until detach, error or end of stream.
async fn pump(
    view: &mut View<'_>,
    mut frames: Box<dyn FrameSource>,
    input: &mut mpsc::UnboundedReceiver<String>,
) -> Result<(), AttachError> {
    let mut reattaches = 0_u32;
    loop {
        let event = tokio::select! {
            frame = frames.next() => Event::Frame(Box::new(frame)),
            line = input.recv() => Event::Line(line),
        };
        match event {
            Event::Frame(frame) => match *frame {
                Ok(Some(env)) => match view.on_frame(&env)? {
                    Flow::Continue => reattaches = 0,
                    Flow::Reattach(from) => {
                        reattaches += 1;
                        if reattaches > MAX_REATTACH {
                            return Err(AttachError::Port(
                                "too many consecutive re-attaches".to_owned(),
                            ));
                        }
                        let session = view.session.clone();
                        let (head, new_frames) = reattach(&view.port, &session, from).await?;
                        view.head = head;
                        frames = new_frames;
                    }
                },
                Ok(None) => return Err(AttachError::StreamEnded),
                Err(error) => return Err(error.into()),
            },
            Event::Line(None) => return Ok(()),
            Event::Line(Some(line)) => {
                if view.on_input(&line).await? {
                    return Ok(());
                }
            }
        }
    }
}

async fn reattach(
    port: &Arc<dyn SessionPort>,
    session: &SessionId,
    from: Cursor,
) -> Result<(Cursor, Box<dyn FrameSource>), AttachError> {
    let (ack, frames) = port
        .attach(AttachParams {
            session_id: session.clone(),
            from: Some(from),
            profile: StreamProfile::Full,
            tail_items: DEFAULT_TAIL_ITEMS,
        })
        .await?;
    Ok((ack.head, frames))
}

async fn pick_session(
    port: &dyn SessionPort,
    out: &mut dyn Write,
) -> Result<SessionId, AttachError> {
    let sessions = port.list().await?;
    match sessions.as_slice() {
        [] => Ok(port.create(CreateParams::default()).await?.session_id),
        [one] => Ok(one.session_id.clone()),
        many => {
            for s in many {
                writeln!(
                    out,
                    "{}  {}",
                    s.session_id.as_str(),
                    s.title.as_deref().unwrap_or("untitled")
                )?;
            }
            Err(AttachError::Ambiguous(
                many.iter()
                    .map(|s| s.session_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use harw_protocol::session_wire::{
        AttachAck, HelloAck, HelloParams, HistoryParams, SessionSummary, SetEffortParams,
        SetModeParams, SetModelParams,
    };
    use harw_protocol::{ClientCaps, HostedState, PortFuture};

    use super::*;

    fn summary(id: &str) -> SessionSummary {
        SessionSummary {
            session_id: SessionId::from_str(id),
            title: Some("t".into()),
            tenant: None,
            state: HostedState::Idle,
            attached: 0,
            updated_at: jiff::Timestamp::UNIX_EPOCH,
            model: None,
        }
    }

    struct Frames(VecDeque<Result<Option<FrameEnvelope>, PortError>>);
    impl FrameSource for Frames {
        fn next(&mut self) -> PortFuture<'_, Option<FrameEnvelope>> {
            let item = self.0.pop_front().unwrap_or(Ok(None));
            Box::pin(async move {
                // Keep the stream open when scripted frames are exhausted so
                // input handling is what ends the test.
                match item {
                    Ok(None) => std::future::pending().await,
                    other => other,
                }
            })
        }
    }

    #[derive(Default)]
    struct Fake {
        sessions: Vec<SessionSummary>,
        attach_error: Option<PortError>,
        frames: Mutex<Vec<FrameEnvelope>>,
        submits: Mutex<Vec<SubmitParams>>,
        submit_result: Mutex<Option<SubmitResult>>,
        responds: Mutex<Vec<ApprovalRespondParams>>,
        detached: Mutex<u32>,
    }

    fn nope<T: Send + 'static>() -> PortFuture<'static, T> {
        Box::pin(async { Err(PortError::Protocol("unused".into())) })
    }

    impl SessionPort for Fake {
        fn hello(&self, _: HelloParams) -> PortFuture<'_, HelloAck> {
            nope()
        }
        fn list(&self) -> PortFuture<'_, Vec<SessionSummary>> {
            let s = self.sessions.clone();
            Box::pin(async move { Ok(s) })
        }
        fn create(&self, _: CreateParams) -> PortFuture<'_, SessionSummary> {
            nope()
        }
        fn attach(&self, p: AttachParams) -> PortFuture<'_, (AttachAck, Box<dyn FrameSource>)> {
            if let Some(e) = self.attach_error.clone() {
                return Box::pin(async move { Err(e) });
            }
            let frames: VecDeque<_> = self
                .frames
                .lock()
                .map(|f| f.clone())
                .unwrap_or_default()
                .into_iter()
                .map(|f| Ok(Some(f)))
                .collect();
            Box::pin(async move {
                let ack = AttachAck {
                    session: summary(p.session_id.as_str()),
                    granted: ClientCaps {
                        observe: true,
                        steer: true,
                        approve: true,
                        control: false,
                        ..ClientCaps::default()
                    },
                    head: Cursor::default(),
                    replay_from: Cursor::default(),
                    host_epoch: 1,
                };
                Ok((ack, Box::new(Frames(frames)) as Box<dyn FrameSource>))
            })
        }
        fn detach(&self, _: SessionId) -> PortFuture<'_, ()> {
            if let Ok(mut n) = self.detached.lock() {
                *n += 1;
            }
            Box::pin(async { Ok(()) })
        }
        fn history(&self, _: HistoryParams) -> PortFuture<'_, Vec<FrameEnvelope>> {
            nope()
        }
        fn submit(&self, p: SubmitParams) -> PortFuture<'_, SubmitResult> {
            if let Ok(mut s) = self.submits.lock() {
                s.push(p);
            }
            let r = self
                .submit_result
                .lock()
                .ok()
                .and_then(|mut r| r.take())
                .unwrap_or(SubmitResult::Accepted { position: 0 });
            Box::pin(async move { Ok(r) })
        }
        fn interrupt(&self, _: InterruptParams) -> PortFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn resume(&self, _: SessionId) -> PortFuture<'_, ()> {
            nope()
        }
        fn close(&self, _: SessionId) -> PortFuture<'_, ()> {
            nope()
        }
        fn respond(&self, p: ApprovalRespondParams) -> PortFuture<'_, RespondResult> {
            if let Ok(mut r) = self.responds.lock() {
                r.push(p);
            }
            Box::pin(async { Ok(RespondResult::Resolved) })
        }
        fn set_model(&self, _: SetModelParams) -> PortFuture<'_, ()> {
            nope()
        }
        fn set_mode(&self, _: SetModeParams) -> PortFuture<'_, ()> {
            nope()
        }
        fn set_effort(&self, _: SetEffortParams) -> PortFuture<'_, ()> {
            nope()
        }
    }

    async fn drive(
        fake: Arc<Fake>,
        session: Option<&str>,
        lines: &[&str],
    ) -> (Result<(), AttachError>, String) {
        let (tx, rx) = mpsc::unbounded_channel();
        for l in lines {
            let _ = tx.send((*l).to_owned());
        }
        // Non-empty input: close the channel so the loop sees EOF after the last line.
        let _keep = if lines.is_empty() {
            Some(tx)
        } else {
            drop(tx);
            None
        };
        let mut out = Vec::new();
        let port: Arc<dyn SessionPort> = fake;
        let result = attach_loop(port, session.map(SessionId::from_str), rx, &mut out).await;
        (result, String::from_utf8_lossy(&out).into_owned())
    }

    #[tokio::test]
    async fn attach_port_error_is_typed() {
        let fake = Arc::new(Fake {
            attach_error: Some(PortError::NotFound),
            ..Fake::default()
        });
        let (result, _) = drive(fake, Some("s1"), &[]).await;
        assert!(matches!(result, Err(AttachError::Port(_))));
    }

    #[tokio::test]
    async fn revoked_attach_maps_to_revoked() {
        let fake = Arc::new(Fake {
            attach_error: Some(PortError::Revoked),
            ..Fake::default()
        });
        let (result, _) = drive(fake, Some("s1"), &[]).await;
        assert!(matches!(result, Err(AttachError::Revoked)));
    }

    #[tokio::test]
    async fn several_sessions_without_choice_is_ambiguous() {
        let fake = Arc::new(Fake {
            sessions: vec![summary("a"), summary("b")],
            ..Fake::default()
        });
        let (result, out) = drive(fake, None, &[]).await;
        assert!(matches!(result, Err(AttachError::Ambiguous(_))));
        assert!(out.contains('a') && out.contains('b'));
    }

    #[tokio::test]
    async fn resolve_prefix_matches_and_rejects() {
        let fake = Fake {
            sessions: vec![summary("abc1"), summary("abd2")],
            ..Fake::default()
        };
        assert!(matches!(
            resolve_session(&fake, "ab").await,
            Err(AttachError::Ambiguous(_))
        ));
        assert!(matches!(
            resolve_session(&fake, "zz").await,
            Err(AttachError::NoMatch(_))
        ));
        assert_eq!(
            resolve_session(&fake, "abc")
                .await
                .map(|s| s.0)
                .ok()
                .as_deref(),
            Some("abc1")
        );
    }

    #[tokio::test]
    async fn input_is_submitted_and_detach_is_sent() {
        let fake = Arc::new(Fake::default());
        let (result, _) = drive(Arc::clone(&fake), Some("s1"), &["hello there"]).await;
        assert!(result.is_ok());
        let submits = fake.submits.lock().map(|s| s.clone()).unwrap_or_default();
        assert_eq!(submits.len(), 1);
        assert_eq!(submits[0].text, "hello there");
        assert!(!submits[0].force);
        assert_eq!(fake.detached.lock().map(|n| *n).unwrap_or(0), 1);
    }

    #[tokio::test]
    async fn stale_submit_is_reported_not_forced() {
        let fake = Arc::new(Fake::default());
        if let Ok(mut r) = fake.submit_result.lock() {
            *r = Some(SubmitResult::Stale {
                head: Cursor {
                    generation: 0,
                    durable: 5,
                    live: 0,
                },
            });
        }
        let (result, out) = drive(Arc::clone(&fake), Some("s1"), &["x"]).await;
        assert!(result.is_ok());
        assert!(out.contains("moved on"));
    }

    #[tokio::test]
    async fn approval_command_without_pending_is_a_notice() {
        let fake = Arc::new(Fake::default());
        let (result, out) = drive(Arc::clone(&fake), Some("s1"), &["/approve", "/bogus"]).await;
        assert!(result.is_ok());
        assert!(out.contains("no matching pending approval"));
        assert!(out.contains("unknown command"));
        assert!(fake.responds.lock().map(|r| r.is_empty()).unwrap_or(false));
    }

    #[tokio::test]
    async fn revoked_frame_ends_with_revoked() {
        let fake = Arc::new(Fake::default());
        if let Ok(mut f) = fake.frames.lock() {
            f.push(FrameEnvelope {
                session_id: SessionId::from_str("s1"),
                cursor: Cursor::default(),
                frame: SessionFrame::Revoked,
            });
        }
        let (result, _) = drive(fake, Some("s1"), &[]).await;
        assert!(matches!(result, Err(AttachError::Revoked)));
    }
}
