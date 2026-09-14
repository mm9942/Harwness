# W3M — Agent OPS-3: `OpOutput`-Nachzug + `web(readonly)` → `method` in harw-ops (Teil 1/8er-Slice)

Owned: `harw-ops/src/{plugins,provider,ps,quit,skills,status,stop,work}.rs`, dieses Ledger.
BUILD-POLICY eingehalten: nichts gebaut/geprüft/getestet, keine git-Schreibbefehle, keine Manifest-Änderung.
Verifikation durch Lesen (Grep-Gegenprobe: keine `OpOutput { text` / `web(...readonly...)`-Reste, alle vier
`web(...)`-Stellen tragen jetzt `method = "get"|"post"`; keine `#[cfg(test)]`-Blöcke in diesen 8 Dateien
konstruieren `OpOutput`/`web(...)`/`OperationMeta`/`Surface::Web` separat — geprüft, keine Treffer).

Pflichtlektüre gelesen: `docs/remediation/AGENT-BRIEF.md`, `docs/remediation/ledger/W3/C-OPS.md` (neuer Vertrag
`OpOutput { text, data } + From<String>`, `Surface::Web { path, method: WebMethod, approval }`, Makro
`web(path=…, method="get"|"post", approval=…)`, `OperationMeta.output_schema`; insbesondere §5.1 und §5.4, die
diese 8 Dateien als Folgearbeit ohne benannten Owner auflisten).

`harw-ops/tests/`: per Grep geprüft (`OpOutput`, `web(.*readonly`, `OperationMeta\s*{`) — kein Treffer außer
`approval_declaration_gate.rs`, das ausschließlich `Surface::ModelTool { readonly, approval }` verwendet
(unverändert laut C-OPS-Vertrag, `readonly` bleibt dort bestehen) → nicht in meinem Owned-Umfang, nicht angefasst.

## 1. Änderungen je Datei (Datei:Zeile alt → neu)

### `harw-ops/src/plugins.rs`
| Zeile (alt) | Alt | Neu |
|---|---|---|
| 128-130 | `return Ok(OpOutput { text: "Keine Plugins konfiguriert.".to_owned() });` | `return Ok(OpOutput::from("Keine Plugins konfiguriert.".to_owned()));` |
| 143-145 | `Ok(OpOutput { text: lines.join("\n") })` | `Ok(OpOutput::from(lines.join("\n")))` |

Kein `web(...)` in dieser Datei (nur `command(...)`) — kein Method-Nachzug nötig.

### `harw-ops/src/provider.rs`
| Zeile (alt) | Alt | Neu |
|---|---|---|
| 241 | `Ok(OpOutput { text })` | `Ok(OpOutput::from(text))` |
| 249 | `Ok(OpOutput { text })` | `Ok(OpOutput::from(text))` |
| 268 | `Ok(OpOutput { text })` | `Ok(OpOutput::from(text))` |
| 325-327 | `Ok(OpOutput { text: lines.join("\n") })` | `Ok(OpOutput::from(lines.join("\n")))` |
| 416-418 | `Ok(OpOutput { text: format!("provider switched to {canonical_target}; next turn will use it") })` | `Ok(OpOutput::from(format!("provider switched to {canonical_target}; next turn will use it")))` |
| 443-447 | `return Ok(OpOutput { text: "No default provider configured — no test possible. …".to_owned() });` | `return Ok(OpOutput::from("No default provider configured — no test possible. …".to_owned()));` |
| 451-457 | `return Ok(OpOutput { text: format!("Default provider '{default_name}' is listed …") });` | `return Ok(OpOutput::from(format!("Default provider '{default_name}' is listed …")));` |
| 484-493 | `Ok(OpOutput { text: format!("Default provider : {default_name} …", api = provider_toml.api) })` | `Ok(OpOutput::from(format!("Default provider : {default_name} …", api = provider_toml.api)))` |

