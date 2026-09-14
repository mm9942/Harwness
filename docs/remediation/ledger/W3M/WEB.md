# W3-M — Agent WEB: harw-web auf deklarierte `Surface::Web`-Methode (F-031), `OpOutput.data` (F-222)

Owned: `harw-web/src/{router,security,lib,error,server}.rs`, `harw-web/tests/**`, dieses Ledger.
Nicht angefasst: `harw-cli/**`, `harw-operations/**`, `harw-web/src/{authz,events,peer}.rs`, `harw-web/Cargo.toml`.
BUILD-POLICY eingehalten: kein cargo build/check/test/clippy, keine git-Schreibbefehle.
`cargo metadata --offline --no-deps --format-version 1` → Exit 0. Verifikation durch Lesen.

Pflichtlektüre: `docs/remediation/AGENT-BRIEF.md`, `ledger/W3/C-OPS.md` (§1, §5.1–5.3), `ledger/W1/W1-11.md`,
Register `x-findings-register-w1-w3.md:110` (F-031), `:229` (F-121).

## 1. Sicherheitswirkung (F-031)

Vorher: `router.rs:38-75` (alt) eigener `WebMethod` + `expected_for(readonly)`; Admission `router.rs:185` (alt)
`WebMethod::expected_for(route.readonly())` ⇒ jede als `readonly` markierte, tatsächlich mutierende Operation
(`/api/analyze`) war per `GET` (Prefetch, `<img src>`, CSRF) auslösbar.

Nachher:
- Kein lokaler Enum, keine Ableitung. `router.rs:49` `pub use harw_operations::operation::WebMethod;` (Pfad laut
  C-OPS §1; `harw_operations::WebMethod` ist an der Crate-Wurzel noch nicht re-exportiert, C-OPS §5.2).
- `WebRouteTable` speichert je Route `WebRoute { adapter, method }` (`router.rs:261`); `method` wird in
  `from_registry` (`router.rs:314`) über `declared_method` (`router.rs:269`) **wörtlich** aus
  `Surface::Web { path, method, .. }` der Operation gelesen (Abgleich über den Adapter-Pfad).
  Fehlt die Deklaration → fail-closed `WebError::RouteMethodUndeclared` (`error.rs:102`), Tabelle wird nicht gebaut.
- Bewusst **nicht** über `WebAdapter::readonly()`/`method()`: `harw-operations/src/adapter/web.rs` trägt noch das
  `readonly`-Feld (C-OPS §5.3, eigene Folgewelle); die Methode kommt so direkt aus dem gefrorenen Vertragstyp,
  unabhängig vom Adapter-Umbau.
- `decide_route` (`router.rs:209`): Methode wird als 2. Schritt vor Peer-/Tier-/Approval-Prüfung gegen
  `entry.method` (`router.rs:221`) verglichen → `RouteDecision::MethodNotAllowed { expected }`.
- `parse_web_method` (`router.rs:105`): nur exakt `"GET"`/`"POST"` (case-sensitiv); `HEAD`, `OPTIONS`, `DELETE`, `get`
  → `None` ⇒ immer 405, auch auf GET-Routen. Server nutzt es in `server.rs:529`.
- 405-Antwort `server.rs:776`: `Allow`-Header = deklarierte Methode (`HeaderValue::from_static(method_name(..))`,
  `method_name` in `router.rs:71`), Rumpf unverändert `{"error":"method_not_allowed","expected":"…"}`.
- Neue öffentliche Lese-API: `WebRouteTable::method_for` (`router.rs:375`), `iter_with_methods` (`router.rs:398`);
  `find`/`iter`/`len`/`is_empty` und `RouteDecision` signaturgleich. `lib.rs:143-146` re-exportiert zusätzlich
  `method_name`, `parse_web_method`; `harw_web::WebMethod` zeigt jetzt auf den Vertragstyp.
  Externe Nutzer von `expected_for`/`as_str`: keine (grep workspace).

## 2. `OpOutput.data` (F-222)

