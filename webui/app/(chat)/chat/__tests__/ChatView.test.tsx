// Integrationstest der Chat-Ansicht gegen den tatsächlichen WEB_ROUTES-Stand
// (Knoten UI-02) — keine echte Netzwerk- oder EventSource-Verbindung.
//
// `WEB_ROUTES` ist leer; dieser Test belegt, dass die Ansicht damit nicht
// abstürzt, sondern für jede fehlende Operation eine Lücken-Meldung mit dem
// exakten Operationsnamen zeigt und keinen `fetch`-Aufruf auslöst.
import { afterEach, describe, expect, it, vi } from "vitest";
import { createRoot, type Root } from "react-dom/client";
import { act, type ReactElement } from "react";

import { ChatView } from "../_components/ChatView";
import { CHAT_OPERATIONS } from "../_lib/chatOperations";

function renderIntoContainer(node: ReactElement): { container: HTMLDivElement; root: Root } {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  act(() => {
    root.render(node);
  });
  return { container, root };
}

describe("ChatView — Master-Chat ohne deklarierte Routen", () => {
  const originalFetch = globalThis.fetch;

  afterEach(() => {
    globalThis.fetch = originalFetch;
  });

  it("zeigt die Lücken-Meldungen für History- und Sende-Operation und ruft nie fetch", async () => {
    const fetchSpy = vi.fn();
    globalThis.fetch = fetchSpy as unknown as typeof fetch;

    const { container, root } = renderIntoContainer(
      <ChatView scope={{ kind: "master" }} title="Master-Chat" />,
    );

    // Effekte (History-Ladeversuch, SSE-Verbindungsversuch) abwarten.
    await act(async () => {
      await Promise.resolve();
    });

    expect(container.textContent).toContain(CHAT_OPERATIONS.masterHistory);
    expect(container.textContent).toContain(CHAT_OPERATIONS.masterSend);
    expect(fetchSpy).not.toHaveBeenCalled();

    const textarea = container.querySelector("textarea");
    expect(textarea?.disabled).toBe(true);

    act(() => root.unmount());
    container.remove();
  });

  it("baut für Session-Chat eine eigene Überschrift mit der sessionId", async () => {
    const { container, root } = renderIntoContainer(
      <ChatView scope={{ kind: "session", sessionId: "s-42" }} title="Session-Chat: s-42" />,
    );

    await act(async () => {
      await Promise.resolve();
    });

    expect(container.querySelector("h1")?.textContent).toBe("Session-Chat: s-42");
    expect(container.textContent).toContain(CHAT_OPERATIONS.sessionHistory);

    act(() => root.unmount());
    container.remove();
  });
});
