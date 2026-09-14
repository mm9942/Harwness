# W3-M — Agent RT: `Surface::Web { readonly }` → `WebMethod`, `OpOutput`/`OperationMeta` in harw-runtime

Owned: `harw-runtime/src/assembly.rs`, `harw-runtime/src/services.rs`, `harw-runtime/tests/**`,
`harw-types/src/principal.rs` (nur falls Code — nicht Doku — `Surface::Web` bzw. `readonly` betrifft), dieses Ledger
(neu). NICHT: `harw-cli/**` (F-MAIN läuft dort).

BUILD-POLICY eingehalten: nichts gebaut/geprüft/getestet/formatiert, keine git-Schreibbefehle, keine Manifest-Änderung.
Einziger Befehl `cargo metadata --offline --no-deps --format-version 1` → Exit 0. Verifikation ausschließlich durch
Lesen/Grep.

Pflichtlektüre gelesen: `docs/remediation/AGENT-BRIEF.md`, `docs/remediation/ledger/W3/C-OPS.md` (gefrorene
Signaturen `OpOutput{text,data}`, `WebMethod{Get,Post}`, `Surface::Web{path,method,approval}`,
`OperationMeta.output_schema`; §5 Folgearbeitsliste), `docs/remediation/ledger/W2d2/F-RT.md` (zuletzt geänderte
`assembly.rs` — Projektkontext-Filterung `registry_project_context`/`ensure_narrowing_fits_operations`, **nicht
zurückgedreht**, nur gelesen).

## 1. Ergebnis: keine Fundstellen in den Owned Files

Auftrag war, jede Code-Stelle mit `Surface::Web { .., readonly, .. }` (Pattern und Literal) auf `method: WebMethod`
umzustellen, `readonly`-als-„nur lesend"-Auswertungen auf `method == WebMethod::Get` zu heben, `OpOutput { text }` →
`data: None` und `OperationMeta`-Literale → `output_schema: None` zu ergänzen, in den vier Owned Files. Grep-Befund
(vollständig, keine Auslassung):

| Datei | `Surface::Web` | `readonly` | `OpOutput` | `OperationMeta` |
|---|---|---|---|---|
| `harw-runtime/src/assembly.rs` | 1 Treffer, Zeile 1161 — Doku-Prosa `[Surface::Web]` als Verweislink, kein Pattern/Literal | 1 Treffer, Zeile 2210 — Testname `test_narrowing_readonly_on_full_entry_drops_write_tools`; betrifft `RegistryProfile::ReadOnlyExplore` (Narrowing-Profil), **nicht** `Surface::Web.readonly` — geprüft: Test konstruiert kein `Surface::Web`, keine Änderung nötig | 0 | 0 |
| `harw-runtime/src/services.rs` | 0 (Treffer sind `ServiceSurface::Web`, ein anderer Enum aus `harw-runtime` selbst, Zeilen 88, 173, 686, 713, 880) | 0 | 0 | 0 |
| `harw-runtime/tests/rights_matrix.rs` (einzige Datei in `tests/**`) | 0 | 0 | 0 | 1 Treffer, Zeile 665 — Doku-Prosa `` `ModelToolAdapter::tool_name` ist `OperationMeta::name` ``, kein Literal |
| `harw-types/src/principal.rs` | 0 (alle `*Surface`-Treffer sind `IngressSurface::Web`, ein von `harw_operations::Surface` unabhängiger Enum, siehe `harw-types/src/principal.rs:70` `pub enum IngressSurface`) | 0 | 0 | 0 |

Keine der vier Dateien konstruiert, destrukturiert oder matcht `harw_operations::operation::Surface::Web`,
`OpOutput` oder `OperationMeta`. Die einzigen Wortfunde sind (a) Doku-Prosa-Verweise auf den fremden Typ
(`assembly.rs:1161`, `rights_matrix.rs:665`), (b) ein Testname mit dem Wort „readonly", der sich auf das
Narrowing-Konzept `RegistryProfile::ReadOnlyExplore` bezieht (Vor-C-OPS-Vokabular, unabhängig vom
`Surface::Web.readonly`-Feld), und (c) der bereits vorhandene, unabhängige Enum `ServiceSurface`
(`harw-runtime` definiert `ServiceSurface::{Web, Job, …}` als Dienst-Flächen-Marker, nicht als Alias für
`harw_operations::Surface`) bzw. `IngressSurface` (`harw-types`, Zugangswege-Marker: Tui/Cli/Web/Mcp/…). Für keinen
dieser vier Fälle ist eine Code-Änderung fällig — **keine Datei wurde verändert**.

### 1.1 Warum `harw-types/src/principal.rs` unberührt bleibt

