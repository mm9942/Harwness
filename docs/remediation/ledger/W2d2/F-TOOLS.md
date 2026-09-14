# W2d-2 — Fix-Agent F-TOOLS — `handle_tools_command` entfernt (Befund R6)

Rolle: focused-bug-fix (Opus). BUILD-POLICY eingehalten: nur lesen und grep,
`cargo metadata --offline --no-deps --format-version 1` ausgeführt (Exit 0).
Kein `cargo build/check/test/clippy/run/add`, kein `make`/`rustc`/
`rust-analyzer`, keine git-Schreibbefehle. **Nichts kompiliert, keine Tests
ausgeführt.** Verifikation ausschließlich durch Lesen und Grep.

Owned files: `harw-tui/src/tools_command.rs`,
`docs/remediation/ledger/W2d2/F-TOOLS.md` (diese Datei).

## Befund R6

`pub fn handle_tools_command` (zuletzt Z.140-165) rief `set_tool`, `reset_all`,
`reset_one`, `set_profile` direkt auf — ohne jede Deckenprüfung (kein
`ceiling`-Parameter). Der einzige Prod-Aufrufer im Workspace
(`harw-tui/src/app.rs:1442`) nutzt bereits `dispatch_tools_command_bounded`
(mit `ceiling = gateway.session_mut().mode_ceiling()`), nicht
`handle_tools_command`. Der unbegrenzte Pfad war also toter, aber öffentlich
erreichbarer Code — genau das Risiko aus R6.

### Grep-Nachweis vor der Änderung (workspace-weit, `*.rs`)

- `handle_tools_command`: nur Treffer in `tools_command.rs` selbst (eigene
  Definition, eigene Doctests, eigene Tests in `mod tests`). Kein Treffer in
  `harw-tui/src/app.rs`, `harw-cli/**`, `harw-web/**` oder sonst irgendwo im
  Workspace (`cargo metadata --offline` listet alle 90 Member-Crates; grep lief
  über das gesamte Repo, nicht nur `harw-tui`).
- `dispatch_tools_command_bounded`: Treffer in `tools_command.rs` (Definition,
  Tests) und in `harw-tui/src/app.rs` (Docs Z.44/Z.1271 + Produktionsaufruf
  Z.1442 mit `&ceiling`).
- `harw-tui/src/lib.rs` exportiert `tools_command` nur als `pub mod
  tools_command;` — **kein** `pub use tools_command::handle_tools_command`
  oder ähnliches an der Crate-Wurzel. Da `handle_tools_command` komplett aus
  `tools_command.rs` entfernt wurde (kein verbleibendes Symbol dieses Namens),
  war keine Änderung an `lib.rs` nötig und damit auch kein BLOCKED-Fall im
  Sinne der Brief-Vorgabe „falls lib.rs es exportiert“.

## Fix

- `pub fn handle_tools_command` (samt vollständigem Doc-Kommentar inkl. des
  darin eingebetteten Doctests, vormals ~Z.90-139) **ersatzlos entfernt**.
- Modul-Doctest (vormals ~Z.24-33) auf `dispatch_tools_command_bounded`
  umgeschrieben (Snapshot + `ceiling` statt `ExtensionRegistry`); der zweite,
  in `handle_tools_command`s eigenem Doc-Kommentar eingebettete Doctest
  (vormals ~Z.124-138) ist mit der Funktion selbst entfallen.
- Modul-`//!`-Doku (Abschnitt „Design“, „Concurrency“) angepasst: beschreibt
  jetzt den Snapshot/Ceiling-Ansatz statt einer direkten
  `ExtensionRegistry`-Abhängigkeit dieses Moduls.
