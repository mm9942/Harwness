# Berichte mit der Harwness-Vorlage: prüfen, kopieren, schreiben, sauber bauen

**Regel:** Ein Bericht, ein Business-Paper oder ein Handbuch entsteht aus der mitgelieferten Vorlage, nicht aus einer frei erfundenen Präambel. Der Ablauf ist immer derselbe: `latex.check` → `latex.template` → Inhalt schreiben → `latex.build` → Overfull-/Underfull-Boxen gezielt beheben (höchstens drei Runden) → ehrlicher Bericht. Fertig ist das Dokument erst, wenn der Build-Bericht sauber ist oder die verbleibenden Warnungen offen genannt sind und eine Stichprobe des PDFs gelesen wurde.

**Warum:** Die Vorlage bringt Schriften, deutsche Silbentrennung ohne `ngerman.ldf`, Farben, Hinweiskästen, Tabellenspalten und großzügige Umbruchregeln bereits mit. Wer sie benutzt, spart Build-Runden und vermeidet die typischen Fehler: englische Trennung im deutschen Text, Zeilen im Rand, zusammengeklebte Nummern im Inhaltsverzeichnis („10.1Was“). Ein Build mit `status = ok` heißt noch nicht, dass das PDF gut aussieht. Erst der Build-Bericht (Seiten, Overfull, Warnungen) und ein Blick ins PDF zeigen das.

> Eigene Worte, keine Handbuchtexte. Dieser Skill ergänzt `latex-writing` (Handwerk, statische Prüfliste), `xelatex-compile` (Build, Schriften, Log-Diagnose) und `business-writing-pyramid` (Aufbau von Geschäftstexten).

## 0. Rolle, Grenzen und Übergabe

Diese Regeln gelten für den LaTeX-Writer (`uia-latex-writer`) in jedem Lauf.

- **Auftrag klären:** Dokumenttyp (`bericht`, `business-paper`, `handbuch`), Titel, Datum, Autorin, Sprache, Leserkreis. **Datum und Autorin nur aus dem Auftrag übernehmen.** Fehlen sie, bleibt `author` leer bzw. das Datum ist das von `latex.template` gesetzte heutige Datum, und die Übergabe nennt das als Annahme. Nie einen Namen oder eine Firma erfinden.
- **Nichts erfinden:** keine Quellen, Zitate, Zahlen, Messwerte, Namen oder `.bib`-Einträge. Fehlt etwas, bleibt es als `% TODO:` im Quelltext und steht in der Übergabe unter „Offene TODOs“.
- **Werkzeuge:** Dateien mit `fs.read`/`fs.write`/`fs.edit`, Vorlage mit `latex.template`, Vorabprüfung mit `latex.check`, Build mit `latex.build`, PDF-Stichprobe mit `doc.read_pdf`. Keine freie Shell, nichts installieren, nicht ins Netz, kein Shell-Escape.
- **Fehlt LaTeX:** Meldet `latex.check` fehlende Pakete oder Schriften oder `latex.build` den Status `not_installed`, hörst du auf zu bauen. Die `user_message` gibst du **wörtlich** unter „Hinweis für die Nutzerin“ zurück. Die `.tex`-Dateien bleiben fertig und statisch geprüft liegen.
- **Budget:** höchstens drei Rückfragen, gebündelt am Anfang. Höchstens drei Korrekturrunden mit `latex.build` nach dem ersten Build. Material nur so weit lesen, wie der Auftrag es braucht.
- **Keine fremden Dateien ändern:** nur die LaTeX-Quellen des Auftrags. `harw-report.sty` bleibt unverändert; Anpassungen gehören in die Hauptdatei (Abschnitt 7).

## 1. Ablauf

