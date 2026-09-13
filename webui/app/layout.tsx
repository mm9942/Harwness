// Wurzel-Layout der Control-Plane-Schale (Knoten UI-01).
//
// Absichtlich frei von jedem Import aus einer Routen-Gruppe
// (`app/(chat)/`, `(sessions)/`, `(plan)/`, `(telemetry)/`, `(security)/`,
// `(admin)/`) — diese gehören den Knoten UI-02..07 und müssen parallel zu
// diesem Knoten baubar bleiben. Dieses Layout rendert `children` und bindet
// nur, was in diesem Knoten selbst entsteht: `globals.css` und
// `components/ui/**`.
import type { Metadata } from "next";
import type { ReactNode } from "react";

import "./globals.css";

export const metadata: Metadata = {
  title: "Harwness Control Plane",
  description:
    "Control-Plane-Oberfläche für die Harwness-Registry — Routen kommen ausschließlich aus harw-web.",
};

/**
 * Das eine Wurzel-Layout aller App-Router-Seiten.
 *
 * # Description
 * Next.js verlangt genau ein `app/layout.tsx` mit `<html>`/`<body>`. Es
 * trägt keine eigene Navigation zu einzelnen Flächen — jede Routen-Gruppe
 * (UI-02..07) bringt ihre eigene Navigation/Kopfzeile mit, sobald sie
 * gebaut ist. Bis dahin rendert dieses Layout allein, ohne dass eine
 * einzige Routen-Gruppe existieren muss (siehe Modultests unter
 * `lib/__tests__` für den Beleg).
 *
 * # Arguments
 * - `children` (`ReactNode`): der von der jeweils aktiven Route gelieferte
 *   Inhalt.
 *
 * # Returns
 * Das vollständige HTML-Dokument-Skelett.
 */
export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="de">
      <body>{children}</body>
    </html>
  );
}
