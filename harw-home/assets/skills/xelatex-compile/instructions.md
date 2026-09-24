# Mit XeLaTeX bauen: erst prüfen, dann gezielt bauen

**Regel:** Gebaut wird erst, wenn die statische Prüfliste aus dem Skill `latex-writing` (Abschnitt 8) sauber durchläuft. Danach startet `latex.build` den Build. Schlägt er fehl, wird genau der erste gemeldete Fehler behoben und neu gebaut. Nach **höchstens drei Build-Versuchen** ist Schluss: Dann folgt ein Bericht mit dem Stand, den offenen Fehlern und dem nächsten Schritt, statt weiter zu raten.

**Warum:** Jeder Build kostet Zeit und eine Freigabe der Nutzerin. TeX meldet nach dem ersten harten Fehler oft nur noch Folgefehler. Wer den ersten Fehler behebt und dann neu baut, kommt schneller ans Ziel als jemand, der zehn Stellen gleichzeitig ändert.

> Eigene Worte, keine Handbuchtexte. Details zu einzelnen Paketen stehen in deren Dokumentation (`texdoc fontspec`, `texdoc unicode-math`).

## Das Werkzeug `latex.build`

- Aufruf: `{"file": "pfad/main.tex"}`. Optional ist `"engine"` mit `"xelatex"` (Standard), `"pdflatex"` oder `"lualatex"`. `"clean": true` räumt die Hilfsdateien auf (`latexmk -c`).
- Es startet `latexmk` mit festen Schaltern (`-interaction=nonstopmode -halt-on-error -file-line-error -no-shell-escape -cd`, dazu `-norc`). Weitere Argumente gibt es nicht. Es läuft in der Sandbox ohne Netz; Pakete werden **nicht** nachgeladen.
- **Rückfall ohne latexmk:** Fehlt `latexmk`, aber die Engine ist da, ruft `latex.build` die Engine direkt mehrfach auf (mit denselben Sicherheitsschaltern), dazwischen biber, falls eine `.bcf` entsteht. Alle Läufe teilen sich ein Zeitlimit. `not_installed` kommt nur, wenn auch die Engine fehlt.
- **Vorabprüfung:** `latex.check` (`{"file": "main.tex"}`) prüft in derselben Sandbox, ob Klasse, Pakete, Schriften und Babel-Sprachen vorhanden sind, und liefert `missing` samt `user_message` mit Installationshinweis. Vor dem ersten Build aufrufen, statt einen Build scheitern zu lassen.
- Für Berichte, Business-Paper und Handbücher gibt es die Vorlage aus dem Skill `latex-report` (Werkzeug `latex.template`).
- `.latexmkrc` wird dabei ignoriert. Alles, was der Build braucht, muss in der `.tex`-Datei stehen (Engine über `engine`, biber läuft automatisch).
- Jeder Aufruf braucht eine Freigabe. Deshalb nicht „auf Verdacht“ bauen.

### Rückgabe lesen

| Feld | Bedeutung |
|---|---|
| `status` | `ok`, `ok_with_warnings` (PDF da, aber Overfull-Box über 1 pt), `failed`, `timeout`, `cleaned` oder `not_installed` |
| `pdf` | Pfad des erzeugten PDFs (bei `ok` und `ok_with_warnings`) |
| `pages`, `overfull`, `underfull`, `missing_chars`, `language_warnings` | Seitenzahl, zu volle/zu leere Zeilen mit Zeile und pt, fehlende Glyphen, fehlende Trennmuster; Deutung und Fixes im Skill `latex-report` |
| `log_excerpt` | die ersten Fehler und Warnungen aus dem `.log`: `! …`-Zeilen samt `l.<n>`, `datei:zeile:`-Zeilen, „Undefined reference“, „Missing character“, `fontspec`-Fehler, „not found“ |
| `output_tail` | die letzten Zeilen der latexmk-Ausgabe (z. B. biber-Meldungen) |
| `exit_code`, `truncated` | Rückgabewert von latexmk; `truncated` heißt, die Ausgabe war zu lang |

### `not_installed`: sofort aufhören

Meldet `latex.build` den Status `not_installed`, ist LaTeX (oder die gewählte Engine) auf dem Rechner nicht installiert. Dann gilt:

