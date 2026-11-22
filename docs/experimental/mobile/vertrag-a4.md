# Vertrag A4 – Session/Mission-UX (Kotlin)

Basis: `feature/android-apk` @ `bfc8d881` (wip/onion). Paket A4 aus `android-apk-bestandsaufnahme.md`.
Ein Writer je Datei. Nicht kompilieren, nicht committen (Commit macht die UIA nach Abschluss aller vier Writer).
Nur die eigene Datei ändern. Sichtbare Signaturen, die andere Dateien aufrufen, bleiben unverändert, außer hier ausdrücklich genannt.

## Ziel A4
Veralteten Stand und unsichere (Ambiguous/IN_FLIGHT) Sendungen deutlich anzeigen, Freigaben mit Restzeit, Mission-Dock ausklappbar, Leer- und Fehlerzustände.

## Gemeinsame Schnittstellen (verbindlich)

### W1 – `MissionFormat.kt` (+ `MissionFormatTest.kt`) – neue reine Funktionen, package `dev.harw.mobile`
```kotlin
internal enum class SessionFreshness { Live, Stale, Offline }

// Offline, wenn !online. Live, wenn workspaceGeneration != null && windowGeneration == workspaceGeneration. Sonst Stale.
internal fun sessionFreshness(online: Boolean, workspaceGeneration: Long?, windowGeneration: Long?): SessionFreshness

// RFC-3339 (z. B. "2026-11-22T02:15:00Z", auch mit Offset) über java.time.OffsetDateTime.parse(...).toInstant().toEpochMilli().
// Leer oder nicht parsebar -> null. Ergebnis = Ablaufzeit - nowMs, darf <= 0 sein.
internal fun approvalRemainingMs(timeoutAt: String, nowMs: Long): Long?

// <= 0 -> "expired". Sonst gleiche Einheiten wie formatElapsed: "45s", "4m05s", "1h02m".
internal fun formatRemaining(remainingMs: Long): String

// Zusammenfassung für das eingeklappte Mission-Dock: "2 agents · 1 job", "1 agent", "3 jobs"; beide 0 -> "idle".
internal fun missionSummary(activeAgents: Int, activeJobs: Int): String
```
Bestehende Funktionen bleiben unverändert. Tests in `MissionFormatTest.kt` ergänzen (JUnit wie vorhanden): je Funktion Normalfall und Randfälle (null/leer/ungültig, Grenze 0, Einzahl/Mehrzahl, Offset-Zeitstempel).

### W2 – `MissionDock.kt`
- `rememberTicker` wird `internal` (statt `private`), Signatur unverändert: `internal fun rememberTicker(running: Boolean): State<Long>`. W3 nutzt ihn.
- `MissionHeader(...)`-Signatur unverändert (Aufrufer: `SessionConversation.kt`). Ein-/Ausklappen über internen Zustand (`rememberSaveable`), eingeklappt eine Zeile mit `missionSummary(activeAgents(agents), activeJobs(jobs))`, Antippen klappt aus/ein; Standard: ausgeklappt, wenn aktiv, sonst eingeklappt.
- Leerzustände in Agents-/Jobs-Karte („No agents yet“, „No jobs yet“).

### W3 – `SessionConversation.kt`
- Signaturen von `SessionConversation(...)` und `SessionActivityPanel(...)` unverändert (Aufrufer: `MainActivity.kt`, `WorkspaceScreen.kt`).
- Banner oben in der Konversation über `sessionFreshness(state.online, state.generation, view?.generation)`: Stale → „Showing an older state – waiting for the server to resync“; Offline → Verbindungsphase/Detail; Live → kein Banner. Senden/Entscheiden bleibt wie bisher an `generation`/`active` gebunden.
- Freigaben: Restzeit statt rohem `Expires ${timeoutAt}` über `rememberTicker(true)`, `approvalRemainingMs`, `formatRemaining`; abgelaufen → Entscheidungsknöpfe deaktiviert und „Expired“ sichtbar; ungültiger/leerer Zeitstempel → keine Restzeit-Zeile.
- Unsichere Sendung (`DraftResult.Rejected(ambiguous = true)` bzw. vorhandenes `uncertain` in der Darstellung, soweit in dieser Datei sichtbar): deutlich als „Delivery unknown – check the transcript before resending“ hervorheben.
- Leer-/Fehlerzustände des Feeds: `view?.error` sichtbar, leere Session mit Hinweistext.

### W4 – `WorkspaceScreen.kt`
- Öffentliche Funktionen (`WorkspaceScreen`, `DirectoryEmpty`, `displayName`-Erweiterungen) mit unveränderter Signatur.
- Leer-/Fehlerzustände der Session-Liste: lädt (`!sessionsLoaded`), Fehler (`listError`), keine Sessions, offline/reconnecting (Phase + `detail`) – jeweils mit klarem Text.
- Keine neuen Aufrufe in `HarwConnection` (keine neuen Methoden).

## Nicht ändern
`SessionActionGate.kt` (kein Bedarf in A4), `HarwConnection.kt`, `MainActivity.kt`, `ConnectionScreen.kt`, alles unter `mobile/rust`, `mobile/scripts`, `mobile-bootstrap.rs`.
UI-Texte bleiben Englisch wie im Bestand (keine `strings.xml`-Änderung in A4).
