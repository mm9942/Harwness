# Nutzeranforderungen aus allen Transkripten

Stand: 2026-09-17. Dies ist eine deduplizierte Anforderungsinventur, kein
Quellcode-Audit und kein Fortschrittsbericht. Alle Einträge haben deshalb den
Status **nicht geprüft**.

## Wichtigste TUI-Restforderungen

- Die TUI muss während eines laufenden Turns verständlich und bedienbar bleiben:
  Denk-/Arbeitsstatus, Agentenherkunft, Warteschlange, Retry, sofortiger
  Abbruch und anschließendes Senden müssen sichtbar bzw. zuverlässig sein.
- Slash-Befehle und Picker müssen vollständig sichtbar, per Tastatur auswählbar
  und vervollständigbar sein – auch während die UIA arbeitet. Provider und
  Modell dürfen dabei nicht verwechselt werden.
- Provider, Modell und echte Token-Nutzung müssen beim Start und in `/status`
  angezeigt werden; nach Resume dürfen sie nicht auf null oder Platzhalter
  zurückfallen.
- Resume und Export müssen die gleichen relevanten Gesprächsartefakte zeigen:
  Tool-Aufrufe/-Resultate, Pläne, Agenten-/Arbeitsereignisse und deren klare
  Zuordnung.
- Die Eingabe braucht normale Editor-Funktionen (Mausmarkierung, Wort-/Zeilen-
  löschen) und einen verlässlichen Exit, ohne dass die Tastaturnavigation die
  Shell verlässt.

## Produktanforderungen

### TUI/UX und Interaktion

#### UR-01 – Vollständiges, scrollbar sichtbares Slash-Befehlsmenü

- Bereich: TUI/UX
- Priorität: hoch (explizit als erstes TUI-Problem genannt)
- Quellen (1 Nennung): `harw-export-1789377255.md:149`.
- Anforderung: Das Slash-Befehls-Popup darf nicht nach den Einträgen 1–8
  abschneiden; weitere Einträge müssen erreichbar und sichtbar sein.
- Akzeptanzkriterien: Bei mehr Befehlen als sichtbaren Zeilen kann der Nutzer
  durch alle Treffer navigieren und erkennt Auswahl sowie Scrollposition.
- Status: nicht geprüft.

#### UR-02 – Picker für Provider und Modell konsistent per Tastatur bedienen

- Bereich: TUI/UX, Provider/Modelle
- Priorität: hoch
- Quellen (2 Nennungen): `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:637`,
  `harw-export-1789511666.md:53`.
- Anforderung: `/provider` und `/model` müssen mit Pfeilen und Enter wechseln;
  die Navigation darf die Harw-Shell nicht verlassen. `/model` zeigt nur Modelle
  des gewählten Providers.
- Akzeptanzkriterien: Pfeil hoch/runter, links/rechts soweit angeboten und
  Enter wirken ausschließlich im aktiven Picker; das Modellangebot wird nach
  Providerwechsel gefiltert.
- Status: nicht geprüft.

#### UR-03 – Gewähltes Modell als Standard merken

- Bereich: TUI/UX, Persistenz, Provider/Modelle
- Priorität: hoch
- Quellen (1 Nennung): `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:680`.
- Anforderung: Das zuletzt gewählte Modell bleibt der Standard.
- Akzeptanzkriterien: Nach einem Neustart wird das zuletzt bestätigte Modell
  als Standard verwendet bzw. angezeigt, sofern es noch verfügbar ist.
- Status: nicht geprüft.

#### UR-04 – Slash-Eingabe vervollständigen und Auswahl übernehmen

- Bereich: TUI/UX
- Priorität: hoch
- Quellen (3 Nennungen): `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:900`,
  `harw-export-1789406677.md:158`,
  `2026-09-17-121558-image-1-nice-wre-wenn-harw-es-nach-sagen-wir.txt:6`.
- Anforderung: Tab vervollständigt eine nur teilweise eingegebene Slash-
  Auswahl; Enter übernimmt den aktuell markierten Vorschlag statt den
  unvollständigen Text auszuführen.
- Akzeptanzkriterien: `/co` plus Tab/Enter führt nachvollziehbar zur passenden
  Auswahl; eine markierte Auswahl hat Vorrang vor rohem Teiltext.
- Status: nicht geprüft.

#### UR-05 – Vervollständigung auch während UIA-Arbeit

- Bereich: TUI/UX, Nebenläufigkeit
- Priorität: hoch
- Quellen (2 Nennungen): `2026-09-17-121558-image-1-nice-wre-wenn-harw-es-nach-sagen-wir.txt:6`,
  `2026-09-17-121558-image-1-nice-wre-wenn-harw-es-nach-sagen-wir.txt:1475`.
- Anforderung: Tab-Vervollständigung und Auswahl von Slash-Befehlen dürfen
  nicht ausfallen, wenn die UIA gerade arbeitet.
- Akzeptanzkriterien: Während eines laufenden Turns lassen sich Slash-Popup und
  Autovervollständigung öffnen, bedienen und schließen.
- Status: nicht geprüft.

#### UR-06 – Laufende Arbeit transparent anzeigen

- Bereich: TUI/UX
- Priorität: hoch
- Quellen (2 Nennungen): `2026-09-16-010759-hilf-mir-nochmal-dabei-das-wir-die-internen-model.txt:6`,
  `2026-09-17-121558-image-1-nice-wre-wenn-harw-es-nach-sagen-wir.txt:6`.
- Anforderung: Die TUI soll nicht nur Modellaufrufe zeigen, sondern erkennbar
  machen, dass Harw denkt/arbeitet und welche Agenten gespawnt oder aktiv sind.
- Akzeptanzkriterien: Ein laufender Turn hat einen sichtbaren Arbeitszustand;
  Agentenereignisse sind von reinen Modell-/Toolereignissen unterscheidbar.
- Status: nicht geprüft.

#### UR-07 – Eingereihte Nachrichten, Abbruch und Fortsetzung

- Bereich: TUI/UX, Runtime
- Priorität: hoch
- Quellen (2 Nennungen): `2026-09-17-121558-image-1-nice-wre-wenn-harw-es-nach-sagen-wir.txt:6`,
  `harw-export-1789511666.md:71`.
- Anforderung: Während Harw arbeitet eingegebene Nachrichten werden unten als
  wartend angezeigt; Ctrl+C unterbricht direkt; eine danach eingereihte Nachricht
  wird anschließend gesendet statt dauerhaft nur „Abbruch angefordert“ zu zeigen.
