# CONTRACTS-W2d2 — Schnitt und gefrorene Verträge (D5, D6, D7, D3b)

Status: vom Orchestrator eingefroren. Entscheidungen E1–E11 (§4) angenommen wie empfohlen.
Hinweis: Der Tree kompiliert vor Abschluss von W2d-2 nicht (`RegistryProfile` ohne `Default`,
`DefaultApprovalPolicy` kein Unit-Struct mehr; Aufrufer app.rs:1732, chat.rs:575, app.rs:5549/5668).
harw-cli ist Binär-Crate: jedes Item ohne Prod-Aufrufer = dead_code unter -D warnings.

## 0. RuntimeAssembly heute (harw-runtime/src/assembly.rs)

Vorhanden: `builder(spec)` :1031; Builder `model/stores/plan_services/memory/session_controller/session_events/secret_resolver/contributor/root_session_id/build` :350–:460;
Accessoren `spec/profile/config/trust_report/project/principal/spawn_context/sandbox/ceiling/root_activation/budget/turn_limits/network_scope/operations/services/model/state_store/job_store/approval_store/spawner/root_session_id` :1048–:1185;
`op_context(surface, session, turn, sandbox)` :1201; `new_root_session(id, events, turn_events, responder) -> RuntimeResult<RootSession>` :1236 (Registry genau einmal :1271; id muss root_session_id sein :1243; Responder nur bei AskResolution::Interactive :1258);
`close_session` :1321; `rights_snapshot` :1346; `RootSession{session, approval_mode}` :201.

Fehlt: Lesezugriff Plan/Memory; Verengung Profil/Identität/Rechte durch Aufrufer; Config-Vorgabe für mode/active_agent (Aufrufer löst auf, E6); GoalContextProvider nur via AssemblyContributor; kein Accessor für session_events (bei SpawnerPolicy::BuiltinRoles denselben Sender an Builder und new_root_session geben, assembly.rs:925-929).

## 1. Gefrorene Signaturen

### 1.1 harw-runtime (R0, additiv)

```rust
// services.rs — impl RuntimeServices
#[must_use] pub fn plan(&self) -> Option<&PlanServices>;
#[must_use] pub fn memory(&self) -> Option<&Arc<dyn Memory>>;
// assembly.rs — impl RuntimeAssembly
#[must_use] pub fn plan_services(&self) -> Option<&PlanServices>;
#[must_use] pub fn memory(&self) -> Option<&Arc<dyn Memory>>;
#[must_use] pub const fn approval_mode(&self) -> &ApprovalModeCell;
// assembly.rs — neu
#[derive(Debug, Clone)]
pub struct RuntimeNarrowing {
    pub registry_profile: harw_registry_defaults::profile::RegistryProfile,
    pub identity: harw_registry_defaults::profile::IdentityOverrides,
    pub permissions: harw_sandbox::PermissionSet,
}
impl RuntimeAssemblyBuilder { #[must_use] pub fn narrowing(mut self, narrowing: RuntimeNarrowing) -> Self; }
// lib.rs
pub use assembly::RuntimeNarrowing;
```
Bau-Semantik (fail-closed, sonst `RuntimeError::Registry`): Einstieg `Full` erlaubt nur `Full|ReadOnlyExplore|NoTools`; Einstieg `NoTools` nur `NoTools`; sonst Fehler (R0 prüft `ReadOnlyExplore`⊆`Full` inkl. `deps.*`, profile.rs:340). Sandbox = `root_sandbox(entry, root).restrict(&permissions)`, Ergebnis ⊆ Profilrechte. `IdentityOverrides` ersetzen Vorgabe (assembly.rs:535-538), `spec.active_agent` hat Vorrang. Keine Wirkung auf activation/Spawner/Chain. rights_matrix.rs unverändert.

### 1.2 harw-tui