1. **`latex.check` zuerst.** Aufruf mit der geplanten Hauptdatei bzw. nach `latex.template` mit der erzeugten Datei. Das Werkzeug prüft Klasse, Pakete, Schriften und Sprache in derselben Sandbox wie der Build. Fehlt etwas (`missing`), gib `user_message` an die Nutzerin weiter und wähle, wenn möglich, eine Alternative, die vorhanden ist (etwa eine andere Schrift über den Parameter der Vorlage). Installiere nie selbst.
2. **`latex.template` kopiert die Vorlage.** Parameter: `file` (Zielpfad der Hauptdatei, z. B. `bericht/bericht.tex`), `kind` (`bericht` | `business-paper` | `handbuch`), dazu optional `title`, `subtitle`, `author`, `date`, `language` (`german` | `english`) und nur auf Wunsch der Nutzerin `accent`, `warn`, `main_font`, `sans_font`, `mono_font`. Das Werkzeug schreibt `harw-report.sty` und das Gerüst neben die Hauptdatei und überschreibt nie eine vorhandene Datei. Existiert die Datei schon, arbeite in ihr weiter oder wähle mit der Nutzerin einen neuen Namen.
3. **Schreiben.** Ersetze die Platzhaltertexte des Gerüsts durch den echten Inhalt. Streiche Bausteine, die der Auftrag nicht braucht; erfinde keine, um sie zu füllen. Danach die statische Prüfliste aus `latex-writing`, Abschnitt 8.
4. **`latex.build`** mit `engine = "xelatex"` (die Vorlage braucht `fontspec`; LuaLaTeX geht auch). Das Werkzeug baut mit `latexmk` oder, falls es fehlt, direkt mit der Engine in mehreren Läufen (biber dazwischen, falls nötig).
5. **Build-Bericht lesen** (Abschnitt 2) und Overfull-/Underfull-Boxen gezielt beheben (Abschnitt 6). Höchstens drei Korrekturrunden, dann aufhören.
6. **Endkontrolle** (Abschnitt 8) und **ehrlicher Bericht** (Abschnitt 9).

## 2. Den Build-Bericht lesen

| Feld | Bedeutung | Was tun |
|---|---|---|
| `status` | `ok`, `ok_with_warnings`, `failed`, `timeout`, `cleaned`, `not_installed` | `ok_with_warnings` heißt: PDF da, aber mindestens eine Overfull-Box über 1 pt. Nicht als „fertig“ melden, sondern beheben oder offen nennen. |
| `pdf` | Pfad des PDFs | für die Stichprobe mit `doc.read_pdf` |
| `pages` | Seitenzahl | plausibel zum Umfang? Eine Seite bei einem langen Bericht heißt meist: abgebrochen oder leer. |
| `overfull` | Liste mit Zeile und Überstand in pt | jede Box über 1 pt beheben (Abschnitt 6) |
| `underfull` | Liste mit Zeile und Badness | nur auffällige beheben; sie sind selten sichtbar |
| `missing_chars` | Zeichen, die die Schrift nicht hat | Zeichen ersetzen oder Schrift wechseln |
| `language_warnings` | fehlende Trennmuster | Sprache prüfen (`language` der Vorlage); nie mit englischen Mustern deutschen Text setzen |
| `log_excerpt` | erste Fehler und Warnungen | bei `failed` den **ersten** Fehler beheben (Skill `xelatex-compile`) |

## 3. Bausteine der Vorlage

Jedes Gerüst bringt dieselben Bausteine mit. Sie sind Angebote, keine Pflicht.

- **„Wichtig vorab“** (`achtung`): das eine, was die Leserin vor allem anderen wissen muss, etwa ein Prototyp-Status oder eine offene Annahme. Höchstens drei Sätze. Kein Anlass im Auftrag, dann streichen.
- **„Die Idee in drei Sätzen“** (`merke`): worum es geht, was herauskommt, was die Leserin damit macht. Beim Business-Paper steht hier die Kernaussage.
- **Glossar** (`tabularx` mit `@{}lL@{}`): nur Begriffe, die im Text wirklich vorkommen und die die Leserin nicht kennt.
- **Abbildung** (TikZ, Abschnitt 5): ein Bild vom Aufbau oder vom Ablauf.
- **Beispiel** (`beispiel`): ein kurzer, konkreter Fall zu einem abstrakten Punkt.
- **Stand/Status** (`description` mit `style=nextline`): was fertig ist, was offen ist, wer als Nächstes was tut.
- **Quellen** (`thebibliography`): nur echte Quellen aus dem Auftrag oder dem Material. Verweise im Text mit `\cite{schluessel}`. Keine Quelle vorhanden: Block streichen.

Die Kästen nehmen einen optionalen Titel: `\begin{merke}[Warum getrennte Teile?]`.

## 4. Tabellen

