# PL-93 — Typisierte Werkzeuge für häufige Kommandos

> **Status:** Crates gebaut und getestet, **nicht an Registry/Profile angebunden** (siehe §8, §10)  
> **Baseline:** `origin/dev@ee1bb10`, Branch `feat/common-command-tools`  
> **Regel:** Code ist die Quelle der Wahrheit (`../README.md` §2). Dieses Dokument beschreibt, was im Code steht; was dort nicht steht, steht in §10 „Nicht gebaut / Lücken“.

## 1. Ziel

Ein Agent soll für die häufigsten Kommandos (`ls`, `stat`, `find`, `du`, `ps`, `cargo test`, `git log`, …) **kein rohes Shell-Kommando** mehr tippen. Jedes Werkzeug hat

- **typisierte Argumente** (kurze und lange Optionen als benannte, validierte Felder; freie Strings nur dort, wo das Kommando selbst Text ist, z. B. ein Muster),
- **strikte Eingabe**: `#[serde(deny_unknown_fields)]` — eine unbekannte Option wird abgelehnt, nicht ignoriert,
- **begrenzte Ausgabe**: harte Obergrenzen für Einträge und Bytes, Feld `truncated`, deterministische Sortierung, kurze Zusammenfassung (`summary`) neben dem JSON.

## 2. Layout (vier Crates, Schicht `A`)

| Crate | Inhalt | Ausführung |
|---|---|---|
| `harw-tool-fsread` | Familie `fsread.*` (16 Werkzeuge: Dateisystem lesend, `diff`, `jq`-artig). Besitzt außerdem die gemeinsamen Bausteine `scope` (Workspace-Auflösung), `budget` (Ausgabegrenzen), `textdiff` | rein Rust |
| `harw-tool-sys` | Familie `sys.*` (12 Werkzeuge: Prozesse/System/Netz lesend, `date`, `which`) | rein Rust (`/proc`, `rustix`) |
| `harw-tool-cargo` | Familie `cargo.*` (8 Werkzeuge) | Fremdprozess **nur** über den injizierten `shell.exec`-Ausführer (siehe 4) |
| `harw-tool-gitread` | Familie `git.*` (6 Werkzeuge, lesend) | rein Rust (eigener Objekt-/Index-Leser) |

Nicht „ein Crate pro Kommando“ und nicht alles in einem: die Schnitte folgen den Ausführungswegen (Dateisystem / Host-Zustand / Fremdprozess / Git-Format).

## 3. Rechte-Stufe

Es gibt keine `Observer`-Berechtigung in `harw_authority::Permission`; Observer ist eine Agentenrolle. Maßgeblich ist die Permission des Tools (aus `#[harw_macros::tool(permission = …)]`, als `TOOL_PERMISSIONS` des Providers abrufbar):

| Familie | Permission | Begründung |
|---|---|---|
| `fsread.*` | `ReadWorkspace` | liest nur unterhalb der Workspace-Wurzel |
| `git.*` | `ReadWorkspace` | liest `<workspace>/.git` (muss innerhalb der Wurzel liegen) |
| `sys.*` | `ExecuteProcess` | Vorbild `process.list`: liest Host-Zustand außerhalb des Workspace, Kommandozeilen/Umgebungen können Geheimnisse enthalten |
| `cargo.*` | `ExecuteProcess` | wie `shell.exec`; der delegierte `shell.exec` prüft zusätzlich selbst |

Nichts schreibt außerhalb des Workspace; `cargo.*` schreibt nur `target/` (und die Metadata-Scratch-Datei `target/harw-tool-cargo/…`, die danach wieder entfernt wird) innerhalb der Sandbox des `shell.exec`-Weges.

## 4. Wie Fremdprozesse starten

