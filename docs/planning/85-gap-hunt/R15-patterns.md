---
id: GAP-R15
title: Lückenjagd R15 — Muster
status: living
date: 2026-09-27
tags: [gap-hunt, patterns, review, workflow]
related:
  - patterns.md
  - README.md
  - ../70-decisions/DEC-003-provider-limits.md
  - ../70-decisions/DEC-007-worker-rights.md
  - ../80-copilot-backlog/README.md
  - ../90-migration-ledger/MIGRATION_LEDGER.md
---

# Lückenjagd R15 — Muster

Laufende Notiz zur Lückenjagd nach R15. Das ist ein Workflow mit
- 8 Finder-Bereichen,
- 3 Prüf-Blickwinkeln (reproduce / intent / scope),
- Fixern, die je nur eine Datei bearbeiten, und Reviewern.

Hier stehen nicht die einzelnen Funde, sondern die **Muster** dahinter und was
wir daraus an Prozess und Code ändern. Die allgemein gültigen Muster sind in den
[Muster-Katalog](patterns.md) übernommen, das Verfahren liegt als
wiederverwendbares [Kit](README.md) vor.

## Muster im Code

### M1 — Der Erfolgspfad überspringt die Prüfung
Die Prüfung sitzt im Fehlerzweig oder im Fallback statt vor dem Erfolgspfad.
Belege aus Runde 1:
- **Koordinator:** `decide()` prüft die Sandbox-Anforderung nur bei Fehlschlag.
- **Judge-Fallback:** `parse_verdict` liest verneintes „passed“ als bestanden.
- **Belege:** Ein fehlgeschlagenes `attach_evidence` nach grüner Prüfung wird
  nur geloggt.
- **Secrets:** Der Store verlässt sich auf die umask statt auf feste Rechte.
- **Audit-Chain:** Erst prüfen, dann benutzen (TOCTOU auf dem Pfad).
- **Mandanten:** `work_driver.enqueue` scannt aktive Läufe über alle Mandanten.

**Regel:** Prüfungen stehen vor dem Erfolgspfad, und Fallbacks verweigern im
Zweifel.

### M2 — Die Limit-Logik ist über Schichten verteilt
Provider, Retry und Worker haben je eigene Deckel und eigene Zählung. Belege:
- Pacing wurde nach 300 s abgeschnitten.
- OpenAI hält den Parallelitäts-Slot während des Wartens.
- Ein Quota-429 bekam keinen Cooldown.
- „Ein 429 kostet keinen Versuch“ geht bei einem Neustart verloren.

Keine Testsuite prüft die Invarianten aus DEC-003 über alle Schichten hinweg.

### M3 — Doku driftet bei Quellenwechseln
Nach dem Wechsel von Git auf crates.io beschrieben drei Stellen weiter die
Git-Quelle:
- `Cargo.toml`
- `docs/architecture/dependency-review.md`
- `docs/architecture/crypto-drift-report.md`

Dazu kam ein voreiliges „LANDED“ im Ledger. Drei Finder meldeten das
unabhängig voneinander, jeweils unter einer anderen Kategorie.

### M4 — Hotspot-Datei
`harw-cli/src/job_worker_work_driver.rs` hat 7 der ersten 29 Funde. Sie bündelt
Verify, Pacing, Scope-Check und das Parsen des Judge-Urteils. Bei „ein Agent
pro Datei“ serialisiert die Dateigröße die Arbeit.

## Muster im Prozess

### P1 — Fixes erzeugen Folgefunde
Über die Hälfte der Funde aus Runde 2 hat Runde 1 selbst verursacht:
- Doku in derselben Datei nicht nachgezogen;
- `.expect()` in neuen Tests;
- `pub`-Helfer ohne Aufrufer;
- Performance-Regression (der ganze Workspace wird pro Block neu gelesen);
- halber Quota-Fix.

Fixer lösen den Fund, räumen aber den Rand nicht auf. Die Reviewer prüften nur
den Fund.

