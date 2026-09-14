# A-APPR – approval.resolve mit Principal, Serveruhr, JSON-Antwort (F-172, F-121, F-122, G-011)

Rolle: focused-coding-task (Opus), Welle W4a. BUILD-POLICY eingehalten: kein cargo build/check/test/clippy/fmt,
keine git-Schreibbefehle. Einziger Befehl `cargo metadata --offline --no-deps --format-version 1` → Exit 0.
Verifikation durch Lesen (Klammerbilanz, Zeilenbreite ≤ 100 außer vorbestehendem `summary`-Attribut
`harw-ops/src/approval.rs:335`, Imports gegen Quelltext geprüft). Ein API-Rate-Limit unterbrach den Lauf; danach
wurden alle Owned Files neu gelesen – vollständig, nur zwei kosmetische Test-Nachbesserungen.

Pflichtlektüre: `AGENT-BRIEF.md`; Plan W4a-Tabelle A-APPR + Design Web-UI; `W3/C-APPR.md`; `W3M/OPS-1.md`,
`W3M/WEB.md` (Methoden `get`/`post` und `OpOutput.data` beibehalten, nichts zurückgedreht).

## Geänderte / neue Dateien

| Datei | Änderung |
|---|---|
| `harw-web/src/security.rs` | neu `ApprovalCaller<'a>` (Principal + optional Web-Peer/Resolver), `list_pending_approvals`; `resolve_approval(store, &caller, session, request, decision, comment, clock: &dyn Clock)` ohne Client-Zeit; `SecurityError` + `NoApproverActor{kind,surface}`, `UnauthenticatedWebPeer`, `ApproverIdentityMismatch{uid}`; `#[allow(clippy::too_many_arguments)]` entfernt (7 Argumente); tracing `info!`/`warn!` strukturiert; Moduldoku |
| `harw-web/src/lib.rs` | Re-Export zusätzlich `ApprovalCaller`, `SecurityError`, `list_pending_approvals`; Doku-Satz |
| `harw-web/tests/approval_security.rs` (neu) | Integrationstests über Crate-Wurzel |
| `harw-ops/src/approval.rs` | Actor aus `Principal`-Service; `resolved_at` entfernt; Serveruhr; `pending_all`; `OpOutput{text, data}`; eigener Verzeichnis-Scan (`discover_pending_candidates`, `list_pending_records`) entfernt; `pub const PENDING_LIMIT = 200` |

## Zugriff auf den Principal (verifiziert)

`OpContext` (`harw-operations/src/context.rs:172-265`) hat **kein** `principal()`; einziger Weg ist
`ctx.service::<Principal>()`. Web legt ihn ein: `harw-cli/src/web.rs:356`
`services.insert(runtime_web::web_principal(peer.uid, tier))` → `Principal::trusted_ingress(Human, "uid:<uid>", Web, tier)`
(`harw-cli/src/runtime_web.rs:90`), `actor_id()` = `Operator{id:"owner"}`.

## Semantik `approval.resolve`

1. `Arc<ApprovalStore>` fehlt → `NotAvailable`.
2. `Principal` fehlt → `NotAvailable("kein Principal …")`.
3. `ApprovalCaller::actor()`: `principal.actor_id()` = `None` (Modell, Kind, Operation, sonstige Kanäle) →
   `NoApproverActor` → `NotAvailable`. Speicher unberührt.
4. Surface `Web`: `PeerCredentials` **und** `Arc<dyn ApprovalActorResolver>` Pflicht (`UnauthenticatedWebPeer`),
   Resolver kennt Peer (`UnknownApprover`), Resolver-Actor == Principal-Actor (`ApproverIdentityMismatch`). Grund:
   `actor_id()` liefert für **jeden** Human×Web-Principal `owner`; ohne UID-Bestätigung bekäme ein vom
   `PeerAuthorizer` zugelassener Fremd-UID (z. B. über `StaticUidTierMap::with_default`) den Owner-Actor.
