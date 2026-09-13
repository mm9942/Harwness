# W2d-2 / R0-F: `RuntimeNarrowing::workspace_root` — Plan-Knoten binden exakt den abgeleiteten Workspace-Root

Rolle: serieller Mehr-Crate-Agent (Opus). Owned: `harw-runtime/src/assembly.rs`, `harw-cli/src/runtime_jobs.rs`,
`harw-cli/src/job_worker.rs` (Plan-Knoten-Montagepfad + Tests), dieses Ledger. `main.rs` nicht angefasst.
Build-Policy eingehalten: nichts gebaut/geprüft/getestet; einziger Befehl
`cargo metadata --offline --no-deps --format-version 1` → Exit 0. Keine git-Schreibbefehle. Verifikation durch Lesen.

## Problem (Regression aus J1-F)

`RuntimeAssembly::build` band die Root-Sandbox an `discover_project(spec.cwd).project_root`. J1-F prüft fail-closed
Gleichheit mit dem abgeleiteten Workspace-Root → jeder Plan-Knoten, dessen Workspace unter einem Verzeichnis mit
Projektmarker liegt, endete `Failed{"plan node workspace root mismatch …"}`.

## Vertragserweiterung (eingefroren durch Brief)

```rust
// harw-runtime/src/assembly.rs:350
#[derive(Debug, Clone)]
pub struct RuntimeNarrowing {
    pub registry_profile: RegistryProfile,
    pub identity: IdentityOverrides,
    pub permissions: PermissionSet,
    pub workspace_root: Option<PathBuf>, // :360
}
```

## Bau-Semantik (assembly.rs)

| Stelle | Verhalten |
|---|---|
| `sandbox_root(project_root, narrowing) -> RuntimeResult<PathBuf>` (:~390, privat) | `None`/kein `workspace_root` → `project_root` unverändert (Verhalten aller übrigen Einstiege identisch). `Some(p)`: relativ → `RuntimeError::Sandbox` (fail-closed, keine stille Basis). Sonst `WorkspaceRegistry::build(project_root, [WorkspaceRegistration{root: p}])` — bestehende Mechanik: kanonisiert Harness-Root und `p` genau einmal, Verzeichnisprüfung, `canonical.starts_with(&harness_root)` (`Path::starts_with` = Komponentenvergleich, harw-sandbox/src/lib.rs:754-787). Fehler (`Io`, `NotDirectory`, `WorkspaceEscapesHarness`) → `RuntimeError::Sandbox` mit verlangtem Pfad, Projekt-Root und Ursache. Rückgabe `binding.canonical_root()`. Mandant/Alias der Prüfregistry: Konstanten `NARROWED_ROOT_TENANT = "narrowing"`, `NARROWED_ROOT_WORKSPACE = "workspace-root"` (nur lokal, keine Bindung nach außen). |
| `ensure_bound_to(sandbox, expected)` (privat) | Nach `root_sandbox(entry, &bound_root)` (kanonisiert erneut): Bindung ≠ `expected` → `RuntimeError::Sandbox` (Absicherung gegen Pfadtausch zwischen zwei Kanonisierungen). Nur bei gesetztem `workspace_root` aufgerufen. |
| `build` Schritt 3 (:~791-803) | `bound_root = sandbox_root(&project.project_root, narrowing)?` → `root_sandbox(spec.entry, &bound_root)?` → ggf. `ensure_bound_to` → `narrowed_sandbox(…)` (restrict + Teilmengenprüfung wie bisher). Dieselbe Sandbox steht im `SpawnContext`. |
| unverändert | `load_config`, `discover_project` (einmal), `ProjectContext` für Registry/Spawner/Contributors, Trust, Decke, Chain, Aktivierung. |

Doku ergänzt: Modulkopf Schritt 3, `RuntimeNarrowing` („vier Achsen“), `RuntimeAssemblyBuilder::narrowing`, `build` → `# Fehler`.

Neue Imports: `std::path::{Path, PathBuf}`, `harw_sandbox::{WorkspaceRegistration, WorkspaceRegistry}`,
`harw_types::{TenantId, WorkspaceId}` (alle bereits in `harw-runtime/src/sandbox.rs` genutzt; Deps vorhanden).

