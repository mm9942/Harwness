# W3-M — Agent OPERATIONS: `OpOutput.data`/`OperationMeta.output_schema`-Nachzug + `Surface::Web.readonly`→`method`
in `harw-operations`/`harw-macros` (Konsumentenseite von C-OPS)

Owned: `harw-operations/src/adapter/**`, `harw-operations/src/registry.rs`, `harw-operations/src/lib.rs` (nur
Re-Export `WebMethod`), `harw-operations/tests/**`, `harw-macros/tests/**`, alle weiteren `harw-macros/src`-Dateien
außer `operation.rs` mit `OpOutput`-Literalen (per Grep: keine gefunden, siehe §0), dieses Ledger (neu). NICHT:
`harw-operations/src/operation.rs`, `harw-macros/src/operation.rs` (C-OPS, fertig — nur gelesen).

BUILD-POLICY eingehalten: kein `cargo build/check/test/clippy/run/add`, kein `make`/`rustc`/`rust-analyzer`, keine
git-Schreibbefehle, keine Manifest-Änderung. Verifikation ausschließlich durch Lesen/Grep (vollständige
Konsistenz-Gegenprobe je Datei: `args_schema: None,`-Zählung == `output_schema: None,`-Zählung, 0 verbleibende
bare `OpOutput {`-Literale außer den zwei bewusst zweifeldrigen `text`+`data`-Fällen in `tests/operations.rs`, 0
verbleibende `readonly`-Vorkommen in `Surface::Web`-Kontexten).

Pflichtlektüre gelesen: `docs/remediation/AGENT-BRIEF.md`; `docs/remediation/ledger/W3/C-OPS.md` (gefrorene
Signaturen `OpOutput{text,data}` + `impl From<String> for OpOutput`, `WebMethod{Get,Post}`,
`Surface::Web{path,method,approval}`, `OperationMeta.output_schema`; §3 additive Entscheidung; §5
Folgearbeitsliste — Zeilen für meine Owned Files: §5.1 Tabelle `harw-operations/src/adapter/{command,web,model_tool}.rs`,
`harw-operations/tests/operations.rs`, `harw-macros/tests/operation.rs`; §5.3 Tabelle `harw-operations/src/adapter/web.rs`,
`harw-operations/src/registry.rs`, `harw-operations/tests/operations.rs`, `harw-macros/tests/operation.rs`).
Zusätzlich zur Orientierung (nicht Pflichtlektüre, parallel entstanden, keine Überschneidung mit meinen Owned
Files) quergelesen: `docs/remediation/ledger/W3M/{RT,CLI-WEB,WEB}.md` — bestätigen unabhängig, dass
`WebAdapter::readonly()` als abgeleiteter Accessor erhalten bleiben sollte (CLI-WEB.md §Pflichtlektüre nimmt genau
das für `harw-operations/src/adapter/web.rs` an; mein Ergebnis in §3 unten erfüllt diese Annahme).

## 0. `harw-macros/src/*.rs` außer `operation.rs` — keine `OpOutput`-Literale (Grep-Nachweis)

Auftrag: alle `harw-macros/src`-Dateien außer `operation.rs`, die `OpOutput`-Literale enthalten. Vollständiger
Grep über `harw-macros/src/*.rs` (16 Dateien, `operation.rs` ausgenommen) nach `OpOutput`: **0 Treffer**. Keine
dieser Dateien referenziert `OpOutput` überhaupt (sie erzeugen nur `TokenStream`s, die `OpOutput`-Nutzung liegt
ausschließlich im vom Makro erzeugten bzw. vom Nutzer geschriebenen Funktionskörper, nicht im Makro-Quelltext
selbst). Kein Owner-Bedarf, keine Änderung.

## 1. `harw-operations/src/lib.rs` — `WebMethod`-Re-Export ergänzt

| Zeile | Änderung |
|---|---|
| `harw-operations/src/lib.rs:100-104` | `pub use operation::{…, Surface, WebMethod};` — `WebMethod` zur bestehenden Re-Export-Liste hinzugefügt (war laut C-OPS §5.2 noch offen: `harw_operations::WebMethod` statt `harw_operations::operation::WebMethod`). |

## 2. `harw-operations/src/adapter/command.rs` — `OpOutput`-Nachzug (kein `readonly`/`Surface::Web`-Bezug)

