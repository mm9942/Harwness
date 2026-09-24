# Verbindungen, Zeitlinien und Muster

**Regel:** Ordne Einzelbefunde erst in eine Struktur (wer mit wem, was vor was, was ähnelt was), bevor du sie deutest. Jede Verbindung braucht einen Beleg und eine Stärke, jedes Muster einen Gegencheck: *Würde ich dieses Muster auch in zufälligen Daten finden?*

**Warum:** Viele Befunde erschließen sich erst im Zusammenhang: zwei Vorfälle mit derselben Spur, eine Person, die in drei Vorgängen auftaucht, eine Häufung an bestimmten Tagen. Ohne Struktur übersieht man diese Verbindungen. Mit Struktur, aber ohne Gegencheck, sieht man Muster, wo keine sind, denn Menschen und Modelle finden in fast jeder Menge irgendeine Regelmäßigkeit.

> Dieser Skill ist eine Arbeitsanleitung in eigenen Worten. Er beschreibt eine Technik und gibt kein Buch wieder.

## Wann nutzen

- Viele Objekte (Personen, Konten, Systeme, Vorfälle, Dokumente) und die Frage, wie sie zusammenhängen.
- Eine Abfolge von Ereignissen soll rekonstruiert oder auf Auffälligkeiten geprüft werden.
- Vorfälle sollen gruppiert werden: Stammen sie aus derselben Ursache, vom selben Akteur, aus derselben Schwachstelle?
- Vorbereitung einer Hypothesenprüfung (`competing-hypotheses`): Muster liefern Kandidaten, die Matrix prüft sie.

## Vorgehen

1. **Einheiten festlegen.** Welche Objekte (Knoten) und welche Beziehungen (Kanten) zählen? Beispiele: Person–Konto, Vorfall–Werkzeug, Dokument–Autor. Lege fest, was eine Beziehung belegt (gemeinsames Auftreten, direkte Transaktion, gleiche Spur).
2. **Namen bereinigen.** Schreibweisen, Aliasse, Tippfehler, geteilte Kennungen. Führe eine Zuordnungsliste und markiere unsichere Zusammenführungen. Falsch zusammengelegte Einheiten erzeugen Scheinverbindungen.
3. **Verknüpfungsmatrix.** Zeilen und Spalten sind die Einheiten; die Zelle enthält Art, Stärke (belegt / wahrscheinlich / vermutet) und Fundstelle. Bei gerichteten Beziehungen (A zahlt an B) getrennt nach Richtung.
4. **Netz lesen.** Wer ist zentral (viele Verbindungen), wer ist Brücke zwischen sonst getrennten Gruppen, wer ist auffällig isoliert? Brücken und Knoten mit wenigen, aber starken Verbindungen sind oft wichtiger als die mit den meisten.
5. **Zeitlinie.** Alle datierten Ereignisse in eine Reihe, mit Quelle und Genauigkeit (Tag, Woche, „vor X“). Achte auf:
   - Abfolgen, die sich wiederholen.
   - Lücken, in denen etwas passiert sein müsste.
   - Häufungen an bestimmten Wochentagen, Jahrestagen, Fristen, Ereignissen.
   - Unmögliche Reihenfolgen (Wirkung vor Ursache): Hinweis auf Datenfehler oder Täuschung.
6. **Cluster bilden.** Gruppiere Vorfälle nach gemeinsamen Merkmalen (Vorgehensweise, Ort, Zeit, Werkzeug, Spur). Nenne für jedes Cluster das verbindende Merkmal und wie spezifisch es ist: Ein seltenes Merkmal verbindet stärker als ein häufiges.
7. **Anomalien.** Was passt in kein Cluster, bricht die Routine, fällt aus der Zeitreihe? Anomalien sind entweder Datenfehler oder die interessantesten Befunde. Prüfe, welches von beiden.
8. **Akteur-Heuristik.** Wenn gefragt ist, wer für etwas in Frage kommt, prüfe je Kandidat fünf Punkte: *Fähigkeit* (kann er es?), *Wissen* (weiß er, wie und wo?), *Mittel* (hat er die Ressourcen?), *Zugang* (kommt er heran?), *Motiv* (hat er einen Grund?). Das ist eine Filterhilfe, kein Beweis.
9. **Gegencheck.** Für jedes Muster: Wie groß ist die Grundgesamtheit? Wie oft träte es zufällig auf? Gibt es eine banale Erklärung (Schichtplan, Feiertag, Systemzeit, Meldeweg)? Würde das Muster verschwinden, wenn man einen Zeitraum oder eine Quelle weglässt?
10. **Übergabe.** Muster sind Hypothesenkandidaten. Formuliere sie als prüfbare Aussagen und gib sie an die Hypothesenprüfung weiter.

## Ergebnisformat

```markdown
## Verbindungs- und Musteranalyse: <Gegenstand>

**Einheiten / Beziehungsarten:** …
**Zusammenführungen (unsicher):** <Alias → Einheit, Grund>

### Zentrale Verbindungen
| Von | Nach | Art | Stärke | Fundstelle |
|---|---|---|---|---|

### Zeitlinie (Auszug)
| Datum (Genauigkeit) | Ereignis | Quelle | Bemerkung |
|---|---|---|---|

### Cluster
- C1: <verbindendes Merkmal, Spezifität> – Mitglieder …

**Anomalien:** …
**Akteur-Heuristik:** <Kandidat: Fähigkeit/Wissen/Mittel/Zugang/Motiv, je belegt/offen>
**Gegencheck:** <banale Erklärungen, Zufallserwartung>
**Hypothesenkandidaten:** H1 …, H2 …
```

## Fallstricke

- **Gemeinsames Auftreten ist keine Beziehung.** Zwei Namen im selben Dokument sagen wenig; lege vorher fest, was als Verbindung zählt.
- **Mustererkennung ohne Grundrate.** Drei Vorfälle am Montag sind bei zwanzig Vorfällen pro Woche kein Muster.
- **Übereifrige Zusammenführung.** Zwei verschiedene Personen mit ähnlichem Namen zu einer zu machen erzeugt eine falsche Zentralfigur.
- **Das Netz ist so gut wie die Sammlung.** Wer nur an einer Stelle gesucht hat, findet dort das Zentrum. Nenne die Grenzen der Datengrundlage.
- **Heuristik als Urteil.** Fähigkeit, Zugang und Motiv machen einen Kandidaten möglich, nicht schuldig.
- **Diagramme ohne Aussage.** Ein Netzbild ist Material. Die Aussage darüber gehört in einen Satz.
