# Vertrag A2 – Lebenszyklus (Kotlin)

Basis: `feature/android-apk` @ `dc5bd8e9` (A1 committet). Paket A2 aus `android-apk-bestandsaufnahme.md`.
Ein Writer je Datei(gruppe). Nicht kompilieren, nicht committen. Nur die eigenen Dateien ändern.

## Ziel
- Netzwechsel erreichen den Rust-Kern: `ConnectivityManager` → `MobileCore.networkAvailable()` (aus A1).
- Notification zeigt Phase **und** Versuchszahl beim Wiederverbinden.
- Hinweis auf Akku-Optimierung, wenn die App beim Wiederverbinden nicht davon ausgenommen ist.
- **Invarianten:** kein Aufruf bei REVOKED/DISCONNECTED/CONNECTED; kein Aufruf während `stopping`/`forgetting`; keine mutierenden Aufrufe; Rust entscheidet endgültig (A1-Tabelle), Kotlin filtert nur vor.

## Rust-API aus A1 (vorhanden, UniFFI-Namen)
`MobileCore.networkAvailable()` (wirft `MobileException`), `ConnectionStatus.attempt: UInt`, `ConnectionStatus.nextRetryAtMs: ULong?`.

## W1 – `HarwConnection.kt`
Neue reine Top-Level-Funktion (package `dev.harw.mobile`):
```kotlin
internal enum class NetworkSignal { None, Nudge, Restart }

// None, wenn stopping || forgetting || !hasProfile.
// CONNECTING, RECONNECTING -> Nudge; FAILED -> Restart; CONNECTED, DISCONNECTED, REVOKED -> None.
internal fun networkSignal(phase: ConnectionPhase, stopping: Boolean, forgetting: Boolean, hasProfile: Boolean): NetworkSignal
```
`WorkspaceState` bekommt am Ende zwei Felder mit Default: `val attempt: UInt = 0u`, `val nextRetryAtMs: ULong? = null`. `applyStatus` übernimmt `status.attempt` und `status.nextRetryAtMs` (per `copy` nach `connectionStatus(...)`/`routeStatus(...)`). `clearSessions()`/`invalidated()`/`forgetSetup()` setzen `attempt = 0u`, `nextRetryAtMs = null`.

Netz-Callback in `HarwConnection` (Application-weit, nicht im Service, weil der Service bei FAILED endet):
- `private val connectivity = context.getSystemService(ConnectivityManager::class.java)`, `private var networkCallback: ConnectivityManager.NetworkCallback? = null`, `private var networkJob: Job? = null`.
- `private fun watchNetwork()` registriert einmalig per `registerDefaultNetworkCallback`; `onAvailable` springt per `scope.launch { networkAvailable() }` auf Main. Aufruf in `connect()` (nach dem Start). Exceptions beim Registrieren abfangen (dann ohne Callback weiter).
- `private fun unwatchNetwork()` deregistriert (Exceptions schlucken). Aufruf in `disconnect()`, `invalidate()` und `beginForgetSetup()`.
- `internal fun networkAvailable()` (Main): 
  - `val s = state.value`; `when (networkSignal(s.phase, s.stopping, s.forgetting, s.profile != null))`;
  - `None` → return; läuft `networkJob` noch → return;
  - `Nudge` → `networkJob = scope.launch { val native = core ?: return@launch; runCatching { withContext(Dispatchers.IO) { native.networkAvailable() } } }`;
  - `Restart` → `networkJob = scope.launch { ... }`: `val native = core ?: return@launch`; `withContext(IO) { native.networkAvailable(); native.status() }` (Fehler: `MobileException.Revoked` → `invalidate()`, sonst ignorieren und FAILED stehen lassen); wenn die neue Phase CONNECTING/CONNECTED/RECONNECTING ist: `val request = ++lifecycle`, `applyStatus(status)`, `startPolling(native, request)`; sonst nur `applyStatus(status)`.
- Keine anderen öffentlichen Signaturen ändern.

## W2 – `HarwConnectionService.kt`, `res/values/strings.xml`, `AndroidManifest.xml`
- Manifest: `<uses-permission android:name="android.permission.ACCESS_NETWORK_STATE" />` nach `INTERNET` einfügen (nur diese Zeile).
- strings.xml: `notification_reconnecting_attempt` = `Waiting to reconnect (attempt %1$d)` und `notification_battery` = `Allow background`.
- Neue reine Top-Level-Funktion im Service-File:
```kotlin
// attempt == 0u -> null (Basistext verwenden); sonst die Versuchszahl als Int für %1$d.
internal fun reconnectAttemptArg(attempt: UInt): Int?
```
- `observeConnection()` beobachtet zusätzlich `attempt` (z. B. `map { Quad/data class(phase, stopping, forgetting, attempt) }`). Bei RECONNECTING: Text `getString(R.string.notification_reconnecting_attempt, arg)`, wenn `reconnectAttemptArg(attempt)` nicht null, sonst `notification_reconnecting`.
- `notification(...)` nimmt den fertigen Text als `String` (statt Res-Id) plus Flags; bisherige Logik für Stop-Aktion bleibt (keine Stop-Aktion bei forgetting/disconnecting).
- Akku-Hinweis: bei RECONNECTING und `!getSystemService(PowerManager::class.java).isIgnoringBatteryOptimizations(packageName)` eine Aktion `notification_battery`, die per `PendingIntent.getActivity(..., FLAG_IMMUTABLE)` `Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS)` öffnet. Keine neue Berechtigung (kein `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`).

## W3 – `app/src/test/kotlin/dev/harw/mobile/ConnectivityTest.kt` (neu)
JUnit-Tests im Stil von `MissionFormatTest.kt`:
- `networkSignal`: None bei stopping, forgetting, ohne Profil; Nudge bei CONNECTING/RECONNECTING; Restart bei FAILED; None bei CONNECTED/DISCONNECTED/REVOKED (jeweils auch mit allen Flags).
- `reconnectAttemptArg`: `0u` → null; `1u` → 1; `8u` → 8.

## Bekannte Grenze
Nach FAILED endet der Foreground-Service. Ein Neustart durch `networkAvailable()` im Hintergrund läuft dann ohne Notification weiter (Android verbietet FGS-Start aus dem Hintergrund); die Notification kommt mit dem nächsten Öffnen der App zurück. In `INTERFACE.md`/PHONE-TEST später dokumentieren (B8).
