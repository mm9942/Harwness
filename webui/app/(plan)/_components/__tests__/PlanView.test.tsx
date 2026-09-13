// Belegt, dass ein Plan-Knotentitel mit HTML-Injektion als Text erscheint
// (Knoten UI-03, Pflichttest laut Auftrag).
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { TextOperationView } from "../PlanView";

describe("TextOperationView (Plan-Zeilen)", () => {
  it("zeigt einen Plan-Knotentitel mit <img>-Injektion als reinen Text", () => {
    const payload = '<img src=x onerror=alert(1)>';
    const html = renderToStaticMarkup(
      <TextOperationView
        title="Plan"
        operation="plan"
        detail="—"
        routeExists={true}
        state={{ kind: "loaded", lines: [payload] }}
      />,
    );

    expect(html).not.toContain("<img src=x");
    const dom = new DOMParser().parseFromString(html, "text/html");
    expect(dom.querySelectorAll("img").length).toBe(0);
    expect(dom.querySelector("pre")?.textContent).toBe(payload);
  });

  it("zeigt eine Lücken-/Fehlermeldung statt einer erfundenen Route, wenn keine Route existiert", () => {
    const html = renderToStaticMarkup(
      <TextOperationView title="Plan" operation="plan" detail="Detail" routeExists={false} state={{ kind: "idle" }} />,
    );
    expect(html).toContain("keine Route deklariert");
  });
});
