# Z2b-F0 — Fix Blocker 2 aus W2B-01: `cwd`-explizite Layer-Ermittlung

Rolle: Fix-Agent Z2b-F0, Welle W2b, Remediation. Ausgeführt: nur
`cargo metadata --offline --no-deps --format-version 1` (Exit 0). **Nichts
kompiliert, keine Tests ausgeführt, kein clippy.** Kein `git`-Schreibbefehl,
keine Manifest-Änderung, keine Platzhalter/`#[allow]`.

## Auftrag

Blocker 2 aus `W2B-01.md` §„Abweichungen": `RuntimeSpec::cwd` ging nicht in
die Layer-Auswahl ein, weil `harw_home::config_layers_report` das
Arbeitsverzeichnis selbst über `std::env::current_dir()` liest und der
testbare, `cwd`-explizite Kern `config_layers_report_in` `pub(crate)` ist.

## Geschriebene Dateien

- `harw-home/src/paths.rs` — neue Funktion `config_layers_report_at`, Test.
- `harw-home/src/lib.rs` — Re-Export von `config_layers_report_at`.
- `harw-runtime/src/config.rs` — `load_config` nutzt `config_layers_report_at`,
  Tests ohne `set_current_dir`/Mutex.
- `docs/remediation/ledger/W2b/Z2b-F0.md` (diese Datei).

Nicht angefasst: alle anderen Dateien/Crates.

## `harw-home/src/paths.rs`

Neue öffentliche Funktion (nach `config_layers_report`, vor
`config_layers_report_in`, Zeile 433):

```rust
pub fn config_layers_report_at(home: &Path, cwd: &Path) -> HomeResult<LayerReport> {
    let profile = active_profile_name(home);
    config_layers_report_in(home, &profile, Some(cwd))
}
```

Profilermittlung ist identisch zu der in `config_layers_report` (belegt:
`paths.rs:406`, vor diesem Fix `let profile = active_profile_name(home);`
direkt vor dem `current_dir()`-Aufruf) — Präzedenz `HARW_PROFILE` →
`active_profile`-Datei → `DEFAULT_PROFILE` (`active_profile_name`,
`paths.rs:78-91`).

`config_layers_report(home)` bleibt öffentlich, unveränderte Signatur, und
delegiert jetzt an `config_layers_report_at`, statt das Profil ein zweites Mal
selbst zu bestimmen:

```rust
pub fn config_layers_report(home: &Path) -> HomeResult<LayerReport> {
    match std::env::current_dir() {
        Ok(cwd) => config_layers_report_at(home, &cwd),
        Err(_) => {
            let profile = active_profile_name(home);
            config_layers_report_in(home, &profile, None)
        }
    }
}
```

(Der `Err`-Zweig bleibt nötig, weil `config_layers_report_at` ein `cwd: &Path`
verlangt, kein `Option`; die bisherige Semantik — "Arbeitsverzeichnis nicht
ermittelbar ⇒ kein Repo-Layer" — ist unverändert erhalten.)

`config_layers_report_in` bleibt `pub(crate)`, unverändert.

### Test (kein Prozess-cwd-Wechsel)

`config_layers_report_at_uses_explicit_cwd_without_process_cwd`
(`paths.rs`-Testmodul): zwei `TempDir`-Repos mit `.harw`
(`repo_with_harw`, bereits vorhandener Helper) — eines per
`crate::trust::trust_project` freigegeben, eines nicht. Beide werden direkt
per `config_layers_report_at(&home.0, repo.0.as_path())` geprüft:
freigegebenes Repo ⇒ `.harw` ist letzter Layer, `status == Trusted`; nicht
freigegebenes ⇒ nur 2 Home-Layer, `untrusted_repo` gesetzt, `status ==
Untrusted`. Kein `std::env::set_current_dir`, kein `set_var`.

## `harw-home/src/lib.rs`

Re-Export ergänzt, analog zu `config_layers_report`:

```rust
pub use paths::{
    LayerReport, active_profile_name, active_profile_path, auth_path, config_layers,
    config_layers_report, config_layers_report_at, home_dir, profile_dir,
};
```

## `harw-runtime/src/config.rs`

`load_config` nutzt jetzt `spec.cwd` explizit:

```rust
let report =
    harw_home::config_layers_report_at(&spec.home, &spec.cwd).map_err(map_home_error)?;
```

Signatur von `load_config` unverändert (wie in W2B-01 als Folgearbeit
vorgesehen). Moduldoku (Schritt 1) und die `# Arguments`-Sektion von
`load_config` auf `config_layers_report_at`/`spec.cwd` aktualisiert; der
Abschnitt „Bekannte Einschränkung (`cwd`)“ ist entfernt, weil die
Einschränkung behoben ist.

### Tests

`cwd_lock()` (Mutex) und alle sechs `std::env::set_current_dir`/
`current_dir()`-Aufrufe in den Tests entfernt — jeder Test ruft `load_config`
jetzt direkt mit `spec_for(home.path(), <cwd>.path())` auf, ohne den
Prozess-Arbeitsordner zu berühren:
`untrusted_repo_is_reported_and_never_contributes_providers`,
`trusted_repo_is_layered_and_may_contribute_providers`,
`changed_repo_falls_back_to_untrusted_after_trusting`,
`dangling_default_provider_is_a_config_error`,
`malformed_config_toml_is_a_config_error`,
`broken_trust_store_is_a_trust_error`. Der Kommentar an der
`TempDir::new`-Kanonisierung wurde korrigiert: sie ist nicht mehr für einen
Vergleich mit `std::env::current_dir()` nötig, sondern damit
`config_layers_report_at`/`config_layers_report_in`s interne Kanonisierung
(Root-Space- vs. Repo-Layer-Identität, `paths.rs:433-448`) dieselben Pfade
sieht wie die Testassertions.

## API-Belege (gelesen, Datei:Zeile)

- `harw-home/src/paths.rs:355-366` `LayerReport`, `:405-412`
  (vorher) `config_layers_report`, `:411-448` (vorher) `config_layers_report_in`
  (`pub(crate)`), `:78-91` `active_profile_name`, `:43`
  `DEFAULT_PROFILE`.
- `harw-home/src/lib.rs:49-53` (vorher) Re-Exports.
- `harw-runtime/src/config.rs:99-121` (vorher) `load_config`, Tests
  `:144-421` (vorher, inkl. `cwd_lock`).
- `docs/remediation/ledger/W2b/W2B-01.md` §„Abweichungen / Blocker“ Punkt 2
  (Zeilen 137-146), §„Compile-/Lint-Risiken“ letzter Absatz (cwd-Sperre).

## Compile-/Lint-Risiken (nicht verifiziert)

- Nichts kompiliert; nur `cargo metadata --offline --no-deps` ausgeführt.
  Typen/Borrows von Hand gegen die oben belegten Quellen geprüft.
- `config_layers_report` ruft jetzt `config_layers_report_at` im `Ok`-Zweig
  auf; im `Err`-Zweig (kein `current_dir()`) bleibt die alte Direktauflösung
  über `config_layers_report_in(.., None)`, um kein `Option<&Path>` an
  `config_layers_report_at` zu verlangen und dessen Signatur nicht durch das
  Auftrag vorgegebene `cwd: &Path` zu verwässern.
- Keine `#[allow]`, keine Platzhalter. `unsafe_code = "forbid"`
  (Workspace-Lint) bleibt unberührt — keine `unsafe`-Nutzung eingeführt.
