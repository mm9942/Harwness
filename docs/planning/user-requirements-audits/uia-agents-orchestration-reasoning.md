# Audit: UIA, Agenten, Worker, Orchestrierung und Reasoning

Stand: 2026-09-17  
Master: `docs/planning/user-requirements-from-all-transcripts.md`  
Geprüfter Masterabschnitt: `### UIA, Agenten, Worker und Orchestrierung`, UR-35 bis UR-44 (Master-Zeilen 478–628).  
Zusätzlich geprüft: explizite Reasoning-Anforderung sowie die nachträglich verbindliche UX-Anforderung zur sequenziellen UIA-Provider-/Modellauswahl.

## Prüfmethode und Abgrenzung

Jeder Befund basiert auf tatsächlichem Quellcode bzw. tatsächlichen Agenten- und Konfigurationsdateien, nicht nur auf Master- oder Modulkommentaren. Die angegebenen Zeilen sind die relevanten Implementierungsstellen. Es wurden keine Produktdateien, keine Masterdatei und keine anderen Auditdateien geändert. Gemäß Auftrag wurden weder Cargo/Rust-Toolchain, Tests, `make` noch `codex exec` ausgeführt.

## UR-35 – UIA als echte Identität mit Dateien und echtem ersten Turn

**Status: teilweise**

**Codebelege**

- `harw-cli/src/uia_bootstrap.rs:250-335`: Der Einrichtungsdialog fragt UIA-Name/Identität, Persönlichkeit und Nutzerkontext ab, zeigt eine Vorschau und verlangt vor dem Schreiben eine ausdrückliche Bestätigung.
- `harw-cli/src/uia_bootstrap.rs:376-422`: Die Anlage schreibt `definition.toml`, `agent.toml`, `identity.md`, `Personality.md` und `USER.md`; die fünf Dateien erhalten unter Unix `0600`.
- `harw-config/src/loader.rs:35-70`: Die drei UIA-Dateien werden tatsächlich geladen und als getrennte Modellkontext-Fragmente in der Reihenfolge Identität, Persönlichkeit, Nutzerkontext eingebunden.
- `harw-runtime/src/assembly.rs:1661-1682`: Die aktive UIA-Datei wird aufgelöst und ihre Personalisierungsfragmente in den Root-Kontext übernommen.
- `harw-tui/src/runtime_root.rs:160-171,915-921`: Der Nutzername wird aus `USER.md` gelesen und die TUI erzeugt eine UIA-bezogene Begrüßungszeile; der Loginname ist nur Rückfall.
- `harw-cli/src/chat.rs:899-927`: Der One-shot-Pfad startet einen Turn ausschließlich mit dem übergebenen Nutzerprompt. Ein automatischer Modell-„erster Turn“ bzw. eine UIA-Modellbegrüßung wird dort nicht erzeugt.

**Restlücke**

Die Identität und die sichtbare TUI-Begrüßung sind real, aber die Annahme „Emily begrüßt den Nutzer durch einen echten ersten Modellturn“ ist nicht erfüllt. Die Begrüßungszeile ist UI-Ausgabe; im One-shot gibt es keinen impliziten ersten Modellaufruf.

**Risikoarmer nächster Schritt**

Den bestehenden UIA-Kontext für einen einmaligen, klar markierten Initialturn verwenden, nur im interaktiven UIA-Start und nur wenn die Sitzungshistorie leer ist; One-shot und Resume ausdrücklich unverändert lassen. Dabei fehlenden/sensiblen `USER.md`-Inhalt wie bisher optional behandeln.

## UR-36 – UIA-Erzeugung und Spawn-Fähigkeiten

**Status: erfüllt**

**Codebelege**

