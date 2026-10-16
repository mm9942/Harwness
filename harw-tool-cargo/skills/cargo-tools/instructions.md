# cargo — Rust bauen und prüfen mit zusammengefasster Ausgabe

**Regel:** Für Rust-Builds und -Tests die `cargo.*`-Werkzeuge nehmen. Sie bauen aus typisierten Argumenten eine feste Kommandozeile (keine freie Shell-Zeichenkette), laufen über das sandboxed Shell-Werkzeug und geben **Diagnosen als Daten** zurück statt eines Log-Berges.

## Wann welches Werkzeug

- **Kompiliert es?** `cargo.check` (schnell, keine Binaries). Erst `check`, dann `clippy`, dann `test`: jede Stufe findet Fehler billiger als die nächste.
- **Lints:** `cargo.clippy` mit `package`, `no_deps`; Lint-Stufen (`deny`/`warn`/`allow`) nur als Lint-Namen wie `warnings` oder `clippy::pedantic`.
- **Tests:** `cargo.test` mit `package` und `test_filter`; zusätzliche Harness-Argumente nur aus der Allowlist (`--nocapture`, `--ignored`, `--exact`, `--test-threads=N`, `--skip=NAME`, ...). Doctests mit `doc: true`.
- **Format:** `cargo.fmt_check` zeigt, welche Dateien abweichen; es schreibt nie. Die Korrektur ist ein bewusster, separater Schritt.
- **Struktur/Abhängigkeiten:** `cargo.metadata` (Mitglieder, Features, Anzahl externer Pakete) und `cargo.tree` (`invert` für „warum ist X drin?“, `duplicates`).
- **Doku/Release-Build:** `cargo.doc`, `cargo.build` — beide schreiben nach `target/` und brauchen meist eine Freigabe.

## Ausgabe lesen

- `status` zuerst: `ok`, `failed`, bei Tests `tests_failed`, bei fmt `needs_formatting`. Dann `errors`/`warnings` (Zähler) und `diagnostics`: `{level, code, message, file, line, column}`, **Fehler zuerst**.
- Bei Tests: `tests.passed/failed/ignored` und `tests.failures[{name, location, message}]` — der Panic-Ort steht direkt dabei, das Log ist selten nötig.
- `log` ist nur ein gekürzter Ausschnitt vom Ende; bei einem unklaren Ergebnis dort nachsehen, nicht umgekehrt.
- Pakete heißen wie in `Cargo.toml`; mit `package` auf ein Crate zu begrenzen spart Minuten.

## Grenzen und `truncated`

- Diagnosen sind auf eine feste Zahl begrenzt; `truncated: true` heißt, es gab mehr. Erst die gezeigten beheben und erneut prüfen.
- `cargo.tree` liefert höchstens 48 KiB Text, `cargo.metadata` fasst große Workspaces zusammen und nennt nie absolute Pfade.
- Nicht vorhanden, mit Absicht: `cargo fetch`/`update`/`install`/`publish`, `clippy --fix`, `--open`, freie Zusatzargumente. Wer das braucht, fragt den Nutzer.
- Die Ausführung braucht die Stufe „Prozess ausführen“; fehlt die Freigabe, kommt ein Ablehnungsfehler. Ohne Netzwerkzugang im Sandbox-Pfad `offline: true` und `locked: true` setzen, wenn die Abhängigkeiten schon vorliegen.