Kein `web(...)` in dieser Datei (nur `command(...)`) — kein Method-Nachzug nötig, obwohl der Brief
„Provider-Wechsel" unter den mutierenden Beispielen nennt: Grep bestätigt, dass `#[operation(...)]` in
`provider.rs` keine `web(...)`-Fläche deklariert; die Erwähnung im Brief war eine generische Beispielliste über
den gesamten OPS-3-Umfang, keine spezifische Fundstelle in dieser Datei.

### `harw-ops/src/ps.rs`
| Zeile (alt) | Alt | Neu |
|---|---|---|
| 89-92 | `model_tool(readonly, approval = "none"), … web(path = "/api/ps", readonly, approval = "none")` | `model_tool(readonly, approval = "none"), … web(path = "/api/ps", method = "get", approval = "none")` |
| 106-108 | `return Ok(OpOutput { text: "No jobs.".to_owned() });` | `return Ok(OpOutput::from("No jobs.".to_owned()));` |
| 121 | `Ok(OpOutput { text })` | `Ok(OpOutput::from(text))` |

`model_tool(readonly, …)` unverändert — `Surface::ModelTool.readonly` ist per C-OPS-Vertrag nicht betroffen, nur
`Surface::Web` verlor `readonly`. `web(...)` war bereits `readonly` (reines Auflisten) → `method = "get"` erhält
das Verhalten.

### `harw-ops/src/quit.rs`
| Zeile (alt) | Alt | Neu |
|---|---|---|
| 62-64 | `Ok(OpOutput { text: "__QUIT__".to_owned() })` | `Ok(OpOutput::from("__QUIT__".to_owned()))` |

Kein `web(...)` in dieser Datei (nur `command(...)`) — kein Method-Nachzug nötig, obwohl der Brief `quit` unter
den mutierenden Beispielen nennt: Grep bestätigt keine `web(...)`-Fläche in `quit.rs`.

### `harw-ops/src/skills.rs`
| Zeile (alt) | Alt | Neu |
|---|---|---|
| 155-157 | `return Ok(OpOutput { text: "Keine Skills konfiguriert.".to_owned() });` | `return Ok(OpOutput::from("Keine Skills konfiguriert.".to_owned()));` |
| 167-169 | `Ok(OpOutput { text: lines.join("\n") })` | `Ok(OpOutput::from(lines.join("\n")))` |
| 182-191 | `Ok(OpOutput { text: format!("{} (enabled={}): {} …", skill.name, …) })` | `Ok(OpOutput::from(format!("{} (enabled={}): {} …", skill.name, …)))` |

Kein `web(...)` in dieser Datei (nur `command(...)`) — kein Method-Nachzug nötig.

### `harw-ops/src/status.rs`
| Zeile (alt) | Alt | Neu |
|---|---|---|
| 17 (Moduldoku) | `` `Ok(OpOutput { text })` wird immer zurückgegeben. `` | `` `Ok(OpOutput::from(text))` wird immer zurückgegeben. `` |
| 54 (Doku) | `` `Ok(OpOutput { text })` mit mehrzeiligem Statustext. `` | `` `Ok(OpOutput::from(text))` mit mehrzeiligem Statustext. `` |
| 72-77 | `model_tool(readonly, approval = "none"), // … readonly, weil … web(path = "/api/status", readonly, approval = "none")` | `model_tool(readonly, approval = "none"), // … \`method = "get"\`, weil … web(path = "/api/status", method = "get", approval = "none")` |
| 92 | `Ok(OpOutput { text })` | `Ok(OpOutput::from(text))` |

Kommentarzeilen 73-76 (Begründung der Web-Fläche) auf `method = "get"` umformuliert, Bedeutung unverändert
(reiner Statusabruf, keine Mutation). `model_tool(readonly, …)` unverändert.

