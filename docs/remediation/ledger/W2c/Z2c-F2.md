# Z2c-F2 — Remediation der Befunde aus `review-Z2c.md`

- Welle: W2c, Fix-Lauf 2 (Vorgänger `Z2c-F1` wurde abgebrochen, ohne Ledger)
- Umfang (Schreibrecht): `harw-runtime/src/{assembly.rs, children.rs,
  services.rs, approval.rs}`, `harw-runtime/tests/rights_matrix.rs`, dieses
  Ledger. Sonst nichts.
- Ausgeführt: nur `cargo metadata --offline --no-deps --format-version 1`
  (Exit 0). **Nichts kompiliert, kein Test, kein clippy, kein `cargo fmt`** —
  in dieser Welle verboten. Zeilenbreite manuell geprüft (`awk 'length>100'`:
  0 Treffer in allen vier Quelldateien und im Testfile; die acht Überlängen in
  `services.rs` stehen in Doc-Tabellen aus W2b und liegen außerhalb dieses
  Auftrags — rustfmt bricht Doc-Kommentare nicht um).
- Kein `#[allow]`, kein `todo!`/`unimplemented!`, kein Platzhalter in den fünf
  Dateien (`grep`-geprüft, siehe §4).

## 1. Was vom Vorgänger übernommen wurde (geprüft, unverändert)

Der Stand von `Z2c-F1` lag uncommittet vor. Ich habe ihn Zeile für Zeile gegen
die tatsächlichen Fremdsignaturen gelesen und **übernommen**:

- **`approval.rs` — Z2c-02, Teil 1.** Neuer `pub struct AskResolutionPolicy`
  (Label `ask:<auflösung>` über `label_for`, `kind() == Other`). Er lehnt
  **genau** die Aufrufe ab, für die die übrige Kette dieses Laufs `AskUser`
  liefern würde — Vorhersage aus denselben Quellen
  (`ConfigApprovalPolicy::requires_approval`,
  `DefaultApprovalPolicy::requires_explicit_approval` über derselben
  `ApprovalModeCell`). Ein pauschales `Deny` wäre falsch, weil `check_approval`
  über **alle** Handler aggregiert (`Deny > AskUser > Allow`). `for_root` ist
  dadurch 4-stellig (`ask: AskResolution` als zweites Argument), `for_child`
  erbt die Auflösung über der gelösten Kindzelle. Belegt und richtig.
- **`approval.rs` — Dublette (Z2c-06 / Ledger §4.9).** Neu:
  `install_over_default(ExtensionRegistry) -> ExtensionRegistryBuilder`, das
  die eigene `DefaultApprovalPolicy` **auslässt**, plus `handlers_with_default`
  als gemeinsame Grundlage.
- **`assembly.rs`.** Z2c-01 (`build_operations` + `install_operation_model_tools`),
  Z2c-02 Teil 2 (Prüfung in `new_root_session`), Z2c-03 (erschöpfendes `match`
  über `SpawnerPolicy`), Z2c-05 (`config.executable_agents` vor den eingebauten
  Rollen), Z2c-07 (`lower_agent_definitions` genau einmal je Montage),
  Z2c-09 (`lock().ok().and_then(..)` statt verschachteltem `if let`),
  Z2c-12 (`assert_eq!(.., Delegated)`).
- **`children.rs`.** `with_definitions` (Z2c-07) und `install_over_default`
  (Z2c-06) in der Kind-Registry.
- **`services.rs`.** Nur Rustdoc: die Trennung der beiden Achsen
  (`OperationSurface` = *welche Operationen gibt es*, `ServiceSurface` =
  *welche Dienste sieht eine Operation*) und die Zusage, dass diese Fabrik
  **nicht** noch einmal verengt.

## 2. Was ich ergänzt habe

### 2.1 Lücke im Vorgängerstand: die Testdatei war nicht nachgezogen

`tests/rights_matrix.rs` stand noch auf der **alten** 3-stelligen
`ApprovalChain::for_root` (drei Aufrufstellen) und hätte den Crate nicht mehr
übersetzt. Alle drei sind jetzt 4-stellig (`AskResolution::Interactive`).

### 2.2 Z2c-04 — `cwd`-Apparat ersatzlos entfernt

`cwd_lock()` samt `static LOCK` und alle fünf `set_current_dir`-Paare sind weg;
`Mutex`/`MutexGuard`/`OnceLock` verschwinden aus den Importen (`Mutex` kehrt
für den Haken-Test aus §2.4 zurück). Das Arbeitsverzeichnis kommt
ausschließlich aus `RuntimeSpec::cwd`; kein Montageschritt liest das des
Prozesses (`load_config` → `config_layers_report_at(&spec.home, &spec.cwd)`,
`discover_project(&spec.cwd, ..)`, `root_sandbox(entry, &project.project_root)`).
Der Apparat serialisierte nicht nur unnötig, er wirkte prozessweit auf alle
Threads desselben Testbinaries.

