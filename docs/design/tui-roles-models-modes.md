# Rollen, Modelle, Modi und Freigabe in der TUI

Status: Ist-Stand 2026-09-24 (Runde 5), verbindlich für TUI und `harw-ops`.
Zugehörig: `tui-command-contract.md` §8 (Befehle, Tasten, Präfixe).
Quellen: `harw-config/src/role_models.rs`, `harw-config/src/internal_models.rs`,
`harw-config/src/uia_worker_models.rs`,
`harw-ops/src/{models,model,mode,effort}.rs`,
`harw-extension-api/src/approval_mode.rs`, `harw-runtime/src/approval.rs`
(`ApprovalChain::for_child`), `harw-runtime/src/assembly.rs`
(`effective_approval_mode`), `harw-runtime/src/config.rs`
(`apply_model_override`), `harw-cli/src/main.rs` (`resolve_startup_mode`).

---

## 1. Modell je Rolle

`harw_config::resolve_role_models(&config)` liefert pro `ModelRole` genau
eine Zeile `RoleModelRow { role, provider, model, source, reasoning_effort }`.
`/models show` und die Ansicht F8 zeigen genau diese Tabelle, dazu das
Live-Modell der laufenden Sitzung.

| Rolle (`key`) | Label | Konfiguration | Mögliche Herkunft (`RoleModelSource`) |
|---|---|---|---|
| `uia` | Benutzeroberfläche (UIA) | `uia_provider` + `uia_model` | `UiaPin` (beide gesetzt **und** Provider vorhanden und aktiviert), sonst `DefaultModel`, sonst `Unset` |
| `uia-worker` | UIA-Worker | `[uia_worker_models] uia_worker`, sonst alter Pin `uia_worker_model` | feste Wahl (`provider/modell`) oder `InheritsUia` (Wert `"uia"`). Seit Runde 5 **ohne** Kopplung an den UIA-Provider, Einzelheiten §1.1 |
| `orchestrator` | Orchestrator | `[internal_models.root_orchestrator]` | `Explicit`, sonst `DefaultModel`/`Unset` — **nie** OpenRouter-Standard |
| `sub-orchestrator` | Sub-Orchestrator | `[internal_models.sub_orchestrator]` | `Explicit`, sonst `InheritsOrchestrator` (Provider/Modell der Orchestrator-Zeile) — nie OpenRouter-Standard |
| `worker-simple` | Worker (einfach) | `[internal_models.worker_simple]` | `Explicit` / `OpenRouterDefault` / `DefaultModel` / `Unset` |
| `worker-complex` | Worker (komplex) | `[internal_models.worker_complex]` | wie oben |
| `explorer` | Explorer | `[internal_models.explorer]` | wie oben |
| `research` | Recherche | `[internal_models.research]` | wie oben |
| `compaction` | Verdichtung | `[internal_models.compaction_summary]` | wie oben |
| `title` | Sitzungstitel | `[internal_models.session_title]` (Legacy: `session.title_model`) | wie oben |
| `memory` | Gedächtnis-Konsolidierung | `[internal_models.memory_consolidation]` | wie oben |
| `dream` | Traum-Reflexion | `[internal_models.dream_reflection]` | wie oben |
| `auto-classifier` | Auto-Modus-Klassifizierer | `[internal_models.auto_classifier]` | `Explicit`, sonst das schnelle Modell des aktiven Providers (Anthropic: `claude-haiku-4-5`; sonst nach Namensmerkmalen wie `haiku`, `mini`, `flash`); nie OpenRouter-Standard |

Herkunft im Einzelnen:

| `RoleModelSource` | Label | Bedeutung |
|---|---|---|
| `Explicit` | explizit gewählt | `[internal_models.<stelle>]` mit gesetztem `model` |
| `OpenRouterDefault` | OpenRouter-Standard | keine explizite Wahl, `use_openrouter_defaults = true` (Standard) und ein Provider `openrouter` ist aktiviert und hat Auth. Modell: `nvidia/nemotron-3-super-120b-a12b` für Explorer, Recherche, Worker (komplex); `nvidia/nemotron-3.5-lightning` für Titel, Verdichtung, Gedächtnis, Traum, Worker (einfach) |
| `UiaPin` | UIA-Pin | `uia_provider`/`uia_model` |
| `UiaWorkerPin` | UIA-Worker-Pin | `uia_worker_model` |
| `InheritsUia` | erbt von UIA | UIA-Worker ohne eigenen Pin |
| `InheritsOrchestrator` | erbt vom Orchestrator | Sub-Orchestrator ohne eigene Wahl |
| `DefaultModel` | Standardmodell | `default_provider`/`default_model`. Für interne Stellen heißt das zur Laufzeit: der Aufrufer nutzt sein aktives Modell (bei Kindern das Elternmodell) |
| `Unset` | nicht gesetzt | kein `default_model` konfiguriert |

