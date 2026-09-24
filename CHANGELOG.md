# Changelog

All notable changes to this workspace are recorded here. Format follows
[Keep a Changelog](https://keepachangelog.com/) and this project uses
Semantic Versioning within the 0.x pre-release range.

## [Unreleased]

### Runde 5 (2026-09-24)

**CI und Toolchain**
- Toolchain gepinnt: `rust-toolchain.toml` mit `channel = "1.98.0"`
  (Komponenten `rustfmt`, `clippy`); lokal und in der CI prüfen damit derselbe
  Compiler, dasselbe `rustfmt` und dasselbe `clippy`. Details in
  `docs/setup/build-prerequisites.md`.
- `ci.yml`: `concurrency` mit `cancel-in-progress` (ein neuer Push bricht den
  alten Lauf ab); ein Filter-Job mit `dorny/paths-filter` entscheidet, welche
  Jobs laufen. Reine Doku-PRs (`docs/**`, `*.md`) fahren nur den Filter-Job,
  `deny` läuft nur bei Änderungen an Manifesten, `Cargo.lock` oder
  `deny.toml`. Bei Workflow-Änderungen läuft zusätzlich `actionlint`. Der
  Root-Job testet mit `cargo nextest`, Doc-Tests weiter mit `cargo test --doc`.
- Dependabot: alle GitHub-Actions-Updates (auch Major) in einer Gruppe, neues
  Ökosystem `rust-toolchain`, `open-pull-requests-limit: 3` je Ökosystem.

**Sicherheit: sudo-Freigabe im eigenen Fenster**
- Neues Werkzeug `host.sudo_exec {argv, reason}`, nur für `uia-shell-worker`
  und `host-process-worker` und nur in der TUI; überall sonst schlägt es
  geschlossen fehl. Es steht in `ALWAYS_ASK_TOOLS`: jeder Root-Befehl braucht
  eine eigene Freigabe.
- Die TUI zeigt ein eigenes Fenster mit exaktem argv, cwd und Worker. Das
  Passwort wird maskiert eingegeben und erreicht nie Modell, Verlauf,
  Eingabe-Historie, Logs, Traces oder Platte. Das Fenster fängt alle Tasten
  und Pastes ab.
- Zwei Varianten wie beim Host-Modus: **Einmalig** oder **Für diese
  Sitzung**. Die Sitzungsvariante behält das Passwort nur im Speicher der
  TUI, höchstens `[host] sudo_session_minutes` lang (Vorgabe 10, höchstens
  60, `0` = aus). Gelöscht wird es auch bei `/new`, `/resume`, Beenden und
  falschem Passwort. sudo läuft immer mit `-k`.
- Passwortloses sudo wird erkannt; dann fragt das Fenster nur „Freigeben /
  Ablehnen“.
- `shell.exec` lehnt Befehle ab, die mit `sudo`, `doas` oder `pkexec`
  beginnen, und verweist auf `host.sudo_exec`.
- Jede Entscheidung wird als Audit-Ereignis ohne Geheimnis protokolliert
  (argv, argv-Hash, Modus, Entscheidung, Exit-Code, Dauer).

**Werkzeuge**
- `fs.edit {path, old_string, new_string, replace_all?}`: gezieltes Ersetzen
  statt ganze Datei schreiben. `old_string` muss genau einmal vorkommen
  (außer mit `replace_all`), sonst gibt es einen Fehler mit Trefferzahl.
  Gleiche Grenzen wie `fs.write` (Workspace, Symlinks, Größe, atomar), braucht
  immer eine Freigabe. Die Werkzeugzelle zeigt „N Ersetzung(en) in <pfad>“ mit
  Diff-Ausschnitt. Zugelassen überall, wo `fs.write` zugelassen ist.
- `agent.result {child_id, offset?, max_bytes?, part?}`: lädt den ungekürzten
  Antworttext eines eigenen Kind-Laufs seitenweise nach; `part: "journal"`
  liefert stattdessen das Aktivitätsjournal (auch nach Abbruch).
- `agent.status {child_id?}` (lesend) und `agent.cancel {child_id}` (immer mit
  Freigabe) für Hintergrund-Agenten.
- `agent.message {child_id, text}` (Eltern → eigenes, laufendes Kind) und
  `parent.message {text, kind?: info|question}` (Kind → direkter Elternteil).
- `ask_user`: 1–4 Fragen mit je 2–4 Optionen plus Freitext, auch
  Mehrfachauswahl; nur Wurzel-Agent in der TUI.
- Plan-Modus-Werkzeuge `plan.write` (schreibt nur unter `.harw/plans/`),
  `plan.exit {plan_path}` und `plan.enter` (nur Vorschlag); nur Wurzel, nur
  TUI.
- `shell.exec` nimmt `request_host {reason}` an (Host-Mode-Anfrage, siehe
  unten) und `timeout_secs` bis `[shell] max_timeout_secs`.

**Agenten**
- Hintergrund-Orchestratoren: Ein Orchestrator, den die UIA in der TUI
  startet, läuft im Hintergrund weiter; der Turn der UIA endet sofort.
  `background: false` erzwingt den synchronen Lauf, Worker bleiben immer
  synchron. Ist ein Ergebnis da, startet im Leerlauf automatisch ein Turn der
  UIA; während eines Turns kommt es als Kontext in den nächsten. Beim Beenden
  fragt die TUI einmal nach, solange Hintergrund-Agenten laufen; `/new`
  bricht sie ab. Freigaben der Hintergrund-Agenten (auch sudo und Host-Modus)
  erscheinen auch im Leerlauf.
- Orchestrierungsgrenzen in `[agents]`: `max_root_orchestrators` (Vorgabe 1,
  erlaubt 1–4), `max_sub_orchestrators` (2, 1–6),
  `max_sub_orchestrator_depth` (2, 1–3), `max_spawn_depth` (4, 1–6).
  Ungültige Werte werden geklemmt. Eine Ablehnung erreicht das Modell als
  Werkzeugfehler, der Turn läuft weiter.
- Übergabe am Budget-Ende: Der Spawner hält eine Reserve des Token-Budgets
  zurück (5 %, mindestens 8000, höchstens 25 %) und lässt das Kind damit eine
  strukturierte Übergabe schreiben (höchstens 3072 Tokens, Zeitlimit 60 s).
  Scheitert das, geht wie bisher die letzte Antwort zurück. Mit
  `continue_from: <child_id>` (in `delegate_wave`-Zielen und bei
  `transfer_to_*`) setzt der Elternteil ein solches Kind mit derselben Rolle
  und frischem Budget fort, höchstens dreimal je ursprünglichem Kind.
- Aktivitätsjournal je Kind (Aufträge, Werkzeugaufrufe, geänderte Dateien,
  Enkel, Nachrichten). Endet ein Kind nicht regulär, bekommt der Elternteil
  einen Endbericht mit der Kopfzeile `[child_end status=… handoff=…]`, Grund,
  Übergabe und Journal-Kurzfassung.
- Nachrichten: `parent.message` mit `kind: "info"` höchstens einmal je 30 s,
  `kind: "question"` wartet bis zu 10 min auf eine Antwort über
  `agent.message`. Nachrichten sind auf 4 KiB gedeckelt.
- Host-Mode-Anfrage: `shell.exec` mit `request_host {reason}` öffnet das
  Host-Mode-Fenster mit Anfragendem (Rolle, Kind-ID, Baum-Pfad) und Grund,
  Varianten einmalig oder für die Sitzung. Scheitert ein Sandbox-Lauf
  erkennbar an der Sandbox, bekommt das Modell die Felder `sandbox_denial`
  und `host_mode_hint`, aber keine automatische Wiederholung.
- Freigaben von Kind-Agenten erscheinen im normalen Freigabedialog mit
  „angefragt von …“ (Wartezeit bis 10 min). Jede Zustimmung gilt nur für
  diesen einen Aufruf.
- Lease-Herzschlag: Solange ein Kind läuft, verlängert ein Takt seine Lease
  und die aller Vorfahren. Lange Builds oder Tests laufen damit nicht mehr in
  den Lease-Ablauf. Die Wanduhrgrenze gilt weiter; für Orchestratoren beträgt
  sie 3600 s.
- Diary auch für Kind-Agenten: Einträge nach einer Verdichtung und am Ende
  des Laufs, höchstens ein Endeintrag je Lauf, nichts bei Läufen ohne Turn.
- Auto-Modus: `auto` gibt Aufrufe außerhalb der festen Listen nach einem
  deterministischen Vorfilter und einem Klassifizierer frei (Rolle
  `auto-classifier`, `[internal_models.auto_classifier]`, ohne Wahl das
  schnelle Modell des aktiven Providers, bei Anthropic Haiku 4.5). Fehler oder
  mehr als 10 s führen zur Rückfrage, nie zur Freigabe. `ALWAYS_ASK_TOOLS`
  bleiben unberührt. Nach 3 Ablehnungen in Folge oder 20 insgesamt fällt die
  Sitzung auf `ask` zurück.
- Eigene Regeln: `[[permissions.allow]]`/`[[permissions.deny]]` nehmen
  Argument-Muster (`match = "cargo test*"` für Shell, `path = "src/**"` für
  Dateien). Deny gilt vor Allow und vor dem Klassifizierer, Deny-Regeln auch
  im Modus `ask`. Ab der dritten gleichartigen manuellen Freigabe bietet der
  Dialog „Ja, und künftig erlauben: <muster>“ an; nie für `ALWAYS_ASK_TOOLS`
  oder riskante Muster.
- Plan-Graphen: Der Plan-Store hält mehrere Pläne (`plan list|switch|archive`).
  Ein Plan der Modell-Fläche ist zunächst ein Vorschlag; `plan submit` legt
  ihn in der TUI zur Bestätigung vor und bindet ihn danach an ein Goal.
  `plan step <id> done <beleg>` meldet Fortschritt (`done` nur mit Beleg).
- Die Regeln für UIA-Worker erlauben kleine, abgegrenzte Code-Pakete.

**TUI**
- Das `/`-Popup reagiert auch während der Arbeit auf Hoch/Runter/Tab/Esc/
  Enter. Die Eingabe-Historie enthält auch `/`-Befehle.
- `Ctrl+O` klappt Werkzeugzellen auch während eines Turns auf und zu.
- `/btw <frage>`: flüchtige Nebenfrage zum Gespräch, ohne Werkzeuge und ohne
  den Agenten zu unterbrechen. Sie landet weder im Verlauf noch in der
  Historie; Esc bricht ab, Zeitlimit 60 s. Nur in der TUI.
- `/agent` zeigt den Agentenbaum mit Live-Werten, die Wurzel heißt
  „UIA · <name>“. Neu: `/agent stream <orchestrators|all|none>`, `/agent bg`
  und `/agent cancel <id>`. `/agents` entfällt; die Eingabe zeigt einen
  Hinweis auf `/agent`.
- Live-Stream: Orchestrator-Kinder zeigen Werkzeugaufrufe, Reasoning und
  Text als eingerückten Block unter ihrer Agent-Zeile
  (`[tui] child_stream`, Vorgabe `orchestrators`).
- Goal-Marke „◎ Goal: … · n/m“ in der Statuszeile für einen bestätigten,
  an ein Goal gebundenen Plan.
- Esc mit laufenden Kind-Agenten fragt nach („nochmal Esc zum Bestätigen“);
  ohne Kinder bricht Esc wie bisher sofort ab. Enter reiht während eines
  Turns ein und bricht nie ab.
- Plan-Modus: `Shift+Tab` schaltet `ask → auto → full → plan`; die Stufe
  `plan` zeigt „⏸ plan mode on (shift+tab to cycle)“ und sperrt schreibende
  und ausführende Werkzeuge sofort, auch mitten im Turn. `plan.exit` öffnet
  ein Fenster mit dem gerenderten Plan und drei Optionen (umsetzen mit
  `auto`, umsetzen mit `ask`, weiter planen mit Rückmeldung). Der
  freigegebene Plan bleibt angeheftet und übersteht die Verdichtung.
  `/plan show|list|open|edit` für Plan-Dateien, `/plan
  plans|switch|archive|inspect|submit|step` für Plan-Graphen.
- Auto-Modus in der Anzeige: automatisch freigegebene Aufrufe tragen
  „auto ✓ <Grund>“, abgelehnte „Vom Auto-Modus abgelehnt · <Kategorie>“.
  Neu: `/permissions allow|deny <tool> [muster] [--session|--project|--user]`,
  `/permissions rules`, `/permissions rm <nr>`, `/permissions log [anzahl]`.
- `/models worker [<rolle|all> <uia|ziel>]`; nach der UIA-Wahl in `/models`
  öffnet sich direkt der Bereich „UIA-Worker-Modelle“.
- Wissensbrowser: im Palace-Browser listet `T` die vorläufigen Topics, `p`
  belegt nach Rückfrage `/palace promote topic/<slug>` vor. Die Werkbank
  aktualisiert sich live bei Modell-Notizen, das Kanban-Board lädt Änderungen
  aus `harw serve` alle 2 s nach.
- „Host-Modus beendet (neue Sitzung)“, wenn ein Sitzungswechsel die
  Host-Phase beendet; `Ctrl+H` beendet auch eine prozessweite Host-Phase.

**Konfiguration**
- `[host] sudo_session_minutes` (Vorgabe 10, höchstens 60, `0` = aus;
  global, Profil darf nur verkürzen).
- `[internal_models.auto_classifier]` (Modell des Auto-Modus-Klassifizierers).
- `[uia_worker_models]` mit einem Eintrag je Rolle (`uia_worker`,
  `uia_shell_worker`, `uia_writer`, `uia_latex_writer`, `uia_explorer`), Wert
  `"uia"` (wie UIA) oder `"provider/modell"`. Ohne Eintrag gilt der alte
  `uia_worker_model`-Pin, jetzt mit dem Provider aus dem Katalog.
- `[tui] child_stream = "orchestrators" | "all" | "none"`.
- `[agents] max_root_orchestrators`, `max_sub_orchestrators`,
  `max_sub_orchestrator_depth`, `max_spawn_depth`.
- `[shell] max_timeout_secs` (Vorgabe 900, erlaubt 30–3600).
- Für `[agents]` und `[shell]` setzen Home und Profil frei; ein nicht
  vertrautes Projekt darf nur senken. Die Feldtabelle
  (`harw-config/src/scope.rs`) hat jetzt 111 Einträge, siehe
  `docs/design/config-scopes.md` §1.19–§1.22.

**Fehlerbehebungen**
- `.gitignore` in für alle beschreibbaren Projektordnern: best-effort, sonst
  Eintrag in `.git/info/exclude`.
- Credentials werden normalisiert: aller ASCII-Leerraum wird entfernt
  (Zeilenumbrüche, CRLF aus `.env`, umbrochene Pastes), auch bei
  `CLAUDE_CODE_OAUTH_TOKEN`/`ANTHROPIC_API_KEY`. `harw auth token` prüft das
  Format eines Anthropic-Tokens, ohne ihn auszugeben; `harw auth status` zeigt
  einen Formatbefund.
- `harw -r` und Sitzungsauswahl: Sitzungen ohne Nutzer-Turn werden nicht mehr
  gespeichert und im Picker ausgeblendet; eine fortgesetzte Sitzung zeigt
  einen Resume-Hinweis statt einer neuen Begrüßung.
- Verdichtung behält die letzten zwei Nutzer-Turns und den Arbeitsstand.
- `analyze` aus UIA-Sitzungen startet `uia-explorer` statt `analyst` (die
  Spawn-Matrix verbietet UIA → Worker); der Bericht trägt `status`/`notice`
  und markiert unvollständige Ergebnisse.
- `fs.read`: `null` bzw. `0` gilt als „nicht gesetzt“; die Fehlermeldung
  nennt die Felder.
- Kind-Antworten dürfen 32 KiB statt 8 KiB lang sein; beim Kürzen bekommen
  Anfang und Schluss je die Hälfte, die Marke verweist auf `agent.result`.
- `/mode` ändert das Modell nicht mehr.
- Das Transkript speichert die echten Ergebnisse von `transfer_to_*`.
- `shell.exec`-Zeitlimits: Vorgabe 30 s, Build- und Testbefehle (`cargo`,
  `make`, `npm`, `pytest`, `go`, …) 600 s, eigenes `timeout_secs` bis
  `[shell] max_timeout_secs`. Das CPU-Limit skaliert mit dem Zeitlimit.

**Modellkatalog**
- Aktuelle Anthropic-Modelle Fable 5.1, Opus 5.5, Sonnet 5 und Haiku 4.5
  sowie die Legacy-Modelle; Standardmodell ist Opus 5.5.

**Doku**
- Neue Anleitung `docs/guides/hintergrund-agenten.md` (Hintergrund-Orchestratoren,
  Nachrichten, Übergabe, Host-Mode-Anfrage, Grenzen).
- `README.md`, `docs/cli.md`, `docs/design/tui-command-contract.md` §8,
  `interaction-contract.md` §2.6, `tui-roles-models-modes.md` und
  `config-scopes.md` nachgezogen.

### Runde 4 (2026-09-24)

**Fehlerbehebungen**
- Onboarding Codex → API: `harw onboard` *ersetzt* jetzt den Credential-Pool
  des Providers, statt nur anzuhängen. Der neue Verweis steht vorn; frühere
  Einträge bleiben nur, wenn sie zur neuen Route passen (Codex-Login-Verweise
  nur auf der Codex-Route). `CredentialPool::from_auth_config` überspringt
  einen nicht auflösbaren *Failover*-Eintrag mit Warnung; ein kaputter
  primärer Verweis bleibt ein harter Fehler. Vorher startete `harw` nach dem
  Wechsel nicht (`file credential must be an absolute path below
  <home>/secrets`).
- TUI: lange Werkzeug-Ausgaben bleiben eingeklappt. Die Zelle rechnet in
  umgebrochenen Bildschirmzeilen (höchstens 3 Zeilen plus Hinweis, Ctrl+O
  klappt auf); auch aufgeklappt gilt ein Zeilenlimit. `explore.projects`,
  `explore.find` und `explore.relations` haben eine eigene Vorschau.
- Kind-Agenten: das Kontextfenster eines Kindes kommt vom Modell, das es
  wirklich ruft (Rückfall auf das Modell des Parents statt 32k-Vorgabe;
  unbekanntes Modell mit Warnung). Übergroße Aufträge werden gekürzt, die
  Verdichtung rechnet mit dem festen Overhead aus System-Prompt und
  Werkzeugen. Das Token-Budget zählt nur neue, ungecachte Eingabe plus
  Ausgabe und wird je Runde geprüft; ab 80 % kommt eine Abschluss-Anweisung,
  beim Limit liefert das Kind seine letzte Antwort als Teilergebnis
  (`budget_exhausted: true`, Fehlertyp `ChildBudgetExhausted` statt
  `AgentSpawnError`). Die Anzeige liest den Auftrag aus `task`.
- TUI: eingefügter Text kam beim Agenten nur als Platzhalter
  `[Pasted text #…]` an; `InputEditor::submission_text()` liefert jetzt den
  aufgelösten Text.

**Unterbrechen und Warteschlange (TUI)**
- Ctrl+C und Esc brechen nur den laufenden Turn ab; bereits abgeschickte
  Nachrichten und Befehle bleiben eingereiht und werden direkt danach
  ausgeliefert („Warteschlange wird gesendet“), auch bei offenem Freigabe-
  oder Host-Permit-Dialog. Esc schließt zuerst ein offenes Popup und scharft
  nie das Beenden.
- Befehle während eines Turns: `BusyAvailability` hat die drei Klassen
  `Immediate`, `Staged` (Änderung gilt ab dem nächsten Turn, z. B. `/model`,
  `/effort`, `/mode`, `/uia-*`) und `DeferredUntilTurnEnd`; Überschreibung je
  Unterbefehl über `busy_subcommands` am `#[operation]`. Sofort- und
  Staged-Befehle laufen als eigene Tasks (Zeitlimit 60 s) und blockieren
  Streaming und Freigaben nicht; busy-sichere lokale Befehle (Overlays,
  Picker, Panels) wirken sofort, Overlays bekommen Tasten. Eingereihte
  Nachrichten stehen sichtbar über dem Composer („Wartet auf den nächsten
  Turn“), Alt+↑ holt die letzte zurück. Ein Tabellentest über den ganzen
  Befehlskatalog hält die Einstufung fest.

**Wissensfläche (Workbench, Kanban, Diary, Palace, Dream)**
- Fundament: gemeinsamer Argument-Parser (`harw-ops/src/knowledge_args.rs`),
  prozessübergreifende Dateisperren (`harw-knowledge/src/lock.rs`, `fs4`),
  strukturierte Seitendateien für Diary und Traumberichte, Sichtbarkeit nach
  dem echten Aufrufer, Live-Updates über `AgentEventKind::Knowledge`.
- Neue Lese-Werkzeuge für Agenten: `workbench.show` (plus Kontext-Provider
  für Pins und Hypothesen), `kanban.list`/`kanban.show`, `diary.read` (nur
  eigene Einträge), `palace.search`/`palace.recall` (nur `established`). Kein
  Schreibwerkzeug; Telegram bekommt keines davon.
- `/workbench note edit|rm`, `/workbench retention [keep|<tage>d]`,
  `--scope=project`; Aufbewahrung je Scope (Sitzung 14 Tage, Projekt
  unbegrenzt).
- `/kanban edit|comment|evidence|approve|reject`; der Job-Worker holt Karten
  aus Worker-Spalten ab, startet den Rollen-Agenten aber erst nach
  `/kanban approve` (Risiko je Rolle) und schreibt Ergebnis und Verlauf an
  die Karte.
- Diary: automatische Einträge nach einer Verdichtung und am Sitzungsende;
  `/diary show --from/--to`, `/diary search`; `[knowledge.diary]
  retention_days`.
- Palace: `/memory promote <fakt-id>` macht aus einem Fakt ein
  `provisional`-Thema; `/palace supersede|edit|link` mit Review-Gate
  (`--confirm` bei `established`, Vorfassung im Verlauf); Index-Cache mit
  mtime-Prüfung.
- Dream: `/dream run|status|review`, strukturierte JSON-Vorschläge mit einem
  Reparaturversuch, Job `JobKind::Dream` im Ledger, Wissenspflege je Lauf
  (Diary-Rollup, Workbench-Aufbewahrung, Palace-Veraltung nur als
  Vorschlag). Neue Config `[dream]` (`enabled`, `budget`, `idle_minutes`,
  `cooldown_minutes`, `schedule`).
- TUI-Wissensbrowser: Diary nach Datum und Agent mit Suche und
  Bereichsansicht, Palace mit Links/Backlinks und Promote/Supersede mit
  Bestätigung, Dream-Review je Vorschlag; Kanban-Board mit Board-Auswahl,
  Worker-Spalten und Freigabe-Taste.

**LaTeX-Worker**
- Neue Rolle `uia-latex-writer` und Bundle-Agent `latex-writer`: schreibt
  LaTeX im Workspace, ohne Shell, Netz oder `deps.*`.
- Neues Werkzeug `latex.build`: startet nur `latexmk` in der Sandbox
  (`-no-shell-escape`, `-halt-on-error`, festes Argv, Zeitlimit, begrenzte
  Ausgabe), Freigabe je Aufruf. Fehlt TeX, meldet es `status: not_installed`
  mit Installationshinweisen, installiert aber nichts.
- Neue Skills `latex-writing` und `xelatex-compile`.

**Doku**
- `README.md` überarbeitet (MCP-Connectors allgemein, Wissensfläche,
  Matrix-Game, Bundle-Rollen und -Skills). Philosophie-Dokumente liegen
  unter `docs/philosophy/`, die DSL-Spezifikation unter
  `docs/design/agent-definition-dsl.md`, Sitzungsprotokolle unter
  `docs/sessions/`.
- `knowledge-surfaces.md` (Status, offene Fragen), `interaction-contract.md`
  §2.2/§2.6.3/§2.6.4, `tui-command-contract.md` und `config-scopes.md`
  (§1.17 `[knowledge]`, §1.18 `[dream]`) nachgezogen.

### Runde 3 (2026-09-24)

**Matrix-Game**
- `/matrix` mit umpire-geführtem Mehrspieler-Spiel: deterministischer
  Spielleiter in Rust, versiegelte Einreichung, Journal mit Replay, Sitze
  sehen nur ihre Projektion; Panel mit F9.
- Verhaltensprofile, Red Cell (neue Rolle `matrix-redcell`),
  Verdachtsleiter, Präzedenzregister, Inject-Pakete
  (`/matrix start <szenario> --package <id>`) und Laufvergleich
  (`/matrix compare`). Matrix-Sitze lesen nur Unterlagen-Kopien des Runners
  (Profil `MatrixReader`).

**Agenten und Recherche**
- `--agent NAME` startet die Sitzung mit dieser Agentendefinition als Wurzel;
  `/agent use NAME` (bzw. `--clear`) merkt die Wahl ab der nächsten Sitzung.
  Das SDK verlangt bei `--agent` keine UIA.
- Startmodus ist `chat` (`[mode] default`, `--mode`).
- `/research` für allgemeine Recherche durch ein read-only Kind
  (`researcher`); `/research-deps --generic` recherchiert
  ökosystem-neutral über `dependency-researcher`.
- Autor-Pipeline: Bundle-Agenten `business-author`, `business-reviewer`,
  `slides-builder`, `matrix-scenario-author`.

**Lernen und Skills**
- `/learn` legt dauerhafte Erkenntnisse der Sitzung nur als Vorschläge ab;
  jede Übernahme braucht ein ausdrückliches `accept` und einen eigenen
  Schritt des Operators.
- Neue Skills `business-writing-pyramid`, `learning-loop`,
  `author-review-pipeline`, `matrix-scenario-design`.

**Telegram**
- Ein Telegram-Chat mit gebundenem Workspace bekommt das Profil
  `WorkspaceEdit`: Lesen läuft ohne Rückfrage, jedes Schreiben fragt per
  Button (erzwungen `Delegated`), keine Shell, kein Netz.

**Modellkatalog**
- GPT-6 Sol und GPT-6 Luna mit Daten aus den Modellkarten (Kontext
  1.050.000, Output 128.000) und OpenRouter-IDs.

**Neuer Befehlsbaum der Kommandozeile (`harw`)**
- Befehle nach Aufgaben geordnet: `chat`, `exec`, `analyze`, `session`,
  `config`, `provider`, `model`, `auth`, `project`, `agent`, `knowledge`,
  `jobs`, `gateway`, `serve`, `web`, `service`, `mcp`, `channel` sowie die
  System-Befehle. Vollständige Referenz mit Zuordnung alt → neu in
  [`docs/cli.md`](docs/cli.md).
- Neu: `harw exec PROMPT…` für eine echte einmalige Anfrage ohne Oberfläche;
  `harw session list [--all] | show ID | resume ID`; `harw provider
  list|add|remove|enable|disable|scan`; `harw model catalog [--refresh]`;
  `harw agent uia-new|skills|plugins`; `harw knowledge index|memory|proposals`;
  `harw jobs list|show|approve [--note]|deny [--reason]|cancel|retry`;
  `harw channel connect telegram [--pair CODE]`; `harw debug echo|classify`.
  Skills, Plugins, Gedächtnis, Kontext-Vorschläge und Aufträge sind damit
  auch außerhalb des Chats erreichbar.
- Neue globale Flags `--profile NAME`, `-C/--cwd DIR` und `--json`; `-v` als
  Kurzform von `--verbose`. Befehle ohne JSON-Form brechen bei `--json` mit
  einer Fehlermeldung ab, statt das Flag still zu ignorieren.
- Neue Sitzungs-Flags `--approval ask|auto|full` und `--model ID`. Sitzungs-
  Flags (`--mode`, `--approval`, `--model`, `--goal`, `--add-dir`) wirken nur
  bei `chat`, `exec` und `analyze`; bei anderen Befehlen meldet `harw` einen
  Fehler, statt sie still zu ignorieren.
- `harw analyze --order bottom-up|top-down` ersetzt `--bottom-up`/`--top-down`
  (beide weiterhin versteckt gültig, aber nicht miteinander oder mit
  `--order` kombinierbar).
- Umbenannt: `settings` → `config`, `models` → `model` (alte Namen bleiben
  Aliase), `models delete` → `model remove` (Alias `delete`). Ältere
  Schreibweisen `connect`, `lens`, `uia`, `catalog`, `run` und `classify`
  funktionieren weiter, sind aber versteckt und verweisen per Hinweis auf den
  neuen Befehl.

### Added

- **`harw project` subcommand**: `harw project trust [DIR]`, `harw project untrust [DIR]`,
  and `harw project status [DIR]` manage a project's trust record explicitly (`DIR`
  defaults to the current directory). A project's repo-local `.harw` configuration
  layer is now loaded only for projects that have been explicitly trusted this way.