```rust
// runtime_root.rs (neu; lib.rs: pub(crate) mod runtime_root; pub use runtime_root::{TuiAssemblyFactory, TuiResume, TuiRunOptions, TuiSessionWiring, run_tui};)
pub struct TuiSessionWiring { controller: Arc<TuiSessionController>, events_tx: UnboundedSender<SessionEvent>, events_rx: UnboundedReceiver<SessionEvent> } // eigenes Debug
impl TuiSessionWiring {
    #[must_use] pub fn new() -> Self;
    #[must_use] pub fn install(&self, builder: harw_runtime::RuntimeAssemblyBuilder) -> harw_runtime::RuntimeAssemblyBuilder; // .session_controller(..).session_events(events_tx.clone())
}
impl Default for TuiSessionWiring;
pub trait TuiAssemblyFactory {
    fn assemble(&self, root_session_id: Option<SessionId>) -> Result<(Arc<harw_runtime::RuntimeAssembly>, TuiSessionWiring), String>;
}
pub struct TuiResume { pub selector: Box<dyn ResumeSessionSelector>, pub factory: Box<dyn TuiAssemblyFactory> }
pub struct TuiRunOptions { pub wiring: TuiSessionWiring, pub resume: Option<TuiResume> } // eigenes Debug
pub fn run_tui(assembly: Arc<harw_runtime::RuntimeAssembly>, options: TuiRunOptions) -> Result<(), TuiError>;
// lib.rs: pub use app::{ChatApp, TuiError}; run_chat_tui entfällt (lib.rs:37)
```
Schnittstelle app.rs → runtime_root.rs (T2a liefert, T1 konsumiert):
```rust
pub(crate) const WELCOME: &str;                                                   // app.rs:136
pub(crate) struct TerminalGuard; impl TerminalGuard { pub(crate) fn enter() -> Result<Self, TuiError>; } // :1113/:1130
pub(crate) async fn frame_scheduler(frame_rx: UnboundedReceiver<()>, tui_tx: UnboundedSender<TuiEvent>); // :3096
pub(crate) async fn run_loop(/* Parameter unverändert, app.rs:2579-2591 */) -> Result<TuiRunOutcome, TuiError>;
pub(crate) fn install_loaded_history(session: &mut AgentSession, app: &mut ChatApp, history: ConversationHistory); // :1288
pub(crate) fn tui_error_from_approval_driver(error: ApprovalDriverError) -> TuiError; // :1578
impl ChatApp {
    pub(crate) fn push_lines(&mut self, lines: Vec<Line<'static>>);               // :1020
    #[must_use] pub(crate) fn with_runtime(self, assembly: Arc<harw_runtime::RuntimeAssembly>) -> Self;
    #[must_use] pub(crate) fn runtime(&self) -> Option<&Arc<harw_runtime::RuntimeAssembly>>;
    // unverändert: with_memory (pub), with_session_controller, with_managed_spawner, with_project_root (pub), with_plan_services (pub), set_active_mode, managed_spawner
}
#[derive(Clone)] pub struct TuiPlanServices { pub plan_store: Arc<dyn PlanStore>, pub goal_store: Arc<dyn GoalStore> }
impl From<&harw_runtime::PlanServices> for TuiPlanServices;
pub trait ResumeSessionSelector { /* unverändert app.rs:342-348 */ }
pub enum TuiRunOutcome { /* unverändert :328 */ }
```
`ResumableGateway` (app.rs:354-394) wandert nach runtime_root.rs, bekommt `replace(&mut self, session, store, model)`.

command_exec.rs (CE):
```rust
pub(crate) async fn execute_command_as<F>(adapters: &[CommandAdapter], sandbox: &SandboxSpec, session_id: &SessionId,
    caller_permission: PermissionTier, raw_line: &str, services: F) -> String where F: FnOnce() -> ServiceMap; // Closure erst nach erfolgreicher Admission
#[cfg(test)] pub(crate) struct CommandServices<'a>;   // :78
#[cfg(test)] pub(crate) fn build_services(..);        // :325
```
Aufrufe in app.rs (T2a):
```rust
execute_command_as(app.adapters(), app.sandbox(), app.session_id(),
    runtime_commands::caller_tier(rt.principal()), &raw, || runtime_commands::slash_service_map(rt.services())).await
let ceiling = gateway.session_mut().mode_ceiling();
crate::tools_command::dispatch_tools_command_bounded(&args, &tool_names, gateway.session_mut().activation_mut(), &ceiling)
```