### `harw-ops/src/stop.rs`
| Zeile (alt) | Alt | Neu |
|---|---|---|
| 79-84 | `model_tool(approval = "always"), // … kein readonly … web(path = "/api/stop", approval = "always")` | `model_tool(approval = "always"), // … \`method = "post"\` … web(path = "/api/stop", method = "post", approval = "always")` |
| 109-114 | `Ok(OpOutput { text: format!("Cancelled job {} (revision {}).", transition.work_id, transition.revision) })` | `Ok(OpOutput::from(format!("Cancelled job {} (revision {}).", transition.work_id, transition.revision)))` |

`web(...)` trug bereits **kein** `readonly` (Kommentar erklärte das explizit, siehe Moduldoku Zeile 7-8: Abbruch
ist Seiteneffekt) — per §5.4 des C-OPS-Ledgers fehlte `method` hier komplett und musste ergänzt werden:
`method = "post"` gewählt, weil `/stop` einen laufenden Job dauerhaft in `Cancelled` überführt (F-031-Fall,
mutierend). Moduldoku Zeile 7-8 (`ModelTool` ohne `readonly`) unverändert — bezieht sich auf `Surface::ModelTool`,
das von diesem Vertragswechsel nicht betroffen ist.

### `harw-ops/src/work.rs`
| Zeile (alt) | Alt | Neu |
|---|---|---|
| 50-52 | `// … readonly, approval = "none". web(path = "/api/work", readonly, approval = "none")` | `// … \`method = "get"\`, approval = "none". web(path = "/api/work", method = "get", approval = "none")` |
| 60-62 | `Ok(OpOutput { text: render_work_panel(&counts) })` | `Ok(OpOutput::from(render_work_panel(&counts)))` |

**Entscheidung dokumentiert:** Der Aufgabentext nennt „work" in der Beispielliste mutierender Operationen
(→ `post`). Code- und Moduldoku-Befund widerspricht dem: Modulkopf (Zeile 1-21, unverändert) beschreibt `/work`
explizit als „durable Job-**Übersicht**", „schreibgeschützte Übersicht der im JobStore persistierten Jobs",
„Die Operation ist schreibgeschützt" (Concurrency-Abschnitt), und der Funktionskörper (`list_state_counts`,
`render_work_panel`) ruft ausschließlich `store.list(...)` auf, keine Store-Mutation. Das bestehende `web(...)`
trug bereits `readonly` vor dieser Migration. Da die Aufgabe „verhaltensgleich" (mechanisch) sein soll und
`readonly` das eindeutige Signal für die bisherige GET-Semantik ist, wurde `method = "get"` gewählt — nicht
`post`. Die Beispielliste im Aufgabentext war eine generische Orientierung über den gesamten OPS-3-Umfang, keine
spezifische Zeilenangabe für `work.rs`; sie widerspricht hier der tatsächlichen, dokumentierten Nicht-Mutation.
Sollte eine spätere Sicherheitsprüfung (F-031-Nachfolge) `work` dennoch als schreibend einstufen wollen, ist das
ein Verhaltens-/Architekturentscheid außerhalb des mechanischen Scopes dieses Agents — bitte gesondert prüfen.

## 2. Zusammenfassung Methode-Zuordnung (F-031)

| Datei | `web(...)` vorher | `method` nachher | Begründung |
|---|---|---|---|
| `ps.rs` | `readonly` | `get` | reines Auflisten laufender Jobs (Store-Read, keine Mutation) |
| `status.rs` | `readonly` | `get` | reiner Statusabruf (`session_id()`/`turn_id()`, keine Mutation) |
| `work.rs` | `readonly` | `get` | dokumentiert schreibgeschützte Job-Übersicht, keine Mutation (siehe §1 Entscheidung oben) |
| `stop.rs` | kein `readonly`, `method` fehlte ganz | `post` | bricht Job dauerhaft ab (`store.cancel(...)`), F-031-mutierend |
| `plugins.rs` | kein `web(...)` | — | keine Web-Fläche vorhanden |
| `provider.rs` | kein `web(...)` | — | keine Web-Fläche vorhanden |
| `quit.rs` | kein `web(...)` | — | keine Web-Fläche vorhanden |
| `skills.rs` | kein `web(...)` | — | keine Web-Fläche vorhanden |

