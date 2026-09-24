# Kommandozeile `harw`

Diese Seite beschreibt den Befehlsbaum des Programms `harw`, die globalen
Flags und die Sitzungs-Flags, das Verhalten von `--json` sowie die Zuordnung
der älteren Schreibweisen zu den neuen Befehlen. Die jeweils genaue Grammatik
der installierten Fassung zeigt `harw --help` bzw. `harw <befehl> --help`.

## Überblick

```text
harw [PROMPT] [-r [SITZUNG]] [--all]            Chat starten (wie `harw chat`)

Arbeiten
  chat [PROMPT] [-r [SITZUNG]] [--all]          Interaktiver Chat
  exec PROMPT…                                  Einmalige Anfrage ohne Oberfläche
  analyze [CRATE] [--workspace] [--order bottom-up|top-down]
          [--dry-run] [--max-parallel N]        Workspace-Abhängigkeitsanalyse
  session list [--all] | show ID | resume ID    Gespeicherte Sitzungen

Konfiguration
  config                                        Interaktives Menü (Alias: settings)
  config get KEY | set KEY [WERT]               Einzelnen Schlüssel lesen/setzen
         [--global | --project]                 (set ohne WERT löscht den Schlüssel)
  config permissions get | set-mode ask|auto|full
         | allow WERKZEUG [--pattern M] | deny WERKZEUG [--pattern M]
         | unallow INDEX | undeny INDEX
  config provider … | config model default ID
  provider list | add NAME --api DIALEKT --base-url URL [--auth REF] [--models A,B]
           | remove NAME | enable NAME | disable NAME
           | scan [NAME] [--free-only] [--prune]
  model                                         Modelle auflisten (Alias: models)
  model list | scan [NAME] [--free-only] [--prune] | add [PROVIDER/MODELL]
        | remove PROVIDER/MODELL | default ID | internal … | catalog [--refresh]
  auth login | token | import | status | prune [PROVIDER]
  project trust | untrust | status [DIR]

Agenten und Wissen
  agent uia-new | skills [ARGS…] | plugins [ARGS…]
  knowledge index [build [--source docs|knowledge] [--force] | status]
            | memory [ARGS…] | proposals [ARGS…]
  jobs list [FILTER] | show ID | approve ID [--note TEXT]
       | deny ID [--reason TEXT] | cancel ID | retry ID

Dienste
  gateway [install|start|stop|restart|enable|disable|status]
  serve [--config-dir DIR] | web [--socket PFAD]
  service install | status | uninstall
  mcp setup [SERVER] | check [SERVER]
  channel connect telegram [--pair CODE]

System
  init | onboard | doctor [--config-dir DIR] | update
  uninstall [--scope …] [--dry-run] [--yes]
  completions [SHELL] [--install | --uninstall] [--dry-run]
  bug-report [--type …] [--title …] …
  sandbox [status]
  debug echo TEXT… | classify TEXT…
  kill [ARGS…]
```

`agent skills`, `agent plugins`, `knowledge memory` und `knowledge proposals`
reichen alle folgenden Argumente unverändert weiter, auch solche mit
führendem Bindestrich. Sie entsprechen den Chat-Befehlen `/skills`,
`/plugins`, `/memory` und `/context-proposal`.

## Globale Flags

Globale Flags dürfen vor oder hinter jedem Befehl stehen und wirken bei allen
Befehlen.

| Flag | Wirkung |
| --- | --- |
| `--home DIR` | Verwendet `DIR` statt `HARW_HOME` bzw. `~/.harw` als Harwness-Verzeichnis. |
| `--profile NAME` | Verwendet das Profil `NAME` statt des aktiven Profils (hat Vorrang vor `HARW_PROFILE`). |
| `-C DIR`, `--cwd DIR` | Arbeitet so, als wäre `harw` in `DIR` gestartet worden (Projekt-Erkennung, Sitzungsliste, Aufträge). |
| `--log LEVEL` | Protokoll-Filter, z. B. `info` (Vorgabe), `debug` oder `harw_core=debug,info`. |
| `--log-sensitive` | Protokolliert auch Prompts, Werkzeugargumente und Antworten. Nur zur Fehlersuche. |
| `-v`, `--verbose` | Zeigt jeden Werkzeugaufruf mit allen Argumenten statt einer kurzen Vorschau. |
| `--json` | Gibt das Ergebnis als JSON aus (siehe unten). |

## Sitzungs-Flags

Sitzungs-Flags werden wie globale Flags an beliebiger Stelle angenommen,
wirken aber nur bei `chat` (einschließlich `harw` ohne Befehl), `exec` und
`analyze`. Bei jedem anderen Befehl bricht `harw` mit einer Fehlermeldung ab,
die auf `chat`, `exec` und `analyze` verweist, statt das Flag still zu
ignorieren.

| Flag | Wirkung |
| --- | --- |
| `--mode MODUS` | Startet im angegebenen Interaktionsmodus (`chat`, `plan`, `explore`, `work`, `shell`). |
| `--approval ask\|auto\|full` | Freigabemodus nur für diese Sitzung: `ask` fragt bei jedem Werkzeugaufruf, `auto` lässt unkritische Aufrufe durch und fragt beim Rest, `full` fragt nie. Überschreibt den konfigurierten Standard. |
| `--model ID` | Verwendet für diese Sitzung das Modell `ID` statt des Standardmodells. |
| `--goal TEXT` | Setzt beim Start ein Ziel, auf das die Sitzung hinarbeitet. |
| `--agent NAME` | Startet die Sitzung mit der Agentendefinition `NAME` als Wurzel (eingebaute Rolle wie `root-orchestrator` oder eine eigene Definition) statt der konfigurierten `active_agent_definition`. Nur für diese Sitzung; dauerhaft setzt man sie mit `/agent use NAME` (Entfernen: `/agent use --clear`), wirksam ab der nächsten Sitzung. |
| `--add-dir PFAD` | Erlaubt Dateizugriffe ohne Rückfrage zusätzlich unter `PFAD` (mehrfach angebbar). |

