# W2d-2 / M1b — main.rs Rest (Planungs-Startup, analyze, doctor)

Owned: `harw-cli/src/main.rs` (Prod ab „Generates a fresh root trace“ bis vor das Testmodul, Tests
`plan_services_over_temp_home` … `analyze_rejects_contradictory_direction_and_scope_flags`, `None`-Arm von
`dispatch` laut M1a-Übergabe 4), dieses Ledger. Vertrag: `CONTRACTS-W2d2.md` §1.3, §2 M1b, §3, E6/E7;
M1a-Übergaben 1–4; C1 §6. Build-Policy eingehalten: nichts gebaut/geprüft/getestet, keine git-Schreibbefehle.
Verifikation durch Lesen und grep.

## 1. Signaturen (wie geschrieben)

```rust
impl PlanServices { #[must_use] pub(crate) fn to_runtime(&self) -> Option<harw_runtime::PlanServices>; }
struct PlanningStartup { mode: InteractionMode, plan_config: PlanToolConfig,
    goal_context: Option<Arc<dyn harw_extension_api::ContextProvider>>, services: PlanServices }
fn local_runtime_spec(home_override: Option<PathBuf>, entry: EntryKind, surface: IngressSurface) -> Result<RuntimeSpec, String>;
fn prepare_planning_startup(spec: &RuntimeSpec, requested_mode: Option<&str>, requested_goal: Option<&str>) -> Result<PlanningStartup, String>;
fn build_plan_node_services(home: &Path, config: &PlanToolConfig, project_root: &Path) -> Result<Option<Arc<job_worker::PlanNodeServices>>, String>; // unverändert
fn analyze_plan_surface_disabled() -> String;
fn cmd_analyze(home_override: Option<PathBuf>, requested_mode: Option<&str>, requested_goal: Option<&str>, args: &AnalyzeArgs) -> Result<(), String>; // unverändert
fn doctor(layers: Vec<PathBuf>, home: Option<&Path>) -> Result<(), String>;   // + home
fn print_runtime_rights(home: &Path);
#[cfg(test)] pub(crate) fn build_operation_registry(config: &PlanToolConfig) -> (harw_operations::registry::OperationRegistry, usize);
fn resolve_startup_mode(requested: Option<&str>, configured: &str) -> Result<InteractionMode, String>; // unverändert
```

`prepare_planning_startup` nimmt statt `home_override` eine `&RuntimeSpec`, weil `harw_runtime::load_config`
eine Spec verlangt (home + cwd → Layer inkl. Repo-Trust) und `cmd_analyze` dieselbe Spec anschließend für die
Montage weiterverwendet. Die Spec baut `local_runtime_spec` (resolve_home, current_dir, `local_principal`).

## 2. Änderungen

### M1a-Übergaben
1. `new_local_root_trace` und `build_local_spawn_context` samt Doku gelöscht (nach Umbau von `cmd_analyze`).
2. Imports entfernt (je per grep auf 0 Treffer geprüft): `harw_core::{ConfigApprovalPolicy, SpawnContext}`,
   `harw_extension_api::ExtensionRegistry`, `harw_extension_api::registry::ContextProviderRegistrationError`,
   `harw_observe::TraceContext`, `harw_operations::registry::OperationRegistry`, `harw_operations::OpContext`,
   `harw_sandbox::{Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry}` (ganze Zeile),
   `uuid::Uuid`, `std::sync::atomic::AtomicU8`.
   Neu: `harw_extension_api::ContextProvider`, `harw_runtime::{RuntimeSpec, ServiceSurface}`.
   Behalten (Prod-Nutzung geprüft): `TenantId`, `WorkspaceId`, `ApprovalActor` (build_principal_registry),
   `TurnId` (cmd_analyze), `OpInput`, `ServiceMap`, `GoalContextProvider`, `InteractionMode`.
   Skript-Prüfung: jedes verbleibende `use`-Symbol hat ≥ 1 Treffer im Prod-Teil (vor `mod tests`).
3. `build_plan_node_services`: Ceiling = `harw_runtime::root_sandbox(EntryKind::JobPlanNode, project_root)
   .map_err(|error| error.to_string())?` (Profil JobPlanNode = `{ReadWorkspace, WriteWorkspace}`, spec.rs; also
   gleiche Rechte wie vorher). Doku angepasst. Die bisher fälschlich über `build_plan_node_services` stehende Doku
   von `prepare_planning_startup` (zusammengeklebter Doc-Block) steht wieder an ihrer Funktion.
