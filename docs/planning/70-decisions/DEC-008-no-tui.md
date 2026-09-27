---
id: DEC-008
title: WorkDriver ohne TUI-Fläche
status: accepted
date: 2026-09-27
tags: [decision, work-driver, tool-surface]
related:
  - ../90-migration-ledger/MIGRATION_LEDGER.md
  - ../../docs/guides/work-driver.md
  - ../../docs/design/agent-definition-dsl.md
---

# DEC-008 — WorkDriver ohne TUI-Fläche

## Entscheidung
Der WorkDriver ist ausschließlich eine Job-Art (`work_driver`) plus drei
Modell-Werkzeuge und die entsprechenden Web-Routen. Er bekommt keine
TUI-Slash-Command- und keine eigene `harw`-Subcommand-Fläche. Gestartet,
beobachtet und gestoppt wird er nur vom Orchestrator selbst über
`work_driver.enqueue`/`.status`/`.stop`, oder stellvertretend über die
Web-API. Beide Wege verlangen immer eine Freigabe für `enqueue`/`stop`.

## Warum
- Eine TUI-Fläche würde die Werkzeug-Semantik verschieben: ein Mensch am
  Terminal hätte einen anderen Freigabe- und Rechte-Pfad als der
  Orchestrator, der über `work_driver.enqueue` denselben Job anstößt —
  zwei Wege für dieselbe Wirkung sind unnötig und ein zweites Ratenlimit-
  und Autorisierungs-Feld zum Pflegen.
- Der WorkDriver ist als Delegationsmechanismus des Orchestrators gedacht
  (DEC-007), nicht als interaktives Bedienfeld für Menschen; ein Mensch
  bleibt über Freigabe-Gates und `work_driver.status` (lesend, ohne
  Freigabe) im Bild.
- Die Werkzeuge sind nur erreichbar, wenn die Agent-IR ein `[work_driver]`
  hat: Lowering setzt sie, der Roster-Clamp hält genau diese drei Werkzeuge
  fest, alles andere fällt weiter durch den Clamp der Basis-Rolle.
- Eine schlanke Fläche (Job-Kind + drei Werkzeuge + Web-Route) lässt sich
  vollständig über die bestehenden Freigabe- und Rechte-Pfade (Kapitel R14)
  prüfen, ohne einen weiteren Oberflächentyp in Roster, Capability-Katalog
  und Autorisierung nachzuziehen.

## Folgen
- Wer den WorkDriver nutzen will, braucht eine Orchestrator-Definition mit
  `[work_driver]`-Tabelle; es gibt keinen Terminal-Weg, der das umgeht.
- Web-Fläche und Modell-Werkzeug-Fläche teilen sich denselben
  `WorkDriverCaller`; ein künftiger dritter Einstiegspunkt (etwa eine TUI)
  müsste denselben Aufrufer und dieselben Freigabe-Regeln verwenden, nicht
  einen eigenen Pfad öffnen.
- Trade-off: Beobachtung ist nur über `work_driver.status` bzw. die
  Web-API möglich, nicht interaktiv am Terminal; das ist bewusst in Kauf
  genommen, solange kein Bedarf für ein Live-Dashboard belegt ist.
- Der Job-Worker, der einen `work_driver`-Job tatsächlich abarbeitet, ist
  laut Leitfaden nur teilweise im Baum; die Entscheidung gilt unabhängig
  vom Implementierungsstand für die Form der Fläche.

## Wo im Code
- `harw-runtime/src/services.rs` — `WORK_DRIVER_JOB_KIND = "work_driver"`; `RuntimeServices::with_work_driver_caller` bindet `Arc<WorkDriverCaller>` nur auf die Modell-Werkzeug- und Web-Fläche.
- `harw-agent-dsl/src/lower_v2.rs` — `WORK_DRIVER_ENQUEUE_TOOL`/`WORK_DRIVER_STATUS_TOOL`/`WORK_DRIVER_STOP_TOOL`, `lower_work_driver`, `grant_work_driver_tools`: die drei Werkzeuge entstehen nur, wenn `[work_driver]` erfolgreich gelowert wurde.
- `harw-registry-defaults/src/roster.rs` — der Clamp behält `work_driver.enqueue/status/stop` für einen Orchestrator mit `[work_driver]`-Sektion, entfernt sie sonst (`custom_orchestrator_with_work_driver_keeps_its_three_tools_through_the_clamp`, `custom_orchestrator_without_work_driver_loses_the_tool_in_the_clamp`).
- `harw-registry-defaults/src/capability_catalog.rs` — `WORK_DRIVER_TOOLS`, Klassifizierung `work_driver.enqueue` als `Shell`, `.status`/`.stop` als `Meta`.
- `harw-ops/src/work_driver.rs` — `WORK_DRIVER_JOB_KIND`, `WorkDriverCaller`, die eigentlichen `enqueue`/`status`/`stop`-Operationen hinter den Werkzeugen.
- `docs/guides/work-driver.md:65` — "There is no TUI slash command and no `harw` subcommand for the WorkDriver."; Abschnitt 2 beschreibt Modell-Werkzeug und `POST /api/work-driver/enqueue` als die beiden einzigen Startwege.

## Verwandt
- [DEC-006 Modellagnostisch](DEC-006-model-agnostic.md)
- [DEC-007 Worker-Rechte](DEC-007-worker-rights.md)