**Shell-Completions (`harw completions`)**
- Neuer Befehl `harw completions [SHELL] [--install|--uninstall] [--dry-run]`
  (bash, zsh, fish, elvish, powershell; ohne `SHELL` wird die Shell aus
  `$SHELL` erkannt). Ohne Flag wird das Skript wie bisher auf stdout
  ausgegeben. `--install` schreibt es an den kanonischen Ort (zsh:
  `$ZSH_CUSTOM/completions/_harw` bei oh-my-zsh, sonst
  `~/.local/share/zsh/site-functions/_harw` plus verwalteter `fpath`-Block in
  `.zshrc` vor `compinit`; bash:
  `~/.local/share/bash-completion/completions/harw`; fish:
  `~/.config/fish/completions/harw.fish`) und ersetzt dabei ältere
  harw-verwaltete Installationen samt veralteter Pfade, rc-Blöcke und
  `.zcompdump*`-Caches, statt sie zu stapeln. `--uninstall` entfernt all das,
  `--dry-run` zeigt nur an, was geschehen würde. `--all-binaries` ruft
  zusätzlich `completions` für jedes auf `$PATH` gefundene DoD-Binary auf.
  `harw completion` bleibt als verstecktes Alias erhalten.
- Neue Crate `harw-completions`: Skripterzeugung mit Verwaltungsmarker,
  Ortsauflösung pro Shell, Installation/Deinstallation sowie die
  wiederverwendbaren clap-Argumente (`CompletionsArgs`,
  `CompletionsSubcommand`, Feature `clap-args`).
