//! Die Plan-Werkzeuge der Wurzel: `plan.write`, `plan.exit`, `plan.enter`,
//! `ask_user` (Runde 5, Teil F).
//!
//! # Beschreibung
//! | Werkzeug | Wirkung |
//! |---|---|
//! | `plan.write {content, slug?, title?}` | schreibt `.harw/plans/<slug>.md` — das einzige Schreibwerkzeug im Plan-Modus |
//! | `plan.exit {plan_path}` | legt den Plan im Freigabefenster der TUI vor (3 Optionen) |
//! | `plan.enter {reason}` | **schlägt** den Plan-Modus vor; wechselt nur nach Bestätigung |
//! | `ask_user {questions}` | Auswahlfenster mit 1–4 Fragen; rein lesend |
//!
//! # Harte Regeln
//! - **Nur Wurzel, nur TUI.** Der Provider wird ausschließlich an die
//!   Wurzel-Registry gehängt; zusätzlich prüft jeder Aufruf die gebundene
//!   Wurzel-Sitzung ([`PlanSession::is_root`]). `plan.exit`, `plan.enter`
//!   und `ask_user` brauchen den TUI-Fragekanal; ohne ihn antworten sie
//!   fail-closed mit einer klaren Meldung.
//! - **Moduswechsel nur nach Bestätigung.** Die Werkzeuge schalten selbst
//!   keinen Modus um. Die TUI setzt Modus und Freigabe erst, wenn die
//!   Nutzerin im Fenster zustimmt; jeder andere Ausgang (Esc, Zeitablauf,
//!   geschlossenes Fenster, Abbruch) lässt alles, wie es ist.
//! - **`plan.write` nur unter `.harw/plans/`.** Der Pfad entsteht aus dem Slug
//!   ([`crate::plan_file`]); das Modell nennt nie einen Pfad zum Schreiben.
//!
//! # Nebenläufigkeit
//! `Send + Sync`; [`ToolProvider::parallel_safe`] ist `false` für alle vier.

use std::sync::Arc;
use std::time::Duration;

use harw_authority::Permission;
use harw_extension_api::ToolProvider;
use harw_tools::schema_helpers::{object_schema_from_pairs, string_property};
use harw_tools::{
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolOutput, ToolsError,
    schema::JsonSchema,
    spec::{FunctionToolSpec, ToolName, ToolSpec},
};
use harw_types::cancel::CancelToken;
use serde::Deserialize;
use tokio::sync::oneshot;

use crate::ask_user::{self, AskUserArgs};
use crate::plan_file::{self, PlanFileError};
use crate::prompt::{
    AskUserPrompt, PlanEnterPrompt, PlanExitDecision, PlanExitPrompt, PlanUiRequest, PlanUiSender,
};
use crate::session::PlanSession;
use crate::{ASK_USER_TOOL, PLAN_ENTER_TOOL, PLAN_EXIT_TOOL, PLAN_WRITE_TOOL};

/// Wartezeit auf eine Entscheidung im Freigabefenster von `plan.exit`.
/// Großzügig, weil die Nutzerin den Plan lesen soll.
pub const PLAN_EXIT_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Wartezeit auf eine Antwort im Auswahlfenster von `ask_user`.
pub const ASK_USER_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Wartezeit auf die Bestätigung eines `plan.enter`-Vorschlags.
pub const PLAN_ENTER_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Meldung ohne TUI-Kanal für `ask_user` (Wortlaut aus dem Plan).
pub const ASK_USER_NO_UI_MSG: &str = "ask_user: keine Rückfrage möglich (keine interaktive \
     Oberfläche) — triff eine Annahme und nenne sie ausdrücklich in deiner Antwort";

/// Meldung ohne TUI-Kanal für `plan.exit`/`plan.enter`.
pub const PLAN_NO_UI_MSG: &str = "keine Freigabe möglich: der Plan-Modus-Wechsel braucht die \
     interaktive TUI (fail-closed). Lege den Plan als Text vor und warte auf die Nutzerin";

/// Meldung für Nicht-Wurzel-Aufrufer.
pub const NOT_ROOT_MSG: &str =
    "nur der Wurzel-Agent in der TUI darf dieses Werkzeug nutzen (Kind-Agenten nie)";