### 2.3 Neue Tests (Z2c-11)

- `the_operation_surface_reaches_the_assembled_run` — über **alle** elf
  Einstiege: `OperationSurface::None` ⇒ `operations().is_empty()` (damit
  `McpServe`, `JobPrompt`, `JobPlanNode`, beide Gateways und `LocalEcho`),
  sonst nicht leer.
- `only_the_full_surface_offers_operations_to_the_model` — `Tui`/`OneShot`
  führen jede Operation mit `Surface::ModelTool` in `rights_snapshot().tools`;
  `Analyze`/`Web` führen **keine** davon, behalten aber ihre
  Command-Operationen. Die Namensliste wird aus dem montierten Lauf abgeleitet
  (`by_surface` + `OperationMeta::name` = `ModelToolAdapter::tool_name`), nicht
  hart hingeschrieben.
- `only_an_interactive_entry_accepts_an_approval_responder` — je Einstieg:
  `Interactive` nimmt einen Responder an (er steht danach im Snapshot, ohne
  Ask-Handler); jede andere Auflösung weist ihn mit `RuntimeError::Registry`
  ab, montiert **ohne** ihn weiter (die Prüfung steht vor der Registry-Vergabe)
  und führt danach `("ask:<auflösung>", Other)` im Snapshot.
- `every_entry_carries_exactly_one_default_policy` — je Einstieg genau **ein**
  `DefaultPolicy`-Eintrag, und sein Label ist `DEFAULT_POLICY_LABEL`.
- `the_child_registry_inherits_the_config_approval_policy` verschärft: statt
  `len() >= parent_handlers` jetzt `registry.approval_handlers().len() ==
  chain.for_child().snapshot().len()` — das ist die Dublettenprobe auf der
  Registry-Seite.
- Die Erwartungstabelle (`expected`) trennt `Tui` von `OneShot`/`Analyze`:
  jeder nicht-interaktive Einstieg trägt jetzt `[DefaultPolicy, Other]`.

### 2.4 Z2c-11(c) — der Erweiterungspunkt ist nicht mehr ungetestet

`closing_a_session_reaches_every_hook` hatte nichts Beobachtbares geprüft.
Jetzt hängt ein Test-`AssemblyContributor` (im Testfile, nicht in
`contributors.rs`) einen zählenden `SessionLifecycleHook` ein; der Test prüft,
dass der Beitrag die Montage erreicht **und** dass `close_session` Wurzel wie
Kind in Reihenfolge meldet.

### 2.5 Z2c-10 — rustfmt-instabile Stellen in meinen Dateien

- `assembly.rs:604` (104 Zeichen) auf mehrzeilige Argumente umgebrochen.
- `ExtensionRegistryBuilder` importiert statt zweimal voll qualifiziert
  (`harw_extension_api::…`) in der Signatur von
  `install_operation_model_tools`.
- Im Testfile: eine `unwrap_or_else`-Closure, die in eine Zeile passt, ohne
  Block; die mehrzeiligen Import-Blöcke nur dort, wo eine Zeile > 100 wäre.

- Die in Z2c-10 genannte Leerzeile vor dem schließenden `}` des Testmoduls
  steht nicht mehr in `assembly.rs` (maschinell geprüft: in keiner der fünf
  Dateien folgt einer Leerzeile eine reine Klammerzeile).

Offen bleibt `lib.rs` (Z2c-10, mehrzeiliger Import-Block) — außerhalb meines
Schreibrechts. Endgültig entscheidet ohnehin erst `cargo fmt --check` in W2d;
diese Welle durfte es nicht laufen lassen.

### 2.6 Kein `#[allow]` mehr

`build_spawner` trug ein dokumentiertes `#[allow(clippy::too_many_arguments)]`
(elf Parameter). Ein unterdrückter Lint ist in diesem Auftrag ausgeschlossen,
deshalb bündelt jetzt ein privates `struct SpawnerInputs<'a>` die neun
Leihgaben; die Funktion hat drei Parameter, der Rumpf destrukturiert einmal und
ist sonst unverändert. Die benannten Felder sagen an der Aufrufstelle mehr als
elf Positionen.

## 3. Die Dublette: warum die Erkennung ein Parameter und keine `kind()`-Probe ist

Der Auftrag nannte „Erkennung über `kind() == DefaultPolicy` in
`approval_handlers()` oder gleichwertig". **Die `kind()`-Probe ist nicht
möglich**, und das ist belegt, nicht vermutet:

- `ApprovalHandler::kind` hat die Vorgabe `ApprovalHandlerKind::Other`
  (`harw-extension-api/src/contributors.rs:321-324`).
