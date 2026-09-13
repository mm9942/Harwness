// Reine Reducer-Funktionen für die Lücken-Anzeige im Chat (Knoten UI-02).
//
// Getrennt von `ChatView.tsx`, aus demselben Grund, aus dem `sse.ts`
// `trackSequence` von `connectWebEventStream` trennt: reine Funktionen sind
// ohne `EventSource` testbar. Eine Lücke wird hier **angehängt**, nie
// überschrieben — ein Verlauf mit stillschweigend verschwundenen
// Lücken-Meldungen wäre derselbe Fehler, den die Lückenerkennung selbst
// verhindern soll.
import { nextLocalId } from "./ids";
import type { ChatGapNotice } from "./types";

/**
 * Hängt eine neu erkannte Lücke an die bestehende Liste an.
 *
 * # Arguments
 * - `notices` (`readonly ChatGapNotice[]`): bisher gesammelte Lücken.
 * - `missingCount` (`number`): Anzahl fehlender Ereignisse, niemals `0`
 *   (siehe `sse.ts`, `WebEventStreamHandlers.onGap`).
 * - `nowIso` (`string`, optional): Zeitpunkt für die Anzeige; Standard
 *   `new Date().toISOString()` — als Parameter injizierbar für
 *   deterministische Tests.
 *
 * # Returns
 * Eine neue Liste mit der angehängten Lücke.
 */
export function appendGapNotice(
  notices: readonly ChatGapNotice[],
  missingCount: number,
  nowIso: string = new Date().toISOString(),
): readonly ChatGapNotice[] {
  const notice: ChatGapNotice = {
    id: nextLocalId(),
    missingCount,
    detectedAtIso: nowIso,
  };
  return [...notices, notice];
}
