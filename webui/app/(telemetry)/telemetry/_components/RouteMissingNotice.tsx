// Meldung für eine (noch) nicht deklarierte Operation (Knoten UI-04).
//
// Baut sich keine eigene Route, wenn `findRoute` `undefined` liefert —
// zeigt stattdessen sichtbar, welche Operation fehlt. Derselbe Baustein wie
// bei UI-02/UI-03 (`(chat)/chat/_components/RouteMissingNotice.tsx`,
// `(sessions)/_components/RouteMissingNotice.tsx`) — eigenständig
// nachgebaut, weil jeder Knoten nur in seiner eigenen Routen-Gruppe
// schreiben darf.
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
    <div className="harw-route-missing" role="status" data-testid="telemetry-route-missing">
      <DataBlock label="Route nicht verfügbar" value={detail ? `${base} ${detail}` : base} />
    </div>
  );
}
