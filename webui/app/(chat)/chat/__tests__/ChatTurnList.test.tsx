// Sicherheitstests für die Chat-Zug-Anzeige (Knoten UI-02).
//
// Belegt die zentrale Auflage des Auftrags direkt am Chat-Baustein (nicht
// nur an `DataBlock` selbst, das UI-01 bereits testet): eine Modellantwort
// mit eingebettetem `<img onerror>` oder einem Markdown-Link erscheint als
// Text, nie als Markup. Kein echtes DOM-Netzwerk, keine echte Verbindung —
// reines Rendering mit `@testing-library/react` wäre ideal, ist aber keine
// vorhandene Abhängigkeit dieses Knotens; stattdessen wird direkt mit
// `react-dom/server` in ein Test-DOM (jsdom) gerendert, um ohne neue
// Abhängigkeit auszukommen.
import { describe, expect, it } from "vitest";
import { createRoot, type Root } from "react-dom/client";
import { act, type ReactElement } from "react";

import { ChatTurnList } from "../_components/ChatTurnList";
import type { ChatTurn } from "../_lib/types";

function renderIntoContainer(node: ReactElement): { container: HTMLDivElement; root: Root } {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  act(() => {
    root.render(node);
  });
  return { container, root };
}

describe("ChatTurnList — angreiferkontrollierter Text bleibt Text", () => {
  it("rendert <img src=x onerror=...> als Textinhalt, nicht als Bild-Tag", () => {
    const turns: ChatTurn[] = [
      {
        id: "1",
        role: "assistant",
        text: '<img src=x onerror="alert(1)">',
        feedback: null,
      },
    ];
    const { container, root } = renderIntoContainer(
      <ChatTurnList turns={turns} feedbackRouteMissing={true} />,
    );

    expect(container.querySelectorAll("img").length).toBe(0);
    expect(container.textContent).toContain('<img src=x onerror="alert(1)">');

    act(() => root.unmount());
    container.remove();
  });

  it("rendert einen Markdown-Link als Text, nicht als <a>", () => {
    const turns: ChatTurn[] = [
      {
        id: "1",
        role: "assistant",
        text: "[klick mich](javascript:alert(1))",
        feedback: null,
      },
    ];
    const { container, root } = renderIntoContainer(
      <ChatTurnList turns={turns} feedbackRouteMissing={true} />,
    );

    expect(container.querySelectorAll("a").length).toBe(0);
    expect(container.textContent).toContain("[klick mich](javascript:alert(1))");

    act(() => root.unmount());
    container.remove();
  });

  it("deaktiviert die Daumen-Runter-Schaltfläche, solange die Signal-Route fehlt", () => {
    const turns: ChatTurn[] = [{ id: "1", role: "assistant", text: "Antwort", feedback: null }];
    const { container, root } = renderIntoContainer(
      <ChatTurnList turns={turns} feedbackRouteMissing={true} />,
    );

    const button = container.querySelector("button");
    expect(button).not.toBeNull();
    expect(button?.disabled).toBe(true);

    act(() => root.unmount());
    container.remove();
  });

  it("ruft onNegativeFeedback auf, wenn die Route existiert und geklickt wird", () => {
    const turns: ChatTurn[] = [{ id: "1", role: "assistant", text: "Antwort", feedback: null }];
    let called: ChatTurn | undefined;
    const { container, root } = renderIntoContainer(
      <ChatTurnList
        turns={turns}
        feedbackRouteMissing={false}
        onNegativeFeedback={(turn) => {
          called = turn;
        }}
      />,
    );

    const button = container.querySelector("button") as HTMLButtonElement;
    act(() => {
      button.click();
    });

    expect(called?.id).toBe("1");

    act(() => root.unmount());
    container.remove();
  });
});
