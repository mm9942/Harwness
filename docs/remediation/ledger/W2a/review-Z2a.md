# Review Z2a-R — statischer Compile-Review Welle W2a

- Rolle: Review-Agent Z2a-R, **READ-ONLY** (kein `cargo`/`make`/`rustc`, keine
  `git`-Schreibbefehle; einzige Schreibdatei ist dieses Ledger).
- Methode: Ersatz für `cargo check`/`clippy -D warnings`. Zeile für Zeile gegen
  die tatsächlich gelesenen APIs geprüft: Syntax, Pfade, Arität, Typen,
  Borrows, Erschöpfung, Trait-Bounds, Test-Helfer, Workspace-weite Aufrufer per
  `grep`.
- Stand: Arbeitsbaum über Basis-Commit `3333441` (W1).

## Geprüfter Umfang

| Datei | Ledger | Befund |
|---|---|---|
| `harw-core/src/session.rs` | W2A-01 | 2 Findings (A1 anteilig, A2/A3) |
| `harw-core/tests/session_mode_intersection.rs` (neu) | W2A-01 | 1 Finding (A4) |
| `harw-core/src/child_controller.rs` | W2A-02 | 1 Finding (A1) |
| `harw-extension-api/src/approval_mode.rs` | W2A-03 | sauber |
| `harw-ops/src/permissions.rs` | W2A-05 | sauber |
| `harw-registry-defaults/src/lib.rs` | W2A-04 | sauber |
| `harw-registry-defaults/src/profile.rs` | W2A-04 | sauber |

Während des Reviews sind zwei **weitere** Dateien im Arbeitsbaum aufgetaucht,
die nicht in `docs/remediation/ownership/W2a.toml` stehen, aber genau die zwei
im Ledger als W2c-Folgearbeit gemeldeten Punkte erledigen:
`harw-core/src/mode.rs` (stale Prosa zur Ratsche, jetzt korrekt) und
`harw-core/tests/child_controller.rs:784` (Effort-Test umbenannt und auf
`Some(Medium)` umgestellt). Beide Diffs sind inhaltlich korrekt und passen
exakt zur neuen Semantik; sie sind damit **erledigt** und keine offene
W2c-Arbeit mehr. Formal liegen sie außerhalb des Ownership-Eintrags — der
Orchestrator sollte `W2a.toml` nachziehen oder die Zuordnung bestätigen.

## Ergebnis in Zahlen

- **Blocker (Compile/Semantik in W2a-Dateien): 0.**
- Findings gesamt: 4 zum Sofort-Fix (0 blocker, 1 hoch, 1 mittel, 2 niedrig),
  7 bekannte W2c-Folgearbeiten (nur gelistet).
- Compile-relevante Brüche außerhalb des Schreibbereichs: 7, allesamt bereits
  in den Ledgern W2A-02/W2A-04 gelistet; **keine weiteren** gefunden
  (Workspace-weite Greps auf `with_external_root_parent`, `ExternalRootParent`,
  `DefaultApprovalPolicy`, `RegistryProfile` + `default`/`unwrap_or_default`,
  `profile_for_role`, `approval_mode::current|set`, `AgentSession {`,
  `registry_profile`, `clamp_child_reasoning_effort`).
- `approval_mode::current`/`::set`/`static ACTIVE`/`to_repr`/`from_repr`:
  **restlos entfernt**, workspace-weit inkl. `*.md` und Doctests — die einzigen
  verbliebenen Treffer sind die W2a-Ledger selbst, die den Vorgang
  protokollieren.
- Zeilenbreite: keine neue Überschreitung > 100 **Zeichen**; die
  `awk length>100`-Treffer sind Byte-Längen mehrsprachiger String-Literale bzw.
  Box-Drawing-Kommentare, die rustfmt nicht umbricht.
- Keine `#[allow(...)]`, kein `todo!`/`unimplemented!`/`FIXME` im Diff.
- Workspace-Lints sind nur `unsafe_code = "forbid"` (Cargo.toml) — kein
  pedantic/nursery-Satz; die gefundenen clippy-Muster sind entsprechend
  niedrig eingestuft.
- MSRV 1.85 / Edition 2024: keine neuere Syntax als `let … else` und
  `if let (…, …) = (…, …)`; beide ≥ 1.65 bzw. stabil.

## (A) Fixes JETZT — in W2a-Dateien oder Tests von `harw-core`