- `harw-cli/src/uia_bootstrap.rs:110-132`: Eine konfigurierte `active_uia_definition` bleibt erhalten; ohne UIA wird entweder eine vorhandene UIA ausgewählt oder der Einrichtungsdialog ausgeführt. Eine UIA wird nicht still aus Modelltext erzeugt.
- `harw-cli/src/uia_bootstrap.rs:135-176`: `harw uia new` startet den Dialog explizit erneut, persistiert die neue Definition und aktiviert sie.
- `harw-agent-dsl/src/roles.rs:92-123`: Die geschlossene Rollenmatrix erlaubt der UIA Root-Orchestrator, `uia-worker` und `agent-steward`, aber keine normalen Worker oder beliebige andere Rollen.
- `harw-runtime/src/assembly.rs:1605-1624`: Die UIA wird als organisatorische Root-Rolle montiert; ihre explizite `child_orchestrators`-Liste wird aus der eingefrorenen Definition übernommen.

**Restlücke**

Für den geforderten Initialzustand und die anschließende Rollenbegrenzung ist kein offener produktiver Pfad erkennbar. Die automatische Auswahl einer einzigen bereits vorhandenen UIA (`uia_bootstrap.rs:118-131`) ist ein bewusst anderer Fall als stille Neuanlage.

**Risikoarmer nächster Schritt**

Die vorhandene Trennung beibehalten und nur eine Regression-Diagnose ergänzen, die in der Runtime-Ausgabe die aktivierte UIA-ID und die erlaubten Zielrollen sichtbar macht; keine Änderung an der Spawn-Matrix.

## UR-37 – Agentengestützte Designer für UIA, Orchestratoren und Worker

**Status: teilweise**

**Codebelege**

- `harw-registry-defaults/src/embedded_agents.rs:984-1011,1043-1051`: Für UIA, Root-Orchestrator, Sub-Orchestrator, Worker, UIA-Worker und Agent-Steward existieren getrennte eingebettete Regelwerke.
- `harw-registry-defaults/src/embedded_agents.rs:1054-1105`: UIA und Agent-Steward erhalten Organisations- und Authoring-Wissen; Root-/Sub-Orchestratoren erhalten Organisationswissen; Worker erhalten nur ihr eigenes Regelwerk.
- `harw-registry-defaults/knowledge/roles/uia.md:15-24`: Die UIA soll Nutzer beraten, Definitionen spezifizieren und an `agent-steward` übergeben.
- `harw-registry-defaults/src/agent_definition_tools.rs:1-30,360-533`: Der Agent-Steward kann Definitionen validieren, Rechte-Deltas berechnen und Vorschläge/Bundle-Schreibpfade bearbeiten.

**Restlücke**

Es gibt Rollenwissen und ein Definitionstool, aber keinen nachweisbaren produktiven Designer-/Übersetzungsworkflow, der einen freien Nutzerwunsch zwingend in eine verständliche, konkrete Topologie mit Definitionen für UIA, Orchestratoren und Worker überführt. Die freie Übersetzung bleibt Modellprompt-Verhalten.

**Risikoarmer nächster Schritt**

Eine reine, read-only Design-Operation bzw. ein strukturierter UIA→Steward-Vorschlagspfad ergänzen, der Rolle, Elternrolle, erlaubte Kinder, Budget, Tiefe und Return-Contract ausgibt; Commit weiter ausschließlich über die vorhandene Bestätigungs- und Steward-Grenze.

## UR-38 – Child-Orchestrator-Rechte, Sichtbarkeit und Parallelisierung

**Status: teilweise**

**Codebelege**

