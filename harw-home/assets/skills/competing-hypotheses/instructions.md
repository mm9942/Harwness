# Konkurrierende Hypothesen: Matrix statt Lieblingserklärung

**Regel:** Prüfe nie nur die Erklärung, die dir als erste einfällt. Stelle mehrere Erklärungen nebeneinander und frag bei jedem Beleg: *Welche Hypothese macht dieser Beleg unwahrscheinlicher?* Am Ende gewinnt nicht die Hypothese mit den meisten Stützen, sondern die mit den wenigsten ernsthaften Widersprüchen.

**Warum:** Wer eine Erklärung im Kopf hat, liest jeden neuen Befund als Bestätigung. Fast jeder Befund passt zu mehreren Erklärungen und unterscheidet deshalb nichts. Nur Belege, die zu einer Hypothese passen und zu einer anderen nicht, tragen Information. Die Matrix macht diesen Unterschied sichtbar und zwingt dazu, Alternativen ernst zu nehmen.

> Dieser Skill ist eine Arbeitsanleitung in eigenen Worten. Er beschreibt eine Technik und gibt kein Buch wieder.

## Wann nutzen

- Eine Frage hat mehrere plausible Antworten: Ursache eines Vorfalls, Absicht eines Akteurs, Erklärung eines Marktverhaltens, Grund für einen Fehler.
- Ein Team (oder ein Agent) hat sich früh auf eine Erklärung festgelegt, und niemand hat die Alternativen geprüft.
- Die Belege sind zahlreich, widersprüchlich oder möglicherweise gezielt irreführend.
- Ein Ergebnis soll später nachvollziehbar begründet werden: Die Matrix ist zugleich Protokoll.
- Nicht nötig bei Fragen mit einer eindeutigen, direkt belegbaren Antwort (etwa „Welche Version steht in der Datei?").

## Vorgehen

1. **Frage schärfen.** Formuliere die Frage so, dass sich die Hypothesen gegenseitig ausschließen. „Warum sind die Umsätze gefallen?“ ist offen; „Welcher Faktor erklärt den Großteil des Rückgangs?“ erlaubt eine Matrix.
2. **Hypothesen sammeln, breit.** Mindestens drei, meist vier bis sieben. Nimm auch unbequeme auf: Täuschung, Zufall, Messfehler, „es hat sich gar nichts geändert“. Wer nur zwei Hypothesen hat, baut ein Entweder-oder, keine Prüfung.
3. **Hypothesen bereinigen.** Fasse Varianten zusammen, trenne Mischformen. Jede Hypothese muss so konkret sein, dass man sagen kann, welche Beobachtungen zu ihr passen würden und welche nicht.
4. **Belege und Argumente listen.** Nicht nur harte Fakten, auch Abwesenheiten („kein Hinweis auf X, obwohl wir ihn erwarten würden“) und tragende Annahmen. Jeder Eintrag bekommt eine Fundstelle.
5. **Matrix füllen, zeilenweise.** Nimm einen Beleg und bewerte ihn gegen **alle** Hypothesen, bevor du zum nächsten gehst. Bewertung: `++` stark konsistent, `+` konsistent, `0` neutral/nicht anwendbar, `−` inkonsistent, `−−` stark inkonsistent. Zeilenweises Arbeiten verhindert, dass du eine Hypothese „durchbewertest“.
6. **Diagnostizität prüfen.** Ein Beleg, der in jeder Spalte gleich bewertet ist, unterscheidet nichts. Markiere ihn als nicht diagnostisch und lass ihn für die Entscheidung weg (er bleibt im Protokoll). Die wertvollsten Zeilen sind die mit großen Unterschieden.
7. **Widersprüche zählen, nicht Stützen.** Lies spaltenweise: Wie viele `−` und `−−` hat jede Hypothese? Die Hypothese mit den wenigsten gewichtigen Widersprüchen ist die vorläufig beste. Viele `+` beweisen wenig, weil sie oft auch für andere gelten.
8. **Sensitivität testen.** Welche zwei oder drei Belege entscheiden das Ergebnis? Was, wenn einer davon falsch, gefälscht oder falsch gelesen ist? Kippt das Ergebnis, sag das ausdrücklich und prüfe diese Belege nach (siehe `evidence-quality-review`).
9. **Schluss mit Rangfolge.** Nenne alle Hypothesen mit Rang, nicht nur die Siegerin. Begründe, warum die anderen schwächer sind, und gib eine Einschätzung der Sicherheit (siehe `confidence-and-uncertainty`).
10. **Indikatoren festlegen.** Welche künftigen Beobachtungen würden die Rangfolge ändern? Pro Hypothese ein bis drei beobachtbare Anzeichen. So wird das Urteil überprüfbar.

## Ergebnisformat

```markdown
## Konkurrierende Hypothesen: <Frage>

| Beleg (Fundstelle) | Qualität | H1 | H2 | H3 | H4 | diagnostisch? |
|---|---|---|---|---|---|---|
| B1 <Kurzform> (<Datei:Zeile>) | hoch | + | −− | + | 0 | ja |
| B2 … | mittel | + | + | + | + | nein |

**Widersprüche je Hypothese:** H1: 0 stark / 1 schwach · H2: 2 stark · …
**Rangfolge:** 1. H1 – <Begründung> · 2. H3 – … · verworfen: H2 – <entscheidender Widerspruch>
**Entscheidende Belege (Sensitivität):** B1, B5 – wenn B1 falsch ist, liegt H3 vorn.
**Sicherheit:** <Stufe nach confidence-and-uncertainty>
**Indikatoren:** H1 bestätigt sich, wenn …; H3 gewinnt, wenn …
```

## Fallstricke

- **Zu wenige Hypothesen.** Die richtige Erklärung fehlt oft von Anfang an. Frag gezielt: Was wäre, wenn die Daten täuschen? Wenn es mehrere Ursachen gleichzeitig gibt?
- **Spaltenweises Bewerten.** Wer eine Hypothese komplett bewertet und dann die nächste, bewertet sie gegen sein Bauchgefühl statt gegen die anderen.
- **Stützen zählen.** Zehn `+` für eine Hypothese sind wertlos, wenn dieselben zehn Belege auch die anderen stützen.
- **Belege ohne Qualitätsangabe.** Eine einzige, schwach belegte Aussage darf keine Hypothese allein kippen. Qualität gehört in die Matrix.
- **Abwesenheit übersehen.** Ein erwartetes Signal, das ausbleibt, ist oft der diagnostischste Beleg. Frag bei jeder Hypothese, welche Spuren sie hinterlassen müsste.
- **Scheinpräzision.** Die Matrix ist ein Denkwerkzeug, keine Rechenmaschine. Summen und Gewichte sind Orientierung, kein Beweis.
- **Die Matrix als Endprodukt.** Der Leser braucht die Aussage oben und die Matrix als Anhang, nicht umgekehrt.
