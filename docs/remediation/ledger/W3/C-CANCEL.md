# C-CANCEL — hierarchischer Cancel-Token (`harw-core/src/cancel.rs`)

Welle: W3 (Verträge). Rolle: focused-coding-task (opus, laut Plan `eventual-wandering-pebble.md` Teil B).
Build-Policy eingehalten: keine `cargo build/check/test/clippy/run/add`, keine Git-Schreibbefehle. Verifikation
ausschließlich durch Lesen (Signaturen, Imports, Registry-Quelle).

## Geänderte / neue Dateien

- **neu** `harw-core/src/cancel.rs` — vollständiges Modul, s.u.
- **neu** `docs/remediation/ledger/W3/C-CANCEL.md` (diese Datei)

Keine weiteren Dateien angefasst. `pub mod cancel;` in `harw-core/src/lib.rs` **nicht** gesetzt — das ist laut
Auftrag X0's Zuständigkeit (Root-`Cargo.toml`/`harw-core/{src/lib.rs,Cargo.toml}`, Plan Teil B §W3-Tabelle,
Zeile `X0 | sonnet | Root-Cargo.toml, harw-core/{src/lib.rs,Cargo.toml} | … pub mod cancel/envelope; cargo add
tokio-util@0.7 -p harw-core`).

## Eingefrorene öffentliche Signaturen (exakt wie geschrieben)

```rust
// harw-core/src/cancel.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelReason { User, Parent, Budget, LeaseLost, Shutdown }

#[derive(Clone, Debug)]
pub struct CancelToken { /* private: inner: tokio_util::sync::CancellationToken,
                             reason: Arc<OnceLock<CancelReason>> */ }

impl CancelToken {
    pub fn new() -> Self;
    pub fn child(&self) -> Self;
    pub fn cancel(&self, reason: CancelReason);
    pub fn reason(&self) -> Option<CancelReason>;
    pub fn is_cancelled(&self) -> bool;
    pub async fn cancelled(&self);
}

impl Default for CancelToken {
    fn default() -> Self; // == Self::new()
}
```

Deckt sich Zeichen für Zeichen mit der gefrorenen Signatur aus `eventual-wandering-pebble.md` Zeile 195
(„Gefrorene W3-Signaturen (Auszug)“) und dem Auftragstext.

## Semantik (wie umgesetzt)

- **Eltern→Kind**: `child()` erzeugt einen `tokio_util::sync::CancellationToken::child_token()`; Kind wird
  Bestandteil des Cancel-Baums, `parent.cancel()` bricht alle Kinder mit ab.
- **Kind→Eltern nicht**: `child_token()` ist laut Registry-Quelle strikt einseitig (siehe API-Nachweis unten);
  `child.cancel()` ruft nur `self.inner.cancel()` auf dem Kind-Knoten auf, der Elternknoten bleibt unberührt.
- **Erster Grund gewinnt, idempotent**: `reason: Arc<OnceLock<CancelReason>>` je Token; `cancel()` ruft
  `self.reason.set(reason)` (Fehlerfall bei bereits gesetztem Wert wird bewusst mit Kommentar verworfen — genau
  der Vertrag „erster Grund gewinnt“) und danach immer `self.inner.cancel()` (idempotent laut Doku:
  „Be aware that cancellation is not an atomic operation… once the call to `cancel` returns, all child nodes
  have been fully cancelled“, mehrfacher Aufruf ist unschädlich).
- **Kind erbt `Parent`, sofern nicht selbst vorher abgebrochen**: `reason()` liest zuerst die eigene
  `OnceLock` (own reason gewinnt immer); ist die eigene Zelle leer, aber `inner.is_cancelled()` true (weil ein
  Vorfahre abgebrochen wurde), wird `Some(CancelReason::Parent)` zurückgegeben, ohne die Zelle zu beschreiben.
  Test `test_child_keeps_own_reason_when_parent_cancels_afterward` belegt: eigener Grund wird durch spätere
  Eltern-Abbrüche nicht überschrieben.
- **`cancelled()` wird fertig**: delegiert 1:1 an `tokio_util::sync::CancellationToken::cancelled().await`
  (resolves sofort, falls bereits abgebrochen; sonst wenn `cancel()` auf sich selbst oder einem Vorfahren
  aufgerufen wird).

## API-Nachweise (Datei:Zeile, verifiziert durch Lesen vor Nutzung)

Registry-Pfad: `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-util-0.7.18/`