`harw-tool-cargo` startet **keinen** Prozess selbst. Es baut aus den validierten Argumenten ein festes `argv`, quotet es POSIX-sicher zu einem Kommando-String und reicht ihn an einen injizierten `Arc<dyn ToolExecutor>` (`ShellDelegate`) weiter — im Betrieb der `shell.exec`-Ausführer aus `harw-tool-shell` (Sandbox, rlimits, Host-Permit-Prüfung, Zeitlimit, Ausgabekappung). Es gibt keinen eigenen `Command`-/`spawn`-Pfad in den vier Crates; ein Test je Crate prüft die Quelltexte darauf (Prozessstarts gibt es nur in `tests/`, für den Vergleich mit den echten Programmen). `--`-Argumente (Testharness, Clippy-Lints) nur über eine Allowlist bzw. als Lint-Namen.

`rg`/`grep` werden **nicht** gebaut: `fs.grep` (`harw-tool-fs`) deckt Regex, `-i`, Kontext, Glob und Limit bereits ab.

## 5. Werkzeugliste (Feldnamen = Rust-Felder der `*Args`-Strukturen)

Gemeinsam: Pfade relativ zur Workspace-Wurzel; `..`-Ausbruch → Fehler; Symlinks werden nie blind gefolgt (jedes Pfadglied wird mit `openat2(RESOLVE_BENEATH|NO_SYMLINKS)` bzw. dirfd-relativ geöffnet). `follow` (`-L`) löst Symlinks selbst auf und lehnt jedes Ziel außerhalb der Wurzel ab. Alle Felder außer den genannten Pflichtfeldern sind `Option<…>`.

### 5.1 `fsread.*`

| Tool | Argumente | Anmerkung |
|---|---|---|
| `fsread.ls` | `path`, `all` (-a), `almost_all` (-A), `long` (-l), `human` (-h), `recursive` (-R), `sort_size` (-S), `sort_time` (-t), `reverse` (-r), `sort`, `group_directories_first`, `follow` (-L), `max_depth`, `limit` | lange Form: Typ, Rechte, Links, uid/gid, Größe, mtime, Name, Symlink-Ziel |
| `fsread.stat` | `paths[]`, `follow` (-L), `format` | Typ, Größe, Rechte (oktal + `rwx`), uid/gid, Zeiten, Inode, Links, Blöcke, dev |
| `fsread.find` | `path`, `name`, `iname`, `path_glob`, `kind`, `min_size`, `max_size`, `modified_within_secs`, `modified_before_secs`, `min_depth`, `max_depth`, `empty`, `limit` | sortiert nach Pfad |
| `fsread.du` | `path`, `summarize` (-s), `apparent_size`, `human`, `max_depth` (-d), `all` (-a), `limit` | Blöcke oder scheinbare Größe, Hardlinks einmal gezählt |
| `fsread.df` | `path`, `human` | `statvfs` des Dateisystems unter `path` (ohne Mountpunkt) |
| `fsread.wc` | `paths[]`, `lines`, `words`, `bytes`, `chars`, `max_line_length` | UTF-8 tolerant |
| `fsread.head` / `fsread.tail` | `path`, `lines` (-n), `bytes` (-c); `tail` zusätzlich `from_line` (`+N`) | Obergrenzen; `tail` liest vom Dateiende |
| `fsread.cat` | `path`, `number_lines` (-n), `offset`, `max_bytes` | begrenzt; Binärdateien werden gemeldet statt gedruckt |
| `fsread.file` | `paths[]` | Magic-Erkennung |
| `fsread.tree` | `path`, `max_depth`, `dirs_only`, `all`, `limit` | gerendertes Baumtext plus JSON-Liste |
| `fsread.readlink` | `path`, `canonicalize` (-f) | aufgelöstes Ziel nur innerhalb der Wurzel |
| `fsread.realpath` | `path` | kanonischer Pfad innerhalb der Wurzel |
| `fsread.hash` | `paths[]`, `algorithm` (`sha256`/`blake3`) | `sha256sum`/`b3sum`; Größe begrenzt (64 MiB) |
| `fsread.diff` | `a`, `b`, `context` (-U), `ignore_whitespace` (-w), `ignore_case` (-i), `brief` (-q) | Unified Diff (eigene Myers-Implementierung) |
| `fsread.json` | `path`, `query` (`.a.b[0]["x"]`), `op` (`keys`/`length`/`type`) | `jq`-artiger Pfadzugriff, kein Filter-Interpreter |