- `harw-agent-dsl/src/roles.rs:101-123`: Root-Orchestratoren dürfen Child-Orchestratoren und Worker spawnen; Child-Orchestratoren dürfen Worker und nur grundsätzlich weitere Child-Orchestratoren spawnen; Worker, UIA-Worker und Steward dürfen nichts spawnen.
- `harw-core/src/child_controller.rs:939-955`: `can_delegate_to` verlangt neben der versiegelten Rollenmatrix für Child-Orchestratoren eine exakte namentliche Freigabe.
- `harw-core/src/child_controller.rs:3238-3301`: Die sichtbaren Delegationsziele werden mit denselben Prädikaten wie die Admission gefiltert; bei fehlender Resttiefe ist die Liste leer.
- `harw-core/src/child_controller.rs:3304-3396`: Nicht autorisierte Spawns werden fail-closed abgelehnt.
- `harw-core/src/child_controller.rs:3440-3452,3478-3511`: Aktive-Kind-Limit, Sandbox-Schnitt und Tiefendecke werden vor der Admission durchgesetzt.
- `harw-core-bridge/src/agent_tool.rs:1943-2054,2077-2144`: Fan-out nutzt ein konfiguriertes `max_parallel`, hält die Zahl aktiver Slots ein und führt die Kindläufe mit Budget-/Effort-Clamp aus.

**Restlücke**

Die Rechte- und Sichtbarkeitskette ist implementiert. Der konkrete normative Spezialfall „bis zu fünf Explore-Worker plus ein Evaluations-Worker“ ist jedoch nicht als unveränderliche Standardtopologie belegt; `max_parallel` kommt im Fan-out-Pfad als Aufruf-/Operationsparameter, nicht als genau dieses Muster.

**Risikoarmer nächster Schritt**

Das konkrete Muster als declarative Cell-/Plan-Konfiguration modellieren und nur die bestehende Fan-out-Obergrenze damit begrenzen; die zentrale Admission-Prüfung unverändert als letzte Grenze beibehalten.

## UR-39 – Automatisches Laden der Delegationsregeln und richtige Struktur

**Status: teilweise**

**Codebelege**

- `harw-runtime/src/assembly.rs:1605-1624`: Organisationsrolle, Trace, Ceiling und exakte Child-Orchestrator-Freigaben werden beim Runtime-Aufbau in den `SpawnContext` gelegt.
- `harw-registry-defaults/src/profile.rs:1845-1849`: Das organisationsbezogene Regelwerk wird anhand der tatsächlichen organisatorischen Rolle in die Agentenidentität eingebunden.
- `harw-registry-defaults/knowledge/organization/agent-organization.md:4-32,46-58`: Die Hierarchie und Delegationszuständigkeit UIA→Root→Worker/Sub-Orchestrator sowie die Delegationsfälle sind als Rollenwissen vorhanden.
- `harw-core/src/delegation_visibility.rs:77-138`: Die Modelloberfläche zeigt nur Ziele, die die spätere Admission ebenfalls akzeptiert.

**Restlücke**

Das Laden und Durchsetzen der Regeln ist real. Ein allgemeiner, code-seitig erzwungener Entscheider, der bei jedem komplexen Nutzerauftrag automatisch den verantwortlichen Orchestrator auswählt oder eine begründete Strukturentscheidung ausgibt, ist nicht nachweisbar; die Auswahl bleibt bei den jeweiligen Modell-/Operationspfaden.

**Risikoarmer nächster Schritt**

Vor dem ersten delegierenden Turn eine read-only Strukturentscheidung aus dem vorhandenen `SpawnContext` und den sichtbaren Zielen erzeugen und an UIA/Root melden; keine neue Berechtigungsquelle einführen.

## UR-40 – Projektneutrale Explore-/Analyse-Agenten

**Status: teilweise**

**Codebelege**

- `harw-registry-defaults/agents/explorer.toml:1-18,21-48`: Der Explorer ist generisch für Workspace-/Dependency-Erkundung, read-only und ohne `fs.write`/`shell.exec`; sein Spawn-Vertrag ist deklarativ.
- `harw-ops/src/explore.rs:267-319,475-490`: `/explore` startet einen read-only Explorer-Kindlauf mit festem Return-Contract und Budget.
- `harw-ops/src/analyze.rs:1-30,839-864,981-1032`: `/analyze` ist workspace-generisch, bildet Abhängigkeitswellen und startet Analyst-Kinder per Fan-out mit Parallelitätsgrenze.
- `harw-agent-dsl/src/roles.rs:115-123`: Die organisatorische Rolle `Worker` darf selbst keine dauerhaften Agenten spawnen.

