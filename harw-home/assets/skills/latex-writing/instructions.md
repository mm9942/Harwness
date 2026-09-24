# LaTeX schreiben: sauber gegliedert, deutsch gesetzt, statisch geprüft

**Regel:** Eine LaTeX-Quelle ist erst fertig, wenn sie ohne Kompilieren plausibel fehlerfrei ist. Dazu gehören ausgeglichene Klammern und Umgebungen, eine passende Marke für jeden Verweis, ein `.bib`-Eintrag für jedes Zitat und ein geladenes Paket für jedes benutzte Makro. Erst danach wird gebaut, falls ein Build-Werkzeug vorhanden ist (Skill `xelatex-compile`). Ist keins vorhanden, bekommt die Nutzerin einen Kompilier-Hinweis.

**Warum:** Ein LaTeX-Lauf bricht beim ersten harten Fehler ab. Die Meldung zeigt oft auf eine Folgezeile statt auf die Ursache. Wer vorher statisch prüft, spart Build-Runden und liefert der Nutzerin Quellen, die beim ersten Versuch durchlaufen.

> **Berichte, Business-Paper, Handbücher:** zuerst die mitgelieferte Vorlage nutzen (Skill `latex-report`, Werkzeug `latex.template`). Vor dem ersten Build prüft `latex.check`, ob Klasse, Pakete, Schriften und Sprache in der Sandbox vorhanden sind.

> Diese Anleitung ist in eigenen Worten geschrieben. Sie beschreibt Handwerk und übernimmt keine Handbuchtexte. Genaue Optionen eines Pakets stehen in dessen Dokumentation (`texdoc <paket>` auf dem Rechner der Nutzerin).

## Wann anwenden

- Seminar-, Bachelor-, Master- und Doktorarbeiten, Paper, Berichte, Protokolle.
- Präsentationen mit Beamer.
- Einzelne Bausteine wie Tabelle, Formel, Diagramm, Literaturliste oder Titelseite.
- Überarbeitung vorhandener `.tex`/`.bib`-Dateien.

## 1. Wissenschaftliche Arbeiten

### Gliederung

- **Paper (6–12 Seiten):** Einleitung (Problem, Beitrag, Aufbau) · Verwandte Arbeiten · Methode · Ergebnisse · Diskussion (Grenzen) · Fazit. Die Kurzfassung (`abstract`) nennt Problem, Vorgehen, Hauptergebnis und Bedeutung in je einem Satz.
- **Thesis:** Titelseite · (Sperrvermerk) · Kurzfassung DE/EN · Inhaltsverzeichnis · Einleitung · Grundlagen · Stand der Forschung · Konzept/Methode · Umsetzung · Evaluation · Fazit und Ausblick · Literatur · Anhang · Eidesstattliche Erklärung. Die Vorgaben der Hochschule gehen immer vor; frag nach Vorlage und Zitierstil.
- Eine Datei je Kapitel (`kapitel/einleitung.tex`), eingebunden mit `\include{…}` (Thesis, neue Seite) oder `\input{…}` (Paper). Die Hauptdatei enthält nur Präambel, Titel und die Einbindungen.
- Überschriften sagen, was im Abschnitt steht. Tiefer als `\subsubsection` wird es selten übersichtlich.

### Literatur mit BibLaTeX und biber

```latex
\usepackage[backend=biber, style=authoryear, maxcitenames=2]{biblatex}
\addbibresource{literatur.bib}
% im Text
\textcite{mueller2021} zeigen … \parencite[S.~12]{mueller2021}
% am Ende
\printbibliography[heading=bibintoc]
```

- `backend=biber` ist der Standard. biber läuft zwischen zwei LaTeX-Läufen; `latexmk` erledigt das selbst.
- Zitierbefehle: `\parencite` (in Klammern), `\textcite` (im Satz), `\footcite` (Fußnote), `\autocite` (folgt dem Stil). Seitenangaben kommen ins optionale Argument.
- **Stile:** `numeric` bzw. `numeric-comp` (Naturwissenschaft, Technik) · `authoryear` (Sozial- und Wirtschaftswissenschaft) · `authortitle`/`verbose` (Fußnotenzitate, Geisteswissenschaft) · `alphabetic` (Informatik) · `ieee` (Paket `biblatex-ieee`) · `apa` (Paket `biblatex-apa`). Die `ext-*`-Varianten (`biblatex-ext`) erlauben Feinheiten. Den Stil legt die Hochschule oder der Verlag fest; nie raten, sondern nachfragen.
- `.bib`-Einträge: eindeutige, sprechende Keys (`nachname2021stichwort`), `author = {Müller, Anna and Schmidt, Ben}`. Titel in `title = {…}`; Großschreibung, die bleiben muss, in zusätzliche Klammern (`{DNA}`). Online-Quellen als `@online` mit `url` und `urldate`.
- Für deutsche Arbeiten `\usepackage{csquotes}` laden. biblatex setzt Anführungszeichen dann sprachrichtig.

