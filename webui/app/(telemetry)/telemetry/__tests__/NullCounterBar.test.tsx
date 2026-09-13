// Tests für die Nullzähler-Leiste (Knoten UI-04).
//
// Deckt die drei Kernauflagen ab: „alle null" vs. „keine Zähler
// registriert" ist unterscheidbar; ein verletzter Zähler erscheint als
// Ereignis mit Invariante, nicht nur als Zahl; ein Labelwert mit Markup
// erscheint als Text.
import { describe, expect, it } from "vitest";
import { createRoot, type Root } from "react-dom/client";
import { act, type ReactElement } from "react";

import { NullCounterBar } from "../_components/NullCounterBar";
import { classifyNullCounterSnapshot } from "../_lib/nullCounters";

function renderIntoContainer(node: ReactElement): { container: HTMLDivElement; root: Root } {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  act(() => {
    root.render(node);
  });
  return { container, root };
}

describe("NullCounterBar", () => {
  it("zeigt 'keine Zähler registriert' unterscheidbar von 'alle null'", () => {
    const emptyState = classifyNullCounterSnapshot([]);
    const allZeroState = classifyNullCounterSnapshot([
      { name: "a_total", count: 0, invariant: "a hält" },
    ]);

    const empty = renderIntoContainer(<NullCounterBar state={emptyState} />);
    const allZero = renderIntoContainer(<NullCounterBar state={allZeroState} />);

    const emptyBar = empty.container.querySelector('[data-testid="null-counter-bar"]');
    const allZeroBar = allZero.container.querySelector('[data-testid="null-counter-bar"]');

    expect(emptyBar?.getAttribute("data-state")).toBe("empty");
    expect(allZeroBar?.getAttribute("data-state")).toBe("all-zero");
    expect(emptyBar?.getAttribute("role")).toBe("status");
    expect(emptyBar?.textContent).toContain("Keine Nullzähler registriert");
    expect(allZeroBar?.textContent).toContain("1 Nullzähler geprüft, alle bei 0");
    expect(emptyBar?.textContent).not.toEqual(allZeroBar?.textContent);

    act(() => empty.root.unmount());
    act(() => allZero.root.unmount());
    empty.container.remove();
    allZero.container.remove();
  });

  it("zeigt einen verletzten Zähler als Ereignis mit Invariante, nicht nur als Zahl", () => {
    const state = classifyNullCounterSnapshot([
      { name: "trust_block_violation_total", count: 3, invariant: "kein Fragment mit TrustClass::Data im Instruktionsblock" },
    ]);
    const { container, root } = renderIntoContainer(<NullCounterBar state={state} />);

    const bar = container.querySelector('[data-testid="null-counter-bar"]');
    expect(bar?.getAttribute("role")).toBe("alert");
    expect(bar?.getAttribute("data-state")).toBe("violated");
    expect(bar?.textContent).toContain("trust_block_violation_total");
    expect(bar?.textContent).toContain("3");
    expect(bar?.textContent).toContain("kein Fragment mit TrustClass::Data im Instruktionsblock");

    act(() => root.unmount());
    container.remove();
  });

  it("zeigt einen Labelwert mit eingebettetem Markup als reinen Text", () => {
    const state = classifyNullCounterSnapshot([
      {
        name: "<img src=x onerror=alert(1)>_total",
        count: 1,
        invariant: "<img src=x onerror=alert(1)>",
      },
    ]);
    const { container, root } = renderIntoContainer(<NullCounterBar state={state} />);

    expect(container.querySelector("img")).toBeNull();
    expect(container.textContent).toContain("<img src=x onerror=alert(1)>");

    act(() => root.unmount());
    container.remove();
  });

  it("meldet Zähler aus gesperrten Namensräumen als Befund", () => {
    const state = classifyNullCounterSnapshot([
      { name: "security.block_total", count: 0, invariant: "sicherheitsrelevant" },
    ]);
    const { container, root } = renderIntoContainer(<NullCounterBar state={state} />);

    const notice = container.querySelector('[data-testid="restricted-namespace-notice"]');
    expect(notice).not.toBeNull();
    expect(notice?.textContent).toContain("security.block_total");

    act(() => root.unmount());
    container.remove();
  });
});
