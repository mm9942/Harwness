# Audit TUI / UX / Interaktion / Export

Geprüfter Master: `docs/planning/user-requirements-from-all-transcripts.md`.
Geprüfter Umfang: alle Punkte unter `TUI/UX und Interaktion` sowie `Sitzungen,
Resume, Persistenz und Export`, also UR-01 bis UR-24. Die Befunde beruhen auf
Quelltextprüfung; Build- und Testbefehle wurden nicht ausgeführt.

## UR-01 – Vollständiges, scrollbar sichtbares Slash-Befehlsmenü

Status: **erfüllt**

Codebelege:

- `harw-tui/src/command_popup.rs:183-227` berechnet für alle registrierten
  Befehle eine gefilterte, deterministisch gerankte Liste; `move_up`/`move_down`
  begrenzen die Auswahl auf den gesamten Trefferbereich (`:229-243`).
- `harw-tui/src/command_popup.rs:298-310` leitet einen sichtbaren Ausschnitt
  ab, der der Auswahl folgt; `:341-379` rendert Index, Markierung und Ausschnitt.
- `harw-tui/src/app.rs:4147-4233` verarbeitet Enter, Tab, Pfeile und Ziffern
  innerhalb des Popup-Zustands.

Restlücke: Keine für die UR-Akzeptanz. Ein expliziter PageUp/PageDown-Schritt
fehlt, ist aber für die vollständige Erreichbarkeit nicht erforderlich.

Risikoarmer nächster Schritt: Nur eine manuelle Sichtprüfung mit mehr Treffern
als Terminalzeilen ergänzen; keine Produktänderung nötig.

## UR-02 – Picker für Provider und Modell konsistent per Tastatur bedienen

Status: **teilweise**

Codebelege:

- `harw-tui/src/app.rs:1264-1315` öffnet `/provider` als Auswahl und markiert
  aktiven/authentifizierten Zustand; `:1429-1499` öffnet `/model` und filtert
  den Katalog auf den kanonischen Provider.
- `harw-tui/src/app.rs:2681-2702` fängt bare `/provider` und `/model` vor dem
  normalen Command-Dispatch ab; `/model` verwendet den aktiven bzw. Default-
  Provider.
- `harw-tui/src/choice_dialog.rs:136-193` unterstützt im Picker Up/Down,
  Enter, Esc und Ziffern. `:214-268` zeichnet jedoch nur bis zur verfügbaren
  Höhe und besitzt keinen Scrollzustand.

Restlücke: Die Provider-/Modell-Dialoge können in kurzen Terminals abgeschnitten
werden; links/rechts ist dort nicht implementiert. Die Shell wird zwar nicht
verlassen und das Modell ist providergefiltert, aber die geforderte konsistente
Picker-Bedienung ist damit nicht vollständig.

Risikoarmer nächster Schritt: `ChoiceDialog` um einen aus der Auswahlposition
abgeleiteten sichtbaren Ausschnitt und dokumentierte Links-/Rechtssemantik
ergänzen; die bestehende Auswahl- und Validierungslogik beibehalten.

## UR-03 – Gewähltes Modell als Standard merken

Status: **erfüllt**

Codebelege:

- `harw-ops/src/model.rs:369-381` validiert `/model switch` und ruft die
  Persistenz des Default-Modells auf.
- `harw-ops/src/model.rs:399-419` dokumentiert und implementiert die atomare
  Sequenz aus Katalog-/Providerprüfung, Controller-Mutation und anschließendem
  Persistieren.
- `harw-ops/src/provider.rs:367-373` und `:520-540` persistieren Provider und
  Modell als Profil-Default für künftige Sitzungen.
- `harw-tui/src/runtime_root.rs:789-799` initialisiert die neue Runtime aus der
  geladenen Konfiguration; `harw-tui/src/runtime_root.rs:109-131` liest daraus
  den Default für die Begrüßung.

Restlücke: Die Persistenz ist best effort; ein Schreibfehler erzeugt nur einen
Hinweis. Das ist im Code ausdrücklich vorgesehen und verhindert keinen Lauf.

Risikoarmer nächster Schritt: Beim Start den vorhandenen Persistenz-Hinweis
zusätzlich sichtbar in der Status-/Begrüßungszeile ausgeben, falls der Default
nicht geladen werden konnte.

