---
id: HP-CC
title: Rechercheblatt — Claude Code
status: research
date: 2026-09-27
tags: [harness-patterns, research, claude-code, mcp, hooks, workflows]
related:
  - README.md
  - ../85-gap-hunt/README.md
  - ../70-decisions/DEC-003-provider-limits.md
---

# Rechercheblatt: Claude Code

Das ist eine Recherche durch einen Sonnet-Agenten (nur lesend). Jede Aussage hat
eine Doku-URL; was nicht bestätigt werden konnte, ist markiert. Das Blatt liefert
Material für den übergreifenden Muster-Katalog ([README](README.md)).

## Befunde

### 1. Headless-Betrieb und JSON-Ausgabe
- **Aufruf und Flags:**
  - `claude -p` mit `--output-format text|json|stream-json`;
  - `--bare` überspringt Hooks, Skills, MCP, Plugins und die CLAUDE.md-Suche;
  - dazu `--allowedTools`, `--permission-mode`, `--permission-prompts none`, `--append-system-prompt[-file]` und `--continue`/`--resume`.
  - Quelle: https://code.claude.com/docs/en/headless.md
- **Strukturierte Ausgabe:** `--output-format json --json-schema '<schema>'` liefert das validierte Ergebnis als `structured_output` neben den Session-Metadaten. Ein ungültiges Schema bricht sofort ab.
- **Stream (`stream-json --verbose --include-partial-messages`):**
  - Das erste Event ist `system/init` mit Modell, Tools, `mcp_servers[]{name,status}`, Plugins, Fehlern und Fähigkeiten.
  - Subagent-Nachrichten tragen `parent_tool_use_id`.
  - Retries kommen als `system/api_retry` (`attempt`, `max_retries`, `retry_delay_ms`, `error_status`).
  - Den Abschluss bildet `result` mit `total_cost_usd`, `modelUsage` je Modell und `permission_denials`.
- *Nicht bestätigt:* die wörtliche Feldliste von `num_turns`, `duration_ms` und `is_error` sowie die Optionsformen von `query()` im Agent SDK.
- Strukturierte Ausgabe im SDK (JSON Schema/Zod/Pydantic, Retries bis `MAX_STRUCTURED_OUTPUT_RETRIES`): https://code.claude.com/docs/en/agent-sdk/structured-outputs.md

### 2. MCP
- **Scopes und Transporte:**
  - Scopes: lokal und User in `~/.claude.json`, Projekt in `.mcp.json`.
  - Transporte: stdio, streamable HTTP (empfohlen), SSE (veraltet), WebSocket.
- **Tool-Namen:** `mcp__<server>__<tool>`; für Plugins `mcp__plugin_<plugin>_<server>__<tool>`.
- **Sonstiges:**
  - Resources und Prompts mit `list_changed`, OAuth, `claude mcp login`.
  - Ausgabe-Limits über `MAX_MCP_OUTPUT_TOKENS` (Warnung ab 10k, Kappung bei 25k, Überlauf in eine Datei).
  - `claude mcp serve` betreibt Claude Code selbst als MCP-Server.
  - Quelle: https://code.claude.com/docs/en/mcp.md
- **Strukturierte Tool-Ausgabe (MCP `2025-06-18`):** `outputSchema` am Tool und `structuredContent` im Ergebnis. Der Server MUSS sich daran halten, der Client SOLLTE validieren. https://modelcontextprotocol.io/specification/2025-06-18/server/tools
- **Elicitation:** `elicitation/create` mit den Antworten `accept`, `decline` oder `cancel`, nur mit flachen Schemas. Claude Code hat dazu die Hooks `Elicitation`/`ElicitationResult`.

### 3. Subagents und SDK
- **Frontmatter in `.claude/agents/*.md`:**
  - `name`, `description`, `tools`, `disallowedTools`, `model`, `permissionMode`, `maxTurns`;
  - `skills`, `mcpServers`, `hooks`, `memory`, `background`, `isolation`, `effort` und weitere.
