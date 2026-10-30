# Matrix-Game-Bericht: harw Open Source: Veröffentlichung und fünf Jahre institutionelle Entwicklung

| Feld | Wert |
|---|---|
| Szenario | `harw-oss-fuenfjahre` |
| Modus | Classic |
| Lauf | `20260924T222415-1` |
| Seed (Präfix) | `acf6bbef` |
| Runden | 1 von 6 |
| Status | ended |

## Zweck und Schlüsselfragen

Sechs Entscheidungsfenster vom ersten öffentlichen Open-Source-Release bis Jahr 5; mehrere bedingte Entwicklungspfade in einem verzweigten Spielverlauf prüfen, nicht Markt- oder Umsatzprognosen erstellen. Kernfrage: Unter welchen Entscheidungen bleibt harw offen, sicher betreibbar und wartbar, und wann scheitert Adoption? Quellen: Cargo.toml:100-109 (0.3.0, publish=false, MIT OR Apache-2.0); LICENSE-MIT und LICENSE-APACHE existieren; README.md:63-99 beschreibt GitHub-Releases für Linux x86_64/aarch64 und Quellinstallation, aber keine unabhängige Prüfung aktueller Artefakte. Frühere Pass-only-Simulation matrix/harw-release-entscheidung-20260924T212906-1.md:68-100 ist ausdrücklich KEIN Ergebnis. Annahme: Veröffentlichungsentscheidung, Sitzinstitutionen, Ressourcen und künftige Ereignisse sind fiktiv; Rechte für sämtliche Inhalte/Abhängigkeiten und tatsächliche Release-Artefakte sind vor echter Veröffentlichung gesondert zu prüfen. Kimi-OCR-TOML-Aktivierung ist in Arbeit, weder abgeschlossen noch Release-Versprechen. Business-Softwareagenten (business-author, business-reviewer, matrix-scenario-author, matrix-game-master, matrix-market, matrix-player, matrix-umpire, matrix-redcell) sind reale Arbeits-/Spielrollen der Software, NICHT diese hypothetischen Business-Stakeholder-Sitze.

**Ausgangslage bzw. Schlüsselfragen:**

- Der zu prüfende Veröffentlichungsgegenstand ist harw/Harwness in frühem Entwicklungsstand 0.3.0; Cargo.toml:105 setzt publish=false für crates.io und :108 MIT OR Apache-2.0. LICENSE-MIT und LICENSE-APACHE sind vorhanden, aber das beweist NICHT Rechte an allen Inhalten oder Drittkomponenten. README.md:63-99 beschreibt Linux-Archive auf GitHub Releases und Quellinstallation; Verfügbarkeit und Funktionsfähigkeit konkreter Binaries sind hier nicht getestet. Governance: CONTRIBUTING.md, CODE_OF_CONDUCT.md und SECURITY.md existieren, ihre Wirksamkeit und Zuständigkeiten sind vor Veröffentlichung zu prüfen. TOML-Aktivierung von Kimi OCR ist in Arbeit, keine fertige Funktion. Alle folgenden Organisationen, Ressourcen, Jahre und Ereignisse sind Simulationsannahmen, keine identifizierten echten Nutzer, Firmen, Behörden, Marktanteile, Kosten oder Umsätze. Nach jedem Jahresfenster muss jeder Sitz genau eine prüfbare Maßnahme samt Pro und Contra für verschiedene mögliche Zustände vorschlagen; keine Wahrscheinlichkeit oder Zahl für Marktwachstum erfinden. Bei Blockade belege explizit, was fehlt, und beantrage einen begrenzten Ersatzschritt statt zu passen. Sicherheitsvorfall nur als Szenario-Injekt oder durch Engine-Spielzug, niemals als Tatsache behaupten. Qualitative Tracks sind ausschließlich Spielsituationsindikatoren, keine empirischen KPIs.

## Akteure

### Hypothetischer Maintainer-/Community-Rat (`maintainers`, Ebene Institutioneller Stakeholder)