## UR-04 – Slash-Eingabe vervollständigen und Auswahl übernehmen

Status: **erfüllt**

Codebelege:

- `harw-tui/src/command_popup.rs:256-296` implementiert Tab als Accept,
  Auswahlübernahme oder Erweiterung auf das längste gemeinsame Präfix.
- `harw-tui/src/app.rs:4147-4189` behandelt Enter bei offenem Popup vor dem
  normalen Submit und schreibt den ausgewählten Namen in den Composer.
- `harw-tui/src/app.rs:4192-4217` ruft die Tab-Logik auf und lässt das Popup bei
  einer Präfixerweiterung offen.

Restlücke: Keine für `/co` plus Tab/Enter im normalen, nicht laufenden Turn.

Risikoarmer nächster Schritt: Keine Codeänderung; eine interaktive Regression
für Mehrdeutigkeiten und eine bereits markierte Auswahl wäre ausreichend.

## UR-05 – Vervollständigung auch während UIA-Arbeit

Status: **teilweise**

Codebelege:

- `harw-tui/src/app.rs:5142-5182` verwendet während des laufenden Turns einen
  eigenen Busy-Eingabepfad. Er behandelt dort Cancel, Scroll, BackTab und Paste
  direkt; alle übrigen Tasten gehen über `queue_busy_key`.
- `harw-tui/src/app.rs:5120-5140` leitet die Busy-Eingabe an `InputEditor` bzw.
  `pending_turns`/`deferred_input` weiter, nicht an die Popup-spezifische
  `handle_key`-Logik.
- `harw-tui/src/app.rs:2637-2641` synchronisiert bei Paste zwar das Popup, und
  `:2681-2743` öffnet Picker bei Commands, aber der laufende Turn nutzt diesen
  normalen Command-Dispatch nicht parallel.

Restlücke: Zeichen und Paste können sichtbar im Composer/Popup erscheinen, aber
Pfeil-, Tab-, Enter- und Esc-Bedienung läuft während eines Turns nicht zuverlässig
über den Popup-Zustand; ein markierter Slash-Vorschlag kann deshalb nicht wie im
Idle-Pfad übernommen werden.

Risikoarmer nächster Schritt: Im Busy-Pfad dieselbe Popup-Key-Dispatch-Funktion
wie in `handle_key` vor `queue_busy_key` aufrufen; nur angenommene Commands weiter
in `deferred_input` stellen.

## UR-06 – Laufende Arbeit transparent anzeigen

Status: **teilweise**

Codebelege:

- `harw-tui/src/app.rs:5308-5317` rendert während eines laufenden Turns Spinner
  und `denkt…` in der permanenten Statuszeile.
- `harw-tui/src/app.rs:3094-3110` ordnet Tool-, Reasoning-, Child- und Plan-
  Ereignisse getrennten Zellentypen zu; `:3259-3327` erzeugt und aktualisiert
  SubAgent-Zellen.
- `harw-tui/src/history_cell.rs:454-503` speichert Child-ID, Rolle, Auftrag,
  Toolzahl, Tokens und Status; `:571-612` rendert diese Angaben sichtbar.
- `harw-tui/src/app.rs:3406-3410` dokumentiert, dass einige zentrale Turn-
  Ereignisse, darunter `TurnStarted`/`TurnFailed`/`TurnAborted`, vom Renderer
  keine sichtbare Wirkung erhalten.

Restlücke: Harw-Arbeit und Sub-Agenten sind erkennbar, aber die allgemeine
Herkunft bzw. der aktive Arbeitsakteur ist nicht als eigener Status ausgewiesen;
reine Modell-/Toolaktivität und Orchestrator-/Worker-Arbeit sind nicht durchgängig
unterscheidbar.

Risikoarmer nächster Schritt: Einen transienten Arbeitsstatus mit Quelle und
Turn-ID aus den vorhandenen Ereignissen speisen, ohne neue Verlaufseinträge zu
erzeugen.

## UR-07 – Eingereihte Nachrichten, Abbruch und Fortsetzung

Status: **teilweise**

Codebelege:

- `harw-tui/src/app.rs:733-747` führt `pending_turns`, aktiven Cancel-Token und
  Cancel-Zeitpunkt getrennt.
- `harw-tui/src/app.rs:2600-2608` entnimmt wartende Nachrichten FIFO vor frischer
  TUI-Eingabe; `:2661-2669` stellt weitere Submit-Ereignisse hinten an.
