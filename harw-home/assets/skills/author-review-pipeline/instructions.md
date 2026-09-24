# Autor-Pipeline — Entwurf → Review → Endfassung

**Regel:** Wer schreibt, prüft nicht selbst ab; wer prüft, schreibt nicht um; wer Folien oder Endfassung baut, ändert die Storyline nicht. Zwischen den Stufen wandern **feste Übergabepakete** – nie lose Chat-Fragmente.

**Warum:** Eine Person (oder ein Agent), die ihren eigenen Text prüft, sieht vor allem, was sie gemeint hat. Ein Prüfer, der umschreibt, ersetzt fremde Argumente durch eigene, und niemand merkt, was sich geändert hat. Eine Folienstufe, die nebenbei die Argumentation umbaut, macht das Review wertlos. Feste Pakete machen jede Änderung nachvollziehbar.

## Wann anwenden

- Texte mit Entscheidungscharakter: Vorlagen, Management-Zusammenfassungen, Berichte, Angebote, Präsentationen.
- Sobald mehr als eine Person/ein Agent am Text arbeitet.
- Nicht nötig für kurze Routinenachrichten ohne Entscheidung.

## Die drei Stufen

| Stufe | Rolle | Eingabe | Ausgabe | Darf nicht |
|-------|-------|---------|---------|------------|
| 1 Entwurf | `business-author` | Auftrag, Material | Entwurfspaket (Storyline oder Text) | sich selbst freigeben, Belege erfinden |
| 2 Review | `business-reviewer` | Entwurfspaket | Befundbericht mit Urteil | umschreiben, Ersatzfassung liefern |
| 3 Endfassung | `business-author` (Text) oder `slides-builder` (Folien) | freigegebenes Paket | Endfassung | Storyline ändern |

Ablauf:

1. **Auftrag klären:** Leser, gewünschte Handlung des Lesers, Frage des Lesers, Umfang, Termin. Fehlt Leser oder Ziel, geht der Auftrag zurück.
2. **Storyline** (Stufe 1) → **Review der Storyline** (Stufe 2). Erst wenn die Storyline freigegeben ist, lohnt Ausformulieren.
3. **Ausformulierter Entwurf** (Stufe 1) → **Review des Entwurfs** (Stufe 2).
4. **Endfassung** (Stufe 3): Text glätten oder Folien bauen. Bei Folien optional eine Titelprobe durch den Reviewer.
5. **Abbruchregel:** Nach zwei Review-Runden ohne Freigabe entscheidet der Auftraggeber über die strittigen Befunde.

## Übergabeformate

### Entwurfspaket (Stufe 1 → 2)

```markdown
## Entwurfspaket v<n>
- Auftrag: <1 Satz>
- Leser / soll danach: <…> / <…>
- Stufe: Storyline | Entwurf
- Kernaussage: <1 Satz>
- Storyline: <eingebettet>
- Text: <eingebettet oder Pfad>
- Offene Lücken: <[BELEG FEHLT: …]>
- Annahmen: <Liste>
- Bitte an Review: <Schwerpunkte>
- Änderungen seit letzter Fassung: <Befund-ID → Änderung | Widerspruch + Begründung>
```

### Befundbericht (Stufe 2 → 1 bzw. → 3)

```markdown
## Review zu Entwurfspaket v<n>
Urteil: freigegeben | überarbeiten | zurück an Auftraggeber
Kernaussage, wie verstanden: <1 Satz>
| ID | Schwere | Kriterium | Stelle | Befund | Richtung |
Offene Belegstellen: <…>
Tragende Annahmen / Risiken: <…>
Status früherer Befunde: <ID → erledigt | begründet abgelehnt | offen>
```

### Freigabevermerk (→ Stufe 3)

```markdown
## Freigabe
- Paket: v<n>, Review <Kennung>, Urteil freigegeben
- Freigegeben durch: <Reviewer und/oder Person>
- Offene kann-Befunde: <Liste oder keine>
- Zielformat: Text | Folien (<Anzahl, Vorlage>)
```

## Review-Rubrik

Reihenfolge einhalten; ein `muss` in einem frühen Kriterium macht spätere Detailprüfung oft überflüssig.

| Kriterium | Prüffrage | typisch `muss`, wenn … |
|-----------|-----------|------------------------|
| Kernaussage zuerst | Steht oben ein Satz, der die Frage des Lesers beantwortet? | nur ein Thema, Antwort erst am Ende, mehrere konkurrierende Kernaussagen |
| Logik | Folgt die Kernaussage zwingend aus den tragenden Aussagen? | Sprung, Zirkel, verdeckte Voraussetzung trägt alles |
| Gruppierung | Gleichartig, überschneidungsfrei, zusammen vollständig? Überschriften als Aussagen? | Ursachen und Maßnahmen gemischt, offensichtliche Lücke, Dopplung |
| Belege | Ist jede tragende Aussage belegt, passen Zahlen zusammen? | zentrale Zahl ohne Quelle, Summen/Einheiten widersprüchlich |
| Risiken | Sind Annahmen und stärkstes Gegenargument benannt? | Entscheidung hängt an unausgesprochener Annahme |
| Leserführung | Versteht die Zielperson es ohne Vorwissen, ist die Bitte klar? | kein nächster Schritt, Fachjargon ohne Erklärung für Laien-Leser |

Schweregrade:

- **muss** – ohne Behebung trägt der Text die Entscheidung nicht oder wäre irreführend.
- **soll** – schwächt Überzeugungskraft oder Verständlichkeit deutlich.
- **kann** – Verbesserung, nicht blockierend.

Mini-Beispiel (frei erfunden): Kernaussage „Wir sollten die Wartung ab Q3 an einen Dienstleister geben, weil das 18 % Kosten spart und Ausfälle halbiert.“ – Befund R1 (muss, Belege): „Die 18 % beziehen sich auf 2023, die Ausfallzahl auf 2024; Zeitraum angleichen oder begründen.“ Richtung: „gleiches Basisjahr verwenden“.

## Regeln für alle Stufen

- Keine erfundenen Zahlen, Quellen oder Zitate; Lücken sichtbar markieren.
- Keine Wiedergabe urheberrechtlich geschützter Werke; nur Technik anwenden.
- Dateien nur auf Auftrag und nur über freigabepflichtige Werkzeuge schreiben.
- Jede Stufe nennt ihre Version (`v<n>`), damit Befunde eindeutig zuordenbar sind.