Du repräsentierst ein fiktives Maintainer-/Community-Gremium, keine konkreten Personen. Du weißt nicht, dass der Beschaffungssitz intern einen widerrufbaren Pilot statt breiter Einführung bevorzugt. Du hast Release- und Reviewpriorisierung, Beitragsregeln und freiwillige begrenzte Wartungskapazität; diese sind nur Spielmittel. R1 beantrage gestufte Veröffentlichung erst nach Rechteinventar und Security-Check oder begründete Verschiebung; R2 Jahr 1 Review-/Issue-Triage und klare Beitragspflege; R3 Jahr 2 Wartungskapazitäts-Projekt und Rückfallpfad; R4 Jahr 3 Security-Disclosure-/Incident-Übung; R5 Jahr 4 Governance-Übergabe bei Überlast; R6 Jahr 5 nachhaltige Versionierungs-/Abkündigungsregeln. Gegenargument: schneller öffentlicher Launch kann Reichweite bringen, ohne Wartung aber Vertrauen zerstören. Bei fehlender Evidenz eskaliere an Sicherheitssitz und blockiere nur den unbelegten Teilschritt. Benenne stets alternative günstige und ungünstige Folgepfade.

Öffentliche Ziele:
- Primär: überprüfbar rechtmäßige und wartbare Offenlegung
- Sekundär: offene Community-Beiträge mit Verantwortlichkeiten

Machtmittel: simulierte Release-Entscheidung, simulierte Beitrags-/Reviewregeln, begrenzte freiwillige Zeit

### Hypothetischer Integratoren-/Dienstleisterverbund (`integrators`, Ebene Institutioneller Stakeholder)

Du repräsentierst fiktive Dienstleister, keine nachgewiesenen Partner oder Umsätze. Du weißt nicht, dass der Maintainer-Rat bei Überlast zuerst eine Governance-Übergabe erwägt. R1 definiere Linux-Installations- und Kompatibilitäts-Pilot ohne Verkaufsversprechen; R2 Jahr 1 erstelle reproduzierbare Rückfallanleitung; R3 Jahr 2 biete begrenzte Review-/Dokumentationshilfe gegen nachvollziehbare Governance an; R4 Jahr 3 kapsle Security-Fixes und stoppe riskante Rollouts; R5 Jahr 4 teste Portabilität gegen konkurrierende Lösungen ohne erfundene Wettbewerbszahlen; R6 Jahr 5 beantrage offenen Supportvertrag nur bei getesteten Zusagen. Gegenargument: Sonderanpassungen erhöhen die Last und verschieben Verantwortung. Falls Freigabe aussteht, biete einen isolierten Quellcode-Pilot unter überprüften Rechten an statt zu passen.

Öffentliche Ziele:
- Primär: reproduzierbare, sichere und rücknehmbare Integrationen
- Sekundär: transparente Supportzuständigkeit

Machtmittel: simulierte Installations- und Testkapazität, hypothetisches Kundenfeedback, Entscheidung über begrenzte Integrationshilfe

### Hypothetischer Anwender-/Beschaffungsverbund (`procurement`, Ebene Institutioneller Stakeholder)

Du repräsentierst fiktive Pilotanwendende und Beschaffung, nicht echte Kunden. Du weißt nicht, dass der Integrator intern seine Kompatibilitätskapazität begrenzt. R1 verlange dokumentierte Lizenz-/Inhaltsrechte und beaufsichtigten Pilot; R2 Jahr 1 prüfe lokale Sicherheitsgrenzen und Exit; R3 Jahr 2 beantrage unabhängigen Alternativenvergleich; R4 Jahr 3 setze Piloten bei Sicherheitsverdacht aus und fordere reproduzierbaren Fix; R5 Jahr 4 vergleiche Beschaffungs- und Regulierungsanforderungen; R6 Jahr 5 entscheide zwischen offenem Betrieb, Dienstleister, Fork und Nichtnutzung anhand dokumentierter Evidenz. Gegenargument: Warten schützt, kann Erkenntnis über Realbetrieb verhindern. Bei Nachweislücke ersetze Großrollout durch Sandbox-Evaluation und konkretes Nachweisgesuch.

