# W2d-2 — Fix-Agent Z2d-2 / F-MAIN (Review Z2d2-CLI: C1, C3, C4, C5, C6)

Owned files:
- `harw-cli/src/main.rs`
- `harw-cli/src/web.rs`
- `docs/remediation/ledger/W2d2/F-MAIN.md` (diese Datei, neu)

Nicht angefasst (parallele Fix-Agents): `lifecycle.rs`, `chat.rs`,
`job_worker.rs`, `runtime_jobs.rs`, `runtime_entry.rs`, `harw-runtime/**`.

BUILD-POLICY eingehalten: kein `cargo build/check/test/clippy/run/add`, kein
`make`/`rustc`/`rust-analyzer`, keine git-Schreibbefehle. Ausgeführt:
`cargo metadata --offline --no-deps --format-version 1` → Exit 0 (Workspace
weiterhin auflösbar, keine `Cargo.toml`-Änderung nötig). Verifikation
ausschließlich durch Lesen und grep, inklusive Klammern-/Parenthesen-
Zählcheck über beide Dateien nach jeder Änderungsrunde
(`main.rs`: 389/389 `{}`, 1714/1714 `()`; `web.rs`: 63/63 `{}`, 407/407 `()`).

## C1 (blocker) — `PlanServices.services` (dead_code unter `-D warnings`)

**Befund**: `PlanServices.services: ServiceMap` (main.rs) hatte im
Produktionsbau keinen Leser mehr — `cmd_analyze` nutzte
`startup.services.to_runtime()`, nicht `startup.services.services`. Nur Tests
(main.rs, web.rs) lasen bzw. konstruierten das Feld. Unter `-D warnings` wäre
das `field 'services' is never read`.

**Fix**:
- Feld `services: ServiceMap` aus `struct PlanServices` entfernt (main.rs,
  `PlanServices`-Definition).
- `register_plan_services(..)`-Aufruf und `let mut services = ServiceMap::new();`
  aus `build_plan_services` entfernt; beide `Ok(PlanServices { .. })`-Zweige
  (disabled/enabled) bauen das Ergebnis jetzt ohne `services`-Feld.
- Imports bereinigt: `use harw_operations::{OpInput, ServiceMap};` →
  `use harw_operations::OpInput;` (ServiceMap sonst nirgends in main.rs
  gebraucht — grep bestätigt 0 Resttreffer). `use harw_plan_bridge::{…,
  register_plan_services};` → `register_plan_services` aus der Liste entfernt
  (Funktion selbst bleibt in `harw-plan-bridge`, nur hier ungenutzt).
- Doku angepasst: `plan_tool_config_from_section`-Doc (verwies auf
  `[ServiceMap]`/`[register_plan_services]`), `PlanServices`-Struct-Doc,
  `PlanServices::to_runtime`-Doc, `build_plan_services`-Doc,
  `PlanningStartup.services`-Feld-Doc, Abschnitts-Kommentar vor
  `DEFAULT_PLAN_SPACE` — alle Erwähnungen von `ServiceMap`/
  `register_plan_services` in main.rs durch Beschreibungen der drei
  Plan-/Goal-/Finding-Stores direkt ersetzt (grep danach: 0 Treffer
  `ServiceMap`/`register_plan_services` in main.rs).
- Tests main.rs:
  - `disabled_plan_surface_registers_nothing_and_creates_no_directories`:
    `services.services.get::<..>().is_none()` (vier Zeilen) ersetzt durch
    `services.plan/goal/findings.is_none()`, `!services.config.is_enabled()`,
    `services.to_runtime().is_none()`.
  - `enabled_plan_surface_registers_six_operations_and_four_services`:
    entsprechend auf `services.plan/goal/findings.is_some()` und
    `services.to_runtime().is_some()` umgestellt.
  - `test_plan_services_to_runtime_requires_all_three_stores`: beide
    `services: ServiceMap::new(),`-Zeilen in den `PlanServices`-Literalen
    (disabled/partial) entfernt.
- web.rs:
  - `serve_web`: `runtime_plan_services(crate::build_plan_services(..)?)` →
    `crate::build_plan_services(..)?.to_runtime()` (gleiche Semantik: `Some`
    nur bei allen drei Stores).
  - freie Funktion `runtime_plan_services` (Doku + Körper) vollständig
    entfernt — sie destrukturierte nur `crate::PlanServices` und baute
    `harw_runtime::PlanServices`, was `PlanServices::to_runtime` bereits tut.
  - Test `test_runtime_plan_services_requires_all_three_stores` auf
    `to_runtime()` umgestellt (Funktionsname jetzt
    `test_plan_services_to_runtime_requires_all_three_stores`, drei
    `services: ServiceMap::new(),`-Zeilen entfernt, Aufrufe
    `runtime_plan_services(x)` → `x.to_runtime()`). `ServiceMap`-Import in
    web.rs bleibt (anderweitig genutzt: `web_op_context`, `registry_map`,
    Test-Fixtures für den generischen `OpContext`-Service-Map-Typ — nicht
    verwandt mit `PlanServices`).

