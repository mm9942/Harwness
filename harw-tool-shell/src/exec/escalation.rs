//! Host-Mode-Anfrage für `shell.exec` (Runde 5, Teil N).
//!
//! # Verantwortungsbereich
//! [`EscalatingShellExecutor`] umhüllt jeden [`ShellExecutor`], den
//! [`super::ShellToolProvider::executor`] ausgibt, und ergänzt genau zwei
//! Dinge — der bestehende Sandbox- und Host-Pfad bleibt unverändert:
//!
//! 1. **Anfrage:** Trägt der Aufruf `request_host: {"reason": "…"}`, fragt
//!    der Executor — nach denselben Vorprüfungen wie `shell.exec` selbst
//!    (Argumente, sudo-Verbot, `ExecuteProcess`) — über die bestehende
//!    [`HostPermitPrompt`] mit denselben zwei Varianten nach, diesmal mit
//!    Anfragendem (Rolle, Kind-ID, Baum-Pfad) und Grund. Nach Zustimmung
//!    läuft der Befehl über den bestehenden Host-Pfad
//!    ([`ShellExecutor::run_host_command`]: Permit-Ledger, setsid/prlimit,
//!    Zeitlimit, cwd = Workspace-Wurzel). Eine Host-Arbeitsphase wird
//!    prozessweit eingetragen
//!    ([`harw_sandbox::HostPermitSessionRegistry::mark_global_approval`]) —
//!    dieselbe Wirkung wie `/sandbox-lease`: alle Shell-fähigen Agenten dieser
//!    harw-Sitzung laufen danach ohne erneute Frage auf dem Host.
//! 2. **Erkennen:** Scheitert ein normaler Sandbox-Lauf erkennbar an der
//!    Sandbox ([`classify_sandbox_denial`]), bekommt die JSON-Ausgabe die
//!    Felder `sandbox_denial` und `host_mode_hint` — ein Hinweis an das
//!    Modell, nie eine automatische Wiederholung auf dem Host.
//!
//! Ohne [`HostEscalation`] (kein TUI-Einstieg) oder ohne Ledger/Registry/
//! Fragekanal endet jede Anfrage fail-closed mit
//! [`HOST_MODE_REQUIRES_TUI_MSG`].
//!
//! # Concurrency
//! `Send + Sync`; gewartet wird nur auf einem `oneshot`, begrenzt durch
//! [`ShellExecutor::host_permit_timeout`].

use super::{HOST_SINGLE_EXECUTION_TTL, ShellExecArgs, ShellExecutor, TOOL_NAME};
use crate::host_escalation::{
    EscalationOutcome, HOST_ESCALATION_DENIED_MSG, HOST_ESCALATION_WORKER_PREFIX,
    HOST_MODE_REQUIRES_TUI_MSG, HostEscalation, HostRequester, HostRequesterBook, RequestHostArgs,
    SandboxDenial, audit_escalation, classify_sandbox_denial, denial_hint,
};
use crate::host_permit_prompt::{HostPermitPrompt, HostPermitVariant};
use harw_authority::{Permission, SandboxSpec};
use harw_sandbox::{HostApprovalScope, ProcessEnvironment, ProcessPermitRequest};
use harw_tools::{
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolOutput, ToolsError,
    schema::{AdditionalProperties, JsonSchema, JsonSchemaType},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use tracing::warn;

/// Name des optionalen Anfragefelds im `shell.exec`-Aufruf.
pub(super) const REQUEST_HOST_FIELD: &str = "request_host";

/// Schema des optionalen Felds `request_host` (`{"reason": string}`).
pub(super) fn request_host_schema() -> JsonSchema {
    let mut properties = BTreeMap::new();
    properties.insert(
        "reason".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "Why this command must run on the host instead of the sandbox (e.g. needs \
                 network for cargo fetch, needs a tool or path outside the sandbox)."
                    .to_owned(),
            ),
            ..Default::default()
        },
    );
    JsonSchema {
        schema_type: Some(JsonSchemaType::Object),
        description: Some(
            "Optional. Ask the user to run THIS command on the host (Host-Mode) because the \
             sandbox blocks it. The user approves once or for a host work phase; without \
             approval nothing runs. Only use after a sandbox limit (network, path outside, \
             missing tool, namespace) actually blocked the command. sudo/doas/pkexec stay \
             forbidden."
                .to_owned(),
        ),
        properties: Some(properties),
        required: Some(vec!["reason".to_owned()]),
        additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
        ..Default::default()
    }
}