### 1.3 harw-cli

```rust
// runtime_entry.rs (E1, additiv)
#[must_use] pub(crate) fn local_principal(surface: IngressSurface) -> Principal;   // Human, id "uid:<getuid>", surface Tui|Cli, Tier Operator (E4)
pub(crate) fn configured_secret_resolver(home: &Path, config: &ResolvedConfig)
    -> Result<Option<Arc<dyn harw_provider_http::SecretResolver + Send + Sync>>, String>;
pub(crate) fn doctor_assembly(home: &Path, cwd: &Path) -> Result<RuntimeAssembly, String>; // EntryKind::Doctor, ModelSource::Echo("doctor"), InMemoryStateStore, keine Jobs/Freigaben

// chat.rs (C1)
pub(crate) struct ChatStartup {
    pub(crate) mode: harw_core::InteractionMode,
    pub(crate) plan: Option<harw_runtime::PlanServices>,
    pub(crate) goal_context: Option<Arc<dyn harw_extension_api::ContextProvider>>,
}
pub fn run_chat(home_override: Option<PathBuf>, initial_prompt: Option<String>,
    resume_selection: Option<Option<String>>, startup: ChatStartup) -> Result<(), String>;
// OneShotPlanServices (chat.rs:106) entfällt.

// main.rs (M1a/M1b)
impl PlanServices { #[must_use] pub(crate) fn to_runtime(&self) -> Option<harw_runtime::PlanServices>; }
fn build_principal_registry(config: &ResolvedConfig) -> Result<PrincipalRegistry, String>; // try_insert (mcp-server transport.rs:88)
// PlanServices-Felder unverändert (web.rs:302-310, Test web.rs:691-716)

// web.rs (W1)
pub(crate) fn serve_web(home: Option<PathBuf>, socket_override: Option<PathBuf>) -> Result<(), String>;

// cli.rs (M2)
pub enum Command { /* … */ Project { #[command(subcommand)] action: ProjectAction } }
#[derive(Debug, Subcommand)]
pub enum ProjectAction {
    Trust   { #[arg(value_name = "DIR")] path: Option<PathBuf> },
    Untrust { #[arg(value_name = "DIR")] path: Option<PathBuf> },
    Status  { #[arg(value_name = "DIR")] path: Option<PathBuf> },
}
// Web: config_dir entfällt; Hilfetext „erfordert --home bzw. HARW_HOME“.

// project_trust.rs (neu, M2)
pub(crate) fn run(home_override: Option<PathBuf>, action: ProjectAction) -> Result<(), String>;
// harw_home::{trust_project, untrust_project, project_trust_status} (trust.rs:374/:402/:427); Pfad-Vorgabe = cwd

// job_worker.rs (J1)
#[derive(Clone, Debug)] pub struct JobRuntimeRoot { pub home: PathBuf, pub cwd: PathBuf }
pub struct JobWorkerContext {
    pub transcript_root: PathBuf,
    pub configured_submitters: Arc<BTreeSet<String>>,
    pub runtime_root: Option<JobRuntimeRoot>,
}
pub async fn run_job_worker_once(store: Arc<JobStore>, executions: Arc<JobExecutionRegistry>,
    provider: Arc<dyn ModelProvider>, plan_services: Option<Arc<PlanNodeServices>>, context: Arc<JobWorkerContext>) -> usize;
pub async fn run_job_worker(store: Arc<JobStore>, executions: Arc<JobExecutionRegistry>,
    provider: Arc<dyn ModelProvider>, plan_services: Option<Arc<PlanNodeServices>>,
    shutdown: watch::Receiver<bool>, context: Arc<JobWorkerContext>);

// runtime_jobs.rs (J1)
pub(crate) struct JobAssemblyInputs<'a> {
    pub(crate) entry: JobEntry, pub(crate) home: &'a Path, pub(crate) cwd: &'a Path,
    pub(crate) principal: Principal, pub(crate) session_id: SessionId,
    pub(crate) state_store: Arc<dyn StateStore>, pub(crate) job_store: Arc<JobStore>,
    pub(crate) model: Arc<dyn ModelProvider>, pub(crate) narrowing: Option<harw_runtime::RuntimeNarrowing>,
}
pub(crate) fn job_assembly(inputs: JobAssemblyInputs<'_>) -> Result<RuntimeAssembly, String>;
// submitter_is_configured (runtime_jobs.rs:188) + Test :274-283 entfernt (E9)
```

