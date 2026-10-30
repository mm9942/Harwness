# Harw OSS: fünf Jahre — korrigierter manueller Spielbericht

> **Manuelle qualitative Übung.** Kein Matrix-Engine-Ergebnis, keine Würfel/2W6 und kein Replay. Alle Spielhandlungen, Sitzrollen und Inject-Ereignisse sind hypothetisch; Zahlen sind qualitative Schiedswerte, keine Messungen oder realen Projektergebnisse.

## Zweck und Einordnung

Dieser Bericht ersetzt als korrigierte manuelle Übung den methodisch überholten stagnierenden Pfad in [der älteren manuellen Fassung](harw-oss-fuenfjahre-manuell-report.md). Die sechs Runden sind Veröffentlichungssituation (R1) und hypothetische Jahre 1–5 (R2–R6). Es bleibt die Szenarioregel erhalten, dass sich jeder Track je Runde höchstens um ±1 verändert. Entfernt wird ausschließlich die künstliche Beschränkung, pro Jahr höchstens einen vorbereitenden Projektschritt zuzulassen: mehrere begrenzte Maßnahmen dürfen in einer Runde hypothetisch stattfinden.

## Methodik, Fakten und Grenzen

Pro Runde modellierte ein UIA-Worker nacheinander die vier hypothetischen Perspektiven (Maintainer, Integration, Beschaffung/Anwendung, Assurance/Sicherheit). Ein anderer UIA-Worker übernahm Red Cell und manuellen Schied. Zusätzliche Worker korrigierten Inkonsistenzen in den Track-Zuordnungen. Es gab keine unabhängigen echten Sitzmenschen. Mia ist laut Nutzerin die einzige menschliche Maintainerin; AI-Tools und Bots zählen nicht automatisch als menschliche Teammitglieder. Die übrigen Sitze sind hypothetische Perspektiven, keine echten Kunden oder Partner. Daraus folgt keine Behauptung echter alleiniger Git-Urheberschaft.

Als Baseline sind folgende Repository-Angaben gesetzt: `git log -n25` enthält am 2026-09-24 die Commits `26c9776 Prepare repository for public release`, `e283707 CI structure gates`, `f90e01f remote OCR konfigurierbar` sowie den neuesten Merge `4b8c3de`. Autorzeichenfolgen wie Claude oder dependabot sind kein Beleg für ein menschliches Team. `README.md:1-7` beschreibt frühe 0.3.x-Versionen als täglich benutzt; `README.md:63-80` enthält eine Release-Anleitung. `Cargo.toml:100-109` nennt Version 0.3.0, `publish = false` und `MIT OR Apache-2.0`. Diese Metadaten verifizieren weder Rechte aller Inhalte/Abhängigkeiten noch ein konkretes Release-Archiv, bestandene Security-Tests oder echte Kundschaft. Kimi-OCR-TOML ist laut Nutzerin in Arbeit.

Der gespeicherte Szenario-Entwurf blieb unverändert: Profilpfad `.../harw-oss-fuenfjahre/20260924T222415-1/scenario.toml` (Kopie vom 2026-09-24; kein heutiges Erstellungsdatum wird behauptet). Der ursprüngliche Engine-Lauf endete nach 1/6 mit `unknown child parent` als Pass-only; er ist nicht dieser Bericht und liefert keine Spielzüge.

**Werte im Spiel sind eng konditionierte Annahmen**, keine realen Rechteprüfungen, Assurance, Sicherheitsprüfungen oder rechtlichen Befunde. Gates: Rechte 0/2, Meldeweg 0/2, Wartungskapazität 0/3; Governance zu Beginn informell. Das Wartungskapazitäts-Gate ist nicht der Wartbarkeitstrack W. Die vier Tracks sind genau T Vertrauen, W Wartbarkeit, A Adoption/EVALUATION und S Sicherheitsevidenz, jeweils −3 bis +3. Pro Runde gilt je Track maximal ±1.