## 3. `OpOutput`-Literale — Übersicht (alle mechanisch auf `OpOutput::from(...)` migriert)

| Datei | Stellen (alt Zeilen) |
|---|---|
| `plugins.rs` | 128, 143 |
| `provider.rs` | 241, 249, 268, 325, 416, 443, 451, 484 |
| `ps.rs` | 106, 121 |
| `quit.rs` | 62 |
| `skills.rs` | 155, 167, 182 |
| `status.rs` | 92 |
| `stop.rs` | 109 |
| `work.rs` | 60 |

Keine Stelle setzt zusätzlich `data` — überall reicht `OpOutput::from(text_ausdruck)` (Verhalten identisch zu
`OpOutput { text: text_ausdruck, data: None }`).

## 4. `OperationMeta`/`Surface::Web`-Testliterale

Keine gefunden in den 8 Owned-Dateien (weder Produktionscode noch `#[cfg(test)]`-Module). Kein Nachzug nötig.

## 5. API-Nachweise (gelesen)

| Item | Beleg |
|---|---|
| `impl From<String> for OpOutput` (neuer Vertrag) | `docs/remediation/ledger/W3/C-OPS.md` §1, Zeilen 25-27 |
| `WebMethod`-Werte `"get"`/`"post"`, Makro-Grammatik `web(path=…, method="get"\|"post", approval=…)` | `docs/remediation/ledger/W3/C-OPS.md` §1, Zeilen 46-53 |
| `Surface::ModelTool { readonly, approval }` bleibt unverändert (nur `Surface::Web` verlor `readonly`) | `docs/remediation/ledger/W3/C-OPS.md` Zeile 35, 93-97 |
| Keine `OperationMeta`/`Surface::Web`-Literale in `harw-ops/src/{plugins,provider,ps,quit,skills,status,stop,work}.rs` oder `harw-ops/tests/**` außerhalb `Surface::ModelTool` | eigener Grep (§0 dieses Ledgers) |

## 6. dep-request

Keine — keine neue Abhängigkeit nötig, reine Struct-Literal-/Attribut-Migration in bereits vorhandenem Code.

## 7. Folgearbeit / offene Punkte außerhalb meiner Dateien

- `harw-ops/src/{analyze,agent,attach,help,provider(bereits erledigt oben),context_proposal,effort,diff,model,
  permissions,approval,memory,mode,explore,goal,plan,research}.rs` — weitere `OpOutput`/`web(readonly)`-Stellen
  laut C-OPS-Ledger §5.1/§5.4, **nicht** Teil dieses Slices (andere OPS-Agents / A-OPSPLAN / A-APPR / W9 laut
  dortiger Owner-Zuordnung).
- `harw-operations/src/lib.rs` Re-Export von `WebMethod` (C-OPS §5.2) — nicht meine Zuständigkeit.
- `harw-web/src/router.rs` (eigener `WebMethod`-Enum + `expected_for(readonly)`) — höchste Sicherheitspriorität
  laut C-OPS §5.3, WB-SRV (W5) zuständig.

## 8. Tests

Keine Tests in den 8 Owned-Dateien betroffen (keine `#[cfg(test)]`-Blöcke konstruieren `OpOutput`/`web(...)` in
diesen Dateien — per Grep bestätigt, §0). Verhalten aller migrierten Stellen ist by-construction identisch
(`OpOutput::from(x)` ≡ `OpOutput { text: x, data: None }`; `method = "get"|"post"` ersetzt die vorige
`readonly: bool`-Achse 1:1 für die vier betroffenen Web-Flächen ohne Bedeutungsänderung der Admission-Erwartung
an dieser Stelle — die tatsächliche Durchsetzung an `harw-web/src/router.rs` ist Folgearbeit, siehe §7).
