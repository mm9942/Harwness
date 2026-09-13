// Sichtbare Lücken-Anzeige im Ereignisstrom (Knoten UI-02).
//
// Zeigt jede erkannte Lücke einzeln und dauerhaft an — kein Zusammenfassen,
// kein automatisches Verschwinden. Ein Chatverlauf mit stillschweigend
// fehlenden Nachrichten ist schlimmer als einer, der die Lücke offen zeigt
// (siehe Auftrag, Abschnitt „Der Ereignisstrom").
import type { JSX } from "react";

import { DataBlockList } from "@/components/ui/DataBlock";

import type { ChatGapNotice } from "../_lib/types";

export interface GapBannerProps {
  readonly notices: readonly ChatGapNotice[];
}

/**
 * Rendert die Liste bisher erkannter Lücken im Ereignisstrom.
 *
 * # Returns
 * `null`, wenn keine Lücke erkannt wurde; sonst eine Liste mit je einer
 * Zeile pro Lücke (Zeitpunkt + Anzahl fehlender Ereignisse).
 */
export function GapBanner({ notices }: GapBannerProps): JSX.Element | null {
  if (notices.length === 0) {
    return null;
  }
  const lines = notices.map(
    (notice) =>
      `${notice.detectedAtIso}: mindestens ${notice.missingCount} Ereignis(se) im Verlauf fehlen möglicherweise.`,
  );
  return (
    <div className="harw-chat-gap-banner" role="alert" data-testid="chat-gap-banner">
      <DataBlockList label="Lücke im Ereignisstrom" values={lines} />
    </div>
  );
}
