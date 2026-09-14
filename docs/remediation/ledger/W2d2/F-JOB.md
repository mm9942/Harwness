# W2d-2 — Fix-Agent F-JOB (Z2d-2) — Befunde R3, R7

Rolle: focused-bug-fix (Opus). BUILD-POLICY eingehalten: nur lesen, grep,
`cargo metadata --offline --no-deps --format-version 1` ausgeführt (Exit 0).
Kein `cargo build/check/test/clippy/run/add`, kein `make`/`rustc`/
`rust-analyzer`, keine git-Schreibbefehle. **Nichts kompiliert, keine Tests
ausgeführt.** Verifikation ausschließlich durch Lesen und Grep.

Owned files: `harw-cli/src/job_worker.rs`, `harw-cli/src/runtime_jobs.rs`,
`docs/remediation/ledger/W2d2/F-JOB.md` (diese Datei). Nicht angefasst:
`harw-runtime/**` (parallel F-RT), `harw-cli/src/main.rs`.

## Befund R3 — `Failed{reason}` von Job-Läufen enthielt RuntimeError-Details

`job_worker.rs::assemble_job_turn` (vormals ~:1088-1124) baute die extern
sichtbaren Fehlgründe direkt aus dem `RuntimeError`/`String`-Fehler der
Montage bzw. der Root-Session-Erzeugung:

```rust
format!("could not assemble the job runtime: {}", sanitize_failure(&error))
format!("could not create the job root session: {}", sanitize_failure(&error.to_string()))
```

`sanitize_failure` kürzt nur auf die erste Zeile / 160 Zeichen — es redigiert
nichts. Die erste Zeile eines `RuntimeError` kann Pfade und Konfigurationsdetails
tragen, z. B. `RuntimeError::Discovery` (`harw-runtime/src/assembly.rs:888-891`)
umschließt `harw_project_discovery::DiscoveryError::InvalidCwd`, dessen
`Display` den kanonisierten cwd-Pfad wörtlich nennt
(`harw-project-discovery/src/discovery.rs:88`: `` "cannot canonicalize cwd `{}`"``).

Dieser Grund landet unverändert im gespeicherten `JobOutcome::Failed{reason}`
(`harw-job-runtime/src/stored.rs:69,91-95`, `Serialize`) und geht über das MCP-
Tool `harw_job_status` an den Client zurück (`harw-mcp-server/src/transport.rs:807-812`
ruft `job_status`, das den gespeicherten `JobCompletion`/`JobOutcome` liefert).
Genau das verbietet der bestehende Kommentar bei `check_prompt_claim_scope`
(job_worker.rs :539-541): Werte, die an den Client zurückgehen, dürfen keine
internen Details tragen.

`assemble_job_turn` ist die einzige Montagestelle für **beide** Job-Familien:
der Prompt-Pfad (`execute_prompt_claim`, vormals ~:484-505) und der
Plan-Knoten-Pfad (`execute_plan_node_claim`, vormals ~:941-970, jetzt
~:960-989, ruft bei Fehler `fail_plan_node` → `JobOutcome::Failed{reason}` →
`report_plan_node_outcome`, das den Grund unverändert weiterreicht) rufen
beide dieselbe Funktion auf. Der Fix an einer Stelle deckt damit R3 für Prompt-
**und** Plan-Knoten-Jobs ab; `fail_plan_node`/`report_plan_node_outcome`
selbst mussten nicht geändert werden, sie geben nur weiter, was
`assemble_job_turn` liefert.

Der `ensure_same_workspace_root`-Zweig (J1-F, Zeilen ~1120-1123) bleibt
unverändert: sein Text ist keine `RuntimeError`-Detailzeile, sondern ein von
diesem Modul selbst formatierter Sicherheits-Befund (kanonische Workspace-
Roots aus der eigenen Vertragsableitung), den der Brief nicht zur
Externalisierung vorgesehen hat; er ist nicht Teil dieses Befundes.

### Fix

Zwei feste Konstanten (`job_worker.rs` ~:91-102):

```rust
const RUNTIME_ASSEMBLY_FAILED_REASON: &str = "could not assemble the job runtime";
const RUNTIME_SESSION_FAILED_REASON: &str = "could not create the job root session";
```

`assemble_job_turn` (~:1127-1158) gibt bei einem Montage- bzw.
Root-Session-Fehler jetzt ausschließlich diese Literale zurück; das Detail
geht nur noch strukturiert nach `tracing::error!`:

