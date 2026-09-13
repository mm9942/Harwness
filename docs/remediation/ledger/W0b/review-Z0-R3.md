# Review Z0-R3 – statischer Compile-Review W0b (W0B-05, W0B-07, W0B-02)

Modus: READ-ONLY, kein Compiler. Grundlage: `git diff HEAD`, neue Dateien direkt gelesen,
`cargo metadata --offline --no-deps` (Exit 0) plus Zyklen-Skript über die Metadaten, Quellcode
von `toml-1.1.3+spec-1.1.0` unter `~/.cargo/registry/src/index.crates.io-*/` (lokal vorhanden,
anders als im Ledger W0B-02 angegeben).

Maßstab: `make clippy-tests` = `cargo clippy --workspace --all-targets --all-features -- -D warnings`
+ `cargo test --workspace --all-features` + `xtask gates` (Makefile:25-28).

## Befunde

| ID | Schwere | Datei:Zeile | Befund | Konkreter Fix |
|---|---|---|---|---|
| R3-01 | **blocker** (Tests rot) | `harw/tests/version_coherence.rs:90`, `:344`, `:351`, `:358`, `:365` | `text.parse::<toml::Value>()` parst in toml 1.1.3 **einen einzelnen TOML-Wert, kein Dokument**: `impl FromStr for Value` → `ValueDeserializer::parse` → `DeValue::parse` („Parse a TOML value“), siehe `toml-1.1.3+spec-1.1.0/src/value.rs:395-400`, `src/de/deserializer/value.rs:48-54`, `src/de/parser/devalue.rs:138-148`. Dokumente gehen nur über `toml::Table: FromStr` (`src/table.rs:53-58` → `crate::from_str`) bzw. `toml::from_str`; die Crate-Doku zeigt selbst `"foo = 'bar'".parse::<Table>()` (`src/lib.rs:42`). Folge: Kompiliert, aber `slice14_workspace_version_is_consistent` panict schon beim Lesen der Wurzel-`Cargo.toml` („Cannot parse … as TOML“), dazu scheitern 4 der 6 Unit-Tests (`test_parse_version_decl_*` ×3, `test_workspace_member_dirs_reads_literal_string_list`). Der Ledger hat die API nur über docs.rs geprüft und dabei die Wert-/Dokument-Unterscheidung übersehen. Die Testlogik selbst stimmt, per `tomllib` gegen die echten Daten nachgerechnet: 97 Mitglieder, keine Globs, alle `harw*` auf 0.2.0 geerbt, Wurzel enthält „0.2.0“ genau einmal. | In `read_toml`: `toml::Value::Table(text.parse::<toml::Table>().unwrap_or_else(\|e\| panic!("Cannot parse {} as TOML: {e}", path.display())))`. In den Unit-Tests je `let manifest = toml::Value::Table("[package]\n…".parse::<toml::Table>().unwrap());` (analog `root`). Rest unverändert (`Value::get`/`as_*` bleiben gültig). Doku-Kommentar Z. 84-87 korrigieren. |
| R3-02 | **hoch** (Tests rot) | `xtask/src/gates.rs:73` (Ursache) → `xtask/src/gate_edges.rs:376`, `xtask/src/gate_privileges.rs:814`, `xtask/src/gate_privileges.rs:957`, `xtask/src/gate_writescopes.rs:1407` | Durch `is_green = checked > 0 && violations.is_empty()` scheitern **4** bestehende Tests. Der Ledger nennt nur 2 sicher. Betroffen sind `gate_edges::test_evaluate_empty_graph_is_green_with_zero_checked` (Z. 373-378), `gate_privileges::test_evaluate_stub_binary_with_empty_deps_counts_as_missing` (Z. 807-820, `checked == 0`), `gate_privileges::test_evaluate_empty_graph_is_green_with_zero_checked` (Z. 954-960) und `gate_writescopes::test_prose_cell_is_ignored_with_notice_not_silently` (Z. 1393-1408, reine Prosa ⇒ `checked == 0`). Alle anderen `is_green()`-Assertions geprüft: `checked ≥ 1`, also grün. Das gilt auch für `gate_warden.rs:1969,1986,2042,2090`. `main.rs`/`webui.rs` rufen `is_green` nicht auf. | Erwartung drehen, z. B. `gate_edges.rs:376` → `assert!(!report.is_green(), "leerer Graph prüft nichts (G-102)");`. Genauso `gate_privileges.rs:814`, `:957` und `gate_writescopes.rs:1407`. Testnamen anpassen (`…_is_red_with_zero_checked`). |
| R3-03 | **hoch** (clippy `-D warnings`) | `xtask/src/gate_warden.rs:284-296` | `CBuildException::reason` und `::since` werden nirgends gelesen. Einziger Zugriff ist `exception.krate`/`.tool` in Z. 321. Abgeleitetes `Debug`/`Clone` zählt in der dead-code-Analyse nicht ⇒ rustc warnt „fields `reason` and `since` are never read“ ⇒ `make clippy`/`clippy-tests` bricht für `xtask` ab. Außerdem steht die Begründung aus dem Befund G-002 dann nirgends in der Ausgabe. | Felder tatsächlich nutzen, z. B. in `c_build::run` nach `evaluate_c_build`: `for e in C_BUILD_EXCEPTIONS { println!("warden-cbuild: Ausnahme {}/{} seit {}: {}", e.krate, e.tool, e.since, e.reason); }`. Minimal alternativ `#[expect(dead_code, reason = "Dokumentation der Ausnahme, G-002")]` auf dem Struct (`expect` ist seit 1.81 stabil). |
| R3-04 | **hoch** (clippy `-D warnings`, Konfidenz mittel) | `harw-runtime/src/spec.rs:340`, `harw-runtime/src/error.rs:55` | `clippy::type_complexity` (Standard-Warnung, Schwelle 250) greift auch bei `let`-Typannotationen. Grob nach der Clippy-Formel gerechnet: `[(EntryKind, &[Permission], P, O, A, S, C); 11]` ≈ 281, `[(fn(String) -> RuntimeError, &str); 8]` ≈ 291 (Funktionszeiger zählt 50×Tiefe). Beide Testmodule laufen bei `--all-targets` mit. | `spec.rs` Testmodul: `type Row = (EntryKind, &'static [Permission], RegistryProfile, OperationSurface, AskResolution, SpawnerPolicy, CeilingPolicy);` und `let expected: [Row; 11] = [...]`. `error.rs`: `type Make = fn(String) -> RuntimeError;` und `let cases: [(Make, &str); 8] = [...]`. |
| R3-05 | mittel | `xtask/src/gates.rs:80-89`, `:124-136` | Folge von G-102: Ein Gate mit `checked == 0` und ohne Verstöße ist jetzt rot. `run()` sammelt aber nur `violations` in die Meldung ⇒ `Err("")`, also ein Fehler ohne jede Aussage. `summary()` gibt dazu „0 Verstöße bei 0 geprüften Kandidaten“ aus. Heute nicht ausgelöst (Produktions-`checked` > 0: alle 4 überwachten Binaries haben Deps), aber genau der Fall, den G-102 sichtbar machen soll. | In `run()`: `if report.checked == 0 { message.push_str(&format!("{}: nichts geprüft (checked == 0) – Gate ohne Aussage\n", report.name)); }` vor der Verstoß-Schleife. In `summary()` einen eigenen Zweig `else if self.checked == 0 { format!("{}: rot – nichts geprüft", self.name) }`. |
| R3-06 | niedrig | `xtask/src/gate_privileges.rs:~118` (Moduldoku), `:633-635` | Die Doku sagt, fehlende/Stub-Binaries seien „kein Verstoß“. Das stimmt pro Binary weiter, das Gate als Ganzes wird aber rot, sobald **alle** überwachten Binaries fehlen. | Doku-Satz ergänzen: „Fehlen alle überwachten Binaries (`checked == 0`), ist das Gate rot (G-102).“ |
| R3-07 | niedrig | `harw-extension-api/src/lib.rs:28-33` | `ApprovalHandlerKind` und `ApprovalModeCell` sind nicht im Crate-Root re-exportiert, nur über `contributors::`/`approval_mode::` erreichbar. Vertragskonform, `spec.rs:16` nutzt korrekt den Modulpfad (`pub mod contributors`, lib.rs:11). | Additiv: `pub use approval_mode::{ApprovalMode, ApprovalModeCell};` und `ApprovalHandlerKind` in die `contributors::{…}`-Liste aufnehmen. |
| R3-08 | niedrig | `harw-types/src/principal.rs:40-41` vs. `xtask/src/webui.rs:879` | `PermissionTier` serialisiert jetzt snake_case (`"owner"`), der generierte TS-Typ lautet aber PascalCase (`"Observer" \| … \| "Owner"`). Heute ohne Wirkung, weil der Tier bisher nirgends per serde auf die Leitung ging. Sobald jemand serde verwendet, laufen beide auseinander. | Beim ersten serde-Nutzer (W2) TS-Typ auf snake_case umstellen oder `webui.rs` aus `PermissionTier`-serde ableiten. |
| R3-09 | niedrig | `docs/remediation/ledger/W0b/W0B-02.md` Risiko 3 | Die Aussage „Cargo.lock nicht regeneriert“ ist veraltet. Der Arbeitsbaum-`Cargo.lock` enthält bereits `harw → toml 1.1.3`, `cargo_metadata`/`camino`/`cargo-platform` sind entfernt, dazu `harw-runtime`-Deps, `harw-tui/cli → harw-runtime`, `harw-fsutil → rustix`, `harw-sandbox → url`. Das ist konsistent, der Urheber steht aber in keinem Ledger. | Ledger-Notiz korrigieren, Lock-Änderung dem Orchestrator zuordnen. |
| R3-10 | niedrig | `harw-core/src/activation.rs` (intersect, `let both_full = …`), `xtask/src/gate_warden.rs:1936,2086,2099`, `harw/tests/version_coherence.rs:89,142,290,344,351` | Zeilen > 100 Zeichen, rustfmt nicht gelaufen ⇒ `make fmt` (`cargo fmt --all --check`) wird rot. Kein Compile-Problem. | `cargo fmt --all` nachholen, sobald verfügbar. |

