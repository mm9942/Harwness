// Modellkatalog-Ansicht — vier Schichten je Modell, plus Layer-4-Rückkanal (Knoten UI-07, Teil 1).
"use client";

import { useEffect, useState } from "react";
import type { JSX } from "react";

import type { ListLoadState, ModelBehaviorProposalView, ResolvedModelView } from "../_lib/types";
import {
  ADMIN_OPERATIONS,
  fetchModelBehaviorProposals,
  fetchModelCatalog,
  resolveModelBehaviorProposalRoute,
  resolveModelCatalogRoute,
} from "../_lib/webRoutes";
import { ListStateNotice } from "./ListStateNotice";
import { ModelBehaviorProposalPanel } from "./ModelBehaviorProposalPanel";
import { ModelLayersCard } from "./ModelLayersCard";

/**
 * Parst die Textantwort der (erwarteten) Modellkatalog-Operation. Solange
 * keine Route existiert, wird diese Funktion nie mit echten Daten
 * aufgerufen (siehe `webRoutes.ts`: `harw-ops` hat keine Operation, die
 * `ResolvedModel` über alle vier Layer exponiert). Liefert `[]`, statt ein
 * Schema zu erfinden.
 */
export function parseModelCatalogText(_text: string): readonly ResolvedModelView[] {
  return [];
}

/** Parst die Textantwort der (erwarteten) `ModelBehaviorProposal`-Operation. Siehe [`parseModelCatalogText`] für dieselbe Begründung. */
export function parseModelBehaviorProposalsText(_text: string): readonly ModelBehaviorProposalView[] {
  return [];
}

function useModelCatalogState(overrideState?: ListLoadState<ResolvedModelView>): ListLoadState<ResolvedModelView> {
  const routeExists = resolveModelCatalogRoute() !== undefined;
  const [state, setState] = useState<ListLoadState<ResolvedModelView>>(
    overrideState ?? { kind: routeExists ? "loading" : "route-missing" },
  );

  useEffect(() => {
    if (overrideState !== undefined || !routeExists) {
      return;
    }
    let cancelled = false;
    fetchModelCatalog()
      .then((result) => {
        if (cancelled || result === undefined) {
          return;
        }
        if (!result.ok) {
          setState({ kind: "error", message: result.error });
          return;
        }
        const items = parseModelCatalogText(result.text);
        setState(items.length === 0 ? { kind: "empty" } : { kind: "loaded", items });
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
  }, [routeExists, overrideState]);

  return state;
}

function useModelBehaviorProposalState(
  overrideState?: ListLoadState<ModelBehaviorProposalView>,
): ListLoadState<ModelBehaviorProposalView> {
  const routeExists = resolveModelBehaviorProposalRoute() !== undefined;
  const [state, setState] = useState<ListLoadState<ModelBehaviorProposalView>>(
    overrideState ?? { kind: routeExists ? "loading" : "route-missing" },
  );

  useEffect(() => {
    if (overrideState !== undefined || !routeExists) {
      return;
    }
    let cancelled = false;
    fetchModelBehaviorProposals()
      .then((result) => {
        if (cancelled || result === undefined) {
          return;
        }
        if (!result.ok) {
          setState({ kind: "error", message: result.error });
          return;
        }
        const items = parseModelBehaviorProposalsText(result.text);
        setState(items.length === 0 ? { kind: "empty" } : { kind: "loaded", items });
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
  }, [routeExists, overrideState]);

  return state;
}

export interface ModelCatalogViewProps {
  /** Optionaler Zustand für Tests — überspringt den `useEffect`-Ladepfad. */
  readonly overrideCatalogState?: ListLoadState<ResolvedModelView>;
  /** Optionaler Zustand für Tests — überspringt den `useEffect`-Ladepfad. */
  readonly overrideProposalState?: ListLoadState<ModelBehaviorProposalView>;
}

/**
 * Zeigt den vierschichtigen Modellkatalog sowie die offenen
 * `ModelBehaviorProposal`-Vorschläge mit Katalog/Beobachtung-Bezug.
 *
 * # Returns
 * Die vollständige Modellkatalog-Ansicht, inklusive Lücken-Meldungen, wenn
 * keine Route existiert.
 */
export function ModelCatalogView({ overrideCatalogState, overrideProposalState }: ModelCatalogViewProps): JSX.Element {
  const catalogState = useModelCatalogState(overrideCatalogState);
  const proposalState = useModelBehaviorProposalState(overrideProposalState);

  const models = catalogState.kind === "loaded" ? catalogState.items : [];
  const findModel = (provider: string, model: string): ResolvedModelView | null =>
    models.find((m) => m.descriptor.provider === provider && m.descriptor.model === model) ?? null;

  return (
    <div className="harw-model-catalog">
      <section aria-label="Modellkatalog">
        <h2>Modellkatalog — vier Schichten</h2>
        <ListStateNotice
          state={catalogState}
          operation={ADMIN_OPERATIONS.modelCatalogList}
          detail="Die bestehende /model-Operation ist Command-only + TUI-only und zeigt nur das aktive Modell, nicht die vier aufgelösten Layer irgendeines Modells."
          emptyLabel="Der Katalog meldet keine Modelle."
        />
        {models.map((model) => (
          <ModelLayersCard key={`${model.descriptor.provider}/${model.descriptor.model}`} model={model} />
        ))}
      </section>

      <section aria-label="Layer-4-Rückkanal">
        <h2>Vorschläge zur Katalogbehauptung (Layer-4-Rückkanal)</h2>
        <ListStateNotice
          state={proposalState}
          operation={ADMIN_OPERATIONS.modelBehaviorProposalList}
          detail="harw-knowledge::model_behavior_proposal exportiert reine Funktionen/Typen ohne #[operation]-Deklaration."
          emptyLabel="Es liegen keine Vorschläge vor."
        />
        {proposalState.kind === "loaded"
          ? proposalState.items.map((proposal) => (
              <ModelBehaviorProposalPanel
                key={proposal.id}
                proposal={proposal}
                targetModel={findModel(proposal.targetProvider, proposal.targetModel)}
              />
            ))
          : null}
      </section>
    </div>
  );
}