Nicht global sind die Chat-Flags `-r/--resume` und `--all`: sie gelten nur für
`harw` ohne Befehl und für `harw chat`. `--all` gibt es zusätzlich bei
`harw session list`.

## Ausgabe mit `--json`

Befehle mit JSON-Form schreiben mit `--json` genau ein formatiertes
JSON-Dokument auf die Standardausgabe; Hinweise und Fehler gehen weiterhin auf
die Standardfehlerausgabe.

- **Tabellen** (z. B. `harw session list`) werden zu einem Array von Objekten,
  deren Schlüssel die Spaltenüberschriften sind.
- **Einzelwerte** (z. B. `harw session show ID`) werden zu einem Objekt.
- **Operationen** (`harw jobs …`, `harw agent skills|plugins`,
  `harw knowledge memory|proposals`) liefern ihre strukturierten Daten, sofern
  vorhanden, sonst `{"text": "…"}` mit der Textausgabe.

Befehle ohne JSON-Form – etwa interaktive Dialoge wie `harw agent uia-new`,
`harw knowledge index` oder `harw provider …` – brechen mit `--json` mit der
Meldung `` `<befehl>` unterstützt --json nicht `` ab, statt das Flag zu
ignorieren. Der Exit-Status ist dann ungleich null.

## Ältere Schreibweisen

Die folgenden Schreibweisen funktionieren weiterhin, erscheinen aber nicht
mehr in der Hilfe. Beim Aufruf weist `harw` auf der Standardfehlerausgabe auf
den neuen Namen hin, z. B. ``Hinweis: `harw lens` heißt jetzt `harw knowledge index`.``
`settings` und `models` sind Aliase von `config` bzw. `model` und bleiben
dauerhaft gültig.

| alt | neu |
| --- | --- |
| `harw settings …` | `harw config …` |
| `harw settings provider …` | `harw provider …` (oder `harw config provider …`) |
| `harw models …` | `harw model …` |
| `harw models delete ZIEL` | `harw model remove ZIEL` |
| `harw models scan …` | `harw provider scan …` (oder `harw model scan …`) |
| `harw catalog [--refresh]` | `harw model catalog [--refresh]` |
| `harw connect --channel telegram [--pair CODE]` | `harw channel connect telegram [--pair CODE]` |
| `harw lens [build\|status]` | `harw knowledge index [build\|status]` |
| `harw uia new` | `harw agent uia-new` |
| `harw run TEXT…` | `harw debug echo TEXT…` |
| `harw classify TEXT…` | `harw debug classify TEXT…` |
| `harw analyze --bottom-up` / `--top-down` | `harw analyze --order bottom-up` / `--order top-down` |
| `/skills`, `/plugins` (nur im Chat) | `harw agent skills …`, `harw agent plugins …` |
| `/memory`, `/context-proposal` (nur im Chat) | `harw knowledge memory …`, `harw knowledge proposals …` |
| Aufträge nur im Chat | `harw jobs list\|show\|approve\|deny\|cancel\|retry` |
| `harw -r SITZUNG` | weiterhin gültig; zusätzlich `harw session resume SITZUNG` |
| `HARW_PROFILE=NAME harw …` | weiterhin gültig; zusätzlich `harw --profile NAME …` |

`--bottom-up` und `--top-down` schließen sich gegenseitig und mit `--order`
aus; eine Kombination ist ein Parse-Fehler.

## Beispiele

```bash
# Chat im Erkundungsmodus mit einem anderen Modell starten
harw --mode explore --model openrouter/qwen/qwen3-coder "Wie ist das Projekt aufgebaut?"

# Einmalige Anfrage in einem anderen Verzeichnis, ohne Rückfragen
harw -C ~/src/projekt --approval full exec "Formatiere alle Rust-Dateien"

# Sitzungen aller Projekte als JSON auflisten und eine fortsetzen
harw session list --all --json
harw session resume 3f2a

# Anbieter anlegen, Modelle abfragen, Standardmodell setzen
harw provider add lokal --api ollama --base-url http://localhost:11434
harw provider scan lokal
harw model default lokal/qwen3

# Anbieter-Katalog aus models.dev aktualisieren
harw model catalog --refresh

# Wartenden Auftrag freigeben bzw. ablehnen
harw jobs list
harw jobs approve job-17 --note "Geprüft"
harw jobs deny job-18 --reason "Zu weitreichend"

# Gedächtnis durchsuchen, Skills verwalten
harw knowledge memory search "Build-Fehler"
harw agent skills list

# Telegram anbinden und Kopplung abschließen
harw channel connect telegram
harw channel connect telegram --pair ABCD-EFGH

# Mit einem anderen Profil prüfen
harw --profile arbeit doctor

# Analyse von den Wurzeln abwärts, nur anzeigen
harw analyze --order top-down --dry-run
```
