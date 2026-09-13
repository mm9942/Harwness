// Telemetrieansicht — Einstiegsseite der Routen-Gruppe (Knoten UI-04).
//
// Rein lesend: siehe `_components/TelemetryView.tsx`-Kopf für die Begründung.
import type { JSX } from "react";

import { TelemetryView } from "./_components/TelemetryView";

/**
 * Rendert die Telemetrieansicht.
 *
 * # Returns
 * Die Next.js-Seite für `app/(telemetry)/telemetry`.
 */
export default function TelemetryPage(): JSX.Element {
  return <TelemetryView />;
}
