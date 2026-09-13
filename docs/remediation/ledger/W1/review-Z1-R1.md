# Review Z1-R1 — statischer Compile-Review Welle W1 (READ-ONLY)

Rolle: Ersatz für `cargo check`/`clippy -D warnings`. Zeilenweise gegen
Edition 2024, MSRV 1.85, `unsafe_code = "forbid"`, Workspace-Lints
(`[workspace.lints.rust] unsafe_code = "forbid"` — **kein**
`[workspace.lints.clippy]`, also nur die default-warn-Lints von clippy).

Geprüfter Stand: Arbeitsbaum (`git diff HEAD` + untracked) am 2026-09-13.
Umfang: W1-01 (session-store/memory/secrets + Manifeste), W1-02
(`harw-tool-fs/**`), W1-03 (`harw-tool-shell/**`, `harw-sandbox/src/bwrap.rs`),
W1-04 (`harw-ops/src/diff.rs`). Querbezüge zu `harw-core`,
`harw-registry-defaults`, `harw-fsutil` mitgeprüft.

**Ergebnis: kein Blocker.** Alle vier Arbeitspakete kompilieren
voraussichtlich (inkl. `--tests` und `--doc`). Gefunden: 0 blocker, 2 hoch,
5 mittel, 9 niedrig.

---

## Befunde