### Abbildungen und Tabellen

```latex
\usepackage{graphicx, booktabs, float, caption}
\begin{table}[htbp]
  \centering
  \caption{Laufzeit je Verfahren}\label{tab:laufzeit}
  \begin{tabular}{lrr}
    \toprule
    Verfahren & Mittel (ms) & Std.-Abw. \\
    \midrule
    A & 12,4 & 0,8 \\
    B & 9,7  & 1,1 \\
    \bottomrule
  \end{tabular}
\end{table}
```

- `booktabs`: nur `\toprule`, `\midrule`, `\bottomrule`, keine senkrechten Linien und keine doppelten Striche.
- Tabellenbeschriftung **über**, Abbildungsbeschriftung **unter** dem Inhalt.
- Platzierung `[htbp]` ist die Vorgabe. `[H]` (Paket `float`) erzwingt die Stelle; das nur sparsam einsetzen, sonst entstehen große Weißräume.
- Breite Tabellen mit `tabularx` (`X`-Spalten) statt `\resizebox`. Zahlen in Spalten mit `siunitx` (`S`-Spalte) am Komma ausrichten.
- Grafiken als Vektor (PDF/SVG→PDF) oder hochaufgelöst (PNG ≥ 300 dpi) einbinden, Größe relativ angeben: `\includegraphics[width=0.8\linewidth]{bilder/aufbau}`.

### Querverweise mit cleveref

```latex
\usepackage{hyperref}
\usepackage[ngerman, capitalise, noabbrev]{cleveref}  % nach hyperref laden
… wie \cref{tab:laufzeit} zeigt, … siehe \cref{sec:methode,fig:aufbau}.
```

- `\label` steht **nach** `\caption` bzw. direkt nach `\section{…}`.
- Präfixe halten Marken lesbar: `sec:`, `fig:`, `tab:`, `eq:`, `lst:`, `app:`.
- `cleveref` setzt das Wort („Tabelle 3“, „Abschnitte 2 und 4“) selbst. Deshalb nie „Tabelle \ref{…}“ von Hand schreiben.
- Ladereihenfolge: `hyperref` fast zuletzt, `cleveref` danach.

## 2. Präsentationen mit Beamer

- Jede Folie trägt eine **Aussage als Titel**, also einen ganzen Satz statt eines Themenworts: „Variante B halbiert die Wartezeit“ statt „Ergebnisse“. Die Storyline davor folgt dem Skill `business-writing-pyramid`. Erst kommt die Kernaussage, dann die tragenden Aussagen, und jede tragende Aussage wird zu einer oder mehreren Folien.
- Eine Folie, ein Gedanke. Stichpunkte höchstens drei bis fünf, lieber eine Grafik.
- `\begin{frame}{Titel als Aussage}` … `\end{frame}`; Abschnitte mit `\section` erzeugen die Gliederung.
- Code auf Folien: Rahmen als `\begin{frame}[fragile]`.
- Schlichte Themes (`\usetheme{default}` oder `metropolis`), `\setbeamertemplate{navigation symbols}{}` blendet die Navigationssymbole aus.
- Aufdecken (`\pause`, `\only<2>`) nur, wo es den Gedankengang wirklich stützt.

## 3. Deutsch und Typografie

