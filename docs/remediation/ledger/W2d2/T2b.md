# W2d-2 — Agent T2b — `harw-tui/src/app.rs` Testmodul (`mod tests`) (D5)

Owned: `harw-tui/src/app.rs` — **nur** `mod tests` (Zeilen 3114–4405 im neuen Stand,
3114–4961 im alten Stand vor dieser Änderung). Tabu (null Diff): Produktionscode
(1–3113) und `mod approval_arming_tests` (4410–4558 neu, vormals 4966–5114) samt
der Trennkommentarzeile davor (W1-08, Zeile 4407 neu / 4963 alt).

BUILD-POLICY eingehalten: kein `cargo build/check/test/clippy/run`, kein
`make`/`rustc`/`rust-analyzer`, keine git-Schreibbefehle. Ausgeführt:
`cargo metadata --offline --no-deps --format-version 1` → Exit 0. Verifikation
ausschließlich durch Lesen (Signaturen, Imports, Struct-Felder gegen die
Read-list gegengeprüft).

## 0. Tabu-Nachweis (SHA-256, vor/nach Änderung)

Vor Beginn per `sed -n '<range>p' app.rs | sha256sum` gesichert, nach der
Änderung an den identischen Zeilenbereichen erneut geprüft (Bereiche sind
Byte-für-Byte unverändert, da nur `mod tests` ersetzt wurde):

| Block | Zeilen (alt = neu, unverändert) | SHA-256 | Ergebnis |
|---|---|---|---|
| Produktionscode (1–3113) | 1–3113 | `fe8fab56839a281237a67b15a10b48d2c53c3335845128215b982ab5cb96d042` | identisch |
| `approval_arming_tests` + Trennkommentar (4962–5114 alt) | 4962–5114 (alt) | `afcbb7568c51524709166f83c09e064a8164ae704c17484228dc34947df3d724` | identisch (jetzt bei 4406–4558) |

Owned-Block vor der Änderung (`mod tests`, 3114–4961 alt):
`0afed47f1a7af7f48825d16eb7a3d4d457109274931f59ea99479f16924aed41` (nur zur
Nachvollziehbarkeit — dieser Block durfte und musste sich ändern).

Datei: 5114 → 4558 Zeilen.

## 1. Ausgangslage

T2a (`docs/remediation/ledger/W2d2/T2a.md` §4) lieferte die vollständige Liste
der 57 Tests + 11 Hilfsfunktionen in `mod tests`, die gelöschte Montage-Items
referenzierten (`build_tui_agent_session`, `trusted_tui_spawn_context`,
`assemble_tui_registry`, `registry_with_approval_handler`,
`build_tui_managed_spawner`, `TuiChildRegistryFactory`, `tui_child_limits`,
`TuiModelToolServices`/`tui_model_tool_context`/`add_tui_model_tool_provider`,
`LOCAL_TUI_OPERATION_PERMISSION`, `ensure_tui_context_roots_align`,
`session_id_for_canonical_project_root`/`selected_session_id`,
`registered_tool_names`/`test_spawn_input`/`test_child_registry_factory`,
`CommandServices` als Struct-Argument). Jeder dieser 57 Tests wurde einzeln
klassifiziert: **löschen** (prüft ausschließlich das Verhalten eines gelöschten
Montage-Helfers, Ersatzabdeckung jetzt in harw-runtime/harw-registry-defaults/
harw-core) oder **migrieren** (prüft echtes TUI-Verhalten — Popup, Tastatur,
Freigabefluss, `/mode`, Zellen-Rendering — und bekommt nur eine neue
Session-/Registry-Konstruktion).

Ergebnis: 57 → 36 Tests, 11 → 8 Hilfsfunktionen (4 gelöscht, 1 neu:
`test_spawn_context`).

## 2. Gelöschte Tests (21) mit Ersatzabdeckung

