//! Tests for managed child admission: roles, parent correlation, sandbox
//! inheritance, depth, and per-parent concurrency ceilings.

use harw_agent_dsl::roles::AgentRoleId;
use harw_catalog::{
    ActivatedCapability, AgentSuggestions, CapabilitySuggestion, SpawnCapabilitySnapshot,
    SuggestionKind,
};
use harw_core::{
    AgentSession, ChildLimits, ChildRegistryFactory, EchoModelProvider, InMemoryStateStore,
    ManagedAgentSpawner, ModelFuture, ModelProvider, ModelRequest, ModelResponse, SessionManager,
    SessionState, SpawnContext, TurnInput, TurnOutcome,
};
use harw_extension_api::{
    AgentSpawnError, AgentSpawner, ExtensionRegistry, ExtensionRegistryBuilder, SpawnInput,
};
use harw_sandbox::{
    Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
};
use harw_session_store::ChildLeaseStore;
use harw_types::{
    AgentRole, ApprovalActor, ReasoningEffort, SessionId, TenantId, ToolCallId, WorkspaceId,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

struct EmptyChildRegistry;

struct BarrierChildRegistry {
    barrier: Arc<tokio::sync::Barrier>,
}

struct BlockingChildRegistry {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

struct SnapshotChildRegistry {
    snapshot: SpawnCapabilitySnapshot,
    seen_activated_name: Arc<Mutex<Option<String>>>,
}

struct BarrierModel {
    barrier: Arc<tokio::sync::Barrier>,
}

struct BlockingModel {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl ModelProvider for BarrierModel {
    fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
        let barrier = self.barrier.clone();
        Box::pin(async move {
            barrier.wait().await;
            Ok(ModelResponse::text("parallel child complete"))
        })
    }
}

impl ModelProvider for BlockingModel {
    fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
        let entered = self.entered.clone();
        let release = self.release.clone();
        Box::pin(async move {
            entered.notify_one();
            release.notified().await;
            Ok(ModelResponse::text("late child complete"))
        })
    }
}

impl ChildRegistryFactory for EmptyChildRegistry {
    fn build_registry(
        &self,
        _role: &str,
        _input: &SpawnInput,
        _suggestions: Option<&AgentSuggestions>,
    ) -> Result<ExtensionRegistry, AgentSpawnError> {
        Ok(ExtensionRegistryBuilder::default().build())
    }

    fn model_for(&self, role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
        Ok(Arc::new(EchoModelProvider::new(format!("{role} complete"))))
    }
}

impl ChildRegistryFactory for BarrierChildRegistry {
    fn build_registry(
        &self,
        _role: &str,
        _input: &SpawnInput,
        _suggestions: Option<&AgentSuggestions>,
    ) -> Result<ExtensionRegistry, AgentSpawnError> {
        Ok(ExtensionRegistryBuilder::default().build())
    }

    fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
        Ok(Arc::new(BarrierModel {
            barrier: self.barrier.clone(),
        }))
    }
}

impl ChildRegistryFactory for BlockingChildRegistry {
    fn build_registry(
        &self,
        _role: &str,
        _input: &SpawnInput,
        _suggestions: Option<&AgentSuggestions>,
    ) -> Result<ExtensionRegistry, AgentSpawnError> {
        Ok(ExtensionRegistryBuilder::default().build())
    }

    fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
        Ok(Arc::new(BlockingModel {
            entered: self.entered.clone(),
            release: self.release.clone(),
        }))
    }
}

impl ChildRegistryFactory for SnapshotChildRegistry {
    fn build_registry(
        &self,
        _role: &str,
        _input: &SpawnInput,
        _suggestions: Option<&AgentSuggestions>,
    ) -> Result<ExtensionRegistry, AgentSpawnError> {
        Ok(ExtensionRegistryBuilder::default().build())
    }

    fn capability_snapshot(
        &self,
        _role: &str,
        _input: &SpawnInput,
    ) -> Result<Option<SpawnCapabilitySnapshot>, AgentSpawnError> {
        Ok(Some(self.snapshot.clone()))
    }