Sonderfall: ein `[internal_models.<stelle>]`-Eintrag **ohne** `model` (auch
mit `provider`) erzwingt das Hauptmodell (`DefaultModel`), unabhängig von
`use_openrouter_defaults`.

Reasoning-Effort je Zeile: rollenspezifisches `[reasoning]`-Feld (`uia`,
`root_orchestrator`, `sub_orchestrator`, `worker_simple`, `worker_complex`),
sonst `default_reasoning_effort` des Modells, sonst des Providers, sonst leer.

Laufzeit-Zuordnung von Kindrollen: `explorer` → Explorer; `researcher-web`,
`researcher-deps`, `analyst` → Recherche; `memory-steward` → Gedächtnis;
`agent-steward` → Worker (komplex); Orchestrator-Definitionen über ihre
Organisationsrolle (`root-orchestrator` → Orchestrator, `child-orchestrator`
→ Sub-Orchestrator); Worker nach Aufgabenkomplexität (einfach/komplex); die
`uia-worker`-Familie (`uia-worker`, `uia-explorer`, `uia-writer`,
`uia-shell-worker`) bekommt ihr Modell aus der UIA-Sitzung.

### 1.1 Eigene Modellwahl je UIA-Worker-Rolle (Runde 5, Teil G)

Jede Rolle der `uia-worker`-Familie hat eine eigene Wahl unter
`[uia_worker_models]` (TOML-Schlüssel mit `_` statt `-`):

```toml
[uia_worker_models]
uia_worker       = "uia"                       # wie UIA (folgt auch einem Live-Wechsel)
uia_shell_worker = "anthropic/claude-sonnet-5" # feste Wahl, Trennung am ersten `/`
# uia_writer, uia_latex_writer, uia_explorer
```

- Ohne Eintrag gilt der alte Pin `uia_worker_model` als feste Wahl, jetzt mit
  dem Provider, dem das Modell im Katalog gehört, sonst „wie UIA“.
- Eine feste Wahl bleibt nur, solange ihr Provider angemeldet ist; sonst fällt
  die Rolle mit einem Hinweis auf „wie UIA“ zurück. Ein Wechsel des
  UIA-Providers scheitert damit nie an einer Worker-Bindung.
- Laufende Worker behalten ihr Modell bis zum Ende ihres Laufs, neue Worker
  nehmen die neue Wahl.
- Setzen: `/models worker [<rolle|all> <uia|ziel>]` oder in `/models` (F8):
  nach der UIA-Wahl öffnet sich direkt der Bereich „UIA-Worker-Modelle“
  (Enter wählt das Modell einer Rolle, `a` setzt alle auf „wie UIA“, Esc
  beendet).

## 2. Wann eine Modelländerung wirkt

**Regel: jede Modell- und Effort-Wahl wirkt ab der nächsten Sitzung — außer
`/model` (und `/effort`), die die laufende Sitzung live ändern.**

| Befehl | Schreibt | Wirkung |
|---|---|---|
| `/model switch <id>` | Live-Zustand der Sitzung (Provider + Modell atomar, auch providerübergreifend) | **sofort** (ab nächstem Turn) |
| `/effort <stufe>` | Live-Zustand der Sitzung | sofort |
| `/models set <rolle> <ziel>` | Profil-`config.toml` (`uia_*`, `uia_worker_model` bzw. `[internal_models.*]`) | ab nächster Sitzung |
| `/models reset <rolle>` | entfernt die explizite Wahl | ab nächster Sitzung |
| `/models worker <rolle\|all> <uia\|ziel>` | Profil-`config.toml` (`[uia_worker_models]`) | neue Worker ab sofort, laufende behalten ihr Modell |
| `/uia-model`, `/uia-worker-model`, `/uia-effort` | Profil-`config.toml` | ab nächster Sitzung |