- `harw-tui/src/app.rs:5147-5159` ruft bei Ctrl+C den aktiven Cancel-Token auf
  und zeigt den transienten Hinweis; `:5318-5325` rendert diesen Hinweis.
- `harw-tui/src/app.rs:5334-5357` zeigt eine Vorschau bzw. die Anzahl wartender
  Nachrichten.

Restlücke: Die Warteschlange ist nur als Statussuffix bzw. Vorschau sichtbar,
nicht als klarer eigener Nachrichtenblock mit allen Einträgen. Während eines
laufenden Dialogs werden nicht-textuelle Busy-Aktionen teilweise verworfen
(`queue_busy_key`, `:5120-5140`), und die Ende-zu-Ende-Fortsetzung ist dadurch
nicht für jede Eingabeart gleich.

Risikoarmer nächster Schritt: Wartende Texte als eigene, nicht persistente
Queue-Zellen rendern und nach Cancel denselben FIFO-Drain für alle zulässigen
Submit-Arten verwenden.

## UR-08 – Verlässlicher TUI-Exit

Status: **teilweise**

Codebelege:

- `harw-tui/src/app.rs:4042-4059` implementiert zweistufiges Ctrl+C; im Busy-
  Pfad wird Ctrl+C dagegen als Turn-Cancel behandelt (`:5147-5159`).
- `harw-tui/src/app.rs:4061-4080` implementiert zweistufiges Ctrl+D unabhängig
  vom Composerinhalt.
- `harw-tui/src/app.rs:4088-4093` lässt globale Exit-Tasten vor Overlay-
  Behandlung laufen; Esc schließt ein Popup (`:4095-4100`).
- `harw-tui/src/app.rs:2609-2614` verwirft abgelaufene Exit-Arms, und
  `harw-tui/src/runtime_root.rs:629-695` beendet die TUI kontrolliert und
  schließt die aktuelle Session.

Restlücke: Ctrl+D ist über Composer und Overlays konsistent. Ctrl+C hat jedoch
je nach Idle-/Busy-Zustand zwei unterschiedliche Bedeutungen; ein unmittelbar
folgender zweiter Ctrl+C beendet während laufender Arbeit nicht über denselben
Exit-Arm, sondern wird erneut als Cancel behandelt.

Risikoarmer nächster Schritt: Nach abgeschlossenem Cancel den vorhandenen
Quit-Arm gezielt auch im Busy-Pfad weiterreichen oder die gewünschte Ctrl+C-
Semantik im laufenden Turn eindeutig festlegen.

## UR-09 – Bearbeitbare Eingabe wie ein Terminal-Editor

Status: **teilweise**

Codebelege:

- `harw-tui/src/input_editor.rs:482-492` implementiert Ctrl+Delete als
  Wortlöschung rechts; `harw-tui/src/app.rs:4114-4119` fängt Ctrl+K im normalen
  App-Pfad ab und ruft `delete_current_line` auf.
- `harw-tui/src/input_editor.rs:315-329` implementiert das Löschen der ganzen
  aktuellen Zeile; im direkten Editor-Key-Pfad wird Ctrl+K jedoch in
  `:800-803` nur als `delete_to_end` behandelt.
- `harw-tui/src/tui_event.rs:93-105,141-158` lässt nur ScrollUp/ScrollDown als
  Mouse-Events durch; Klicks und Bewegungen werden verworfen.
- `harw-tui/src/app.rs:2625-2635` verarbeitet daher ausschließlich Mausrad-
  Scrollen, keine Mausmarkierung im Composer.

Restlücke: Ctrl+Delete funktioniert; die Mausauswahl fehlt vollständig. Ctrl+K
ist im Idle-Pfad ganzzeilig, im Busy-Pfad aber nur bis zum Zeilenende und damit
nicht semantisch einheitlich.

Risikoarmer nächster Schritt: Mouse-Selection als eigener, begrenzter Composer-
Eventpfad ergänzen und Busy-/Idle-Ctrl+K auf dieselbe Editoroperation bündeln.

## UR-10 – Keine störende Statuszeile im fortlaufenden Chat

Status: **nicht prüfbar**

Codebelege:

- Der Master benennt in `docs/planning/user-requirements-from-all-transcripts.md:146-149`
  keine eindeutige konkrete Zeile.
