// Tests für die Operationsanbindung der Sicherheitszentrale (Knoten UI-05).
//
// Prüft die Auflage „kein Aufruf geht an einen Pfad, der nicht in
// WEB_ROUTES steht": `WEB_ROUTES` ist zum Zeitpunkt dieses Knotens leer
// (`lib/generated/operations.ts`), jede `fetch*`-Funktion muss deshalb
// `route-missing` melden, ohne `fetch` überhaupt aufzurufen — kein
// erratener Pfad, keine stillschweigend leere Liste.
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  fetchFindings,
  fetchFreezeRecords,
  fetchHostSamples,
  fetchSecurityEvents,
  fetchSecurityVerdicts,
  SECURITY_OPERATIONS,
} from "../_lib/securityOperations";

describe("Sicherheits-Operationen ohne deklarierte Route", () => {
  const fetchSpy = vi.spyOn(globalThis, "fetch");

  afterEach(() => {
    fetchSpy.mockClear();
  });

  it("fetchFindings meldet route-missing und ruft kein fetch auf", async () => {
    const result = await fetchFindings();
    expect(result).toEqual({ status: "route-missing", operation: SECURITY_OPERATIONS.findingsList });
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("fetchHostSamples meldet route-missing und ruft kein fetch auf", async () => {
    const result = await fetchHostSamples();
    expect(result).toEqual({
      status: "route-missing",
      operation: SECURITY_OPERATIONS.hostSamplesList,
    });
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("fetchSecurityEvents meldet route-missing und ruft kein fetch auf", async () => {
    const result = await fetchSecurityEvents();
    expect(result).toEqual({ status: "route-missing", operation: SECURITY_OPERATIONS.eventsList });
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("fetchSecurityVerdicts meldet route-missing und ruft kein fetch auf", async () => {
    const result = await fetchSecurityVerdicts();
    expect(result).toEqual({
      status: "route-missing",
      operation: SECURITY_OPERATIONS.verdictsList,
    });
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("fetchFreezeRecords meldet route-missing und ruft kein fetch auf", async () => {
    const result = await fetchFreezeRecords();
    expect(result).toEqual({ status: "route-missing", operation: SECURITY_OPERATIONS.freezeList });
    expect(fetchSpy).not.toHaveBeenCalled();
  });
});