- Sub-Handler-Prüfung (je per Grep, siehe unten):
  - `list_tools` — nur von `handle_tools_command` aufgerufen → **entfernt**
    (samt Doc-Kommentar).
  - `set_tool` — von `handle_tools_command` **und** von `bounded_set_tool`
    (Z.283, Produktionspfad) aufgerufen → **bleibt bestehen**, unverändert.
  - `reset_all` — nur von `handle_tools_command` aufgerufen (nicht zu
    verwechseln mit dem separaten `bounded_reset_all`, das
    `dispatch_tools_command_bounded` nutzt) → **entfernt**.
  - `reset_one` — nur von `handle_tools_command` aufgerufen (separat von
    `bounded_reset_one`) → **entfernt**.
  - `set_profile` — nur von `handle_tools_command` aufgerufen (separat von
    `bounded_set_profile`, das intern `SessionActivation::set_profile`
    (Methode aus `harw-core`, nicht die entfernte freie Funktion) nutzt) →
    **entfernt**.
- Zwei tote Intra-Doc-Links auf die jetzt entfernten Funktionen korrigiert
  (waren zuvor `[`reset_all`]` in `bounded_reset_all`s Doku und
  `[`set_profile`]` in `bounded_set_profile`s Doku) — auf Prosa ohne Link
  umgestellt, damit kein unauflösbarer Intra-Doc-Link entsteht.
- Doku von `dispatch_tools_command_bounded` (Abschnitt zu W2d-2/CE/E8) ergänzt:
  nennt jetzt auch die Entfernung von `handle_tools_command` (R6) und
  korrigiert die vorherige (durch CE geerbte) Aussage „die Sub-Handler … bleiben
  bestehen, da `handle_tools_command` und diese Funktion sie weiterhin
  nutzen“ — das stimmte nur noch für `set_tool`.
- `use harw_extension_api::{ExtensionRegistry, ToolName};` → `use
  harw_extension_api::ToolName;`. `ExtensionRegistry` wurde ausschließlich von
  `list_tools` und `handle_tools_command` referenziert (beide entfernt); ein
  belassener Import wäre ein `unused import` unter `-D warnings` gewesen.

## Tests

`handle_tools_command` hatte 9 eigene Tests. Je Test geprüft, ob er
Parsing/Ausgabe des `/tools`-Kommandos prüft (→ auf
`dispatch_tools_command_bounded` migriert) oder ein reines Duplikat einer
bereits vorhandenen `dispatch_tools_command_bounded`-Testabdeckung ist (→
gelöscht, kein Ersatz nötig):

| Alter Test (`handle_tools_command`) | Entscheidung | Neuer/abdeckender Test |
|---|---|---|
| `list_returns_profile_line_when_no_tools` | migriert | `test_dispatch_tools_command_bounded_list_returns_profile_line_when_no_tools` |
| `list_shows_registered_tools_with_status` | migriert (Snapshot statt Registry-Provider) | `test_dispatch_tools_command_bounded_list_shows_known_tools_with_snapshot_status` |
| `on_enables_disabled_tool` | migriert — **keine** vorhandene Bounded-Abdeckung für „on innerhalb der Decke erfolgreich“ (nur „on jenseits der Decke abgelehnt“ existierte) | `test_dispatch_tools_command_bounded_on_enables_disabled_tool_within_ceiling` |
| `off_disables_tool` | **gelöscht**, reines Duplikat | bereits abgedeckt durch `test_dispatch_tools_command_bounded_disable_known_tool_applies` |
| `reset_all_reverts_overrides` | **gelöscht**, auf Parsing-Ebene Duplikat | bereits abgedeckt durch `test_dispatch_tools_command_bounded_reset_restores_ceiling` |
| `reset_one_reverts_single_tool` | **gelöscht**, auf Parsing-Ebene Duplikat | bereits abgedeckt durch `test_dispatch_tools_command_bounded_reset_one_keeps_ceiling_disabled_tool_off` und `..._reset_then_reset_one_stays_within_ceiling` |
| `profile_switch_changes_activation_profile` | migriert, aber **nicht** 1:1 — siehe Hinweis unten | `test_dispatch_tools_command_bounded_profile_switch_reports_plain_confirmation_when_unrestricted` |
| `profile_switch_rejects_unknown_name` | migriert (früher Error-Return, unverändert von Ceiling-Logik betroffen) | `test_dispatch_tools_command_bounded_profile_switch_rejects_unknown_name` |
| `unknown_subcommand_returns_usage_error` | migriert (Wildcard-Arm identisch in beiden Funktionen) | `test_dispatch_tools_command_bounded_unknown_subcommand_returns_usage_error` |