**Restlücke**

Explore und Analyse parallelisieren tatsächlich Informationen, aber der eingebaute Explorer ist organisatorisch ein Worker und kann keine weiteren Agenten spawnen. Sein `[spawn] max_depth = 1` (`explorer.toml:39-45`) erweitert die geschlossene Worker-Matrix nicht. Damit ist „Explore-/Analyse-Agenten haben Spawn-Rechte“ nicht vollständig erfüllt.

**Risikoarmer nächster Schritt**

Spawn-Rechte nur einem ausdrücklich definierten Analyse-Orchestrator geben, nicht dem bestehenden read-only Explorer; alternativ die Anforderung auf die bereits vorhandene Operations-Orchestrierung (`analyze`) präzisieren.

## UR-41 – Tatsächliche Delegation komplexer Arbeit und Zwischenberichte

**Status: teilweise**

**Codebelege**

- `harw-registry-defaults/knowledge/roles/uia.md:6-17`: Die UIA soll größere, schreibende oder rechercheaufwändige Arbeit an den Root-Orchestrator geben.
- `harw-registry-defaults/knowledge/roles/root-orchestrator.md:1-17`: Der Root ist für Ziel, Budget, Zerlegung und Synthese zuständig und soll nicht selbst ausführen.
- `harw-ops/src/analyze.rs:1022-1032,1063-1070`: Analysearbeit wird tatsächlich an Analyst-Kinder delegiert; pro Welle werden Start-/Ende-Ereignisse und Findings protokolliert.
- `harw-core/src/child_controller.rs:3454-3456,3715-3727`: Kind-Trace, Elternbezug, Rolle, Tiefe, Budget und Status werden im Admission-/ChildRecord-Pfad festgehalten.

**Restlücke**

Es gibt echte Delegationspfade und technische Child-Traces, aber keinen allgemeinen Zwang, dass jeder komplexe UIA-Auftrag über einen Root-Orchestrator läuft. Ebenso ist der Zwischenbericht nicht als einheitlicher UIA-Progressvertrag für alle Orchestrierungen erkennbar; `/analyze` ist ein spezifischer positiver Pfad.

**Risikoarmer nächster Schritt**

Den vorhandenen Child-Trace und ProgressObserver für einen kleinen, strukturierten UIA-Bericht adaptieren: delegiert, aktive Kinder, letzte Welle, Budgetrest, nächster Schritt. Keine neue Modellentscheidung im Berichtspfad.

## UR-42 – Plan vor Go, danach Goal und Orchestrierung

**Status: teilweise**

**Codebelege**

- `harw-ops/src/plan.rs:650-717`: Es gibt einen echten Plan-Store mit `inspect`, `ready`, `waves` und `reconcile`; der Zugriff ist an Konfiguration und Principal gebunden.
- `harw-ops/src/plan.rs:719-840`: Plan, Knoten, Abhängigkeiten, Status und Evidenz werden konkret im Store bearbeitet.
- `harw-ops/src/goal.rs:328-365,495-535`: Goals werden mit authentifiziertem Akteur gesetzt bzw. geändert; terminale Status sind für Modellflächen gesperrt.
- `harw-ops/src/plan.rs:1400-1470`: `reconcile` liefert Runtime-Schritte und Vorschläge, statt still eine vollständige Freigabe zu behaupten.

**Restlücke**

Plan- und Goal-Mechanik existieren, aber der geforderte UX-Gate „Plan präsentieren, explizit auf Go warten, erst danach Goal setzen und orchestrieren“ ist nicht als zentrale Zustandsmaschine erzwungen. `plan create`/`plan add` und `goal set` sind getrennte Operationen; ein verbindlicher Go-Status vor Orchestrierung ist nicht belegt.

