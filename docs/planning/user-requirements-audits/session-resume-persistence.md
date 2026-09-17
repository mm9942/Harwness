# Luna-Audit: Session, Resume, Persistenz, State, Pläne, Ziele und Memory

Stand: 2026-09-17  
Prüfung: statischer Abgleich der Masterpunkte mit dem tatsächlichen Code. Die
Masterdatei und Produktdateien wurden nicht geändert. Es wurden keine Builds,
Tests, `cargo`, `rustc`, `rustfmt`, `make` oder `codex exec` ausgeführt.

## Kurzbefund

| UR | Status |
|---|---|
| UR-03 | teilweise |
| UR-11 | teilweise |
| UR-15 | erfüllt |
| UR-17 | erfüllt |
| UR-19 | teilweise |
| UR-20 | teilweise |
| UR-21 | teilweise |
| UR-22 | teilweise |
| UR-23 | teilweise |
| UR-35 | teilweise |
| UR-42 | offen |
| UR-52 | teilweise |

## Einzelprüfungen

### UR-03 – Gewähltes Modell als Standard merken

Status: **teilweise**

Codebelege:

- `/model switch` mutiert den aktiven Controller und ruft anschließend die
  Persistenzfunktion auf: `harw-ops/src/model.rs:462-480`.
- Die Persistenz schreibt `default_model` (und optional den Provider) in die
  Profil-`config.toml`: `harw-ops/src/config_util.rs:132-164`.
- Der interaktive Root setzt beim Aufbau jedoch zunächst ausschließlich die
  UIA-Auswahl aus `uia_*` mit Fallback auf `default_*`:
  `harw-tui/src/runtime_root.rs:789-816` und
  `harw-tui/src/runtime_root.rs:122-132`.

Restlücke: Ein vorhandener `uia_model`-Pin kann den mit `/model`
gespeicherten `default_model` überlagern. Außerdem wird der generische
Controller-State beim Neustart nicht aus einem Session-State-Snapshot geladen.

Risikoarmer nächster Schritt: Die Semantik explizit trennen und dokumentieren:
`/model` nur als generischen Default verwenden, sofern kein UIA-Pin existiert;
den UIA-Pfad ausschließlich über `/uia-model` initialisieren und persistieren.

### UR-11 – Provider, Modell und Tokenverbrauch sichtbar und korrekt

Status: **teilweise**

Codebelege:

- Die laufende TUI rendert echte Gesamt-, Input-, Output- und Cache-Tokens:
  `harw-tui/src/app.rs:5301-5363`.
- Die TUI akkumuliert abgeschlossene Turn-Nutzung aus
  `SessionEvent::TurnCompleted`: `harw-tui/src/app.rs:2924-2929`.
- Beim Resume wird nur der Verlauf geladen und installiert, nicht der
  Session-State: `harw-tui/src/runtime_root.rs:905-914` und
  `harw-tui/src/app.rs:2469-2476`.
- Das frische `ChatApp` startet mit `TokenUsage::default()`:
  `harw-tui/src/app.rs:975`.
- Persistente Nutzungsdaten werden zwar als `UsageRound` und Sidecar-Metadaten
  geschrieben: `harw-core/src/state_store.rs:870-899`; der `/usage`-Befehl
  kann einen gespeicherten Snapshot lesen: `harw-ops/src/usage.rs:181-205`.

Restlücke: Nach Resume ist die TUI-Anzeige nicht aus dem gespeicherten
Gesamtverbrauch rehydriert; die Live-Akkumulation beginnt erneut bei null.

Risikoarmer nächster Schritt: Beim Resume denselben zuletzt gespeicherten
`SessionStateSnapshot` laden und die Anzeige aus dessen `total_usage`
initialisieren; die vorhandene `/usage`-Leselogik kann als gemeinsame Quelle
dienen.

### UR-15 – UIA-Modell unabhängig vom Chat-Modell auswählen

Status: **erfüllt**

Codebelege:

- `/uia-model switch` validiert das Modell gegen den effektiven UIA-Provider,
  mutiert nur die UIA-Auswahl und persistiert `uia_provider`/`uia_model`:
  `harw-ops/src/model.rs:486-538`.
