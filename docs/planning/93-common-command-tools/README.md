# PL-93 — Typisierte Werkzeuge für häufige Kommandos

> **Status:** IN PROGRESS (Zyklen werden einzeln committet)  
> **Baseline:** `origin/dev@ee1bb10`  
> **Regel:** Code ist die Quelle der Wahrheit (`../README.md` §2). Dieses Dokument beschreibt Absicht; was nicht im Code steht, ist nicht gebaut. Der Abschnitt „Nicht gebaut" wird am Ende gegen den Code abgeglichen.

## 1. Ziel

Ein Agent soll für die häufigsten Kommandos (`ls`, `stat`, `find`, `du`, `ps`, `cargo test`, `git log`, …) **kein rohes Shell-Kommando** mehr tippen. Jedes Werkzeug hat

- **typisierte Argumente** (kurze und lange Optionen als benannte, validierte Felder; freie Strings nur dort, wo das Kommando selbst Text ist, z. B. ein Muster),
- **strikte Eingabe**: `#[serde(deny_unknown_fields)]` — eine unbekannte Option wird abgelehnt, nicht ignoriert,
- **begrenzte Ausgabe**: harte Obergrenzen für Einträge und Bytes, Feld `truncated`, deterministische Sortierung, kurze Zusammenfassung (`summary`) neben dem JSON.

## 2. Layout (vier Crates, Schicht `A`)

| Crate | Inhalt | Ausführung |
|---|---|---|
| `harw-tool-fsread` | Familie `fsread.*` (Dateisystem lesend, `diff`, `jq`-artig). Besitzt außerdem die gemeinsamen Bausteine `scope` (Workspace-Auflösung), `budget` (Ausgabegrenzen), `textdiff` | rein Rust |
| `harw-tool-sys` | Familie `sys.*` (Prozesse/System/Netz lesend, `date`, `which`) | rein Rust (`/proc`, `rustix`) |
| `harw-tool-cargo` | Familie `cargo.*` | Fremdprozess **nur** über den vorhandenen `shell.exec`-Ausführer (siehe 4) |
| `harw-tool-gitread` | Familie `git.*` (lesend) | rein Rust (eigener Objekt-/Index-Leser) |

Nicht „ein Crate pro Kommando" und nicht alles in einem: die Schnitte folgen den Ausführungswegen (Dateisystem / Host-Zustand / Fremdprozess / Git-Format).

## 3. Rechte-Stufe

Es gibt keine `Observer`-Berechtigung in `harw_authority::Permission`; Observer ist eine Agentenrolle. Maßgeblich ist die Permission des Tools:

| Familie | Permission | Begründung |
|---|---|---|
| `fsread.*` | `ReadWorkspace` | liest nur unterhalb der Workspace-Wurzel |
| `git.*` | `ReadWorkspace` | liest `<workspace>/.git` |
| `sys.*` | `ExecuteProcess` | Vorbild `process.list`: liest Host-Zustand außerhalb des Workspace, Kommandozeilen/Umgebungen fremder Prozesse können Geheimnisse enthalten. Erscheint damit nur in Profilen, die ohnehin Prozesse ausführen dürfen |
| `cargo.*` | `ExecuteProcess` | wie `shell.exec`; der delegierte `shell.exec` prüft zusätzlich selbst |

Keines der Werkzeuge gehört in `AUTO_APPROVED_TOOLS`, außer den rein workspace-lesenden (`fsread.*`, `git.*`), die wie `fs.read` behandelt werden. Nichts schreibt außerhalb des Workspace; `cargo.*` schreibt nur `target/` und den Cargo-Cache **innerhalb der Sandbox** des `shell.exec`-Weges.

## 4. Wie Fremdprozesse starten

`harw-tool-cargo` startet **keinen** Prozess selbst. Es baut aus den validierten Argumenten ein festes `argv`, quotet es POSIX-sicher zu einem Kommando-String und reicht ihn an einen injizierten `Arc<dyn ToolExecutor>` (`ShellDelegate`) weiter — im Betrieb der `shell.exec`-Ausführer aus `harw-tool-shell` (bwrap-Sandbox, rlimits, Host-Permit-Prüfung, Zeitlimit, Ausgabekappung, Build-Timeout-Vorgabe). Es gibt keinen eigenen `Command`-/`spawn`-Pfad in den vier Crates (per Test auf den Quelltext abgesichert). `--`-Argumente (Testharness, Clippy-Lints) nur über eine Allowlist.

`rg` wird **nicht** gebaut: `fs.grep` (`harw-tool-fs`) deckt Regex, `-i`, Kontext, Glob und Limit bereits ab.

## 5. Werkzeugliste

Gemeinsam: Pfade relativ zur Workspace-Wurzel (absolute Pfade nur, wenn sie unter der Wurzel liegen); `..`-Ausbruch → Fehler; Symlinks werden nie blind gefolgt (jedes Pfadglied wird mit `openat2(RESOLVE_BENEATH|NO_SYMLINKS)` bzw. dirfd-relativ geöffnet). `follow` (`-L`) löst Symlinks selbst auf und lehnt jedes Ziel außerhalb der Wurzel ab.

