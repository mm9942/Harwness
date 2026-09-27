---
id: GAP-PATTERNS
title: Muster-Katalog für Lückenjagden
status: living
date: 2026-09-27
tags: [gap-hunt, patterns, review, catalog]
related:
  - README.md
  - R15-patterns.md
  - ../70-decisions/DEC-003-provider-limits.md
  - ../70-decisions/DEC-007-worker-rights.md
---

# Muster-Katalog

Wiederverwendbarer Katalog für jede Lückenjagd:
- Die Finder taggen jeden Fund mit einem Muster-Kürzel.
- Neue Muster kommen erst als `NEW:<name>` herein und werden hier
  aufgenommen, sobald sie in mehr als einem Bereich auftauchen.

Jeder Eintrag hat die gleichen Teile:
- **Erkennen:** woran ein Finder es sieht.
- **Regel:** wie der Code es richtig macht.
- **Werkzeug:** ob ein Gate es dauerhaft abfangen kann.

## Muster im Code

### M1 — Prüfung nach dem Erfolgspfad (fail-open)
- **Erkennen:**
  - Die Prüfung steht im `else`/Fehlerzweig oder im Fallback. Beispiel:
    `if ok { return Succeed }` steht vor dem Policy-Check.
  - Lockere Fallbacks: ein Freitext-Parser liest „passed“ auch in
    verneinter Form.
  - Die Default-Erlaubnis gewinnt.
  - Eine nicht vertrauenswürdige Konfigurationsschicht kann Guards
    abschalten.
  - Dateirechte werden der umask überlassen.
  - Erst prüfen, dann benutzen (TOCTOU).
  - Fehler werden nur geloggt, wo sie den Ablauf stoppen müssten.
- **Regel:**
  - Der Guard steht vor dem Erfolgspfad.
  - Default ist Verweigern.
  - Die Vertrauensschicht ist explizit.
  - Rechte werden explizit gesetzt.
  - Check-then-use wird zu Open-then-check.
- **Werkzeug:** Das ist ein Review-Punkt. Ein eigener Finder mit dieser Brille
  lohnt sich in jedem Bereich.

### M2 — Limit- und Zähl-Logik über Schichten verteilt
- **Erkennen:** Provider, Retry und Worker haben je eigene Deckel und Zähler.
  - Eine Wartezeit wird gekürzt.
  - Ein Slot bleibt während einer Wartezeit belegt.
  - Die Zählung geht bei einem Neustart verloren.
- **Regel:**
  - Invarianten stehen zentral, siehe DEC-003.
  - Jede Schicht leitet die Werte nur weiter.
  - Zustände, die einen Neustart überleben müssen, werden persistiert.
- **Werkzeug:** eine Invarianten-Testsuite über alle Schichten.

### M3 — Doku driftet vom Code
- **Erkennen:**
  - Kommentare, Guides, das Ledger oder Architektur-Dokumente beschreiben
    den alten Zustand.
  - Besonders nach dem Wechsel von Quellen oder Abhängigkeiten.
  - Status-Behauptungen kommen vor dem Merge („LANDED“).
- **Regel:**
  - Wer Verhalten ändert, grept nach dem alten Begriff im ganzen Repo.
  - Ein Status steht erst nach dem Merge im Ledger.
- **Werkzeug:** ein abschließender Cross-File-Check (Opus) über den ganzen
  Diff.

### M4 — Hotspot-Datei
- **Erkennen:** Eine Datei vereint mehrere Zuständigkeiten und sammelt
  überproportional viele Funde.
- **Regel:** In Module teilen. Bei „ein Agent pro Datei“ entscheidet die
  Dateigröße über die Parallelität.
- **Werkzeug:** Funde pro Datei zählen, die fünf größten zuerst teilen.

### M5 — Unbegrenzte Eingaben an Vertrauensgrenzen
- **Erkennen:** `read_to_end`, Zeilen lesen, Event-Puffer oder `await` ohne
  Obergrenze bzw. Timeout, besonders bei MCP, SSE, Kanälen und Netz.
- **Regel:** Gemeinsame Helfer für begrenztes Lesen und Timeouts statt Lösungen
  pro Stelle.