/// Umhüllt [`ShellExecutor`] um Host-Mode-Anfrage und Sandbox-Hinweis.
pub(super) struct EscalatingShellExecutor {
    inner: ShellExecutor,
    escalation: Option<HostEscalation>,
    /// Zeitlimit-Politik (Vorgabe, Build-Vorgabe, Obergrenze) — gilt für
    /// Sandbox-, Host- und Eskalationspfad gleich (siehe
    /// [`super::timeouts`]).
    timeouts: super::timeouts::TimeoutPolicy,
}

impl EscalatingShellExecutor {
    /// Baut die Hülle; `escalation = None` heißt: kein Freigabekanal für
    /// Host-Mode-Anfragen (fail-closed mit klarer Meldung).
    pub(super) fn new(
        inner: ShellExecutor,
        escalation: Option<HostEscalation>,
        timeouts: super::timeouts::TimeoutPolicy,
    ) -> Self {
        Self {
            inner,
            escalation,
            timeouts,
        }
    }

    /// Trennt das Feld `request_host` vom übrigen Aufruf.
    ///
    /// # Errors
    /// [`ToolsError::InvalidArguments`], wenn `request_host` weder `null`
    /// noch ein gültiges `{"reason": "…"}` ist.
    fn split_request(call: &ToolCall) -> Result<(ToolCall, Option<RequestHostArgs>), ToolsError> {
        let mut arguments = call.arguments.clone();
        let raw = arguments
            .as_object_mut()
            .and_then(|object| object.remove(REQUEST_HOST_FIELD));
        let request = match raw {
            None | Some(Value::Null) => None,
            Some(value) => Some(serde_json::from_value::<RequestHostArgs>(value).map_err(
                |err| ToolsError::InvalidArguments {
                    name: TOOL_NAME.to_owned(),
                    reason: format!("{REQUEST_HOST_FIELD}: {err}"),
                },
            )?),
        };
        Ok((
            ToolCall {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments,
            },
            request,
        ))
    }

    /// `true`, wenn dieser Lauf tatsächlich fragen kann: Verdrahtung aus der
    /// TUI plus Ledger, Registry und Fragekanal.
    fn can_request(&self) -> bool {
        self.escalation.is_some()
            && self.inner.permit_ledger.is_some()
            && self.inner.host_permit_registry.is_some()
            && self.inner.host_permit_prompts.is_some()
    }

    /// Der Anfragende für `session` aus dem gemeinsamen Buch.
    fn requester_for(&self, session: &str) -> HostRequester {
        match &self.escalation {
            Some(escalation) => escalation.requesters().requester_for(session),
            None => HostRequesterBook::default().requester_for(session),
        }
    }

    /// Ergänzt einen gescheiterten **Sandbox**-Lauf um `sandbox_denial` und
    /// `host_mode_hint`, falls die Heuristik eine Sandbox-Grenze erkennt.
    /// Host-Läufe (`executed_on = host`) und erfolgreiche Läufe bleiben
    /// unverändert.
    fn annotate_denial(&self, output: ToolOutput) -> ToolOutput {
        match output {
            ToolOutput::Json { mut content } => {
                if content.get("executed_on").is_none() {
                    let exit_code = content
                        .get("exit_code")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                    let stdout = content.get("stdout").and_then(Value::as_str).unwrap_or("");
                    let stderr = content.get("stderr").and_then(Value::as_str).unwrap_or("");
                    let denial = classify_sandbox_denial(exit_code, stdout, stderr);
                    if let (Some(denial), Some(object)) = (denial, content.as_object_mut()) {
                        object.insert("sandbox_denial".to_owned(), json!(denial.as_str()));
                        object.insert(
                            "host_mode_hint".to_owned(),
                            json!(denial_hint(denial, self.can_request())),
                        );
                    }
                }
                ToolOutput::Json { content }
            }
            ToolOutput::Error { message } if message.contains("sandbox setup failed") => {
                ToolOutput::Error {
                    message: format!(
                        "{message}\n{}",
                        denial_hint(SandboxDenial::SandboxSetup, self.can_request())
                    ),
                }
            }
            other => other,
        }
    }