- Weder `DefaultApprovalPolicy` (`harw-registry-defaults`) noch
  `ConfigApprovalPolicy` (`harw-core/src/policy.rs`) überschreiben sie.
  `grep -rn "fn kind(&self) -> ApprovalHandlerKind"` über den ganzen Workspace
  findet fünf Treffer: die Vorgabe im Trait, die neue `AskResolutionPolicy`
  und drei Testdoppel. `kind() == DefaultPolicy` findet in einer Registry also
  nie etwas.
- `ApprovalHandlerKind::DefaultPolicy` existiert trotzdem — aber nur in
  `ApprovalChain::snapshot()`, wo die Kette ihr eigenes Wissen über die
  Herkunft ausdrückt.

Gewählte, gleichwertige Form: die Aussage „diese Registry kommt von
`assemble_registry_for_project`" steht an der Aufrufstelle
(`install_over_default` in `assembly.rs` und `children.rs`) und wird dort
zusätzlich geprüft — trägt die übergebene Registry nicht **genau einen**
Handler, installiert die Kette ihre eigene Grundlinie doch, mit
`tracing::warn!`. Die fail-closed Grundlinie kann damit nie fehlen. Belegt:
`assemble_registry_for_project` registriert genau einen `ApprovalHandler`
(`harw-registry-defaults/src/profile.rs:920-921`, eigener Test `:1196-1197`).

Ergebnis: **genau eine** `DefaultApprovalPolicy` in Wurzel- und Kindregistry,
und `rights_snapshot().approval_chain` bildet die Registry ab. Geprüft von
`install_over_default_does_not_add_a_second_default_policy`,
`install_over_default_still_adds_the_baseline_to_an_empty_registry`
(`approval.rs`), `every_entry_carries_exactly_one_default_policy` und
`the_child_registry_inherits_the_config_approval_policy` (Rechte-Matrix).

## 4. Konsistenzprüfung der fünf Dateien

- Alle Aufrufe passen zu den Signaturen: `for_root` 4-stellig an **allen**
  sechzehn Aufrufstellen (12× `approval.rs`-Tests, 1× `assembly.rs`,
  3× Rechte-Matrix; einzeln nachgesehen),
  `RuntimeChildRegistryFactory::{new, with_definitions}` beide benutzt (`new`
  in den Tests, `with_definitions` in `build_spawner`), `build_operations` /
  `install_operation_model_tools` je genau einmal gerufen, `handlers_with_default`
  nur aus `handlers`/`install_over_default`.
- Keine verwaisten oder doppelten Funktionen; keine halben Edits.
  `grep -n "#\[allow\|todo!\|unimplemented!\|FIXME\|TODO"` über die fünf
  Dateien: 0 Treffer.
- `AskResolution` ist `Copy + PartialEq` (`spec.rs:50`), `ApprovalHandlerKind`
  `Copy + PartialEq` (`contributors.rs:293`), `SessionId` `Clone + PartialEq +
  Debug` (`harw-types/src/ids.rs:27-31`) — alle neuen `assert_eq!`/`!=`-Formen
  sind damit gedeckt.
- Kollisionsprobe für den Modell-Tool-Test: die Operationen mit
  `Surface::ModelTool` heißen `analyze`, `explore`, `diff`, `goal`,
  `research_deps`, `research_web`; die Werkzeuge von `RegistryProfile::Full`
  sind namensräumig (`fs.*`, `shell.*`, `web.*`, `deps.*`, `lens.*`,
  `browser.*`). Keine Überschneidung — die Disjunktheitszusage für
  `Analyze`/`Web` prüft wirklich die Modell-Tool-Fläche.

## 5. Offen (außerhalb meines Schreibrechts)

1. `docs/remediation/ledger/W2c/W2C-01.md`: §6 und §8.2 behaupten weiterhin,
   `harw_home` biete keine cwd-explizite Form — falsch (Z2c-04), der Apparat
   ist jetzt entfernt. §9 behauptete ein erschöpfendes `match` in
   `build_spawner`; das stimmt jetzt (Z2c-03), die Begründung sollte dort
   nachgezogen werden.
2. `harw-runtime/src/lib.rs`: Z2c-10 (mehrzeiliger `pub use
   contributors::{..}`-Block, der in 100 Spalten passt).
3. `harw-runtime/src/sandbox.rs`: Z2c-08 (`root_sandbox` ruft `entry.profile()`
   ein zweites Mal) — W2b-Signatur, also W2d.
4. `cargo fmt -p harw-runtime` + `clippy -D warnings` + `cargo test -p
   harw-runtime` als erste W2d-Aktion. Diese Welle durfte keinen davon laufen
   lassen; die Aussage „kompiliert" ist weiterhin eine Lesart, kein Beleg.