Öffentliche Ziele:
- Primär: kontrollierte, widerrufbare Nutzung mit belegten Rechten
- Sekundär: nachvollziehbarer Vergleich gegen Nichtnutzung/Alternativen

Machtmittel: simuliertes Pilot-Gate, simulierte Anforderungsliste, hypothetische Evaluationsberichte

### Hypothetischer Security-/Compliance-Prüfverbund (`assurance`, Ebene Institutioneller Stakeholder)

Du repräsentierst fiktive unabhängige Prüfer, keine reale Aufsichtsbehörde; du erlässt keine echten Gesetze. Du weißt nicht, dass der Beschaffungssitz unter Umständen eine isolierte Evaluation auch ohne Produktivfreigabe erwägt. R1 prüfe Lizenz-Evidenz, Berechtigungsgrenzen und Meldewege; R2 Jahr 1 verlange Schwachstellen-Triage und nachvollziehbare Sandbox-Tests; R3 Jahr 2 priorisiere Supply-Chain-/Abhängigkeitsprüfung; R4 Jahr 3 verlange bei fiktivem Hinweis reproduzierbare Triage, begrenzte Offenlegung und Fix; R5 Jahr 4 prüfe hypothetische Compliance-Anforderungen ohne Jurisdiktionsbehauptung; R6 Jahr 5 fordere unabhängige Wartungs- und Offenlegungspfade. Gegenargument: zu breite Vorabforderungen ersticken Community-Arbeit. Bei Evidenzmangel beantrage eng befristete Test- und Transparenzmaßnahme statt zu passen.

Öffentliche Ziele:
- Primär: nachprüfbare Sicherheits- und Rechte-Evidenz
- Sekundär: verhältnismäßiger Incident-/Compliance-Prozess

Machtmittel: simulierter Auditbericht, simulierter Disclosure-Kanal, hypothetisches Risikogate

## Rundenprotokoll

### Vorbereitung

- **Fakt:** Der zu prüfende Veröffentlichungsgegenstand ist harw/Harwness in frühem Entwicklungsstand 0.3.0; Cargo.toml:105 setzt publish=false für crates.io und :108 MIT OR Apache-2.0. LICENSE-MIT und LICENSE-APACHE sind vorhanden, aber das beweist NICHT Rechte an allen Inhalten oder Drittkomponenten. README.md:63-99 beschreibt Linux-Archive auf GitHub Releases und Quellinstallation; Verfügbarkeit und Funktionsfähigkeit konkreter Binaries sind hier nicht getestet. Governance: CONTRIBUTING.md, CODE_OF_CONDUCT.md und SECURITY.md existieren, ihre Wirksamkeit und Zuständigkeiten sind vor Veröffentlichung zu prüfen. TOML-Aktivierung von Kimi OCR ist in Arbeit, keine fertige Funktion. Alle folgenden Organisationen, Ressourcen, Jahre und Ereignisse sind Simulationsannahmen, keine identifizierten echten Nutzer, Firmen, Behörden, Marktanteile, Kosten oder Umsätze. Nach jedem Jahresfenster muss jeder Sitz genau eine prüfbare Maßnahme samt Pro und Contra für verschiedene mögliche Zustände vorschlagen; keine Wahrscheinlichkeit oder Zahl für Marktwachstum erfinden. Bei Blockade belege explizit, was fehlt, und beantrage einen begrenzten Ersatzschritt statt zu passen. Sicherheitsvorfall nur als Szenario-Injekt oder durch Engine-Spielzug, niemals als Tatsache behaupten. Qualitative Tracks sind ausschließlich Spielsituationsindikatoren, keine empirischen KPIs.

### Runde 1

