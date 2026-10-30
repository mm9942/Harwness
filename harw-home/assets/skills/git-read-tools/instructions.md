# git — Repository lesen ohne git-Prozess

**Regel:** Zum Ansehen von Stand und Historie die `git.*`-Werkzeuge nehmen. Sie lesen `.git` selbst (lose Objekte, Packs, Index, Refs), starten keinen Prozess und können nichts verändern: kein commit, add, checkout, reset, push.

## Wann welches Werkzeug

- **Was ist geändert?** `git.status` (Zweig, Vorsprung/Rückstand, staged/unstaged/untracked/Konflikte). Vor jedem Commit-Vorschlag und nach größeren Änderungen.
- **Wie sieht die Änderung aus?** `git.diff`: ohne Argumente Index gegen Arbeitsverzeichnis, `staged: true` für HEAD gegen Index, `base` für einen Commit gegen den Arbeitsstand, `base`+`target` für zwei Stände. `format: "stat"` oder `"name_status"` für einen Überblick, `paths` für eine Datei.
- **Wer hat wann was getan?** `git.log` (`max_count`, `author`, `grep`, `since`, `paths`, `rev: "A..B"`); `git.show` für einen Commit mit Diff oder `rev: "HEAD~2:src/lib.rs"` für eine Datei zu einem Zeitpunkt.
- **Woher kommt diese Zeile?** `git.blame` mit `path` und `start_line`/`end_line`.
- **Welche Zweige/Tags gibt es?** `git.branch` (`remotes`, `tags`, `pattern`).

## Ausgabe lesen

- JSON mit `summary`; Commits tragen `commit` (voll) und `short`, Daten als ISO-8601 mit Zeitzone des Autors.
- `git.diff`/`git.show`: `files[]` mit `status`, `additions`, `deletions` zuerst lesen, dann `patch`. Binärdateien erscheinen nur als „Binary files differ“.
- `git.status`: `entries[]` mit `area` (`staged`, `unstaged`, `conflict`, `untracked`, `ignored`) und `clean`. Ein ungetracktes Verzeichnis steht als `dir/`.
- `git.blame`: `lines[]` verweisen per `commit` (kurz) auf die Tabelle `commits`.

## Grenzen und `truncated`

- Patch-Text höchstens 40 KiB, Blobs 32 KiB, Listen begrenzt; `truncated`/`patch_truncated`/`files_truncated: true` heißt „mehr vorhanden“. Dann `paths` oder `max_count` verengen.
- **Geheimnisse:** Dateien wie `.env` und Schlüssel erscheinen in Listen mit Namen, **ihr Inhalt nie** (kein Diff, kein show, kein blame).
- Nicht unterstützt: Umbenennungserkennung (Löschen + Neu), Blame über Umbenennungen oder verschobenen Code, Historienvereinfachung bei `paths`, `autocrlf`/Attributfilter, geteilter/Sparse-Index, Repositories, deren `.git` außerhalb des Workspace liegt (verknüpfte Worktrees). Dann gibt es eine klare Fehlermeldung; nicht raten.
- Blame und Show betrachten den **committeten** Stand; ungespeicherte Änderungen zeigt `git.diff`.
