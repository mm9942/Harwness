# Harw OSS über fünf Jahre: manueller Sechs-Runden-Bericht

> **Methodisch überholt:** die hier selbstauferlegte Beschränkung höchstens ein vorbereitender Schritt pro Jahr ist NICHT Szenarioregel und erzeugte einen irreführend stagnierenden Verlauf; korrigierte manuelle Übung [hier](harw-oss-fuenfjahre-manuell-korrigiert.md).

> **Wichtiger Hinweis:** Dies ist die Auswertung einer sequenziellen, hypothetischen **MANUELLEN** Szenarioübung. Sie wurde nicht von einer Matrix-Engine erzeugt. Die Szenarioangaben sind keine Feststellungen über reale Nutzer, Unternehmen, Rechtslagen, Sicherheitsvorfälle, Umsätze oder Märkte.

## Kurzüberblick

Die Übung folgt sechs aufeinanderfolgenden Runden: R1 ist die Veröffentlichungssituation; R2 bis R6 entsprechen den hypothetischen Jahren 1 bis 5. In jeder Runde wurden vier Sitzvorschläge qualitativ gegeneinander abgewogen, anschließend setzte ein anderer UIA-Worker die Red Cell an und formulierte ein Stop-Kriterium. Eine manuelle Schiedsentscheidung hielt fest, welcher begrenzte Schritt — falls überhaupt — als hypothetisch vollzogen gelten durfte.

Der konservative Pfad führt zu keiner Freigabe, Veröffentlichung oder Produktivnutzung. Am Ende stehen die vier Tracks weiterhin bei 0; Rechte sind nicht nachgewiesen, Wartung ist lediglich durch einen Entwurf adressiert, der Meldeweg wurde nur als Tischübung weiter ausgearbeitet, Governance bleibt informell. Das Ergebnis ist kein Produkturteil und keine Prognose, sondern ein vorsichtiges, bedingtes Szenarioergebnis bei den hier gesetzten Informationslücken.

## Methodik und Geltungsbereich

Die sechs Runden wurden sequenziell und manuell behandelt. Pro Runde modellierte EIN UIA-Worker nacheinander alle vier hypothetischen Sitz-Perspektiven: Maintainer/Community, Integratoren, Beschaffung/Anwender und Security/Compliance. Jeder Vorschlag wurde knapp mit Maßnahme, Pro, Contra und einer Erwiderung aus einer anderen Sitzperspektive beschrieben. Danach modellierte EIN ANDERER UIA-Worker sowohl die Red Cell samt Stop-Kriterium als auch die manuelle qualitative Schiedsentscheidung; diese ist keine berechnete Abstimmung. Die Sitzperspektiven wurden also nicht von vier voneinander unabhängigen Agenten modelliert.

Es gab **keine Würfel, keine Engine, keine echten unabhängigen Sitzakteure und kein Replay**. Die Sitze, Ereignisse und Ressourcen sind hypothetisch. Die Rollenperspektiven strukturieren die Erörterung, sind aber weder echte Organisationen noch unabhängige externe Gutachten. Die Ergebnisse sind nicht statistisch, nicht empirisch und nicht als Prognose zu lesen.

Die unverändert zugrunde gelegte Szenariodatei ist `/home/mm29942/.harw/profiles/default/knowledge/matrix/harw-oss-fuenfjahre/20260924T222415-1/scenario.toml` (140 Zeilen). Dieser Bericht gibt ihren Entwurf nicht als unabhängig geprüfte Tatsachenbasis aus. Insbesondere nennt die Szenarioquelle `Cargo.toml` v0.3.0 mit `publish = false`, MIT OR Apache-2.0, vorhandene Lizenzdateien und eine README-Releasebeschreibung; weder Rechte sämtlicher Inhalte noch Release-Artefakte wurden hier unabhängig verifiziert. Kimi-OCR-TOML war in Arbeit, nicht zugesagt. Der vorherige Engine-Lauf `20260924T222415-1` endete nach 1/6 wegen `unknown child parent` als Pass-only. Er ist **weder Inhalt dieser manuellen Simulation noch empirisches Ergebnis** und wird nicht als Spielverlauf verwendet.

