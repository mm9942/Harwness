# Sicherheitsprüfung — autorisiert, nicht-destruktiv, belegbar

**Regel:** Geprüft wird nur, was **ausdrücklich freigegeben** ist, und nur **lesend**. Ausgangspunkt ist ein kurzes **Bedrohungsmodell**; jeder Befund nennt **Ort, Beleg, Auswirkung, Schweregrad und Empfehlung**. Vermutungen werden als solche gekennzeichnet.

**Warum:** Eine Prüfung ohne Bedrohungsmodell sucht wahllos und übersieht das Wichtige. Aktive Tests ohne Freigabe können Systeme beschädigen oder rechtliche Folgen haben. Befunde ohne Beleg werden (zu Recht) ignoriert.

## Wann anwenden

- Code, Konfiguration oder ein System soll auf Schwachstellen untersucht werden.
- Vor einem Release, nach größeren Änderungen an Authentifizierung, Eingabeverarbeitung oder Berechtigungen.
- Nach einem Hinweis auf eine mögliche Lücke.

## Rahmen klären

- **Umfang:** Welche Repositories, Dienste, Hosts? Was ist ausdrücklich ausgeschlossen?
- **Erlaubte Methoden:** nur Code-/Konfigurationslesen? Lesende Systemabfragen? Keine Last- oder Exploit-Tests ohne gesonderte Freigabe.
- **Umgang mit Funden:** Wohin werden kritische Befunde sofort gemeldet?

## 1. Bedrohungsmodell (kurz)

- **Werte:** Was ist schützenswert (Daten, Zugänge, Verfügbarkeit, Integrität)?
- **Angreifer:** Wer kommt in Frage (anonym aus dem Internet, angemeldeter Nutzer, Insider, kompromittierte Abhängigkeit)?
- **Vertrauensgrenzen:** Wo überqueren Daten eine Grenze (Netz → Dienst, Nutzer → Datenbank, Plugin → Kern)?
- **Einstiegspunkte:** APIs, Formulare, Dateiimporte, CLI-Argumente, Umgebungsvariablen, Nachrichten-Queues.

Die Prüfung konzentriert sich auf Einstiegspunkte und Vertrauensgrenzen.

## 2. Prüfbereiche

### Eingabevalidierung

- Wird jede Eingabe an der Vertrauensgrenze geprüft (Typ, Länge, Format, Wertebereich)?
- Injection: SQL/NoSQL, Shell-Befehle, Pfade (`../`), Templates, Deserialisierung, Log-Injection.
- Ausgabe korrekt kodiert (HTML, URL, Shell, SQL-Parameter statt String-Verkettung)?

### Geheimnisse

- Zugangsdaten, Tokens, Schlüssel im Code, in Konfigurationsdateien, Logs, Fehlermeldungen, Testdaten oder der Versionshistorie?
- Werden Geheimnisse aus einem sicheren Speicher geladen und nicht ausgegeben (siehe `secret-handling`)?
- Gefundene Geheimnisse **nicht** im Bericht wiederholen — nur Ort und Art nennen, Rotation empfehlen.

### Authentifizierung und Autorisierung

- Ist jede schützenswerte Operation authentifiziert?
- Wird die Berechtigung **serverseitig** und **pro Objekt** geprüft (nicht nur „ist angemeldet“)?
- Sitzungen: Ablauf, Invalidierung, sichere Cookies, Schutz vor Fixierung.
- Standardkonten, Standardpasswörter, Debug-Zugänge?

### Abhängigkeiten

- Bekannte Advisories für die verwendeten Versionen (Audit-Werkzeuge des Ökosystems).
- Verwaiste oder aus unklaren Quellen stammende Pakete (siehe `dependency-research`).

### Minimale Rechte

- Laufen Dienste mit mehr Rechten als nötig (root, Admin-Rollen, breite Cloud-Policies)?
- Dateirechte auf Konfiguration und Schlüsseln angemessen?
- Netzwerk: Dienste nur dort erreichbar, wo nötig?

### Kryptografie und Transport

- Eigene Kryptografie statt etablierter Bibliotheken? Veraltete Verfahren?
- Transportverschlüsselung erzwungen, Zertifikatsprüfung nicht deaktiviert?
- Zufallszahlen für Sicherheitszwecke aus einer kryptografisch sicheren Quelle?

### Fehler und Protokollierung

- Geben Fehlermeldungen interne Details preis (Stacktraces, Pfade, Abfragen)?
- Werden sicherheitsrelevante Ereignisse protokolliert — ohne sensible Daten?

## 3. Befunde bewerten

| Schweregrad | Richtwert |
|---|---|
| **Kritisch** | aus der Ferne ohne Anmeldung ausnutzbar, Vollzugriff oder Datenabfluss |
| **Hoch** | ausnutzbar mit geringer Hürde, erheblicher Schaden |
| **Mittel** | ausnutzbar unter Bedingungen oder begrenzter Schaden |
| **Niedrig** | Härtung, Defense-in-Depth, geringe Auswirkung |
| **Info** | Beobachtung ohne direktes Risiko |

## 4. Berichtsformat

```text
[HOCH] Fehlende Objekt-Autorisierung beim Abruf von Rechnungen
Ort:        src/api/invoices.* — Handler für GET /invoices/{id}
Beleg:      Handler prüft Anmeldung, aber nicht, ob die Rechnung dem Nutzer gehört (Zeilen …).
Auswirkung: Angemeldete Nutzer können fremde Rechnungen durch Raten der ID lesen.
Status:     durch Codelesen bestätigt, nicht aktiv ausgenutzt.
Empfehlung: Eigentümerprüfung im Handler bzw. in der Abfrage; Test für fremde ID → 403/404.
```

Am Ende: Zusammenfassung nach Schweregrad, geprüfter Umfang, **nicht** geprüfte Bereiche, Einschränkungen der Methode.

## Fallstricke

- **Außerhalb des Umfangs prüfen** — auch wenn es „nur kurz“ wäre.
- **Aktive Ausnutzung** zum „Beweis“ ohne Freigabe.
- **Werkzeugausgabe ungeprüft übernehmen:** Scanner liefern Fehlalarme; jeden Befund nachvollziehen.
- **Geheimnisse im Bericht** zitieren und damit weiterverbreiten.
- **Fehlende Befunde als Sicherheit ausgeben:** „nichts gefunden“ heißt nur „mit dieser Methode in diesem Umfang nichts gefunden“.

## Checkliste

1. Umfang und erlaubte Methoden schriftlich geklärt?
2. Bedrohungsmodell mit Werten, Angreifern, Vertrauensgrenzen, Einstiegspunkten?
3. Eingaben, Geheimnisse, AuthN/AuthZ, Abhängigkeiten, Rechte, Krypto, Fehler geprüft?
4. Jeder Befund mit Ort, Beleg, Auswirkung, Schweregrad, Empfehlung?
5. Bestätigt vs. vermutet klar gekennzeichnet?
6. Keine Geheimnisse im Bericht, nicht geprüfte Bereiche benannt?