| ID | Schwere | Datei:Zeile | Befund | Fix-Snippet |
|----|---------|-------------|--------|-------------|
| R1-01 | hoch | `harw-ops/src/diff.rs:225-230` (`render_shell_output`) | **Querwirkung W1-03 → W1-04.** `shell.exec` tötet den Prozessbaum jetzt bei Überschreitung von `max_output_bytes` (64 KiB) per SIGKILL und meldet `exit_code = -1`, `killed_by_output_limit = true` (`harw-tool-shell/src/exec.rs:395-404`). `render_shell_output` prüft nur `exit_code != 0` und wirft dann `OpError::Execution("git diff exited with status -1")`. Vorher sammelte `wait_with_output` alles ein und kürzte danach → `exit_code = 0` plus gekürzter Diff. Damit **schlägt `/diff` auf jedem Workspace mit >64 KiB Diff jetzt hart fehl** statt einen gekürzten Diff zu liefern. Kein Test deckt das ab (weder `harw-ops` noch `harw-tool-shell`). | ```rust\nlet killed = object.get("killed_by_output_limit").and_then(Value::as_bool).unwrap_or(false);\nlet truncated = object.get("truncated").and_then(Value::as_bool).unwrap_or(false);\nif exit_code != 0 && !killed {\n    return Err(OpError::Execution(format!("git diff exited with status {exit_code}")));\n}\nlet note = if killed || truncated {\n    "\n[diff gekürzt: Ausgabegrenze von shell.exec erreicht]"\n} else { "" };\n// note an das gerenderte Ergebnis anhängen\n``` |
| R1-02 | hoch | `harw-ops/src/diff.rs:231-237` | `render_shell_output` ignoriert das `truncated`-Feld vollständig. Ein durch das Byte-Budget gekürzter Diff wird dem Aufrufer als vollständiger Diff präsentiert. Vorbestehend, durch W1-03 aber deutlich wahrscheinlicher (bisher nur weiche Kürzung, jetzt zusätzlich harte Kappung im `BoundedCapture`). | siehe R1-01 (`truncated` mit auswerten und im Text kennzeichnen). |
| R1-03 | mittel | `harw-tool-shell/src/limits.rs:66` + `harw-tool-shell/src/exec.rs:308-315` | `ShellLimits::default().require_rlimits = true` und `PRLIMIT_CANDIDATES = ["/usr/bin/prlimit"]`. Auf jedem Host/Container ohne util-linux-`prlimit` (oder mit `prlimit` unter `/bin`, `/usr/local/bin`) liefert `shell.exec` ab sofort **immer** `ToolOutput::error("… resource limits …")` — und damit fällt auch `/diff` (W1-04) aus. `harw-registry-defaults/src/profile.rs:696` baut `ShellToolProvider::default()`, setzt also nie `require_rlimits = false`. Fail-closed ist gewollt, aber der Kandidatenpfad ist zu eng. | `pub const PRLIMIT_CANDIDATES: [&str; 2] = ["/usr/bin/prlimit", "/bin/prlimit"];` — analog zu `BWRAP_CANDIDATES`; zusätzlich im Deployment-Doc als harte Voraussetzung vermerken. |
| R1-04 | mittel | `harw-sandbox/src/bwrap.rs:12` (`BWRAP_CANDIDATES`) + `:47-53` (`Default`) | `BwrapLauncher::default()` löst nicht mehr über `PATH` auf, sondern nur `/usr/bin/bwrap` bzw. `/bin/bwrap`. `harw-core/src/mcp_runtime.rs:186` und `:220` nutzen `BwrapLauncher::default()` für stdio-MCP-Spawns. Auf Hosts mit `bwrap` unter `/usr/local/bin` (Quellbau, Nix, Homebrew-artige Layouts) bricht der MCP-Start jetzt mit „not found“ ab. Gewollt (F-021), aber die Kandidatenliste deckt verbreitete Installationen nicht ab. | `pub const BWRAP_CANDIDATES: [&str; 3] = ["/usr/bin/bwrap", "/bin/bwrap", "/usr/local/bin/bwrap"];` (bleibt fest, `PATH` weiterhin nie ausgewertet). |
| R1-05 | mittel | `harw-memory/Cargo.toml:9`, `harw-secrets/Cargo.toml:15`, `harw-session-store/Cargo.toml:11` | Die drei Crates ziehen `harw-fsutil` **unbedingt**, obwohl `harw-fsutil` unix-only ist (`harw-fsutil/src/perm.rs:4` `use std::os::unix::fs::MetadataExt;` ohne `cfg`, `rustix` mit `fs`/`process`). Die in `store.rs`/`job_store.rs`/`file_store.rs`/`reader.rs`/`approval.rs` sorgfältig erhaltenen `#[cfg(not(unix))]`-Zweige sind damit toter Code: auf Nicht-Unix scheitert schon die Dependency. Entweder Gate oder Zweige entfernen. | ```toml\n[target.'cfg(unix)'.dependencies]\nharw-fsutil = { path = "../harw-fsutil" }\n``` |
| R1-06 | mittel | `harw-tool-fs/src/list.rs:57-64`, `harw-tool-fs/src/search.rs:78-88` | **Verhaltensänderung der Tool-Ausgabe:** `fs.list` liefert statt eines JSON-Arrays jetzt `{entries, stopped?}`, `fs.search` statt eines Arrays `{matches, stopped?, skipped_files?}`. Die Tool-*Beschreibungen* in `provider.rs:318-327`/`:373-383` sind konsistent nachgezogen (gut), aber alle Modell-Prompts, Evals und Golden-Files, die die alte Array-Form annehmen, brechen. In-Tree konsumiert niemand die Ausgabe (`harw_tool_fs` wird extern nur über `FsToolProvider` benutzt — geprüft), also kein Compile-Bruch. | Nur dokumentieren: Eintrag in `CHANGELOG.md` unter „breaking tool output“. |
| R1-07 | mittel | `harw-tool-shell/src/exec.rs:613-619` | `ShellToolProvider` hat ein neues **öffentliches** Feld `limits: ShellLimits`. Jede externe Struct-Literal-Konstruktion (`ShellToolProvider { timeout_secs, max_output_bytes }`) bricht. In-Tree nur `::new()`/`::default()` (registry-defaults:696, ops/diff.rs:266) — kein Bruch, aber semver-relevant für die öffentliche Fläche. | `#[non_exhaustive]` auf `ShellToolProvider` oder Builder-Methoden statt öffentlicher Felder. |
| R1-08 | niedrig | `harw-tool-fs/src/grep.rs:67` | `MAX_FILE_SIZE_BYTES` von 2 MiB auf 8 MiB (`MAX_SCAN_FILE_BYTES`) angehoben — Verhaltensänderung, im Ledger W1-02 genannt, aber die `pub const`-Doku im Modul nennt weiter nur „Log-Dumps oder Binär-Artefakte“ ohne den neuen Wert. | Doc-Kommentar um „8 MiB, gemeinsam mit `fs.search`“ ergänzen. |
| R1-09 | niedrig | `harw-tool-fs/Cargo.toml:21-24` | `tokio = { version = "1.53.0", features = ["rt"] }` in `[dependencies]` vs. `tokio = { version = "1", features = ["rt","macros"] }` in `[dev-dependencies]`. Der Lock löst beides auf 1.53.0 auf (`Cargo.lock:6185-6186`), also korrekt — aber die Pin-Konvention ist inkonsistent zu den anderen Crates. | Beide auf `"1.53.0"` (bzw. beide auf `"1"`) vereinheitlichen. |
| R1-10 | niedrig | `harw-tool-fs/src/tree.rs:376-383` (`load_ancestor_ignores`), `harw-tool-fs/src/read.rs:186-191` (`trim_partial_utf8`), `harw-tool-fs/src/grep.rs:326-330` | Verschachtelte `if let { if let … }` bzw. `if let { if … }`. `clippy::collapsible_if` schlägt ab clippy 1.88 (let-chains) hier zu — **aber** der MSRV-Gate greift, weil `rust-version = 1.85` im Workspace steht. Kein Fehler, solange die MSRV nicht angehoben wird. | Beim MSRV-Bump auf ≥ 1.88 zu let-chains umschreiben. |
| R1-11 | niedrig | `harw-tool-shell/src/capture.rs:153-166` | `endless_stdout_is_capped_without_eof` setzt voraus, dass das erste `read` ≥ 1001 Bytes liefert (`assert_eq!(capture.stdout().len(), 1001)`). Mit `duplex(64 KiB)` und 4096-Byte-Chunks gilt das praktisch immer, ist aber eine Timing-Annahme. | Robuster: `assert!(capture.stdout().len() <= 1001 && capture.limit_exceeded())` — oder vor `drain` einmal `tokio::task::yield_now().await`. |
| R1-12 | niedrig | `harw-tool-fs/src/blocking.rs:28-32` | Ein Panic im Blocking-Task wird zu `Ok(ToolOutput::error(…))`; `JoinError::is_cancelled()` (Runtime-Shutdown) landet in derselben Meldung wie ein echter Panic. Diagnostisch ununterscheidbar. | `if err.is_panic() { … "Panic" … } else { … "abgebrochen" … }`. |
| R1-13 | niedrig | `harw-tool-fs/src/tree.rs:1-48` | `harw-tool-fs` ist durch `tree.rs` (`std::os::unix::fs::MetadataExt`, `std::os::fd::AsRawFd`, `/proc/self/fd`) jetzt unix-only, ohne `#[cfg(unix)]`-Gates und ohne `[target.'cfg(unix)']` im Manifest. Vorher war das Crate portabel (`ignore`/`globset`). | Bewusst so? Dann im Crate-Doc von `lib.rs` als „Linux/Unix only“ vermerken. |
| R1-14 | niedrig | `harw-tool-fs/src/search.rs:132-134`, `harw-tool-fs/src/glob.rs:163-167` | Die Ausgabebudget-Schätzung (`MATCH_OVERHEAD_BYTES` 32 bzw. 4) ist heuristisch; `test_fs_search_output_limit_and_line_truncation` prüft nur `<= MAX_OUTPUT_BYTES + 1024`. Bei sehr langen Pfaden kann die echte JSON-Größe die Schätzung überschreiten. Kein Sicherheitsproblem (Zeilen sind auf 1024 B gekappt, Treffer auf 1000). | Optional: `serde_json::to_string(&hit).len()` statt Schätzung, oder Puffer im Test auf `+ 4096`. |
| R1-15 | niedrig | `harw-tool-shell/src/exec.rs:568` | `self.effective_timeout(&args)?;` in `execute` verwirft das Ergebnis; `run_command` berechnet es direkt danach erneut. Doppelte Validierung ist beabsichtigt (früher Abbruch vor der Permission-Prüfung), aber der verworfene Wert liest sich wie ein Versehen. | Kommentar „nur Validierung, Wert wird in `run_command` neu berechnet“ ist vorhanden — ok, alternativ `let _ = self.effective_timeout(&args)?;`. |
| R1-16 | niedrig | `harw-ops/tests/approval_declaration_gate.rs` (neu, W1-05-Gebiet) | Liegt im Crate meines W1-04-Umfangs, gehört inhaltlich zu W1-05. API-Prüfung bestanden: `harw_extension_api::{ToolCall, ToolName}` (lib.rs:18-21), `OperationRegistry::{new,iter,find_by_name}` (registry.rs:227/546/481), `OperationMeta.surfaces: Vec<Surface>` (operation.rs:480 — `for surface in &meta.surfaces` funktioniert nur, weil es ein `Vec` und kein `&'static [Surface]` ist), `ApprovalPolicy: Copy+PartialEq+Debug` (operation.rs:147), `ToolCallId: Default` (ids.rs:109), `harw_ops::{register_all, register_plan_tools, PLAN_TOOL_COUNT}` (lib.rs:205/301/247), `harw_registry_defaults::{AUTO_APPROVED_TOOLS, DefaultApprovalPolicy::requires_explicit_approval}` (lib.rs:96/161). Alle benutzten Crates sind `[dependencies]` von `harw-ops` und damit für `tests/` sichtbar — `harw-ops/Cargo.toml` braucht keine Änderung. | — (nur Bestätigung) |