- Die UIA-Auswahl wird aus den UIA-Schlüsseln mit getrenntem Fallback auf
  `default_*` gebildet: `harw-tui/src/runtime_root.rs:122-132`.
- Die TUI setzt diese Auswahl beim Aufbau der Root-Session ein:
  `harw-tui/src/runtime_root.rs:789-816`.

Restlücke: Im geprüften Code keine für die Akzeptanz wesentliche Restlücke.

Risikoarmer nächster Schritt: Bei späteren Änderungen die getrennten
`/model`-/`/uia-model`-Konfigurationsschlüssel als Invariante beibehalten.

### UR-17 – Integrierte Bug-Report-Funktion

Status: **erfüllt**

Codebelege:

- Der CLI-Befehl sammelt Pflichtfelder, baut einen strukturierten Bericht und
  schreibt ihn lokal: `harw-cli/src/main.rs:532-588`.
- Der gemeinsame Schreiber legt `<home>/bug-report/<id>.md` an, verwendet eine
  sichere ID und redigiert bekannte Secret-Träger:
  `harw-ops/src/bug_report.rs:169-194` und
  `harw-ops/src/bug_report.rs:224-244`.
- Die TUI weist bei bekannten Incidents auf `/bug-report` hin:
  `harw-tui/src/app.rs:2436-2465`.

Restlücke: Die automatische Incident-Erkennung schreibt laut Modulgrenze noch
keinen automatischen Bericht; die Anforderung verlangt jedoch nur die
integrierte, lokal auffindbare Funktion.

Risikoarmer nächster Schritt: Keine Änderung für UR-17 erforderlich; eine
spätere automatische Erfassung sollte denselben Redaction-/Dateischreiber
verwenden.

### UR-19 – Projekt-Sitzungen tatsächlich sichern und über `-r` auffinden

Status: **teilweise**

Codebelege:

- Die produktive CLI legt den Transcript-Store unter
  `<profil>/sessions` an, nicht unter `<projekt>/.harw`:
  `harw-cli/src/runtime_entry.rs:90-119` und
  `harw-cli/src/chat.rs:366-369`.
- Turns werden als JSONL-Items dauerhaft angehängt:
  `harw-core/src/state_store.rs:747-757`; der Turn-Loop persistiert das letzte
  History-Item wiederholt und auch vor Abbruchpfaden:
  `harw-core/src/turn_loop.rs:3351-3367` und
  `harw-core/src/turn_loop.rs:2991-3012`.
- `-r` entdeckt reguläre `.jsonl`-Transkripte, lädt/ableitet Sidecars und
  sortiert sie: `harw-cli/src/resume.rs:114-182`.
- Ein ausgewähltes Resume montiert die Session, lädt den Verlauf und stellt ihn
  sichtbar dar: `harw-tui/src/runtime_root.rs:873-922`.

Restlücke: Der Transcript-/Picker-Speicher ist profilglobal statt im
Projekt-`.harw`; das Projekt-`.harw` erhält nur angrenzende Daten. Damit ist
die wörtliche Projektablage-Anforderung nicht erfüllt, obwohl der Resume-Fluss
selbst vorhanden ist.

Risikoarmer nächster Schritt: Projektzuordnung und globale JSONL als bewusstes
Zwei-Ebenen-Layout festlegen; mindestens einen projektlokalen, kleinen Verweis
auf die globale Session im vorhandenen `.harw/state`-Bereich ergänzen, ohne das
bewährte globale Transcript zu verschieben.

### UR-20 – Vollständiges Projektgedächtnis und Sitzungs-Metadaten

Status: **teilweise**

Codebelege:

- `ProjectHome` definiert projektlokale Verzeichnisse für Memories, Pläne,
  Goals und State und legt sie an: `harw-home/src/project.rs:397-419` und
  `harw-home/src/project.rs:421-461`.
- `SessionMeta` enthält Session-ID, Zeitpunkte, Projektzuordnung, erste
  Nutzernachricht, Turns und Tokenverbrauch:
  `harw-session-store/src/meta.rs:108-156`.
- Fehlende Sidecars werden aus dem Transcript abgeleitet; dabei werden jedoch
  nur Zeitpunkte, erste Nutzernachricht und abgeschlossene Turns rekonstruiert,
  Usage bleibt null: `harw-session-store/src/meta.rs:293-325` und
  `harw-session-store/src/meta.rs:328-382`.
