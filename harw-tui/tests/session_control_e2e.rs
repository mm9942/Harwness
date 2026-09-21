//! End-to-end integration tests for /effort, /model, /provider session-control flow.
//!
//! # Spec reference
//! harw-tui Design §session_controller — apply_to_session + ModelRequest plumbing.
//!
//! # Strategy
//! These tests prove the full chain:
//!   `TuiSessionController::set_*(…)` →
//!   `TuiSessionController::apply_to_session(&mut session)` →
//!   `AgentSession::{reasoning_effort,active_model,active_provider}()` →
//!   `ModelRequest::{reasoning_effort,model_id,provider_id}` (mirroring turn_loop.rs ~815-822)
//!
//! Handler invocation (via `Operation::run`) is used in tests 5
//! (`slice9_model_switch_to_different_provider_is_atomic`, `#[ignore]`d — see
//! its doc comment for why) and 5b
//! (`slice9b_model_switch_to_disabled_target_provider_is_atomic_on_failure`)
//! only, where the atomic provider+model switch logic lives exclusively
//! inside the `/model switch` handler and cannot be reached through the
//! controller setters. `/provider switch <id>` no longer exists as a text
//! sub-command; `/model switch <id>` is now the sole atomic switch entry
//! point, even when the target model belongs to a different provider than
//! the one currently active.

use std::sync::Arc;

use harw_core::model::ModelRequest;
use harw_core::session::AgentSession;
use harw_extension_api::{ExtensionRegistryBuilder, LoadedInstructions};
use harw_operations::SessionController;
use harw_tui::session_controller::TuiSessionController;
use harw_types::{AgentRole, ModelId, ProviderId, ReasoningEffort};

// ── Fixtures ──────────────────────────────────────────────────────────────────

