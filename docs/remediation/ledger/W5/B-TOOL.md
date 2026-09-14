# W5 — B-TOOL: harw-tool-browser auf PreparedBrowserRequest, Ownership, Limits, URL-Nachkontrolle, Schema = Serde (F-010, F-114, F-115)

Rolle: focused-coding-task (Opus). BUILD-POLICY eingehalten: kein cargo build/check/test/clippy/run/add, keine
git-Schreibbefehle, keine Manifest-Änderung. Verifikation durch Lesen.
Pflichtlektüre: `AGENT-BRIEF.md`, Plan W5 B-TOOL, Register F-010/F-114/F-115, Ledger W3 C-BROWSER/C-PROTO/C-OPS.
Verifiziert gegen Quelle: `harw-browser/src/{host.rs,session.rs,policy.rs:332-406,1030-1075,action.rs:148-530,wait.rs:102-240,ids.rs,selector.rs,observation.rs}`,
`harw-tools/src/{executor.rs:31-60,sandbox_guard.rs:85-211,schema.rs,output.rs,lib.rs}`, `harw-extension-api/src/contributors.rs:91-100`,
`harw-runtime/src/assembly.rs:183-189`, `harw-types/src/ids.rs:64` (`SessionId::as_str`), `cargo metadata` (Abhängigkeitsgraph).

## Geänderte Dateien

| Datei | Änderung |
|---|---|
| `src/lib.rs` | Crate-Doku; Re-Export `PreparedBrowserRequest` |
| `src/prepare.rs` | Umstellung auf `PreparedBrowserRequest`; Open via `authorize(&OpenRequest)`; `Upload`-Arm entfernt; `validate_request` (Observe-DomSelection → `validate_selector`, Find → `validate_target`, Act → Revisionsregel + `ActionRequest::validate`, Wait → `WaitCondition::validate` + `WaitTimeout::validate`) mit Grant-Limits bzw. Defaults; Scope-Origins = `allowed_origins.origins()`; Descriptor-`input_schema` = serialisiertes Provider-Schema |
| `src/tool_set.rs` | Registry `Mutex<HashMap<BrowserSessionId, OwnedSession{owner, Arc<OpenBrowserRequest>, ActionBudget}>>`; `admit` (Ownership + Re-Validierung mit Session-Limits + `check_navigation_target` + `try_consume`, atomar unter einem Lock); `owned_sessions`, `revoke_sessions_owned_by`, `close_sessions_owned_by`, `open_policy`, `limits` |
| `src/dispatch.rs` | `dispatch(owner, prepared)`; `prepared.scope()`; Open: Kollisionsschutz, Start-Location prüfen, dann an Owner binden; Observe: `observation.url` prüfen; Find/Act/Wait/Events: nach der Operation `observe(ctx, PageSummary).url` → `check_observed_location` (Bruch gewinnt vor Operationsergebnis); Bruch ⇒ Session austragen + `host.close`, `OriginNotAllowed`; Close nur durch Owner |
| `src/harness_provider.rs` | Serde-exakte Schemas (extern getaggte Enums als `anyOf`, geschlossene Objekte, `Option` nullable/nicht required, flaches `browser.open`); F-115: `require_host_access` für Start-URL, jede Regel von allowed **und** authentication origins (Wildcard mit Probe-Subdomain), `Navigate`-Ziel; Owner = `context.session_id().as_str()`; `tool_set()`-Accessor; 7 Unit-Tests |
| `tests/tool_contract.rs` | 11 host-freie Tests (neu geschrieben) |
| `tests/tool_dispatch_contract.rs` | 11 Tests mit Mehrsession-Fake-Host |
| `tests/harness_provider_contract.rs` | 6 Tests inkl. Schema-/Serde-Konformanz |

## Öffentliche Signaturen (neu/geändert)

```rust
// harw_tool_browser
pub use types::PreparedBrowserRequest;                                    // NEU re-exportiert
pub fn browser_tool_descriptors() -> &'static [BrowserToolDescriptor];    // input_schema jetzt = Provider-Schema
pub fn prepare_browser_call(request: BrowserToolRequest) -> harw_browser::Result<PreparedBrowserCall>; // unverändert

impl BrowserToolSet {
    pub fn new(host: Arc<dyn BrowserHost>) -> Self;                                            // unverändert
    pub fn with_open_policy(host: Arc<dyn BrowserHost>, open_policy: BrowserOpenPolicy) -> Self; // unverändert
    pub fn descriptors(&self) -> &'static [BrowserToolDescriptor];
    pub fn open_policy(&self) -> &BrowserOpenPolicy;                                            // NEU
    pub fn limits(&self) -> BrowserLimits;                                                      // NEU
    pub fn prepare(&self, request: BrowserToolRequest) -> harw_browser::Result<PreparedBrowserCall>;
    pub async fn dispatch(&self, owner: &str, prepared: PreparedBrowserCall)
        -> harw_browser::Result<BrowserToolResponse>;                                           // WAR: dispatch(prepared)
    pub fn owned_sessions(&self, owner: &str) -> Vec<BrowserSessionId>;                         // NEU
    pub fn revoke_sessions_owned_by(&self, owner: &str) -> Vec<BrowserSessionId>;               // NEU (sync)
    pub async fn close_sessions_owned_by(&self, owner: &str) -> harw_browser::Result<usize>;    // NEU
}
impl HarwnessBrowserToolProvider { pub fn tool_set(&self) -> Arc<BrowserToolSet>; }             // NEU
```