### Drei getrennte Aussageebenen

1. **Szenario-Ausgangsangaben:** Die oben genannten Dateiinhalte und Ausgangswerte sind Angaben des unveränderten Entwurfs, keine durch diese Übung verifizierten Befunde.
2. **Annahmen:** Sitzrollen, Ereignisse, Ressourcen, Injects und alle im Spiel beschriebenen Handlungen sind hypothetisch. Es werden keine realen Nutzer/Firmen, Gesetze, Umsätze oder Marktdaten behauptet.
3. **Manuelles Spielergebnis:** Die Rundenentscheidungen und Zustandsänderungen unten sind qualitative Schiedsergebnisse dieser Übung. Sie belegen keine tatsächliche Maßnahme oder Fähigkeit des Projekts.

### Zustandsmodell

Die Tracks sind T/W/A/S = Vertrauen / Wartung / Adoption / Sicherheits-Evidenz, jeweils auf der Skala −3 bis +3. Die Rechte-Skala reicht von 0 bis 2, der Meldeweg von 0 bis 2 und Wartung von 0 bis 3. Governance beginnt informell; der Release ist ungeprüft. Zahlen zeigen ausschließlich den hypothetischen Ledgerstand, nicht Messwerte. Ein unveränderter Wert bedeutet, dass die Runde keinen hinreichend belegten Grund für eine Änderung bot.

## Runde 1 — Veröffentlichungssituation

**Ausgangslage:** Baseline vor den hypothetischen Schritten: T/W/A/S = 0/0/0/0; Rechte 0/2, Meldeweg 0/2, Wartung 0/3; Governance informell, Release ungeprüft.

| Sitz | Vorgeschlagene Maßnahme | Pro | Contra | Erwiderung |
|---|---|---|---|---|
| Maintainer/Community | Rechteinventar für Code und Artefakte erstellen und Security-Checkstatus sichtbar machen. | Macht Vorbedingungen und Lücken sichtbar. | Kostet Zeit und kann Veröffentlichung verzögern. | Integratoren: Ein Pilot ersetzt weder Rechteklärung noch Security-Prüfung. |
| Integratoren | Beaufsichtigten Linux-Installations- und Kompatibilitätspilot protokollieren. | Könnte Installationsprobleme früh sichtbar machen. | Geringe Abdeckung; kein belastbarer Nachweis allgemeiner Kompatibilität. | Beschaffung: Dokumentation ist kein Test. |
| Beschaffung/Anwender | Rechteunterlagen als Pilot-Gate anfordern. | Verbessert Nachvollziehbarkeit vor Einsatz. | Bremst einen frühen Pilot. | Maintainer: Ein Inventar allein ist noch kein Beschaffungsnachweis. |
| Security/Compliance | Lizenz, Berechtigungen und Meldeweg prüfen; Lücken dokumentieren. | Macht offene Prüfpunkte explizit. | Dokumentation schafft noch keinen wirksamen Meldeprozess. | Integratoren: Eine Installationserprobung ist kein Security-Nachweis. |

**Red-Cell-Stop-Kriterium:** Keine Veröffentlichung ohne Bestands-Rechtenachweis, Security-Prüfung und validierten Meldeweg.

**Manuelle Schiedsentscheidung:** Maintainer und Beschaffung bereiten die Nachweise vor; Integrator- und Assurance-Aktivitäten bleiben beschränkte Simulationen. Als einziger hypothetisch vollzogener Schritt wird der Meldeweg in einer Tischübung dokumentiert: Meldeweg 0→1. Keine Veröffentlichung.

**Ledger nach R1:** T/W/A/S 0/0/0/0; Rechte 0/2; Meldeweg 1/2 (Tischübung); Wartung 0/3; Governance informell; Release ungeprüft und gesperrt.

## Runde 2 — Jahr 1

