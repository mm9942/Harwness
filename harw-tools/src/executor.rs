//! Der `ToolExecutor`-Trait: die Ausführungs-Boundary für Tools.
//!
//! Zusätzlich definiert dieses Modul den [`TracedToolExecutor`]-Extension-Trait,
//! der über eine Blanket-Implementierung jeden [`ToolExecutor`] mit einem
//! `tracing`-instrumentierten Wrapper versieht.
//!
//! # Redaction-Regel
//! - `call.arguments` (JSON-Value) wird **niemals** geloggt — nur die Byte-Länge.
//! - `ToolOutput`-Inhalt wird **niemals** geloggt — nur der `status` (`"ok"` / `"err"`).

use crate::call::ToolCall;
use crate::error::ToolsError;
use crate::output::ToolOutput;
use harw_authority::SandboxSpec;
use harw_types::{SessionId, TurnId};
use std::future::Future;
use std::pin::Pin;

/// Boxed Future, das ein [`ToolExecutor`] zurückgibt.
pub type ToolExecutorFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ToolOutput, ToolsError>> + Send + 'a>>;

/// Immutable execution authority established by the harness, not by a model
/// response or inbound channel payload.
///
/// A tool receives this context alongside the untrusted [`ToolCall`]. File,
/// process, MCP, and plugin executors must use [`Self::sandbox`] as their
/// syscall boundary rather than accepting workspace or permission data from
/// `call.arguments`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolExecutionContext {
    session_id: SessionId,
    turn_id: TurnId,
    sandbox: SandboxSpec,
}

impl ToolExecutionContext {
    #[must_use]
    pub fn new(session_id: SessionId, turn_id: TurnId, sandbox: SandboxSpec) -> Self {
        Self {
            session_id,
            turn_id,
            sandbox,
        }
    }

    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    #[must_use]
    pub fn turn_id(&self) -> &TurnId {
        &self.turn_id
    }

    #[must_use]
    pub fn sandbox(&self) -> &SandboxSpec {
        &self.sandbox
    }
}

/// Führt eine [`ToolCall`] aus und liefert eine [`ToolOutput`].
pub trait ToolExecutor: Send + Sync {
    /// The context is mandatory so an executor cannot accidentally run with
    /// ambient host authority. Callers must fail closed when they have not
    /// established a server-side sandbox for the active session.
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a>;

    /// Gibt sich selbst als [`crate::context_load::ContextLoadExecutor`] zu
    /// erkennen, wenn dieser Ausführer genau das ist — sonst `None`.
    ///
    /// # Warum eine Vorgabe (`None`)
    ///
    /// Ein Aufrufer, der nur `Arc<dyn ToolExecutor>` in der Hand hat (etwa
    /// `harw-core`s `find_executor`), hat sonst keinen Weg zurück zum
    /// konkreten `ContextLoadExecutor` — nötig, um ihn z. B. über
    /// `ContextLoadExecutor::seed_turn` mit dem bereits verbrauchten
    /// Turn-Budget der Montage vorzubelegen. Diese Methode schließt genau
    /// diesen Weg, ohne dafür einen Vertrag mit jedem bestehenden
    /// `ToolExecutor`-Implementierer neu auszuhandeln: die Vorgabe `None`
    /// sorgt dafür, dass **jeder bestehende Implementierer unverändert
    /// weiterkompiliert** — nur [`crate::context_load::ContextLoadExecutor`]
    /// selbst überschreibt sie.
    ///
    /// # Warum kein `downcast` (`std::any::Any`)
    ///
    /// Ein `downcast` wäre eine Typprüfung zur Laufzeit an einer Stelle, die
    /// sie nicht braucht — der einzige Aufrufer, der das Ergebnis überhaupt
    /// verwenden will, kennt den Zieltyp bereits statisch. Ein `downcast`
    /// bräuchte außerdem ein zusätzliches `+ Any`-Supertrait bzw. eine
    /// `as_any`-Methode auf `ToolExecutor` — eine Erweiterung, die jeder
    /// Implementierer sähe, obwohl nur einer sie je braucht. Diese
    /// Default-Methode ist die schmalere, dauerhaft tragbare Alternative:
    /// kein `unsafe`, keine Typprüfung zur Laufzeit, kein `downcast`, den ein
    /// Folgeknoten später wieder entfernen müsste.
    ///
    /// # Returns
    ///
    /// `Some(&ContextLoadExecutor)`, wenn dieser Ausführer tatsächlich ein
    /// `ContextLoadExecutor` ist; `None` für jeden anderen Ausführer
    /// (Standardverhalten dieser Methode).
    fn as_context_load_executor(&self) -> Option<&crate::context_load::ContextLoadExecutor> {
        None
    }
}