- Akzeptanzkriterien: Wartende Eingaben sind sichtbar, ihre Reihenfolge bleibt
  erhalten, Ctrl+C hat eine erkennbare Wirkung und der nächste Turn startet
  danach ohne manuelle Reparatur.
- Status: nicht geprüft.

#### UR-08 – Verlässlicher TUI-Exit

- Bereich: TUI/UX
- Priorität: hoch
- Quellen (2 Nennungen): `harw-export-1789388771.md:60`,
  `harw-export-1789511666.md:71`.
- Anforderung: Es muss einen funktionierenden Exit geben, einschließlich des
  gewünschten doppelten Ctrl+D-Verhaltens und eines korrekt arbeitenden Ctrl+C.
- Akzeptanzkriterien: Exit funktioniert auch bei nichtleerem Composer bzw.
  offenen Overlays/Popups; kein Tastendruck lässt einen scheinbar hängenden
  interaktiven Zustand zurück.
- Status: nicht geprüft.

#### UR-09 – Bearbeitbare Eingabe wie ein Terminal-Editor

- Bereich: TUI/UX
- Priorität: mittel
- Quellen (2 Nennungen): `2026-09-17-121558-image-1-nice-wre-wenn-harw-es-nach-sagen-wir.txt:758`,
  `2026-09-17-121558-image-1-nice-wre-wenn-harw-es-nach-sagen-wir.txt:776`.
- Anforderung: Text soll mit der Maus markierbar sein; Ctrl+Entf löscht ganze
  Wörter; Ctrl+K löscht die gesamte aktuelle Zeile und rückt Folgelinien nach.
- Akzeptanzkriterien: Die drei Aktionen verändern ausschließlich die erwartete
  Eingabeauswahl bzw. -zeile und lassen den Composer konsistent gerendert zurück.
- Status: nicht geprüft.

#### UR-10 – Keine störende Statuszeile im fortlaufenden Chat

- Bereich: TUI/UX
- Priorität: mittel
- Quellen (1 Nennung): `harw-export-1789381305.md:78`.
- Anforderung: Die konkret beanstandete Status-Informationszeile soll nach
  fortschreitendem Chat nicht mehr stören bzw. angezeigt werden.
- Akzeptanzkriterien: Die betroffene Zeile ist im beschriebenen Chat-Zustand
  nicht sichtbar; notwendige Statusinformationen bleiben anderweitig erreichbar.
- Status: nicht geprüft.

#### UR-11 – Provider, Modell und Tokenverbrauch sichtbar und korrekt

- Bereich: TUI/UX, Provider/Modelle, Runtime
- Priorität: hoch
- Quellen (4 Nennungen): `harw-export-1789524310.md:2985`,
  `harw-export-1789524310.md:3601`, `harw-export-1789523460.md:3041`,
  `harw-export-1789523460.md:3657`.
- Anforderung: Beim Start und in `/status` werden Provider und Modell gezeigt;
  die TUI zeigt echte Gesamt-, Eingabe- und Ausgabetoken statt dauerhaft 0.
- Akzeptanzkriterien: Anzeige stimmt mit der aktiven Konfiguration und den
  Turn-Metadaten überein, auch nach mehreren Runden und Resume.
- Status: nicht geprüft.

#### UR-12 – Interaktionsmodi müssen echte Unterschiede haben

- Bereich: TUI/UX, CLI/Runtime
- Priorität: mittel
- Quellen (1 Nennung): `harw-export-1789523460.md:4518`.
- Anforderung: `/mode` und `--mode` müssen nachvollziehbar unterschiedliche
  Modi liefern; insbesondere darf Chat-Modus nicht faktisch nur lesen.
- Akzeptanzkriterien: Für chat, plan, explore und work sind Eingabe-,
  Delegations- und/oder Ausführungsverhalten dokumentierbar verschieden und im
  jeweiligen Modus beobachtbar.
- Status: nicht geprüft.

#### UR-13 – Detaillierte Toolansicht per `/verbose`

- Bereich: TUI/UX, CLI
- Priorität: mittel
- Quellen (1 Nennung): `harw-export-1789511666.md:45`.
- Anforderung: `/verbose` soll verfügbar sein, damit Toolaktivität bei Bedarf
  detaillierter statt nur verdichtet sichtbar wird.
- Akzeptanzkriterien: Der Befehl schaltet eine klar erkennbare Detailansicht
  für Tool-Aufrufe und deren Argument-/Ergebnisdarstellung ein bzw. aus.
- Status: nicht geprüft.

#### UR-14 – Herkunft von Chat-/Agentenereignissen kennzeichnen

- Bereich: TUI/UX, Agenten/Worker
- Priorität: hoch
- Quellen (2 Nennungen): `harw-export-1789523460.md:4398`,
  `2026-09-16-010759-hilf-mir-nochmal-dabei-das-wir-die-internen-model.txt:6`.
- Anforderung: In der TUI muss sichtbar sein, ob ein Ereignis von UIA,
  UIA-Worker, (Sub-)Orchestrator oder Worker stammt, statt alles ungeordnet als
  Toolcalls erscheinen zu lassen.
- Akzeptanzkriterien: Jede relevante Ereignisgruppe trägt Rollen-/Agenten-
  Herkunft; parallele Arbeit bleibt lesbar zuordenbar.
- Status: nicht geprüft.

#### UR-15 – UIA-Modell unabhängig vom Chat-Modell auswählen

- Bereich: TUI/UX, Provider/Modelle, UIA
- Priorität: hoch
- Quellen (2 Nennungen): `2026-09-17-121558-image-1-nice-wre-wenn-harw-es-nach-sagen-wir.txt:54`,
  `2026-09-17-121558-image-1-nice-wre-wenn-harw-es-nach-sagen-wir.txt:59`.
- Anforderung: Einer UIA kann ein eigenes Modell unabhängig von `/model`
  zugewiesen werden; `/uia-model` wählt aus dem zugehörigen `/uia-provider`.
- Akzeptanzkriterien: UIA-Provider und UIA-Modell sind getrennt von Chat-
  Provider/-Modell persistierbar und die Modellauswahl ist providergefiltert.
- Status: nicht geprüft.

#### UR-16 – Browserfähigkeit an UIA/UIA-Worker binden

- Bereich: Agenten/Worker, TUI/UX
- Priorität: mittel
- Quellen (2 Nennungen): `harw-export-1789656943.md:4523`,
  `harw-export-1789656943.md:4608`.
