// Belegt, dass die Vorschlagswarteschlange „annehmen" nicht als Anwenden
// darstellt (Knoten UI-03, Pflichttest laut Auftrag).
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { ProposalQueue } from "../ProposalQueue";

describe("ProposalQueue", () => {
  it("bietet keinen Knopf an, der annehmen als Anwenden darstellt", () => {
    const html = renderToStaticMarkup(<ProposalQueue />);
    const dom = new DOMParser().parseFromString(html, "text/html");

    // Keine ausführbare Handlung in dieser Ansicht — es gibt keine
    // Interaktionselemente, die etwas verändern könnten.
    expect(dom.querySelectorAll("button").length).toBe(0);
    expect(html).not.toContain("Übernehmen");
    expect(html).not.toContain("Anwenden");
  });

  it("erklärt, dass annehmen/ablehnen nur den Status markiert, nie anwendet", () => {
    const html = renderToStaticMarkup(<ProposalQueue />);
    expect(html).toContain("wendet aber niemals eine vorgeschlagene Änderung an");
    expect(html).toContain("ApprovalRequest");
  });

  it("meldet, dass /context-proposal nicht über HTTP erreichbar ist", () => {
    const html = renderToStaticMarkup(<ProposalQueue />);
    expect(html).toContain("Surface::Command");
    expect(html).toContain("nicht als Surface::Web");
  });

  it("meldet, dass für ModelBehaviorProposal keine Operation existiert", () => {
    const html = renderToStaticMarkup(<ProposalQueue />);
    expect(html).toContain("überhaupt keine Operation");
  });
});