### 5.2 `sys.*`

| Tool | Argumente |
|---|---|
| `sys.ps` | `pids[]`, `ppids[]`, `user`, `name`, `name_contains`, `command_contains`, `states[]`, `sort`, `reverse`, `fields[]`, `limit`, `tree` |
| `sys.pgrep` | `pattern` (Pflicht), `full` (-f), `exact` (-x), `ignore_case` (-i), `user`, `ppids[]`, `newest`, `oldest`, `limit` |
| `sys.top` | `limit`, `sort` (`cpu`/`mem`), `interval_ms` (100–2000) |
| `sys.free` | `human` |
| `sys.uptime` | – |
| `sys.uname` | `all`, `kernel_name`, `nodename`, `kernel_release`, `kernel_version`, `machine`, `operating_system` |
| `sys.env` | `names[]`, `prefix`, `name_contains`, `limit` — ein „reveal“ gibt es **nicht**; Werte mit KEY/TOKEN/SECRET/PASSWORD/… und tokenartige Werte sind maskiert, Filter wirken nie auf Werte |
| `sys.id` | `user`, `group`, `groups`, `name`, `real`, `whoami` |
| `sys.ss` | `tcp`, `udp`, `unix`, `listening`, `all`, `ipv4`, `ipv6`, `port`, `limit` |
| `sys.lsof` | `pid` (Pflicht; nur eigener Benutzer), `limit` |
| `sys.date` | `format` (strftime-Teilmenge), `offset_minutes`, `epoch_secs` |
| `sys.which` | `names[]`, `all` |

### 5.3 `cargo.*` (Sandbox-Pfad)

`cargo.check`, `cargo.build`, `cargo.clippy`, `cargo.test`, `cargo.fmt_check`, `cargo.tree`, `cargo.doc`, `cargo.metadata`.
Gemeinsame typisierte Argumente: `package[]` (-p), `workspace`, `exclude[]`, `features[]`, `all_features`, `no_default_features`, Ziel-Auswahl (`all_targets`, `lib`, `bins`, `tests`, …), `release`, `profile`, `locked`, `offline`, `jobs`, `timeout_secs`, `max_diagnostics`; ein `target_dir` wird nie angeboten. Je Tool: `cargo.test` `test_filter`, `no_run`, `doc`, `no_fail_fast`, `harness_args[]` (nur Allowlist: `--nocapture`, `--show-output`, `--ignored`, `--include-ignored`, `--exact`, `-q`, `--test-threads=N`, `--skip=NAME`); `cargo.clippy` `no_deps`, `deny[]`/`warn[]`/`allow[]` (nur Lint-Namen, `-D|-W|-A`); `cargo.fmt_check` `package[]`, `all`; `cargo.tree` `invert`, `depth`, `duplicates`, `edges`, `prefix`; `cargo.doc` `no_deps`, `document_private_items`, `lib`, `bins`; `cargo.metadata` `no_deps`, `features`, `all_features`, `no_default_features`.
Ausgabe: das Tool setzt das Kurzformat (`--message-format=short`) selbst; die Ausgabe wird zu `diagnostics[{level, code, message, file, line, column}]`, Testzählern, Dauer und gekürztem Rohlog zusammengefasst. `cargo.metadata` liest eine Scratch-Datei statt der 64-KiB-gekappten Standardausgabe und nennt nie absolute Pfade.

### 5.4 `git.*` (rein Rust, `ReadWorkspace`)

