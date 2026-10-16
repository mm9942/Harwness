# Veröffentlichung von harw – Business-Paper zur Release-Entscheidung

> **Kernaussage:** Eine breite Veröffentlichung von harw ist auf Basis der derzeit belegten Informationen nicht freizugeben. Sinnvoll ist eine bedingte, beaufsichtigte Linux-Pilotfreigabe erst nach dokumentierter Prüfung der Lizenz, Tests/CI, konkreten Binärartefakte, Installation und Rücknahme. Bis dahin bleibt die Entscheidung offen; das Matrix-Game liefert wegen seines Ausfalls ausdrücklich kein Votum.

**Autorin:** Mia  
**Datum:** 2026-10-02

## Executive Summary

harw ist ein Rust-Workspace für beaufsichtigte KI-Agenten mit expliziten Berechtigungen, typisierten Werkzeugen, dauerhaftem Ausführungszustand und optionaler Host-Sicherheitsdurchsetzung. Das README weist den Status als aktive Entwicklung aus und sagt, Schnittstellen und Konfiguration könnten sich zwischen Revisionen ändern (README.md:5–9). Die Workspace-Metadaten nennen Version 0.9.0 und `publish = false` (Cargo.toml:183–192). `publish = false` betrifft crates.io; es belegt weder eine GitHub-Freigabe noch deren Unmöglichkeit.

Das Planspiel endete nach zwei von drei vorgesehenen Runden. Alle vier Sitze passten in beiden Runden; die Ergebnistabelle weist pro Sitz zwei verworfene/gepasste Züge aus (insgesamt acht). Der Bericht nennt zusätzlich 16 gepasste Züge im Abschnitt „Offene Fragen“; diese Zählung ist nicht deckungsgleich mit der Ergebnistabelle und wird hier nicht als Zahl der Entscheidungen interpretiert. Es gab keine auswertbaren Spielzüge, keine Umpire-Synthese und keine abgeschlossene Nachbetrachtung (Spielbericht, Zeilen 7–10, 68–104, 110–124, 127–159, 162–164). Das ist ein **Methoden-/Simulationsversagen**, kein positives oder negatives Entscheidungsergebnis. Ein geänderter Neustart ist wegen der erforderlichen neuen Szenariofreigabe blockiert; dieses Papier ersetzt den nicht erfolgten Lauf durch eine getrennte, ausdrücklich hypothetische Business-Analyse.

**Bedingte Empfehlung:** weder breite Veröffentlichung noch eine behauptete „Freigabe“ jetzt. Bereite einen begrenzten, rücknehmbaren und beaufsichtigten Linux-Pilot vor; starte ihn nur, wenn die untenstehenden Gates erfüllt und durch echte Prüfbelege dokumentiert sind. Andernfalls verschieben. Das ist eine analytische Empfehlung, nicht das Ergebnis des Planspiels.

## Kontext und Fragestellung

README.md beschreibt harw als lokal ausgeführtes Agententeam mit kontrollierten Seiteneffekten (README.md:5, 13–17, 59–61). Es benennt strategische Matrix-Games und Business-Paper als Anwendungsfälle (README.md:19–23). Dieses Papier beurteilt ausschließlich die **Veröffentlichung des Produkts harw**, nicht eine Veröffentlichung der Matrix-Game-Funktion.

Die zentrale Frage lautet: Welche Form der Veröffentlichung ist angesichts des belegten Reife- und Nachweisstands verantwortbar? Das README kennzeichnet aktive Entwicklung und veränderliche Schnittstellen/Konfiguration, nennt aber keine frühe Version 0.9.0. Für die Versionsangabe dieses Papiers gilt 0.9.0, entsprechend der lokalen Cargo.toml-Version (workspace.package, Zeile 184). `publish = false` betrifft crates.io und belegt nichts über GitHub-Binär-Releases.

## Belegte Fakten, Auftraggeberhinweis und offene Annahmen