- Anforderung: Browser soll Emily (der UIA) oder vorzugsweise einem UIA-Worker
  zugeordnet sein, nicht ungebunden im allgemeinen Agentennetz hängen.
- Akzeptanzkriterien: Die Browserberechtigung hat einen eindeutigen Besitzer;
  andere Rollen erhalten sie nur über dessen vorgesehene Delegation.
- Status: nicht geprüft.

#### UR-17 – Integrierte Bug-Report-Funktion

- Bereich: TUI/UX, Persistenz
- Priorität: mittel
- Quellen (1 Nennung): `2026-09-17-121558-image-1-nice-wre-wenn-harw-es-nach-sagen-wir.txt:694`.
- Anforderung: Eine Bug-Report-Funktion nach dem gezeigten Feedback-Vorbild soll
  Berichte intern unter `~/.harw/bug-report` sammeln.
- Akzeptanzkriterien: Ein Bericht enthält strukturierte Fehlerdaten ohne
  unnötige Geheimnisse, wird lokal abgelegt und ist später auffindbar.
- Status: nicht geprüft.

#### UR-18 – Nach temporärem Fehler erneut versuchen

- Bereich: Runtime, TUI/UX
- Priorität: mittel
- Quellen (1 Nennung): `2026-09-17-121558-image-1-nice-wre-wenn-harw-es-nach-sagen-wir.txt:6`.
- Anforderung: Harw soll nach etwa fünf Sekunden noch einmal versuchen, wenn
  eine Anfrage scheitert.
- Akzeptanzkriterien: Wiederholbare, begrenzte Retry-Logik zeigt den Wartestatus
  an und unterscheidet Erfolg nach Retry von endgültigem Fehler.
- Status: nicht geprüft.

### Sitzungen, Resume, Persistenz und Export

#### UR-19 – Projekt-Sitzungen tatsächlich sichern und über `-r` auffinden

- Bereich: Session/Resume, Persistenz
- Priorität: kritisch
- Quellen (2 Nennungen): `harw-export-1789381305.md:8`,
  `harw-export-1789465226.md:364`.
- Anforderung: Projekt-`.harw` darf nicht leer bleiben; beendete bzw.
  unterbrochene Sitzungen müssen in `-r/--resume` auswählbar sein.
- Akzeptanzkriterien: Nach einem Chat und Ctrl+C erscheint eine zugehörige
  Sitzung im Picker und lässt sich laden.
- Status: nicht geprüft.

#### UR-20 – Vollständiges Projektgedächtnis und Sitzungs-Metadaten

- Bereich: Persistenz, Session/Resume, Langzeitgedächtnis
- Priorität: kritisch
- Quellen (3 Nennungen): `harw-export-1789388771.md:60`,
  `harw-export-1789388771.md:114`, `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:4384`.
- Anforderung: Goals und Pläne werden in Projekt-`.harw` gefüllt, Erinnerungen
  aus Projekt-Erkundungen erfasst, und `state` wird beim Sitzungsende mit
  Sitzungsnummer, Kontext, Dauer, Tokenverbrauch usw. aktualisiert. Der Picker
  nutzt diese Daten und lädt die passende globale JSONL-Sitzung.
- Akzeptanzkriterien: Jede beendete Sitzung hat auffindbare Metadaten; Goals,
  Pläne, Erinnerungen und State sind im Projektkontext vorhanden und konsistent
  auf die wiederherstellbare Sitzung referenziert.
- Status: nicht geprüft.

#### UR-21 – Long-term Memory aus Erkundung, Fehlern und Sessionabschluss

- Bereich: Persistenz, Langzeitgedächtnis
- Priorität: hoch
- Quellen (4 Nennungen): `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:3858`,
  `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:3880`,
  `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:3975`,
  `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:4001`.
- Anforderung: Harw sammelt aktiv Projektwissen beim Lesen/Explorieren,
  einschließlich widerlegter Codeannahmen, aufgetretener Fehler und Lernpunkte;
  beim Schließen wird es in Long-term Memory konsolidiert.
- Akzeptanzkriterien: Relevante Erkenntnisse werden projektbezogen abgelegt,
  ohne bloße Rohtranskriptduplizierung; ein Sitzungsabschluss löst eine
  nachvollziehbare Konsolidierung aus.
- Status: nicht geprüft.

#### UR-22 – Resume, TUI und Export mit identischen Toolartefakten

- Bereich: Session/Resume, Persistenz, Export, TUI/UX
- Priorität: kritisch
- Quellen (5 Nennungen): `harw-export-1789523460.md:4189`,
  `harw-export-1789524310.md:4133`,
  `2026-09-17-121558-image-1-nice-wre-wenn-harw-es-nach-sagen-wir.txt:1739`,
  `2026-09-17-121558-image-1-nice-wre-wenn-harw-es-nach-sagen-wir.txt:1757`,
  `2026-09-17-121558-image-1-nice-wre-wenn-harw-es-nach-sagen-wir.txt:1759`.
- Anforderung: Exakte Tool-Aufrufe, Resultate und die gezeigten relevanten
  Ereignisse werden gespeichert, bei `-r` in der TUI sichtbar rehydriert und
  im Export aufgenommen.
- Akzeptanzkriterien: Derselbe abgeschlossene Toolcall ist nach Resume und im
  Export mit Name, Ergebnisstatus und sinnvoller Darstellung vorhanden; keine
  bloßen generischen Platzhalter anstelle vorhandener Daten.
- Status: nicht geprüft.

#### UR-23 – Projekt-Home sicher behandeln

- Bereich: Session/Resume, Sicherheit
- Priorität: hoch
- Quellen (1 Nennung): `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:980`.
- Anforderung: `-r` darf nicht am Home-Verzeichnis als Projektwurzel scheitern,
  wenn der Nutzer eine bestehende Projekt-Sitzung fortsetzen will.
- Akzeptanzkriterien: Die Projekterkennung erklärt bzw. wählt korrekt einen
  zulässigen Projektkontext; ein Resume aus der Shell bleibt nutzbar.
- Status: nicht geprüft.

#### UR-24 – Git-/Projektinitialisierung und `.harw`-Ignorierung

- Bereich: Projekt-/Workspace-Struktur
- Priorität: mittel
- Quellen (2 Nennungen): `harw-export-1789388771.md:347`,
  `harw-export-1789388771.md:389`.
- Anforderung: Gesamtes `.harw` soll per Git ignoriert werden; beim Start soll
  Harw fehlende Projektinitialisierung als zusammenhängenden Vorgang behandeln
  (Git init, `.gitignore`, `.harw` init).
