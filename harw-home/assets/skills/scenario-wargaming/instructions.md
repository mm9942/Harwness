# Szenario-Planspiel: mehrere Welten, Rollen, Überraschungen

**Regel:** Ein Planspiel soll zeigen, woran noch niemand gedacht hat. Es wird deshalb in mehreren Welten gespielt, mit Gegenspielern, die aktiv nach Schwächen suchen, und es endet mit einer Liste von Überraschungen und Lehren, nicht mit einem Sieger.

**Warum:** Einzelne Szenarien sind meist Fortschreibungen der Vergangenheit; sie spiegeln die Vorstellungskraft ihres Verfassers. Menschen und Agenten, die eine andere Partei ernsthaft spielen, finden Züge, die in keiner Tabelle stehen. Wer aber nach einem Gewinner fragt, belohnt das Spielen auf Sieg statt das Lernen, und eine einzelne Partie zeigt ohnehin nur einen möglichen Verlauf.

> Dieser Skill ist eine Arbeitsanleitung in eigenen Worten. Er beschreibt eine Technik und gibt kein Buch wieder.

## Wann nutzen

- Eine Strategie, ein Produktstart oder eine Maßnahme soll getestet werden, bevor echtes Geld oder echte Reputation fließt.
- Mehrere Parteien reagieren aufeinander: Wettbewerber, Regulierer, Angreifer, Kunden, Partner.
- Die Lage ist zu verflochten für eine lineare Analyse.
- Es wird nach „Was könnte uns überraschen?“ gefragt.
- Nicht geeignet, um eine Zahl vorherzusagen oder eine Entscheidung allein zu begründen.

## Die Rollen

| Rolle | Aufgabe | in der Analyse-Familie |
|---|---|---|
| **Spielleitung** | baut Welten und Regeln, verteilt Rollen, wertet Züge aus, streut Schocks ein, hält Informationsgleichheit | `wargaming-orchestrator` |
| **Eigenes Team** | spielt die zu testende Strategie | `scenario-player` |
| **Gegenspieler** | spielen Wettbewerber oder Gegner mit deren Zielen und Mitteln, nicht mit unseren | `scenario-player` (je Partei einer) |
| **Gegenspieler-Zelle** | sucht gezielt Lücken, Täuschungen und unkonventionelle Züge | `scenario-player` mit Auftrag „red cell“ |
| **Umfeld / Markt** | bewertet, wie Dritte (Kunden, Öffentlichkeit, Regulierer) auf die Züge reagieren | `scenario-player` oder Spielleitung |
| **Struktur** | liefert die Dynamik der Welt (Rückkopplungen, Schwellen) | `systems-modeller` |

## Vorgehen

1. **Lernziel festlegen.** Welche Frage soll das Spiel beantworten? Beispiele: „Wo ist unsere Strategie verwundbar?“, „Welche Reaktion der Konkurrenz haben wir unterschätzt?“. Kein Lernziel, kein Spiel.
2. **Welten bauen.** Zwei bis vier deutlich verschiedene Ausgangslagen. Unterscheide sie an den zwei unsichersten und folgenreichsten Treibern (zum Beispiel Nachfrage hoch/niedrig × Regulierung streng/locker). Jede Welt bekommt eine kurze, konsistente Beschreibung.
3. **Spielbuch je Partei.** Ziele, Mittel, Zwänge, bekannte Gewohnheiten, was die Partei weiß und was nicht. Gegenspieler bekommen dieselbe Sorgfalt wie das eigene Team.
4. **Regeln.** Anzahl der Züge (meist drei), simulierte Zeit je Zug, was ein Zug enthalten muss (Handlung, Begründung, erwartete Reaktion), wie kommuniziert wird. Alle Mitteilungen laufen über die Spielleitung, damit niemand heimlich mehr weiß.
5. **Züge spielen.** Pro Zug: Alle Parteien ziehen unabhängig. Die Spielleitung bewertet das Zusammenspiel und die Reaktion des Umfelds und gibt jeder Partei nur die Rückmeldung, die sie realistisch sehen würde.
6. **Schocks.** In mindestens einem Zug pro Welt ein unerwartetes Ereignis (Ausfall, Skandal, neuer Akteur, Regeländerung). Schocks prüfen die Anpassungsfähigkeit, nicht die Planerfüllung.
7. **Überraschungen einsammeln.** Nach jedem Zug fragt die Spielleitung jede Partei: Was hat dich überrascht? Welchen Zug hattest du nicht erwartet? Wo hat deine Annahme über die anderen nicht gestimmt?
8. **Welten vergleichen.** Was geschieht in allen Welten (robust)? Was nur in einer (weltabhängig)? Welche Züge der eigenen Strategie funktionieren überall, welche nur unter günstigen Bedingungen?
9. **Auswerten.** Lehren, Überraschungskandidaten, tragende Annahmen, Frühwarnzeichen je Welt. Keine Rangliste der Parteien.

## Ergebnisformat

```markdown
## Planspiel-Auswertung: <Lernziel>

**Welten:** W1 <Kurzname> · W2 … (Treiber: <A> × <B>)
**Parteien:** <Rolle → Agent/Spieler>

### Überraschungskandidaten
| # | Überraschung | Welt(en) | ausgelöst durch | warum unerwartet | Relevanz |
|---|---|---|---|---|---|

### Robust vs. weltabhängig
- robust (alle Welten): …
- nur in W2: …

### Lehren für die Strategie
1. <Aussage> – belegt durch Zug <n> in W<k>

**Frühwarnzeichen je Welt:** …
**Grenzen des Spiels:** <was nicht gespielt wurde, welche Parteien fehlten>
```

## Fallstricke

- **Sieger küren.** Wer „gewinnt“, hat oft nur die günstigere Welt oder die schwächeren Gegenspieler bekommen. Die Frage lautet: Was haben wir gelernt?
- **Strohmann-Gegner.** Gegenspieler, die nach unserer Logik handeln, bestätigen nur unsere Strategie. Sie brauchen eigene Ziele und dürfen unfair, klug und überraschend spielen.
- **Eine einzige Welt.** Ein Verlauf ist eine Anekdote. Erst der Vergleich über Welten trennt Robustes von Glück.
- **Regeln, die das Ergebnis vorwegnehmen.** Wenn die Spielleitung die Reaktion des Umfelds schon vorher festgelegt hat, ist es kein Spiel, sondern eine Präsentation.
- **Zu viel Realismus.** Je detaillierter die Regeln, desto langsamer das Spiel und desto weniger Raum für ungewöhnliche Züge. Genug Realismus für glaubwürdige Reaktionen, nicht mehr.
- **Spielergebnis als Prognose.** Ein Planspiel zeigt Möglichkeiten und Verwundbarkeiten. Wahrscheinlichkeiten braucht eine eigene Einschätzung (`confidence-and-uncertainty`).
