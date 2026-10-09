---
id: DEC-005
title: Kleine Scopes, viele Wellen
status: accepted
date: 2026-09-27
tags: [decision, work-driver, cost, scheduling]
related:
  - ../90-migration-ledger/MIGRATION_LEDGER.md
  - ../../harw-plan-bridge/src/work_driver.rs
  - ../../harw-provider-http/src/cache_strategy.rs
---

# DEC-005 — Kleine Scopes, viele Wellen

## Entscheidung
Jeder Worker bekommt einen möglichst kleinen, isolierten `WorkScope` — im
Regelfall eine Datei, höchstens ein eng begrenztes Verzeichnis — statt
großer Multi-Datei-Aufträge. Worker sind kurzlebig: Fortschritt entsteht
über viele Wellen (`drive_settled_wave`/`drive_running_wave`), nicht über
wenige lange Läufe. Vertragsdaten (Scope, Kriterien, Handoff-Text) leben in
den Feldern von `WorkScope`/`WorkerState`, nicht im Kontext des
Koordinators.

## Warum
- Kleine `owned_paths` minimieren Überlappungen zwischen Workern
  (`scopes_overlap`, `first_path_overlap` in `work_driver.rs`) und damit
  Merge-Konflikte und blockierte Wellen.
- Kurzlebige Worker mit knappem Scope halten den Kontext pro Lauf klein;
  `continue_or_respawn` respawnt erst, wenn `context_tokens_used` die
  `respawn_context_tokens`-Schwelle (Default 150 000) überschreitet — kleine
  Scopes schieben diese Schwelle seltener an.
- Denselben Worker über mehrere Runden fortzusetzen (statt jedes Mal neu zu
  spawnen) hält den Prompt-Präfix stabil und damit cache-fähig
  (`resolve_cache_strategy`, `CacheStrategy::ImplicitPrefix` /
  `ExplicitEphemeral` in `harw-provider-http/src/cache_strategy.rs`); ein
  respawnter Worker verliert diesen Cache und zahlt den Präfix neu.
- Viele kleine Wellen statt wenigen großen halten die zentrale Verifikation
  (DEC-004) günstig: jede Welle prüft nur wenige, klar abgegrenzte Änderungen,
  Fehlschläge sind leicht einem Scope zuzuordnen.
- Verträge in Dateien (Scope-Definition, Handoff-Text aus `handoff()`) statt
  im Koordinator-Kontext halten den Treiber selbst zustandsarm und machen
  Respawns verlustfrei: der Nachfolger bekommt exakt den gleichen
  Vertrag erneut.

## Folgen
- Der Scheduler muss bei jeder neuen Welle prüfen, ob offene Kriterien noch
  unabgedeckte, nicht überlappende Scopes zulassen; das ist mehr
  Buchhaltung als ein einzelner Großauftrag, zahlt sich aber in weniger
  Rework aus.
- Trade-off: sehr kleine Scopes erzeugen mehr Wellen und damit mehr
  Verifikationsläufe insgesamt — DEC-004 begrenzt das mit `verify.lock` auf
  einen Lauf gleichzeitig, nicht auf weniger Läufe insgesamt.
- Respawns wegen Kontextlimit oder hartem Fehler müssen den vollen Vertrag
  (Scope, offene Kriterien, bisheriger Stand) im `handoff`-Text mitgeben,
  da der neue Prozess sonst nichts vom Vorgänger weiß.

## Wo im Code
- `harw-plan-bridge/src/work_driver.rs` — `WorkScope` (`owned_paths`,
  `criteria`), `scopes_overlap`, `first_path_overlap`, `drive_settled_wave`,
  `drive_running_wave`, `continue_or_respawn`, `respawn_context_tokens`,
  `handoff()`.
- `harw-provider-http/src/cache_strategy.rs` — `resolve_cache_strategy`,
  `CacheStrategy::{ImplicitPrefix, ExplicitEphemeral}`,
  `apply_chat_cache_control`, `apply_messages_cache_control`.

## Verwandt
- [DEC-004 Keine parallelen Builds](DEC-004-no-parallel-builds.md)
- [DEC-007 Worker-Rechte](DEC-007-worker-rights.md)
- [DEC-006 Modell-agnostisch](DEC-006-model-agnostic.md)