- Akzeptanzkriterien: Neu eingerichtete Projekte enthalten die nötige
  Initialisierung; Laufzeitdaten landen nicht versehentlich im Repository.
- Status: nicht geprüft.

### Provider, Modelle, Authentisierung und CLI

#### UR-25 – Interne Modelle reparieren und Katalogdaten bereinigen

- Bereich: Provider/Modelle
- Priorität: kritisch
- Quellen (3 Nennungen): `2026-09-16-010759-hilf-mir-nochmal-dabei-das-wir-die-internen-model.txt:6`,
  `latest-claude-session.txt:6`,
  `2026-09-16-010759-hilf-mir-nochmal-dabei-das-wir-die-internen-model.txt:181`.
- Anforderung: Interne Modelle, passende BytePlus-Modelle und fehlerhafte
  Modellidentitäten (beispielsweise das genannte falsche Terra-Modell) müssen
  korrigiert werden.
- Akzeptanzkriterien: Katalogeinträge besitzen valide Provider-/Modellnamen,
  plausiblen Kontext/Preis/Tool-Support und können ausgewählt werden.
- Status: nicht geprüft.

#### UR-26 – Modellkatalog automatisch scannen und stale Einträge ersetzen

- Bereich: Provider/Modelle, CLI
- Priorität: hoch
- Quellen (2 Nennungen): `codex-session-01a0a6bd-2550-76a1-b623-75259209bfe3.md:32830`,
  `codex-session-01a0a6bd-2550-76a1-b623-75259209bfe3.md:39339`.
- Anforderung: `harw models scan` soll verfügbare Modelle automatisiert erkennen,
  nicht bloß aus Webrecherche Dateien anhäufen, nicht mehr bereitgestellte
  Einträge löschen und verbleibende Daten ersetzen/aktualisieren.
- Akzeptanzkriterien: Nach einem Scan gibt es keine verwaisten Einträge; aktive
  Katalogdaten sind aus dem Scan nachvollziehbar aktualisiert.
- Status: nicht geprüft.

#### UR-27 – `harw models` als vollständige Verwaltungsoberfläche

- Bereich: CLI, Provider/Modelle, TUI/UX
- Priorität: hoch
- Quellen (5 Nennungen): `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:5196`,
  `harw-export-1789398616.md:597`,
  `codex-session-01a0a6bd-2550-76a1-b623-75259209bfe3.md:41067`,
  `codex-session-01a0a6bd-2550-76a1-b623-75259209bfe3.md:41071`,
  `codex-session-01a0a6bd-2550-76a1-b623-75259209bfe3.md:41075`.
- Anforderung: `harw models add [provider]/[model]` und Löschen müssen Modelle
  verwalten. Ohne konkreten Namen zeigt die Oberfläche aktivierte Provider,
  navigiert per Tastatur in die Modellliste, erlaubt Mehrfachauswahl mit
  Leerzeichen und bestätigt mit Enter.
- Akzeptanzkriterien: Hinzufügen, Entfernen, Provider-Navigation und sichtbare
  Checkbox-Auswahl funktionieren; nicht deployte Modelle verschwinden.
- Status: nicht geprüft.

#### UR-28 – Provider-TOML ist Quelle der in der TUI sichtbaren Modelle

- Bereich: Provider/Modelle, Konfiguration
- Priorität: hoch
- Quellen (1 Nennung): `codex-session-01a0a6bd-2550-76a1-b623-75259209bfe3.md:40073`.
- Anforderung: Die Modellliste eines Providers in dessen Konfiguration soll die
  Modelle bestimmen, die die TUI anbietet.
- Akzeptanzkriterien: Änderungen an der Provider-Modellliste spiegeln sich
  nach dem vorgesehenen Reload in der TUI-Auswahl wider.
- Status: nicht geprüft.

#### UR-29 – Kostenbewusste freie, lokale und Open-Weight-Modelle

- Bereich: Provider/Modelle
- Priorität: hoch
- Quellen (3 Nennungen): `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:5196`,
  `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:5561`,
  `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:5769`.
- Anforderung: Harw soll Open-Weight-, freie und lokal laufende Modelle als
  ernsthafte Optionen konfigurierbar machen; interne Prozessstellen sollen dem
  Nutzer eine Modellwahl bieten. Bei konfiguriertem OpenRouter soll dieser beim
  Setup empfohlen werden, mit einem geeigneten Nvidia-Nemotron-Default für
  interne Aufgaben.
- Akzeptanzkriterien: Modellquellen/Preise werden korrekt eingeordnet (nur
  ausdrücklich freie OpenRouter-Varianten als kostenlos); Nutzer können die
  Wahl beim Onboarding und für interne Arbeit treffen.
- Status: nicht geprüft.

#### UR-30 – DashScope- und Providerregressionen beheben

- Bereich: Provider/Modelle, Runtime
- Priorität: hoch
- Quellen (3 Nennungen): `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:5982`,
  `codex-session-01a0a6bd-2550-76a1-b623-75259209bfe3.md:30501`,
  `codex-session-01a0a6bd-2550-76a1-b623-75259209bfe3.md:39069`.
- Anforderung: DashScope soll statt OpenRouter auswählbar sein; nach Updates
  aufgetretene Providerfehler (u. a. Mistral und weitere) sind zu beheben, ohne
  Schlüsselwechsel beim Nutzer zu unterstellen.
- Akzeptanzkriterien: Betroffene Provider lassen sich onboarden und anfragen;
  Fehlerdiagnosen unterscheiden Konfigurations-, Auth- und Routingfehler.
- Status: nicht geprüft.

#### UR-31 – Codex- und Claude-OAuth zuverlässig und transparent

- Bereich: Provider/Modelle, Sicherheit
- Priorität: kritisch
- Quellen (5 Nennungen): `harw-export-1789398616.md:37`,
  `codex-session-01a0a6bd-2550-76a1-b623-75259209bfe3.md:1739`,
  `codex-session-01a0a6bd-2550-76a1-b623-75259209bfe3.md:1962`,
  `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:12053`,
  `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:12356`.
- Anforderung: Der Codex-Auth-Prozess und Claude/Anthropic-OAuth-Setup müssen
  funktionieren; die ausdrücklich erlaubte Codex-OAuth-Nutzung ist zu
  unterstützen.
- Akzeptanzkriterien: Onboarding und Start verwenden die passende Auth-Methode,
  scheitern nicht an inkonsistenten Credential-Pfaden und geben sichere,
  handlungsfähige Fehlermeldungen aus.
- Status: nicht geprüft.

