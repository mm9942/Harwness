// Plan und Ziel — rein lesende Ansicht (Knoten UI-03).
//
// Zeigt `plan inspect` und `goal show` als Text. Jede Zeile — insbesondere
// ein Plan-Knotentitel — geht durch [`DataBlock`] und wird damit garantiert
// als Text ausgegeben, nie als Markup (siehe `DataBlock.tsx`-Kopf).
"use client";

import { useEffect, useState } from "react";
import type { JSX } from "react";

import { DataBlock, DataBlockList } from "@/components/ui/DataBlock";

import { PLAN_OPERATIONS, fetchGoal, fetchPlan, resolveGoalRoute, resolvePlanRoute } from "../_lib/webRoutes";
import { RouteMissingNotice } from "./RouteMissingNotice";

/** Ladezustand einer text-basierten Plan-/Ziel-Operation — exportiert für Tests. */
export type LoadState =
  | { readonly kind: "idle" }
  | { readonly kind: "loading" }
  | { readonly kind: "loaded"; readonly lines: readonly string[] }
  | { readonly kind: "error"; readonly message: string };

/**
 * Lädt und zeigt eine text-basierte Operationsantwort zeilenweise.
 *
 * # Description
 * Gemeinsame Lade-Logik für Plan und Ziel — beide liefern nur
 * `OpOutput.text`. Exportiert, damit `PlanView`/`GoalView` sie identisch
 * verwenden und Tests dieselbe Zeilen-Zerlegung prüfen können.
 *
 * # Arguments
 * - `fetcher` (`() => Promise<OperationResult | undefined>`): `fetchPlan`
 *   oder `fetchGoal`.
 *
 * # Returns
 * `[LoadState, () => void]`: aktueller Zustand plus Auslöser (intern über
 * `useEffect` verdrahtet — kein manueller Aufruf nötig).
 */
function useTextOperation(
  routeExists: boolean,
  fetcher: () => ReturnType<typeof fetchPlan>,
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
        if (result.ok) {
          setState({ kind: "loaded", lines: result.text.split("\n").filter((line) => line.length > 0) });
        } else {
          setState({ kind: "error", message: result.error });
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

/** Rendert einen [`LoadState`] als Fragment aus [`RouteMissingNotice`]/[`DataBlock`]. Exportiert für Tests. */
export function TextOperationView({
  title,
  operation,
  detail,
  routeExists,
  state,
}: {
  readonly title: string;
  readonly operation: string;
  readonly detail: string;
  readonly routeExists: boolean;
  readonly state: LoadState;
}): JSX.Element {
  return (
    <section aria-label={title}>
      <h2>{title}</h2>
      {!routeExists ? <RouteMissingNotice operation={operation} detail={detail} /> : null}
      {state.kind === "loading" ? <DataBlock label="Status" value="Lade…" /> : null}
      {state.kind === "error" ? <DataBlock label="Fehler" value={state.message} /> : null}
      {state.kind === "loaded" ? <DataBlockList label={title} values={state.lines} /> : null}
    </section>
  );
}

/**
 * Zeigt den aktuellen Plan (`plan inspect`) zeilenweise an.
 *
 * # Returns
 * Das gerenderte Plan-Fragment.
 */
export function PlanView(): JSX.Element {
  const routeExists = resolvePlanRoute() !== undefined;
  const state = useTextOperation(routeExists, fetchPlan);
  return (
    <TextOperationView
      title="Plan"
      operation={PLAN_OPERATIONS.planInspect}
      detail="Die bestehende /plan-Operation ist Command-only + ModelTool, kein Web-Zweig (#[operation] kennt kein web(...)-Unterattribut), und liefert nur Freitext, kein Plan-Baum-Schema."
      routeExists={routeExists}
      state={state}
    />
  );
}

/**
 * Zeigt das aktuelle Ziel (`goal show`) zeilenweise an.
 *
 * # Returns
 * Das gerenderte Ziel-Fragment.
 */
export function GoalView(): JSX.Element {
  const routeExists = resolveGoalRoute() !== undefined;
  const state = useTextOperation(routeExists, fetchGoal);
  return (
    <TextOperationView
      title="Ziel"
      operation={PLAN_OPERATIONS.goalShow}
      detail="Die bestehende /goal-Operation ist Command-only + ModelTool, kein Web-Zweig."
      routeExists={routeExists}
      state={state}
    />
  );
}