### M6 — Unstrukturierte Nebenläufigkeit
- **Erkennen:**
  - losgelöste `spawn`s pro Event oder Pumpe;
  - Abbruch wird nicht weitergereicht, Future-Zustand geht verloren;
  - Tasks sterben unbeaufsichtigt;
  - ein Accept-Fehler beendet den Listener;
  - ein Lock wird über `sleep`/`await` gehalten.
- **Regel:** Beaufsichtigte Tasks (JoinSet/Handle mit Owner), Abbruch fließt
  durch, Listener überleben Fehler einzelner Verbindungen.

### M7 — Temp-Dateien
- **Erkennen:** Vorhersagbare oder unsichere Temp-Pfade, besonders für
  Ausführbares.
- **Regel:** `tempfile` mit privaten Rechten, nichts Ausführbares in
  gemeinsamen Temp-Verzeichnissen.

### M8 — Dateien mehrerer Prozesse
- **Erkennen:** Veraltete Locks werden übernommen, Schreiber arbeiten ohne
  Lock, Verlauf wird nicht persistiert.
- **Regel:** fs4-Locks mit klarer Übernahme-Regel, atomares Schreiben
  (temp + rename).

### M9 — Zustand ohne Endübergang
- **Erkennen:** Ein Zustand hat einen Eingang, aber keinen sicheren Ausgang.
  Ein Job bleibt für immer `Running`, ein fertiger Worker gibt seinen Slot nie
  frei, eine Zielschleife dreht ohne Abbruch, ein abgelaufener Lease wird nie
  bereinigt.
- **Regel:** Jede Zustandsmaschine nennt ihre Endzustände. Jeder Pfad, auch
  Fehler, Abbruch und Neustart, führt in einen davon. Beim Start werden
  verwaiste Zustände abgeglichen (reconcile).
- **Werkzeug:** Tests pro Endübergang, dazu ein Neustart-Test mit einem
  Zustand, der mitten im Lauf liegen geblieben ist.

### M10 — Erst sichtbar, dann persistiert
- **Erkennen:** Ein Prozess macht einen Zustand sichtbar (Speicher, Event,
  Antwort), bevor er dauerhaft geschrieben ist. Nach Absturz oder Neustart
  fehlt, was Clients schon gesehen haben. Beispiele: Speicher vor dem fsync
  des Verzeichnisses, eine Session-ID nur im Speicher, ein Event-Bus ohne
  dauerhaften Replay.
- **Abgrenzung:** M8 betrifft mehrere Prozesse an einer Datei, M10 die
  Reihenfolge in einem Prozess.
- **Regel:** Erst schreiben, dann sichtbar machen. Wo das nicht geht, ist
  „bestätigt, aber nicht dauerhaft“ ein eigener Fehlerfall, den Aufrufer
  unterscheiden können.

### Zuordnen statt NEW
Finder taggen viele Funde als `NEW:<name>`, die schon ein Muster haben:
- `unbounded-read`, `unbounded-line-read`, `unbounded-connections`,
  `unbounded-event-buffer`, `unbounded-cache-growth`, `unbounded-await`,
  `no-timeout`: **M5**.
- `detached-sse-pump`, `detached-per-event-spawn`, `cancel-not-propagated`,
  `dropped-future-state`, `lock-across-sleep`, `unsupervised-…-death`:
  **M6**.
- `insecure-temp-exec`, `predictable-temp-exec`: **M7**.
- `stale-lock-takeover`, `unlocked multi-process writer`: **M8**.
- `expired-lease-never-reconciled`, `non-idempotent-recovery`: **M9**.
- `unpersisted-history-mutation`: **M10**.
- `let-chain`, `third-party-type-in-public-api`, `unused-dependency`,
  `dead-feature-flag`: **P8**. `pub-without-caller`: **P1**.

Der Finder-Prompt nennt deshalb alle Kürzel mit je einem Erkennungssatz.

## Muster im Prozess

### P1 — Fixes erzeugen Folgefunde
Fixer lösen den Fund, räumen aber den Rand nicht auf:
- Doku in derselben Datei,
- `expect` in neuen Tests,
- API ohne Aufrufer,
- Performance-Regressionen.