### 5.1 `fsread.*` (Gruppe 1, 5)

| Tool | Argumente | Anmerkung |
|---|---|---|
| `fsread.ls` | `path`, `all` (-a), `almost_all` (-A), `long` (-l), `human` (-h), `recursive` (-R), `sort_size` (-S), `sort_time` (-t), `reverse` (-r), `sort` (`name\|size\|time\|extension\|none`), `group_directories_first`, `max_depth`, `limit` | lange Form: Typ, Rechte, Links, uid/gid, Größe, mtime, Name, Symlink-Ziel |
| `fsread.stat` | `paths[]`, `follow` (-L), `format` (`short\|long`) | Typ, Größe, Rechte (oktal + `rwx`), uid/gid, atime/mtime/ctime, Inode, Links, Blöcke, dev |
| `fsread.find` | `path`, `name` (Glob), `iname`, `path_glob`, `type` (`f\|d\|l\|other`), `min_size`/`max_size`, `newer_than_secs`/`older_than_secs` (mtime), `max_depth`, `min_depth`, `empty`, `limit` | sortiert nach Pfad |
| `fsread.du` | `path`, `summarize` (-s), `apparent_size`, `human`, `max_depth` (-d), `all` (-a), `limit` | Blöcke (`st_blocks*512`) oder scheinbare Größe, Hardlinks einmal gezählt |
| `fsread.df` | `path` | `statvfs` des Dateisystems unter `path` (Größe/frei/verfügbar, Inodes), `human` |
| `fsread.wc` | `paths[]`, `lines`/`words`/`bytes`/`chars`/`max_line_length` | UTF-8 tolerant (kaputtes UTF-8 zählt Bytes, `chars` nur gültige Folgen) |
| `fsread.head` / `fsread.tail` | `path`, `lines` (-n), `bytes` (-c), `from_line` (`+N`-Form für tail) | Obergrenzen; `tail` liest vom Dateiende |
| `fsread.cat` | `path`, `number_lines` (-n), `offset`, `max_bytes` | begrenzt; Binärdateien werden gemeldet statt gedruckt |
| `fsread.file` | `paths[]` | Magic-Erkennung (ELF, PNG, JPEG, GIF, PDF, ZIP, gzip, tar, Skript-Shebang, UTF-8/ASCII/Binär, leer, Verzeichnis, Symlink …) |
| `fsread.tree` | `path`, `max_depth`, `dirs_only`, `all`, `limit`, `sort` | gerendertes Baumtext plus JSON-Liste |
| `fsread.readlink` | `path`, `canonicalize` (-f) | zeigt Ziel; mit `canonicalize` aufgelöstes Ziel, **nur** wenn innerhalb der Wurzel |
| `fsread.realpath` | `path` | kanonischer Pfad innerhalb der Wurzel |
| `fsread.hash` | `paths[]`, `algorithm` (`sha256\|blake3`) | `sha256sum`/`b3sum`; Größe begrenzt (64 MiB) |
| `fsread.diff` | `a`, `b`, `context` (-U), `ignore_whitespace` (-w), `brief` (-q) | Unified Diff (eigene Myers-Implementierung), begrenzt |
| `fsread.json` | `path`, `query` (`.a.b[0]["x"]`), `limit` | `jq`-artiges Auslesen eines Pfads, kein Filter-Interpreter |

`grep`/`rg`: `fs.grep` (vorhanden). `cat`-Basis: `fs.read` (vorhanden); `fsread.cat` ergänzt Zeilennummern/Binär-Erkennung.

### 5.2 `sys.*` (Gruppe 2, 5)

| Tool | Argumente |
|---|---|
| `sys.ps` | `pids[]`, `ppids[]`, `user`, `name`, `states[]`, `sort` (`cpu\|mem\|start\|pid\|name`), `fields[]` (Allowlist), `limit`, `reverse`, `tree` |
| `sys.pgrep` | `pattern` (Regex gegen Name), `full` (-f, Kommandozeile), `user`, `newest`/`oldest`, `limit` |
| `sys.top` | `limit`, `sort` (`cpu\|mem`), `interval_ms` (zwei Messungen, max 2000) |
| `sys.free` | `human` |
| `sys.uptime` | – |
| `sys.uname` | `all`/`kernel_name`/`nodename`/`release`/`version`/`machine`/`os` |
| `sys.env` | `names[]`/`prefix`, `reveal` gibt es **nicht** | Werte mit KEY/TOKEN/SECRET/PASSWORD/… maskiert |
| `sys.id` | `user` (-u), `group` (-g), `groups` (-G), `name` (-n), `whoami` |
| `sys.ss` | `tcp`, `udp`, `unix`, `listening` (-l), `all` (-a), `port`, `limit` |
| `sys.lsof` | `pid` (nur eigener Benutzer), `limit` |
| `sys.date` | `format` (strftime-Teilmenge), `utc`, `epoch` |
| `sys.which` | `names[]`, `all` (-a) |

### 5.3 `cargo.*` (Gruppe 3, Sandbox-Pfad)

