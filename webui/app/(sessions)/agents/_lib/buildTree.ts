// Reine Baum-Konstruktion für den Agentenbaum (Knoten UI-03).
//
// Getrennt von jeder Komponente, damit die Hierarchie-Logik ohne DOM und
// ohne Netzwerk testbar ist (siehe `lib/sse.ts`-Kopf für dieselbe
// Begründung bei `trackSequence`). Der Trace ist die Klammer: ein Kind
// erbt die `traceId` seines Elternteils und trägt dessen `spanId` als
// eigene `parentSpanId` (siehe `harw-core/src/child_controller.rs`,
// `inherit_trace`). Fehlt dieser Schlüssel vollständig, baut diese Funktion
// **keinen** erfundenen Baum, sondern meldet das über
// `AgentTreeResult.parentKeyMissing` — der Aufrufer zeigt dann eine flache
// Liste mit ausdrücklichem Hinweis statt einer falschen Hierarchie.
import type { AgentNode } from "../../_lib/types";

/** Ein Knoten im aufgebauten Baum, inklusive seiner Kinder. */
export interface AgentTreeNode {
  readonly node: AgentNode;
  readonly children: readonly AgentTreeNode[];
}

/** Ergebnis von [`buildAgentTree`]. */
export interface AgentTreeResult {
  /** Wurzelknoten — Agenten ohne (auffindbaren) Elternteil. */
  readonly roots: readonly AgentTreeNode[];
  /**
   * `true`, wenn mehr als ein Knoten vorliegt und **keiner** eine
   * `parentSpanId` trägt — der Baumschlüssel fehlt dann vollständig und die
   * Hierarchie ist nicht ableitbar, statt fälschlich flach zu wirken.
   */
  readonly parentKeyMissing: boolean;
  /**
   * Anzahl der Knoten, deren `parentSpanId` auf keinen bekannten `spanId`
   * verweist (z. B. weil der Elternteil außerhalb des gelieferten
   * Ausschnitts liegt). Diese Knoten werden als Wurzeln behandelt, aber
   * gezählt, damit die Ansicht das nicht stillschweigend verschluckt.
   */
  readonly orphanCount: number;
}

/**
 * Baut die Eltern-Kind-Hierarchie aus einer flachen Liste von Agentenknoten.
 *
 * # Arguments
 * - `nodes` (`readonly AgentNode[]`): flache Liste, z. B. aus einer
 *   (aktuell nicht existierenden) `agent`-Web-Route.
 *
 * # Returns
 * [`AgentTreeResult`] mit den Wurzelknoten und den beiden Lücken-Signalen
 * (`parentKeyMissing`, `orphanCount`).
 */
export function buildAgentTree(nodes: readonly AgentNode[]): AgentTreeResult {
  const bySpanId = new Map<string, AgentNode>(nodes.map((node) => [node.spanId, node]));
  const childrenOf = new Map<string, AgentNode[]>();
  const roots: AgentNode[] = [];
  let orphanCount = 0;

  const parentKeyMissing = nodes.length > 1 && nodes.every((node) => node.parentSpanId === null);

  for (const node of nodes) {
    if (node.parentSpanId === null) {
      roots.push(node);
      continue;
    }
    const parent = bySpanId.get(node.parentSpanId);
    if (parent === undefined) {
      orphanCount += 1;
      roots.push(node);
      continue;
    }
    const siblings = childrenOf.get(parent.spanId) ?? [];
    siblings.push(node);
    childrenOf.set(parent.spanId, siblings);
  }

  const toTreeNode = (node: AgentNode): AgentTreeNode => ({
    node,
    children: (childrenOf.get(node.spanId) ?? []).map(toTreeNode),
  });

  return {
    roots: roots.map(toTreeNode),
    parentKeyMissing,
    orphanCount,
  };
}

/**
 * Versucht, `AgentNode[]` aus dem freien Textkörper einer Operationsantwort
 * zu lesen.
 *
 * # Description
 * `OpOutput`/`OperationResult` tragen nur `{ text: string }` — es gibt
 * (Stand dieses Knotens) kein dokumentiertes JSON-Schema für einen
 * Agentenbaum im Web-Transport. Diese Funktion ist ein defensiver,
 * spekulativer Parser: sie versucht `JSON.parse` und prüft grob die
 * erwartete Form, wirft aber nie — eine nicht auswertbare Antwort liefert
 * `null`, nie eine erfundene Struktur.
 *
 * # Arguments
 * - `text` (`string`): der rohe `OperationResult.text`-Wert.
 *
 * # Returns
 * `AgentNode[]`, wenn der Text als solches JSON-Array lesbar ist; sonst
 * `null`.
 */
export function parseAgentNodesFromText(text: string): readonly AgentNode[] | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return null;
  }
  if (!Array.isArray(parsed)) {
    return null;
  }
  const nodes: AgentNode[] = [];
  for (const entry of parsed) {
    if (
      typeof entry !== "object" ||
      entry === null ||
      typeof (entry as { spanId?: unknown }).spanId !== "string" ||
      typeof (entry as { traceId?: unknown }).traceId !== "string" ||
      typeof (entry as { label?: unknown }).label !== "string"
    ) {
      return null;
    }
    const raw = entry as {
      spanId: string;
      traceId: string;
      label: string;
      parentSpanId?: unknown;
      contextCeiling?: unknown;
      permissions?: unknown;
    };
    nodes.push({
      spanId: raw.spanId,
      traceId: raw.traceId,
      label: raw.label,
      parentSpanId: typeof raw.parentSpanId === "string" ? raw.parentSpanId : null,
      contextCeiling: typeof raw.contextCeiling === "string" ? raw.contextCeiling : null,
      permissions: typeof raw.permissions === "string" ? raw.permissions : null,
    });
  }
  return nodes;
}
