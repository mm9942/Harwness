# W2d-2 / J1-F: Plan-Knoten-Sandbox darf nicht über den abgeleiteten Workspace hinaus binden

Rolle: focused-bug-fix (Opus). Owned: `harw-cli/src/job_worker.rs` (Plan-Knoten-Montagepfad + Tests), dieses Ledger.
`harw-cli/src/runtime_jobs.rs` unverändert, `main.rs` nicht angefasst. Build-Policy eingehalten: nichts gebaut/geprüft/
getestet, keine git-Schreibbefehle, keine Websuche (Brief). Verifikation durch Lesen.

## Befund (sicherheitsrelevant)

Seit J1 baut `RuntimeAssembly::build` die Plan-Knoten-Sandbox als
`root_sandbox(JobPlanNode, discover_project(cwd).project_root).restrict(derived.permissions())`
(harw-runtime/src/assembly.rs:671, :678-682). `cwd` ist `derived.workspace().canonical_root()`. Liegt ein Projektmarker
(`.git`, `Cargo.toml`, `package.json`, `pyproject.toml`; harw-project-discovery/src/discovery.rs:196-204) in einem
Elternverzeichnis, bindet `find_project_root` (discovery.rs:474) dieses Elternverzeichnis → die Sitzung (Sandbox,
SpawnContext, Registry-Tools) erreicht mehr Verzeichnisse als die vertraglich abgeleitete Sandbox: Erweiterung statt
Verengung. `restrict` schneidet nur die Rechteachse, nie den Workspace (harw-sandbox/src/lib.rs:898-904).

## Fix (fail-closed, minimal)

`harw-cli/src/job_worker.rs`:
```rust
fn ensure_same_workspace_root(assembled: &SandboxSpec, derived: &SandboxSpec) -> Result<(), String>; // :991
fn assemble_job_turn(inputs: JobAssemblyInputs<'_>, pause: PauseDisposition,
    required_sandbox: Option<&SandboxSpec>) -> Result<(TurnSetup, Arc<dyn ModelProvider>), String>; // :1083 (privat, 3. Parameter neu)
```
- `ensure_same_workspace_root` vergleicht `assembled.workspace().canonical_root()` mit
  `derived.workspace().canonical_root()` exakt; sonst
  `Err("plan node workspace root mismatch: assembly bound <a>, contract requires <b>")`.
- `assemble_job_turn` prüft direkt nach `job_assembly` und **vor** `new_root_session` (keine Sitzung, kein Modell),
  Grund durch `sanitize_failure` (erste Zeile, 160 Zeichen). Plan-Knoten-Aufruf übergibt `Some(&sandbox)` (:941-957),
  Prompt-Job `None` (:484-497, Verhalten unverändert — `JobPrompt` hat ohnehin keine Rechte).
- Fehler fließt über den bestehenden `Err`-Arm → `fail_plan_node` → `JobOutcome::Failed{reason}` + Knoten-Invalidierung.

Warum nicht `SandboxSpec::ensure_child_of` (lib.rs:950): prüft `workspace ==` auf der ganzen `WorkspaceBinding`
(Tenant + WorkspaceId + Root). Die Montage bindet unter eigenem Tenant-Alias (`tenant_name(entry)`, WorkspaceId
`PROJECT_WORKSPACE`; harw-runtime/src/sandbox.rs:90-115, :141), die geerbte Sandbox unter dem Job-Tenant — ein
Vergleich schlüge immer fehl. Beide Roots sind kanonisch aus `WorkspaceRegistry::build`/`resolve`
(harw-sandbox/src/lib.rs:759, :775), daher genügt `Path`-Gleichheit. Rechte-Teilmenge ist bereits durch R0
(`narrowed_sandbox`, assembly.rs:397-417) garantiert. Keine API-Änderung an harw-runtime/harw-sandbox, öffentliche
Signaturen (`run_job_worker(_once)`, `JobWorkerContext`, `job_assembly`, `JobAssemblyInputs`) unverändert.