#### UR-32 – Anthropic-OAuth als bewusste, sichere Nutzerentscheidung

- Bereich: Sicherheit, Provider/Modelle
- Priorität: hoch
- Quellen (4 Nennungen): `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:12961`,
  `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:13057`,
  `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:13324`,
  `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:13488`.
- Anforderung: Wenn Anthropic über den angesprochenen Auth-Weg ausgewählt wird,
  braucht es eine Warnung im Einrichtungsfluss, nicht als dauerhafte Runtime-
  Meldung; danach gilt Nutzung auf eigene Verantwortung.
- Akzeptanzkriterien: Die Warnung erscheint vor der Aktivierung, enthält keine
  Secrets und blockiert nicht mit einer irreführenden Runtime-Warnung.
- Status: nicht geprüft.

#### UR-33 – Ausführende Bash erhält den PATH des Nutzers

- Bereich: Runtime, Sandbox
- Priorität: mittel
- Quellen (2 Nennungen): `harw-export-1789377255.md:210`,
  `harw-export-1789377255.md:250`.
- Anforderung: Die Harw-Implementierung soll Bash-Anwendungen den PATH des
  ausführenden Nutzers bereitstellen, nicht nur den PATH der entwickelnden
  Umgebung.
- Akzeptanzkriterien: Ein erlaubter Shell-Aufruf findet übliche Nutzerprogramme
  gemäß seinem tatsächlichen PATH; die Herkunft des PATH bleibt kontrollierbar.
- Status: nicht geprüft.

#### UR-34 – Shell-Autovervollständigung für alle Unterbefehle

- Bereich: CLI
- Priorität: mittel
- Quellen (1 Nennung): `harw-export-1789583648.md:8`.
- Anforderung: Die Harw-Shell-Autovervollständigung deckt alle Subcommands ab.
- Akzeptanzkriterien: Für jeden dokumentierten Unterbefehl liefert die Shell
  passende Completion-Vorschläge einschließlich verschachtelter Befehle.
- Status: nicht geprüft.

### UIA, Agenten, Worker und Orchestrierung

#### UR-35 – UIA als echte Identität mit Dateien und echtem ersten Turn

- Bereich: UIA, Agenten/Worker, Persistenz
- Priorität: kritisch
- Quellen (5 Nennungen): `harw-export-1789381305.md:166`,
  `harw-export-1789381305.md:216`,
  `harw-export-1789406677.md:8`,
  `2026-09-16-010759-hilf-mir-nochmal-dabei-das-wir-die-internen-model.txt:262`,
  `2026-09-16-010759-hilf-mir-nochmal-dabei-das-wir-die-internen-model.txt:305`.
- Anforderung: Eine UIA besteht mindestens aus Agent-/Definition-Konfiguration,
  `Personality.md` und `USER.md`; diese beeinflussen wirklich Verhalten,
  Begrüßung und Nutzerbezug. Emily soll die tatsächlich aktive UIA sein und den
  Nutzer per echtem Turn begrüßen; der Name stammt aus USER-Wissen, nicht blind
  aus einem Login-Namen.
- Akzeptanzkriterien: Bei Aktivierung werden die genannten Dateien geladen; ein
  beobachtbarer erster Turn nutzt die konfigurierte Identität und wahrt fehlende
  oder sensible USER-Daten.
- Status: nicht geprüft.

#### UR-36 – UIA wird vom Nutzer erstellt und erhält Spawnfähigkeiten

- Bereich: UIA, Agenten/Worker
- Priorität: hoch
- Quellen (2 Nennungen): `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:12637`,
  `harw-export-1789656943.md:8`.
- Anforderung: Eine UIA existiert anfangs nicht automatisch, sondern wird durch
  den Nutzer erstellt; danach müssen ihr die vorgesehenen Spawnfähigkeiten
  zugewiesen sein.
- Akzeptanzkriterien: Frische Profile aktivieren keine unerwünschte UIA; eine
  vom Nutzer angelegte/aktivierte UIA kann ihre erlaubten Helfer rollenbasiert
  starten.
- Status: nicht geprüft.

#### UR-37 – Agenten-Designer vereinfachen die Rollenbildung

- Bereich: Agenten/Worker
- Priorität: hoch
- Quellen (3 Nennungen): `harw-export-1789388571.md:8`,
  `harw-export-1789388571.md:16`, `harw-export-1789388571.md:57`.
- Anforderung: Es soll agentengesteuerte Designer für UIA, Orchestrator,
  Suborchestrator, Worker und UIA-Worker plus Design-Orchestrator geben. Die
  UIA übersetzt den Wunsch des Nutzers in eine verständliche Agententopologie.
- Akzeptanzkriterien: Ein Nutzer ohne Rollenwissen kann sein Ziel beschreiben
  und erhält nachvollziehbare Definitionen statt unverständlicher Rollendetails.
- Status: nicht geprüft.

#### UR-38 – Spawnrechte strikt und deklarativ begrenzen

- Bereich: Agenten/Worker, Sicherheit
- Priorität: kritisch
- Quellen (5 Nennungen): `harw-export-1789388571.md:107`,
  `harw-export-1789465226.md:516`, `harw-export-1789643187.md:753`,
  `harw-export-1789643187.md:843`, `harw-export-1789643187.md:906`.
- Anforderung: Ein Suborchestrator darf standardmäßig nur Worker spawnen.
  Weitere Suborchestratoren sind nur erlaubt, wenn die Berechtigung ausdrücklich
  in seiner Agentendefinition delegiert wurde. Rollen sollen strukturell keine
  nicht erlaubten Agenten/Tools kennen. Als konkretes Muster wurde ein
  Suborchestrator mit bis zu fünf parallelen Explore-Workern und einem
  Auswertungsworker genannt.
- Akzeptanzkriterien: Nicht deklarierte Spawnversuche werden abgewiesen;
  erlaubte Kinder, Parallelgrenzen und Rollenpfade sind aus der Agentenkonfigu-
  ration prüfbar.
- Status: nicht geprüft.

#### UR-39 – Regelwissen für korrekte Orchestrierung automatisch bereitstellen

- Bereich: Agenten/Worker, Runtime
- Priorität: hoch
- Quellen (3 Nennungen): `harw-export-1789465226.md:8`,
  `harw-export-1789465226.md:56`, `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:6252`.
- Anforderung: Harw soll beim Orchestrieren sein Rollen-/Delegationsregelwerk
  als Wissen automatisch laden, damit es nicht manuell falsche Worker startet
  oder verfügbare Rollen wie UIA-Worker übersieht.