    /// Ein Aufruf ohne `request_host`: unveränderter Pfad plus Hinweis.
    async fn execute_plain(
        &self,
        context: &ToolExecutionContext,
        call: &ToolCall,
    ) -> Result<ToolOutput, ToolsError> {
        let output = self.inner.execute(context, call).await?;
        Ok(self.annotate_denial(output))
    }

    /// Ein Aufruf mit `request_host`: Vorprüfungen, Anfrage, Host-Ausführung.
    async fn execute_escalation(
        &self,
        context: &ToolExecutionContext,
        call: &ToolCall,
        request: RequestHostArgs,
    ) -> Result<ToolOutput, ToolsError> {
        // Ein Host-Profil-Worker fragt ohnehin vor jeder Ausführung
        // (`authorize_host_command`) — dort ist `request_host` redundant.
        if self.inner.sandbox_profile.is_host() {
            return self.inner.execute(context, call).await;
        }

        let args: ShellExecArgs =
            serde_json::from_value(call.arguments.clone()).map_err(|err| {
                ToolsError::InvalidArguments {
                    name: TOOL_NAME.to_owned(),
                    reason: err.to_string(),
                }
            })?;
        let effective_timeout = self.inner.effective_timeout(&args)?;
        let reason =
            request
                .normalized_reason()
                .map_err(|reason| ToolsError::InvalidArguments {
                    name: TOOL_NAME.to_owned(),
                    reason,
                })?;

        // „never sudo“ gilt auch im Host-Mode — geprüft, bevor gefragt wird.
        if let Some(program) = crate::sudo::escalation_program(&args.command) {
            warn!(
                program,
                "shell.exec host escalation denied: privilege escalation program"
            );
            return Ok(ToolOutput::error(crate::sudo::shell_escalation_message(
                program,
            )));
        }
        // Nur wer Ausführungsrecht hat, darf überhaupt fragen.
        if let Some(err) = harw_tools::sandbox_guard::require_permission(
            context,
            Permission::ExecuteProcess,
            TOOL_NAME,
        ) {
            warn!("shell.exec host escalation denied: ExecuteProcess permission missing");
            return Ok(err);
        }

        let sandbox = context.sandbox();
        let session = context.session_id().as_str();
        let requester = self.requester_for(session);
        let cwd = sandbox.workspace().canonical_root();

        // Eine laufende Host-Arbeitsphase (auch `/sandbox-lease`) deckt den
        // Aufruf bereits — keine erneute Frage.
        let covered = self
            .inner
            .host_permit_registry
            .as_ref()
            .is_some_and(|registry| registry.is_session_approved(session));
        if covered {
            audit_escalation(
                &requester,
                &args.command,
                cwd,
                &reason,
                EscalationOutcome::CoveredByLease,
            );
        } else if let Err(message) = self
            .authorize_escalation(&args, sandbox, session, &requester, &reason)
            .await
        {
            return Ok(ToolOutput::error(message));
        }

        self.inner
            .run_host_command(&args, sandbox, effective_timeout, session, context.cancel())
            .await
    }