**Gegenmittel:** eine Checkliste für Fixer und Reviewer (siehe Kit).

### P2 — Die Ein-Datei-Regel erzeugt Halb-Infrastruktur
Braucht ein Fix zwei Dateien, entsteht toter oder unverdrahteter Code oder ein
Umweg in der Datei.

**Gegenmittel:** Mehrdatei-Funde gehen in eine Vertragswelle (ein Vertrag,
dann ein Agent pro Datei), nicht in Einzel-Fixer. Diff-Grenze etwa 150 Zeilen.

### P3 — Testanteil
Gesunde Fixes bestehen zu 60–75 % aus Tests. Ausreißer beim Produktivcode
deuten auf P2.

### P4 — Der Intent-Blickwinkel
Stimmt „intent“ dagegen, ist der Fund meist eine Absicherung in der Tiefe und
kein akuter Bug. Das hilft beim Priorisieren.

### P5 — Regelbruch in Tests und Doctests
`.unwrap(`, `.expect(` und `panic!(` in Tests und Doctests. Clippy prüft
Doctests nicht.

**Werkzeug:** ein `xtask`-Gate.

### P6 — Durchsatz hängt am Host
Der Workflow lässt höchstens CPUs − 2 Agenten gleichzeitig laufen, **pro
Workflow**.

**Gegenmittel:** Parallelschnitt, also ein Workflow pro Bereich auf disjunkten
Dateien.

### P7 — Pfade kanonisch halten
Absolute und relative Pfade gemischt ergeben doppelte Gruppen und zwei Fixer
an einer Datei.

**Gegenmittel:** Pfade repo-relativ normalisieren, bevor gruppiert wird.

### P8 — Regeln ohne Werkzeug, Teil 2
- öffentliche Fremdtypen;
- let-chains trotz MSRV;
- ungenutzte Abhängigkeiten, tote Feature-Flags;
- Buch- oder Autorbezüge.

**Werkzeug:** ein `xtask`-Gate, `cargo-machete`/`udeps` und ein MSRV-Check.

### P9 — Trennschärfe der Prüf-Blickwinkel messen
Stand der Workspace-Jagd mit 8 Bereichen, rund 260 geprüften Funden von
Opus-Findern:
- **reproduce** sagt in 98,5 % der Fälle „echt“ (257 zu 4). Bei präzisen
  Findern trennt dieser Blickwinkel kaum.
- **intent** verwirft 10 % (230 zu 25). Er ist praktisch der einzige
  Blickwinkel, der trennt (siehe P4).
- **scope** wird als Stichentscheid selten gebraucht (13 Mal) und stimmt immer
  mit „echt“.

**Gegenmittel:** Die Ausbeute pro Blickwinkel laufend messen. Einen
Blickwinkel, der nie widerspricht, ersetzen oder nur bei hoher Schwere
einsetzen. Beispiel: intent zuerst, reproduce nur bei `critical`/`high`.
Zusätzlich einen gegnerischen Blickwinkel „Ausnutzbarkeit“ für M1-Funde.

### P10 — Vertrauen je Kategorie
Anteil der Funde, die die Prüfung überstehen:

| Kategorie | übersteht |
|---|---:|
| M3 Doku-Drift | 98 % |
| M1 | 91 % |
| M2 | 91 % |
| P5 | 90 % |
| NEW | 91 % |
| hohe und kritische Funde | 100 % |
| niedrige Funde | 89 % |

**Gegenmittel:**
- Prüfaufwand dorthin lenken, wo verworfen wird, also auf niedrige Funde und
  auf NEW.
- M3 lässt sich günstig per grep bestätigen statt mit drei Modell-Prüfern.
- `critical`/`high` von Opus-Findern brauchen einen Prüfer für Umfang und
  Risiko (scope), aber keinen Existenzbeweis.

### P11 — Ein Workflow, eine Branch
Alle Fixer im selben Arbeitsbaum heißt: Niemand darf committen, solange
irgendein Fixer schreibt, der Baum ist stundenlang schmutzig, und ein Abbruch
am Session-Limit hinterlässt halbe Edits mitten zwischen fertigen.