```rust
let assembly: RuntimeAssembly = job_assembly(inputs).map_err(|error| {
    tracing::error!(job_id = %session_id.as_str(), error = %error, "job runtime assembly failed");
    RUNTIME_ASSEMBLY_FAILED_REASON.to_owned()
})?;
...
let root = assembly
    .new_root_session(session_id.clone(), event_tx, turn_tx, None)
    .map_err(|error| {
        tracing::error!(job_id = %session_id.as_str(), error = %error, "job root session creation failed");
        RUNTIME_SESSION_FAILED_REASON.to_owned()
    })?;
```

`session_id.as_str()` statt `%session_id`, weil `SessionId` (Newtype-Makro,
`harw-types/src/ids.rs:25-100`) kein `Display` implementiert — nur `as_str()`.
Für den zweiten Aufruf wird `session_id.clone()` an `new_root_session`
übergeben, damit das Original für die `tracing`-Zeile im `map_err` erhalten
bleibt (kein Move-Konflikt).

Die beiden äußeren `tracing::error!`-Aufrufe an den Aufrufstellen
(`execute_prompt_claim` ~:501-505, `execute_plan_node_claim` ~:980-988) bleiben
unverändert bestehen; sie loggen jetzt nur noch den bereits sanitisierten
festen Grund zusätzlich zur `work_id`/`task_id` — redundant, aber harmlos, da
kein internes Detail mehr darin steckt. Keine API-Änderung: `assemble_job_turn`
bleibt `fn(JobAssemblyInputs<'_>, PauseDisposition, Option<&SandboxSpec>) ->
Result<(TurnSetup, Arc<dyn ModelProvider>), String>`, nur der `Err`-Inhalt
ändert sich.

### Test

`test_assemble_job_turn_prompt_assembly_failure_reason_is_fixed_and_path_free`
(`job_worker.rs`, `mod tests`): `cwd` zeigt auf ein nicht existierendes
Verzeichnis unter einem sonst gültigen `runtime_root_under`-Home →
`assemble_job_turn` mit `JobEntry::Prompt`, `narrowing: None` liefert
`Err(reason)` mit `reason == RUNTIME_ASSEMBLY_FAILED_REASON` und ohne den
cwd-Pfad (`!reason.contains(&missing_cwd.display().to_string())` und
`!reason.contains("does-not-exist")`). Provoziert denselben Fehlerpfad, den
R3 beschreibt (`DiscoveryError::InvalidCwd` → `RuntimeError::Discovery`), ohne
`harw-runtime` anzufassen.

Bestehender Test bei :2864 (`!reason.starts_with("could not assemble the job
runtime")`) bleibt unverändert gültig: der Literal-Text ist exakt derselbe wie
vorher der Präfix, nur ohne den früher angehängten Detail-Suffix.

## Befund R7 — `job_principal` machte ein Mutations-Label zur Job-Identität

`runtime_jobs.rs::job_principal` setzte für **jeden** Job (Prompt und
Plan-Knoten) `PermissionTier::Operator`. Der Plan-Knoten-Aufruf in
`job_worker.rs` (vormals ~:946, jetzt über den neuen Helfer ~:978) rief
`job_principal(services.actor())` — `services.actor()` ist laut eigenem
Doc-Kommentar von `PlanNodeServices::new` (job_worker.rs ~:165-167) *"a label,
not a trust identity — the trust identity lives in the job's
`harw_job_runtime::JobScope`"*. Der Fix schließt genau diese vom Modul selbst
dokumentierte Lücke: das Label wurde trotzdem zur `Principal::id` gemacht.

### Tier-Verifikation (`PermissionTier::Observer` ist wirkungslos auf Job-Einstiegen)

Grep-Nachweis über den Workspace:

- `harw_runtime::permissions_for_tier` (`harw-runtime/src/sandbox.rs:192`,
  `Observer => {ReadWorkspace}`, `Operator => {ReadWorkspace, WriteWorkspace}`)
  wird **nur** von `harw-cli/src/runtime_web.rs:220,272` aufgerufen (Web-
  Einstieg) und in `harw-runtime/src/sandbox.rs`-Tests selbst — kein Aufruf in
  `RuntimeAssembly::build` (`harw-runtime/src/assembly.rs`, Grep über
  `\.tier()` in dieser Datei: nur `spec.principal.clone()`/
  `spec.principal.approval_actor()`, nie `.tier()`).