## Sechs Runden

### R1 — Veröffentlichungssituation

| Sitz | Maßnahme; Pro / Contra | Kreuz-Gegenargument |
|---|---|---|
| Mia / Maintainer | Scoped Rechte-, Asset- und Abhängigkeitsinventar, Security-Check/Checksums und GitHub-Kandidat; Pro: konkrete Freigabegrenzen; Contra: Aufwand und verbleibende Lücken. | Integration: Installationserfolg beweist keine Rechte. |
| Integration | Frische isolierte Linux-Installation samt Rollback testen; Pro: reproduzierbarer Installationspfad; Contra: kein Audit. | Assurance: CI ist kein unabhängiges Audit. |
| Beschaffung | Begrenztes Rechte-/Exit-Pilotgate; Pro: Scope begrenzt; Contra: kein breiter Einsatz. | Mia: Dokumentation ist kein Nutzertest. |
| Assurance | Lizenz-/Berechtigungsmatrix und reproduzierbaren Meldeweg; Pro: Prüfpunkte auffindbar; Contra: Artefakte ersetzen keine externe Prüfung. | Integration: Checksums belegen nicht die Rechtmäßigkeit. |

**Red Cell / Stop-Resume:** Stop, falls eine enthaltene Datei unklare Herkunft/Rechte hat, Installation oder Rollback scheitert oder der Meldeprozess versagt. Wiederaufnahme erst nach Entfernung/Klärung problematischer Inhalte und bestandenem begrenztem Test samt Meldeweg.

**Manueller Schied:** Als ausdrücklich konditionierte Spielannahme sind Vorab-Prüfartefakte erstellt, problematische Teile entfernt, Rechte 0→2/2, Meldeweg 0→2/2, der isolierte Linux-Kandidat geprüft. Ein gestufter GitHub-Release und isolierter Pilot starten fiktiv; **nicht crates.io**. Scheitert ein Vorab-Gate, wird in diesem Alternativzweig nicht veröffentlicht. Tracks 0/0/0/0→1/1/1/0: S bleibt 0, da kein unabhängiger Sicherheitsnachweis vorliegt. Wartungskapazität 0→1/3 (erste definierte Verantwortung); Governance informell.

### R2 — Jahr 1

| Sitz | Maßnahme; Pro / Contra | Kreuz-Gegenargument |
|---|---|---|
| Mia / Maintainer | Begrenzte Issue-/Review-Triage; Pro: Priorisierung; Contra: Solo-Last. | Assurance: Triage braucht Rückfallregeln. |
| Integration | Versionierte Linux-Installations-/Rollback-Anleitung mit frischem Test; Pro: Exit wird praktisch greifbar; Contra: begrenzter Scope. | Beschaffung: getesteter Rückfall ersetzt keine Security-Prüfung. |
| Beschaffung | Lokale Berechtigungs-, Daten- und Netzgrenzen plus Exit in beaufsichtigter Evaluation; Pro: Einsatzgrenzen sichtbar; Contra: keine breite Freigabe. | Mia: beaufsichtigt ist nicht unabhängig. |
| Assurance | Fiktiven Fehler in Sandbox, Meldung und Regression durchspielen; Pro: Ablauf wird prüfbar; Contra: kein Audit. | Integration: Regressionstest beweist keine allgemeine Fehlerfreiheit. |

**Red Cell / Stop-Resume:** Kein Rollout außerhalb des begrenzten Scopes; bei nicht beherrschbarem reproduzierbarem Fehler stoppen. Resume nur innerhalb des Scopes nach dokumentierter Triage und funktionierendem Rückfall.

**Manueller Schied:** Alle vier scoped Übungen gelten im Spiel als durchgeführt. T/W/A/S 1/1/1/0→2/2/2/1; Wartung 1→2/3. Rechte und Meldeprozess scoped 2/2. Keine breite Adoption und keine echte unabhängige Prüfung.

### R3 — Jahr 2 — Inject `hypothetical-triage`