- `harw-tui/src/app.rs:5286-5294` reserviert dauerhaft eine eigene Statuszeile.
- `harw-tui/src/app.rs:5358-5363` rendert dort permanent Modus, Tokenwerte und
  transient weitere Hinweise.

Restlücke: Ohne die konkrete, ursprünglich beanstandete Informationszeile lässt
sich nicht sicher entscheiden, ob genau sie gemeint ist. Der aktuelle Code zeigt
jedenfalls eine permanente Statuszeile; damit ist ein pauschales „nicht störend“
nicht belegbar.

Risikoarmer nächster Schritt: Die beanstandete Zeile anhand des Transkripts
eindeutig identifizieren und dann entweder konditional ausblenden oder in
`/status`/ein Overlay verschieben.

## UR-11 – Provider, Modell und Tokenverbrauch sichtbar und korrekt

Status: **teilweise**

Codebelege:

- `harw-tui/src/runtime_root.rs:109-131,197-202` baut die Begrüßung aus den
  konfigurierten UIA-/Default-Provider- und Modellwerten; `:564-570` und
  `:915-921` zeigen sie beim Start und nach Resume.
- `harw-tui/src/app.rs:5358-5363` zeigt echte Tokenfelder aus `app.total_usage`
  in der laufenden TUI-Statuszeile, aber keine Provider- oder Modell-ID.
- `harw-ops/src/status.rs:89-115` liest Provider/Modell aus dem
  SessionController und gibt sie aus, enthält aber keine Tokenverbrauchswerte.
- `harw-tui/src/app.rs:2924-2957` aktualisiert `total_usage` nur am
  `SessionEvent::TurnCompleted`; andere SessionEvent-Varianten werden dort
  ignoriert.
- `harw-tui/src/app.rs:2279-2434` hydriert Verlauf und Tooldaten, setzt aber
  keinen aggregierten Tokenstand aus dem geladenen Verlauf; die neue App startet
  mit `TokenUsage::default()` (`:975`), während Resume nur die sichtbare History
  installiert (`harw-tui/src/runtime_root.rs:905-914`).

Restlücke: Startbegrüßung und einzelne laufende Turn-Werte sind vorhanden, aber
`/status` enthält keine Tokens, die Statuszeile keinen Provider/Modellwert und
Resume stellt den Gesamtverbrauch nicht aus den gespeicherten Turn-Metadaten
her.

Risikoarmer nächster Schritt: Beim Resume den persistierten Session-State in
`AgentSession`/App übernehmen und `/status` um denselben Token-Snapshot ergänzen;
Provider/Modell und Tokens anschließend aus einer gemeinsamen Anzeigequelle
rendern.

## UR-12 – Interaktionsmodi müssen echte Unterschiede haben

Status: **erfüllt**

Codebelege:

- `harw-core/src/mode.rs:150-162` definiert Chat, Plan, Explore und Work.
- `harw-core/src/mode.rs:164-255` gibt für Plan/Explore Minimalprofile und
  Positivlisten sowie Read-only-Ceilings zurück; Chat/Work verwenden Full und
  haben kein namensbasiertes Ceiling.
- `harw-core/src/session.rs:632-658` wendet Modus, Tool-Aktivierung und
  Sandbox-Ceiling tatsächlich auf die Session an und emittiert `ModeChanged`.
- `harw-tui/src/session_controller.rs:404-429` nimmt `/mode` typisiert an und
  wendet die Änderung an der Turn-Grenze an; `harw-tui/src/app.rs:3396-3405`
  übernimmt den sichtbaren Modus.
- `harw-cli/src/main.rs:1859-1899` löst `--mode` gegen CLI-Override bzw.
  Konfigurationsdefault auf.

Restlücke: Der Wechsel wird bewusst erst an der nächsten Turn-Grenze wirksam;
das ist im Controller dokumentiert und kein Gleichheitsbruch. Chat ist im
Quelltext ausdrücklich Full statt read-only.

Risikoarmer nächster Schritt: Keine Produktänderung; nur die vier Ceilings und
den Turn-Grenzen-Hinweis in einer UI-Hilfe zusammenfassen.

## UR-13 – Detaillierte Toolansicht per `/verbose`

Status: **teilweise**

Codebelege:

- `harw-tui/src/history_cell.rs:1128-1143` definiert Compact/Verbose; Verbose
  erzwingt eine erweiterte Ansicht.
