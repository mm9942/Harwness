# W2d-2 — Agent CE — `execute_command_as`-Closure, `dispatch_tools_command` entfernt (E8), T5-Doku

Owned: `harw-tui/src/command_exec.rs`, `harw-tui/src/tools_command.rs`,
`harw-tui/src/session_controller.rs` (nur Rustdoc ~Z.195-215, neu ~Z.195-224).
Verifikation nur durch Lesen (BUILD-POLICY): kein `cargo build/check/test/
clippy/run`, kein `make`/`rustc`/`rust-analyzer`, keine git-Schreibbefehle
ausgeführt. Erlaubt und ausgeführt: `cargo metadata --offline --no-deps
--format-version 1` (Exit 0).

## 1. `command_exec.rs` — `execute_command_as<F>` (§1.2)

Exakte Signatur, wörtlich wie in CONTRACTS-W2d2.md §1.2 gefroren:

```rust
pub(crate) async fn execute_command_as<F>(
    adapters: &[CommandAdapter],
    sandbox: &SandboxSpec,
    session_id: &SessionId,
    caller_permission: PermissionTier,
    raw_line: &str,
    services: F,
) -> String
where
    F: FnOnce() -> ServiceMap;

#[cfg(test)] pub(crate) struct CommandServices<'a> { .. }
#[cfg(test)] pub(crate) fn build_services(..) -> ServiceMap;
```

- Bisheriger Parameter `services: &CommandServices<'_>` durch die generische
  Closure `services: F where F: FnOnce() -> ServiceMap` ersetzt. Die Closure
  wird **ausschließlich** in `execute_with_context` innerhalb des Zweigs
  `CommandAction::Command(spec, raw_args)` aufgerufen — nach dem
  `adapters.iter().find(..)`-Lookup und **vor** `CommandAdapter::dispatch`.
  Jeder Admission-Fehler (`CommandRegistry::dispatch` → `Err`) kehrt vorher via
  `render_admission_error` zurück; die Closure existiert an dieser Stelle im
  Code noch gar nicht im Scope der Ausführung (frühes `return`), wird also nie
  ausgewertet.
- `execute_with_context` (private, unveränderter Name) wurde parallel generisch
  gemacht (`async fn execute_with_context<F>(.., services: F) -> String where
  F: FnOnce() -> ServiceMap`), da `execute_command_as` nur an sie delegiert und
  denselben Closure-Typ durchreicht.
- Heutiger Parameterzuschnitt (vor dieser Änderung) geprüft: `adapters: &[CommandAdapter]`,
  `sandbox: &SandboxSpec`, `session_id: &SessionId`, `caller_permission: PermissionTier`,
  `raw_line: &str` blieben unverändert — nur der letzte Parameter (`services`)
  wechselt vom Struct-Bündel zur Closure. Sandbox/Session werden weiterhin als
  einfache Referenzen übergeben (keine versteckte Zusatzverpackung); das
  entspricht §1.2 exakt, keine BLOCKED-Situation.
- `CommandServices<'a>` und `build_services(..)` sind jetzt `#[cfg(test)]`.
  Ihre Feldsignatur/Parameterliste ist unverändert (`runtime_config`, `memory`,
  `controller`, `job_store`); nur die Sichtbarkeit außerhalb von Tests entfällt.
- Moduldoc (`//!`) um Abschnitt „Services-Closure (W2d-2/CE)" ergänzt: erklärt
  die Closure-Semantik, den Produktionsaufruf
  `|| runtime_commands::slash_service_map(rt.services())` (App.rs, T2a-Zuständigkeit)
  und dass `CommandServices`/`build_services` nur noch Testwerkzeuge sind.
- Docblocks von `execute_command_as` und `execute_with_context` entsprechend
  aktualisiert (Closure-Zeitpunkt, `# Nebenläufigkeit`, `# Argumente`).

### Migrierte Tests (Struct-Bündel → Closure-Aufruf, Verhalten unverändert)

Der `#[cfg(test)]`-Wrapper `execute_command` (Owner-Tier-Kompatibilität) blieb
mit Signatur `(.., services: &CommandServices<'_>)` bestehen und baut die
Closure jetzt intern selbst (`|| build_services(adapters, services.runtime_config,
services.memory, services.controller, services.job_store)`). Dadurch mussten
**alle** Tests, die nur über `execute_command(...)` laufen, nicht angefasst
werden (11 Stück: `test_help_lists_registered_operations`,
`test_status_embeds_session_id`, `test_unknown_command_returns_honest_message`,
`test_shell_not_available`, `test_shell_repeat_not_available`,
`test_plain_text_is_chat`, `test_note_prefix_renders_note`,
`test_mention_renders_correctly`, `owner_wrapper_dispatches_operator_command_without_error`,
`test_empty_adapters_reports_unknown_for_any_command`,
`alias_dispatches_to_same_handler_as_canonical`).

