//! Drives a [`SessionView`] from a [`SessionPort`].
//!
//! Transport-neutral: the port is a Unix socket, the node transport or an
//! in-memory fake. The controller attaches with the phone stream profile,
//! applies frames, re-attaches when the host asks for it, and offers the few
//! actions a phone needs: decide an approval, send a prompt, interrupt.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use harw_protocol::session_wire::{
    ApprovalRespondParams, AttachParams, ClientCaps, Cursor, DEFAULT_TAIL_ITEMS, InterruptParams,
    RespondResult, StreamProfile, SubmitParams, SubmitResult,
};
use harw_protocol::{FrameSource, PortError, SessionPort};
use harw_types::{ApprovalId, ReviewDecision, SessionId};

use crate::{Action, SessionView};

/// Controllers created in this process; with the start time it makes the
/// idempotency epoch unique even for two controllers built in one tick.
static INSTANCES: AtomicU64 = AtomicU64::new(0);

/// A fresh epoch for the idempotency keys of one controller. A device
/// restart must not repeat `label-1`, `label-2`, ...: the host remembers
/// recent keys and answers a duplicate `Accepted` without queueing the new
/// prompt, which would drop the user's message silently.
fn fresh_epoch() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!("{nanos:x}.{}", INSTANCES.fetch_add(1, Ordering::Relaxed))
}

/// One attached session on one port.
pub struct Controller {
    port: Arc<dyn SessionPort>,
    label: String,
    epoch: String,
    session: Option<SessionId>,
    source: Option<Box<dyn FrameSource>>,
    view: SessionView,
    granted: Option<ClientCaps>,
    head: Option<Cursor>,
    sent: u64,
}

impl std::fmt::Debug for Controller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Controller")
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

impl Controller {
    /// A controller on `port`; `label` names the device in the idempotency
    /// keys of submitted prompts. Each controller adds its own epoch, so a
    /// restart never reuses a key.
    #[must_use]
    pub fn new(port: Arc<dyn SessionPort>, label: impl Into<String>) -> Self {
        Self {
            port,
            label: label.into(),
            epoch: fresh_epoch(),
            session: None,
            source: None,
            view: SessionView::new(),
            granted: None,
            head: None,
            sent: 0,
        }
    }

    /// The state to render.
    #[must_use]
    pub fn view(&self) -> &SessionView {
        &self.view
    }

    /// What the host granted this device on the attached session. A UI hides
    /// the approve buttons unless `approve` is set; the host decides anyway.
    #[must_use]
    pub fn granted(&self) -> Option<&ClientCaps> {
        self.granted.as_ref()
    }

    /// Attach to `session`, resuming after `from` when the client has a
    /// cursor from an earlier connection.
    ///
    /// # Errors
    /// The port's error when the attach is refused or the transport fails.
    pub async fn attach(
        &mut self,
        session: SessionId,
        from: Option<Cursor>,
    ) -> Result<(), PortError> {
        let (ack, source) = self
            .port
            .attach(AttachParams {
                session_id: session.clone(),
                from,
                profile: StreamProfile::Compact,
                tail_items: DEFAULT_TAIL_ITEMS,
            })
            .await?;
        if from.is_none() {
            self.view = SessionView::new();
        }
        self.granted = Some(ack.granted);
        self.head = Some(ack.head);
        self.session = Some(session);
        self.source = Some(source);
        Ok(())
    }

    /// Wait for the next frame and apply it. Returns `Ok(false)` when the
    /// attachment ended (the caller reconnects and attaches from
    /// `view().cursor()`).
    ///
    /// # Errors
    /// The port's error when the stream fails or a re-attach is refused.
    pub async fn next(&mut self) -> Result<bool, PortError> {
        let Some(source) = self.source.as_mut() else {
            return Err(PortError::Protocol("not attached".to_owned()));
        };
        let Some(envelope) = source.next().await? else {
            self.source = None;
            return Ok(false);
        };
        self.head = Some(envelope.cursor);
        if let Action::Reattach(cursor) = self.view.apply(&envelope) {
            let session = self.attached()?;
            self.attach(session, Some(cursor)).await?;
        }
        Ok(true)
    }