/// Builds the minimal [`AgentSession`] fixture used across all tests.
///
/// Mirrors the test-session helper already present in harw-core session.rs tests.
fn fresh_session() -> AgentSession {
    let (event_tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    AgentSession::new(
        AgentRole::Assistant,
        None,
        ExtensionRegistryBuilder::default().build(),
        event_tx,
    )
}

/// Mirrors what turn_loop.rs does at ~lines 815-822: builds a `ModelRequest`
/// that carries the session's active effort/model/provider overrides.
///
/// We use a minimal `LoadedInstructions` so `ModelRequest::new` doesn't panic.
/// The builder fields (`reasoning_effort`, `model_id`, `provider_id`) are what
/// the tests assert on.
fn build_model_request_like_turn_loop(session: &AgentSession) -> ModelRequest {
    let instructions = LoadedInstructions {
        system_prompt: String::new(),
        fragments: Vec::new(),
    };
    ModelRequest::new(
        instructions,
        Vec::new(),
        harw_core::ConversationHistory::new(),
        Vec::new(),
    )
    .with_reasoning_effort(session.reasoning_effort())
    .with_model_id(session.active_model().cloned())
    .with_provider_id(session.active_provider().cloned())
}

// ── Test 1: slice5_effort_high_influences_next_model_request ─────────────────

/// Proves that setting effort=High on the controller, then applying it to a
/// session, propagates through to the `ModelRequest` produced by the turn-loop
/// mirror helper.
///
/// Chain tested: controller.set_reasoning_effort(High) →
///   apply_to_session → session.reasoning_effort() == High →
///   ModelRequest.reasoning_effort == High
#[test]
fn slice5_effort_high_influences_next_model_request() {
    let ctrl = Arc::new(TuiSessionController::new());
    let mut session = fresh_session();

    // Precondition: session starts clean.
    assert_eq!(
        session.reasoning_effort(),
        None,
        "precondition: effort must be None initially"
    );

    ctrl.set_reasoning_effort(Some(ReasoningEffort::High))
        .expect("set_reasoning_effort must succeed");

    let applied = ctrl.apply_to_session(&mut session);
    assert!(
        applied,
        "apply_to_session must return true when mutations are pending"
    );

    // Session field asserted.
    assert_eq!(
        session.reasoning_effort(),
        Some(ReasoningEffort::High),
        "session.reasoning_effort must be High after apply"
    );

    // ModelRequest propagation asserted.
    let req = build_model_request_like_turn_loop(&session);
    assert_eq!(
        req.reasoning_effort,
        Some(ReasoningEffort::High),
        "ModelRequest.reasoning_effort must carry High from session"
    );
}

// ── Test 2: slice6_effort_clear_restores_provider_default ────────────────────

/// Proves that clearing the effort (using the "clear"/"none" semantics —
/// `set_reasoning_effort(None)`) leaves the session and ModelRequest with None.
///
/// Chain tested: set(High) → apply → set(None) → apply →
///   session.reasoning_effort() == None → ModelRequest.reasoning_effort == None
#[test]
fn slice6_effort_clear_restores_provider_default() {
    let ctrl = Arc::new(TuiSessionController::new());
    let mut session = fresh_session();

    // First, set effort to High.
    ctrl.set_reasoning_effort(Some(ReasoningEffort::High))
        .expect("initial set_reasoning_effort must succeed");
    ctrl.apply_to_session(&mut session);
    assert_eq!(
        session.reasoning_effort(),
        Some(ReasoningEffort::High),
        "precondition: effort must be High after first apply"
    );

    // Now clear it (equivalent to /effort clear).
    ctrl.set_reasoning_effort(None)
        .expect("clearing reasoning_effort must succeed");

    let applied = ctrl.apply_to_session(&mut session);
    assert!(
        applied,
        "apply_to_session must return true after a new setter call"
    );

    // Session field asserted.
    assert_eq!(
        session.reasoning_effort(),
        None,
        "session.reasoning_effort must be None after clear+apply"
    );

    // ModelRequest propagation asserted.
    let req = build_model_request_like_turn_loop(&session);
    assert_eq!(
        req.reasoning_effort, None,
        "ModelRequest.reasoning_effort must be None (provider will choose default)"
    );
}

// ── Test 3: slice7_model_switch_influences_next_turn ─────────────────────────

/// Proves that switching active_model on the controller propagates through
/// apply_to_session to the ModelRequest.
///
/// Uses "claude-opus-4-8" (a catalog-validated anthropic model). The test first
/// sets active_provider to "anthropic" so that a subsequent /model switch would
/// pass the provider-compatibility check; but since we use the setter directly,
/// no catalog check is performed here — that is the handler's concern.
///
/// Chain tested: set_active_provider("anthropic") + set_active_model("claude-opus-4-8") →
///   apply_to_session → session.active_model() == ModelId::from("claude-opus-4-8") →
///   ModelRequest.model_id == Some(ModelId::from("claude-opus-4-8"))
#[test]
fn slice7_model_switch_influences_next_turn() {
    let ctrl = Arc::new(TuiSessionController::new());
    let mut session = fresh_session();

    // Set provider first so the snapshot is self-consistent.
    ctrl.set_active_provider("anthropic".to_owned())
        .expect("set_active_provider must succeed");
    ctrl.set_active_model("claude-opus-4-8".to_owned())
        .expect("set_active_model must succeed");

    let applied = ctrl.apply_to_session(&mut session);
    assert!(
        applied,
        "apply_to_session must return true when mutations are pending"
    );

    // Session field asserted.
    let expected_model = ModelId::from("claude-opus-4-8");
    assert_eq!(
        session.active_model(),
        Some(&expected_model),
        "session.active_model must be claude-opus-4-8 after apply"
    );

    // ModelRequest propagation asserted.
    let req = build_model_request_like_turn_loop(&session);
    assert_eq!(
        req.model_id,
        Some(ModelId::from("claude-opus-4-8")),
        "ModelRequest.model_id must carry claude-opus-4-8 from session"
    );
}

// ── Test 4: slice8_provider_switch_influences_next_turn ──────────────────────

/// Proves that switching active_provider on the controller propagates through
/// apply_to_session to the ModelRequest.
///
/// Uses the setter directly (no handler invocation needed since we are testing
/// the chain, not the handler's validation logic). Provider ID is arbitrary and
/// does not need to be in the global registry for the setter path.
///
/// Chain tested: set_active_provider("test-provider-slice8") →
///   apply_to_session → session.active_provider() == ProviderId::from("test-provider-slice8") →
///   ModelRequest.provider_id == Some(ProviderId::from("test-provider-slice8"))
#[test]
fn slice8_provider_switch_influences_next_turn() {
    let ctrl = Arc::new(TuiSessionController::new());
    let mut session = fresh_session();

    ctrl.set_active_provider("test-provider-slice8".to_owned())
        .expect("set_active_provider must succeed");

    let applied = ctrl.apply_to_session(&mut session);
    assert!(
        applied,
        "apply_to_session must return true when mutations are pending"
    );

    // Session field asserted.
    let expected_provider = ProviderId::from("test-provider-slice8");
    assert_eq!(
        session.active_provider(),
        Some(&expected_provider),
        "session.active_provider must be test-provider-slice8 after apply"
    );

    // ModelRequest propagation asserted.
    let req = build_model_request_like_turn_loop(&session);
    assert_eq!(
        req.provider_id,
        Some(ProviderId::from("test-provider-slice8")),
        "ModelRequest.provider_id must carry test-provider-slice8 from session"
    );
}

// ── Test 5: slice9_model_switch_to_different_provider_is_atomic ─────────────
//
// `/provider switch <id>` no longer exists as a text sub-command (its
// catch-all now points operators at `/model`). The atomic-refuse behaviour
// this slice used to prove on `/provider switch` moved entirely onto
// `/model switch <id>`: harw-ops/src/model.rs's `handle_switch_core` resolves
// the target model's configured provider and delegates fully to
// harw-ops/src/provider.rs's `handle_switch_core`, which now switches
// provider+model together in one atomic call, even when the target model
// belongs to a different provider than the one currently active — it no
// longer rejects with a "/provider switch ... first" hint on mismatch.

/// Builds the shared fixture for both `slice9*` tests below: a temp
/// workspace/sandbox, a `TuiSessionController` seeded with active
/// provider "anthropic" / active model "claude-opus-4-8", and a
/// context-scoped `ResolvedConfig` with two providers — "anthropic" (holding
/// "claude-opus-4-8") and "openai" (holding "gpt-test-slice9", whose
/// `enabled` flag is controlled by `openai_enabled` so the same builder
/// serves both the atomic-success case and the atomic-failure/atomicity-
/// preserved case).
///
/// # Returns
/// `(OpContext, tmp_dir, controller)`. The caller is responsible for
/// removing `tmp_dir` (best effort) once done, and for snapshotting
/// `controller` directly (the context only holds a type-erased clone of it).
fn build_slice9_fixture(
    openai_enabled: bool,
) -> (
    harw_operations::OpContext,
    std::path::PathBuf,
    Arc<TuiSessionController>,
) {
    use harw_config::{ModelToml, ProviderToml, ResolvedConfig, SecretRef};
    use harw_operations::{SharedSessionController, context::ServiceMap};
    use harw_authority::{Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};

    // ── Build a minimal OpContext with a real SandboxSpec ─────────────────────
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("harw-slice9-{}-{}", std::process::id(), id));
    std::fs::create_dir_all(tmp.join("ws")).expect("create tmp workspace dir");

    let ws_registry = WorkspaceRegistry::build(
        &tmp,
        [WorkspaceRegistration {
            tenant: TenantId::from_str("test-tenant"),
            workspace: WorkspaceId::from_str("ws"),
            root: std::path::PathBuf::from("ws"),
        }],
    )
    .expect("WorkspaceRegistry::build must succeed");

    let binding = ws_registry
        .resolve(
            &TenantId::from_str("test-tenant"),
            &WorkspaceId::from_str("ws"),
        )
        .expect("workspace binding must resolve");

    let sandbox = SandboxSpec::from_resolved(
        binding,
        PermissionSet::from_policy([Permission::ReadWorkspace]),
    );

    // ── Compose the resolved provider configuration for this operation ───────
    // SecretRef values are references only; no credential material is stored in
    // the test fixture.
    let mut config = ResolvedConfig::default();
    config.providers.insert(
        "openai".to_owned(),
        ProviderToml {
            name: "openai".to_owned(),
            api: "openai-chat".to_owned(),
            base_url: "https://api.openai.com/v1".to_owned(),
            auth: Some(SecretRef::Env("HARW_TEST_OPENAI_KEY".to_owned())),
            auth_header: None,
            api_key: None,
            headers: Default::default(),
            models: vec!["gpt-test-slice9".to_owned()],
            enabled: openai_enabled,
            origin_allowlist: Default::default(),
            rate_limit: None,
            max_concurrency: None,
            originator: None,
        },
    );
    config.providers.insert(
        "anthropic".to_owned(),
        ProviderToml {
            name: "anthropic".to_owned(),
            api: "anthropic-messages".to_owned(),
            base_url: "https://api.anthropic.com/v1".to_owned(),
            auth: Some(SecretRef::Env("HARW_TEST_ANTHROPIC_KEY".to_owned())),
            auth_header: None,
            api_key: None,
            headers: Default::default(),
            models: vec!["claude-opus-4-8".to_owned()],
            enabled: true,
            origin_allowlist: Default::default(),
            rate_limit: None,
            max_concurrency: None,
            originator: None,
        },
    );
    config.models.insert(
        "gpt-test-slice9".to_owned(),
        ModelToml {
            id: "gpt-test-slice9".to_owned(),
            name: None,
            provider: "openai".to_owned(),
            aliases: Vec::new(),
            context_window: None,
            max_tokens: None,
            reasoning: false,
            input_types: Vec::new(),
            capabilities: Default::default(),
            prompt_caching: None,
        },
    );
    config.models.insert(
        "claude-opus-4-8".to_owned(),
        ModelToml {
            id: "claude-opus-4-8".to_owned(),
            name: None,
            provider: "anthropic".to_owned(),
            aliases: Vec::new(),
            context_window: None,
            max_tokens: None,
            reasoning: false,
            input_types: Vec::new(),
            capabilities: Default::default(),
            prompt_caching: None,
        },
    );

    // ── Set up TuiSessionController with anthropic provider + anthropic model ─
    let ctrl = Arc::new(TuiSessionController::new());
    ctrl.set_active_provider("anthropic".to_owned())
        .expect("set_active_provider must succeed");
    ctrl.set_active_model("claude-opus-4-8".to_owned())
        .expect("set_active_model must succeed");

    // ── Build OpContext with the controller ───────────────────────────────────
    let shared: SharedSessionController = Arc::clone(&ctrl) as SharedSessionController;
    let mut services = ServiceMap::new();
    services.insert(shared);
    services.insert(Arc::new(config));
    let ctx = harw_operations::OpContext::new(SessionId::new(), TurnId::new(), sandbox, services);

    (ctx, tmp, ctrl)
}