- `CancellationToken::new() -> CancellationToken` — `src/sync/cancellation_token.rs:139`
- `CancellationToken::child_token(&self) -> CancellationToken` — `src/sync/cancellation_token.rs:184-188`;
  Doku bestätigt Einseitigkeit: „Unlike a cloned `CancellationToken`, cancelling a child token does not cancel
  the parent token.“ (Zeile 147-148).
- `CancellationToken::cancel(&self)` — `src/sync/cancellation_token.rs:200-202`; Doku zu Mehrfachaufruf/Atomarität
  Zeile 195-199.
- `CancellationToken::is_cancelled(&self) -> bool` — `src/sync/cancellation_token.rs:205-207`.
- `CancellationToken::cancelled(&self) -> WaitForCancellationFuture<'_>` (per `.await` konsumiert) —
  `src/sync/cancellation_token.rs:223-228`.
- `impl Clone for CancellationToken` (teilt denselben Knoten, siehe Doku Zeile 115-116: „will get cancelled
  whenever the current token gets cancelled, and vice versa“) — `src/sync/cancellation_token.rs:114-123`.
- `impl core::fmt::Debug for CancellationToken` — `src/sync/cancellation_token.rs:106-112` (zeigt `is_cancelled`);
  damit ist `#[derive(Debug)]` auf `CancelToken` (Felder `CancellationToken` + `Arc<OnceLock<CancelReason>>`,
  beide `Debug`) deckungsgleich zulässig.
- `impl Default for CancellationToken` — `src/sync/cancellation_token.rs:131-135`.
- `pub mod sync;` **ohne** Feature-Gate in `src/lib.rs:57` (kein `cfg_*!`-Makro davor, im Gegensatz zu
  `codec`/`net`/`compat`/`io`/`rt`/`time`, die alle über `cfg_xxx!`-Makros gattert sind, siehe
  `src/lib.rs:24-53`); `sync::cancellation_token` re-exportiert `CancellationToken` unconditional
  (`src/sync/mod.rs:3-8`). **Ergebnis: keine zusätzliche Cargo-Feature-Flag nötig**, Default-Features reichen.
  Damit korrigiere ich die Annahme im Auftragstext („Feature für CancellationToken“) — es gibt keins zu setzen.

## Dep-Request (nicht selbst ausgeführt — Build-Policy)

```
dep-request: cargo add -p harw-core tokio-util@0.7.18
```

- Version `0.7.18` exakt aus `Cargo.lock:6259` übernommen (bereits im Lockfile vorhanden — vermutlich von
  einer anderen Crate transitiv gezogen), erfüllt „bereits im Lockfile vorhandene Version bevorzugen“.
- **Keine Feature-Flags nötig** (siehe API-Nachweis oben) — Standard-`cargo add -p harw-core tokio-util@0.7.18`
  ohne `--features` genügt für `tokio_util::sync::CancellationToken`.
- `harw-core/Cargo.toml` hat aktuell **keinen** `tokio-util`-Eintrag (geprüft: `[dependencies]`-Block enthält nur
  `tokio = { version = "1", features = ["rt", "sync", "macros", "time"] }`, kein `tokio-util`).
- `tokio` selbst ist bereits normale Abhängigkeit mit `macros` + `rt` (`harw-core/Cargo.toml`, `[dependencies]`),
  daher genügt das für `#[tokio::test]` in diesem Modul — **kein separater dep-request für `tokio` als
  Dev-Dependency nötig** (normale Dependencies gelten auch für Test-Builds).
- Owner der `Cargo.toml`-Änderung: X0 (siehe oben), Reihenfolge laut Plan: X0 zieht `tokio-util` und setzt
  `pub mod cancel;` — beides Voraussetzung dafür, dass dieses Modul im Crate-Baum kompiliert. Bis dahin ist
  `harw-core/src/cancel.rs` eine unverdrahtete, aber vollständige Datei (kein `todo!()`/`unimplemented!()`,
  jede Funktion ist real implementiert).

## Tests (in `harw-core/src/cancel.rs`, `#[cfg(test)] mod tests`)

