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
use harw_types::cancel::CancelToken;
use harw_types::{SessionId, TurnId};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Boxed Future, das ein [`ToolExecutor`] zurückgibt.
pub type ToolExecutorFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ToolOutput, ToolsError>> + Send + 'a>>;

/// Boxed Future von [`ToolExecutor::execute_placed`].
pub type PlacedToolExecutorFuture<'a> = Pin<Box<dyn Future<Output = PlacedToolOutput> + Send + 'a>>;

/// JSON-Feld, mit dem ein sandboxierter Harness-Ausführer (`shell.exec`)
/// eine genehmigte Host-Ausführung kennzeichnet (`"executed_on": "host"`).
pub const EXECUTED_ON_FIELD: &str = "executed_on";

/// Wo ein Tool-Aufruf tatsächlich lief (R18 D-D, Vertrag
/// `docs/planning/65-cloud-sessions/contracts/R18-tool-gateway.md` §5).
///
/// # Beschreibung
/// Das Vokabular dieses Crates für `harw_protocol::items::ToolPlacement`:
/// `harw-tools` hängt nicht von `harw-protocol` ab, deshalb bildet
/// `harw-core` diesen Wert beim Melden von `ToolCallCompleted` ab. Den Wert
/// setzt immer die ausführende Seite (Ausführer-Metadaten bzw. die
/// Ergebnis-Frame des Gateways), nie das Modell.
///
/// - `Host`: lief lokal außerhalb jeder Sandbox (genehmigte Host-Eskalation).
/// - `Sandbox`: lief in der lokalen Sandbox des Agentenprozesses.
/// - `Gateway`: lief im Gateway-Tool-Host (dort immer sandboxiert).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ExecutionPlacement {
    /// Lokaler Host, außerhalb jeder Sandbox.
    Host,
    /// Lokale Sandbox des Agentenprozesses.
    Sandbox,
    /// Gateway-Tool-Host.
    Gateway {
        /// Knotenname des Gateways, falls bekannt.
        node: Option<String>,
    },
}

impl ExecutionPlacement {
    /// Leitet die Platzierung eines lokal beendeten Aufrufs aus den
    /// Ausführer-Metadaten ab.
    ///
    /// # Beschreibung
    /// Nur ein Ausführer, der selbst [`ExecutionPlacement::Sandbox`] meldet,
    /// darf über das Ausgabefeld [`EXECUTED_ON_FIELD`] (`"host"`) auf
    /// [`ExecutionPlacement::Host`] umschalten — das ist genau die genehmigte
    /// Host-Eskalation von `shell.exec`. Jeder andere Ausführer behält seine
    /// deklarierte Platzierung; eine Ausgabe allein (etwa JSON eines
    /// Fremdwerkzeugs) kann nie eine Platzierung erfinden.
    ///
    /// # Argumente
    /// - `declared`: [`ToolExecutor::placement`] des Ausführers.
    /// - `output`: das Ergebnis des Aufrufs.
    ///
    /// # Rückgabe
    /// Die Platzierung, oder `None` („unbekannt“), wenn der Ausführer keine
    /// deklariert.
    #[must_use]
    pub fn resolve_local(
        declared: Option<ExecutionPlacement>,
        output: &Result<ToolOutput, ToolsError>,
    ) -> Option<ExecutionPlacement> {
        match declared {
            Some(ExecutionPlacement::Sandbox) => {
                let on_host = matches!(
                    output,
                    Ok(ToolOutput::Json { content })
                        if content.get(EXECUTED_ON_FIELD).and_then(serde_json::Value::as_str)
                            == Some("host")
                );
                Some(if on_host {
                    ExecutionPlacement::Host
                } else {
                    ExecutionPlacement::Sandbox
                })
            }
            other => other,
        }
    }
}

/// Ergebnis von [`ToolExecutor::execute_placed`]: Ausgabe plus Platzierung.
#[derive(Debug)]
pub struct PlacedToolOutput {
    /// Die Ausgabe, exakt wie [`ToolExecutor::execute`] sie liefert.
    pub output: Result<ToolOutput, ToolsError>,
    /// Wo der Aufruf lief; `None` heißt unbekannt (Oberflächen zeigen dann
    /// keine Platzierung statt `host` zu raten).
    pub placement: Option<ExecutionPlacement>,
}