    /// Stellt die Host-Mode-Anfrage und verbucht eine ausdrückliche
    /// Zustimmung im Permit-Ledger; jeder andere Ausgang ist eine Ablehnung.
    ///
    /// # Errors
    /// [`HOST_MODE_REQUIRES_TUI_MSG`] ohne Freigabekanal,
    /// [`HOST_ESCALATION_DENIED_MSG`] bei Ablehnung/Zeitablauf/verworfener
    /// Antwort, sonst die Ledger-Meldung.
    async fn authorize_escalation(
        &self,
        args: &ShellExecArgs,
        sandbox: &SandboxSpec,
        session: &str,
        requester: &HostRequester,
        reason: &str,
    ) -> Result<(), String> {
        let cwd = sandbox.workspace().canonical_root();
        let (true, Some(ledger), Some(registry), Some(sender)) = (
            self.escalation.is_some(),
            self.inner.permit_ledger.as_ref(),
            self.inner.host_permit_registry.as_ref(),
            self.inner.host_permit_prompts.as_ref(),
        ) else {
            audit_escalation(
                requester,
                &args.command,
                cwd,
                reason,
                EscalationOutcome::NoChannel,
            );
            return Err(HOST_MODE_REQUIRES_TUI_MSG.to_owned());
        };

        let request = ProcessPermitRequest {
            session: session.to_owned(),
            worker_definition: format!("{HOST_ESCALATION_WORKER_PREFIX}:{}", requester.role),
            command: args.command.clone(),
            workspace: cwd.to_path_buf(),
            environment: ProcessEnvironment::LocalHost,
        };
        let (prompt, answer) = HostPermitPrompt::new(
            request.session.clone(),
            request.worker_definition.clone(),
            request.command.clone(),
            request.workspace.clone(),
            self.inner.preselected_permit_variant,
        );
        let prompt = prompt.with_requester(requester.clone()).with_reason(reason);
        audit_escalation(
            requester,
            &args.command,
            cwd,
            reason,
            EscalationOutcome::Requested,
        );
        if sender.send(prompt).is_err() {
            warn!(session, "shell.exec: host escalation prompt channel closed");
            audit_escalation(
                requester,
                &args.command,
                cwd,
                reason,
                EscalationOutcome::NoChannel,
            );
            return Err(HOST_MODE_REQUIRES_TUI_MSG.to_owned());
        }

        let decision = match tokio::time::timeout(self.inner.host_permit_timeout, answer).await {
            Ok(Ok(decision)) => decision,
            Ok(Err(_)) => {
                warn!(session, "shell.exec: host escalation answer dropped");
                None
            }
            Err(_elapsed) => {
                warn!(session, "shell.exec: host escalation prompt timed out");
                None
            }
        };
        let Some(variant) = decision else {
            audit_escalation(
                requester,
                &args.command,
                cwd,
                reason,
                EscalationOutcome::Denied,
            );
            return Err(HOST_ESCALATION_DENIED_MSG.to_owned());
        };

        let (scope, ttl, outcome) = match variant {
            HostPermitVariant::SingleExecution => (
                HostApprovalScope::SingleExecution,
                Some(HOST_SINGLE_EXECUTION_TTL),
                EscalationOutcome::ApprovedOnce,
            ),
            HostPermitVariant::SessionLease => {
                // Dieselbe Wirkung wie `/sandbox-lease`: die Phase gilt für
                // die ganze harw-Sitzung inkl. aller Kind-Agenten, ohne
                // Zeitablauf — bis der Nutzer sie beendet (Strg+H oder
                // `/sandbox-lease revoke`).
                registry.mark_global_approval();
                (
                    HostApprovalScope::SessionLease,
                    None,
                    EscalationOutcome::ApprovedSession,
                )
            }
        };
        match ShellExecutor::issue_and_remember(ledger, registry, request, scope, ttl) {
            Ok(()) => {
                audit_escalation(requester, &args.command, cwd, reason, outcome);
                Ok(())
            }
            Err(message) => {
                audit_escalation(
                    requester,
                    &args.command,
                    cwd,
                    reason,
                    EscalationOutcome::PermitFailed,
                );
                Err(message)
            }
        }
    }
}

