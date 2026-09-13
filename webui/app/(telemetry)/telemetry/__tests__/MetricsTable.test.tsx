// Tests für die allgemeine Metriktabelle (Knoten UI-04).
import { describe, expect, it } from "vitest";
import { createRoot, type Root } from "react-dom/client";
import { act, type ReactElement } from "react";

import { MetricsTable } from "../_components/MetricsTable";

function renderIntoContainer(node: ReactElement): { container: HTMLDivElement; root: Root } {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  act(() => {
    root.render(node);
  });
  return { container, root };
}

describe("MetricsTable", () => {
  it("meldet, dass keine Metriken angekommen sind, statt Stille zu zeigen", () => {
    const { container, root } = renderIntoContainer(<MetricsTable entries={[]} />);
    const table = container.querySelector('[data-testid="metrics-table"]');
    expect(table?.getAttribute("data-state")).toBe("empty");
    expect(table?.textContent).toContain("Keine Metrikwerte angekommen");
    act(() => root.unmount());
    container.remove();
  });

  it("zeigt Name, Art, Einheit und Wert ohne Beschreibungstext", () => {
    const { container, root } = renderIntoContainer(
      <MetricsTable
        entries={[{ name: "jobs_completed_total", kind: "Counter", unit: "Count", value: "42" }]}
      />,
    );
    const text = container.textContent ?? "";
    expect(text).toContain("jobs_completed_total");
    expect(text).toContain("Counter");
    expect(text).toContain("Count");
    expect(text).toContain("42");
    expect(text).toContain("trägt keinen Beschreibungstext");
    act(() => root.unmount());
    container.remove();
  });

  it("meldet eine Metrik aus einem gesperrten Namensraum", () => {
    const { container, root } = renderIntoContainer(
      <MetricsTable
        entries={[{ name: "warden.ceiling_total", kind: "Counter", unit: "Count", value: "1" }]}
      />,
    );
    expect(container.textContent).toContain("gesperrten Namensraum");
    expect(container.textContent).toContain("warden.ceiling_total");
    act(() => root.unmount());
    container.remove();
  });
});