- **Sprache:** robuste Vorgabe ist `\usepackage{babel}` plus `\babelprovide[import,main]{german}` (bzw. `{english}`). Das lädt die Sprache aus babels `.ini`-Dateien und braucht kein `ngerman.ldf`, das auf schlanken TeX-Installationen oft fehlt (Folge sonst: Abbruch oder englische Trennung im deutschen Text). `\usepackage[ngerman]{babel}` geht, wenn `texlive-lang-german` installiert ist; `polyglossia` ist die Alternative. Nicht babel und polyglossia zusammen laden. Mit pdfLaTeX zusätzlich `\usepackage[T1]{fontenc}`; `inputenc` ist seit 2018 unnötig.
- **KOMA-Script:** `scrartcl` (Artikel, Paper), `scrreprt` (Thesis ohne Doppelseite-Logik), `scrbook` (Buch, doppelseitig). Nützliche Optionen: `paper=a4`, `fontsize=11pt`, `parskip=half` (Absatz mit Abstand statt Einzug), `DIV=12` (Satzspiegel), `bibliography=totoc`, `listof=totoc`.
- **microtype:** `\usepackage{microtype}` verbessert den Randausgleich und reduziert Trennungen. Immer laden.
- **Anführungszeichen:** `\usepackage[autostyle]{csquotes}` und `\enquote{…}`. Nie `"` oder ASCII-Anführungen von Hand setzen.
- **Trennung:** Bei unbekannten Wörtern hilft `\hyphenation{Mess-rei-he Da-ten-bank}` in der Präambel. Wörter mit Bindestrich trennt `"=` (babel) sauber: `Ein"=Ausgabe`. Harte Umbrüche (`\\`, `\newline`) gehören nicht in Fließtext.
- **Kleinigkeiten:** geschützter Abstand vor Einheiten und Verweisen (`S.~12`, `\cref{…}`), Abkürzungen mit schmalem Abstand (`z.\,B.`, `d.\,h.`), Gedankenstrich `--`, Auslassung `\dots`. Zahlen und Einheiten setzt `siunitx`.

## 4. Mathe und Diagramme

- `\usepackage{amsmath, mathtools}` (`mathtools` lädt `amsmath` mit) und für Symbole `amssymb`. Bei XeLaTeX mit `unicode-math` entfällt `amssymb`, siehe Skill `xelatex-compile`.
- Abgesetzte Formeln mit `equation` (eine Zeile, nummeriert), `align` (mehrere, an `&` ausgerichtet) und `gather`. Nie `$$ … $$` und nie `eqnarray` verwenden.
- Eigene Operatoren: `\DeclareMathOperator{\argmax}{arg\,max}`. Wiederkehrende Notation als Makro in der Präambel (`\newcommand{\R}{\mathbb{R}}`).
- **siunitx:** `\qty{3,5}{\milli\second}`, `\num{12345}`, `\sisetup{locale=DE}` für Dezimalkomma. Bei älteren Installationen heißen die Befehle `\SI` statt `\qty`.
- **TikZ:** `\usepackage{tikz}` plus gezielte `\usetikzlibrary{arrows.meta, positioning}`. Knoten relativ platzieren (`right=of a`), statt absolute Koordinaten zu raten.
- **pgfplots:** `\usepackage{pgfplots}` mit `\pgfplotsset{compat=1.18}`. Daten aus Datei: `\addplot table[x=t, y=v, col sep=comma]{daten/messung.csv};`. Achsenbeschriftung immer mit Einheit.
- Große Diagramme in eigene Dateien (`abbildungen/verlauf.tex`) auslagern und per `\input` einbinden.

## 5. Minimalvorlagen

**Artikel (`scrartcl`)**

```latex
\documentclass[paper=a4, fontsize=11pt, parskip=half]{scrartcl}
\usepackage{babel}
\babelprovide[import,main]{german}
\usepackage{microtype, csquotes, graphicx, booktabs}
\usepackage[backend=biber, style=authoryear]{biblatex}
\addbibresource{literatur.bib}
\usepackage{hyperref}
\usepackage[ngerman, capitalise]{cleveref}
\title{Titel als Aussage}
\author{Vorname Nachname}
\begin{document}
\maketitle
\section{Einleitung}\label{sec:einleitung}
Text mit Zitat \parencite{beispiel2024}.
\printbibliography
\end{document}
```

**Thesis (`scrreprt`)**: wie oben, aber `\documentclass[paper=a4, fontsize=12pt, DIV=12, bibliography=totoc]{scrreprt}`, danach `\tableofcontents`, Kapitel als `\chapter` und Dateien per `\include{kapitel/…}`.

**Buch (`scrbook`)**: `\documentclass[paper=a4, twoside, open=right]{scrbook}` mit `\frontmatter`, `\mainmatter`, `\backmatter`.

**Folien (`beamer`)**