impl ToolExecutor for EscalatingShellExecutor {
    /// Führt `shell.exec` aus: ohne `request_host` unverändert (plus
    /// Sandbox-Hinweis), mit `request_host` über die Host-Mode-Anfrage.
    ///
    /// # Errors
    /// Wie [`ShellExecutor`]; zusätzlich [`ToolsError::InvalidArguments`] für
    /// ein ungültiges `request_host`.
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let (mut call, request) = Self::split_request(call)?;
            // Wirksames Zeitlimit (Argument geklemmt bzw. Vorgabe je
            // Befehlsart) — bevor irgendein Pfad die Argumente liest.
            let effective = super::timeouts::apply_timeout(&mut call.arguments, &self.timeouts);
            let output = match request {
                None => self.execute_plain(context, &call).await?,
                Some(request) => self.execute_escalation(context, &call, request).await?,
            };
            Ok(match effective {
                Some(secs) => {
                    super::timeouts::annotate_timeout(output, secs, self.timeouts.max_secs)
                }
                None => output,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ShellToolProvider;
    use crate::host_escalation::HostEscalation;
    use crate::host_permit_prompt::host_permit_prompt_channel;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{PermissionSet, WorkspaceRegistration, WorkspaceRegistry};
    use harw_extension_api::contributors::ToolProvider;
    use harw_sandbox::{HostPermitSessionRegistry, ProcessPermitLedger, SandboxProfile};
    use harw_tools::spec::ToolName;
    use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::TempDir;

    fn make_sandbox(dir: &TempDir, permissions: Vec<Permission>) -> TestResult<SandboxSpec> {
        let ws = dir.path().join("project");
        std::fs::create_dir_all(&ws).map_err(ctx("project subdir"))?;
        let registry = WorkspaceRegistry::build(
            dir.path(),
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("project"),
                root: ws,
            }],
        )
        .map_err(ctx("registry build"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("project"),
            )
            .map_err(ctx("resolve"))?;
        Ok(SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy(permissions),
        ))
    }