### Wichtiger Unterschied bei der Profil-Migration

Der alte Test `profile_switch_changes_activation_profile` prüfte
`activation.profile() == ToolProfile::Coding` nach `/tools profile coding`.
Das gilt **nicht** 1:1 für den bounded Pfad: `bounded_set_profile` setzt
`activation = requested.intersect(ceiling)`
(`SessionActivation::intersect`, `harw-core/src/activation.rs:395`). Laut
Doku dieser Methode (Zeilen ~355-373 ebd.) ist das Ergebnisprofil nur dann
`Full`, wenn **beide** Seiten `Full` sind — sonst ist es immer `Minimal` (mit
`tools_enabled_extra` für die tatsächlich von beiden Seiten erlaubten Namen).
Bei `ceiling = Full`, `requested = Coding` ist also `effective.profile() ==
Minimal`, nicht `Coding`, obwohl die Werkzeugsichtbarkeit exakt der
Coding-Allowlist entspricht. Der migrierte Test prüft deshalb bewusst
`is_tool_enabled("fs.read")` (Coding-Allowlist, muss an sein) und
`!is_tool_enabled("custom.tool")` (außerhalb der Allowlist, muss aus sein)
statt `activation.profile()` — verifiziert durch Lesen von
`harw-core/src/activation.rs:107-119` (`ToolProfile::allowlist`), `:233-243`
(`is_tool_enabled`) und `:395-` (`intersect`), nicht durch Ausführung (keine
Tests liefen, BUILD-POLICY).

## API-Nachweise (durch Lesen, `harw-core/src/activation.rs`)

- `ToolProfile::allowlist(self) -> Option<HashSet<ToolName>>` (:107-119):
  `Minimal` → `Some(∅)`, `Coding` → `Some({fs.read, fs.write, fs.list,
  fs.search, shell.exec})`, `Full` → `None`.
- `SessionActivation::is_tool_enabled` (:233-243): `tools_disabled` schlägt
  `tools_enabled_extra`, das wiederum die Profil-Allowlist schlägt.
- `SessionActivation::intersect` (:395-…): Kandidatenmenge = Vereinigung aller
  expliziten Overrides beider Seiten plus beider Profil-Allowlists;
  Ergebnisprofil `Full` nur wenn beide Seiten `Full`, sonst `Minimal` mit
  `tools_enabled_extra` für jeden von beiden Seiten erlaubten Kandidaten.
- `harw-tui/src/runtime_commands.rs:139-157` (`validate_tool_toggle`):
  unbekannter Name → `UnknownTool` (auch bei `off`); `enable && !ceiling
  .is_tool_enabled(name)` → `BeyondCeiling`; sonst `Ok`. Genutzt von
  `bounded_set_tool` (Z.271-284 in `tools_command.rs`), unverändert.

## Aufräumen im Testmodul

Nach dem Löschen aller `handle_tools_command`-spezifischen Tests war die
komplette Registry-Testinfrastruktur unbenutzt (kein verbleibender Aufrufer
im Workspace geprüft, siehe Grep unten) und wurde ebenfalls entfernt, um
`dead_code`/`unused_imports` unter `-D warnings` zu vermeiden:

- Struct `FakeToolProvider`, Funktion `make_tool_spec`, `impl ToolProvider for
  FakeToolProvider`, Funktionen `registry_with_tools()` und
  `empty_registry()` — entfernt.
- Imports `harw_extension_api::contributors::ToolProvider`,
  `harw_extension_api::registry::ExtensionRegistryBuilder`,
  `harw_extension_api::{ToolExecutor, ToolSpec}` (das dortige `ToolName` kam
  ohnehin nur redundant zum bereits via `use super::*` sichtbaren
  `harw_extension_api::ToolName` aus dem äußeren Modul), `harw_tools::
  {FunctionToolSpec, JsonSchema}`, `std::sync::Arc` — entfernt.
