# fsread — Dateisystem lesen ohne Shell

**Regel:** Für lesende Dateisystem-Fragen die typisierten `fsread.*`-Werkzeuge nehmen, nicht `ls`, `cat`, `find` & Co. über die Shell. Sie sind reines Rust, laufen ohne Fremdprozess, bleiben im Workspace und liefern JSON mit einer festen Form.

## Wann welches Werkzeug

- **Überblick über ein Projekt:** `fsread.tree` (Tiefe 3 genügt meist), danach gezielt `fsread.ls` für ein Verzeichnis (`long`, `sort`).
- **Datei finden:** `fsread.find` mit `name`/`iname`/`path_glob`, `kind`, Größe oder Alter. Inhalt durchsuchen ist nicht seine Aufgabe, dafür gibt es `fs.grep`.
- **Inhalt lesen:** kleine Datei `fsread.cat` (mit Zeilennummern), Anfang `fsread.head`, Ende eines Logs `fsread.tail`, ab Zeile N `fsread.tail` mit `from_line`. Große Dateien in Scheiben mit `offset` lesen.
- **Eine Zahl:** `fsread.wc` (Zeilen, Bytes), `fsread.du` (Platzbedarf), `fsread.df` (freier Platz, Inodes), `fsread.stat` (genaue Metadaten bis 64 Pfade).
- **Was ist das?** `fsread.file` vor dem Lesen unbekannter Dateien; `fsread.hash` zum Vergleichen; `fsread.diff` für zwei Textdateien; `fsread.json` für ein einzelnes Feld einer großen JSON-Datei.
- **Links:** `fsread.readlink` zeigt das Ziel, `fsread.realpath` den kanonischen Pfad (nur innerhalb des Workspace).

## Ausgabe lesen

- Jede Antwort ist ein JSON-Objekt mit `tool`, `summary` und den Nutzdaten (`entries`, `results`, `content`, ...). Zuerst `summary` lesen.
- Listen sind **deterministisch sortiert** (nach Pfad/Name), Aufrufe sind also vergleichbar.
- Mehrere Pfade in einem Aufruf (`paths`) liefern je Pfad `ok` oder einen Fehlertext; ein fehlender Pfad bricht den Rest nicht ab.
- `fsread.cat`/`head`/`tail` liefern `next_offset` bzw. `more_available`: damit genau dort weiterlesen, nicht von vorn.

## Grenzen und `truncated`

- Ausgaben sind hart begrenzt (Standard 200 Einträge, höchstens 5000; Text höchstens 48 KiB). `truncated: true` heißt: **es gibt mehr**. Dann Filter verengen (`path`, `name`, `max_depth`) oder mit `offset` fortsetzen, nicht dieselbe Anfrage wiederholen.
- `fsread.du` und `fsread.find` melden `complete`/`stopped`: bei `complete: false` ist die Summe nur eine untere Schranke.
- **Nie gelesen werden:** Geheimnis-Dateien (`.env`, Schlüssel, `.ssh`), Symlink-Ziele außerhalb des Workspace, Binärdateien über `cat`/`head`/`tail`. Das ist Absicht; nicht umgehen, sondern den Nutzer fragen.
- Unbekannte Optionen werden abgelehnt (kein stilles Ignorieren): Tippfehler im Feldnamen zeigen sich sofort als Fehler.
