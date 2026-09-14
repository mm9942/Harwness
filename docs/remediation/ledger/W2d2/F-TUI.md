# W2d-2 — Fix-Agent Z2d-2 / F-TUI

Owned files:
- `harw-tui/src/app.rs` — nur `mod tests`-Imports und Doku-Kommentare (W1-08-Block
  und `mod approval_arming_tests` nicht angefasst).
- `harw-tui/src/command_exec.rs` — nur Doku ~Zeile 134.
- `harw-tui/src/gateway.rs` — nur Doku ~Zeile 24, 68.
- `docs/remediation/ledger/W2d2/F-TUI.md` (diese Datei, neu).

BUILD-POLICY eingehalten: kein `cargo build/check/test/clippy/run/add`, kein
`make`/`rustc`/`rust-analyzer`, keine git-Schreibbefehle, keine Websuche.
Ausgeführt: `cargo metadata --offline --no-deps --format-version 1` → Exit 0
(nur Workspace-Struktur gelesen, kein Build). Verifikation ausschließlich
durch Lesen (grep + Signaturprüfung).

## T1 (blocker) — fehlender Import in `mod tests`

**Befund**: `mod tests` in `app.rs` ruft `harw_event_channel()` an drei
Stellen auf (Zeilen 3543, 3588, 3658 im neuen Stand — vor der Änderung
3542, 3587, 3657), ohne dass die Funktion importiert war.

**Verifikation**: `harw_event_channel` ist `pub(crate) fn` in
`harw-tui/src/events.rs:191`. Die äußere `use`-Liste von `app.rs`
(Zeile 133: `use crate::events::{HarwEvent, HarwEventSender};`) importiert sie
nicht, und `mod tests` bringt sie auch nicht über einen anderen Pfad herein
(`use super::*;` re-exportiert nur, was das äußere Modul importiert hat —
`harw_event_channel` fehlte dort).

**Fix**: In `mod tests` neben `use crate::command_exec::build_services;`
(Zeile 3140) ergänzt:

```rust
use crate::events::harw_event_channel;
```

Kein Namenskonflikt: Das äußere Modul importiert nur `HarwEvent` und
`HarwEventSender` aus `crate::events`, nicht die Funktion selbst.

## T2 (minor) — Intra-Doc-Links auf `pub(crate)`-Items entfernt

Befund: Doku an `pub`-Items verlinkte `pub(crate)`-Ziele per Intra-Doc-Link
(`[...]`), was für nach außen sichtbare Doku bricht (Rustdoc-Lint
`private_intra_doc_links`). In reine Backticks ohne Link umgewandelt,
Verweisziel-Visibility vorab per grep verifiziert:

| Ziel | Visibility | Fundort |
|---|---|---|
| `crate::command_exec::execute_command_as` | `pub(crate) async fn` (`command_exec.rs:203`) | app.rs ~37, ~418 (jetzt Zeile ~417), ~480 |
| `crate::runtime_commands::caller_tier` | `pub(crate) fn` (`runtime_commands.rs:52`) | app.rs ~38 |
| `crate::runtime_commands::slash_service_map` | `pub(crate) fn` (`runtime_commands.rs:71`) | app.rs ~40 |
| `ChatApp::with_runtime` / `Self::with_runtime` | `pub(crate) fn` (`app.rs:704`) | app.rs ~41, ~638 |
| `HarwEvent::Command` | Variante von `pub(crate) enum HarwEvent` (`events.rs:71`) | app.rs ~417, ~479 |

Geänderte Stellen (Zeilennummern nach der T1-Änderung, +1 gegenüber dem
Befund wegen des eingefügten `use`):
- Modul-Doku Zeilen 36–41 (`## Slash-Kommandos und /tools`): vier Links
  (`execute_command_as`, `caller_tier`, `slash_service_map`,
  `ChatApp::with_runtime`) zu Backticks.
- `Action::Command`-Doku (Zeilen 416–419): `HarwEvent::Command` und
  `execute_command_as` zu Backticks. `[`CommandRegistry`]` unverändert
  gelassen — `CommandRegistry` ist `pub struct` (`registry.rs:168`), der
  Link ist gültig.
- `ChatApp`-Struct-Doku (Zeilen 478–480): `HarwEvent::Command` und
  `execute_command_as` zu Backticks. `[`CommandAdapter`]`, `[`SandboxSpec`]`,
  `[`SessionId`]` unverändert — alle drei sind `pub` in ihren Crates.
- `with_memory`-Doku (Zeilen 636–638): `Self::with_runtime` zu Backticks.

Keine Verhaltensänderung, reine Doku-Textänderung.

## T4 (minor) — veraltete `run_chat_tui`-Erwähnungen aktualisiert

Befund: Doku-Kommentare erwähnten die (nicht mehr existierende) Funktion
`run_chat_tui`. Seit W2d-2 ist der TUI-Einstiegspunkt
`crate::runtime_root::run_tui` (`pub fn run_tui`, `runtime_root.rs:300`).

Geändert:
- `harw-tui/src/command_exec.rs:134` — `run_chat_tui` → `crate::runtime_root::run_tui`.
- `harw-tui/src/gateway.rs:24` — `run_chat_tui` → `crate::runtime_root::run_tui`.
- `harw-tui/src/gateway.rs:68` — `run_chat_tui` → `crate::runtime_root::run_tui`.

## Abschlussprüfung

```
$ grep -rn "run_chat_tui" harw-tui/src
0 Treffer
```

`cargo metadata --offline --no-deps --format-version 1` bestätigt weiterhin
Exit 0 nach allen Änderungen (Workspace-Manifeste unverändert, nur
Kommentare/Imports in bereits vorhandenen Dateien betroffen).

## Ergebnis

- T1 (blocker) behoben: fehlender Import ergänzt, keine Verhaltensänderung.
- T2 (minor) behoben: vier Intra-Doc-Link-Stellen in Backticks umgewandelt,
  gültige Links auf `pub`-Items unangetastet gelassen.
- T4 (minor) behoben: drei veraltete `run_chat_tui`-Erwähnungen auf
  `crate::runtime_root::run_tui` aktualisiert.
- W1-08-Block und `mod approval_arming_tests` in `app.rs`: unangetastet
  (per grep gegengeprüft, Zeilen 2319/2383/4408/4411 unverändert vorhanden).
- Keine Build-Policy-Verstöße, keine Scope-Erweiterung, keine
  Verhaltensänderung.
