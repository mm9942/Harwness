# W2d-2 — Fix-Agent Z2d-2 / F-ROOT

Owned files:
- `harw-tui/src/runtime_root.rs` — R4 (fehlende `close_session`-Aufrufe auf
  frühen Fehlerpfaden), R5 (Fremd-Wiring-Prüfung in `build_root_runtime`
  + Regressionstest).
- `docs/remediation/ledger/W2d2/F-ROOT.md` (diese Datei, neu).

Nicht angefasst: `app.rs`, `tools_command.rs` (parallel, T2a), `harw-runtime/**`.

BUILD-POLICY eingehalten: kein `cargo build/check/test/clippy/run/add`, kein
`make`/`rustc`/`rust-analyzer`, keine git-Schreibbefehle. Ausgeführt:
`cargo metadata --offline --no-deps --format-version 1` → Exit 0 (nur
Workspace-Struktur gelesen, kein Build). Verifikation ausschließlich durch
Lesen (grep + Signaturprüfung gegen `harw-runtime/src/assembly.rs`,
`harw-runtime/src/services.rs`, `harw-operations/src/context.rs`,
`docs/remediation/ledger/W2d2/T1.md`, `CONTRACTS-W2d2.md` §0/§1.1/§1.2).

## R4 — fehlende `close_session`-Aufrufe auf frühen Fehlerpfaden

**Befund**: In `run_tui` erzeugt `build_root_runtime` bereits die
Wurzelsitzung (`assembly.new_root_session(...)`, jetzt Zeile ~516), bevor
zwei frühe Fehlerpfade die Funktion mit `?`/`return Err(..)` verlassen,
ohne `assembly.close_session(...)` aufzurufen:
- Verlaufsladen (`assembly.state_store().load_history(...)`, vormals
  Zeilen 315–317).
- `TerminalGuard::enter()?` (vormals Zeile 326).

**Signaturprüfung**: `RuntimeAssembly::close_session(&self, id: &SessionId)`
(`harw-runtime/src/assembly.rs:1667`) — `&self` (kein `&mut self`), gibt
`()` zurück (kein `Result`); ruft intern jeden `SessionLifecycleHook` und
loggt selbst per `tracing::debug!` (`session_id`, `hooks`,
`"runtime.session.closed"`, Zeilen 1671–1675). Es gibt also keinen
Fehlerfall von `close_session`, der zusätzlich geloggt werden müsste — der
im Befund vorgeschlagene `tracing::warn` auf einem `Result` entfällt, weil
die Funktion keinen zurückgibt.

**Fix** (Muster aus dem bereits vorhandenen `resume_session`-Pfad
übernommen, Zeilen ~552–563 des Ausgangsstands, dort schließt
`assembly.close_session(assembly.root_session_id())` die neu montierte
Sitzung, wenn ihr Verlauf nicht lädt):

1. Verlaufsladen der Startsitzung zu `match` umgebaut:
   ```rust
   let history = match runtime.block_on(assembly.state_store().load_history(session.id())) {
       Ok(history) => history,
       Err(error) => {
           assembly.close_session(assembly.root_session_id());
           return Err(TuiError::Core(format!("durable history load failed: {error}")));
       }
   };
   ```
2. `TerminalGuard::enter()?` zu `match` umgebaut, nach `let mut current = assembly;`
   (die Montage ist zu diesem Zeitpunkt bereits in `current` umbenannt):
   ```rust
   let mut guard = match TerminalGuard::enter() {
       Ok(guard) => guard,
       Err(error) => {
           current.close_session(current.root_session_id());
           return Err(error);
       }
   };
   ```

`run_tui`s Doku (`# Beschreibung`, `# Fehler`) ergänzt: beide Pfade
schließen die bereits erzeugte Wurzelsitzung vor der Fehlerrückgabe.

Der dritte im Befund genannte Pfad — Verlassen der Schleife (Quit oder
Fehler) — schloss die Sitzung bereits vor diesem Fix
(`current.close_session(current.root_session_id())` nach `drop(guard)`,
unverändert). Kein weiterer früher Fehlerpfad zwischen `build_root_runtime`
und der Schleife vorhanden (geprüft: nur die zwei oben genannten Stellen
liegen zwischen Sitzungserzeugung und Schleifenstart).

## R5 — `run_tui` prüft nicht, ob `options.wiring` zur Montage gehört

**Befund**: `build_root_runtime` nahm `wiring.controller` ungeprüft
entgegen und reichte ihn an `ChatApp::with_session_controller` weiter, ohne
zu verifizieren, dass genau dieser Controller beim Bau der übergebenen
`assembly` per `TuiSessionWiring::install` in deren Slash-`ServiceMap`
gelegt wurde. Eine Verdrahtung aus einer anderen Montage hätte einen
Controller montiert, dessen Ereignisse nirgends in dieser Montage ankommen
(die Montage kennt nur ihren eigenen, beim Bau installierten Controller).

