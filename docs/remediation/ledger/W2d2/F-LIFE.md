# W2d-2 — Fix-Agent F-LIFE (Z2d-2)

Owned files: `harw-cli/src/lifecycle.rs`, `docs/remediation/ledger/W2d2/F-LIFE.md` (diese Datei).
BUILD-POLICY eingehalten: kein `cargo build/check/test/clippy/run/add`, kein `make/rustc/rust-analyzer`,
keine git-Schreibbefehle. Verifikation ausschließlich durch Lesen — inkl. `harw-cli/src/runtime_entry.rs`
(parallel F-MAIN/E1, nicht angefasst), `harw-install/src/doctor.rs`, `harw-runtime/src/assembly.rs`,
`harw-runtime/src/spec.rs`, `harw-runtime/src/approval.rs`, `harw-extension-api/src/approval_mode.rs`,
`harw-extension-api/src/contributors.rs`, `harw-sandbox/src/lib.rs`, `harw-core/src/session.rs`,
`harw-project-discovery/src/discovery.rs`, `harw-config/src/harness_config.rs`,
`harw-config/src/discovery.rs`, `harw-home/src/paths.rs`.

## 1. Befund C2 (major) — `harw doctor` bricht komplett ab

**Vorher**: `health_checks` rief `runtime_composition_evidence(home)?` mit `?` auf. Scheitert
`crate::runtime_entry::doctor_assembly` (Konfiguration, Vertrauen, Projekterkennung, Sandbox, Registry,
Speicher — laut dessen eigener Fehler-Dokumentation), propagiert dieser Fehler durch `health_checks` und
`health()` bis zum Aufrufer, bevor auch nur ein einziger `DoctorCheck` (System/Sandbox/ServiceManager/Home)
lief. `harw doctor` liefert dann Exit 2 ohne jede Diagnose, obwohl die anderen Checks von der Runtime-Montage
unabhängig sind.

**Fix**: `health_checks` fängt den Fehler jetzt ab, loggt ihn strukturiert und fährt mit
"nicht konfigurierter" Evidenz fort, statt abzubrechen:

```rust
let evidence = runtime_composition_evidence(home).unwrap_or_else(|error| {
    tracing::warn!(error = %error, "doctor runtime assembly failed");
    harw_install::doctor::RuntimeCompositionEvidence::not_configured()
});
```

`RuntimeCompositionEvidence::not_configured()` (`harw-install/src/doctor.rs:217`, `= Self::default()`)
liefert exakt die im Brief geforderten Feldwerte (`advertised_tools: None, has_trusted_spawn_context: None,
has_approval_boundary: None`) — verifiziert an der Struct-Definition `harw-install/src/doctor.rs:200-212`:
alle drei Felder sind `Option<_>`, kein BLOCKED nötig. `RuntimeCompositionCheck::run` (`doctor.rs:246-252`)
behandelt `advertised_tools: None` bereits als `CheckOutcome::Warn("... nicht konfiguriert ...")`, nicht als
`Fail` — der Doctor-Report zeigt also einen Warn-Eintrag für die Runtime-Komposition und alle anderen Checks
laufen normal.

Signaturen unverändert: `health(home_override: Option<PathBuf>) -> Result<(), String>`,
`health_checks(home: &Path) -> Result<Vec<Box<dyn harw_install::DoctorCheck>>, String>`,
`runtime_composition_evidence(home: &Path) -> Result<harw_install::doctor::RuntimeCompositionEvidence, String>`
— die Funktion selbst liefert weiterhin `Err` bei fehlgeschlagener Montage (Dokumentation an ihr angepasst:
sie deklariert jetzt explizit, dass die Entscheidung, was ein Fehler für den Rest von `doctor` bedeutet, beim
Aufrufer `health_checks` liegt). `tracing` ist bereits Abhängigkeit von `harw-cli`
(`harw-cli/Cargo.toml:61-62`) und wird crate-weit ohne lokalen `use tracing;`-Import aufgerufen
(`harw-cli/src/job_worker.rs:250` u. a. als Vorbild für `tracing::warn!(error = %error, "…")`).

## 2. Befund C13 (minor) — Evidenz war immer wahr

**Vorher**:
- `has_trusted_spawn_context = assembly.spawn_context().approval_actor.is_some()` — für jeden
  CLI/TUI-Principal immer `Some(ApprovalActor::Operator { .. })`
  (`harw-types/src/principal.rs:198-206`, Tabelle: `Human×Cli → "local-cli"`, `Human×Tui → "local-tui"`),
  beobachtete also nichts.