- Die DoD-Binaries `harw-sentinel`, `harw-warden`, `harw-probe-fs` und
  `harw-probe-bpf` haben einen Unterbefehl `completions` mit denselben
  Optionen erhalten; er läuft vor jedem Sensor-, Socket- oder
  Landlock-Start.

**Freigaben, Regeln und Lebensdauern (Scopes)**
- Neuer `SettingScope`-Typ (`harw-config/src/scope.rs`) für die drei Lebensdauern
  einer Einstellung: `Session` (nur im Speicher), `Project` (dauerhaft pro Projekt,
  autoritätsgewährend außerhalb des Repos unter
  `~/.harw/profiles/<profil>/projects/<schlüssel>/`) und `Global` (dauerhaft für
  den User, `~/.harw/config.toml`). Bei einem Modus-Konflikt gewinnt die höhere
  Präzedenz (`Session > Project > Global`).
- `PermissionsSection`/`RuleToml` (`harw-config/src/permissions_toml.rs`) und ein
  neuer `ConfigWriter` (`harw-config/src/writer.rs`, `toml_edit`, atomares
  Schreiben, `.bak.<n>`-Backup-Rotation) zum dauerhaften Setzen von
  Freigabemodus, Freigabe-Timeout, Allow-/Deny-Regeln und zusätzlichen
  Arbeitswurzeln, ohne bestehende Kommentare oder Formatierung der Datei zu
  verlieren. `ConfigWriter::save` validiert vor dem Schreiben und lässt die
  Datei bei einem ungültigen Wert unangetastet.