| Kategorie | Aussage | Beleg / Status |
|---|---|---|
| Projektfakt | Aktive Entwicklung; Schnittstellen und Konfiguration können sich ändern; beaufsichtigte lokale Nutzung | README.md:5–9 |
| Projektfakt | Der README-Text dokumentiert getaggte GitHub-Releases mit vorgebauten Linux-Binärdateien für x86_64 und aarch64, Prüfsummenprüfung und Laufzeitvoraussetzungen (README.md:63–80). Das ist kein Beleg für ein konkretes aktuelles Release oder vorhandene Artefakte. | README.md:63–80 |
| Projektfakt | Quellinstallation über `cargo install --locked --path harw-cli` wird dokumentiert | README.md:82–91 |
| Projektfakt | Workspace-Version laut Release-/Tag-Hinweis: 0.9.1. Die lokale Cargo.toml-Kopie weicht ab: `workspace.package.version = 0.9.0`; `publish = false` (Cargo.toml:183–192). | Auftraggeberhinweis; lokale Kopie geprüft |
| Projektfakt | Changelog führt Änderungen unter „Unreleased“; Matrix-Game-Arbeit ist dort dokumentiert | CHANGELOG.md:7, 52–63 |
| Projektfakt | Packaging-Anleitung empfiehlt derzeit Quellinstallation; Homebrew-Datei ist eine Vorlage mit TODOs für URL, SHA-256 und Version | packaging/README.md:3–22 |
| Auftraggeberhinweis | Kimi OCR wird TOML-aktivierbar gemacht, ist aber nicht fertig und nicht standardmäßig aktiv | Mitteilung von Mia im Auftrag; kein unabhängiger Repository-Beleg behauptet |
| Nicht belegt | Rechteprüfung sämtlicher Release-Inhalte und Freigabe für eine konkrete Verteilung; aktuelle CI-/Testergebnisse, konkrete Release-Artefakte/Checksummen, Rollback-Erprobung, Nachfrage, Erlöse, Marktgröße und Supportkapazität | Cargo.toml deklariert MIT OR Apache-2.0 (Zeile 108), und LICENSE-MIT sowie LICENSE-APACHE liegen im Repository-Root. Das belegt weder die Rechteprüfung sämtlicher Release-Inhalte noch die Freigabe für eine konkrete Verteilung. Die übrigen Punkte sind im geprüften Material nicht nachgewiesen. |
| Planspielstatus | 2/3 Runden; alle Sitze passten, kein Spielzug; Status offen, Prüfpaket 0/3, keine Umpire-Synthese | Spielbericht: Zeilen 7–10, 68–104, 110–124, 157–170 |

Die fehlenden Nachweise sind keine Behauptung, dass Tests, Lizenz oder Artefakte nicht existieren. Sie sind Freigabelücken, bis ein Verantwortlicher sie prüft und belegt.

## Planspiel: Ergebnis ist nicht auswertbar

Der Workspace-Bericht nennt das Szenario `harw-release-entscheidung`, einen Lauf mit zwei von drei Runden und vier institutionellen Perspektiven (Spielbericht: Zeilen 5–10, 20–60). In Runde eins und zwei verzichteten alle Sitze auf Beiträge; jeweils wurden alle vier Züge als `Forfeited` protokolliert (Zeilen 68–104). Die Tabelle weist null Erfolge und null Misserfolge aus; der Spielbericht hält die Entscheidung offen und nennt 16 gepasste Züge (Zeilen 106–124, 162–170).

Daraus folgt **keine** simulierte Zustimmung, Ablehnung, Risikoabwägung oder Akteurspräferenz. Das Ende nach 2/3 Runden, 0 auswertbare Spielzüge, fehlender Debrief und fehlende Umpire-Synthese sind ein Methoden-/Simulationsversagen für den Zweck einer Release-Entscheidung. Die Zustandsfelder im Bericht sind bloß der unvollständige Simulationszustand und keine empirischen Produktmesswerte. Der veränderte Neustart wurde nicht durchgeführt; es liegt kein auswertbares Matrix-Game-Ergebnis vor. Es werden weder ein weiterer Lauf noch fingierte Resultate behauptet.

## Hypothetische Akteursperspektiven und Szenariopfade

Die folgenden Argumente sind analytische Hypothesen, keine Spielzüge oder tatsächlich geäußerten Positionen. Die Rollen entsprechen den im Bericht beschriebenen institutionellen Perspektiven (Spielbericht: Zeilen 22–60).

| Pfad | Mögliche Argumente der Akteure (hypothetisch) | Bedingungen / Kehrseite |
|---|---|---|
| **Pilot** | Release-Team kann begrenzte Reichweite und transparente Grenzen vertreten. Erstanwendende können beaufsichtigte, rücknehmbare Erprobung bevorzugen. Integrationsperspektive kann einen kleinen Kreis mit klaren Installationsschritten unterstützen. Review-Perspektive kann einen Pilot als Informationsgewinn gegenüber einer breiten Zusage bewerten. | Nur nach nachweisbaren Mindest-Gates; klarer Pilotumfang, Supportgrenzen, Rücknahmeweg, keine Behauptung unbelegter Sicherheit oder Einsatzreife. |
| **Breite Veröffentlichung** | Release-Team könnte Reichweite und einfache Auffindbarkeit anstreben. Anwender könnten ein fertiges Einstiegserlebnis erwarten; Integrations- und Review-Perspektiven würden konsistente Pakete, Kompatibilität und belastbare Betriebs-/Supportbelege verlangen. | Derzeit nicht begründbar: Reifegradhinweis und Änderlichkeit sind dokumentiert, während Rechteprüfung sämtlicher Release-Inhalte, Freigabe der konkreten Verteilung, Test- und Artefaktnachweise hier offen sind. OCR nicht als fertige Funktion bewerben. |
| **Verschieben** | Release-Team kann die Entscheidung bis zur Schließung der Nachweislücken vertagen. Anwender und Integratoren können klarere Grenzen und reproduzierbare Installation verlangen. Review kann Nicht-Veröffentlichung als legitime Alternative werten. | Verzögerung schützt vor überzogenen Zusagen, verschiebt aber auch Feedback; kein Markt- oder Umsatzschaden wird mangels Daten beziffert. Kriterien und nächste Prüfung festlegen. |