1. **Nicht weiter probieren.** Keinen anderen Engine-Namen durchprobieren, keinen zweiten Aufruf starten und nichts installieren.
2. Den Text aus `user_message` **wörtlich** an die Nutzerin weitergeben. Er nennt das fehlende Programm und typische Installationsbefehle als Hinweis.
3. Die `.tex`-Dateien bleiben fertig und statisch geprüft liegen. In der Rückgabe steht der Hinweis prominent im Abschnitt „Hinweis für die Nutzerin“.

## XeLaTeX-Präambel

```latex
\documentclass[paper=a4, fontsize=11pt, parskip=half]{scrartcl}
\usepackage{fontspec}                 % Schriften per Name, UTF-8 direkt
\usepackage{babel}                    % oder polyglossia (nicht beides)
\babelprovide[import,main]{german}    % braucht kein ngerman.ldf
\usepackage{unicode-math}             % Mathe-Schrift als OpenType
\setmainfont{TeX Gyre Pagella}        % aus dem TeX-Baum, überall vorhanden
\setsansfont{TeX Gyre Heros}
\setmonofont{Latin Modern Mono}[Scale=MatchLowercase]
\setmathfont{TeX Gyre Pagella Math}
\usepackage{microtype, csquotes, booktabs, graphicx}
\usepackage[backend=biber, style=authoryear]{biblatex}
\addbibresource{literatur.bib}
\usepackage{hyperref}
\usepackage[ngerman, capitalise]{cleveref}
```

- Mit XeLaTeX **nicht** `inputenc` und nicht `fontenc` mit `T1` laden. Beides stammt aus der pdfLaTeX-Welt und stört `fontspec`.
- `unicode-math` ersetzt `amssymb`. Wer beides lädt, bekommt „Command … already defined“. Deshalb `amssymb` weglassen; `amsmath`/`mathtools` vor `unicode-math` laden.
- Sprache: `\babelprovide[import,main]{german}` ist die robuste Vorgabe. `\usepackage[ngerman]{babel}` scheitert, wenn `ngerman.ldf` (Paket `texlive-lang-german`) fehlt; meldet der Build-Bericht `language_warnings` („no patterns for …“), ist die Trennung falsch und der Text wird englisch getrennt.
- polyglossia-Variante: `\usepackage{polyglossia}`, `\setdefaultlanguage[spelling=new]{german}`, für englische Zitate `\setotherlanguage{english}`.

### Systemschriften und TeX-Schriften

- **TeX-Schriften** (Latin Modern, TeX Gyre, Libertinus, Source Serif/Sans, Fira) liegen im TeX-Baum. Sie funktionieren immer, auch in der Sandbox von `latex.build`. Für Dokumente, die überall bauen sollen, sind sie die erste Wahl.
- **Systemschriften** unter `/usr/share/fonts` findet XeLaTeX über fontconfig per Namen (`\setmainfont{Noto Serif}`). Schriften im Home-Verzeichnis der Nutzerin (`~/.fonts`, `~/.local/share/fonts`) sieht die Sandbox **nicht**.
- **Eigene Schriftdatei:** Die `.otf`/`.ttf` in das Projekt legen und per Dateiname laden. Das baut überall gleich:

  ```latex
  \setmainfont{Hausschrift}[Path=fonts/, Extension=.otf,
    UprightFont=*-Regular, BoldFont=*-Bold, ItalicFont=*-Italic]
  ```
- Fehlende Zeichen (Emoji, CJK, seltene Symbole) mit einer Zusatzschrift setzen: `\newfontfamily\emojifont{Noto Color Emoji}` und `{\emojifont 🙂}`, oder eine Schrift wählen, die die Glyphen hat.

## `.latexmkrc` für die Nutzerin

Für Builds von Hand (nicht für `latex.build`) neben die Hauptdatei legen:

```perl
$pdf_mode = 5;        # XeLaTeX
$bibtex_use = 2;      # biber automatisch, .bbl beim Aufräumen löschen
$clean_ext = 'bbl run.xml';
```

Dann genügt `latexmk` im Projektordner; `latexmk -c` räumt auf, `latexmk -pvc` baut bei jeder Änderung neu.

## biber-Ablauf

1. XeLaTeX-Lauf 1 schreibt die Zitat-Anfragen in `main.bcf`.
2. biber liest `main.bcf` und die `.bib`-Dateien und erzeugt `main.bbl`.
3. XeLaTeX-Lauf 2 (und ggf. 3) setzt Literaturliste und Verweise.