- `has_approval_boundary = !rights.approval_chain.is_empty()` — `ApprovalChain::snapshot`
  (`harw-runtime/src/approval.rs:529-543`) hängt **immer** unbedingt einen `DefaultPolicy`-Eintrag an
  (Zeile 535, kein `if`), die Kette ist also nie leer, unabhängig von jeder echten Konfiguration.

**Fix**: neue reine Funktion `evaluate_evidence` (ohne `RuntimeAssembly`-Typ, testbar ohne Montage):

```rust
fn evaluate_evidence(
    approval_chain: &[(&'static str, ApprovalHandlerKind)],
    approval_mode: ApprovalMode,
    approval_actor_present: bool,
    sandbox_bound_to_project: bool,
) -> (bool, bool) {
    let has_config_policy = approval_chain
        .iter()
        .any(|(_, kind)| *kind == ApprovalHandlerKind::ConfigPolicy);
    let has_approval_boundary = has_config_policy && approval_mode != ApprovalMode::FullAccess;
    let has_trusted_spawn_context = sandbox_bound_to_project && approval_actor_present;
    (has_trusted_spawn_context, has_approval_boundary)
}
```

aufgerufen aus `runtime_composition_evidence` mit:

```rust
let approval_actor_present = assembly.spawn_context().approval_actor.is_some();
let sandbox_root = assembly.sandbox().workspace().canonical_root();
let sandbox_bound_to_project = sandbox_root == assembly.project().project_root.as_path();
let (has_trusted_spawn_context, has_approval_boundary) = evaluate_evidence(
    &rights.approval_chain,
    assembly.approval_mode().get(),
    approval_actor_present,
    sandbox_bound_to_project,
);
```

### 2.1 `has_approval_boundary`-Kriterium

`ApprovalHandlerKind::ConfigPolicy` erscheint in `snapshot()` nur, wenn `self.config.is_some()`
(`approval.rs:532-534`), und `ApprovalChain::for_root` (`approval.rs:311-333`) setzt `config_policy` nur
`Some(..)`, wenn `[policy].require_approval_for` **nicht leer** ist (`config_tools.is_empty()` → `None`,
sonst `Some(Arc::new(ConfigApprovalPolicy::new(..)))`) — Default von `require_approval_for` ist `Vec::new()`
(`harw-config/src/harness_config.rs:155,218`). Ein Eintrag der Kategorie `ConfigPolicy` beweist also
tatsächlich eine geladene, nicht-leere Config-Politik, nicht nur "irgendein Handler ist registriert".

`ApprovalMode::FullAccess` ("kein Werkzeugaufruf fragt nach", `approval_mode.rs:57-58`) macht selbst eine
vorhandene Politik wirkungslos — `DefaultApprovalPolicy` prüft laut Modul-Doku denselben Modus, um zu
entscheiden, ob überhaupt nachgefragt wird. Deshalb: Boundary nur, wenn ConfigPolicy vorhanden **und** Modus
≠ `FullAccess`. `assembly.approval_mode()` ist `pub const fn approval_mode(&self) -> &ApprovalModeCell`
(`harw-runtime/src/assembly.rs:1489`), `.get()` liefert den aktuellen `ApprovalMode`
(`harw-extension-api/src/approval_mode.rs:183-189`).

### 2.2 `has_trusted_spawn_context`-Kriterium

