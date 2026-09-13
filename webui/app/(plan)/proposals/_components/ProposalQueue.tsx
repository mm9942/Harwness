// Vorschlagswarteschlange — rein lesende Ansicht (Knoten UI-03).
//
// # Doktrin: schlägt vor, committet nie
// `ContextProposal` und `ModelBehaviorProposal` sind beide `OperatorOnly` —
// ein Vorschlag verrät Betriebsinterna. Und beide „annehmen" heißt:
// **markieren, nicht anwenden** — es gibt im ganzen Baum keine Funktion,
// die einen Vorschlag anwendet (siehe `harw-ops/src/context_proposal.rs`-
// Kopf: „Annehmen markiert nur"). Diese Ansicht bildet das ab, indem sie
// **keine** Handlung anbietet, die wie ein Ausführen aussieht: kein Knopf,
// kein Label „Übernehmen"/„Anwenden". Sie zeigt Status und Inhalt jedes
// Vorschlags und beschreibt in Prosa, was „annehmen"/„ablehnen" tatsächlich
// bedeutet (eine Statusänderung am Vorschlag-Artefakt) und wohin eine
// tatsächliche Entscheidung gehört: die Bestätigungsfläche UI-06
// (`ApprovalRequest`) — nie ein Knopf dieser Ansicht. Siehe Auftrag,
// Abschnitt „Die Vorschlagswarteschlange".
"use client";

import { useEffect, useState } from "react";
import type { JSX } from "react";

import { DataBlock } from "@/components/ui/DataBlock";

import type { ProposalSummary } from "../../_lib/types";
import { RouteMissingNotice } from "../../_components/RouteMissingNotice";
import { parseProposalListText } from "../_lib/parseProposals";
import {
  PROPOSAL_OPERATIONS,
  fetchContextProposalList,
  fetchModelBehaviorProposalList,
  resolveContextProposalListRoute,
  resolveModelBehaviorProposalListRoute,
} from "../_lib/webRoutes";

type LoadState =
  | { readonly kind: "idle" }
  | { readonly kind: "loading" }
  | { readonly kind: "loaded"; readonly proposals: readonly ProposalSummary[] }
  | { readonly kind: "error"; readonly message: string };

/** Erklärt die Markieren-nicht-Anwenden-Doktrin — nie ein Knopf, immer Prosa. */
function AcceptRejectExplainer(): JSX.Element {
  return (
    <DataBlock
      label={'Was „annehmen"/„ablehnen" bedeutet'}
      value={
        '„Annehmen" setzt den Status des Vorschlags-Artefakts auf Accepted, „ablehnen" auf Rejected — ' +
        "beides ändert ausschließlich diese eine Datei, wendet aber niemals eine vorgeschlagene Änderung an. " +
        "Eine tatsächliche Entscheidung wird hier nicht getroffen: dafür gibt es die Bestätigungsfläche " +
        "(ApprovalRequest), nicht diese Ansicht."
      }
    />
  );
}

/** Rendert eine Liste von Vorschlägen inklusive Status, ohne jede Handlung. */
function ProposalList({ title, proposals }: { readonly title: string; readonly proposals: readonly ProposalSummary[] }): JSX.Element {
  if (proposals.length === 0) {
    return (
      <section aria-label={title}>
        <h3>{title}</h3>
        <DataBlock value="Keine Vorschläge." />
      </section>
    );
  }
  return (
    <section aria-label={title}>
      <h3>{title}</h3>
      <ul>
        {proposals.map((proposal, index) => (
          // eslint-disable-next-line react/no-array-index-key -- reine Anzeigezeilen ohne stabile Identität außerhalb von `raw`.
          <li key={index}>
            <DataBlock label={proposal.status ?? "Status unbekannt"} value={proposal.raw} />
          </li>
        ))}
      </ul>
    </section>
  );
}

/** Lädt eine der beiden Vorschlagslisten, falls die zugehörige Route existiert. */
function useProposalList(
  routeExists: boolean,
  fetcher: () => Promise<{ readonly ok: boolean; readonly text?: string; readonly error?: string } | undefined>,
): LoadState {
  const [state, setState] = useState<LoadState>({ kind: "idle" });

  useEffect(() => {
    if (!routeExists) {
      return;
    }
    let cancelled = false;
    setState({ kind: "loading" });
    fetcher()
      .then((result) => {
        if (cancelled || result === undefined) {
          return;
        }
        if (result.ok && typeof result.text === "string") {
          setState({ kind: "loaded", proposals: parseProposalListText(result.text) });
        } else {
          setState({ kind: "error", message: result.error ?? "unbekannter Fehler" });
        }
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
  }, [routeExists]);

  return state;
}

/**
 * Zeigt beide Vorschlagswarteschlangen (`ContextProposal`,
 * `ModelBehaviorProposal`) rein lesend an.
 *
 * # Returns
 * Das gerenderte Vorschlagswarteschlangen-Fragment.
 */
export function ProposalQueue(): JSX.Element {
  const contextRouteExists = resolveContextProposalListRoute() !== undefined;
  const modelBehaviorRouteExists = resolveModelBehaviorProposalListRoute() !== undefined;
  const contextState = useProposalList(contextRouteExists, fetchContextProposalList);
  const modelBehaviorState = useProposalList(modelBehaviorRouteExists, fetchModelBehaviorProposalList);

  return (
    <section className="harw-proposal-queue" aria-label="Vorschlagswarteschlange">
      <h2>Vorschlagswarteschlange</h2>
      <AcceptRejectExplainer />

      {!contextRouteExists ? (
        <RouteMissingNotice
          operation={PROPOSAL_OPERATIONS.contextProposalList}
          detail="/context-proposal ist als Surface::Command deklariert (channel_parity), nicht als Surface::Web — über HTTP ist die Operation dadurch strukturell nicht erreichbar, unabhängig von WEB_ROUTES."
        />
      ) : null}
      {contextState.kind === "loading" ? <DataBlock label="Status" value="Lade Kontextprogramm-Vorschläge…" /> : null}
      {contextState.kind === "error" ? <DataBlock label="Fehler" value={contextState.message} /> : null}
      {contextState.kind === "loaded" ? (
        <ProposalList title="Kontextprogramm-Vorschläge (ContextProposal)" proposals={contextState.proposals} />
      ) : null}

      {!modelBehaviorRouteExists ? (
        <RouteMissingNotice
          operation={PROPOSAL_OPERATIONS.modelBehaviorProposalList}
          detail="Unter harw-ops existiert für ModelBehaviorProposal (AW6-06) überhaupt keine Operation — nur der Datentyp in harw-knowledge/src/model_behavior_proposal.rs."
        />
      ) : null}
      {modelBehaviorState.kind === "loading" ? <DataBlock label="Status" value="Lade Modellverhalten-Vorschläge…" /> : null}
      {modelBehaviorState.kind === "error" ? <DataBlock label="Fehler" value={modelBehaviorState.message} /> : null}
      {modelBehaviorState.kind === "loaded" ? (
        <ProposalList title="Modellverhalten-Vorschläge (ModelBehaviorProposal)" proposals={modelBehaviorState.proposals} />
      ) : null}
    </section>
  );
}
