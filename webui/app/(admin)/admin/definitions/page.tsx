// Definitionsregistry-Seite (Knoten UI-07, Teil 2).
import type { JSX } from "react";

import { DefinitionRegistryView } from "../_components/DefinitionRegistryView";

/**
 * Rendert die Definitionsregistry-Ansicht.
 *
 * # Returns
 * Die Next.js-Seite für `/admin/definitions`.
 */
export default function AdminDefinitionsPage(): JSX.Element {
  return <DefinitionRegistryView />;
}
