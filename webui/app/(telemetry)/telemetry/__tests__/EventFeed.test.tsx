// Test für den Ereignis-Feed der Telemetrieansicht (Knoten UI-04).
import { describe, expect, it } from "vitest";
import { createRoot, type Root } from "react-dom/client";
import { act, type ReactElement } from "react";

import { EventFeed } from "../_components/EventFeed";

function renderIntoContainer(node: ReactElement): { container: HTMLDivElement; root: Root } {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  act(() => {
    root.render(node);
  });
  return { container, root };
}

describe("EventFeed", () => {
  it("zeigt einen Leerzustand, solange kein Ereignis empfangen wurde", () => {
    const { container, root } = renderIntoContainer(<EventFeed events={[]} />);
    const feed = container.querySelector('[data-testid="event-feed"]');
    expect(feed?.getAttribute("data-state")).toBe("empty");
    expect(feed?.textContent).toContain("Noch kein Ereignis empfangen");
    act(() => root.unmount());
    container.remove();
  });

  it("zeigt empfangene Ereignisse als Zeilen", () => {
    const { container, root } = renderIntoContainer(
      <EventFeed
        events={[
          { id: 1, sequence: 1, summary: "Heartbeat (Sequenz 1)", receivedAtIso: "t1" },
        ]}
      />,
    );
    expect(container.textContent).toContain("Heartbeat (Sequenz 1)");
    act(() => root.unmount());
    container.remove();
  });
});
