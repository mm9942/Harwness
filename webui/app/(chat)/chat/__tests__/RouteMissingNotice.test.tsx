// Tests für die Lücken-Meldung fehlender Operationen (Knoten UI-02).
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
  it("nennt den erwarteten Operationsnamen im Text", () => {
    const { container, root } = renderIntoContainer(
      <RouteMissingNotice operation="chat.master.send" />,
    );
    expect(container.textContent).toContain("chat.master.send");
    act(() => root.unmount());
    container.remove();
  });

  it("baut keinen Link und kein Bild aus dem Operationsnamen", () => {
    const { container, root } = renderIntoContainer(
      <RouteMissingNotice operation="chat.master.send" />,
    );
    expect(container.querySelectorAll("a").length).toBe(0);
    expect(container.querySelectorAll("img").length).toBe(0);
    act(() => root.unmount());
    container.remove();
  });
});