```latex
\documentclass[aspectratio=169]{beamer}
\usepackage[ngerman]{babel}
\usepackage{csquotes, booktabs}
\setbeamertemplate{navigation symbols}{}
\title{Kernaussage des Vortrags}
\begin{document}
\begin{frame}\titlepage\end{frame}
\begin{frame}{Variante B halbiert die Wartezeit}
  \begin{itemize}
    \item Messung über vier Wochen
    \item Median von 8 auf 4 Minuten
  \end{itemize}
\end{frame}
\end{document}
```

**Literaturdatei (`literatur.bib`)**

```bibtex
@book{beispiel2024,
  author    = {Muster, Erika},
  title     = {Ein Beispieltitel},
  publisher = {Verlag},
  location  = {Berlin},
  year      = {2024},
}
```

## 6. `.latexmkrc` für Builds von Hand

Diese Datei liegt neben der Hauptdatei und steuert `latexmk`, wenn die Nutzerin selbst baut. Das Werkzeug `latex.build` ignoriert sie bewusst (`-norc`), weil eine `.latexmkrc` beliebigen Perl-Code ausführen kann.

```perl
$pdf_mode = 5;              # 5 = XeLaTeX, 4 = LuaLaTeX, 1 = pdfLaTeX
$bibtex_use = 2;            # biber/bibtex ausführen und Hilfsdateien aufräumen
$out_dir = 'build';         # Hilfsdateien getrennt ablegen (optional)
@default_files = ('main.tex');
$clean_ext = 'bbl run.xml';
```

**Kompilier-Hinweis für die Nutzerin:** `latexmk -xelatex main.tex` (bzw. `-pdf` für pdfLaTeX, `-lualatex` für LuaLaTeX) im Verzeichnis der Hauptdatei. Ohne latexmk: `xelatex main.tex` zweimal, mit Literatur `xelatex main`, `biber main`, `xelatex main`, `xelatex main`. Als Alternative ohne TeX-Installation lädt `tectonic main.tex` die Pakete bei Bedarf selbst. Aufräumen: `latexmk -c`.

## 7. Häufige Log-Fehler und ihre Ursache

Im Log (`main.log`) zuerst nach Zeilen suchen, die mit `!` beginnen. Mit `-file-line-error` stehen Fehler als `./datei.tex:42: …`. Die Zeile `l.42 …` darunter zeigt, wo TeX stehen blieb. Die Ursache liegt oft etwas davor.

| Meldung | Ursache | Lösung |
|---|---|---|
| `Undefined control sequence` | Tippfehler im Makro oder Paket fehlt | Schreibweise prüfen, Paket laden (Tabelle unten) |
| `LaTeX Error: File 'x.sty' not found` | Paket nicht installiert | Nutzerin bitten, das Paket zu installieren, oder darauf verzichten |
| `Missing $ inserted` | `_`, `^` oder ein Mathe-Makro außerhalb der Mathematik | als `\_` schreiben oder in `$…$` setzen |
| `Runaway argument?` / `File ended while scanning use of …` | geschweifte Klammer nicht geschlossen | Klammerbilanz prüfen (Abschnitt 8) |
| `\begin{x} on input line n ended by \end{y}` | Umgebungen verschachtelt oder falsch geschlossen | Paare prüfen |
| `Missing \begin{document}` | Text oder Zeichen in der Präambel | Streuzeichen entfernen |
| `Extra alignment tab has been changed to \cr` | mehr `&` als Spalten | Spaltenzahl der Tabelle anpassen |
| `Option clash for package x` | Paket zweimal mit verschiedenen Optionen geladen | einmal laden, Optionen zusammenführen |
| `Citation 'k' undefined` | Key fehlt in der `.bib` oder biber lief nicht | Key abgleichen, erneut bauen |
| `Reference 'x' undefined` / `There were undefined references` | `\label` fehlt oder ein Lauf fehlt | Marke anlegen, `latexmk` wiederholt selbst |
| `Overfull \hbox` | Zeile zu lang (URL, langes Wort, breite Tabelle) | umformulieren, `\url{}`, `tabularx`, Trennhinweis |
| `Package inputenc Error: Unicode character` (pdfLaTeX) | Zeichen ohne Definition | XeLaTeX nehmen oder Zeichen ersetzen |
| biber: `Cannot find 'main.bcf'` | LaTeX-Lauf davor gescheitert | ersten Fehler im `.log` beheben |
| biber: `Data source 'x.bib' not found` | Pfad in `\addbibresource` falsch | Pfad relativ zur Hauptdatei setzen, Endung angeben |

