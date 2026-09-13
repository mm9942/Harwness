// Pflichttest: eine extends-Kette zeigt sichtbar, wo TrustClass geschnitten
// wurde (Knoten UI-07, Teil 2).
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { CeilingChainView } from "../../_lib/types";
import { ContextCeilingChain } from "../ContextCeilingChain";

describe("ContextCeilingChain", () => {
  it("markiert eine Stufe, die TrustClass senkt, sichtbar als geschnitten", () => {
    const chain: CeilingChainView = {
      leafProgramId: "coding.implement",
      steps: [
        { programId: "base", trustBefore: "Instruction", trustAfter: "Instruction", cut: false },
        { programId: "coding.implement", trustBefore: "Instruction", trustAfter: "Data", cut: true },
      ],
    };
    const html = renderToStaticMarkup(<ContextCeilingChain chain={chain} />);
    expect(html).toContain("ja — TrustClass auf Data gesenkt");
    expect(html).toContain("nein — unverändert übernommen");
    expect(html).toContain("Mindestens eine Stufe dieser Kette hat die TrustClass gegenüber ihrer Basis gesenkt.");
  });

  it("zeigt eine Kette ohne Schnitt als solche", () => {
    const chain: CeilingChainView = {
      leafProgramId: "research.explore",
      steps: [{ programId: "base", trustBefore: "Evidence", trustAfter: "Evidence", cut: false }],
    };
    const html = renderToStaticMarkup(<ContextCeilingChain chain={chain} />);
    expect(html).toContain("Keine Stufe dieser Kette senkt die TrustClass gegenüber ihrer Basis.");
  });

  it("zeigt eine leere Kette (kein extends) ausdrücklich statt einer leeren Liste", () => {
    const chain: CeilingChainView = { leafProgramId: "base", steps: [] };
    const html = renderToStaticMarkup(<ContextCeilingChain chain={chain} />);
    expect(html).toContain("keine Ableitung — dieses Kontextprogramm hat kein extends");
  });
});