## 2. Aufgaben

### Stufe 1 (parallel)
- **R0** opus — `harw-runtime/src/{assembly,services,lib}.rs`. §1.1; Doku-Verweise auf gelöschte Montage-Zeilen (assembly.rs:5,:41,:393,:797) und spec.rs:237-240 (E6: Aufrufer löst mode/active_agent auf) — spec.rs mitgeowned nur für diese Doku. Tests: `test_plan_services_accessor_returns_builder_input`, `test_memory_accessor_none_without_memory`, `test_narrowing_readonly_on_full_entry_drops_write_tools`, `test_narrowing_rejects_full_on_notools_entry`, `test_narrowing_permissions_never_exceed_profile`.
- **E1** sonnet — `harw-cli/src/runtime_entry.rs`. `local_principal`, `configured_secret_resolver`, `doctor_assembly`; je ein Test.
- **CE** sonnet — `harw-tui/src/{command_exec,tools_command}.rs`, `session_controller.rs` (nur Doku :195-215). Closure-Parameter; `CommandServices`/`build_services` `#[cfg(test)]`; `dispatch_tools_command` entfernen (E8), Helfer behalten; T5-Doku (`/mode <gleich>` No-op). Neuer Test `test_execute_command_as_does_not_build_services_on_denied_admission`.
- **M2** sonnet — `harw-cli/src/{cli,project_trust}.rs`. §1.3. Ausgabe `trusted: <root> (digest …)`, `removed`/`not trusted`, `Trusted|Untrusted|Changed`. Tests: `test_project_trust_subcommands_parse`, `test_web_rejects_config_dir_flag`, `test_run_trust_then_status_reports_trusted`, `test_run_untrust_unknown_reports_false`.
- **W1** sonnet — `harw-cli/src/web.rs`. `_layers` raus (Signatur + Doku).
- **J1** opus — `harw-cli/src/{job_worker,runtime_jobs}.rs` (D3b). Prompt-Job: `job_principal(submitter)`, `JobEntry::Prompt`, session_id `durable-job-<id>`, TranscriptStateStore, BudgetedModelProvider, `new_root_session(id, tx, ttx, None)`, `run_turn` mit `assembly.state_store()`. Plan-Knoten: `derive_plan_node_sandbox` bleibt Prüfung; `RuntimeNarrowing{profile_for_node_kind, plan_node_identity, derived.permissions()}`; cwd = `derived.workspace().canonical_root()`; `job_principal(services.actor())`. `TurnSetup{session, state_store, pause}`. Ohne runtime_root: `Blocked{reason:"job runtime requires a HARW home"}` (E3). Montagefehler: Prompt → Failed(sanitize_failure), Knoten → fail_plan_node. Entfernen: assemble_registry-Import, empty_extension_registry, `crate::root_context` (:915), submitter_is_configured. Reihenfolge Scope → Prompt → Budget (W1-13) bleibt. Tests: alle Aufrufer mit JobWorkerContext (Temp-Home via `harw_home::ensure_home`, Temp-cwd); neu `test_prompt_job_without_runtime_root_is_blocked_before_model_call`, `test_plan_node_job_registry_is_narrowed_to_readonly_for_research`, `test_prompt_job_session_id_is_durable_job_id`.
- **L1** sonnet — `harw-cli/src/lifecycle.rs`. `runtime_composition_evidence(home)` über `doctor_assembly`; tools = `rights_snapshot().tools`; `has_trusted_spawn_context = spawn_context().approval_actor.is_some()`; `has_approval_boundary = !rights_snapshot().approval_chain.is_empty()`; `build_doctor_spawn_context`, `new_doctor_root_trace` + Imports raus. Test neu (Mengenprüfung), `doctor_context_*` löschen.

