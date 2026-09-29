# PL-69 REVISION — Model-Turn-Chain

> **Status:** Revision des PL-69-Plans (durable-agent-chains) nach Design der Erfinderin (2026-09-29, Chat-Design-Session).
> **Bezug:** docs/planning/69-durable-agent-chains/ (README, W00–W02, PR #67). Diese Revision korrigiert die Grundstruktur, bevor W00–W02 implementiert werden.
> **Basis:** dev@153da055 (pinned in PL-69).

## Kernkorrektur: Zwei Profile → Eine Kette

W01 (reasoning) und W02 (generation) sind **keine zwei getrennten Profile**, sondern **Vorschausschnitte EINER Kette**: der **Model-Turn-Chain**. Jeder Modell-Turn ist ein Kettenglied und trägt als First-Class-Paar:

```
Turn := { model, effort }  — atomares Bündel, checkpointed im Chain-State
```

## Revision der fünf Prüfpunkte (Befund gegen PL-69-Draft)

### 1. Model+Effort-Bündel im checkpointed Chain-State
PL-69 hat `ReasoningStateV1.working_model` und „last successful model/provider route" — **der Effort-Anteil fehlt**. Revision: `ReasoningStateV1` um `working_effort: ReasoningEffort` erweitert; Checkpoint-Vertrag um `(model, effort)` als atomares Paar ergänzt. Persistenz-Schicht existiert bereits: `/models set <rolle> <modell> [effort]` (PR #70, `persist_role_reasoning_effort` → `reasoning.<feld>`).

### 2. Effort entkoppelt von Modellgröße
Effort ist ein First-Class-Config-Feld, **nicht** abgeleitet aus der Modellgröße. Konsequenz: **ein kleines Modell kann reasoning führen** — kleines Modell + hoher Effort in einer organisch geführten Kette ersetzt teilweise ein großes Modell (Kosten-/Latenzvorteil). Das ist ein Designziel, kein Zufallseffekt.

### 3. Die Kette ist aufschneidbar (cuttable)
PL-69 kennt nur global/provider-owned Pacing (W00 C8 — bleibt unverändert). Revision ergänzt: Die Kette lässt sich in **Segmente** teilen, die über **chain-interne Semaphoren** parallel *oder* sequenziell laufen. Jedes Segment trägt sein eigenes `(model, effort)`-Bündel und checkpointed typisiert. Chain-interne Semaphoren koordinieren nur die Segmente untereinander — Provider-Pacing/-Limits bleiben global und werden nie umgangen.

### 4. fan-out → fan-in Synthesis → Decision Point (Terminal)
Neue Kettensemantik: Ein Rundenmuster ist First-Class — parallel fan-out (n Segmente, gemischte Profile), konvergiert in **EINE Synthesis**, die **der letzte Schritt** der Runde ist, danach der **Decision Point**. Die Synthesis ist ein terminales FAN-IN („nach oben konsolidierend"), kein weiteres paralleles Segment und kein Stop-Condition-Fall. Der Decision Point ist das Terminal-Element der Chain-Semantik.

**Rekursivität:** Das Muster gilt **je Segment und je Kette** — ein Segment kann intern selbst fan-out/fan-in mit eigener kleiner Synthesis und Mini-Decision haben („innen drin"); die Kette selbst endet in einer Synthesis, die alles konsolidiert. W00 braucht dafür: Synthesis als Terminal-Typ mit Semaphore-Join-Semantik.

### 5. Wiederholungen + verschachtelte Ketten (ersetzt Non-Goal)
PL-69 schreibt als Non-Goal: „recursive self-spawning chain trees". **Revision hebt das auf** in abgegrenzter Form:
- **Wiederholungen:** dasselbe Segment iteriert mit gewanderten Fragen (Key-Drift-Iteration — bewährtes Muster aus der Analyse-Praxis: Fragen wandern mit den wandelnden Key Drivern).
- **Verschachtelung:** ein Segment kann selbst wieder eine Model-Turn-Chain sein (eigenes Bündel, eigene Semaphoren, eigener fan-out → Synthesis → Decision).
- **Hard Bound:** Rekursions-Tiefe als Chain-Level-Bound (analog `max_candidates` in W02), Wert festzulegen bei Implementierung.
- **Checkpoint-Merge:** Semantik, wie der Decision Point einer inneren Kette in den State der äußeren zurückfused — fehlt in W00/W01 vollständig, ist Pflichtelement dieser Revision.

## Interne Modellstellen als Chain-Segmente

Durch das Bündel-Design werden DREAM, COMPACTION, JOURNAL/DIARY-LEDGER und CONSOLIDATE selbst zu Kettengliedern: Als **Bundle an Bundle an Bundle kombiniert** innerhalb der organischen Kette — statt isolierter Nebenläufe. Die heutigen `/model internal`-Stellen (`dream_reflection`, `compaction_summary`, `memory_consolidation`) werden Chain-Segmente mit eigenem `(model, effort)`-Bündel. Compaction wird dadurch kein Kontext-Verlust-Ereignis, sondern ein Turn mit Bündel: kleines Modell + hoher Effort konsolidiert, checkpointed typisiert, next-turn-Config wandert mit.

## Konkretes Recipe-Beispiel: Pattern Analysis

```
Runde r (Scope wandert je Runde mit dem Erkenntnisstand):
  fan-out (parallel, semaphore-gated):
    ├─ Data Collection
    ├─ Link Analysis
    ├─ Pattern Analysis
    │    ├─ Trend Analysis    ┐ BUNDLE
    │    └─ Tendency Analysis ┘
    └─ Steps X, Y …
  fan-in: Aggregation — Collection(r-1) + Collection(r) konsolidiert,
          EMERGENZ: neue Infos entstehen, definieren Scope(r+1)
  → Synthesis (letzter Schritt, konsolidierend)
  → Decision Point → Runde r+1 (neuer Scope)
```

## Auswirkung auf Implementierungsreihenfolge (PL-69 §Scope)

Unverändert in der Reihenfolge, aber geändert im Inhalt:
1. Shared chain contracts/runtime — **jetzt inkl. Bündel-Typ, Segment- und Synthesis-/Decision-Terminal-Typen, Rekursions-Bound, Checkpoint-Merge**
2. Job adapter + checkpoint/lease fencing — unverändert
3. „Reasoning profile" → **Model-Turn-Chain-Preview (W01-Schnitte)**
4. „Generation profile" → **Model-Turn-Chain-Preview (W02-Schnitte)**
5. Model-aware continuation — jetzt direkt mit Effort-Entkopplung
6. Adaptive compositions — jetzt als Recipes (Pattern Analysis als erstes)

## Verwandte PRs

- **PR #70** (`feat/models-reasoning-effort`): Persistenz-Schicht für `(model, effort)` pro Rolle — Grundlage dieses Designs.
- **PL-71** (PR #68, semantic-activity-patterns): PatternInstances als strukturierte Beobachtungen für Chains — nachgelagert, nach dieser Revision gegenzulesen (Integrationsformulierung 06, diff 937–946).