- `AllowRuleSet`/`ApprovalRule` (`harw-extension-api/src/allow_rules.rs`):
  Allow-/Deny-Regeln je Werkzeug mit `RuleScope` (Session/Project/Global). Eine
  passende Deny-Regel gewinnt scope-übergreifend immer über jede Allow-Regel.
  Regeln für `shell.exec` vergleichen ganze Befehls-Tokens als Präfix und
  greifen als Allow-Regel nie bei einem zusammengesetzten Befehl (`;`, `&&`,
  `||`, `|`, Backtick, `$(`, Umleitung, Zeilenumbruch, Hintergrundjob); Regeln
  für `fs.*`-Werkzeuge werten ein Pfad-Glob aus und greifen als Allow-Regel nie
  bei einer `..`-Pfadkomponente. `derive_shell_rule` leitet aus einem
  tatsächlich ausgeführten Befehl einen konservativen Regel-Vorschlag ab und
  verweigert das für eine feste Liste breiter Interpreter/Wrapper (`bash`,
  `python3`, `sudo`, `xargs`, `eval` u. a.).
- Diese Regeln sind in die tatsächliche Freigabeentscheidung verdrahtet:
  `DefaultApprovalPolicy::review` (`harw-registry-defaults/src/lib.rs`) befragt
  die geteilte `AllowRuleSet` vor der Modus-Logik; eine Deny-Regel fragt immer
  nach, auch im `full`-Modus. `RuntimeAssembly` sät Modus, Allow-/Deny-Regeln
  und zusätzliche Arbeitswurzeln beim Start aus der Global- und der
  Projekt-Konfiguration (`harw-runtime/src/assembly.rs`).