- `harw-tui/src/history_cell.rs:1610-1670` rendert bei Verbose die rohen
  Argumente als JSON und begrenzt die Darstellung nachvollziehbar.
- `harw-tui/src/app.rs:1107-1130` verdrahtet dafür nur einen globalen
  `with_verbose_tools`-/CLI-Verbosity-Zustand; die CLI-Option ist in
  `harw-cli/src/cli.rs:67-72` definiert.
- Die Command-Registry und Ops-Liste in `harw-ops/src/lib.rs:19-27,198-233`
  enthält keinen `/verbose`-Befehl.

Restlücke: Eine Detailansicht über `--verbose` und Ctrl+O existiert, aber die
geforderte interaktive Umschaltung per `/verbose` fehlt.

Risikoarmer nächster Schritt: Eine TUI-only-Operation bzw. lokale Command-
Behandlung für `/verbose` ergänzen, die nur den bestehenden Bool-Zustand toggelt.

## UR-14 – Herkunft von Chat-/Agentenereignissen kennzeichnen

Status: **teilweise**

Codebelege:

- `harw-protocol/src/events.rs:61-74` trägt bei ToolCallRequested/Completed
  Call-ID, Toolname, Argumente/Resultat und Dauer, aber keine Herkunftsrolle.
- `harw-tui/src/app.rs:3124-3231` erzeugt Toolzellen anhand der Call-ID und
  exportiert sie mit `agent: None` bzw. ohne Herkunftsfeld.
- `harw-tui/src/app.rs:3259-3287` rendert für ChildSpawned immerhin Child-ID und
  Rolle; `:3289-3327` schreibt Progress/Completion jedoch mit `role: None` und
  `parent_id: None` in den Export.
- `harw-tui/src/history_cell.rs:605-612` macht Child-Rolle und ID in der TUI
  sichtbar. Die Rollenregeln existieren in `harw-agent-dsl/src/roles.rs:27-34,89-112`.

Restlücke: Sub-Agenten sind teilweise zuordenbar, aber UIA, UIA-Worker,
Orchestrator und Worker werden nicht für jede Ereignisgruppe geführt. Besonders
Toolcalls und Toolresults verlieren die Herkunft; parallele Toolarbeit ist daher
nicht vollständig attribuiert.

Risikoarmer nächster Schritt: Eine optionale Origin-/Agent-Referenz in
TurnEvent und Persistenz ergänzen und sie durchgängig in ToolCell, Plan, Fehler
und Export weiterreichen.

## UR-15 – UIA-Modell unabhängig vom Chat-Modell auswählen

Status: **erfüllt**

Codebelege:

- `harw-tui/src/session_controller.rs:63-66,137-152` hält eine eigene
  `uia_selection`, getrennt von `active_model`/`active_provider`; `:243-259`
  verwendet sie nur für die UIA-Root-Session.
- `harw-tui/src/app.rs:1501-1564` filtert den UIA-Modellpicker nach dem
  UIA-Provider; `:2704-2743` öffnet `/uia-provider` und `/uia-model` getrennt
  vom normalen Picker.
- `harw-ops/src/config_util.rs:167-231` persistiert `uia_provider` und
  `uia_model` in eigenen Konfigurationsschlüsseln.
- `harw-tui/src/runtime_root.rs:789-799` lädt diese Auswahl beim Aufbau jeder
  Root-/Resume-Montage erneut; `harw-ops/src/provider.rs:399-411` trennt die
  Persistenzpfade von `/model` und `/uia-model`.

Restlücke: Keine wesentliche für die geforderten Trennungskriterien. Die
Auswahl wird wie die normale Auswahl best effort persistent geschrieben.

Risikoarmer nächster Schritt: Im `/status`-Overlay zusätzlich beide Paare
nebeneinander ausgeben, damit die bereits getrennte Konfiguration sichtbar
prüfbar ist.

## UR-16 – Browserfähigkeit an UIA/UIA-Worker binden

Status: **offen**

Codebelege:

- `harw-agent-dsl/src/roles.rs:27-34,52-64` definiert zwar UserInterface,
  Root-/Child-Orchestrator, Worker und UiaWorker als getrennte Rollen.
