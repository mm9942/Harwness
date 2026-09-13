# review-Z2b-R1 — Statischer Compile-Review W2b (READ-ONLY)

Rolle: Review-Agent Z2b-R1. **Kein Compiler-Lauf.** Geprüft Zeile für Zeile
gegen die tatsächlichen Definitionen der abhängigen Crates (alle Pfade unten
gelesen). Kein `cargo build`, kein `clippy`, kein `rustfmt`, keine
Code-Änderung. Einzige geschriebene Datei: diese.

Umfang: `harw-runtime/src/{config.rs, model.rs, approval.rs, services.rs}`,
`harw-runtime/Cargo.toml`, `harw-runtime/src/lib.rs`,
`harw-home/src/{paths.rs, lib.rs}` (Z2b-F0).

## Ergebnis in einem Satz

**Kein Blocker.** Alle 4 Runtime-Module und der harw-home-Fix sind aus meiner
Sicht übersetzbar; jeder benutzte Pfad, jede Arität, jedes Trait-Bound und
jedes Struct-Literal wurde gegen die Quelle verifiziert. Gefunden wurden nur
7 Befunde der Schwere B/C (Manifest-Hygiene, veraltete Kommentare, Formatierung).

## Befunde

| ID | Schwere | Datei:Zeile | Befund | Fix-Snippet |
|---|---|---|---|---|
| R1-01 | B | `harw-runtime/Cargo.toml:31,37` | `harw-secrets` und `secrecy` sind deklariert, werden aber in **keiner** Datei von `harw-runtime/src` benutzt (`grep -rw` = 0 Treffer). Kein Compile-Fehler (`unused_crate_dependencies` ist nicht aktiv), aber toter Dep-Graph + zusätzliche Build-Zeit. W2B-01 §Manifest hatte sie nur als *Option* vorgeschlagen; `model.rs` hat stattdessen den injizierten `&dyn SecretResolver` gewählt. | Entweder entfernen: <br>`-harw-secrets = { path = "../harw-secrets" }`<br>`-secrecy = "0.10.3"`<br>oder in W2c beim Bau des Resolvers benutzen. Kein Zyklus: `harw-secrets` hängt nur an `harw-fsutil`/`harw-macros`/`harw-observe`. |
| R1-02 | B | `harw-runtime/Cargo.toml:41-42` | `[dev-dependencies] tempfile` ist neu und ungenutzt: `config.rs` und `model.rs` bauen ihr `TempDir` weiterhin von Hand. | Entweder `tempfile` streichen oder `config.rs`-`TempDir` (Z. 140-175) durch `tempfile::TempDir::new()?` ersetzen (spart ~35 Zeilen Testcode inkl. `Drop`). |
| R1-03 | C | `harw-runtime/src/config.rs:138-139` | Kommentar behauptet „kein `tempfile` in den Dev-Dependencies dieses Crates“ — seit der Manifest-Änderung falsch. | `/// Temporäres Verzeichnis unterhalb von `std::env::temp_dir()`, das sich`<br>`/// beim Fallenlassen selbst entfernt.` (Begründungssatz ersatzlos streichen oder R1-02 umsetzen.) |
| R1-04 | C | `harw-runtime/src/config.rs:99-105` | Einrückung des `map_err`-Blocks ist nicht rustfmt-stabil (Closure-Body auf Spalte 8 statt Spalte 12); `cargo fmt --check` wird die Stelle umbrechen. Kein Compile-Fehler. | `let config = harw_config::discover_config_with_restricted(&layers, untrusted_repo.as_deref())`<br>`    .map_err(\|error\| RuntimeError::Config { detail: error.to_string() })?;` |
| R1-05 | C | `harw-runtime/src/services.rs:67,181,205,245,591,679,724,779` | 8 Zeilen > 100 Spalten (193-217). Alle sind Box-Drawing-Trennkommentare (`// ── ServiceSurface ───…`); `rustfmt` bricht Kommentare ohne `wrap_comments` nicht um, `cargo fmt --check` bleibt also grün — die Projektkonvention „≤ 100 Spalten“ (W2B-01/Z2b-F0 Ledger, `awk 'length>100'`) ist trotzdem verletzt. | Füllstriche kürzen, z. B. `// ── ServiceSurface ──────────────` (auf ≤ 100 Spalten inkl. Einrückung). |
| R1-06 | C | `harw-runtime/src/config.rs:120-127` | `map_home_error` matcht mit Catch-all `other => RuntimeError::Config`. Eine künftige `HomeError`-Variante (z. B. eine weitere Trust-Variante) wird damit **stillschweigend** zum Config-Fehler statt zum Trust-Fehler; der Compiler meldet nichts. | Alle sieben Varianten explizit aufzählen (`NoHomeDirectory`, `HomeNotADirectory`, `InvalidProfileName`, `InvalidVisibilityName`, `Io` → `Config`; `TrustStore`, `UntrustableProject` → `Trust`), damit eine neue Variante einen Compile-Fehler erzeugt. |
| R1-07 | C | `harw-runtime/src/approval.rs:232-238` | `install(&self, builder: ExtensionRegistryBuilder)` mit `let mut builder = builder;` im Rumpf. | `pub fn install(&self, mut builder: ExtensionRegistryBuilder) -> ExtensionRegistryBuilder {` und die erste Zeile streichen. |

