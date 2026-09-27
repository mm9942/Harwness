---
id: DEC-006
title: Modellagnostischer Work Driver
status: accepted
date: 2026-09-27
tags: [decision, work-driver, provider-abstraction]
related:
  - ../90-migration-ledger/MIGRATION_LEDGER.md
  - ../../harw-plan-bridge/src/work_driver.rs
  - ../../harw-provider-http/src/tool_names.rs
---

# DEC-006 — Modellagnostischer Work Driver

## Entscheidung
Der Work Driver bewertet Fortschritt und Zustand eines Workers ausschließlich
über generische, providerunabhängige Artefakte: Ergebnistext, geänderte
Dateien innerhalb der eigenen `owned_paths`, das Verifikationsergebnis und das
Judge-Urteil. Er urteilt niemals anhand von Tool-Namen, Anzahl der Aufrufe
oder providerspezifischer Trace-Struktur. Traces bleiben opak und werden
unverändert weitergegeben; die Übergabe bei einem Respawn erfolgt als reiner
Text-Handoff.

## Warum
- Provider unterscheiden sich in Tool-Aufruf-Semantik, Trace-Formaten und
  erlaubten Tool-Namen — Logik, die daran hängt, bricht bei jedem
  Providerwechsel oder -Update.
- Fortschritt lässt sich robust nur an dem messen, was tatsächlich zählt:
  Text-Ergebnis, tatsächlich geänderte Dateien im eigenen Scope, ob die
  zentrale Verifikation (DEC-004) läuft, und das Judge-Urteil (DEC-001).
- Tool-Namen müssen provider-spezifische Zeichenregeln erfüllen (z. B.
  striktere Wire-Formate als interne Namen); ein reversibler Codec erlaubt
  interne Namen frei zu halten (Punkte, längere Namen) und übersetzt sie nur
  für die Übertragung.
- Ein reiner Text-Handoff beim Respawn ist provider- und modellunabhängig
  lesbar und erfordert keine Kenntnis der internen Trace-Struktur des
  Vorgängers.

## Folgen
- Neue Provider lassen sich anschließen, ohne die Fortschrittslogik des
  Work Drivers anzufassen — nur der Tool-Name-Codec und die
  Provider-Anbindung selbst müssen die Zeichenregeln erfüllen.
- Trade-off: Der Driver kann keine feingranulare, providerspezifische
  Diagnose liefern (z. B. "Tool X wurde nicht aufgerufen"); er verlässt sich
  bewusst auf gröbere, generische Signale.
- Der Codec muss Kollisionen nach Sanitisierung/Kürzung auf 64 Zeichen
  eindeutig auflösen und in beide Richtungen (encode/decode) verlustfrei
  bleiben — das ist bei jeder Änderung an `tool_names.rs` erneut zu prüfen.
- Handoff-Texte sind Freitext und müssen alle für einen Respawn nötigen
  Informationen (offene Punkte, bisheriger Stand, erlaubte Pfade) enthalten,
  da der Nachfolger keinen strukturierten Zugriff auf den Trace des
  Vorgängers hat.

## Wo im Code
- `harw-plan-bridge/src/work_driver.rs` — `continue_or_respawn`, `handoff`
  (Zeilen ~850–910): Fortschritt/Respawn-Entscheidung anhand von
  `owned_paths`, Judge-Urteil (`JudgeVerdict.passed`) und Kontextgröße, nicht
  anhand von Tool-Aufrufen.
- `harw-plan-bridge/src/work_driver.rs` — `WorkDriveStep::Respawn { handoff, .. }`
  (Zeile ~378ff.): Handoff ist ein reiner `String`.
- `harw-provider-http/src/tool_names.rs` — `ToolNameCodec` (`register`,
  `encode`, `decode`, `sanitize`, `MAX_WIRE_NAME_LEN`): reversibler Codec auf
  die strengste gemeinsame Regel `^[A-Za-z_][A-Za-z0-9_-]{0,63}$`.

## Verwandt
- [DEC-001 Judge-Urteil passed:bool](DEC-001-passed-true.md)
- [DEC-004 Keine parallelen Builds](DEC-004-no-parallel-builds.md)
- [DEC-007 Worker-Rechte](DEC-007-worker-rights.md)
- [DEC-008 Kein TUI](DEC-008-no-tui.md)