- Persistente `FilePlanStore`-/`FileGoalStore`-Implementierungen existieren:
  `harw-plan/src/file_store.rs:394-495` und
  `harw-plan/src/goal_store.rs:387-469`.
- Die produktive TUI verwendet dennoch `InMemoryPlanStore` und
  `InMemoryGoalStore`, auch wenn `persist = true` gesetzt ist:
  `harw-runtime/src/assembly.rs:649-709`.
- Die produktive TUI lädt beim Start/Resume nur die History, nicht den
  Session-State: `harw-tui/src/runtime_root.rs:554-563` und
  `harw-tui/src/runtime_root.rs:905-914`.
- Der Picker übernimmt nur Titel, letzte Aktivität, Projektlabel und Turns;
  Usage, Dauer, State, Plans und Memories werden nicht als Picker-Daten
  abgebildet: `harw-tui/src/runtime_root.rs:965-977`.

Restlücke: Projekt-Directories werden zwar vorbereitet und Memory kann dort
schreiben, aber Plan/Goal-Persistenz ist im normalen TUI-Pfad nicht verdrahtet;
Session-State, Dauer/Sitzungsnummer und konsistente Querverweise fehlen.

Risikoarmer nächster Schritt: Zuerst `persist` tatsächlich auf
`FilePlanStore`/`FileGoalStore` abbilden und danach einen einzelnen,
versionierten Session-State-Snapshot im bestehenden Transcript-/State-Pfad beim
Schließen schreiben und beim Resume laden.

### UR-21 – Long-term Memory aus Erkundung, Fehlern und Sessionabschluss

Status: **teilweise**

Codebelege:

- `ProjectMemoryCapture` beobachtet Tool-Ergebnisse, schreibt Dateiwissen,
  Recherche-Funde und Pitfall-Kandidaten projektbezogen in den Incoming-Speicher:
  `harw-memory/src/capture.rs:1-30` und
  `harw-memory/src/capture.rs:285-379`.
- Nicht vorübergehende Toolfehler werden gesammelt, bei einem passenden Erfolg
  als Pitfall aufgelöst und beim Flush auch unaufgelöst geschrieben:
  `harw-memory/src/capture.rs:403-461` und
  `harw-memory/src/capture.rs:489-510`.
- Beim Schließen ruft der Runtime-Hook synchron Flush und Konsolidierung auf:
  `harw-runtime/src/memory_wiring.rs:139-175`.
- Der Hook wird in die Assembly aufgenommen und `close_session` ruft alle Hooks
  auf: `harw-runtime/src/assembly.rs:1894-1905` und
  `harw-runtime/src/assembly.rs:3428-3444`.

Restlücke: Die Erfassung ist primär tool-outcome-basiert. Eine allgemeine
Transkript-Extraktion von widerlegten Annahmen, Lernpunkten und sonstigen
Erkenntnissen ist nicht in diesen Runtime-Pfad verdrahtet. Das vorhandene
Extraktionsmodul liefert dafür nur Bausteine; es beschreibt den Modellaufruf
als externen, noch zu verdrahtenden Pfad: `harw-memory/src/extraction.rs:1-19`.

Risikoarmer nächster Schritt: Nach dem bestehenden Flush eine begrenzte,
projektbezogene Extraktionsstufe für abgeschlossene Transkripte ergänzen;
bestehende Redaction, IncomingStore und Konsolidierung unverändert wieder-
verwenden.

### UR-22 – Resume, TUI und Export mit identischen Toolartefakten

Status: **teilweise**

Codebelege:

- Exakte `TurnItem`s werden als JSONL-Items gespeichert und beim Laden wieder
  deserialisiert: `harw-core/src/state_store.rs:747-757` und
  `harw-core/src/state_store.rs:785-824`.
- Die History-Hydration rekonstruiert User-/Assistant-Nachrichten,
  Tool-Aufrufname und Argumente, Toolresultat, Status, Dauer, Trust,
  Reasoning und Fehler als sichtbare/exportierbare Einträge:
  `harw-tui/src/app.rs:2279-2405`.