### Geprüft und ohne Befund (Auszug)

**W0B-05**
- `harw-types/src/principal.rs`: Derives und serde-Attribute ok (serde `derive` vorhanden, `harw-types/Cargo.toml:25`). `ApprovalActor` hat `Debug, Clone, PartialEq, Eq, Hash` (`ids.rs:173`), `ids` ist `pub mod`. Private `use crate::ids::ApprovalActor` ist über `use super::*` im Testmodul sichtbar. Doctests kompilieren. Keine Namenskollision in `harw-types` (Fassade `harw::types` globt nur `harw_types`).
- `PermissionTier`-Umzug: alle Nutzer laufen über `harw_operations::{operation::,}PermissionTier` (Re-Export `harw-operations/src/lib.rs:100-103` bleibt gültig), `harw_tui::PermissionTier` (`command.rs:56`) oder Makro-Codegen `::harw_operations::PermissionTier::…` (`harw-macros/src/operation.rs:685-688`), alles weiter gültig. Es gibt keine `impl … for PermissionTier` im Workspace (Orphan-Regel ok) und keine `serde(remote)`. Neue Derives Hash/Serialize/Deserialize kollidieren mit nichts. `xtask/src/webui.rs:335` wertet nur Quelltext von `OperationMeta` aus, unberührt.
- `harw-runtime`: Pfade `harw_core::mode` (pub, lib.rs:24), `harw_registry_defaults::profile` (pub, lib.rs:46), `harw_sandbox::{Permission, PermissionSet}`, `PermissionSet::from_policy<I: IntoIterator<Item=Permission>>`, `contains(Permission)`, `is_subset_of(&Self)`, `iter()` (BTreeSet, Reihenfolge R<W<X ⇒ Test Z. 574-577 korrekt). `ReasoningEffort::High`, `InteractionMode::Work`. Die Derive-Ketten `Eq` für `EntryProfile`/`RuntimeSpec`/`RightsSnapshot` sind erfüllt. `HarwError`-Attribute `#[msg("…{detail}")]` entsprechen der Makrosyntax (`harw-macros/src/error.rs`, benannte Felder) und erzeugen `Display`, `Error` und `RuntimeResult`. Koerzion Closure → `fn`-Zeiger im annotierten Tupel-Array ist zulässig.
- Manifest: alle 21 Path-Deps existieren, `serde`/`tracing` stehen in `[workspace.dependencies]`, tokio 1.53.0 im Lock. `cargo metadata --offline --no-deps` Exit 0. Skript über normal+build-Kanten: **keine Zyklen**, `harw-runtime` wird nur von `harw-tui`/`harw-cli` genutzt und erreicht keinen der beiden.