`CommandAdapter` kennt keine `Surface::Web`-Fläche — hier ist ausschließlich §5.1 (OpOutput/OperationMeta)
einschlägig, §5.3 (readonly→method) betrifft diese Datei nicht.

| Zeile | Änderung | Befund |
|---|---|---|
| `command.rs:56-57` (Doku-Beispiel) | `args_schema: None,` → zusätzlich `output_schema: None,`; `OpOutput { text: "ok".to_owned() }` → `OpOutput::from("ok".to_owned())` | F-222 |
| `command.rs:408-415` (`NoSurfaceOp`) | `output_schema: None,` ergänzt; `Ok(OpOutput::from("noop".to_owned()))` | F-222 |
| `command.rs:435-442` (`SingleCommandOp`) | wie oben, `"single"` | F-222 |
| `command.rs:468-475` (`TwoCommandOp`) | wie oben, `"two"` | F-222 |
| `command.rs:501-508` (`MixedSurfaceOp`) | wie oben, `"mixed"` | F-222 |
| `command.rs:528-539` (`ArgCountOp`) | `output_schema: None,` ergänzt; `Ok(OpOutput { text: format!(…) })` → `Ok(OpOutput::from(format!("{}", input.invocation.raw_args().len())))` | F-222 |
| `command.rs:561-562` (`InvalidArgsOp`) | nur `output_schema: None,` ergänzt (kein `OpOutput`-Konstrukt — die Funktion gibt immer `Err` zurück) | F-222 (Kontraktfolge) |

Alle 7 `OperationMeta`-Literale der Datei (1 Doku + 6 Test-Fixtures) trugen zuvor nur `args_schema: None,` als
letztes Feld ohne `..Default::default()` — jedes davon hätte ohne `output_schema` nicht kompiliert, sobald
`OperationMeta` (C-OPS, `operation.rs:517-553`) das neue Pflichtfeld trägt. Gegenprobe: `args_schema: None,`-Zählung
== `output_schema: None,`-Zählung == 7.

## 3. `harw-operations/src/adapter/web.rs` — `readonly: bool` → `method: WebMethod` + `OpOutput`-Nachzug

Größte Änderung meines Zuständigkeitsbereichs (§5.1 und §5.3 aus C-OPS beide einschlägig).

### 3.1 Strukturelle Änderung

| Zeile | Änderung | Befund |
|---|---|---|
| `web.rs:105-108` | Import: `WebMethod` zur `use crate::operation::{…}`-Liste ergänzt | F-031 |
| `web.rs:136-143` | `WebAdapter.readonly: bool` → `method: WebMethod` (Struct-Feld) | F-031 |
| `web.rs:145-157` | `Debug`-Impl: `.field("readonly", &self.readonly)` → `.field("method", &self.method)` | F-031 |
| `web.rs:199-225` (`from_operation`) | Destrukturierung `Surface::Web { path, readonly, approval }` → `{ path, method, approval }`; `readonly: *readonly` → `method: *method` | F-031 |
| `web.rs:249-273` | **Neuer** Accessor `pub fn method(&self) -> WebMethod` — liest `Surface::Web.method` unverändert durch (F-031: `harw-web` soll die Methode **direkt** verwenden, keine Ableitung mehr) | F-031 |
| `web.rs:275-300` | `pub fn readonly(&self) -> bool` **beibehalten**, aber umgebaut: liest nicht mehr ein eigenes Feld, sondern liefert `self.method == WebMethod::Get` — Doku warnt explizit, dass `true` nur „Methode ist GET" bedeutet, nicht „Operation ist nebenwirkungsfrei" (das war exakt die F-031-Schwachstelle) | F-031 |
| `web.rs:432-438` (`invoke`, Tracing-Span) | `readonly = self.readonly` → `method = ?self.method` (strukturiertes Feld, `Debug`-Formatierung, da `WebMethod` kein `Display` hat) | F-031 |

**Entscheidung, `readonly()` nicht zu entfernen**: der Brief verlangt nur, dass die readonly-*Semantik* jetzt aus
`method == WebMethod::Get` abgeleitet wird — nicht, dass der Accessor verschwindet. Er als reiner
Bequemlichkeits-Wrapper ist harmlos, solange seine Doku unmissverständlich macht, dass er keine
Sicherheits-Aussage mehr über Nebenwirkungsfreiheit trifft. Das erspart einen Bruch aller Aufrufer außerhalb
meiner Zuständigkeit — Gegenprobe: `docs/remediation/ledger/W3M/CLI-WEB.md` (parallel entstanden, unabhängig)
bestätigt in seiner Pflichtlektüre exakt diese Erwartung für `harw-cli/src/web.rs:709,715` und musste dort
entsprechend **nicht** auf einen entfernten Accessor reagieren.

