// Ereignis-Feed der Telemetrieansicht (Knoten UI-04).
//
// `GET /events` liefert `WebEventKind`-Werte vom Typ `operation_completed`
// oder `heartbeat` (siehe `lib/sse.ts`-Moduldoku) — keinen Metrikwert und
// keine Nullzähler-Änderung. Dieser Feed ist deshalb ein
// Aktivitäts-/Lückenindikator, keine Metrikquelle: er zeigt, dass der
// Server lebt und welche Operationen abgeschlossen wurden, nicht, ob ein
// Nullzähler gerade gestiegen ist (dafür gibt es keinen Ereignistyp — siehe
// Abschlussbericht dieses Knotens).
import type { JSX } from "react";

import { DataBlockList } from "@/components/ui/DataBlock";

import type { TelemetryFeedEvent } from "../_lib/types";

export interface EventFeedProps {
  readonly events: readonly TelemetryFeedEvent[];
}

/**
 * Zeigt die zuletzt empfangenen Ereignisse als Zeilenliste.
 *
 * # Returns
 * Ein `<section>` mit einer [`DataBlockList`] oder einem Leerzustand.
 */
export function EventFeed({ events }: EventFeedProps): JSX.Element {
  if (events.length === 0) {
    return (
      <section className="harw-event-feed" data-testid="event-feed" data-state="empty">
        <DataBlockList label="Ereignisstrom" values={["Noch kein Ereignis empfangen."]} />
      </section>
    );
  }
  return (
    <section className="harw-event-feed" data-testid="event-feed" data-state="loaded">
      <DataBlockList
        label="Ereignisstrom (nur Aktivität — keine Metrikwerte, siehe Dateikopf)"
        values={events.map((event) => event.summary)}
      />
    </section>
  );
}
