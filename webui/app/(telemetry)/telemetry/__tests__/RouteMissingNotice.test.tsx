// Test für die Lücken-Meldung bei fehlender Operation (Knoten UI-04).
import { describe, expect, it } from "vitest";
import { createRoot, type Root } from "react-dom/client";
import { act, type ReactElement } from "react";

import { RouteMissingNotice } from "../_components/RouteMissingNotice";

function renderIntoContainer(node: ReactElement): { container: HTMLDivElement; root: Root } {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  act(() => {
    root.render(node);
  });
  return { container, root };
}

describe("RouteMissingNotice", () => {
  it("nennt den fehlenden Operationsnamen", () => {
    const { container, root } = renderIntoContainer(
      <RouteMissingNotice operation="telemetry.null_counters.snapshot" />,
    );
    const notice = container.querySelector('[data-testid="telemetry-route-missing"]');
    expect(notice?.textContent).toContain("telemetry.null_counters.snapshot");
    act(() => root.unmount());
    container.remove();
  });
});