- Akzeptanzkriterien: Eine Orchestrierungsanfrage verwendet die gültige
  Rollenstruktur; bei nicht erlaubtem Direktsprung wird der zuständige
  Orchestrator gewählt oder verständlich erklärt.
- Status: nicht geprüft.

#### UR-40 – Projektneutrale Explore/Analyse-Agenten

- Bereich: Agenten/Worker
- Priorität: mittel
- Quellen (4 Nennungen): `harw-export-1789643187.md:678`,
  `harw-export-1789643187.md:781`, `harw-export-1789643187.md:843`,
  `harw-export-1789643187.md:953`.
- Anforderung: Neue Explore-/Analyse-Agenten sollen nicht Rust- oder
  projektspezifisch sein, Spawnrechte besitzen und Informationsgewinnung
  parallelisieren können.
- Akzeptanzkriterien: Dasselbe Agentenprofil lässt sich in mehreren Projekten
  verwenden; Parallelität bleibt durch die deklarierte Grenze beschränkt.
- Status: nicht geprüft.

#### UR-41 – Orchestratoren für komplexe Arbeit tatsächlich nutzen

- Bereich: Agenten/Worker, Arbeitsweise
- Priorität: hoch
- Quellen (6 Nennungen): `harw-export-1789643187.md:352`,
  `harw-export-1789643187.md:377`, `harw-export-1789643187.md:1243`,
  `harw-export-1789524310.md:5538`, `harw-export-1789524310.md:5554`,
  `harw-export-1789519086.md:1762`.
- Anforderung: Umfangreiche, gut teilbare Aufgaben sind an den Orchestrator zu
  delegieren, nicht vom Root still allein auszuführen; der Nutzer verlangt
  regelmäßige Zwischenberichte statt bloßer Ankündigungen.
- Akzeptanzkriterien: Die Delegationsspur zeigt Orchestrator und Teilaufgaben;
  Fortschrittsberichte erscheinen während langer Arbeit in sinnvoller Frequenz.
- Status: nicht geprüft.

#### UR-42 – Plan zuerst, Go abwarten, danach Ziel konsequent abarbeiten

- Bereich: Agenten/Worker, Arbeitsweise
- Priorität: hoch
- Quellen (3 Nennungen): `harw-export-1789643187.md:1400`,
  `harw-export-1789656943.md:4478`, `harw-export-1789656943.md:4656`.
- Anforderung: Für größere Arbeit wird ein Plan mit nächsten Schritten vorgelegt;
  nach Nutzer-Go wird ein Goal gesetzt und mit Orchestrierung durchgeführt,
  nicht in Evidenzprosa stehen geblieben.
- Akzeptanzkriterien: Vor Go erfolgt nur die gewünschte Planabstimmung; danach
  läuft die Umsetzung bis zu einem echten Ergebnis oder klaren Blocker weiter.
- Status: nicht geprüft.

#### UR-43 – Focused Coding Agents schreiben Code, Root baut einmal am Ende

- Bereich: Agenten/Worker, Build-/Arbeitsweise
- Priorität: kritisch
- Quellen (7 Nennungen): `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:1719`,
  `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:2218`,
  `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:10398`,
  `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:10445`,
  `harw-export-1789469522.md:109`,
  `2026-09-17-121558-image-1-nice-wre-wenn-harw-es-nach-sagen-wir.txt:108`,
  `harw-export-1789465226.md:1361`.
- Anforderung: Focused Coding/Subagents arbeiten isoliert und schreiben primär
  Code; Cargo/Compile/Test ist ihnen verboten. Erst nach Abschluss und
  Vollständigkeitsprüfung baut der Root-Orchestrator einmal. Prüfkommandos dürfen
  keinen unerwarteten Compile-Vorgang auslösen.
- Akzeptanzkriterien: Subagent-Briefs enthalten kein Cargo; der Build erfolgt
  zentral und serialisiert; Parallelarbeit verwendet einen Fan-in statt
  konkurrierender Builds.
- Status: nicht geprüft.

#### UR-44 – Interne Wächter korrekt als Systemrollen behandeln

- Bereich: Agenten/Worker, Sicherheit
- Priorität: mittel
- Quellen (1 Nennung): `harw-export-1789643187.md:1800`.
- Anforderung: Harw-Wächter und Guards sind interne LLM-Agenten zur
  Parametereinhaltung und dürfen nicht fälschlich als der menschliche Nutzer
  klassifiziert werden.
- Akzeptanzkriterien: Ereignis-/Nachrichtenmetadaten unterscheiden eindeutig
  menschlichen Nutzer, UIA/Worker und interne Guard-Rollen.
- Status: nicht geprüft.

### Sandbox und Sicherheit

#### UR-45 – Sandbox modular statt pauschal zu hart gestalten

- Bereich: Sandbox, Sicherheit
- Priorität: kritisch
- Quellen (7 Nennungen): `harw-export-1789388771.md:467`,
  `harw-export-1789465226.md:293`, `harw-export-1789465226.md:307`,
  `harw-export-1789465226.md:1405`, `harw-export-1789524310.md:1235`,
  `harw-export-1789523460.md:1235`, `harw-export-1789583648.md:574`.
- Anforderung: Die frühere, zu harte Sandbox muss modular gelockert werden,
  wenn sie Arbeit verhindert; Cargo soll entweder sicher darin laufen oder über
  einen dafür vorgesehenen Ausführungspfad möglich sein.
- Akzeptanzkriterien: Ein Sicherheitsprofil kann gezielt Fähigkeiten erlauben
  statt global alles freizugeben; notwendige Build-/Arbeitsfälle haben einen
  kontrollierten Pfad.
- Status: nicht geprüft.

#### UR-46 – Sandbox-Deaktivierung nur durch intent- und bestätigungsgebundenen Ablauf

- Bereich: Sandbox, Sicherheit
- Priorität: kritisch
- Quellen (6 Nennungen): `harw-export-1789465226.md:307`,
  `harw-export-1789465226.md:1429`, `harw-export-1789465226.md:1479`,
  `harw-export-1789465226.md:1518`, `harw-export-1789465226.md:1547`,
  `harw-export-1789643187.md:1646`.
- Anforderung: Der Agent darf Sandbox-Lockerung/Deaktivierung nicht selbst
  ausführen. Er erkennt ein Nutzersignal, stellt einen Funktionsantrag und
  verlangt danach eine explizite Ja-Bestätigung. Kein Start-CLI-Flag und kein
  einfacher Slash-Befehl darf die Sandbox direkt deaktivieren. Eine Lockerung
  kann bis Sitzungsende oder bis ausdrücklich wieder aktiviert gelten und soll
  nicht die erste Wahl sein.