| Gelöschter Test | Referenziertes gelöschtes Item | Ersatzabdeckung |
|---|---|---|
| `model_operation_provider_is_appended_to_tui_registry` | `add_tui_model_tool_provider`, `TuiModelToolServices` | `harw-runtime/tests/rights_matrix.rs::the_operation_surface_reaches_the_assembled_run`, `::only_the_full_surface_offers_operations_to_the_model` |
| `model_tool_context_binds_execution_sandbox_and_trusted_services` | `tui_model_tool_context`, `TuiModelToolServices` | dieselben zwei rights_matrix-Tests (Modell-Tool-Fläche entsteht jetzt vollständig in `harw-runtime/src/assembly.rs::install_operation_model_tools`) |
| `model_tool_context_omits_spawner_service_when_no_spawner_is_configured` | `tui_model_tool_context`, `TuiModelToolServices` | keine 1:1-Entsprechung; die Optionalität ist strukturell erzwungen durch `harw-runtime/src/assembly.rs::build_spawner` (`SpawnerPolicy::None => Ok((None, vec![]))`) und mittelbar durch `rights_matrix.rs::spawner_roles_are_exactly_the_builtin_roles` geprüft |
| `model_tool_provider_registers_without_a_managed_spawner` | `add_tui_model_tool_provider`, `TuiModelToolServices` | wie oben — Modell-Tool-Fläche unabhängig vom Spawner ist über `rights_matrix.rs::the_operation_surface_reaches_the_assembled_run` abgedeckt |
| `managed_spawner_admits_configured_worker_from_trusted_external_root` | `build_tui_managed_spawner`, `trusted_tui_spawn_context` | `harw-core/tests/child_controller.rs::managed_spawner_creates_a_governed_child_and_tracks_its_lifecycle` |
| `managed_spawner_refuses_an_unconfigured_child_role_set` | `build_tui_managed_spawner`, `trusted_tui_spawn_context` | `harw-runtime/tests/rights_matrix.rs::the_child_factory_refuses_an_unknown_role` |
| `trusted_spawn_context_preserves_runtime_sandbox_authority` | `trusted_tui_spawn_context` | `harw-runtime/tests/rights_matrix.rs::only_spawning_entries_are_root_orchestrators` (organizational_role) + `harw-runtime/src/spec.rs::rights_snapshot_carries_principal_actor` (approval_actor `"local-tui"`) + `harw-runtime/src/sandbox.rs::root_sandbox_takes_its_permissions_from_the_entry_profile`/`::root_sandbox_binds_the_given_project_root` (Sandbox-Autorität) |
| `trusted_spawn_context_carries_a_freshly_generated_root_trace` | `trusted_tui_spawn_context` | `harw-runtime/src/trace.rs::every_entry_gets_a_well_formed_root_span` (Hex-Form, kein Parent-Span) |
| `trusted_spawn_context_root_traces_differ_across_two_calls` | `trusted_tui_spawn_context` | `harw-runtime/src/trace.rs::two_roots_never_share_a_trace_id` |
| `session_id_mapper_is_stable_and_opaque` | `session_id_for_canonical_project_root` | keine — E5 (CONTRACTS-W2d2 §4) ersetzt die deterministische Projekt-Ableitung durch `SessionId::new()`; die Funktion hat keine Entsprechung mehr |
| `session_id_mapper_separates_project_roots` | `session_id_for_canonical_project_root` | keine (dito) |
| `explicit_session_id_overrides_project_derived_id` | `selected_session_id` | keine (dito); `/resume <id>` löst jetzt über `TuiAssemblyFactory`/`ResumeSessionSelector` auf (T1) |
| `ordinary_starts_create_distinct_sessions_for_the_same_project` | `selected_session_id`, `session_id_for_canonical_project_root` | keine (dito) |
| `tui_registry_discovery_failure_is_typed_and_fail_closed` | `assemble_tui_registry` | `harw-runtime/src/error.rs::display_names_phase_and_detail` (`RuntimeError::Discovery` ist typisiert, formatiert fail-closed); die Projekterkennung selbst läuft jetzt in der Montage (`harw-runtime/src/assembly.rs:671-675`, `discover_project` → `RuntimeError::Discovery`) |
| `tui_context_root_alignment_accepts_the_shared_project_root` | `ensure_tui_context_roots_align` | keine dedizierte Entsprechung nötig — die Montage kennt nur noch eine Projekt-Wurzel (`assembly.project()`); ein Auseinanderlaufen von Prompt-/Werkzeug-Wurzel ist architektonisch ausgeschlossen |
| `tui_context_root_alignment_rejects_mismatched_prompt_and_tool_roots` | `ensure_tui_context_roots_align` | keine (dito) |
| `explorer_child_registry_has_no_write_and_no_shell_tool` | `TuiChildRegistryFactory`, `assemble_tui_registry` (über `test_child_registry_factory`) | `harw-registry-defaults/src/profile.rs::test_read_only_explore_exposes_exact_tool_set`, `::test_read_only_profiles_never_expose_write_or_shell_tools` |
| `child_registry_profiles_come_from_the_shared_role_mapping` | `TuiChildRegistryFactory` | `harw-registry-defaults/src/profile.rs::test_profile_for_role_maps_every_builtin_role`, `::test_profile_for_unknown_role_is_none_not_full` |
| `child_limits_are_derived_monotonically_from_the_runtime_profile` | `tui_child_limits` | `harw-runtime/src/budget.rs::child_limits_never_exceed_the_conservative_grant` (identische Invariante gegen `child_limits(config, model_id)`) |
| `tui_approval_handler_is_appended_behind_the_existing_policy` | `registry_with_approval_handler` | `harw-tui/src/runtime_root.rs::test_build_root_runtime_mounts_responder_in_chain` (T1) — prüft dieselbe Reihenfolge-Invariante an der echten Produktionsstelle (`RuntimeAssembly::new_root_session`) |
| `appending_the_approval_handler_preserves_every_other_contribution` | `registry_with_approval_handler` | `harw-extension-api/src/registry.rs::into_builder_roundtrip_preserves_providers_and_namespaces` + `harw-tui/src/runtime_root.rs::test_build_root_runtime_mounts_responder_in_chain` |

