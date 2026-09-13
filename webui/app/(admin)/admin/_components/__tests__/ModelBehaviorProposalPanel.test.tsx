// Pflichttest: ein Vorschlag wird nicht als anwendbar dargestellt — kein
// Knopf, kein Label, das "übernehmen"/"anwenden" verspricht (Knoten UI-07).
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { ModelBehaviorProposalView, ResolvedModelView } from "../../_lib/types";
import { ModelBehaviorProposalPanel } from "../ModelBehaviorProposalPanel";

const proposal: ModelBehaviorProposalView = {
  id: "model-behavior-proposal/gpt-5-tool-schema",
  title: "Tool-Schema wiederholt verletzt",
  targetProvider: "openai",
  targetModel: "gpt-5",
  changes: [{ kind: "downgrade_tool_calling", to: "basic" }],
  producedBy: "heuristic:tool-calling-downgrade@1",
  status: "Pending",
};

const targetModel: ResolvedModelView = {
  provider: null,
  descriptor: {
    provider: "openai",
    model: "gpt-5",
    contextWindow: 200_000,
    maxOutputTokens: null,
    modalities: ["text"],
    toolCalling: "native",
  },
  runtime: {
    contextPolicy: "sliding_window",
    compactionPolicy: "aggressive",
    delegationPolicy: "restricted",
    maxParallelTools: 4,
    maxChildFanout: 2,
  },
  observed: {
    provider: "openai",
    model: "gpt-5",
    updatedAtIso: "2026-08-01T00:00:00Z",
    scores: [{ label: "tool_schema_reliability", value: 12, measured: true, evidenceCount: 8 }],
  },
};

describe("ModelBehaviorProposalPanel", () => {
  it("enthält keinen Knopf und kein Label, das eine Katalogänderung verspricht", () => {
    const html = renderToStaticMarkup(<ModelBehaviorProposalPanel proposal={proposal} targetModel={targetModel} />);
    const dom = new DOMParser().parseFromString(html, "text/html");
    expect(dom.querySelectorAll("button").length).toBe(0);
    const lowered = html.toLowerCase();
    expect(lowered).not.toContain(">übernehmen<");
    expect(lowered).not.toContain(">anwenden<");
    expect(lowered).not.toContain("apply</");
  });

  it("zeigt den Bezug 'Katalog sagt X, Beobachtung sagt Y, Vorschlag liegt vor'", () => {
    const html = renderToStaticMarkup(<ModelBehaviorProposalPanel proposal={proposal} targetModel={targetModel} />);
    expect(html).toContain("native"); // Katalogbehauptung, Layer 2
    expect(html).toContain("12"); // Beobachtung, Layer 4
    expect(html).toContain("downgrade_tool_calling"); // der Vorschlag selbst
  });

  it("meldet fehlenden Modellbezug statt einen Vergleich zu erfinden", () => {
    const html = renderToStaticMarkup(<ModelBehaviorProposalPanel proposal={proposal} targetModel={null} />);
    expect(html).toContain("nicht auffindbar");
  });
});
