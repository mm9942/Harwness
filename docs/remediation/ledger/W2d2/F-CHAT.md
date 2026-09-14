# W2d-2 — Test-Agent Z2d-2 / F-CHAT (Befund C8)

Owned files:
- `harw-cli/src/chat.rs` — nur `mod tests` (neuer Test + neuer lokaler
  Test-Double `ScriptedModel`; Produktionscode unverändert).
- `docs/remediation/ledger/W2d2/F-CHAT.md` (diese Datei, neu).

BUILD-POLICY eingehalten: kein `cargo build/check/test/clippy/run/add`, kein
`make`/`rustc`/`rust-analyzer`, keine git-Schreibbefehle, keine Websuche.
Ausgeführt: `cargo metadata --offline --no-deps --format-version 1` → Exit 0
(nur Workspace-Struktur/Manifeste gelesen, kein Build). Verifikation
ausschließlich durch Lesen: Signaturen, Feldnamen, Re-Exporte und ein
Klammern-/Klammern-Zählcheck über die gesamte Datei (145/145 `{}`,
640/640 `()`).

## Befund C8

Es fehlte ein Test, dass im One-shot-Einstieg ein per
`[policy] require_approval_for = ["fs.write"]` gesperrter Tool-Call **ohne
Responder** abgelehnt wird — und zwar als `Deny`, nicht als `AwaitingApproval`
(`AskUser→Deny` laut `harw-runtime/src/approval.rs` ~:201-211,
`AskResolutionPolicy::would_ask` → `review()`).

## Mechanismus (verifiziert durch Lesen, nicht durch Bau)

- `harw-runtime/src/spec.rs:150-155`: `EntryKind::OneShot.profile().ask ==
  AskResolution::RejectTurn` (Tabellenzeile `OneShot | … | RejectTurn | …`).
- `harw-runtime/src/assembly.rs:924-926`: jede Montage hängt für jeden
  Einstieg außer `Interactive` eine `AskResolutionPolicy` in die
  `ApprovalChain::for_root(&config, profile.ask, approval_mode, None)`.
- `harw-runtime/src/approval.rs:166-193` (`AskResolutionPolicy::review`):
  `would_ask(call)` prüft zuerst `config.requires_approval(call)` (aus
  `[policy] require_approval_for`) **und unabhängig davon** den Modus
  (`AlwaysAsk`/`Delegated`/`FullAccess`). Trifft eine der beiden Bedingungen
  zu, liefert `review()` sofort `ApprovalDecision::Deny(reason)` — **nie**
  `AskUser` — weil dieser Handler nur für Einstiege existiert, die *keinen*
  interaktiven Responder haben.
- `harw-core/src/turn_loop.rs:490-509` (`check_approval`): aggregiert über
  **alle** Registry-Handler; `first_deny.or(first_ask)` — ein `Deny` von
  `AskResolutionPolicy` schlägt ein `AskUser` von `ConfigApprovalPolicy`, das
  ohne die Ask-Auflösung sonst pausieren würde (Gegenprobe im bestehenden
  `harw-core/tests/turn_loop.rs::configured_policy_section_is_a_runtime_approval_handler`,
  das dieselbe `ConfigApprovalPolicy` **ohne** `AskResolutionPolicy` testet und
  dort — korrekt — `AwaitingApproval` erwartet).
- `harw-core/src/turn_loop.rs:1462-1472`: `ApprovalDecision::Deny(reason)` →
  `session.history_mut().push_tool_result(call.id, ToolCallResult::error(
  format!("denied: {reason}")), 0)`, dann `continue` — der Tool-Call wird nie
  dispatcht (kein Executor-Lookup nötig) und der Turn läuft mit dem nächsten
  Modell-Aufruf weiter statt zu pausieren.

Damit reicht ein skriptbares Modell, das in Runde 1 einen `fs.write`-Call
liefert und in Runde 2 Text — ganz ohne einen echten `fs.write`-Tool-Executor
zu registrieren, weil die Ablehnung vor jedem Dispatch greift.

## Neuer Test

`harw-cli/src/chat.rs`, `mod tests`:

```rust
#[test]
fn test_one_shot_policy_gated_tool_call_is_denied_without_pausing()
```

Aufbau (Muster von `test_one_shot_assembly_applies_mode_and_config_policy`
übernommen):

1. `chat_fixture()` + `[policy] require_approval_for = ["fs.write"]` an
   `<home>/config.toml` angehängt (identischer Block wie im bestehenden Test).
2. `fixture_inputs(&fixture, EntryKind::OneShot, IngressSurface::Cli,
   startup(InteractionMode::Chat))` — `Chat` statt `Explore`, weil
   `InteractionMode::tool_profile()` (`harw-core/src/mode.rs:189-194`) nur
   `Chat`/`Work` auf `ToolProfile::Full` abbildet; `Explore`/`Plan` liegen auf
   `ToolProfile::Minimal` (deny-by-default) und wären für einen `fs.write`-Test
   die falsche Baseline. `Chat` genügt (kein `Work` nötig), weil das
   Modell den Tool-Call fest verdrahtet liefert statt ihn aus den
   `ModelRequest`-Specs abzuleiten.