/// Immutable execution authority established by the harness, not by a model
/// response or inbound channel payload.
///
/// A tool receives this context alongside the untrusted [`ToolCall`]. File,
/// process, MCP, and plugin executors must use [`Self::sandbox`] as their
/// syscall boundary rather than accepting workspace or permission data from
/// `call.arguments`.
///
/// # Cancellation (W3/C-CANCEL, F-160/G-017)
///
/// [`Self::cancel`] carries the same [`CancelToken`] the turn-loop derives
/// for the active turn (analogous to `ModelRequest::cancel` in
/// `harw-core/src/model.rs`), so long-running tool executors (process, MCP,
/// context-load) can observe `Ctrl+C`, a budget breach, or lease loss
/// mid-call. `CancelToken` lives in `harw_types::cancel` — not
/// `harw_core::cancel` — specifically so this crate (which `harw-core`
/// depends on, ruling out the reverse direction) can name it without a
/// dependency cycle; `harw_core::cancel` re-exports the same type
/// unchanged for existing callers.
#[derive(Debug, Clone)]
pub struct ToolExecutionContext {
    session_id: SessionId,
    turn_id: TurnId,
    sandbox: SandboxSpec,
    cancel: Option<CancelToken>,
}

impl ToolExecutionContext {
    #[must_use]
    pub fn new(session_id: SessionId, turn_id: TurnId, sandbox: SandboxSpec) -> Self {
        Self {
            session_id,
            turn_id,
            sandbox,
            cancel: None,
        }
    }

    /// Attaches a [`CancelToken`] to this context, consuming and returning
    /// `self` for builder-style chaining.
    ///
    /// # Arguments
    /// - `cancel` (`CancelToken`): the turn-scoped (or narrower) cancellation
    ///   handle a long-running executor should observe.
    ///
    /// # Returns
    /// The same context with [`Self::cancel`] now returning
    /// `Some(&cancel)`.
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = Some(cancel);
        self
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

    /// Returns the attached [`CancelToken`], if this context was built with
    /// [`Self::with_cancel`].
    ///
    /// # Returns
    /// `None` for a context built only via [`Self::new`] — an executor that
    /// does not check cancellation runs exactly as before this field was
    /// added.
    #[must_use]
    pub fn cancel(&self) -> Option<&CancelToken> {
        self.cancel.as_ref()
    }
}

/// Compares every field except [`ToolExecutionContext::cancel`].
///
/// `cancel` is a volatile per-call handle (a live [`CancelToken`], which
/// itself has no `PartialEq` impl — it wraps a
/// `tokio_util::sync::CancellationToken`) without bearing on the identity or
/// equality of the execution context it rides along with: two contexts
/// built from the same session/turn/sandbox are equal regardless of which
/// (if any) cancellation handle happens to be attached.
impl PartialEq for ToolExecutionContext {
    fn eq(&self, other: &Self) -> bool {
        self.session_id == other.session_id
            && self.turn_id == other.turn_id
            && self.sandbox == other.sandbox
    }
}

/// `PartialEq` above only ever compares `SessionId`/`TurnId`/`SandboxSpec`,
/// all three of which are themselves `Eq`, so the relation is reflexive,
/// symmetric and transitive — `Eq` holds as a marker with no extra method.
impl Eq for ToolExecutionContext {}

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

    /// Platzierungs-Metadaten dieses Ausführers (R18 D-D).
    ///
    /// # Beschreibung
    /// Vorgabe `None` („unbekannt“): jeder bestehende Implementierer
    /// kompiliert unverändert weiter und meldet keine Platzierung. Ein
    /// sandboxierter lokaler Ausführer meldet
    /// [`ExecutionPlacement::Sandbox`] (siehe [`PlacedToolExecutor`]), der
    /// entfernte Proxy (`harw-tool-remote`) [`ExecutionPlacement::Gateway`].
    fn placement(&self) -> Option<ExecutionPlacement> {
        None
    }

    /// Führt den Aufruf aus und meldet zusätzlich, wo er lief.
    ///
    /// # Beschreibung
    /// Die Vorgabe ruft [`Self::execute`] und leitet die Platzierung über
    /// [`ExecutionPlacement::resolve_local`] aus [`Self::placement`] ab. Ein
    /// Ausführer, der die Platzierung erst aus der Antwort kennt (der
    /// Gateway-Proxy kopiert sie aus der Ergebnis-Frame), überschreibt diese
    /// Methode.
    fn execute_placed<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> PlacedToolExecutorFuture<'a> {
        Box::pin(async move {
            let output = self.execute(context, call).await;
            let placement = ExecutionPlacement::resolve_local(self.placement(), &output);
            PlacedToolOutput { output, placement }
        })
    }
}

