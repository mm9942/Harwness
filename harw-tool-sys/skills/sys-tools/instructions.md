# sys — Prozesse und System ansehen

**Regel:** Fragen nach Prozessen, Speicher, Ports, Umgebung oder Uhrzeit mit den `sys.*`-Werkzeugen beantworten. Sie lesen `/proc` direkt (Linux), starten keinen Fremdprozess und maskieren Geheimnisse bereits in der Ausgabe.

## Wann welches Werkzeug

- **Läuft einer meiner Prozesse X? Welche PID?** `sys.pgrep` (`pattern`, `exact`) oder `sys.ps` mit `name_contains` und `fields`. Beide sehen standardmäßig **nur die eigenen Prozesse** dieser Sitzung (eigener cgroup-Teilbaum samt Job-cgroups, sonst der eigene Prozessbaum; Feld `scope`). Fehlt ein Prozess, heißt das nur „nicht unter meinen Prozessen“, nicht „läuft nicht auf dem System“. `scope: "all"` sieht alle Prozesse, gilt aber nur im Freigabemodus Full Access; sonst kommt ein Fehler.
- **Was verbraucht gerade CPU/RAM?** `sys.top` (echtes Messintervall, `sort` nach cpu oder mem); Liste und Taskzahlen nur über die eigenen Prozesse, CPU-, Last- und Speicherwerte systemweit. Für eine Momentaufnahme ohne Messung `sys.ps` mit `sort`.
- **Speicher/Last:** `sys.free`, `sys.uptime` (Last 1/5/15 Minuten), `sys.uname` (Kernel, Architektur).
- **Welche Ports lauschen?** `sys.ss` mit `listening`, `tcp`/`udp`, `port`. Prozessnamen liefert es nicht (das verlangt Root).
- **Was hält Prozess P offen?** `sys.lsof` mit `pid` (nur eigene Prozesse): Dateien, Sockets, `cwd`, `exe`. Nützlich, bevor man etwas beendet oder wenn eine Datei „busy“ ist.
- **Umgebung/Identität/Zeit/Programme:** `sys.env` (Filter nach Name oder Präfix), `sys.id`, `sys.date` (UTC oder fester Offset), `sys.which` (ist ein Programm installiert und wo).

## Ausgabe lesen

- JSON mit `tool`, `summary`, den Daten und Zählern (`count`, `matched`, `total`). `matched` > `count` heißt: Liste gekürzt.
- `sys.env`: Werte mit `masked: true` sind `***`; Namen mit KEY/TOKEN/SECRET/PASSWORD und tokenartige Werte werden immer maskiert. Filter wirken nur auf Namen, nie auf Werte — es gibt keinen Weg, maskierte Werte zu erraten oder zu enthüllen.
- `command` in `sys.ps`/`sys.top`/`sys.pgrep` ist vollständig geschwärzt: nur Prozessname plus Argumentanzahl (`curl <redacted: 4 args>`, Kernel-Threads als `[name]`). Argumentwerte erscheinen nie; `full`/`command_contains` filtern nur diese geschwärzte Form.
- Prozesse können zwischen zwei Aufrufen verschwinden; ein fehlender Eintrag ist normal, kein Fehler.

## Grenzen und `truncated`

- Listen sind begrenzt (Standard 100, höchstens 2000); `truncated: true` heißt „mehr vorhanden“ — Filter verengen statt erhöhen.
- `sys.lsof` zeigt nur Prozesse des eigenen Nutzers, nur Namen und Modi, nie Inhalte.
- `sys.date` kennt keine Zeitzonen-Datenbank, nur UTC und feste Offsets.
- Diese Werkzeuge **beenden, ändern oder starten nichts**. Zum Beenden eines Prozesses braucht es den eigens freigegebenen Weg, nicht diese Werkzeuge.
- Rechte: `sys.*` brauchen die Stufe „Prozess ausführen/inspizieren“ des Profils; fehlt sie, kommt ein klarer Ablehnungsfehler.