## 8. Prüfliste ohne Ausführung (vor jeder Übergabe)

Mit den Datei-Werkzeugen (lesen, suchen) jede geänderte Datei durchgehen. Kommentare (ab einem nicht maskierten `%`) zählen nicht.

1. **Klammerbilanz:** Je Datei öffnende und schließende geschweifte Klammern zählen, ohne `\{` und `\}`. Die Differenz muss 0 sein. Bei Abweichung abschnittsweise zählen, bis die Stelle gefunden ist. Dasselbe gilt für `[`/`]` in optionalen Argumenten und für `$` (gerade Anzahl je Absatz).
2. **Umgebungen:** Alle `\begin{…}` und `\end{…}` als Liste in Reihenfolge herausziehen (Suche nach `\\(begin|end)\{`). Wie bei einem Stapel muss jedes `\end` die zuletzt geöffnete Umgebung schließen. `document` öffnet genau einmal und schließt am Ende.
3. **Marken und Verweise:** Alle Marken aus `\label{…}` sammeln, dazu alle Ziele aus `\ref`, `\eqref`, `\pageref`, `\autoref`, `\cref`, `\Cref`. Kommagetrennte Listen aufteilen. Jedes Ziel braucht genau eine Marke, und keine Marke darf doppelt vorkommen. Unbenutzte Marken sind kein Fehler.
4. **Zitate gegen die `.bib`:** Alle Keys aus `\cite…{…}`, `\parencite`, `\textcite`, `\footcite`, `\autocite`, `\nocite` sammeln (Listen aufteilen, `*` bei `\nocite{*}` ignorieren). Jeder Key muss als `@typ{key,` in einer über `\addbibresource` eingebundenen Datei stehen. Groß- und Kleinschreibung zählt.
5. **Pakete für benutzte Makros:**

   | Makro/Umgebung | Paket |
   |---|---|
   | `\toprule`, `\midrule`, `\bottomrule` | `booktabs` |
   | `\includegraphics` | `graphicx` |
   | `\qty`, `\num`, `\SI`, Spaltentyp `S` | `siunitx` |
   | `\cref`, `\Cref` | `cleveref` |
   | `\href`, `\url`, `\autoref` | `hyperref` (`\url` auch `url`) |
   | `\enquote` | `csquotes` |
   | `\parencite`, `\textcite`, `\printbibliography`, `\addbibresource` | `biblatex` |
   | `align`, `gather`, `\eqref`, `\text` | `amsmath` |
   | `\coloneqq`, `\DeclarePairedDelimiter` | `mathtools` |
   | `\mathbb`, `\mathfrak` | `amssymb` (bzw. `unicode-math`) |
   | `tikzpicture` | `tikz` |
   | `axis` | `pgfplots` |
   | `tabularx`-Umgebung | `tabularx` |
   | Platzierung `[H]` | `float` |
   | `subfigure`-Umgebung, `\subcaption` | `subcaption` |
   | `\textcolor`, `\color` | `xcolor` |
   | `\setmainfont`, `\newfontfamily` | `fontspec` (nur XeLaTeX/LuaLaTeX) |
   | `lstlisting` | `listings` |

6. **Sonderzeichen im Fließtext:** `&`, `%`, `#`, `_`, `$`, `~`, `^` außerhalb von Mathematik und Befehlen maskieren (`\&`, `\%` …). Das betrifft besonders URLs, Dateinamen und Firmennamen.
7. **Dateien:** Alle Pfade aus `\input`, `\include`, `\includegraphics`, `\addbibresource` müssen im Workspace existieren (relativ zur Hauptdatei). Bei Grafiken ohne Endung prüfen, ob `.pdf` oder `.png` vorhanden ist.

8. **Installation:** Vor dem ersten Build `latex.check` auf die Hauptdatei anwenden. Meldet es fehlende Pakete oder Schriften, die `user_message` an die Nutzerin weitergeben und eine vorhandene Alternative wählen, statt auf gut Glück zu bauen.

Das Ergebnis dieser Prüfung kommt in die Übergabe: „statisch geprüft: Klammern/Umgebungen ok, 14 Verweise ok, 9 Zitate ok, Pakete vollständig“ oder die konkreten Befunde mit Datei und Zeile.
