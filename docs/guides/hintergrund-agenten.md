# Orchestratoren im Hintergrund: Steuerung, Nachrichten, Übergabe

Stand: Runde 5 (2026-09-24). Diese Anleitung beschreibt, wie die UIA in der
TUI Orchestratoren im Hintergrund laufen lässt, wie Eltern und Kinder
miteinander sprechen, was bei Budget-Ende oder Abbruch übrig bleibt und
welche Grenzen gelten. Maßgeblich ist der Code; die Quellen stehen am Ende.

## 1. Überblick

```text
Nutzerin ──► UIA (Wurzel, TUI)
               │ transfer_to_<orchestrator> {task, background?}
               ▼
        Root-Orchestrator  ◄── agent.status / agent.result / agent.message / agent.cancel
               │ delegate_wave / transfer_to_*          ▲
               ▼                                        │ parent.message {info|question}
        Sub-Orchestratoren, Worker ─────────────────────┘
```

Ein Hintergrund-Kind ist dasselbe admittierte Kind wie im synchronen Fall:
dieselbe Sandbox, dasselbe Budget, dieselbe Freigabekette. Der
Hintergrundlauf verschiebt nur, **wann** die UIA das Ergebnis sieht.

## 2. Orchestratoren im Hintergrund

- Übergibt die UIA in der TUI an einen Orchestrator (`transfer_to_<rolle>`),
  läuft er im Hintergrund. Das Werkzeug kehrt sofort mit
  `{child_id, status, hint}` zurück, der Turn der UIA endet, und die
  Nutzerin kann weiterarbeiten.
- `background: false` erzwingt den synchronen Lauf. Worker-Ziele laufen
  immer synchron, ebenso jeder Einstieg außerhalb der TUI (`harw exec`,
  Telegram, Gateway).
- Ist der Orchestrator fertig, startet im Leerlauf automatisch ein Turn der
  UIA mit dem Ergebnis. Läuft gerade ein Turn, kommt das Ergebnis als
  Kontext in den nächsten.
- Freigaben, sudo- und Host-Mode-Fenster eines Hintergrund-Agenten
  erscheinen auch dann, wenn die TUI im Leerlauf ist.
- `/new` bricht laufende Hintergrund-Agenten ab. Beim Beenden fragt die TUI
  einmal nach, solange noch welche laufen. Die Statuszeile nennt laufende
  Hintergrund-Agenten.

In der TUI:

| Befehl | Wirkung |
|---|---|
| `/agent` | Agentenbaum mit Live-Werten (Wurzel „UIA · <name>“) |
| `/agent bg` | Hintergrund-Agenten mit Fortschritt auflisten |
| `/agent cancel <id>` | eigenen Hintergrund-Agenten abbrechen |
| `/agent stream <orchestrators\|all\|none>` | Live-Block der Kinder im Verlauf (Vorgabe aus `[tui] child_stream`) |

Der Live-Stream zeigt Werkzeugaufrufe, Reasoning und Text eines Kindes als
eingerückten Block unter seiner Agent-Zeile. `/agents` gibt es nicht mehr.

## 3. Abfragen und Steuern (Werkzeuge der Eltern)

| Werkzeug | Argumente | Freigabe | Zweck |
|---|---|---|---|
| `agent.status` | `{child_id?}` | keine (lesend) | laufende und zuletzt beendete Hintergrund-Läufe der eigenen Sitzung: Werkzeugaufrufe, Tokens, Laufzeit, letzter Schritt, Journal-Auszug, geänderte Dateien |
| `agent.result` | `{child_id, offset?, max_bytes?, part?}` | keine (lesend) | ungekürzter Antworttext eines eigenen, beendeten Kindes, seitenweise (höchstens 48 KiB je Seite); `part: "journal"` liefert das Aktivitätsjournal, auch bei laufenden oder abgebrochenen Kindern |
| `agent.message` | `{child_id, text}` | keine | Nachricht an ein eigenes, laufendes Kind |
| `agent.cancel` | `{child_id}` | **immer** (`ALWAYS_ASK_TOOLS`) | eigenen Hintergrund-Lauf abbrechen |

Alle vier kennen nur eigene Kinder. Die aufrufende Sitzung kommt aus dem
Ausführungskontext, nie aus Modell-Argumenten; fremde und unbekannte IDs
bekommen dieselbe Meldung.

