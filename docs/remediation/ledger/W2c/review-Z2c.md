# Review Z2c — statischer Compile-Review W2c (READ-ONLY)

- Welle: W2c, Ledger `W2C-01.md`
- Umfang: `harw-runtime/src/{assembly.rs, children.rs, contributors.rs, lib.rs}`,
  `harw-runtime/tests/rights_matrix.rs`, `harw-runtime/Cargo.toml`
- Methode: Zeile für Zeile gegen die tatsächlichen Signaturen der
  Fremd-Crates (jede unten genannte Datei:Zeile wurde gelesen). **Kein
  Compiler, kein clippy, kein Test gelaufen** — nur `cat`/`grep`/`sed`.

## 0. Verifizierte Schnittstellen (kein Befund, aber geprüft)

Alle Importe existieren und sind `pub`; alle Aritäten und Typen stimmen:

- `harw_core` re-exportiert `AgentSession, ChildRegistryFactory,
  ManagedAgentSpawner, ModelProvider, SessionActivation, SessionManager,
  SpawnContext, StateStore, ToolProfile, InMemoryStateStore,
  EchoModelProvider, HISTORY_TAIL_SECTION` (`harw-core/src/lib.rs:33-72`).
- `SpawnContext` hat genau die sieben gesetzten Felder
  (`harw-core/src/session.rs:224-286`) — die Konstruktion in
  `assembly.rs:471-479` ist vollständig, kein `..Default`.
- `SessionManager::new(mpsc::UnboundedSender<SessionEvent>)`
  (`session_manager.rs:19`), `ManagedAgentSpawner::new(Arc<Mutex<SessionManager>>,
  ChildLimits)` (`child_controller.rs:514`), `with_role(impl Into<String>,
  AgentRole, AgentRoleId, Arc<dyn ChildRegistryFactory>)` (`:535`),
  `with_external_root_parent(SessionId, SpawnContext, Option<ReasoningEffort>,
  SessionActivation) -> Result<Self, AgentSpawnError>` (`:580`) — **4-stellig**,
  wie W2a verlangt. ✔
- `ChildRegistryFactory` (`child_controller.rs:392-458`): `build_registry`
  3-stellig mit `Option<&harw_catalog::AgentSuggestions>`, `model_for`
  pflichtig, `executable_agent_ir` mit Default. Der Impl in `children.rs:205-279`
  überschreibt genau diese drei, keine Signaturabweichung, objektsicher;
  `RuntimeChildRegistryFactory` ist `Send + Sync + 'static` (alle Felder sind
  es), die Coercion auf `Arc<dyn ChildRegistryFactory>` in `assembly.rs:683`
  trägt. ✔
- `AgentSession::new_with_id(SessionId, AgentRole, Option<SessionId>,
  ExtensionRegistry, UnboundedSender<SessionEvent>)` (`session.rs:340`),
  `with_spawn_context` (`:429`), `with_reasoning_effort(Option<ReasoningEffort>)`
  (`:621`), `with_turn_event_sink` (`:690`),
  `with_executable_agent_ir(&ExecutableAgentIr)` (`:389`, ruft selbst
  `apply_mode()`), `set_mode(&mut self, InteractionMode)` (`:529`). Die Kette in
  `assembly.rs:1030-1039` ist typkorrekt, `let mut session` ist nötig und
  vorhanden. ✔
- `assemble_registry_for_project(RegistryProfile, &ProjectContext,
  IdentityOverrides, ApprovalModeCell)` (`profile.rs:892`), `profile_for_role
  -> Option<RegistryProfile>` (`:532`), `IdentityOverrides` 3 Felder +
  `Default` (`:574`), `AssembledRegistry{registry,project,identity}`
  (`lib.rs:244`), `role_names::ALL: &[&str]` (9 Rollen, `:175`). ✔
- `ExtensionRegistryBuilder::build() -> ExtensionRegistry` (kein `Result`,
  `registry.rs:507`), `into_builder()` (`:223`), `approval_handler(Arc<dyn
  ApprovalHandler>)` (`:495`), `ToolSpec::name(&self) -> &str`
  (`harw-tools/src/spec.rs:47`) → `registered_tool_names` liefert wirklich
  `Vec<String>`. ✔
- `harw_ops::register_all(&mut OperationRegistry)` (`:205`),
  `register_plan_tools(&mut OperationRegistry, &harw_plan::config::PlanToolConfig)
  -> usize` (`:301`, gleicher Typ wie `PlanServices::plan_config`). ✔
