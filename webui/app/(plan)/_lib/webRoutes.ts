// Operationsanbindung für Plan und Ziel (Knoten UI-03).
//
// # Auflage
// Ausschließlich `findRoute`/`callOperation` aus `@/lib/webClient` — kein
// roher Pfad-String. `WEB_ROUTES` ist leer; jeder `resolve*`-Aufruf liefert
// aktuell `undefined`.
//
// # Geprüft, nicht erfunden
// `harw-ops::plan` und `harw-ops::goal` deklarieren die Operationen `"plan"`
// und `"goal"` — beide `Surface::Command` + `Surface::ModelTool`, **kein**
// `Surface::Web` (aus demselben Grund wie bei `agent`: `#[operation]` kennt
// kein `web(...)`-Unterattribut, siehe `(sessions)/agents/_lib/webRoutes.ts`
// für die vollständige Begründung). Beide Operationen liefern außerdem nur
// `OpOutput { text: String }` — kein strukturiertes Plan-/Ziel-Schema.
//
// Diese Datei fragt bewusst die lesenden Unterkommandos an (`inspect` für
// Plan, `show` für Ziel) — nie ein Unterkommando, das den Plan/das Ziel
// verändert. Eine Änderung gehört, falls sie je über das Web nötig wird, in
// eine eigene, mit `ApprovalPolicy` versehene Route (siehe UI-06,
// Bestätigungsfläche) — nicht in diese Ansicht.
import { callOperation, findRoute, type OperationResult } from "@/lib/webClient";

/** Erwartete Operationsnamen — Erweiterung der bestehenden `"plan"`/`"goal"`-Operationen. */
export const PLAN_OPERATIONS = {
  planInspect: "plan",
  goalShow: "goal",
} as const;

/** Sucht die Plan-Route in [`WEB_ROUTES`]. */
export function resolvePlanRoute() {
  return findRoute(PLAN_OPERATIONS.planInspect);
}

/** Sucht die Ziel-Route in [`WEB_ROUTES`]. */
export function resolveGoalRoute() {
  return findRoute(PLAN_OPERATIONS.goalShow);
}

/**
 * Ruft die Plan-Route lesend auf (`inspect`), falls deklariert.
 *
 * # Returns
 * `undefined`, wenn die Route (noch) nicht existiert. Sonst das
 * [`OperationResult`] von `callOperation`.
 */
export async function fetchPlan(): Promise<OperationResult | undefined> {
  const route = resolvePlanRoute();
  if (route === undefined) {
    return undefined;
  }
  return callOperation(route, { args: "inspect" });
}

/**
 * Ruft die Ziel-Route lesend auf (`show`), falls deklariert.
 *
 * # Returns
 * `undefined`, wenn die Route (noch) nicht existiert. Sonst das
 * [`OperationResult`] von `callOperation`.
 */
export async function fetchGoal(): Promise<OperationResult | undefined> {
  const route = resolveGoalRoute();
  if (route === undefined) {
    return undefined;
  }
  return callOperation(route, { args: "show" });
}