/// Proves the atomic-switch success path: `/model switch <id>`, invoked via
/// `ModelOperation::run()` (the `Operation` trait — the actual handler code
/// path, not the controller setters already covered by slice7/8), switches
/// provider+model together in a single call when the target model
/// ("gpt-test-slice9") belongs to a *different* configured provider
/// ("openai") than the one currently active ("anthropic").
///
/// Precondition: active_provider = "anthropic", active_model = "claude-opus-4-8"
/// Action:       /model switch gpt-test-slice9   (target provider "openai" enabled)
/// Assert:       Ok(...) AND snapshot_after.active_provider == "openai" AND
///               snapshot_after.active_model == "gpt-test-slice9"
///
/// # Ignored — real, non-injectable filesystem I/O
/// A successful switch through `ModelOperation::run()` always reaches
/// `harw-ops::provider::handle_switch_core`'s Step 5, which calls
/// `harw-ops::config_util::persist_default_selection` — hardcoded at the
/// `/model switch` call site in `harw-ops/src/model.rs`, with no
/// test-injectable seam through the public `Operation` surface (unlike
/// `resolved_config`, which *does* prefer a context-scoped config — see
/// `build_slice9_fixture` above). `persist_default_selection` resolves
/// `$HARW_HOME` (default `~/.harw`) via `harw_home::home_dir()` and, on a
/// successful switch, always writes `default_provider`/`default_model` into
/// the *active profile's real* `config.toml` — even on failure to persist it
/// swallows the error into a text note rather than an `Err`, so this is not
/// observable as a test failure, only as a live side effect.
///
/// In-crate unit tests avoid this by calling the private
/// `handle_switch_core`/`handle_uia_switch_core` core functions directly
/// with a no-op `persist` closure (see
/// `harw-ops/src/model.rs::tests::model_switch_to_different_provider_switches_both_atomically`);
/// that seam is `pub(crate)`/private and unreachable from this external
/// `harw-tui` integration-test crate. Redirecting `$HARW_HOME` into this
/// test's own tmp dir via `std::env::set_var` is also unavailable: this
/// workspace declares `[workspace.lints.rust] unsafe_code = "forbid"`
/// (root `Cargo.toml`), inherited by `harw-tui` via `[lints] workspace =
/// true`, and `std::env::set_var`/`remove_var` require an `unsafe` block on
/// current Rust.
///
/// Run unignored, this test would silently overwrite `default_provider`/
/// `default_model` in the developer's real, active harwness profile on every
/// local `cargo test` — a live-filesystem side effect this project's own
/// Rust testing rules forbid outside `#[ignore]` ("Tests must not rely on
/// ... the filesystem unless marked `#[ignore]` with a comment explaining
/// the requirement"). Run explicitly via `cargo test -- --ignored` only with
/// `HARW_HOME` pointed at a throwaway directory.
#[tokio::test]
#[ignore = "reaches the real, non-injectable persist_default_selection and would \
            overwrite the developer's live ~/.harw profile config.toml; see doc comment"]