3. Neuer lokaler Test-Double `ScriptedModel` (Warteschlange vorprogrammierter
   `ModelResponse`s, ein Aufruf pro `respond()`) — 1:1 das Muster aus
   `harw-core/tests/turn_loop.rs::ScriptedModel`, hier lokal im Testmodul
   definiert wie im Brief verlangt (kein neuer Crate-Abhängigkeitsbedarf:
   `ModelProvider`/`ModelResponse`/`ModelRequest`/`ModelError` kommen aus
   `harw_core`, `ToolCall`/`ToolName` aus `harw_extension_api`, beides bereits
   Workspace-Abhängigkeiten von `harw-cli`).
4. Montage über den vorhandenen privaten Helfer `one_shot_assembly(&inputs,
   ModelSource::Override(model), event_tx.clone())` — `ModelSource::Override`
   (`harw-runtime/src/model.rs:64`) reicht den skriptbaren Provider
   unverändert durch, exakt der in `harw-runtime/src/model.rs`-Tests belegte
   Signatur- und Nutzungsweg (`override_source_is_passed_through_unchanged`).
5. `assembly.new_root_session(assembly.root_session_id().clone(), event_tx,
   turn_tx, None)` — `None` als Responder, wie es die Befund-Beschreibung
   verlangt ("ohne Responder").
6. `run_turn(&mut root.session, assembly.model().as_ref(),
   assembly.state_store().as_ref(), TurnInput::user(...))` in einer
   `Builder::new_current_thread()`-Runtime (Muster aus
   `test_transcript_state_store_persists_cli_turns_in_the_active_profile_sessions_root`).

Assertions:
- `matches!(outcome, TurnOutcome::Completed)` — **nicht**
  `TurnOutcome::AwaitingApproval`; das ist die Kernaussage von Befund C8.
- `root.session.history().items()` enthält ein `TurnItem::ToolResult` mit dem
  `call_id` des `fs.write`-Calls, dessen `result` `ToolCallResult::Error {
  message }` ist und `message.contains("denied")` — belegt die Ablehnung
  konkret (kein reines "pausiert nicht"-Argument), passend zum Produktionscode
  `format!("denied: {reason}")` in `turn_loop.rs:1467`.
- Gegenprobe im selben `match`: `ToolCallResult::Success { .. }` löst einen
  expliziten `panic!` aus — ein versehentlich dispatchter, gesperrter
  Schreibzugriff würde den Test hart scheitern lassen statt still
  durchzurutschen.

## Verifikation (durch Lesen)

- Alle neu verwendeten Typen/Funktionen mit Fundstelle geprüft:
  `ModelSource::Override` (harw-runtime/src/model.rs:64),
  `one_shot_assembly` (harw-cli/src/chat.rs, unverändert, Signatur laut
  Ledger C1 §1), `RootSession{session,..}` (assembly.rs:208, Feld `session`
  bereits in bestehenden Tests als `root.session.…` gelesen),
  `TurnResultItem`/`ToolCallResult` (harw-protocol/src/items.rs:56-131),
  `AgentSession::history()` (bereits in Produktionscode `run_one_shot`
  verwendet), `ModelResponse`/`ModelProvider`/`ModelFuture`/`ModelError`
  (harw-core/src/model.rs, re-exportiert in harw-core/src/lib.rs:52-54),
  `ToolCall`/`ToolName` (harw-tools/src/call.rs:9, re-exportiert in
  harw-extension-api/src/lib.rs:18-20), `ToolCallId`
  (harw-types, re-exportiert lib.rs:34).
- Keine neue Crate-Abhängigkeit nötig: alle genutzten Typen kommen aus
  Crates, die `harw-cli/Cargo.toml` bereits führt (`harw-core`,
  `harw-extension-api`, `harw-types`, `harw-protocol`, `serde_json`,
  `tokio`) — kein `dep-request`.
- Namenskollisionsprüfung: keine der neu in `mod tests` importierten Namen
  (`ModelError`, `ModelFuture`, `ModelProvider`, `ModelRequest`,
  `ModelResponse`, `ToolCallResult`, `ToolCall`, `ToolName`, `TurnItem`,
  `ToolCallId`) ist bereits über `use super::*;` aus der äußeren
  `use`-Liste von `chat.rs` sichtbar (Abgleich gegen Zeilen 56-78 der Datei).
- `ModelResponse`-Literal nutzt `..Default::default()` statt aller Felder
  einzeln aufzuzählen, weil `ModelResponse` inzwischen fünf Felder hat
  (`message, tool_calls, usage, stop, reasoning` — harw-core/src/model.rs:491-505),
  während ältere Testdateien im Workspace (`harw-core/tests/turn_loop.rs`)
  noch Drei-Felder-Literale ohne `..Default::default()` verwenden; das wäre
  gegen die aktuelle Struct-Definition nicht mehr baubar. Der neue Test in
  `chat.rs` verwendet deshalb bewusst `..Default::default()`, um gegen
  künftige additive Felder robust zu bleiben.
- Klammern-/Parens-Bilanz der gesamten Datei nach der Änderung: 145 `{`/145
  `}`, 640 `(`/640 `)` — kein struktureller Bruch eingeführt.

## Ausgabe

```json
{"agent":"F-CHAT","files_created":["/home/mia/projects/harwness/docs/remediation/ledger/W2d2/F-CHAT.md"],
 "files_modified":["/home/mia/projects/harwness/harw-cli/src/chat.rs"],
 "test_added":"test_one_shot_policy_gated_tool_call_is_denied_without_pausing",
 "verification":{"command":"read-only (cargo metadata --offline --no-deps), parent builds/tests","exit_code":null,"pass":null},
 "dep_requests":[],
 "blocked":false}
```