/// Hängt einem bestehenden Ausführer feste Platzierungs-Metadaten an.
///
/// # Beschreibung
/// Für Ausführer aus fremden Crates, die ihre Platzierung nicht selbst
/// melden (z. B. `shell.exec`, das immer in der Sandbox startet und nur
/// über eine genehmigte Eskalation auf den Host wechselt). Alles außer
/// [`ToolExecutor::placement`] wird unverändert an den inneren Ausführer
/// weitergereicht.
pub struct PlacedToolExecutor {
    inner: Arc<dyn ToolExecutor>,
    placement: ExecutionPlacement,
}

impl PlacedToolExecutor {
    /// Umhüllt `inner` mit der Platzierung `placement`.
    #[must_use]
    pub fn new(inner: Arc<dyn ToolExecutor>, placement: ExecutionPlacement) -> Self {
        Self { inner, placement }
    }
}

impl ToolExecutor for PlacedToolExecutor {
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        self.inner.execute(context, call)
    }

    fn as_context_load_executor(&self) -> Option<&crate::context_load::ContextLoadExecutor> {
        self.inner.as_context_load_executor()
    }

    fn placement(&self) -> Option<ExecutionPlacement> {
        Some(self.placement.clone())
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

    /// Wie [`Self::traced_execute`], aber über [`ToolExecutor::execute_placed`]:
    /// liefert zusätzlich die Platzierung des Aufrufs (R18 D-D). Dieselbe
    /// Redaction-Regel; das Event `tool.execute.done` trägt zusätzlich nur
    /// das Feld `placement` (`host`/`sandbox`/`gateway`/`unknown`).
    fn traced_execute_placed<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> PlacedToolExecutorFuture<'a>;
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

    fn traced_execute_placed<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> PlacedToolExecutorFuture<'a> {
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
                let placed = self.execute_placed(context, call).await;
                tracing::info!(
                    duration_ms = start.elapsed().as_millis() as u64,
                    status = if placed.output.is_ok() { "ok" } else { "err" },
                    placement = match &placed.placement {
                        Some(ExecutionPlacement::Host) => "host",
                        Some(ExecutionPlacement::Sandbox) => "sandbox",
                        Some(ExecutionPlacement::Gateway { .. }) => "gateway",
                        None => "unknown",
                    },
                    "tool.execute.done",
                );
                placed
            }
            .instrument(span),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EXECUTED_ON_FIELD, ExecutionPlacement, PlacedToolExecutor, ToolExecutionContext,
        ToolExecutor, ToolExecutorFuture, TracedToolExecutor,
    };
    use crate::call::ToolCall;
    use crate::error::ToolsError;
    use crate::output::ToolOutput;
    use crate::spec::ToolName;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_types::cancel::CancelToken;
    use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
    use std::future::Future;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::task::{Context, Poll, Waker};

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

    fn make_ctx(test_id: &str) -> TestResult<ToolExecutionContext> {
        let base = std::env::temp_dir()
            .join("harw_tools_executor_tests")
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
    fn test_executor_without_override_still_builds_a_valid_execution_context() -> TestResult {
        // No tokio dependency in this crate: this exercises the same
        // `ToolExecutionContext` construction the async `execute` path would
        // receive, confirming the default method addition changed nothing
        // about how an existing `ToolExecutor` implementer is constructed or
        // invoked (only a new, ignorable trait method was added).
        let executor = NoopExecutor;
        let ctx = make_ctx("still_constructs_normally")?;
        let call = ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("noop"),
            arguments: serde_json::json!({}),
        };

        let future = executor.execute(&ctx, &call);
        drop(future);
        Ok(())
    }

    #[test]
    fn test_new_context_has_no_cancel_token() -> TestResult {
        let ctx = make_ctx("new_context_has_no_cancel_token")?;

        assert!(
            ctx.cancel().is_none(),
            "ToolExecutionContext::new must leave cancel unset"
        );
        Ok(())
    }

    #[test]
    fn test_with_cancel_attaches_token_and_cancel_reads_it_back() -> TestResult {
        let ctx = make_ctx("with_cancel_attaches_token")?.with_cancel(CancelToken::new());

        let attached = ctx.cancel();

        assert!(
            attached.is_some(),
            "with_cancel must make cancel() report Some"
        );
        let token = attached.ok_or(TestError::Missing("attached cancel token"))?;
        assert!(
            !token.is_cancelled(),
            "a freshly attached, uncancelled token must report not-cancelled through the context"
        );
        Ok(())
    }

    #[test]
    fn test_partial_eq_ignores_cancel_field() -> TestResult {
        let base = make_ctx("partial_eq_ignores_cancel_field")?;
        let without_cancel = base.clone();
        let with_cancel = base.with_cancel(CancelToken::new());

        assert_eq!(
            without_cancel, with_cancel,
            "two contexts that differ only in `cancel` (None vs. Some) must still be equal"
        );
        Ok(())
    }

    /// Ein Ausführer, der wie `shell.exec` nach einer genehmigten
    /// Host-Eskalation `"executed_on": "host"` meldet.
    struct HostMarkerExecutor;

    impl ToolExecutor for HostMarkerExecutor {
        fn execute<'a>(
            &'a self,
            _context: &'a ToolExecutionContext,
            _call: &'a ToolCall,
        ) -> ToolExecutorFuture<'a> {
            Box::pin(async {
                Ok(ToolOutput::json(
                    serde_json::json!({ "exit_code": 0, EXECUTED_ON_FIELD: "host" }),
                ))
            })
        }
    }

    /// Pollt ein sofort fertiges Future einmal (dieses Crate hat kein tokio).
    fn ready<F: Future>(future: F) -> TestResult<F::Output> {
        let mut future = std::pin::pin!(future);
        let mut context = Context::from_waker(Waker::noop());
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => Ok(output),
            Poll::Pending => Err(TestError::Unexpected(
                "the test future was expected to be ready".to_owned(),
            )),
        }
    }

    fn noop_call() -> ToolCall {
        ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("shell.exec"),
            arguments: serde_json::json!({}),
        }
    }

    #[test]
    fn test_resolve_local_upgrades_only_a_sandboxed_executor_to_host() {
        let host_marker: Result<ToolOutput, ToolsError> = Ok(ToolOutput::json(
            serde_json::json!({ EXECUTED_ON_FIELD: "host" }),
        ));
        let plain: Result<ToolOutput, ToolsError> =
            Ok(ToolOutput::json(serde_json::json!({ "exit_code": 0 })));

        assert_eq!(
            ExecutionPlacement::resolve_local(Some(ExecutionPlacement::Sandbox), &host_marker),
            Some(ExecutionPlacement::Host)
        );
        assert_eq!(
            ExecutionPlacement::resolve_local(Some(ExecutionPlacement::Sandbox), &plain),
            Some(ExecutionPlacement::Sandbox)
        );
        assert_eq!(
            ExecutionPlacement::resolve_local(
                Some(ExecutionPlacement::Sandbox),
                &Err(ToolsError::Cancelled)
            ),
            Some(ExecutionPlacement::Sandbox)
        );
        // Eine Ausgabe allein erfindet nie eine Platzierung.
        assert_eq!(ExecutionPlacement::resolve_local(None, &host_marker), None);
        let gateway = ExecutionPlacement::Gateway {
            node: Some("gw-1".to_owned()),
        };
        assert_eq!(
            ExecutionPlacement::resolve_local(Some(gateway.clone()), &host_marker),
            Some(gateway)
        );
    }

    #[test]
    fn test_default_execute_placed_reports_no_placement() -> TestResult {
        let ctx = make_ctx("default_execute_placed")?;
        let call = noop_call();

        let placed = ready(NoopExecutor.execute_placed(&ctx, &call))?;

        assert!(placed.output.is_ok());
        assert_eq!(placed.placement, None);
        assert_eq!(NoopExecutor.placement(), None);
        Ok(())
    }

    #[test]
    fn test_placed_executor_reports_sandbox_and_host_escalation() -> TestResult {
        let ctx = make_ctx("placed_executor")?;
        let call = noop_call();
        let sandboxed =
            PlacedToolExecutor::new(Arc::new(NoopExecutor), ExecutionPlacement::Sandbox);
        let escalated =
            PlacedToolExecutor::new(Arc::new(HostMarkerExecutor), ExecutionPlacement::Sandbox);

        let in_sandbox = ready(sandboxed.traced_execute_placed(&ctx, &call))?;
        let on_host = ready(escalated.traced_execute_placed(&ctx, &call))?;

        assert_eq!(in_sandbox.placement, Some(ExecutionPlacement::Sandbox));
        assert_eq!(on_host.placement, Some(ExecutionPlacement::Host));
        assert!(sandboxed.as_context_load_executor().is_none());
        Ok(())
    }
}