- **Rückgabe an den Parent:** Im Vordergrund blockiert der Aufruf und liefert das Ergebnis zurück. Im Hintergrund kommt eine Meldung bei Abschluss; mit `SendMessage` lässt sich der Agent fortsetzen.
- **Grenzen:** `CLAUDE_CODE_MAX_CONCURRENT_SUBAGENTS=20` und eine Spawn-Tiefe von 3.
- Quelle: https://code.claude.com/docs/en/sub-agents.md
- **Kosten:** `total_cost_usd`, `usage` je Schritt (dedupliziert nach Message-ID) und `modelUsage`. Die Summen stehen im Transkript und werden bei `resume` wiederhergestellt. https://code.claude.com/docs/en/agent-sdk/cost-tracking.md

### 4. Hooks
- **Events:**
  - Tools: `PreToolUse`, `PostToolUse`, `PostToolUseFailure`;
  - Session und Prompt: `UserPromptSubmit`, `SessionStart`/`End`;
  - Stoppen: `Stop`, `StopFailure`, `SubagentStart`/`Stop`;
  - Rechte: `PermissionRequest`/`Denied`;
  - Sonstiges: `Notification`, `Pre`/`PostCompact`, `Pre`/`PostModelSwitch`, `Elicitation*`, `Setup` und asynchrone Datei- und cwd-Events.
- **Vertrag:**
  - stdin ist JSON mit `session_id`, `transcript_path`, `cwd`, `permission_mode`, `hook_event_name` und `agent_id`.
  - stdout ist JSON mit `hookSpecificOutput.{permissionDecision, updatedInput, blockStopping, stopReason, additionalContext}`.
  - Exit-Codes: 0 bedeutet, das JSON wird gelesen; 2 blockiert, die Meldung kommt aus stderr; jeder andere Code blockiert nicht.
- **Handler-Typen:** `command`, `http`, `mcp_tool`, `prompt`, `agent`.
- **Stop-Hook mit Ziel:**
  - `blockStopping: true` zwingt die Session weiterzuarbeiten.
  - `/goal` ist ein **Stop-Hook mit Prompt pro Session**: Ein kleines Modell urteilt `met`, `not yet met` oder `impossible`.
  - Quellen: https://code.claude.com/docs/en/hooks.md und https://code.claude.com/docs/en/goal.md

### 5. Workflows, Loops, Skills
- **Dynamische Workflows:**
  - Ein JS-Skript mit `agent(prompt,{schema,label,phase,model,agentType})`, `parallel`, `pipeline`, `phase` und `log`.
  - Die Ausgabe ist per JSON-Schema mit Retries abgesichert.
  - Journal und Skript liegen unter `~/.claude/projects/…`.
  - Resume holt fertige Agenten aus dem Cache.
  - Grenzen: `CLAUDE_CODE_WORKFLOW_MAX_CONCURRENT_AGENTS` (Standard 16, CPU-abhängig) **pro Lauf**, 4096 Elemente pro Aufruf, 1000 Agenten pro Lauf.
  - Quelle: https://code.claude.com/docs/en/workflows.md
  - Genau darauf läuft unser Kit (`../85-gap-hunt/kit/`). In harw selbst gibt es das **noch nicht**.
- **`/loop` und Cron:**
  - `CronCreate`/`List`/`Delete` mit 5-Feld-Cron und einem Scheduler-Takt von 1 s.
  - Dazu Jitter, Ablauf nach 7 Tagen, gebunden an die Session, wiederhergestellt bei `--resume`.
  - Quelle: https://code.claude.com/docs/en/scheduled-tasks.md
- Skills, Plugins, Output-Styles und Statusline wurden identifiziert, aber nicht im Detail gelesen.

### 6. Kosten und Telemetrie
- **OTel-Metriken:** `claude_code.cost.usage`, `claude_code.token.usage`, `session.count`, `lines_of_code.count`, `active_time.total`.
- **OTel-Events:** `user_prompt`, `api_request`, `api_error`, `tool_decision`, `tool_result`.
- Quelle: https://code.claude.com/docs/en/monitoring-usage.md
- **`/usage`:** Gesamtkosten, API- und Wandzeit, Tokens und Kosten je Modell, Cache-Statistik, Zuordnung nach Skill, Subagent und MCP. https://code.claude.com/docs/en/costs.md

## Fähigkeiten-Matrix Claude Code → harw