- Akzeptanzkriterien: Ohne zweistufige Zustimmung bleibt die Sandbox aktiv;
  jede Ausnahme hat Scope, Laufzeit und Reaktivierungspfad; die Entscheidung ist
  auditierbar ohne Geheimnisse.
- Status: nicht geprüft.

#### UR-47 – Spezielle Ausführungsworker für Shell-/tmux-Fälle

- Bereich: Sandbox, Agenten/Worker
- Priorität: hoch
- Quellen (2 Nennungen): `harw-export-1789465226.md:1361`,
  `2026-09-17-121558-image-1-nice-wre-wenn-harw-es-nach-sagen-wir.txt:357`.
- Anforderung: Für Bash, Cargo oder das Zeigen einer tmux-Sitzung sollen
  besondere Worker außerhalb bzw. mit anderem Sandbox-Profil arbeiten, die
  ohne Ausführungsfreigabe ihres Parents keinen Shell-Befehl verwenden dürfen.
- Akzeptanzkriterien: Der Worker besitzt ein enges Rollenprofil; Elternfreigabe
  ist vor jedem Ausführungsweg nachweisbar; langlebige SSH/tmux-Sitzungen werden
  nicht unnötig abgebrochen.
- Status: nicht geprüft.

### Projekt-/Workspace-Struktur und weitere Produktziele

#### UR-48 – DoD als abgegrenzten Cargo-Workspace strukturieren

- Bereich: Projekt-/Workspace-Struktur
- Priorität: mittel
- Quellen (1 Nennung): `harw-export-1789465226.md:640`.
- Anforderung: Das DoD soll als eigener Cargo-Workspace mit seinen zugehörigen
  Bestandteilen restrukturiert werden, weiterhin Teil des Gesamtprojekts, aber
  von der normalen Produktlaufzeit abgegrenzt.
- Akzeptanzkriterien: DoD-spezifische Aufgaben liegen in einem klaren Workspace;
  der Kernlaufzeit wird keine ungenutzte DoD-Komplexität aufgezwungen.
- Status: nicht geprüft.

#### UR-49 – Android-/Mobile-Variante für lokale autonome Modelle

- Bereich: Runtime, Projekt-/Workspace-Struktur
- Priorität: mittel (Zukunftsvorhaben)
- Quellen (7 Nennungen): `harw-export-1789429814.md:8`,
  `harw-export-1789429814.md:18`, `harw-export-1789429814.md:39`,
  `harw-export-1789429814.md:68`, `harw-export-1789429814.md:88`,
  `harw-export-1789429814.md:118`, `harw-export-1789429814.md:175`.
- Anforderung: Eine minimalistische, aber richtig nutzbare mobile Variante
  `harw-agentic-mobile` soll auf Android mit kleinen lokalen Modellen autonom
  laufen, chatten können und mit expliziten Ja/Nein-Freigaben (u. a. per
  Benachrichtigung) aufräumende Verwaltungsaufgaben erledigen. Telegram und
  zuhause betriebene Dienste/Datenspeicher sind als mögliche Anbindung genannt;
  Offline-Betrieb bleibt erwünscht.
- Akzeptanzkriterien: Mobile Aktionen mit Außenwirkung fragen vorher nach;
  lokale/offline Nutzung ist möglich; externe Komponenten bleiben optional und
  klar getrennt.
- Status: nicht geprüft.

#### UR-50 – Fachlich verständliche Datenfluss-/Betriebsdokumentation

- Bereich: Dokumentation, Agenten/Worker
- Priorität: mittel
- Quellen (2 Nennungen): `harw-export-1789460270.md:8`,
  `2026-09-17-004053-this-session-is-being-continued-from-a-previous-c.txt:308`.
- Anforderung: Technische Datenflussdokumentation soll zusätzlich strukturiert
  und auf höherem, weniger technischem Niveau verständlich werden. Fachliche
  Exploration soll Perspektiven für Dokumentstruktur, Business Process,
  Datenfluss/Governance und DevOps/Betrieb einbeziehen und parallelisierbar sein.
- Akzeptanzkriterien: Fachliche Leser verstehen Ablauf und Verantwortung; Teams
  finden dennoch technische Übergaben, Grenzen und Betriebsanforderungen.
- Status: nicht geprüft.

### Wiederkehrende Arbeits- und Lieferanforderungen

#### UR-51 – Nicht bei Ankündigungen stehen bleiben

- Bereich: Arbeitsweise
- Priorität: hoch
- Quellen (7 Nennungen): `harw-export-1789469522.md:95`,
  `harw-export-1789465226.md:1395`, `harw-export-1789583648.md:606`,
  `harw-export-1789583648.md:622`, `harw-export-1789643187.md:1027`,
  `harw-export-1789643187.md:1141`, `harw-export-1789656943.md:3293`.
- Anforderung: Nach ausreichender Klärung soll Harw umsetzen und bis zu einem
  echten Ergebnis weiterarbeiten, statt wiederholt nur nächste Schritte,
  Erklärungen oder Toolcalls auszugeben. Bei langer Arbeit sind häufigere,
  nützliche Zwischenberichte gewünscht.
- Akzeptanzkriterien: Zwischenberichte benennen greifbaren Fortschritt; eine
  Aufgabe endet mit Ergebnis, expliziter Abnahmefrage oder klarer Blockade.
- Status: nicht geprüft.

#### UR-52 – Transkript- und Planwissen für Folgear­beit auswerten

- Bereich: Arbeitsweise, Persistenz
- Priorität: hoch
- Quellen (5 Nennungen): `harw-export-1789511666.md:8`,
  `harw-export-1789523460.md:4390`, `harw-export-1789524310.md:5208`,
  `harw-export-1789469522.md:8`, `2026-09-17-004053-this-session-is-being-continued-from-a-previous-c.txt:1676`.
- Anforderung: Vor Fortsetzung komplexer Arbeit sollen relevante Transkripte,
  Pläne und Projektgedächtnis ausgewertet werden, um offene Punkte,
  Delegationsvorschriften und tragende Invarianten zu berücksichtigen.
- Akzeptanzkriterien: Folgearbeit nennt ihre verwendeten direkten Quellen bzw.
  Memory-Kontexte und wiederholt keine dokumentierten Fehlmuster.
- Status: nicht geprüft.

## Historische Einzelaufträge, die nicht als dauerhafte Produktspezifikation zu lesen sind