Direkt auf `execute_command_as`/`execute_with_context` migriert (Struct-Literal
durch `|| build_services(&adapters, .., &controller, ..)`-Closure ersetzt,
keine Assertion geändert):

- `observer_cannot_dispatch_an_operator_command`
- `denied_alias_does_not_build_services_or_dispatch`
- `test_execute_command_as_observer_cannot_run_operator_command` (zwei
  Aufrufe → zwei separate Closures, da `F: FnOnce`)
- `test_execute_command_as_unknown_command_suggests_nearest`
- `test_execute_with_context_admitted_shell_is_not_available` (zwei Aufrufe →
  zwei separate Closures)

Keine Tests gelöscht. `use super::{CommandServices, build_services};` im
Testmodul ergänzt (vorher nur `CommandServices`).

### Imports (Produktions-Build ohne unused-import unter `-D warnings`)

Da `CommandServices` und `build_services` jetzt `#[cfg(test)]` sind, wurden die
Importe, die außerhalb von `mod tests` ausschließlich von diesen beiden Items
gebraucht werden, ebenfalls `#[cfg(test)]`-gebunden (sonst unused-import in
einem Produktions-Build, was gegen die clippy `-D warnings`-Pflicht aus
AGENT-BRIEF.md §4 verstieße): `std::collections::HashSet`, `std::sync::Arc`,
`harw_operations::registry::OperationRegistry`,
`harw_operations::SharedSessionController` (aus dem kombinierten
`harw_operations`-`use` herausgelöst; `OpContext`/`PermissionTier`/`ServiceMap`
bleiben ungated, sie werden von `execute_with_context`/`execute_command_as`
gebraucht), `crate::session_controller::TuiSessionController`. Geprüft: keiner
dieser Namen wird außerhalb von `CommandServices`/`build_services` und
`mod tests` (das eigene, unabhängige `use`-Deklarationen hat) referenziert.

### Neuer Test

`test_execute_command_as_does_not_build_services_on_denied_admission`:
`AtomicUsize`-Zähler in der Closure. Erster Aufruf mit `PermissionTier::Observer`
gegen `/model list` (Operator-Command) → Admission verweigert, Zähler bleibt
`0` (assertiert). Zweiter Aufruf mit `PermissionTier::Operator` gegen dieselbe
Zeile → admittiert, Zähler wird genau `1` (assertiert), Ausgabe ist weder
„Unbekannter Command" noch „Berechtigung verweigert". Beweist, dass die
Closure nicht bei Admission-Ablehnung, sondern exakt einmal nach erfolgreicher
Admission ausgewertet wird — der zentrale Vertrag aus §1.2.

## 2. `tools_command.rs` — `dispatch_tools_command` entfernt (E8)

- Die unbegrenzte `pub fn dispatch_tools_command(args, tool_snapshot, activation)
  -> ToolsCommandOutcome` (vormals ~Z.200-225, keine Deckenprüfung) wurde
  ersatzlos gestrichen.
- Grep vor der Löschung bestätigt: **kein** Test in `tools_command.rs` ruft
  `dispatch_tools_command(` (unbegrenzt) auf — alle bestehenden Tests decken
  entweder `handle_tools_command` oder `dispatch_tools_command_bounded` ab.
  Daher keine Testmigration/-löschung nötig; die Vorgabe „Tests löschen oder
  auf `dispatch_tools_command_bounded` umstellen" trifft auf keinen
  vorhandenen Test zu.
- Helfer `list_from_snapshot`, `set_tool`, `reset_all`, `reset_one`,
  `set_profile` **behalten** — `handle_tools_command` (unverändert) und
  `dispatch_tools_command_bounded` (unverändert) nutzen sie weiterhin.
- Doku-Verweise angepasst:
  - Ehemals :227 (Doc-Kommentar über `dispatch_tools_command_bounded`, der auf
    `[dispatch_tools_command]` zurückverwies): neuer Text beschreibt die
    begrenzte Funktion eigenständig und verweist stattdessen erklärend auf die
    entfernte Funktion (als Prosa in Backticks, kein Intra-Doc-Link mehr, also
    kein toter `[`…`]`-Link).
  - Ehemals :345 (Doc von `list_from_snapshot`, `[dispatch_tools_command]`):
    auf `[dispatch_tools_command_bounded]` umgestellt — das ist der einzige
    verbleibende Aufrufer dieser Funktion.
- Restgrep nach Löschung: `grep -n "dispatch_tools_command\b" tools_command.rs`
  liefert nur noch die erklärende Prosa-Zeile (kein Funktionssymbol, kein
  Intra-Doc-Link mehr).

## 3. `session_controller.rs` — T5-Doku (`/mode <gleich>` No-op)