**Risikoarmer nächster Schritt**

Einen kleinen, menschlich autorisierten Planfreigabe-Zustand an den bestehenden Plan-Store hängen und nur den Start der Orchestrierungsoperation daran koppeln; vorhandene read-only Planabfragen unverändert lassen.

## UR-43 – Fokussierte Coding-Subagenten, zentrale Builds und Fan-in

**Status: teilweise**

**Codebelege**

- `harw-registry-defaults/agents/family/coding.toml:1-8,14-27,64-80`: Die Coding-Family dokumentiert, dass keine eingebaute Rolle selbst Code entwirft; `executor` führt beschlossene Datei-/Befehlsoperationen aus, `planner`/`explorer` bereiten vor.
- `harw-registry-defaults/agents/cargo-worker.toml:1-20,34-72`: Ein separater Worker darf explizit `cargo build`, `cargo test` usw. in einer Cargo-Sandbox ausführen, schreibt aber selbst keinen Code.
- `harw-registry-defaults/agents/executor.toml:1-20,72-105` (Rollen-/Toolprofil gemäß eingebautem Roster): Der schreibende Executor ist von den read-only Vorbereitungsrollen getrennt.
- `harw-ops/src/analyze.rs:957-1070`: Die konkrete Analyse-Orchestrierung arbeitet in Wellen, führt Fan-out aus, wartet auf Ergebnisse und persistiert den Fan-in.
- `harw-core/src/child_controller.rs:3440-3452,3715-3727`: Kindzahl, Sandbox, Budget und ChildRecord sind zentral kontrolliert.

**Restlücke**

Die Trennung von read-only Recherche, schreibendem Executor und separatem Cargo-Worker ist vorhanden. Ein fokussierter Coding-Subagent mit vollständig erzwungenem Brief ohne Cargo/Compile/Test sowie ein zentraler, serieller „Root baut genau einmal nach voller Vollständigkeit“-Gate sind im geprüften Code nicht nachweisbar. Der explizite Cargo-Worker ist für diese Anforderung zudem eine klar zu dokumentierende Ausnahme.

**Risikoarmer nächster Schritt**

Einen deklarativen Coding-Return-Contract und eine zentrale Build-/Verification-Operation ergänzen; Schreib-Subagenten weiterhin ohne Shell/Cargo halten und den bestehenden Cargo-Worker nur über diesen zentralen Pfad zulassen.

## UR-44 – Guards sind interne Rollen, nicht der menschliche Nutzer

**Status: erfüllt**

**Codebelege**

- `harw-types/src/principal.rs:53-65`: `PrincipalKind` unterscheidet `Human`, `Model`, `Operation` und `Channel`.
- `harw-types/src/principal.rs:89-116,118-181`: Principals entstehen nur über vertrauenswürdigen Ingress bzw. monotone Kindableitung; die Felder sind privat und es gibt kein `Deserialize` für frei rekonstruierbare Modellidentitäten.
- `harw-types/src/principal.rs:166-181`: Ein Kind wird ausdrücklich als `Model` auf `Child` mit abgeleiteter Herkunft angelegt.
- `harw-ops/src/plan.rs:142-172,201-220,298-310`: Command-/Model-Fläche und authentifizierter Principal werden getrennt ausgewertet; `human:` entsteht nur aus menschlichem Principal plus Command-Fläche.
- `harw-ops/src/goal.rs:336-350,507-519`: Modellflächen dürfen `achieve`/`abandon` nicht ausführen; der Akteur wird aus dem authentifizierten Kontext gebildet.
- `harw-core/src/guard.rs:1-17,103-128`: Guards sind deterministische Beobachter für Turn-/Tool-/Child-Drift und keine menschliche Identität.

**Restlücke**

Für die geforderte Unterscheidung und die menschliche Autoritätsgrenze ist eine echte, zentrale Typ-/Ingress-Kette vorhanden.

