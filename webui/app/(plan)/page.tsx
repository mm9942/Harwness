// Plan und Ziel — Einstiegsseite der Routen-Gruppe (Knoten UI-03).
//
// Rein lesend: siehe `_components/PlanView.tsx`-Kopf für die Begründung.
import type { JSX } from "react";

import { GoalView, PlanView } from "./_components/PlanView";

/**
 * Rendert Plan- und Ziel-Ansicht nebeneinander.
 *
 * # Returns
 * Die Next.js-Seite für `app/(plan)`.
 */
export default function PlanPage(): JSX.Element {
  return (
    <>
      <GoalView />
      <PlanView />
    </>
  );
}
