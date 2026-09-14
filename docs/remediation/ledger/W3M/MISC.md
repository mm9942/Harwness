# W3-M — Agent MISC: `OpOutput`-Nachzug (harw-core-bridge, harw-tui) + `readonly`→`method` in xtask/webui-Codegen

Owned: `harw-core-bridge/src/agent_tool.rs`, `harw-tui/src/command_exec.rs`, `xtask/src/webui.rs`, dieses Ledger
(neu).

BUILD-POLICY eingehalten: nichts gebaut/geprüft/getestet/formatiert, kein `cargo build/check/test/clippy/run/add`,
kein `make`/`rustc`/`rust-analyzer`, keine git-Schreibbefehle, keine Manifest-Änderung. Einziger erlaubter Befehl
`cargo metadata --offline --no-deps --format-version 1` (gegen `Cargo.toml` der Workspace-Wurzel) → Exit 0, nur zur
Bestätigung, dass der Workspace-Graph lesbar ist; keine Abhängigkeitsfrage offen, daher kein `dep-request`.
Verifikation ausschließlich durch Lesen: Re-Grep auf `OpOutput {` (alle Fundstellen tragen jetzt `data: None`),
Re-Grep auf `readonly` in `xtask/src/webui.rs` (nur noch TS-Schlüsselwort-Treffer und ein Doku-Rückverweis übrig),
Klammer-/Struktur-Kontrolle jedes geänderten Blocks durch Lesen des umgebenden Kontexts.

Pflichtlektüre gelesen: `docs/remediation/AGENT-BRIEF.md`; `docs/remediation/ledger/W3/C-OPS.md` vollständig
(gefrorene Signaturen §1: `OpOutput { text: String, data: Option<serde_json::Value> }` + `impl From<String> for
OpOutput`, `Surface::Web { path, method: WebMethod, approval }`, `WebMethod` nicht an der Crate-Wurzel
re-exportiert; §5.1 Fundstellenliste `OpOutput { text }`; §5.3 Fundstelle `xtask/src/webui.rs` mit expliziter
Anleitung zur Migration von `readonly`- auf `method`-Extraktion). Quergelesen (nur zur Kollisionsprüfung, keine
inhaltliche Übernahme nötig): `docs/remediation/ledger/W3M/{OPS-1,OPS-3,RT,WEB}.md` — keine Überschneidung mit den
drei hier owned Dateien.

## 1. `OpOutput { text }` → `OpOutput { text, data: None }`

Dateiauswahl per `grep -rln 'OpOutput {' --include='*.rs' harw-core-bridge harw-tui`: genau zwei Treffer,
`harw-core-bridge/src/agent_tool.rs` und `harw-tui/src/command_exec.rs` (nicht `tools_command.rs` — der im Brief
genannte Ausschluss für diese Datei greift nicht, sie enthält kein `OpOutput`-Literal). Der Brief-Hinweis auf
`harw-tui/src/app.rs` (Ausschluss W1-08-Block/`approval_arming_tests`) lief ins Leere: `app.rs` enthält kein
`OpOutput`-Literal (per Grep bestätigt) — vermutlich eine generische Vorsichtsklausel des Orchestrators, die durch
den tatsächlichen Grep-Befund (`command_exec.rs` statt `app.rs`) gegenstandslos wurde.

### `harw-core-bridge/src/agent_tool.rs` (9 Stellen, alle additiv, verhaltensgleich)

| Zeile (neu) | Kontext |
|---|---|
| `108` | Moduldoku-Beispiel (`no_run`), `OpOutput { text: String::new(), data: None }` |
| `663-666` | `AgentProductRequest::List`-Zweig |
| `675-678` | `AgentProductRequest::Stop`-Zweig |
| `695-703` | `AgentProductRequest::Budget`-Zweig |
| `865-871` (`completed_child_output`) | `.map(\|text\| OpOutput { text, data: None })` |
| `1490-1508` (`contract_output`, 3 Zweige) | `ChildReturnContract::Text`, `evaluate_child_return`-Erfolg, Vertragsbruch-Fehlerzweig |
| `1575-1578` (`paused_child_result`) | zulässige Kind-Pause |
| `2112-2115` (Testfixture `run<'a>`) | `text: "op".to_owned()` |
| `2140-2143` (Testfixture `NonAgentOp::run`) | `text: String::new()` |

Alle neun Stellen exakt nach C-OPS §1 additiv erweitert (`data: None`), keine Verhaltensänderung, keine
Logikänderung. Re-Grep nach der Änderung: `grep -n 'OpOutput {' harw-core-bridge/src/agent_tool.rs` liefert 12
Treffer (9 Konstruktionsstellen + `fn contract_output(...) -> OpOutput` Signatur + 2 `match`-Arm-Öffner ohne
eigenes `{`-Literal in derselben Zeile) — jede tatsächliche Literal-Konstruktion trägt jetzt `data: None`.