| Sitz | Vorgeschlagene Maßnahme | Pro | Contra | Erwiderung |
|---|---|---|---|---|
| Maintainer/Community | Review- und Issue-Triage-Beitragsregeln samt Beispiel entwerfen. | Klärt erwartete Zuständigkeiten. | Ein Dokument ist noch keine gelebte Praxis. | Integratoren: Für einen Rückfall braucht es einen konkreten Auslöser. |
| Integratoren | Versionierte Rückfallanleitung für Sandbox und Zweitprüfung erstellen. | Macht einen begrenzten Exit-Pfad beschreibbar. | Gilt nicht für Produktion und ist noch nicht erprobt. | Beschaffung: Der Exit muss in nachvollziehbaren Schritten beschrieben sein. |
| Beschaffung/Anwender | Sicherheitsgrenzen und Exit lokal in einer Sandbox prüfen. | Begrenzt den Umfang der Betrachtung. | Schließt Rechtefragen nicht. | Assurance: Den Testumfang ausdrücklich abgrenzen. |
| Security/Compliance | Security-Triage-Schema und Sandbox-Protokoll skizzieren. | Unterstützt geordnete Priorisierung. | Ist keine Freigabe und kein Sicherheitsbefund. | Maintainer: Security-Triage muss von allgemeiner Beitrags-Triage unterscheidbar bleiben. |

**Red-Cell-Stop-Kriterium:** Kein Weitergehen ohne Rechteklärung und Zweitprüfung des Rückfalls.

**Manuelle Schiedsentscheidung:** Nur ein hypothetischer Rückfallanleitungs-**Entwurf** wird versioniert. Wartung steigt 0→1/3 ausschließlich als Entwurfsstatus; Tracks bleiben unverändert. Keine Freigabe.

**Ledger nach R2:** T/W/A/S 0/0/0/0; Rechte 0/2; Meldeweg 1/2; Wartung 1/3 (Entwurf); Governance informell; Release ungeprüft und gesperrt.

## Runde 3 — Jahr 2

**Inject `hypothetical-triage` (genauer Inhalt):** „Annahme, keine reale Meldung: Nach dem zweiten Betriebsjahr häufen sich nicht reproduzierte Berichte über unerwartete Berechtigungsanfragen. Prüft konservative Triage, Test und Stop/Go; keine Schwachstelle als erwiesen ausgeben.“

Da die Veröffentlichung weiterhin gesperrt ist, werden die fiktiven Berichte einer isolierten Evaluation zugeordnet, nicht einem realen Betrieb.

| Sitz | Vorgeschlagene Maßnahme | Pro | Contra | Erwiderung |
|---|---|---|---|---|
| Maintainer/Community | Kapazitätsplan mit Vertretung und Rückfallübung entwerfen. | Macht mögliche Engpässe sichtbar. | Bindet knappe Zeit; ist kein Ersatz für Tests. | Assurance: Triage braucht einen Rückfallpfad. |
| Integratoren | Befristetes, nicht exklusives Review von Berechtigungsdokumentation und Testfällen. | Ergänzt die Sicht auf Unterlagen und Prüfdesign. | Schafft Abhängigkeit und keine unabhängige Freigabe. | Beschaffung: Kein Ersatz für einen unabhängigen Vergleich. |
| Beschaffung/Anwender | Kriteriensymmetrischen Vergleich von mindestens zwei Alternativen vorbereiten. | Hält Exit-Optionen offen. | Kriterien sind zunächst selbst ungetestet. | Integratoren: Vergleichskriterien müssen unabhängig formuliert werden. |
| Security/Compliance | Abhängigkeiten und Herkunft sowie nicht reproduzierte Meldungen mit Reproduktions- und Stop-Kriterien dokumentieren. | Ermöglicht konservative Triage ohne voreilige Behauptung. | Klärt weder Ursache noch Reproduzierbarkeit. | Maintainer: Ein Kapazitätsplan ersetzt keine Tests. |

**Red-Cell-Stop-Kriterium:** Eine hypothetische unerwartete Rechteanfrage liegt außerhalb des bestätigten Scopes; bei Reproduktion Evaluation stoppen.

**Manuelle Schiedsentscheidung:** Nur Assurance erfasst Meldungen, Stop-Kriterien und Abhängigkeiten als hypothetischen Teilschritt. Kein Bug wird bestätigt; keine Track- oder Gateänderung, keine Freigabe.

