// Reine Reducer-Funktion für die Lücken-Anzeige (Knoten UI-03).
//
// Getrennt von jeder Komponente, aus demselben Grund wie
// `(chat)/chat/_lib/gapLog.ts`: reine Funktionen sind ohne `EventSource`
// testbar. Eine Lücke wird **angehängt**, nie überschrieben — ein
// Agentenbaum, dem stillschweigend Knoten fehlen, ist von einem
// vollständigen nicht zu unterscheiden; dieselbe Begründung gilt für die
// Sitzungsliste.
import type { GapNotice } from "./types";

let nextId = 1;

/** Setzt den internen Zähler zurück — ausschließlich für deterministische Tests. */
export function resetGapNoticeIdsForTest(): void {
  nextId = 1;
}

/**
 * Hängt eine neu erkannte Lücke an die bestehende Liste an.
 *
 * # Arguments
 * - `notices` (`readonly GapNotice[]`): bisher gesammelte Lücken.
 * - `missingCount` (`number`): Anzahl fehlender Ereignisse, niemals `0`
 *   (siehe `lib/sse.ts`, `WebEventStreamHandlers.onGap`).
 * - `nowIso` (`string`, optional): Zeitpunkt für die Anzeige; Standard
 *   `new Date().toISOString()` — als Parameter injizierbar für
 *   deterministische Tests.
 *
 * # Returns
 * Eine neue Liste mit der angehängten Lücke.
 */
export function appendGapNotice(
  notices: readonly GapNotice[],
  missingCount: number,
  nowIso: string = new Date().toISOString(),
): readonly GapNotice[] {
  const notice: GapNotice = { id: nextId++, missingCount, detectedAtIso: nowIso };
  return [...notices, notice];
}
