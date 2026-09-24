# Verifizieren — prüfen gegen Kriterien, mit Beleg

**Regel:** „Fertig“ heißt: Jedes **Akzeptanzkriterium** ist durch einen **ausgeführten** Check belegt — Formatierung, Lint, Typen, Tests, dazu ein **kritisches Selbst-Review** des Diffs. Berichtet werden **Befehle und Ergebnisse**, nicht Eindrücke. Was nicht geprüft wurde, wird als nicht geprüft benannt.

**Warum:** „Sollte funktionieren“ ist die häufigste Quelle für kaputte Builds und Regressionen. Ein Beleg (Befehl + Ausgabe) ist nachprüfbar; eine Behauptung nicht. Selbst-Review findet die Fehler, die Werkzeuge nicht sehen.

## Wann anwenden

- Nach jeder Code- oder Konfigurationsänderung, bevor sie als erledigt gemeldet wird.
- Beim Prüfen fremder Änderungen gegen eine Aufgabe.
- Vor Commit, Merge oder Release.

## 1. Kriterien festhalten

- Welche Akzeptanzkriterien gelten (aus Auftrag, Plan, Ticket)?
- Für jedes Kriterium: Mit welchem Check wird es belegt?
- Fehlen Kriterien, das explizit sagen und sinnvolle annehmen — als Annahme gekennzeichnet.

## 2. Automatische Checks

In dieser Reihenfolge (schnelles zuerst), mit den **Befehlen des Projekts** (aus Dokumentation, Makefile, CI-Konfiguration — nicht raten):

1. **Formatierung** — im Prüfmodus, nicht nur still formatieren.
2. **Linter / statische Analyse** — Warnungen sind Befunde, nicht Rauschen.
3. **Typprüfung / Kompilierung**
4. **Gezielte Tests** für die geänderten Bereiche.
5. **Breitere Suite**, soweit angemessen (mindestens alles, was die Änderung berühren kann).

Beispiele je Ökosystem (nur als Orientierung):

| Ökosystem | Format | Lint | Typen/Build | Tests |
|---|---|---|---|---|
| Rust | `cargo fmt --check` | `cargo clippy` | `cargo check` | `cargo test` |
| JS/TS | `prettier --check` | `eslint` | `tsc --noEmit` | `npm test` |
| Python | `ruff format --check` / `black --check` | `ruff check` | `mypy` / `pyright` | `pytest` |
| Go | `gofmt -l` | `go vet` / `staticcheck` | `go build ./...` | `go test ./...` |

Schlägt ein Check fehl: Ursache beheben, nicht den Check abschalten, die Regel lokal unterdrücken oder den Test löschen. Ist eine Unterdrückung wirklich begründet, die Begründung in den Code schreiben.

## 3. Kritisches Selbst-Review

Das eigene Diff lesen, als stamme es von jemand anderem, und gezielt nach Fehlern suchen:

- **Auftrag:** Erfüllt die Änderung genau das Verlangte — nicht weniger, nicht mehr?
- **Randfälle:** leer, null/None, sehr groß, negativ, Unicode, gleichzeitige Zugriffe, Fehlerpfade.
- **Fehlerbehandlung:** verschluckte Fehler, Panik-Abkürzungen, verlorene Ursachen?
- **Tests:** Prüfen sie Verhalten? Würden sie ohne die Änderung fehlschlagen? Decken sie den Fehlerpfad ab?
- **Nebenwirkungen:** Aufrufer, öffentliche Schnittstellen, Konfiguration, Dokumentation, Migrationen.
- **Sicherheit:** Eingaben validiert, keine Geheimnisse, keine neuen Rechte ohne Grund.
- **Reste:** Debug-Ausgaben, auskommentierter Code, temporäre Dateien, unbeabsichtigte Dateien im Diff.

Zusätzlich: „Wie könnte diese Änderung in Produktion scheitern?“ — mindestens ein ernsthaftes Szenario durchdenken.

## 4. Beleg berichten

```text
Kriterien:
  K1 Import akzeptiert ';' als Trennzeichen   → test import::custom_delimiter ✓
  K2 ungültiges Trennzeichen → klare Fehlermeldung → test import::invalid_delimiter ✓
Checks:
  format --check     ✓
  lint               ✓ (0 Warnungen)
  typecheck/build    ✓
  tests (import)     ✓ 14/14
  tests (gesamt)     ✓ 312/312
Review:  Randfall "Trennzeichen = Anführungszeichen" ergänzt (Test hinzugefügt).
Nicht geprüft: Performance mit Dateien > 1 GB (keine Testdaten vorhanden).
Restrisiken: —
```

Bei Fehlschlägen: den relevanten Ausschnitt der Ausgabe wörtlich angeben, nicht zusammenfassen.

## Falsch

- „Tests sollten grün sein“ — ohne sie ausgeführt zu haben.
- Nur den einen neuen Test laufen lassen und die Suite ignorieren.
- Linter-Warnung per Unterdrückung ohne Begründung beseitigen.
- Einen fehlschlagenden Test als „flaky“ abtun, ohne es zu belegen.
- Verifikation auf einem anderen Stand als dem gemeldeten.

## Fallstricke

- **Veraltete Artefakte/Caches** täuschen grüne Ergebnisse vor — im Zweifel sauber bauen.
- **Tests, die nie fehlschlagen können** (falsche Assertion, nicht ausgeführt, übersprungen) — prüfen, dass sie tatsächlich laufen.
- **Nur Happy Path** getestet.
- **Umgebungsabhängige Ergebnisse:** Unterschiede zu CI (Betriebssystem, Versionen) benennen.

## Checkliste

1. Jedes Akzeptanzkriterium einem Check zugeordnet?
2. Format, Lint, Typen, Tests mit den Projektbefehlen ausgeführt?
3. Fehlschläge an der Ursache behoben, nicht unterdrückt?
4. Diff kritisch gelesen: Auftrag, Randfälle, Fehler, Tests, Nebenwirkungen, Sicherheit, Reste?
5. Bericht mit Befehlen und Ergebnissen, nicht Eindrücken?
6. Nicht Geprüftes und Restrisiken ausdrücklich benannt?