### 3.2 Moduldoku (F-031-Kontext ergänzt)

`web.rs:12-34` — Abschnitt „Warum keine dritte Autorisierungslogik entsteht" umformuliert (nennt jetzt `method`
statt `readonly` als erste Achse) und neuer Abschnitt „F-031: keine Methode mehr aus `readonly` abgeleitet"
eingefügt (erklärt die Schwachstelle und die Migration in Adapter-Doku-Tiefe, spiegelt die Begründung aus
`operation.rs`s `WebMethod`-Doku). `web.rs:51-54,121-124`: Nebenläufigkeits-Doku von „`&'static str`, `bool` und
`Copy`-Enums" auf „`&'static str` und `Copy`-Enums" korrigiert (kein `bool`-Feld mehr in `WebAdapter`).

### 3.3 `OpOutput`/`OperationMeta`-Nachzug (Doku-Beispiel + 6 Test-Fixtures)

| Zeile | Änderung |
|---|---|
| `web.rs:80-92` (Doku-Beispiel) | `readonly: true` → `method: WebMethod::Get`; `args_schema: None,` + `output_schema: None,`; `OpOutput { text: … }` → `OpOutput::from(…)` |
| `web.rs:556-574` (`SingleWebOp`) | `method: WebMethod::Get`, `output_schema: None,`, `OpOutput::from("single")` |
| `web.rs:584-607` (`TwoWebOp`) | Route 1 `method: WebMethod::Get`, Route 2 `method: WebMethod::Post` (vormals `readonly: true`/`false`) |
| `web.rs:619-637` (`MixedSurfaceOp`) | `method: WebMethod::Get` |
| `web.rs:653-671` (`EchoXOp`) | `method: WebMethod::Post` (vormals `readonly: false`) |
| `web.rs:685-696` (`AlwaysErrOp`) | `method: WebMethod::Post` (vormals `readonly: false`) |
| `web.rs:534-551` (`NoSurfaceOp`) | nur `OpOutput::from("noop")` + `output_schema: None,` (keine `Surface::Web`) |

Gegenprobe: `args_schema: None,`-Zählung == `output_schema: None,`-Zählung == 7 (1 Doku + 6 Fixtures).

### 3.4 Testanpassungen (Assertions)

| Zeile | Änderung |
|---|---|
| `web.rs:715` | `test_from_operation_single_web_surface_…`: neue Assertion `assert_eq!(adapters[0].method(), WebMethod::Get);` **vor** der bestehenden `assert!(adapters[0].readonly());` (beide bleiben — belegen sowohl neuen Accessor als auch abgeleiteten alten) |
| `web.rs:727,730` | `test_from_operation_two_web_surfaces_…`: analog für beide Routen (`method()` und `readonly()`) |
| `web.rs:494-497` | Test-Modul-Import: `WebMethod` ergänzt |

## 4. `harw-operations/src/adapter/model_tool.rs` — nur `OpOutput`/`OperationMeta`-Nachzug

`ModelToolAdapter`/`Surface::ModelTool` sind von F-031 **nicht** betroffen — `Surface::ModelTool.readonly` bleibt
unverändert bestehen (eigene, von `Surface::Web` unabhängige Semantik, vom Auftrag ausdrücklich nicht erwähnt).
Nur §5.1 (F-222) einschlägig.

