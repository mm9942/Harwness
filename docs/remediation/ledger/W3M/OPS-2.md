# W3-M — Agent OPS-2: `OpOutput`/`web(readonly)`-Nachzug in `harw-ops` (Teil 2/N)

Owned: `harw-ops/src/{goal,help,lib,memory,model,mode,permissions,plan}.rs`, dieses Ledger (neu).

BUILD-POLICY eingehalten: nichts gebaut/geprüft/getestet/formatiert, keine git-Schreibbefehle, keine
Manifest-Änderung, kein `cargo add`. Einziger erlaubter Befehl `cargo metadata --offline --no-deps
--format-version 1` ausgeführt (Exit 0, Workspace-Struktur unverändert auflösbar — keine Abhängigkeitsfrage
offen). Verifikation ausschließlich durch Lesen/Grep: `{`/`}`-Zählung je Datei ausgeglichen (§4), vollständiger
Re-Grep auf `OpOutput {`, `readonly` und `web(path` nach den Änderungen (§4).

Pflichtlektüre gelesen: `docs/remediation/AGENT-BRIEF.md`; `docs/remediation/ledger/W3/C-OPS.md` (gefrorene
Signaturen `OpOutput { text, data: Option<serde_json::Value> }` + `impl From<String> for OpOutput`,
`Surface::Web { path, method: WebMethod, approval }`, Makro-Grammatik `web(path=…, method="get"|"post",
approval=…)` mit `readonly` als unsupported-key-Fehler, `OperationMeta.output_schema`, §5.1/§5.3/§5.4
Fundstellenlisten für `harw-ops/**`); zusätzlich `docs/remediation/ledger/W3M/OPS-1.md` und
`docs/remediation/ledger/W3M/OPS-3.md` (Parallel-Slices desselben Nachzugs — insbesondere OPS-1 §3.1: F-031-Fix
`analyze.rs` → `method = "post"`, relevant für die `EXPECTED_WEB_SURFACES`-Tabelle in meinem `lib.rs`).

## 1. Aufgabe

Mechanischer, verhaltensgleicher Nachzug in den acht Owned Files auf den in C-OPS eingefrorenen Vertrag:

1. Jedes `OpOutput { text: X }`-Literal → `OpOutput::from(X)`.
2. Jedes `web(..., readonly, ...)` / fehlendes `method` → `method = "get"` (lesend) bzw. `method = "post"`
   (mutierend); mutierende Operationen unter meinen Owned Files (`plan`, `goal`, `permissions`) erhalten
   `method = "post"`, auch wo bisher kein `readonly`-Schlüssel stand (Attribut hatte `method` schlicht noch nie).
3. Handgeschriebene `Surface::Web`-Test-Literale/-Destrukturierungen in `lib.rs` auf `method: WebMethod` umgestellt.
4. Beide `OperationMeta`-Literale (`lib.rs`, `help.rs`-Testfixture) um `output_schema: None` ergänzt.
5. `model_tool(readonly, …)` und `Surface::ModelTool { readonly, .. }` sind **nicht** Teil dieses Vertrages
   (andere Makro-/Enum-Achse) und bleiben unverändert — keine Vorkommen in meinen Owned Files ohnehin
   (`mode.rs`/`model.rs`/`memory.rs` verweigern die ModelTool-Fläche ganz, siehe deren Moduldoku).

## 2. `OpOutput`-Konversion je Datei (Zeile alt → neu)