## Entscheidungslogik und Veröffentlichungsgates

Die Wahl soll anhand von überprüfbaren Mindestbedingungen erfolgen, nicht anhand des nicht auswertbaren Spiels. **Breit veröffentlichen** kommt erst in Betracht, wenn alle rechtlichen, technischen und operativen Gates geschlossen sind. **Pilotieren** ist die bevorzugte Zwischenoption, falls die Mindest-Gates erfüllt, aber Reife oder Supportumfang noch begrenzt sind. **Verschieben** gilt, sobald ein kritisches Gate offen ist oder ein Rücknahmeweg fehlt.

1. **Recht:** Cargo.toml deklariert MIT OR Apache-2.0 und beide Lizenzdateien liegen im Repository-Root. Die Rechte sämtlicher Release-Inhalte und die Freigabe für die konkrete Verteilung sind damit nicht belegt; beides vorab prüfen und dokumentieren.
2. **Qualität:** maßgebliche Tests und CI für den vorgesehenen Release-Commit sind gelaufen; Ergebnis und bekannte Grenzen sind dokumentiert. Dieses Papier behauptet keinen Testlauf.
3. **Lieferumfang:** konkrete Binärartefakte, Zielplattformen, Version und Prüfsummen sind erzeugt und verifiziert. README-Installationsbeschreibung allein ist kein Nachweis tatsächlicher Artefakte.
4. **Installation und Rücknahme:** Quell- und vorgesehener Binärpfad sind nachvollziehbar; Voraussetzungen, Upgrade-/Rollback-Verfahren und Fehlerbehebung sind praktisch geprüft.
5. **Kommunikation:** frühe Entwicklungsphase, beaufsichtigte Nutzung, veränderliche Schnittstellen sowie nicht verfügbare oder in Arbeit befindliche Funktionen werden eindeutig benannt. Kimi OCR nicht als fertig/standardmäßig aktiv darstellen.
6. **Betrieb:** Verantwortlichkeit, Supportumfang, Meldung und Behandlung kritischer Probleme sowie Entscheidung zur Fortsetzung oder zum Stopp sind benannt.

## Risiko- und Mitigationsmatrix

| Risiko | Beleg-/Unsicherheitslage | Mitigation / Gate |
|---|---|---|
| Unklare Rechte/Freigabe | Cargo.toml deklariert MIT OR Apache-2.0 (Zeile 108); LICENSE-MIT und LICENSE-APACHE sind vorhanden. Rechteprüfung sämtlicher Release-Inhalte und Freigabe der konkreten Verteilung sind nicht belegt. | Rechteprüfung und dokumentierte Freigabeentscheidung vor jeder öffentlichen Verteilung |
| Fehler oder Regression im frühen Reifegrad | README weist frühen Stand und veränderliche Schnittstellen aus (README.md:7) | Release-Commit festschreiben; relevante automatisierte und manuelle Prüfungen dokumentieren; bekannte Grenzen offenlegen |
| Paket-/Installationsfehler | README nennt Linux-Pfade/Voraussetzungen (README.md:63–91); Packaging empfiehlt Source und Homebrew-Datei ist Vorlage (packaging/README.md:3–22) | Zielplattformen begrenzen; Installationspfad und Deinstallation/Rollback praktisch prüfen; Artefakte und Prüfsummen verifizieren |
| Überzogene OCR-Erwartung | Kimi OCR laut Mia in Arbeit, nicht fertig/standardmäßig aktiv | Featurestatus und TOML-Voraussetzungen präzise kommunizieren; nicht in Release-Versprechen einrechnen |
| Supportüberlastung | Kapazität nicht belegt | Pilotkreis und Supportgrenzen festlegen; Rückmeldungen triagieren; bei fehlender Kapazität verschieben |
| Fehlinterpretation des Spiels | 2/3 Runden, alle Sitze passen, kein Ergebnis (Spielbericht: Zeilen 68–124) | Spielversagen getrennt von Business-Analyse berichten; keinerlei Entscheidung aus Simulation ableiten |
| Unklare Rücknahme / Nutzerfolgen | Rücknahme nicht nachgewiesen | dokumentierten Rücknahmeweg vor Pilotfreigabe testen; Stoppkriterium und Kommunikationsweg festlegen |