4. `None`-Arm von `dispatch`:
   ```rust
   let (entry, surface) = if cli.chat.prompt.is_some() { (EntryKind::OneShot, IngressSurface::Cli) }
                          else { (EntryKind::Tui, IngressSurface::Tui) };
   let spec = local_runtime_spec(home_override.clone(), entry, surface)?;
   let startup = prepare_planning_startup(&spec, requested_mode.as_deref(), requested_goal.as_deref())?;
   let chat_startup = chat::ChatStartup { mode: startup.mode, plan: startup.services.to_runtime(), goal_context: startup.goal_context };
   chat::run_chat(home_override, cli.chat.prompt, cli.chat.resume, chat_startup)
   ```
   Feldpfade `cli.chat.prompt: Option<String>`, `cli.chat.resume: Option<Option<String>>` (cli.rs:47/:50);
   `ChatStartup`/`run_chat` wortgleich chat.rs:107/:145.

### Globaler Startmodus
`STARTUP_MODE`, `mode_code`, `mode_from_code`, `startup_mode` gelöscht. `resolve_startup_mode` unverändert;
der Modus lebt nur noch in `PlanningStartup.mode`.

### PlanServices
`as_chat_runtime` gelöscht, `to_runtime` neu: `Some(harw_runtime::PlanServices{ plan: Arc::clone(self.plan.as_ref()?),
goal: …, findings: …, plan_config: self.config.clone() })`. Feldnamen belegt harw-runtime/src/services.rs:194-203
(`plan`, `goal`, `findings`, `plan_config`). Felder von `crate::PlanServices` unverändert; Doku von `findings`
verweist nicht mehr auf `chat::OneShotPlanServices`.

### PlanningStartup / prepare_planning_startup
`approval_policy` und `impl PlanningStartup { compose_extension_registry }` entfernt (die Montage baut
`ApprovalChain` aus `[policy]` selbst, C1 §6). `goal_context` jetzt `Option<Arc<dyn ContextProvider>>`
(explizite Bindung für die Unsized-Coercion). Ablauf: `ensure_home(&spec.home)` → `harw_runtime::load_config(spec)`
(config.rs:90, liefert `(ResolvedConfig, ConfigTrustReport)`) → `resolve_startup_mode` → `plan_tool_config_from_section`
→ `build_plan_services(&spec.home, …)` → GoalContextProvider → `--goal` seeden.

### cmd_analyze (E7)
`analyze_tokens` → `local_runtime_spec(.., EntryKind::Analyze, IngressSurface::Cli)` → `prepare_planning_startup` →
`!plan_config.is_enabled()` bzw. `to_runtime() == None` ⇒ bisherige `[tools.plan]`-Meldung
(`analyze_plan_surface_disabled`) → `spec.mode_override = Some(startup.mode)` →
Modell: `--dry-run` ⇒ `ModelSource::Echo("harw analyze --dry-run")`, kein Resolver; sonst `load_config(&spec)` +
`runtime_entry::configured_secret_resolver(&spec.home, &config)?` und `ModelSource::Configured` →
`RuntimeAssembly::builder(spec).model(..).stores(RuntimeStores{ InMemoryStateStore, None, None })
.plan_services(plan).session_events(event_tx)` (+ `.secret_resolver` falls Some) → `build()` →
`assembly.operations().find_by_command("/analyze")` (registry.rs:515; fehlt ⇒ `[tools.plan]`-Meldung) →
`assembly.op_context(ServiceSurface::Slash, assembly.root_session_id().clone(), TurnId::new(), assembly.sandbox().clone())`
(assembly.rs:1426, Signatur verifiziert) → current_thread-Runtime → `operation.run(&ctx, OpInput::command("/analyze", tokens))`.
Sender/Empfänger: `SpawnerPolicy::BuiltinRoles` (spec.rs Analyze) verlangt `session_events` (assembly.rs:1111);
`_event_rx` bleibt bis Funktionsende gebunden. Keine Wurzelsitzung: die Operation braucht nur den `OpContext`;
`new_root_session` wird nicht aufgerufen (der Spawner registriert `root_session_id` beim Bau).
`build_operation_registry` hat keinen Prod-Aufrufer mehr ⇒ `#[cfg(test)]`, voll qualifizierter Registry-Typ,
ohne `tracing`-Zeile.

### doctor
Config-Zusammenfassung unverändert. Neu `home: Option<&Path>`; bei `Some` druckt `print_runtime_rights`:
`runtime_entry=<EntryKind:?>`, `runtime_permissions=…`, `runtime_tools=<n>`, `runtime_approval_chain=<label:Kind>, …`,
`runtime_untrusted_repo=<pfad|none>` (Felder `RightsSnapshot` spec.rs:249-273). Scheitert cwd/`doctor_assembly`:
eine Zeile `runtime_warning=runtime assembly failed: <fehler>`, Rückgabe bleibt `Ok` ⇒ Exit-Code wie heute.
**Aufrufstelle im M1a-Bereich minimal angepasst** (nötig für Spec-Punkt 6):
`let layers = resolve_layers(..)?; let home = home::resolve_home(home_override.clone()).ok(); doctor(layers, home.as_deref())?;`

