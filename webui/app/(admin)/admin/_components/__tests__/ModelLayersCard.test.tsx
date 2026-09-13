// Pflichttests: vier Schichten bleiben unterscheidbar; HTML-Injektion im
// Modellnamen erscheint als Text (Knoten UI-07).
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { ResolvedModelView } from "../../_lib/types";
import { ModelLayersCard } from "../ModelLayersCard";

function buildModel(overrides: Partial<ResolvedModelView["descriptor"]> = {}): ResolvedModelView {
  return {
    provider: {
      id: "openai",
      name: "OpenAI",
      baseUrl: "https://api.openai.com",
      api: "openai_chat",
      featured: true,
      defaultModel: "gpt-5",
    },
    descriptor: {
      provider: "openai",
      model: "gpt-5",
      contextWindow: 200_000,
      maxOutputTokens: 32_000,
      modalities: ["text", "image"],
      toolCalling: "native",
      ...overrides,
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
      updatedAtIso: null,
      scores: [
        { label: "tool_schema_reliability", value: 50, measured: false, evidenceCount: 0 },
      ],
    },
  };
}

describe("ModelLayersCard", () => {
  it("zeigt alle vier Schichten als getrennte Blöcke, nicht als eine Zahl", () => {
    const html = renderToStaticMarkup(<ModelLayersCard model={buildModel()} />);
    expect(html).toContain("Layer 1");
    expect(html).toContain("Layer 2");
    expect(html).toContain("Layer 3");
    expect(html).toContain("Layer 4");
    // Kein einzelner "Gesamt-Score"-Begriff, der die vier Schichten verrührt.
    expect(html.toLowerCase()).not.toContain("gesamtscore");
    expect(html.toLowerCase()).not.toContain("overall score");
  });

  it("zeigt einen Modellnamen mit <img>-Injektion als reinen Text", () => {
    const payload = '<img src=x onerror=alert(1)>';
    const html = renderToStaticMarkup(<ModelLayersCard model={buildModel({ model: payload })} />);
    expect(html).not.toContain("<img src=x");
    const dom = new DOMParser().parseFromString(html, "text/html");
    expect(dom.querySelectorAll("img").length).toBe(0);
    const preTexts = Array.from(dom.querySelectorAll("pre")).map((el) => el.textContent);
    expect(preTexts.some((text) => text?.includes(payload))).toBe(true);
  });

  it("zeigt einen nicht gemessenen Score ehrlich als 'nicht gemessen', nicht als Zahl 50", () => {
    const html = renderToStaticMarkup(<ModelLayersCard model={buildModel()} />);
    expect(html).toContain("nicht gemessen");
  });
});