| Zeile | Änderung |
|---|---|
| `model_tool.rs:326-331` (Doku-Beispiel) | `output_schema: None,` ergänzt; `OpOutput::from("ok")` |
| `model_tool.rs:646-653` (`NoSurfaceOp`) | `output_schema: None,`; `OpOutput::from("no-surface-ok")` |
| `model_tool.rs:674-679` (`ReadonlyModelToolOp`) | wie oben, `"readonly-ok"` |
| `model_tool.rs:700-705` (`WritingModelToolOp`) | wie oben, `"writing-ok"` |
| `model_tool.rs:732-737` (`MixedSurfaceOp`) | wie oben, `"mixed-ok"` |
| `model_tool.rs:758-770` (`EchoXOp`) | `output_schema: None,`; `Ok(OpOutput { text: x.to_owned() })` → `Ok(OpOutput::from(x.to_owned()))` |
| `model_tool.rs:792-793` (`AlwaysErrOp`) | nur `output_schema: None,` (kein `OpOutput`, immer `Err`) |
| `model_tool.rs:817-832` (`AuthorityEchoOp`) | `output_schema: None,`; `Ok(OpOutput { text: format!(…) })` → `Ok(OpOutput::from(format!(…)))` |
| `model_tool.rs:846-847` (`NamedModelToolOp::run`) | `Ok(OpOutput::from("named-model-tool-ok"))` |
| `model_tool.rs:874-891` (`named_model_tool_op_with_schema`) | `output_schema: None,` nach dem Shorthand-Feld `args_schema,` ergänzt (dieses Literal übernimmt `args_schema` als Funktionsparameter, `output_schema` bleibt hart auf `None` — kein Anwendungsfall für ein konfigurierbares Ausgabe-Schema in diesem Test) |
| `model_tool.rs:1024-1032` (`CountRawArgsOp`) | `output_schema: None,`; `Ok(OpOutput::from(input.invocation.raw_args().len().to_string()))` |

Gegenprobe: `args_schema: None,`/`args_schema,`-Zählung == `output_schema: None,`-Zählung == 10 (1 Doku + 9
Test-Fixtures, davon eine mit Shorthand-Feld).

## 5. `harw-operations/src/registry.rs` — `WebPathCollision` auf `(path, method)` umgestellt

### 5.1 Design-Entscheidung: Kollision auf dem Paar `(path, method)`, nicht mehr auf `path` allein

Vorher prüfte `try_register` nur `path`-Gleichheit zwischen zwei `Surface::Web`-Einträgen. Mit explizitem `method`
(F-031) ist das zu strikt: `GET /api/session` und `POST /api/session` sind zwei legitime, unterschiedliche Routen
(Lese- und Schreibpfad unter derselben URL) — genau das Muster, das ein HTTP-Router ohnehin als zwei getrennte
Routen behandelt. Eine Kollision liegt nur noch vor, wenn **beide**, `path` **und** `method`, übereinstimmen.

| Zeile | Änderung | Befund |
|---|---|---|
| `registry.rs:45` | Import: `WebMethod` ergänzt | F-031 |
| `registry.rs:49-56` | Enum-Doku (`RegistryError`): „web-path check is an exact, case-sensitive string match" → „web-route check compares the full `(path, method)` pair" | F-031 |
| `registry.rs:62-64` | `Self::WebPathCollision`-Kurzbeschreibung: „identical `path`" → „identical `(path, method)` pair" | F-031 |
| `registry.rs:89-123` | `WebPathCollision`-Variante: neues Feld `method: WebMethod` ergänzt; Doku um Abschnitt „Why `(path, method)`, not `path` alone" erweitert (erklärt die Design-Entscheidung explizit, mit Beispiel) | F-031 |
| `registry.rs:145-155` (`Display`) | `"web path '{path}' is claimed by …"` → `"web route '{method:?} {path}' is claimed by …"` (z. B. `"web route 'Get /api/status' is claimed by …"`) | F-031 |
| `registry.rs:322-329,345-346,361-362` (Doku von `try_register`) | Beschreibung der vierten Prüfung von „path matches" auf „`(path, method)` pair matches" präzisiert, inkl. Hinweis, dass derselbe Pfad unter anderer Methode **keine** Kollision ist | F-031 |
| `registry.rs:452-482` (`try_register`, Prüfung 4) | Destrukturierung erweitert um `method: new_method`/`method: ex_method`; Kollisionsbedingung `new_path == ex_path` → `new_path == ex_path && new_method == ex_method`; `RegistryError::WebPathCollision`-Konstruktion um `method: *new_method` ergänzt | F-031 |

Der Variantenname `WebPathCollision` (nicht umbenannt) — `harw-web/src/router.rs:427` matcht ihn bereits als
`RegistryError::WebPathCollision { .. }` (Wildcard, laut Grep vor dieser Änderung); ein neues Feld hinter `..`
bricht diesen Matcher nicht. Eine Umbenennung des Variantennamens war nicht gefordert und hätte diesen
Konsumenten unnötig riskiert (außerhalb meiner Zuständigkeit, nicht verifizierbar ohne `cargo check`).

### 5.2 Tests