## Migration aller Konstruktionen (`grep 'RuntimeNarrowing\s*{'` über alle `*.rs`)

- assembly.rs R0-Tests (6×: :2114, :2153, :2195, :2218, :2240, :2261) → `workspace_root: None`.
- job_worker.rs `plan_node_narrowing` (:1022) → `workspace_root: Some(sandbox.workspace().canonical_root().to_path_buf())`.
  Accessor verifiziert: `SandboxSpec::workspace() -> &WorkspaceBinding` (harw-sandbox/src/lib.rs:874),
  `WorkspaceBinding::canonical_root() -> &Path` (:671). Die Montage-cwd ist bereits derselbe Pfad (job_worker.rs Plan-Pfad).
- runtime_jobs.rs: keine Konstruktion; nur Doku des Felds `JobAssemblyInputs::narrowing` angepasst.
- Sonst keine Konstruktionen im Workspace (harw-tui, tests/ geprüft per grep).

## job_worker.rs

- `ensure_same_workspace_root` + Aufruf in `assemble_job_turn` **behalten** (Defense in depth); Kommentare aktualisiert:
  greift jetzt nur bei echter Abweichung.
- Test umgedreht/umbenannt: `test_plan_node_under_a_marked_parent_directory_fails_without_calling_model` →
  `test_plan_node_under_a_marked_parent_directory_binds_the_workspace_root`:
  `<temp>/Cargo.toml`, geerbte Sandbox `<temp>/workspace` {R}, Research-Knoten `t-1` (Ready) →
  `completed == 1`; `RecordingModelProvider` **nicht leer**; Ergebnis ist kein `Failed/Blocked` mit Präfix
  `plan node workspace root mismatch` oder `could not assemble the job runtime`. Zusätzlich direkt:
  `plan_node_narrowing` liefert `workspace_root == derived root`; `job_assembly` mit dieser Verengung →
  `project().project_root == canonical(<temp>)` (Erkennung bleibt am Marker), `sandbox()` und
  `spawn_context().sandbox` gebunden an `derived root`, `ensure_same_workspace_root == Ok(())`.

## Neue Tests assembly.rs

- `test_narrowing_workspace_root_descendant_binds_sandbox_to_it` — cwd = `project/nested/workspace`, JobPlanNode +
  ReadOnlyExplore + {R} + `Some(workspace)`: Projekt-Root bleibt `project` (kanonisch), Sandbox und Spawn-Kontext an
  kanonischen Workspace gebunden, Rechte {R}; zweiter Fall `workspace_root == project` (Gleichheit) baut.
- `test_narrowing_workspace_root_outside_project_is_rejected` — je `RuntimeError::Sandbox` für: Elternverzeichnis,
  Geschwister, String-Präfix-Geschwister `project-evil` (Komponenten- statt String-Vergleich), `project/../sibling`,
  fehlendes Verzeichnis, relativer Pfad; unter `cfg(unix)` Symlink `project/escape-link → sibling`.

## Annahmen / offene Punkte

1. Relative `workspace_root` werden abgelehnt (Brief nennt nur „kanonisieren“; fail-closed gewählt, dokumentiert).
2. Fehlender/Nicht-Verzeichnis-`workspace_root` → `RuntimeError::Sandbox` (nicht `Discovery`).
3. `AgentIdentity` der Registry nennt weiter `project.project_root`/`project.cwd` (harw-registry-defaults/src/profile.rs:915-917,
   rein informativ im Prompt); Werkzeuge nutzen die Sandbox. Brief: Projekt-Erkennung bleibt am erkannten Projekt.
4. Plan-Knoten-Test: `on_job_completed` auf einem `Ready`-Knoten kann scheitern → Job ggf. `Failed{"job succeeded but …"}`;
   der Test lässt das zu und prüft Modellaufruf + Bindung, nicht `Succeeded`.
5. Nicht kompiliert. Risikostellen: rustfmt (lange `match`-/`assert!`-Zeilen in den neuen Tests), `TenantId::from_str`
   (inhärent, wie sandbox.rs), Tempdir 0700/benutzereigen für die Markererkennung (wie J1-F-Test).
6. Orchestrator: `cargo test -p harw-runtime assembly`, `cargo test -p harw-cli job_worker`, clippy `-D warnings`.