`sandbox_root(project_root, narrowing)` (`harw-runtime/src/assembly.rs:488-532`) liefert ohne Narrowing
(`narrowing: None`, wie bei `doctor_assembly` — `build_assembly(spec, ModelSource::Echo(..), stores, None)`,
`harw-cli/src/runtime_entry.rs:311-317`) exakt `project_root.to_path_buf()` zurück (Zeile 492-494: früher
Return, wenn `narrowing.and_then(|n| n.workspace_root.as_deref())` `None` ist). Für `harw doctor` ist die
Bedingung `sandbox.workspace().canonical_root() == project.project_root` also strukturell erfüllt — sie ist
trotzdem keine Tautologie: bei einem künftigen Narrowing (`RuntimeNarrowing::workspace_root = Some(..)`)
kann `sandbox_root` eine engere, vom Projekt-Root verschiedene Wurzel liefern (`assembly.rs:496-531`,
`ensure_bound_to` erzwingt genau diese Bindung an anderer Stelle, `assembly.rs:534-556`), und der Check
würde dann korrekt `false` melden, falls die Bindung je auseinanderliefe. Kombiniert mit `approval_actor.
is_some()` (echter Freigabe-Akteur vorhanden) ergibt das ein Kriterium, das tatsächlich beide Fakten prüft,
statt nur den zweiten. `assembly.sandbox()` (`assembly.rs:1398`, `&SandboxSpec`, laut Doku "dieselbe [Sandbox],
die im SpawnContext steht") und `assembly.project()` (`assembly.rs:1380`, `&ProjectContext`) sind beide
`pub const fn`. `SandboxSpec::workspace()` → `&WorkspaceBinding` (`harw-sandbox/src/lib.rs:874`),
`WorkspaceBinding::canonical_root()` → `&Path` (`harw-sandbox/src/lib.rs:671`), `ProjectContext::project_root`
ist `pub project_root: PathBuf` (`harw-project-discovery/src/discovery.rs:362-374`, bereits kanonisch:
`discover_project` kanonisiert `cwd` und läuft nur über `.parent()` aufwärts, was kanonisch bleibt).

## 3. API-Belege (Datei:Zeile, ergänzend zu L1s Tabelle)

| Typ/Funktion | Fundstelle | Beleg |
|---|---|---|
| `RuntimeCompositionEvidence::not_configured() -> Self` | `harw-install/src/doctor.rs:214-219` | `= Self::default()`; alle drei Felder `Option`, `None` beim Default. |
| `RuntimeCompositionCheck::run` behandelt `advertised_tools: None` als `Warn`, nicht `Fail` | `harw-install/src/doctor.rs:246-252` | Beleg, dass der C2-Fallback den Doctor-Report nicht fälschlich rot färbt. |
| `ApprovalChain::snapshot` hängt `DefaultPolicy` unbedingt an, `ConfigPolicy` nur bei `self.config.is_some()` | `harw-runtime/src/approval.rs:529-543` | Beleg für C13: alte `!approval_chain.is_empty()` war immer `true`. |
| `ApprovalChain::for_root`: `config_policy = None`, wenn `config_tools.is_empty()` | `harw-runtime/src/approval.rs:311-333` | Beleg, dass `ConfigPolicy`-Präsenz eine echte, nicht-leere `[policy].require_approval_for` voraussetzt. |
| `PolicySection::require_approval_for` Default `Vec::new()` | `harw-config/src/harness_config.rs:155,218` | Beleg, dass ein unkonfiguriertes Temp-Home ohne `config.toml` keine `ConfigPolicy` erzeugt. |
| `ApprovalMode::FullAccess` = "kein Werkzeugaufruf fragt nach" | `harw-extension-api/src/approval_mode.rs:57-58` | Grundlage für `approval_mode != FullAccess` als zweite Bedingung. |
| `RuntimeAssembly::approval_mode(&self) -> &ApprovalModeCell` | `harw-runtime/src/assembly.rs:1489` | Zugriffspfad für den aktuellen Modus. |
| `RuntimeAssembly::sandbox(&self) -> &SandboxSpec`, `::project(&self) -> &ProjectContext` | `harw-runtime/src/assembly.rs:1380`, `:1398` | Zugriffspfade für die Bindungs-Prüfung. |
| `sandbox_root(project_root, narrowing: None) == project_root` | `harw-runtime/src/assembly.rs:488-494` | Beleg, dass die Bindungs-Bedingung für `doctor_assembly` (kein Narrowing) strukturell erfüllt, aber nicht tautologisch ist. |
| `doctor_assembly(..) = build_assembly(spec, ModelSource::Echo(..), stores, None)` | `harw-cli/src/runtime_entry.rs:311-317` | Beleg für `narrowing: None` im Doctor-Pfad (nicht verändert, nur gelesen). |
| `SandboxSpec::workspace() -> &WorkspaceBinding`, `WorkspaceBinding::canonical_root() -> &Path` | `harw-sandbox/src/lib.rs:874`, `:671` | Zugriffskette für `sandbox_bound_to_project`. |
| `ProjectContext { pub project_root: PathBuf, .. }` | `harw-project-discovery/src/discovery.rs:362-374` | Feldzugriff `assembly.project().project_root`. |
| `SpawnContext { pub approval_actor: Option<ApprovalActor>, .. }` | `harw-core/src/session.rs:230-236` | Unverändert genutzt, jetzt als eine von zwei UND-Bedingungen statt alleinig. |
| `harw_extension_api::approval_mode::ApprovalMode`, `harw_extension_api::contributors::ApprovalHandlerKind` als `pub mod` | `harw-extension-api/src/lib.rs:9,11` | Beleg, dass beide Importpfade aus `harw-cli` auflösen (Crate bereits Abhängigkeit, `harw-cli/Cargo.toml:26`), ohne neue Dependency. |
| `tracing = "0.1.44"` bereits Abhängigkeit; Verwendungsmuster `tracing::warn!(error = %error, "…")` | `harw-cli/Cargo.toml:62`; `harw-cli/src/job_worker.rs:250` | Beleg für den C2-Log-Aufruf, kein neuer Import/keine neue Dependency nötig. |

## 4. Tests

- **Geändert** (bestehender Test, jetzt echter positiver Fall statt zufälligem `true`):
  `health_evidence_observes_default_cli_composition` schreibt jetzt vor dem Aufruf ein
  `home/config.toml` mit `[policy]\nrequire_approval_for = ["shell.exec"]\n` (Muster wortgleich zu
  `harw-runtime/src/config.rs`s eigenem `trusted_home`-Testhelfer, der ebenfalls direkt `<home>/config.toml`
  beschreibt). Ohne diese Datei wäre `has_approval_boundary` unter dem neuen, strengeren Kriterium `false`
  (kein `ConfigPolicy`-Eintrag, siehe §2.1) — die Assertion `Some(true)` wäre also falsch geworden, nicht nur
  zufällig weiterhin wahr. `default_provider` bleibt bewusst ungesetzt: `doctor_assembly` nutzt
  `ModelSource::Echo` und ruft kein Modell auf; `ResolvedConfig::validate()` prüft `default_provider` nur,
  wenn `Some` (`harw-config/src/discovery.rs:76-78`) — belegt zusätzlich durch E1s eigenen
  `test_doctor_assembly_succeeds_with_empty_home`, der ganz ohne `config.toml` erfolgreich baut. Die
  Tool-Mengen- und `advertised_tools`-Assertions bleiben unverändert (Policy-Konfiguration ändert nicht die
  registrierten Tools).
- **Neu**, reine Funktion ohne `RuntimeAssembly`/Montage (Brief-Vorgabe §C13, "negativer Fall über reine
  Hilfsfunktion"):
  - `test_evaluate_evidence_both_true_when_configured_and_bound` — positiver Referenzfall.
  - `test_evaluate_evidence_no_boundary_without_config_policy_entry` — nur `DefaultPolicy` in der Kette
    (Standardfall jedes unkonfigurierten Laufs) → `has_approval_boundary == false`.
  - `test_evaluate_evidence_no_boundary_under_full_access_mode` — `ConfigPolicy` vorhanden, aber
    `ApprovalMode::FullAccess` → `false`.
  - `test_evaluate_evidence_no_boundary_under_always_ask_mode_is_still_config_gated` — Modus allein (nicht
    `FullAccess`) reicht ohne `ConfigPolicy` nicht.
  - `test_evaluate_evidence_no_trusted_context_without_approval_actor` — Sandbox gebunden, aber kein Akteur
    → `has_trusted_spawn_context == false`.
  - `test_evaluate_evidence_no_trusted_context_when_sandbox_unbound` — Akteur vorhanden, Sandbox nicht
    gebunden → `false`.
  - `test_evaluate_evidence_all_false_when_nothing_observed` — leere Kette, `FullAccess`, beide Flags `false`
    → `(false, false)`.

**Bewusst nicht geschrieben**: ein Test, der `health_checks`/`health()` tatsächlich über einen scheiternden
`doctor_assembly`-Aufruf laufen lässt (C2-Regressionstest im engeren Sinn). Ein deterministischer,
reproduzierbarer Fehlerpfad für `doctor_assembly` (Konfigurationsfehler, Vertrauensfehler,
Projekterkennungsfehler, Sandbox-, Registry- oder Speicherfehler) hängt von internem Verhalten von
`harw-project-discovery`/`harw-config`/`harw-home` ab, die nicht auf meiner Read-List standen und deren
exaktes Fehlverhalten ich ohne Bau nicht verifizieren kann (Gefahr eines nicht reproduzierbaren oder
plattformabhängigen Tests). Der Codepfad selbst ist durch Lesen eindeutig: `unwrap_or_else` fängt jeden
`Err(String)` von `runtime_composition_evidence` ab, ruft `tracing::warn!` auf und liefert
`RuntimeCompositionEvidence::not_configured()`; `health_checks` gibt danach immer `Ok(vec![...])` mit allen
sechs Checks zurück, `?` kann diesen Pfad nicht mehr verlassen. Sollte der Orchestrator einen expliziten
Regressionstest wünschen, wäre ein Ansatz: `home` auf einen Pfad zeigen lassen, der `harw_home::trust_project`
scheitern lässt (z. B. ein nicht kanonisierbarer/symlink-loop-Pfad) — das läge aber außerhalb dieses
Owned-Scopes (`harw-home`, `harw-project-discovery` nicht auf der Read-/Write-Liste) und wird hier nur als
Vorschlag vermerkt statt umgesetzt.

## 5. Bekannte Einschränkung — kein Bau ausgeführt

Wie bei L1: **nur gelesen, nicht gebaut/getestet** (BUILD-POLICY). Die Korrektheit der Pfad-Ketten
(`assembly.sandbox().workspace().canonical_root()`, `assembly.project().project_root`,
`assembly.approval_mode().get()`) sowie der `&Path`/`PathBuf`-Vergleich (`sandbox_root ==
assembly.project().project_root.as_path()`, beide Seiten explizit auf `&Path` gebracht, um mich nicht auf
implizite `PathBuf`/`&Path`-Cross-`PartialEq`-Impls verlassen zu müssen) stützen sich auf die oben zitierten
Signaturen; der genaue Laufzeitwert (`Some(true)`/`Some(true)` für den geänderten Temp-Home-Test) wird erst
durch den Orchestrator-Bau in Stufe 4 verifiziert.

## 6. Ausgabe

```json
{
  "agent": "F-LIFE",
  "symptom": "harw doctor exits 2 (C2); RuntimeCompositionEvidence booleans always true regardless of real config (C13)",
  "root_cause": "health_checks propagated any runtime_composition_evidence error via `?`, aborting every doctor check; has_trusted_spawn_context only checked approval_actor presence (always Some for CLI/TUI principals) and has_approval_boundary only checked chain non-emptiness (the built-in DefaultPolicy entry is always present), so neither flag observed a real fact.",
  "fix_summary": "health_checks now logs and falls back to RuntimeCompositionEvidence::not_configured() on assembly failure instead of propagating; evidence flags now require a configured ConfigPolicy entry plus a non-FullAccess approval mode (boundary) and a project-bound sandbox plus a present approval_actor (trusted context), via a new pure evaluate_evidence helper.",
  "files_modified": [
    {"path": "/home/mia/projects/harwness/harw-cli/src/lifecycle.rs", "lines_changed": 205}
  ],
  "files_created": [
    "/home/mia/projects/harwness/docs/remediation/ledger/W2d2/F-LIFE.md"
  ],
  "web_research": [],
  "hypotheses_tested": [
    {"hypothesis": "C2: `?` in health_checks aborts all doctor checks on assembly failure", "verdict": "confirmed", "evidence": "harw-cli/src/lifecycle.rs old line 32 `runtime_composition_evidence(home)?` inside health_checks, itself called via `?` from health()"},
    {"hypothesis": "C13: has_approval_boundary is always true because approval_chain always contains DefaultPolicy", "verdict": "confirmed", "evidence": "harw-runtime/src/approval.rs:535 pushes DEFAULT_POLICY_LABEL unconditionally in ApprovalChain::snapshot"},
    {"hypothesis": "C13: has_trusted_spawn_context is always true because CLI principals always resolve an approval_actor", "verdict": "confirmed", "evidence": "harw-types/src/principal.rs:198-206, Human×Cli/Tui always map to Some(ApprovalActor::Operator)"},
    {"hypothesis": "RuntimeCompositionEvidence fields are not Option, would require BLOCKED", "verdict": "rejected", "evidence": "harw-install/src/doctor.rs:200-212, all three fields are Option<_>"}
  ],
  "verification": {
    "command": "read-only, parent builds",
    "exit_code": null,
    "pass": null
  },
  "blocked": false
}
```