| # | Claude Code | harw heute | Vorschlag |
|---|---|---|---|
| 1 | `-p --output-format stream-json` | **teilweise:** `harw-cli/src/output.rs` (Text/Json nur für Kommando-Ergebnisse); `chat.rs` ist interaktiv; `harw serve` ist eine Job-Runtime | `harw run -p … --output-format text\|json\|stream-json` mit harw-eigenem `HeadlessEvent` (NDJSON) und abschließendem `result` aus dem Kosten-Status (#5) |
| 2 | Workflows (agent/parallel/pipeline, Schema, Journal, Resume, Cap) | **fehlt als Feature.** Der nächste Verwandte ist der WorkDriver: `harw-plan-bridge/src/work_driver.rs` (Entscheidungskern), `verify_exec.rs` (zentrales Verify, DEC-004), `harw-ops/src/work_driver.rs`, `harw-cli/src/job_worker_work_driver.rs`. Das ist ein fester Algorithmus, kein Skript | neues `harw-workflow`: ein Skript mit `agent/parallel/pipeline/phase/log`, Schema-Validierung mit begrenzten Retries, ein `journal.jsonl` pro Lauf mit Resume aus dem Cache, **ein** gemeinsamer Deckel mit DEC-003. Agenten kommen über den Runner und die Admission, „baut nie“ wird damit kompiliert statt nur geprompted |
| 3 | Hooks (Pre/PostToolUse, Stop, `blockStopping`) | **fehlt generisch.** `/goal` (`harw-ops/src/goal.rs`, `harw-plan` Goal/Criterion) und die Judge-Schritte decken den Fall Stop/Ziel fest verdrahtet ab | `harw-hooks`: `HookEvent`, `trait Hook`, geschlossenes `HookDecision` (`Allow`, `Deny`, `BlockStopping`), `CommandHook` mit dem stdin-JSON- und Exit-Code-Vertrag, eingebunden in die Turn-Loop und die WorkDriver-Runde |
| 4 | `/loop` und Cron | **fehlt** (keine Scheduler-Funde) | `schedule`-Operation (Create/List/Delete) plus Scheduler, der Jobs über den Job-Koordinator startet |
| 5 | Kosten-Status (`total_cost_usd`, je Modell, API-Zeit) | **teilweise:** `harw-core/src/state_store.rs` (`total_usage`, nur Tokens), `harw-ops/src/usage.rs`, `harw-session-store/src/meta.rs` | `CostSummary {total_cost_usd, duration_api_ms, num_turns, per_model}` im Snapshot, Preistabelle mit Override, übersteht Resume |
| 6 | MCP `outputSchema`/`structuredContent`, Elicitation | **wahrscheinlich teilweise oder fehlend:** `harw-mcp-client` `McpTool` hat nur `input_schema`; `tool_bridge.rs` und `harw-mcp-server/src/session.rs` sind noch ungeprüft. Beide nutzen schon das Protokoll `2025-06-18` | `output_schema` und `structured_content` ergänzen, gegen das Schema validieren, Elicitation hinter einen Hook legen |

## Roadmap-Vorschlag dieses Blatts
1. Kosten- und Usage-Status
2. `harw-hooks`
3. MCP mit strukturierter Ausgabe und Elicitation
4. Headless `harw run -p`
5. Scheduler
6. `harw-workflow`
7. gemeinsamer Parallelitäts-Deckel mit DEC-003
8. Das Lücken-Kit auf `harw-workflow` portieren und gegen den Lauf mit Claude Code abgleichen

Die endgültige Reihenfolge legt der übergreifende Katalog fest.

## Offene Punkte
- Vor dem MCP-Schritt `harw-mcp-client/src/tool_bridge.rs` und `harw-mcp-server/src/session.rs` lesen.
- Die **Skript-Engine für `harw-workflow`** braucht eine eigene DEC:
  - Die Wahl steht zwischen einer eingebetteten Engine und einer Rust-Builder-API.
  - Eine eingebettete Engine darf nicht in öffentlichen Typen auftauchen.
- Einen doppelten Parallelitäts-Deckel vermeiden (Muster M2).
- Die Felder aus dem Agent SDK noch nachlesen.