    fn build_registry_with_capabilities(
        &self,
        _role: &str,
        _input: &SpawnInput,
        snapshot: Option<&SpawnCapabilitySnapshot>,
    ) -> Result<ExtensionRegistry, AgentSpawnError> {
        let activated = snapshot
            .and_then(|snapshot| snapshot.activated.first())
            .map(|capability| capability.name.clone());
        *self.seen_activated_name.lock().expect("seen snapshot lock") = activated;
        Ok(ExtensionRegistryBuilder::default().build())
    }

    fn model_for(&self, role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
        Ok(Arc::new(EchoModelProvider::new(format!("{role} complete"))))
    }
}

fn test_sandbox(permissions: PermissionSet) -> SandboxSpec {
    let harness_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("harw-core has a workspace parent")
        .to_path_buf();
    let registry = WorkspaceRegistry::build(
        &harness_root,
        [WorkspaceRegistration {
            tenant: TenantId::from_str("test-tenant"),
            workspace: WorkspaceId::from_str("controller-tests"),
            root: PathBuf::from("harw-core"),
        }],
    )
    .expect("test workspace is registered");
    SandboxSpec::from_resolved(
        registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("controller-tests"),
            )
            .expect("test workspace resolves"),
        permissions,
    )
}

fn managed_parent() -> (Arc<Mutex<SessionManager>>, SessionId, SandboxSpec) {
    let (events, _receiver) = mpsc::unbounded_channel();
    let manager = Arc::new(Mutex::new(SessionManager::new(events)));
    let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
    let parent = manager
        .lock()
        .expect("manager lock")
        .create_governed_session(
            AgentRole::Assistant,
            None,
            ExtensionRegistryBuilder::default().build(),
            SpawnContext {
                sandbox: sandbox.clone(),
                suggestions: None,
                capability_snapshot: None,
                approval_actor: Some(ApprovalActor::Operator {
                    id: "test-operator".to_owned(),
                }),
                organizational_role: AgentRoleId::RootOrchestrator,
                // Generic fixture — not exercising trace propagation.
                trace: None,
                // Generic fixture — not exercising context-ceiling propagation.
                ceiling: None,
            },
        );
    (manager, parent, sandbox)
}

/// Same governed-parent shape as [`managed_parent`], but admitted under an
/// arbitrary §3 DSL organizational role instead of the fixed
/// `RootOrchestrator` default — used by tests exercising
/// `harw_agent_dsl::roles::can_spawn` enforcement in `admit()`.
fn managed_parent_with_organizational_role(
    organizational_role: AgentRoleId,
) -> (Arc<Mutex<SessionManager>>, SessionId, SandboxSpec) {
    let (events, _receiver) = mpsc::unbounded_channel();
    let manager = Arc::new(Mutex::new(SessionManager::new(events)));
    let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
    let parent = manager
        .lock()
        .expect("manager lock")
        .create_governed_session(
            AgentRole::Assistant,
            None,
            ExtensionRegistryBuilder::default().build(),
            SpawnContext {
                sandbox: sandbox.clone(),
                suggestions: None,
                capability_snapshot: None,
                approval_actor: Some(ApprovalActor::Operator {
                    id: "test-operator".to_owned(),
                }),
                organizational_role,
                // Generic fixture — not exercising trace propagation.
                trace: None,
                // Generic fixture — not exercising context-ceiling propagation.
                ceiling: None,
            },
        );
    (manager, parent, sandbox)
}

