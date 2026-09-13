// Pflichttest: "keine Daten verfügbar" ist von "nichts konfiguriert"
// unterscheidbar, und beide sind von "Route nicht deklariert" unterscheidbar
// (Knoten UI-07).
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { ListLoadState } from "../../_lib/types";
import { ListStateNotice } from "../ListStateNotice";

function render(state: ListLoadState<string>): string {
  return renderToStaticMarkup(
    <ListStateNotice state={state} operation="test.op" detail="Testdetail" emptyLabel="Nichts da." />,
  );
}

describe("ListStateNotice", () => {
  it("unterscheidet 'Route nicht deklariert' von 'keine Daten verfügbar'", () => {
    const routeMissing = render({ kind: "route-missing" });
    const errored = render({ kind: "error", message: "500" });
    expect(routeMissing).toContain("keine Route deklariert");
    expect(errored).toContain("Keine Daten verfügbar");
    expect(routeMissing).not.toEqual(errored);
  });

  it("unterscheidet 'keine Daten verfügbar' (Fehler) von 'nichts konfiguriert' (explizit leer)", () => {
    const errored = render({ kind: "error", message: "Zeitüberschreitung" });
    const explicitlyEmpty = render({ kind: "empty" });
    expect(errored).toContain("Keine Daten verfügbar");
    expect(explicitlyEmpty).toContain("Nichts konfiguriert");
    expect(errored).not.toContain("Nichts konfiguriert");
    expect(explicitlyEmpty).not.toContain("Keine Daten verfügbar");
  });

  it("zeigt bei geladenen, aber leeren Einträgen ebenfalls 'nichts konfiguriert'", () => {
    const loadedEmpty = render({ kind: "loaded", items: [] });
    expect(loadedEmpty).toContain("Nichts konfiguriert");
  });

  it("zeigt nichts, wenn Einträge geladen sind", () => {
    const loaded = render({ kind: "loaded", items: ["a"] });
    expect(loaded).toBe("");
  });
});