- `/permissions` (`harw-ops/src/permissions.rs`) mit den Unterbefehlen `show`
  (Default), `mode`/`set <ask|auto|full>`, `allow`/`deny <tool> [muster]` und
  `remove <nr>`, jeweils wahlweise mit `--session`, `--project` oder
  `--global`. Jede Änderung wirkt sofort auf die laufende Session und wird
  zusätzlich in der jeweiligen Konfigurationsebene persistiert.
- `/add-workdir` (`harw-ops/src/add_workdir.rs`) sowie `ExtraRootsCell`
  (`harw-sandbox/src/extra_roots.rs`): zusätzliche, sitzungsweite
  Arbeitsverzeichnis-Wurzeln, mit `--save` dauerhaft im Projekt gemerkt.
  Kandidaten werden symlink-frei kanonisiert; abgelehnt werden das
  Wurzelverzeichnis `/`, das Home-Verzeichnis des Nutzers, jeder Vorfahre der
  primären Arbeitswurzel sowie mehr als 8 zusätzliche Wurzeln.

**Projekt-Erkennung und Projekt-Home**
- `harw-home/src/project.rs`: `discover_project` erkennt den Projekt-Root
  anhand konfigurierbarer Marker (Default `.git`, `project_root_markers` in
  der Konfiguration), unterscheidet gewöhnliche Git-Repositories,
  Git-Worktrees (der Trust-Anker zeigt dabei auf das Haupt-Repository) und
  markerlose Verzeichnisse. `project_key` liefert einen stabilen,
  dateisystemsicheren Schlüssel je Projekt-Root. `ProjectHome` legt
  `<root>/.harw/{memories,plans,goals,state}` mit Rechten `0700` an, schreibt
  ein `.gitignore` für `state/` und verweigert dies für `/` und `$HOME`.
  `RuntimeAssembly` legt dieses Projekt-Home bei jedem Start an.