- `EntryKind::JobPrompt` und `EntryKind::JobPlanNode` haben
  `operations: OperationSurface::None` (`harw-runtime/src/spec.rs:203,212`,
  belegt durch den Test `harw-runtime/src/spec.rs:534`), unabhängig vom
  gewählten `RegistryProfile`. `build_operations(OperationSurface::None, _)`
  (`harw-runtime/src/assembly.rs:1184`) liefert `OperationRegistry::new()` —
  leer. Damit hat kein Job-Einstieg eine Operations-Fläche, an der ein
  `PermissionTier` je geprüft würde.
- Rechte für Job-Einstiege kommen ausschließlich aus der Profiltabelle
  (`root_sandbox`/`plan_node_sandbox`, `runtime_jobs.rs::job_sandbox`) bzw. aus
  `RuntimeNarrowing::permissions` (R0-F) — beide unabhängig vom Principal-Tier.

Damit ist die Ledger-Behauptung „Tier auf Job-Einstiegen ohne Operations-
Fläche wirkungslos" durch Lesen bestätigt, nicht nur behauptet.

### Fix

`runtime_jobs.rs::job_principal` (~:96-108): `PermissionTier::Operator` →
`PermissionTier::Observer`; Doc-Kommentar ergänzt um die obige Begründung
(Zitat der beiden Belege: `permissions_for_tier` nur in `runtime_web.rs`,
`RuntimeAssembly::build` liest `.tier()` nirgends). Signatur unverändert
(`pub(crate) fn job_principal(submitter_id: &str) -> Principal`), betrifft
also beide Aufrufer unverändert in der Signatur.

`job_worker.rs`: neuer privater Helfer vor `execute_plan_node_claim`
(~:864-880):

```rust
fn plan_node_submitter_id(claim: &JobClaim) -> String {
    match claim.scope.submitter() {
        harw_types::ApprovalActor::Operator { id } if is_scope_identifier(id) => id.to_owned(),
        harw_types::ApprovalActor::Operator { .. }
        | harw_types::ApprovalActor::ChannelPeer { .. } => "plan-controller".to_owned(),
    }
}
```

Accessor-Nachweis: `JobClaim::scope -> JobScope` (`harw-job-runtime/src/stored.rs:56-60`),
`JobScope::submitter(&self) -> &ApprovalActor` (stored.rs:50), identisches
Muster bereits für Prompt-Jobs in `check_prompt_claim_scope` (job_worker.rs
:576-589) und in der Identitätsentpackung von `execute_prompt_claim`
(job_worker.rs :460-466, dort `id.to_owned()` — derselbe Stil übernommen).
`is_scope_identifier` (job_worker.rs :643-645, unverändert) filtert leere,
Rand-Leerzeichen- oder Steuerzeichen-ids. Fällt keine gültige `Operator`-id
an (Plan-Knoten werden intern von `PlanJobBridge::admit_ready_nodes` erzeugt,
nicht von einem authentifizierten MCP-Principal eingereicht — der Scope trägt
also praktisch nie einen validen Operator), liefert die Funktion das feste
Literal `"plan-controller"`.

Aufrufstelle in `execute_plan_node_claim` (~:978, vormals `job_principal(services.actor())`):

```rust
principal: job_principal(&plan_node_submitter_id(&claim)),
```

`services.actor()` bleibt unverändert die Aufrufkonvention für
`report_plan_node_failure`/`report_plan_node_outcome` (Plan-Mutations-Actor,
job_worker.rs :1585 ff.) — nur die Principal-Konstruktion wechselt die Quelle.
Keine Signaturänderung an `PlanNodeServices`, `JobAssemblyInputs` oder
`job_assembly`.

### Tests

- `runtime_jobs.rs`: `test_job_principal_is_channel_jobworker_operator` →
  umbenannt zu `test_job_principal_uses_observer_tier`; Assertion
  `principal.tier() == PermissionTier::Observer` (vormals `Operator`), Rest
  unverändert (`kind`, `id`, `surface`, `approval_actor() == None`).
