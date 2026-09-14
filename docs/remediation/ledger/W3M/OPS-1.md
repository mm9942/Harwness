# W3-M — Agent OPS-1: `OpOutput`/`web(readonly)`-Nachzug in `harw-ops` (Teil 1/N)

Owned: `harw-ops/src/{agent,analyze,approval,attach,context_proposal,diff,effort,explore}.rs`, dieses Ledger (neu).

BUILD-POLICY eingehalten: nichts gebaut/geprüft/getestet/formatiert, keine git-Schreibbefehle, keine
Manifest-Änderung, kein `cargo add`. Einziger erlaubter Befehl `cargo metadata --offline --no-deps
--format-version 1` — nicht benötigt, da keine Abhängigkeitsfrage offen war. Verifikation ausschließlich durch
Lesen/Grep: Klammer-/Struktur-Kontrolle je Datei (`{`/`}`-Zählung ausgeglichen in allen acht Dateien, siehe §4),
vollständiger Re-Grep auf `OpOutput {`, `readonly` und `web(path` nach den Änderungen.

Pflichtlektüre gelesen: `docs/remediation/AGENT-BRIEF.md`; `docs/remediation/ledger/W3/C-OPS.md` (gefrorene
Signaturen `OpOutput { text, data: Option<serde_json::Value> }` + `impl From<String> for OpOutput`,
`Surface::Web { path, method: WebMethod, approval }`, Makro-Grammatik `web(path=…, method="get"|"post",
approval=…)` mit `readonly` als unsupported-key-Fehler, §5.1/§5.3/§5.4 Fundstellenlisten für `harw-ops/**`);
`docs/remediation/ledger/W3M/RT.md` nur zur Konvention (keine inhaltliche Überschneidung — RT betrifft
`harw-runtime`/`harw-types`, hat dort keine Fundstellen).

## 1. Aufgabe

Mechanischer, verhaltensgleicher Nachzug in den acht Owned Files auf den in C-OPS eingefrorenen Vertrag:

1. Jedes `OpOutput { text: X }`-Literal → `OpOutput::from(X)`.
2. Jedes `web(..., readonly, ...)`/`web(..., readonly = true|false, ...)`-Makroattribut → `method = "get"`
   (readonly) bzw. `method = "post"` (mutierend); `harw-ops/src/analyze.rs` erhält laut Auftrag **bewusst**
   `method = "post"` trotz deklariertem `readonly` (F-031 — siehe §3).
3. `model_tool(readonly, …)` und `Surface::ModelTool { readonly, .. }` sind **nicht** Teil dieses Verträges
   (andere Makro-/Enum-Achse) und bleiben unverändert.

## 2. `OpOutput`-Konversion je Datei (Zeile alt → neu)