| Tool | Argumente | Anmerkung |
|---|---|---|
| `git.status` | `untracked` (`normal`/`all`/`no`), `ignored`, `paths[]`, `limit` | Zweig, Upstream (Vorsprung/Rückstand), staged/unstaged/Konflikt/untracked/ignored, laufende Operation (merge/rebase/cherry-pick/revert/bisect) |
| `git.diff` | `base`, `target`, `staged`, `paths[]`, `format` (`patch`/`stat`/`name_only`/`name_status`), `context`, `ignore_whitespace`, `max_files` | Index↔Arbeitsverzeichnis, HEAD↔Index (`staged`), Commit↔Arbeitsverzeichnis/Index (`base`), Commit↔Commit (`base`+`target`) |
| `git.log` | `rev` (auch `A..B`), `max_count`, `skip`, `author`, `grep`, `since`, `until`, `paths[]`, `first_parent`, `no_merges` | Reihenfolge wie `git log`; Pfadfilter wie `--full-history` |
| `git.show` | `rev` (`<rev>` oder `<rev>:<pfad>`), `format`, `diff`, `context`, `ignore_whitespace`, `paths[]`, `max_files` | Commit (Diff gegen ersten Elternteil), Tag, Baum, Blob |
| `git.branch` | `remotes`, `tags`, `pattern`, `limit` | lokale Zweige mit Spitze/Betreff/Upstream/Vorsprung/Rückstand; nur lesend |
| `git.blame` | `path` (Pflicht), `rev`, `start_line`, `end_line`, `max_lines` | committeter Stand von `rev`; Myers je Elternteil |

Eigener Leser für Refs (lose + `packed-refs`), Objekte (lose + Pack-Index v2 inkl. OFS-/REF-Delta mit Größen- und Tiefengrenzen), Index (v2/v3/v4 mit Prüfsumme, Intent-to-add, Skip-Worktree, assume-valid, Konflikt-Stages), `.gitignore`/`info/exclude` (über die Crate `ignore`), Commit-/Tree-/Tag-Parser. Schreibende Git-Pfade gibt es **nicht**.

## 6. Ausgabegrenzen (Standard / hart)

JSON-Antwort höchstens rund 64 KiB (`MAX_OUTPUT_BYTES`; bei `git.diff`/`git.show` Patch-Text 40 KiB + Dateiliste 12 KiB), Einträge je Tool 200–500 Standard, hart 5 000 (`git.log` 500, `git.diff` 2 000 Dateien, `git.blame` 2 000 Zeilen); Walks: höchstens 100 000 besuchte Einträge, 5 s (`git.status`: 500 000 Einträge, 20 s Frist je Aufruf); Dateilesen höchstens 8 MiB je Datei (Hash 64 MiB, Git-Objekte 64 MiB, Diff-Seiten 1 MiB). Überschreitung setzt `truncated: true` und einen Grund (`stopped`/`stop_reason`). Sortierung ist immer deterministisch.

## 7. Sicherheit

- Pfadauflösung nur über `scope`; abgelehnt werden `..`, absolute Pfade außerhalb, Symlinks im Pfad, `follow` auf Ziele außerhalb. `git.*` akzeptiert nur ein `.git` **innerhalb** der Wurzel (eine `.git`-Datei mit `gitdir:` nach außen, ein Symlink als `.git` oder ein Index mit `..`/`.git`-Pfaden wird abgelehnt).
- Geheimnisse: Pfade der Denylist (`.ssh`, `.gnupg`, `.env*`, `*.pem`/`*.key`/`*.p12`, `auth.toml`, `secrets/` …) werden von `fsread.*` nie gelesen und von `git.*` nie **ausgegeben** (Diff/Show/Blame zeigen nur den Namen; für Vergleiche wird der Inhalt gehasht, nie ausgegeben). Remote-URLs (mögliche Zugangsdaten) gibt kein Git-Werkzeug aus.
- `sys.env`: Wertemaskierung nach Namensmuster und Wertform; `sys.ps`/`sys.pgrep`/`sys.top` maskieren Kommandozeilen; `sys.lsof` nur eigene Prozesse, nur Namen.
- `unsafe` verboten; keine `unwrap`/`expect`/`panic` im Produktivcode (per Skript geprüft; Treffer nur in Fehlermeldungstexten mit dem Wort „unsafe“).
- Mutationsproben (Prüfung entfernen → Test rot → zurück) für Scope-Prüfung, Geheimnis-Maskierung/-Auslassung und Limits je Crate; Ergebnisse im Abschlussbericht, äquivalente Mutanten dort benannt.