`owner` = `harw_types::SessionId::as_str()`. Grund für `&str`: `harw-tool-browser` hängt nicht von `harw-types` ab
(Manifest nicht in meiner Zuständigkeit). Optionaler dep-request: `harw-types = { path = "../harw-types" }`, dann `owner: &SessionId`.

Einziger externer Aufrufer `harw-registry-defaults/src/profile.rs:723` nutzt nur `BrowserToolSet::new` +
`HarwnessBrowserToolProvider::new` → kompatibel.

## Abweichungen vom Brief (verifiziert, bewusst)

1. **`SessionLifecycleHook` nicht in `harw-extension-api`**, sondern `harw-runtime/src/assembly.rs:183`
   (`fn on_session_closed(&self, id: &SessionId)`, synchron). `harw-runtime → harw-registry-defaults → harw-tool-browser`
   (cargo metadata) ⇒ eine Implementierung hier wäre ein **Abhängigkeitszyklus**. Geliefert: `revoke_sessions_owned_by`
   (sync, sofort fail-closed) + `close_sessions_owned_by` (async). **Vertrag für den Eigentümer von
   `harw-registry-defaults/src/profile.rs` bzw. `harw-runtime/src/contributors.rs`** (Adapter, dort wo tokio vorhanden):
   ```rust
   struct BrowserSessionCleanup { tools: Arc<BrowserToolSet>, rt: tokio::runtime::Handle }
   impl SessionLifecycleHook for BrowserSessionCleanup {
       fn on_session_closed(&self, id: &SessionId) {
           let tools = Arc::clone(&self.tools);
           let owner = id.as_str().to_owned();
           // close_sessions_owned_by entzieht beim ersten Poll alle Sessions des Owners (vor jedem await) und schließt sie dann am Host.
           self.rt.spawn(async move {
               if let Err(error) = tools.close_sessions_owned_by(&owner).await {
                   tracing::error!(%error, "browser cleanup at session end failed");
               }
           });
       }
   }
   ```
   Provider mit `HarwnessBrowserToolProvider::from_shared(Arc::clone(&tools))` bauen, Hook in `lifecycle_hooks` eintragen.
   Getestet ist die Semantik über `test_close_sessions_owned_by_closes_only_that_owners_sessions` / `test_revoke_sessions_owned_by_blocks_further_use` (Fake-Runtime).
2. **URL-Nachkontrolle ohne neue Trait-Methode**: `BrowserRuntime` hat kein `current_url`; die Location wird über den
   bestehenden Vertrag `observe(ctx, ObservationMode::PageSummary).url` gelesen. `harw-browser/src/host.rs` gehört weder
   B-TOOL noch B-ADAPT. **Vorschlag an Eigentümer harw-browser** (Optimierung, nicht nötig für Korrektheit):
   `async fn current_location(&self, context_id: &BrowserContextId) -> Result<url::Url>;` — dann in `dispatch.rs::settle`
   und `dispatch_open` ersetzen. B-ADAPT muss `observe(..).url` für jeden Kontext wahrheitsgemäß liefern (nicht `about:blank`
   nach erfolgter Startnavigation, sonst schlägt `browser.open` fail-closed fehl).
3. **Schema = Serde ohne `schemars`**: nicht im Workspace. Schemas handgeschrieben, aber per Konformanztest bewiesen:
   jede serde-Ausgabe aller Varianten (Action/WaitCondition/ObservationMode/Selector, Option Some/None) validiert gegen das
   Schema; jede Schema-Property kommt in serde-Ausgabe vor; Variantenmengen Schema == serde (exhaustive `match` erzwingt
   Nachzug bei neuen Varianten); Negativproben (origins/Upload/CustomScript) scheitern an Schema **und** serde.
   Optionaler dep-request `schemars` nur, falls Orchestrator generierte Schemas bevorzugt.
