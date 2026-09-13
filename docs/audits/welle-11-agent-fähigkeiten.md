# Welle 11 — Agent-Fähigkeits-Audit

**Datum:** 2026-07-16
**Umfang:** 6 Prüfpunkte aus der Session-Doku ("obs halt funktioniert").
**Modus:** statisch (offline), da diese Umgebung keinen API-Key trägt. Live-Punkte sind markiert.

## Ergebnisübersicht

| # | Prüfpunkt | Status | Nachweis |
|---|-----------|--------|----------|
| 1 | Tools sichtbar bis in `ModelRequest.tools` | ✅ strukturell | `harw-core/src/model.rs:38` — `pub tools: Vec<ToolSpec>`; `turn_loop::collect_tools` (Z. 183) sammelt alle `ToolProvider::tools()` |
| 2 | MCP-Tools in Tool-Merge | ⚠ partiell | `mcp_runtime.rs` liefert nur Plan-/Spawn-Utilities (`plan_streamable_http_mcp`, `plan_stdio_mcp`, `spawn_stdio_mcp`). Kein direkter `ToolProvider`-Impl gefunden — Merge in `collect_tools` läuft nur über registrierte `ToolProvider`. Runtime-Klammer zwischen MCP-Spawn und ToolProvider-Registrierung ist offen. |
| 3 | Skills-Load-Pfad | ⛔ Platzhalter | `harw-ops/src/skills.rs:1`-Kommentar: „echte Skill-Liste, Aktivierungslogik in einer späteren Welle". Der `run()`-Body ist noch nicht real; die Sichtbarkeitsstruktur (`Surface::Command channel_parity`, ModelTool readonly) steht. |
| 4 | System-/Rollen-Prompts aus Config | ✅ strukturell | `turn_loop::load_instructions` (Z. 132) iteriert `session.registry().instructions_providers()`; `LoadedInstructions.system_prompt` wird in `ModelRequest.system_prompt` gemappt (Z. 79 `model.rs`). Voraussetzung: Rollen liefern `InstructionsProvider` an die Registry. |
| 5 | Agent-Rollen-Configs (`registry_factory`, `capability_snapshot`) | ✅ strukturell | `child_controller.rs:140` `capability_snapshot()` als Trait-Methode; `:172` `registry_factory: Arc<dyn ChildRegistryFactory>` in Definition; `:614` reale Nutzung in `admit()`. Aktive Rollen-Definitionen prüft der Integrationstest (siehe unten). |
| 6 | parent→child→grandchild-Routing | ✅ Depth-Test grün, ⚠ Live-Rundtrip offen | `max_depth`-Wächter in `child_controller.rs:parent_depth` verifiziert (bestehender Testfall). Ein voller 3-Ebenen-Rundtrip mit realem Provider steht auf Live-Test-Backlog (kein API-Key in dieser Env). |

## Detailbefunde

### (1) Tool-Sichtbarkeit → `ModelRequest.tools`

`collect_tools(session)` in `harw-core/src/turn_loop.rs:183` sammelt die Tool-Specs aller `ToolProvider` der Session-Registry und dedupliziert nach Funktionsname. Der Turn-Loop hängt das Ergebnis an `ModelRequest.tools` an (Konstruktoren in `model.rs:54`, `:74`). Wave-2-Provider-Bridge (Sitzungslog) leitet `tools` bereits als OpenAI-/Anthropic-Wire-Feld weiter. **Damit vollständig durchverdrahtet — jeder Tool-Provider, der einer Session-Registry beitritt, ist für das Modell sichtbar.**

### (2) MCP → ToolProvider

Gefunden: `mcp_runtime` (Plan, Spawn), `mcp_http` (HTTP-Klient). **Nicht gefunden**: ein `impl ToolProvider for McpBackend`-Adapter, der die per MCP entdeckten Tools in `collect_tools` einspeist. Der Merge existiert also auf der Session-/Registry-Seite (Punkt 1 grün), aber die Brücke MCP→Registry wird bislang extern vom Extension-Loader zusammengebaut. Für einen echten End-to-End-Nachweis fehlt hier ein Live-Test mit registriertem MCP-Server; die strukturelle Naht ist offen, nicht kaputt.

**Empfehlung:** eigenes Ticket „MCP-ToolProvider-Adapter" — nicht Teil dieser Welle, aber sichtbar zu machen.

### (3) Skills

Die Op-Fläche existiert (`/skills` channel_parity + readonly ModelTool). Der `run()`-Body ist heute eine Trockenimplementierung — der TODO-Kommentar deklariert das offen. Struktur (Sichtbarkeit, Sicherheitsgrenzen, Argument-Grammatik) ist da, Runtime nicht.

**Empfehlung:** Skill-Loader als eigene Welle behandeln; nicht mit Welle 11 vermengen.

### (4) System-/Rollen-Prompts

Die Pipeline `InstructionsProvider → LoadedInstructions.system_prompt → ModelRequest.system_prompt` steht kohärent. Erste ausgefüllte System-Instruction gewinnt (Kombinationsregel bei `turn_loop.rs:136` — bewusst first-nonempty statt Concat, um Duplikate zu vermeiden). Die Rolle bestimmt via Registry, was hier ankommt.

### (5) `registry_factory` + `capability_snapshot`

Beide Symbole sind live und werden in `admit()` (`child_controller.rs:614`) tatsächlich abgefragt. Der Test-Suite-Bestand (`harw-core/tests/child_controller.rs`) enthält Positiv- und Reject-Pfade.

### (6) Verschachteltes Routing

`parent_depth()`-Wächter schützt gegen Endlos-Rekursion. Alle bestehenden Depth-Tests grün. **Was hier fehlt**, ist ein Live-Test, der einen 3-Ebenen-Turn durchläuft (parent → child → grandchild → Result-Bubble-Up nach oben). Das benötigt einen echten Provider und ist offline nicht demonstrierbar; die Testinfrastruktur (`ManagedAgentSpawner`, `SessionManager`) ist aber vorhanden.

## Live-Nachweise (bei API-Key)

Ohne API-Key nicht abgeschlossen. Bei Verfügbarkeit sollten laufen:

1. `/status`, `/help`, `/memory list` — bereits offline verifiziert.
2. Ein Turn mit einem echten Tool-Call gegen OpenAI oder Anthropic (Round-Trip: Tool-Schema geschickt → Modell fordert Tool-Call → Result → zweiter Modell-Call → finale Antwort).
3. Ein Turn mit einem MCP-registrierten Tool.
4. `/agent spawn <role>` mit einer Rolle, die ihrerseits ein Sub-Agent-Tool aufruft (Ebenen: Turn → Child-Turn → Grandchild-Turn).
5. Reasoning-Effort-Trace: `--log debug` + `/memory record reflection …` und Beobachtung der `agent.turn`-Spans mit `reasoning_effort=medium|high` durch die drei Ebenen.

## Verbleibende, nicht in dieser Welle zu schließende Backlog-Posten

- **B11-1** MCP-ToolProvider-Adapter (Punkt 2 abschließen).
- **B11-2** Echter Skill-Loader (Punkt 3).
- **B11-3** Grandchild-Live-Test (Punkt 6).
- **B11-4** Auto-Erkennung von Correction-Signalen aus User-Turns in `harw-memory` (M3-Milestone).
