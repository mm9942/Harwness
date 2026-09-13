// Operationsanbindung für Sitzungsliste und Agentenbaum (Knoten UI-03).
//
// # Auflage
// Jeder Aufruf geht ausschließlich über [`findRoute`]/[`callOperation`] aus
// `@/lib/webClient` — diese Datei ruft an keiner Stelle einen rohen
// Pfad-String auf. Die hier genannten Operationsnamen (`SESSION_OPERATIONS.*`)
// sind **erwartete** Namen, keine garantierten: `WEB_ROUTES` ist zum
// Zeitpunkt dieses Knotens leer (`lib/generated/operations.ts` → `[] as
// const`) — es gibt noch keine einzige deklarierte Route, für Sitzungen so
// wenig wie für alles andere. `resolveSessionListRoute` liefert deshalb
// aktuell `undefined`; die UI zeigt eine Lücken-Meldung mit dem
// Operationsnamen statt eine Route zu erfinden. Siehe Abschlussbericht
// dieses Knotens für die vollständige Meldung an den Registry-Eigentümer,
// inklusive der Beobachtung, dass es unter `harw-ops` überhaupt keine
// Operation namens „session"/„session.list" gibt (die nächstliegende
// bestehende Operation ist `/ps`, die Jobs listet, keine Sitzungen).
import { callOperation, findRoute, type OperationResult } from "@/lib/webClient";

/** Erwartete, aber (Stand dieses Knotens) nicht deklarierte Operationsnamen. */
export const SESSION_OPERATIONS = {
  /**
   * Erwarteter Name für die Sitzungsliste — orientiert an der
   * `OperationMeta::name`-Konvention aus `webClient.ts`-Kopf
   * (`"session.list"` als Beispiel). Es existiert unter `harw-ops` keine
   * Operation mit diesem oder einem verwandten Namen (Stand dieses
   * Knotens) — siehe Bericht.
   */
  sessionList: "session.list",
} as const;

/** Sucht die Sitzungslisten-Route in [`WEB_ROUTES`]. */
export function resolveSessionListRoute() {
  return findRoute(SESSION_OPERATIONS.sessionList);
}

/**
 * Ruft die Sitzungslisten-Route auf, falls deklariert.
 *
 * # Returns
 * `undefined`, wenn die Route (noch) nicht existiert — der Aufrufer zeigt
 * dann eine Lücken-Meldung statt einer erfundenen Liste. Sonst das
 * [`OperationResult`] von `callOperation`.
 */
export async function fetchSessionList(): Promise<OperationResult | undefined> {
  const route = resolveSessionListRoute();
  if (route === undefined) {
    return undefined;
  }
  return callOperation(route);
}