async fn slice9_model_switch_to_different_provider_is_atomic() {
    use harw_operations::{OpInput, Operation};

    let (ctx, tmp, ctrl) = build_slice9_fixture(true);

    // ── Invoke ModelOperation::run() with ["switch", "gpt-test-slice9"] ───────
    let op = harw_ops::model::ModelOperation;
    let input = OpInput::command(
        "/model",
        vec!["switch".to_owned(), "gpt-test-slice9".to_owned()],
    );
    let result = op.run(&ctx, input).await;

    match result {
        Ok(_) => {}
        other => panic!(
            "Expected Ok for a cross-provider atomic /model switch, got: {other:?}"
        ),
    }

    // ATOMIC SUCCESS: both provider and model must have switched together.
    let snapshot_after = ctrl.snapshot();
    assert_eq!(
        snapshot_after.active_provider.as_deref(),
        Some("openai"),
        "active_provider must have switched atomically alongside the model"
    );
    assert_eq!(
        snapshot_after.active_model.as_deref(),
        Some("gpt-test-slice9"),
        "active_model must be the requested target"
    );

    // Cleanup tmp dir (best effort).
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Proves the handler-level atomicity guard still holds on the *failure*
/// side of `/model switch <id>`: when the target model's configured provider
/// is disabled, the switch must be rejected with `InvalidArguments` *before*
/// touching the controller — the rejection happens in
/// `provider::handle_switch_core`'s Step 2 (enabled check), ahead of Step 4
/// (controller mutation) and Step 5 (persist), so this path never reaches
/// the filesystem and needs no `#[ignore]`.
///
/// Precondition: active_provider = "anthropic", active_model = "claude-opus-4-8"
/// Action:       /model switch gpt-test-slice9   (target provider "openai" DISABLED)
/// Assert:       Err(InvalidArguments) AND snapshot is byte-for-byte unchanged
#[tokio::test]
async fn slice9b_model_switch_to_disabled_target_provider_is_atomic_on_failure() {
    use harw_operations::{OpInput, Operation};

    let (ctx, tmp, ctrl) = build_slice9_fixture(false);
    let snapshot_before = ctrl.snapshot();

    // ── Invoke ModelOperation::run() with ["switch", "gpt-test-slice9"] ───────
    let op = harw_ops::model::ModelOperation;
    let input = OpInput::command(
        "/model",
        vec!["switch".to_owned(), "gpt-test-slice9".to_owned()],
    );
    let result = op.run(&ctx, input).await;

    match result {
        Err(harw_operations::OpError::InvalidArguments(msg)) => {
            assert!(
                msg.contains("openai") || msg.contains("disabled"),
                "error must mention the disabled target provider: {msg}"
            );
        }
        other => {
            panic!("Expected InvalidArguments for a disabled target provider, got: {other:?}")
        }
    }

    // ATOMICITY ON FAILURE: controller snapshot must be unchanged.
    let snapshot_after = ctrl.snapshot();
    assert_eq!(
        snapshot_after.active_provider, snapshot_before.active_provider,
        "active_provider must not change after a rejected switch"
    );
    assert_eq!(
        snapshot_after.active_model, snapshot_before.active_model,
        "active_model must not change after a rejected switch"
    );

    // Cleanup tmp dir (best effort).
    let _ = std::fs::remove_dir_all(&tmp);
}

// ── Test 6: slice10_controller_state_survives_multiple_commands ──────────────

/// Proves that sequential mutations accumulate correctly and all three fields
/// (effort, model, provider) are propagated together in a single apply.
///
/// Chain tested: set(effort) → set(model) → set(provider) →
///   apply_to_session (single call) →
///   all three session fields set → ModelRequest carries all three
#[test]
fn slice10_controller_state_survives_multiple_commands() {
    let ctrl = Arc::new(TuiSessionController::new());
    let mut session = fresh_session();

    // Three sequential mutations — each bumps the generation counter.
    ctrl.set_reasoning_effort(Some(ReasoningEffort::Medium))
        .expect("set_reasoning_effort must succeed");
    ctrl.set_active_model("claude-opus-4-8".to_owned())
        .expect("set_active_model must succeed");
    ctrl.set_active_provider("anthropic".to_owned())
        .expect("set_active_provider must succeed");

    // Snapshot must reflect all three before apply.
    let snap = ctrl.snapshot();
    assert_eq!(snap.reasoning_effort, Some(ReasoningEffort::Medium));
    assert_eq!(snap.active_model.as_deref(), Some("claude-opus-4-8"));
    assert_eq!(snap.active_provider.as_deref(), Some("anthropic"));

    // Single apply drains all pending mutations.
    let applied = ctrl.apply_to_session(&mut session);
    assert!(
        applied,
        "apply_to_session must return true after setter calls"
    );

    // All three session fields must carry through.
    assert_eq!(session.reasoning_effort(), Some(ReasoningEffort::Medium));
    assert_eq!(
        session.active_model(),
        Some(&ModelId::from("claude-opus-4-8"))
    );
    assert_eq!(
        session.active_provider(),
        Some(&ProviderId::from("anthropic"))
    );

    // ModelRequest must carry all three fields.
    let req = build_model_request_like_turn_loop(&session);
    assert_eq!(
        req.reasoning_effort,
        Some(ReasoningEffort::Medium),
        "ModelRequest.reasoning_effort must carry Medium"
    );
    assert_eq!(
        req.model_id,
        Some(ModelId::from("claude-opus-4-8")),
        "ModelRequest.model_id must carry claude-opus-4-8"
    );
    assert_eq!(
        req.provider_id,
        Some(ProviderId::from("anthropic")),
        "ModelRequest.provider_id must carry anthropic"
    );

    // Second apply must be a no-op (generation == applied_generation).
    let second_applied = ctrl.apply_to_session(&mut session);
    assert!(
        !second_applied,
        "second apply without new mutations must be a no-op"
    );
}

// ── Test 7: slice11_second_session_is_isolated ───────────────────────────────

/// Proves that two independent `Arc<TuiSessionController>` instances have
/// completely isolated state — mutations on A do not bleed into B's session.
///
/// This ensures `Arc` is wrapping a fresh `TuiSessionController` each time,
/// not a shared singleton.
#[test]
fn slice11_second_session_is_isolated() {
    let ctrl_a = Arc::new(TuiSessionController::new());
    let ctrl_b = Arc::new(TuiSessionController::new());

    // Mutate A only.
    ctrl_a
        .set_reasoning_effort(Some(ReasoningEffort::High))
        .expect("set_reasoning_effort on A must succeed");
    ctrl_a
        .set_active_model("claude-opus-4-8".to_owned())
        .expect("set_active_model on A must succeed");
    ctrl_a
        .set_active_provider("anthropic".to_owned())
        .expect("set_active_provider on A must succeed");

    // B's snapshot must still be all-None.
    let snap_b = ctrl_b.snapshot();
    assert!(
        snap_b.reasoning_effort.is_none(),
        "B.reasoning_effort must be None — isolation violated"
    );
    assert!(
        snap_b.active_model.is_none(),
        "B.active_model must be None — isolation violated"
    );
    assert!(
        snap_b.active_provider.is_none(),
        "B.active_provider must be None — isolation violated"
    );

    // apply_to_session on B's fresh session must leave all fields None.
    let mut session_b = fresh_session();
    let modified = ctrl_b.apply_to_session(&mut session_b);
    assert!(
        !modified,
        "apply_to_session on unmodified B must be a no-op"
    );

    assert_eq!(
        session_b.reasoning_effort(),
        None,
        "session_b.reasoning_effort must be None"
    );
    assert_eq!(
        session_b.active_model(),
        None,
        "session_b.active_model must be None"
    );
    assert_eq!(
        session_b.active_provider(),
        None,
        "session_b.active_provider must be None"
    );

    // ModelRequest built from B's session must carry all-None.
    let req_b = build_model_request_like_turn_loop(&session_b);
    assert_eq!(req_b.reasoning_effort, None);
    assert_eq!(req_b.model_id, None);
    assert_eq!(req_b.provider_id, None);

    // A's session must still carry everything (verify A's apply still works).
    let mut session_a = fresh_session();
    ctrl_a.apply_to_session(&mut session_a);
    assert_eq!(session_a.reasoning_effort(), Some(ReasoningEffort::High));
    assert_eq!(
        session_a.active_model(),
        Some(&ModelId::from("claude-opus-4-8"))
    );
    assert_eq!(
        session_a.active_provider(),
        Some(&ProviderId::from("anthropic"))
    );
}