## API-Nachweise

| Nutzung | Quelle |
|---|---|
| `SandboxSpec::workspace() -> &WorkspaceBinding` | harw-sandbox/src/lib.rs:874 |
| `WorkspaceBinding::canonical_root() -> &Path` | harw-sandbox/src/lib.rs:671 |
| `RuntimeAssembly::sandbox() -> &SandboxSpec` (= SpawnContext-Sandbox) | harw-runtime/src/assembly.rs:1277 |
| Montage bindet `project.project_root` | harw-runtime/src/assembly.rs:671-682 |
| Default-Marker, `$HOME`-Grenze, Eigentümer-/world-writable-Prüfung | harw-project-discovery/src/discovery.rs:196-214, :459-474 |
| `harw_plan::testing::base_node`, `PlanAction::{Create, AddNode}`, `PlanNodeStatus` | harw-plan/src/testing.rs:159; actions.rs:182-192; lib.rs:80, :88-91 |
| `JobStore::new(&Path)` | harw-session-store (job store) :147 |

## Tests (mod tests, job_worker.rs)

Neue Fixture `sandbox_bound_to(harness_root, relative, tenant, permissions)` (wie `sandbox_with`, `"."` bindet die Wurzel).
Import erweitert: `use harw_plan::{InMemoryPlanStore, PlanAction, PlanId, PlanNodeStatus};`.
- `test_ensure_same_workspace_root_rejects_parent_root` — Kind `<temp>/workspace` {R,W} vs. Eltern `<temp>` {R}
  (weniger Rechte, weiterer Root) → `Err`, Präfix + geforderter Root im Text.
- `test_ensure_same_workspace_root_accepts_identical_root` — gleicher Root, andere Tenants → `Ok(())`.
- `test_plan_node_under_a_marked_parent_directory_fails_without_calling_model` — `<temp>/Cargo.toml` (`[workspace]`,
  wie rights_matrix-Fixture), geerbte Sandbox `<temp>/workspace` {R}, Plan mit Research-Knoten `t-1` (Ready), Vertrag ohne
  `allowed_paths` → `completed == 1`, `RecordingModelProvider` leer, `Failed{reason}` mit Präfix
  `plan node workspace root mismatch`.

## Verhaltensänderung (für CHANGELOG/Tracking; ersetzt J1 Punkt 4 „offen für Review“)

Plan-Knoten laufen nur noch, wenn die Projekterkennung ab dem abgeleiteten Workspace-Root genau diesen Root liefert.
Ein Workspace, der Unterverzeichnis eines Projekts mit Marker ist (z. B. `harw serve` mit Workspace `repo/sub`, `.git` in
`repo`), lässt jeden Plan-Knoten `Failed` enden (Knoten invalidiert) statt ihn mit erweitertem Root auszuführen.
Abhilfe für Betreiber: Workspace-Bindung auf den Projekt-Root legen. Eine echte Lösung (Montage mit vorgegebenem Root
statt Erkennung) bräuchte eine harw-runtime-Option — außerhalb dieses Auftrags.

## Risiken / offen

- Nicht kompiliert. Risikostellen: rustfmt der neuen Tests; Job-Test setzt voraus, dass `/tmp`-Tempdirs 0700 und
  benutzereigen sind (tempfile-Standard) und dass die Montage mit `Cargo.toml`-Marker + ReadOnlyExplore baut
  (belegt durch R0-Test `test_narrowing_readonly_on_full_entry_drops_write_tools`).
- `PlanJobBridge::on_job_failed` auf einen `Ready`-Knoten kann scheitern; das wird nur geloggt und ändert das
  Job-Ergebnis nicht (Test prüft nur Job-Ergebnis).
- Der bestehende Test `test_plan_node_job_registry_is_narrowed_to_readonly_for_research` ruft `job_assembly` direkt
  (ohne Prüfung) und bleibt unverändert gültig.
