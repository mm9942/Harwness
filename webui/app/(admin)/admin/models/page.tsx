// Modellkatalog-Seite (Knoten UI-07, Teil 1).
import type { JSX } from "react";

import { ModelCatalogView } from "../_components/ModelCatalogView";

/**
 * Rendert die Modellkatalog-Ansicht.
 *
 * # Returns
 * Die Next.js-Seite für `/admin/models`.
 */
export default function AdminModelsPage(): JSX.Element {
  return <ModelCatalogView />;
}