**Verifikation der Prüfmethode** gegen den bereits bestehenden Test
`test_tui_session_wiring_install_sets_controller_and_events`
(`runtime_root.rs`, jetzt Zeilen 717–737): dort wird exakt so verglichen —
`assembly.services().service_map(ServiceSurface::Slash)`, darauf
`.get::<SharedSessionController>()` (`Option<&SharedSessionController>`,
`ServiceMap::get`, `harw-operations/src/context.rs:125`), Vergleich per
`std::ptr::addr_eq(Arc::as_ptr(installed), Arc::as_ptr(&wiring.controller))`.
`ServiceSurface` ist `Clone + Copy + Debug + PartialEq + Eq + Hash`
(`harw-runtime/src/services.rs:90`) und über `harw_runtime::ServiceSurface`
re-exportiert (`harw-runtime/src/lib.rs:36`). `RuntimeServices::service_map`
gibt eine neue, eigenständige `ServiceMap` zurück (`services.rs:418`,
Wertsemantik, keine Referenz auf die Montage) — deshalb an eine lokale
Variable gebunden, damit die geliehene `&SharedSessionController` die
Anweisung überlebt.

**Fix** in `build_root_runtime`, direkt nach dem Destrukturieren von
`TuiSessionWiring` und vor jeder weiteren Verwendung von `controller`:
```rust
let slash_services = assembly.services().service_map(ServiceSurface::Slash);
let belongs_to_assembly = slash_services
    .get::<SharedSessionController>()
    .is_some_and(|installed| {
        std::ptr::addr_eq(Arc::as_ptr(installed), Arc::as_ptr(&controller))
    });
if !belongs_to_assembly {
    return Err(TuiError::Core(
        "wiring does not belong to this assembly".to_owned(),
    ));
}
```
`TuiError::Core` verifiziert (`app.rs:3054`, `Core(String)`); `import`
ergänzt: `harw_runtime::ServiceSurface` in die bestehende
`use harw_runtime::{RootSession, RuntimeAssembly, RuntimeAssemblyBuilder};`-Zeile
aufgenommen. `controller` bleibt danach unverändert nutzbar (nur per `&controller`
geliehen, nicht konsumiert) — `.with_session_controller(controller)` weiter
unten unverändert.

`run_tui`s `# Fehler`-Doku ergänzt um den neuen `TuiError::Core`-Fall
(„`options.wiring` gehört nicht zu `assembly`“).

**Test** `test_build_root_runtime_rejects_foreign_wiring` (neu, am Ende von
`mod tests` in `runtime_root.rs` angehängt): baut zwei unabhängige
Echo-Montagen mit je eigener `TuiSessionWiring` über die vorhandenen
Fixtures `fixture()`/`tui_assembly()`, ruft `build_root_runtime(&assembly_a,
wiring_b)` auf und erwartet `Err(TuiError::Core(message))` mit
`message.contains("does not belong")`. `TuiError` hat kein `PartialEq`
(wickelt `io::Error` ein); der Test matcht daher explizit auf
`Err(TuiError::Core(_))` statt `assert_eq!`, mit `Err(other) =>
panic!("... {other:?}")` als Fallback (kompiliert nur, weil `TuiError:
Debug` per Handschrift existiert, `app.rs:3074` — `RootRuntime` selbst hat
kein `Debug`, deshalb `Ok(_) => panic!(...)` als eigener Arm statt
`{other:?}` über den ganzen `Result`).

## Zusätzliche Prüfungen

- Zeilenlängen der geänderten Stellen ≤ 100 Zeichen (rustfmt-Standardbreite),
  manuell nachgezählt (`awk`), da `cargo fmt` durch die Build-Policy
  ausgeschlossen ist.
- `resume_session`/`ResumedRuntime`-Pfad unverändert: Montagen, die
  `TuiResume::factory` baut, erzeugen ihre `TuiSessionWiring` selbst über
  `TuiSessionWiring::new()` + `install()` + `build()` (Verantwortung der
  Fabrik, außerhalb dieses Moduls) und bestehen die neue Prüfung deshalb
  konstruktionsbedingt.
- `mod tests` importiert `ServiceSurface` bereits separat
  (`use harw_runtime::{..., ServiceSurface};`, Zeile ~649); der neue
  Modul-Import kollidiert nicht (identisches Item, zwei `use`-Anweisungen
  in unterschiedlichen Geltungsbereichen — Modul-Ebene und `mod tests` via
  `use super::*;` plus explizitem Re-Import — sind in Rust zulässig).
- Kein `unwrap()`/`expect()` in Produktionspfaden ergänzt; im Testcode nur
  `.expect(...)` an bereits bestehenden Stellen (unverändert).
- Nicht gebaut/getestet: `cargo test -p harw-tui runtime_root`, clippy
  `-D warnings`, rustfmt — Orchestrator führt das sequenziell nach dem
  Integrations-Audit aus.
