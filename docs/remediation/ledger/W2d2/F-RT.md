# W2d-2 / F-RT: Projektkontext-Abfluss in Modellkontext (Review Z2d2 R1, R2, R8) + Doku `config_policy_tools`

Rolle: focused-bug-fix (Opus), hohe Sicherheitsstufe. Owned/geändert: `harw-runtime/src/spec.rs`,
`harw-runtime/src/assembly.rs`, dieses Ledger (neu). `harw-runtime/tests/rights_matrix.rs` **unverändert**
(konstruiert/listet keine `EntryProfile`-Felder; einzige Konstruktionen von `EntryProfile { .. }` im Workspace liegen in
spec.rs, per grep geprüft). harw-cli/**, harw-tui/**, harw-registry-defaults/** nicht angefasst.
Build-Policy eingehalten: nichts gebaut/geprüft/getestet/formatiert; einziger Befehl
`cargo metadata --offline --no-deps --format-version 1` → Exit 0. Keine git-Schreibbefehle.

## Vertragsnachtrag (eingefroren durch Brief)

```rust
// harw-runtime/src/spec.rs — EntryProfile (Clone, Debug, PartialEq, Eq)
pub struct EntryProfile {
    pub permissions: PermissionSet,
    pub registry_profile: RegistryProfile,
    pub operations: OperationSurface,
    pub ask: AskResolution,
    pub spawner: SpawnerPolicy,
    pub ceiling: CeilingPolicy,
    pub project_context: bool, // neu
}
```

| EntryKind | project_context |
|---|---|
| Tui, OneShot, LocalEcho, Analyze, Doctor, JobPlanNode | `true` |
| Web, McpServe, JobPrompt, GatewayTelegram, GatewayDream | `false` |

Invariante (Test): jeder Einstieg mit `CeilingPolicy::Closed` hat `project_context == false`.

## Befund / Ursache

`assemble_registry_for_project` (harw-registry-defaults/src/profile.rs:892-936) registriert unabhängig vom Profil
`BaselineInstructionsProvider(AgentIdentity{cwd = project.cwd, project_root = project.project_root})` und
`ProjectContextProvider(project.clone())`. Letzterer liefert `project.root` = `project_root=<pfad>\ncwd=<pfad>` und je
`DiscoveredDoc` ein Fragment `project.doc:<filename>` mit Inhalt (harw-project-discovery/src/provider.rs:78-100).
`RuntimeAssembly::build` reichte den erkannten `ProjectContext` ungefiltert durch → JobPrompt/McpServe erhielten
HARW.md/AGENTS.md/CLAUDE.md und Host-Pfade im Modellkontext.

API-Nachweis für den Fix ohne harw-registry-defaults-Änderung: `ProjectContext { pub cwd, pub project_root, pub docs }`
(harw-project-discovery/src/discovery.rs:362-375, kein `non_exhaustive`), `DiscoveredDoc { pub path, pub filename, pub content }`
(:340-349). Beide Provider lesen ausschließlich diese Felder → eine Kopie genügt; Platzhalter möglich, kein BLOCKED.

## Fix (assembly.rs)

| Stelle | Verhalten |
|---|---|
| `const PROJECT_CONTEXT_PLACEHOLDER: &str = "<workspace>"` (privat) | neutraler Pfad-Platzhalter |
| `fn registry_project_context<'a>(project: &'a ProjectContext, profile: &EntryProfile, narrowed_root: Option<&Path>) -> Cow<'a, ProjectContext>` (privat) | `!profile.project_context` → `Owned{cwd = project_root = "<workspace>", docs: Vec::new()}` (gewinnt auch gegen gebundenen Root). `narrowed_root = Some(root)` → `Owned{project_root = root, cwd = project.cwd falls starts_with(root) sonst root, docs = nur doc.path.starts_with(root)}` (`Path::starts_with` = Komponentenvergleich). Sonst `Borrowed(project)`. |
| `fn ensure_narrowing_fits_operations(entry, &EntryProfile, Option<&RuntimeNarrowing>) -> RuntimeResult<()>` (privat, R8) | `Some(n)` mit `n.registry_profile != RegistryProfile::Full` **und** `profile.operations == OperationSurface::AllWithModelTools` → `RuntimeError::Registry`; sonst `Ok(())`. |
| `build` Schritt 0 | direkt nach `narrowed_registry_profile`, vor `load_config`: `ensure_narrowing_fits_operations(spec.entry, &profile, narrowing.as_ref())?` |
| `build` Schritt 7 | `narrowed_root = narrowing.workspace_root.is_some() → Some(bound_root)` (bereits kanonisch aus `sandbox_root`); `assemble_registry_for_project(registry_profile, &registry_project_context(&project, &profile, narrowed_root), …)` (Temporary, endet mit dem Statement). |
| unverändert | `RuntimeAssembly::project()`, Spawner (`SpawnerInputs.project`), Contributors (`AssemblyInputs.project`) sehen den **erkannten** Kontext; Sandbox-Bindung, Trust, Config. |

Doku: Modulkopf Schritt 7, `build` → `# Fehler` (R8-Fall ergänzt).

## Nebenauftrag (Koordinator): Doku `RightsSnapshot::config_policy_tools`

spec.rs: „Werkzeuge, die die Konfigurationspolitik ohne Rückfrage erlaubt" war falsch herum. Neu: „Werkzeuge, für die
die Konfigurationspolitik eine Freigabe verlangt (`[policy].require_approval_for`, sortiert und dublettenfrei; siehe
`crate::approval::ApprovalChain::config_policy_tools`)". Nachweis: `ApprovalChain::config_policy_tools` → „Die Werkzeuge
aus `[policy].require_approval_for`" (harw-runtime/src/approval.rs:567-575), Befüllung `rights_snapshot`
(assembly.rs `config_policy_tools: self.chain.config_policy_tools()`). Nur Doku, kein Verhalten.

## Tests

spec.rs:
- `profile_matches_contract_table`: `Row` um `bool` erweitert, Erwartung je Zeile gemäß Tabelle oben.
- neu `test_profile_project_context_only_for_local_project_entries` — exakte Menge + Closed-Decke ⇒ `false`.

assembly.rs (Helfer `ready` = Waker::noop-Poll wie approval.rs; `registry_model_context` nimmt die Registry aus
`assembly.registry` und sammelt Systemprompt/Instruktionsfragmente + Kontextfragmente `label\ncontent`):
- `test_job_prompt_assembly_has_no_project_docs` — `project/AGENTS.md` mit Marker; für JobPrompt, McpServe,
  GatewayTelegram, GatewayDream, Web: Erkennung findet den Marker (`project().docs`), Modellkontext enthält weder Marker,
  noch `project.doc:`, noch Projektpfad (kanonisch/unkanonisch); `project.root` = `project_root=<workspace>\ncwd=<workspace>`.
- `test_tui_assembly_keeps_project_docs` — Tui (mit `session_events`): Fragment `project.doc:AGENTS.md` mit Marker und
  `project.root` mit kanonischem Projekt-Root.
- `test_narrowing_workspace_root_filters_docs_above_root` — Montage JobPlanNode + ReadOnlyExplore + {R} +
  `workspace_root = project/nested/workspace`: `project().docs.len() == 2`, Kontext ohne oberes, mit innerem AGENTS.md,
  `project_root`/`cwd` = kanonischer Workspace. Reine Entscheidung: `/work/space-evil/…` wird bei Root `/work/space`
  verworfen (Komponentenvergleich); `None` → `Cow::Borrowed`; JobPrompt-Profil schwärzt auch mit Root.
- `test_narrowing_rejects_restricted_profile_with_model_tool_operations` — Doctor + ReadOnlyExplore/NoTools →
  `RuntimeError::Registry`; Doctor + Full und LocalEcho + ReadOnlyExplore bauen; Helper-Matrix Tui/OneShot/Doctor.

Bestehende Tests geprüft: keine bisherige Verengung trifft R8 (R0/R0-F-Tests nutzen LocalEcho/JobPrompt/Web/McpServe/
JobPlanNode oder Doctor mit `Full`); einziger Prod-Aufrufer von `.narrowing` ist harw-cli/src/runtime_jobs.rs:213
(JobPlanNode, `OperationSurface::None`). Keine Assertion in harw-cli/harw-tui/harw-runtime/tests auf `project_root=`,
`project.doc:` oder „Project root" (grep).

## Web-Recherche

- https://doc.rust-lang.org/std/path/index.html — `Path::starts_with` vergleicht Komponenten, löst aber weder `..` noch
  Symlinks auf → Filter nur gegen kanonische Pfade (`bound_root` aus `WorkspaceRegistry`, Doc-Pfade aus kanonischem cwd).
- https://doc.rust-lang.org/std/fs/fn.canonicalize.html — Kanonisierung löst Symlinks; `discover_project` kanonisiert cwd,
  Doc-Kandidaten mit Symlink werden übersprungen (discovery.rs:385-396).

## Annahmen / offene Punkte

1. `Web` verliert Projekt-Doku und Host-Pfade im Systemprompt (Vertragswert `false`). Verhaltensänderung für CHANGELOG.
2. Contributors (`AssemblyInputs.project`) und Spawner erhalten weiter den erkannten Kontext. Ein künftiger Contributor an
   Web/Gateway/MCP, der Kontext-Provider registriert, müsste `inputs.profile.project_context` selbst beachten.
3. Andere Prompt-Quellen außerhalb der Wurzel-Registry (z. B. `harw-tui/src/runtime_root.rs:509`
   `with_project_root(assembly.project()…)`, nur Tui = `true`) nicht Gegenstand.
4. R8 greift vor `load_config` (wie Whitelist) und nur bei gesetzter Verengung.
5. Nicht kompiliert/formatiert. Risikostellen: rustfmt-Umbrüche in neuen Tests; `Waker::noop` (stabil seit 1.85 = MSRV,
   bereits in approval.rs genutzt); Tui-Test setzt voraus, dass die Montage wie in rights_matrix.rs ohne Tokio-Runtime baut.
6. Orchestrator: `cargo test -p harw-runtime` (spec, assembly), `cargo test -p harw-cli job_worker runtime_jobs`,
   clippy `-D warnings`, `cargo fmt --check`.
