# W3-M — Agent CLI-WEB: `WebAdapter::readonly()`-Testaufrufe auf deklarierte `WebMethod` umstellen

Owned: `harw-cli/src/web.rs`, `harw-cli/src/runtime_web.rs`, dieses Ledger (neu).

BUILD-POLICY eingehalten: kein `cargo build/check/test/clippy/run/add`, kein `make`/`rustc`/`rust-analyzer`, keine
git-Schreibbefehle. Einziger Befehl `cargo metadata --offline --no-deps --format-version 1` → Exit 0. Verifikation
ausschließlich durch Lesen/Grep, zusätzlich Klammern-/Parenthesen-Zählcheck über `web.rs` nach der Änderung
(`{}`: 63/63, `()`: 411/411 — unverändert zu F-MAIN's Ausgangszählung, da nur Testkörper umgeschrieben, keine neue
Klammerung eingeführt außer 1:1 balancierten `assert_eq!`-Blöcken).

Pflichtlektüre gelesen: `docs/remediation/AGENT-BRIEF.md`, `docs/remediation/ledger/W3M/RT.md` (Fundstellenliste
`harw-cli/src/web.rs:709,715` bereits dort als Folgearbeit benannt), `docs/remediation/ledger/W3M/WEB.md` (§1: neue
Lese-API `WebRouteTable::method_for`/`iter_with_methods`, `router.rs:375,398`; `harw_web::WebMethod`-Re-Export,
`lib.rs:143-146`), `docs/remediation/ledger/W2d2/F-MAIN.md` (zuletzt geänderte `web.rs` — `PlanServices.services`-Fix,
`runtime_plan_services`-Entfernung — **nicht** zurückgedreht, nur gelesen), `harw-web/src/router.rs` (verifiziert:
`method_for`, `iter_with_methods`, `WebRoute{adapter, method}`, `declared_method`), `harw-operations/src/adapter/web.rs`
(verifiziert: `WebAdapter::method()` liest `Surface::Web{method,..}` direkt; `WebAdapter::readonly()` bleibt als
abgeleiteter Bequemlichkeits-Accessor `method == WebMethod::Get`, laut eigener Doku **nicht** mehr entfernt — Aufruf
wäre also weiterhin kompilierbar gewesen, Migration erfolgt trotzdem für Konsistenz mit F-031/harw-web, das den
Adapter-`readonly()`-Wrapper bewusst nicht mehr benutzt, siehe WEB.md §1).

## 1. Befund

Nur zwei Code-Fundstellen in beiden Owned Files insgesamt (vollständiger Grep auf `readonly`, `Surface::Web`,
`OpOutput {`, `OperationMeta {`, `WebMethod` über beide Dateien):

| Datei | Zeile | Fundstelle | Einordnung |
|---|---|---|---|
| `harw-cli/src/web.rs` | 709 | `assert!(pending.readonly(), "approval.pending is declared readonly");` | Code — migriert |
| `harw-cli/src/web.rs` | 715 | `assert!(!resolve.readonly(), "approval.resolve is declared mutating");` | Code — migriert |
| `harw-cli/src/web.rs` | 693–696 | Doku-Kommentar „`Surface::Web`, kein `Surface::ModelTool` — der CommandsOnly-Filter …" | Prosa, kein Literal — unverändert (beschreibt weiterhin gültiges `CommandsOnly`-Verhalten, nicht die Methodenprüfung) |
| `harw-cli/src/runtime_web.rs` | — | keine Fundstelle | Grep auf `readonly`, `Surface::Web`, `OpOutput {`, `OperationMeta {`: 0 Treffer. Einzige `*Surface::Web`-Treffer sind `IngressSurface::Web` (Zeilen 88, 91, 240) — ein von `harw_operations::operation::Surface` unabhängiger Principal-Zugangsweg-Enum, keine Änderung nötig (deckt sich mit RT.md's Einordnung derselben Datei) |

`OpOutput {`/`OperationMeta {`-Literale: 0 Treffer in beiden Dateien (weder Konstruktion noch Destrukturierung).

## 2. Fix

`harw-cli/src/web.rs`, Test `test_web_route_table_from_registry_includes_approval_routes` (Zeile 698):

- Lokaler Import `use harw_web::router::WebMethod;` innerhalb der Testfunktion ergänzt (Stil konsistent mit den
  bereits vorhandenen funktionslokalen `use`-Anweisungen in anderen Tests derselben Datei, z. B. Zeilen 502–503,
  544–545, 596–599) — kein Modulkopf-Import nötig, `WebMethod` wird sonst nirgends in `web.rs` gebraucht.
- `assert!(pending.readonly(), …)` → `assert_eq!(routes.method_for("/api/approval-pending"), Some(WebMethod::Get), "approval.pending is declared readonly (GET)")`.
- `assert!(!resolve.readonly(), …)` → `assert_eq!(routes.method_for("/api/approval-resolve"), Some(WebMethod::Post), "approval.resolve is declared mutating (POST)")`.
- Testaussage unverändert: `approval.pending` = GET (readonly), `approval.resolve` = POST (mutating) — jetzt gegen
  die deklarierte `WebMethod` aus `Surface::Web{method,..}` geprüft (`WebRouteTable::method_for`, `harw-web/src/router.rs:375`)
  statt gegen den abgeleiteten `WebAdapter::readonly()`-Bequemlichkeits-Accessor. Die `pending`/`resolve`-Bindungen
  aus `routes.find(..)` bleiben erhalten (weiterhin für `operation_name()`-Assertions genutzt).
- `WebMethod` ist `#[derive(Clone, Copy, Debug, PartialEq, Eq, …)]` (`harw-operations/src/operation.rs:288`) —
  `assert_eq!` funktioniert ohne weitere Trait-Importe.

`harw-cli/src/runtime_web.rs`: keine Änderung (keine Fundstelle).

## 3. Verifikation

- Grep `\.readonly(` über `web.rs` nach der Änderung: 0 Treffer.
- `cargo metadata --offline --no-deps --format-version 1` → Exit 0 (Workspace weiterhin auflösbar, keine
  `Cargo.toml`-Änderung).
- Klammern-/Parenthesen-Zählung `web.rs`: `{}` 63/63, `()` 411/411 (balanciert).
- `WebRouteTable::find`, `WebAdapter::operation_name()` unverändert weiterverwendet — keine Signaturänderung an
  Konsumentenseite außer der beiden migrierten Assertions.

## 4. Folgearbeit / offene Punkte

- Keine. Die in `RT.md` §2.2 benannte Folgearbeit (`harw-cli/src/web.rs:709,715`) ist mit diesem Ledger erledigt.
- `harw-operations/src/adapter/web.rs` behält `WebAdapter::readonly()` als abgeleiteten Accessor (nicht entfernt,
  siehe eigene Doku dort) — falls eine spätere Welle ihn entfernt, sind in `harw-cli/**` danach keine Aufrufer mehr
  vorhanden (dieser war der letzte, grep-bestätigt).
- dep-request: keine.