- Offene Toolcalls bleiben sichtbar, werden aber mit einem synthetischen
  Resume-Abbruchresultat ergänzt: `harw-tui/src/app.rs:2409-2433`.
- Der Export wird aus `app.export_entries` gebaut:
  `harw-tui/src/app.rs:3685-3738`.
- Live-Plan- und Agentenereignisse werden zwar in Export-/UI-Einträge
  übersetzt, aber nur im Eventpfad: Plan bei
  `harw-tui/src/app.rs:3329-3394`; nicht persistierte sonstige Events werden
  in der History-Hydration nicht behandelt (`harw-tui/src/app.rs:2283-2406`).

Restlücke: Plan-, Child-Agent-, Fortschritts- und sonstige relevante
`TurnEvent`s werden nicht als rehydrierbare Transcript-Items gespeichert. Nach
Resume fehlen sie daher im sichtbaren Verlauf und Export. Für tatsächlich
offene Calls bleibt außerdem ein generischer synthetischer Platzhalter, auch
wenn nur der Abschluss fehlt.

Risikoarmer nächster Schritt: Ein versioniertes, opakes Event-Item für die
relevanten UI-/Agent-/Plan-Ereignisse ergänzen und denselben Hydrations-Mapper
für Live- und Resume-Einträge verwenden; synthetische Einträge nur bei real
fehlenden Daten erzeugen.

### UR-23 – Projekt-Home sicher behandeln

Status: **teilweise**

Codebelege:

- Die Projekterkennung stoppt am kanonischen Home und fällt dort auf das
  tatsächliche Arbeitsverzeichnis zurück, statt Home als Repositorywurzel zu
  behandeln: `harw-home/src/project.rs:138-196`.
- `ProjectHome::ensure` behandelt `$HOME` speziell, legt `~/.harw` an und
  schreibt keine `~/.gitignore`; `/` wird weiterhin abgelehnt:
  `harw-home/src/project.rs:421-461`.
- `-r` verwendet den Profil-Sessionstore und denselben Projektfilter im TUI-
  und Non-TTY-Pfad: `harw-cli/src/chat.rs:240-275` und
  `harw-cli/src/chat.rs:659-705`.
- Eine Session mit Projekt-Key matcht nur den aktuellen Projekt-Key; eine
  ungetaggte Session matcht bei aktuellem Projekt-Key nicht:
  `harw-cli/src/resume.rs:328-358`.

Restlücke: Der Home-Start scheitert nicht mehr. Von `$HOME` aus sind jedoch
projektgetaggte Sessions standardmäßig nicht auswählbar, weil dort kein
aktueller Projekt-Key vorhanden ist; dafür ist `--all` nötig. Das verfehlt die
Akzeptanz für Resume aus der Shell bei bestehender Projekt-Session teilweise.

Risikoarmer nächster Schritt: Bei `-r` aus Home einen expliziten, sicheren
Projektkontext aus dem Resume-Selector ableiten oder die Auswahl mit klarer
Projektanzeige öffnen; den bestehenden Filter nicht global abschalten.

### UR-35 – UIA als echte Identität mit Dateien und echtem ersten Turn

Status: **teilweise**

Codebelege:

- Ohne aktive UIA sucht der CLI-Bootstrap eine UIA, legt sie bei Bedarf im
  Profil an und persistiert `active_uia_definition`:
  `harw-cli/src/uia_bootstrap.rs:110-132`.
- Der Setup-Dialog erzeugt Definition, Agent-Konfiguration,
  `Personality.md` und `USER.md`: `harw-cli/src/uia_bootstrap.rs:221-239` und
  `harw-cli/src/uia_bootstrap.rs:294-334`.
- Runtime-Kontext lädt Identität, Persönlichkeit und USER-Kontext getrennt in
  gekennzeichnete Modellfragmente: `harw-config/src/loader.rs:40-70`.
- Die TUI liest den Namen ausschließlich aus einer expliziten `Name:`-Zeile in
  `USER.md`, mit Login-Namen als Rückfall, und verwendet ihn in der Begrüßung:
  `harw-config/src/loader.rs:73-105` und
  `harw-tui/src/runtime_root.rs:138-172`.
- Die UIA-Definition wird für TUI/One-shot auf die Rolle
  `user-interface` validiert: `harw-runtime/src/assembly.rs:2059-2083`.

