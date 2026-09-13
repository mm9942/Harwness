// Operationsanbindung für den Agentenbaum (Knoten UI-03).
//
// # Auflage
// Wie `(sessions)/_lib/webRoutes.ts`: ausschließlich `findRoute`/
// `callOperation`, kein roher Pfad-String. `WEB_ROUTES` ist leer — jeder
// `resolve*`-Aufruf liefert aktuell `undefined`.
//
// # Geprüft, nicht erfunden
// `harw-ops::agent` deklariert eine Operation namens `"agent"`
// (list/stop/budget), aber:
// - sie ist `Surface::Command` (channel_parity), **kein** `Surface::Web` —
//   und `#[operation]` (siehe `harw-macros/src/operation.rs`) kennt aktuell
//   ohnehin nur die Unterattribute `command(...)` und `model_tool(...)`;
//   es gibt **kein** `web(...)`-Unterattribut, über das irgendeine
//   Operation im gesamten `harw-ops`-Baum eine Web-Route bekommen könnte.
//   `Surface::Web` wird ausschließlich in Testcode von
//   `harw-operations/src/operation.rs` manuell konstruiert.
// - selbst wenn sie exponiert würde: der `list`-Zweig liefert aktuell immer
//   `OpError::NotAvailable`, weil `ManagedAgentSpawner` noch nicht in
//   `OpContext` verdrahtet ist (siehe `harw-ops/src/agent.rs`-Kopf).
// - `AgentArgs`/`ChildRecord` tragen kein Feld, das eine Eltern-Kind-
//   Beziehung (`parent_span_id`) an die Operationsschicht durchreicht — die
//   Trace-Vererbung existiert in `harw-core::child_controller`
//   (`inherit_trace`), ist aber nicht Teil der Operation.
//
// Der hier verwendete Operationsname ist deshalb eine **erwartete**
// Erweiterung der bestehenden `"agent"`-Operation um eine Web-Route, kein
// neuer Name — siehe Abschlussbericht.
import { callOperation, findRoute, type OperationResult } from "@/lib/webClient";

/** Erwarteter Operationsname für den Agentenbaum — Erweiterung von `"agent"`. */
export const AGENT_TREE_OPERATIONS = {
  agentTree: "agent",
} as const;

/** Sucht die Agentenbaum-Route in [`WEB_ROUTES`]. */
export function resolveAgentTreeRoute() {
  return findRoute(AGENT_TREE_OPERATIONS.agentTree);
}

/**
 * Ruft die Agentenbaum-Route auf, falls deklariert.
 *
 * # Returns
 * `undefined`, wenn die Route (noch) nicht existiert. Sonst das
 * [`OperationResult`] von `callOperation`.
 */
export async function fetchAgentTree(): Promise<OperationResult | undefined> {
  const route = resolveAgentTreeRoute();
  if (route === undefined) {
    return undefined;
  }
  return callOperation(route, { action: "list" });
}