- `harw-agent-dsl/src/roles.rs:89-112` begrenzt, welche Rollen welche Kinder
  spawnen dürfen; das ist eine Organisationsregel, keine Browserbesitz- oder
  Capability-Zuordnung.
- `harw-protocol/src/events.rs:89-115` überträgt bei Child-Ereignissen nur
  Kind, Rolle, Fortschritt und Outcome. Eine Browser-Capability bzw. ein
  eindeutiger Besitzer ist dort nicht enthalten.
- In den für Runtime, TUI, Agent-DSL und Ops geprüften Quellen existiert keine
  produktive `browser`-Capability mit Owner-/Delegationsprüfung; vorhanden sind
  allgemeine Web-Werkzeuge, aber kein UIA/UIA-Worker-Binding.

Restlücke: Die Rollenstruktur allein erfüllt nicht die Anforderung, dass genau
Emily/UIA oder vorzugsweise ein UIA-Worker Browserberechtigung besitzt und andere
Rollen sie nur delegiert erhalten.

Risikoarmer nächster Schritt: Browser als explizite Capability mit Besitzerrolle
und erlaubtem Delegationspfad modellieren; die bestehende Rollenprüfung als
zusätzliche Schranke verwenden.

## UR-17 – Integrierte Bug-Report-Funktion

Status: **erfüllt**

Codebelege:

- `harw-ops/src/bug_report.rs:17-29` beschreibt den lokalen manuellen Fallback
  für CLI und `/bug-report`; `harw-ops/src/lib.rs:19-27,220-233` registriert die
  Operation.
- `harw-home/src/paths.rs:269-277` definiert den Zielpfad `<home>/bug-report`.
- `harw-ops/src/bug_report.rs:169-194` legt das Verzeichnis an, validiert die
  ID und redigiert Titel, Fehlertext, Nutzertext, Repro und Evidence.
- `harw-ops/src/bug_report.rs:196-216` schreibt strukturierte Markdown-Felder;
  der restliche Schreiber nutzt eine exklusive temporäre Datei und verhindert
  Überschreiben (`:224-230` ff.).

Restlücke: Der Report ist manuell und minimal; automatische Incident-Erkennung
ist laut Modulkommentar ausdrücklich nicht Teil dieses Pfads.

Risikoarmer nächster Schritt: Optional aus den vorhandenen strukturierten
Fehler-/Retry-Daten einen vorbefüllten TUI-Report anbieten, ohne automatische
Netzwerkübertragung einzuführen.

## UR-18 – Nach temporärem Fehler erneut versuchen

Status: **teilweise**

Codebelege:

- `harw-tui/src/app.rs:4356-4387` definiert eine begrenzte Retry-Policy für
  Rate-Limits; `:4363-4368` setzt Warteobergrenze und maximale Versuche.
- `harw-tui/src/app.rs:4491-4510` retried tatsächlich nur
  `ModelError::RateLimited`, mit `retry_after` und Jitter; `:4511-4520` zeigt
  nach Ausschöpfung einen endgültigen Hinweis.
- `harw-core/src/model.rs:647-673` klassifiziert zusätzlich Transient und
  Timeout als retryable, aber der TUI-Loop verzweigt für diese Varianten nicht in
  denselben Auto-Retry-Pfad.

Restlücke: Eine begrenzte sichtbare Wiederholung existiert für HTTP-429/
Rate-Limit, nicht für den allgemein geforderten temporären Fehler nach etwa
fünf Sekunden.

Risikoarmer nächster Schritt: Nur `Transient`/`Timeout` mit einer kurzen,
gekappten Wartezeit in die bestehende Retry-Funktion aufnehmen und die vorhandene
Versuchs-/Abbruchanzeige wiederverwenden.

## UR-19 – Projekt-Sitzungen tatsächlich sichern und über `-r` auffinden

Status: **teilweise**

Codebelege:

- `harw-tui/src/runtime_root.rs:554-563` lädt die durable History aus dem
  StateStore; `:587-603` öffnet bei barem `-r` den Session-Picker.
- `harw-tui/src/runtime_root.rs:630-685` behandelt `/resume`, montiert die
  ausgewählte Session neu und ersetzt Gateway, Kanäle und App.
- `harw-cli/src/runtime_entry.rs:107-126` legt das Transcriptverzeichnis im
  aktiven Profil an; `harw-cli/src/chat.rs:496-517` verwendet diese globale
  Session-Montage und versieht sie mit Projektmetadaten.
