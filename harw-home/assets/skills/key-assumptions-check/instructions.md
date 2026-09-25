# Tragende Annahmen prüfen

**Regel:** Jede Analyse ruht auf Annahmen, die niemand ausgesprochen hat. Schreib sie auf, bevor du das Ergebnis verteidigst, und prüfe für jede: *Was wäre, wenn sie nicht stimmt?*

**Warum:** Die meisten schweren Fehlurteile entstehen nicht aus fehlenden Daten, sondern aus einer Voraussetzung, die so selbstverständlich wirkte, dass niemand sie prüfte: „Der Akteur handelt rational in unserem Sinn“, „Die Messung ist vergleichbar mit letztem Jahr“, „Was früher galt, gilt weiter“. Eine offen liegende Annahme kann man prüfen, beobachten und notfalls austauschen. Eine verborgene wirkt einfach weiter.

> Dieser Skill ist eine Arbeitsanleitung in eigenen Worten. Er beschreibt eine Technik und gibt kein Buch wieder.

## Wann nutzen

- Zu Beginn einer Analyse, sobald eine erste Arbeitshypothese steht.
- Vor der Abgabe eines Ergebnisses, besonders wenn es sich „glatt“ anfühlt.
- Wenn verschiedene Leute aus denselben Daten verschiedene Schlüsse ziehen: Meist unterscheiden sie sich in den Annahmen, nicht in den Fakten.
- Wenn sich die Lage schnell ändert und alte Einschätzungen weiterverwendet werden.
- Als Pflichtschritt für `evidence-critic` und `method-auditor` bei jedem Analyseprodukt.

## Vorgehen

1. **Aussage fixieren.** Schreib die Kernaussage der Analyse in einem Satz auf. Annahmen prüft man immer gegen eine konkrete Aussage.
2. **Annahmen ernten.** Geh die Begründungskette Schritt für Schritt durch und frag bei jedem Übergang: *Was muss wahr sein, damit dieser Schritt gilt?* Typische Fundorte:
   - Akteure: Ziele, Fähigkeiten, Rationalität, Einigkeit, Lernfähigkeit.
   - Daten: Vollständigkeit, Vergleichbarkeit über Zeit, Ehrlichkeit der Quellen.
   - Umfeld: Stabilität der Rahmenbedingungen, keine neuen Akteure, keine Schocks.
   - Methode: Das Modell passt zum Problem; Vergangenheit sagt etwas über die Zukunft.
   - Begriffe: Alle meinen dasselbe mit „Erfolg“, „Risiko“, „Wirksamkeit“.
3. **Formulieren.** Jede Annahme als überprüfbaren Satz: „Wir nehmen an, dass …“. Unscharfe Annahmen („die Lage bleibt ungefähr gleich“) präzisieren, bis man sagen kann, wann sie verletzt wäre.
4. **Bewerten.** Für jede Annahme zwei Urteile:
   - **Belastbarkeit:** gestützt (mit Beleg), plausibel (ohne direkten Beleg, aber ohne Gegenhinweis), fragwürdig (Gegenhinweise vorhanden).
   - **Tragweite:** Kippt die Kernaussage, wenn die Annahme fällt? ja / teilweise / nein.
5. **Kipp-Annahmen markieren.** Annahmen mit Tragweite „ja“ und Belastbarkeit „plausibel“ oder „fragwürdig“ sind die gefährlichsten. Sie gehören in den Ergebnisbericht, nicht nur in die Arbeitsnotizen.
6. **Gegenprobe.** Für jede Kipp-Annahme: Formuliere das Gegenteil und skizziere in zwei, drei Sätzen, wie die Welt dann aussähe. Gibt es bereits Anzeichen dafür?
7. **Beobachtungspunkte festlegen.** Für jede Kipp-Annahme ein bis zwei Signale, an denen man früh merkt, dass sie nicht mehr gilt. Wer beobachtet, wann wird nachgesehen?
8. **Ergebnis anpassen.** Hängt die Aussage an einer fragwürdigen Annahme, senke die angegebene Sicherheit oder formuliere die Aussage bedingt („Solange X gilt, …“).

## Ergebnisformat

```markdown
## Annahmenprüfung zu: <Kernaussage in einem Satz>

| # | Annahme („Wir nehmen an, dass …“) | Belastbarkeit | Tragweite | Beleg / Gegenhinweis | Beobachtungspunkt |
|---|---|---|---|---|---|
| A1 | … | gestützt | ja | <Fundstelle> | – |
| A2 | … | plausibel | ja | kein direkter Beleg | <Signal, wer, wann> |
| A3 | … | fragwürdig | teilweise | <Gegenhinweis> | <Signal> |

**Kipp-Annahmen:** A2, A3 – <was passiert, wenn sie fallen>
**Folgen für das Ergebnis:** <Sicherheit gesenkt auf …> | <Aussage bedingt formuliert>
```

## Fallstricke

- **Nur die bequemen Annahmen.** Leicht findet man Annahmen über andere; schwer die über die eigene Sicht, die eigenen Begriffe und die eigene Methode. Frag ausdrücklich danach.
- **Fakten als Annahmen tarnen.** „Die Quelle ist zuverlässig“ ist eine Annahme, solange niemand die Zuverlässigkeit geprüft hat.
- **Liste ohne Konsequenz.** Eine Annahmenliste, die das Ergebnis nicht verändert, war Beschäftigungstherapie. Jede Kipp-Annahme muss sich in Sicherheit oder Formulierung niederschlagen.
- **Zu viele Annahmen.** Zwanzig Einträge verdünnen die fünf wichtigen. Sortiere nach Tragweite und kürze.
- **Einmal prüfen, nie wieder.** Annahmen altern. Beobachtungspunkte sind nur nützlich, wenn jemand sie tatsächlich prüft.
