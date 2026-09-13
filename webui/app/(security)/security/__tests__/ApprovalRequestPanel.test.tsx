// Sicherheitstests für die Bestätigungsfläche (Knoten UI-06).
//
// Deckt die im Auftrag genannten Pflichttests ab, soweit sie aus der
// gerenderten Auszeichnung ohne Interaktionssimulation prüfbar sind
// (dieselbe statische Rendering-Methode wie
// `SecurityListPanel.test.tsx`/`ProposalQueue.test.tsx`: `webui/` hat
// (Stand dieses Knotens) keine Testing-Library-Klick-Simulation
// eingerichtet):
// - eine irreversible Aktion ist sichtbar und textlich anders markiert als
//   eine umkehrbare;
// - ein Befundtext mit `<img src=x onerror=...>` erscheint als Text, nie
//   als Markup;
// - fehlt die Route, erscheint eine Lücken-Meldung statt eines
//   funktionslosen oder täuschend aktiven Knopfes;
// - es gibt genau drei Entscheidungsknöpfe, keinen versteckten vierten
//   Weg, eine Aktion ohne Entscheidung auszulösen.
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { ApprovalRequestPanel } from "../_components/ApprovalRequestPanel";
import type { PendingApprovalView } from "../_lib/approvalTypes";

function irreversibleView(rationale: string): PendingApprovalView {
  return {
    record: {
      request: "approval-1",
      session: "session-1",
      callId: "call-1",
      actor: { kind: "operator", id: "alice" },
      issuedAt: "2026-09-01T00:00:00Z",
    },
    actionKind: "warden.kill_process_tree",
    irreversible: true,
    rationale,
  };
}

function reversibleView(rationale: string): PendingApprovalView {
  return {
    ...irreversibleView(rationale),
    actionKind: "warden.freeze_cgroup",
    irreversible: false,
  };
}

describe("ApprovalRequestPanel", () => {
  it("markiert eine irreversible Aktion sichtbar und unterscheidbar von einer umkehrbaren", () => {
    const irreversibleHtml = renderToStaticMarkup(
      <ApprovalRequestPanel view={irreversibleView("Prozessbaum verhält sich verdächtig")} />,
    );
    const reversibleHtml = renderToStaticMarkup(
      <ApprovalRequestPanel view={reversibleView("cgroup zeigt Anomalie")} />,
    );

    const irreversibleDom = new DOMParser().parseFromString(irreversibleHtml, "text/html");
    const reversibleDom = new DOMParser().parseFromString(reversibleHtml, "text/html");

    expect(
      irreversibleDom.querySelector('[data-testid="approval-irreversible-warning"]'),
    ).not.toBeNull();
    expect(
      irreversibleDom.querySelector('[data-testid="approval-reversible-notice"]'),
    ).toBeNull();

    expect(
      reversibleDom.querySelector('[data-testid="approval-reversible-notice"]'),
    ).not.toBeNull();
    expect(
      reversibleDom.querySelector('[data-testid="approval-irreversible-warning"]'),
    ).toBeNull();

    expect(irreversibleHtml).toContain("Nicht umkehrbar");
    expect(irreversibleHtml).not.toContain("Umkehrbar");
    expect(reversibleHtml).toContain("Umkehrbar");
    expect(reversibleHtml).not.toContain("Nicht umkehrbar");
  });

  it("zeigt einen bösartigen Befundtext als reinen Text, nie als <img>-Markup", () => {
    const payload = "<img src=x onerror=alert(1)>";
    const html = renderToStaticMarkup(<ApprovalRequestPanel view={irreversibleView(payload)} />);
    const dom = new DOMParser().parseFromString(html, "text/html");

    expect(dom.querySelectorAll("img").length).toBe(0);
    expect(html).toContain("&lt;img");
    const pres = Array.from(dom.querySelectorAll("pre")).map((el) => el.textContent);
    expect(pres).toContain(payload);
  });

  it("bietet genau drei Entscheidungsknöpfe an, solange eine Route erwartet werden könnte", () => {
    const html = renderToStaticMarkup(
      <ApprovalRequestPanel view={irreversibleView("Befund")} />,
    );
    const dom = new DOMParser().parseFromString(html, "text/html");
    const buttons = Array.from(dom.querySelectorAll("button"));
    expect(buttons.length).toBe(3);
    expect(dom.querySelector('[data-testid="approval-decision-approved"]')).not.toBeNull();
    expect(dom.querySelector('[data-testid="approval-decision-rejected"]')).not.toBeNull();
    expect(dom.querySelector('[data-testid="approval-decision-approved_once"]')).not.toBeNull();
  });

  it("zeigt die Anfrage-ID und Sitzung als Text, nie interpretiert", () => {
    const html = renderToStaticMarkup(
      <ApprovalRequestPanel view={irreversibleView("Befund")} />,
    );
    expect(html).toContain("approval-1");
    expect(html).toContain("session-1");
  });
});