| Datei | Zeile(n) alt | Fundstelle alt | Neu | Hinweis |
|---|---|---|---|---|
| `agent.rs` | 148-150 | `Ok(OpOutput { text: "Keine aktiven Child-Agents.".to_owned() })` | `Ok(OpOutput::from("Keine aktiven Child-Agents.".to_owned()))` | — |
| `agent.rs` | 159-162 | `Ok(OpOutput { text: lines.join("\n") })` (eingebettetes `\n` als echtes Zeilenende im String-Literal, unverändert übernommen) | `Ok(OpOutput::from(lines.join("\n")))` | eingebettetes Zeilenende beibehalten, nicht auf `"\n"`-Escape umgestellt (keine Verhaltensänderung) |
| `agent.rs` | 172-174 | `Ok(OpOutput { text: format!("Cancellation für Child-Agent {target} angefordert.") })` | `Ok(OpOutput::from(format!(…)))` | — |
| `approval.rs` | 334-336 | `Ok(OpOutput { text: "Keine offenen Genehmigungsanfragen.".to_owned() })` | `Ok(OpOutput::from(…))` | — |
| `approval.rs` | 345 | `Ok(OpOutput { text: buf })` | `Ok(OpOutput::from(buf))` | — |
| `approval.rs` | 483-492 | `Ok(OpOutput { text: format!(…) })` (5-Parameter-Format) | `Ok(OpOutput::from(format!(…)))` | — |
| `attach.rs` | 113-118 | `Ok(OpOutput { text: format!("Job {}\nState: {:?}\nRevision: {}", …) })` | `Ok(OpOutput::from(format!(…)))` | — |
| `context_proposal.rs` | 187-189 | `Ok(OpOutput { text: "Keine Kontextprogramm-Vorschläge.".to_owned() })` | `Ok(OpOutput::from(…))` | — |
| `context_proposal.rs` | 199 | `Ok(OpOutput { text: buf })` | `Ok(OpOutput::from(buf))` | — |
| `context_proposal.rs` | 206-208 | `Ok(OpOutput { text: render_proposal(&proposal) })` | `Ok(OpOutput::from(render_proposal(&proposal)))` | — |
| `context_proposal.rs` | 268-273 | `Ok(OpOutput { text: format!(…) })` | `Ok(OpOutput::from(format!(…)))` | — |
| `diff.rs` | 327 | `Ok(OpOutput { text })` — `text` ist bereits fertig gerendertes `git diff`-Stdout (`render_shell_output`), kein strukturiertes JSON | `Ok(OpOutput::from(text))` | — |
| `effort.rs` | 133 | `Ok(OpOutput { text })` | `Ok(OpOutput::from(text))` | — |
| `effort.rs` | 139-141 | `Ok(OpOutput { text: "Reasoning-Effort zurückgesetzt (Provider-Default)".to_string() })` | `Ok(OpOutput::from(…))` | — |
| `effort.rs` | 153-155 | `Ok(OpOutput { text: format!("Reasoning-Effort gesetzt: {other}") })` | `Ok(OpOutput::from(format!(…)))` | — |
| `explore.rs` | 432 | `Ok(OpOutput { text })` in `finding_output()` — `text` ist bereits ein `serde_json::to_string_pretty`-String eines strukturierten `payload`-JSON-Werts | `Ok(OpOutput::from(text))` | **Bewusst nicht** auf `OpOutput { text, data: Some(payload) }` umgebaut — laut Auftrag ist das Verschieben strukturierter Daten in das neue `data`-Feld Sache von W5 WB-OPS, nicht dieses Nachzugs. Nur die mechanische `From`-Konversion (Verhalten identisch: `data: None`) wurde angewendet. |
| `analyze.rs` | 1105 (jetzt 1110) | `Ok(OpOutput { text })` in `render()` — `text` ist ebenfalls ein bereits serialisierter JSON-Bericht | `Ok(OpOutput::from(text))` | dieselbe Begründung wie `explore.rs`: `data`-Feld nicht befüllt, das ist W5 WB-OPS |

17 Konversionsstellen insgesamt, alle mit `OpOutput::from(...)` (keine Stelle nutzte die alternative
`OpOutput { text: X, data: None }`-Schreibweise — `From` war überall die lokal lesbarere Form, da jede
Fundstelle bereits einen einzelnen `text`-Ausdruck baute).

## 3. `web(...)`-Makroattribut: `readonly` → `method` je Datei (Zeile alt → neu)

| Datei | Zeile | Alt | Neu | Begründung |
|---|---|---|---|---|
| `attach.rs` | 99 | `web(path = "/api/attach", readonly, approval = "none")` | `web(path = "/api/attach", method = "get", approval = "none")` | reine Inspektion über `JobStore`, keine Mutation (Moduldoku-Kommentar direkt darüber bereits vorhanden, Kommentartext an neue Schlüsselbezeichnung angepasst) |
| `approval.rs` | 325 | `web(path = "/api/approval-pending", readonly, approval = "none")` | `web(path = "/api/approval-pending", method = "get", approval = "none")` | listet nur, mutiert nichts |
| `approval.rs` | 449 (jetzt 447) | `web(path = "/api/approval-resolve", approval = "none")` (**kein** `readonly`-Schlüssel vorhanden — Operation war schon vorher ohne `readonly` deklariert) | `web(path = "/api/approval-resolve", method = "post", approval = "none")` | **Auftrag**: „jede Operation, die Zustand ändert … post" — `approval.resolve` verbraucht einen `ApprovalRecord` einmalig (Moduldoku Zeile 17-18 der Datei: „`approval.resolve` **ändert** einen Zustand"); `method` fehlte hier ganz (nicht nur `readonly`→`method` umbenannt, sondern echte Ergänzung, da vor dieser Welle keine `method`-Angabe existierte und `method` laut C-OPS-Makrogrammatik ein Pflichtfeld ist) |
| `diff.rs` | 291 | `web(path = "/api/diff", readonly, approval = "none")` | `web(path = "/api/diff", method = "get", approval = "none")` | reiner Lesevorgang (`git diff`), Kommentar direkt darüber bestätigt dieselbe Achse wie `model_tool(readonly, approval = "always")` (unverändert, andere Makro-Achse) |
| `explore.rs` | 485 | `web(path = "/api/explore", readonly, approval = "none")` | `web(path = "/api/explore", method = "get", approval = "none")` | Kindagent läuft mit `reduce_to_read_only`-Autorität, Operation selbst schreibt nichts |
| `analyze.rs` | 879 (jetzt 884) | `web(path = "/api/analyze", readonly, approval = "none")` | `web(path = "/api/analyze", method = "post", approval = "none")` | **F-031, Sicherheitsfix laut Auftrag zwingend** — siehe §3.1 |

