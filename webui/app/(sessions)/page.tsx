// Sitzungsliste — Einstiegsseite der Routen-Gruppe (Knoten UI-03).
//
// Rein lesend: siehe `_components/SessionList.tsx`-Kopf für die Begründung.
import type { JSX } from "react";

import { SessionList } from "./_components/SessionList";

/**
 * Rendert die Sitzungslisten-Ansicht.
 *
 * # Returns
 * Die Next.js-Seite für `app/(sessions)`.
 */
export default function SessionsPage(): JSX.Element {
  return <SessionList />;
}