| Zeile | Änderung |
|---|---|
| `registry.rs:691-694` | Test-Modul-Import: `WebMethod` ergänzt |
| `registry.rs:719-722` (`TestOp::meta`) | `output_schema: None,` ergänzt |
| `registry.rs:1000-1027` (`test_try_register_returns_err_on_web_path_collision`) | beide `Surface::Web`-Literale `readonly: true` → `method: WebMethod::Get`; `matches!`-Pattern um `method` erweitert, Bedingung um `method == WebMethod::Get` ergänzt |
| `registry.rs:1030-1051` (`test_try_register_allows_distinct_web_paths`) | beide Literale `readonly: true` → `method: WebMethod::Get` (Verhalten unverändert: verschiedene `path`, kollidiert weiterhin nicht) |
| `registry.rs:1053-1078` (**neu**: `test_try_register_allows_same_path_with_different_methods`) | Regressionstest für die Design-Entscheidung §5.1: `GET /api/session` + `POST /api/session` registrieren sich beide erfolgreich (`registry.len() == 2`) |
| `registry.rs:1085-1096` (`test_web_path_collision_display_mentions_both_owners_and_path`) | Literal um `method: WebMethod::Get,` ergänzt |
| `registry.rs:1075-1085,1102-1112,1139-1149,1194-1204` (`ModelOp`, `MiniOp`, `BadOp`, `StableOp` in den Alias-/Self-Collision-Tests) | je `output_schema: None,` ergänzt (kein `Surface::Web`-Bezug, reine Kontraktfolge) |

Gegenprobe: `args_schema: None,`-Zählung == `output_schema: None,`-Zählung == 5.

## 6. `harw-operations/tests/operations.rs` — Integrationstests

| Zeile | Änderung | Befund |
|---|---|---|
| `operations.rs:13-17` | Import: `WebMethod` ergänzt | F-031 |
| `operations.rs:32-50` (`EchoOp::meta`) | `output_schema: None,` ergänzt (Command- + ModelTool-Surface unverändert — `Surface::ModelTool.readonly` bleibt) | F-222 |
| `operations.rs:53-58` (`EchoOp::run`) | `Ok(OpOutput { text: joined })` → `Ok(OpOutput::from(joined))` | F-222 |
| `operations.rs:73-85` (`FailingOp::meta`) | `output_schema: None,` ergänzt | F-222 |
| `operations.rs:397-421` (`test_op_output_{equality,inequality}_via_public_reexport`) | beide `OpOutput`-Literale um `data: None,` ergänzt (bewusst **nicht** auf `OpOutput::from` umgestellt — dieser Test prüft explizit die volle Struct-Literal-Syntax über den öffentlichen Re-Export, nicht die `From<String>`-Bequemlichkeitsroute) | F-222 |
| `operations.rs:495-513` (`WebOp::meta`) | `Surface::Web { path, readonly: true, approval }` → `{ path, method: WebMethod::Get, approval }`; `output_schema: None,` ergänzt | F-031, F-222 |
| `operations.rs:516-531` (`WebOp::run`) | `Ok(OpOutput { text: echoed.to_owned() })` → `Ok(OpOutput::from(echoed.to_owned()))` | F-222 |
| `operations.rs:531-547` (`test_web_op_meta_surface_is_web`) | Pattern `Surface::Web { path, readonly, approval }` → `{ path, method, approval }`; `assert!(*readonly)` → `assert_eq!(*method, WebMethod::Get)` | F-031 |

Gegenprobe: `args_schema: None,`-Zählung == `output_schema: None,`-Zählung == 3 (`EchoOp`, `FailingOp`, `WebOp`).

## 7. `harw-macros/tests/operation.rs` — `web(...)`-Attributgrammatik + `Surface::Web`-Assertions

Integrationstest der `harw-macros`-Crate (kompiliert konkrete `#[operation(...)]`-Aufrufe gegen die **bereits
migrierte** `expand_operation`-Implementierung aus C-OPS, `harw-macros/src/operation.rs` — nur gelesen, nicht
verändert).