- Literale in `router.rs` (Testfixtures, 2 Stellen) → `OpOutput { text, data: None }`; `OperationMeta`-Literale um
  `output_schema: None` ergänzt; `TierOp.readonly` → `method: WebMethod`.
- `server.rs:737` `op_result_response`: Antwort bleibt `{"text","trust"}`; bei `data: Some(v)` additiv Feld `"data"`
  (`server.rs:745`). Ohne Nutzlast kein neues Feld (Format unverändert). Keine Folgearbeit WB-SRV nötig.

## 3. Weitere Änderungen

- `security.rs:23-25` Moduldoku: `web(...)`-Beispiele auf `method = "get"`/`"post"` (entspricht `harw-ops/src/approval.rs:325,447`).
- `server.rs` W1-11-Teile (Limited-Body, Timeouts, accept-Backoff, Bind) unverändert.

## 4. Tests

`router.rs` (unit): `test_decide_route_get_on_post_route_yields_method_not_allowed` (801),
`test_decide_route_post_on_get_route_yields_method_not_allowed` (813),
`test_decide_route_correct_method_dispatches_to_declaring_operation` (825),
`test_decide_route_unsupported_method_on_post_route_is_rejected_before_authorization` (847),
`test_from_registry_takes_method_from_surface_web_declaration` (860),
`test_from_registry_post_without_approval_stays_post` (881, F-031-Muster `/api/analyze`),
`test_parse_web_method_maps_only_exact_get_and_post` (890), `test_method_name_matches_http_and_serde_form` (902);
entfernt: `test_web_method_expected_for_matches_readonly_flag`. Bestehende Matrix-Tests auf `method` umgestellt.

`server.rs` (unit): `test_method_not_allowed_response_allow_header_names_post` (973),
`test_op_result_response_without_data_keeps_text_trust_shape` (986), `test_op_result_response_with_data_adds_data_field` (1000).

`error.rs`: `test_display_route_method_undeclared_contains_path_and_operation` (192).

`tests/method_admission.rs` (neu, echter Unix-Socket + produktiver `BoundWebServer`, Kontextfabrik `unreachable!`,
Operation zählt `run`-Aufrufe): `test_get_on_post_route_yields_405_with_allow_post_and_never_runs` (188),
`test_post_on_get_route_yields_405_with_allow_get_and_never_runs` (199),
`test_head_and_options_are_never_mapped_onto_routes` (210; HEAD/OPTIONS/DELETE → 405 + Allow),
`test_route_table_from_registry_takes_declared_method` (233),
`test_correct_method_is_dispatched_to_declaring_operation` (242; über `decide_route`, da `Execute` über den Server
wegen `OpContext`/`SandboxSpec` nicht e2e testbar ist — W1-11 „Offene Punkte").

## 5. Kompiliert erst mit / Folgearbeit (nicht meine Dateien)

- `harw-operations/src/adapter/web.rs:124,193,200,255,393` destrukturiert noch `Surface::Web { readonly, .. }` — Crate
  `harw-operations` (und damit `harw-web`) baut erst nach dessen Umstellung (C-OPS §5.3). `harw-web` benutzt vom
  Adapter nur `from_operation`, `path`, `operation_name`, `permission`, `approval`, `invoke`; `readonly()` wird nicht
  mehr aufgerufen, ein Entfernen bricht `harw-web` nicht.
- `harw-operations/src/lib.rs:100-103`: `WebMethod` re-exportieren (C-OPS §5.2) — optional, `harw-web` nutzt den
  vollqualifizierten Pfad.
- `harw-ops/src/analyze.rs:879` muss `method = "post"` deklarieren (C-OPS §5.4) — erst dann ist F-031 für
  `/api/analyze` am laufenden System geschlossen; `harw-web` erzwingt ab jetzt nur, was deklariert ist.
- `xtask/src/webui.rs` (WB-GEN): TS-Codegen leitet Methode noch aus `readonly` ab (C-OPS §5.3).
- F-042 (`WebAdapter::invoke` → `OpInput::model_tool`) und Origin/CSRF-Header-Prüfung (S2) nicht adressiert.
- dep-request: keine.
