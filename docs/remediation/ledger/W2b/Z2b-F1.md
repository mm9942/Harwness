# Z2b-F1 — Fix R2-01/R2-02/R2-03 aus review-Z2b-R2: `tempfile::TempDir` statt fester/handgebauter Temp-Pfade

Rolle: Fix-Agent Z2b-F1, Welle W2b, Remediation. Ausgeführt: nur
`cargo metadata --offline --no-deps` (Exit 0). **Nichts kompiliert, keine
Tests ausgeführt, kein clippy.** Kein `git`-Schreibbefehl, kein `cargo build`/
`test`, keine Manifest-Änderung, keine Platzhalter/`#[allow]`.

## Auftrag

`docs/remediation/ledger/W2b/review-Z2b-R2.md`, Befunde R2-01, R2-02 (Eigentum
W2B-02, Datei `harw-runtime/src/sandbox.rs`) und Nebenbefund R2-03 (fremdes
Eigentum, Datei `harw-runtime/src/config.rs`, an den Eigentümer übergeben,
hier ausgeführt): die Sandbox-Tests banden `std::env::temp_dir()` — das
geteilte, systemweite `/tmp` — als Harness-Root; die Negativprobe
`root_sandbox_rejects_a_root_that_is_not_a_directory` hängte daran einen
**festen** Pfadnamen (`…/harw-runtime-no-such-directory-w2b02`) unter diesem
geteilten Verzeichnis, wodurch der Test nicht selbst-isolierend war. `tempfile`
war seit dem Orchestrator-Nachtrag in `harw-runtime/Cargo.toml:41` als
Dev-Dependency vorhanden, aber ungenutzt. In `config.rs` deckte ein
handgebautes `TempDir` (~35 Zeilen, eigener Zähler/Zeitstempel-Name +
manuelles `Drop`) genau das ab, was `tempfile::TempDir` bereits leistet; sein
Doc-Kommentar (`:137-139`) behauptete fälschlich, `tempfile` sei keine
Dev-Dependency dieses Crates.

## Geschriebene Dateien

- `harw-runtime/src/sandbox.rs` — nur Testmodul (`#[cfg(test)] mod tests`).
- `harw-runtime/src/config.rs` — nur Testmodul (`#[cfg(test)] mod tests`,
  darin der Doc-Kommentar `:137-139` sowie die handgebaute `TempDir`-Hilfe).
- `docs/remediation/ledger/W2b/Z2b-F1.md` (diese Datei).

Nicht angefasst: alle anderen Dateien/Crates, insbesondere `Cargo.toml` (kein
Manifest-Schreibrecht) und jeglicher Nicht-Testcode.

## `harw-runtime/src/sandbox.rs`

`existing_root()` liefert jetzt ein eigenes, isoliertes `tempfile::TempDir`
statt des geteilten `std::env::temp_dir()`:

```rust
fn existing_root() -> tempfile::TempDir {
    tempfile::TempDir::new().expect("temp dir")
}
```

Alle sieben Aufrufstellen (`root_sandbox_takes_its_permissions_from_the_entry_profile`,
`no_entry_sandbox_carries_network`, `root_sandbox_binds_the_given_project_root`,
`root_sandbox_rejects_a_root_that_is_not_a_directory`,
`web_entry_narrowed_by_tier_never_exceeds_its_profile`,
`plan_node_sandbox_is_monotone_and_never_executes_or_networks`,
`read_only_node_kinds_never_write_even_with_a_writing_contract`) binden das
`TempDir` jetzt an eine eigene Variable, um es für die Testlaufzeit am Leben
zu halten (`let dir = existing_root(); let root = dir.path();`), und reichen
`root: &Path` unverändert an `root_sandbox`/`plan_node_sandbox` weiter — keine
Semantikänderung, nur die Quelle des Wurzelpfads.

R2-02 löst sich damit auf: die Negativprobe hängt den nicht existierenden
Namen jetzt an das gerade erst angelegte, pro Lauf eindeutige `TempDir`:

```rust
fn root_sandbox_rejects_a_root_that_is_not_a_directory() {
    let dir = existing_root();
    let missing = dir.path().join("harw-runtime-no-such-directory-w2b02");
    let error = root_sandbox(EntryKind::Tui, &missing).expect_err("missing root must fail");
    assert!(matches!(error, RuntimeError::Sandbox { .. }));
}
```

Kein anderer Testcode, keine Assertion und kein Nicht-Testcode wurden
verändert. `PathBuf`-Import (`sandbox.rs:29`) bleibt nötig (weiterhin genutzt
bei `SandboxSpec::default`-artigem Aufbau außerhalb der Tests, `:101`).

## `harw-runtime/src/config.rs`

