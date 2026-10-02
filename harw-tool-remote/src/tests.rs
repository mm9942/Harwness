//! Tests des entfernten Werkzeug-Proxys (Vertrag R18 §12, P2: RP-T1..RP-T4).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use harw_authority::{
    Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
};
use harw_extension_api::contributors::ToolProvider;
use harw_protocol::items::{ResultTrust, ToolCallResult, ToolPlacement};
use harw_protocol::session_port::{PortError, PortFuture, ToolPort, ToolRefusal};
use harw_protocol::session_wire::{
    ToolApproval, ToolCallParams, ToolCallResultFrame, ToolCancelParams, ToolDescriptor,
    ToolListParams, ToolListResult,
};
use harw_tools::executor::{ExecutionPlacement, ToolExecutionContext, ToolExecutor};
use harw_tools::{ToolCall, ToolName, ToolOutput, ToolsError};
use harw_types::cancel::{CancelReason, CancelToken};
use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};

use crate::test_support::{TestError, TestResult, ctx};
use crate::{RemoteToolError, RemoteToolProvider, refusal_message};

/// Was die Attrappe auf `tool.call` antwortet.
#[derive(Clone)]
enum CallBehavior {
    /// Eine Ergebnis-Frame für genau den gesendeten Aufruf.
    Frame {
        result: ToolCallResult,
        placement: ToolPlacement,
    },
    /// Eine Ablehnung vor der Ausführung.
    Refuse(PortError),
    /// Antwortet nie (für Abbruch-Tests).
    Hang,
}

/// `ToolPort`-Attrappe: zählt Aufrufe, merkt sich Parameter und
/// `tool.cancel`s.
struct FakePort {
    descriptors: Vec<ToolDescriptor>,
    list_error: Option<PortError>,
    behavior: CallBehavior,
    calls: Mutex<Vec<ToolCallParams>>,
    cancels: Mutex<Vec<ToolCancelParams>>,
    entered: AtomicBool,
}

impl FakePort {
    fn new(descriptors: Vec<ToolDescriptor>, behavior: CallBehavior) -> Arc<Self> {
        Arc::new(Self {
            descriptors,
            list_error: None,
            behavior,
            calls: Mutex::new(Vec::new()),
            cancels: Mutex::new(Vec::new()),
            entered: AtomicBool::new(false),
        })
    }

    fn calls(&self) -> Vec<ToolCallParams> {
        self.calls
            .lock()
            .map(|calls| calls.clone())
            .unwrap_or_default()
    }

    fn cancels(&self) -> Vec<ToolCancelParams> {
        self.cancels
            .lock()
            .map(|cancels| cancels.clone())
            .unwrap_or_default()
    }
}

impl ToolPort for FakePort {
    fn list_tools(&self, _params: ToolListParams) -> PortFuture<'_, ToolListResult> {
        let answer = match &self.list_error {
            Some(error) => Err(error.clone()),
            None => Ok(ToolListResult {
                tools: self.descriptors.clone(),
            }),
        };
        Box::pin(async move { answer })
    }

    fn call_tool(&self, params: ToolCallParams) -> PortFuture<'_, ToolCallResultFrame> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(params.clone());
        }
        self.entered.store(true, Ordering::SeqCst);
        let behavior = self.behavior.clone();
        Box::pin(async move {
            match behavior {
                CallBehavior::Frame { result, placement } => Ok(ToolCallResultFrame {
                    call_id: params.call_id,
                    result,
                    placement,
                    duration_ms: 7,
                    trust: ResultTrust::Untrusted,
                }),
                CallBehavior::Refuse(error) => Err(error),
                CallBehavior::Hang => std::future::pending().await,
            }
        })
    }

    fn cancel_tool(&self, params: ToolCancelParams) -> PortFuture<'_, ()> {
        if let Ok(mut cancels) = self.cancels.lock() {
            cancels.push(params);
        }
        Box::pin(async { Ok(()) })
    }
}

fn descriptor(name: &str, parallel_safe: bool) -> ToolDescriptor {
    ToolDescriptor {
        name: name.to_owned(),
        description: format!("{name} (gateway)"),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"],
        }),
        approval: ToolApproval::Policy,
        placement: ToolPlacement::Gateway {
            node: Some("gw-1".to_owned()),
        },
        parallel_safe,
    }
}

