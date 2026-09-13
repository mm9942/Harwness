// Sichtbare Lücken-Anzeige für den Telemetrie-Ereignisfeed (Knoten UI-04).
//
// Dieselbe Begründung wie bei UI-02/UI-03: eine Metrikansicht mit
// stillschweigend fehlenden Datenpunkten zeigt einen ruhigeren Verlauf, als
// er war — bei Zählern, die null sein sollen, der gefährlichste Fehler
// (Auftrag, Abschnitt „Der Ereignisstrom"). Jede erkannte Lücke bleibt
// dauerhaft sichtbar, verschwindet nicht von selbst.
import type { JSX } from "react";

import { DataBlock } from "@/components/ui/DataBlock";

import type { TelemetryGapNotice } from "../_lib/types";

export interface GapBannerProps {
  readonly notices: readonly TelemetryGapNotice[];
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
    <div className="harw-gap-banner" role="alert" data-testid="telemetry-gap-banner">
      {notices.map((notice) => (
        <DataBlock
          key={notice.id}
          label="Lücke im Ereignisstrom"
          value={`${notice.missingCount} Ereignis(se) fehlen seit ${notice.detectedAtIso} — die Telemetrieansicht ist bis zur nächsten vollständigen Aktualisierung unvollständig.`}
        />
      ))}
    </div>
  );
}