5. Uhr: Service `Arc<dyn Clock>` falls registriert (Tests), sonst `SystemClock`; beide serverseitig.
6. `ApprovalStore::resolve(.., &actor, clock)` → Einmaligkeit, Actor-Bindung, TTL.
7. Fehlerabbildung: Identitätsablehnungen + `ApprovalActorMismatch` → `NotAvailable`; `ApprovalNotFound`,
   `ApprovalAlreadyResolved`, `ApprovalExpired` → `InvalidArguments`; Rest → `Execution`.
8. Antwort: `OpOutput { text, data: Some({"id","session","decision","resolved_at","actor"}) }` – serde-Wire-Form
   (`decision` snake_case, `resolved_at` RFC 3339, `actor` `{"kind":"operator","id":…}`). `session` ist additiv zu
   den vier geforderten Feldern (Anfrage-IDs sind nur je Sitzung eindeutig).

`ApprovalResolveArgs` = `{session, request, decision, comment}`; kein Actor-, kein Zeitfeld. Kein
`deny_unknown_fields`: ein altes `resolved_at`/`actor` im Rumpf wird ignoriert (Test belegt), damit bestehende
Clients/`harw-cli`-Tests nicht an der Deserialisierung scheitern – Verschärfung optional in WB-SRV.

## Semantik `approval.pending`

`list_pending_approvals(store, PENDING_LIMIT, clock)` = `ApprovalStore::pending_all` (sortiert, ohne
aufgelöste/abgelaufene, defekte Einträge vom Store mit `warn!` übersprungen). Antwort
`data = {"pending": [ApprovalRecord…], "limit": 200}`. Permission `observer`, keine Principal-Pflicht (reine Anzeige,
wie bisher).

## Web-Peer-Prüfung (verifiziert, nicht geändert)

`harw-web/src/server.rs:369` Verbindung ohne lesbare `SO_PEERCRED` wird verworfen; `decide_route`
(`router.rs:209-246`): Methode → `PeerAuthorizer::tier_for` (unbekannt → 403) → `tier_permits(tier, operator)` →
Approval-Policy; erst dann `context_factory(&peer, tier)` (`server.rs:556`). Zusätzlich jetzt die
Resolver-Bestätigung in der Operation.

## Grenzen / Folgearbeit (kein Scheinschutz)

- **G-011 Transport (WB-SRV, W5):** Unix-Socket authentisiert nur per UID; ein Same-UID-Prozess (auch
  modellgestartet) ist vom Bediener nicht unterscheidbar. Token-Pflicht (`Authorization: Bearer`) für
  `approval.resolve` auch am Unix-Socket fehlt weiter; Modellprozesse ohne Zugriff auf Socket/`HARW_HOME`.
- **Selbstgenehmigung:** `ApprovalRecord.actor` = gebundener Beantworter; der Anfragende wird nicht persistiert.
  Prüfung „Anfragender ≠ Beantworter" **nicht** umgesetzt (C-APPR-Folgearbeit: `requested_by` mit
  `#[serde(default)]`, Befüllung `harw-core/src/turn_loop.rs` Aussteller, Variante `ApprovalSelfApproval`).
  Wirksam ist nur: Principals ohne `actor_id()` werden abgelehnt.
- **`harw-cli/src/web.rs` (nicht meine Datei, I-CLI/A-MAIN):**
  - `web_op_context` registriert keinen `Arc<dyn Clock>` → Fallback `SystemClock` (korrekt, keine Änderung nötig).
  - Test `test_approval_resolve_derives_the_actor_from_peer_credentials_not_the_body` (`:595`) stellt mit
    `issued_at = Timestamp::constant(1, 0)` aus → unter C-APPR-TTL + Systemuhr jetzt `ApprovalExpired`
    (`InvalidArguments`) statt Erfolg. Fix: `issued_at: Timestamp::now()` oder `Arc<dyn Clock>`-Service einfügen.
  - Tests `:580/:645` senden noch `resolved_at` – harmlos (ignoriert), sollten entfernt werden.
  - Test `test_web_route_table_from_registry_includes_approval_routes` (`:716/:722`) nutzt `readonly()` – W3M/WEB-Folgearbeit.
  - Unknown-Peer-Test (`:543`) bleibt grün: Principal→owner, Resolver kennt 9999 nicht → „keinem Genehmiger zugeordnet".