---

## Explizit geprüft und **in Ordnung** (keine Befunde)

`harw-fsutil`-API exakt gegen `CONTRACTS.md §fsutil` und HEAD abgeglichen:
`OpenMode` (Copy, 7 pub Felder, Konstruktoren `read_only`/`read_write`/
`write_create_new`/`write_truncate`/`append_create`), `open_nofollow(&Path,
OpenMode) -> io::Result<File>`, `open_dir_nofollow`, `open_beneath(BorrowedFd,
&Path, OpenMode)`, `walk_beneath(&Path, WalkLimits)`, `WalkBeneath::stopped()`,
`write_atomic(&Path, &[u8], AtomicWriteOptions)`, `AtomicWriteOptions::{private,
with_mode}`, `ensure_private_regular`. Alle Aufrufstellen stimmen in Arität,
Typen und `&`/`&mut` überein.

- **MSRV 1.85**: `io::ErrorKind::FilesystemLoop` (`approval.rs:322`) ist seit
  1.83 stabil (`io_error_more`) — ok. Weiter geprüft und ok: `let … else`
  (1.65, inkl. Tupel-Pattern in `write.rs:141` und `exec.rs:365`),
  `std::pin::pin!` (1.68), `bool::then_some` (1.62), `is_some_and`/`is_ok_and`
  (1.70), `OnceLock` (1.70), `[T; N]::map` (1.55), `std::os::fd::AsRawFd`
  (1.66). Keine let-chains (1.88) im Code.
