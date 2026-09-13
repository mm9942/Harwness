// Meldung für eine (noch) nicht deklarierte Operation (Knoten UI-07).
//
// Eigenständige Kopie derselben Komponente aus `(plan)/_components` /
// `(sessions)/agents/_components` — Konvention seit UI-02: jede Routen-
// Gruppe bleibt ohne Import aus einer fremden Gruppe baubar.
import type { JSX } from "react";

import { DataBlock } from "@/components/ui/DataBlock";

export interface RouteMissingNoticeProps {
  /** Der erwartete, aber nicht in WEB_ROUTES gefundene Operationsname. */
  readonly operation: string;
  /** Zusätzlicher Hinweis, z. B. warum die Operation strukturell fehlt. */
  readonly detail?: string;
}

/**
 * Zeigt an, dass für `operation` keine Route existiert.
 *
 * # Returns
 * Ein `<div>` mit erklärendem Text und dem Operationsnamen — niemals ein
 * erfundener Pfad.
 */
export function RouteMissingNotice({ operation, detail }: RouteMissingNoticeProps): JSX.Element {
  const base = `Für die Operation "${operation}" ist in WEB_ROUTES (noch) keine Route deklariert. Diese Ansicht bleibt inaktiv, bis harw-web sie registriert.`;
  return (
    <div className="harw-route-missing" role="status">
      <DataBlock label="Route nicht verfügbar" value={detail ? `${base} ${detail}` : base} />
    </div>
  );
}
