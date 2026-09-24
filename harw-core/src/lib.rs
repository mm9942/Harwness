//! `harw-core` — das Gravity Well des Harness.
//!
//! Hier wohnt die `AgentSession`-FSM, der Turn-Loop und die
//! Session-Orchestrierung. Der Core redet NIE direkt mit einem Terminal:
//! jede Ausgabe läuft über `SessionEvent`s.
//!
//! Session-level tool/instructions/context filtering is exposed through
//! [`activation::SessionActivation`] and [`activation::ToolProfile`].

#![forbid(unsafe_code)]
#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod activation;
pub mod admission;
pub mod agent_events;
pub mod auto_compact;
// Runde 5, Teil K: Hintergrund-Kinder und Orchestrierungsgrenzen.
pub mod background_children;
pub mod cancel;
pub mod capture;
// Runde 5, Teil O: Freigabe-Fragen von Kindern an die Nutzerin.
pub mod child_approval;
// Runde 5, Teil M: Aktivitätsjournal, Endbericht und Eltern-Kind-Nachrichten.
pub mod child_comms;
pub mod child_controller;
// Runde 5, Teil J: Übergabe-Verdichtung am Budget-Ende eines Kindes.
pub mod child_handoff;
// Runde 5, Teil O: Lease-Herzschlag laufender Kinder.
pub mod child_lease_heartbeat;
pub mod compaction;
pub mod context_budget;
pub mod delegation_visibility;
pub mod durable_job_runner;
pub mod envelope;
pub mod error;
pub mod execution_registry;
pub mod guard;
pub mod history;
pub mod history_tail;
pub mod mode;
pub mod model;
pub mod one_shot;
pub mod pinned_model;
pub mod policy;
pub mod session;
pub mod session_manager;
pub mod state_store;
pub mod stream;
#[cfg(test)]
mod test_support;
pub mod testing;
pub mod turn_loop;

pub use activation::{SessionActivation, ToolProfile};
pub use admission::{
    AdmissionContext, JobAdmissionError, JobAdmissionPolicy, JobAdmissionService, JobIntent,
    ResolvedAdmission,
};
pub use agent_events::{
    AgentEvent, AgentEventHub, AgentEventKind, HubOrchestrationObserver, UsageReportingProvider,
};
pub use auto_compact::{
    AutoCompactPolicy, CompactDecision, DEFAULT_ABSOLUTE_CEILING_TOKENS,
    DEFAULT_ORCHESTRATOR_TURN_START_TARGET_TOKENS,
};
// Runde 5, Teil K.
pub use background_children::{
    BackgroundChildren, BackgroundNotice, BackgroundProgress, BackgroundRun, BackgroundStatus,
    ORCHESTRATION_LIMIT_MARKER, OrchestrationLimits, is_orchestration_limit_rejection,
};
pub use capture::{ToolOutcome, ToolOutcomeObserver, ToolOutcomeStatus};
// Runde 5, Teil M.
pub use child_comms::{
    AGENT_MESSAGE_TOOL, CHILD_END_MARKER, ChildComms, ChildEndCause, ChildEndHeader,
    ChildEndReport, ChildEndStatus, ChildJournal, MessageDelivery, PARENT_MESSAGE_TOOL,
    ParentMessage, ParentMessageKind, child_end_label, parse_child_end,
};
pub use child_controller::{
    AgentBudget, BudgetDimension, ChildContextOverload, ChildLimits, ChildRecord,
    ChildRegistryFactory, ChildRunError, ChildRunResult, ChildSessionObservers, ChildUsage,
    ContextWindowResolver, DEFAULT_CHILD_CONTEXT_WINDOW, ExpiredChild, FanoutRequest,
    JoinSemantics, ManagedAgentSpawner, ModelKnownProbe, OrchestrationObserver, ParentGrant,
    RoleEffortWeights, TRANSFER_BUDGET_NOTE, TaskComplexity,
};
pub use compaction::{
    CompactionObserver, CompactionOutcome, CompactionPlan, SUMMARY_MARKER, compact_session,
    deterministic_pass,
};
pub use context_budget::{
    ContextAssembly, ContextBudget, DEFAULT_BYTES_PER_TOKEN, TokenCalibration,
    estimate_request_bytes, estimate_request_tokens, output_reserve_tokens,
};
pub use delegation_visibility::{
    DelegationTarget, DelegationTargetKind, visible_delegation_targets,
};
pub use durable_job_runner::{DurableJobRunner, DurableJobRunnerError};
pub use error::{CoreError, CoreResult};
pub use execution_registry::{
    CancellationResult, ExecutionControl, ExecutionRegistryError, JobExecutionRegistry,
};
pub use guard::{
    DriftEvent, DriftKind, DriftObserver, GuardPolicy, GuardVerdict, PitfallAdvisor,
    ProgressObserver, TurnGuard,
};
pub use harw_protocol::ToolCallResult;
pub use history::{ConversationHistory, ModelMessage};
pub use history_tail::{
    HISTORY_TAIL_GUARANTEED_GROUPS, HISTORY_TAIL_SECTION, HistoryTailRender, render_history_tail,
};
pub use mode::InteractionMode;
pub use model::{
    EchoModelProvider, ModelError, ModelFuture, ModelProvider, ModelRequest, ModelResponse,
    RequestIdentity,
};
pub use one_shot::{OneShotError, complete_text};
pub use pinned_model::PinnedModelProvider;
pub use policy::ConfigApprovalPolicy;
pub use session::{
    APPROVAL_TIMEOUT_REASON, AgentSession, DEFAULT_APPROVAL_TIMEOUT, LiveEmitter, PendingApproval,
    PendingHandoff, SessionState, SpawnContext, TurnHandle, TurnRejection,
};
pub use session_manager::SessionManager;
pub use state_store::{
    InMemoryStateStore, SessionThreadMapper, StateStore, TranscriptStateStore, UsageRound,
};
pub use stream::{ModelStreamEvent, StreamSink};
pub use testing::RecordingModelProvider;
pub use turn_loop::{
    ApprovalResolution, TurnInput, TurnOutcome, TurnTokenBudget, resume_after_approval,
    resume_after_approval_durable, resume_after_child, resume_after_child_durable, run_turn,
    run_turn_durable,
};