**Risikoarmer nächster Schritt**

Keine Änderung nötig; bei neuen Guard-/Approval-Pfaden dieselbe `PrincipalKind`- und Ingressableitung wiederverwenden.

## Zusätzliche Anforderung – Reasoning-Kette bis zur UIA

**Anforderung:** UIA nutzt bei reasoning-fähigen Modellen standardmäßig `medium`; Root-Orchestrator bestimmt das Reasoning seiner Orchestratoren, diese bestimmen die Worker; die Entscheidungs- und Budgetkette muss bis zur UIA nachvollziehbar sein.

**Status: widersprüchlich**

**Codebelege**

- `harw-core/src/child_controller.rs:836-846,857-866`: Der dokumentierte und implementierte Default für die UIA ist `High`, nicht `Medium`; auch `RoleEffortWeights::default().uia` ist `High`.
- `harw-config/src/harness_config.rs:119-147`: `[reasoning].uia` dokumentiert ebenfalls Vorgabe `high`; die übrigen Rollen haben eigene Gewichte.
- `harw-runtime/src/guard_wiring.rs:204-245`: Konfigurationswerte werden auf diese Rollen-Gewichte aufgelöst; fehlende/ungültige Werte fallen auf den Default zurück.
- `harw-runtime/src/assembly.rs:3355-3369`: Eine UIA-Root-Session bekommt ohne expliziten Override das UIA-Rollengewicht, aktuell also `High`.
- `harw-core/src/child_controller.rs:3699-3713`: Beim Child werden Parent-Effort und Rollen-/Komplexitätsgewicht monoton mit `min` verbunden; das begrenzt Orchestratoren und Worker tatsächlich.
- `harw-runtime/src/assembly.rs:1815-1825,2800-2818`: Der Spawner erhält für die externe Root-Elternregistrierung `spec.reasoning_effort`, nicht das später in `new_root_session` aus dem UIA-Gewicht aufgelöste effektive UIA-Level. Bei `None` greift im Child-Pfad `DEFAULT_CHILD_REASONING_EFFORT = Medium` (`harw-core/src/child_controller.rs:108-125,2797-2811`).
- `harw-core/src/child_controller.rs:3537-3547,3715-3727`: `ParentGrant` und `ChildRecord` halten Elternrolle, Budget, Reasoning, Trace, Tiefe und Status fest; die Datenkette ist technisch vorhanden.
- `harw-runtime/src/budget.rs:111-169,173-224`: Root-Budgets und modellabhängige Child-Limits werden zentral abgeleitet und geloggt.
- `harw-core/src/turn_loop.rs:2146-2159`: Das jeweils gesetzte Session-Reasoning wird tatsächlich in den ModelRequest und damit zum Provider weitergegeben.

**Restlücke**

Die monotone Parent→Orchestrator→Worker-Vererbung und die technischen Budget-/Trace-Felder existieren. Der verbindliche UIA-Default ist aber direkt falsch (`High` statt `Medium`). Zusätzlich wird bei der extern registrierten UIA-Wurzel das effektive UIA-Reasoning nicht an den Spawner weitergereicht; dadurch kann die nachgelagerte Kette auf dem fehlenden-Parent-Fallback `Medium` starten. Eine kompakte, bis zur UIA sichtbare Entscheidungs-/Budgetzusammenfassung ist nicht nachgewiesen.

**Risikoarmer nächster Schritt**

Den UIA-Default zentral auf `Medium` stellen, die effektive Root-Resolution einmal berechnen und denselben Wert an Session und externe Root-Parent-Registrierung geben. Danach die vorhandenen `ParentGrant`-/`ChildRecord`-/Trace-Daten in einen read-only UIA-Fortschrittsbericht projizieren.

## Zusätzliche verbindliche UX-Anforderung – sequenzielle UIA-Auswahl Provider → kompatibles Modell

