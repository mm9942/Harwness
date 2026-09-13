// Sichtbare Lücken-Anzeige im Ereignisstrom der Sicherheitszentrale (Knoten UI-05).
//
// Eigene Kopie statt Import aus `(chat)/chat/_components/GapBanner.tsx` —
// siehe `SecurityRouteMissingNotice.tsx`-Kopf für die Begründung. Zeigt
// jede erkannte Lücke einzeln und dauerhaft an — kein Zusammenfassen, kein
// automatisches Verschwinden. In einer Sicherheitsansicht ist ein
// stillschweigend fehlendes Ereignis der schlimmste Fall: der Bediener
// hält das System für ruhiger, als es ist (siehe Auftrag, Abschnitt „Der
// Ereignisstrom").
import type { JSX } from "react";

import { DataBlockList } from "@/components/ui/DataBlock";

import type { SecurityGapNotice } from "../_lib/types";

export interface SecurityGapBannerProps {
  readonly notices: readonly SecurityGapNotice[];
}

/**
 * Rendert die Liste bisher erkannter Lücken im Sicherheits-Ereignisstrom.
 *
 * # Returns
 * `null`, wenn keine Lücke erkannt wurde; sonst eine Liste mit je einer
 * Zeile pro Lücke (Zeitpunkt + Anzahl fehlender Ereignisse).
 */
export function SecurityGapBanner({ notices }: SecurityGapBannerProps): JSX.Element | null {
  if (notices.length === 0) {
    return null;
  }
  const lines = notices.map(
    (notice) =>
      `${notice.detectedAtIso}: mindestens ${notice.missingCount} Ereignis(se) im Sicherheits-Ereignisstrom fehlen möglicherweise.`,
  );
  return (
    <div className="harw-security-gap-banner" role="alert" data-testid="security-gap-banner">
      <DataBlockList label="Lücke im Ereignisstrom" values={lines} />
    </div>
  );
}