- `harw-cli/src/chat.rs:560-568` filtert Picker-Sessions auf das aktuelle
  Projekt; `:531-535` dokumentiert, dass Ctrl+A den Filter zur Laufzeit derzeit
  nicht umschalten kann.

Restlücke: Resume und Ctrl+C-nahe durable Speicherung sind grundsätzlich
verdrahtet, aber die eigentlichen Transcripts liegen im globalen Profil, nicht
unter `<projekt>/.harw`; der Projektpicker kann wegen des nicht umschaltbaren
Filters passende globale Sessions ausblenden.

Risikoarmer nächster Schritt: Projektfilter und Picker-`show_all` über eine
gemeinsame Selector-API verbinden und im Picker den globalen Transcriptpfad
explizit als Speicherort kenntlich machen.

## UR-20 – Vollständiges Projektgedächtnis und Sitzungs-Metadaten

Status: **teilweise**

Codebelege:

- `harw-home/src/project.rs:397-419,441-461` legt `.harw/memories`,
  `.harw/plans`, `.harw/goals` und `.harw/state` an und sichert die Gitignore.
- `harw-session-store/src/meta.rs:108-157` persistiert Session-ID, Titel,
  Zeitpunkte, cwd/Projekt, erste Nachricht, Turns, Usage-Runden,
  Tokenverbrauch und Driftzähler.
- `harw-session-store/src/meta.rs:293-325,327-382` leitet fehlende Metadaten
  aus dem Transcript ab; `:490-516` kann Usage-Runden additiv speichern.
- Im `SessionMeta`-Schema fehlen jedoch Dauer, Plan-/Goal-/Memory-Referenzen und
  eine explizite Verknüpfung zu den Projektartefakten; `:159-180` initialisiert
  diese Felder nicht.

Restlücke: Die Verzeichnisse und ein brauchbarer Resume-Sidecar existieren, aber
das geforderte konsistente Projektgedächtnis samt Sitzungsnummer, Dauer,
Kontextreferenzen und Picker-Nutzung ist nicht vollständig verdrahtet.

Risikoarmer nächster Schritt: SessionMeta um optionale, rückwärtskompatible
Referenzen und Dauer erweitern und sie beim kontrollierten Sessionabschluss
best effort aktualisieren.

## UR-21 – Long-term Memory aus Erkundung, Fehlern und Sessionabschluss

Status: **teilweise**

Codebelege:

- `harw-memory/src/learning.rs:69-85,187-193,239-260` enthält
  Korrektursignale und PatternCounter-Logik.
- `harw-memory/src/consolidation.rs:5-25` enthält Merge-/Konfliktplanung,
  Apply und Lock; es dokumentiert ausdrücklich, dass `steward_prompt` nur Text
  baut und den Steward nicht selbst aufruft.
- `harw-tui/src/app.rs:2973-2984` erkennt im Chatpfad Korrekturhinweise und
  arbeitet mit einem Memory-Backend; `harw-tui/src/app.rs:948-960` injiziert es
  in die TUI.
- `harw-tui/src/runtime_root.rs:693-695` schließt die Session beim Verlassen,
  enthält aber keinen sichtbaren Aufruf einer Memory-Konsolidierung. Die
  vorhandene Consolidation-API ist damit nicht automatisch an Sessionabschluss,
  Explorations- oder Fehlerereignisse gebunden.

Restlücke: Bausteine für Lernen, Fakten und Konsolidierung existieren, aber ein
nachvollziehbarer automatischer Lebenszyklus „lesen/Fehler/Schließen → projekt-
bezogenes LTM“ ist im geprüften TUI-/Runtimepfad nicht belegt.

Risikoarmer nächster Schritt: Einen idempotenten Abschluss-Hook mit Session-ID,
redigierten Findings und ConsolidationLock einführen; bei Fehlern nur warnen,
damit Sessionabschluss nicht blockiert.

## UR-22 – Resume, TUI und Export mit identischen Toolartefakten

Status: **teilweise**

Codebelege:

- `harw-tui/src/app.rs:2283-2384` hydriert User/Assistant, ToolCall und
  ToolResult; Call-ID, Name, Argumente, Resultat, Dauer und Trust werden
  korreliert und in ExportEntries übernommen.