**Inject:** Nicht reproduzierte Meldungen über unerwartete Berechtigungsanfragen; später wird im isolierten Pilot ein einzelner begrenzter Berechtigungsfehler reproduziert — ausschließlich als Spielereignis, kein echter Bug.

| Sitz | Maßnahme; Pro / Contra | Kreuz-Gegenargument |
|---|---|---|
| Mia / Maintainer | Getesteten Rückfall und Solo-Kapazitätsplan dokumentieren; Pro: Folgen begrenzt; Contra: keine langfristige Vertretung. | Integration: Kapazitätsplan ist kein tatsächlicher Helfer. |
| Integration | Hypothetisches, nicht exklusives Doku-/Testreview; Pro: zusätzliche Prüfperspektive; Contra: begrenzt und kein dauerhafter Helfer. | Mia: Review ≠ tatsächliche fortlaufende Unterstützung. |
| Beschaffung | Objektiver Alternativenvergleich und Exit-Schwelle; Pro: Entscheidung bleibt offen; Contra: bindet knappe Zeit. | Assurance: Kriterien selbst benötigen Belege. |
| Assurance | Abhängigkeitsherkunft und reproduzierbare Triage; Pro: Ursache eingrenzbar; Contra: erst Reproduktion klärt den Befund. | Mia: Herkunftserfassung ersetzt keinen Rückfalltest. |

**Red Cell / Stop-Resume:** Bei Reproduktion Pilot und neue Release-Verteilung pausieren; vorhandene Archive sind nicht rückholbar. Resume nur mit reproduzierbar getesteten Fix, eingegrenzten Folgen und Hinweis für bereits heruntergeladene Archive.

**Manueller Schied:** Nach Reproduktion gilt die Pause. T/W/A/S 2/2/2/1→1/3/1/2: Vertrauen und aktuelle Evaluation leiden, W steigt durch den getesteten Rückfall, S steigt als Sicherheitsevidenz der Reproduktion — nicht als behauptete Sicherheit. Wartungskapazität bleibt 2/3, langfristige Vertretung ungeklärt; Rechte/Meldeweg scoped 2/2. Korrekte Spur: **A1**, nicht A3; ein früherer fehlerhafter Eintrag vermischte einen fünften „Assurance“-Track. Es gibt nur die vier genannten Tracks.

### R4 — Jahr 3

| Sitz | Maßnahme; Pro / Contra | Kreuz-Gegenargument |
|---|---|---|
| Mia / Maintainer | Disclosure-/Incident-/Patch-Diff-Ablauf; Pro: Zuständigkeiten und Änderung sichtbar; Contra: Prozess ist kein Fix. | Assurance: Prozess ersetzt keine wirksame Korrektur. |
| Integration | Reproduzierbare Testumgebung und Rollout-Stopp; Pro: Fix kann begrenzt geprüft werden; Contra: Test deckt nicht alle Fehler ab. | Beschaffung: Fix-Test ≠ allgemeine Fehlerfreiheit. |
| Beschaffung | Pilot bis Fix-Verifikation pausieren; Pro: begrenzt erneutes Risiko; Contra: Verzögerung, Archive bleiben bestehen. | Mia: Pause holt vorhandene Archive nicht zurück. |
| Assurance | Vorher-/Nachher-Regression und begrenzte Offenlegung; Pro: Änderung nachvollziehbar; Contra: keine umfassende Prüfung. | Integration: Regression ≠ unabhängiges Audit. |

**Red Cell / Stop-Resume:** Kein Weiterverteilen vor reproduzierbarem Patch-Test, Rechteprüfung des Diffs, Rollback und begrenztem Hinweis. Resume erst nach Bestehen aller Gates.