Auftragsbedingung: „nur falls dort Code — nicht Doku — `Surface::Web` bzw. `readonly` betroffen ist". Die Datei
definiert `IngressSurface` (Zugangsweg eines Principals: Tui/Cli/Web/Mcp/Telegram/Gateway/JobWorker/Child,
`harw-types/src/principal.rs:70-79`) — ein Domänentyp von `harw-types`, unabhängig von
`harw_operations::operation::Surface` (Betriebsflächen einer Operation: Command/ModelTool/AgentTool/Web). Beide
Enums tragen zufällig eine `Web`-Variante, sind aber unterschiedliche Typen ohne Konvertierung zwischeneinander in
dieser Datei. Kein `readonly`-Feld, kein `OpOutput`, kein `OperationMeta` in der Datei (Grep bestätigt: 0 Treffer für
alle drei Suchbegriffe). Bedingung des Auftrags nicht erfüllt → Datei bleibt unverändert.

## 2. Folgearbeit außerhalb Owned Files: `harw-cli` (Auftrag: Zeilen benennen)

`harw-cli/**` ist laut Auftrag NICHT meine Zuständigkeit (F-MAIN läuft dort) — nur gelesen, nicht verändert.

### 2.1 `harw-cli/src/runtime_web.rs`

Keine Fundstelle. Grep auf `readonly`, `Surface::Web`, `OpOutput`, `OperationMeta`, `WebMethod`: 0 Treffer. Die drei
`*Surface`-Treffer (Zeilen 88, 91, 240) sind ausschließlich `IngressSurface::Web` (Principal-Konstruktion für
eingehende Web-Requests), kein Bezug zu `harw_operations::Surface`.

### 2.2 `harw-cli/src/web.rs`

| Zeile | Fundstelle | Einordnung |
|---|---|---|
| 693, 695 | Doku-Kommentar: „`Surface::Web`, kein `Surface::ModelTool` — der CommandsOnly-Filter …" | Prosa, kein Code-Literal; beschreibt korrekt weiterhin gültiges Verhalten (`CommandsOnly` behält jede Operation mit Command- oder Web-Fläche) — keine Änderung durch diese Migration nötig |
| 709 | `assert!(pending.readonly(), "approval.pending is declared readonly");` | **Code-Fundstelle**: ruft `WebAdapter::readonly()` auf (`pending` kommt aus `WebRouteTable::find`, `harw-web/src/router.rs:267` → `Option<&WebAdapter>`, `WebAdapter` ist in `harw-operations/src/adapter/web.rs` definiert). `readonly()` selbst steht in `harw-operations/src/adapter/web.rs:255` — bereits von C-OPS §5.3 als Migrationsziel benannt (`WebAdapter.readonly: bool` → `method: WebMethod`). Sobald die Methode dort entfernt/umbenannt wird, muss diese Zeile auf `pending.method() == WebMethod::Get` (oder äquivalent) umgestellt werden. |
| 715 | `assert!(!resolve.readonly(), "approval.resolve is declared mutating");` | Gleiche Einordnung wie Zeile 709, für `resolve` (`/api/approval-resolve`). |

Beide Fundstellen (709, 715) sind Konsumenten der in C-OPS §5.3 bereits benannten `harw-operations/src/adapter/web.rs`-Migration und dort noch **nicht** in der dortigen Fundstellenliste aufgeführt (C-OPS §5.3 nennt
`harw-operations/src/adapter/web.rs` und `harw-operations/tests/operations.rs` als Leser/Schreiber, nicht diesen
`harw-cli`-Testaufrufer). Ergänzung für die Folgewelle: Wer `harw-operations/src/adapter/web.rs` auf `method:
WebMethod` umstellt, muss zusätzlich `harw-cli/src/web.rs:709,715` (Test `test_web_route_table_from_registry_includes_approval_routes`,
Zeile 697) nachziehen — kein W3/W4a-Owner für `harw-cli/src/web.rs` in dieser Migration benannt (F-MAIN ist mit
anderem Befund befasst).

## 3. Zusammenfassung

- Owned Files (`harw-runtime/src/assembly.rs`, `harw-runtime/src/services.rs`, `harw-runtime/tests/rights_matrix.rs`,
  `harw-types/src/principal.rs`): **keine Änderung** — Grep bestätigt Abwesenheit jedes `Surface::Web{readonly}`-,
  `OpOutput{text}`- und `OperationMeta{…}`-Literals bzw. jeder entsprechenden Auswertung.
- `harw-runtime/src/assembly.rs:2210` (`test_narrowing_readonly_on_full_entry_drops_write_tools`) und
  `harw-runtime/tests/rights_matrix.rs:665` geprüft und als false positives (anderes Konzept bzw. reine Doku)
  eingestuft — dokumentiert, damit eine Folgewelle sie nicht erneut prüfen muss.
- Folgearbeit außerhalb meiner Zuständigkeit benannt: `harw-cli/src/web.rs:709,715` (`.readonly()`-Aufrufe auf
  `WebAdapter`, hängt an `harw-operations/src/adapter/web.rs`-Migration aus C-OPS §5.3); `harw-cli/src/runtime_web.rs`
  ohne Fundstelle.
