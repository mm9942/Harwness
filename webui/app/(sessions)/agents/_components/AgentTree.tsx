// Agentenbaum — rein lesende Ansicht (Knoten UI-03).
//
// Baut die Hierarchie ausschließlich über [`buildAgentTree`] aus
// `_lib/buildTree.ts` (reine Funktion, siehe deren Kopf) und zeigt jeden
// Knotennamen sowie jede geschnittene Decke/Permission über [`DataBlock`] —
// beide Felder sind angreiferkontrolliert.
"use client";

import { useEffect, useState } from "react";
import type { JSX } from "react";

import { DataBlock } from "@/components/ui/DataBlock";
import { connectWebEventStream } from "@/lib/sse";

import { appendGapNotice } from "../../_lib/eventGap";
import type { AgentNode, GapNotice } from "../../_lib/types";
import { GapBanner } from "../../_components/GapBanner";
import { RouteMissingNotice } from "../../_components/RouteMissingNotice";
import { buildAgentTree, parseAgentNodesFromText, type AgentTreeNode } from "../_lib/buildTree";
import { AGENT_TREE_OPERATIONS, fetchAgentTree, resolveAgentTreeRoute } from "../_lib/webRoutes";

const EVENTS_URL = "/events";

type LoadState =
  | { readonly kind: "idle" }
  | { readonly kind: "loading" }
  | { readonly kind: "loaded"; readonly nodes: readonly AgentNode[] }
  | { readonly kind: "unparseable"; readonly rawText: string }
  | { readonly kind: "error"; readonly message: string };

/** Rendert einen Baumknoten und rekursiv seine Kinder als eingerückte Liste. */
function TreeNode({ tree }: { readonly tree: AgentTreeNode }): JSX.Element {
  return (
    <li>
      <DataBlock label="Agent" value={tree.node.label} />
      <DataBlock label="span_id / trace_id" value={`${tree.node.spanId} / ${tree.node.traceId}`} />
      {tree.node.contextCeiling !== null ? (
        <DataBlock label="Kontext-Decke" value={tree.node.contextCeiling} />
      ) : null}
      {tree.node.permissions !== null ? (
        <DataBlock label="Permissions" value={tree.node.permissions} />
      ) : null}
      {tree.children.length > 0 ? (
        <ul>
          {tree.children.map((child) => (
            <TreeNode key={child.node.spanId} tree={child} />
          ))}
        </ul>
      ) : null}
    </li>
  );
}

/**
 * Lädt und zeigt den Agentenbaum; abonniert den Ereignisstrom für Lücken.
 *
 * # Description
 * Zeigt [`RouteMissingNotice`], solange keine Route existiert. Liefert eine
 * Route Text, der sich nicht als `AgentNode[]` lesen lässt (kein
 * dokumentiertes Schema, siehe `_lib/buildTree.ts`), zeigt diese Ansicht den
 * Rohtext über [`DataBlock`] statt eine Struktur zu erfinden.
 *
 * # Returns
 * Das gerenderte Agentenbaum-Fragment.
 */
export function AgentTree(): JSX.Element {
  const [state, setState] = useState<LoadState>({ kind: "idle" });
  const [gaps, setGaps] = useState<readonly GapNotice[]>([]);
  const route = resolveAgentTreeRoute();

  useEffect(() => {
    if (route === undefined) {
      return;
    }
    let cancelled = false;
    setState({ kind: "loading" });
    fetchAgentTree()
      .then((result) => {
        if (cancelled || result === undefined) {
          return;
        }
        if (!result.ok) {
          setState({ kind: "error", message: result.error });
          return;
        }
        const nodes = parseAgentNodesFromText(result.text);
        setState(nodes === null ? { kind: "unparseable", rawText: result.text } : { kind: "loaded", nodes });
      })
      .catch((error: unknown) => {
        if (!cancelled) {
          setState({ kind: "error", message: error instanceof Error ? error.message : String(error) });
        }
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [route === undefined]);

  useEffect(() => {
    const handle = connectWebEventStream(EVENTS_URL, {
      onGap: (missingCount) => {
        setGaps((previous) => appendGapNotice(previous, missingCount));
      },
    });
    return () => handle.close();
  }, []);

  const tree = state.kind === "loaded" ? buildAgentTree(state.nodes) : null;

  return (
    <section className="harw-agent-tree" aria-label="Agentenbaum">
      <h2>Agentenbaum</h2>
      <GapBanner notices={gaps} />
      {route === undefined ? (
        <RouteMissingNotice
          operation={AGENT_TREE_OPERATIONS.agentTree}
          detail="Die bestehende /agent-Operation ist Command-only, hat keinen Web-Zweig (#[operation] kennt kein web(...)-Unterattribut), liefert für list aktuell OpError::NotAvailable und trägt kein Feld für parent_span_id/ContextCeiling."
        />
      ) : null}
      {state.kind === "loading" ? <DataBlock label="Status" value="Lade Agentenbaum…" /> : null}
      {state.kind === "error" ? <DataBlock label="Fehler" value={state.message} /> : null}
      {state.kind === "unparseable" ? (
        <>
          <DataBlock
            label="Hinweis"
            value="Die Antwort ist kein bekanntes Baum-Format (kein dokumentiertes JSON-Schema für den Agentenbaum) — Rohtext:"
          />
          <DataBlock value={state.rawText} />
        </>
      ) : null}
      {tree !== null ? (
        <>
          {tree.parentKeyMissing ? (
            <DataBlock
              label="Hinweis"
              value="Kein gelieferter Knoten trägt eine parent_span_id — der Baumschlüssel fehlt vollständig, daher wird keine Hierarchie angezeigt, nur die flache Liste unten."
            />
          ) : null}
          {tree.orphanCount > 0 ? (
            <DataBlock
              label="Hinweis"
              value={`${tree.orphanCount} Agent(en) verweisen auf eine parent_span_id außerhalb der gelieferten Liste — als Wurzel angezeigt.`}
            />
          ) : null}
          <ul>
            {tree.roots.map((root) => (
              <TreeNode key={root.node.spanId} tree={root} />
            ))}
          </ul>
        </>
      ) : null}
    </section>
  );
}
