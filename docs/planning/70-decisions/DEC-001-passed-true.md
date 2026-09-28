---
id: DEC-001
title: Bewerter-Urteil `{"passed": bool}` — Testkonvention, fail closed
status: accepted
date: 2026-09-27
tags: [decision, judge, work-driver]
related:
  - ../90-migration-ledger/MIGRATION_LEDGER.md
  - ../../harw-plan-bridge/src/work_driver.rs
  - ../../harw-cli/src/job_worker_work_driver.rs
---

# DEC-001 — Bewerter-Urteil `{"passed": bool}` — Testkonvention, fail closed

## Entscheidung
Das Urteil des Bewerters ist `{"passed": bool, "comment": string, "missing": [string]}`.
`passed` folgt ausschließlich der etablierten Testkonvention (`true` = bestanden) und
trägt keine eigene, abweichende Semantik. Jede Antwort, die sich nicht eindeutig als
Urteil lesen lässt, gilt als `passed: false` (fail closed) — nie als `true`.

## Warum
- Testframeworks, CI-Gates und Menschen lesen `passed: true` einheitlich als
  "Kriterium erfüllt"; eine eigene Bedeutung (z. B. invertiert oder mehrwertig)
  würde jede Integration zu einer Fehlerquelle machen.
- Der Bewerter ist ein kleiner interner Worker ohne Tools und ohne Rückfragen
  (siehe DEC-002); er muss knapp und eindeutig antworten können, ohne dass die
  Aufrufer zusätzliche Fallunterscheidungen brauchen.
- Modelle liefern nicht immer valides JSON oder das erwartete Feld. Ohne eine
  klare Fail-Closed-Regel würde ein kaputtes oder unlesbares Urteil optimistisch
  als "bestanden" durchgehen und Fehler stillschweigend verdecken.
- Ältere Feldnamen (`met`, `verified`, `rationale`) müssen weiter lesbar sein,
  ohne die Kernsemantik von `passed` zu verändern.

## Folgen
- `JudgeVerdict::passed` ist der einzige Wahrheitswert, den der Work Driver für
  das Bewerter-Kriterium auswertet; `comment`/`missing` sind nur erklärender
  Zusatz und fließen als Feedback in die nächste Runde, nicht in die Entscheidung.
- Das tolerante Parsen (`parse_verdict`) probiert zuerst ein JSON-Objekt mit
  `passed`/`met`/`verified`, und fällt sonst auf `passed: false` mit dem
  Rohtext als Kommentar zurück — es gibt keinen Klartext-Marker-Scan
  (`PASSED`/`FAILED`) und keinen Pfad, auf dem unlesbarer Text zu `true` wird.
- Trade-off: ein Bewerter, der z. B. nur "sieht gut aus" ohne JSON schreibt,
  wird als nicht bestanden gewertet, auch wenn er inhaltlich zustimmen wollte.
  Das ist bewusst so gewählt, damit im Zweifel nie falsch positiv gewertet wird.
- Neue Aufrufer dürfen `passed` nicht selbst neu interpretieren oder invertieren;
  jede Erweiterung des Urteils (weitere Felder) muss dieser Konvention folgen.

## Wo im Code
- `harw-plan-bridge/src/work_driver.rs` — `struct JudgeVerdict` (Feld `passed`,
  Alias `met`), `const JUDGE_INSTRUCTION` (verlangt exakt `{"passed": bool, ...}`).
- `harw-cli/src/job_worker_work_driver.rs` — `fn verdict_from_object` (liest
  `passed`/`met`/`verified`), `fn parse_verdict` (tolerant, fail closed: kein
  erkennbares Urteil → `passed: false` mit Rohtext als Kommentar).
- `harw-ops/src/work_driver.rs` — `last_judge: Option<JudgeVerdict>` als
  gespeicherter Zustand der letzten Bewertung.

## Verwandt
- [DEC-002 Judge billige Reads](DEC-002-judge-cheap-reads.md)
- [DEC-006 Modellagnostisch](DEC-006-model-agnostic.md)