Diese direkten Nutzeraufträge sind dedupliziert festgehalten, aber ihr Zweck war
offenbar jeweils die konkrete damalige Sitzung. Sie dürfen nicht automatisch als
weitergeltende Produktanforderung interpretiert werden.

- Repository/DoD erkunden: `harw-export-1789377255.md:8`.
- Den Codex-Auth-Prozess anhand von `codex-rs` reparieren: `codex-session-01a0a6bd-2550-76a1-b623-75259209bfe3.md:1962`.
- Letzte Transkripte auswerten, TUI-Aufgaben zuerst erledigen und offene Punkte
  berichten: `harw-export-1789519086.md:8`, `harw-export-1789523460.md:8`,
  `harw-export-1789524310.md:8`.
- Bereits Begonnenes vollständig beenden: `harw-export-1789503044.md:943`.
- Vor Sandbox-Nacharbeit zunächst alles committen: `harw-export-1789519086.md:588`,
  `harw-export-1789523460.md:576`, `harw-export-1789524310.md:576`.
- Gesamten damaligen Arbeitsstand pushen, die `spaces/` aber ignorieren:
  `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:13806`,
  `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt:13849`.
- OpenAI-Providerroute reparieren und Planungsnotizen durch Terra/Luna erstellen:
  `codex-session-01a0a807-9ad0-7d63-a3b4-37d9581be28b.md:3`.

Status aller historischen Einzelaufträge: nicht geprüft.

## Offene Widersprüche und noch zu treffende Entscheidungen

1. **Sandbox-Cargo:** Der Nutzer verlangt zugleich Cargo in der Sandbox *oder*
   einen speziellen Außensandbox-Pfad. Die Zielarchitektur (welcher Weg für
   welchen Build) ist noch nicht abschließend entschieden.
2. **Dauer der Sandbox-Lockerung:** Genannt sind Sitzungsende und „bis
   ausdrücklich wieder aktivieren“. Es fehlt eine eindeutige Entscheidung, ob
   beide Varianten auswählbar sind und wie sie sichtbar persistiert werden.
3. **Browserbesitzer:** Browser soll „immer Emily/UIA“ gehören, anschließend
   wurde UIA-Worker als bessere Alternative vorgeschlagen. Primärer Besitzer und
   Delegationsmodell sind offen.
4. **Standardmodell interner Arbeit:** OpenRouter wird bei Verfügbarkeit
   empfohlen und Nvidia Nemotron als Default genannt; zugleich wurde DashScope
   statt OpenRouter gewünscht. Der Vorrang zwischen globaler Empfehlung und
   konkreter Nutzerwahl muss festgelegt werden.
5. **UIA-Erstellung vs. aktive Emily:** Eine UIA soll anfangs nicht existieren
   und vom Nutzer angelegt werden; gleichzeitig soll Emily die tatsächliche UIA
   sein. Offen ist, ob Emily ein optionales Template oder eine bereits angelegte
   benutzeraktivierte Instanz ist.
6. **DoD-Abgrenzung:** DoD soll ein eigener Workspace sein, aber weiterhin zum
   Projekt gehören. Besitz von Crates, Release-Pfad und Laufzeitintegration sind
   nicht spezifiziert.
7. **Mobile-Backend:** Offline-Betrieb ist erwünscht, daneben Telegram und
   zuhause betriebene Daten-/Vektordienste. Welche Komponenten zwingend lokal
   und welche optional remote sind, ist offen.
8. **TUI-Statuszeile:** Eine konkrete störende Statuszeile soll verschwinden,
   während Provider/Modell/Token/Queue sichtbar sein sollen. Das endgültige
   Layout bzw. welche Statusinformationen dauerhaft gezeigt werden, braucht eine
   UX-Entscheidung.
9. **Retry-Politik:** Ein Retry nach ungefähr fünf Sekunden ist gefordert, aber
   Anzahl, Fehlerklassen, Backoff und Nutzerabbruch sind nicht festgelegt.

## Provenienz und Methodik

Es wurden ausschließlich die folgenden 25 vom Nutzer vorgegebenen Dateien
vollständig gelesen (insgesamt 101.235 Zeilen):

- `2026-09-15-201709-bash-inputcargo-cleanbash-input.txt`
- `2026-09-16-010759-hilf-mir-nochmal-dabei-das-wir-die-internen-model.txt`
- `2026-09-17-004053-this-session-is-being-continued-from-a-previous-c.txt`
- `2026-09-17-121558-image-1-nice-wre-wenn-harw-es-nach-sagen-wir.txt`
- `codex-session-01a0a6bd-2550-76a1-b623-75259209bfe3.md`
- `codex-session-01a0a807-9ad0-7d63-a3b4-37d9581be28b.md`
- `harw-export-1789377255.md` bis `harw-export-1789656943.md` (alle 18
  ausdrücklich aufgelisteten Exporte)
- `latest-claude-session.txt`

Als primäre Quellen gelten nur direkte Nutzerblöcke: `❯` in Claude-Texten,
`## User` in Codex-Exporten und `## Du` in Harw-Exporten. Wiederholungszahlen
zählen nur die hier aufgeführten direkten Fundstellen, nicht semantisch ähnliche
Assistant- oder Tooltexte. Inhaltlich identische Exporte wurden als getrennte
Nennungen gezählt, aber nicht als getrennte Anforderungen dupliziert.

Mehrere `## Du`-Blöcke enthalten eine kopierte Terminal-/Agentensitzung oder
einen eingeschobenen Hand-back-Report (besonders
`harw-export-1789388771.md:8`, `harw-export-1789398616.md:8`,
`harw-export-1789523460.md:4398` und
`harw-export-1789643187.md:1800`). Diese Blöcke sind als vermischt behandelt:
Nur eindeutig vom Nutzer formulierte Forderungen wurden übernommen; Aussagen,
Pläne, Statusmeldungen und angebliche Erledigungen von Assistant, Agenten,
Guards oder Tools begründen keine Primäranforderung. Eingebettete Credentials,
Pfade zu Secret-Dateien, IDs und ähnliche sensible Inhalte wurden nicht
wiedergegeben bzw. redigiert.

Die Formulierung „Status: nicht geprüft“ bedeutet ausdrücklich: Aus den
Transkripten wurde **nicht** abgeleitet, ob eine Anforderung im Code bereits
erfüllt, teilweise erfüllt oder offen ist. Es wurde kein Quellcode-Audit,
Build, Test oder sonstige Implementierungsprüfung durchgeführt.
