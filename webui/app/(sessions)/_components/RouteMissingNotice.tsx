// Meldung für eine (noch) nicht deklarierte Operation (Knoten UI-03).
//
// Baut sich keine eigene Route, wenn `findRoute` `undefined` liefert —
// zeigt stattdessen sichtbar, welche Operation fehlt. Der Operationsname ist
// eine vom Aufrufer fest codierte Zeichenkette (kein Nutzerdaten-Wert), wird
// aber trotzdem über [`DataBlock`] gerendert — Konsistenz mit der Regel
// „alles, was angezeigt wird, geht durch DataBlock" ist billiger als eine
// Ausnahme zu begründen.
import type { JSX } from "react";

import { DataBlock } from "@/components/ui/DataBlock";

export interface RouteMissingNoticeProps {
  /** Der erwartete, aber nicht in WEB_ROUTES gefundene Operationsname. */
  readonly operation: string;
  /** Zusätzlicher Hinweis, z. B. warum die Operation überhaupt noch fehlt. */
  readonly detail?: string;
}

/**
 * Zeigt an, dass für `operation` keine Route existiert.
 *
 * # Returns
 * Ein `<div>` mit erklärendem Text und dem Operationsnamen.
 */
export function RouteMissingNotice({ operation, detail }: RouteMissingNoticeProps): JSX.Element {
  const base = `Für die Operation "${operation}" ist in WEB_ROUTES (noch) keine Route deklariert. Diese Ansicht bleibt inaktiv, bis harw-web sie registriert.`;
  return (
    <div className="harw-route-missing" role="status">
      <DataBlock label="Route nicht verfügbar" value={detail ? `${base} ${detail}` : base} />
    </div>
  );
}
