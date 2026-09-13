// Belegt, dass kein Aufruf an einen nicht deklarierten Pfad geht (Knoten UI-03).
//
// `WEB_ROUTES` ist zum Zeitpunkt dieses Knotens leer — jeder `fetch*`-Helfer
// muss deshalb `undefined` liefern, ohne je `fetch` aufzurufen. Ein Fehler
// hier wäre ein zweiter, ungeprüfter Autoritätspfad (siehe Auftrag).
import { afterEach, describe, expect, it, vi } from "vitest";

import { fetchSessionList } from "../_lib/webRoutes";
import { fetchAgentTree } from "../agents/_lib/webRoutes";

describe("kein Aufruf an einen nicht deklarierten Pfad", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("fetchSessionList ruft global fetch nicht auf, solange WEB_ROUTES leer ist", async () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch");
    const result = await fetchSessionList();
    expect(result).toBeUndefined();
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("fetchAgentTree ruft global fetch nicht auf, solange WEB_ROUTES leer ist", async () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch");
    const result = await fetchAgentTree();
    expect(result).toBeUndefined();
    expect(fetchSpy).not.toHaveBeenCalled();
  });
});
