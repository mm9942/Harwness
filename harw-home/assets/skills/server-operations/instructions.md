# Serverbetrieb — erst beobachten, dann mit Freigabe ändern

**Regel:** Auf Servern wird **zuerst lesend** gearbeitet. Jede zustandsändernde Aktion wird vorher mit **Wirkung, Risiko, Sicherung und Rückweg** erklärt und erst **nach ausdrücklicher Freigabe** ausgeführt. `sudo`, Prozesse beenden, Dienste neu starten, Pakete installieren, Dateien löschen oder Konfiguration ändern — **nie ohne Freigabe**.

**Warum:** Server tragen Produktivlast und Daten. Ein einziger falscher Befehl (falscher Host, falscher Pfad, falscher Prozess) kann Ausfälle oder Datenverlust verursachen, die sich nicht rückgängig machen lassen. Beobachten ist fast immer gefahrlos; Ändern fast nie.

## Wann anwenden

- Zustand eines Servers oder Dienstes soll untersucht werden (Last, Speicher, Plattenplatz, Logs, Prozesse).
- Eine Wartung, ein Update oder eine Konfigurationsänderung steht an.
- Ein Dienst verhält sich auffällig und soll analysiert werden.

## 1. Orientieren (nur lesend)

- **Wo bin ich?** Hostname, Umgebung (Produktion/Staging/Test), angemeldeter Benutzer, aktuelles Verzeichnis. Vor jedem Befehl bewusst prüfen.
- **Systemzustand:** Laufzeit, Last, Speicher, Plattenbelegung, Netzwerkverbindungen.
- **Dienste:** Status, letzte Neustarts, Fehlermeldungen im Dienstprotokoll.
- **Logs:** relevante Zeiträume lesen, filtern — nicht mitschneiden, was nicht nötig ist.
- **Konfiguration:** lesen und mit der erwarteten/versionierten Fassung vergleichen.

Typische lesende Befehle (je nach System): `uptime`, `free`, `df -h`, `ps`, `top -b -n 1`, `ss -tlnp`, `systemctl status <dienst>`, `journalctl -u <dienst> --since …`, `cat`/`less` auf Konfigurationsdateien. Auch lesende Befehle können teuer sein (`du` über große Dateisysteme, `find /`) — gezielt einsetzen.

## 2. Befund und Vorschlag

Vor jeder Änderung einen Vorschlag formulieren:

```text
Beobachtung: /var belegt 97 %, Hauptanteil /var/log/app (41 GB), Logrotation greift nicht.
Vorschlag:   Logrotation-Konfiguration korrigieren und einmalig rotieren.
Wirkung:     Platz wird frei, Dienst läuft weiter.
Risiko:      gering; ältere Logs werden komprimiert, nicht gelöscht.
Sicherung:   Kopie der aktuellen Konfiguration nach /root/backup/logrotate-app.<datum>.
Rückweg:     Konfiguration aus der Sicherung zurückspielen.
Befehle:     <genaue Liste>
Freigabe:    erforderlich (Produktion).
```

## 3. Änderungen sicher durchführen (nach Freigabe)

- **Sicherung zuerst:** Konfiguration kopieren, Datenbank-/Dateisicherung prüfen, Snapshot wenn möglich. Nachsehen, dass die Sicherung tatsächlich existiert und lesbar ist.
- **Wartungsfenster** respektieren; Beteiligte vorher informieren.
- **Kleinste wirksame Änderung**, ein Schritt nach dem anderen, nach jedem Schritt prüfen.
- **Konfiguration validieren**, bevor ein Dienst sie lädt (viele Dienste haben einen Prüfmodus).
- **Reload vor Restart**, wenn der Dienst es unterstützt.
- **Nicht interaktive Fallen:** Befehle, die auf Eingaben warten, Pager öffnen oder ohne Rückfrage rekursiv arbeiten, vermeiden bzw. bewusst parametrieren.
- Jeden ausgeführten Befehl mit Zeitpunkt und Ergebnis protokollieren.

## 4. Nachher prüfen

- Dienststatus, Logs, Kennzahlen: Ist der gewünschte Zustand erreicht?
- Keine neuen Fehler seit der Änderung?
- Ergebnis und ggf. offene Punkte melden.

## 5. Rückweg

- Der Rückweg steht **vor** der Änderung fest, nicht danach.
- Bei unerwartetem Verhalten: anhalten, nicht improvisieren, Rückweg vorschlagen und Freigabe einholen.

## Freigabepflichtig (Auswahl)

- alles mit `sudo` oder als root
- `kill`, `pkill`, Dienst stoppen/starten/neu starten
- Pakete installieren, aktualisieren, entfernen
- Dateien löschen, verschieben, überschreiben; `rm -r`, `truncate`, Umleitungen mit `>`
- Firewall-, Netzwerk-, Benutzer-, Rechteänderungen
- Datenbankänderungen, Migrationen
- Cronjobs, Timer, Autostart
- Neustart des Systems

## Falsch

- „Speicher voll — kurz `rm -rf /var/log/*`.“
- Dienst neu starten, „weil das meistens hilft“, ohne vorher Logs zu sichern.
- Befehl aus dem Gedächtnis auf dem Produktivhost statt auf Staging.
- `kill -9` auf eine PID aus einer veralteten Prozessliste.

## Fallstricke

- **Falscher Host:** Terminals mit mehreren Sitzungen — Hostname vor jeder Änderung prüfen.
- **Wildcards und Leerzeichen:** Globs vorher mit `ls` bzw. `echo` auflösen lassen.
- **Gelöschte, aber offene Dateien** geben keinen Platz frei, solange ein Prozess sie hält.
- **Geheimnisse** in Konfigurationen oder Umgebungen nicht ausgeben oder kopieren.
- **Automatisierung** (Konfigurationsmanagement) überschreibt Handänderungen — Änderung dort machen, wo sie hingehört.

## Checkliste

1. Host, Umgebung und Benutzer bestätigt?
2. Zustand lesend erfasst und dokumentiert?
3. Vorschlag mit Wirkung, Risiko, Sicherung, Rückweg und exakten Befehlen?
4. Ausdrückliche Freigabe vorhanden?
5. Sicherung existiert und ist geprüft?
6. Änderung in kleinen Schritten, jeder geprüft und protokolliert?
7. Endzustand verifiziert und gemeldet?
