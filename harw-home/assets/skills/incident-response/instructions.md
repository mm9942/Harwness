# Incident Response — einordnen, eindämmen, dokumentieren

**Regel:** Bei einem Vorfall gilt: **Beweise sichern, bevor man etwas ändert**, Schweregrad bestimmen, **reversible** Eindämmung vorschlagen, laufend kommunizieren und eine Zeitleiste führen. **Keine destruktiven oder zustandsändernden Befehle ohne ausdrückliche Freigabe.**

**Warum:** Unter Druck werden hastige Schritte gemacht, die Spuren vernichten oder den Schaden vergrößern (Neustart löscht den Speicherzustand, Löschen entfernt Belege, ein Rollback trifft die falsche Version). Eine ruhige, dokumentierte Reaktion ist schneller am Ziel und lässt sich hinterher auswerten.

## Wann anwenden

- Ein Dienst ist ausgefallen, langsam oder liefert Fehler.
- Verdacht auf Sicherheitsvorfall: unbekannte Prozesse, ungewöhnliche Anmeldungen, Datenabfluss, kompromittierte Zugangsdaten.
- Datenverlust oder -beschädigung wird vermutet.

## Ablauf

### 1. Triage (erste Minuten)

- **Was** ist betroffen (Dienst, System, Daten), **seit wann**, **wie viele** Nutzer/Systeme?
- Ist es noch aktiv oder vorbei? Verschlimmert es sich?
- Erste Beobachtungen **wörtlich** notieren (Fehlermeldungen, Metriken, Zeitstempel mit Zeitzone).

### 2. Schweregrad festlegen

| Stufe | Merkmal | Reaktion |
|---|---|---|
| **Kritisch** | Totalausfall, aktiver Angriff, Datenabfluss, Datenverlust | sofort eskalieren, Verantwortliche informieren |
| **Hoch** | wesentliche Funktion gestört, viele Nutzer betroffen, Workaround fehlt | zeitnah eskalieren |
| **Mittel** | Teilfunktion gestört, Workaround vorhanden | im normalen Ablauf, aber priorisiert |
| **Niedrig** | kosmetisch, einzelne Nutzer, kein Datenrisiko | regulär einplanen |

Im Zweifel die **höhere** Stufe wählen; herabstufen ist leichter als verspätet eskalieren.

### 3. Beweise sichern (nur lesend)

- Relevante Logs, Prozesslisten, offene Verbindungen, Konfigurationsstände, Versionen erfassen.
- Zeitraum großzügig wählen (vor Beginn des Vorfalls bis jetzt).
- Nichts löschen, rotieren oder überschreiben. Kopien statt Originale bearbeiten.
- Bei Sicherheitsverdacht: keine Befehle ausführen, die ein Angreifer bemerken oder die Spuren verändern könnten, ohne Rücksprache.

### 4. Eindämmung vorschlagen

- Bevorzugt **reversible** Maßnahmen: Feature-Flag aus, Traffic umleiten, Zugang sperren statt löschen, Instanz isolieren statt beenden, Rollback auf bekannte gute Version.
- Für jede Maßnahme angeben: **Wirkung, Risiko, Rücknahmeweg**, wer freigeben muss.
- Ausführen erst **nach Freigabe**. Das gilt insbesondere für Neustarts, Prozessbeenden, Löschen, Firewall-/Rechteänderungen, Datenbankeingriffe und alles mit `sudo`.

### 5. Kommunizieren

- Regelmäßige, kurze Updates: Status, Auswirkung, nächste Schritte, nächster Update-Zeitpunkt.
- Fakten und Vermutungen klar trennen („bestätigt“ / „Hypothese“).
- Keine Geheimnisse, personenbezogenen Daten oder Angriffsdetails in breite Kanäle.

### 6. Zeitleiste führen

```text
2026-03-04 09:12 UTC  Alarm: Fehlerrate API > 20 %
2026-03-04 09:15 UTC  Bestätigt: Anfragen an /orders liefern 502
2026-03-04 09:21 UTC  Beobachtet: Deployment v4.8.0 um 09:05 UTC
2026-03-04 09:24 UTC  Vorschlag: Rollback auf v4.7.3 (reversibel) — Freigabe angefragt
2026-03-04 09:27 UTC  Freigabe erteilt durch <Rolle>; Rollback ausgeführt
2026-03-04 09:31 UTC  Fehlerrate normal; Beobachtung läuft
```

Jeder Eintrag: Zeitpunkt mit Zeitzone, was beobachtet oder getan wurde, von wem.

### 7. Wiederherstellung und Nachbereitung

- Nach der Eindämmung: Ursache belegen, dauerhafte Lösung planen, Normalbetrieb bestätigen.
- **Postmortem** (schuldfrei): Zusammenfassung, Auswirkung, Zeitleiste, Ursache und beitragende Faktoren, was gut lief, was nicht, konkrete Maßnahmen mit Verantwortlichen und Termin.

## Falsch

- Sofort den Dienst neu starten — Speicherzustand und Hinweise sind weg.
- Verdächtige Dateien löschen, bevor sie gesichert sind.
- Vermutung als Ursache kommunizieren.
- „Ich habe schnell die Firewall angepasst“ — ohne Freigabe, ohne Notiz.

## Fallstricke

- **Zeitzonen mischen:** alle Zeitstempel in einer Zeitzone (bevorzugt UTC).
- **Tunnelblick:** Die erste plausible Ursache ist nicht automatisch die richtige.
- **Zugangsdaten** in Logs oder Tickets: nicht weiterverbreiten, als kompromittiert behandeln und Rotation vorschlagen.
- **Eigene Rechte:** Dieser Skill beobachtet und empfiehlt; ausführende Schritte brauchen Freigabe.

## Checkliste

1. Auswirkung, Beginn und Umfang erfasst?
2. Schweregrad festgelegt und ggf. eskaliert?
3. Beweise gesichert, bevor etwas geändert wurde?
4. Eindämmung reversibel, mit Risiko und Rücknahmeweg — und freigegeben?
5. Zeitleiste lückenlos mit Zeitzone?
6. Fakten und Hypothesen getrennt kommuniziert?
7. Postmortem mit konkreten Maßnahmen angelegt?
