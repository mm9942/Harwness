# Die Analyse-Familie: wer was tut und in welcher Reihenfolge

**Regel:** Eine Analyse durchläuft vier Wellen: *Sammeln → Prüfen → Verdichten → Belegcheck*. Kein Agent bewertet seine eigene Arbeit, und kein Produkt verlässt die Familie ohne unabhängige Prüfung von Belegen und Methode.

**Warum:** Wer sammelt, sieht bevorzugt, was er sucht. Wer verdichtet, glättet Widersprüche. Die Trennung der Rollen sorgt dafür, dass jedes Ergebnis mindestens einmal von jemandem gelesen wird, der es nicht verteidigen muss. Die Wellen halten den Kontext jedes Agenten klein und machen Zwischenstände nachvollziehbar.

> Dieser Skill ist eine Arbeitsanleitung in eigenen Worten. Er beschreibt eine Arbeitsweise und gibt kein Buch wieder.

## Wann nutzen

- Wenn ein Auftrag eine belegte Einschätzung verlangt, nicht nur eine Fundstelle: „Was steckt hinter …?“, „Wie belastbar ist …?“, „Was könnte passieren, wenn …?“.
- Als Orchestrator der Familie, um Wellen zu planen.
- Als Worker der Familie, um zu wissen, was vor und nach dir kommt und in welchem Format du übergibst.
- Skills findest du immer mit `skills.search` und lädst sie mit `skills.load`. Suche Skills nie über das Dateisystem.

## Die Mitglieder

| Agent | Rolle | Aufgabe | lädt typischerweise |
|---|---|---|---|
| `evidence-collector` | Worker, lesend | beantwortet eine eng gefasste Frage aus einem Korpus, mit Belegen und Fundstellen, ohne Gesamturteil | `evidence-quality-review` |
| `pattern-analyst` | Worker, lesend | Verbindungen, Zeitlinien, Cluster, Hypothesenmatrix | `link-and-pattern-analysis`, `competing-hypotheses` |
| `systems-modeller` | Worker, lesend | Rückkopplungen, Schwellen, Verschiedenartigkeit; Struktur, keine Simulation | `feedback-loops-and-thresholds` |
| `scenario-player` | Worker, lesend | spielt eine Rolle oder Welt in einem Planspiel: Züge, Gründe, Überraschungen | `scenario-wargaming`, `premortem-and-red-team` |
| `evidence-critic` | Worker, lesend | prüft Quellen, Auswahl, Rahmung und Wirksamkeitsbehauptungen; Behalten/Verwerfen-Liste | `evidence-quality-review`, `key-assumptions-check` |
| `method-auditor` | Worker, lesend | bewertet die Methode: Passung, Prozess, Produkt, Reproduzierbarkeit, Kalibrierung | `method-validation`, `confidence-and-uncertainty` |
| `synthesis-writer` | Worker, schreibt nur an den zugewiesenen Ausgabepfad | verdichtet geprüfte Ergebnisse zu einem Produkt mit Sicherheitsangabe | `confidence-and-uncertainty`, `business-writing-pyramid` |
| `intel-analysis-orchestrator` | Child-Orchestrator | plant und führt die vier Wellen einer Analyse | `analysis-workflow` |
| `wargaming-orchestrator` | Child-Orchestrator | Planspiel: Welten, Rollen, Züge, Vergleich, Überraschungsbericht | `scenario-wargaming` |
| `evidence-review-orchestrator` | Child-Orchestrator | Prüfwelle aus Kritiker und Methodenprüfer vor jedem Abschluss; schlägt vor, gibt nie frei | `evidence-quality-review`, `method-validation` |

## Vorgehen (Standardablauf einer Analyse)