`cargo.check`, `cargo.build`, `cargo.test`, `cargo.clippy`, `cargo.fmt_check`, `cargo.metadata`, `cargo.tree`, `cargo.doc`.
Gemeinsame typisierte Argumente: `package[]` (-p), `workspace`, `exclude[]`, `features[]`, `all_features`, `no_default_features`, `all_targets`, `lib`, `bins`, `tests`, `release`, `profile`, `locked`, `offline`, `target_dir` **nicht** (wird nie angeboten), `timeout_secs`. Je Tool: `cargo.test` `test_filter`, `no_run`, `nocapture`/`test_threads` und `harness_args[]` (nur Allowlist: `--nocapture`, `--test-threads=N`, `--ignored`, `--include-ignored`, `--exact`, `--skip=…`, `--show-output`); `cargo.clippy` `no_deps`, `lint_args[]` (Allowlist `-D\|-W\|-A <lint>` mit Lint-Namensmuster); `cargo.metadata` `no_deps`, `format_version=1`; `cargo.tree` `invert`, `depth`, `duplicates`, `edges`; `cargo.doc` `no_deps`, `document_private_items`.
Ausgabe: `--message-format=json` wird vom Tool selbst gesetzt (nicht vom Aufrufer) und zu `diagnostics[{level, code, message, file, line, column}]`, Testzählern (`passed/failed/ignored/filtered`), Dauer und gekürztem Rohlog zusammengefasst.

### 5.4 `git.*` (Gruppe 4, rein Rust)

`git.status` (`short`/`branch`, `untracked`, `ignored`), `git.diff` (`staged`, `stat`, `name_only`, `name_status`, `paths[]`, `context`), `git.log` (`max_count` -n, `oneline`, `path`, `author`, `since_secs`), `git.show` (`rev`, `stat`, `name_only`), `git.branch` (`all` -a, `remote` -r, `verbose`), `git.blame` (`path`, `from_line`, `to_line`, `rev`). Eigener Leser für Refs (lose + `packed-refs`), Objekte (lose + Pack v2 inkl. Delta), Index (v2/v3), Commit-/Tree-Parser. Schreibende Git-Pfade werden **nicht** angeboten.

## 6. Ausgabegrenzen (Standard / hart)

Text-/JSON-Antwort höchstens 64 KiB (`MAX_OUTPUT_BYTES`), Einträge je Tool 200–500 Standard, hart 5 000; Walks: höchstens 100 000 besuchte Einträge, 5 s, Tiefe 32; Dateilesen höchstens 8 MiB je Datei (Hash 64 MiB). Überschreitung setzt `truncated: true` und `stopped` (Grund). Sortierung ist immer deterministisch (Bytefolge der Namen, bzw. dokumentierter Schlüssel + Name als Tiebreak).

## 7. Sicherheit

- Pfadauflösung nur über `scope` (siehe oben); Fehlerfälle: `..`, absolute Pfade außerhalb, Symlink im Pfad, `follow` auf Ziel außerhalb.
- Verweigerte Bereiche: `.git/` Schlüssel gibt es nicht, aber `.ssh`-Pfade, `<HARW_HOME>`-Schlüsselmaterial (`secrets/`, `*.key`, `auth.toml`) werden von `fsread.*` nie gelesen (Pfadname-Denylist, unabhängig vom Workspace).
- `sys.env`: Wertemaskierung nach Namensmuster; Werte, die wie Token aussehen, ebenfalls.
- `sys.ps`/`sys.lsof`/`sys.ss`: kein Schreibzugriff, `/proc`-Races (Prozess verschwindet) sind kein Fehler.
- `unsafe` verboten; keine `unwrap`/`expect`/`panic` im Produktivcode.
- Mutationsproben (Prüfung entfernen → Test rot → zurück) für: Scope-Prüfung, Geheimnis-Maskierung, Limits.

## 8. Anbindung

`capability_catalog.rs` (Provider + Zeilen, Feature im Runner), `profile.rs` (Tool-Namenslisten + Provider-Registrierung), `authority.rs` (Permission-Tabelle), `harw-agent-runner` (Features). Der Tool-Index (PR #89, `tool_index.rs`) und der Gap-Report (PR #88, `87-tool-gaps`) leiten Familie/Zusammenfassung aus Katalog und `ToolSpec`-Beschreibung ab: deshalb beginnt jede Beschreibung mit einem vollständigen ersten Satz „Use when …"-artig und Familien folgen dem Namenspräfix.

## 9. Nicht gebaut (Stand Planung; am Ende abgeglichen)

- `rg`/`grep`: vorhanden als `fs.grep`.
- schreibende Git-Befehle, `git blame` mit Umbenennungs-/Kopie-Erkennung (`-C`/`-M`), `git log --graph`.
- `top` als dauerhaft laufende Ansicht (nur Schnappschuss).
- `lsof` für fremde Benutzer, `ss -p` (Prozess-Zuordnung braucht Root).
- `jq`-Filtersprache (nur Pfadzugriff).
- Netzwerk-Variante von `cargo` (`fetch`/`update`/`add`).