- `harw-tui/src/app.rs:2409-2431` macht persistierte Calls ohne Result sichtbar,
  markiert sie als unvollständig und erzeugt einen expliziten Resume-Abbruch-
  Resultat-Hinweis statt Erfolg vorzutäuschen.
- `harw-tui/src/app.rs:3124-3231` verwendet live eine gemeinsame ToolCell pro
  Call-ID und hängt ToolCall/ToolResult getrennt an den Export.
- `harw-tui/src/export.rs:292-325` definiert strukturierte ToolCall-/ToolResult-
  Einträge; `:564-593` und `:645-715` erhalten Reihenfolge, IDs, Argumente,
  Resultatstatus und Fehler in JSON. Markdown rendert beide getrennt
  (`:490-535`).
- `harw-tui/src/export.rs:130-161` deaktiviert Reasoning standardmäßig;
  `:536-547` exportiert nur die vorhandene Zusammenfassung, nie opaque Reasoning.

Restlücke: Für vorhandene Tooldaten ist die Korrelation gut. Nicht identisch
sind jedoch offene/abgebrochene Calls, wenn ein echtes Resultat fehlt, und die
Herkunft bleibt bei Tool-/Plan-/Fehlerartefakten oft `None`; Child-Ereignisse sind
im Resume-Historypfad nicht als vollständiger Agentenbaum belegt.

Risikoarmer nächster Schritt: Persistierte Origin-/Agent-Referenzen und
abschließende Turn-/Agent-Ereignisse in denselben History-/Exportstrom aufnehmen;
die bestehende Call-ID-Korrelation unverändert weiterverwenden.

## UR-23 – Projekt-Home sicher behandeln

Status: **teilweise**

Codebelege:

- `harw-home/src/project.rs:421-461` behandelt `$HOME` ausdrücklich als
  zulässigen Root-Space ohne `~/.gitignore`; das normale Projekt erhält `.harw`
  und die Gitignore-Regel.
- `harw-home/src/project.rs:497-511` lehnt nur `/` ab und erkennt den
  kanonisierten Benutzer-Home separat.
- `harw-tui/src/runtime_root.rs:587-603` und `harw-cli/src/chat.rs:659-706`
  lassen bare `-r` im Terminal in den TUI-Picker laufen bzw. nutzen im Nicht-
  TTY den Auswahlprompt; explizite Selektoren werden aufgelöst.
- `harw-cli/src/chat.rs:560-568` bindet die Pickerliste aber an den aktuellen
  Projekt-Key; `:531-535` dokumentiert den fehlenden Laufzeitwechsel zu allen
  Projekten.

Restlücke: Das Home-Verzeichnis selbst scheitert nicht mehr als Projekt-Home,
aber eine im Home gestartete Picker-Sicht kann projektfremde bestehende Sessions
ausblenden. Ein expliziter Session-Selector bleibt der sichere Ausweg.

Risikoarmer nächster Schritt: Bei `current_project_key == None` bzw. Home-Root
den Picker standardmäßig mit klarer „alle Projekte“-Sicht öffnen, ohne die
explizite Projektfilterung in echten Projekten zu lockern.

## UR-24 – Git-/Projektinitialisierung und `.harw`-Ignorierung

Status: **teilweise**

Codebelege:

- `harw-cli/src/main.rs:466-501` führt `harw init` als zusammenhängenden Ablauf
  aus: Root-Space, nötigenfalls `git init`, danach `ProjectHome::ensure`.
- `harw-home/src/project.rs:448-461` legt `.harw` samt Unterverzeichnissen an;
  `:465-480` ergänzt idempotent `.harw/` bzw. `/.harw/` in der Root-
  `.gitignore`.
- `harw-home/src/scaffold.rs:60-88` initialisiert den globalen Root-Space und
  ignoriert dessen Cache.

Restlücke: Die drei Schritte sind im expliziten `harw init` vorhanden; der
normale Chat-/TUI-Start stellt Projekt-`.harw`/Gitignore best effort her, führt
aber nicht automatisch `git init` aus. Damit ist die Anforderung „beim Start als
zusammenhängender Vorgang“ nicht vollständig erfüllt.

Risikoarmer nächster Schritt: Vor dem normalen Chatstart nur die fehlende
Repository-Initialisierung erkennen und mit einer bestätigten, klaren
Einmalaktion anbieten; `.gitignore` und `.harw` weiterhin idempotent behandeln.