    fn call(arguments: Value) -> ToolCall {
        ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(TOOL_NAME),
            arguments,
        }
    }

    struct Rig {
        executor: Arc<dyn ToolExecutor>,
        registry: Arc<HostPermitSessionRegistry>,
        escalation: HostEscalation,
    }

    /// Ein Kind-Worker mit Strict-Profil, voll verdrahtet wie in der TUI.
    fn tui_child_rig(
        sender: crate::host_permit_prompt::HostPermitPromptSender,
        timeout: Duration,
    ) -> TestResult<Rig> {
        let registry = Arc::new(HostPermitSessionRegistry::default());
        let escalation = HostEscalation::new();
        let provider = ShellToolProvider::default()
            .with_sandbox_profile(SandboxProfile::Strict)
            .with_permit_ledger(Arc::new(ProcessPermitLedger::default()))
            .with_host_permit_registry(Arc::clone(&registry))
            .with_host_permit_prompts(sender)
            .with_host_permit_timeout(timeout)
            .with_host_escalation(escalation.clone());
        let executor = provider
            .executor(&ToolName::new(TOOL_NAME))
            .ok_or(TestError::Missing("shell.exec executor"))?;
        Ok(Rig {
            executor,
            registry,
            escalation,
        })
    }

    fn register_executor_child(book: &HostRequesterBook, child: &SessionId) {
        book.register_root("root", "uia");
        book.note_spawn("root", "root-orchestrator");
        book.bind_child("root-orchestrator", "orchestrator-session");
        book.note_spawn("orchestrator-session", "executor");
        book.bind_child("executor", child.as_str());
    }

    #[tokio::test]
    async fn test_child_request_shows_role_and_path_and_single_approval_runs_on_host() -> TestResult
    {
        let tmp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let context = ToolExecutionContext::new(
            SessionId::new(),
            TurnId::new(),
            make_sandbox(&tmp, vec![Permission::ExecuteProcess])?,
        );
        let (sender, mut receiver) = host_permit_prompt_channel();
        let rig = tui_child_rig(sender, Duration::from_secs(30))?;
        register_executor_child(rig.escalation.requesters(), context.session_id());
        let expected_session = context.session_id().as_str().to_owned();

        let responder = tokio::spawn(async move {
            let prompt = receiver.recv().await.ok_or(TestError::Missing("prompt"))?;
            let requester = prompt
                .requester()
                .cloned()
                .ok_or(TestError::Missing("requester"))?;
            assert_eq!(requester.role, "executor");
            assert_eq!(requester.session, expected_session);
            assert_eq!(requester.path, "uia › root-orchestrator › executor");
            assert_eq!(prompt.reason(), Some("cargo fetch braucht Netz"));
            assert_eq!(prompt.command(), "echo escalated_ok");
            assert!(prompt.approve(HostPermitVariant::SingleExecution));
            Ok::<(), TestError>(())
        });

        let output = rig
            .executor
            .execute(
                &context,
                &call(json!({
                    "command": "echo escalated_ok",
                    "request_host": { "reason": "cargo fetch braucht Netz" }
                })),
            )
            .await
            .map_err(ctx("execute"))?;
        responder.await.map_err(ctx("responder task"))??;

        match output {
            ToolOutput::Json { content } => {
                assert_eq!(content["executed_on"], "host", "{content}");
                assert!(
                    content["stdout"]
                        .as_str()
                        .is_some_and(|out| out.contains("escalated_ok")),
                    "{content}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected host JSON output, got {other:?}"
                )));
            }
        }
        assert!(
            !rig.registry.is_session_approved("some-other-session"),
            "a single approval must not open a host work phase"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_session_approval_lets_follow_up_commands_run_without_prompt() -> TestResult {
        let tmp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let context = ToolExecutionContext::new(
            SessionId::new(),
            TurnId::new(),
            make_sandbox(&tmp, vec![Permission::ExecuteProcess])?,
        );
        let (sender, mut receiver) = host_permit_prompt_channel();
        let rig = tui_child_rig(sender, Duration::from_secs(30))?;

        let responder = tokio::spawn(async move {
            let prompt = receiver.recv().await.ok_or(TestError::Missing("prompt"))?;
            assert!(prompt.approve(HostPermitVariant::SessionLease));
            Ok::<_, TestError>(receiver)
        });
        let first = rig
            .executor
            .execute(
                &context,
                &call(json!({
                    "command": "echo first",
                    "request_host": { "reason": "Tests brauchen Host-Werkzeuge" }
                })),
            )
            .await
            .map_err(ctx("first execute"))?;
        let mut receiver = responder.await.map_err(ctx("responder task"))??;
        assert!(
            matches!(first, ToolOutput::Json { ref content } if content["executed_on"] == "host")
        );

        // Die Phase gilt prozessweit — auch für ein Geschwister-Kind.
        assert!(rig.registry.is_session_approved("sibling-child"));

        // Folgebefehl ohne `request_host`, andere Kind-Sitzung: kein Prompt.
        let sibling = ToolExecutionContext::new(
            SessionId::new(),
            TurnId::new(),
            make_sandbox(&tmp, vec![Permission::ExecuteProcess])?,
        );
        let second = rig
            .executor
            .execute(&sibling, &call(json!({ "command": "echo second" })))
            .await
            .map_err(ctx("second execute"))?;
        assert!(
            matches!(second, ToolOutput::Json { ref content } if content["executed_on"] == "host"),
            "follow-up must run on the host: {second:?}"
        );
        assert!(
            receiver.try_recv().is_err(),
            "no further prompt may be sent during an approved host work phase"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_denial_and_timeout_fail_closed() -> TestResult {
        let tmp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let context = ToolExecutionContext::new(
            SessionId::new(),
            TurnId::new(),
            make_sandbox(&tmp, vec![Permission::ExecuteProcess])?,
        );
        let marker = tmp.path().join("project").join("must_not_exist");
        let command = format!("touch {}", marker.display());

        // Ablehnung.
        let (sender, mut receiver) = host_permit_prompt_channel();
        let rig = tui_child_rig(sender, Duration::from_secs(30))?;
        let responder = tokio::spawn(async move {
            let prompt = receiver.recv().await.ok_or(TestError::Missing("prompt"))?;
            assert!(prompt.deny());
            Ok::<(), TestError>(())
        });
        let denied = rig
            .executor
            .execute(
                &context,
                &call(json!({ "command": command, "request_host": { "reason": "x" } })),
            )
            .await
            .map_err(ctx("denied execute"))?;
        responder.await.map_err(ctx("responder task"))??;
        assert!(
            matches!(denied, ToolOutput::Error { ref message } if message == HOST_ESCALATION_DENIED_MSG),
            "{denied:?}"
        );

        // Zeitablauf: Empfänger lebt, antwortet aber nie.
        let (sender, _silent_receiver) = host_permit_prompt_channel();
        let rig = tui_child_rig(sender, Duration::from_millis(50))?;
        let timed_out = rig
            .executor
            .execute(
                &context,
                &call(json!({ "command": command, "request_host": { "reason": "x" } })),
            )
            .await
            .map_err(ctx("timed-out execute"))?;
        assert!(
            matches!(timed_out, ToolOutput::Error { ref message } if message == HOST_ESCALATION_DENIED_MSG),
            "{timed_out:?}"
        );
        assert!(!marker.exists(), "nothing may run without approval");
        Ok(())
    }

    #[tokio::test]
    async fn test_without_execute_permission_no_request_is_sent() -> TestResult {
        let tmp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let context = ToolExecutionContext::new(
            SessionId::new(),
            TurnId::new(),
            make_sandbox(&tmp, vec![Permission::ReadWorkspace])?,
        );
        let (sender, mut receiver) = host_permit_prompt_channel();
        let rig = tui_child_rig(sender, Duration::from_secs(30))?;
        let output = rig
            .executor
            .execute(
                &context,
                &call(json!({ "command": "echo hi", "request_host": { "reason": "x" } })),
            )
            .await
            .map_err(ctx("execute"))?;
        assert!(matches!(output, ToolOutput::Error { .. }), "{output:?}");
        assert!(
            receiver.try_recv().is_err(),
            "a read-only context must never ask"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_without_tui_channel_the_message_is_clear() -> TestResult {
        let tmp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let context = ToolExecutionContext::new(
            SessionId::new(),
            TurnId::new(),
            make_sandbox(&tmp, vec![Permission::ExecuteProcess])?,
        );
        // Ledger, Registry und sogar ein Sender — aber keine
        // `HostEscalation` (kein TUI-Einstieg).
        let (sender, mut receiver) = host_permit_prompt_channel();
        let provider = ShellToolProvider::default()
            .with_permit_ledger(Arc::new(ProcessPermitLedger::default()))
            .with_host_permit_registry(Arc::new(HostPermitSessionRegistry::default()))
            .with_host_permit_prompts(sender);
        let executor = provider
            .executor(&ToolName::new(TOOL_NAME))
            .ok_or(TestError::Missing("executor"))?;
        let output = executor
            .execute(
                &context,
                &call(json!({ "command": "echo hi", "request_host": { "reason": "x" } })),
            )
            .await
            .map_err(ctx("execute"))?;
        match output {
            ToolOutput::Error { message } => {
                assert_eq!(message, HOST_MODE_REQUIRES_TUI_MSG);
                assert!(message.contains("Host-Mode nötig – nur in der TUI mit Freigabe möglich"));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected error, got {other:?}"
                )));
            }
        }
        assert!(receiver.try_recv().is_err(), "no prompt without TUI wiring");
        Ok(())
    }

    #[tokio::test]
    async fn test_sudo_stays_forbidden_even_with_request_host() -> TestResult {
        let tmp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let context = ToolExecutionContext::new(
            SessionId::new(),
            TurnId::new(),
            make_sandbox(&tmp, vec![Permission::ExecuteProcess])?,
        );
        let (sender, mut receiver) = host_permit_prompt_channel();
        let rig = tui_child_rig(sender, Duration::from_secs(30))?;
        // Auch eine laufende Phase ändert daran nichts.
        rig.registry.mark_global_approval();
        for command in ["sudo ls /root", "doas true", "pkexec id"] {
            let output = rig
                .executor
                .execute(
                    &context,
                    &call(json!({ "command": command, "request_host": { "reason": "x" } })),
                )
                .await
                .map_err(ctx("execute"))?;
            assert!(
                matches!(output, ToolOutput::Error { .. }),
                "{command} must be refused: {output:?}"
            );
        }
        assert!(
            receiver.try_recv().is_err(),
            "sudo must never reach a prompt"
        );
        Ok(())
    }

    /// Folgeauftrag: das `timeout_secs` des Modells gilt (geklemmt auf die
    /// Obergrenze) und ein Zeitablauf nennt Grenze und Maximum; die
    /// Teilausgabe bleibt erhalten. Läuft über den Host-Pfad (aktive Phase),
    /// braucht also kein bwrap.
    #[tokio::test]
    async fn test_requested_timeout_applies_and_timeout_message_is_actionable() -> TestResult {
        let tmp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let context = ToolExecutionContext::new(
            SessionId::new(),
            TurnId::new(),
            make_sandbox(&tmp, vec![Permission::ExecuteProcess])?,
        );
        let registry = Arc::new(HostPermitSessionRegistry::default());
        registry.mark_global_approval();
        let provider = ShellToolProvider::default()
            .with_host_permit_registry(Arc::clone(&registry))
            .with_max_timeout_secs(120);
        let executor = provider
            .executor(&ToolName::new(TOOL_NAME))
            .ok_or(TestError::Missing("executor"))?;
        let output = executor
            .execute(
                &context,
                &call(json!({ "command": "echo partial_ok; sleep 5", "timeout_secs": 1 })),
            )
            .await
            .map_err(ctx("execute"))?;
        match output {
            ToolOutput::Error { message } => {
                assert!(
                    message.starts_with(
                        "Zeitlimit 1 s erreicht – Befehl ggf. mit höherem timeout_secs (max 120)"
                    ),
                    "{message}"
                );
                assert!(message.contains("partial_ok"), "{message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected a timeout error, got {other:?}"
                )));
            }
        }
        // Ein Wert über der Obergrenze wird geklemmt, nicht abgelehnt.
        let clamped = executor
            .execute(
                &context,
                &call(json!({ "command": "echo clamped_ok", "timeout_secs": 99_999 })),
            )
            .await
            .map_err(ctx("execute clamped"))?;
        assert!(
            matches!(clamped, ToolOutput::Json { ref content } if content["exit_code"] == 0),
            "{clamped:?}"
        );
        Ok(())
    }

    #[test]
    fn test_invalid_request_host_is_rejected_as_invalid_arguments() {
        let result = EscalatingShellExecutor::split_request(&call(
            json!({ "command": "echo", "request_host": { "why": "x" } }),
        ));
        assert!(matches!(result, Err(ToolsError::InvalidArguments { .. })));
        let null = EscalatingShellExecutor::split_request(&call(
            json!({ "command": "echo", "request_host": null }),
        ));
        assert!(matches!(null, Ok((_, None))));
    }

    #[test]
    fn test_sandbox_denial_is_annotated_with_a_hint() {
        let provider = ShellToolProvider::default()
            .with_permit_ledger(Arc::new(ProcessPermitLedger::default()))
            .with_host_permit_registry(Arc::new(HostPermitSessionRegistry::default()))
            .with_host_permit_prompts(host_permit_prompt_channel().0)
            .with_host_escalation(HostEscalation::new());
        let wrapper = EscalatingShellExecutor::new(
            provider.build_executor(),
            provider.host_escalation.clone(),
            provider.timeout_policy(),
        );
        let annotated = wrapper.annotate_denial(ToolOutput::json(json!({
            "exit_code": 101,
            "stdout": "",
            "stderr": "error: failed to download from `https://index.crates.io/config.json`",
            "truncated": false,
            "killed_by_output_limit": false,
        })));
        assert!(
            matches!(
                annotated,
                ToolOutput::Json { ref content }
                    if content["sandbox_denial"] == "network"
                        && content["host_mode_hint"]
                            .as_str()
                            .is_some_and(|hint| hint.contains("request_host"))
            ),
            "{annotated:?}"
        );
        let host = wrapper.annotate_denial(ToolOutput::json(json!({
            "exit_code": 1, "stdout": "", "stderr": "Could not resolve host", "executed_on": "host",
        })));
        assert!(
            matches!(host, ToolOutput::Json { ref content } if content.get("sandbox_denial").is_none()),
            "host runs are never annotated: {host:?}"
        );
    }
}