### P2 — Die Ein-Datei-Regel erzeugt Halb-Infrastruktur
Braucht der saubere Fix zwei Dateien, entsteht toter oder nicht verdrahteter
Code:
- ein Cgroup-Sweep, den nichts aufruft;
- `preview_wait_for` ohne Aufrufer.

Verlangt der Reviewer dann die Verdrahtung, weicht der Fixer auf einen
Umweg in der Datei aus (ein losgelöster `tokio::spawn`-Task im Executor).
Das bedeutet Scope-Creep: `linux.rs` +229 Zeilen Produktivcode für zwei Funde.

### P3 — Testanteil ist gesund, Produktivanteil schwankt
Bei Fixes sind 60–75 % der neuen Zeilen Tests. Das ist gut. Ausreißer beim
Produktivcode entstehen genau bei P2.

### P4 — Der Intent-Blickwinkel ist der wertvollste Neinsager
Fast alle abweichenden Stimmen kamen von „intent“:
- Koordinator,
- Router-Pacing,
- 300-s-Deckel,
- Telemetrie-Doctests.

Stimmt er dagegen, ist der Fund meist eine Absicherung in der Tiefe und kein
akuter Bug.

### P5 — Regeln ohne Werkzeug werden gebrochen
`expect`/`panic!` in Tests und Doctests tauchen in jeder Runde wieder auf.
Clippy prüft Doctests nicht.

### P6 — Durchsatz: Der Host begrenzt, nicht das Modell
Der Container hat 4 CPUs. Der Workflow lässt höchstens CPUs − 2 = **2 Agenten
gleichzeitig** laufen. Drei Prüfer pro Fund sind der größte Posten.

### P7 — Pfade müssen kanonisch sein
Finder meldeten Pfade gemischt, absolut und relativ. Dadurch entstanden zwei
Gruppen für dieselbe Datei, und zwei Fixer arbeiteten gleichzeitig an
`linux.rs`.

## Workspace-weite Jagd (8 Bereiche, Zwischenstand)

Die Tabelle zeigt ungeprüfte Rohfunde der Opus-Finder, 345 insgesamt, getaggt
nach Muster. Die Prüfung läuft noch.

| Muster | Rohfunde | Anteil |
|---|---:|---:|
| P5 Regelbruch in Tests/Doctests | 100 | 29 % |
| M1 Fail-open / Prüfung nach dem Erfolgspfad | 85 | 25 % |
| NEW (siehe unten) | 73 | 21 % |
| M3 Doku-Drift | 64 | 19 % |
| M2 Limit-/Zähl-Logik über Schichten | 23 | 7 % |

Nach Schwere: 1 kritisch, 28 hoch, 95 mittel, 221 niedrig. Der kritische Fund:
- `harw-config/src/merge.rs` — eine nicht vertrauenswürdige `.harw` im Repo
  kann Full Access einschalten und Guards abschalten. Das ist M1 auf der Ebene
  der Konfigurationsschichten.

### Neue Muster aus der Breite
- **M5 — Unbegrenzte Eingaben:** Lesen, Zeilen, Event-Puffer und `await`
  laufen ohne Obergrenze oder Timeout, oft an Vertrauensgrenzen (MCP, SSE,
  Kanäle).
- **M6 — Unstrukturierte Nebenläufigkeit:**
  - losgelöste `spawn`s pro Event oder SSE-Pumpe;
  - Abbruch wird nicht weitergereicht, Future-Zustand geht verloren;
  - ein Runner stirbt unbeaufsichtigt;
  - ein Accept-Fehler beendet den Listener.
- **M7 — Temp-Dateien:** Temp-Pfade für Ausführbares sind vorhersagbar oder
  unsicher.
- **M8 — Dateien mehrerer Prozesse:** Übernahme veralteter Locks, Schreiber
  ohne Lock.