**Session-Metadaten und Auswahl**
- `SessionMeta`-Sidecar (`harw-session-store/src/meta.rs`,
  `<session-id>.meta.json`): Titel samt Herkunft (`model`/`manual`/`fallback`/
  `none`), Erstellungs- und letzter-Öffnen-Zeitpunkt, Arbeitsverzeichnis,
  Projekt-Zuordnung, gekürzte erste Nutzernachricht und Turn-Zahl. Fehlt der
  Sidecar (ältere Sessions), wird er beim ersten Zugriff aus dem Transcript
  abgeleitet und danach persistiert.
- Neue TUI-Bausteine: `harw-tui/src/session_picker.rs` (Navigation per
  Pfeiltasten/PageUp/PageDown/Home/End, Tippfilter auf Titel/Projekt,
  Umschalten aller Projekte) und `harw-tui/src/relative_time.rs`
  (`gerade eben`, `vor 5 min`, `vor 3 h`, `vor 2 d`, sonst Datum).

**Freigabe-Dialog und Bedienbausteine (TUI)**
- `harw-tui/src/approval_dialog.rs`: Freigabe-Dialog-Widget mit vier Optionen
  (Ja / Ja und nicht mehr fragen für diesen Befehl / Ja und in den
  Auto-Modus wechseln / Nein mit optionaler Freitext-Begründung).
  Tastendrücke wirken erst nach einer Arming-Verzögerung, Esc gilt als Nein,
  ein Countdown wechselt unter einer Minute die Warnfarbe.
- `harw-tui/src/choice_dialog.rs`: generisches Auswahl-Dialog-Widget (u. a.
  für `/export`: Zwischenablage kopieren / als Datei speichern / abbrechen).
- `harw-tui/src/clipboard.rs`: Kopieren in die Zwischenablage über
  `wl-copy`/`xclip`/`xsel`/`pbcopy` je nach erkannter Umgebung, mit
  OSC-52-Fallback für Terminals ohne lokalen Zugriff.
- `harw-tui/src/export.rs`: rendert einen Chat-Verlauf als Markdown
  (Metadaten, Nutzer-/Assistenz-/Systemzeilen) und schreibt ihn atomar in
  eine Datei, kollisionssicher und ohne bestehende Dateien zu überschreiben.
- `harw-tui/src/history_cell.rs`: neue `ToolCell`/`ToolGroupCell`-Zelltypen
  mit einstellbarer Ausführlichkeit (`ToolVerbosity`), die die kompakte
  Darstellung von Werkzeugaufrufen tragen sollen.
- Neuer Befehl `/export` (`harw-ops/src/export.rs`) und erweitertes `/memory`
  (`harw-ops/src/memory.rs`) um `recall <stichwort>`,
  `record <text> [--project|--global]` und `forget <name>`.

**Langzeitgedächtnis v3 (Fakten)**
- `harw-memory/src/facts.rs`: adressierbare Fakten als einzelne
  Markdown-Dateien (`facts/<name>.md`) mit Frontmatter (`type`, `scope`,
  `confidence`, `sources`, `tags`) und `FactStore` zum Lesen, Schreiben,
  Löschen und Durchsuchen. Ein generierter Index (`MEMORY.md`), Nutzungszähler
  (`usage.json`) und ein Verfallsmechanismus (`decay`: unbenutzte Fakten
  verlieren nach einer konfigurierbaren Frist die Hälfte ihrer `confidence`)
  gehören dazu. Vor jedem Schreiben werden gängige Geheimnis-Muster
  (API-Schlüssel, GitHub-/AWS-/Slack-Tokens, PEM-Blöcke, `Bearer`-Header,
  `key=`/`token=`/`secret=`-Werte) redigiert.

### Fixed

- **Kontextbudget verdrängt die auslösende Nutzernachricht nicht mehr.** Eine
  lange Werkzeug-Runde (viele `fs.read`/`shell.exec`-Ergebnisse im selben
  Turn) konnte zuvor die zuletzt gesendete Nutzernachricht aus dem an das
  Modell geschickten Verlauf verdrängen, weil die Byte-Budget-Auswahl allein
  nach Alter der Gruppen entschied. `TurnHistory::tail_preserving_current_turn`
  (`harw-core/src/history.rs`) hält die auslösende Nutzernachricht jetzt in
  jedem Fall im Budget: übergroße Einzelergebnisse werden zuerst gekappt,
  danach werden ältere Tool-Ergebnispaare des offenen Turns gekürzt oder
  ausgelassen, bevor die Nutzernachricht selbst gefährdet wäre.

### Changed

- **CLI-Werte werden beim Parsen geprüft.** Ein ungültiger `--log`-Filter oder
  ein unbekannter `--mode` ist jetzt ein Parse-Fehler (vorher stillschweigend
  akzeptiert). Feste Wertemengen sind als Enums typisiert und erscheinen in
  `--help` und in den Shell-Completions: `connect --channel`,
  `uninstall --scope`, `auth login|token --provider`, `auth import --source`,
  `mcp setup|check --server`, `lens build --source`,
  `settings provider add --api`, `settings permissions set-mode` und
  `models internal openrouter-defaults`. Pfad-, URL- und Freitext-Argumente
  tragen passende Value-Hints.

- **Auto-Compact löst bei 500 000 Input-Tokens aus** (vorher 120 000):
  `DEFAULT_ABSOLUTE_CEILING_TOKENS` (`harw-core/src/auto_compact.rs`) steuert
  über den absoluten Deckel der `AutoCompactPolicy`, wie `maybe_compact`
  (`turn_loop.rs`) aus `last_round_usage.input_tokens` entscheidet.

- `harw web` no longer accepts `--config-dir`; the root space is resolved exclusively
  from `--home`/`HARW_HOME`, which is now required. The web surface's permission
  ceiling remains `ReadWorkspace` for every caller tier — no tier can gain write
  access through the web API.
- `harw serve`: job submission is now restricted to configured submitter principals,
  and both prompt jobs and plan-node jobs require a resolvable HARW home; a job
  submitted without one now completes as `Blocked` instead of running with an
  implicit, looser context.
- One-shot prompts (non-interactive chat) now apply the resolved interaction mode
  and the configured approval policy; a tool call that would need an interactive
  approval prompt is now rejected outright instead of being left pending.