fn gateway_session() -> SessionId {
    SessionId::from_str("gateway-session")
}

fn make_ctx(test_id: &str) -> TestResult<ToolExecutionContext> {
    let base = std::env::temp_dir()
        .join("harw_tool_remote_tests")
        .join(test_id);
    let ws = base.join("ws");
    std::fs::create_dir_all(&ws).map_err(ctx("Workspace-Verzeichnis anlegen"))?;
    let registry = WorkspaceRegistry::build(
        &base,
        [WorkspaceRegistration {
            tenant: TenantId::from_str("t"),
            workspace: WorkspaceId::from_str("w"),
            root: PathBuf::from("ws"),
        }],
    )
    .map_err(ctx("WorkspaceRegistry::build"))?;
    let binding = registry
        .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("w"))
        .map_err(ctx("registry.resolve"))?;
    let spec = SandboxSpec::from_resolved(
        binding,
        PermissionSet::from_policy(vec![Permission::ReadWorkspace]),
    );
    Ok(ToolExecutionContext::new(
        SessionId::new(),
        TurnId::new(),
        spec,
    ))
}

fn fs_read_call() -> ToolCall {
    ToolCall {
        id: ToolCallId::new(),
        name: ToolName::new("fs.read"),
        arguments: serde_json::json!({ "path": "README.md" }),
    }
}

async fn provider_for(port: &Arc<FakePort>) -> TestResult<RemoteToolProvider> {
    let port: Arc<dyn ToolPort> = Arc::clone(port) as Arc<dyn ToolPort>;
    RemoteToolProvider::connect(port, gateway_session())
        .await
        .map_err(ctx("RemoteToolProvider::connect"))
}

fn executor(provider: &RemoteToolProvider, name: &str) -> TestResult<Arc<dyn ToolExecutor>> {
    provider
        .executor(&ToolName::new(name))
        .ok_or(TestError::Missing("executor for a listed tool"))
}

// ---------------------------------------------------------------------------
// RP-T1: exakt die Deskriptoren des Ports
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rp_t1_provider_lists_exactly_the_port_descriptors() -> TestResult {
    let port = FakePort::new(
        vec![descriptor("fs.read", true), descriptor("shell.exec", false)],
        CallBehavior::Hang,
    );

    let provider = provider_for(&port).await?;

    assert_eq!(provider.tool_names(), ["fs.read", "shell.exec"]);
    assert_eq!(provider.session_id(), &gateway_session());
    let specs = provider.tools();
    assert_eq!(specs.len(), 2);
    let harw_tools::ToolSpec::Function(first) =
        specs.first().ok_or(TestError::Missing("first spec"))?;
    assert_eq!(first.description, "fs.read (gateway)");
    assert_eq!(first.parameters.required, Some(vec!["path".to_owned()]));
    assert!(provider.parallel_safe(&ToolName::new("fs.read")));
    assert!(!provider.parallel_safe(&ToolName::new("shell.exec")));
    // Nichts außer den gelisteten Werkzeugen ist erreichbar.
    assert!(provider.executor(&ToolName::new("fs.write")).is_none());
    assert!(!provider.parallel_safe(&ToolName::new("fs.write")));
    // `tool.list` baut nur; es wird nichts aufgerufen.
    assert!(port.calls().is_empty());
    Ok(())
}

#[tokio::test]
async fn rp_t1_invalid_descriptors_reject_the_whole_set() -> TestResult {
    let mut host_placed = descriptor("shell.exec", false);
    host_placed.placement = ToolPlacement::Host;
    let mut scalar_schema = descriptor("fs.read", false);
    scalar_schema.input_schema = serde_json::json!("object");
    let cases = [
        vec![descriptor("fs.read", false), host_placed],
        vec![descriptor("fs.read", false), descriptor("fs.read", true)],
        vec![scalar_schema],
        vec![descriptor(" ", false)],
    ];
    for descriptors in cases {
        let port: Arc<dyn ToolPort> = FakePort::new(descriptors, CallBehavior::Hang);
        let built = RemoteToolProvider::connect(port, gateway_session()).await;
        assert!(
            matches!(built, Err(RemoteToolError::InvalidDescriptor { .. })),
            "an invalid descriptor must reject the set"
        );
    }
    Ok(())
}

