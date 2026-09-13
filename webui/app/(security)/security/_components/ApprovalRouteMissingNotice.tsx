// Meldung für eine (noch) nicht deklarierte Genehmigungs-Operation
// (Knoten UI-06).
//
// Eigene Kopie statt Import aus `SecurityRouteMissingNotice.tsx` (Knoten
// UI-05) — dieser Knoten schreibt nur neue Dateien in `(security)/**`
// (siehe Auftrag, Abschnitt „Dein exklusiver Schreibbereich") und überschreibt
// keine vorhandene Datei. Gleiches Verhalten wie das Vorbild: „keine Daten
// verfügbar" ist etwas anderes als „abgelehnt" oder „nichts zu tun" — hier
// zusätzlich mit der Auflage, dass eine fehlende Route bei einer
// irreversiblen Aktion **niemals** in eine stillschweigend erlaubte
// Aktion umschlägt (siehe `ApprovalRequestPanel.tsx`).
import type { JSX } from "react";

import { DataBlock } from "@/components/ui/DataBlock";

export interface ApprovalRouteMissingNoticeProps {
  /** Der erwartete, aber nicht in WEB_ROUTES gefundene Operationsname. */
  readonly operation: string;
}

/**
 * Zeigt an, dass für `operation` keine Route existiert — es gibt deshalb
 * keinen Weg, die Entscheidung des Bedieners an `harw-web` zu übermitteln.
 *
 * # Returns
 * Ein `<div role="status">` mit erklärendem Text und dem Operationsnamen.
 */
export function ApprovalRouteMissingNotice({
  operation,
}: ApprovalRouteMissingNoticeProps): JSX.Element {
  return (
    <div
      className="harw-approval-route-missing"
      role="status"
      data-testid="approval-route-missing"
    >
      <DataBlock
        label="Keine Daten verfügbar"
        value={`Für die Operation "${operation}" ist in WEB_ROUTES (noch) keine Route deklariert. Eine Entscheidung kann deshalb (noch) nicht übermittelt werden — die Anfrage bleibt unaufgelöst, es gibt keinen Ersatzweg.`}
      />
    </div>
  );
}