Die Antwort eines Kindes an den Elternteil ist auf 32 KiB gekürzt (Anfang und
Schluss je zur Hälfte). Die Kürzungsmarke nennt `agent.result` mit der
`child_id`.

## 4. Nachrichten zwischen Eltern und Kind

```text
agent.message  { child_id, text }              // Eltern → eigenes, laufendes Kind
parent.message { text, kind?: info|question }  // Kind → direkter Elternteil
```

- Das Kind liest eine `agent.message` an seiner nächsten Runden-Grenze als
  „[Nachricht von <rolle>] …“. So gibt die Nutzerin über die UIA
  Kurskorrekturen, ohne den Lauf abzubrechen.
- `parent.message {kind: "info"}` meldet einen Zwischenstand, höchstens
  einmal je 30 s.
- `parent.message {kind: "question"}` wartet bis zu 10 min auf eine Antwort.
  Die nächste `agent.message` des Elternteils beantwortet sie. Ohne Antwort
  bekommt das Kind „keine Antwort – arbeite mit einer begründeten Annahme
  weiter“.
- Nachrichten sind reiner Text, höchstens 4 KiB, und verleihen keine Rechte.
  Postfächer sind begrenzt.

## 5. Budget-Ende, Abbruch, Fortsetzen

**Übergabe am Budget-Ende.** Der Spawner hält vom Token-Budget eines Kindes
eine Reserve zurück: 5 %, mindestens 8000 Tokens, höchstens 25 %. Der Turn
des Kindes endet schon bei `limit − reserve`. Mit der Reserve schreibt
dasselbe Modell genau eine strukturierte Übergabe (höchstens 3072 Tokens,
Zeitlimit 60 s) mit den Abschnitten Auftrag, Erledigt, Befunde mit Belegen,
Offene Punkte, Empfohlene nächste Schritte und Stand beim Abbruch. Scheitert
das, geht wie bisher die letzte Antwort als Teilergebnis zurück.

**Endbericht bei Abbruch und Fehler.** Endet ein Kind nicht regulär
(Zeitbudget, Lease-Ablauf, Abbruch, Provider- oder Werkzeugfehler), bekommt
der Elternteil statt eines nackten Fehlers einen Endbericht. Die erste Zeile
ist maschinenlesbar:

```text
[child_end status=cancelled handoff=…]
```

Danach folgen Grund, Übergabe (falls möglich) und eine Kurzfassung des
Journals. Das vollständige Journal holt `agent.result {child_id, part:
"journal"}`.

**Fortsetzen.** Ein am Budget beendetes Kind lässt sich mit derselben Rolle
fortsetzen:

```json
{ "targets": [ { "role": "explorer", "continue_from": "child-7", "task": "nur Modul C" } ] }
```

`continue_from` gibt es in `delegate_wave`-Zielen und bei
`transfer_to_<rolle>`. Das neue Kind bekommt die Übergabe als ersten Kontext
(`task` ist dann optional) und ein frisches Budget. Höchstens drei
Fortsetzungen je ursprünglichem Kind. Die Fortsetzung durchläuft die normale
Zulassung, und ihre Sandbox darf nicht weiter sein als die des Vorgängers.

## 6. Host-Mode anfragen

Scheitert ein Befehl an der Sandbox, kann ein Shell-fähiger Agent den
Host-Modus anfragen:

```json
{ "command": "…", "request_host": { "reason": "Build braucht Zugriff auf /dev/kvm" } }
```

- Die TUI zeigt das Host-Mode-Fenster mit dem Anfragenden (Rolle, Kind-ID,
  Pfad im Agentenbaum) und dem Grund. Varianten wie bei `/sandbox-lease`:
  einmalig oder für die Sitzung.
- Nach der Zustimmung läuft der Befehl über den bestehenden Host-Pfad. Die
  Variante „für die Sitzung“ gilt prozessweit: alle Shell-fähigen Agenten
  dieser Sitzung laufen danach ohne neue Frage auf dem Host. `Ctrl+H` beendet
  die Phase, ein Sitzungswechsel ebenfalls („Host-Modus beendet (neue
  Sitzung).“).
- Scheitert ein normaler Sandbox-Lauf erkennbar an der Sandbox, bekommt das
  Modell die Felder `sandbox_denial` und `host_mode_hint`. Das ist nur ein
  Hinweis, nie eine automatische Wiederholung auf dem Host.