- `RuntimeServicesParts` hat exakt die zehn gesetzten Felder
  (`services.rs:218-244`). `RightsSnapshot` hat exakt die elf gesetzten Felder
  (`spec.rs:244-267`) — **vollständig gefüllt**, kein Feld ausgelassen. ✔
- `Cargo.toml`: jede der drei neuen Zeilen wird gebraucht
  (`harw_catalog::AgentSuggestions` in `children.rs:226`,
  `harw_protocol::events::{SessionEvent,TurnEvent}` in `assembly.rs:63`,
  `serde_json::Value` in `tests/rights_matrix.rs:586`); keine überflüssige und
  keine fehlende Dependency gefunden. Alle `use`-Pfade in den vier
  Quelldateien werden benutzt (kein `unused_imports`). ✔
- **tokio ohne `macros` reicht**: kein `#[tokio::test]` im Testfile. Die Tests
  sind synchron; `mpsc::unbounded_channel()` und `send` brauchen keine
  laufende Runtime, und weder `AgentSession::new_with_id` noch
  `ManagedAgentSpawner::new` betreten eine. Async-Pfade werden gar nicht
  getestet (siehe Z2c-11). ✔
- Erschöpfende `match`: `EntryKind` (`default_approval_mode`, `expected` im
  Test), `OperationSurface` (`build`), `CeilingPolicy` (`root_ceiling`,
  Testarm), `SpawnerPolicy` (`root_organizational_role`, Testarm). **Nicht**
  erschöpfend: `build_spawner` (Z2c-03). `AskResolution` wird nirgends
  gematcht (Z2c-02).

**Kompiliert voraussichtlich: ja.** Ich habe keinen Befund gefunden, der
`rustc` stoppt. Alle Blocker-Kandidaten (Arität von
`with_external_root_parent`, `build()`-Rückgabetyp, `ToolSpec::name`-Typ,
Teil-Moves aus `EntryProfile`, `&Arc<T>`→`&T`-Coercions, Borrow von `parts`
neben `inputs`, `Mutex`-Guard vor `drop(slot)`) lösen sich sauber auf. Die
Befunde unten sind Semantik, Vertragstreue, Tests und Lint-Hygiene.

## 1. Befunde