## 8. Anbindung (offen)

**Nicht erfolgt.** Vorgesehen sind `capability_catalog.rs` (Provider + Zeilen, Feature im Runner), `profile.rs` (Tool-Namenslisten + Provider-Registrierung), `authority.rs` (Permission-Tabelle) und `harw-agent-runner` (Features), nach dem Muster von `ProcessToolProvider`. Jeder Provider stellt dafür `TOOL_NAMES` und `TOOL_PERMISSIONS` bereit (`FsreadToolProvider`, `SysToolProvider`, `CargoToolProvider`, `GitReadToolProvider`; `CargoToolProvider` braucht den `shell.exec`-Ausführer als Abhängigkeit). Das berührt Goldentests und Profile quer durch `harw-registry-defaults` und ist ein eigener Zyklus.

## 9. Nutzungs-Skills (PL-95)

Je Werkzeuggruppe liegt eine kurze Nutzungs-Skill-Datei im Crate: `<crate>/skills/<name>/skill.toml` + `instructions.md` (Abschnitte „Wann welches Werkzeug“, „Ausgabe lesen“, „Grenzen und `truncated`“), Namen `fsread-tools`, `sys-tools`, `cargo-tools`, `git-read-tools`. Die Manifeste nutzen nur die heutigen Felder (`name`, `description`, `instructions_file`, `tools`); `when`/`[triggers]` kommen mit PL-95. Die Einbindung in den Skill-Index (`harw-home/assets/skills`) folgt in Zyklus S4.

## 10. Nicht gebaut / Lücken (gegen den Code abgeglichen)

- `rg`/`grep`: vorhanden als `fs.grep`.
- Schreibende Git-Befehle, `git log --graph`, `git blame -C/-M` und Verfolgung von Umbenennungen, Umbenennungserkennung in `git.diff`/`git.status` (Umbenennen = gelöscht + neu), Historienvereinfachung bei `git.log` mit `paths` (es gilt `--full-history`), `A...B`, Reflog-Ausdrücke (`@{…}`), Funktionskontext hinter `@@`.
- Git-Leser: kein geteilter Index (`link`) und kein Sparse-Index (klare Fehlermeldung), keine `core.autocrlf`-/Attribut-/Clean-Filter (Dateien können als geändert erscheinen), Submodule nur als Gitlink, keine `core.excludesFile`-Datei außerhalb des Repositories, SHA-256-Repositories, `.git` außerhalb der Wurzel (verknüpfte Worktrees), Packs ohne `.idx` v2, keine Reverse-Index-/Bitmap-Nutzung.
- `top` als dauerhaft laufende Ansicht (nur Schnappschuss); `lsof` für fremde Benutzer; `ss -p`; `uname -p/-i`; `df` ohne Mountpunkt.
- `jq`-Filtersprache (nur Pfadzugriff).
- `cargo fetch`/`update`/`add`/`install`/`publish`, `clippy --fix`, `doc --open`; `cargo.clippy` kennt Lint-Namen nur als Zeichenmuster, nicht gegen eine Liste.
- Anbindung an Registry/Profile/Katalog (§8) und Skill-Index (§9).