Restlücke: Der sichtbare erste Kontakt ist eine lokal erzeugte TUI-
Begrüßungszeile, kein nachweisbarer echter UIA-Modellturn. Zudem erzeugt
`ensure_active_uia` bei fehlender Auswahl automatisch eine minimale UIA nach
Dialog; ob die konkret gewünschte Identität (z. B. Emily) aktiv ist, wird nur
aus der persistenten Konfiguration übernommen, nicht als Produktinvariante
erzwingbar.

Risikoarmer nächster Schritt: Einen expliziten ersten UIA-Turn als normalen,
persistierten Turn modellieren und dessen Abschluss abwarten; die vorhandenen
geladenen Identitäts-/USER-Fragmente und die sichere Namensauflösung weiter-
verwenden.

### UR-42 – Plan zuerst, Go abwarten, danach Ziel konsequent abarbeiten

Status: **offen**

Codebelege:

- Plan- und Goal-Operationen sind vorhanden und mutieren die registrierten
  Stores; `/plan` kann als ModelTool aufgerufen werden:
  `harw-ops/src/plan.rs:650-730`; `/goal` besitzt ebenfalls eine ModelTool-
  Fläche: `harw-ops/src/goal.rs:281-355`.
- Der produktive TUI-Planpfad nutzt In-Memory-Stores, und selbst bei
  `persist = true` wird nicht auf File-Stores umgeschaltet:
  `harw-runtime/src/assembly.rs:649-709`.
- Der Turn-Loop montiert unmittelbar Context, Instructions, Tools und den
  Modellaufruf; ein vorgelagerter Plan-/Go-/Goal-Zustandsautomat ist dort nicht
  erkennbar: `harw-core/src/turn_loop.rs:2103-2159`.

Restlücke: Es gibt keinen produktseitigen Gate-Pfad, der vor größerer Arbeit
einen Plan vorlegt, auf ein menschliches Go wartet, danach ein Goal setzt und
die Orchestrierung bis Ergebnis/Blocker verfolgt.

Risikoarmer nächster Schritt: Einen kleinen, persistenten Workflow-Status für
`plan_presented -> go_received -> goal_bound -> executing -> done/blocked`
einführen und zunächst nur den Start komplexer TUI-Aufträge daran binden.

### UR-52 – Transkript- und Planwissen für Folgearbeit auswerten

Status: **teilweise**

Codebelege:

- Beim Resume wird die vorhandene Conversation-History geladen und sichtbar
  rehydriert: `harw-tui/src/runtime_root.rs:905-921`.
- Der Runtime-Context-Provider liefert projekt- vor globalen Preference-/Pitfall-
  Fakten sowie bekanntes Dateiwissen in jeden Turn:
  `harw-runtime/src/assembly.rs:2433-2474` und
  `harw-runtime/src/assembly.rs:2611-2629`.
- Der Turn-Loop sammelt diese Context-Fragmente vor jedem Modellaufruf:
  `harw-core/src/turn_loop.rs:2112-2159`.
- Transcript-Extraktion ist als Auswahl-/Filterlogik vorhanden, einschließlich
  abgeschlossener Sessions und `TranscriptEntry`; der eigentliche Modellaufruf
  ist laut Modulgrenze jedoch ein externer, noch zu verdrahtender Pfad:
  `harw-memory/src/extraction.rs:1-19` und
  `harw-memory/src/extraction.rs:178-212`.

Restlücke: Es gibt keinen einheitlichen Folgearbeits-Preflight, der relevante
Transkripte, Pläne, Goals, offene Punkte, Delegationsregeln und Invarianten
gezielt auswählt, als direkte Quellen benennt und in die Folgearbeit überführt.
Pläne/Goals werden im normalen TUI-Pfad außerdem nicht aus File-Stores
rehydriert.

Risikoarmer nächster Schritt: Einen read-only ContextProvider für einen
`follow-up-preflight` ergänzen, der Session-Meta, ausgewählte Transcript-
Abschnitte, aktuellen Plan/Goal und Projekt-Memory mit Pfad/Revision als
Quellenlabels zusammenführt; erst danach die vorhandene Plan-/Goal-Aktion
ausführen.