### Stufe 2 (parallel; nach Stufe 1)
- **T1** opus — `harw-tui/src/runtime_root.rs` (neu) + `lib.rs` (mod/use). `run_tui` (current_thread-Runtime; `build_root_runtime`: TuiApprovalHandler + ApprovalDriver, `new_root_session(root_id, wiring.events_tx.clone(), turn_tx, Some(handler))`, Adapter aus `assembly.operations()`, ChatApp `with_memory(..).with_runtime(..).with_session_controller(..).with_project_root(..).with_managed_spawner(..)` + plan; `set_active_mode`; Verlauf + WELCOME; ResumableGateway-Schleife; Resume Some → factory.assemble → replace → old.close_session; Quit → close_session). Tests: `test_tui_session_wiring_install_sets_controller_and_events`, `test_build_root_runtime_mounts_responder_in_chain`, `test_build_root_runtime_applies_mode_override`.
- **T2a** opus — `harw-tui/src/app.rs` nur Prod (1-3573, 3981-4373). **Tabu: 3574-3980 und 6225-6374 (W1-08).** Montage :1297-2509 löschen (außer tui_error_from_approval_driver), run_chat_tui* (:2045-2342), SessionRuntime, ResumableGateway, LOCAL_TUI_OPERATION_PERMISSION, Session-ID-Funktionen :1190-1244 (E5); TuiPlanServices reduzieren + From; ChatApp runtime_config/job_store raus, runtime rein; run_loop bounded `/tools` + Principal-Tier-Commands; ohne runtime → "Fehler: keine Runtime-Montage"; Sichtbarkeiten; Modul-Doku; Imports.
- **C1** opus — `harw-cli/src/chat.rs` (D6). Config über `harw_runtime::load_config`; spec mode_override/active_agent; privater `chat_builder`; `ChatTuiFactory: TuiAssemblyFactory`; One-shot EntryKind::OneShot mit Modus (F-154); privater `GoalContextContributor` (E11); Lösch-/Behalt-Listen laut Entwurf; Tests.
- **M1a** opus — `harw-cli/src/main.rs` Z.1-813 + Tests :1868-2160, :2629-2894; `root_context.rs` löschen (Orchestrator `git rm`). mod-Zeilen, dispatch Project/Web, run_startup_migrations, serve_mcp (try_insert, JobWorkerContext, build_plan_node_services ceiling über `root_sandbox(EntryKind::JobPlanNode, ..)`), run_local_echo über Runtime, build_local_spawn_context/new_local_root_trace raus.

### Stufe 3
- **T2b** sonnet — app.rs nur Tests :4374-6224 (approval_arming_tests tabu).
- **M1b** opus — main.rs Z.826-1861 + Tests :2160-2628. STARTUP_MODE raus, to_runtime, PlanningStartup ohne approval_policy/compose_extension_registry, cmd_analyze über EntryKind::Analyze (E7), doctor mit rights_snapshot.

### Stufe 4 (Orchestrator)
Audit-grep §3 leer, ownership-Prüfung, Review Z2d-2, Fixes, Commit.

