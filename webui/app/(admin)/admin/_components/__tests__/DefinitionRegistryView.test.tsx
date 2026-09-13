// Pflichttests: kein Knopf, der eine Definition ändert; Route-Lücke wird
// mit Operationsnamen gemeldet (Knoten UI-07, Teil 2).
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { DefinitionSummaryView } from "../../_lib/types";
import { DefinitionRegistryView } from "../DefinitionRegistryView";

const definition: DefinitionSummaryView = {
  definitionId: "harwness.agent.explorer@1",
  role: "explorer",
  family: "family/research",
  contextProgramId: "explore",
  snapshotId: "a".repeat(64),
};

describe("DefinitionRegistryView", () => {
  it("meldet die Route-Lücke mit dem Namen der erwarteten Operation, wenn keine Route existiert", () => {
    const html = renderToStaticMarkup(<DefinitionRegistryView overrideState={{ kind: "route-missing" }} />);
    expect(html).toContain("agent.definition.list");
    expect(html).toContain("keine Route deklariert");
  });

  it("enthält keinen Knopf, der eine Definition ändert", () => {
    const html = renderToStaticMarkup(<DefinitionRegistryView overrideState={{ kind: "loaded", items: [definition] }} />);
    const dom = new DOMParser().parseFromString(html, "text/html");
    expect(dom.querySelectorAll("button").length).toBe(0);
    expect(dom.querySelectorAll("form").length).toBe(0);
    expect(dom.querySelectorAll("input").length).toBe(0);
  });

  it("zeigt die SnapshotId einer geladenen Definition", () => {
    const html = renderToStaticMarkup(<DefinitionRegistryView overrideState={{ kind: "loaded", items: [definition] }} />);
    expect(html).toContain(definition.snapshotId as string);
  });

  it("unterscheidet 'nichts konfiguriert' (explizit leer) von der Route-Lücke", () => {
    const empty = renderToStaticMarkup(<DefinitionRegistryView overrideState={{ kind: "empty" }} />);
    const missing = renderToStaticMarkup(<DefinitionRegistryView overrideState={{ kind: "route-missing" }} />);
    expect(empty).toContain("Nichts konfiguriert");
    expect(missing).not.toContain("Nichts konfiguriert");
  });
});
