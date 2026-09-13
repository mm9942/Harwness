// Pflichttest: kein Aufruf geht an einen Pfad, der nicht in WEB_ROUTES steht
// (Knoten UI-07). Da `WEB_ROUTES` leer ist, müssen alle `resolve*`-Funktionen
// `undefined` liefern und alle `fetch*`-Funktionen dürfen `fetch` NICHT
// aufrufen — das ist der strukturelle Beweis, nicht nur eine Beobachtung.
import { afterEach, describe, expect, it, vi } from "vitest";

import { WEB_ROUTES } from "@/lib/webClient";

import {
  fetchDefinitionRegistry,
  fetchModelBehaviorProposals,
  fetchModelCatalog,
  resolveDefinitionRegistryRoute,
  resolveModelBehaviorProposalRoute,
  resolveModelCatalogRoute,
} from "../webRoutes";

describe("webRoutes (Verwaltungsfläche)", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("WEB_ROUTES ist zum Zeitpunkt dieses Knotens leer", () => {
    expect(WEB_ROUTES.length).toBe(0);
  });

  it("resolve* liefert überall undefined, solange keine Route deklariert ist", () => {
    expect(resolveModelCatalogRoute()).toBeUndefined();
    expect(resolveModelBehaviorProposalRoute()).toBeUndefined();
    expect(resolveDefinitionRegistryRoute()).toBeUndefined();
  });

  it("fetch* ruft niemals global fetch auf, wenn keine Route existiert", async () => {
    const fetchSpy = vi.fn();
    vi.stubGlobal("fetch", fetchSpy);

    await Promise.all([fetchModelCatalog(), fetchModelBehaviorProposals(), fetchDefinitionRegistry()]);

    expect(fetchSpy).not.toHaveBeenCalled();
  });
});
