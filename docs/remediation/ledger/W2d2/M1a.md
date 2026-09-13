# W2d-2 / M1a — main.rs Produktion Z.1-813 (Dispatch, serve_mcp, run_local_echo)

Owned: `harw-cli/src/main.rs` (Prod-Bereich bis vor „Planungsfläche — Composition-Root“, Tests im Bereich
`cli_parses_*` … `build_serve_provider_*` sowie `cli_exposes_mode_and_goal_*` … Dateiende), dieses Ledger.
Vertrag: `docs/remediation/CONTRACTS-W2d2.md` §1.3, §2 M1a, §3, E3. Build-Policy eingehalten: nichts gebaut,
keine git-Schreibbefehle. Verifikation durch Lesen.

## Änderungen

### Modulliste / Imports
- `mod root_context;` entfernt, `mod project_trust;` (nach `mod onboarding;`) ergänzt.
- Modulkopf-Doku nennt `web`, `project`.
- Imports entfernt: `harw_core::{AgentSession, EchoModelProvider, TranscriptStateStore}`,
  `harw_session_store::TranscriptStore`, `harw_types::AgentRole`.
- Imports neu: `harw_runtime::{EntryKind, ModelSource, RuntimeStores}`, `harw_types::IngressSurface`.
- Weiter importiert, weil noch von `build_local_spawn_context`/`new_local_root_trace` (s. Übergabe) bzw. dem
  M1b-Bereich genutzt: `SpawnContext`, `TraceContext`, `Uuid`, `SandboxSpec`, `PermissionSet`, `Permission`,
  `WorkspaceRegistr{y,ation}`, `TenantId`, `WorkspaceId`, `ApprovalActor`.

### dispatch
```rust
Some(Command::Web { socket }) => {
    let home = web_home(home::resolve_home(home_override))?;
    web::serve_web(Some(home), socket)
}
Some(Command::Project { action }) => project_trust::run(home_override, action),
```
Neu privat: `fn web_home(resolved: Result<PathBuf, String>) -> Result<PathBuf, String>` — Fehler
`"harw web requires a HARW home (--home or HARW_HOME): {error}"`. Nimmt das Auflösungsergebnis entgegen, damit der
Fehlerpfad ohne `set_var` testbar ist.

### run_startup_migrations
- `Some(Command::Web { .. })`: eigener Arm auf Home-Layer (`web_home(resolve_home)` → `ensure_home` → `config_layers`).
- `Doctor { config_dir } | Serve { config_dir }`: unverändert (explizites `--config-dir` oder Home-Layer).
- `Command::Project { .. }` im Arm ohne Migration (`return Ok(())`).
- `resolve_layers`/`resolve_serve_paths` unverändert (nur noch Doctor/Serve).

### serve_mcp
- `let principals = build_principal_registry(&config)?;`
- `fn build_principal_registry(config: &ResolvedConfig) -> Result<PrincipalRegistry, String>` über
  `PrincipalRegistry::try_insert` (harw-mcp-server/src/transport.rs:88); Duplikat ⇒
  `Err("MCP principal id '<id>' is configured more than once")` (Display von `DuplicatePrincipalId`), Start bricht ab.
- `JobWorkerContext` exakt nach J1-Ledger, vor `runtime.block_on` gebaut:
  `transcript_root` (move), `configured_submitters` (move), `runtime_root: match home.as_deref() { Some(h) =>
  Some(JobRuntimeRoot{ home: h.to_path_buf(), cwd: current_dir()? }), None => None }` (eigenes `current_dir`).
- Aufruf `run_job_worker(worker_store, worker_executions, worker_provider, worker_plan_services, shutdown_rx,
  worker_context)`; `worker_transcript_root`/`worker_submitters` entfallen.
- `build_plan_node_services` (liegt bei ~:1665, M1b-Bereich) **nicht angefasst** — s. Übergabe.

### run_local_echo (über Runtime)
`profile_sessions_root(home)` → `transcript_state_store(&sessions_root, run_thread_for_session)` →
`runtime_spec(EntryKind::LocalEcho, home, &cwd, local_principal(IngressSurface::Cli))` →
`build_assembly(spec, ModelSource::Echo(format!("echo: {input}")), RuntimeStores{state_store, None, None}, None)`
(LocalEcho: `SpawnerPolicy::None`, kein Session-Events-Kanal nötig) → in `block_on`:
`new_root_session(assembly.root_session_id().clone(), event_tx, turn_tx, None)` (AskResolution::Fail ⇒ kein
Responder) → `run_turn(&mut session, assembly.model().as_ref(), assembly.state_store().as_ref(), TurnInput::user(input))`
→ `close_session(&root_session_id)` → Ergebnisprüfung/letzte Assistant-Nachricht wie bisher. Kein unwrap/expect.
`assemble_default_registry` und der handgebaute SpawnContext entfallen dort.

### Tests
- Unverändert: `local_run_executes_a_completed_core_turn`, `local_run_persists_transcript_under_isolated_home`
  (Echo-Text und eine `.jsonl` unter `<profil>/sessions` bleiben gleich).