`agent.rs`, `context_proposal.rs` und `effort.rs` deklarieren **keine** `web(...)`-Fläche (nur `command(...)`);
keine Änderung an diesen drei Attributen nötig oder vorgenommen — durch Grep bestätigt (0 Treffer für `web(` in
allen drei Dateien).

### 3.1 F-031-Fix in `analyze.rs`: bewusste Abweichung von der bisherigen `readonly`-Deklaration

Der Auftrag verlangt ausdrücklich `method = "post"` für `harw-ops/src/analyze.rs`, **obwohl** die Operation vor
dieser Welle `readonly` deklarierte — Befund F-031 (Register `x-findings-register-w1-w3.md:359`, referenziert in
C-OPS §5.4: „hier muss die Folgewelle bewusst `method = "post"` wählen, nicht `"get"`, um die Schwachstelle
tatsächlich zu schließen"). Beleg im eigenen Code (gelesen, nicht verändert — Zeilen liegen außerhalb des
`#[operation(...)]`-Attributs, innerhalb derselben Owned-Datei):

- `analyze.rs` implementiert bottom-up-Analyse über Analyst-Kindagenten und persistiert das Ergebnis dauerhaft im
  Plan-Store sowie startet einen Fan-out (Funktionskörper von `async fn analyze`, ab Zeile ~886 in dieser Fassung)
  — ein durabler Schreibpfad, keine reine Anzeige.
- `model_tool(readonly, approval = "none")` (Zeile 876) bleibt **unverändert** — diese Achse ist nicht Teil des
  `web(...)`-Verträges und bezieht sich auf die ModelTool-Fläche, deren Kindagenten mit reduzierter,
  lesender Autorität laufen (Doku-Kommentar direkt daneben ergänzt, warum das keinen Widerspruch zur
  `method = "post"`-Web-Fläche darstellt).