/// Same governed-parent shape as [`managed_parent`], but built through the
/// `AgentSession::new(...).with_reasoning_effort(...)` builder chain so the
/// parent carries a reasoning-effort level *before* any child is admitted.
/// `admit()` inherits this value into the child (Wave 8), which is the base
/// that `clamp_child_reasoning_effort` tests are exercising.
fn managed_parent_with_effort(
    effort: Option<ReasoningEffort>,
) -> (Arc<Mutex<SessionManager>>, SessionId, SandboxSpec) {
    let (events, _receiver) = mpsc::unbounded_channel();
    let manager = Arc::new(Mutex::new(SessionManager::new(events.clone())));
    let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
    let session = AgentSession::new(
        AgentRole::Assistant,
        None,
        ExtensionRegistryBuilder::default().build(),
        events,
    )
    .with_spawn_context(SpawnContext {
        sandbox: sandbox.clone(),
        suggestions: None,
        capability_snapshot: None,
        approval_actor: Some(ApprovalActor::Operator {
            id: "test-operator".to_owned(),
        }),
        organizational_role: AgentRoleId::RootOrchestrator,
        // Generic fixture — not exercising trace propagation.
        trace: None,
        // Generic fixture — not exercising context-ceiling propagation.
        ceiling: None,
    })
    .with_reasoning_effort(effort);
    let parent = session.id().clone();
    manager
        .lock()
        .expect("manager lock")
        .restore(session)
        .expect("parent session inserts cleanly");
    (manager, parent, sandbox)
}

/// A `worker`-role spawner over the given manager, matching the role wiring
/// used throughout this file (`EmptyChildRegistry`, conservative limits).
fn worker_spawner(manager: Arc<Mutex<SessionManager>>) -> ManagedAgentSpawner {
    ManagedAgentSpawner::new(manager, ChildLimits::conservative()).with_role(
        "worker",
        AgentRole::Agent {
            name: "worker".to_owned(),
        },
        AgentRoleId::Worker,
        Arc::new(EmptyChildRegistry),
    )
}

fn spawn_input(parent_session_id: SessionId) -> SpawnInput {
    SpawnInput {
        parent_session_id,
        handoff_call_id: ToolCallId::new(),
        instructions: None,
        context: serde_json::json!({"task": "bounded child"}),
        // No ceiling demand of its own: inherits the parent's ceiling.
        ceiling: None,
    }
}

#[tokio::test]
async fn managed_spawner_creates_a_governed_child_and_tracks_its_lifecycle() {
    let (manager, parent, sandbox) = managed_parent();
    let spawner = ManagedAgentSpawner::new(manager.clone(), ChildLimits::conservative()).with_role(
        "worker",
        AgentRole::Agent {
            name: "worker".to_owned(),
        },
        AgentRoleId::Worker,
        Arc::new(EmptyChildRegistry),
    );

    let child = spawner
        .spawn_child("worker", spawn_input(parent.clone()), sandbox.clone(), None)
        .await
        .expect("registered role admits governed child");

    let record = spawner.child_record(&child).expect("child is tracked");
    assert_eq!(record.parent, parent);
    assert_eq!(record.role, "worker");
    assert_eq!(record.depth, 1);
    let manager_guard = manager.lock().expect("manager lock");
    let child_session = manager_guard.get(&child).expect("child exists");
    assert_eq!(child_session.parent_session_id(), Some(&record.parent));
    assert_eq!(
        child_session
            .spawn_context()
            .expect("child context")
            .sandbox,
        sandbox
    );

    spawner.close_child(&child);
    assert!(spawner.child_record(&child).is_none());
}