| ID | Schwere | Datei:Zeile | Befund | Fix |
|---|---|---|---|---|
| Z2c-01 | **hoch** | `assembly.rs:497-506` | `OperationSurface::AllWithModelTools` und `CommandsOnly` liegen im **selben** `match`-Arm und rufen beide `register_all`. Die Unterscheidung des Vertrags wird nirgends durchgesetzt: `RuntimeServices::assemble` (`services.rs:388-394`) legt jeder der vier Flächen dieselbe volle `OperationRegistry` in die `ServiceMap`. `Analyze` und `Web` (Vertrag: „nur Commands") bekommen damit faktisch die Modell-Tool-Fläche. `Surface::ModelTool` je Operation existiert (`harw-operations/src/operation.rs:168-180`) und würde den Filter tragen. | siehe §2 |
| Z2c-02 | **hoch** | `assembly.rs:1006-1027` | `EntryProfile::ask` ([`AskResolution`]) wird in der **gesamten** Montage nie gelesen. `new_root_session` nimmt einen `Option<Arc<dyn ApprovalHandler>>` für **jeden** Einstieg entgegen und hängt ihn an die Registry — auch für `GatewayTelegram`/`JobPrompt`/`McpServe`, deren Vertragszeile `Fail`/`BlockJob` sagt. Ein Einstieg kann so eine Rückfragefläche montieren, die die Tabelle ihm verweigert; `rights_snapshot().approval_chain` zeigt sie danach als `Interactive`/`Channel`. Fail-open gegenüber der Tabelle. | siehe §2 |
| Z2c-03 | mittel | `assembly.rs:686-689` | `if policy == SpawnerPolicy::None { return … }` statt `match`. Ledger §9 behauptet ausdrücklich ein erschöpfendes `match` über `SpawnerPolicy` in `build_spawner` — das stimmt nicht. Eine künftige Variante (z. B. `ConfiguredRoles`) fiele **still** in den `BuiltinRoles`-Zweig statt den Compiler zu brechen. | siehe §2 |
| Z2c-04 | mittel | `tests/rights_matrix.rs:54-65, 120-144, 328-347, 487-507, 527-547` | Der `cwd_lock`/`set_current_dir`-Apparat ist **gegenstandslos**. `load_config` ruft `harw_home::config_layers_report_at(&spec.home, &spec.cwd)` (`config.rs:93`), und diese Funktion ist seit W1 `pub` (`harw-home/src/paths.rs:433`, eigener Test `:729` „uses_explicit_cwd_without_process_cwd"). Kein Montageschritt liest das Prozess-Arbeitsverzeichnis: `discover_project(&spec.cwd, …)`, `root_sandbox(entry, &project.project_root)`. Ledger §6 und die daraus abgeleitete „Folgearbeit 2" (§8.2) sind veraltet. Der Prozess-cwd-Wechsel serialisiert fünf Tests unnötig und ist in einem Testbinary mit Threads eine Fremdwirkung auf alle anderen Tests derselben Datei. | siehe §2 |
| Z2c-05 | mittel | `assembly.rs:613-632` | `resolve_active_agent` schaut **nur** in `builtin_agent_definitions`. `config.executable_agents` (die gelowerten `[agents]`-Rollen, `harw-config/src/discovery.rs:28`) wird ignoriert, obwohl `ResolvedConfig` an derselben Stelle bereits vorliegt. `--agent <konfigurierte Rolle>` scheitert damit ab W2d hart (`RuntimeError::Registry`), wo TUI/CLI heute startet. Fail-closed, also kein Sicherheitsbefund — aber eine Verhaltensänderung, die das Ledger nur für den **Spawner** (§4.8) erklärt, nicht für `active_agent`. | siehe §2 |
| Z2c-06 | mittel | `assembly.rs:1093-1099` | `rights_snapshot().approval_chain` speist sich aus `chain.snapshot()` + gespeichertem Responder, **nicht** aus `registry.approval_handlers()`. Die Wurzel-Registry trägt aber zwei `DefaultApprovalPolicy` (eine aus `profile.rs:921`, eine aus `chain.install`). Der Snapshot behauptet „Momentaufnahme der effektiven Rechte" und weicht von der tatsächlichen Handlerliste ab. — **Ledger §4.9 geprüft und bestätigt:** die Dublette ist rein einschränkend, sogar idempotent: `assemble_registry_for_project` bekommt `chain.mode().clone()`, und `ApprovalModeCell` ist `Arc<RwLock<…>>` mit teilendem `Clone` (`approval_mode.rs:161`), beide Politiken lesen also dieselbe Zelle; die Aggregation ist `Deny > AskUser > Allow` (W1-05). In `children.rs:241-251` gilt dasselbe mit der **detachten** Kindzelle. Kein Rechteleck. | siehe §2 |
| Z2c-07 | niedrig | `assembly.rs:620` / `children.rs:192` | `builtin_agent_definitions(&HashMap::new())` senkt in **jeder** Montage zweimal den kompletten eingebetteten Rollensatz (einmal für `active_agent`, einmal in der Kind-Fabrik). Reine Doppelarbeit; das Ergebnis ist beide Male identisch. | `resolve_active_agent` die Map zurückgeben lassen und in `RuntimeChildRegistryFactory::new` hineinreichen (`new_with_definitions`). |
| Z2c-08 | niedrig | `assembly.rs:466` → `sandbox.rs:141-147` | `root_sandbox(entry, …)` ruft `entry.profile()` ein **zweites** Mal, obwohl `build()` das Profil schon in `profile` hält. Die Reduktionstabelle soll genau einmal je Lauf laufen. | `root_sandbox(&EntryProfile, &Path)` oder `root_sandbox_with(profile.permissions.clone(), …)`; W2b-Signatur, also W2d-Aufgabe. |
| Z2c-09 | niedrig | `assembly.rs:1095-1099` | Verschachteltes `if let Ok(..) { if let Some(..) { … } }` — `clippy::collapsible_if` schlägt seit Clippy 1.88 auch auf `if let`-Ketten an (Edition 2024). Unterdrückt wird das hier nur durch das MSRV-Gate (`rust-version = "1.85"` < 1.88 → let-chains nicht verfügbar). Bei einem MSRV-Schritt wird daraus sofort ein `-D warnings`-Fehler. | `if let Ok(responder) = self.responder.lock()` durch `self.responder.lock().ok().and_then(\|r\| r.as_ref().map(\|h\| (h.label(), h.kind())))` ersetzen. |
| Z2c-10 | niedrig | `lib.rs:30-32`, `assembly.rs:80-82`, `tests/rights_matrix.rs:277-284, 466-469, 511-517`, `assembly.rs:1237` | rustfmt-Drift: mehrere Import-/Aufruf-Blöcke sind mehrzeilig, obwohl sie in 100 Spalten passen (`pub use contributors::{AssemblyContributor, AssemblyInputs, AssemblyParts, default_contributors};` = 96 Zeichen), und in `assembly.rs` steht eine Leerzeile vor dem schließenden `}` des Testmoduls. `cargo fmt --check` schlägt fehl. In dieser Welle war `fmt` verboten — gehört in die erste W2d-Aktion. | `cargo fmt -p harw-runtime`. |
| Z2c-11 | niedrig | `tests/rights_matrix.rs` | Testlücken gegenüber der Vertragstabelle: (a) **keine** Zeile prüft `OperationSurface` (weder `operations().len()` je Einstieg noch `None` ⇒ 0), obwohl genau dort Z2c-01 sitzt; (b) **keine** Zeile prüft `AskResolution`; (c) `closing_a_session_reaches_every_hook` prüft nichts Beobachtbares (kein Haken registrierbar, weil `default_contributors()` leer ist und `AssemblyParts` von außen nicht füllbar). | Zeile um `operations_len_is_zero`/`>0` und `ask` ergänzen; für (c) einen Test-`AssemblyContributor` im Testfile bauen und über `.contributor(..)` einhängen — das testet zugleich den Erweiterungspunkt, der heute ungetestet ist. |
| Z2c-12 | niedrig | `assembly.rs:1154` | Testname `no_entry_defaults_to_full_access` beschreibt eine schwächere Zusage als der Rustdoc (`immer Delegated`). Ein `assert_eq!(.., ApprovalMode::Delegated)` wäre die Zusage, die die Funktion gibt. | `assert_eq!(default_approval_mode(entry), ApprovalMode::Delegated)`. |

## 2. Fix-Snippets

### Z2c-01 — `OperationSurface` durchsetzen (`assembly.rs`)

```rust
// statt: AllWithModelTools | CommandsOnly => { register_all(..) }
match profile.operations {
    OperationSurface::AllWithModelTools => {
        harw_ops::register_all(&mut operations);
        if let Some(plan) = plan_services.as_ref() {
            let _ = harw_ops::register_plan_tools(&mut operations, &plan.plan_config);
        }
    }
    OperationSurface::CommandsOnly => {
        let mut all = OperationRegistry::new();
        harw_ops::register_all(&mut all);
        if let Some(plan) = plan_services.as_ref() {
            let _ = harw_ops::register_plan_tools(&mut all, &plan.plan_config);
        }
        // Nur Operationen, die eine Command-Fläche deklarieren.
        for op in all.iter() {
            if op.surfaces().iter().any(|s| matches!(s, Surface::Command { .. })) {
                operations.register(Arc::clone(op));
            }
        }
    }
    OperationSurface::None => {}
}
```
(Der genaue Accessor heißt in `harw-operations` `by_surface(..)`
(`registry.rs:492`); dessen Rückgabe lässt sich direkt umhängen. Wenn die
Verengung stattdessen erst beim Bau der Modell-Tool-Liste greifen soll,
gehört das mindestens als `debug_assert` + Rustdoc-Vertrag hierher, damit
`CommandsOnly` nicht weiter unbelegt ist.)

### Z2c-02 — `AskResolution` fail-closed prüfen (`assembly.rs::new_root_session`)

```rust
if responder.is_some() && self.profile.ask != AskResolution::Interactive {
    return Err(RuntimeError::Registry {
        detail: format!(
            "entry {:?} resolves approvals as {:?} and must not carry an interactive \
             responder", self.spec.entry, self.profile.ask
        ),
    });
}
```
Dazu `use crate::spec::AskResolution;` und ein Testfall je Einstieg
(`for entry in ALL_ENTRIES` — nur `Tui` darf einen Responder annehmen).

### Z2c-03 — `match` statt `==` (`assembly.rs::build_spawner`)

```rust
match policy {
    SpawnerPolicy::None => return Ok((None, Vec::new())),
    SpawnerPolicy::BuiltinRoles => {}
}
```

### Z2c-04 — cwd-Apparat entfernen (`tests/rights_matrix.rs`)

`cwd_lock()`, `previous`/`set_current_dir` in `assemble`,
`a_spawning_entry_needs_a_session_event_sender`,
`root_activation_matches_the_session_base_activation` und
`an_unknown_active_agent_fails_closed` ersatzlos streichen; `spec.cwd`/`home`
kommen bereits aus der Fixture. Ledger §6 und §8.2 entsprechend berichtigen
(`config_layers_report_at` **existiert** und wird benutzt).

### Z2c-05 — konfigurierte Agenten auflösen (`assembly.rs`)

```rust
fn resolve_active_agent(
    name: Option<&str>,
    config: &ResolvedConfig,
) -> RuntimeResult<Option<ExecutableAgentIr>> {
    let Some(name) = name else { return Ok(None) };
    if let Some(ir) = config.executable_agents.get(name) {
        return Ok(Some(ir.clone()));
    }
    let definitions = builtin_agent_definitions(&config.executable_agents)
        .map_err(..)?;
    definitions.get(name).cloned().map(Some).ok_or_else(..)
}
```
(`builtin_agent_definitions` nimmt genau dafür eine `existing`-Map entgegen,
`embedded_agents.rs:621`.) Alternativ bewusst ablehnen — dann aber im Ledger
§4 als eigene Entscheidung führen, nicht als Nebenwirkung von §4.8.

### Z2c-06 — Snapshot aus der Registry speisen

Entweder `tools`/`approval_chain` beide beim `build()` aus der fertigen
`ExtensionRegistry` ableiten (`registry.approval_handlers().iter().map(|h|
(h.label(), h.kind()))`) — dann fällt die Dublette im Snapshot auf und ist
belegt —, oder den Rustdoc von `RightsSnapshot::approval_chain` auf „die
Kette der Montage, ohne die von `assemble_registry_for_project` selbst
registrierte Politik" schärfen. Die im Ledger §8.3 vermerkte Folgearbeit
(Montageweg ohne eigene `DefaultApprovalPolicy` in `harw-registry-defaults`)
löst beides und bleibt die saubere Variante.

## 3. Fix-Aufgaben nach Datei

- `harw-runtime/src/assembly.rs`: Z2c-01, Z2c-02, Z2c-03, Z2c-05, Z2c-06,
  Z2c-07, Z2c-09, Z2c-10, Z2c-12.
- `harw-runtime/src/sandbox.rs` (W2b-Signatur, W2d): Z2c-08.
- `harw-runtime/src/children.rs`: Z2c-07 (Konstruktor mit vorgesenkten
  Definitionen). Sonst ohne Befund.
- `harw-runtime/src/contributors.rs`: ohne Befund (Doctest korrekt, Trait
  `Send + Sync`, `AssemblyParts` vergibt kein Recht — die im Modul-Rustdoc
  behauptete Nicht-Erweiterbarkeit auf der Rechte-Achse hält: kein Feld
  trägt Sandbox, Permissions, Decke oder Modus; `network_scope` ist die
  einzige Ausnahme und ist im Vertrag als W5-Punkt benannt).
- `harw-runtime/src/lib.rs`: Z2c-10 (nur fmt). Re-Exporte vollständig und
  konfliktfrei.
- `harw-runtime/tests/rights_matrix.rs`: Z2c-04, Z2c-11, Z2c-10.
- `harw-runtime/Cargo.toml`: ohne Befund.
- `docs/remediation/ledger/W2c/W2C-01.md`: §6 und §8.2 (Z2c-04) sowie §9
  (Z2c-03) berichtigen.

## 4. Vertragstabelle — Abgleich der Zusagen

| Zusage | Stand |
|---|---|
| Discovery genau einmal | ✔ `assembly.rs:459`; Kind-Fabrik hält den `ProjectContext` und ruft `assemble_registry_for_project` (nimmt keinen Pfad) — G-071/R5 geschlossen. Test `discovery_runs_exactly_once_per_assembly` beweist es über das gelöschte Verzeichnis. |
| Ein `SpawnContext`, ein Trace | ✔ `assembly.rs:470-479`, geklont für Spawner (`:716`) und Sitzung (`:1031`) — G-044 geschlossen. |
| Config-Politik unabhängig vom Plan-Flag | ✔ `ApprovalChain::for_root` liest nur `harness.policy.require_approval_for` (`approval.rs:139`), vor und ohne Bezug auf `plan_services` — G-010/F-154 geschlossen. |
| Kinder ohne `FullAccess` | ✔ `ApprovalChain::for_child` detached + senkt `FullAccess` → `Delegated` (`approval.rs:187-205`); der Kindmodus geht als **dieselbe** Zelle in beide Politiken der Kind-Registry. |
| Kein Netz | ✔ `network_scope` bleibt `NetworkScope::empty()` (nur ein Contributor könnte ihn setzen, und es gibt keinen); kein Profil trägt `NetworkAccess`. Test `no_entry_carries_network` prüft Snapshot, `network_scope()` **und** Sandbox-Scope. |
| Unbekannte Rolle → `Err` | ✔ `children.rs:228-233` (R4, kein `unwrap_or_default`); Test `the_child_factory_refuses_an_unknown_role` inkl. Gegenprobe. |
| Kette wird vererbt (F-018) | ✔ `children.rs:241/251`. Der Test prüft allerdings nur `len() >= parent_handlers` — eine schwache Zusage; besser gegen `snapshot()`-Kinds. |
| Ask-Auflösung je Einstieg | ✘ Z2c-02 — deklariert, nirgends durchgesetzt. |
| Operations-Fläche je Einstieg | ✘ Z2c-01 — `CommandsOnly` == `AllWithModelTools`. |

## 5. Bewertung der Ledger-Abweichungen §3/§4

- **§3 (Reihenfolge Modell → Spawner → Contributors → Services statt
  Services → Spawner → …): begründet und richtig.** Nachgeprüft:
  `RuntimeServicesParts::spawner` ist ein Konstruktionsfeld
  (`services.rs:230`), `RuntimeServices` hat keinen Nachrüst-Setter, und
  `RuntimeChildRegistryFactory::new` braucht den fertigen
  `Arc<dyn ModelProvider>`. Die Auftragsreihenfolge ist nicht baubar. Zustimmung.
- **§4.1 (`session_events` am Builder, fail-closed bei `BuiltinRoles`):**
  korrekt begründet (`SessionManager::new` nimmt den Sender bei der
  Konstruktion). Ein eigener, ins Leere laufender Kanal hätte alle
  Kind-Events verschluckt. Zustimmung; Test vorhanden.
- **§4.2/§4.3 (Wurzel-ID gehört der Montage, genau eine Wurzelsitzung):**
  korrekt und durch `with_external_root_parent`s Einmaligkeit erzwungen
  (`child_controller.rs:585`). `ExtensionRegistry` ist nicht `Clone` — die
  `Mutex<Option<..>>`-Vergabe ist die einzige saubere Form. Zustimmung.
- **§4.4 (Responder erst in `new_root_session`):** Konstruktion in Ordnung,
  aber unvollständig — siehe Z2c-02 (keine Prüfung gegen `profile.ask`).
- **§4.5 (`default_approval_mode` immer `Delegated`):** Begründung trägt;
  der Test bleibt hinter der Zusage zurück (Z2c-12).
- **§4.6 (`root_organizational_role`):** zweite, unabhängige Sperre über
  `can_spawn`; korrekt.
- **§4.7 (`root_activation` dupliziert `with_executable_agent_ir`):** die
  Kopie ist wortgleich zu `session.rs:389-397` (verglichen), und
  `with_external_root_parent` braucht den Wert tatsächlich vor der Sitzung.
  Der benannte Test prüft allerdings nur `profile() == Minimal`, nicht die
  Gleichheit der Flächen — `SessionActivation` hat kein `PartialEq`, aber
  ein Vergleich über `is_tool_enabled` je Name aus `tool_surface()` wäre
  möglich und sollte nachgezogen werden.
- **§4.8 (nur `role_names::ALL` im Spawner):** trägt; ergänzend Z2c-05, weil
  dieselbe Entscheidung `active_agent` mitbetrifft, dort aber nicht erklärt ist.
- **§4.9 (doppelte `DefaultApprovalPolicy`):** **geprüft und bestätigt —
  wirklich nur einschränkend**, sogar wirkungsgleich, weil beide Instanzen
  über geteilte `Arc`-Zellen denselben Modus lesen und die Aggregation
  `Deny > AskUser > Allow` ist. Verbleibender Rest: der Snapshot bildet die
  Registry nicht ab (Z2c-06).

## 6. Zahlen

- Befunde: 12 — 0 blocker, 2 hoch, 4 mittel, 6 niedrig.
- Geprüfte Fremd-Signaturen: 41 (alle bestätigt, keine Abweichung).
- **Kompiliert voraussichtlich: ja** (rustc + clippy unter MSRV 1.85);
  `cargo fmt --check` schlägt fehl (Z2c-10).