| Datei | Zeile(n) alt | Fundstelle alt | Neu |
|---|---|---|---|
| `goal.rs` | 489 | `Ok(OpOutput { text })` | `Ok(OpOutput::from(text))` |
| `help.rs` | 262 | `Ok(OpOutput { text })` | `Ok(OpOutput::from(text))` |
| `help.rs` (Test) | 315-318 | `Ok(OpOutput { text: String::new() })` | `Ok(OpOutput::from(String::new()))` |
| `lib.rs` | 164-166 | `OpOutput { text: "Session compaction is not available in this runtime.".to_owned() }` | `OpOutput::from("Session compaction is not available in this runtime.".to_owned())` |
| `memory.rs` | 102 | `Ok(OpOutput { text })` (`render_hot`) | `Ok(OpOutput::from(text))` |
| `memory.rs` | 117 | `Ok(OpOutput { text })` (`render_stats`) | `Ok(OpOutput::from(text))` |
| `memory.rs` | 138-140 | `Ok(OpOutput { text: format!("Keine Treffer für: {}", …) })` (`render_recall`, leerer Treffer-Zweig) | `Ok(OpOutput::from(format!("Keine Treffer für: {}", …)))` |
| `memory.rs` | 151 | `Ok(OpOutput { text: buf })` (`render_recall`) | `Ok(OpOutput::from(buf))` |
| `memory.rs` | 203-205 | `Ok(OpOutput { text: format!("Signal '{label}' aufgezeichnet.") })` (`record`) | `Ok(OpOutput::from(format!("Signal '{label}' aufgezeichnet.")))` |
| `memory.rs` | 212-219 | `Ok(OpOutput { text: format!(…4 Parameter…) })` (`run_maintain`) | `Ok(OpOutput::from(format!(…)))` |
| `model.rs` | 387-389 | `Ok(OpOutput { text: format!("model switched to {configured_id}; next turn will use it") })` | `Ok(OpOutput::from(format!(…)))` |
| `model.rs` | 406 | `Ok(OpOutput { text })` | `Ok(OpOutput::from(text))` |
| `mode.rs` | 250 | `Ok(OpOutput { text })` — `text` ist bereits ein `serde_json::to_string_pretty`-String eines strukturierten Berichts | `Ok(OpOutput::from(text))` |
| `permissions.rs` | 114-121 | `Ok(OpOutput { text: format!(…5 Parameter…) })` (`permissions`, Zweig `show`) | `Ok(OpOutput::from(format!(…)))` |
| `permissions.rs` | 158-163 | `Ok(OpOutput { text: format!(…2 Parameter…) })` (`set_mode`) | `Ok(OpOutput::from(format!(…)))` |
| `plan.rs` | 743 | `Ok(OpOutput { text })` | `Ok(OpOutput::from(text))` |