/// Registriert die vier Plan-Werkzeuge der Wurzel.
///
/// # Beschreibung
/// Ohne [`PlanUiSender`] (jeder Nicht-TUI-Einstieg) bleiben die Werkzeuge
/// sichtbar, antworten aber fail-closed: `ask_user` mit
/// [`ASK_USER_NO_UI_MSG`], `plan.exit`/`plan.enter` mit [`PLAN_NO_UI_MSG`].
/// `plan.write` funktioniert auch ohne TUI (es schreibt nur die Plan-Datei).
#[derive(Debug, Clone)]
pub struct PlanToolProvider {
    session: PlanSession,
    ui: Option<PlanUiSender>,
    exit_timeout: Duration,
    ask_timeout: Duration,
    enter_timeout: Duration,
}

impl PlanToolProvider {
    /// Die Werkzeugnamen dieses Providers.
    pub const TOOL_NAMES: &'static [&'static str] = &[
        PLAN_WRITE_TOOL,
        PLAN_EXIT_TOOL,
        PLAN_ENTER_TOOL,
        ASK_USER_TOOL,
    ];

    /// Die Rechteklasse je Werkzeug (gleiche Reihenfolge wie
    /// [`Self::TOOL_NAMES`]): alle `ReadWorkspace`. `plan.write` schreibt nur
    /// das Harness-Artefakt `.harw/plans/<slug>.md` und muss unter der
    /// Plan-Decke (ohne `WriteWorkspace`) laufen.
    pub const TOOL_PERMISSIONS: &'static [Option<Permission>] = &[
        Some(Permission::ReadWorkspace),
        Some(Permission::ReadWorkspace),
        Some(Permission::ReadWorkspace),
        Some(Permission::ReadWorkspace),
    ];

    /// Baut den Provider.
    ///
    /// # Arguments
    /// - `session` ([`PlanSession`]): geteilter Plan-Zustand der Montage.
    /// - `ui` (`Option<PlanUiSender>`): TUI-Fragekanal; `None` außerhalb der TUI.
    #[must_use]
    pub fn new(session: PlanSession, ui: Option<PlanUiSender>) -> Self {
        Self {
            session,
            ui,
            exit_timeout: PLAN_EXIT_TIMEOUT,
            ask_timeout: ASK_USER_TIMEOUT,
            enter_timeout: PLAN_ENTER_TIMEOUT,
        }
    }

    /// Nur Tests: kürzere Wartezeiten.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_timeouts(mut self, timeout: Duration) -> Self {
        self.exit_timeout = timeout;
        self.ask_timeout = timeout;
        self.enter_timeout = timeout;
        self
    }

    fn spec(name: &str, description: &str, parameters: JsonSchema) -> ToolSpec {
        ToolSpec::Function(FunctionToolSpec {
            name: ToolName::new(name),
            description: description.to_owned(),
            parameters,
            strict: false,
        })
    }
}

impl ToolProvider for PlanToolProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![
            Self::spec(
                PLAN_WRITE_TOOL,
                &format!(
                    "Write (or overwrite) your plan as Markdown to {}/<slug>.md. The only \
                     writing tool in plan mode; it cannot write anywhere else. Repeated calls in \
                     this session overwrite the same plan unless you pass a new slug. Suggested \
                     structure: Context / Approach in steps / Affected files / Verification.",
                    plan_file::plan_display_prefix()
                ),
                object_schema_from_pairs(
                    &[
                        ("content", string_property("The complete plan as Markdown.")),
                        (
                            "slug",
                            string_property(
                                "Optional file name: lowercase a-z, 0-9 and '-', max 64 chars. \
                                 Omit to keep the current plan of this session.",
                            ),
                        ),
                        (
                            "title",
                            string_property(
                                "Optional title; used to derive the slug of a new plan.",
                            ),
                        ),
                    ],
                    &["content"],
                ),
            ),
            Self::spec(
                PLAN_EXIT_TOOL,
                "Present the finished plan to the user for approval (like ExitPlanMode). The TUI \
                 shows it with three options: implement in auto mode, implement with per-change \
                 approval, or keep planning with feedback. Only works in plan mode, only in the \
                 interactive TUI. Call it only after plan.write.",
                object_schema_from_pairs(
                    &[(
                        "plan_path",
                        string_property(&format!(
                            "The plan written with plan.write, e.g. {}/<slug>.md",
                            plan_file::plan_display_prefix()
                        )),
                    )],
                    &["plan_path"],
                ),
            ),
            Self::spec(
                PLAN_ENTER_TOOL,
                "Suggest switching to plan mode (read-only exploration, then a written plan) for \
                 a larger or risky task. The user must confirm; nothing changes without her.",
                object_schema_from_pairs(
                    &[(
                        "reason",
                        string_property("One sentence why planning first is worth it."),
                    )],
                    &["reason"],
                ),
            ),
            Self::spec(
                ASK_USER_TOOL,
                "Ask the user 1-4 multiple-choice questions (2-4 options each, free text \
                 'Other' is always offered) in a selection window and get her answers. Use it \
                 to clarify requirements instead of guessing. Read-only. Not available outside \
                 the interactive TUI: then make an assumption and state it.",
                ask_user::parameter_schema(),
            ),
        ]
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        let kind = match name.as_str() {
            PLAN_WRITE_TOOL => PlanToolKind::Write,
            PLAN_EXIT_TOOL => PlanToolKind::Exit,
            PLAN_ENTER_TOOL => PlanToolKind::Enter,
            ASK_USER_TOOL => PlanToolKind::AskUser,
            _ => return None,
        };
        Some(Arc::new(PlanToolExecutor {
            kind,
            provider: self.clone(),
        }))
    }

    fn parallel_safe(&self, _name: &ToolName) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanToolKind {
    Write,
    Exit,
    Enter,
    AskUser,
}

