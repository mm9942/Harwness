---
id: GAP-HUNT
title: Lückenjagd — wiederverwendbares Kit
status: living
date: 2026-09-27
tags: [gap-hunt, workflow, reuse, patterns]
related:
  - patterns.md
  - R15-patterns.md
  - ../70-decisions/README.md
  - ../80-copilot-backlog/README.md
  - ../90-migration-ledger/MIGRATION_LEDGER.md
---

# Lückenjagd

Die Lückenjagd ist ein wiederverwendbares Verfahren, mit dem viele kurzlebige
Agenten Lücken im ganzen Workspace finden, prüfen und beheben, ohne dass ein
Agent baut. Sie ist aus der Runde R15 entstanden.

| Datei | Zweck |
|---|---|
| [patterns.md](patterns.md) | Muster-Katalog (M1–M10, P1–P18). Die Finder taggen Funde damit. |
| [R15-patterns.md](R15-patterns.md) | Lauf-Protokoll R15: Zahlen, Beobachtungen, Lehren |
| [kit/workflows/gap-hunt-area.js](kit/workflows/gap-hunt-area.js) | Suchen und Prüfen für einen Bereich, nur lesend |
| [kit/workflows/gap-fix.js](kit/workflows/gap-fix.js) | Fixen, Reviewen und Reparieren für eine disjunkte Dateimenge, dazu der Cross-File-Check |
| [kit/workflows/gap-verify.js](kit/workflows/gap-verify.js) | Nachprüfen vorhandener Funde (gestaffelt), optional mit Vollständigkeits-Kritiker; auch für Feldberichte |
| [kit/workflows/contract-wave.js](kit/workflows/contract-wave.js) | Mehrdatei-Funde je Cluster: Opus-Vertrag, ein Coder pro Datei, Opus-Cluster-Review, Reparatur |
| [kit/skills/gap-hunt/SKILL.md](kit/skills/gap-hunt/SKILL.md) | Ablauf für die orchestrierende Session |
| [kit/wave_manifest.py](kit/wave_manifest.py) | Schreibt das unveränderliche Manifest einer Welle nach [waves/](waves/) |
| [kit/tests/workflow-gates.test.js](kit/tests/workflow-gates.test.js) | Tests der Abschluss-Gates (`node …`, ohne Abhängigkeiten) |
| [kit/agents/](kit/agents/) | `focused-explorer` (nur lesen) und `focused-coder` (eine Datei, baut nie) |

## Warum dieses Verfahren (Entscheidungen)
- **Ein Agent pro Datei, niemand baut:**
  - Parallele Builds füllen die Platte und prüfen Zustände, die es nie gibt.
  - Siehe die Build-Regel in `CLAUDE.md`.
- **Nur lesendes Suchen getrennt vom Fixen:** Viele Such-Workflows können
  gleichzeitig laufen, ohne sich in die Quere zu kommen. Geschrieben wird erst
  auf disjunkten Dateimengen.
- **Gestaffelte Prüfung nach Muster und Schwere (P9/P10), Stichentscheid nur
  bei Uneinigkeit:**
  - M3 (Doku-Drift): nur intent, der per grep bestätigt.
  - critical/high: intent und scope; bei M1 zusätzlich „exploit“
    (Ausnutzbarkeit).
  - Alles andere: intent und reproduce, scope als Stichentscheid.
  - `args.verify = 'classic'` schaltet zurück auf reproduce und intent für
    jeden Fund.
  - Weil der Host die Parallelität begrenzt (P6), spart das Prüfungen genau
    dort, wo fast nie verworfen wird.
  - Jeder Fund trägt `votes` mit Blickwinkel und Urteil, damit P9 weiter
    gemessen werden kann.
- **Opus zum Suchen und für kritische Fixes, Sonnet zum Prüfen und für kleine
  Fixes:**
  - Die Suche braucht Tiefe.
  - Das gestaffelte Gegenprüfen fängt Fehlalarme günstig ab.
- **Parallelschnitt:**
  - Die Grenze gleichzeitiger Agenten gilt pro Workflow, bei CPUs − 2.
  - Mehrere Top-Level-Läufe auf getrennten Bereichen nutzen das Kontingent aus,
    ein einzelner großer Lauf nicht.
- **Ein Workflow, eine Branch (P11):** Jede Welle arbeitet in einem eigenen
  git-Worktree auf eigener Branch (`root`-Argument) und committet sofort nach
  ihrem Review. Eine Integrations-Branch sammelt die Wellen per Merge; nur dort
  läuft der zentrale Build. Der Haupt-Arbeitsbaum bleibt sauber.
- **Mehrdatei-Funde als Vertragswelle:** Einzel-Fixer erzeugen sonst
  Halb-Infrastruktur (P2).
- **Abschluss nur über harte Gates:** `complete` kommt aus festem Code, nicht
  aus einem Modellbericht. Ein Ripple-Befund blockiert die Welle, ein Vertrag
  muss genau die deklarierten Dateien abdecken, und jede gemergte Welle hat
  ein unveränderliches Manifest in [waves/](waves/).

## Installation (lokal, nicht versioniert)
`.claude/` ist per `.gitignore` bewusst nicht Teil des Projekts. Das Kit liegt
deshalb hier und wird lokal verlinkt:

```sh
mkdir -p .claude/workflows .claude/skills .claude/agents
ln -sf ../../docs/planning/85-gap-hunt/kit/workflows/gap-hunt-area.js .claude/workflows/
ln -sf ../../docs/planning/85-gap-hunt/kit/workflows/gap-fix.js       .claude/workflows/
ln -sfn ../../docs/planning/85-gap-hunt/kit/skills/gap-hunt           .claude/skills/gap-hunt
ln -sf ../../docs/planning/85-gap-hunt/kit/agents/focused-explorer.md .claude/agents/
ln -sf ../../docs/planning/85-gap-hunt/kit/agents/focused-coder.md    .claude/agents/
```

Danach lassen sich die Workflows per Name starten, zum Beispiel
`gap-hunt-area` mit `{key, area, crates, exclude}` oder `gap-fix` mit
`{key, findings}`.