- **`?`-Konvertierungen**: `SessionStoreError: From<io::Error>` (error.rs:17),
  `FsToolError: From<io::Error>` (error.rs), `ShellExecError: From<io::Error>`
  (exec.rs:136), `ToolsError: From<serde_json::Error>` (error.rs:19) — alle
  vorhanden.
- **Borrow-Checker**: `tree.rs::walk_children` (`Entry<'a>` vereint die
  Lebensdauern von `dir`/`children`/`child_rel` auf den Schleifenkörper),
  `search.rs::Scanner`/`grep.rs::GrepRun` (mutabler Closure-Borrow endet per
  NLL vor dem Auslesen von `.matches`/`.hits`), `capture.rs::drain`
  (`tokio::select!` legt das Future-Tupel in einen inneren Block, die Borrows
  auf `stdout_buf`/`stderr_buf` enden vor den Handlern — der Standardfall für
  select!-in-Schleife), `read.rs`/`list.rs`/`search.rs`
  `Workspace::open(..).and_then(|ws| …)` (alle Rückgabewerte sind `File`/`Vec`,
  borgen `ws` nicht).
- **`Send`-Bounds**: `run_blocking<F: FnOnce() -> Result<ToolOutput, ToolsError>
  + Send + 'static>` — alle sechs Aufrufstellen übergeben ausschließlich
  Owned-Captures (`executor`, `context.clone()`, `call.clone()`, `root`,
  `args`). `ToolExecutorFuture<'a> = Pin<Box<dyn Future + Send + 'a>>` ist in
  allen vier handgeschriebenen `execute`-Impls und im
  `#[harw_macros::tool]`-Wrapper erfüllt (`&ToolExecutionContext: Send`, weil
  `ToolExecutionContext: Sync`).