Das handgebaute `TempDir` (Zähler, Zeitstempel, `std::env::temp_dir()`-Join,
manuelles `Drop::drop` mit `remove_dir_all`) ist ersetzt durch einen dünnen
Wrapper um `tempfile::TempDir`, der die für die Testassertions nötige
Kanonisierung beibehält (`config_layers_report_at`/`config_layers_report_in`
kanonisieren intern, `harw-home/src/paths.rs:433-448`; ohne gleiche
Kanonisierung auf Testseite würden Pfadgleichheits-Assertions wie
`trust.untrusted_repo == Some(repo.path().join(".harw"))` brechen):

```rust
/// Isoliertes, selbst entfernendes Verzeichnis (`tempfile::TempDir`),
/// dessen Pfad kanonisiert wird, damit die Assertions unten
/// (Pfadgleichheit gegen `trust.layers`/`trust.untrusted_repo`) mit dem
/// intern von `config_layers_report_at`/`config_layers_report_in`
/// kanonisierten Vergleich übereinstimmen (z. B. Root-Space- gegen
/// Repo-Layer-Identität, `harw-home/src/paths.rs:433-448`).
struct TempDir {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = std::fs::canonicalize(dir.path()).expect("canonical temp dir");
        Self { _dir: dir, path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}
```

Das eigene `impl Drop` entfällt ersatzlos: `tempfile::TempDir` räumt beim
Fallenlassen des `_dir`-Felds selbst auf (Feldreihenfolge in `Self` egal,
Rust droppt Struct-Felder in Deklarationsreihenfolge — `_dir` nach `path` zu
droppen wäre ohnehin unproblematisch, da `path` kein eigenes `Drop` hat).
`_dir` ist mit Unterstrich benannt, weil es nur für seinen Drop-Nebeneffekt
gehalten wird und sonst nie gelesen wird.

Der bisherige Tag-Parameter (`TempDir::new("untrusted-home")` etc.) entfällt:
`tempfile::TempDir` garantiert Eindeutigkeit bereits selbst, das Tag diente
nur der Lesbarkeit handgebauter Pfadnamen. Alle zwölf Aufrufstellen (in
`untrusted_repo_is_reported_and_never_contributes_providers`,
`trusted_repo_is_layered_and_may_contribute_providers`,
`changed_repo_falls_back_to_untrusted_after_trusting`,
`dangling_default_provider_is_a_config_error`,
`malformed_config_toml_is_a_config_error`, `broken_trust_store_is_a_trust_error`)
sind auf `TempDir::new()` angepasst; keine Assertion, kein Testablauf, keine
Fixture-Datei wurde verändert.

Der Doc-Kommentar `:137-139` (R2-03) ist korrigiert: er behauptet nicht mehr,
`tempfile` fehle in den Dev-Dependencies, sondern benennt den tatsächlichen
Grund für den Wrapper (Kanonisierung, s. o.).

## API-Belege (gelesen, Datei:Zeile)

- `tempfile::TempDir::new() -> io::Result<TempDir>`, `TempDir::path(&self) -> &Path`
  — Standard-`tempfile`-API, bereits so in `harw-runtime/Cargo.toml:41`
  (`tempfile = { workspace = true }`, Dev-Dependency) eingebunden; vor diesem
  Fix in keiner Datei des Crates benutzt (Befund R2-01).
- `harw-runtime/src/sandbox.rs:141` `root_sandbox(entry: EntryKind, project_root: &Path)`,
  `:251-255` `plan_node_sandbox(kind, may_write, project_root: &Path)` — beide
  Signaturen unverändert, nehmen weiterhin `&Path` entgegen.
- `harw-home/src/paths.rs:460,462` `std::fs::canonicalize` auf Repo- bzw.
  vertrauten Pfad — Beleg, dass die Kanonisierung in `config.rs`s
  `TempDir::new` weiterhin nötig ist.
- `docs/remediation/ledger/W2b/review-Z2b-R2.md` §„Fix-Aufgaben nach Datei“,
  Punkte 1 (`sandbox.rs`) und 5 (`config.rs`, R2-03).

## Compile-/Lint-Risiken (nicht verifiziert)

- Nichts kompiliert; nur `cargo metadata --offline --no-deps` ausgeführt.
  Typen/Methoden von Hand gegen die oben belegten Quellen geprüft.
- `_dir`-Feld in `config.rs`s `TempDir` wird nie gelesen, nur für `Drop`
  gehalten — der führende Unterstrich unterdrückt `dead_code` ohne
  `#[allow]`.
- Kein `#[allow]`, kein Platzhalter, kein `unwrap()` neu eingeführt (nur
  bereits vorhandenes `.expect(...)`-Muster fortgeführt). `unsafe_code =
  "forbid"` (Workspace-Lint) bleibt unberührt.