**Ledger nach R3:** T/W/A/S 0/0/0/0; Rechte 0/2; Meldeweg 1/2; Wartung 1/3 (Entwurf); Governance informell; Release ungeprüft und gesperrt.

## Runde 4 — Jahr 3

| Sitz | Vorgeschlagene Maßnahme | Pro | Contra | Erwiderung |
|---|---|---|---|---|
| Maintainer/Community | Fiktive Incident-/Disclosure-Tischübung mit Zuständigkeit, Eskalation und Rückmeldung durchführen. | Macht einen möglichen Ablauf nachvollziehbar. | Ist kein Ernstfall und belegt keine Einsatzfähigkeit. | Assurance: Ein Prozess ergänzt die Triage, ersetzt sie aber nicht. |
| Integratoren | Stop- und Prüfablauf für einen künftigen Fix entwerfen. | Könnte spätere Rolloutkontrolle strukturieren. | Es existiert kein Fix, den man prüfen könnte. | Beschaffung: Ein Stop-Ablauf ersetzt keinen wirksamen Fix. |
| Beschaffung/Anwender | Pause-/Wiederaufnahme-Gate für einen künftigen Pilot formulieren. | Begrenzte Risiken bei späteren Erprobungen. | Kann Verzögerungen erzeugen. | Integratoren: Ein Stop allein reicht ohne Prüfschritte nicht aus. |
| Security/Compliance | Fiktiven Hinweis auf Reproduzierbarkeit und Offenlegungsgrenze durchspielen. | Macht Triageentscheidungen prüfbar. | Es gibt keinen bestätigten Bug. | Maintainer: Dafür braucht es vorher klare Kriterien. |

**Red-Cell-Stop-Kriterium:** Kein Weitergehen bei Druck zu vorschneller Offenlegung oder ungeprüftem Fix; ohne unabhängige Reproduktion stoppen.

**Manuelle Schiedsentscheidung:** Assurance spielt nur einen fiktiven Hinweis bis zum Stop als Tischübung durch. Meldeweg 1→2/2 **nur simuliert**. Sicherheits-Evidenz bleibt 0; kein realer Fix und kein Release.

**Ledger nach R4:** T/W/A/S 0/0/0/0; Rechte 0/2; Meldeweg 2/2 (nur Tischübung); Wartung 1/3 (Entwurf); Governance informell; Release ungeprüft und gesperrt.

## Runde 5 — Jahr 4

**Inject `hypothetical-compliance` (genauer Inhalt):** „Annahme, keine tatsächliche Gesetzesänderung: Ein fiktiver Beschaffungsverbund verlangt nun dokumentierte Datenflüsse, Verantwortliche und Export-/Exit-Möglichkeit. Prüft Governance gegen knappe Kapazität.“

| Sitz | Vorgeschlagene Maßnahme | Pro | Contra | Erwiderung |
|---|---|---|---|---|
| Maintainer/Community | Übergabeprotokoll mit Verantwortlichen, Vertretung und offenen Aufgaben erstellen. | Erhöht Sichtbarkeit möglicher Zuständigkeiten. | Ist kein echter Ersatz für verfügbare Verantwortliche. | Assurance: Prüffähige Zuständigkeit ist kein Sicherheitsnachweis. |
| Integratoren | Exportformat spezifizieren und synthetische Daten testen. | Eröffnet eine konkrete Portabilitätsfrage. | Belegt keinen realen Exit. | Beschaffung: Eine Spezifikation belegt weder Praxis noch Überlegenheit. |
| Beschaffung/Anwender | Neutrale Anforderungs- und Quellenmatrix mit offenen Feldern anlegen. | Macht Nachweislücken sichtbar. | Bleibt vorläufig und unvollständig. | Integratoren: Ein Format allein ist keine Compliance. |
| Security/Compliance | Prüfspur hypothetischer Datenflüsse und ungeprüften Status dokumentieren. | Vermeidet Scheinsicherheit. | Beansprucht knappe Kapazität und verifiziert keine Datenflüsse. | Maintainer: Ein Übergabedokument ist noch keine Verifikation. |