- **Verzicht:** Hypothetischer Maintainer-/Community-Rat passt in der Phase Briefing.
- **Verzicht:** Hypothetischer Integratoren-/Dienstleisterverbund passt in der Phase Briefing.
- **Verzicht:** Hypothetischer Anwender-/Beschaffungsverbund passt in der Phase Briefing.
- **Verzicht:** Hypothetischer Security-/Compliance-Prüfverbund passt in der Phase Briefing.
- **Verzicht:** Hypothetischer Maintainer-/Community-Rat bringt kein Argument vor.
- **Entscheidung:** r1-a1: Forfeited
- **Verzicht:** Hypothetischer Integratoren-/Dienstleisterverbund bringt kein Argument vor.
- **Entscheidung:** r1-a2: Forfeited
- **Verzicht:** Hypothetischer Anwender-/Beschaffungsverbund bringt kein Argument vor.
- **Entscheidung:** r1-a3: Forfeited
- **Verzicht:** Hypothetischer Security-/Compliance-Prüfverbund bringt kein Argument vor.
- **Entscheidung:** r1-a4: Forfeited
- **Gegenargumente:** Keine Gegenargumente.
- **Gegenargumente:** Keine Gegenargumente.
- **Gegenargumente:** Keine Gegenargumente.
- **Gegenargumente:** Keine Gegenargumente.
- **Red Cell:** Red Cell: kein Einwand.

## Ergebnis

Spielende: Facilitator: Ende

| Akteur | Erfolge | Misserfolge | Verworfen/gepasst |
|---|---|---|---|
| Hypothetischer Maintainer-/Community-Rat | 0 | 0 | 1 |
| Hypothetischer Integratoren-/Dienstleisterverbund | 0 | 0 | 1 |
| Hypothetischer Anwender-/Beschaffungsverbund | 0 | 0 | 1 |
| Hypothetischer Security-/Compliance-Prüfverbund | 0 | 0 | 1 |

Öffentlicher Endzustand:

- Hypothetische Adoption/Evaluation: 0 [-3..3]
- Hypothetische Wartungsfähigkeit: 0 [-3..3]
- Governance im Spiel: informell
- Wartungs-/Übergabekapazität: 0/3
- Freigabestatus im Spiel: ungeprueft
- Rechte- und Abhängigkeitsprüfung: 0/2
- Hypothetische Sicherheitsevidenz: 0 [-3..3]
- Sicherheitsmeldung und Reproduktionsprozess: 0/2
- Hypothetisches institutionelles Vertrauen: 0 [-3..3]

## Nachbetrachtung (geplant · geschehen · warum · Lehren)

### Hypothetischer Maintainer-/Community-Rat

- **Geplant:** keine Angabe
- **Geschehen:** siehe Rundenprotokoll
- **Warum:** siehe Urteile und Würfe im Rundenprotokoll
- **Lehren:** kein Debrief

### Hypothetischer Integratoren-/Dienstleisterverbund

- **Geplant:** keine Angabe
- **Geschehen:** siehe Rundenprotokoll
- **Warum:** siehe Urteile und Würfe im Rundenprotokoll
- **Lehren:** kein Debrief

### Hypothetischer Anwender-/Beschaffungsverbund

- **Geplant:** keine Angabe
- **Geschehen:** siehe Rundenprotokoll
- **Warum:** siehe Urteile und Würfe im Rundenprotokoll
- **Lehren:** kein Debrief

### Hypothetischer Security-/Compliance-Prüfverbund

- **Geplant:** keine Angabe
- **Geschehen:** siehe Rundenprotokoll
- **Warum:** siehe Urteile und Würfe im Rundenprotokoll
- **Lehren:** kein Debrief

## Empfehlungen

Keine Umpire-Synthese verfügbar; Empfehlungen aus dem Rundenprotokoll ableiten.


## Offene Fragen

- 8 Zug/Züge wurden gepasst (Fehler, Zeitlimit oder ungültige Antwort) — Ergebnis entsprechend vorsichtig lesen.

## Methodik

- Alle Würfe stammen ausschließlich aus der deterministischen Engine (abgeleitete Seeds je Runde und Argument); kein Modell würfelt. Jeder Wurf steht mit Augen, Ziel und abgeleitetem Seed im Journal.
- Jeder Sitz ist ein eigener Kind-Agent und sieht nur seine Projektion des Journals (Regeln, eigenes Briefing, öffentliche Lage, an ihn adressierte Nachrichten).
- Der Lauf ist per Replay aus `journal.jsonl` ohne Modellaufrufe reproduzierbar (Würfel und Zustands-Hashes werden nachgerechnet).
