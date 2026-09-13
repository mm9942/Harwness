// Definitionsregistry-Ansicht — Rollen, Familien, Kontextprogramme (Knoten UI-07, Teil 2).
//
// # Was diese Ansicht NICHT tut
// Sie ändert keine Definition. Es gibt in dieser Datei keinen Knopf, kein
// Formular und keinen Handler, der einen Wert an `harw-agent-dsl` schreibt
// — eine Definition zu ändern heißt, die `AuthorityCeiling`/`ContextCeiling`
// eines Agenten zu ändern, und das ist laut Auftrag ausschließlich Sache
// eines `ApprovalRequest` (Knoten UI-06), nie eines Knopfs hier.
"use client";

import { useEffect, useState } from "react";
import type { JSX } from "react";

import { DataBlock } from "@/components/ui/DataBlock";

import type { CeilingChainView, DefinitionSummaryView, ListLoadState } from "../_lib/types";
import { ADMIN_OPERATIONS, fetchDefinitionRegistry, resolveDefinitionRegistryRoute } from "../_lib/webRoutes";
import { ContextCeilingChain } from "./ContextCeilingChain";
import { ListStateNotice } from "./ListStateNotice";
import { SnapshotIdBlock } from "./SnapshotIdBlock";

/**
 * Parst die Textantwort der (erwarteten) Definitionsregistry-Operation in
 * eine Liste von [`DefinitionSummaryView`]. Solange keine Route existiert,
 * wird diese Funktion nie mit echten Daten aufgerufen — sie steht bereit,
 * exportiert für Tests, die einen zukünftigen Wire-Format-Vertrag prüfen
 * können, ohne eines zu erfinden: aktuell liefert sie nur `[]`, weil
 * `harw-ops` kein Schema für diese Antwort deklariert (siehe `webRoutes.ts`).
 */
export function parseDefinitionRegistryText(_text: string): readonly DefinitionSummaryView[] {
  return [];
}

/** Einzelne Definitionskarte — Rolle, Familie, Kontextprogramm, SnapshotId. */
function DefinitionCard({
  definition,
  chain,
}: {
  readonly definition: DefinitionSummaryView;
  readonly chain: CeilingChainView | null;
}): JSX.Element {
  return (
    <article aria-label={`Definition ${definition.definitionId}`}>
      <DataBlock label="Definition" value={definition.definitionId} />
      <DataBlock label="Rolle" value={definition.role} />
      <DataBlock label="Familie" value={definition.family ?? "keine Familie"} />
      <DataBlock label="Kontextprogramm" value={definition.contextProgramId} />
      <SnapshotIdBlock snapshotId={definition.snapshotId} />
      {chain !== null ? <ContextCeilingChain chain={chain} /> : null}
    </article>
  );
}

export interface DefinitionRegistryViewProps {
  /** Optionaler Zustand für Tests — überspringt den `useEffect`-Ladepfad. */
  readonly overrideState?: ListLoadState<DefinitionSummaryView>;
  /** Optionale Decken-Ketten je `definitionId`, für Tests/Demo-Zwecke. */
  readonly chainsByDefinitionId?: ReadonlyMap<string, CeilingChainView>;
}

/**
 * Lädt und zeigt die Definitionsregistry — oder, mangels Route, die
 * entsprechende Lücken-Meldung mit dem Namen der fehlenden Operation.
 *
 * # Returns
 * Die vollständige Definitionsregistry-Ansicht.
 */
export function DefinitionRegistryView({ overrideState, chainsByDefinitionId }: DefinitionRegistryViewProps): JSX.Element {
  const routeExists = resolveDefinitionRegistryRoute() !== undefined;
  const [state, setState] = useState<ListLoadState<DefinitionSummaryView>>(
    overrideState ?? { kind: routeExists ? "loading" : "route-missing" },
  );

  useEffect(() => {
    if (overrideState !== undefined || !routeExists) {
      return;
    }
    let cancelled = false;
    fetchDefinitionRegistry()
      .then((result) => {
        if (cancelled || result === undefined) {
          return;
        }
        if (!result.ok) {
          setState({ kind: "error", message: result.error });
          return;
        }
        const items = parseDefinitionRegistryText(result.text);
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

  const notice = (
    <ListStateNotice
      state={state}
      operation={ADMIN_OPERATIONS.definitionRegistryList}
      detail="harw-agent-dsl hat keine einzige #[operation]-Datei im gesamten Baum — es gibt nicht einmal einen Command-Zweig, geschweige denn eine Web-Route."
      emptyLabel="Die Registry meldet keine Definitionen."
    />
  );

  return (
    <section aria-label="Definitionsregistry">
      <h2>Agentendefinitionen</h2>
      {notice}
      {state.kind === "loaded"
        ? state.items.map((definition) => (
            <DefinitionCard
              key={definition.definitionId}
              definition={definition}
              chain={chainsByDefinitionId?.get(definition.definitionId) ?? null}
            />
          ))
        : null}
    </section>
  );
}