| Test | Szenario |
|---|---|
| `test_new_is_not_cancelled` | frischer Token: `is_cancelled() == false`, `reason() == None` |
| `test_default_matches_new` | `Default::default()` verhält sich wie `new()` |
| `test_parent_cancel_propagates_to_child_with_reason_parent` | **Eltern→Kind**: `parent.cancel(Shutdown)` → `child.is_cancelled()==true`, `child.reason()==Some(Parent)` |
| `test_child_cancel_does_not_propagate_to_parent` | **Kind→Eltern nicht**: `child.cancel(Budget)` lässt `parent.is_cancelled()==false` |
| `test_child_keeps_own_reason_when_parent_cancels_afterward` | Kind bricht zuerst mit eigenem Grund ab, Eltern-Abbruch danach überschreibt `child.reason()` nicht |
| `test_cancel_first_reason_wins_and_is_idempotent` | drei `cancel()`-Aufrufe mit unterschiedlichen Gründen → `reason()` bleibt beim ersten |
| `test_cancelled_future_completes_after_cancel` (`#[tokio::test]`) | `cancelled()` in gespawnter Task hängt, bis `cancel()` aufgerufen wird; `JoinHandle` muss beenden |
| `test_cancelled_future_resolves_immediately_if_already_cancelled` (`#[tokio::test]`) | bereits abgebrochener Token: `cancelled().await` hängt nicht |
| `test_cancelled_future_completes_via_parent_cancellation` (`#[tokio::test]`) | Kind wartet in gespawnter Task auf `cancelled()`, Abbruch kommt vom Elternteil |

Alle Tests sind reine In-Process-Assertions (kein Netz, kein Dateisystem, keine externen Prozesse); erfüllen
„konkrete Assertion, nie nur `it doesn't panic`“.

## Geschlossene Register-IDs

- **F-160** (`x-findings-register-w1-w3.md:273`, `harw-core/turn_loop.rs:1306`): „kein Cancel-Token“ — die
  Primitive existiert jetzt vollständig; **Verdrahtung** in `turn_loop.rs` bleibt Folgearbeit (siehe unten),
  daher hier nur als *Vertrag geliefert*, nicht als *Finding vollständig geschlossen* markiert.
- **G-017** (`x-findings-register-w4.md:88`, `harw-tui/app.rs:3443-3474,3827-3843,3142-3159`): „kein
  Cancel-Token in `TurnInput`“ — dito, Primitive geliefert, TUI-Verdrahtung ist W4b (T-TUI) laut Plan.

Beide IDs bleiben bis zur tatsächlichen Verdrahtung (A-LOOP in W4a, T-TUI in W4b laut
`eventual-wandering-pebble.md` Zeilen 227 und 241) offiziell **offen**; dieser Agent liefert nur den
gefrorenen Vertrag, wie im Plan für W3 vorgesehen.

## Offene Annahmen

1. Kein zusätzliches Cargo-Feature für `tokio-util` nötig (s. API-Nachweis) — abweichend vom Wortlaut des
   Auftrags („Feature für CancellationToken“), aber durch Lesen der Registry-Quelle widerlegt.
2. `CancelReason::Parent` ist bewusst *generisch* („irgendein Vorfahre hat abgebrochen“), nicht der exakte
   Grund des Vorfahren — der Auftragstext verlangt exakt das („reason des Kindes = Parent“), es wird kein
   Enum-Payload mit dem tatsächlichen Vorfahren-Grund transportiert. Falls ein Aufrufer den *tatsächlichen*
   Vorfahren-Grund braucht, ist das eine Erweiterung für eine spätere Welle (nicht in der gefrorenen Signatur
   enthalten).
3. `Clone` auf `CancelToken` folgt `tokio_util`-Semantik (geteilter Knoten, kein neues Kind) — Konsequenz aus
   `#[derive(Clone, Debug)]` in der gefrorenen Signatur; wer einen unabhängigen, aber abhängigen Token will,
   muss `child()` verwenden, nicht `clone()`.

## Folgearbeit (für benannte Folgewellen, außerhalb dieser Zuständigkeit)

- **X0** (W3): `pub mod cancel;` in `harw-core/src/lib.rs` setzen, `dep-request` oben ausführen
  (`cargo add -p harw-core tokio-util@0.7.18`, keine Extra-Features).
- **A-LOOP** (W4a, `harw-core/src/turn_loop.rs`): Cancel-Prüfung zwischen Runden/vor Tools, `TurnOutcome::Cancelled`,
  offene Tool-Calls bekommen synthetisches Error-Result (F-160, G-017 laut Plan Zeile 227).
- **A-CHILD** (W4a, `harw-core/src/child_controller.rs`): Kind-Token = `parent.child()` beim Spawn (Plan Zeile 228).
- **T-TUI** (W4b, `harw-tui/src/{app,approval,command_exec,session_controller}.rs`): `Ctrl+C` = Turn-Cancel statt
  `deferred_input`-Pufferung (G-017, Plan Zeile 241).
- **A-JOBRUN** (W4a, `harw-core/src/durable_job_runner.rs`): Lease-Verlust → `CancelReason::LeaseLost`.