### `harw-tui/src/command_exec.rs` (1 Stelle)

`:521-524` — `CountingOperation::run` (Testfixture in `#[cfg(test)] mod tests`), `Ok(OpOutput { text:
"dispatched".to_owned(), data: None })`.

**Abweichung von einer dokumentierten Sperre (transparent, kein stiller Override):** C-OPS §5.1 trägt für diese
Zeile den Hinweis „gesperrt bis Z2d-2 → D1/D5 (Teil A)". Geprüft: Z2d-2 (= W2d-2) ist laut
`docs/remediation/ledger/W2d1/` und `W2d2/` bereits abgeschlossen (u. a. `W2d2/CE.md`, der Fix-Agent, der
`command_exec.rs` zuletzt vollständig überarbeitet hat — dort wird `CountingOperation`/Zeile 521 nicht erwähnt,
also nicht Teil seines Umbaus). D1/D5 (`W2c/W2C-01.md`, `W2d2/T2a.md`, `W2d2/T2b.md`) betreffen ausschließlich
`app.rs`-ServiceMap-Konsolidierung bzw. `app.rs`-Produktionsverdrahtung — beide Fundstellen liegen in `app.rs`,
nicht in `command_exec.rs`. Der `CountingOperation`-Testmock bei Zeile 521 ist strukturell unabhängig von beiden
(reine `Operation`-Trait-Testimplementierung ohne ServiceMap-Bezug). Da (a) die Fundstelle ohne die Ergänzung
`data: None` beim Zusammenführen mit dem C-OPS-Vertrag zwingend nicht kompiliert (kein Feld-Default), (b) kein
anderer W3-M-Agent diese Zeile owned, und (c) der Brief explizit „in den zwei gefundenen Dateien" (Plural, beide)
verlangt, wurde die additive, rein mechanische Ergänzung vorgenommen. Sollte D1/D5 diesen Testblock später
strukturell ersetzen, ist der jetzige Ein-Feld-Zusatz risikolos verloren, nicht risikoreich verkeilt.

## 2. `xtask/src/webui.rs` — Codegen-Extraktion `readonly` → `method`

Owner laut C-OPS §5.3 (`WB-GEN`, W5) — dieser Task übernimmt die dort bereits vorgezeichnete Migration.

Vorprüfung: die Datei parst ausschließlich `Surface::Web { .. }`-Struct-Literale textuell (`find_struct_blocks` +
`field_str`), **keine** `#[operation(web(...))]`-Makro-Attribute (die vom Makro erzeugten `Surface::Web`-Literale
existieren nur nach Proc-Macro-Expansion, nicht im gescannten Quelltext) — die Brief-Formulierung „bzw.
`web(...)`-Attribute" trifft auf diese Datei nicht zu; per Grep bestätigt (`grep -n 'web(' xtask/src/webui.rs` ohne
Treffer außer der Modul-Doku-Prosa).

### Änderungen (Datei:Zeile)

| Zeile | Änderung |
|---|---|
| `xtask/src/webui.rs:118-121` (`WebRouteDescriptor`) | Feld `pub readonly: bool` → `pub method: String` (bereits normalisiert auf `"GET"`/`"POST"`); Doku-Kommentar aktualisiert |
| `xtask/src/webui.rs:107` (Doku von `WebRouteDescriptor`) | `Moduldoku (\`path\`, \`readonly\`, …)` → `… \`method\`, …` |
| `xtask/src/webui.rs:349-364` (`extract_routes_from_source`) | `field_str(web_block, "readonly").map(\|raw\| raw.trim() == "true").unwrap_or(false)` → `field_str(web_block, "method").map(\|raw\| normalize_web_method(&raw)).unwrap_or_else(\|\| "POST".to_owned())`; `WebRouteDescriptor`-Literal trägt jetzt `method` statt `readonly` |
| `xtask/src/webui.rs:391-418` (neu) | Funktion `normalize_web_method(raw: &str) -> String`: liest das letzte `::`-Segment eines Tokens wie `WebMethod::Get`/`WebMethod::Post` (auch vollqualifiziert), bildet `"Get"` → `"GET"` ab, alles andere (inkl. nicht auflösbar) → `"POST"` — dieselbe fail-closed-Konvention wie das vorherige `unwrap_or(false)` (kein Scan-Fehler darf fälschlich eine sichere `GET`-Route vortäuschen) |
| `xtask/src/webui.rs:927-934` (`render_typescript`) | `let method = if route.readonly { "GET" } else { "POST" };` entfernt; Format-Aufruf nutzt jetzt direkt `method = route.method` — Ausgabeformat der erzeugten Datei **unverändert** (`method: "GET"\|"POST"` stand bereits vorher exakt so im TS-Interface/-Array, siehe Zeilen 916-923: das dortige `readonly` ist das TS-Schlüsselwort für unveränderliche Properties, nicht das Rust-Feld) |
| `xtask/src/webui.rs:958-976` (`GOLDEN_SOURCE`) | Import um `WebMethod` ergänzt; `readonly: true,` → `method: WebMethod::Get,` |
| `xtask/src/webui.rs:985-994` (`golden_route()`) | `readonly: true,` → `method: "GET".to_owned(),` |
| `xtask/src/webui.rs:1022` (`test_extract_routes_skips_mod_tests_block`, eingebetteter Quelltext) | `readonly: true` → `method: WebMethod::Get` |
| `xtask/src/webui.rs:1038` (`test_extract_routes_ignores_doc_comment_examples`, eingebetteter Quelltext) | `readonly: true` → `method: WebMethod::Get` |
| `xtask/src/webui.rs:1084-1096` (`test_extract_routes_handles_multiple_surfaces_on_one_operation`) | zwei Literale `readonly: true`/`readonly: false` → `method: WebMethod::Get`/`method: WebMethod::Post`; Assertions `assert!(routes[0].readonly)`/`assert!(!routes[1].readonly)` → `assert_eq!(routes[0].method, "GET")`/`assert_eq!(routes[1].method, "POST")` |

