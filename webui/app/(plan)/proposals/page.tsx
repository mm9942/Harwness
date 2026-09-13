// Vorschlagswarteschlange — Unterseite der Routen-Gruppe (Knoten UI-03).
//
// Rein lesend: siehe `_components/ProposalQueue.tsx`-Kopf für die Doktrin
// „schlägt vor, committet nie" und deren Umsetzung.
import type { JSX } from "react";

import { ProposalQueue } from "./_components/ProposalQueue";

/**
 * Rendert die Vorschlagswarteschlangen-Ansicht.
 *
 * # Returns
 * Die Next.js-Seite für `app/(plan)/proposals`.
 */
export default function ProposalsPage(): JSX.Element {
  return <ProposalQueue />;
}