    fn attached(&self) -> Result<SessionId, PortError> {
        self.session
            .clone()
            .ok_or_else(|| PortError::Protocol("not attached".to_owned()))
    }

    /// Approve or reject an open approval.
    ///
    /// # Errors
    /// [`PortError::Denied`] locally when the host did not grant `approve`
    /// to this device (no round trip); otherwise the host's answer.
    pub async fn decide(
        &self,
        request: ApprovalId,
        decision: ReviewDecision,
        reason: Option<String>,
    ) -> Result<RespondResult, PortError> {
        if !self.granted.as_ref().is_some_and(|caps| caps.approve) {
            return Err(PortError::Denied(
                "this device is not allowed to approve".to_owned(),
            ));
        }
        self.port
            .respond(ApprovalRespondParams {
                request_id: request,
                decision,
                reason,
            })
            .await
    }

    /// Send a prompt. A stale head is remembered, so the caller can show the
    /// newer state and send again.
    ///
    /// # Errors
    /// The port's error; a refusal by the host is a [`SubmitResult`].
    pub async fn submit(&mut self, text: impl Into<String>) -> Result<SubmitResult, PortError> {
        let session = self.attached()?;
        let expect_head = self
            .view
            .cursor()
            .copied()
            .or(self.head)
            .ok_or_else(|| PortError::Protocol("no head known".to_owned()))?;
        self.sent += 1;
        let result = self
            .port
            .submit(SubmitParams {
                session_id: session,
                text: text.into(),
                expect_head,
                client_msg_id: format!("{}-{}-{}", self.label, self.epoch, self.sent),
                force: false,
            })
            .await?;
        if let SubmitResult::Stale { head } = &result {
            self.head = Some(*head);
        }
        Ok(result)
    }

