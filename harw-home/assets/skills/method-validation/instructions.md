# Methoden prüfen: Funktioniert das Vorgehen wirklich?

**Regel:** Eine Analysemethode ist nicht gut, weil sie vernünftig klingt oder weil ein Ergebnis zufällig stimmte. Prüfe getrennt, ob der *Prozess* sauber war und ob das *Produkt* taugt, und frag nach Belegen, dass die Methode unter vergleichbaren Bedingungen bessere Urteile liefert als eine einfache Alternative.

**Warum:** Viele strukturierte Methoden werden empfohlen, weil sie einleuchten, nicht weil jemand ihre Wirkung gemessen hat. Ein sauberer Prozess garantiert kein richtiges Ergebnis, und ein richtiges Ergebnis beweist keinen guten Prozess, denn Glück ist häufig. Außerdem kann zu viel erzwungene Formalisierung erfahrene Urteile verschlechtern. Wer Methoden nicht prüft, verwechselt Ritual mit Qualität.

> Dieser Skill ist eine Arbeitsanleitung in eigenen Worten. Er beschreibt eine Technik und gibt kein Buch wieder.

## Wann nutzen

- Als Prüfschritt vor dem Abschluss eines Analyseprodukts (`method-auditor`).
- Wenn eine Methode für wiederkehrende Aufgaben eingeführt oder beibehalten werden soll.
- Wenn zwei Vorgehen konkurrieren und entschieden werden muss, welches bleibt.
- Rückblickend, wenn frühere Einschätzungen mit dem Ausgang verglichen werden können.

## Vorgehen

1. **Methode und Anspruch benennen.** Was genau wurde gemacht (Schritte, Werkzeuge, Datenbasis)? Was verspricht die Methode: bessere Treffer, weniger Verzerrung, schnellere Ergebnisse, nachvollziehbare Begründung?
2. **Passung prüfen.** Passt die Methode zur Frage? Eine Hypothesenmatrix passt zu „Welche Erklärung?“, nicht zu „Wie groß wird der Markt?“. Die Frage bestimmt die Methode, nicht Gewohnheit oder Mode.
3. **Prozessqualität** (Wurde die Methode sauber angewandt?):
   - Wurden Alternativen ernsthaft betrachtet?
   - Sind Annahmen offengelegt?
   - Sind Belege mit Fundstellen und Qualitätsangaben versehen?
   - Sind Sicherheitsangaben in festen Begriffen gemacht?
   - Ist jeder Schritt so dokumentiert, dass ein Dritter ihn wiederholen könnte?
4. **Produktqualität** (Taugt das Ergebnis?):
   - Beantwortet es die gestellte Frage?
   - Folgt die Aussage aus den Belegen?
   - Ist es präzise genug, um sich als falsch herausstellen zu können?
   - Ist es für den Leser rechtzeitig und nutzbar?
5. **Reproduzierbarkeit.** Käme ein zweiter Durchlauf mit anderem Bearbeiter oder anderem Agenten auf dasselbe Ergebnis? Wenn möglich, prüfe es: Lass einen Teilschritt unabhängig wiederholen und vergleiche. Große Abweichungen zeigen, dass das Ergebnis am Bearbeiter hängt, nicht an der Methode.
6. **Kalibrierung.** Gibt es frühere Einschätzungen mit Sicherheitsangaben und bekanntem Ausgang? Dann vergleiche: Traten Dinge, die als „wahrscheinlich“ galten, in etwa 55–80 % der Fälle ein? Systematisch zu hohe Treffererwartung ist Übervertrauen, zu niedrige ist Überängstlichkeit. Ohne Rückmeldung ist keine Kalibrierung möglich; dann das als Lücke benennen.
7. **Wirksamkeitsnachweis.** Gibt es Belege über die Plausibilität hinaus, dass die Methode besser ist als ein einfaches Vorgehen (Expertenurteil ohne Struktur, Fortschreibung, Basisrate)? Wenn nein: Die Methode ist „plausibel, aber ungeprüft“ und so zu kennzeichnen.
8. **Verzerrungen der Bewertung selbst meiden.**
   - *Ergebnisfehler:* Ein gutes Ergebnis macht den Prozess nicht nachträglich gut.
   - *Rückschaufehler:* Nach dem Ausgang wirkt jedes Warnsignal offensichtlich.
   - *Rückwirkung:* Eine Warnung kann das Gewarnte verhindern oder herbeiführen und so die eigene Überprüfung verzerren.
9. **Punktzahl vergeben.** Je Kriterium 0–3 (0 fehlt, 1 schwach, 2 ausreichend, 3 gut) mit einem Satz Begründung. Die Summe ist Orientierung, die Begründungen sind das Ergebnis.
10. **Empfehlung.** Beibehalten, nachbessern (was genau), ersetzen (wodurch) oder erst prüfen (welcher Test).

## Ergebnisformat

```markdown
## Methodenprüfung: <Methode> für <Frage>

| Kriterium | Punkte (0–3) | Begründung |
|---|---|---|
| Passung zur Frage | | |
| Prozess: Alternativen | | |
| Prozess: Annahmen offen | | |
| Prozess: Belege mit Qualität | | |
| Prozess: Dokumentation | | |
| Produkt: beantwortet die Frage | | |
| Produkt: folgt aus Belegen | | |
| Produkt: prüfbar präzise | | |
| Reproduzierbarkeit | | |
| Kalibrierung | | <oder: keine Rückmeldedaten> |
| Wirksamkeitsnachweis | | |

**Summe:** <n>/33 – Orientierung, kein Urteil
**Befund:** <zwei bis drei Sätze>
**Empfehlung:** beibehalten | nachbessern: … | ersetzen durch … | erst prüfen: …
```

## Fallstricke

- **Prozess mit Produkt verwechseln.** Beide getrennt bewerten; ein hoher Prozesswert rettet kein unbrauchbares Produkt.
- **Einzelfall als Beleg.** Ein Treffer oder ein Fehlschlag sagt fast nichts über eine Methode. Erst Serien zeigen Kalibrierung.
- **Übermethodisierung.** Pflichtschritte, die niemandem helfen, kosten Zeit und können gute Urteile verflachen. Frag bei jedem Schritt, was er beiträgt.
- **Nur die eigene Methode prüfen.** Ohne Vergleich mit einer einfachen Alternative bleibt offen, ob der Aufwand etwas bringt.
- **Punktzahl als Wahrheit.** Die Zahl ist ein Überblick. Entscheidend sind die Begründungen je Kriterium.
