# W2d-2 — Agent T2a — `harw-tui/src/app.rs` Produktionscode (D5)

Owned: `harw-tui/src/app.rs` (nur Prod), dieses Ledger.
BUILD-POLICY eingehalten: kein `cargo build/check/test/clippy/run`, kein `make`/`rustc`/`rust-analyzer`, keine
git-Schreibbefehle. Ausgeführt: `cargo metadata --offline --no-deps --format-version 1` (Exit 0). Verifikation durch Lesen.

## 0. Bereichsnachweis (Tabu-Zonen null Diff)

Vorab verifiziert (Original): W1-08 Z.3574–3980 (`APPROVAL_ARMING_DELAY` :3583), Testmodul `mod tests` Z.4374–6224,
`mod approval_arming_tests` Z.6225–6374. Vor der Änderung SHA-256 der drei Blöcke gesichert, danach Blöcke im neuen Stand
per Inhaltsanker gesucht und verglichen — **alle drei byte-identisch**:

| Block | alt | neu | SHA-256 |
|---|---|---|---|
| W1-08 | 3574–3980 | 2314–2720 | `d1115522…ab6c9eb` identisch |
| `mod tests` | 4374–6224 | 3114–4964 | `788e891c…f2b855d` identisch |
| `approval_arming_tests` | 6225–6374 | 4965–5114 | `89b5101c…666f521d` identisch |

Datei: 6374 → 5114 Zeilen (inkl. Nachtrag §6).

## 1. Gelöschte Items

Session-ID (E5, alt :1190-1244): `session_id_for_canonical_project_root`, `new_session_id`, `selected_session_id`.

Montage (alt :1297-2509, ohne `tui_error_from_approval_driver`):
`sandbox_for_project`, `new_tui_root_trace`, `local_tui_root_context_ceiling` (enthielt ein Prod-`expect`),
`trusted_tui_spawn_context`, `assemble_tui_registry`, `as_dyn_approval_handler` (kein Prod-Rest-Aufrufer → gelöscht,
T1 übernimmt), `registry_with_approval_handler`, `registry_with_plan_contributions`, `ensure_tui_context_roots_align`,
`struct TuiChildRegistryFactory` + `impl`/`impl ChildRegistryFactory`, `configured_child_organizational_role`,
`tui_child_limits`, `build_tui_managed_spawner`, `struct TuiModelToolServices`, `tui_model_tool_context`,
`add_tui_model_tool_provider`, `pub fn run_chat_tui`, `pub fn run_chat_tui_resumable`,
`pub fn run_chat_tui_resumable_with_plan`, `struct SessionRuntime`, `build_session_runtime`, `build_tui_agent_session`.

Weitere: `struct ResumableGateway` + `impl` + `impl ChatGateway` (alt :350-394; nur von `run_chat_tui*` benutzt → gelöscht,
T1 legt ihn in runtime_root.rs neu an), `const LOCAL_TUI_OPERATION_PERMISSION` (alt :140-143).

`TuiPlanServices`: Felder `context_providers`, `approval_handlers`, `initial_mode` entfernt.

`ChatApp`: Felder `runtime_config`, `job_store`; Methoden `pub fn with_runtime_config`, `pub(crate) fn runtime_config`,
`pub fn with_job_store`, `pub(crate) fn job_store`; zusätzlich `pub(crate) fn session_controller(&self)` — einziger
Aufrufer war der entfernte `CommandServices`-Aufbau in `run_loop`, auch in Tests kein Aufruf (grep crate-weit) → wäre
`dead_code` unter `-D warnings`. Feld `session_controller` bleibt (von `apply_pending_controller_state` benutzt).