#[tokio::test]
async fn managed_spawner_attaches_target_capability_contract_to_child_and_registry() {
    let (manager, parent, sandbox) = managed_parent();
    let snapshot = SpawnCapabilitySnapshot {
        agent: "worker".to_owned(),
        suggestions: AgentSuggestions {
            available: vec![CapabilitySuggestion {
                kind: SuggestionKind::Skill,
                name: "rust-review".to_owned(),
                description: "Review Rust changes".to_owned(),
                tools: vec!["read".to_owned()],
                mcps: Vec::new(),
            }],
            omitted: Vec::new(),
        },
        activated: vec![ActivatedCapability {
            kind: SuggestionKind::Skill,
            name: "rust-review".to_owned(),
            description: "Review Rust changes".to_owned(),
            tools: vec!["read".to_owned()],
            mcps: Vec::new(),
            definition_sha256: "a".repeat(64),
        }],
    };
    let seen_activated_name = Arc::new(Mutex::new(None));
    let spawner = ManagedAgentSpawner::new(manager.clone(), ChildLimits::conservative()).with_role(
        "worker",
        AgentRole::Agent {
            name: "worker".to_owned(),
        },
        AgentRoleId::Worker,
        Arc::new(SnapshotChildRegistry {
            snapshot: snapshot.clone(),
            seen_activated_name: seen_activated_name.clone(),
        }),
    );

    let child = spawner
        .spawn_child("worker", spawn_input(parent), sandbox, None)
        .await
        .expect("child admitted with target capability contract");
    assert_eq!(
        seen_activated_name
            .lock()
            .expect("seen snapshot lock")
            .as_deref(),
        Some("rust-review")
    );
    let manager = manager.lock().expect("manager lock");
    let context = manager
        .get(&child)
        .expect("child session")
        .spawn_context()
        .expect("trusted child context");
    assert_eq!(context.capability_snapshot.as_ref(), Some(&snapshot));
    assert_eq!(context.suggestions.as_ref(), Some(&snapshot.suggestions));
}

#[tokio::test]
async fn managed_spawner_rejects_unknown_role_escalation_and_limit_bypass() {
    let (manager, parent, sandbox) = managed_parent();
    let spawner = ManagedAgentSpawner::new(
        manager,
        ChildLimits {
            max_depth: 1,
            max_active_children_per_parent: 1,
            lease_seconds: 60,
        },
    )
    .with_role(
        "worker",
        AgentRole::Agent {
            name: "worker".to_owned(),
        },
        AgentRoleId::Worker,
        Arc::new(EmptyChildRegistry),
    );

    let unknown = spawner
        .spawn_child(
            "unregistered",
            spawn_input(parent.clone()),
            sandbox.clone(),
            None,
        )
        .await
        .expect_err("unknown roles are denied");
    assert!(unknown.message.contains("not registered"));

    let escalation = spawner
        .spawn_child(
            "worker",
            spawn_input(parent.clone()),
            test_sandbox(PermissionSet::from_policy([
                Permission::ReadWorkspace,
                Permission::WriteWorkspace,
            ])),
            None,
        )
        .await
        .expect_err("child cannot broaden parent permissions");
    assert!(escalation.message.contains("sandbox escalation"));

    let child = spawner
        .spawn_child("worker", spawn_input(parent.clone()), sandbox.clone(), None)
        .await
        .expect("first child fits limit");
    let limited = spawner
        .spawn_child("worker", spawn_input(parent), sandbox, None)
        .await
        .expect_err("second active child exceeds parent limit");
    assert!(limited.message.contains("active child limit"));

    // The record remains usable for later child-result correlation.
    assert_eq!(
        spawner.child_record(&child).expect("tracked child").depth,
        1
    );
}

/// A session admitted under the `Worker` organizational role (§3 DSL spawn
/// matrix) must never be able to spawn a durable child, even when the target
/// role is registered, the sandbox is compatible, and every other limit
/// (active-children, depth, lease) would otherwise admit it. This proves
/// `ManagedAgentSpawner::admit` enforces `harw_agent_dsl::roles::can_spawn`
/// as the first authority check, ahead of sandbox/depth/lease checks.
#[tokio::test]
async fn admit_rejects_spawn_forbidden_by_organizational_role_matrix() {
    let (manager, parent, sandbox) = managed_parent_with_organizational_role(AgentRoleId::Worker);
    let spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative()).with_role(
        "worker",
        AgentRole::Agent {
            name: "worker".to_owned(),
        },
        AgentRoleId::Worker,
        Arc::new(EmptyChildRegistry),
    );

    let rejected = spawner
        .spawn_child("worker", spawn_input(parent), sandbox, None)
        .await
        .expect_err("Worker organizational role must never spawn a Worker child");

    assert!(
        rejected.message.contains("organizational role"),
        "rejection should name the organizational-role authority check, got: {}",
        rejected.message
    );
}