**Gegenmittel:**
- Jede schreibende Welle bekommt einen eigenen git-Worktree auf eigener Branch
  und committet sofort nach ihrem Review.
- Eine Integrations-Branch sammelt die Wellen per Merge; nur dort läuft der
  zentrale Build.
- Wellen bleiben dateidisjunkt. Wer Dateien einer früheren Welle berührt,
  startet erst nach deren Merge.
- Nach einem Abbruch die Welle mit **unverändertem** Skript fortsetzen
  (resume): Fertige Agenten kommen aus dem Journal, fehlgeschlagene laufen
  neu. Mit geändertem Skript gilt P12.

Dasselbe Prinzip steckt in DEC-045 für harw selbst: ein Klon je Zellhost mit
Merge-Barriere statt vieler Schreiber auf einem Workspace.

### P12 — Resume trifft nur den unveränderten Präfix
Ein Resume liefert nur den **längsten unveränderten Präfix** der Agent-Aufrufe
aus dem Cache. Ab dem ersten geänderten oder neuen Aufruf läuft alles live,
auch Fixer und Coder, die schon fertig waren. Beispiel: Eine Welle bekommt
nachträglich einen Re-Review-Schritt, dann laufen nach dem ersten neuen
Re-Review alle späteren Fixer ein zweites Mal. Die Edits landen dann doppelt
oder werden überschrieben, und die Tokens sind verloren.

**Gegenmittel:**
- Schreibende Wellen nie auf geänderte Logik fortsetzen.
- Einen fehlenden Schritt (etwa den Re-Review einer Reparatur) als eigenen
  Agenten nachholen, mit Vertrag, Befund und Reparaturbericht aus dem Journal.
- Prüfen, ob schon doppelt geschrieben wurde: die mtimes der Dateien mit dem
  Resume-Zeitpunkt vergleichen.

### P13 — Gegen die Basis prüfen, nicht gegen den gefixten Baum
Werden Funde nachträglich geprüft, während ihre Fixes schon im Arbeitsbaum
liegen, lesen die Prüfer den gefixten Code. Dann verwerfen sie echte Funde
mit „ist schon behoben“. In R16 traf das drei von sechs nachgeprüften Funden.
Alle drei waren auf HEAD echt.

**Gegenmittel:** `gap-verify` bekommt `base`, den Commit, gegen den gefunden
wurde. Die Prüfer lesen `git show <base>:<datei>`. „Im Arbeitsbaum schon
behoben“ zählt als echt und wird als `fixed_in_tree` gemeldet.

### P14 — Das Arbeitsverzeichnis wandert mit
Wechselt die Hauptsession per `cd` in einen Worktree, wandert ihr
Arbeitsverzeichnis mit. Jeder Agent, der danach startet und nur „das aktuelle
Verzeichnis“ kennt, arbeitet im falschen Checkout:
- Fixer schreiben in den fremden Worktree.
- Reviewer sehen dort einen leeren Diff und melden „Fix fehlt“.
- Die Reparatur folgt dem Pfad aus dem Review-Text und schreibt den Fix ein
  zweites Mal, wieder im falschen Baum, auch wenn sie selbst im richtigen
  Verzeichnis startet.

In R16 traf das zwei kurze `cd`-Fenster. Betroffen waren etwa 15 Dateien aus
sieben Wellen, darunter ein doppelter, byte-gleicher Fix und zwei
verschiedene Fixes für denselben Fund.

**Gegenmittel:**
- Die Hauptsession wechselt nie per `cd` in einen Worktree, sondern nutzt
  `git -C <pfad>` und absolute Pfade.
- Schreibende Workflows verlangen `root`, auch für den Hauptbaum. Ohne
  `root` brechen `gap-fix` und `contract-wave` ab.
- Ein Wellen-Commit nimmt nur die benannten Dateien der Welle. Fremde
  Änderungen im Worktree werden gemeldet und nach dem Ende aller Wellen
  abgeglichen: Hat der Hauptbaum keinen Fix, wird verschoben; ist er gleich,
  wird verworfen; sind beide verschieden, entscheidet ein Review.