- **P8 — Regeln ohne Werkzeug, Teil 2:**
  - öffentliche Fremdtypen (etwa 8 Funde);
  - let-chains trotz MSRV 1.85;
  - ungenutzte Abhängigkeiten und tote Feature-Flags;
  - ein Buch- bzw. Autorbezug.

  Wie bei P5 ist die Regel dokumentiert, wird aber nicht erzwungen.

### Was daraus folgt
- **Werkzeuge vor Menschen:** P5 und P8 machen zusammen über ein Drittel aller
  Funde aus. Ein `xtask`-Gate prüft:
  - `.unwrap(`/`.expect(`/`panic!(` in Tests und Doctests;
  - let-chains;
  - Fremdtypen in `pub`-Signaturen;
  - Buch- und Autorbezüge.

  Dazu kommen `cargo-machete`/`udeps` für ungenutzte Abhängigkeiten und ein
  MSRV-Check (`cargo +1.85 check` oder clippy `incompatible_msrv`). Damit
  entfällt eine ganze Fund-Klasse dauerhaft.
- **M1 ist Architektur, kein Einzelfehler:** Jeder Bereich hat solche Funde.
  Wir brauchen eine Regel und einen eigenen Review-Punkt: „Guard vor dem
  Erfolgspfad, Default verweigert, Vertrauensschicht explizit.“
- **M5 und M6 an Vertrauensgrenzen** gehören in eine gemeinsame Hilfsschicht:
  begrenztes Lesen, Timeouts, beaufsichtigte Tasks. Heute implementiert das
  jede Stelle selbst.

## Was im Workflow schon geändert ist (ab Runde 2)
- **Pfade:** normalisiert (P7). Die Deduplizierung ignoriert die Kategorie (M3).
- **Finder:** laufen auf Opus.
- **Fixer bei `critical`/`high`:** Opus.
- **Checkliste für Fixer und Reviewer** (P1, P2):
  - Doku in der Datei nachziehen;
  - kein `expect`/`panic!` in neuen Tests;
  - keine API ohne Aufrufer;
  - keine Performance-Regression;
  - höchstens etwa 150 Zeilen.
- **Prüfen:** Zuerst prüfen zwei Blickwinkel. Der dritte entscheidet nur bei
  Uneinigkeit (P6).
- **Abschluss:** Ein Opus-Agent sucht über den ganzen Diff nach Wirkungen über
  Dateigrenzen hinweg (P2, M3).

## Vorschläge für eine Folgerunde
1. **M1:** Regel in `.github/copilot-instructions.md` und in der Review-Liste,
   plus ein eigener Finder „Prüfung nach dem Erfolgspfad“.
2. **M2:** eine Invarianten-Testsuite für DEC-003. Beispiele:
   - kein Slot während einer Wartezeit;
   - ein 429 kostet nie einen Versuch, auch nicht nach einem Neustart;
   - Pacing wird nie gekürzt.
3. **M4:** `job_worker_work_driver.rs` in Module aufteilen:
   `verify`, `pacing`, `scope`, `verdict`.
4. **P5:** ein `xtask`-Gate für `.unwrap(`, `.expect(` und `panic!(` in Tests
   und Doc-Kommentaren.
5. **P2:** Mehrdatei-Funde automatisch in eine Vertrags-Welle überführen: ein
   Planer schreibt den Vertrag, Datei-Agenten arbeiten dagegen.
6. **P6:** Die Workflow-Parallelität hängt am Host, nicht am Kontingent. Das
   Abo trägt 15–20 gleichzeitige Agenten, der 4-CPU-Container nur 2 pro
   Workflow. Für große Jagden gibt es drei Wege:
   - **Parallelschnitt:** ein Workflow pro Suchbereich, jeder mit einer eigenen,
     disjunkten Dateimenge. Mehrdatei-Funde koordiniert die Hauptsession.
   - **Finder über das Agent-Werkzeug** (bis zu 20 gleichzeitig), danach Prüfen
     und Fixen im Workflow.
   - **Eine Cloud-Umgebung mit mehr CPUs** (siehe C-10 „Cloud Home“).