Imports entfernt (je per Wort-grep im Prod-Rest ohne Kommentare = 0 Treffer geprüft):
`BTreeMap`, `harw_agent_dsl::{ExecutableAgentIr, roles::AgentRoleId}`, aus `harw_core`: `ChildLimits`,
`ChildRegistryFactory`, `ModelProvider`, `SessionManager`, `SpawnContext`, `StateStore`; `harw_extension_api::{AgentSpawnError,
ApprovalHandler, ExtensionRegistry, SpawnInput}`, `harw_observe::TraceContext`, `ModelToolProvider`, `OperationRegistry`,
`harw_operations::{OpContext, PermissionTier, ServiceMap, SharedSessionController}`, `assemble_default_registry`,
`IdentityOverrides`, `profile_for_role`, `role_names`, `WorkspaceRegistration`, `WorkspaceRegistry`, `AgentRole`,
`ApprovalActor`, `TenantId`, `WorkspaceId`, `uuid::Uuid`, `TuiApprovalHandler`, `CommandServices`, `harw_event_channel`,
`frame_channel`, `crate::gateway::ChatGateway` (alle Nutzungen voll qualifiziert als `dyn crate::gateway::ChatGateway`;
Trait-Methoden auf `dyn Trait` brauchen keinen Import), `spawn_input_reader`.
Neu: `use crate::runtime_commands;`. Doc-Links auf entfernte Importnamen auf volle Pfade umgestellt
(`harw_core::ModelProvider`, `harw_operations::ServiceMap`).

## 2. Neue / geänderte Signaturen (exakt, Stand neu)

```rust
pub(crate) const WELCOME: &str;                                                        // :156
#[derive(Clone)] pub struct TuiPlanServices { pub plan_store: Arc<dyn PlanStore>, pub goal_store: Arc<dyn GoalStore> } // :306
impl From<&harw_runtime::PlanServices> for TuiPlanServices;  // :322, plan_store = Arc::clone(&services.plan), goal_store = Arc::clone(&services.goal)
// Feldnamen verifiziert: harw-runtime/src/services.rs:194-203 { plan, goal, findings, plan_config }
impl ChatApp {
    #[must_use] pub(crate) fn with_runtime(mut self, assembly: Arc<harw_runtime::RuntimeAssembly>) -> Self; // :704
    #[must_use] pub(crate) fn runtime(&self) -> Option<&Arc<harw_runtime::RuntimeAssembly>>;                // :715
    pub(crate) fn push_lines(&mut self, lines: Vec<Line<'static>>);                                          // :989
    // unverändert: pub fn new, pub fn with_memory, pub(crate) fn with_session_controller (:682),
    // pub(crate) fn with_managed_spawner (:734), pub fn with_project_root, pub fn with_plan_services,
    // pub(crate) fn set_active_mode, pub(crate) fn managed_spawner
}
pub(crate) struct TerminalGuard { terminal: Terminal<CrosstermBackend<Stdout>> }         // :1082
impl TerminalGuard { pub(crate) fn enter() -> Result<Self, TuiError>; }                 // :1099
pub(crate) fn install_loaded_history(session: &mut AgentSession, app: &mut ChatApp, history: ConversationHistory); // :1201
pub(crate) fn tui_error_from_approval_driver(error: ApprovalDriverError) -> TuiError;   // :1224
#[allow(clippy::too_many_arguments)]                                                     // :1310 (bestehend)
pub(crate) async fn run_loop(
    guard: &mut TerminalGuard,
    app: &mut ChatApp,
    gateway: &mut dyn crate::gateway::ChatGateway,
    event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<SessionEvent>,
    turn_event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<TurnEvent>,
    tui_rx: &mut tokio::sync::mpsc::UnboundedReceiver<TuiEvent>,
    harw_tx: &HarwEventSender,
    harw_rx: &mut tokio::sync::mpsc::UnboundedReceiver<HarwEvent>,
    frame_req: &FrameRequester,
    approval_driver: &ApprovalDriver,
    approvals: &mut ApprovalPromptReceiver,
) -> Result<TuiRunOutcome, TuiError>;                                                    // :1311, Parameter unverändert
pub(crate) async fn frame_scheduler(
    mut frame_rx: tokio::sync::mpsc::UnboundedReceiver<()>,
    tui_tx: tokio::sync::mpsc::UnboundedSender<TuiEvent>,
);                                                                                        // :1837
pub trait ResumeSessionSelector { /* unverändert */ }                                    // :359
pub enum TuiRunOutcome { /* unverändert */ }                                             // :345
```

`TuiError` unverändert (inkl. Variante `ContextProviderRegistration` und `From<ContextProviderRegistrationError>`; nur Doku
angepasst, die auf gelöschte Funktionen verwies).

