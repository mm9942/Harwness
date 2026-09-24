# Implementieren — klein, konventionstreu, nachweisbar

**Regel:** Änderungen erfolgen in **kleinen, jeweils lauffähigen Schritten**, folgen den **Konventionen des Projekts**, werden von **Tests** begleitet und am Ende **verifiziert**. Der Umfang bleibt auf den Auftrag beschränkt; jede Abweichung wird benannt.

**Warum:** Große, gemischte Änderungen sind schwer zu prüfen und schwer zurückzunehmen. Code, der die Konventionen ignoriert, erzeugt Reibung für alle danach. Ohne Nachweis ist „fertig“ nur eine Behauptung.

## Wann anwenden

- Ein Feature, ein Fix oder ein Refactoring soll im Code umgesetzt werden.
- Ein Plan liegt vor und soll schrittweise abgearbeitet werden.

## Vor dem ersten Edit

1. **Auftrag und Grenzen klären:** Was genau soll sich ändern? Welche Dateien/Module gehören dazu, welche ausdrücklich nicht?
2. **Akzeptanzkriterien** festhalten: Woran erkennt man, dass es fertig ist?
3. **Umgebung lesen:** Nachbarcode, bestehende Muster, Projektrichtlinien (z. B. CLAUDE.md, CONTRIBUTING, Linter-Konfiguration), Namenskonventionen, Fehlerbehandlung, Teststil.
4. **Wiederverwenden statt neu erfinden:** Gibt es bereits eine Hilfsfunktion, einen Typ, ein Muster für genau das?

## Vorgehen

### Kleine Schritte

- Jeder Schritt lässt den Code in einem kompilierbaren, testbaren Zustand.
- Reihenfolge: Typen/Schnittstellen → Kernlogik → Anbindung → Aufräumen.
- Refactoring und Verhaltensänderung **trennen**: erst umstrukturieren (Tests bleiben grün), dann Verhalten ändern.

### Tests zuerst oder parallel

- Für neues Verhalten: Test schreiben, der das gewünschte Verhalten beschreibt; er muss zunächst fehlschlagen.
- Für Fixes: Regressionstest, der den Fehler reproduziert.
- Randfälle bedenken: leere Eingaben, Grenzwerte, Fehlerpfade, ungültige Daten.
- Tests prüfen Verhalten, nicht Implementierungsdetails.

### Robuster Code statt Abkürzungen

- **Keine Panik-Abkürzungen** im Produktionscode: kein `unwrap`/`expect` (Rust), kein `!`-Non-Null-Assertion (TypeScript), kein nacktes `except:` (Python), kein ignoriertes `err` (Go). Fehler werden weitergereicht oder bewusst behandelt (siehe `error-propagation`).
- Keine stillen Defaults, die Fehler verdecken.
- Keine auskommentierten Codeblöcke, keine TODOs ohne Verweis.
- Keine Geheimnisse im Code oder in Tests (siehe `secret-handling`).
- Ungültige Zustände möglichst durch Typen ausschließen, statt sie zur Laufzeit abzufangen.

### Umfang halten

- Nur ändern, was zum Auftrag gehört. Auffälligkeiten außerhalb notieren, nicht still mitändern.
- Keine neuen Abhängigkeiten ohne Recherche und Freigabe (siehe `dependency-research`).
- Öffentliche Schnittstellen nur ändern, wenn der Auftrag es verlangt — dann alle Aufrufer anpassen.

## Verifizieren

Vor „fertig“ die Projektprüfungen ausführen — typischerweise:

1. Formatierung
2. Linter / statische Analyse (Warnungen ernst nehmen)
3. Typprüfung / Kompilierung
4. Gezielte Tests, dann die relevante Suite
5. Eigenes Diff lesen, als wäre es fremder Code (siehe `verification`)

Welche Befehle genau gelten, steht in der Projektdokumentation oder CI-Konfiguration — nicht raten.

## Falsch

- Ein Commit mit Feature, Umbenennungen, Formatierung des halben Repos und Versionsupdate.
- Fehler per `unwrap()` / `!` / `catch {}` „vorläufig“ ignorieren.
- Test so lange anpassen, bis er grün ist, ohne zu verstehen, warum er rot war.
- „Sollte funktionieren“ melden, ohne die Tests ausgeführt zu haben.

## Richtig

```text
Auftrag:  Import akzeptiert optional ein Trennzeichen.
Schritte: 1) Option im Konfigurationstyp + Parser, Test für Default/Angabe/ungültig
          2) Importfunktion nutzt die Option, Test mit ';'
          3) CLI-Flag, Hilfetext, Doku
Geprüft:  fmt ✓  lint ✓  typecheck ✓  tests (import::*) ✓  Suite ✓
Geändert: src/config.*, src/import.*, src/cli.*, docs/import.md
Offen:    Encoding-Erkennung bleibt unverändert (außerhalb des Auftrags).
```

## Checkliste

1. Auftrag, Grenzen und Akzeptanzkriterien klar?
2. Konventionen des Projekts gelesen und befolgt?
3. Schritte klein, jeder lauffähig?
4. Tests für neues Verhalten und Randfälle vorhanden?
5. Keine Panik-Abkürzungen, keine verschluckten Fehler, keine Geheimnisse?
6. Format, Lint, Typen, Tests ausgeführt — mit Ergebnis?
7. Bericht: geänderte Pfade, Verifikation, Abweichungen, Restrisiken.