- **Spalten mit Fließtext** als `L` (linksbündiges `X` aus `tabularx`): `\begin{tabularx}{\textwidth}{@{}lL@{}}`. Nie `p{…}`-Spalten raten, die zusammen breiter als `\textwidth` sind.
- **Ab etwa 15 Zeilen** oder wenn die Tabelle über eine Seite laufen kann: `longtable` mit `>{\raggedright}p{…}`-Spalten, die letzte mit `\arraybackslash`, und Kopf über `\endhead`, damit er sich auf jeder Seite wiederholt. Summe der Spaltenbreiten plus Abstände höchstens `\textwidth` (bei 2,4 cm Rand etwa 16 cm; die Gerüste nutzen 15 cm Spalten).
- **booktabs:** nur `\toprule`, `\midrule`, `\bottomrule`; keine senkrechten Linien, keine doppelten Striche.
- **Zahlen rechtsbündig** (`r`), Tausender mit schmalem Abstand (`12\,000`), Einheit in den Spaltenkopf.
- Große Tabellen in `{\small …}` setzen statt sie zu skalieren. Nie `\resizebox` um eine Tabelle.

## 5. Abbildungen mit TikZ

Die Vorlage definiert vier Stile: `harwbox` (Kasten mit runden Ecken, 3,2 cm Textbreite), `harwpfeil` (Pfeil), `harwdoppelpfeil` (Pfeil in beide Richtungen) und `harwlabel` (kleine Beschriftung mit weißem Grund). Knoten relativ platzieren (`right=of …`), nie absolute Koordinaten raten.

```latex
\begin{figure}[htbp]
\centering
\begin{tikzpicture}[node distance=1.6cm]
  \node[harwbox,fill=harwgrau] (eingang) {Eingang\\(Daten)};
  \node[harwbox,fill=harwaccent!15,right=of eingang] (kern)
    {\textbf{Verarbeitung}\\prüfen, aufbereiten};
  \node[harwbox,fill=harwgrau,right=of kern] (ergebnis) {Ergebnis};
  \draw[harwpfeil] (eingang) -- node[harwlabel,above=2pt]{liefert} (kern);
  \draw[harwpfeil] (kern) -- node[harwlabel,above=2pt]{erzeugt} (ergebnis);
\end{tikzpicture}
\caption{Die Bausteine und ihre Verbindungen}\label{fig:aufbau}
\end{figure}
```

- Drei Kästen nebeneinander mit 1,6 cm Abstand passen in die Textbreite; bei vier Kästen `text width=2.4cm` setzen oder zweizeilig anordnen.
- Farben nur aus der Vorlage (`harwaccent`, `harwwarn`, `harwgrau`, Mischungen wie `harwaccent!15`).
- Ist die Abbildung zu breit, `[scale=0.85,transform shape]` an die `tikzpicture` hängen, statt Text zu verkleinern.

## 6. Overfull- und Underfull-Boxen beheben

Jede Overfull-Box über 1 pt ist sichtbar: Text ragt in den Rand. Die Zeilennummer aus `overfull` lesen, die Stelle öffnen und gezielt beheben. Typische Ursachen und Fixes:

| Ursache | Fix |
|---|---|
| Dateiname, Pfad, URL, Befehl im Fließtext | `\path{ordner/datei.txt}` bzw. `\url{…}`: bricht an `/` und `.`. Lange Bezeichner in `\code{…}` mit `\allowbreak` an sinnvollen Stellen teilen (`\code{sehr\allowbreak\_langer\allowbreak\_name}`). |
| langes zusammengesetztes Wort | Trennhinweis `Da\-ten\-bank\-ab\-fra\-ge` oder `\hyphenation{…}` in der Präambel der Hauptdatei |
| Tabelle breiter als der Text | `L`-Spalten statt fester `p`-Breiten; Spaltenbreiten nachrechnen; `{\small …}` |
| Abbildung zu breit | `scale=…,transform shape`, kleinere `text width` |
| hartnäckige Zeile | den Satz umformulieren (kürzeres Wort, andere Reihenfolge) — oft die beste Lösung |
| Absatz mit vielen unbrechbaren Teilen | lokal `{\emergencystretch=5em …}` um den Absatz |

- Underfull-Boxen entstehen oft durch `\\` im Fließtext oder sehr schmale Spalten; `\\` entfernen, Spalte als `L` setzen.
- Nie global `\hbadness=10000` oder `\hfuzz` hochsetzen, um Warnungen zu verstecken.
- Nach jeder Korrekturrunde neu bauen und den Bericht erneut lesen. **Nach drei Runden aufhören** und die verbleibenden Boxen mit Zeile und pt nennen.

## 7. Dokumenttypen

### bericht

Für Sachstände, Auswertungen, Dokumentationen eines Vorhabens. Aufbau: Wichtig vorab → Die Idee in drei Sätzen → Inhaltsverzeichnis → Worum es geht (mit Glossar) → Aufbau (Abbildung) → Ergebnisse im Detail → Stand und nächste Schritte → Quellen.

