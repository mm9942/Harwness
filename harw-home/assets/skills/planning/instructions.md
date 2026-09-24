# Planen — Ziel, Grenzen, Schritte, Risiken, Nachweis

**Regel:** Ein Plan beginnt mit einem **überprüfbaren Ziel** und den **Randbedingungen**, zerlegt die Arbeit in **kleine, abhängigkeitsbewusste Schritte** mit Akzeptanzkriterien, benennt **Risiken** und legt fest, **wie** das Ergebnis verifiziert wird. Er enthält Checkpoints, an denen neu entschieden wird.

**Warum:** Ohne klares Ziel wird am falschen Problem gearbeitet. Ohne Zerlegung bleibt unklar, wo man steht. Ohne Verifikationsplan weiß niemand, wann man fertig ist. Ohne Checkpoints läuft ein falscher Plan bis zum Ende durch.

## Wann anwenden

- Eine Aufgabe umfasst mehr als ein paar Minuten Arbeit oder mehrere Dateien/Komponenten.
- Mehrere Personen oder Agenten arbeiten parallel.
- Die Aufgabe ist riskant (Migration, Sicherheitsrelevanz, Produktionssysteme).
- Anforderungen sind unklar — Planen deckt Lücken auf, bevor Code entsteht.

## Bausteine eines Plans

### 1. Ziel

- Ein bis zwei Sätze: Welcher Zustand soll am Ende erreicht sein, und für wen?
- **Messbar** formulieren: „Import verarbeitet 1 GB CSV in < 60 s“ statt „Import schneller machen“.
- **Nicht-Ziele** ausdrücklich nennen: Was gehört bewusst nicht dazu?

### 2. Randbedingungen

- Technisch: Sprache, Plattformen, Schnittstellen, die stabil bleiben müssen, Abwärtskompatibilität.
- Organisatorisch: Zeitrahmen, Freigaben, Zuständigkeiten, Wartungsfenster.
- Sicherheit und Recht: Datenschutz, Lizenzen, Berechtigungen.
- Annahmen explizit auflisten — jede Annahme ist ein potenzielles Risiko.

### 3. Zerlegung

- Schritte so klein, dass jeder einzeln prüfbar und idealerweise einzeln auslieferbar ist.
- Pro Schritt: **Ergebnis**, **Akzeptanzkriterium**, **Abhängigkeiten**, **Zuständigkeit**, betroffene Bereiche.
- Abhängigkeiten sichtbar machen: Was muss vorher fertig sein, was kann parallel laufen?
- Riskantes und Unbekanntes **früh** einplanen (Spike/Prototyp), nicht am Ende.

### 4. Risiken

| Risiko | Wahrscheinlichkeit | Auswirkung | Gegenmaßnahme / Frühindikator |
|---|---|---|---|
| API des Drittanbieters ändert sich | mittel | hoch | Adapter-Schicht, Vertragstest |
| Migration dauert länger als Wartungsfenster | niedrig | hoch | Probelauf mit Kopie, Rollback-Skript |

Nur Risiken aufnehmen, für die es eine konkrete Reaktion gibt.

### 5. Verifikationsplan

- Welche Prüfungen belegen jedes Akzeptanzkriterium (Tests, Messungen, Reviews, manuelle Abnahme)?
- Welche automatischen Checks laufen ohnehin (Format, Lint, Typen, Tests)?
- Wie wird ein Rollback geprüft, falls nötig?

### 6. Checkpoints

- Nach riskanten oder unklaren Schritten: anhalten, Ergebnis bewerten, Plan anpassen.
- Abbruchkriterien festlegen: Wann ist der Ansatz gescheitert und wird neu geplant?
- Freigabepunkte markieren, an denen eine Person entscheiden muss.

## Ergebnisformat

```text
Ziel:        <messbarer Endzustand>
Nicht-Ziele: <bewusst ausgeschlossen>
Annahmen:    <Liste>
Randbedingungen: <Liste>

Schritte:
  1. <Ergebnis> — Akzeptanz: <Kriterium> — hängt ab von: — — Zuständig: <Rolle>
  2. <Ergebnis> — Akzeptanz: <Kriterium> — hängt ab von: 1 — Zuständig: <Rolle>
  ◆ Checkpoint nach 2: <was wird entschieden>
  3. …

Risiken:      <Tabelle>
Verifikation: <Kriterium → Nachweis>
Offene Fragen: <was geklärt werden muss, von wem>
```

## Falsch

- „1. Implementieren. 2. Testen. 3. Fertig.“ — keine Kriterien, keine Abhängigkeiten.
- Ein Plan, der jedes Detail vorab festlegt und keine Anpassung vorsieht.
- Offene Fragen stillschweigend durch Annahmen ersetzen.
- Verifikation erst am Ende überlegen.

## Fallstricke

- **Zu grob:** Schritte wie „Backend umbauen“ sind nicht prüfbar — weiter zerlegen.
- **Zu fein:** Ein Plan mit 80 Mikroschritten wird nicht gelesen — auf prüfbare Einheiten verdichten.
- **Umfang wächst beim Planen:** Neue Wünsche als eigene Punkte notieren, nicht in den aktuellen Plan schieben.
- **Planen ist nicht Ausführen:** Dieser Skill liest und strukturiert; Änderungen am System folgen erst nach Zustimmung zum Plan.

## Checkliste

1. Ziel messbar, Nicht-Ziele genannt?
2. Randbedingungen und Annahmen explizit?
3. Jeder Schritt mit Ergebnis, Akzeptanzkriterium, Abhängigkeit, Zuständigkeit?
4. Riskantes früh eingeplant?
5. Risiken mit konkreten Gegenmaßnahmen?
6. Jedes Akzeptanzkriterium hat einen Nachweis im Verifikationsplan?
7. Checkpoints, Abbruchkriterien und Freigabepunkte gesetzt?
8. Offene Fragen mit Adressat aufgelistet?