Zusätzlich gelöscht (nur noch von den obigen Tests benutzte Hilfsfunktionen,
selbst ohne verbleibenden Aufrufer): `test_managed_spawner`,
`test_child_registry_factory`, `test_spawn_input`, `registered_tool_names`.

## 3. Migrierte Tests (7) — echtes TUI-Verhalten, neue Konstruktion

- `selected_executable_policy_limits_tui_session_tools_and_retains_snapshot`,
  `absent_executable_policy_preserves_full_tui_session_visibility`: statt
  `build_tui_agent_session(..)` jetzt
  `AgentSession::new_with_id(SessionId::new(), AgentRole::Assistant, None, ExtensionRegistry::builder().build(), event_tx)
  .with_spawn_context(test_spawn_context(&sandbox)).with_turn_event_sink(turn_event_tx)[.with_executable_agent_ir(&policy)]`
  (Signaturen gegen `harw-core/src/session.rs:340,356,445,700` verifiziert).
  Geprüftes Verhalten (`with_executable_agent_ir` schneidet die Werkzeugfläche
  deny-by-default) unverändert.
- `local_tui_permissions_are_accessible_but_maintainer_commands_are_blocked`:
  `LOCAL_TUI_OPERATION_PERMISSION` → `harw_operations::PermissionTier::Operator`
  (E4, CONTRACTS-W2d2 §4: lokaler Principal-Tier ist Operator — derselbe Wert,
  den `runtime_commands::caller_tier(rt.principal())` in Produktion liefert);
  `&CommandServices { .. }` → Closure `|| build_services(&adapters, None, None, &controller, None)`
  (CE, CONTRACTS-W2d2 §1.2: `execute_command_as<F: FnOnce() -> ServiceMap>`).