#[tokio::test]
async fn rp_t1_list_refusal_is_a_typed_error() -> TestResult {
    let port = Arc::new(FakePort {
        descriptors: Vec::new(),
        list_error: Some(PortError::Denied("no tool_call cap".to_owned())),
        behavior: CallBehavior::Hang,
        calls: Mutex::new(Vec::new()),
        cancels: Mutex::new(Vec::new()),
        entered: AtomicBool::new(false),
    });
    let port: Arc<dyn ToolPort> = port;

    let built = RemoteToolProvider::connect(port, gateway_session()).await;

    assert!(matches!(
        built,
        Err(RemoteToolError::List(PortError::Denied(_)))
    ));
    Ok(())
}

// ---------------------------------------------------------------------------
// RP-T2: Ergebnis und Platzierung der Frame
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rp_t2_call_returns_the_frame_result_and_placement() -> TestResult {
    let port = FakePort::new(
        vec![descriptor("fs.read", false)],
        CallBehavior::Frame {
            result: ToolCallResult::success(serde_json::json!({ "content": "hello" })),
            placement: ToolPlacement::Gateway {
                node: Some("gw-7".to_owned()),
            },
        },
    );
    let provider = provider_for(&port).await?;
    let executor = executor(&provider, "fs.read")?;
    let context = make_ctx("rp_t2")?;
    let call = fs_read_call();

    let placed = executor.execute_placed(&context, &call).await;

    match placed.output {
        Ok(ToolOutput::Json { content }) => {
            assert_eq!(content, serde_json::json!({ "content": "hello" }));
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "expected the frame's JSON result, got {other:?}"
            )));
        }
    }
    assert_eq!(
        placed.placement,
        Some(ExecutionPlacement::Gateway {
            node: Some("gw-7".to_owned())
        }),
        "the frame's placement wins over the descriptor's node"
    );
    let sent = port.calls();
    assert_eq!(sent.len(), 1);
    let params = sent.first().ok_or(TestError::Missing("sent tool.call"))?;
    assert_eq!(params.session_id, gateway_session());
    assert_eq!(&params.turn_id, context.turn_id());
    assert_eq!(params.call_id, call.id);
    assert_eq!(params.tool_name, "fs.read");
    assert_eq!(params.arguments, call.arguments);
    assert_eq!(params.parent_call_id, None);
    assert!(port.cancels().is_empty());
    assert_eq!(
        executor.placement(),
        Some(ExecutionPlacement::Gateway {
            node: Some("gw-1".to_owned())
        })
    );
    Ok(())
}