**W0B-07**
- `ApprovalHandler::kind/label`: Default-Methoden mit `&self`, ohne Generics ⇒ dyn-kompatibel (`Arc<dyn ApprovalHandler>` in `registry.rs:132`, `harw-tui/src/app.rs:1433` bleibt gültig). Die 5 Implementierer (`harw-tui/src/approval.rs:690`, `harw-registry-defaults/src/lib.rs:140`, `harw-core/src/policy.rs:40`, `harw-core/src/child_controller.rs:2049`, `harw-core/tests/turn_loop.rs:286`) haben weder inhärente noch fremde `kind`/`label`-Methoden und keine Mehrfach-Trait-Bounds ⇒ keine Mehrdeutigkeit (E0034). `harw-macros` erzeugt keine `ApprovalHandler`-Impls.
- `ApprovalModeCell`: `#[derive(Clone)]`, manuelles `Debug`, `Default` → `Delegated`. Poison-Behandlung `*poisoned.into_inner()` (lesend) bzw. `*poisoned.into_inner() = mode` (schreibend, Temporary lebt bis Anweisungsende) ist korrekt. Doctest-Pfad `harw_extension_api::approval_mode` ist `pub mod`.
- `into_builder`: alle 7 Felder aus `ExtensionRegistry` (`registry.rs:127-142`) 1:1 in `ExtensionRegistryBuilder` (`:302-310`), inklusive Namespace-Map. `expect_err` braucht `ExtensionRegistryBuilder: Debug`, vorhanden (manuell, `:318`).
- `SessionActivation::intersect`: alle 5 Felder berücksichtigt. Beweis nachgerechnet: Außerhalb der Kandidatenmenge erlaubt ein Coding-/Minimal-Profil nie etwas, daher gilt für alle Namen `result.is_tool_enabled(n) == a && b`, sowohl im Fall „beide Full“ (Full + Disabled) als auch sonst (Minimal + Extras). Instructions/Context über die Vereinigung der Disabled-Mengen. Typen (`HashSet<ToolName>`, `BTreeSet<String>`) passen. Tests kompilieren (`&&str` → `&str` per Deref-Koerzion).