- **`#[harw_macros::tool]`**: Das Makro emittiert `#func` unverändert
  (`harw-macros/src/tool.rs:474`), die Direktaufrufe `fs_glob(&ctx, args).await`
  / `fs_grep(&ctx, args).await` in den Tests kompilieren. `permission =
  "read_workspace"` + `parallel_safe` sind gültige Schlüssel (tool.rs:247-280).
- **Erschöpfende `match`**: `WalkStop` (tree.rs:279), `EntryType` (list.rs:148),
  `ignore::Match` (tree.rs:400), `StopReason` (tree.rs:96), `ToolOutput`
  (diff.rs:277), `DrainEnd` (exec.rs:382).
- **`bwrap::plan()`-Signatur unverändert** (`harw-sandbox/src/bwrap.rs:122-125`)
  → `harw-core/src/mcp_runtime.rs:94/127/220` kompiliert weiter; dort werden
  keine Executable-Namen assertiert.
- **`harw_tool_fs`-Nutzer außerhalb**: nur `harw-registry-defaults/src/
  profile.rs:46` über `FsToolProvider` — unverändertes `pub`-Item. Kein Bruch.
- **`write_atomic` über `/proc/self/fd/<fd>/<name>`**: funktioniert, weil
  `write_atomic` das Elternverzeichnis **ohne** `O_NOFOLLOW` öffnet
  (`atomic.rs:158`, `RDONLY|DIRECTORY|CLOEXEC`) — der Magic-Link wird also
  korrekt aufgelöst. `walk_beneath` bekommt dagegen `/proc/self/fd/<fd>/.`
  (`tree.rs:205`), damit `open_dir_nofollow` nicht am Magic-Link scheitert.
  Beide Pfade sind konsistent zur Moduldoku in `tree.rs:16-28`.
- **Tote `pub`-Items im privaten `mod tree`/`mod blocking`**: alle 22 geprüften
  Items (`HARD_MAX_*`, `WALK_DEADLINE`, `MAX_*`, `StopReason`,
  `normalize_relative`, `Workspace`, `open_file_in`, `read_bounded`,
  `truncate_line`, `read_dir_limited`, `ExcludeFn`, `WalkOptions`, `Entry`,
  `walk_tree`, `dir_path`, `open_any`, `open_dir`, `as_str`, `run_blocking`)
  werden benutzt → kein `dead_code` unter `-D warnings`.