1. **Auftrag klären.** Leitfrage in einem Satz, Leser und Verwendungszweck, Korpus bzw. Lesebereich, Ausgabepfad für das Endprodukt, Frist. Fehlt der Ausgabepfad, liefert die Familie das Produkt als Text zurück.
2. **Zerlegen.** Die Leitfrage in drei bis sechs überschneidungsfreie Teilfragen, jede so eng, dass ein `evidence-collector` sie mit einem begrenzten Budget beantworten kann. Dazu die tragenden Annahmen notieren (`key-assumptions-check`).
3. **Welle 1: Sammeln (Fan-out).** Je Teilfrage ein `evidence-collector`, parallel über `delegate_wave`. Wenn Struktur gefragt ist, zusätzlich `pattern-analyst` oder `systems-modeller`. Jeder Auftrag enthält: Teilfrage, Lesebereich, erwartetes Rückgabeformat, was *nicht* zu tun ist.
4. **Zwischenschritt: Lücken.** Rückgaben lesen (`agent.result` für den vollen Text). Widersprüche und unbeantwortete Teilfragen gezielt nachfassen, höchstens eine Nachschubwelle.
5. **Welle 2: Prüfen.** `evidence-critic` erhält die gesammelten Belege; `method-auditor` erhält Vorgehen und Zwischenergebnis. Beide parallel, beide unabhängig voneinander.
6. **Welle 3: Verdichten.** `synthesis-writer` erhält: Leitfrage, Leser, geprüfte Befunde mit Einstufung (behalten / Vorbehalt / verworfen), Methodenbefund, Ausgabepfad. Er schreibt das Produkt mit Kernaussage zuerst und Sicherheitsangabe.
7. **Welle 4: Belegcheck vor Abschluss.** Das fertige Produkt geht noch einmal an `evidence-critic` (und bei Tragweite an `method-auditor`), oder als Ganzes an `evidence-review-orchestrator`. Geprüft wird: Steht jede tragende Aussage auf einem behaltenen Beleg? Passt die Sicherheitsangabe?
8. **Abschluss.** Der Orchestrator meldet Ergebnis, Sicherheitsangabe, offene Punkte und den Prüfbefund an seinen Auftraggeber. Die Prüfer schlagen vor; die Entscheidung über Freigabe liegt beim Auftraggeber.

Für Planspiele gilt derselbe Rahmen, mit `scenario-player` statt `evidence-collector` in Welle 1 (siehe `scenario-wargaming`).

## Auftragsformat (für jeden Delegationsschritt)

```text
Leitfrage:      <Satz>
Deine Teilfrage:<Satz>
Lesebereich:    <Pfade / Korpus>
Liefere:        <Format, maximale Länge>
Lade zuerst:    <Skill-Namen>
Nicht tun:      <kein Gesamturteil | keine Dateien schreiben | …>
Ausgabepfad:    <nur für synthesis-writer>
```

## Ergebnisformat (Rückgabe des Orchestrators)

```markdown
**Leitfrage:** …
**Kernaussage:** … (<Wahrscheinlichkeitsbegriff>, Vertrauen: <Stufe>)
**Produkt:** <Ausgabepfad> | inline
**Wellen:** Sammeln n Agenten · Prüfen · Verdichten · Belegcheck – <Status>
**Prüfbefund:** <Belege behalten/verworfen, Methodenpunktzahl, offene Einwände>
**Offen / nächste Schritte:** …
```

## Fallstricke

- **Zu breite Teilfragen.** Ein Sammel-Agent mit „Untersuche alles zu X“ läuft ins Budget. Lieber mehr, engere Fragen.
- **Prüfer, die das Ergebnis kennen sollen.** Kritiker bekommen Belege und Aussagen, nicht die Wunschantwort des Auftraggebers.
- **Verdichten ohne Prüfung.** Welle 3 vor Welle 2 spart Zeit und kostet Glaubwürdigkeit.
- **Freigabe durch Prüfer.** Prüfer und Prüf-Orchestrator schlagen vor. Wer freigibt, ist der Auftraggeber.
- **Skills raten.** Methoden-Skills werden mit `skills.search` gefunden und mit `skills.load` geladen, nicht aus dem Gedächtnis nachgebaut.