**Manueller Schied:** Rein hypothetisch gelingt ein begrenzter Fix mit automatischer Regression; Patch-Diff-Rechte werden geprüft, ein Hinweis für bestehende Archive erstellt, gepatchte GitHub-Version und begrenzter Pilot folgen erst danach. T/W/A/S 1/3/1/2→2/3/2/3. Rechte/Meldeweg scoped 2/2, Wartungskapazität 2/3. Keine reale Sicherheitslücke, kein Audit und keine reale Veröffentlichung werden behauptet.

### R5 — Jahr 4 — Inject `hypothetical-compliance`

**Inject:** Ein fiktiver Beschaffungsverbund fordert Datenflüsse, Verantwortliche und Export-/Exit-Angaben. Keine tatsächliche Gesetzesänderung.

| Sitz | Maßnahme; Pro / Contra | Kreuz-Gegenargument |
|---|---|---|
| Mia / Maintainer | Überlast-/Übergabe-Runbook und Stopps dokumentieren; Pro: Grenzen sichtbar; Contra: schafft keine reale Vertretung. | Assurance: benannte Dokumentation ist keine verfügbare Kapazität. |
| Integration | Kleines Exportformat plus synthetischen Roundtrip; Pro: Formatpfad erprobt; Contra: synthetisch ≠ reale Migration. | Beschaffung: Test ist kein Compliance-Nachweis. |
| Beschaffung | Quellen-/Anforderungsmatrix mit offenen Feldern; Pro: Lücken transparent; Contra: keine vollständige Antwort. | Mia: offene Felder bleiben ausdrücklich offen. |
| Assurance | Versionierte Datenflüsse Quelle/Ziel/Zweck mit ungeprüftem Status; Pro: Prüfpfad; Contra: nicht verifiziert. | Integration: Diagramm ≠ reales Datenfluss-Audit. |

**Red Cell / Stop-Resume:** Keine Compliancebehauptung ohne Quellen, Verantwortliche und Verifikation. Resume einer konkreten Behauptung erst nach geschlossenen, nachvollziehbaren Nachweisen.

**Manueller Schied:** Im Spiel werden vier enge Artefakte fertig; Governance wechselt von informell zu dokumentiert, nicht gemeinschaftlich. T/W/A/S bleibt 2/3/2/3; Wartungskapazität 2/3. Offene Matrix- und Datenfelder bleiben offen. Keine echten Kunden und kein reales Gesetz.

### R6 — Jahr 5 — Inject `hypothetical-competition`

**Inject:** Angenommene alternative offene Agentenlösung mit leichterem Einstieg; unabhängige Sicherheitsevidenz wird wichtiger. Kein belegter Anbieter und kein Marktanteil.

| Sitz | Maßnahme; Pro / Contra | Kreuz-Gegenargument |
|---|---|---|
| Mia / Maintainer | Enges Versionierungs-/Abkündigungs-/Interoperabilitätsprogramm mit realistischen Solo-Fristen; Pro: planbarer Exit; Contra: Solo-Ausfallrisiko. | Integration: Regeln ersetzen keine Supportkapazität. |
| Integration | Nur einen Schnittstellenpfad vor begrenzter Zusage reproduzierbar testen; Pro: konkrete Interoperabilität; Contra: kein echter Supportvertrag. | Mia: ein Pfad ist keine breite Kompatibilitätszusage. |
| Beschaffung | Objektiver Vergleich Eigenbetrieb/Dienstleister/Fork/Nichtnutzung; Pro: Optionen transparent; Contra: Optionen sind nicht real vergeben. | Assurance: Vergleich ersetzt unabhängige Prüfung nicht. |
| Assurance | Unabhängige Prüfpfade als offene Lücke ausweisen; Pro: keine Scheinsicherheit; Contra: kein externer Auditor. | Beschaffung: Prüfplan ist kein Prüfbericht. |

**Red Cell / Stop-Resume:** Stop bei nicht reproduzierbarer Interoperabilität, versagendem Exit oder fehlender Zuständigkeit; keine unbegrenzte Supportzusage. Resume nur für den getesteten Pfad mit dokumentiertem Exit und klarer Begrenzung.