- `harw run` (local echo) now executes under a real local principal (`uid:<n>`,
  operator tier) instead of a fixed placeholder identity.
- `harw doctor` now reports the actual assembled runtime's permissions, tool count,
  and approval chain alongside the existing configuration summary.
- `harw analyze` without `--dry-run` now requires a fully configured model provider
  up front, instead of failing only once the operation actually needed one.

### Removed

- `harw web --config-dir` flag.
- `harw-channel-browser`.
- The embedded MCP client in the core runtime library.

### Security

- `harw serve` now refuses to start if the configuration lists the same MCP
  principal ID more than once, instead of silently disabling the duplicate and
  continuing.
- TUI `/tools`: runtime overrides (`on`, `reset`, `profile`) can never enable a tool
  beyond the intersection of the base tool set and the active mode's tool set;
  previously a runtime toggle could re-enable a tool the active mode had disabled.
- TUI: `!`-prefixed shell commands are rejected outright — the TUI surface does not
  grant shell-execute capability.
- TUI: child roles configured under the `[agents]` config section cannot currently
  be spawned from the TUI (temporary regression; tracked for a follow-up wave).
- Gateway: the echo-model fallback has been removed, and sealed `secrets:`-provider
  credentials are now resolved when mounting the gateway, instead of the mount
  silently proceeding without a real provider.
- Plan-node jobs: the session sandbox is now bound exactly to the derived workspace
  root; a workspace nested under a parent directory that carries a project marker
  no longer inherits that parent's broader filesystem access.
- Prompt jobs run without any project documents injected into context.
- `harw-agent-dsl::roles::can_spawn` (the closed UIA/Root/Child/Worker spawn
  matrix, §3 of the DSL spec) was documented as "enforced by the runtime" but
  was never actually called anywhere outside `harw-agent-dsl` itself —
  `harw-core` had no dependency on `harw-agent-dsl` at all, so
  `ManagedAgentSpawner::admit` never checked whether a spawning session's
  organizational role was permitted to create the target role (e.g. a
  `Worker`, which must never create durable children, was not prevented from
  doing so by the runtime). `SpawnContext` gained an `organizational_role:
  AgentRoleId` field, `ChildRoleDefinition`/`ManagedAgentSpawner::with_role`
  now carry the target organizational role, and `admit()` rejects the spawn
  fail-closed via `can_spawn` before any sandbox/depth/lease check. Covered
  by a new `harw-core` integration test. `ManagedAgentSpawner` is not yet
  wired into `harw-cli`/`harw-tui` production paths, so this closes a latent
  gap in the library ahead of that wiring rather than a currently reachable
  vulnerability. Identified via source-grounded review inspired by
  `hardening-suggestive-inspiration.md`.
- **G11 — `deny_unknown_fields` audit**: Added `#[serde(deny_unknown_fields)]`
  to 17 container-level structs across `harw-config` and `harw-protocol` that
  deserialize from untrusted TOML configs or wire payloads. Unknown fields are
  now rejected at deserialization, preventing silent authority injection via
  surplus keys. Identified via bottom-up codebase review inspired by
  `hardening-suggestive-inspiration.md` §11.
- **G12 — Remove redundant `unsafe impl Send/Sync`**: `ShortTermMemory` in
  `harw-memory/src/short_term.rs` had manual `unsafe impl Send` and `unsafe
  impl Sync` blocks that were redundant — `RwLock<Inner>` with all-`Send`
  fields is automatically `Send + Sync`. Removed both blocks, eliminating a
  soundness risk surface.
- **Hardening gap analysis**: `docs/design/hardening-gap-analysis.md` —
  comprehensive 14-gap analysis from bottom-up codebase review against
  `hardening-suggestive-inspiration.md` (32 sections). (the closed UIA/Root/Child/Worker spawn
  matrix, §3 of the DSL spec) was documented as "enforced by the runtime" but
  was never actually called anywhere outside `harw-agent-dsl` itself —
  `harw-core` had no dependency on `harw-agent-dsl` at all, so
  `ManagedAgentSpawner::admit` never checked whether a spawning session's
  organizational role was permitted to create the target role (e.g. a
  `Worker`, which must never create durable children, was not prevented from
  doing so by the runtime). `SpawnContext` gained an `organizational_role:
  AgentRoleId` field, `ChildRoleDefinition`/`ManagedAgentSpawner::with_role`
  now carry the target organizational role, and `admit()` rejects the spawn
  fail-closed via `can_spawn` before any sandbox/depth/lease check. Covered
  by a new `harw-core` integration test. `ManagedAgentSpawner` is not yet
  wired into `harw-cli`/`harw-tui` production paths, so this closes a latent
  gap in the library ahead of that wiring rather than a currently reachable
  vulnerability. Identified via source-grounded review inspired by
  `hardening-suggestive-inspiration.md`.

### TUI Hardening

- **Grapheme cluster cursor movement**: InputEditor cursor movement
  (move_left, move_right, backspace, delete, word-jump) now operates on
  Unicode extended grapheme cluster boundaries via unicode_segmentation,
  not char boundaries. This fixes cursor corruption with combining diacritics
  (e.g. German umlauts encoded as base + combining mark, ZWJ emoji sequences).
  Backspace and forward-delete now drain the entire grapheme cluster range,
  not a single byte. Inspired by codex-rs TextArea grapheme-aware movement.

- **Display-width-aware wrapping**: visible_lines and cursor_position
  now use unicode_width::UnicodeWidthStr for column math instead of char
  count. CJK/wide glyphs (display width 2) are correctly accounted for in
  soft-wrap breakpoints and cursor column calculation. Fixes misalignment
  with wide-character text.

- **History recall boundary gate**: Up/Down keys now only trigger history
  recall when the buffer is empty or the cursor is at position 0/len AND the
  current text matches the last-recalled entry. A last_recalled field tracks
  the most recently loaded history entry. This prevents accidental history
  navigation when the cursor is mid-text in a single-line draft.

- **Paste normalization**: TuiEvent::Paste handler now normalizes CRLF
  to LF and CR to LF before inserting. Prevents stray carriage returns
  from corrupting multi-line pasted text.

- **Dynamic composer height**: The input box height now grows with the actual
  wrapped line count (visible_lines(width)) instead of counting only hard
  newlines. Max height raised from 6 to 10 rows.

- **Semantic border color**: The input box border now uses
  style::border_color(theme), applying the semantic palette (RGB values
  for dark/light themes) to the block border. Fixes the dead_code warning
  for the previously unused border_color function.

## [0.2.0] — 2026-07-16

### Added

**Operation Registry & Command Surface**
- `OperationMeta.aliases` flow through `CommandRegistry::from_operation_registry`;
  `/m`, `/p`, `/reasoning` now dispatch to the same handlers as `/model`,
  `/provider`, `/effort`.
- `OperationRegistry::try_register` returns `Err(RegistryError::DuplicateName |
  AliasCollision | SelfCollision)` on name/alias conflicts at registration time.
- Multi-segment `Surface::Command { path }` values are accepted (previously
  silently skipped).
