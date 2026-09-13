// Agentenbaum — Unterseite der Routen-Gruppe (Knoten UI-03).
//
// Rein lesend: siehe `_components/AgentTree.tsx`-Kopf für die Begründung.
import type { JSX } from "react";

import { AgentTree } from "./_components/AgentTree";

/**
 * Rendert die Agentenbaum-Ansicht.
 *
 * # Returns
 * Die Next.js-Seite für `app/(sessions)/agents`.
 */
export default function AgentsPage(): JSX.Element {
  return <AgentTree />;
}