**Verifikation**: grep über beide Dateien auf `ServiceMap`/
`register_plan_services`/`runtime_plan_services` zeigt in main.rs 0 Treffer,
in web.rs nur noch die unveränderten, unabhängigen `ServiceMap`-Stellen (Zeile
67, 93, 115 Moduldoc; 309/338/349/413/418/479/498 — `OpContext`-Service-Map)
plus einen Kommentar, der die entfernte Funktion erwähnt (kein Code). Alle
`PlanServices { .. }`-Konstruktionsstellen (main.rs 2×, web.rs 3×) wurden
angepasst; keine Konstruktion außerhalb dieser beiden Dateien (grep
`crate::PlanServices` über das gesamte Repo).

## C3 (major, Testlücke) — `cmd_analyze` ohne isolierte Montage-Testbarkeit

**Befund**: Der Montagebau (Builder, Stores, Sitzungs-Ereigniskanal) lag
inline in `cmd_analyze` zwischen CLI-Parsing und `println!`/Operationsaufruf
— nicht isoliert testbar ohne die komplette Kommandozeilen- und
Ausführungs-Pipeline mitzubauen.

**Fix**: Montagebau nach `fn analyze_assembly(spec: RuntimeSpec, plan:
harw_runtime::PlanServices, model: ModelSource, resolver: Option<Arc<dyn
harw_provider_http::SecretResolver + Send + Sync>>) -> Result<
(harw_runtime::RuntimeAssembly, tokio::sync::mpsc::UnboundedReceiver<
harw_protocol::SessionEvent>), String>` herausgelöst (Typen anhand
`RuntimeAssemblyBuilder::{builder, model, stores, plan_services,
session_events, secret_resolver, build}`, `harw-runtime/src/assembly.rs:694-
758,1444`, verifiziert — Körper 1:1 aus `cmd_analyze` übernommen, keine
Verhaltensänderung). `cmd_analyze` ruft jetzt
`analyze_assembly(spec, plan, model, secret_resolver)?` und bindet den
zurückgegebenen Empfänger als `_event_rx` bis Funktionsende (unverändertes
Verhalten: Kanal bleibt offen, bis die Operation gelaufen ist).
Neuer Import: `use harw_protocol::SessionEvent;` (bereits Workspace-Abhängigkeit,
identisch zu `chat.rs`/`runtime_entry.rs`/`job_worker.rs`).

**Tests** (main.rs, neu):
- `test_analyze_assembly_registers_analyze_operation_when_plan_enabled`:
  Temp-Home + `ensure_home`, `config.toml` mit `[tools.plan]\nenabled = true\n`
  (TOML-Schlüssel verifiziert gegen `harw-config/src/harness_config.rs:418-
  441` und `harw-config/src/discovery.rs:474` — Layer lesen `<layer>/
  config.toml`, `home` ist selbst der erste Layer). Baut `RuntimeSpec` über
  `runtime_entry::runtime_spec(EntryKind::Analyze, home, cwd,
  runtime_entry::local_principal(IngressSurface::Cli))`, ruft
  `prepare_planning_startup` (bestätigt `plan_config.is_enabled()`), dann
  `analyze_assembly(spec, startup.services.to_runtime().unwrap(),
  ModelSource::Echo("test"), None)` und prüft
  `assembly.operations().find_by_command("/analyze").is_some()`. Kein
  Operationsaufruf (`operation.run`) — nur Registrierung, damit der Test
  keine echte Analyse gegen das Test-Arbeitsverzeichnis auslöst.
- `test_cmd_analyze_disabled_plan_surface_names_config_key`: Temp-Home ohne
  `config.toml` (Default `[tools.plan] enabled = false`, siehe
  `harw-config/src/harness_config.rs:400-410`), `AnalyzeArgs{ dry_run: true,
  .. }`, ruft `cmd_analyze` über die volle Signatur und prüft
  `error == analyze_plan_surface_disabled()`.

Beide Tests referenzieren nur Temp-Verzeichnisse (`tempfile::tempdir`), kein
`set_var`/`set_current_dir`, keine Netz-/Prozessabhängigkeit.

