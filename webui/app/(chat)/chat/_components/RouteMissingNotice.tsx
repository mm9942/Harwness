// Meldung für eine (noch) nicht deklarierte Operation (Knoten UI-02).
//
// Siehe `_lib/chatOperations.ts`-Kopf: Baut sich keine eigene Route, wenn
// `findRoute` `undefined` liefert — zeigt stattdessen sichtbar, welche
// Operation fehlt. Der Operationsname ist eine vom Aufrufer fest codierte
// Zeichenkette (kein Nutzerdaten-Wert), wird aber trotzdem über
// [`DataBlock`] gerendert — Konsistenz mit der Regel „alles, was angezeigt
// wird, geht durch DataBlock" ist billiger als eine Ausnahme zu begründen.
import type { JSX } from "react";

import { DataBlock } from "@/components/ui/DataBlock";

export interface RouteMissingNoticeProps {
  /** Der erwartete, aber nicht in WEB_ROUTES gefundene Operationsname. */
  readonly operation: string;
}

/**
 * Zeigt an, dass für `operation` keine Route existiert.
 *
 * # Returns
 * Ein `<div>` mit erklärendem Text und dem Operationsnamen.
 */
export function RouteMissingNotice({ operation }: RouteMissingNoticeProps): JSX.Element {
  return (
    <div className="harw-chat-route-missing" role="status">
      <DataBlock
        label="Route nicht verfügbar"
        value={`Für die Operation "${operation}" ist in WEB_ROUTES (noch) keine Route deklariert. Diese Funktion bleibt inaktiv, bis harw-web sie registriert.`}
      />
    </div>
  );
}