- `harw_tools` bleibt Workspace-Dev-Dependency von `harw-tui`
  (`Cargo.toml:41`, unter `[dev-dependencies]`) — geprüft per Grep:
  `approval.rs`, `app.rs`, `history_cell.rs`, `session_controller.rs` nutzen
  es weiterhin in ihren jeweiligen `#[cfg(test)] mod tests`-Blöcken. Kein
  Cargo.toml-Eintrag berührt (nicht im Owned-Scope).
- `both_known()`, `full_activation()`, `bounded_ceiling()`,
  `full_ceiling_without_beta()` — unverändert übernommen, werden weiterhin von
  den bereits vorhandenen `dispatch_tools_command_bounded`-Tests **und** den
  migrierten Tests genutzt.

## Restgrep nach der Änderung

- `grep -rn "handle_tools_command" --include="*.rs" .` → nur noch
  Prosa-Erwähnungen in Doc-Kommentaren/Testnamen von `tools_command.rs`
  selbst (`removed handle_tools_command`, kein Funktionssymbol mehr).
- `grep -n "ExtensionRegistry\|ToolProvider\|ToolExecutor\|ToolSpec\b\|
  FunctionToolSpec\|JsonSchema\|registry_with_tools\|empty_registry\|
  FakeToolProvider\|make_tool_spec" harw-tui/src/tools_command.rs` → nur noch
  ein einziger Treffer, der fully-qualified Intra-Doc-Link
  `[`harw_extension_api::ExtensionRegistry`]` im Modul-Doc (gültig, da
  crate-qualifizierter Pfad, unabhängig vom lokalen `use`).
- `cargo metadata --offline --no-deps --format-version 1` — Exit 0 (Workspace
  weiterhin auflösbar; diese Änderung berührt keine `Cargo.toml`).
- Klammern-Bilanz der Datei geprüft (77 `{` / 77 `}`).

## Abweichungen vom Brief

Keine. `lib.rs` nicht angefasst (kein Re-Export von `handle_tools_command`
gefunden, daher kein BLOCKED-Fall). `dispatch_tools_command_bounded`s Signatur
unverändert — `app.rs:1442` (W2d-2/CE-Aufrufer) bleibt kompatibel.

## Bekannte, geerbte Inkonsistenz (nicht Teil dieses Fixes)

`bounded_set_tool`s Doku verlinkt `[`set_tool`]` (privates Item) als
Intra-Doc-Link statt als Backtick-Prosa — dieselbe Art Problem, die T8
(W2d-1/F-T) für andere Stellen bereits behoben hat. Diese eine Stelle existierte
bereits vor meiner Änderung (nicht durch R6/F-TOOLS eingeführt) und liegt
außerhalb des R6-Befundumfangs; nicht angefasst, um den Diff auf R6 zu
beschränken. Für den Orchestrator als Notiz vermerkt.

## Ausgabe

```json
{
  "agent": "F-TOOLS",
  "befund": "R6",
  "files_created": ["/home/mia/projects/harwness/docs/remediation/ledger/W2d2/F-TOOLS.md"],
  "files_modified": ["/home/mia/projects/harwness/harw-tui/src/tools_command.rs"],
  "removed_items": ["handle_tools_command", "list_tools", "reset_all", "reset_one", "set_profile"],
  "kept_items": ["set_tool (used by bounded_set_tool)", "list_from_snapshot", "bounded_set_tool", "bounded_reset_all", "bounded_reset_one", "bounded_set_profile", "dispatch_tools_command_bounded (signature unchanged)"],
  "tests_migrated": 6,
  "tests_deleted_as_duplicate": 3,
  "lib_rs_export_found": false,
  "verification": {
    "command": "cargo metadata --offline --no-deps --format-version 1",
    "exit_code": 0,
    "pass": true,
    "method": "read-only (grep + full file read); no cargo build/check/test/clippy run per BUILD-POLICY"
  },
  "blocked": false
}
```