## C4 (minor, Sicherheit) — keine Berechtigungsprüfung vor `operation.run`

**Befund**: `cmd_analyze` fand `/analyze` über `find_by_command` und rief
`operation.run(..)` ohne zu prüfen, ob der montierte Principal die von der
Operation verlangte Mindeststufe erreicht.

**Beleg für das Muster**: `harw-tui/src/registry.rs:554`
(`CommandRegistry::dispatch`): `if context.caller_tier < spec.permission {
return Err(CommandError::PermissionDenied { .. }) }` — `PermissionTier`
(`harw-types/src/principal.rs:40-51`) ist total geordnet
(`PartialOrd + Ord`, `Observer < Operator < Maintainer < Owner`).
`OperationMeta::permission: PermissionTier` (`harw-operations/src/
operation.rs:517-525`, `pub`), `Operation::meta(&self) -> &OperationMeta`
(`operation.rs:878`). `RuntimeAssembly::principal(&self) -> &Principal`
(`harw-runtime/src/assembly.rs:1386`), `Principal::tier(&self) ->
PermissionTier` (`harw-types/src/principal.rs:162`).

**Fix**: in `cmd_analyze`, nach dem `find_by_command`-Lookup und vor
`assembly.op_context(..)`/`operation.run(..)`:

```rust
let required: PermissionTier = operation.meta().permission;
let actual: PermissionTier = assembly.principal().tier();
if actual < required {
    return Err(format!(
        "`analyze` erfordert mindestens Berechtigungsstufe {required:?}, \
         der aufrufende Principal hat aber nur {actual:?}"
    ));
}
```

Import ergänzt: `harw_types::PermissionTier` (in die bestehende
`use harw_types::{..}`-Liste aufgenommen; explizite Typannotation an beiden
`let`-Bindungen, damit der Import auch tatsächlich referenziert wird und
nicht als unbenutzt auffällt).

**Test** (main.rs): abgedeckt indirekt über
`test_analyze_assembly_registers_analyze_operation_when_plan_enabled`
(EntryKind::Analyze montiert den Principal über `local_principal(Cli)` mit
Tier `Operator`, E4; `/analyze`s registrierte Stufe liegt in `harw-ops` und
war zum Zeitpunkt dieses Fixes ≤ `Operator`, sodass der bestehende Analyze-
Pfad nicht bricht). Ein dedizierter Negativtest (Principal unterhalb der
Operation-Stufe) hätte einen zweiten `EntryKind`/Principal mit einer Stufe
unterhalb `Observer` gebraucht, was es nicht gibt (`Observer` ist die
niedrigste Stufe und `local_principal` liefert immer `Operator`, E4) — die
Admission-Logik selbst ist wörtlich die in `harw-tui/src/registry.rs:554`
bereits getestete (`registry_invariants.rs`), hier nur auf `cmd_analyze`
übertragen; ein main.rs-eigener Negativtest hätte einen zusätzlichen,
synthetischen `Principal` mit manipulierter Stufe gebraucht, den
`RuntimeAssembly` nicht über eine öffentliche `main.rs`-Schnittstelle
zulässt (Principal entsteht ausschließlich über `Principal::trusted_ingress`/
`child_of`, `harw-types/src/principal.rs:118-182` — außerhalb des
Owned-Bereichs dieses Fixes).

## C5 (minor) — `goal_context` nur geloggt

**Befund**: `tracing::info!(.., goal_context = startup.goal_context.is_some(), ..)`
in `cmd_analyze` loggte nur ein Bool-Feld ohne weitere Wirkung; die Operation
selbst bekam den Ziel-Kontext nicht.

**Fix**: Logfeld `goal_context = startup.goal_context.is_some(),` aus dem
`tracing::info!("analyze.runtime.assembled")`-Aufruf entfernt. Kommentar
direkt darüber ergänzt:

```rust
// Goal-Kontext wirkt bei analyze nur über Store-Seeding (--goal legt das
// Ziel bereits in prepare_planning_startup an); Kind-Registry-Anbindung
// (GoalContextContributor für /analyze) folgt in W4a.
```

`startup.goal_context` selbst bleibt im `PlanningStartup`-Struct (wird vom
Chat-Einstieg, `dispatch`-`None`-Arm Zeile ~232, weiterhin gebraucht) — nur
die analyze-lokale Logzeile ist entfernt, kein totes Feld.

## C6 (minor) — `doctor --config-dir X`: stille Home-Fehler, irreführender Montageversuch