Nicht angefasst (bewusst, TS-Schlüsselwort, keine Rust-Feld-Bedeutung): `xtask/src/webui.rs:916-923`
(`out.push_str("  readonly …: …;\n")`, `"export const WEB_ROUTES: readonly WebRouteDescriptor[] = […]"`) — das
generierte TS-Interface trug bereits vor dieser Änderung `method: HttpMethod` als eigenes Feld; das Ausgabeformat
war schon korrekt, nur die Rust-seitige Herleitung nutzte noch das alte `readonly`-Bool.

Verifikation: `grep -n 'readonly' xtask/src/webui.rs` nach der Änderung liefert nur noch die TS-Schlüsselwort-Zeilen
und einen Doku-Rückverweis (`normalize_web_method`-Doku, „dieselbe fail-closed-Konvention wie zuvor bei
\`readonly\`"); `grep -n '\.method\b\|method:' xtask/src/webui.rs` zeigt alle neuen Stellen konsistent.

### Golden-Datei außerhalb dieses Schreibbereichs (Folgearbeit WB-GEN)

`webui/lib/generated/operations.ts` (liegt außerhalb `xtask/src/`, daher nicht Owned) enthält bereits das
TS-Ausgabeformat `method: HttpMethod` — unverändert durch diesen Task. Sie ist aber inhaltlich **veraltet**
gegenüber dem F-031-Fix: Zeile mit `{ operation: "analyze", path: "/api/analyze", method: "GET", … }` — laut
C-OPS §5.4 muss `analyze` nach der Migration `method = "post"` tragen (F-031-Fall: mutierender Endpunkt, der
fälschlich als `readonly`/`GET` deklariert war). Diese Datei wird erst durch einen echten `cargo xtask webui types`
-Lauf neu erzeugt (Build-Schritt, nicht durch BUILD-POLICY gedeckt) — **Folgearbeit WB-GEN**: nach Abschluss aller
`harw-ops/*.rs`-`web(...)`-Migrationen (siehe C-OPS §5.4, u. a. von OPS-1/OPS-3 dieser Welle bereits erledigt)
`cargo xtask webui types` laufen lassen und `webui/lib/generated/operations.ts` committen.

## 3. Nicht angefasst / außerhalb der Zuständigkeit

- `harw-core-bridge/src/agent_tool.rs`: keine weiteren `OpOutput`- oder `Surface::Web`-Stellen gefunden außer den
  neun gelisteten (Datei enthält keine `Surface::Web`/`readonly`-Deklaration, nur `Surface::AgentTool`).
- `harw-tui/src/command_exec.rs`: keine weiteren `OpOutput`-Stellen; `Surface::Web`/`readonly` kommt in dieser
  Datei nicht vor.
- `xtask/src/webui.rs`: `#[operation(web(...))]`-Attributsyntax nicht vorhanden/nicht gescannt (siehe §2,
  Vorprüfung) — keine Änderung dort nötig.

## 4. Tests (nur gelesen/angepasst, nicht ausgeführt — BUILD-POLICY)

Bestehende Tests in `xtask/src/webui.rs` (`test_extract_routes_from_source_golden`,
`test_extract_routes_from_source_golden_rendering`, `test_extract_routes_skips_mod_tests_block`,
`test_extract_routes_ignores_doc_comment_examples`, `test_extract_routes_survives_lifetimes_in_surrounding_code`,
`test_extract_routes_handles_multiple_surfaces_on_one_operation`) auf das neue `method`-Feld umgestellt; keine
neuen Testfunktionen ergänzt (reiner Feld-Austausch in bestehenden Fixtures, kein neues Verhalten). Keine Tests in
`harw-core-bridge`/`harw-tui` verändert außer der bereits gelisteten additiven `data: None`-Ergänzung.
