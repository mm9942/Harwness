# PL-98 — Root-Orchestrator: Stabilität und Verdrahtung

> **Status:** umgesetzt (Branch `ccr-859b52e5-uecpa8`), offene Entscheidungen unten
> **Anlass:** häufige „Continuation“-Fehler und viele Werkzeugfehler im
> Root-Orchestrator (Modelle: OpenAI, Anthropic, GLM‑5.3)
> **Quellen:** Code bei `78b22408`, Feldbericht
> [`85-gap-hunt/coordination/claude-field-report.md`](../85-gap-hunt/coordination/claude-field-report.md)

## 1. Befund

Es gab nicht *einen* Fehler, sondern vier Fehlerklassen, die sich beim
Root-Orchestrator (lange Turns, viele Kinder, häufige Kompaktierung)
gegenseitig verstärkten:

| # | Klasse | Symptom | Ursache |
|---|--------|---------|---------|
| 1 | Reasoning-Replay im Provider | `… reasoning continuity unavailable … start a fresh conversation` (harter Turn-Abbruch) | Drei Replay-Caches (`responses_reasoning`, `chat_reasoning`, `anthropic_replay`) mit **64 Runden je Provider-Instanz für den ganzen Agentenbaum**; jede nicht exakt wiederherstellbare Runde (Kompaktierung, Neustart, `MaxTokens`-Fortsetzung, Verdrängung) brach den Request ab. |
| 2 | Fortsetzungsketten `continue_from` | `continue_from: kein eigenes, beendetes Kind …`, `Kettengrenze erreicht` | Ledger global 64 Einträge; der Job-Submitter verdrängte die Übergabe durch den rohen Auftrag (Fortsetzung startete von vorn); fortgesetzte Orchestratoren verloren ihre Kinder. |
| 3 | `MaxTokens`-Fortsetzung im Turn-Loop | Turn endet als `Truncated`, Modell liest dauernd „setze exakt an der letzten Stelle fort“ | Zähler nie zurückgesetzt, Anweisung klebte an jeder Folgerunde. |
| 4 | Verdrahtung Root ↔ Kinder | „außerhalb der Rolle“, Netz verboten, Lesebudget erschöpft, Ergebnisse kommen nie an, Wellen abgelehnt | Feldbericht F6/F8/F10; Profile fielen für GLM‑5.3/GPT‑6/Claude‑5.x auf `max_child_fanout = 2`; nur 2 Sub-Orchestratoren; Hintergrund-Meldungen nur für die UIA. |

## 2. Umsetzung (Commits auf dem Branch)

1. **Replay degradiert statt abzubrechen** — `harw-provider-http/src/replay_bounds.rs`
   (Kontingent je Identität, Politik `degrade|strict`),
   `ModelError::ReplayUnavailable` (`harw-core/src/model.rs`),
   `[capabilities] reasoning_replay` (`harw-config/src/model_toml.rs`).
2. **`MaxTokens`-Fortsetzung** — nur aufeinanderfolgende Abschneidungen zählen;
   zu große Tool-Calls werden verworfen und kleiner angefordert; jede abgelehnte
   Delegation ist ein Werkzeugfehler (`harw-core/src/turn_loop.rs`).
3. **Fortsetzungsketten** — `has_pending_task` + Job-Submitter
   (`harw-runtime/src/agent_job_wiring.rs`), Ledger je Elternteil, `reparent`,
   Sandbox-Prüfung vor Admission (`harw-core/src/child_handoff.rs`,
   `child_controller.rs`).
4. **Limits und Netz** — Familien-Profile (`harw-model-catalog/src/runtime.rs`),
   `max_sub_orchestrators = 3`, F6 (`child_approval.rs`), F8 (`session.rs`).
5. **Lesebudget** — 12 Lesezugriffe, Orientierung frei (`guard.rs`), Rollenwissen.
6. **`delegate_wave` an einer Orchestrator-Wurzel** (`harw-runtime/src/assembly.rs`,
   `children.rs`).
