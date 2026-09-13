// Sichtbare Lücken-Anzeige für Sitzungsliste und Agentenbaum (Knoten UI-03).
//
// Ein Bediener, der einen fehlenden Kindprozess oder eine fehlende Sitzung
// nicht sieht, hält das System für ruhiger, als es ist (siehe Auftrag,
// Abschnitt „Der Ereignisstrom"). Diese Komponente rendert jede über
// [`GapNotice`] gesammelte Lücke sichtbar und dauerhaft — sie verschwindet
// nicht von selbst, weil ein Bediener sonst eine bereits gesehene Lücke für
// eine neue halten könnte oder umgekehrt.
import type { JSX } from "react";

import { DataBlock } from "@/components/ui/DataBlock";

import type { GapNotice } from "../_lib/types";

export interface GapBannerProps {
  readonly notices: readonly GapNotice[];
}

/**
 * Zeigt jede erkannte Lücke im Ereignisstrom als eigenen Block.
 *
 * # Returns
 * `null`, wenn keine Lücke vorliegt; sonst ein `<div>` mit einem
 * [`DataBlock`] pro Lücke.
 */
export function GapBanner({ notices }: GapBannerProps): JSX.Element | null {
  if (notices.length === 0) {
    return null;
  }
  return (
    <div className="harw-gap-banner" role="alert" data-testid="gap-banner">
      {notices.map((notice) => (
        <DataBlock
          key={notice.id}
          label="Lücke im Ereignisstrom"
          value={`${notice.missingCount} Ereignis(se) fehlen seit ${notice.detectedAtIso} — die Ansicht ist bis zur nächsten vollständigen Aktualisierung unvollständig.`}
        />
      ))}
    </div>
  );
}