16 Konversionsstellen, alle mit `OpOutput::from(...)` (keine Stelle brauchte die alternative
`OpOutput { text: X, data: None }`-Schreibweise). `mode.rs:250` liefert bereits einen JSON-String als `text` —
das `data`-Feld bleibt bewusst `None` (Verschieben nach `data` ist laut Auftrag/C-OPS §2 Sache von W5, nicht
dieses Nachzugs — „JSON-String-Ausgaben nicht umbauen").

Doku-Prosa (`/// \`Ok(OpOutput::from(text))\` mit …`) wurde an drei Stellen (`goal.rs:299`, `memory.rs:63`,
`plan.rs:459`) von der vorherigen `Ok(OpOutput { text })`-Notation auf die tatsächlich zurückgegebene Form
umgestellt — reine Doku-Genauigkeit, keine Verhaltensänderung.

## 3. `web(...)`-Makroattribut: `readonly`/fehlendes `method` → `method` (Zeile alt → neu)

| Datei | Zeile | Alt | Neu | Begründung |
|---|---|---|---|---|
| `help.rs` | 251 | `web(path = "/api/help", readonly, approval = "none")` | `web(path = "/api/help", method = "get", approval = "none")` | reine Auflistung der registrierten Commands, kein Seiteneffekt (Moduldoku-Kommentar direkt darüber, Wortlaut auf `method = "get"` angepasst) |
| `goal.rs` | 326 | `web(path = "/api/goal", approval = "always")` (kein `readonly`-Schlüssel — Attribut hatte `method` schlicht noch nie) | `web(path = "/api/goal", method = "post", approval = "always")` | gemischte Lese-/Schreib-Sub-Kommandos über einen Aufrufpfad (`set`, `bind`, `achieve`, `abandon` mutieren den Goal-Store); `approval = "always"` unverändert |
| `plan.rs` | 486 | `web(path = "/api/plan", approval = "always")` (kein `readonly`-Schlüssel) | `web(path = "/api/plan", method = "post", approval = "always")` | dieselbe Begründung wie `goal.rs` — Knoten anlegen/pflegen/abgleichen mutieren den Plan-Store |
| `permissions.rs` | 73→74 | `web(path = "/api/permissions", approval = "always")` (kein `readonly`-Schlüssel; Kommentar erklärte bereits „nicht mehr `readonly`, seit `set` den Freigabemodus umschaltet") | `web(path = "/api/permissions", method = "post", approval = "always")` | `/permissions set` schreibt die `ApprovalModeCell` der Sitzung — Kommentarwortlaut auf `method = "post"` angepasst |

`memory.rs`, `model.rs`, `mode.rs` deklarieren **keine** `web(...)`-Fläche (nur `command(...)`, teils
`model_tool(...)`); durch Grep bestätigt (0 Treffer für `web(` in allen drei Dateien) — kein Method-Nachzug nötig.
Keine dieser vier `web(...)`-Stellen trug vor dieser Welle ein `model_tool(readonly, …)`-Gegenstück mit
abweichender Semantik (`goal`/`plan` haben `model_tool(approval = "always")` ohne `readonly`; `help`/`permissions`
haben keine `model_tool(...)`-Fläche).

## 4. `lib.rs`: `Surface::Web`-Testliterale und `OperationMeta`-Literale

`harw-ops/src/lib.rs` ist die einzige meiner acht Dateien, die `Surface::Web` testweise destrukturiert (Tabelle
`EXPECTED_WEB_SURFACES`, Tests `every_exposed_operation_appears_with_its_web_path_in_the_registry` und die
vormals `readonly_web_surfaces_only_appear_on_operations_that_do_not_mutate` benannte Gegenprobe) sowie ein
`OperationMeta`-Literal (`UnavailableCompactOperation::meta`).

- **Import ergänzt**: `use harw_operations::operation::WebMethod;` (WebMethod ist an der Crate-Wurzel von
  `harw-operations` nicht re-exportiert, siehe C-OPS §5.2 — derselbe Importpfad-Stil wie das bereits bestehende
  `use harw_operations::operation::{CommandVisibility, Surface};` in `memory.rs:227`).
- **`EXPECTED_WEB_SURFACES`**: Tupeltyp `(&str, &str, bool, ApprovalPolicy)` → `(&str, &str, WebMethod,
  ApprovalPolicy)`; jeder `bool`-Wert mechanisch übersetzt (`true` → `WebMethod::Get`, `false` →
  `WebMethod::Post`) — **mit einer Ausnahme**: die Zeile für `"analyze"` stand vorher mit `true` (spiegelt die
  alte, laut F-031 falsche `readonly`-Deklaration in `harw-ops/src/analyze.rs`). Agent OPS-1 hat in seinem
  eigenen Owned-File `analyze.rs` bereits den Sicherheitsfix vorgenommen (`method = "post"`, siehe
  `ledger/W3M/OPS-1.md` §3.1) — die Tabelle in meiner Datei folgt daher `WebMethod::Post` für `analyze`, damit
  der Test gegen den tatsächlichen (korrigierten) Code prüft statt gegen die alte Fehldeklaration. Ohne diese
  Korrektur würde der erste Test (`every_exposed_operation_appears_with_its_web_path_in_the_registry`) fehlschlagen,
  sobald das Gesamtsystem wieder kompiliert. Alle übrigen Zeilen (`help`, `status`, `ps`, `diff`, `work`,
  `attach`, `explore`, `research_deps`, `research_web`, `approval.pending` → `Get`; `permissions`, `stop`, `plan`,
  `goal`, `approval.resolve` → `Post`) sind reine 1:1-Übersetzungen ohne inhaltliche Korrektur — `ps`/`status`/
  `work` decken sich mit den in `ledger/W3M/OPS-3.md` dokumentierten Entscheidungen (`get`), `stop` mit `post`.
  `research_deps`/`research_web` (Datei `harw-ops/src/research.rs`, kein Owner in W3-M benannt) bleiben `Get` —
  das Attribut dort trägt weiterhin `readonly` unverändert (nicht meine Zuständigkeit, siehe §6).
- **Destrukturierung** in `every_exposed_operation_appears_with_its_web_path_in_the_registry`:
  `Surface::Web { path: p, readonly: r, approval: a }` → `Surface::Web { path: p, method: m, approval: a }`,
  Vergleich `*r == *readonly` → `*m == *method`, Fehlermeldungstext `readonly: {readonly}` →
  `method: {method:?}` (Debug-Format, da `WebMethod` kein `Display` hat).
- **Test umbenannt**: `readonly_web_surfaces_only_appear_on_operations_that_do_not_mutate` →
  `web_surfaces_use_get_only_for_operations_that_do_not_mutate` (Feld, das der Test prüft, hat sich
  bedeutungstragend umbenannt — Namensänderung folgt demselben Muster wie C-OPS'
  `test_surface_web_readonly_flag_differs` → `test_surface_web_method_differs`). Logik umgestellt von
  `assert_eq!(*readonly, !is_mutating)` auf `let expected = if is_mutating { WebMethod::Post } else {
  WebMethod::Get }; assert_eq!(*method, expected)`; `is_mutating`-Liste um `"analyze"` erweitert (siehe oben,
  sonst Widerspruch zur korrigierten Tabellenzeile).
- Zwei weitere Tests (`no_two_web_surfaces_share_a_path_and_registration_does_not_panic`,
  `negative_model_tool_checks_are_unaffected_by_web_exposure`) destrukturieren `Surface::Web { .. }` bzw.
  `matches!(s, Surface::Web { .. })` mit Wildcard — unverändert kompilierbar, keine Anpassung nötig.
  `web_surfaces_carry_the_same_permission_tier_as_the_operation` prüft nur `PermissionTier`, unberührt.
- **`OperationMeta`-Literale**: `output_schema: None,` ergänzt in `lib.rs:155` (Produktionscode,
  `UnavailableCompactOperation::meta`) und `help.rs:310` (Testfixture `StubOp::meta`) — je direkt nach
  `args_schema: None,`.
- **Moduldoku-Prosa** (`lib.rs:51-54`, `67-70`, `335-337`) auf `method: WebMethod::Get` bzw. `method`
  umformuliert, wo sie zuvor das jetzt entfernte Feld `readonly` beim Namen nannte — Bedeutung unverändert,
  nur Begriffe an den neuen Vertrag angepasst.

## 5. Verifikation (durch Lesen)

- `cargo metadata --offline --no-deps --format-version 1` → Exit 0, Workspace-Root/Members unverändert auflösbar
  (Manifeste nicht angefasst).
- `{`/`}`-Zählung je Datei ausgeglichen: `goal.rs` 405/405, `help.rs` 61/61, `lib.rs` 74/74, `memory.rs` 66/66,
  `model.rs` 105/105, `mode.rs` 67/67, `permissions.rs` 93/93, `plan.rs` 483/483.
- Re-Grep `OpOutput {` über alle acht Dateien: ein Treffer, `lib.rs:164` (`fn compact_unavailable_output() ->
  OpOutput {` — Funktionssignatur, kein Struct-Literal) — keine Code-Literale mehr.
- Re-Grep `readonly` über alle acht Dateien: ein Treffer, `help.rs:339` (`Surface::ModelTool { readonly: true,
  … }` in einem Test-Fixture-Helper) — **keine** `Surface::Web`-Konstruktion, per C-OPS-Vertrag unverändert.
- Re-Grep `web(path` über alle acht Dateien: vier Treffer (`goal.rs:326`, `help.rs:251`, `permissions.rs:74`,
  `plan.rs:486`), alle mit `method = "get"` oder `method = "post"`, keiner mehr ohne `method` oder mit
  `readonly`.
- Re-Grep `OperationMeta {` über alle acht Dateien: zwei Treffer (`lib.rs:143`, `help.rs:301`), beide jetzt mit
  `output_schema: None,`.
- Re-Grep `Surface::Web` über alle acht Dateien: ausschließlich in `lib.rs` (Moduldoku + Testmodul), keine
  weitere Datei konstruiert oder destrukturiert `Surface::Web`.
- `memory.rs`, `model.rs`, `mode.rs`, `permissions.rs` (Tests): keine `#[cfg(test)]`-Blöcke konstruieren
  `OpOutput`/`web(...)`/`OperationMeta`/`Surface::Web` — geprüft, keine Treffer außer den in §4 behandelten
  `help.rs`/`lib.rs`-Stellen.

**Kompiliert erst mit allen unter C-OPS §5 gelisteten Folgearbeiten** (u. a. `harw-ops/src/{research,agent,
attach,…}.rs` außerhalb meines und OPS-1s/OPS-3s Umfangs, `harw-operations`/`harw-macros` selbst bereits durch
C-OPS geliefert) — Build-Policy dieses Knotens verbietet ohnehin jeden Build-/Check-Lauf.

## 6. Folgearbeit außerhalb dieses Knotens

- `harw-ops/src/research.rs:372,433` (`research_deps`/`research_web`) — `web(..., readonly, ...)` noch
  unverändert, kein W3-M-Owner benannt (C-OPS §5.4). Meine `EXPECTED_WEB_SURFACES`-Tabelle geht von `method =
  "get"` als Zielzustand aus (1:1-Übersetzung der bisherigen `readonly = true`, keine F-031-ähnliche Abweichung
  bekannt) — sollte der zuständige Folgeagent zu einem anderen Ergebnis kommen, muss die Tabellenzeile in
  `lib.rs` entsprechend nachgezogen werden.
- `harw-operations/src/lib.rs`-Re-Export von `WebMethod` (C-OPS §5.2) — nicht meine Zuständigkeit; ich habe
  stattdessen den vollqualifizierten Pfad `harw_operations::operation::WebMethod` verwendet (Ergonomie-Punkt,
  kein Compile-Blocker).
- `harw-web/src/router.rs`, `harw-operations/src/adapter/web.rs`, `xtask/src/webui.rs`, `harw-macros/tests/
  operation.rs`, `harw-operations/tests/operations.rs` — alle laut C-OPS §5.3/§5.1 weiterhin `readonly`-basiert,
  nicht in meinem Owned-Umfang.
- Restliche `harw-ops/*.rs`-Dateien mit `OpOutput`/`web(readonly)`-Fundstellen, die weder OPS-1 noch OPS-3 noch
  ich besitzen (`approval.pending`/`approval.resolve`-Konsumenten außerhalb `harw-ops`, `harw-core-bridge`,
  `harw-tui`, `harw-web`, `harw-operations/src/adapter/**`, `harw-operations/tests/**`) — siehe C-OPS §5.1
  vollständige Liste.

## 7. Zusammenfassung

- 8 Owned Files, 16 `OpOutput { text }` → `OpOutput::from(...)`-Konversionen (davon 1 in einer Test-Fixture in
  `help.rs`), 4 `web(...)`-Attribute auf `method = "get"|"post"` umgestellt (1 reine `readonly`→`method`-
  Umbenennung in `help.rs`; 3 Ergänzungen eines zuvor fehlenden `method`-Schlüssels für mutierende Operationen in
  `goal.rs`, `plan.rs`, `permissions.rs`).
- `lib.rs`: `EXPECTED_WEB_SURFACES`-Tabelle (16 Einträge) und zwei Tests auf `WebMethod` umgestellt, inklusive
  einer inhaltlichen Korrektur (`analyze` → `WebMethod::Post`, synchronisiert mit OPS-1s F-031-Fix in
  `analyze.rs`); zwei `OperationMeta`-Literale um `output_schema: None` ergänzt.
- `model_tool(readonly, …)`/`Surface::ModelTool { readonly, .. }` — keine Vorkommen in meinen Owned Files
  (bewusst unverändert, andere Vertragsachse; das eine `readonly: true` in `help.rs:339` betrifft
  `Surface::ModelTool`, nicht `Surface::Web`).
- Keine Build-Policy-Verletzung; Verifikation vollständig durch Lesen/Grep + `cargo metadata --offline`.
