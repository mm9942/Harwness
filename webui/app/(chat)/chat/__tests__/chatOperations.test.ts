// Tests für die Operationsanbindung des Chats (Knoten UI-02).
//
// Stellt echte Netzwerkverbindung nie her: `WEB_ROUTES` ist zum Zeitpunkt
// dieses Knotens leer (siehe `lib/generated/operations.ts`), daher liefern
// `resolve*`-Funktionen ohne jeden Mock bereits `undefined` — das ist der
// Ist-Zustand, den dieser Test dokumentiert. Ein zweiter Testfall simuliert
// per `vi.mock` eine künftig existierende Route, um zu belegen, dass ein
// Aufruf ausschließlich über `callOperation`/`WEB_ROUTES` geht und niemals
// einen rohen Pfad-String verwendet.
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  CHAT_OPERATIONS,
  buildNegativeFeedbackBody,
  buildSendBody,
  historyOperationName,
  resolveHistoryRoute,
  resolveNegativeFeedbackRoute,
  resolveSendRoute,
  sendOperationName,
} from "../_lib/chatOperations";

describe("Operationsnamen je Bereich", () => {
  it("wählt die Master-Namen für scope=master", () => {
    expect(historyOperationName({ kind: "master" })).toBe(CHAT_OPERATIONS.masterHistory);
    expect(sendOperationName({ kind: "master" })).toBe(CHAT_OPERATIONS.masterSend);
  });

  it("wählt die Session-Namen für scope=session", () => {
    const scope = { kind: "session" as const, sessionId: "s-1" };
    expect(historyOperationName(scope)).toBe(CHAT_OPERATIONS.sessionHistory);
    expect(sendOperationName(scope)).toBe(CHAT_OPERATIONS.sessionSend);
  });
});

describe("Rumpf-Aufbau (reine Funktionen)", () => {
  it("baut den Sende-Rumpf für Master ohne sessionId", () => {
    expect(buildSendBody({ kind: "master" }, "hallo")).toEqual({ text: "hallo" });
  });

  it("baut den Sende-Rumpf für Session mit sessionId", () => {
    const scope = { kind: "session" as const, sessionId: "s-1" };
    expect(buildSendBody(scope, "hallo")).toEqual({ sessionId: "s-1", text: "hallo" });
  });

  it("baut den Feedback-Rumpf für Master ohne sessionId", () => {
    expect(buildNegativeFeedbackBody({ kind: "master" }, "t-1", "text")).toEqual({
      turnId: "t-1",
      text: "text",
      reason: "negative",
    });
  });

  it("baut den Feedback-Rumpf für Session mit sessionId", () => {
    const scope = { kind: "session" as const, sessionId: "s-9" };
    expect(buildNegativeFeedbackBody(scope, "t-1", "text")).toEqual({
      sessionId: "s-9",
      turnId: "t-1",
      text: "text",
      reason: "negative",
    });
  });
});

describe("Route-Auflösung gegen den tatsächlichen WEB_ROUTES-Stand", () => {
  it("liefert undefined für alle Chat-Operationen, solange WEB_ROUTES leer ist", () => {
    expect(resolveHistoryRoute({ kind: "master" })).toBeUndefined();
    expect(resolveSendRoute({ kind: "master" })).toBeUndefined();
    expect(resolveNegativeFeedbackRoute()).toBeUndefined();
  });
});

describe("Aufruf geht ausschließlich über eine deklarierte Route", () => {
  const originalFetch = globalThis.fetch;

  afterEach(() => {
    globalThis.fetch = originalFetch;
    vi.resetModules();
    vi.doUnmock("@/lib/generated/operations");
  });

  it("ruft fetch niemals mit einem rohen Pfad-String auf, wenn keine Route existiert", async () => {
    const fetchSpy = vi.fn();
    globalThis.fetch = fetchSpy as unknown as typeof fetch;

    const ops = await import("../_lib/chatOperations");
    const result = await ops.sendChatMessage({ kind: "master" }, "hallo");

    expect(result).toBeUndefined();
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("ruft fetch mit exakt dem Pfad der deklarierten Route auf, sobald sie existiert", async () => {
    vi.doMock("@/lib/generated/operations", () => ({
      WEB_ROUTES: [
        {
          operation: CHAT_OPERATIONS.masterSend,
          path: "/api/chat/master/send",
          method: "POST",
          permission: "Operator",
          approval: "None",
        },
      ],
    }));
    vi.resetModules();

    const fetchSpy = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({ text: "Antwort" }),
    });
    globalThis.fetch = fetchSpy as unknown as typeof fetch;

    const ops = await import("../_lib/chatOperations");
    const result = await ops.sendChatMessage({ kind: "master" }, "hallo");

    expect(result).toEqual({ ok: true, text: "Antwort" });
    expect(fetchSpy).toHaveBeenCalledTimes(1);
    const [calledPath] = fetchSpy.mock.calls[0] as [string, RequestInit];
    expect(calledPath).toBe("/api/chat/master/send");
  });
});