## Verifizierte Punkte (Auszug — alle mit Quelle gelesen)

**Pfade/Sichtbarkeit** — alle vorhanden und `pub`:

- `harw_home::config_layers_report_at` (`harw-home/src/paths.rs:433`), re-exportiert (`harw-home/src/lib.rs:53-56`). **Keine Namenskollision**: der Re-Export-Block enthält den Namen genau einmal; `config_layers_report_in` bleibt korrekt `pub(crate)`. `config_layers_report` behält Signatur und delegiert (der `Err`-Zweig für `current_dir()` ist nötig, da `…_at` ein `&Path` und kein `Option` nimmt — korrekt gelöst).
- `harw_config::discover_config_with_restricted(&[PathBuf], Option<&Path>) -> ConfigResult<ResolvedConfig>` (`discovery.rs:435`, re-exportiert `lib.rs:23-26`); `ResolvedConfig::validate(&self) -> ConfigResult<()>` (`discovery.rs:54`). Aufruf in `config.rs:99` passt in Arität und Typen (`untrusted_repo.as_deref()` → `Option<&Path>`).
- `harw_config::{HarnessConfig, PolicySection, ProviderToml, OriginAllowlistToml, SecretRef}` alle re-exportiert; `LayerReport { layers, untrusted_repo, status }` destrukturiert vollständig (`paths.rs:355-366`).
- `harw_provider_http::build_provider_with_home(config: &ResolvedConfig, home: &Path, resolver: Option<&dyn SecretResolver>) -> HttpProviderResult<Box<dyn ModelProvider>>` (`lib.rs:175-181`) — Arität 3 ✔. `SecretResolver` ist `pub trait` in `lib.rs:68` ✔. `Ok(Arc::from(provider))` ist gültig: `impl<T: ?Sized> From<Box<T>> for Arc<T>` liefert direkt `Arc<dyn ModelProvider>`.
- `harw_core::{EchoModelProvider, ModelProvider, ModelRequest, ModelResponse, ConversationHistory, ConfigApprovalPolicy, ManagedAgentSpawner, StateStore, InMemoryStateStore, SessionManager, ChildLimits}` — alle in `harw-core/src/lib.rs` re-exportiert. `EchoModelProvider::new(impl Into<String>)` ✔, `ModelRequest::new/4` ✔, `ConversationHistory::new()` ✔, `ModelResponse::{message, is_final}` ✔.
- `ConfigApprovalPolicy::new(impl IntoIterator<Item = String>)` (`harw-core/src/policy.rs:28`) — Aufruf mit `Vec<String>` ✔; `impl ApprovalHandler for ConfigApprovalPolicy` (`policy.rs:40`) ✔, überschreibt `kind`/`label` **nicht** → die Begründung für `CONFIG_POLICY_LABEL` in `approval.rs:47-53` stimmt, und `assert_eq!(handlers[1].label(), "unnamed")` (Z. 557) ist korrekt.
- `DefaultApprovalPolicy::new(ApprovalModeCell)` (`harw-registry-defaults/src/lib.rs:196`) — by value ✔; `impl ApprovalHandler` ohne `kind`/`label`-Override ✔. `AUTO_APPROVED_TOOLS` enthält `fs.read`, nicht `shell.exec` → die Erwartungen in `config_policy_restricts_a_tool_…`, `a_child_never_inherits_full_access` und `install_registers_the_handlers_in_order` sind inhaltlich richtig.
- `ExtensionRegistryBuilder` ist `#[derive(Default)]` (`registry.rs:299`), `approval_handler(mut self, Arc<dyn ApprovalHandler>) -> Self` (`:495`), `build(self) -> ExtensionRegistry` (`:507`), `ExtensionRegistry::approval_handlers(&self) -> &[Arc<dyn ApprovalHandler>]` (`:163`) — Indexierung/`len()` in den Tests ✔.
- `ApprovalHandlerKind` liegt nur in `harw_extension_api::contributors` (**nicht** im Wurzel-Re-Export) — `approval.rs:43` nutzt korrekt den Modulpfad. Derives `Clone, Copy, Debug, PartialEq, Eq, Hash` ⇒ `assert_eq!` auf `Vec<(&str, ApprovalHandlerKind)>` übersetzt ✔.
- `ApprovalModeCell` liegt in `approval_mode` (Wurzel exportiert nur `ApprovalMode`) — `approval.rs:42`/`services.rs:58` nutzen den Modulpfad ✔. `new/get/set/detached` alle vorhanden (`approval_mode.rs:170-215`).
- `harw_operations::{ServiceMap, SharedSessionController}` re-exportiert (`lib.rs:97,104-107`). **`ServiceMap::insert<S: Any + Send + Sync>`** (`context.rs:103`) — `insert_service<S: Any + Send + Sync>` (`services.rs:270`) hat exakt dasselbe Bound (`Any` impliziert `'static`) ✔; `get<S>() -> Option<&S>` ✔. `SharedSessionController = Arc<dyn SessionController>` (`session_control.rs:278`), `SessionController: Send + Sync` (`:183`) ⇒ `Any + Send + Sync` erfüllt, und `Some(Arc::new(NullSessionController::new()))` unsized-coerced am Struct-Feld ✔.
- `OperationRegistry::{new, register(Arc<dyn Operation>), iter() -> impl Iterator<Item = &Arc<dyn Operation>>, len}` ✔ — `Arc::clone(operation)` liefert genau den Parametertyp; `Operation` muss nicht importiert werden.
- `register_plan_services(&mut ServiceMap, Arc<dyn PlanStore>, Arc<dyn GoalStore>, Arc<FindingStore>, PlanToolConfig)` (`harw-plan-bridge/src/context_ext.rs:195-206`) — Signatur und Reihenfolge stimmen mit `services.rs:585-592` überein, und die vier in `names` protokollierten Typen sind exakt die vier `services.insert`-Aufrufe der Bridge ✔.
- `harw_memory::Memory` (`store.rs:25`, `: Send + Sync`): `hot`, `recall<'a>(&self, RecallQuery<'a>)`, `record(Signal)`, `maintain`, `stats` — `StubMemory` implementiert alle fünf **signaturgleich**, inkl. der Lebenszeit auf `recall` ✔. `MaintenanceReport`/`Stats` sind `Default` ✔.
- `JobStore::new(&Path)` ✔, `FindingStore::new(impl Into<PathBuf>)` ✔, `PlanToolConfig::enabled_defaults()` + `Clone` ✔, `InMemoryPlanStore::new()`/`InMemoryGoalStore::new()` aus `harw_plan` re-exportiert (`lib.rs:86,98`) ✔, `GoalStore`/`PlanStore` beide `: Send + Sync` ✔.
- `Principal::trusted_ingress(PrincipalKind, impl Into<String>, IngressSurface, PermissionTier)` ✔; `Principal` ist `Clone, Debug, PartialEq, Eq` ⇒ `assert_eq!(principal, services.principal())` (`&Principal` vs. `&Principal`) übersetzt ✔.
- `RuntimeSpec`-Literale in beiden Testmodulen nennen alle 7 Felder (`spec.rs:228-243`) ✔; `ProviderToml`-Literal in `model.rs:199-210` nennt alle 10 Felder (`provider_toml.rs:9-34`) ✔ (kein `#[non_exhaustive]`).
- `RuntimeError::{Config,Trust,Provider}{ detail }` und `RuntimeResult` stammen aus `HarwError` (`error.rs`) ✔; alle `?`/`map_err`-Stellen erzeugen die Variante direkt, kein `From`-Bedarf.