#[tokio::test]
async fn managed_children_run_without_holding_the_session_registry_lock() {
    let (manager, parent, sandbox) = managed_parent();
    let spawner = ManagedAgentSpawner::new(
        manager.clone(),
        ChildLimits {
            max_depth: 2,
            max_active_children_per_parent: 2,
            lease_seconds: 60,
        },
    )
    .with_role(
        "worker",
        AgentRole::Agent {
            name: "worker".to_owned(),
        },
        AgentRoleId::Worker,
        Arc::new(BarrierChildRegistry {
            barrier: Arc::new(tokio::sync::Barrier::new(2)),
        }),
    );
    let first = spawner
        .spawn_child("worker", spawn_input(parent.clone()), sandbox.clone(), None)
        .await
        .expect("first child admitted");
    let second = spawner
        .spawn_child("worker", spawn_input(parent), sandbox, None)
        .await
        .expect("second child admitted");
    let store = InMemoryStateStore::new();

    let (left, right) = tokio::join!(
        spawner.run_child(&first, &store, TurnInput::user("first")),
        spawner.run_child(&second, &store, TurnInput::user("second")),
    );
    assert!(matches!(
        left.expect("first child runs").outcome,
        TurnOutcome::Completed
    ));
    assert!(matches!(
        right.expect("second child runs").outcome,
        TurnOutcome::Completed
    ));
    assert!(manager.lock().expect("manager lock").get(&first).is_ok());
    assert!(manager.lock().expect("manager lock").get(&second).is_ok());
}

#[tokio::test]
async fn expired_child_is_reaped_with_exact_parent_handoff_correlation() {
    let (manager, parent, sandbox) = managed_parent();
    let spawner = ManagedAgentSpawner::new(
        manager.clone(),
        ChildLimits {
            max_depth: 1,
            max_active_children_per_parent: 1,
            lease_seconds: 1,
        },
    )
    .with_role(
        "worker",
        AgentRole::Agent {
            name: "worker".to_owned(),
        },
        AgentRoleId::Worker,
        Arc::new(EmptyChildRegistry),
    );
    let input = spawn_input(parent.clone());
    let expected_call = input.handoff_call_id.clone();
    let child = spawner
        .spawn_child("worker", input, sandbox, None)
        .await
        .expect("child admitted");
    let future = jiff::Timestamp::now()
        .checked_add(jiff::SignedDuration::from_secs(2))
        .expect("future timestamp");

    let expired = spawner.reap_expired(future);
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].child, child);
    assert_eq!(expired[0].parent, parent);
    assert_eq!(expired[0].handoff_call_id, expected_call);
    assert_eq!(spawner.active_children_for(&expired[0].parent), 0);
    assert!(matches!(
        manager.lock().expect("manager lock").get(&child).expect("child session").state(),
        SessionState::Failed(reason) if reason.contains("lease expired")
    ));
}

#[tokio::test]
async fn durable_leases_survive_controller_reconciliation_and_close_as_an_audit_record() {
    let temp = tempfile::tempdir().unwrap();
    let leases = Arc::new(ChildLeaseStore::new(temp.path()));
    let (manager, parent, sandbox) = managed_parent();
    let spawner = ManagedAgentSpawner::new(
        manager,
        ChildLimits {
            max_depth: 1,
            max_active_children_per_parent: 1,
            lease_seconds: 1,
        },
    )
    .with_lease_store(leases.clone())
    .with_role(
        "worker",
        AgentRole::Agent {
            name: "worker".to_owned(),
        },
        AgentRoleId::Worker,
        Arc::new(EmptyChildRegistry),
    );
    let input = spawn_input(parent);
    let expected_call = input.handoff_call_id.clone();
    let child = spawner
        .spawn_child("worker", input, sandbox, None)
        .await
        .expect("durable admission succeeds");
    let record = leases
        .active()
        .expect("lease active")
        .pop()
        .expect("one lease");
    assert_eq!(record.child, child);
    assert_eq!(record.handoff_call_id, expected_call);

    let expired = spawner
        .reconcile_expired_leases(
            record
                .lease_expires_at
                .checked_add(jiff::SignedDuration::from_secs(1))
                .expect("future timestamp"),
        )
        .expect("reconciliation succeeds");
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].child, child);
    assert!(
        spawner
            .reconcile_expired_leases(jiff::Timestamp::now())
            .expect("second reconciliation succeeds")
            .is_empty()
    );

    spawner
        .close_child_durable(&child, jiff::Timestamp::now())
        .expect("terminal child is durably completed");
    assert!(leases.active().expect("active leases readable").is_empty());
}