Nur Rustdoc am `apply_to_session`-Docblock ergänzt (kein Codepfad geändert,
Semantik war laut Befund T5 „bereits korrekt"). Neuer Absatz nach dem
bestehenden `/model`/`/effort`-Absatz, referenziert Befund
`docs/remediation/ledger/W2d1/Z2d1-tui.md` T5:

> `/mode <gleicher Modus>` … setzt zwar `inner.interaction_mode`, aber der
> Vergleich `session.mode() != mode` … bleibt `false` — `set_mode` wird nicht
> aufgerufen. Das ist absichtlich ein No-op sowohl für die Tool-Aktivierung
> (kein erneuter Schnitt aus der Basis, W2A-01) als auch für zur Laufzeit
> gesetzte Overrides (`activation_mut()`-Änderungen … bleiben erhalten).
> Effort/Modell/Provider werden davon unabhängig trotzdem übernommen, falls
> mitgesendet.

Intra-Doc-Link `[`SessionController::request_mode`]` geprüft: Methode ist über
das bereits importierte `harw_operations::SessionController`-Trait im Scope
(`impl SessionController for TuiSessionController` in derselben Datei,
Z.336) — kein toter Link.

## 4. Abweichungen vom Brief

Keine. `execute_command_as`/`CommandServices`/`build_services` entsprechen
§1.2 wörtlich; `dispatch_tools_command` entfernt wie unter E8 vorgegeben;
T5-Doku ergänzt wie vorgegeben. `app.rs` (Stufe 2 / T2a) nicht angefasst.

## 5. Bekannte, akzeptierte Inkonsistenz (Stufe 2 löst auf)

`app.rs:114` importiert weiterhin `use crate::command_exec::{CommandServices,
execute_command_as};` — `CommandServices` ist jetzt `#[cfg(test)]` und in
einem normalen Build für `app.rs` (nicht-Test-Code) nicht mehr sichtbar; die
beiden Aufrufstellen `app.rs:2729` und `app.rs:4924/4941` (letztere bereits in
Tests) übergeben zudem noch `&CommandServices { .. }` statt der neuen
`F: FnOnce() -> ServiceMap`-Closure. `app.rs:2707` ruft weiterhin das nun
entfernte `crate::tools_command::dispatch_tools_command` auf. Beides ist laut
CONTRACTS-W2d2.md-Kopfnotiz „Der Tree kompiliert vor Abschluss von W2d-2 nicht"
erwartet und ausdrücklich T2a (Stufe 2, §2 der CONTRACTS) zugewiesen:
`execute_command_as(app.adapters(), app.sandbox(), app.session_id(),
runtime_commands::caller_tier(rt.principal()), &raw, ||
runtime_commands::slash_service_map(rt.services())).await` sowie
`dispatch_tools_command_bounded(&args, &tool_names, gateway.session_mut().activation_mut(),
&ceiling)`. CE hat `app.rs` bewusst nicht angefasst (außerhalb der Owned
files).

## 6. Verifikation

- `cargo metadata --offline --no-deps --format-version 1` — Exit 0 (Workspace
  weiterhin auflösbar; diese Änderung berührt keine `Cargo.toml`).
- `command_exec.rs`, `tools_command.rs`, `session_controller.rs` vollständig
  gelesen vor und nach jeder Änderung.
- Kein `unwrap()`/`expect()` in Produktionspfaden hinzugefügt (Tests nutzen
  weiterhin `.expect(..)`, unverändert aus dem Bestand).
- Kein neues `#[allow(...)]`.
- Kein `println!`.
- Signaturen von `execute_command_as`, `CommandServices`, `build_services`
  stimmen wörtlich mit CONTRACTS-W2d2.md §1.2 überein.
- `grep -n "dispatch_tools_command\b" tools_command.rs` zeigt nach der
  Änderung nur noch die erklärende Prosa-Zeile, kein Funktionssymbol.

## 7. Stubbed imports

Keine.

## 8. Annahmen

Keine (Brief war vollständig; heutiger Parameterzuschnitt von
`execute_command_as` wurde vor der Änderung gelesen und exakt abgebildet,
kein Raten nötig).

## 9. Ausgabe

```json
{
  "agent": "CE",
  "files_created": ["/home/mia/projects/harwness/docs/remediation/ledger/W2d2/CE.md"],
  "files_modified": [
    "/home/mia/projects/harwness/harw-tui/src/command_exec.rs",
    "/home/mia/projects/harwness/harw-tui/src/tools_command.rs",
    "/home/mia/projects/harwness/harw-tui/src/session_controller.rs"
  ],
  "verification": {
    "command": "cargo metadata --offline --no-deps --format-version 1",
    "exit_code": 0,
    "pass": true
  },
  "stubbed_imports": [],
  "assumptions_made": [],
  "blocked": false
}
```
