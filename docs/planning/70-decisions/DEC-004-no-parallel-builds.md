---
id: DEC-004
title: Keine parallelen Builds — zentrale Verifikation
status: accepted
date: 2026-09-27
tags: [decision, build, verification, concurrency]
related:
  - ../90-migration-ledger/MIGRATION_LEDGER.md
  - ../../guides/work-driver.md
---

# DEC-004 — Keine parallelen Builds — zentrale Verifikation

## Entscheidung
Worker (Subagenten) rufen niemals `cargo`/`rustc` oder einen `make`-Zielpfad
auf, der sie aufruft — sie lesen und schreiben nur Code in ihrem Scope.
Verifikation (Build, Test, Lint) läuft ausschließlich zentral, einmal pro
Welle, über den vollständigen kombinierten Stand aller Worker. Pro
Workspace läuft höchstens eine Verifikation gleichzeitig, prozessübergreifend
erzwungen durch eine Dateisperre.

## Warum
- Mehrere gleichzeitige Compiler-Läufe füllen `target/` mit unterschiedlichen
  Build-Zuständen (Flags, Crate-Kombinationen) und haben den Plattenplatz
  bereits mehrfach erschöpft.
- Ein Worker kompiliert einen Zwischenzustand, der nie existiert: Worker A
  baut, während Worker B mitten in einer Edit an einem gemeinsamen Crate
  steckt — das erzeugt Fehler, die der fertige Zustand nicht hat. Der Worker
  jagt dann Phantomfehler oder „reparierten" fremden Code.
- Ein grüner Lauf eines einzelnen Workers verifiziert einen Zustand, der so
  nie ausgeliefert wird. Nur ein Build nach Abschluss aller Worker ist
  aussagekräftig.
- Konkurrierende Builds entwerten sich gegenseitig den Cache und warten auf
  die Cargo-Lock-Datei; ein zentraler Build, der normalerweise Minuten
  dauert, brauchte dadurch schon fast eine Stunde.
- Dies spiegelt die repo-weite Build-Regel aus `CLAUDE.md` und macht sie
  technisch erzwingbar statt nur eine Prompt-Konvention zu sein.

## Folgen
- Worker haben grundsätzlich keine Prozess-Werkzeuge (kein `cargo`, `rustc`,
  `make`); sie melden am Ende nur, welche Tests sie hinzugefügt haben und
  welche Kommandos der zentrale Build ausführen muss.
- Die einzige Verifikation läuft zentral, einmal je Welle, über den
  vollständigen kombinierten Stand — nicht pro Worker und nicht pro Datei.
- Eine Workspace-weite Sperrdatei (`<workspace_root>/.harw/verify.lock`)
  verhindert, dass zwei Prozesse gleichzeitig verifizieren; ein Aufrufer, der
  auf eine belegte Sperre trifft, bekommt `VerifyRunOutcome::Busy` — das ist
  kein Testfehlschlag und kein `VerifyOutcome::Failed`, sondern ein Signal
  für „später erneut versuchen".
- Trade-off: Feedback an einzelne Worker verzögert sich bis zur nächsten
  zentralen Verifikation; das wird bewusst in Kauf genommen, da paralleles
  Bauen unzuverlässige Ergebnisse und Ressourcenkonflikte erzeugt.
- Passt zusammen mit kleinen, isolierten Scopes (DEC-005): je kleiner die
  Scopes, desto seltener kollidieren Worker in gemeinsamen Dateien, bevor die
  zentrale Verifikation läuft.

## Wo im Code
- [harw-plan-bridge/src/verify_exec.rs](../../../harw-plan-bridge/src/verify_exec.rs) —
  `VerificationExecutor`, `VerifyRunOutcome::Busy`, `DEFAULT_LOCK_TIMEOUT`,
  `DEFAULT_LOCK_POLL_INTERVAL`, Sperrdatei `<workspace_root>/.harw/verify.lock`
  (`LOCK_DIR_NAME`, `LOCK_FILE_NAME`).
- [harw-plan-bridge/src/work_driver.rs](../../../harw-plan-bridge/src/work_driver.rs) —
  Work-Driver, der Worker ohne Prozess-Werkzeuge einsetzt und die zentrale
  Verifikation nach jeder Welle anstößt.

## Verwandt
- [DEC-003 Provider-Limits](DEC-003-provider-limits.md)
- [DEC-005 Kleine Scopes, viele Wellen](DEC-005-small-scopes-waves.md)
- [DEC-007 Worker-Rechte](DEC-007-worker-rights.md)
