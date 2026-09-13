// Meldung für eine (noch) nicht deklarierte Sicherheits-Operation (Knoten UI-05).
//
// Eigene Kopie statt Import aus `(chat)/chat/_components/RouteMissingNotice.tsx`:
// dieser Knoten schreibt ausschließlich in `(security)/**` (siehe Auftrag,
// Abschnitt „Dein exklusiver Schreibbereich") und hält deshalb auch seine
// Lese-Abhängigkeiten innerhalb der eigenen Routen-Gruppe, statt sich an
// die private `_components`-Struktur eines parallelen Knotens zu binden.
// Gleiches Verhalten wie das Vorbild: baut sich keine eigene Route, wenn
// `findRoute` `undefined` liefert, sondern zeigt sichtbar, welche Operation
// fehlt — „keine Daten verfügbar", nie eine still leere Fläche (siehe
// Auftrag, Abschnitt „Was zu bauen ist").
import type { JSX } from "react";

import { DataBlock } from "@/components/ui/DataBlock";

export interface SecurityRouteMissingNoticeProps {
  /** Der erwartete, aber nicht in WEB_ROUTES gefundene Operationsname. */
  readonly operation: string;
}

/**
 * Zeigt an, dass für `operation` keine Route existiert — „keine Daten
 * verfügbar", ausdrücklich unterscheidbar von „keine Befunde"
 * (leere, aber erfolgreich abgerufene Liste; siehe
 * `SecurityListPanel.tsx`).
 *
 * # Returns
 * Ein `<div role="status">` mit erklärendem Text und dem Operationsnamen.
 */
export function SecurityRouteMissingNotice({
  operation,
}: SecurityRouteMissingNoticeProps): JSX.Element {
  return (
    <div
      className="harw-security-route-missing"
      role="status"
      data-testid="security-no-data"
    >
      <DataBlock
        label="Keine Daten verfügbar"
        value={`Für die Operation "${operation}" ist in WEB_ROUTES (noch) keine Route deklariert. Diese Fläche zeigt deshalb keine Daten — nicht, weil es keine Befunde gäbe, sondern weil harw-web diesen Datenweg (noch) nicht öffnet.`}
      />
    </div>
  );
}