- Gelöscht (testeten nur `build_local_spawn_context`): `local_spawn_context_binds_registered_tools_to_discovered_root`,
  `local_spawn_context_uses_fixed_operator_approval_and_root_role`,
  `local_spawn_context_carries_a_freshly_generated_root_trace`, `local_spawn_context_root_traces_differ_across_two_calls`.
- Auf `Result` umgestellt: `build_principal_registry_maps_capabilities_and_tenant_scope`,
  `build_principal_registry_grants_submit_own_only_when_configured` (`.expect(..)`).
- Neu: `test_build_principal_registry_rejects_duplicate_id` (zwei Einträge `id = "twice"` ⇒ Err enthält `'twice'`
  und `more than once`), `test_dispatch_web_without_home_names_home_flag` (über `web_home`: Err nennt `--home`,
  `HARW_HOME` und Ursache; Ok-Durchreichung), `test_run_startup_migrations_project_skips_home_resolution`
  (`harw project status` legt kein Home an).

## Übergaben an M1b (exakt)

1. **`build_local_spawn_context` (~:835) und `new_local_root_trace` (~:822) NICHT gelöscht**, weil
   `cmd_analyze` (~:1876, M1b) noch `build_local_spawn_context(&project_root)?.sandbox` nutzt. M1b muss beim Umbau
   von `cmd_analyze` auf `EntryKind::Analyze` (E7) beide Funktionen samt Doku löschen. **Bis dahin kompiliert
   main.rs nicht**: `build_local_spawn_context` referenziert `root_context::local_root_context_ceiling()` (~:875),
   `mod root_context;` ist gemäß Spec bereits entfernt (und root_context.rs wird vom Orchestrator gelöscht).
2. Danach ungenutzte Imports entfernen (prüfen gegen M1b-Code): `SpawnContext`, `TraceContext` (`harw_observe`),
   `Uuid`, `Permission`, `PermissionSet`, `SandboxSpec`, `WorkspaceRegistration`, `WorkspaceRegistry` — letztere vier
   nutzt heute noch `build_plan_node_services`.
3. **`build_plan_node_services` (~:1665)**: Ceiling über `harw_runtime::root_sandbox(EntryKind::JobPlanNode,
   project_root)` statt `WorkspaceRegistry`/`SandboxSpec::from_resolved` (Spec §2 M1a, liegt im M1b-Bereich).
   `EntryKind` ist bereits importiert. Aufrufer in `serve_mcp` (`build_plan_node_services(home_path, &plan_config,
   &project_root)?`) unverändert.
4. `run_chat`-Aufruf im `None`-Arm von `dispatch` (C1-Signatur mit `ChatStartup`) sowie `as_chat_runtime` —
   nicht angefasst (laut Brief M1b).

## Verhaltensänderungen
- `harw web` löst nur noch Home auf; `--config-dir` existiert nicht (M2). Fehlender Home ⇒ Fehler nennt `--home`.
- Doppelte MCP-Principal-IDs brechen `harw serve` ab (vorher: ID fail-closed deaktiviert, Start lief weiter).
- `harw run`: Principal `uid:<getuid>` (Cli, Operator) statt Approval-Actor `local-cli`; Sandbox/Decke/Trace aus
  der Runtime (`root_sandbox(LocalEcho)`, `CeilingPolicy::LocalRoot`); Config-/Trust-Fehler lassen `run` scheitern.
- `harw project …` ohne Startup-Migration.

## Belege
- `PrincipalRegistry::try_insert(&mut self, String, McpPrincipal) -> Result<(), DuplicatePrincipalId>`, Display
  „MCP principal id '{}' is configured more than once“ — harw-mcp-server/src/transport.rs:70-100.
- `runtime_entry::{runtime_spec, profile_sessions_root, transcript_state_store, build_assembly, local_principal}` —
  harw-cli/src/runtime_entry.rs:68/:107/:135/:176/:221.
- `RuntimeStores{state_store, job_store, approval_store}` assembly.rs:188; `new_root_session` :1461;
  `root_session_id()` :1410; `model()` :1374; `state_store()` :1380; `close_session` :1546; `RootSession.session` :210.
- `ModelSource::Echo(String)` model.rs:61; `EntryKind::LocalEcho` Profil spec.rs:155-162 (`ask: Fail`, `spawner: None`).
- `run_turn(&mut AgentSession, &dyn ModelProvider, &dyn StateStore, TurnInput)` harw-core/src/turn_loop.rs:835.
- `JobRuntimeRoot` job_worker.rs:102, `JobWorkerContext` :118, `run_job_worker` :328.
- `serve_web(Option<PathBuf>, Option<PathBuf>)` web.rs (W1); `project_trust::run` (M2);
  `home::resolve_home` harw-cli/src/home.rs:14 (außerhalb Read-list gelesen, nur Signatur/Fehlerverhalten für `web_home`).

## Risiken
- Nicht gebaut. rustfmt könnte `web_home` auf eine Zeile ziehen (reine Formatierung).
- `run_local_echo`-Tests hängen am Test-cwd (`harw-cli/`): Projekterkennung/Trust über die Runtime statt
  `assemble_default_registry` — gleiche Annahme wie `runtime_entry::test_build_assembly_local_echo_succeeds`.
