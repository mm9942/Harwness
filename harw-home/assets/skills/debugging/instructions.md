# Debugging — reproduzieren, eingrenzen, minimal beheben, absichern

**Regel:** Erst **reproduzieren**, dann **eingrenzen**, dann **eine** Hypothese nach der anderen prüfen. Behoben wird die **Ursache**, nicht das Symptom — mit der kleinsten möglichen Änderung und einem **Regressionstest**, der vorher rot und nachher grün ist.

**Warum:** Wer ohne Reproduktion ändert, rät. Geratene Fixes verschieben den Fehler, verdecken ihn oder bauen neue ein. Ohne Regressionstest kommt derselbe Fehler beim nächsten Refactoring zurück.

## Wann anwenden

- Ein Test schlägt fehl, ein Build bricht, ein Programm stürzt ab oder liefert falsche Ergebnisse.
- Ein Fehler tritt nur manchmal auf (flaky), nur in einer Umgebung oder erst seit einer bestimmten Änderung.
- Jemand meldet „geht nicht“ ohne klare Ursache.

## Ablauf

### 1. Symptom präzise festhalten

- Exakte Fehlermeldung, Exit-Code, Stacktrace — **wörtlich** kopieren, nicht paraphrasieren.
- Erwartetes vs. tatsächliches Verhalten in je einem Satz.
- Umgebung: Version/Commit, Betriebssystem, Konfiguration, relevante Eingaben.
- Seit wann? Was hat sich zuletzt geändert (Code, Abhängigkeiten, Daten, Umgebung)?

### 2. Reproduzieren

- Kleinsten Befehl finden, der den Fehler **zuverlässig** auslöst (einzelner Test, ein Aufruf mit fester Eingabe).
- Nicht reproduzierbar? Nicht weiterraten — Unterschiede der Umgebung sammeln (Daten, Zeitzone, Parallelität, Cache, Rechte, Netzwerk) und gezielt variieren.
- Flaky: mehrfach laufen lassen, Häufigkeit notieren, Reihenfolge und Nebenläufigkeit als Verdächtige aufnehmen.

### 3. Eingrenzen

- **Eingabe verkleinern:** Testdaten halbieren, bis der Fehler gerade noch auftritt.
- **Code eingrenzen:** Aufrufkette von außen nach innen verfolgen; an Grenzen prüfen, ob die Daten dort noch stimmen.
- **Zeitlich eingrenzen (Bisect):** Gibt es einen guten und einen schlechten Stand, per binärer Suche den ersten schlechten Commit finden (`git bisect start <schlecht> <gut>`, dann `git bisect run <testbefehl>`).
- **Logs lesen, bevor man neue schreibt:** Vorhandene Logs rund um den Zeitpunkt prüfen; erst dann gezielt zusätzliche Ausgaben einfügen.

### 4. Hypothesenschleife

Für jede Hypothese:

1. Hypothese als überprüfbaren Satz formulieren („Der Cache liefert veraltete Werte, weil der Schlüssel die Sprache nicht enthält“).
2. Vorhersage ableiten: Was müsste man beobachten, wenn sie stimmt — und was, wenn nicht?
3. **Ein** Experiment durchführen, das beide Fälle unterscheidet.
4. Ergebnis notieren: bestätigt, widerlegt oder unklar.

Immer nur **eine Variable** ändern. Widerlegte Hypothesen notieren, damit sie nicht erneut geprüft werden. Eine Hypothese ist erst dann Ursache, wenn das Experiment sie **belegt** — nie als Fakt berichten, was nur vermutet ist.

### 5. Minimal beheben

- Zuerst den **Regressionstest** schreiben, der den Fehler reproduziert, und sehen, dass er rot ist.
- Dann die kleinste Änderung, die die Ursache behebt. Kein Aufräumen „nebenbei“, keine Umbauten ohne Auftrag.
- Keine Symptombehandlung: kein Fangen und Verschlucken des Fehlers, kein Hochsetzen eines Timeouts, kein Überspringen des Tests, ohne dass die Ursache verstanden ist.

### 6. Verifizieren

- Regressionstest jetzt grün.
- Die ursprüngliche Reproduktion aus Schritt 2 erneut ausführen.
- Umgebende Tests / gesamte Suite laufen lassen, um Nebenwirkungen auszuschließen.
- Bei flaky Fehlern: mehrfach wiederholen, bevor „behoben“ gemeldet wird.

## Falsch

- „Könnte am Cache liegen“ → Cache deaktiviert → Fehler weg → fertig. (Ursache unbekannt, Leistung verschlechtert.)
- Fünf Dinge gleichzeitig ändern und hoffen, dass eines hilft.
- Fehlermeldung zusammenfassen statt zitieren — das entscheidende Detail geht verloren.
- Test anpassen, bis er passt, statt den Code zu korrigieren.
- Debug-Ausgaben und auskommentierter Code bleiben nach dem Fix im Code.

## Richtig

```text
Symptom:   Export bricht bei Datei > 2 GiB mit "invalid length" ab (Commit abc123).
Repro:     export --input big.bin  → Exit 1, reproduzierbar 5/5.
Eingrenzt: Fehler entsteht beim Schreiben der Kopfzeile; Länge wird als 32-Bit-Zahl gespeichert.
Hypothese: Überlauf der Längenangabe bei > 2^31 Bytes.
Beleg:     Eingabe mit 2^31 − 1 Bytes läuft, 2^31 Bytes schlägt fehl.
Fix:       Längenfeld auf 64 Bit; Regressionstest mit Grenzwerten 2^31 − 1 / 2^31.
Verifiziert: Regressionstest rot → grün, Repro läuft durch, Suite grün.
```

## Fallstricke

- **Heisenbugs:** Zusätzliche Ausgaben verändern das Timing — bei Nebenläufigkeitsfehlern lieber zählen/aufzeichnen als an jeder Stelle loggen.
- **Falscher Ort:** Die Stelle, an der es kracht, ist oft nicht die Stelle, an der der Fehler entsteht. Daten rückwärts bis zur ersten falschen Stelle verfolgen.
- **Veralteter Build/Cache:** Vor tiefer Analyse sicherstellen, dass der getestete Stand wirklich der aktuelle ist.
- **Umgebungsunterschiede** (Locale, Zeitzone, Pfade, Rechte) erklären viele „nur bei mir“-Fehler.
- **Destruktive Schritte** (Daten löschen, Dienste neu starten, Prozesse beenden) nur mit ausdrücklicher Freigabe.

## Checkliste

1. Symptom wörtlich und mit Umgebung festgehalten?
2. Zuverlässige, minimale Reproduktion vorhanden?
3. Ursache durch ein Experiment belegt, nicht nur vermutet?
4. Regressionstest vor dem Fix rot, danach grün?
5. Fix minimal und auf die Ursache beschränkt?
6. Debug-Reste entfernt, Suite grün?
7. Bericht: Ursache, Fix, geänderte Pfade, Verifikation, Restrisiken.
