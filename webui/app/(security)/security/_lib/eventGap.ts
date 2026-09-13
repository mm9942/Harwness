// Reine Reducer-Funktion für die Lücken-Anzeige (Knoten UI-05).
//
// Getrennt von jeder Komponente, aus demselben Grund wie
// `(sessions)/_lib/eventGap.ts` und `(chat)/chat/_lib/gapLog.ts`: reine
// Funktionen sind ohne `EventSource` testbar. Eine Lücke wird **angehängt**,
// nie überschrieben oder zusammengefasst — in einer Sicherheitsansicht ist
// ein stillschweigend fehlendes Ereignis der schlimmste Fall: der Bediener
// hält das System für ruhiger, als es ist (siehe Auftrag, Abschnitt „Der
// Ereignisstrom").
import type { SecurityGapNotice } from "./types";

let nextId = 1;

/** Setzt den internen Zähler zurück — ausschließlich für deterministische Tests. */
export function resetSecurityGapNoticeIdsForTest(): void {
  nextId = 1;
}

/**
 * Hängt eine neu erkannte Lücke an die bestehende Liste an.
 *
 * # Arguments
 * - `notices` (`readonly SecurityGapNotice[]`): bisher gesammelte Lücken.
 * - `missingCount` (`number`): Anzahl fehlender Ereignisse, niemals `0`
 *   (siehe `lib/sse.ts`, `WebEventStreamHandlers.onGap`).
 * - `nowIso` (`string`, optional): Zeitpunkt für die Anzeige; Standard
 *   `new Date().toISOString()` — als Parameter injizierbar für
 *   deterministische Tests.
 *
 * # Returns
 * Eine neue Liste mit der angehängten Lücke.
 */
export function appendSecurityGapNotice(
  notices: readonly SecurityGapNotice[],
  missingCount: number,
  nowIso: string = new Date().toISOString(),
): readonly SecurityGapNotice[] {
  const notice: SecurityGapNotice = { id: nextId++, missingCount, detectedAtIso: nowIso };
  return [...notices, notice];
}