### run_loop — Verhaltensänderungen (§1.2)

- `/tools` (:1437-1447): `let ceiling = gateway.session_mut().mode_ceiling();` (owned `SessionActivation`,
  `harw-core/src/session.rs:784`; die temporäre `&mut`-Leihe endet mit dem Statement), danach
  `crate::tools_command::dispatch_tools_command_bounded(&args, &tool_names, gateway.session_mut().activation_mut(), &ceiling)`
  (Signatur `tools_command.rs:235`). Rückgabe `ToolsCommandOutcome` wie bisher → `into_lines()` unverändert.
  Hinweis F-T.md:94: Bestätigungstexte für `reset`/`profile` des bounded-Pfads weichen vom alten unbeschränkten Pfad ab (beabsichtigt).
- Command-Pfad (:1464-1483): `match app.runtime()` →
  `Some(rt)`: `execute_command_as(app.adapters(), app.sandbox(), app.session_id(), runtime_commands::caller_tier(rt.principal()), &raw, || runtime_commands::slash_service_map(rt.services())).await`;
  `None`: `tracing::error!("tui.command.no_runtime_assembly")` und Ausgabe `"Fehler: keine Runtime-Montage"`.
  Typen geprüft: `caller_tier` liefert `harw_types::PermissionTier`; `harw_operations::PermissionTier` ist Re-Export
  desselben Typs (`harw-operations/src/operation.rs:110`). `RuntimeAssembly::principal` (:1269) / `services` (:1334) existieren.
  Nur geteilte Leihen auf `app` bis nach `.await`; `app.push_lines` danach.

Modul-Doku (`//!`) neu: keine Montage mehr in app.rs, Slash-/`/tools`-Pfad, crate-interne Schnittstelle zu `runtime_root`,
Beispiel ohne `run_chat_tui`. Docs in `ChatApp` (Felder `memory`, `session_controller`, `new`, `with_memory`,
`with_project_root`) und `LineAction`/`run_loop` auf neuen Stand; Links `execute_command` → `execute_command_as`.

## 3. Schnittstellenbedarf / Hinweise an T1 und Orchestrator

- **Kein** Import von `crate::runtime_root::ResumableGateway` nötig: `run_loop` nimmt `&mut dyn crate::gateway::ChatGateway`.
  T1 muss `ResumableGateway` (inkl. `impl crate::gateway::ChatGateway`) vollständig selbst anlegen; der alte Code liegt nicht mehr in app.rs.
- `as_dyn_approval_handler` existiert nicht mehr in app.rs (T1 legt eigene Fassung an).
- T1 muss in runtime_root.rs selbst importieren: `crate::events::harw_event_channel`, `crate::frame_requester::frame_channel`,
  `crate::input_reader::spawn_input_reader`, `crate::approval::TuiApprovalHandler`.
- `lib.rs:37` `pub use app::{ChatApp, TuiError, run_chat_tui};` bricht (T1 owned lib.rs, §1.2).
- `harw-cli/src/chat.rs` nutzt `run_chat_tui_resumable(_with_plan)` und die entfernten `TuiPlanServices`-Felder (C1).
- Bis T1 landet sind folgende crate-internen Items ohne Aufrufer (dead_code): `WELCOME`, `TerminalGuard::enter`,
  `frame_scheduler`, `run_loop` (transitiv `handle_key`, `draw_viewport`, `resume_request`, …), `install_loaded_history`,
  `ChatApp::with_runtime`, `with_session_controller`, `with_managed_spawner`. Erwartet, Build erst nach Stufe 2.
- `Cargo.toml` von harw-tui: `harw-config`/`harw-session-store`/`uuid`/`harw-registry-defaults`/`harw-agent-dsl`/`harw-observe`
  werden in app.rs-Prod nicht mehr benutzt; ob sie crate-weit (Tests, andere Module) noch nötig sind, nicht geprüft (nicht owned).

## 4. Tests in `mod tests`, die gelöschte/entfernte Items referenzieren (Input T2b)

Zeilen = neuer Stand. Items in Klammern; „Import" = über `use super::*` nicht mehr sichtbarer Name.

