//! Per-invocation effect accounting for WorkDriver model turns.
//!
//! This tracks retained execution-context guards, not operating-system or
//! remote completion. No state here is serialized or restored, and an empty
//! tracker is never a retirement/re-admission certificate. In particular,
//! unreviewed executors can drop their context before detached work finishes.
//! The existing job lease guard remains responsible for claim fencing.

use std::sync::{Arc, Mutex, MutexGuard};

use harw_core::{ToolCallAdmissionFuture, ToolCallMeter, ToolCallPermit};
use tokio::sync::Notify;
use tokio::time::Instant;

#[derive(Default)]
struct State {
    closed: bool,
    active: usize,
    unverified_effects: bool,
}

struct Inner {
    /// Diagnostic binding only; the Arc instance is unique to this invocation.
    session: String,
    state: Mutex<State>,
    changed: Notify,
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.closed = true;
                state.unverified_effects = true;
                state
            }
        }
    }
}

/// Process-local accounting for one worker/judge turn, never reused for a
/// continuation. Session names do not establish identity or grant authority.
#[derive(Clone)]
pub(super) struct TurnEffects(Arc<Inner>);

/// All retained local guards ended. This deliberately does not say that every
/// executor's effects ended; unverified executors may have detached work.
#[derive(Debug)]
pub(super) struct DrainedGuards {
    pub(super) unverified_effects: bool,
}

impl TurnEffects {
    pub(super) fn new(session: &str) -> Self {
        Self(Arc::new(Inner {
            session: session.to_owned(),
            state: Mutex::new(State::default()),
            changed: Notify::new(),
        }))
    }

    fn reserve(&self) -> Result<EffectTicket, String> {
        let mut state = self.0.state();
        if state.closed {
            return Err("work-driver effect admission is closed".to_owned());
        }
        state.active = state
            .active
            .checked_add(1)
            .ok_or_else(|| "work-driver effect accounting capacity exhausted".to_owned())?;
        Ok(EffectTicket(Arc::clone(&self.0)))
    }

    /// Serialized with reservation and final admission. An admission awaiting
    /// its upstream meter must recheck this gate before returning its permit.
    pub(super) fn close(&self) {
        self.0.state().closed = true;
        self.0.changed.notify_waiters();
    }

    pub(super) fn close_on_drop(&self) -> CloseAdmission {
        CloseAdmission(self.clone())
    }

    pub(super) fn meter(&self, inner: Arc<dyn ToolCallMeter>) -> Arc<dyn ToolCallMeter> {
        Arc::new(TrackedMeter {
            inner,
            effects: self.clone(),
        })
    }

    /// Close new admission, then wait for every outstanding admission or
    /// retained guard. Notify is registered before inspecting the count so a
    /// concurrent last-drop cannot be lost. The caller's existing deadline
    /// bounds waiting; timeout is not permission to release a worker scope.
    pub(super) async fn drain(&self, deadline: Instant) -> Result<DrainedGuards, String> {
        self.close();
        loop {
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let state = self.0.state();
                if state.active == 0 {
                    return Ok(DrainedGuards {
                        unverified_effects: state.unverified_effects,
                    });
                }
            }
            if tokio::time::timeout_at(deadline, changed).await.is_err() {
                tracing::warn!(session = %self.0.session, "work-driver retained effect guards did not drain before deadline");
                return Err(
                    "work-driver local effects remain unsettled; scope release is not established"
                        .to_owned(),
                );
            }
        }
    }
}

pub(super) struct CloseAdmission(TurnEffects);

impl Drop for CloseAdmission {
    fn drop(&mut self) {
        self.0.close();
    }
}

struct EffectTicket(Arc<Inner>);

impl EffectTicket {
    fn admitted(&self, tool: &str) -> Result<(), String> {
        let mut state = self.0.state();
        if state.closed {
            return Err(
                "work-driver effect admission closed while awaiting the job lease".to_owned(),
            );
        }
        // This allowlist only labels reviewed lifetime behavior, not tool
        // authority. It never enables a tool or permits worker retirement.
        if !matches!(
            tool,
            "fs.read"
                | "fs.write"
                | "fs.edit"
                | "fs.list"
                | "fs.search"
                | "fs.grep"
                | "fs.glob"
                | "work_driver.report"
                | "collection.read"
                | "coding.read"
                | "coding.replace"
        ) {
            state.unverified_effects = true;
        }
        Ok(())
    }
}

impl Drop for EffectTicket {
    fn drop(&mut self) {
        let mut state = self.0.state();
        state.active = state.active.saturating_sub(1);
        drop(state);
        self.0.changed.notify_waiters();
    }
}