**Red-Cell-Stop-Kriterium:** Stop bei unbelegten Behauptungen, etwas sei „geklärt“, bei Realdata oder bei behaupteter Compliance ohne Nachweis.

**Manuelle Schiedsentscheidung:** Nur der Entwurf einer neutralen Beschaffungsmatrix wird hypothetisch angelegt. Unbelegte Felder bleiben offen; keine Track- oder Gateänderung. Governance bleibt informell, Release gesperrt.

**Ledger nach R5:** T/W/A/S 0/0/0/0; Rechte 0/2; Meldeweg 2/2 (nur Tischübung); Wartung 1/3 (Entwurf); Governance informell; Beschaffungsmatrix Entwurf; Release ungeprüft und gesperrt.

## Runde 6 — Jahr 5

**Inject `hypothetical-competition` (genauer Inhalt):** „Annahme, kein belegter Anbieter oder Marktanteil: Eine alternative offene Agentenlösung bietet leichteren Einstieg; zugleich wird unabhängige Security-Evidenz wichtiger. Entscheidet Fokus, Interoperabilität oder bewusste Nische.“

| Sitz | Vorgeschlagene Maßnahme | Pro | Contra | Erwiderung |
|---|---|---|---|---|
| Maintainer/Community | Versionierungs- und Abkündigungsregeln mit Fristen entwerfen. | Kann Planbarkeit unterstützen. | Pflege und Einhaltung beanspruchen knappe Kapazität. | Integratoren: Support braucht solche Regeln, nicht nur spontane Zusagen. |
| Integratoren | Offenen Supportvertrag mit einer vorab zu testenden Zusage entwerfen. | Macht ein Versprechen prüfbar. | Belegt keine dauerhafte Leistung. | Maintainer: Regeln allein sind noch kein funktionierender Prozess. |
| Beschaffung/Anwender | Evidenzvergleich für offene Lösung, Dienstleister, Fork und Nichtnutzung vorbereiten. | Macht offene Vergleichsfelder sichtbar. | Ohne Nachweise unvollständig. | Assurance: Ein Vergleich schafft selbst keinen Prüfpfad. |
| Security/Compliance | Prüfplan unabhängiger Wartungs- und Disclosure-Pfade mit benannter Stelle und Nachweisart erstellen. | Legt Evidenzlücken offen. | Ein Plan ist keine durchgeführte Prüfung. | Beschaffung: Prüfpfade müssen durch Belege getragen sein. |

**Red-Cell-Stop-Kriterium:** Keine Freigabe oder positive Beschaffungswertung ohne definierte, unabhängig überprüfbare Zusage und unabhängigen Prüfpfad.

**Manuelle Schiedsentscheidung:** Kein Schritt gilt als wirksam abgeschlossen. Der Endzustand bleibt konservativ; weder eine Wettbewerbsentscheidung noch eine positive oder negative Marktprognose wird abgeleitet.

**Endledger nach R6:** T/W/A/S = 0/0/0/0; Rechte 0/2; Meldeweg 2/2 (nur Tischübung); Wartung 1/3 (nur Entwurf); Governance informell; Release ungeprüft; Veröffentlichung und Produktivnutzung gesperrt; Beschaffungsmatrix Entwurf.

## Gespielter Pfad und konditionierte Alternativen

### Gespielter konservativer Pfad

Die Runde hält bei fehlendem Rechtebestand, ungeprüften Artefakten und fehlender unabhängiger Security-Evidenz an. Es entstehen in der hypothetischen Übung nur begrenzte Dokumentationsartefakte: ein Rückfallanleitungsentwurf, simulierte Meldeweg-Tischübungen, dokumentierte hypothetische Triagekriterien und eine neutrale Beschaffungsmatrix als Entwurf. Das begründet weder tatsächliche Einsatzbereitschaft noch Freigabe. Die Tracks bleiben neutral, weil die beschriebenen Vorbereitungen keine belastbare Veränderung von Vertrauen, Wartung im Betrieb, Adoption oder Sicherheits-Evidenz belegen.

### Günstiger, ausdrücklich bedingter Alternativpfad — nicht eingetreten