## 3. Aufrufer zu entfernender/geänderter Items
(vollständige Liste siehe Entwurf; Kurzform)
- root_context.rs: main.rs:28,:811; chat.rs:470; lifecycle.rs:137; job_worker.rs:915; Doku harw-runtime/src/ceiling.rs:10,:43,:61
- STARTUP_MODE/startup_mode/mode_code/mode_from_code: main.rs:867,:870,:883,:920-921,:1648; Tests :2316-2323
- build_local_spawn_context: main.rs:701,:1812; Tests :1936,:1989,:2008,:2028,:2032
- serve_web: main.rs:252 · build_principal_registry: main.rs:433; Tests :2814,:2855
- run_job_worker(_once): main.rs:513; job_worker.rs:289; Tests :2242,:2267,:2302,:2338,:2371,:2533,:2827
- OneShotPlanServices: main.rs:1125,:1143; chat.rs:106,:154,:738,:784,:901,:1670,:1672 · run_chat: main.rs:231
- as_chat_runtime/compose_extension_registry/PlanningStartup: main.rs:226,:1120,:1372,:1802,:1319,:1641,:1674; Tests :2346,:2380,:2456,:2465
- chat.rs-Factories: build_tui_sandbox, build_one_shot_spawn_context, OneShotChildRegistryFactory, build_one_shot_managed_spawner, add_one_shot_model_tool_provider, one_shot_model_tool_context, build_cli_state_store, active_profile_sessions_root, build_model, selected_executable_agent, one_shot_approval_actor
- app.rs-Montage: run_chat_tui (lib.rs:37), run_chat_tui_resumable(_with_plan) (chat.rs:227), TuiPlanServices (chat.rs:220), build_session_runtime, build_tui_agent_session, trusted_tui_spawn_context, assemble_tui_registry, registry_with_approval_handler, registry_with_plan_contributions, ensure_tui_context_roots_align, TuiChildRegistryFactory, build_tui_managed_spawner, tui_child_limits, add_tui_model_tool_provider/tui_model_tool_context/TuiModelToolServices, sandbox_for_project, selected_session_id/new_session_id/session_id_for_canonical_project_root, LOCAL_TUI_OPERATION_PERMISSION, DefaultApprovalPolicy-Unit-Struct (Tests :5549,:5668)
- dispatch_tools_command: app.rs:2707; Doku tools_command.rs:227,:345
- execute_command_as/CommandServices: app.rs:114,:2729,:2735; Tests :4924,:4930,:4941,:4947
- caller_tier/slash_service_map: danach app.rs (T2a)

## 4. Entscheidungen (angenommen)
E1 neue Datei runtime_root.rs · E2 TuiAssemblyFactory für /resume · E3 Jobs ohne Home → Blocked (CHANGELOG) · E4 lokaler Principal Tier Operator · E5 Root-Session-ID = SessionId::new() · E6 mode/active_agent löst Aufrufer auf · E7 analyze: --dry-run Echo, sonst Configured über Slash-Fläche · E8 dispatch_tools_command entfernen · E9 submitter_is_configured entfernen · E10 Plan-Knoten jetzt über RuntimeNarrowing · E11 GoalContextContributor privat in chat.rs.

Tracking (Verhaltensänderungen): konfigurierte [agents]-Rollen aus TUI nicht mehr spawnbar (→ W4a); TUI-Kontextdecke LocalRoot; Startmodus auch ohne Planungsfläche; One-shot AskUser→Deny; Slash-ServiceMap reicher; Doctor-Nachweise neu; Job-Tests brauchen Temp-Home; Montage je Job; Web `--config-dir` entfällt; doppelte MCP-Principal-IDs brechen Start ab; W1-08-Bereiche null Diff.

## 5. Nachtrag R0-F (eingefroren)
`RuntimeNarrowing` erhält `pub workspace_root: Option<PathBuf>`: bindet die Root-Sandbox an genau dieses Verzeichnis, das kanonisch gleich dem erkannten Projekt-Root oder ein Nachfahre sein muss (sonst `RuntimeError::Sandbox`). Plan-Knoten übergeben den abgeleiteten Workspace-Root; `ensure_same_workspace_root` (J1-F) bleibt als zweite Prüfung.