- `FromRawArgs` derive gained strict compile-time checks: field type must be
  `Option<String>` for positional attrs, conflicting `#[raw(...)]` attrs on one
  field are an error, missing `#[raw(...)]` on a named field is an error, tuple
  and unit structs are rejected, multiple `#[raw(required)]` fields are rejected,
  unknown raw keys are rejected, `nth = 0` is rejected with a friendly message.
  Seven trybuild compile-fail cases enforce all diagnostics.

**Long-lived Session Controller**
- `ChatApp` holds `Arc<TuiSessionController>` as a persistent field.
- `command_exec::build_services()` accepts the controller Arc as a parameter.
- `apply_pending_controller_state` hook fires at the safe turn boundary (before
  `run_turn_streaming` starts), flushing queued mutations onto `AgentSession`.
- `TuiSessionController::apply_to_session` propagates `reasoning_effort`,
  `active_model`, and `active_provider` onto the `AgentSession`.

**Real /model, /provider, /effort routing**
- `AgentSession` gained `active_model: Option<ModelId>` and
  `active_provider: Option<ProviderId>` fields with accessors and setters,
  mirroring `reasoning_effort`.
- `ModelRequest` gained `model_id` and `provider_id` fields; `turn_loop.rs`
  plumbs both from the session into the next request.
- `/provider switch <id>` validates provider existence, credential resolvability,
  and active-model compatibility before mutating. Atomic fail-close on any failure.
- `/model switch <id>` validates via `harw_model_catalog::resolve` and
  cross-checks against the active provider. Atomic fail-close on incompatibility.
- `/provider show` reports the actual runtime provider; `/provider list` marks
  active, auth-ok, auth-missing states.
- `/model show` reports the actual runtime model; `/model list` filters by active
  provider and marks the current selection.
- `ReasoningEffort::Minimal` maps to `None` on the Anthropic adapter (omits
  `output_config.effort`; reasoning stays adaptive).

**HARW SDK Facade**
- New crate `harw` at the workspace root re-exports canonical types from nine
  crates under modules: `extension`, `ops`, `agent`, `provider`, `model`, `core`,
  `defaults`, `types`. `harw::prelude` re-exports the load-bearing types.
- `harw/examples/minimal.rs` provides a runnable example.

**Agent DSL / IR**
- `ExecutableAgentIr` gained `snapshot_id: SnapshotId` computed via BLAKE3 over a
  stable byte stream; deterministic across processes, excludes `trace` (which
  carries timestamps).
- `DslError` variants `MissingBase`, `MissingMixin`, `AuthorityElevation`,
  `IllegalRoleForMixin` carry `DiagLocation { layer, field_path }`. Display now
  includes the field path (e.g. `"authority.capabilities"` on `AuthorityElevation`).

**Type Consolidation**
- `harw-types` newtypes (`ProviderId`, `ModelId`, `ProviderName`, `ModelName`,
  `AgentName`, `CustomerId`) gained `Deref<Target = str>`, `AsRef<str>`,
  `Borrow<str>`, bidirectional `PartialEq<str/&str/String>`, `PartialOrd`, `Ord`.

**Testing Infrastructure**
- `harw-core::testing::RecordingModelProvider` records every `ModelRequest` for
  integration tests.
- New integration tests: `harw-agent-dsl/tests/toml_to_ir_e2e.rs` (5 tests),
  `harw/tests/sdk_example.rs` (1 test), plus expanded unit tests in all touched
  modules.
- E2E suite §7 slices 1–4, 12, 13 shipped; slices 5–11 (real-turn recording) are
  in flight and not gated for this pre-release.

### Changed

- `CommandRegistry::from_operation_registry` now returns
  `Result<Self, TuiRegistryError>`; call sites must propagate or handle the error.
- The dispatch-adapter list in `harw-tui/src/command_exec.rs` no longer maintains
  a parallel alias truth source; the executor consults `OperationMeta::aliases`
  directly.
- `register_all_second_pass_rejects_duplicates` replaces the old
  `register_all_is_additive` test; additive registration is no longer permitted.
- `/model` and `/provider` unknown subcommands now return `InvalidArguments`
  listing the supported set instead of silently falling back to `show`.
- `harw-model-catalog` migrated from `pub type ProviderId = String` to
  `pub use harw_types::{ProviderId, ModelId}` (199 mechanical substitutions across
  12 files; no runtime behaviour change).
- `AgentArgs / SkillsArgs / PluginsArgs`: the single `cmd: Option<String>` field
  is replaced by `action / target / value` fields.

### Fixed

- `AgentArgs / SkillsArgs / PluginsArgs` argument-tokenization bug that discarded
  the second token in `/agent stop <id>`.
- `FromRawArgs` permissive derive accepted invalid field configurations silently;
  now a compile-time error.
- Fresh-per-command `TuiSessionController` construction caused state resets on
  every command; the controller is now long-lived on `ChatApp`.
- Silent snapshot writes for `/model` and `/provider` when no actual switch
  occurred.
- `run_loop` in `harw-tui` no longer hard-crashes the process when a chat
  turn fails (e.g. an invalid/expired provider credential returning HTTP
  401): `TuiError::Core` is now caught, shown as a readable system line in
  the chat transcript, and the session stays alive; only `TuiError::Io`
  (fatal terminal failures) still exits and ends the process.
- `harw-ops::provider::auth_status_label` had an unreachable wildcard match
  arm that only surfaced under the `harw-provider` crate's default (non
  `chatgpt-oauth`) feature set, breaking `cargo clippy -- -D warnings` on
  ordinary builds; replaced with an explicit `#[cfg(feature =
  "chatgpt-oauth")]`-gated arm, and `harw-ops` now forwards a matching
  `chatgpt-oauth` feature to `harw-provider`.
- `harw` SDK facade example test extended (`sdk_example_toml_to_executable_runtime_context`)
  to prove the full pipeline — TOML parse → `resolve_definition` → `lower`
  into `ExecutableAgentIr` → `assemble_default_registry` →
  `AgentSession::new` — is expressible using only the public `harw::`
  facade, closing the previously-partial coverage of E2E test item #12
  (SDK example must build a registry, compile an agent definition, AND
  produce an executable runtime context).

### Deprecated

Nothing formally deprecated in this 0.x pre-release cycle.

### Known Unstable

The following areas are intentionally out of scope for this 0.2.0 milestone and carry no
stability guarantee:

- `Harness::builder()` fluent API — planned for a later 0.2.x pre-release.
- Typed Parent-to-Child return pipeline (Agent-as-Tool) — child returns a string
  today; fachliche typed return is deferred.
- Full IR-to-Runtime consumption — `ExecutableAgentIr` exists but the runtime
  still consumes `AgentRole` from `harw-types` directly, not the IR.
- `GoalGraph` / `FinalObjective` / `GoalEvaluator` — architecture only, not
  implemented runtime.
- `DurableJobRunner` has no CLI runtime caller yet.
- Memory system Cognition Loop is not fully wired end-to-end.

### Migration

See [docs/migration/0.1.0-to-0.2.0.md](docs/migration/0.1.0-to-0.2.0.md).