- `runtime_jobs.rs`: bestehender Montage-Test
  `test_job_assembly_prompt_uses_given_session_id_and_no_tools` ergänzt um
  `assert_eq!(assembly.operations().iter().count(), 0);` (Accessor:
  `RuntimeAssembly::operations(&self) -> &Arc<OperationRegistry>`,
  `harw-runtime/src/assembly.rs:1445`; `OperationRegistry::iter(&self) ->
  impl Iterator<Item = &Arc<dyn Operation>>`, `harw-operations/src/registry.rs:546`).
- `job_worker.rs`: bestehender Montage-Test
  `test_plan_node_job_registry_is_narrowed_to_readonly_for_research` ergänzt
  um dieselbe Zusicherung für den Plan-Knoten-Pfad
  (`assembly.operations().iter().count() == 0`).

Kein neuer Test rekonstruiert den vollen `run_job_worker_once`-Pfad für die
Principal-Identität, weil kein bestehender Test die resultierende
`Principal::id` eines Plan-Knoten-Laufs prüft (Grep über
`\.principal\(\)|spec\(\)\.principal|principal:` in `job_worker.rs`: nur die
vier Konstruktionsstellen, keine Assertion auf das Ergebnis) — die
Verhaltensänderung ist daher ausschließlich über die Unit-Tests von
`plan_node_submitter_id`s Bausteinen (`job_principal`, `is_scope_identifier`,
beide bereits getestet) und die beiden Montage-Assertions belegt. Die
bestehenden Tests, die `job_principal("test-runtime")` direkt aufrufen
(job_worker.rs :2745, :2984 — Konstruktion außerhalb von
`execute_plan_node_claim`, zur direkten Prüfung der Montage), bleiben
unverändert gültig: sie prüfen nicht die Herkunft der id, nur dass eine
gegebene id eine baubare Montage ergibt.

## Verhaltensänderungen (für CHANGELOG/Tracking)

1. R3: `JobOutcome::Failed{reason}` einer fehlgeschlagenen Runtime-Montage
   oder Root-Session-Erzeugung ist jetzt für Prompt- **und** Plan-Knoten-Jobs
   ein fester, pfadfreier Text (`"could not assemble the job runtime"` bzw.
   `"could not create the job root session"`); das volle Detail steht nur noch
   im Server-Log (`tracing::error!`, Feld `job_id`/`error`).
2. R7: Ein Job-Principal (`IngressSurface::JobWorker`) trägt jetzt
   `PermissionTier::Observer` statt `Operator`. Ohne Wirkung auf tatsächliche
   Rechte (siehe Verifikation oben). Ein Plan-Knoten-Jobs `Principal::id` ist
   jetzt die Einreicher-id aus `JobClaim::scope.submitter()` (typischerweise
   der bei der Zulassung serverseitig aufgelöste Operator) statt des
   `PlanNodeServices`-Mutationslabels; ohne validen Operator im Scope lautet
   die id `"plan-controller"`.

## Risiken / offen

- Nicht kompiliert (Build-Policy). Risikostellen:
  - `test_assemble_job_turn_prompt_assembly_failure_reason_is_fixed_and_path_free`
    nimmt an, dass ein nicht existierendes Unterverzeichnis unter einem
    gültigen Temp-Root `discover_project` mit `DiscoveryError::InvalidCwd`
    scheitern lässt, bevor irgendein anderer Bau-Schritt greift (belegt durch
    `RuntimeAssembly::build`s Reihenfolge: `load_config` vor
    `discover_project`, assembly.rs :880-891; `load_config` mit einem per
    `harw_home::ensure_home` frisch gescaffoldeten Home sollte wie in den
    bestehenden J1-Tests erfolgreich sein).
  - `plan_node_submitter_id`s `"plan-controller"`-Zweig ist mit keinem echten
    Plan-Knoten-Testlauf abgedeckt, der tatsächlich einen `ChannelPeer`- oder
    ungültigen `Operator`-Scope durchläuft (die vorhandenen Plan-Knoten-Tests
    nutzen alle `ready_record_of_kind` mit `ApprovalActor::Operator{id:
    "operator"}`, einer gültigen id) — die Verzweigung selbst ist aber
    identisch zum bereits getesteten Muster in `check_prompt_claim_scope`.
  - Orchestrator: `cargo test -p harw-cli job_worker`, `cargo test -p harw-cli
    runtime_jobs`, `cargo fmt --check` (neue Match-Arm-Formatierung wurde von
    Hand nach rustfmt-Konvention umbrochen, nicht verifiziert), clippy
    `-D warnings`.