impl PlanToolKind {
    fn name(self) -> &'static str {
        match self {
            Self::Write => PLAN_WRITE_TOOL,
            Self::Exit => PLAN_EXIT_TOOL,
            Self::Enter => PLAN_ENTER_TOOL,
            Self::AskUser => ASK_USER_TOOL,
        }
    }
}

struct PlanToolExecutor {
    kind: PlanToolKind,
    provider: PlanToolProvider,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    content: String,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExitArgs {
    plan_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnterArgs {
    reason: String,
}

fn parse<T: for<'de> Deserialize<'de>>(tool: &str, call: &ToolCall) -> Result<T, ToolsError> {
    serde_json::from_value(call.arguments.clone()).map_err(|err| ToolsError::InvalidArguments {
        name: tool.to_owned(),
        reason: err.to_string(),
    })
}

/// Wartet auf eine Antwort, höchstens `timeout`, abbrechbar.
///
/// # Returns
/// `Ok(Some(value))` bei Antwort, `Ok(None)` bei Zeitablauf oder
/// fallengelassener Frage, `Err(Cancelled)` bei Turn-Abbruch.
async fn await_answer<T>(
    answer: oneshot::Receiver<T>,
    timeout: Duration,
    cancel: Option<&CancelToken>,
) -> Result<Option<T>, ToolsError> {
    tokio::select! {
        waited = tokio::time::timeout(timeout, answer) => Ok(waited.ok().and_then(Result::ok)),
        () = cancelled(cancel) => Err(ToolsError::Cancelled),
    }
}

async fn cancelled(cancel: Option<&CancelToken>) {
    match cancel {
        Some(cancel) => cancel.cancelled().await,
        None => std::future::pending().await,
    }
}

impl PlanToolExecutor {
    fn session(&self) -> &PlanSession {
        &self.provider.session
    }

    fn send(&self, request: PlanUiRequest) -> bool {
        self.provider
            .ui
            .as_ref()
            .is_some_and(|ui| ui.send(request).is_ok())
    }

    fn write(&self, args: WriteArgs) -> ToolOutput {
        let session = self.session();
        let slug = match args.slug.as_deref() {
            Some(raw) => match plan_file::validate_slug(raw) {
                Ok(slug) => slug,
                Err(error) => return ToolOutput::error(format!("{PLAN_WRITE_TOOL}: {error}")),
            },
            None => session.current_slug().unwrap_or_else(|| {
                args.title
                    .as_deref()
                    .and_then(plan_file::slugify)
                    .unwrap_or_else(plan_file::fallback_slug)
            }),
        };
        match session.dir().write(&slug, &args.content) {
            Ok(outcome) => {
                session.set_current_slug(Some(outcome.slug.clone()));
                ToolOutput::json(serde_json::json!({
                    "plan_path": plan_file::display_path(&outcome.slug),
                    "slug": outcome.slug,
                    "bytes": outcome.bytes,
                    "overwritten": outcome.overwritten,
                    "empty": args.content.trim().is_empty(),
                    "next": "Wenn der Plan fertig ist: plan.exit mit diesem plan_path.",
                }))
            }
            Err(error) => ToolOutput::error(format!("{PLAN_WRITE_TOOL}: {error}")),
        }
    }

    async fn exit(
        &self,
        args: ExitArgs,
        cancel: Option<&CancelToken>,
    ) -> Result<ToolOutput, ToolsError> {
        let session = self.session();
        if self.provider.ui.is_none() {
            return Ok(ToolOutput::error(format!(
                "{PLAN_EXIT_TOOL}: {PLAN_NO_UI_MSG}"
            )));
        }
        if !session.lock().is_locked() {
            return Ok(ToolOutput::error(format!(
                "{PLAN_EXIT_TOOL}: nur im Plan-Modus möglich (Shift+Tab bis „plan“, /plan oder \
                 plan.enter)"
            )));
        }
        let slug = match session.dir().resolve(&args.plan_path) {
            Ok(slug) => slug,
            Err(error) => return Ok(ToolOutput::error(format!("{PLAN_EXIT_TOOL}: {error}"))),
        };
        let content = match session.dir().read(&slug) {
            Ok(content) => content,
            Err(PlanFileError::NotFound(_)) => {
                return Ok(ToolOutput::error(format!(
                    "{PLAN_EXIT_TOOL}: {} existiert nicht — zuerst plan.write aufrufen",
                    plan_file::display_path(&slug)
                )));
            }
            Err(error) => return Ok(ToolOutput::error(format!("{PLAN_EXIT_TOOL}: {error}"))),
        };
        let empty = content.trim().is_empty();
        let path = match session.dir().path_for(&slug) {
            Ok(path) => path,
            Err(error) => return Ok(ToolOutput::error(format!("{PLAN_EXIT_TOOL}: {error}"))),
        };
        let (prompt, decision) = PlanExitPrompt::new(slug.clone(), path, content.clone());
        if !self.send(PlanUiRequest::ExitPlan(prompt)) {
            return Ok(ToolOutput::error(format!(
                "{PLAN_EXIT_TOOL}: {PLAN_NO_UI_MSG}"
            )));
        }
        let decision = await_answer(decision, self.provider.exit_timeout, cancel).await?;
        let display = plan_file::display_path(&slug);
        let warning = empty.then_some("Warnung: der Plan war leer.");
        Ok(match decision {
            Some(approved @ (PlanExitDecision::ImplementAuto | PlanExitDecision::ImplementAsk)) => {
                let approval = if approved == PlanExitDecision::ImplementAuto {
                    "auto"
                } else {
                    "ask"
                };
                session.set_current_slug(Some(slug.clone()));
                session.pinned().pin(&slug, &content);
                ToolOutput::json(serde_json::json!({
                    "approved": true,
                    "plan_path": display,
                    "mode": "work",
                    "approval": approval,
                    "warning": warning,
                    "next": "Die Nutzerin hat den Plan freigegeben. Er ist ab jetzt angeheftet. \
                             Beende diesen Turn mit einer kurzen Bestätigung — die Umsetzung \
                             startet im nächsten Turn im Modus work (die Werkzeugsperre des \
                             Plan-Modus gilt bis zur Turn-Grenze).",
                }))
            }
            Some(PlanExitDecision::KeepPlanning { feedback }) => {
                let feedback = feedback.trim();
                let user_feedback = (!feedback.is_empty()).then_some(feedback);
                ToolOutput::json(serde_json::json!({
                    "approved": false,
                    "plan_path": display,
                    "user_feedback": user_feedback,
                    "warning": warning,
                    "next": "Nutzer-Nachricht: der Plan ist NICHT freigegeben. Bleibe im \
                             Plan-Modus, arbeite die Rückmeldung ein (plan.write) und lege den \
                             Plan erneut mit plan.exit vor.",
                }))
            }
            None => ToolOutput::error(format!(
                "{PLAN_EXIT_TOOL}: keine Entscheidung (Fenster geschlossen oder Zeitablauf) — der \
                 Plan-Modus bleibt aktiv; frage die Nutzerin, wie es weitergehen soll"
            )),
        })
    }

    async fn enter(
        &self,
        args: EnterArgs,
        cancel: Option<&CancelToken>,
    ) -> Result<ToolOutput, ToolsError> {
        if self.session().lock().is_locked() {
            return Ok(ToolOutput::text(
                "Der Plan-Modus ist bereits aktiv. Erkunde, frage nach (ask_user), schreibe den \
                 Plan (plan.write) und lege ihn mit plan.exit vor.",
            ));
        }
        let reason = args.reason.trim();
        if reason.is_empty()
            || reason.chars().count() > 500
            || reason.chars().any(|c| ask_user::is_forbidden_char(c, true))
        {
            return Ok(ToolOutput::error(format!(
                "{PLAN_ENTER_TOOL}: reason muss ein einzeiliger Satz ohne Steuerzeichen sein \
                 (höchstens 500 Zeichen)"
            )));
        }
        if self.provider.ui.is_none() {
            return Ok(ToolOutput::error(format!(
                "{PLAN_ENTER_TOOL}: {PLAN_NO_UI_MSG}"
            )));
        }
        let (prompt, answer) = PlanEnterPrompt::new(reason.to_owned());
        if !self.send(PlanUiRequest::EnterPlan(prompt)) {
            return Ok(ToolOutput::error(format!(
                "{PLAN_ENTER_TOOL}: {PLAN_NO_UI_MSG}"
            )));
        }
        Ok(
            match await_answer(answer, self.provider.enter_timeout, cancel).await? {
                Some(true) => ToolOutput::text(
                    "Die Nutzerin hat zugestimmt: der Plan-Modus ist ab sofort aktiv. Schreibende \
                     und ausführende Werkzeuge sind gesperrt. Erkunde zuerst, frage bei Unklarheit \
                     mit ask_user nach, schreibe den Plan mit plan.write und lege ihn mit \
                     plan.exit vor.",
                ),
                Some(false) | None => ToolOutput::text(
                    "Die Nutzerin hat den Plan-Modus nicht bestätigt. Arbeite im bisherigen Modus \
                     weiter.",
                ),
            },
        )
    }

    async fn ask(
        &self,
        args: AskUserArgs,
        cancel: Option<&CancelToken>,
    ) -> Result<ToolOutput, ToolsError> {
        if self.provider.ui.is_none() {
            return Ok(ToolOutput::error(ASK_USER_NO_UI_MSG));
        }
        if let Err(why) = ask_user::validate(&args) {
            return Ok(ToolOutput::error(format!("{ASK_USER_TOOL}: {why}")));
        }
        let (prompt, answer) = AskUserPrompt::new(args.questions);
        if !self.send(PlanUiRequest::AskUser(prompt)) {
            return Ok(ToolOutput::error(ASK_USER_NO_UI_MSG));
        }
        Ok(
            match await_answer(answer, self.provider.ask_timeout, cancel).await? {
                Some(answer) => ToolOutput::json(ask_user::answer_json(&answer)),
                None => ToolOutput::error(
                    "ask_user: die Nutzerin hat nicht geantwortet (abgebrochen oder Zeitablauf) — \
                     triff eine Annahme und nenne sie ausdrücklich",
                ),
            },
        )
    }
}

impl ToolExecutor for PlanToolExecutor {
    /// Führt eines der vier Plan-Werkzeuge aus.
    ///
    /// # Beschreibung
    /// 1. Argumente parsen (`deny_unknown_fields`).
    /// 2. `ReadWorkspace` prüfen.
    /// 3. Nur die gebundene Wurzel-Sitzung darf aufrufen.
    /// 4. Werkzeug ausführen; Fragen an die TUI warten höchstens ihre Frist.
    ///
    /// # Errors
    /// [`ToolsError::InvalidArguments`] bei unpassenden Argumenten,
    /// [`ToolsError::Cancelled`] bei Abbruch; alles andere als [`ToolOutput`].
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let tool = self.kind.name();
            if let Some(denied) = harw_tools::sandbox_guard::require_permission(
                context,
                Permission::ReadWorkspace,
                tool,
            ) {
                return Ok(denied);
            }
            if !self.session().is_root(context.session_id().as_str()) {
                tracing::warn!(tool, "plan_tools.denied_non_root");
                return Ok(ToolOutput::error(format!("{tool}: {NOT_ROOT_MSG}")));
            }
            match self.kind {
                PlanToolKind::Write => Ok(self.write(parse(tool, call)?)),
                PlanToolKind::Exit => self.exit(parse(tool, call)?, context.cancel()).await,
                PlanToolKind::Enter => self.enter(parse(tool, call)?, context.cancel()).await,
                PlanToolKind::AskUser => self.ask(parse(tool, call)?, context.cancel()).await,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan_file::PlanDir;
    use crate::prompt::{AskUserAnswer, QuestionAnswer, plan_ui_channel};
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};

    struct Fixture {
        _temp: tempfile::TempDir,
        session: PlanSession,
        root: SessionId,
        sandbox: SandboxSpec,
    }

    fn fixture() -> TestResult<Fixture> {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let workspace = temp.path().join("project");
        std::fs::create_dir_all(&workspace).map_err(ctx("workspace"))?;
        let registry = WorkspaceRegistry::build(
            temp.path(),
            [WorkspaceRegistration {
                tenant: TenantId::from_str("tenant"),
                workspace: WorkspaceId::from_str("project"),
                root: workspace.clone(),
            }],
        )
        .map_err(ctx("registry"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("tenant"),
                &WorkspaceId::from_str("project"),
            )
            .map_err(ctx("binding"))?;
        // Plan-Decke: kein WriteWorkspace, kein ExecuteProcess.
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([
                Permission::ReadWorkspace,
                Permission::ReadCargoRegistry,
                Permission::NetworkAccess,
            ]),
        );
        let session = PlanSession::new(PlanDir::new(workspace.join(".harw").join("plans")), true);
        let root = SessionId::new();
        session.bind_root(root.as_str());
        Ok(Fixture {
            _temp: temp,
            session,
            root,
            sandbox,
        })
    }

    fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(name),
            arguments,
        }
    }

