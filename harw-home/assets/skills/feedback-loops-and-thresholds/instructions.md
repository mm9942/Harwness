# Rückkopplungen, Schwellen und Verschiedenartigkeit

**Regel:** Beschreibe ein System nicht als Kette von Ursache und Wirkung, sondern als Geflecht von Kreisläufen. Frag bei jedem Kreis: Verstärkt er Abweichungen oder dämpft er sie? Wo liegen Schwellen, jenseits derer sich das Verhalten schlagartig ändert? Und wie verschieden sind die Beteiligten?

**Warum:** In Systemen, in denen Akteure aufeinander reagieren, sind kleine Ursachen manchmal groß in der Wirkung und große manchmal wirkungslos. Durchschnitte täuschen: Ein System aus lauter „durchschnittlichen“ Akteuren verhält sich anders als eines aus verschiedenen. Wer nur lineare Zusammenhänge sucht, übersieht Kipppunkte und hält Überraschungen für Zufall, obwohl sie aus der Struktur folgen.

> Dieser Skill ist eine Arbeitsanleitung in eigenen Worten. Er beschreibt eine Technik und gibt kein Buch wieder. Er liefert Struktur, keine Simulation und keine Zahlenprognose.

## Wann nutzen

- Märkte, Organisationen, Netzwerke, soziale Bewegungen, Lieferketten, Ökosysteme, Softwareplattformen mit Nutzern.
- Wenn eine Maßnahme das Gegenteil des Beabsichtigten bewirkt hat.
- Wenn sich ein Zustand lange stabil hielt und dann plötzlich umschlug.
- Wenn Prognosen aus Durchschnittswerten wiederholt danebenliegen.
- Als Baustein für Szenarien (`scenario-wargaming`): Rückkopplungen liefern die Dynamik der Welten.

## Vorgehen

1. **Grenze ziehen.** Was gehört zum System, was ist Umwelt? Welche Größe interessiert (etwa Marktanteil, Vertrauen, Auslastung)?
2. **Akteure und Bestände.** Wer handelt? Welche Größen häufen sich an oder bauen sich ab (Kapital, Rückstand, Reputation, Erschöpfung)? Bestände erklären Trägheit und Verzögerung.
3. **Kompliziert oder komplex?** Test: Entfernt man ein Element, fällt dann nur dessen Beitrag weg (kompliziert) oder ändert sich das Verhalten des Ganzen (komplex)? Bei komplizierten Systemen genügt Zerlegen; bei komplexen muss man die Verknüpfungen analysieren.
4. **Kreisläufe zeichnen.** Für jede wichtige Wirkung: Wirkt sie über andere Größen auf ihre Ursache zurück? Schreib jeden Kreis als Kette `A ↑ → B ↑ → C ↓ → A ↓`.
   - **Verstärkend:** Die Abweichung wächst (Wachstum, Panik, Abwanderung, Netzwerkeffekt).
   - **Ausgleichend:** Die Abweichung wird gedämpft (Preisanpassung, Regelung, Sättigung).
   Bestimme für jeden Kreis den Typ, die Stärke (grob) und die Verzögerung.
5. **Dominanz klären.** Welcher Kreis bestimmt das Verhalten heute? Unter welcher Bedingung übernimmt ein anderer? Der Wechsel der Dominanz ist oft der eigentliche Umschlagpunkt.
6. **Schwellen suchen.** Wo ändert sich das Verhalten sprunghaft? Typische Hinweise: Kapazitätsgrenzen, Mindestmengen, Vertrauensverlust ab einer Schwelle, Angebot knapp über dem Bedarf. Systeme, die dauerhaft knapp an einer solchen Grenze arbeiten, sind stabil, bis sie es plötzlich nicht mehr sind.
7. **Verschiedenartigkeit prüfen.** Haben alle Akteure dieselben Reaktionsschwellen oder verschiedene?
   - Unter **ausgleichender** Rückkopplung stabilisiert Verschiedenartigkeit meist: Nicht alle reagieren gleichzeitig, die Reaktion verteilt sich.
   - Unter **verstärkender** Rückkopplung kann Verschiedenartigkeit destabilisieren: Wenige mit niedriger Schwelle stoßen andere an, eine Kaskade läuft durch.
   - Gleichartige Akteure reagieren dagegen entweder gar nicht oder alle zugleich.
8. **Hebel und Nebenwirkungen.** Wo wirkt ein Eingriff auf einen Kreis statt auf ein Symptom? Welche Kreise würden einen Eingriff kompensieren oder umkehren?
9. **Frühwarnzeichen.** Was würde anzeigen, dass sich das System einer Schwelle nähert (steigende Schwankung, langsamere Erholung nach Störungen, schrumpfender Puffer)?

## Ergebnisformat

```markdown
## Systemstruktur: <Gegenstand>

**Grenze und Zielgröße:** …
**Charakter:** kompliziert | komplex – <Begründung in einem Satz>

| Kreis | Kette | Typ | Stärke | Verzögerung | dominiert heute? |
|---|---|---|---|---|---|
| R1 | Nutzer ↑ → Angebot ↑ → Attraktivität ↑ → Nutzer ↑ | verstärkend | hoch | Monate | ja |
| B1 | Auslastung ↑ → Wartezeit ↑ → Nutzer ↓ | ausgleichend | mittel | Wochen | nein |

**Schwellen:** <Größe, ungefährer Bereich, was jenseits passiert>
**Verschiedenartigkeit:** <wie verteilt sind die Reaktionsschwellen; stabilisierend oder kaskadenfördernd>
**Hebelpunkte:** <Eingriff → betroffener Kreis → erwartete Gegenreaktion>
**Frühwarnzeichen:** …
**Unsicherheit:** <was an dieser Struktur Annahme ist>
```

## Fallstricke

- **Durchschnitt als Stellvertreter.** Der „typische Akteur“ existiert nicht; gerade die Ränder der Verteilung lösen Kaskaden aus.
- **Linear denken.** Doppelte Ursache heißt nicht doppelte Wirkung. Nahe einer Schwelle kann eine kleine Änderung alles kippen, fern davon bewirkt eine große wenig.
- **Verzögerungen vergessen.** Ein Eingriff, dessen Wirkung erst nach Monaten eintritt, wird oft verstärkt, weil „nichts passiert“, und schießt dann über.
- **Scheinpräzision.** Ohne Daten keine Zahlen für Stärken und Schwellen. Grobe Stufen und ehrliche Unsicherheit sind besser als erfundene Parameter.
- **Alles verbinden.** Ein Diagramm mit dreißig Kreisen erklärt nichts. Konzentriere dich auf die zwei bis vier Kreise, die das Verhalten tragen.
- **Struktur mit Vorhersage verwechseln.** Die Analyse zeigt, *welche* Verläufe möglich sind und wovon sie abhängen, nicht welcher eintritt.