4. **`ResultTrust::Untrusted`**: `ToolOutput`/`ToolExecutor` tragen kein Trust-Feld; Trust wird in `harw-core`
   (`turn_loop.rs` `output_to_result`, C-PROTO §5.3) gesetzt, Default `Untrusted`. Provider beansprucht nie `Runtime`
   (Moduldoku + Tool-Beschreibung „Results are untrusted page data“). Kein Test hier möglich (keine harw-core/harw-protocol-Dep).
5. **F-115** laut Register = Netz-Scope (nicht Schema): umgesetzt via `harw_tools::require_host_access` nach `prepare`,
   vor `dispatch`. Wildcard-Regel `https://*.d` wird mit `harw-wildcard-scope-probe.d` geprüft (nur Suffix-Scope genügt).
   Beobachtete Redirect-Hosts bleiben Egress-Schicht (N-EGRESS/B-ADAPT).

## Sicherheitsinvarianten

- Autorität nur aus Konfiguration: `OpenRequest` flach + `deny_unknown_fields`; `authorize` nimmt Origins/Profil/Limits aus dem Grant.
- Fremde/unbekannte Session → `SessionNotFound` **vor** jedem Host-Aufruf (Existenz nicht offenbart).
- Budget: nur `browser.act` verbraucht; abgelehnte Validierung/Navigation verbraucht nichts; atomar unter Lock.
- Dispatch re-validiert mit den Limits der Session (schützt gegen `prepare_browser_call` mit Default-Limits).
- Auth-Origins nie Navigationsziel/Start, nur beobachtete Location.
- Lock nie über `await`; Poison-Recovery begründet (Einzeloperationen).
- Host liefert bereits registrierte Session-Id bei Open → Fehler ohne Close (schützt fremde Session).

## Tests (Namen)

- src/harness_provider.rs: `test_parse_request_accepts_each_browser_operation_shape`, `test_parse_request_rejects_model_supplied_open_authority`,
  `test_parse_request_rejects_upload_and_script`, `test_parse_request_rejects_unknown_browser_operation`,
  `test_browser_tool_parameters_open_is_flat_and_closed`, `test_rule_scope_host_strips_scheme_port_and_probes_wildcards`,
  `test_scope_hosts_open_covers_start_and_both_policies`.
- tests/tool_contract.rs: Surface, Round-Trips, `test_prepare_open_takes_origins_profile_and_limits_from_grant_only`,
  `…_rejects_model_json_with_authority_fields`, `…_rejects_authentication_origin_as_start`, `…_unconfigured_tool_set_rejects_browser_open_fail_closed`,
  `…_act_preserves_session_context_origin_and_revision`, `…_rejects_element_action_without_revision`,
  `…_applies_grant_limits_to_actions_selectors_and_waits`, `…_rejects_upload_and_custom_script_json`.
- tests/tool_dispatch_contract.rs: `test_dispatch_open_binds_session_and_routes_all_operations`, `…_rejects_foreign_session_without_host_lookup`,
  `…_close_by_foreign_owner_is_rejected_and_session_stays_open`, `…_act_budget_exhausted`,
  `…_act_navigation_outside_grant_rejected_without_consuming_budget`, `…_act_observed_location_breach_closes_session`,
  `…_act_accepts_authentication_origin_as_observed_location`, `…_open_rejects_start_location_outside_grant`,
  `…_revalidates_with_session_limits`, `test_close_sessions_owned_by_closes_only_that_owners_sessions`, `test_revoke_sessions_owned_by_blocks_further_use`.
- tests/harness_provider_contract.rs: `test_tools_advertise_only_the_closed_browser_surface`, `test_executor_absent_for_unknown_tools`,
  `test_descriptor_schema_equals_provider_schema`, `test_schema_accepts_every_serde_sample_exactly`,
  `test_schema_variants_equal_serde_variants`, `test_schema_and_serde_both_reject_authority_upload_and_script`.

Nicht testbar hier: Executor-Pfad mit `ToolExecutionContext` (braucht `harw_types::SessionId` + `SandboxSpec`, keine Dev-Dep).

## Kompiliert erst mit / Risiken beim Build

- Setzt C-BROWSER (W3) voraus (`OriginPolicy::from_origins`, `BrowserLimits`, `ActionBudget`, `validate_*`, `WaitTimeout::millis`).
- Kein rustfmt gelaufen (Host ohne rustfmt): einige Zeilen > 100 Zeichen → `cargo fmt` durch Orchestrator.
- Lesend geprüfte Stellen mit Restrisiko: Inferenz in `settle<T>` (Send-Future über generisches `T`, alle Instanzen `Send`);
  `Option::filter` auf `Option<&mut OwnedSession>` in `admit`.