struct TrackedMeter {
    inner: Arc<dyn ToolCallMeter>,
    effects: TurnEffects,
}

impl ToolCallMeter for TrackedMeter {
    fn admit_tool_call<'a>(&'a self, tool: &'a str) -> ToolCallAdmissionFuture<'a> {
        Box::pin(async move {
            // Reserve before the await: close/drain sees admission in flight.
            let ticket = self.effects.reserve()?;
            let permit = self.inner.admit_tool_call(tool).await?;
            ticket.admitted(tool)?;
            // The original permit (and its lease) is never replaced/dropped
            // early. Context propagation carries both until actual local work
            // releases its last copy. No mutex is held across an await.
            Ok(ToolCallPermit::guarded(Box::new((permit, ticket))))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct CountDrop(Arc<AtomicUsize>);
    impl Drop for CountDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct Meter {
        calls: Arc<AtomicUsize>,
        drops: Arc<AtomicUsize>,
    }
    impl ToolCallMeter for Meter {
        fn admit_tool_call<'a>(&'a self, _: &'a str) -> ToolCallAdmissionFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(ToolCallPermit::guarded(Box::new(CountDrop(Arc::clone(
                    &self.drops,
                )))))
            })
        }
    }

    #[tokio::test]
    async fn closed_admission_waits_for_retained_effect() -> TestResult {
        let effects = TurnEffects::new("worker-a");
        let ticket = effects.reserve().map_err(ctx("reserve"))?;
        effects.close();
        assert!(effects.reserve().is_err());
        assert!(effects.drain(Instant::now()).await.is_err());
        drop(ticket);
        let result = effects
            .drain(Instant::now() + Duration::from_secs(1))
            .await
            .map_err(ctx("drain"))?;
        assert!(!result.unverified_effects);
        Ok(())
    }

    #[tokio::test]
    async fn wrapper_retains_original_lease_and_denies_after_close() -> TestResult {
        let calls = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let effects = TurnEffects::new("worker-a");
        let meter = effects.meter(Arc::new(Meter {
            calls: Arc::clone(&calls),
            drops: Arc::clone(&drops),
        }));
        let permit = meter
            .admit_tool_call("fs.write")
            .await
            .map_err(ctx("admit"))?;
        effects.close();
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert!(meter.admit_tool_call("fs.write").await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(effects.drain(Instant::now()).await.is_err());
        drop(permit);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(
            !effects
                .drain(Instant::now())
                .await
                .map_err(ctx("drain"))?
                .unverified_effects
        );
        Ok(())
    }

    #[tokio::test]
    async fn unknown_process_remote_effects_never_become_verified() -> TestResult {
        for tool in [
            "process.kill",
            "remote.call",
            "shell.exec",
            "custom.operation",
        ] {
            let effects = TurnEffects::new("worker-a");
            let meter = effects.meter(Arc::new(Meter {
                calls: Arc::new(AtomicUsize::new(0)),
                drops: Arc::new(AtomicUsize::new(0)),
            }));
            drop(meter.admit_tool_call(tool).await.map_err(ctx("admit"))?);
            assert!(
                effects
                    .drain(Instant::now())
                    .await
                    .map_err(ctx("drain"))?
                    .unverified_effects
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn distinct_invocations_do_not_share_scope_or_counter() -> TestResult {
        let old = TurnEffects::new("same-session");
        let new = TurnEffects::new("same-session");
        let ticket = old.reserve().map_err(ctx("reserve"))?;
        assert!(old.drain(Instant::now()).await.is_err());
        assert!(new.drain(Instant::now()).await.is_ok());
        drop(ticket);
        assert!(old.drain(Instant::now()).await.is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn drop_closes_admission_without_claiming_drain() -> TestResult {
        let effects = TurnEffects::new("worker-a");
        let ticket = effects.reserve().map_err(ctx("reserve"))?;
        drop(effects.close_on_drop());
        assert!(effects.reserve().is_err());
        assert!(effects.drain(Instant::now()).await.is_err());
        drop(ticket);
        assert!(effects.drain(Instant::now()).await.is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn last_drop_wakes_drain_without_lost_notification() -> TestResult {
        let effects = TurnEffects::new("worker-a");
        let ticket = effects.reserve().map_err(ctx("reserve"))?;
        let clone = effects.clone();
        let drain =
            tokio::spawn(async move { clone.drain(Instant::now() + Duration::from_secs(2)).await });
        tokio::task::yield_now().await;
        drop(ticket);
        drain.await.map_err(ctx("join"))?.map_err(ctx("drain"))?;
        Ok(())
    }

    struct PendingMeter {
        entered: Arc<Notify>,
        release: Arc<Notify>,
        drops: Arc<AtomicUsize>,
    }
    impl ToolCallMeter for PendingMeter {
        fn admit_tool_call<'a>(&'a self, _: &'a str) -> ToolCallAdmissionFuture<'a> {
            Box::pin(async move {
                self.entered.notify_one();
                self.release.notified().await;
                Ok(ToolCallPermit::guarded(Box::new(CountDrop(Arc::clone(
                    &self.drops,
                )))))
            })
        }
    }

    #[tokio::test]
    async fn close_during_upstream_admission_denies_dispatch_and_releases_lease() -> TestResult {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let drops = Arc::new(AtomicUsize::new(0));
        let effects = TurnEffects::new("worker-a");
        let meter = effects.meter(Arc::new(PendingMeter {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
            drops: Arc::clone(&drops),
        }));
        let admission = tokio::spawn(async move { meter.admit_tool_call("fs.write").await });
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .map_err(ctx("entered"))?;
        effects.close();
        assert!(effects.drain(Instant::now()).await.is_err());
        release.notify_one();
        assert!(admission.await.map_err(ctx("join"))?.is_err());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(effects.drain(Instant::now()).await.is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn cancelling_pending_admission_releases_reservation() -> TestResult {
        let entered = Arc::new(Notify::new());
        let effects = TurnEffects::new("worker-a");
        let meter = effects.meter(Arc::new(PendingMeter {
            entered: Arc::clone(&entered),
            release: Arc::new(Notify::new()),
            drops: Arc::new(AtomicUsize::new(0)),
        }));
        let admission = tokio::spawn(async move { meter.admit_tool_call("fs.write").await });
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .map_err(ctx("entered"))?;
        admission.abort();
        assert!(admission.await.is_err());
        assert!(effects.drain(Instant::now()).await.is_ok());
        Ok(())
    }
}

#[cfg(test)]
mod integration_tests {
    use super::super::{
        PauseDisposition, TurnSetup, WorkerExecutionControl, WorkerReply, drive_worker_turn,
    };
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_core::{AgentSession, InMemoryStateStore, ModelProvider, ModelResponse, SpawnContext};
    use harw_extension_api::{ExtensionRegistryBuilder, ToolProvider};
    use harw_tools::{
        FunctionToolSpec, JsonSchema, ToolCall, ToolExecutionContext, ToolExecutor,
        ToolExecutorFuture, ToolName, ToolOutput, ToolSpec,
    };
    use harw_types::{AgentRole, SessionId, ToolCallId};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// Deliberately returns before its retained local effect ends. The
    /// WorkDriver adapter (not this executor or the model) must wait for it.
    struct DetachedLocal {
        release: Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
        finished: tokio::sync::mpsc::UnboundedSender<()>,
    }
    impl ToolProvider for DetachedLocal {
        fn tools(&self) -> Vec<ToolSpec> {
            vec![ToolSpec::Function(FunctionToolSpec {
                name: ToolName::new("test.detached_local"),
                description: "retains a local effect context".to_owned(),
                parameters: JsonSchema::default(),
                strict: false,
            })]
        }
        fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
            (name.as_str() == "test.detached_local").then(|| {
                Arc::new(Self {
                    release: Arc::clone(&self.release),
                    finished: self.finished.clone(),
                }) as Arc<dyn ToolExecutor>
            })
        }
    }
    impl ToolExecutor for DetachedLocal {
        fn execute<'a>(
            &'a self,
            context: &'a ToolExecutionContext,
            _: &'a ToolCall,
        ) -> ToolExecutorFuture<'a> {
            let context = context.clone();
            let release = Arc::clone(&self.release);
            let finished = self.finished.clone();
            Box::pin(async move {
                tokio::task::spawn_blocking(move || {
                    // Bound even failing tests so runtime shutdown cannot hang.
                    if let Ok(rx) = release.lock() {
                        let _ = rx.recv_timeout(Duration::from_secs(5));
                    }
                    drop(context);
                    let _ = finished.send(());
                });
                Ok(ToolOutput::text("local work queued"))
            })
        }
    }
    struct Model {
        round: AtomicUsize,
        completed: Arc<Notify>,
    }
    impl ModelProvider for Model {
        fn respond<'a>(&'a self, _: harw_core::ModelRequest) -> harw_core::ModelFuture<'a> {
            let response = if self.round.fetch_add(1, Ordering::SeqCst) == 0 {
                ModelResponse {
                    tool_calls: vec![ToolCall {
                        id: ToolCallId::new(),
                        name: ToolName::new("test.detached_local"),
                        arguments: serde_json::json!({}),
                    }],
                    ..Default::default()
                }
            } else {
                self.completed.notify_one();
                ModelResponse::text("done")
            };
            Box::pin(async move { Ok(response) })
        }
    }
    struct Meter(Arc<AtomicUsize>);
    struct Released(Arc<AtomicUsize>);
    impl Drop for Released {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl ToolCallMeter for Meter {
        fn admit_tool_call<'a>(&'a self, _: &'a str) -> ToolCallAdmissionFuture<'a> {
            Box::pin(async move {
                Ok(ToolCallPermit::guarded(Box::new(Released(Arc::clone(
                    &self.0,
                )))))
            })
        }
    }

    async fn drive_with_detached_context(cancel: bool) -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("workspace"))?;
        let sandbox = crate::runtime_jobs::job_sandbox(
            crate::runtime_jobs::JobEntry::PlanNode {
                kind: harw_plan::PlanNodeKind::Explore,
                may_write: false,
            },
            root.path(),
        )
        .map_err(ctx("sandbox"))?;
        let (release, rx) = std::sync::mpsc::channel();
        let (finished, mut finish_rx) = tokio::sync::mpsc::unbounded_channel();
        let provider = Arc::new(DetachedLocal {
            release: Arc::new(Mutex::new(rx)),
            finished,
        });
        let registry = ExtensionRegistryBuilder::default()
            .tool_provider(provider)
            .build();
        let (events, _events_rx) = tokio::sync::mpsc::unbounded_channel();
        let session_id = SessionId::new();
        let session = AgentSession::new_with_id(
            session_id.clone(),
            AgentRole::Assistant,
            None,
            registry,
            events,
        )
        .with_spawn_context(SpawnContext {
            sandbox,
            suggestions: None,
            capability_snapshot: None,
            approval_actor: None,
            organizational_role: harw_agent_dsl::roles::AgentRoleId::Worker,
            allowed_child_orchestrators: Vec::new(),
            trace: None,
            ceiling: None,
        });
        let setup = TurnSetup {
            session,
            state_store: Arc::new(InMemoryStateStore::new()),
            pause: PauseDisposition::Blocked,
        };
        let completed = Arc::new(Notify::new());
        let model: Arc<dyn ModelProvider> = Arc::new(Model {
            round: AtomicUsize::new(0),
            completed: Arc::clone(&completed),
        });
        let drops = Arc::new(AtomicUsize::new(0));
        let meter: Arc<dyn ToolCallMeter> = Arc::new(Meter(Arc::clone(&drops)));
        let control = WorkerExecutionControl::new();
        let task_control = Arc::clone(&control);
        let worker = tokio::spawn(async move {
            drive_worker_turn(
                setup,
                session_id,
                "work".to_owned(),
                model,
                &task_control,
                Instant::now() + Duration::from_secs(5),
                &meter,
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), completed.notified())
            .await
            .map_err(ctx("model final response"))?;
        // The model and executor returned; the actual effect still retains a
        // context. A successful worker reply must not overtake that context.
        tokio::task::yield_now().await;
        let prematurely_finished = worker.is_finished();
        let early_drops = drops.load(Ordering::SeqCst);
        if cancel {
            control.cancelled.send(true).map_err(ctx("cancel worker"))?;
            let reply = tokio::time::timeout(Duration::from_secs(2), worker)
                .await
                .map_err(ctx("cancel timeout"))?
                .map_err(ctx("join"))?;
            let drops_after_cancel = drops.load(Ordering::SeqCst);
            release.send(()).map_err(ctx("release effect"))?;
            assert!(matches!(reply, WorkerReply::Cancelled));
            assert_eq!(
                drops_after_cancel, 0,
                "cancel released retained effect lease"
            );
        } else {
            release.send(()).map_err(ctx("release effect"))?;
            let reply = tokio::time::timeout(Duration::from_secs(2), worker)
                .await
                .map_err(ctx("worker timeout"))?
                .map_err(ctx("join"))?;
            assert!(matches!(reply, WorkerReply::Completed { .. }));
        }
        tokio::time::timeout(Duration::from_secs(2), finish_rx.recv())
            .await
            .map_err(ctx("finish timeout"))?
            .ok_or(TestError::Missing("effect finished"))?;
        assert!(
            !prematurely_finished,
            "worker finished before retained context ended"
        );
        assert_eq!(early_drops, 0);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn worker_completion_waits_for_actual_retained_context() -> TestResult {
        drive_with_detached_context(false).await
    }
    #[tokio::test]
    async fn cancellation_during_drain_does_not_release_actual_effect() -> TestResult {
        drive_with_detached_context(true).await
    }
}
