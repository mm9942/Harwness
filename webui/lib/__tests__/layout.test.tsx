// Belegt, dass die Schale ohne eine einzige Routen-Gruppe rendert.
//
// UI-02..07 (app/(chat)/, (sessions)/, (plan)/, (telemetry)/, (security)/,
// (admin)/) existieren zum Zeitpunkt dieses Knotens noch nicht. Dieser Test
// rendert `RootLayout` unmittelbar (ohne Next.js-Routing-Runtime) und belegt,
// dass es sich kompilieren und rendern lässt, ohne irgendeine Routen-Gruppe
// zu importieren — ein Blick in `app/layout.tsx` zeigt zusätzlich, dass der
// einzige Import aus diesem Knoten selbst kommt (`./globals.css`).
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import RootLayout from "../../app/layout";

describe("RootLayout", () => {
  it("rendert ein vollständiges HTML-Skelett ohne Routen-Gruppen-Import", () => {
    const html = renderToStaticMarkup(
      RootLayout({ children: <p>Platzhalter-Inhalt einer künftigen Routen-Gruppe</p> }),
    );
    expect(html).toContain("<html");
    expect(html).toContain("<body>");
    expect(html).toContain("Platzhalter-Inhalt");
  });
});