**Anforderung:** Die UIA-Auswahl ist sequenziell Provider → kompatibles Modell. `/uia-model` darf ausschließlich Modelle des aktuell gewählten effektiven UIA-Providers anbieten. Ein Providerwechsel darf kein inkompatibles altes Modell still beibehalten. Wenn kein kompatibles Modell gesetzt ist, muss die Modellwahl geöffnet oder klar als erforderlich gemeldet werden.

**Status: teilweise**

**Codebelege für den erfüllten Teil**

- `harw-tui/src/app.rs:1318-1331,1348-1375`: Der UIA-Provider wird in einem eigenen Picker gewählt; die Auswahl erfolgt vor der Modellwahl und verwendet den UIA-Pin als effektive Vorauswahl.
- `harw-tui/src/app.rs:1516-1561,2711-2734`: Der UIA-Modellpicker erhält einen Provider und filtert den sichtbaren Katalog tatsächlich auf diesen kanonischen Provider; ohne Provider wird eine klare Systemmeldung ausgegeben.
- `harw-ops/src/provider.rs:580-624,626-639`: `/uia-provider switch` prüft das alte Modell. Bei einem inkompatiblen oder nicht validierbaren alten Modell wird es aus der atomaren UIA-Auswahl entfernt und eine Auswahl eines kompatiblen Modells verlangt.
- `harw-ops/src/model.rs:486-523`: `/uia-model switch` akzeptiert nur ein Modell, dessen Provider dem effektiven UIA-Provider entspricht; ein inkompatibler Wechsel wird abgelehnt.
- `harw-ops/src/model.rs:526-538` und `harw-ops/src/provider.rs:631-639`: Bei fehlendem Modell wird klar gemeldet, dass `/uia-model list`/eine kompatible Modellwahl erforderlich ist.

**Codebelege für die Restlücke bzw. den Widerspruch**

- `harw-ops/src/model.rs:581-612`: `/uia-model list` iteriert über `config.models.values()` und zeigt auch andere Provider mit dem Label `other-provider`. Damit bietet dieser `/uia-model`-Pfad weiterhin inkompatible Modelle an, obwohl der TUI-Dialog korrekt filtert.
- `harw-operations/src/session_control.rs:80-92` und `harw-tui/src/runtime_root.rs:122-132`: Die Konfigurationsinitialisierung fällt je Achse separat auf `default_provider` bzw. `default_model` zurück. Ein gesetzter UIA-Provider kann dadurch mit einem generischen, inkompatiblen Default-Modell kombiniert werden.
- `harw-config/src/discovery.rs:78-97,120-123`: `ResolvedConfig::validate` validiert `default_provider`/`default_model`, aber keine Referenz- oder Provider-Kompatibilität von `uia_provider`/`uia_model`.
- `harw-tui/src/session_controller.rs:235-258`: Die so initialisierte UIA-Auswahl wird für die UIA-Root-Session auf Provider und Modell angewandt; die Initialkombination ist daher nicht nur Anzeige.

**Restlücke**

Die interaktive UI erfüllt die Sequenz und der Providerwechsel bewahrt kein inkompatibles altes Modell. Der command-basierte `/uia-model list`-Pfad verletzt jedoch das ausschließliche Filtergebot. Zusätzlich kann die achsenweise Konfigurationsfallback-Logik eine inkompatible UIA-Paarung in die Root-Session übernehmen, ohne dass die zentrale Config-Validierung sie meldet.

**Risikoarmer nächster Schritt**

`format_uia_list` auf den effektiven UIA-Provider filtern und bei fehlendem Provider bzw. fehlendem kompatiblem Modell nur eine klare erforderliche-Auswahl-Meldung ausgeben. Danach `uia_provider`/`uia_model` gemeinsam gegen den konfigurierten Modellkatalog validieren; beim Laden entweder das Modell leeren und die Auswahl verlangen oder den Start fail-closed abbrechen. Die bereits atomare Providerwechsel-Logik unverändert weiterverwenden.