## 3. Tests

Gelöscht (testeten gelöschte Items; Vertrag §3 :2309/:2334/:2375/:2442 → heutige Zeilen):
- `startup_mode_round_trips_every_interaction_mode` (war :2264; STARTUP_MODE/mode_code/mode_from_code/startup_mode)
- `chat_runtime_carries_goal_context_policy_and_mode_not_only_stores` (war :2289; as_chat_runtime)
- `chat_runtime_is_absent_when_the_plan_surface_is_disabled` (war :2330; as_chat_runtime)
- `composed_registry_appends_plan_contributions_without_replacing_defaults` (war :2397; compose_extension_registry/approval_policy)

Neu:
- `test_plan_services_to_runtime_requires_all_three_stores` — leer ⇒ None; Plan+Goal ohne Findings ⇒ None;
  Temp-Home mit `enabled_defaults` ⇒ Some, `Arc::ptr_eq` für alle drei Stores, Konfiguration reist mit.
- `test_prepare_planning_startup_resolves_mode_without_global_state` — Temp-Home + Temp-cwd, Spec über
  `runtime_entry::runtime_spec(EntryKind::Tui, …)`; `explore` ⇒ Explore, danach `work` ⇒ Work (erster Wert bleibt
  Explore); `to_runtime().is_some() == plan_config.is_enabled()`; unbekannter Modus ⇒ Err nennt den Namen.
  Unabhängig vom Home-Template (kein Assert auf `[mode] default`).

Unverändert: Plan-Surface-Tests (nutzen `#[cfg(test)] build_operation_registry`), Modus-Tests, Startziel-Tests,
`plan_section_*`, `config_node_kind_*`, `analyze_*`.

## 4. Audit-grep (harw-cli, nach Änderung)

| Muster | Treffer außerhalb `root_context.rs` | in `root_context.rs` |
|---|---|---|
| `root_context` | 0 | 11 (Datei selbst; Orchestrator `git rm`) |
| `STARTUP_MODE` | 0 | 0 |
| `build_local_spawn_context` | 0 | 1 (Moduldoku der zu löschenden Datei) |
| `as_chat_runtime` | 0 | 0 |
| `OneShotPlanServices` | 0 | 0 |
| `compose_extension_registry` | 0 | 0 |

Zusätzlich in main.rs 0 Treffer: `ConfigApprovalPolicy`, `approval_policy`, `Uuid`, `TraceContext`, `SpawnContext`,
`WorkspaceRegistr`, `SandboxSpec`, `PermissionSet`, `AtomicU8`, `mode_code`, `mode_from_code`, `startup_mode()`.

## 5. Offene Punkte für den Orchestrator

1. **dead_code-Risiko `PlanServices.services`**: Einziger Prod-Leser war `cmd_analyze`
   (`startup.services.services`). Jetzt lesen das Feld nur noch Tests (main.rs) — web.rs destrukturiert mit `..`.
   Unter `-D warnings` droht „field `services` is never read“. Vertrag verlangt Felder unverändert, und web.rs
   (W1) konstruiert `crate::PlanServices { services: ServiceMap::new(), … }` in `test_runtime_plan_services_requires_all_three_stores`
   (web.rs:689-714). Lösungsvorschlag: Feld + `register_plan_services`-Aufruf entfernen, web.rs-Test und
   main.rs-Tests `disabled_plan_surface_*`/`enabled_plan_surface_*` (prüfen `services.services.get::<…>()`)
   anpassen — zwei Owner, daher nicht eigenmächtig umgesetzt.
2. `harw analyze` ohne `--dry-run` braucht jetzt eine vollständige Provider-Einrichtung (E7); vorher lief die
   Operation ohne Provider bis `NotAvailable`. Config wird dort zweimal gelesen (prepare + Resolver), wie chat.rs.
3. `harw analyze` reicht den Ziel-Kontext nicht in die Montage (GoalContextContributor ist privat in chat.rs, E11);
   `/analyze` läuft ohne Modell-Turn der Wurzel, nur Logfeld `goal_context`.
4. `harw doctor --config-dir X` montiert zusätzlich gegen das auflösbare Home (nicht gegen X); Montagefehler nur Warnzeile.
5. `prepare_planning_startup` baut für den Chat-Pfad eine Spec (Tui/OneShot) nur zum Config-Laden; `run_chat` baut
   seine eigene.

## 6. Ausgabe

```json
{"agent":"M1b","files_created":["/home/mia/projects/harwness/docs/remediation/ledger/W2d2/M1b.md"],
 "files_modified":["/home/mia/projects/harwness/harw-cli/src/main.rs"],
 "verification":{"command":"read-only + grep, parent builds","exit_code":null,"pass":null},
 "stubbed_imports":[],"blocked":false}
```