#[tokio::test]
async fn late_child_completion_after_reap_is_failed_and_never_restored_as_healthy() {
    let (manager, parent, sandbox) = managed_parent();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let spawner = Arc::new(
        ManagedAgentSpawner::new(
            manager.clone(),
            ChildLimits {
                max_depth: 1,
                max_active_children_per_parent: 1,
                lease_seconds: 1,
            },
        )
        .with_role(
            "worker",
            AgentRole::Agent {
                name: "worker".to_owned(),
            },
            AgentRoleId::Worker,
            Arc::new(BlockingChildRegistry {
                entered: entered.clone(),
                release: release.clone(),
            }),
        ),
    );
    let child = spawner
        .spawn_child("worker", spawn_input(parent), sandbox, None)
        .await
        .expect("child admitted");
    let record = spawner.child_record(&child).expect("child record");
    let entered_wait = entered.notified();
    let running_spawner = spawner.clone();
    let running_child = child.clone();
    let store = Arc::new(InMemoryStateStore::new());
    let running_store = store.clone();
    let running = tokio::spawn(async move {
        running_spawner
            .run_child(
                &running_child,
                running_store.as_ref(),
                TurnInput::user("work"),
            )
            .await
    });
    entered_wait.await;

    let future = record
        .lease_expires_at
        .checked_add(jiff::SignedDuration::from_secs(1))
        .expect("future timestamp");
    assert_eq!(spawner.reap_expired(future).len(), 1);
    assert!(spawner.child_record(&child).is_none());
    let late = running.await.expect("child task joins");
    assert!(late.is_err());
    assert!(
        late.expect_err("late result rejected")
            .message
            .contains("result discarded")
    );
    assert!(matches!(
        manager.lock().expect("manager lock").get(&child).expect("restored terminal session").state(),
        SessionState::Failed(reason) if reason.contains("lease expired")
    ));
    spawner.close_child(&child);
}

// --- `clamp_child_reasoning_effort` (Wave 8: monotone reasoning-effort
// inheritance clamped/overridden after admission) -------------------------

#[tokio::test]
async fn test_clamp_child_reasoning_effort_cap_lowers_high_base_to_low() {
    let (manager, parent, sandbox) = managed_parent_with_effort(Some(ReasoningEffort::High));
    let spawner = worker_spawner(manager.clone());
    let child = spawner
        .spawn_child("worker", spawn_input(parent), sandbox, None)
        .await
        .expect("child admitted and inherits parent effort");
    assert_eq!(
        manager
            .lock()
            .expect("manager lock")
            .get(&child)
            .expect("child session")
            .reasoning_effort(),
        Some(ReasoningEffort::High)
    );

    let effective = spawner
        .clamp_child_reasoning_effort(&child, Some(ReasoningEffort::Low), None)
        .expect("known child clamps cleanly");

    assert_eq!(effective, Some(ReasoningEffort::Low));
    assert_eq!(
        manager
            .lock()
            .expect("manager lock")
            .get(&child)
            .expect("child session")
            .reasoning_effort(),
        Some(ReasoningEffort::Low)
    );
}