/// Extension-Trait, der jeden [`ToolExecutor`] um einen `tracing`-instrumentierten
/// Wrapper ergänzt.
///
/// # Verwendung
///
/// ```rust,no_run
/// use harw_tools::executor::{TracedToolExecutor, ToolExecutionContext, ToolExecutor};
///
/// fn run<E: ToolExecutor>(executor: &E, ctx: &ToolExecutionContext, call: &harw_tools::ToolCall) {
///     let fut = executor.traced_execute(ctx, call);
///     // `fut` ist ein `ToolExecutorFuture<'_>` mit tracing-Span.
///     let _ = fut;
/// }
/// ```
///
/// # Redaction-Regel
///
/// - `call.arguments` wird **nicht** geloggt; es wird ausschließlich die Byte-Länge
///   (`arg_bytes`) als strukturiertes Span-Feld emittiert.
/// - `ToolOutput`-Inhalt wird **nicht** geloggt; das Event `tool.execute.done`
///   enthält nur das Feld `status` (`"ok"` / `"err"`).
pub trait TracedToolExecutor: ToolExecutor {
    /// Wraps [`ToolExecutor::execute`] in a `tracing::info_span!("tool.execute")`
    /// and emits a `tool.execute.done` event on completion.
    ///
    /// # Arguments
    ///
    /// - `context` (`&ToolExecutionContext`): immutable harness-established authority.
    /// - `call` (`&ToolCall`): the untrusted invocation to execute. Arguments are
    ///   **not** logged; only their serialized byte-length is recorded.
    ///
    /// # Returns
    ///
    /// A [`ToolExecutorFuture<'a>`] that resolves to `Result<ToolOutput, ToolsError>`.
    /// The future is instrumented with a `tool.execute` span that carries the
    /// `tool` name and `arg_bytes` fields.
    ///
    /// # Errors
    ///
    /// Propagates any [`ToolsError`] returned by the underlying executor unchanged.
    ///
    /// # Concurrency
    ///
    /// Safe to call from any thread. The returned future is `Send + 'a`.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use harw_tools::executor::{TracedToolExecutor, ToolExecutionContext, ToolExecutor};
    ///
    /// fn dispatch<E: ToolExecutor>(
    ///     executor: &E,
    ///     ctx: &ToolExecutionContext,
    ///     call: &harw_tools::ToolCall,
    /// ) {
    ///     let _fut = executor.traced_execute(ctx, call);
    /// }
    /// ```
    fn traced_execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a>;
}

impl<T: ToolExecutor + ?Sized> TracedToolExecutor for T {
    fn traced_execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        use tracing::Instrument as _;

        // Capture only the byte-length of the serialized arguments — never the value itself.
        let tool_name = call.name.as_str().to_owned();
        let arg_bytes = call.arguments.to_string().len();

        let span = tracing::info_span!(
            "tool.execute",
            tool = %tool_name,
            arg_bytes,
        );

        Box::pin(
            async move {
                let start = std::time::Instant::now();
                let result = self.execute(context, call).await;
                tracing::info!(
                    duration_ms = start.elapsed().as_millis() as u64,
                    status = if result.is_ok() { "ok" } else { "err" },
                    "tool.execute.done",
                );
                result
            }
            .instrument(span),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{ToolExecutionContext, ToolExecutor, ToolExecutorFuture};
    use crate::call::ToolCall;
    use crate::output::ToolOutput;
    use crate::spec::ToolName;
    use harw_authority::{Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
    use std::path::PathBuf;

    /// Ein `ToolExecutor`, der `as_context_load_executor` nicht überschreibt —
    /// der Fall, den die Vorgabe `None` unverändert kompilieren und
    /// funktionieren lassen muss.
    struct NoopExecutor;

    impl ToolExecutor for NoopExecutor {
        fn execute<'a>(
            &'a self,
            _context: &'a ToolExecutionContext,
            _call: &'a ToolCall,
        ) -> ToolExecutorFuture<'a> {
            Box::pin(async { Ok(ToolOutput::text("noop")) })
        }
    }

    fn make_ctx(test_id: &str) -> ToolExecutionContext {
        let base = std::env::temp_dir()
            .join("harw_tools_executor_tests")
            .join(test_id);
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let registry = WorkspaceRegistry::build(
            &base,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("t"),
                workspace: WorkspaceId::from_str("w"),
                root: PathBuf::from("ws"),
            }],
        )
        .unwrap();
        let binding = registry
            .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("w"))
            .unwrap();
        let spec = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy(vec![Permission::ReadWorkspace]),
        );
        ToolExecutionContext::new(SessionId::new(), TurnId::new(), spec)
    }

    #[test]
    fn test_executor_without_override_reports_no_context_load_executor() {
        let executor = NoopExecutor;

        let found = executor.as_context_load_executor();

        assert!(
            found.is_none(),
            "an executor that does not override as_context_load_executor must default to None"
        );
    }

    #[test]
    fn test_executor_without_override_still_builds_a_valid_execution_context() {
        // No tokio dependency in this crate: this exercises the same
        // `ToolExecutionContext` construction the async `execute` path would
        // receive, confirming the default method addition changed nothing
        // about how an existing `ToolExecutor` implementer is constructed or
        // invoked (only a new, ignorable trait method was added).
        let executor = NoopExecutor;
        let ctx = make_ctx("still_constructs_normally");
        let call = ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("noop"),
            arguments: serde_json::json!({}),
        };

        let future = executor.execute(&ctx, &call);
        drop(future);
    }
}