**Manueller Schied:** Im Spiel entstehen eine dokumentierte Richtlinie, ein getesteter Pfad und ein Exit-Vergleich. T/W/A/S 2/3/2/3→2/3/3/3. A3 bedeutet vertiefte Evaluation eines einzigen Pfads, keinen Marktanteil und keine weite Adoption. Rechte/Meldeweg scoped 2/2; Wartungskapazität 2/3; Governance dokumentiert. Ein eng gestufter GitHub-Release gilt nur im Spiel. Hohe Scores bedeuten keinen automatischen institutionellen Erfolg.

## Ledger

| Stand | T | W | A (Evaluation) | S | Rechte-Gate | Meldeweg-Gate | Wartungskapazität-Gate | Governance | Spielstatus |
|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| Ausgang | 0 | 0 | 0 | 0 | 0/2 | 0/2 | 0/3 | informell | ungeprüft |
| R1 Veröffentlichung | 1 | 1 | 1 | 0 | 2/2 | 2/2 | 1/3 | informell | gestuft, hypothetisch |
| R2 Jahr 1 | 2 | 2 | 2 | 1 | 2/2 | 2/2 | 2/3 | informell | scoped Übungen |
| R3 Jahr 2 | 1 | 3 | 1 | 2 | 2/2 | 2/2 | 2/3 | informell | Pilot/Neuverteilung pausiert |
| R4 Jahr 3 | 2 | 3 | 2 | 3 | 2/2 | 2/2 | 2/3 | informell | Fix, begrenztes Resume |
| R5 Jahr 4 | 2 | 3 | 2 | 3 | 2/2 | 2/2 | 2/3 | dokumentiert | enge Nachweisartefakte |
| R6 Jahr 5 | 2 | 3 | 3 | 3 | 2/2 | 2/2 | 2/3 | dokumentiert | eng gestufter GitHub-Pfad |

Die Gates und Spielstatus sind hypothetische, scoped Befunde der Übung; sie dürfen nicht als reale Prüfung oder Freigabe zitiert werden. W ist Wartbarkeit als Track, nicht das separate Wartungskapazitäts-Gate.

## Ergebnis, Abzweigungen und AAR

Der gespielte qualitative Pfad ist ein erfolgreicher, enger und bedingter OSS-Releasepfad: unter Vorab-Gates gestartet, durch ein hypothetisch reproduziertes Problem unterbrochen und nach begrenztem Patch wieder aufgenommen. Zugleich bleiben Solo-Wartung und unabhängige Assurance ungelöste Lücken. Das ist keine Aussage, dass reale Release-Gates bestanden wurden oder tatsächlich fünf Jahre vergangen sind.

Zwei ausdrücklich konditionierte Abzweigungen: (1) Scheitert ein Vorab-Gate, wird Veröffentlichung verschoben; Produkt- und CI-Arbeit können dennoch weitergehen. (2) Gelingt langfristige menschliche/organisatorische Absicherung und echte externe Prüfung, wird ein breiterer prüfbarer Pfad möglich, aber nicht garantiert.

AAR: Die Git-Baseline zeigt schnellen Fortschritt unter Mias Leitung, nicht automatisch Erfolg oder Markt. Der alte Null-Pfad entstand durch die künstliche Beschränkung auf einen vorbereitenden Schritt pro Jahr und ist als stagnierender Verlauf ungeeignet. Die Korrektur entfernt nur diese Beschränkung; sie ändert weder die ±1-pro-Track-pro-Runde-Regel noch den hypothetischen Status der Ergebnisse.

**Unsicherheiten:** Es wurden in diesem Bericht keine realen Rechte aller Inhalte/Abhängigkeiten, konkreten Release-Archive, bestandenen Security-Tests oder Kundschaft nachgewiesen. Auch Spielereignisse, Rollen und Gates bleiben Annahmen. Dieser Text ist keine Rechts-, Sicherheits- oder Beschaffungsfreigabe und keine Prognose.