- **Ungenutzte Imports**: `OpenOptions` bleibt in `approval.rs` auch auf Unix
  benutzt (`:78`); in `store.rs:222`, `reader.rs:46`, `job_store.rs:719`,
  `file_store.rs:444` liegt die einzige Nutzung im `#[cfg(not(unix))]`-Zweig und
  der Import ist passend gegated. `ignore::WalkBuilder` wurde aus `glob.rs`
  entfernt, `ignore` bleibt über `tree.rs` in Gebrauch.
- **clippy default-warn**: `too_many_arguments` (max. 7 bei `walk_children` —
  Schwelle ist „> 7“), `new_without_default` (`Fixture::new` liegt in einem
  privaten `#[cfg(test)]`-Modul, also nicht `is_exported` → kein Lint),
  `items_after_test_module` (in allen 13 geänderten/neuen Dateien steht
  `mod tests` zuletzt), `needless_borrow` (`(&file).take(..)` in `read.rs:161`
  ist für die Methodenauflösung auf `impl Read for &File` nötig und trägt keine
  Deref-Adjustment → kein Lint), `module_inception`, `large_enum_variant`.
- **Tests rechnerisch nachvollzogen** (Erwartungswerte stimmen):
  `tree.rs` Walk-/Tiefen-/Gitignore-/Zeitgrenzen-Tests, `read.rs`
  UTF-8-Grenze (`aäb`, cap 2 → `"a"`, `offset=1`) und 64-KiB-Kappung,
  `write.rs` Mode-Erhalt/Symlink-Ersatz, `list.rs` `entry_limit`,
  `search.rs`/`grep.rs` Hard-Limits und Output-Budget, `capture.rs` alle sechs
  Budget-Szenarien, `limits.rs` Golden-Argv, `exec.rs`
  `truncate_combined_output`. `#[ignore = "requires bwrap+userns"]` in
  `sandbox_test!` ist syntaktisch korrekt; `tempfile` ist dev-dep in
  `harw-tool-fs`, `harw-tool-shell`, `harw-session-store`, `harw-memory`.
- **Cargo.lock**: `harw-fsutil` ist in den Dependency-Listen von
  `harw-memory`, `harw-secrets`, `harw-session-store`, `harw-tool-fs`
  eingetragen; `tokio` steht bei `harw-tool-fs`. Gelockte Versionen passen zu
  den Manifest-Pins: tokio 1.53.0, ignore 0.4.32, globset 0.4.19, regex 1.13.0,
  tempfile 3.27.0, rustix 1.1.4. `harw-fsutil` ist Workspace-Member
  (`Cargo.toml:104`).
- **JSON-Schemas additiv**: `fs.read` bekommt die optionale Property `offset`
  (`provider.rs:227-238`); `GlobArgs`/`GrepArgs` unverändert; `shell.exec`
  unverändert. Nur Beschreibungstexte präzisiert.

---

## Fix-Aufgaben nach Datei

### `harw-ops/src/diff.rs` (W1-04)
1. **R1-01 / R1-02 (hoch):** `render_shell_output` muss
   `killed_by_output_limit` und `truncated` aus dem `shell.exec`-JSON lesen.
   Bei `killed_by_output_limit == true` **nicht** als Fehler behandeln, sondern
   die (gekappte) Ausgabe mit sichtbarem Hinweis zurückgeben; `exit_code != 0`
   bleibt nur dann ein Fehler, wenn nicht gekappt wurde.
2. Test ergänzen, der `render_shell_output(json!({"exit_code": -1, "stdout":
   "…", "stderr": "", "truncated": true, "killed_by_output_limit": true}))`
   auf `Ok` mit Hinweis prüft.

### `harw-tool-shell/src/limits.rs` (W1-03)
3. **R1-03 (mittel):** `PRLIMIT_CANDIDATES` um `/bin/prlimit` erweitern (Array-
   Länge 2). `resolve_prlimit` bleibt unverändert.