**Send+Sync / Object-Safety** — `ModelProvider: Send + Sync` (`harw-core/src/model.rs:399`), `Memory`, `StateStore`, `PlanStore`, `GoalStore`, `SessionController`, `ApprovalHandler` alle `: Send + Sync` und objektsicher (nur `&self`-Methoden, keine Generics, keine `Self`-Rückgaben). Jedes `Arc<dyn …>` in `ServiceMap` erfüllt damit `Any + Send + Sync`.

**`Waker::noop`** (`approval.rs:303-309`): `Waker::noop()` liefert `&'static Waker`, `Context::from_waker` nimmt `&Waker` — Aufruf korrekt, stabil seit 1.85.0 (= MSRV), passt zu `#![forbid(unsafe_code)]`. `future.as_mut().poll(&mut cx)` auf `Pin<Box<dyn Future + Send>>` braucht `Future` im Scope: Edition **2024** hat `Future`/`IntoFuture` im Prelude → kein fehlender Import. `mut future` ist deklariert ✔.

**Manuelles `Debug` für `ModelSource`** (`model.rs:67-80`): erschöpfend über alle drei Varianten, `debug_struct(...).finish()` korrekt abgeschlossen; `format!("{:?}", ModelSource::Configured) == "Configured"` und `"Override(<dyn ModelProvider>)"` stimmen mit `f.write_str` überein; der Echo-Text wird nie geschrieben (nur `reply_len`) — der Test `debug_hides_the_echo_text_and_labels_every_source` prüft genau das Behauptete ✔.