    /// Interrupt the running turn.
    ///
    /// # Errors
    /// The port's error.
    pub async fn interrupt(&self) -> Result<(), PortError> {
        let session = self.attached()?;
        self.port
            .interrupt(InterruptParams {
                session_id: session,
                turn_id: None,
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use harw_protocol::session_wire::{
        AttachAck, CreateParams, FrameEnvelope, HelloAck, HelloParams, HistoryParams, HostedState,
        SessionFrame, SessionSummary, SetEffortParams, SetModeParams, SetModelParams,
    };
    use harw_protocol::{ApprovalRequest, PortFuture};

    use crate::test_support::{TestResult, ctx, ensure};

    fn cursor(durable: u64) -> Cursor {
        Cursor {
            generation: 1,
            durable,
            live: 0,
        }
    }

    fn unsupported<T: Send + 'static>() -> PortFuture<'static, T> {
        Box::pin(async { Err(PortError::Protocol("unsupported by the fake".to_owned())) })
    }

    struct Frames(VecDeque<FrameEnvelope>);

    impl FrameSource for Frames {
        fn next(&mut self) -> PortFuture<'_, Option<FrameEnvelope>> {
            let next = self.0.pop_front();
            Box::pin(async move { Ok(next) })
        }
    }

    struct FakePort {
        frames: Mutex<VecDeque<FrameEnvelope>>,
        approve: bool,
        attaches: Mutex<Vec<AttachParams>>,
        responded: Mutex<Vec<ApprovalRespondParams>>,
        submits: Mutex<Vec<SubmitParams>>,
        stale: Option<Cursor>,
    }

    impl FakePort {
        fn new(approve: bool, frames: Vec<FrameEnvelope>) -> Arc<Self> {
            Arc::new(Self {
                frames: Mutex::new(frames.into()),
                approve,
                attaches: Mutex::new(Vec::new()),
                responded: Mutex::new(Vec::new()),
                submits: Mutex::new(Vec::new()),
                stale: None,
            })
        }
    }

    fn summary(id: &SessionId) -> SessionSummary {
        SessionSummary {
            session_id: id.clone(),
            title: None,
            tenant: None,
            state: HostedState::Idle,
            attached: 1,
            updated_at: jiff::Timestamp::UNIX_EPOCH,
            model: None,
        }
    }

    impl SessionPort for FakePort {
        fn hello(&self, _: HelloParams) -> PortFuture<'_, HelloAck> {
            unsupported()
        }
        fn list(&self) -> PortFuture<'_, Vec<SessionSummary>> {
            unsupported()
        }
        fn create(&self, _: CreateParams) -> PortFuture<'_, SessionSummary> {
            unsupported()
        }
        fn attach(
            &self,
            params: AttachParams,
        ) -> PortFuture<'_, (AttachAck, Box<dyn FrameSource>)> {
            let session = params.session_id.clone();
            if let Ok(mut calls) = self.attaches.lock() {
                calls.push(params);
            }
            let frames = self
                .frames
                .lock()
                .map(|mut queue| std::mem::take(&mut *queue))
                .unwrap_or_default();
            let granted = ClientCaps {
                observe: true,
                steer: true,
                approve: self.approve,
                ..ClientCaps::default()
            };
            Box::pin(async move {
                let ack = AttachAck {
                    session: summary(&session),
                    granted,
                    head: cursor(0),
                    replay_from: cursor(0),
                    host_epoch: 1,
                };
                let source: Box<dyn FrameSource> = Box::new(Frames(frames));
                Ok((ack, source))
            })
        }
        fn detach(&self, _: SessionId) -> PortFuture<'_, ()> {
            unsupported()
        }
        fn history(&self, _: HistoryParams) -> PortFuture<'_, Vec<FrameEnvelope>> {
            unsupported()
        }
        fn submit(&self, params: SubmitParams) -> PortFuture<'_, SubmitResult> {
            if let Ok(mut calls) = self.submits.lock() {
                calls.push(params);
            }
            let result = match self.stale {
                Some(head) => SubmitResult::Stale { head },
                None => SubmitResult::Accepted { position: 0 },
            };
            Box::pin(async move { Ok(result) })
        }
        fn interrupt(&self, _: InterruptParams) -> PortFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn resume(&self, _: SessionId) -> PortFuture<'_, ()> {
            unsupported()
        }
        fn close(&self, _: SessionId) -> PortFuture<'_, ()> {
            unsupported()
        }
        fn respond(&self, params: ApprovalRespondParams) -> PortFuture<'_, RespondResult> {
            if let Ok(mut calls) = self.responded.lock() {
                calls.push(params);
            }
            Box::pin(async { Ok(RespondResult::Resolved) })
        }
        fn set_model(&self, _: SetModelParams) -> PortFuture<'_, ()> {
            unsupported()
        }
        fn set_mode(&self, _: SetModeParams) -> PortFuture<'_, ()> {
            unsupported()
        }
        fn set_effort(&self, _: SetEffortParams) -> PortFuture<'_, ()> {
            unsupported()
        }
    }

    fn approval_frame(durable: u64, id: &str) -> FrameEnvelope {
        use harw_protocol::ApprovalKind;
        use harw_types::{RiskLevel, TurnId, WorkId};
        let now = jiff::Timestamp::UNIX_EPOCH;
        FrameEnvelope {
            session_id: SessionId::from_str("s1"),
            cursor: cursor(durable),
            frame: SessionFrame::ApprovalRequested(ApprovalRequest {
                id: ApprovalId::from_str(id),
                work_id: WorkId::from_str("w"),
                kind: ApprovalKind::DynamicTool {
                    turn_id: TurnId::from_str("t"),
                    tool_name: "shell".to_owned(),
                    arguments: serde_json::Value::Null,
                },
                summary: "run ls".to_owned(),
                risk: RiskLevel::Low,
                requested_at: now,
                timeout_at: ApprovalRequest::default_timeout_at(now),
                decisions: vec![ReviewDecision::Approved, ReviewDecision::Rejected],
            }),
        }
    }

    #[tokio::test]
    async fn attaches_with_the_phone_profile_and_fills_the_inbox() -> TestResult {
        let port = FakePort::new(true, vec![approval_frame(1, "a1")]);
        let mut controller = Controller::new(port.clone(), "phone");
        controller.attach(SessionId::from_str("s1"), None).await?;
        ensure(controller.next().await?, "frame applied")?;
        ensure(!controller.next().await?, "stream end reported")?;
        ensure(
            controller.view().pending_approvals().count() == 1,
            "inbox filled",
        )?;
        let attaches = port.attaches.lock().map_err(ctx("lock"))?;
        ensure(
            attaches.first().map(|a| a.profile) == Some(StreamProfile::Compact),
            "compact profile requested",
        )
    }

    #[tokio::test]
    async fn deciding_needs_the_granted_approve_cap_and_sends_no_actor() -> TestResult {
        let denied = FakePort::new(false, vec![]);
        let mut controller = Controller::new(denied.clone(), "phone");
        controller.attach(SessionId::from_str("s1"), None).await?;
        let refused = controller
            .decide(ApprovalId::from_str("a1"), ReviewDecision::Approved, None)
            .await;
        ensure(
            matches!(refused, Err(PortError::Denied(_))),
            "refused locally",
        )?;
        ensure(
            denied.responded.lock().map_err(ctx("lock"))?.is_empty(),
            "no round trip",
        )?;

        let allowed = FakePort::new(true, vec![]);
        let mut controller = Controller::new(allowed.clone(), "phone");
        controller.attach(SessionId::from_str("s1"), None).await?;
        let result = controller
            .decide(
                ApprovalId::from_str("a1"),
                ReviewDecision::Rejected,
                Some("no".to_owned()),
            )
            .await?;
        ensure(result == RespondResult::Resolved, "resolved")?;
        ensure(
            allowed.responded.lock().map_err(ctx("lock"))?.len() == 1,
            "one respond call",
        )
    }

    #[tokio::test]
    async fn resync_reattaches_from_the_hosts_head() -> TestResult {
        let resync = FrameEnvelope {
            session_id: SessionId::from_str("s1"),
            cursor: cursor(3),
            frame: SessionFrame::Resync {
                reason: "compaction".to_owned(),
                head: Cursor {
                    generation: 2,
                    durable: 0,
                    live: 0,
                },
            },
        };
        let port = FakePort::new(true, vec![approval_frame(1, "a1"), resync]);
        let mut controller = Controller::new(port.clone(), "phone");
        controller.attach(SessionId::from_str("s1"), None).await?;
        controller.next().await?;
        controller.next().await?;
        let attaches = port.attaches.lock().map_err(ctx("lock"))?;
        ensure(attaches.len() == 2, "attached twice")?;
        ensure(
            attaches.get(1).and_then(|a| a.from).map(|c| c.generation) == Some(2),
            "second attach resumes at the new generation",
        )?;
        ensure(
            controller.view().pending_approvals().count() == 0,
            "view dropped on resync",
        )
    }

    #[tokio::test]
    async fn submit_keys_are_unique_across_controller_restarts() -> TestResult {
        let mut fake = FakePort::new(true, vec![]);
        if let Some(inner) = Arc::get_mut(&mut fake) {
            inner.stale = Some(cursor(7));
        }
        // The same device label before and after an app restart.
        let mut before = Controller::new(fake.clone(), "phone");
        before.attach(SessionId::from_str("s1"), None).await?;
        let first = before.submit("hi").await?;
        ensure(
            first == SubmitResult::Stale { head: cursor(7) },
            "stale surfaced",
        )?;
        before.submit("hi again").await?;
        let mut after = Controller::new(fake.clone(), "phone");
        after.attach(SessionId::from_str("s1"), None).await?;
        after.submit("hello").await?;

        let submits = fake.submits.lock().map_err(ctx("lock"))?;
        let keys: Vec<&str> = submits.iter().map(|s| s.client_msg_id.as_str()).collect();
        ensure(keys.len() == 3, "three submits")?;
        ensure(
            keys.iter().all(|k| k.starts_with("phone-")),
            "device label kept",
        )?;
        let mut unique = keys.clone();
        unique.sort_unstable();
        unique.dedup();
        ensure(unique.len() == 3, "no key repeats across a restart")?;
        ensure(
            submits.get(1).map(|s| s.expect_head) == Some(cursor(7)),
            "the second submit expects the newer head",
        )
    }
}
