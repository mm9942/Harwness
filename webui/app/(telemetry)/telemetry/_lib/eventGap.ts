// Reine Reducer-Funktionen für Ereignis-Feed und Lücken-Anzeige (Knoten UI-04).
//
// Getrennt von jeder Komponente und von `lib/sse.ts` selbst, aus demselben
// Grund wie bei UI-02/UI-03: reine Funktionen sind ohne `EventSource`
// testbar (siehe `lib/sse.ts`-Moduldoku — dieser Knoten öffnet in keinem
// Test eine echte Verbindung). Eine Lücke wird **angehängt**, nie
// überschrieben — eine Metrikansicht mit stillschweigend fehlenden
// Datenpunkten zeigt einen ruhigeren Verlauf, als er war (Auftrag,
// Abschnitt „Der Ereignisstrom").
import type { WebEvent } from "@/lib/sse";

import type { TelemetryFeedEvent, TelemetryGapNotice } from "./types";

let nextGapId = 1;
let nextFeedId = 1;

/** Setzt die internen Zähler zurück — ausschließlich für deterministische Tests. */
export function resetTelemetryIdsForTest(): void {
  nextGapId = 1;
  nextFeedId = 1;
}

/**
 * Hängt eine neu erkannte Lücke an die bestehende Liste an.
 *
 * # Arguments
 * - `notices` (`readonly TelemetryGapNotice[]`): bisher gesammelte Lücken.
 * - `missingCount` (`number`): Anzahl fehlender Ereignisse, niemals `0`
 *   (siehe `lib/sse.ts`, `WebEventStreamHandlers.onGap`).
 * - `nowIso` (`string`, optional): Zeitpunkt für die Anzeige; Standard
 *   `new Date().toISOString()` — als Parameter injizierbar für
 *   deterministische Tests.
 *
 * # Returns
 * Eine neue Liste mit der angehängten Lücke.
 */
export function appendTelemetryGapNotice(
  notices: readonly TelemetryGapNotice[],
  missingCount: number,
  nowIso: string = new Date().toISOString(),
): readonly TelemetryGapNotice[] {
  const notice: TelemetryGapNotice = { id: nextGapId++, missingCount, detectedAtIso: nowIso };
  return [...notices, notice];
}

/**
 * Formatiert ein rohes [`WebEvent`] zu einer menschenlesbaren Zusammenfassung.
 *
 * # Description
 * `WebEventKind` kennt nur `operation_completed` und `heartbeat` (siehe
 * `lib/sse.ts`) — keine Metrikwert-Änderung. Die Telemetrieansicht kann den
 * Ereignisstrom deshalb ausschließlich als Aktivitäts-/Lückenindikator
 * zeigen, nicht als Quelle für Metrikwerte selbst (siehe Abschlussbericht
 * dieses Knotens).
 *
 * # Arguments
 * - `event` (`WebEvent`): das eingetroffene Ereignis.
 *
 * # Returns
 * Einen einzeiligen, menschenlesbaren Text.
 */
export function summarizeTelemetryEvent(event: WebEvent): string {
  if (event.kind.type === "operation_completed") {
    const outcome = event.kind.ok ? "erfolgreich" : "fehlgeschlagen";
    return `Operation "${event.kind.operation}" ${outcome} (Sequenz ${event.sequence})`;
  }
  return `Heartbeat (Sequenz ${event.sequence})`;
}

/**
 * Hängt ein neu eingetroffenes Ereignis an den Feed an.
 *
 * # Arguments
 * - `feed` (`readonly TelemetryFeedEvent[]`): bisher gesammelte Ereignisse.
 * - `event` (`WebEvent`): das neu eingetroffene, bereits lückengeprüfte
 *   Ereignis.
 * - `nowIso` (`string`, optional): Empfangszeitpunkt; als Parameter
 *   injizierbar für deterministische Tests.
 *
 * # Returns
 * Eine neue Liste mit dem angehängten Ereignis.
 */
export function appendTelemetryFeedEvent(
  feed: readonly TelemetryFeedEvent[],
  event: WebEvent,
  nowIso: string = new Date().toISOString(),
): readonly TelemetryFeedEvent[] {
  const entry: TelemetryFeedEvent = {
    id: nextFeedId++,
    sequence: event.sequence,
    summary: summarizeTelemetryEvent(event),
    receivedAtIso: nowIso,
  };
  return [...feed, entry];
}