| ID | Schwere | Datei:Zeile | Befund | Fix-Snippet |
|---|---|---|---|---|
| **A1** | **hoch** | `harw-core/src/child_controller.rs:1873-1889` (Schnitt-Block) + `harw-core/src/session.rs:539-561` (`apply_mode`) | **Wechselwirkung W2A-01 ↔ W2A-02.** Der Eltern-Schnitt wird nur in `activation` geschrieben (`*child_session.activation_mut() = cut`), nicht in die neue `base_activation`. W2A-01 leitet `activation` ab jetzt aber bei **jedem** `apply_mode()` frisch aus `base_activation` ab. Ein späteres `set_mode`/`with_mode` auf einer Kind-Session verwirft den Eltern-Schnitt daher stillschweigend und stellt die volle IR-Fläche wieder her. In der Doku von `set_mode` steht zugleich ausdrücklich, Laufzeit-Overrides über `activation_mut()` fielen bei jedem Modus-Wechsel weg — genau dieser Weg wird hier für eine **Autoritätsgrenze** benutzt. Heute nicht auslösbar (kein Produktions-`set_mode` auf Kind-Sessions; Aufrufer sind nur `harw-tui/src/app.rs:2470` und `session_controller.rs:202` auf der Wurzel), aber exakt die Klasse stiller Lücke, gegen die F-017/E1 antritt. | `session.rs`, neben `base_activation()`:<br>`/// Verengt die Basis dauerhaft auf den Schnitt mit `ceiling`.`<br>`/// Monoton: nie weiter als vorher; der aktuelle Modus wird neu angewandt.`<br>`pub fn narrow_base_activation(&mut self, ceiling: &SessionActivation) {`<br>`    self.base_activation = self.base_activation.intersect(ceiling);`<br>`    self.apply_mode();`<br>`}`<br><br>`child_controller.rs`, im Schnitt-Block statt `activation()/activation_mut()`:<br>`child_session.narrow_base_activation(&parent_activation);`<br><br>Dazu ein Test in `child_controller.rs`: Kind admittieren, `set_mode(InteractionMode::Work)`, danach muss `shell.exec` weiterhin verboten sein. |
| **A2** | mittel | `harw-core/src/session.rs:774-779` (`with_activation`) | `with_activation` setzt `base_activation` **und** `activation`, ruft aber als einziger Basis-Setzer **kein** `apply_mode()`. `with_mode(Explore).with_activation(a)` liefert deshalb eine ungeschnittene `activation` — die in `with_spawn_context`/`set_mode` dokumentierte Reihenfolgeunabhängigkeit gilt für diesen Builder nicht. Die Ledger-Begründung („Chat-Schnitt wäre Identität") trägt nur, solange der Modus `Chat` ist. Genutzt u. a. von `harw-core/src/turn_loop.rs:2194` und dem neuen Test `grandchild_activation_inherits_the_whole_intersection_chain` (dort nach `with_spawn_context`). | `pub fn with_activation(mut self, activation: SessionActivation) -> Self {`<br>`    self.base_activation = activation;`<br>`    self.apply_mode();`<br>`    self`<br>`}` |
| **A3** | niedrig | `harw-core/src/session.rs:777` | `self.base_activation = activation.clone(); self.activation = activation;` — eine vollständige `HashSet`-Kopie pro Builder-Aufruf, die mit A2 ersatzlos entfällt (`apply_mode` berechnet `activation` ohnehin neu). Kein `-D warnings`-Fehler (`clippy::redundant_clone` ist nursery), aber überflüssig. | entfällt mit dem Snippet aus A2. |
| **A4** | niedrig | `harw-core/tests/session_mode_intersection.rs` (Ende) | Die sieben Tests decken Decke, Reversibilität, Sandbox-Gleichheit und Builder-Reihenfolge sauber ab — es fehlt aber genau der Fall aus A1/A2: (a) eine über `with_activation` gesetzte Basis überlebt einen Modus-Wechsel, (b) `with_mode(x).with_activation(a)` == `with_activation(a).with_mode(x)`. Ohne (b) bleibt die Regression aus A2 untestiert. | `#[test] fn with_activation_is_independent_of_builder_order() {`<br>`    let mut act = SessionActivation::new(ToolProfile::Full);`<br>`    act.disable_tool(ToolName::new("shell.exec"));`<br>`    let a = plain_session().with_mode(InteractionMode::Explore).with_activation(act.clone());`<br>`    let b = plain_session().with_activation(act).with_mode(InteractionMode::Explore);`<br>`    for n in ["fs.read", "fs.write", "shell.exec"] {`<br>`        assert_eq!(enabled(&a, n), enabled(&b, n), "{n}");`<br>`    }`<br>`}` |

### Was ausdrücklich sauber ist (geprüft, kein Finding)

- **Imports/Pfade:** `SandboxSpec` ist in `session.rs:110` importiert;
  `crate::activation::SessionActivation` in `child_controller.rs:22`,
  `ToolProfile` im Testmodul; `ApprovalModeCell` mit vollem Modulpfad in
  `permissions.rs`, `lib.rs`, `profile.rs` (korrekt — `harw-extension-api`
  re-exportiert nur `ApprovalMode` am Crate-Wurzel);
  `ProjectContext` ist am Wurzel von `harw-project-discovery` re-exportiert.
  Keine ungenutzten Imports, keine verwaisten.
- **Arität/Typen:** `SessionActivation::intersect(&self, &Self) -> Self`
  (`activation.rs:395`), `SandboxSpec::restrict(&self, &PermissionSet) -> Self`
  (`harw-sandbox/src/lib.rs:898`), `OpContext::service<S>() -> Option<&S>`
  (`harw-operations/src/context.rs`), `ServiceMap::{new,insert}` pub,
  `ExtensionRegistry::approval_handlers() -> &[Arc<dyn ApprovalHandler>]`,
  `ApprovalModeCell::{new,get,set,detached}` + `Clone`/`Debug`/`Default` —
  alle Aufrufe passen.
- **Borrow-Checker:** `apply_mode`s
  `if let (Some(context), Some(base)) = (self.spawn_context.as_mut(), self.base_sandbox.as_ref())`
  greift auf **disjunkte Felder** zu (erlaubt); `ceiling` wird bewusst vorher
  berechnet. In `admit` ist der Schnitt-Block ein eigener Scope unter demselben
  `manager`-`MutexGuard`; `parent_activation` ist ein Eigentümer-Clone, es gibt
  keinen gleichzeitigen Borrow auf `manager`. `parent.activation().clone()` und
  `Self::parent_depth(&manager, …)` sind beide *immutable* — kein Konflikt.
- **Clone-Anforderungen:** `SessionActivation: Clone` (`activation.rs:154`),
  `SandboxSpec: Clone` (`harw-sandbox/src/lib.rs:825`),
  `ProjectContext: Clone` (`discovery.rs:361`), `ReasoningEffort: Copy + Ord`.
- **Erschöpfende `match`:** `clamp_child_reasoning_effort` ersetzt das
  4-armige Tupel-`match` durch `match cap { Some(c) => …, None => … }` —
  vollständig. Die drei `ApprovalMode`-Arme in `review` sind unverändert
  vollständig.
- **W2A-02-Semantik:** `effective = Some(owner_override.unwrap_or(capped))` ist
  äquivalent zur beschriebenen Regel und immer `Some(..)`. Die zwei Nutzer in
  `harw-core-bridge/src/agent_tool.rs:514,1831` werten das Ergebnis nicht gegen
  `None` aus — kein Bruch.
- **W2A-03:** `current`/`set`/`static ACTIVE`/`to_repr`/`from_repr` sind
  vollständig weg, `use std::sync::{Arc, RwLock}` wird noch gebraucht, das
  Modul-Doc-Beispiel ist auf `ApprovalModeCell` umgestellt und ist als
  Doctest gültig.
- **W2A-04:** `EntryProfile` (`harw-runtime/src/spec.rs:92`) leitet **kein**
  `Default` ab und setzt `registry_profile` überall explizit — das Entfernen
  von `RegistryProfile: Default` bricht dort nichts. Kein `serde(default)` auf
  `RegistryProfile` im Workspace. Der neue Doctest zu
  `assemble_registry_for_project` ist `no_run` und nutzt das offizielle
  `# Ok::<(), Box<dyn std::error::Error>>(())`-Idiom; sowohl
  `RegistryDefaultsError` (`error.rs:73`) als auch `DiscoveryError`
  (`discovery.rs:95`) implementieren `std::error::Error`.
  `test_assemble_registry_for_project_never_runs_discovery_again` ist
  deterministisch (PID + Nanosekunden im Pfad, kein geteilter Zustand) und
  beweist die Zusage über die Unmöglichkeit statt über einen Zähler.
- **W2A-05:** Argumentvalidierung läuft vor dem Cell-Zugriff, die beiden
  bestehenden `InvalidArguments`-Tests bleiben damit gültig; `test_context()`
  ist jetzt pro Test eindeutig (Atomic + PID) und parallelisierbar; die
  Modul-Konstante ist genutzt (kein `dead_code`). Die einzigen weiteren
  `permissions`-Fundstellen im Workspace (`harw-ops/src/lib.rs`,
  `harw-tui/tests/registry_invariants.rs`, `harw/tests/sdk_example.rs`) prüfen
  nur Registrierung/Namen, nie die Ausführung — keine Folgearbeit.
- **Neue Integrationstests (`session_mode_intersection.rs`):** alle Helfer sind
  wortgleich zu den bestehenden Unit-Test-Helfern in `session.rs`; jede
  benutzte Crate (`harw-agent-dsl`, `harw-extension-api`, `harw-sandbox`,
  `harw-tools`, `harw-types`, `tokio`) ist reguläre `[dependencies]` von
  `harw-core` und damit für ein `tests/`-Target sichtbar. Lifetime-Elision in
  `sandbox_of` ist korrekt. Alle Imports werden benutzt.
- **Bestehende Modus-Tests in `session.rs`** bleiben unter der neuen Semantik
  gültig — durchgerechnet über `ToolProfile::allowlist()` (`Minimal → Some(∅)`,
  `Full → None`) und den `both_full`-Zweig von `intersect`: Default-Basis `Full`
  ∩ `Work`(Full) = `Full` (`test_set_mode_work_reopens_tool_activation_only`),
  ∩ `Explore`(Minimal + `EXPLORE_TOOLS`) = `Minimal` + Explore-Fläche
  (`test_set_mode_explore_…`, `test_set_mode_drops_earlier_tool_overrides`,
  `test_with_mode_sets_and_applies_without_emitting_an_event`).

## (B) Bekannte W2c/W2d-Folgearbeit — nur gelistet, **nicht** als Blocker gewertet

Diese Stellen brechen den Workspace-Build, gehören aber alle zu bereits
protokollierter Folgearbeit. Die Suche nach *weiteren* Fundstellen war
ergebnislos.

| # | Datei:Zeile | Bruch | Gemeldet in |
|---|---|---|---|
| B1 | `harw-tui/src/app.rs:1919` | `with_external_root_parent(id, ctx, effort)` — neue Arität (4. Parameter `parent_activation`) | W2A-02 |
| B2 | `harw-cli/src/chat.rs:690` | dito, zusätzlich `reasoning_effort = None` (zweite Hälfte von E3b) | W2A-02 |
| B3 | `harw-tui/src/app.rs:5549` (Test) | `Arc::new(harw_registry_defaults::DefaultApprovalPolicy)` als Unit-Wert → `DefaultApprovalPolicy::new(<Zelle>)` | W2A-04 |
| B4 | `harw-tui/src/app.rs:5668` (Test) | dito | W2A-04 |
| B5 | `harw-tui/src/app.rs:1732` | `profile_for_role(role).unwrap_or_default()` — `RegistryProfile` hat kein `Default` mehr; fail-closed ersetzen (`ok_or(AgentSpawnError …)?`) | W2A-04 |
| B6 | `harw-cli/src/chat.rs:575` | dito | W2A-04 |
| B7 | `harw-cli/src/chat.rs:1536` (Test) | `RegistryProfile::default().registered_tool_names()` → `RegistryProfile::Full` | W2A-04 |

Nicht-brechend, aber im selben Migrationsschritt zu erledigen:

- `harw-cli/src/chat.rs:556-559` — Rustdoc-Intra-Doc-Link auf
  `RegistryProfile::default`; der Pfad existiert nicht mehr → `cargo doc`-Warnung
  (kein `rustc`/`clippy`-Fehler).
- `harw-tui/src/app.rs:1646-1648` — Prosa „Default-Profil … `RegistryProfile::Full`",
  inhaltlich veraltet; der Link auf `RegistryProfile::Full` selbst bleibt gültig.
- **Durchreichen der Sitzungs-Zelle (G-009, eigentlicher Nutzen):** solange
  `harw-tui`/`harw-cli` weiter über `assemble_registry`/`assemble_default_registry`
  montieren, bekommt jede Registry eine `ApprovalModeCell::default()`, die
  niemand von außen umlegen kann, und `/permissions` liefert in jeder Laufzeit
  ohne registrierte Cell `OpError::NotAvailable`. Die Composition-Roots müssen
  auf `assemble_registry_for_project(…, cell.clone())` umstellen **und** dieselbe
  Cell unter `ApprovalModeCell` in die `ServiceMap` legen (W2B-04/W2C,
  RuntimeServices). Das ist bewusst fail-closed, aber bis dahin ist
  `/permissions` funktionslos.
- Optional/additiv: `harw-extension-api/src/lib.rs:28` re-exportiert
  `ApprovalModeCell` nicht am Crate-Wurzel — drei Dateien nennen deshalb den
  vollen Modulpfad.

### Bereits erledigt (vormals W2c, im Arbeitsbaum gefixt)

- `harw-core/src/mode.rs:14-20` — die Prosa behauptet nicht mehr, `Explore → Work`
  stelle nichts wieder her; sie beschreibt jetzt korrekt den Schnitt gegen die
  **Basis**-Sandbox. Reine Doku, kein Doctest.
- `harw-core/tests/child_controller.rs:784-811` — `…_no_base_with_cap_stays_none`
  → `…_no_base_with_cap_falls_back_to_the_default`, Erwartung `Some(Medium)`,
  Kommentar korrigiert. **Weitere brechende Tests in dieser Datei: keine** —
  die Clamp-Tests bei 716, 750, 768, 815 setzen eine Basis, 846 prüft den
  Fehlerpfad; `with_external_root_parent` wird dort nicht aufgerufen.