**Tests / Fixtures**:
- `config.rs`-Tests wechseln **kein** cwd und setzen **keine** Env-Variable mehr (Z2b-F0 umgesetzt); `TempDir` kanonisiert, damit der interne Identitätsvergleich in `config_layers_report_in` (`paths.rs:455-462`) dieselben Pfade sieht — die Assertions gegen `cwd.join(".harw")` (nicht kanonisiert gespeichert, `paths.rs:465/468`) sind dadurch korrekt.
- `broken_trust_store_is_a_trust_error` setzt `0600` **nach** dem Schreiben, sonst käme `HomeError::Io` statt `TrustStore` — richtig gelöst und im Kommentar begründet.
- `harw-home/src/paths.rs:728-754` (neuer Test): nutzt die vorhandenen Helfer `TempDir(PathBuf)` (`.0`) und `repo_with_harw`; `TrustStatus` kommt über `use super::*`; kein `set_current_dir`, kein `set_var` ✔.
- `services.rs`-Tests: `tokio::sync::mpsc::unbounded_channel` braucht Feature `sync` — im Manifest gesetzt ✔; `model.rs` braucht `tokio::runtime::Builder` → Feature `rt` ✔. `SessionManager::new` erwartet genau `tokio::sync::mpsc::UnboundedSender` (`session_manager.rs:10,19`) — identische tokio-Instanz (eine Version im Lock) ✔.
- Doctests in `services.rs:79-89,110-113,121-125,148-152,170-174` benutzen nur `pub`-API (`harw_runtime::services::ServiceSurface`), `ALL`/`as_str`/`allows_spawner`/`allows_session_controller` sind `pub const fn` bzw. `pub const` ✔ — kompilierbar und laufbar.