7. **Hintergrund-Ergebnisse an Orchestrator-Kinder** — `AgentSpawner::take_background_notices`
   / `running_background_children`; der Turn-Loop eines Kindes injiziert Meldungen
   und wartet vor dem Abschluss auf laufende Kinder. Kind-Registries bekommen den
   Job-Submitter nur, wenn die Wurzel einen hat (Ablehnung *vor* der Admission).

## 3. Fehlercodes

| Code | Bedeutung | Wo |
|------|-----------|----|
| `HARW-REPLAY-001` | Kontinuität verloren, Modell auf `strict` → Request abgelehnt | `ModelError::ReplayUnavailable` |
| `HARW-REPLAY-002` | Kontinuität verloren, ohne natives Reasoning weitergesendet | Log `harw.replay` (warn) |
| `HARW-REPLAY-003` | Runde konnte nicht gemerkt werden; Antwort bleibt gültig | Log `harw.replay` (warn) |
| `HARW-REPLAY-004` | Anthropic: offene Tool-Runde ohne Thinking-Block → eine Anfrage ohne Thinking | Log `harw.replay` (warn) |
| `HARW-CONT-001` | `continue_from`: kein eigenes, beendetes Kind | Werkzeugfehler |
| `HARW-CONT-002` | `continue_from`: falsche Rolle | Werkzeugfehler |
| `HARW-CONT-003` | Kettengrenze (3 Fortsetzungen je Ursprung) | Werkzeugfehler |
| `HARW-CONT-004` | Sandbox des Vorgängers unbekannt | Werkzeugfehler |
| `HARW-CONT-010` | Hinweis: gescheitertes Hintergrund-Kind ist fortsetzbar | Meldung an den Elternteil |
| `HARW-ORCH-001` | sonstige abgelehnte Delegation (Turn läuft weiter) | Werkzeugfehler + Log |

Diagnose im Betrieb: `RUST_LOG=harw.replay=warn,harw_core::turn_loop=info` zeigt,
welcher Pfad greift.

## 4. Offene Entscheidungen (Owner nötig)

- **E1 — Exec-Rechte in `delegate_wave`.** Die Reducer in
  `harw-registry-defaults/src/authority.rs` sind bewusst nur lesend; `executor`
  bekommt über eine Welle deshalb kein `shell.exec`, und Root/Child-Orchestratoren
  haben selbst kein `ExecuteProcess` zum Durchreichen. Optionen: (a) eigener
  Reducer `ExecuteWorkspace` nur für `executor`, begrenzt durch die
  Eltern-Sandbox; (b) Exec-Arbeit ausschließlich über die UIA
  (`uia-shell-worker`) und `executor` aus den Wellen-Zielen der Orchestratoren
  entfernen. Sicherheitsrelevant — nicht ohne Entscheidung ändern.
- **E2 — 32k-Rückfallfenster** für Modelle ohne `context_window` (PL-97 §9.2, G1):
  ein Full-Root passt nicht in die erste Runde. Modelle im Katalog pflegen oder
  Rückfall anheben.
- **E3 — Hintergrund-Ergebnisse an der Wurzel ohne TUI** (OneShot/serve): dort
  holt niemand die Meldungen ab. Gleiches Muster wie für Kind-Sitzungen anwenden?

## 5. Nächste Schritte

1. **Persistenz des Fortsetzungsbuchs** im `StateStore` (Neustart verliert heute
   Ketten und Übergaben; Recovery setzt `pending_task`/`continuation` auf `None`).
2. **Replay-Persistenz** (verschlüsselte Reasoning-Blöcke im Verlauf ablegen),
   damit nach Neustart nicht degradiert werden muss.
3. **Thinking-Reserve** in `RoundBudget::of` (`turn_loop.rs`) zusammen mit der
   Auto-Compact-Policy berechnen (heute `thinking = false`, kein Katalog-Limit).
4. **`delegate_wave` nicht-blockierend** (PL-90-TODOs in `agent_tool.rs`,
   `delegate_wave.rs`) — heute blockiert die Welle den Orchestrator-Turn.
5. **PL-94**: Agent-Job-Rehydrator, Crash/Restart-Integrationstests.
6. Ende-zu-Ende-Test: Root-Orchestrator mit echtem Spawner, Welle, Budget-Ende
   eines Kindes, `continue_from` über den Job-Pfad.