- Der bisherige Doku-Kommentar über der `web(...)`-Zeile („Web-Fläche übernimmt dieselbe Achse wie das ModelTool
  … keine Mutation") war laut F-031 sachlich falsch für die Web-Fläche und wurde durch eine Begründung ersetzt,
  die die tatsächliche Mutation und die Sicherheitsbegründung (GET-Route mit Nebenwirkungen wäre die
  Schwachstelle) benennt.

Damit ist die in C-OPS §5.4 benannte Pflichtarbeit für `harw-ops/src/analyze.rs:879` erledigt.

## 4. Verifikation (durch Lesen)

- Alle acht Dateien: `{`/`}`-Zählung ausgeglichen (`agent.rs` 62/62, `analyze.rs` 248/248, `approval.rs` 106/106,
  `attach.rs` 42/42, `context_proposal.rs` 83/83, `diff.rs` 103/103, `effort.rs` 25/25, `explore.rs` 87/87).
- Re-Grep `OpOutput {` über alle acht Dateien: nur noch drei Treffer, alle in Doku-Prosa (`/// `Ok(OpOutput {
  text })` …` in `approval.rs:307,418` und `context_proposal.rs:93`) — keine Code-Literale mehr. Diese
  Prosa-Stellen beschreiben nur noch generisch die Rückgabeform und sind kein kompilierter Code; nicht Teil des
  Auftrags (mechanische Literal-Konversion), unverändert belassen.
- Re-Grep `readonly` über alle acht Dateien: nur noch `model_tool(readonly, …)` (`analyze.rs:876`, `diff.rs:288`,
  `explore.rs:481`), `Surface::ModelTool { readonly, .. }` (`explore.rs:659`), ein Testfixture-Feld
  `Surface::ModelTool`-Vergleich (`diff.rs:599`, testet `Surface::ModelTool { readonly: true, approval: … }` —
  **keine** `Surface::Web`-Konstruktion, unberührt) und eine Doku-Zeile (`explore.rs:26`). Kein
  `web(..., readonly, ...)` mehr vorhanden.
- Re-Grep `web(path` über alle acht Dateien: sechs Treffer, alle mit `method = "get"` oder `method = "post"`,
  keiner mehr mit `readonly` (siehe Tabelle §3).
- Keine der acht Dateien konstruiert oder destrukturiert `Surface::Web { .. }` bzw. `WebMethod` direkt — die
  `web(...)`-Attributsyntax wird ausschließlich vom `#[operation]`-Makro (`harw-macros/src/operation.rs`, W3
  C-OPS) in `Surface::Web { path, method, approval }` expandiert. Kein `use harw_operations::WebMethod`-Import
  in einer der acht Dateien nötig oder hinzugefügt.
- Keine Test-Assertion in den acht Dateien prüft `Surface::Web`/`WebMethod` (geprüft:
  `context_proposal.rs:341-360`, `explore.rs:633-684`, `analyze.rs:1260-1278`, `diff.rs:592-603` — alle
  bestehenden Surface-Assertions betreffen `Surface::Command`, `Surface::AgentTool` oder `Surface::ModelTool`,
  keine davon musste angepasst werden).
- `diff.rs:599` (`Surface::ModelTool { readonly: true, approval: ApprovalPolicy::Always }`) bewusst **nicht**
  geändert — testet die ModelTool-Fläche, die vom C-OPS-Vertrag nicht berührt wird (nur `Surface::Web` verlor
  `readonly`).

**Kompiliert erst mit den unter §5 der C-OPS-Ledger genannten Voraussetzungen** (u. a. `harw-operations`,
`harw-macros` müssen bereits die neue Signatur tragen) — Build-Policy dieses Knotens verbietet ohnehin jeden
Build-/Check-Lauf.

## 5. Tests

Keine Tests in den acht Owned Files konstruierten `OpOutput { .. }` oder `Surface::Web { .. }` literal (geprüft
per Grep, §4) — daher keine Testanpassung in diesen Dateien nötig. Die drei o. g. Doku-Prosa-Stellen sind keine
Doctests (kein ` ```rust ` … `-Codeblock, nur Fließtext in Backticks) und daher nicht compile-relevant.

## 6. Folgearbeit außerhalb dieses Knotens

- C-OPS §5.1/§5.3/§5.4 listen weitere `harw-ops/*.rs`-Dateien (u. a. `work.rs`, `ps.rs`, `help.rs`,
  `provider.rs`, `plugins.rs`, `model.rs`, `status.rs`, `quit.rs`, `permissions.rs`, `stop.rs`, `skills.rs`,
  `memory.rs`, `mode.rs`, `plan.rs`, `goal.rs`, `research.rs`, `lib.rs`), die **nicht** in diesem Auftrag Owned
  Files sind — bleiben für einen Folgeknoten (W3-M OPS-2 o. ä.).
- `harw-operations/src/adapter/web.rs`, `harw-web/src/router.rs`, `xtask/src/webui.rs` (readonly-Leser der
  `Surface::Web`-Semantik) bleiben wie in C-OPS §5.3 benannt unverändert und unangetastet von diesem Knoten.
- Das `data`-Feld von `OpOutput` bleibt in `analyze.rs::render()` und `explore.rs::finding_output()` bewusst
  `None`, obwohl beide Funktionen bereits ein strukturiertes `serde_json::Value` vor der Serialisierung besitzen
  (`payload`/`report`) — Verschieben dieser Werte in `data` ist W5 WB-OPS, nicht Teil dieses mechanischen
  Nachzugs.

## 7. Zusammenfassung

- 8 Owned Files, 17 `OpOutput { text }` → `OpOutput::from(...)`-Konversionen, 6 `web(..., readonly, ...)` →
  `web(..., method = "get"|"post", ...)`-Umstellungen (davon 1 Sicherheitsfix F-031 in `analyze.rs`: `readonly`
  deklariert, aber `method = "post"` gewählt; 1 Ergänzung eines zuvor fehlenden `method`-Schlüssels in
  `approval.rs:447`, `approval.resolve`, ebenfalls `post`).
- `model_tool(readonly, …)` und `Surface::ModelTool { readonly, .. }` (4 Fundstellen in `analyze.rs`, `diff.rs`,
  `explore.rs` ×2) bewusst unverändert — andere Vertragsachse.
- Keine Tests angepasst (keine betroffenen Test-Literale in den Owned Files).
- Keine Build-Policy-Verletzung; Verifikation vollständig durch Lesen/Grep.
