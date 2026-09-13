// Tests für die Operationsanbindung der Bestätigungsfläche (Knoten UI-06).
//
// Prüft die Auflage „kein Aufruf geht an einen Pfad, der nicht in
// WEB_ROUTES steht": `WEB_ROUTES` ist zum Zeitpunkt dieses Knotens leer
// (`lib/generated/operations.ts`), `submitApprovalDecision` muss deshalb
// `route-missing` melden, ohne `fetch` überhaupt aufzurufen.
import { afterEach, describe, expect, it, vi } from "vitest";

import { APPROVAL_OPERATIONS, submitApprovalDecision } from "../_lib/approvalOperations";

describe("Genehmigungs-Operation ohne deklarierte Route", () => {
  const fetchSpy = vi.spyOn(globalThis, "fetch");

  afterEach(() => {
    fetchSpy.mockClear();
  });

  it("submitApprovalDecision meldet route-missing und ruft kein fetch auf", async () => {
    const result = await submitApprovalDecision("session-1", "approval-1", "approved", null);
    expect(result).toEqual({
      status: "route-missing",
      operation: APPROVAL_OPERATIONS.resolve,
    });
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("meldet route-missing unabhängig von der Entscheidung (rejected)", async () => {
    const result = await submitApprovalDecision(
      "session-1",
      "approval-1",
      "rejected",
      "Kommentar",
    );
    expect(result.status).toBe("route-missing");
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("meldet route-missing unabhängig von der Entscheidung (approved_once)", async () => {
    const result = await submitApprovalDecision("session-1", "approval-1", "approved_once", null);
    expect(result.status).toBe("route-missing");
    expect(fetchSpy).not.toHaveBeenCalled();
  });
});