- Außerhalb der TUI endet jede Anfrage geschlossen mit einem Fehler.

Root-Befehle sind davon getrennt: Nur `uia-shell-worker` und
`host-process-worker` dürfen `host.sudo_exec {argv, reason}` nutzen. Das
Passwort wird im eigenen TUI-Fenster eingegeben und erreicht nie das Modell.

## 7. Freigaben von Kind-Agenten

Braucht ein Kind eine Freigabe, erscheint sie im normalen Freigabedialog mit
„angefragt von …“. Das Kind wartet bis zu 10 min. Jede Zustimmung gilt nur
für diesen einen Aufruf: kein Moduswechsel und keine Regel für das Kind. Eine
Frage der Wurzel hat Vorrang; die Kind-Frage erscheint danach wieder.

Kinder folgen dem Freigabemodus der Wurzel, höchstens `auto`
(`tui-roles-models-modes.md` §3.1).

## 8. Grenzen

| Grenze | Wert | Einstellbar |
|---|---|---|
| gleichzeitige Orchestratoren der UIA | 1 (1–4) | `[agents] max_root_orchestrators` |
| gleichzeitige Sub-Orchestratoren je Baum | 2 (1–6) | `[agents] max_sub_orchestrators` |
| Verschachtelung der Sub-Orchestratoren | 2 (1–3) | `[agents] max_sub_orchestrator_depth` |
| allgemeine Kind-Tiefe | 4 (1–6) | `[agents] max_spawn_depth` |
| Wanduhrgrenze eines Orchestrators | 3600 s | `[spawn.budget] max_wall_secs` der Definition |
| Fortsetzungen je Kind | 3 | fest |
| Nachrichtenlänge | 4 KiB | fest |
| `parent.message` info / question | 1 je 30 s / 10 min Wartezeit | fest |
| Kind-Freigabe | 10 min Wartezeit, einmalig | fest |
| `shell.exec`-Zeitlimit | 30 s, Build/Test 600 s, `timeout_secs` bis 900 s (30–3600) | `[shell] max_timeout_secs` |

- Ungültige Werte in `[agents]` und `[shell]` werden geklemmt. Ein nicht
  vertrautes Projekt darf sie nur senken.
- Eine Ablehnung an einer Orchestrierungsgrenze beginnt mit
  „Orchestrierungsgrenze:“ und erreicht das Modell als Werkzeugfehler; der
  Turn läuft weiter.
- **Lease statt Zeitlimit:** Die Lease eines Kindes (15 min) ist ein
  Lebenszeichen. Solange das Kind läuft, verlängert ein Herzschlag sie und
  die Lease aller Vorfahren, auch während eines langen Builds. Ein
  festhängendes Kind endet trotzdem an seiner Wanduhrgrenze.

## 9. Praxis

- **Langer Auftrag:** Die UIA übergibt an den Orchestrator, arbeitet weiter
  und fragt bei Bedarf mit `agent.status` nach. Das Ergebnis kommt von
  selbst.
- **Kurskorrektur:** „Sag dem Orchestrator, er soll Modul B auslassen“ → die
  UIA sendet `agent.message`.
- **Rückfrage des Kindes:** erscheint bei der UIA; ihre Antwort geht per
  `agent.message` zurück.
- **Budget erschöpft:** Übergabe lesen, bei Bedarf mit `continue_from`
  fortsetzen.
- **Abgebrochen:** `[child_end …]` lesen, Journal mit `agent.result
  {part: "journal"}` holen, gezielt neu starten.
- **Sandbox blockiert:** `sandbox_denial` beachten und Host-Mode mit Grund
  anfragen, statt den Befehl umzubauen.

## Quellen

`harw-core/src/background_children.rs`, `child_comms.rs`,
`child_handoff.rs`, `child_lease_heartbeat.rs`, `turn_loop.rs`
(`handoff_tool_spec`); `harw-core-bridge/src/agent_status.rs`,
`agent_background.rs`, `agent_result.rs`, `agent_messaging.rs`,
`delegate_wave.rs`; `harw-tool-shell/src/exec/escalation.rs`;
`harw-config/src/agent_limits.rs`, `shell_limits.rs`;
`harw-tui/src/app/background_agents.rs`, `child_approvals.rs`,
`turn_safety.rs`.