| Zeile | Änderung | Befund |
|---|---|---|
| `operation.rs:31-37` (`test_noop`) | `Ok(OpOutput { text: "ok".to_owned() })` → `Ok(OpOutput::from("ok".to_owned()))` | F-222 |
| `operation.rs:60-63` (`session_status`) | wie oben, `String::new()` | F-222 |
| `operation.rs:96-100,101-104` (`catalog_read`) | `OpOutput::from(String::new())`; `model_tool(readonly, …)` **unverändert** (ModelTool-Achse, nicht F-031) | F-222 |
| `operation.rs:140-146` (`knowledge_search`) | `OpOutput::from(String::new())` | F-222 |
| `operation.rs:188-201` (`test_agent`) | `OpOutput::from("ok".to_owned())` | F-222 |
| `operation.rs:242-263` (`test_web_only`, Test `web_surface_method_and_fields_match`, vormals `web_surface_defaults_and_fields_match`) | Attribut `web(path = "…", readonly, approval = "none")` → `web(path = "…", method = "get", approval = "none")`; Assertion-Literal `readonly: true` → `method: WebMethod::Get`; Import `WebMethod` ergänzt; `OpOutput::from` | F-031, F-222 |
| `operation.rs:281-306` (`test_web_write`, Test **umbenannt** `web_surface_without_readonly_key_defaults_to_false` → `web_surface_with_explicit_post_method`) | Attribut ohne `readonly`-Schlüssel (vormals impliziter `false`-Default → `POST`) → jetzt **Pflichtangabe** `method = "post"`; Assertion `readonly: false` → `method: WebMethod::Post`; Kommentar erklärt den Semantikwechsel (F-031: kein impliziter Default mehr) | F-031, F-222 |
| `operation.rs:318-350` (`test_triple_surface`) | `web(path = "/api/triple", readonly, approval = "none")` → `web(path = "/api/triple", method = "get", approval = "none")`; `Surface::ModelTool { readonly: true, .. }`-Assertion **unverändert** (ModelTool-Achse); `Surface::Web`-Assertion `readonly: true` → `method: WebMethod::Get`; `OpOutput::from` | F-031, F-222 |

### 7.1 Negativfall „web ohne `method`" — keine trybuild-UI-Fixture vorhanden

