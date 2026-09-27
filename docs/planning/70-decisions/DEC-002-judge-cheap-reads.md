---
id: DEC-002
title: Richter — billige Lesevorgänge, teure Ausgabe gedeckelt
status: accepted
date: 2026-09-27
tags: [decision, work-driver, cost, judge]
related:
  - ../90-migration-ledger/MIGRATION_LEDGER.md
  - ../../guides/work-driver.md
---

# DEC-002 — Richter — billige Lesevorgänge, teure Ausgabe gedeckelt

## Entscheidung
Der Bewerter (Judge) im Work-Driver läuft als eigener interner Worker
(`InternalModelPoint::WorkDriverJudge`) mit eigener Provider-/Modell-Wahl,
ohne Werkzeuge. Er darf im Hintergrund viel lesen (Goal, Kriterien,
Nachweise), weil das über Runden hinweg größtenteils aus dem Provider-Cache
kommt; kostenrelevant ist nur die Ausgabe, und die ist auf minimale Antworten
(`{"passed": false}` im Zweifel) sowie eine feste Obergrenze von 256
Ausgabe-Token gedeckelt. Aktuell läuft der Richter einmal je abgeschlossener
Welle, nach der zentralen Verifikation (`decide` hat keine Eingabe für
Einzel-Worker-Verdikte innerhalb einer Welle); ein Aufruf nach jedem
Worker-Ergebnis ist durch die Kostenlogik gedeckt und günstig, aber erst
sinnvoll, sobald `decide` Verdikte pro Worker konsumiert — das ist möglich,
aber noch nicht aktiv.

## Warum
- Getrennte `InternalModelPoint`-Stelle statt Wiederverwendung des
  Hauptmodells: eigenes, günstigeres Modell/Provider für eine reine
  Klassifikationsaufgabe, unabhängig von der Wahl des Hauptworkers.
- Keine Werkzeuge: der Richter urteilt einmalig aus der vorliegenden Evidenz,
  keine Rückfragen, kein Tool-Overhead, kein zusätzlicher Runden-Ping-Pong.
- Stabiler Prompt-Präfix (`JUDGE_INSTRUCTION` + Kriterien) über eine
  fortgeführte Session statt Neuaufbau je Runde: der wiederholte Anteil trifft
  den Provider-Cache, nur der variable Teil (neue Evidenz) ist ein echter
  Treffer gegen den Cache-Rabatt — Lesen ist dadurch faktisch billig.
- Ausgabe ist der teure Teil bei den meisten Anbietern (Output-Token kosten
  mehr als Cache-Reads), deshalb die harte Grenze `JUDGE_MAX_OUTPUT_TOKENS =
  256` und eine minimale Antwortform: `{"passed": bool}` reicht, `comment`/
  `missing` sind optional und knapp.
- Fail-closed passt zur Kostenlogik: unklare/unlesbare Antwort → `passed:
  false` (siehe DEC-001) statt einer teuren Nachfrage oder einem zweiten
  Versuch.
- Ein Aufruf nach jedem Worker-Ergebnis statt nur je Welle wäre durch die
  Kostenlogik gedeckt (Lesen bleibt Cache-Read, nur die Ausgabe zählt), muss
  aber ebenso günstig bleiben — sonst skaliert die Richter-Kostenlast linear
  mit der Anzahl Worker-Runden statt mit der Anzahl Wellen.

## Folgen
- Der Richter braucht eine eigene Konfigurationsstelle
  (`work_driver_judge` in `harw-config`) mit eigenem Fallback (schnelles
  Modell des aktiven Providers, kein OpenRouter-Standard ohne explizite
  Wahl) — analog zu `AutoClassifier`.
- Trade-off: die 256-Token-Grenze zwingt zu knappen `comment`/`missing`-
  Feldern; ein Richter, der ausführlich begründen soll, passt nicht in dieses
  Budget und bräuchte eine andere Stelle.
- Trade-off: selbst im aktuellen Zuschnitt (einmal je Welle, nach der
  Verifikation) bedeuten Cache-Reads plus gedeckelte Ausgabe weiterhin
  nicht-null Kosten pro Welle; die Entscheidung verlagert die Kosten nur auf
  die günstigere Seite (Lesen statt Schreiben), sie eliminiert sie nicht.
- Ein Wechsel auf „nach jedem Worker-Ergebnis" setzt voraus, dass `decide`
  Verdikte pro Worker als Eingabe bekommt — das ist eine spätere Erweiterung,
  keine heutige.
- Zukünftige Änderungen an der Judge-Instruktion (`JUDGE_INSTRUCTION`)
  müssen die Cache-Präfix-Stabilität erhalten, sonst verfällt der
  Kostenvorteil aus dem wiederverwendeten Präfix.

## Wo im Code
- [harw-config/src/internal_models.rs](../../../harw-config/src/internal_models.rs) —
  `InternalModelPoint::WorkDriverJudge`, `key()` (`"work_driver_judge"`),
  `description()`, `uses_openrouter_default()`, `openrouter_default_model()`.
- [harw-cli/src/job_worker_work_driver.rs](../../../harw-cli/src/job_worker_work_driver.rs) —
  `JUDGE_MAX_OUTPUT_TOKENS: u32 = 256`, `load_run_config` (löst
  `InternalModelPoint::WorkDriverJudge` über `resolve_internal_model` auf,
  liefert `RunConfig.judge`).
- [harw-plan-bridge/src/work_driver.rs](../../../harw-plan-bridge/src/work_driver.rs) —
  `JudgeVerdict`, `JUDGE_INSTRUCTION` (stabiler Präfix, keine Tools, keine
  Rückfragen).

## Verwandt
- [DEC-001 Judge-Verdikt `passed: bool`](DEC-001-passed-true.md)
- [DEC-003 Provider-Limits](DEC-003-provider-limits.md)
- [DEC-005 Kleine Scopes, viele Wellen](DEC-005-small-scopes-waves.md)