Nur falls Rechte tatsächlich vollständig belegt, Artefakte und Security unabhängig geprüft, eine tragfähige Wartung nachgewiesen und ein Pilot mit wirksamem Stop-/Exit-Pfad durchgeführt würden, wäre eine **gestufte Freigabe erneut prüfbar**. Gates: (1) belastbarer Rechte- und Artefaktnachweis, (2) nachvollziehbare Security-Prüfung mit unabhängiger Evidenz, (3) benannte und tragfähige Wartungsverantwortung, (4) begrenzter Pilot samt getesteter Stop-, Rückfall- und Exit-Möglichkeit. Keines dieser Gates wird durch den Alternativpfad als bereits erfüllt behauptet.

### Ungünstiger, ausdrücklich bedingter Alternativpfad — nicht eingetreten

Falls Rechte-, Security- oder Kapazitätslücken offen bleiben, der Exit unverifizierbar ist oder voreilige Zusagen an die Stelle von Nachweisen treten, bleibt eine Freigabe aus. Mögliche qualitative Folge wäre Nichtnutzung und gegebenenfalls ein Vergleich mit Alternativen anhand offener Kriterien. Das ist keine Aussage, dass eine Alternative verfügbar oder überlegen sei; eine quantitative Marktprognose wird nicht erstellt.

## After Action Review (AAR)

- Die sequenzielle Struktur macht sichtbar, wie leicht ein Plan, ein Protokoll oder eine Tischübung mit tatsächlicher Betriebsfähigkeit verwechselt werden könnte. Der Bericht trennt deshalb Entwurf, Simulation und Nachweis.
- Die Red-Cell-Stopps hielten die hypothetische Übung bei fehlenden Grundlagen an. Der Meldeweg erreicht im Ledger zwar 2/2, aber ausschließlich als Tischübung; daraus folgt keine operative Validierung.
- Wartung 1/3 bezeichnet nur einen Entwurf, keine verfügbare Kapazität oder verlässliche Wartungsleistung. Die unveränderten Tracks vermeiden eine unbelegte Erfolgsannahme.
- Prozedurdokumentation ohne unabhängige Durchführung genügt nicht. Reale Rechte-, Artefakt-, Security-, Wartungs- und Exit-Prüfungen wären erforderlich, bevor eine reale Entscheidung über Einsatz oder Beschaffung getroffen werden könnte.

## Offene Nachweise

1. Vollständiger Rechte- und Herkunftsnachweis für Code, Inhalte und Artefakte.
2. Tatsächliche Release-Artefakte und reproduzierbare, unabhängige Prüfungen ihres Inhalts.
3. Security-Prüfung mit nachvollziehbarem Scope, Reproduzierbarkeit, Zuständigkeiten und belastbarem Disclosure-Pfad.
4. Nachweis einer tragfähigen Wartungs- und Vertretungskapazität statt eines bloßen Entwurfs.
5. Praktisch erprobter, unabhängiger Rückfall-/Exit-Pfad und begrenzte Pilot-Evidenz.
6. Verifizierte Datenflüsse und Verantwortlichkeiten, falls für eine konkrete Beschaffung erforderlich.
7. Jede spätere OCR- oder TOML-Aufbereitung wäre gesondert zu prüfen; Kimi-OCR-TOML war in Arbeit, nicht zugesagt.

## Einschränkungen

Dieser Bericht ist ein manuelles hypothetisches Szenario, keine technische Prüfung, Rechtsberatung, Sicherheitsbewertung, Beschaffungsfreigabe oder Aussage über Marktverhältnisse. Die Ausgangsangaben der Szenarioquelle wurden für diesen Bericht nicht unabhängig verifiziert. Rollen, Vorschläge, Ressourcen, Injects, Tischübungen und Zustandsänderungen sind keine realen Vorgänge. Insbesondere sind weder die nicht reproduzierten Hinweise tatsächliche Meldungen noch eine Schwachstelle, ein Anbieter, ein Marktanteil oder eine Gesetzesänderung belegt. Die Red-Cell-Kriterien und Schiedssprüche sind qualitative Entscheidungen dieser Übung und keine Engine-Ausgabe.