- **P1.6:** Actor-Namen erst nach Transport-Authentisierung angleichen (unverändert aus C-APPR).
- **WB-SEC (webui):** `approvalTypes.ts` kann `data` direkt als `ApprovalRecord[]` bzw. Auflösung
  (`id` statt `request`, ohne `call_id`/`comment`) lesen; Client darf kein `resolved_at` mehr senden.
- dep-request: keine.

## Tests

`harw-web/src/security.rs` (unit): `test_static_uid_approval_actor_map_resolves_listed_uid`,
`…_unknown_uid_is_none`, `test_approval_caller_actor_rejects_model_principal`, `…_rejects_child_principal`,
`…_web_without_peer_is_unauthenticated`, `…_web_unknown_peer_is_rejected`,
`…_web_peer_mapping_to_other_actor_is_mismatch`, `…_web_confirmed_peer_yields_principal_actor`,
`test_resolve_approval_rejects_caller_without_actor_without_touching_store`,
`test_resolve_approval_uses_server_clock_for_resolved_at` (auch durabel),
`test_resolve_approval_expired_request_is_rejected` (+31 min, nichts geschrieben),
`test_resolve_approval_second_confirmation_is_rejected_not_overwritten`,
`test_list_pending_approvals_excludes_resolved_and_uses_clock`, `test_pending_approval_not_found_is_reported_not_invented`,
`test_pending_approval_view_carries_irreversibility_flag`, `test_security_error_display_names_rejection`;
Doctests Modul (`no_run`), `ApprovalCaller`.

`harw-web/tests/approval_security.rs`: Modell-Principal abgelehnt/Store unberührt; Web ohne Peer bzw. mit fremdem
Peer abgelehnt; `resolved_at` == Fake-Clock; abgelaufen (Grenze 30 min inklusiv) weder gelistet noch auflösbar.

`harw-ops/src/approval.rs` (Fake-Clock als `Arc<dyn Clock>`-Service):
`test_approval_resolve_without_principal_is_rejected`, `test_approval_resolve_principal_without_actor_is_rejected`,
`test_approval_resolve_web_principal_without_peer_credentials_is_rejected`,
`test_approval_resolve_rejects_a_peer_unknown_to_the_resolver`,
`test_approval_resolve_uses_server_clock_and_returns_json_data` (exakte `data`-Struktur),
`test_approval_resolve_expired_request_is_rejected`, `test_approval_resolve_rejects_a_second_confirmation_of_the_same_request`,
`test_approval_pending_lists_open_requests_from_pending_all` (Sortierung, abgelaufen/aufgelöst ausgelassen, `data.pending`),
`test_approval_pending_with_empty_store_reports_none_open`, `test_approval_pending_without_store_returns_not_available`,
`test_approval_resolve_without_approval_store_returns_not_available`,
`test_approval_resolve_args_ignore_client_time_and_actor`, Raw-Args-Tests (umbenannt nach `test_<fn>_<szenario>`).

## Compile-Risiken (nicht kompiliert)

1. `serde_json::json!` mit `?` in Wert-Ausdrücken (`resolution_data`, `approval_pending`) – gültige Ausdrucks-tt.
2. `ctx.service::<Arc<dyn Clock>>()` – `Arc<dyn Clock + 'static>: Any + Send + Sync` (Clock: Send + Sync).
3. `ApprovalCaller` `#[derive(Clone, Copy)]` mit `&'a dyn ApprovalActorResolver`; `Debug` manuell.
4. Kompiliert erst zusammen mit C-APPR-Stand (`resolve(.., clock)`, `pending_all`, `ApprovalExpired`) – vorhanden.
5. rustfmt nicht gelaufen; Formatierung von Hand.