**Befund**: `Some(Command::Doctor { config_dir }) => { .. let home =
home::resolve_home(home_override.clone()).ok(); doctor(layers,
home.as_deref())?; .. }` (main.rs ~:247-253) verwarf einen
`resolve_home`-Fehler still (`.ok()` → `None` → `doctor` druckt in dem Fall
gar nichts zu Laufzeit-Rechten) und versuchte bei gesetztem `--config-dir`
trotzdem, gegen das aufgelöste (ggf. unabhängig davon existierende) HARW-Home
zu montieren, obwohl `--config-dir` explizit *kein* HARW-Home ist.

**Fix**:
- Dispatch-Arm vereinfacht:
  ```rust
  Some(Command::Doctor { config_dir }) => {
      let layers = resolve_layers(home_override.clone(), config_dir.clone())?;
      doctor(layers, home_override.clone(), config_dir)?;
      lifecycle::health(home_override)
  }
  ```
- `doctor`-Signatur geändert: `fn doctor(layers: Vec<PathBuf>, home_override:
  Option<PathBuf>, config_dir: Option<PathBuf>) -> Result<(), String>`
  (einziger Aufrufer ist der Dispatch-Arm, kein weiterer Call-Site zu
  migrieren — grep bestätigt).
- Neue reine Funktion `fn doctor_home_resolution(config_dir: Option<&Path>,
  home_override: Option<PathBuf>) -> Result<PathBuf, String>`: liefert
  `Err("skipped: --config-dir")`, wenn `config_dir` gesetzt ist (Vorrang vor
  einem zufällig gesetzten `--home`), sonst `home::resolve_home(home_override)`
  unverändert.
- `doctor` ruft `match doctor_home_resolution(config_dir.as_deref(),
  home_override) { Ok(home) => print_runtime_rights(&home), Err(reason) =>
  println!("runtime_warning={reason}") }` — beide Fälle (`--config-dir`
  gesetzt, `resolve_home` schlägt fehl) erscheinen jetzt als
  `runtime_warning=`-Zeile statt stillschweigend zu verschwinden; der
  Exit-Code bleibt unverändert der der Config-Prüfung (`doctor` gibt weiterhin
  `Ok(())` zurück, ein Montage-/Auflösungsfehler ist kein Befehlsfehler,
  konsistent mit dem bereits vorhandenen `print_runtime_rights`-Warnpfad für
  Montagefehler).

**Tests** (main.rs, neu, ohne `println!`-Erfassung — reine Auflösungslogik):
- `test_doctor_home_resolution_skips_runtime_rights_with_config_dir`: mit und
  ohne zusätzlich gesetztes `--home` liefert `doctor_home_resolution` bei
  gesetztem `config_dir` immer `Err("skipped: --config-dir")`.
- `test_doctor_home_resolution_resolves_explicit_home_without_config_dir`:
  ohne `config_dir` liefert die Funktion den `home_override`-Pfad unverändert
  über `home::resolve_home` zurück.

Ein Test für den `resolve_home`-Fehlerfall selbst (kein `--home`, keine
auflösbare Umgebung) würde `HARW_HOME`/`$HOME` beeinflussen müssen
(`set_var`), was die Testregeln ausschließen; `home::resolve_home` selbst ist
bereits durch `harw_home::home_dir`-Tests abgedeckt (außerhalb dieses
Owned-Bereichs).

## Zusammenfassung Diff-Umfang

- `harw-cli/src/main.rs`: `PlanServices`-Feld entfernt, `build_plan_services`
  vereinfacht, `analyze_assembly` neu extrahiert, Berechtigungsprüfung in
  `cmd_analyze` ergänzt, Logfeld entfernt, `doctor`/`doctor_home_resolution`
  neu geschnitten, Dispatch-Arm vereinfacht, sechs neue Tests, mehrere
  Doku-Blöcke präzisiert. Kein Verhalten außerhalb der sechs Befunde
  geändert.
- `harw-cli/src/web.rs`: `runtime_plan_services` entfernt, `serve_web` nutzt
  `to_runtime()` direkt, ein Test umgestellt.

## Ausgabe

```json
{"agent":"F-MAIN","files_modified":["/home/mia/projects/harwness/harw-cli/src/main.rs","/home/mia/projects/harwness/harw-cli/src/web.rs"],
 "files_created":["/home/mia/projects/harwness/docs/remediation/ledger/W2d2/F-MAIN.md"],
 "verification":{"command":"read-only + grep + cargo metadata --offline --no-deps --format-version 1","exit_code":0,"pass":null},
 "findings_fixed":["C1","C3","C4","C5","C6"],
 "blocked":false}
```