Hilfsfunktionen:
- `test_executable_agent_ir` (:3221) — Import `ExecutableAgentIr`
- `test_operations` (:3325) — Import `OperationRegistry`
- `test_managed_spawner` (:3331) — `build_tui_managed_spawner`, `trusted_tui_spawn_context`, `runtime_config`; Import `ModelProvider`, `StateStore`
- `approval_test_session` (:4280) — `as_dyn_approval_handler`, `registry_with_approval_handler`, `trusted_tui_spawn_context`, `DefaultApprovalPolicy` (Unit-Struct, §3); Import `AgentRole`, `ExtensionRegistry`, `TuiApprovalHandler`
- `drive_turn_answering` (:4308) — Import `TuiApprovalHandler`
- `test_child_registry_factory` (:4578) — `TuiChildRegistryFactory`, `assemble_tui_registry`; Import `ModelProvider`
- `test_spawn_input` (:4596) — Import `SpawnInput`
- `registered_tool_names` (:4607) — Import `ExtensionRegistry`

Tests:
- `selected_executable_policy_limits_tui_session_tools_and_retains_snapshot` (:3262) — `build_tui_agent_session`; Import `ExtensionRegistry`
- `absent_executable_policy_preserves_full_tui_session_visibility` (:3293) — `build_tui_agent_session`; Import `ExtensionRegistry`
- `model_operation_provider_is_appended_to_tui_registry` (:3372) — `add_tui_model_tool_provider`, `TuiModelToolServices`
- `model_tool_context_binds_execution_sandbox_and_trusted_services` (:3406) — `tui_model_tool_context`, `TuiModelToolServices`; Import `OperationRegistry`, `SharedSessionController`, `StateStore`
- `model_tool_context_omits_spawner_service_when_no_spawner_is_configured` (:3457) — `tui_model_tool_context`, `TuiModelToolServices`
- `model_tool_provider_registers_without_a_managed_spawner` (:3496) — `add_tui_model_tool_provider`, `TuiModelToolServices`
- `managed_spawner_admits_configured_worker_from_trusted_external_root` (:3532) — über `test_managed_spawner`; Import `SpawnInput`
- `managed_spawner_refuses_an_unconfigured_child_role_set` (:3568) — `build_tui_managed_spawner`, `trusted_tui_spawn_context`
- `trusted_spawn_context_preserves_runtime_sandbox_authority` (:3600) — `trusted_tui_spawn_context`; Import `AgentRoleId`, `ApprovalActor`
- `trusted_spawn_context_carries_a_freshly_generated_root_trace` (:3620) — `trusted_tui_spawn_context`
- `trusted_spawn_context_root_traces_differ_across_two_calls` (:3641) — `trusted_tui_spawn_context`
- `local_tui_permissions_are_accessible_but_maintainer_commands_are_blocked` (:3655) — `LOCAL_TUI_OPERATION_PERMISSION`, `CommandServices` (jetzt `#[cfg(test)]` in command_exec, aber nicht mehr importiert) als Struct statt Closure
- `test_handle_key_enter_submits_line_and_closes_popup` (:3843), `test_handle_key_tab_accepts_popup_selection_without_submit` (:3884), `test_handle_key_types_digits_in_argument_not_swallowed` (:3966) — Import `harw_event_channel`
- `session_id_mapper_is_stable_and_opaque` (:3989), `session_id_mapper_separates_project_roots` (:4013) — `session_id_for_canonical_project_root`
- `explicit_session_id_overrides_project_derived_id` (:4021) — `selected_session_id`
- `ordinary_starts_create_distinct_sessions_for_the_same_project` (:4033) — `selected_session_id`, `session_id_for_canonical_project_root`
- `durable_history_hydrates_core_and_redacts_non_text_visible_content` (:4062) — Import `AgentRole`, `ExtensionRegistry` (`install_loaded_history` selbst existiert weiter)
- `tui_registry_discovery_failure_is_typed_and_fail_closed` (:4152) — `assemble_tui_registry`
- `tui_context_root_alignment_accepts_the_shared_project_root` (:4180) — `ensure_tui_context_roots_align`; Import `ModelProvider` (laut §3 auch `build_session_runtime`-Pfad)
- `tui_approval_handler_is_appended_behind_the_existing_policy` (:4406) — `registry_with_approval_handler`, `as_dyn_approval_handler`, `DefaultApprovalPolicy`
- `appending_the_approval_handler_preserves_every_other_contribution` (:4431) — `registry_with_approval_handler`, `as_dyn_approval_handler`
- `answering_a_prompt_updates_the_same_cell` (:4480), `rejecting_a_prompt_marks_the_same_cell_as_denied` (:4517) — Import `TuiApprovalHandler`
- `approval_driver_errors_keep_their_cause` (:~4544) — über `test_child_registry_factory`/`assemble_tui_registry` (Zuordnung heuristisch; `tui_error_from_approval_driver` bleibt)
- `explorer_child_registry_has_no_write_and_no_shell_tool` (:4619), `child_registry_profiles_come_from_the_shared_role_mapping` (:4647) — `TuiChildRegistryFactory` (über Helfer); Import `role_names`
- `child_limits_are_derived_monotonically_from_the_runtime_profile` (:4684) — `tui_child_limits`; Import `ChildLimits`
- `mode_request_is_applied_at_the_turn_boundary` (:4711), `unknown_mode_names_never_change_the_session` (:4761) — `build_tui_agent_session`; Import `ExtensionRegistry`
- `three_child_events_produce_a_single_sub_agent_cell` (:4799) — Import `role_names`
- `tui_context_root_alignment_rejects_mismatched_prompt_and_tool_roots` (:4942) — `ensure_tui_context_roots_align`