**W0B-02**
- `evaluate_c_build`: Closure-/Lifetime-Typen korrekt, Format-String mischt Inline- und Positionsargumente zulässig. `ManifestFacts` wird außerhalb von `facts()` nur via `Default` gebaut (`:2227`), das neue Feld bricht also keinen Literal-Aufbau. Beide Parse-Stellen befüllen `build_tool_names`. `C_BUILD_EXCEPTIONS` ist korrekt über `is_c_build_exception` verdrahtet und wirkt nur auf das cc/bindgen-Signal, nicht auf `links`.
- `harw/Cargo.toml`: `toml = { workspace = true }` → `Cargo.toml:126` `toml = "1.1.3"`. Die Entfernung von `cargo_metadata` bricht nichts (kein weiterer Nutzer in `harw/` oder im Workspace). Übrige `toml`-API ist gültig: `Value::get` (`value.rs:82`), `as_str`/`as_bool`/`as_array` und `Value::String`/`Value::Table` in Mustern.

## Fix-Aufgaben nach Datei

| Datei | Aufgabe | Befund | Zuständig |
|---|---|---|---|
| `harw/tests/version_coherence.rs` | Dokumente als `toml::Table` parsen, in `toml::Value::Table` einwickeln (Z. 90, 344, 351, 358, 365), API-Kommentar Z. 84-87 korrigieren | R3-01 | W0B-02 |
| `xtask/src/gate_warden.rs` | `reason`/`since` in `c_build::run` ausgeben (oder `#[expect(dead_code, reason=…)]`) | R3-03 | W0B-02 |
| `xtask/src/gates.rs` | `run()`: Meldung für `checked == 0`; `summary()`: eigener Zweig „nichts geprüft“ | R3-05 | W0B-02 |
| `xtask/src/gate_edges.rs` | Z. 373-378: Test auf rot drehen und umbenennen | R3-02 | fremd, Orchestrator |
| `xtask/src/gate_privileges.rs` | Z. 814 und Z. 957: Erwartung auf rot drehen; Moduldoku ~Z. 118 ergänzen | R3-02, R3-06 | fremd, Orchestrator |
| `xtask/src/gate_writescopes.rs` | Z. 1407: `assert!(!report.is_green())` | R3-02 | fremd, Orchestrator |
| `harw-runtime/src/spec.rs` | Typalias `Row` für die Erwartungstabelle im Testmodul (Z. 340) | R3-04 | W0B-05 |
| `harw-runtime/src/error.rs` | Typalias `Make = fn(String) -> RuntimeError` (Z. 55) | R3-04 | W0B-05 |
| `harw-extension-api/src/lib.rs` | optional: `ApprovalHandlerKind`, `ApprovalModeCell` re-exportieren | R3-07 | fremd, Orchestrator |
| `docs/remediation/ledger/W0b/W0B-02.md` | Aussage zu Cargo.lock (Risiko 3) und die Liste der brechenden Tests (Risiko 1) korrigieren | R3-09, R3-02 | W0B-02 |
| alle genannten | `cargo fmt --all` | R3-10 | alle |

## Kompiliert voraussichtlich (rustc, `--all-targets`)

| Crate | rustc | clippy `-D warnings` | Tests |
|---|---|---|---|
| harw-types | ja | ja | grün |
| harw-operations | ja | ja | grün |
| harw-runtime | ja | **nein** (R3-04, Konfidenz mittel) | grün |
| harw-tui (nur Manifest) | ja | ja (unverändert) | unverändert |
| harw-cli (nur Manifest) | ja | ja (unverändert) | unverändert |
| harw-extension-api | ja | ja | grün |
| harw-core | ja | ja | grün |
| harw (Fassade + tests) | ja | ja | **rot**: 5 von 6 (R3-01) |
| xtask | ja (1 dead_code-Warnung) | **nein** (R3-03) | **rot**: 4 (R3-02) |
| Nutzer von `PermissionTier` (harw-web, harw-ops, harw-macros-Tests, harw-core-bridge, harw-observe, harw-tui, harw-cli) | ja | unverändert | unverändert |

Befundzahl: 1 blocker, 3 hoch, 1 mittel, 5 niedrig.