### `harw-sandbox/src/bwrap.rs` (W1-03, Fremddatei für W1-04)
4. **R1-04 (mittel):** `BWRAP_CANDIDATES` um `/usr/local/bin/bwrap` erweitern.
   `default_executable_is_always_a_fixed_absolute_path` und
   `discover_returns_only_trusted_fixed_candidates` bleiben gültig.

### `harw-memory/Cargo.toml`, `harw-secrets/Cargo.toml`, `harw-session-store/Cargo.toml` (W1-01)
5. **R1-05 (mittel):** `harw-fsutil` nach
   `[target.'cfg(unix)'.dependencies]` verschieben — oder, falls Nicht-Unix
   ohnehin nicht unterstützt wird, die `#[cfg(not(unix))]`-Zweige in
   `store.rs`, `job_store.rs`, `reader.rs`, `approval.rs`, `file_store.rs`
   samt der gegateten `OpenOptions`-Imports ersatzlos entfernen. Eine der
   beiden Varianten, nicht beide.

### `harw-tool-shell/src/exec.rs` (W1-03)
6. **R1-07 (niedrig/mittel):** `#[non_exhaustive]` auf `ShellToolProvider`
   oder `with_limits(…)`-Builder statt eines weiteren `pub`-Feldes.

### `harw-tool-fs/Cargo.toml` (W1-02)
7. **R1-09 (niedrig):** tokio-Pin zwischen `[dependencies]` und
   `[dev-dependencies]` vereinheitlichen.

### `harw-tool-fs/src/blocking.rs`, `grep.rs` (W1-02) — optional
8. **R1-12 / R1-08 (niedrig):** Panic vs. Cancel unterscheiden;
   `MAX_FILE_SIZE_BYTES`-Doku auf 8 MiB nachziehen.

### `CHANGELOG.md` — optional
9. **R1-06 (mittel):** Ausgabeform von `fs.list`/`fs.search` (Array → Objekt)
   und die neue Kill-Semantik von `shell.exec` als breaking change eintragen.

---

## Kompiliert voraussichtlich

| Crate | `cargo check` | `--tests` | `--doc` | clippy `-D warnings` |
|---|---|---|---|---|
| `harw-session-store` (W1-01) | **ja** | **ja** | ja | **ja** |
| `harw-memory` (W1-01) | **ja** | **ja** | ja | **ja** |
| `harw-secrets` (W1-01) | **ja** | **ja** | ja | **ja** |
| `harw-tool-fs` (W1-02) | **ja** | **ja** | ja | **ja** |
| `harw-tool-shell` (W1-03) | **ja** | **ja** | ja | **ja** |
| `harw-sandbox` (W1-03) | **ja** | **ja** | ja | **ja** |
| `harw-ops` (W1-04) | **ja** | **ja** | ja | **ja** |
| `harw-core` (Querwirkung) | **ja** (bwrap-`plan()`-Signatur unverändert) | ja | ja | ja |
| `harw-registry-defaults` (Querwirkung) | **ja** (nutzt nur `FsToolProvider`/`ShellToolProvider::default()`) | ja | ja | ja |

Vorbehalt: `-D warnings` gilt unter der Annahme, dass CI clippy mit dem
Manifest-MSRV (`rust-version = 1.85`) auswertet — sonst greift R1-10
(`collapsible_if` über let-chains) an drei Stellen.

Laufzeit-Vorbehalt (kein Compile-Problem): R1-03 und R1-04 lassen
`shell.exec` und stdio-MCP auf Hosts ohne `/usr/bin/prlimit` bzw.
`/usr/bin/bwrap` fail-closed abbrechen; die zugehörigen Integrationstests
überspringen sich in dem Fall selbst (`sandbox_runtime_available()`), die
`#[ignore]`-Pflichtvarianten schlagen fehl.
