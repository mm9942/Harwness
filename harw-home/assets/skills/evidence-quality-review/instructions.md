# Belegqualität prüfen: Quelle, Auswahl, Rahmung

**Regel:** Ein Beleg ist nur so gut wie drei Dinge zusammen: die Quelle, die Information selbst und die Auswahl, durch die er in die Analyse kam. Prüfe alle drei getrennt, bevor ein Beleg ein Urteil tragen darf.

**Warum:** Analysen scheitern selten an zu wenig Material, sondern daran, dass schwaches Material wie starkes behandelt wird. Eine glaubwürdige Quelle kann Falsches weitergeben. Eine wahre Information kann durch einseitige Auswahl ein falsches Bild erzeugen. Und schon die Entscheidung, was überhaupt als „Evidenz“ zählt, legt fest, welche Fragen beantwortbar sind und welche aus dem Blick geraten.

> Dieser Skill ist eine Arbeitsanleitung in eigenen Worten. Er beschreibt eine Technik und gibt kein Buch wieder.

## Wann nutzen

- Bevor Befunde eines Sammel-Agenten (`evidence-collector`) in eine Synthese gehen.
- Wenn ein Ergebnis auf wenigen oder auf einer einzigen Quelle ruht.
- Bei Wirksamkeitsbehauptungen („Methode X funktioniert“, „Maßnahme Y senkt Z“).
- Wenn Belege auffällig gut zur gewünschten Antwort passen.
- Wenn Interessen im Spiel sind: Auftraggeber, Hersteller, Lager in einer Debatte.

## Vorgehen

1. **Belege inventarisieren.** Jede tragende Aussage mit Fundstelle (Datei, Seite, Zeile, URL) und dem Satz, den sie stützen soll. Aussagen ohne Fundstelle kommen auf die Liste „unbelegt“.
2. **Quelle bewerten.** Für jede Quelle getrennt:
   - Zugang: Konnte sie das wissen (Augenzeuge, Messung, Hörensagen, Zusammenfassung Dritter)?
   - Bilanz: Lag sie früher richtig? Gibt es bekannte Fehler?
   - Interesse: Was gewinnt sie, wenn man ihr glaubt?
   - Unabhängigkeit: Stammen mehrere „Quellen“ in Wahrheit aus einem Ursprung? Zählt als eine.
   Stufen: zuverlässig / meist zuverlässig / unklar / fragwürdig.
3. **Information bewerten, unabhängig von der Quelle.** Ist sie in sich stimmig? Passt sie zu anderen, unabhängigen Belegen? Ist sie spezifisch genug, um falsch sein zu können? Stufen: bestätigt / wahrscheinlich / möglich / zweifelhaft.
4. **Auswahl prüfen.** Wie kam dieser Beleg in die Analyse?
   - Wurde gezielt nach Bestätigung gesucht und nach Widerspruch nicht?
   - Fehlen ganze Gruppen, Zeiträume oder Orte? Wer taucht in den Daten nicht auf, und warum?
   - Überleben nur Erfolgsfälle (die Gescheiterten hinterlassen keine Berichte)?
   - Wurde ein Ausschnitt gewählt, der ein Muster zeigt, das im Ganzen nicht besteht?
5. **„Plausibel, aber unbelegt“ markieren.** Viele Behauptungen klingen richtig, weil sie zu unserem Weltbild passen. Frag bei jeder Wirksamkeitsbehauptung: Gibt es einen Vergleich mit einer Kontrollgruppe, einem Vorher-Nachher mit ausgeschlossenen Alternativerklärungen oder eine unabhängige Wiederholung? Wenn nicht, ist sie eine Vermutung und wird so bezeichnet.
6. **Rahmung prüfen: Wer definiert, was zählt?** Welche Arten von Belegen hat der Auftrag zugelassen (nur Zahlen, nur Studien, nur interne Daten)? Welche Fragen lassen sich damit gar nicht stellen? Welche Werte oder Ziele stecken stillschweigend in der Auswahl der Messgrößen? Benenne das offen, statt es als neutrale Technik auszugeben.
7. **Passung statt Rangliste.** Ein Beleg ist nicht gut, weil er in einer allgemeinen Hierarchie oben steht, sondern weil er zur Frage passt. Eine kontrollierte Studie aus einem anderen Umfeld kann schlechter passen als eine sorgfältige lokale Beobachtung.
8. **Entscheiden.** Jeder Beleg bekommt eine von drei Einstufungen: **behalten** (trägt), **behalten mit Vorbehalt** (nur mit Hinweis im Ergebnis), **verwerfen** (trägt nicht; Grund nennen).
9. **Folgen benennen.** Welche Aussagen der Analyse verlieren durch die Einstufung ihre Stütze? Welche Lücken müssen neu gesammelt werden?

## Ergebnisformat

```markdown
## Belegprüfung: <Analyse / Frage>

| ID | Beleg (Fundstelle) | stützt Aussage | Quelle | Information | Auswahlrisiko | Urteil | Grund |
|---|---|---|---|---|---|---|---|
| E1 | … (<Datei:Zeile>) | S2 | zuverlässig | bestätigt | gering | behalten | – |
| E2 | … | S1 | unklar | möglich | hoch: nur Erfolgsfälle | Vorbehalt | … |
| E3 | … | S3 | fragwürdig | zweifelhaft | – | verwerfen | Einzelquelle mit Eigeninteresse |

**Plausibel, aber unbelegt:** <Behauptungen>
**Rahmung:** <welche Belegarten zugelassen waren, was dadurch ausgeblendet bleibt>
**Aussagen ohne tragfähige Stütze:** S3 – <Folge>
**Nachsammeln:** <gezielte Fragen an evidence-collector>
```

## Fallstricke

- **Quelle und Inhalt vermischen.** Eine gute Quelle macht eine unwahrscheinliche Aussage nicht wahr; eine schlechte Quelle macht eine bestätigte Aussage nicht falsch.
- **Zirkuläre Bestätigung.** Fünf Artikel, die dieselbe Pressemitteilung wiedergeben, sind ein Beleg, nicht fünf.
- **Menge statt Passung.** Viele schwache Belege ergeben keinen starken.
- **Neutralität behaupten.** Jede Auswahl von Messgrößen ist eine Entscheidung. Sie offen zu nennen ist kein Makel, sie zu verschweigen schon.
- **Alles verwerfen.** Perfekte Belege gibt es selten. Ziel ist eine ehrliche Einstufung, keine leere Liste.
- **Selbst nachrecherchieren.** Die Prüfung stellt Lücken fest und formuliert Nachfragen. Das Nachsammeln ist eine eigene Aufgabe.