Nicht betroffen: `mod approval_arming_tests` (Tabu, nutzt nur W1-08-Items). Keine Tests referenzieren `ChatApp::session_controller()`,
`with_runtime_config`, `with_job_store`, `TuiPlanServices`-Literale oder `ChatGateway`-Methoden auf konkreten Typen (grep).
Die Liste wurde per Skript (Wortgrenzen, Kommentare/`use`-Zeilen ausgenommen, Grenzen = `fn` auf 4er-Einrückung) erzeugt;
einzelne Zuordnungen an Funktionsgrenzen können um eine Funktion verrutschen.

## 6. Nachtrag (Koordinator-Notiz T1): `as_dyn_approval_handler`

T1 hat `as_dyn_approval_handler` als `pub(crate)` nach `runtime_root.rs:431` verlegt. In app.rs ruft ihn kein Prod-Code mehr
auf; unqualifiziert aufgerufen wird er nur noch im Testmodul `mod tests` (neu :4291, :4412, :4422, :4440 — nicht in
`approval_arming_tests`). Ergänzt im Importblock (außerhalb aller Tabu-Zonen, app.rs:145-146):

```rust
#[cfg(test)]
use crate::runtime_root::as_dyn_approval_handler;
```

Über `use super::*` im Testmodul sichtbar. Damit entfällt `as_dyn_approval_handler` in der T2b-Liste (§4) als Fehlerursache;
die umgebenden Aufrufe von `registry_with_approval_handler`/`DefaultApprovalPolicy` bleiben T2b-Input. Tabu-Hashes nach dem
Nachtrag erneut geprüft: identisch. Zeilenangaben in §0/§2/§4 sind auf den Stand nach dem Nachtrag (+4 ab Z.146) korrigiert.
`ResumableGateway` (T1: `replace(&mut self, session, store, model)`) wird von app.rs nicht referenziert.

## 5. Standards

Kein neues `unwrap`/`expect` (ein Prod-`expect` mit `local_tui_root_context_ceiling` entfallen); kein neues `#[allow]`
(bestehende `#[allow(clippy::too_many_arguments)]` an `run_loop`/`run_turn_streaming`/`drive_turn_animated`/W1-08 unverändert,
`#[allow(dead_code)]` an `project_root` bestehend); keine Tests geschrieben (T2b); `tracing` mit Event-Namen.

```json
{"agent":"T2a","files_modified":["/home/mia/projects/harwness/harw-tui/src/app.rs"],
 "files_created":["/home/mia/projects/harwness/docs/remediation/ledger/W2d2/T2a.md"],
 "verification":{"command":"cargo metadata --offline --no-deps --format-version 1","exit_code":0,"pass":true},
 "tabu_regions_identical":true,"blocked":false}
```