`latexmk` erkennt das selbst und wiederholt so oft wie nötig; ohne `latexmk` übernimmt der Rückfall von `latex.build` diese Reihenfolge. biber nie separat aufrufen. Typische biber-Meldungen im `output_tail`:

- `Cannot find 'main.bcf'`: Lauf 1 ist gescheitert; den ersten LaTeX-Fehler beheben.
- `Data source 'literatur.bib' not found`: Pfad in `\addbibresource` stimmt nicht (relativ zur Hauptdatei, mit Endung).
- `syntax error` in der `.bib`: fehlendes Komma oder eine offene Klammer im genannten Eintrag.
- Biber-Version passt nicht zu biblatex: Installation der Nutzerin betroffen, als Hinweis melden.

## Typische XeLaTeX-Fehler und wie man sie im `log_excerpt` erkennt

| Im Auszug | Bedeutung | Gezielte Korrektur |
|---|---|---|
| `Package fontspec Error: The font "X" cannot be found.` | Schriftname unbekannt oder nicht in der Sandbox sichtbar | TeX-Schrift nehmen oder Schriftdatei ins Projekt legen (`Path=`) |
| `! Font \TU/X(0)/m/n/10=… not loadable: Metric (TFM) file or installed font not found.` | Schrift fehlt, oft Tippfehler im Namen | Namen prüfen; Systemschrift durch TeX-Schrift ersetzen |
| `Missing character: There is no ß in font cmr10!` | Schrift ohne Unicode-Abdeckung (klassische Type1-Schrift) | `fontspec` laden und eine OpenType-Schrift setzen |
| `Missing character: There is no 🙂 (U+1F642) in font …` | Glyph fehlt in der gewählten Schrift | Zusatzschrift per `\newfontfamily` oder Zeichen ersetzen |
| `Fatal Package fontspec Error: The fontspec package requires either XeTeX or LuaTeX.` | Build lief mit pdfLaTeX | `engine` auf `xelatex` setzen |
| `LaTeX Error: Command \… already defined.` nach `unicode-math` | Kollision mit `amssymb` o. Ä. | `amssymb` entfernen |
| `Package inputenc Error` / `fontenc`-Warnungen | pdfLaTeX-Pakete unter XeLaTeX | `inputenc`/`fontenc` entfernen |
| `./kapitel/x.tex:17: Undefined control sequence.` | Makro unbekannt | Paket laden (Paket-Tabelle im Skill `latex-writing`) |
| `! LaTeX Error: File 'x.sty' not found.` | Paket fehlt in der Installation | Alternative ohne das Paket oder Hinweis an die Nutzerin; nie selbst installieren |
| `LaTeX Warning: Citation 'k' undefined` bei `status: ok` | Key fehlt in der `.bib` | Key abgleichen; ein weiterer Build ist nur nötig, wenn Quellen geändert wurden |
| `status: timeout` | Endlosschleife (z. B. rekursives Makro) oder sehr großes Dokument | Makros prüfen; große Grafiken vorab als PDF erzeugen lassen |

**Vorgehen bei `failed`:** Die erste `!`- oder `datei:zeile:`-Zeile im `log_excerpt` nehmen. Die genannte Datei an der genannten Zeile lesen und etwas davor schauen. Die Ursache beheben und genau einmal neu bauen. Unicode-Probleme (unsichtbare Zeichen, falsche Anführungen, geschützte Leerzeichen aus Kopien) zeigen sich als `Missing character` oder seltsame Abstände. Dann an der Stelle nach nicht druckbaren Zeichen suchen und sie ersetzen.

## Übergabe

- Bei `ok`: PDF-Pfad, Anzahl der Build-Versuche, verbleibende Warnungen (etwa undefinierte Verweise oder Overfull-Boxen) mit Datei und Zeile.
- Bei `failed` nach drei Versuchen: letzter `log_excerpt`, die vermutete Ursache und der konkrete Vorschlag. Die Quellen bleiben im zuletzt besten Zustand.
- Bei `not_installed`: `user_message` wörtlich im Abschnitt „Hinweis für die Nutzerin“ und dazu der Kompilier-Hinweis (`latexmk -xelatex main.tex`; ohne latexmk: `xelatex main.tex` zweimal, bei Literatur mit `biber main` dazwischen).