### business-paper (nach Minto)

Für Entscheidungsvorlagen, Positionspapiere, Empfehlungen. Es gilt der Skill `business-writing-pyramid`:

- **Kernaussage zuerst:** Der Kasten „Die Idee in drei Sätzen“ enthält die Antwort, die Gründe in einem Satz und die geforderte Entscheidung. Der Titel ist eine Aussage, kein Thema.
- **Einleitung als SCQ:** Lage (Situation), Störung (Complication), Frage (Question). Die Frage ist die, die die Kernaussage beantwortet.
- **Gliederung:** Jeder Abschnitt unter der Empfehlung ist ein tragender Grund, als ganzer Satz im Abschnittstitel, gleichartig (alles Gründe oder alles Schritte), überschneidungsfrei, geordnet (Reihenfolge nach Gewicht, Zeit oder Struktur). Belege stehen im Abschnitt, vom wichtigsten zum unwichtigsten.
- Danach Umsetzung und Risiken, Stand und Entscheidungsbedarf, Glossar im Anhang, Quellen.

### handbuch

Für Anleitungen und Nachschlagewerke: Wichtig vorab (Voraussetzungen, Grenzen) → Die Idee in drei Sätzen → Überblick mit Abbildung und Glossar → Schritt für Schritt (nummeriert, mit Erfolgskriterium je Schritt) → Referenz (longtable) → Fehlerbehebung (Symptom, Ursache, Abhilfe) → Stand → Quellen.

### Farben und Schriften

Nur auf ausdrücklichen Wunsch der Nutzerin ändern, und dann über die Parameter von `latex.template` (`accent`, `warn` als Hex ohne `#`, `main_font`, `sans_font`, `mono_font`). Ist die Vorlage schon kopiert, die Makros `\harwAccentHex`, `\harwWarnHex`, `\harwMainFont`, `\harwSansFont`, `\harwMonoFont` bzw. `\harwLanguage` im Kopf der Hauptdatei ändern, nie `harw-report.sty`. Vorgaben: Akzent `714B67`, Warnung `C0392B`, DejaVu Serif/Sans/Sans Mono. Eine gewünschte Schrift vorher mit `latex.check` prüfen; fehlt sie in der Sandbox, die Vorgabe behalten und das melden.

## 8. Endkontrolle (textbasiert)

Vor der Übergabe, nach dem letzten Build:

1. **Build-Bericht:** `status` ist `ok` (oder `ok_with_warnings` mit genannten Resten), `pages` passt zum Umfang, `overfull` ist leer oder jede Box über 1 pt ist erklärt, `missing_chars` und `language_warnings` sind leer.
2. **PDF-Stichprobe mit `doc.read_pdf`:** mindestens Seite 1–2 (`pages = "1-2"`: Titel, Datum, Autorin, Inhaltsverzeichnis mit richtigen Nummern und Titeln) und eine Seite aus der Mitte (Tabelle oder Abbildung). Prüfen: stehen Titel, Datum und Autorin so da wie im Auftrag? Sind Platzhaltertexte der Vorlage übrig („Platzhalter“, „Begriff A“, „Grund 1 als ganzer Satz“, „example.org“)? Stimmen die Umlaute?
3. **Quelltext:** keine `%%…%%`-Platzhalter mehr, keine `% TODO:` ohne Eintrag in der Übergabe.

## 9. Übergabe

```markdown
## LaTeX-Ergebnis

**Hinweis für die Nutzerin:** <nur falls nötig, z. B. user_message wörtlich; sonst weglassen>

- Dokument: <Typ> · <Hauptdatei> · PDF: <Pfad> (<pages> Seiten)
- Build: <ok | ok_with_warnings | failed nach n Runden → erster Fehler | not_installed | nicht gebaut>
- Overfull/Underfull: <keine | Liste Datei:Zeile, pt>
- Warnungen: <Sprache, fehlende Zeichen, undefinierte Verweise>
- PDF-Stichprobe: <gelesene Seiten, Befund>
- Annahmen: <Datum, Autorin, Sprache, Farben/Schriften = Vorgabe …>
- Offene TODOs: <Liste>
- Selbst bauen: `latexmk -xelatex <hauptdatei>.tex` (ohne latexmk: `xelatex` zweimal)
```

Ehrlich berichten: „gebaut, aber drei Zeilen ragen 4–12 pt in den Rand (Zeilen …)“ ist eine brauchbare Übergabe; „fertig“ bei `ok_with_warnings` ist es nicht.
