// Einstiegsseite der Verwaltungsfläche (Knoten UI-07).
//
// Eigener Pfad `/admin` (statt Wurzel `/`), damit diese Routen-Gruppe nicht
// mit dem Wurzelpfad einer anderen Gruppe (z. B. `(sessions)`) kollidiert —
// dieselbe Konvention wie `(chat)/chat` und `(security)/security`.
import Link from "next/link";
import type { JSX } from "react";

/**
 * Rendert die Übersicht der Verwaltungsfläche mit Links zu ihren zwei Teilen.
 *
 * # Returns
 * Die Next.js-Seite für `/admin`.
 */
export default function AdminPage(): JSX.Element {
  return (
    <main aria-label="Verwaltung">
      <h1>Verwaltung</h1>
      <p>
        Modellkatalog (vier Schichten, Layer-4-Rückkanal) und Agentendefinitionen
        (Kontext-Decke, SnapshotId) — rein lesend. Keine Fläche hier ändert eine
        Definition oder einen Katalogeintrag.
      </p>
      <ul>
        <li>
          <Link href="/admin/models">Modellkatalog</Link>
        </li>
        <li>
          <Link href="/admin/definitions">Agentendefinitionen</Link>
        </li>
      </ul>
    </main>
  );
}
