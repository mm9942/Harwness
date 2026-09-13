// Tests für die Lücken-Anzeige der Telemetrieansicht (Knoten UI-04).
import { describe, expect, it } from "vitest";
import { createRoot, type Root } from "react-dom/client";
import { act, type ReactElement } from "react";

import { GapBanner } from "../_components/GapBanner";
import type { TelemetryGapNotice } from "../_lib/types";

function renderIntoContainer(node: ReactElement): { container: HTMLDivElement; root: Root } {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  act(() => {
    root.render(node);
  });
  return { container, root };
}

describe("GapBanner", () => {
  it("zeigt nichts, solange keine Lücke erkannt wurde", () => {
    const { container, root } = renderIntoContainer(<GapBanner notices={[]} />);
    expect(container.querySelector('[data-testid="telemetry-gap-banner"]')).toBeNull();
    act(() => root.unmount());
    container.remove();
  });

  it("zeigt eine erkannte Lücke im Ereignisstrom an", () => {
    const notices: TelemetryGapNotice[] = [
      { id: 1, missingCount: 4, detectedAtIso: "2026-09-01T00:00:00.000Z" },
    ];
    const { container, root } = renderIntoContainer(<GapBanner notices={notices} />);
    const banner = container.querySelector('[data-testid="telemetry-gap-banner"]');
    expect(banner).not.toBeNull();
    expect(banner?.textContent).toContain("4");
    act(() => root.unmount());
    container.remove();
  });
});
