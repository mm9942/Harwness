# Sicherheit und Unsicherheit ausdrücken

**Regel:** Jede Einschätzung trägt zwei getrennte Angaben: *wie wahrscheinlich* das Eingeschätzte ist, und *wie belastbar* die Grundlage dieser Wahrscheinlichkeit ist. Beide werden mit festen, vorher vereinbarten Begriffen ausgedrückt, nie mit freien Adjektiven.

**Warum:** Wörter wie „möglich“, „wahrscheinlich“ oder „kann nicht ausgeschlossen werden“ bedeuten für verschiedene Leser Werte zwischen 10 und 90 Prozent. Ein Leser, der entscheiden muss, braucht aber eine Größenordnung. Außerdem kann dieselbe Wahrscheinlichkeit auf gutem oder auf dünnem Material ruhen; das ist für die Entscheidung genauso wichtig. Feste Begriffe machen Einschätzungen vergleichbar und später überprüfbar.

> Dieser Skill ist eine Arbeitsanleitung in eigenen Worten. Er beschreibt eine Technik und gibt kein Buch wieder.

## Wann nutzen

- In jedem Analyseprodukt, das eine Aussage über Unbekanntes macht: Ursachen, Absichten, künftige Entwicklungen, Wirksamkeit.
- Beim Verdichten mehrerer Teilergebnisse (`synthesis-writer`).
- Beim Prüfen eines Produkts (`method-auditor`, `evidence-critic`): Passen Begriffe und Grundlage zusammen?
- Nicht nötig für reine Tatsachenberichte mit direkter Fundstelle.

## Die Begriffstabelle (verbindlich für die Analyse-Familie)

| Begriff | Bereich | Faustwert |
|---|---|---|
| nahezu sicher | 95–99 % | „wir würden 19 zu 1 wetten“ |
| sehr wahrscheinlich | 80–95 % | |
| wahrscheinlich | 55–80 % | |
| etwa gleich wahrscheinlich | 45–55 % | |
| unwahrscheinlich | 20–45 % | |
| sehr unwahrscheinlich | 5–20 % | |
| nahezu ausgeschlossen | 1–5 % | |

Absolute Aussagen („sicher“, „ausgeschlossen“) nur bei direktem, geprüftem Beleg. Wo eine Zahl verfügbar und sinnvoll ist, nenne sie zusätzlich in Klammern.

**Vertrauen in die Grundlage** (unabhängig von der Wahrscheinlichkeit):

| Stufe | Bedeutung |
|---|---|
| hoch | mehrere unabhängige, geprüfte Belege; tragende Annahmen gestützt; Alternativen geprüft |
| mittel | Belege glaubwürdig, aber lückenhaft oder aus wenigen Quellen; einzelne Annahmen ungeprüft |
| niedrig | dünnes, widersprüchliches oder ungeprüftes Material; Urteil beruht wesentlich auf Annahmen |

## Vorgehen

1. **Aussage präzisieren.** Was genau wird eingeschätzt, bis wann, unter welcher Bedingung? „X passiert“ ist nicht bewertbar; „X passiert vor Ende des Quartals“ schon.
2. **Wahrscheinlichkeit festlegen.** Wähle den Begriff aus der Tabelle. Frag dich: Würde ich bei diesem Wert tatsächlich wetten? Wenn nein, ist der Wert falsch.
3. **Grundlage bewerten.** Stufe das Vertrauen nach der zweiten Tabelle ein. Stütze dich auf die Ergebnisse von `evidence-quality-review` und `key-assumptions-check`, wenn vorhanden.
4. **Beides zusammen nennen.** Standardsatz: „Wir halten X für *wahrscheinlich* (Vertrauen: *mittel*), weil …“.
5. **Wissenslücken benennen.** Was wissen wir nicht, und würde es das Urteil ändern? Eine Lücke, die das Urteil nicht ändern würde, braucht keinen Absatz.
6. **Treiber und Umschlagpunkte.** Welche ein bis drei Beobachtungen würden die Einschätzung um eine Stufe verschieben?
7. **Abweichende Sichten.** Wenn Teilergebnisse oder Prüfer anders urteilen, nenne die Gegenposition mit ihrem stärksten Argument, nicht als Fußnote.
8. **Für später festhalten.** Präzise Einschätzungen mit Datum lassen sich im Nachhinein gegen den Ausgang halten. Das ist die Grundlage jeder Kalibrierung (siehe `method-validation`).

## Ergebnisformat

```markdown
**Einschätzung:** <Aussage mit Zeitraum/Bedingung> ist <Begriff> (<Bereich>).
**Vertrauen in die Grundlage:** <hoch | mittel | niedrig> – <ein Satz Begründung>
**Wichtigste Lücke:** <was fehlt, und in welche Richtung es das Urteil verschieben könnte>
**Umschlagpunkte:** <Beobachtung> → <neue Stufe>
**Abweichende Sicht:** <wer, was, stärkstes Argument>
```

## Fallstricke

- **Wahrscheinlichkeit und Vertrauen vermischen.** „Wir sind uns nicht sicher, also unwahrscheinlich“ ist ein Kategorienfehler. Ein Ereignis kann wahrscheinlich sein, auch wenn die Grundlage dünn ist.
- **Absichern durch Vagheit.** „Es kann nicht ausgeschlossen werden“ schützt den Verfasser und hilft dem Leser nicht. Nenne eine Stufe.
- **Übervertrauen.** Erfahrung steigert das Selbstvertrauen oft schneller als die Trefferquote. Wer häufig „nahezu sicher“ schreibt, sollte seine Bilanz kennen.
- **Rückschaufehler.** Nach dem Ausgang wirkt alles vorhersehbar. Beurteile frühere Einschätzungen nach der damaligen Lage, nicht nach dem Ergebnis.
- **Begriffe mischen.** Innerhalb eines Produkts gilt nur diese Tabelle. Keine Synonyme wie „vermutlich“, „denkbar“, „eher“ als Ersatz.