`/models set uia-worker` akzeptiert seit Runde 5 auch Modelle anderer
Provider. `/mode` ändert das Modell nicht (mehr). Keiner dieser Befehle hat ein
Modell-Werkzeug: das Modell darf weder sein eigenes noch das Modell seiner
Kinder wählen.

## 3. Modus, Freigabe und Shift+Tab

Zwei unabhängige Achsen:

| | Interaktionsmodus (`InteractionMode`) | Freigabemodus (`ApprovalMode`) |
|---|---|---|
| Werte | `chat`, `plan`, `explore`, `work`, `shell` | `ask` (`AlwaysAsk`), `auto` (`Delegated`), `full` (`FullAccess`) |
| Steuert | welche Werkzeuge das Modell sieht und die Sandbox-Obergrenze | ob ein erlaubter Werkzeugaufruf nachfragt |
| Ändern | `/mode <modus>`, F7 (Abschnitt „Modus“) | `/permissions mode <ask\|auto\|full> [--session\|--project\|--global]`, F7 (Abschnitt „Freigabe“, Scope `--session`) |
| Wirkt | an der **nächsten Turn-Grenze** (vorgemerkt; Statuszeile „(ausstehend)“) | **sofort**, auch während eines Turns |
| Standard | `[mode] default` (Vorgabe `chat`), persistierbar per `/mode default <modus>` bzw. `d` in F7 | `[permissions].default_mode`, sonst `auto` |

`Shift+Tab` ist ein Schnellzyklus über beide Achsen:
`ask → auto → full → plan → ask`. Die Stufe `plan` merkt sich den bisherigen
Modus, setzt Freigabe `ask` und fordert Modus `plan` an; die nächste Stufe
setzt wieder nur die Freigabe und stellt den gemerkten Modus wieder her.
Bei offenem `/`-Popup wirkt `Shift+Tab` nicht. Statuszeile:
`Modus: <modus> · Freigabe: <ask|auto|full>`.

### 3.0 Plan-Modus (Runde 5, Teil F)

Die Stufe `plan` ist ein vollwertiger Plan-Modus:

- Statuszeile „⏸ plan mode on (shift+tab to cycle)“ in eigener Farbe;
  Composer-Hinweis „Plan-Modus – es wird nichts verändert“.
- Die Sperre wirkt **sofort**, auch mitten im Turn (`PlanModeGate`): nichts
  Schreibendes, keine Ausführung. Das einzige Schreibwerkzeug ist
  `plan.write`, und es schreibt nur unter `.harw/plans/<slug>.md`.
- Der Agent darf schreibgeschützte Kinder (`explorer`/`researcher`,
  höchstens 3 parallel) starten und mit `ask_user` strukturierte Rückfragen
  stellen (nur Wurzel, nur TUI).
- `plan.exit` öffnet ein Fenster mit dem gerenderten Plan und drei Optionen:
  1. umsetzen im Auto-Modus → Modus `work`, Freigabe `auto`;
  2. umsetzen, Änderungen einzeln freigeben → `work`, `ask`;
  3. weiter planen, mit Freitext-Rückmeldung an den Agenten.
- `plan.enter` ist nur ein Vorschlag des Agenten; erst „Ja“ schaltet um.
- Der freigegebene Plan bleibt als angehefteter Kontext in jeder Anfrage und
  übersteht die Verdichtung. `/plan show|list|open|edit` verwaltet die
  Plan-Dateien; `/plan` bzw. `/mode plan` schalten ein.

### 3.0.1 Auto-Modus (Runde 5, Teil E)

In `auto` gibt die Runtime frei, was in `AUTO_APPROVED_TOOLS` steht. Für die
übrigen Aufrufe gilt diese Reihenfolge:

1. `ALWAYS_ASK_TOOLS` (`process.kill`, `host.sudo_exec`, `agent.cancel`, …)
   fragen immer.
2. Deny-Regeln, dann Allow-Regeln (`[[permissions.deny]]`/`[[permissions.allow]]`
   mit `tool` und optional `match` bzw. `path`). Deny-Regeln gelten auch in
   `ask`.
3. Ein deterministischer Vorfilter: Schreiben außerhalb des Workspace, in
   `.git`/`.harw` oder an Credential-Pfaden, `rm -rf` außerhalb,
   `git push --force`, `curl … | sh`, Netz außerhalb der Policy — ein Treffer
   ist nie eine Freigabe.