    async fn run(
        provider: &PlanToolProvider,
        session: &SessionId,
        sandbox: &SandboxSpec,
        name: &str,
        arguments: serde_json::Value,
    ) -> TestResult<ToolOutput> {
        let executor = provider
            .executor(&ToolName::new(name))
            .ok_or(TestError::Missing("executor"))?;
        let context = ToolExecutionContext::new(session.clone(), TurnId::new(), sandbox.clone());
        executor
            .execute(&context, &call(name, arguments))
            .await
            .map_err(ctx("execute"))
    }

    fn is_error(output: &ToolOutput) -> bool {
        matches!(output, ToolOutput::Error { .. })
    }

    fn error_text(output: &ToolOutput) -> String {
        match output {
            ToolOutput::Error { message } => message.clone(),
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn provider_lists_the_four_tools_with_executors_and_permissions() {
        let provider = PlanToolProvider::new(
            PlanSession::new(PlanDir::new("/p/.harw/plans"), false),
            None,
        );
        let names: Vec<String> = provider
            .tools()
            .iter()
            .map(|spec| spec.name().to_owned())
            .collect();
        assert_eq!(names, PlanToolProvider::TOOL_NAMES);
        assert_eq!(
            PlanToolProvider::TOOL_NAMES.len(),
            PlanToolProvider::TOOL_PERMISSIONS.len()
        );
        for name in PlanToolProvider::TOOL_NAMES {
            assert!(provider.executor(&ToolName::new(*name)).is_some(), "{name}");
            assert!(!provider.parallel_safe(&ToolName::new(*name)));
        }
        assert!(provider.executor(&ToolName::new("plan")).is_none());
    }

    #[tokio::test]
    async fn plan_write_writes_only_under_harw_plans_and_reuses_the_session_slug() -> TestResult {
        let fx = fixture()?;
        let provider = PlanToolProvider::new(fx.session.clone(), None);
        let first = run(
            &provider,
            &fx.root,
            &fx.sandbox,
            PLAN_WRITE_TOOL,
            serde_json::json!({ "content": "# Plan A", "title": "Auth umbauen" }),
        )
        .await?;
        assert!(!is_error(&first), "{first:?}");
        assert_eq!(fx.session.current_slug().as_deref(), Some("auth-umbauen"));
        let second = run(
            &provider,
            &fx.root,
            &fx.sandbox,
            PLAN_WRITE_TOOL,
            serde_json::json!({ "content": "# Plan B" }),
        )
        .await?;
        assert!(!is_error(&second));
        assert_eq!(
            fx.session.dir().read("auth-umbauen").map_err(ctx("read"))?,
            "# Plan B",
            "zweiter Aufruf überschreibt dieselbe Datei"
        );
        for escape in ["../../src/main", "/etc/passwd", "a/b", "..", "SRC"] {
            let out = run(
                &provider,
                &fx.root,
                &fx.sandbox,
                PLAN_WRITE_TOOL,
                serde_json::json!({ "content": "x", "slug": escape }),
            )
            .await?;
            assert!(is_error(&out), "{escape} muss abgelehnt werden");
        }
        // Unbekannte Felder (z. B. ein Pfad) werden nie angenommen.
        let executor = provider
            .executor(&ToolName::new(PLAN_WRITE_TOOL))
            .ok_or(TestError::Missing("executor"))?;
        let context = ToolExecutionContext::new(fx.root.clone(), TurnId::new(), fx.sandbox.clone());
        let with_path = call(
            PLAN_WRITE_TOOL,
            serde_json::json!({ "content": "x", "path": "/tmp/x.md" }),
        );
        assert!(executor.execute(&context, &with_path).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn child_sessions_are_refused_for_every_tool() -> TestResult {
        let fx = fixture()?;
        let (sender, _receiver) = plan_ui_channel();
        let provider = PlanToolProvider::new(fx.session.clone(), Some(sender));
        let child = SessionId::new();
        for (name, args) in [
            (PLAN_WRITE_TOOL, serde_json::json!({ "content": "x" })),
            (PLAN_EXIT_TOOL, serde_json::json!({ "plan_path": "x" })),
            (PLAN_ENTER_TOOL, serde_json::json!({ "reason": "groß" })),
            (
                ASK_USER_TOOL,
                serde_json::json!({ "questions": [{ "question": "?", "options": [
                    { "label": "a" }, { "label": "b" }] }] }),
            ),
        ] {
            let out = run(&provider, &child, &fx.sandbox, name, args).await?;
            assert!(error_text(&out).contains(NOT_ROOT_MSG), "{name}: {out:?}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn without_tui_ask_user_and_plan_exit_fail_closed() -> TestResult {
        let fx = fixture()?;
        let provider = PlanToolProvider::new(fx.session.clone(), None);
        let ask = run(
            &provider,
            &fx.root,
            &fx.sandbox,
            ASK_USER_TOOL,
            serde_json::json!({ "questions": [{ "question": "Welche?", "options": [
                { "label": "a" }, { "label": "b" }] }] }),
        )
        .await?;
        assert!(error_text(&ask).contains("triff eine Annahme"), "{ask:?}");
        let exit = run(
            &provider,
            &fx.root,
            &fx.sandbox,
            PLAN_EXIT_TOOL,
            serde_json::json!({ "plan_path": "x" }),
        )
        .await?;
        assert!(error_text(&exit).contains("fail-closed"), "{exit:?}");
        let enter = run(
            &provider,
            &fx.root,
            &fx.sandbox,
            PLAN_ENTER_TOOL,
            serde_json::json!({ "reason": "groß" }),
        )
        .await;
        // Plan-Modus ist in der Fixture schon aktiv → Hinweis statt Fehler.
        assert!(!is_error(&enter?));
        Ok(())
    }

    #[tokio::test]
    async fn ask_user_delivers_the_answer_as_tool_result() -> TestResult {
        let fx = fixture()?;
        let (sender, mut receiver) = plan_ui_channel();
        let provider = PlanToolProvider::new(fx.session.clone(), Some(sender));
        let ui = tokio::spawn(async move {
            if let Some(PlanUiRequest::AskUser(prompt)) = receiver.recv().await {
                let question = prompt.questions()[0].question.clone();
                prompt.answer(AskUserAnswer {
                    answers: vec![QuestionAnswer {
                        question,
                        selected: vec!["b".to_owned()],
                        other: None,
                    }],
                });
            }
        });
        let out = run(
            &provider,
            &fx.root,
            &fx.sandbox,
            ASK_USER_TOOL,
            serde_json::json!({ "questions": [{ "question": "Welche?", "options": [
                { "label": "a" }, { "label": "b" }] }] }),
        )
        .await?;
        ui.await.map_err(ctx("ui task"))?;
        let ToolOutput::Json { content } = out else {
            return Err(TestError::Unexpected(format!("{out:?}")));
        };
        assert_eq!(content["answers"][0]["selected"][0], "b");
        Ok(())
    }

    #[tokio::test]
    async fn plan_exit_pins_the_plan_only_after_approval() -> TestResult {
        let fx = fixture()?;
        fx.session
            .dir()
            .write("auth", "# Plan\n1. Schritt")
            .map_err(ctx("write"))?;
        let (sender, mut receiver) = plan_ui_channel();
        let provider = PlanToolProvider::new(fx.session.clone(), Some(sender));
        let ui = tokio::spawn(async move {
            if let Some(PlanUiRequest::ExitPlan(prompt)) = receiver.recv().await {
                assert_eq!(prompt.display_path(), ".harw/plans/auth.md");
                prompt.decide(PlanExitDecision::ImplementAuto);
            }
        });
        let out = run(
            &provider,
            &fx.root,
            &fx.sandbox,
            PLAN_EXIT_TOOL,
            serde_json::json!({ "plan_path": ".harw/plans/auth.md" }),
        )
        .await?;
        ui.await.map_err(ctx("ui task"))?;
        let ToolOutput::Json { content } = out else {
            return Err(TestError::Unexpected(format!("{out:?}")));
        };
        assert_eq!(content["approved"], true);
        assert_eq!(content["approval"], "auto");
        assert!(
            fx.session
                .pinned()
                .get()
                .is_some_and(|doc| doc.content.contains("1. Schritt"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn plan_exit_rejection_forwards_the_feedback_and_pins_nothing() -> TestResult {
        let fx = fixture()?;
        fx.session.dir().write("p", "# P").map_err(ctx("write"))?;
        let (sender, mut receiver) = plan_ui_channel();
        let provider = PlanToolProvider::new(fx.session.clone(), Some(sender));
        let ui = tokio::spawn(async move {
            if let Some(PlanUiRequest::ExitPlan(prompt)) = receiver.recv().await {
                prompt.decide(PlanExitDecision::KeepPlanning {
                    feedback: "Bitte Tests zuerst".to_owned(),
                });
            }
        });
        let out = run(
            &provider,
            &fx.root,
            &fx.sandbox,
            PLAN_EXIT_TOOL,
            serde_json::json!({ "plan_path": "p" }),
        )
        .await?;
        ui.await.map_err(ctx("ui task"))?;
        let ToolOutput::Json { content } = out else {
            return Err(TestError::Unexpected(format!("{out:?}")));
        };
        assert_eq!(content["approved"], false);
        assert_eq!(content["user_feedback"], "Bitte Tests zuerst");
        assert!(fx.session.pinned().get().is_none());
        assert!(fx.session.lock().is_locked(), "Plan-Modus bleibt");
        Ok(())
    }

    #[tokio::test]
    async fn plan_exit_without_decision_changes_nothing() -> TestResult {
        let fx = fixture()?;
        fx.session.dir().write("p", "# P").map_err(ctx("write"))?;
        let (sender, mut receiver) = plan_ui_channel();
        let provider = PlanToolProvider::new(fx.session.clone(), Some(sender))
            .with_timeouts(Duration::from_millis(50));
        let ui = tokio::spawn(async move {
            // Fenster schließt ohne Entscheidung (Drop).
            drop(receiver.recv().await);
        });
        let out = run(
            &provider,
            &fx.root,
            &fx.sandbox,
            PLAN_EXIT_TOOL,
            serde_json::json!({ "plan_path": "p" }),
        )
        .await?;
        ui.await.map_err(ctx("ui task"))?;
        assert!(is_error(&out));
        assert!(fx.session.pinned().get().is_none());
        Ok(())
    }

    #[tokio::test]
    async fn plan_exit_outside_plan_mode_or_outside_plans_dir_is_refused() -> TestResult {
        let fx = fixture()?;
        let (sender, _receiver) = plan_ui_channel();
        let provider = PlanToolProvider::new(fx.session.clone(), Some(sender));
        let outside = run(
            &provider,
            &fx.root,
            &fx.sandbox,
            PLAN_EXIT_TOOL,
            serde_json::json!({ "plan_path": "../../etc/passwd" }),
        )
        .await?;
        assert!(is_error(&outside));
        fx.session.lock().set(false);
        let not_plan = run(
            &provider,
            &fx.root,
            &fx.sandbox,
            PLAN_EXIT_TOOL,
            serde_json::json!({ "plan_path": "p" }),
        )
        .await?;
        assert!(error_text(&not_plan).contains("nur im Plan-Modus"));
        Ok(())
    }

    #[tokio::test]
    async fn plan_enter_only_proposes_and_reports_the_users_answer() -> TestResult {
        let fx = fixture()?;
        fx.session.lock().set(false);
        let (sender, mut receiver) = plan_ui_channel();
        let provider = PlanToolProvider::new(fx.session.clone(), Some(sender));
        let ui = tokio::spawn(async move {
            if let Some(PlanUiRequest::EnterPlan(prompt)) = receiver.recv().await {
                prompt.answer(false);
            }
        });
        let out = run(
            &provider,
            &fx.root,
            &fx.sandbox,
            PLAN_ENTER_TOOL,
            serde_json::json!({ "reason": "Großer Umbau" }),
        )
        .await?;
        ui.await.map_err(ctx("ui task"))?;
        assert!(!is_error(&out));
        assert!(
            !fx.session.lock().is_locked(),
            "das Werkzeug selbst schaltet nie um"
        );
        Ok(())
    }
}
