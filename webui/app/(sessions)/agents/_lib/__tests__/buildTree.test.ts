// Tests für die reine Baum-Konstruktion des Agentenbaums (Knoten UI-03).
import { describe, expect, it } from "vitest";

import type { AgentNode } from "../../../_lib/types";
import { buildAgentTree, parseAgentNodesFromText } from "../buildTree";

function node(overrides: Partial<AgentNode> & { spanId: string }): AgentNode {
  return {
    traceId: "trace-1",
    parentSpanId: null,
    label: overrides.spanId,
    contextCeiling: null,
    permissions: null,
    ...overrides,
  };
}

describe("buildAgentTree", () => {
  it("baut Eltern-Kind-Beziehungen aus parentSpanId", () => {
    const nodes = [
      node({ spanId: "root" }),
      node({ spanId: "child", parentSpanId: "root" }),
      node({ spanId: "grandchild", parentSpanId: "child" }),
    ];
    const result = buildAgentTree(nodes);
    expect(result.parentKeyMissing).toBe(false);
    expect(result.orphanCount).toBe(0);
    expect(result.roots).toHaveLength(1);
    expect(result.roots[0]?.node.spanId).toBe("root");
    expect(result.roots[0]?.children[0]?.node.spanId).toBe("child");
    expect(result.roots[0]?.children[0]?.children[0]?.node.spanId).toBe("grandchild");
  });

  it("meldet parentKeyMissing, wenn mehrere Knoten aber keiner eine parentSpanId trägt", () => {
    const nodes = [node({ spanId: "a" }), node({ spanId: "b" })];
    const result = buildAgentTree(nodes);
    expect(result.parentKeyMissing).toBe(true);
    expect(result.roots).toHaveLength(2);
  });

  it("meldet nie parentKeyMissing bei genau einem Knoten", () => {
    const result = buildAgentTree([node({ spanId: "solo" })]);
    expect(result.parentKeyMissing).toBe(false);
  });

  it("behandelt einen Knoten mit unbekannter parentSpanId als Wurzel und zählt ihn als Waise", () => {
    const nodes = [node({ spanId: "orphan", parentSpanId: "does-not-exist" })];
    const result = buildAgentTree(nodes);
    expect(result.orphanCount).toBe(1);
    expect(result.roots).toHaveLength(1);
    expect(result.roots[0]?.node.spanId).toBe("orphan");
  });

  it("baut mehrere Geschwister unter demselben Elternteil", () => {
    const nodes = [
      node({ spanId: "root" }),
      node({ spanId: "child-a", parentSpanId: "root" }),
      node({ spanId: "child-b", parentSpanId: "root" }),
    ];
    const result = buildAgentTree(nodes);
    expect(result.roots[0]?.children).toHaveLength(2);
  });
});

describe("parseAgentNodesFromText", () => {
  it("liefert null für nicht-JSON-Text", () => {
    expect(parseAgentNodesFromText("kein json")).toBeNull();
  });

  it("liefert null für JSON, das kein Array ist", () => {
    expect(parseAgentNodesFromText("{}")).toBeNull();
  });

  it("liefert null, wenn ein Eintrag die Pflichtfelder nicht trägt", () => {
    expect(parseAgentNodesFromText(JSON.stringify([{ spanId: "a" }]))).toBeNull();
  });

  it("liest ein gültiges Array in AgentNode[] ein", () => {
    const raw = [{ spanId: "a", traceId: "t", label: "Agent A", parentSpanId: null }];
    const result = parseAgentNodesFromText(JSON.stringify(raw));
    expect(result).toEqual([
      { spanId: "a", traceId: "t", label: "Agent A", parentSpanId: null, contextCeiling: null, permissions: null },
    ]);
  });
});