- `approval_test_session` (Helfer für `drive_turn_answering` →
  `approved_pause_completes_the_turn_and_runs_the_tool`,
  `rejected_pause_completes_the_turn_without_running_the_tool`): statt
  `ExtensionRegistry::builder()...build()` + `registry_with_approval_handler(base, registered)`
  jetzt ein Builder-Aufruf, der `DefaultApprovalPolicy::new(ApprovalModeCell::default())`
  und den TUI-Handler in genau dieser Reihenfolge direkt registriert (Brief-Vorgabe
  „Approval-Handler direkt am Registry-Builder registrieren"); `DefaultApprovalPolicy`
  ist seit R0 kein Unit-Struct mehr (`harw-registry-defaults/src/lib.rs:161-198`).
  `trusted_tui_spawn_context(sandbox)` → `test_spawn_context(sandbox)`.
- `mode_request_is_applied_at_the_turn_boundary`,
  `unknown_mode_names_never_change_the_session`: dieselbe
  `AgentSession::new_with_id(..).with_spawn_context(..).with_turn_event_sink(..)`-Kette
  wie oben (ohne `with_executable_agent_ir`).

Neuer Helfer `test_spawn_context(sandbox: &SandboxSpec) -> SpawnContext`: baut
denselben `ApprovalActor::Operator { id: "local-tui" }` / `AgentRoleId::RootOrchestrator`
/ `capability_snapshot: None` / `suggestions: None`, den die gelöschte
`trusted_tui_spawn_context` vergab; `trace`/`ceiling` sind für die verbliebenen
Tests ohne Bedeutung und bleiben `None` (deren jeweilige Erzeugung ist in
`harw-runtime/src/trace.rs` bzw. `harw-runtime/src/ceiling.rs` eigenständig
getestet — siehe §2). Struct-Felder gegen `harw-core/src/session.rs:226-286`
verifiziert.

## 4. Unverändert erhaltene Tests (29) + Helfer

Alle übrigen Tests (Popup-Öffnen/-Schließen, Tastatur-Regressionstests,
Zellen-Rendering, `classify_line`/`classify_approval_key`, Freigabe-Zelle,
`durable_history_hydrates_core_and_redacts_non_text_visible_content`,
`three_child_events_produce_a_single_sub_agent_cell` u. a.) sind inhaltlich
unverändert; einzige Änderung sind die Test-lokalen `use`-Anweisungen für
Namen, die mit der Montage aus den Produktions-Imports gefallen sind
(`AgentRole`, `ExtensionRegistry`, `ModelProvider`, `TuiApprovalHandler`,
`role_names`, `ApprovalActor`, `SpawnContext`, `AgentRoleId`,
`ExecutableAgentIr`, `ApprovalModeCell`, `OperationRegistry`, `build_services`)
— jeder Name gegen mindestens eine tatsächliche Verwendung im migrierten
Testcode gegengeprüft (kein unbenutzter Import unter `-D warnings`).

Helfer `test_sandbox`, `test_chat_app`, `test_executable_agent_ir`,
`test_operations`, `rendered_cells` unverändert übernommen.

## 5. Imports

Neues, vollständiges `use`-Bündel am Kopf von `mod tests` (siehe Datei,
Zeilen 3116–3140). Jeder neu hinzugefügte Name wurde per Vorkommenszählung
(`grep -o '\bName\b'` im Testmodul) gegen ≥ 2 Treffer (Import + mindestens eine
Verwendung) geprüft — keiner ist unbenutzt. Kein Name kollidiert mit einem
Produktions-Import (Abgleich gegen app.rs:95–150).

## 6. Standards

Kein neues `#[allow]`. Kein neues `unwrap()`/`expect()` außerhalb bestehender
Testmuster (unverändert aus dem Bestand, `.expect(..)` in Tests ist laut
AGENT-BRIEF.md §4 erlaubt). Kein `println!`. Keine Netz-/Prozess-Abhängigkeit;
alle Sandboxen laufen gegen Temp-Verzeichnisse (`test_sandbox`, unverändert).
Testnamen unverändert (`test_<funktion>_<szenario>`-Konvention war bereits
eingehalten).

## 7. Abweichungen vom Brief

Keine. Alle drei im Brief benannten Migrationsmuster (`approval_test_session`
mit `DefaultApprovalPolicy::new` + direkter Builder-Registrierung,
`build_tui_agent_session`-Aufrufer über `AgentSession::new_with_id(..)`-Kette,
`/permissions`-Command-Test über `execute_command_as(.., PermissionTier::Operator, .., closure)`)
wurden exakt wie vorgegeben umgesetzt.

## 8. Ausgabe

```json
{
  "agent": "T2b",
  "files_modified": ["/home/mia/projects/harwness/harw-tui/src/app.rs"],
  "files_created": ["/home/mia/projects/harwness/docs/remediation/ledger/W2d2/T2b.md"],
  "verification": {
    "command": "cargo metadata --offline --no-deps --format-version 1",
    "exit_code": 0,
    "pass": true
  },
  "tests_before": 57,
  "tests_after": 36,
  "tests_deleted": 21,
  "tests_migrated": 7,
  "tests_unchanged": 29,
  "tabu_regions_identical": true,
  "blocked": false
}
```