## Grober Phasenplan

| Phase | Ergebnis / Eintrittsbedingung | Entscheidungspunkt |
|---|---|---|
| 1. Nachweisaufnahme | Zuständige Personen prüfen Lizenz, Release-Commit, CI/Tests, Artefakte, Plattformumfang, Installation und Rücknahme | Offene kritische Punkte verhindern Start |
| 2. Release-Kandidat | Begrenzter, reproduzierbarer Kandidat mit Dokumentation, Prüfsummen und bekannten Einschränkungen | Review anhand aller Mindest-Gates |
| 3. Beaufsichtigter Pilot | Nur nach Gate-Freigabe; begrenzter Umfang, definierter Support und Rücknahme | Fortsetzen, nachbessern oder stoppen anhand beobachteter Befunde; keine vorweggenommenen Kennzahlen |
| 4. Breitere Freigabe oder Verschiebung | Pilotbeobachtungen und erneut geprüfte Nachweise verfügbar | Breite Veröffentlichung nur bei geschlossenen Gates; sonst verschieben |

Zeitangaben und Erfolgsschwellen bleiben offen, da keine belastbaren Aufwands- oder Nutzungsdaten vorliegen.

## Bedingte Empfehlung und Entscheidung

**Empfehlung:** die Veröffentlichung jetzt nicht als breit verfügbaren, produktionsreifen Release freigeben. Stattdessen die Vorbereitung eines beaufsichtigten, klar begrenzten Linux-Piloten priorisieren und diesen erst nach Erfüllung der Mindest-Gates starten. Sind Lizenz, Test-/CI-Nachweise, reale Artefakte oder Rücknahme nicht geklärt, lautet die Entscheidung **verschieben**.

Diese Empfehlung folgt aus dokumentiertem frühem Reifegrad und noch offenen Freigabenachweisen, nicht aus einem positiven oder negativen Matrix-Game-Ergebnis. Eine breite Freigabe bleibt eine spätere Option, keine Prognose. Die TOML-Aktivierbarkeit von Kimi OCR ist eine separate, noch laufende Arbeit und keine Voraussetzung, die als bereits erledigt dargestellt werden darf.

## Belegverzeichnis

- `README.md`: Zeilen 5–7 (Produktbild und Reifegrad), 13–29 (Anwendungsfälle), 59–61 (Architekturgrenzen), 63–95 (Installation und Voraussetzungen).
- `Cargo.toml`: Zeilen 183–192 (`workspace.package.version = 0.9.0` in der lokalen Kopie; `publish = false`). Versionsreferenz dieses Papiers: 0.9.1 laut Release-/Tag-Hinweis aus dem Auftrag; Abweichung bleibt ausdrücklich dokumentiert.
- `CHANGELOG.md`: Zeile 7 sowie 52–63 (Unreleased und Matrix-Game-Arbeit).
- `packaging/README.md`: Zeilen 3–13 (Quellinstallation als heutige Empfehlung), 15–22 (Homebrew-Formelvorlage mit TODOs).
- `matrix/harw-release-entscheidung-20260924T212906-1.md`: Zeilen 5–10 (Laufdaten), 14–18 (Zweck und bekannte Grenzen), 20–60 (hypothetische Rollen), 68–104 (Pässe), 106–124 (Zustand/Entscheidung offen), 127–170 (kein Debrief/keine Synthese/Methodik).

## Restrisiken und offene Punkte

- Rechteprüfung sämtlicher Release-Inhalte und Freigabe für die konkrete Verteilung sind trotz deklarierter Lizenz und vorhandener Lizenzdateien noch zu belegen; ebenso CI-/Teststatus, konkrete aktuelle Binärartefakte und Rollback.
- Kompatibilitätsumfang, tatsächliche Supportkapazität und Betriebsgrenzen sind nicht quantifiziert.
- Kimi OCR TOML-Aktivierung ist laut Auftraggeberin in Arbeit; Fertigstellung und Standardstatus sind nicht behauptet.
- Das Matrix-Game ist nicht entscheidungsfähig; die analytischen Pfade in diesem Papier sind hypothetisch und nicht als Simulationsergebnisse zu zitieren.
- Autorin und Datum sind nicht angegeben und daher nicht erfunden.