Auftrag: „neuer Negativfall „web ohne method" falls trybuild-UI-Tests existieren (sonst im Ledger vermerken)."
Befund: `harw-macros/tests/compile_fail.rs` (`trybuild::TestCases::compile_fail("tests/compile_fail/*.rs")`) deckt
ausschließlich die `#[derive(FromRawArgs)]`-Diagnosen ab (`tests/compile_fail/{01..33}_*.rs`, 34 Fixtures,
Kommentar `compile_fail.rs:1-8`: „Compile-fail test suite for the `#[derive(FromRawArgs)]` hardened diagnostics").
**Keine** dieser Fixtures verwendet `#[operation(web(...))]` — vollständiger Grep über alle 34 `.rs`-Dateien in
`tests/compile_fail/` auf `web(` und `#\[.*operation`: 0 Treffer außer `08_unknown_authority_reducer.rs`, das
`#[harw_macros::operation(...)]` mit `command`/`agent_tool`, aber keinem `web(...)`-Schlüssel nutzt. Es gibt also
**keine** trybuild-Suite für `#[operation(...)]`-Attributfehler (z. B. `web(...)` ohne `method`) — diese Diagnose
wird laut C-OPS (`harw-macros/src/operation.rs`, nicht meine Zuständigkeit) über den regulären Compile-Error-Pfad
des Makros erzeugt und aktuell **nirgends** durch einen automatisierten Test abgedeckt (weder trybuild noch ein
`#[test]`, der einen Kompilierfehler erwartet — das ist in `rustc`/`cargo test` ohne trybuild/eigene
Prozess-Kompilierung nicht direkt prüfbar).

**Empfehlung an eine Folgewelle** (kein W3M-Owner für eine neue trybuild-Suite in `harw-macros` benannt): eine
neue Datei `harw-macros/tests/compile_fail_operation.rs` mit eigener `trybuild::TestCases` und Fixtures unter
`tests/compile_fail_operation/` einführen (getrennt von der bestehenden `FromRawArgs`-Suite, um deren Snapshots
nicht zu berühren), mindestens mit einem Fall `web(path = "/x", approval = "none")` (kein `method`) → erwarteter
Fehler laut C-OPS `harw-macros/src/operation.rs`: `` `web(...)` requires a `method = "get"` or `method = "post"` key ``.
Da dies eine **neue** Testinfrastruktur-Datei wäre (nicht nur eine Änderung bestehender Fixtures) und explizit
außerhalb meines Owned-Scopes läge (`harw-macros/tests/**` gehört zwar mir, aber die Anlage einer *neuen*
Test-Suite mit eigenem `#[test] fn ui()` ist eine Scope-Erweiterung, die der Brief nicht verlangt hat — er
verlangt nur, den Negativfall zu ergänzen **falls** die Infrastruktur existiert), wird sie hier nur vermerkt statt
umgesetzt.

## 8. `harw-macros/tests/compile_fail/08_unknown_authority_reducer.rs` — kosmetischer Nachzug (kein Verhaltenseinfluss)

| Zeile | Änderung |
|---|---|
| `08_unknown_authority_reducer.rs:36-38` | `Ok(harw_operations::OpOutput { text: String::new() })` → `Ok(harw_operations::OpOutput::from(String::new()))` |

**Warum keine `.stderr`-Änderung nötig** (bestätigt C-OPS §5.1 letzte Zeile): `#[harw_macros::operation(...)]`
bricht beim Parsen von `authority = "reduce_to_read_excute"` mit einem `syn::Error`, den `expand_operation`
(`harw-macros/src/operation.rs`, nicht meine Zuständigkeit) als **alleinige** Ersatz-Ausgabe (`compile_error!(...)`)
zurückgibt — der ursprüngliche Funktionskörper (inkl. der `OpOutput`-Konstruktion) wird dabei **verworfen**, nie
Teil des vom Compiler tatsächlich typgeprüften Codes. Ob die Zeile die alte oder neue `OpOutput`-Form nutzt, ändert
daher nichts an der emittierten Diagnose (`08_unknown_authority_reducer.stderr`, unverändert, 1 Fehlerblock) — die
Änderung dient ausschließlich der Lesbarkeits-/Stilkonsistenz mit dem Rest der Codebasis.

## 9. Gesamt-Gegenprobe (Grep, nach allen Änderungen)

| Prüfung | Befehl (sinngemäß) | Ergebnis |
|---|---|---|
| Keine bare `OpOutput {`-Literale mehr in Owned Files außer den zwei bewusst vollständigen `text`+`data`-Fällen | `grep -rn "OpOutput {" harw-operations/src/adapter/ harw-operations/src/registry.rs harw-operations/tests/ harw-macros/tests/` | Nur `tests/operations.rs:400,404,413,417` (die vier Zeilen der zwei `test_op_output_{equality,inequality}`-Literale, je mit `data: None,`) |
| `args_schema`/`output_schema`-Zählung je Datei gleich | `grep -c` je Datei | `command.rs` 7/7, `model_tool.rs` 10/10, `web.rs` 7/7, `registry.rs` 5/5, `tests/operations.rs` 3/3 |
| Kein verbleibendes `readonly` in `Surface::Web`-Kontext | `grep -rn readonly …` + manuelle Sichtprüfung jeder Fundstelle | Alle verbleibenden Treffer sind `Surface::ModelTool`, `make_readonly_tool_op`/`readonly-tool` (Registry-Fixture-Namen für ModelTool), Doku-Prosa (`WebAdapter::readonly` als weiterhin gültiger Accessor-Name) oder mein eigener Erklärkommentar — keiner betrifft ein `Surface::Web`-Feld mehr |
| `WebMethod` überall importiert, wo verwendet | `grep -l "WebMethod::"` | `web.rs`, `registry.rs`, `tests/operations.rs`, `harw-macros/tests/operation.rs` — alle vier haben den Import |

## 10. Folgearbeit außerhalb meiner Zuständigkeit

- §7.1: optionale neue trybuild-Suite `harw-macros/tests/compile_fail_operation/` für „`web(...)` ohne `method`" —
  kein Owner benannt, hier nur empfohlen (Scope-Erweiterung, nicht im Brief verlangt).
- Kein weiterer Punkt: die in C-OPS §5.1/§5.3 für meine Owned Files gelisteten Fundstellen sind vollständig
  abgearbeitet; alle anderen dort gelisteten Dateien (`harw-ops/**`, `harw-web/**`, `harw-cli/**`,
  `harw-core-bridge/**`, `harw-tui/**`, `xtask/**`, `harw-runtime/**`) gehören laut C-OPS bzw. den parallel
  entstandenen `W3M/{OPS-1,OPS-2,OPS-3,WEB,CLI-WEB,RT,MISC}.md`-Ledgern zu anderen Agenten.

## 11. dep-request

Keine — `serde`/`serde_json` bereits vorhanden (siehe C-OPS §4), keine neue Dependency für diese Änderungen
benötigt (keine Manifest-Datei angefasst).