**Querwirkungen**: `harw-runtime` steht in `harw-tui/Cargo.toml:43` und `harw-cli/Cargo.toml:88`, aber `grep -rn harw_runtime harw-tui/src harw-cli/src` = **0 Treffer** — keine externen Aufrufer, keine Breaking-Change-Gefahr. `harw-home`s neue `pub fn` kollidiert mit nichts (einziger Name im Re-Export-Block, kein gleichnamiges Item in `scaffold`/`trust`). `Cargo.lock` ist konsistent zum geänderten Manifest (`harw-lens-types`, `harw-secrets`, `secrecy 0.10.3`, `tempfile` sind eingetragen) → `--locked`-Builds brechen nicht. Keine Dependency-Zyklen (`harw-secrets` → nur `harw-fsutil`/`harw-macros`/`harw-observe`; `harw-lens-types` → nur `harw-types`/`harw-macros`).

**clippy (`-D warnings`)**: `[workspace.lints.rust]` enthält nur `unsafe_code = "forbid"`, es sind **keine** clippy-Lint-Gruppen (pedantic/nursery) workspace-weit aktiviert. Gegen die Default-Gruppen (`correctness`/`style`/`complexity`/`perf`/`suspicious`) habe ich die vier Dateien durchgesehen: `#[must_use]` ist überall dort gesetzt, wo ein Wert zurückgegeben wird; keine `unwrap` außerhalb von Tests; keine `&Vec`/`&String`-Parameter; keine `needless_return`/`redundant_clone`-Muster; `type Make = fn(String) -> RuntimeError` vermeidet `type_complexity`. **Kein Default-clippy-Verstoß gefunden.**

## Fix-Aufgaben nach Datei

**`harw-runtime/Cargo.toml`** (Zuständigkeit: Orchestrator, nicht die Coding-Agents)
1. R1-01 — `harw-secrets` + `secrecy` entfernen **oder** in W2c benutzen.
2. R1-02 — `tempfile`-Dev-Dep entfernen **oder** die Hand-`TempDir`s ersetzen.

**`harw-runtime/src/config.rs`**
3. R1-03 — Kommentar Z. 138-139 korrigieren.
4. R1-04 — `map_err`-Block Z. 99-105 rustfmt-konform einrücken.
5. R1-06 — `map_home_error` erschöpfend matchen (Catch-all entfernen).

**`harw-runtime/src/approval.rs`**
6. R1-07 — `mut builder` in die Signatur von `install` ziehen.

**`harw-runtime/src/services.rs`**
7. R1-05 — 8 Trennkommentare auf ≤ 100 Spalten kürzen.

**`harw-runtime/src/model.rs`** — keine Fix-Aufgabe.
**`harw-home/src/{paths.rs, lib.rs}`** — keine Fix-Aufgabe.

## „Kompiliert voraussichtlich“ je Crate

| Crate | `cargo build` | `cargo test` (inkl. Testmodule) | `cargo doc`/Doctests | `cargo clippy -D warnings` |
|---|---|---|---|---|
| `harw-home` | **ja** | **ja** | ja | ja |
| `harw-runtime` (Umfang: `config.rs`, `model.rs`, `approval.rs`, `services.rs`, `lib.rs`, Manifest) | **ja** | **ja** | ja | ja |

Einschränkung: `budget.rs`, `ceiling.rs`, `sandbox.rs`, `spec.rs`, `trace.rs`
gehörten **nicht** zu meinem Umfang; `lib.rs` deklariert sie alle und alle
Dateien existieren, aber ein Übersetzungsfehler dort würde `harw-runtime` als
Ganzes scheitern lassen. Ein zweiter Review (oder der erste echte
`cargo check`) muss sie abdecken. `cargo fmt --check` habe ich nicht
simuliert, sehe aber mit R1-04 mindestens eine Stelle, die rustfmt umbrechen
würde.