#[tokio::test]
async fn test_clamp_child_reasoning_effort_cap_above_low_base_keeps_base() {
    let (manager, parent, sandbox) = managed_parent_with_effort(Some(ReasoningEffort::Low));
    let spawner = worker_spawner(manager.clone());
    let child = spawner
        .spawn_child("worker", spawn_input(parent), sandbox, None)
        .await
        .expect("child admitted and inherits parent effort");

    let effective = spawner
        .clamp_child_reasoning_effort(&child, Some(ReasoningEffort::High), None)
        .expect("known child clamps cleanly");

    // A cap only ever lowers the effective level; it must never raise it
    // above the inherited base.
    assert_eq!(effective, Some(ReasoningEffort::Low));
}

#[tokio::test]
async fn test_clamp_child_reasoning_effort_no_cap_keeps_base_unchanged() {
    let (manager, parent, sandbox) = managed_parent_with_effort(Some(ReasoningEffort::Medium));
    let spawner = worker_spawner(manager.clone());
    let child = spawner
        .spawn_child("worker", spawn_input(parent), sandbox, None)
        .await
        .expect("child admitted and inherits parent effort");

    let effective = spawner
        .clamp_child_reasoning_effort(&child, None, None)
        .expect("known child clamps cleanly");

    assert_eq!(effective, Some(ReasoningEffort::Medium));
}

#[tokio::test]
async fn test_clamp_child_reasoning_effort_no_base_with_cap_falls_back_to_the_default() {
    // `managed_parent` builds a parent with no reasoning-effort level set, so
    // `admit()` never inherits a base into the child.
    let (manager, parent, sandbox) = managed_parent();
    let spawner = worker_spawner(manager.clone());
    let child = spawner
        .spawn_child("worker", spawn_input(parent), sandbox, None)
        .await
        .expect("child admitted with no inherited effort");
    assert_eq!(
        manager
            .lock()
            .expect("manager lock")
            .get(&child)
            .expect("child session")
            .reasoning_effort(),
        None
    );

    let effective = spawner
        .clamp_child_reasoning_effort(&child, Some(ReasoningEffort::High), None)
        .expect("known child clamps cleanly");

    // F-017/E3b: a missing inherited base is not a free pass. Without a base,
    // `DEFAULT_CHILD_REASONING_EFFORT` (`Medium`) stands in for it, and the
    // cap still clamps downward from there — here `min(Medium, High)` keeps
    // `Medium`, so the cap has no further effect but the default is not lost.
    assert_eq!(effective, Some(ReasoningEffort::Medium));
}

#[tokio::test]
async fn test_clamp_child_reasoning_effort_owner_override_beats_cap_and_base() {
    let (manager, parent, sandbox) = managed_parent_with_effort(Some(ReasoningEffort::Minimal));
    let spawner = worker_spawner(manager.clone());
    let child = spawner
        .spawn_child("worker", spawn_input(parent), sandbox, None)
        .await
        .expect("child admitted and inherits parent effort");

    let effective = spawner
        .clamp_child_reasoning_effort(
            &child,
            Some(ReasoningEffort::Low),
            Some(ReasoningEffort::High),
        )
        .expect("known child clamps cleanly");

    // Owner authority deliberately breaks monotonicity: it wins over both the
    // cap and the inherited base, even raising the level above both.
    assert_eq!(effective, Some(ReasoningEffort::High));
    assert_eq!(
        manager
            .lock()
            .expect("manager lock")
            .get(&child)
            .expect("child session")
            .reasoning_effort(),
        Some(ReasoningEffort::High)
    );
}

#[tokio::test]
async fn test_clamp_child_reasoning_effort_unknown_child_returns_err() {
    let (manager, _parent, _sandbox) = managed_parent();
    let spawner = worker_spawner(manager);
    let unknown = SessionId::new();

    let result = spawner.clamp_child_reasoning_effort(&unknown, Some(ReasoningEffort::Low), None);

    match result {
        Err(error) => assert!(error.message.contains("unknown child")),
        Ok(_) => panic!("clamping an unregistered child must return an error"),
    }
}
