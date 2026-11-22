# Vertrag A1 – Connector-Robustheit (Rust)

Basis: `feature/android-apk` @ `9ca132f3` (Onion-Stand `bfc8d881` + A4). Paket A1 aus `android-apk-bestandsaufnahme.md`.
Ein Writer je Datei. Nicht kompilieren, nicht committen. Nur die eigene Datei ändern. Keine Änderung an `core/route.rs`, `mobile-bootstrap.rs`, `mobile/scripts`, Tor-/Onion-Logik, Kotlin.

## Ziel
- Wenn das Netz zurückkommt, verbindet der Kern sofort neu, statt die Backoff-Pause abzuwarten oder nach 8 Fehlversuchen tot zu bleiben.
- `ConnectionStatus` zeigt Versuch und Zeitpunkt des nächsten Versuchs.
- **Invarianten:** REVOKED wird nie aufgehoben. Endgültige Fehler (Pin/Handshake/403/Route-Plan, `DialFailure::Final`, `LinkDisposition::Final`) lösen keinen Auto-Reconnect aus. Kein mutierender Aufruf (submit/decide/interrupt/create/detach) wird erneut gesendet. Ein vom Nutzer gewähltes `Disconnected` bleibt getrennt.

## Gemeinsame Schnittstellen (verbindlich)

### `types.rs` (W1) – `ConnectionStatus` bekommt zwei Felder, **am Ende angehängt**
```rust
    /// Reconnect attempts since the last successful connection; 0 while connected or idle.
    pub attempt: u32,
    /// Unix time in ms of the next scheduled dial while waiting in backoff, else None.
    pub next_retry_at_ms: Option<u64>,
```

### `core.rs` (W2)
Neue öffentliche UniFFI-Methode im bestehenden `#[uniffi::export] impl MobileCore` (neben `reconnect`/`disconnect`):
```rust
/// Signals that the platform reports a usable network again.
/// Never resends mutating calls; never lifts a revocation.
pub fn network_available(&self) -> Result<(), MobileError>
```
Reine, testbare Entscheidungsfunktion (privat, im Modul):
```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NetworkAction { Ignore, WakeBackoff, Restart }

fn network_action(phase: ConnectionPhase, run_active: bool, retryable_failure: bool) -> NetworkAction
```
Regeln für `network_action`:
| phase | run_active | retryable_failure | Ergebnis |
|---|---|---|---|
| Revoked | * | * | Ignore |
| Disconnected | * | * | Ignore |
| Connected | * | * | Ignore |
| Connecting / Reconnecting | true | * | WakeBackoff |
| Connecting / Reconnecting | false | * | Ignore |
| Failed | * | true | Restart |
| Failed | * | false | Ignore |

Verhalten von `network_available`: zuerst `apply_pending_revocation()`; dann `network_action(...)`:
- `Ignore` → `Ok(())`.
- `WakeBackoff` → laufende Backoff-Pause sofort beenden und Versuchszähler auf 0 setzen (nächster Dial sofort).
- `Restart` → wie `reconnect()` (`start_connection(None)`), Ergebnis zurückgeben.

Interne Zustände (privat, Namen frei, Vorschlag):
- im Verbindungszustand: `attempt: u32`, `next_retry_at_ms: Option<u64>`, `retryable_failure: bool`;
- in `Shared`: `wake: tokio::sync::Notify` (oder gleichwertig), das `drive_connection` in der Backoff-Pause zusammen mit `run` abwartet (`tokio::select!`); beim Wecken `retry = 0`, `delay = 0`.
- `retryable_failure = true` nur, wenn `Failed` wegen `DialFailure::Retry` gesetzt wurde (Fehlversuche erschöpft bzw. vorübergehender Erstverbindungsfehler); `false` bei Route-Plan-Fehler, `DialFailure::Final`, `LinkDisposition::Final`, Revocation; zurück auf `false` bei jedem neuen `start_connection`.
- `attempt`/`next_retry_at_ms` pflegen: vor jeder Backoff-Pause setzen (`next_retry_at_ms = jetzt + delay`), bei `Connected` auf `0`/`None`, bei `Failed`/`Disconnected`/`Revoked` `next_retry_at_ms = None`.
- `status()` füllt die beiden neuen Felder.
- Unit-Tests im bestehenden `mod tests` von `core.rs` für `network_action` (alle Tabellenzeilen) und dafür, dass `MAX_RECONNECT_ATTEMPTS` unverändert die obere Grenze bleibt.

### `tests.rs` (W3) – Integrationstests im vorhandenen Stil
- `network_available_never_lifts_revocation`: nach Revocation (Muster wie `policy_invalidation_clears_session_content_and_allows_explicit_reconnect`, ca. Zeile 549-598) bleibt `status().phase == Revoked` nach `network_available()`.
- `network_available_is_noop_when_disconnected`: nach `disconnect()` bleibt `Disconnected`, keine neue Verbindung.
- `network_available_is_noop_when_connected`: verbunden → bleibt verbunden, `attempt == 0`, `next_retry_at_ms == None`.
- `status_reports_attempt_and_next_retry_while_reconnecting`: nach Verbindungsverlust (vorhandenes Muster zum Server-Stopp/Neustart nutzen, falls vorhanden) gilt `phase == Reconnecting`, `attempt >= 1`, `next_retry_at_ms.is_some()`; sonst Test weglassen und melden.
- Kein Test darf einen mutierenden Aufruf doppelt erwarten.

### `mobile/INTERFACE.md` (W1)
Abschnitt zu `network_available()` (Semantik laut Tabelle, Invarianten) und zu den Feldern `attempt`, `next_retry_at_ms`; Hinweis, dass die App sie aus einem `ConnectivityManager.NetworkCallback` aufruft (Paket A2) und von einem Plattform-Thread (nicht vom Tokio-Runtime-Thread).