4. Der Klassifizierer (Rolle `auto-classifier`, ohne Werkzeuge, Geheimnisse
   vorher entfernt) entscheidet `allow|ask|deny` mit Kategorie und Grund.
   Fehler, Parsefehler oder mehr als 10 s → Rückfrage.

Die Werkzeugzelle zeigt „auto ✓ <Grund>“ bzw. „Vom Auto-Modus abgelehnt ·
<Kategorie>“; `/permissions log` listet die letzten Entscheidungen. Nach 3
Ablehnungen in Folge oder 20 in der Sitzung fällt die Sitzung auf `ask`
zurück. Ab der dritten gleichartigen manuellen Freigabe bietet der Dialog
„Ja, und künftig erlauben: <muster>“ (Sitzung oder Projekt) an, nie für
`ALWAYS_ASK_TOOLS` oder riskante Muster.

### 3.1 Kinder folgen der Freigabe live, gedeckelt auf `auto`

`ApprovalChain::for_child` gibt jedem Kind eine **Folgezelle**
(`ApprovalModeCell::follower(ApprovalMode::Delegated)`): das Kind liest bei
jeder Prüfung den aktuellen Modus der Elternzelle, höchstens aber `auto`
(`ApprovalMode::capped_at`). Folgen:

- Wurzel `ask` → Kinder `ask`; Wurzel `auto` oder `full` → Kinder `auto`.
- Eine Umstellung an der Wurzel (Befehl, F7, Shift+Tab) erreicht auch
  bereits **laufende** Kinder sofort.
- Ein Kind erhält nie `full`.
- Ein lokales `set` auf der Kindzelle koppelt dieses Kind ab; es bleibt
  gedeckelt, Eltern und Geschwister sind unberührt.

Freigaberegeln (`AllowRuleSet`) werden unverändert geteilt (eine Regel
erlaubt einem Kind nie mehr, als seine eigene Werkzeugfläche zulässt).

## 4. Rangfolge beim Start

Gilt für `harw`/`harw chat`, `harw exec` und `harw analyze`.

| Größe | Rangfolge (höchste zuerst) | Fehlerfall |
|---|---|---|
| Interaktionsmodus | `--mode` > `[mode] default` (Vorgabe `chat`) | unbekannter Name in **beiden** Fällen ein Fehler (kein stiller Rückfall auf `chat`) |
| Wurzel-Agent | `--agent` > `active_agent_definition` (persistierbar per `/agent use <name>`, entfernbar per `/agent use --clear`, beides ab nächster Sitzung) | unbekannter Name: Startfehler (fail-closed); `/agent use` prüft Name und Rolle (Wurzel-, Kind-Orchestrator oder Worker) vor dem Speichern |
| Freigabemodus | `--approval` > Projekt-`[permissions].default_mode` > globales `[permissions].default_mode` > Vorgabe der Einstiegsart (für alle Einstiege `auto`) | ungültiger `--approval`-Wert: Parser-Fehler; ungültiger Config-Wert wird übersprungen |
| Modell | `--model` > `default_provider`/`default_model` (bzw. UIA-Pin) | `--model` wird gegen `config.models` geprüft: zuerst Katalogschlüssel, dann Modell-ID, dann Alias (bei mehreren Treffern der alphabetisch erste Schlüssel); kein Treffer = Konfigurationsfehler |

`--model` setzt `default_model` (Katalogschlüssel) und `default_provider`
(Provider des Eintrags) für diesen Lauf und hebt einen UIA-Pin
(`uia_provider`/`uia_model`) für diesen Lauf auf, damit die Wahl auch für
die interaktive Sitzung gilt. Die übrigen Rollen folgen daraus nach §1
(z. B. `DefaultModel`, `InheritsUia`). Nichts davon wird persistiert.

## 5. Offen

- ~~`[mode] default` = `plan` → `chat`~~: entschieden und umgesetzt —
  die Vorgabe ist jetzt `chat` (`harw-config/src/mode_toml.rs`,
  `default_mode`). Wer mit Planung starten will, setzt
  `[mode] default = "plan"` bzw. `/mode default plan`.
- Modell im Agenten-Ereignis (`AgentOrchestrationEvent.model`,
  `ChildRecord.model`) für die Anzeige des Kind-Modells: geplant.