#[tokio::test]
async fn rp_t2_text_and_error_results_keep_their_shape() -> TestResult {
    for (result, expect_error) in [
        (
            ToolCallResult::success(serde_json::json!("plain text")),
            false,
        ),
        (ToolCallResult::error("exit 1"), true),
    ] {
        let port = FakePort::new(
            vec![descriptor("fs.read", false)],
            CallBehavior::Frame {
                result,
                placement: ToolPlacement::Gateway { node: None },
            },
        );
        let provider = provider_for(&port).await?;
        let output = executor(&provider, "fs.read")?
            .execute(&make_ctx("rp_t2_shapes")?, &fs_read_call())
            .await
            .map_err(ctx("execute"))?;
        match (output, expect_error) {
            (ToolOutput::Text { content }, false) => assert_eq!(content, "plain text"),
            (ToolOutput::Error { message }, true) => assert_eq!(message, "exit 1"),
            (other, _) => {
                return Err(TestError::Unexpected(format!("unexpected shape {other:?}")));
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn rp_t2_unknown_frame_placement_stays_unknown() -> TestResult {
    let port = FakePort::new(
        vec![descriptor("fs.read", false)],
        CallBehavior::Frame {
            result: ToolCallResult::success(serde_json::json!({})),
            placement: ToolPlacement::Unknown,
        },
    );
    let provider = provider_for(&port).await?;

    let placed = executor(&provider, "fs.read")?
        .execute_placed(&make_ctx("rp_t2_unknown")?, &fs_read_call())
        .await;

    assert!(placed.output.is_ok());
    assert_eq!(placed.placement, None);
    Ok(())
}

// ---------------------------------------------------------------------------
// RP-T3: jede Ablehnung wird zu einem benannten Fehlerergebnis
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rp_t3_every_refusal_is_an_error_result_naming_it() -> TestResult {
    let mut refusals: Vec<(PortError, &str)> = vec![
        (PortError::Denied("caps".to_owned()), "denied"),
        (PortError::NotFound, "not_found"),
        (PortError::Revoked, "revoked"),
        (PortError::Protocol("bad".to_owned()), "protocol"),
        (PortError::Transport("closed".to_owned()), "transport"),
    ];
    for refusal in ToolRefusal::ALL {
        let name = match refusal {
            ToolRefusal::UnknownTool => "tool_unknown",
            ToolRefusal::NotGranted => "tool_not_granted",
            ToolRefusal::SandboxUnavailable => "tool_sandbox_unavailable",
            ToolRefusal::Draining => "host_draining",
            ToolRefusal::DuplicateCall => "tool_call_duplicate",
        };
        refusals.push((
            PortError::ToolRefused {
                refusal,
                detail: "fs.read".to_owned(),
            },
            name,
        ));
    }
    for (error, name) in refusals {
        let port = FakePort::new(
            vec![descriptor("fs.read", false)],
            CallBehavior::Refuse(error.clone()),
        );
        let provider = provider_for(&port).await?;

        let placed = executor(&provider, "fs.read")?
            .execute_placed(&make_ctx("rp_t3")?, &fs_read_call())
            .await;

        match placed.output {
            Ok(ToolOutput::Error { message }) => {
                assert!(message.contains(name), "{message}");
                assert!(message.contains(&error.code().to_string()), "{message}");
                assert!(message.contains("no local fallback"), "{message}");
                assert_eq!(message, refusal_message("fs.read", &error));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "refusal {name} must be an error result, got {other:?}"
                )));
            }
        }
        assert_eq!(placed.placement, None, "a refused call ran nowhere");
        assert_eq!(port.calls().len(), 1, "exactly the one forwarded call");
        assert!(port.cancels().is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn rp_t3_a_frame_for_another_call_is_discarded() -> TestResult {
    /// Antwortet mit einer fremden `call_id`.
    struct WrongCallPort;

    impl ToolPort for WrongCallPort {
        fn list_tools(&self, _params: ToolListParams) -> PortFuture<'_, ToolListResult> {
            Box::pin(async {
                Ok(ToolListResult {
                    tools: vec![descriptor("fs.read", false)],
                })
            })
        }

        fn call_tool(&self, _params: ToolCallParams) -> PortFuture<'_, ToolCallResultFrame> {
            Box::pin(async {
                Ok(ToolCallResultFrame {
                    call_id: ToolCallId::new(),
                    result: ToolCallResult::success(serde_json::json!({ "secret": true })),
                    placement: ToolPlacement::Gateway { node: None },
                    duration_ms: 1,
                    trust: ResultTrust::Untrusted,
                })
            })
        }

        fn cancel_tool(&self, _params: ToolCancelParams) -> PortFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    let port: Arc<dyn ToolPort> = Arc::new(WrongCallPort);
    let provider = RemoteToolProvider::connect(port, gateway_session())
        .await
        .map_err(ctx("connect"))?;

    let placed = executor(&provider, "fs.read")?
        .execute_placed(&make_ctx("rp_t3_wrong_call")?, &fs_read_call())
        .await;

    assert!(
        matches!(placed.output, Ok(ToolOutput::Error { .. })),
        "{:?}",
        placed.output
    );
    assert_eq!(placed.placement, None);
    Ok(())
}

#[tokio::test]
async fn rp_t3_a_call_for_another_tool_is_never_forwarded() -> TestResult {
    let port = FakePort::new(
        vec![descriptor("fs.read", false)],
        CallBehavior::Frame {
            result: ToolCallResult::success(serde_json::json!({})),
            placement: ToolPlacement::Gateway { node: None },
        },
    );
    let provider = provider_for(&port).await?;
    let mut call = fs_read_call();
    call.name = ToolName::new("shell.exec");

    let output = executor(&provider, "fs.read")?
        .execute(&make_ctx("rp_t3_other_tool")?, &call)
        .await
        .map_err(ctx("execute"))?;

    assert!(matches!(output, ToolOutput::Error { .. }), "{output:?}");
    assert!(port.calls().is_empty());
    Ok(())
}

// ---------------------------------------------------------------------------
// RP-T4: Abbruch sendet `tool.cancel` genau einmal
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rp_t4_cancel_token_sends_tool_cancel_once() -> TestResult {
    let port = FakePort::new(vec![descriptor("fs.read", false)], CallBehavior::Hang);
    let provider = provider_for(&port).await?;
    let executor = executor(&provider, "fs.read")?;
    let token = CancelToken::new();
    let context = make_ctx("rp_t4_observed")?.with_cancel(token.clone());
    let call = fs_read_call();

    let canceller = async {
        while !port.entered.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        token.cancel(CancelReason::User);
    };
    let (placed, ()) = tokio::join!(executor.execute_placed(&context, &call), canceller);

    assert!(matches!(placed.output, Err(ToolsError::Cancelled)));
    assert_eq!(placed.placement, None);
    let cancels = port.cancels();
    assert_eq!(cancels.len(), 1, "tool.cancel exactly once");
    let cancel = cancels.first().ok_or(TestError::Missing("tool.cancel"))?;
    assert_eq!(cancel.call_id, call.id);
    assert_eq!(cancel.session_id, gateway_session());
    // Kein zweites `tool.cancel` aus dem Wächter, auch nicht verspätet.
    tokio::task::yield_now().await;
    assert_eq!(port.cancels().len(), 1);
    Ok(())
}

#[tokio::test]
async fn rp_t4_dropping_an_in_flight_call_sends_tool_cancel_once() -> TestResult {
    let port = FakePort::new(vec![descriptor("fs.read", false)], CallBehavior::Hang);
    let provider = provider_for(&port).await?;
    let executor = executor(&provider, "fs.read")?;
    let context = make_ctx("rp_t4_dropped")?.with_cancel(CancelToken::new());
    let call = fs_read_call();

    // Wie der Turn-Loop: das laufende Future wird verworfen, bevor der
    // Ausführer den Abbruch selbst sieht.
    {
        let mut in_flight = executor.execute_placed(&context, &call);
        let polled =
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut in_flight).await;
        assert!(polled.is_err(), "the hanging call must still be in flight");
    }
    let mut spins = 0;
    while port.cancels().is_empty() && spins < 100 {
        tokio::task::yield_now().await;
        spins += 1;
    }

    let cancels = port.cancels();
    assert_eq!(cancels.len(), 1, "tool.cancel exactly once after the drop");
    let cancel = cancels.first().ok_or(TestError::Missing("tool.cancel"))?;
    assert_eq!(cancel.call_id, call.id);
    Ok(())
}

#[tokio::test]
async fn rp_t4_finished_and_precancelled_calls_send_no_cancel() -> TestResult {
    let port = FakePort::new(
        vec![descriptor("fs.read", false)],
        CallBehavior::Frame {
            result: ToolCallResult::success(serde_json::json!({})),
            placement: ToolPlacement::Gateway { node: None },
        },
    );
    let provider = provider_for(&port).await?;
    let executor = executor(&provider, "fs.read")?;

    let finished = executor
        .execute_placed(
            &make_ctx("rp_t4_finished")?.with_cancel(CancelToken::new()),
            &fs_read_call(),
        )
        .await;
    assert!(finished.output.is_ok());

    let token = CancelToken::new();
    token.cancel(CancelReason::User);
    let precancelled = executor
        .execute_placed(
            &make_ctx("rp_t4_precancelled")?.with_cancel(token),
            &fs_read_call(),
        )
        .await;
    assert!(matches!(precancelled.output, Err(ToolsError::Cancelled)));

    tokio::task::yield_now().await;
    assert_eq!(port.calls().len(), 1, "a pre-cancelled call is never sent");
    assert!(port.cancels().is_empty());
    Ok(())
}

#[test]
fn refusal_message_names_tool_kind_and_code() {
    let message = refusal_message(
        "shell.exec",
        &PortError::ToolRefused {
            refusal: ToolRefusal::NotGranted,
            detail: "shell.exec".to_owned(),
        },
    );
    assert!(message.contains("'shell.exec'"), "{message}");
    assert!(message.contains("tool_not_granted"), "{message}");
    assert!(message.contains("-32011"), "{message}");
}
