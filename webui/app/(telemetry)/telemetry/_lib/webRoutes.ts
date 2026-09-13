// Operationsanbindung der Telemetrieansicht (Knoten UI-04).
//
// # Auflage
// Jeder Aufruf geht ausschließlich über [`findRoute`]/[`callOperation`] aus
// `@/lib/webClient` — diese Datei ruft an keiner Stelle einen rohen
// Pfad-String auf.
//
// # Befund: der Telemetrie-Teilbaum ist ohne Web-Fläche gebaut worden
// `WEB_ROUTES` ist zum Zeitpunkt dieses Knotens leer (`lib/generated/
// operations.ts` → `[] as const`) — es gibt noch keine einzige deklarierte
// Route, für Telemetrie so wenig wie für alles andere (derselbe Befund wie
// UI-03 für Sitzungen, siehe `(sessions)/_lib/webRoutes.ts`). Zusätzlich
// wurde geprüft: `harw-operations` (siehe `harw-operations/src/*.rs`,
// `grep -rl "harw_observe\|NullCounterRegistry" harw-operations`) enthält
// **keine** Operation, die `harw-observe` referenziert — es gibt also nicht
// nur keine Route, sondern auch keine registrierte Operation, die eine
// Route je bekommen könnte. Die unten benannten Operationsnamen sind
// **erwartete** Namen nach der `OperationMeta::name`-Konvention
// (`"session.list"`-Beispiel in `@/lib/webClient`), keine garantierten.
import { callOperation, findRoute, type OperationResult } from "@/lib/webClient";

/** Erwartete, aber (Stand dieses Knotens) nicht deklarierte Operationsnamen. */
export const TELEMETRY_OPERATIONS = {
  /**
   * Erwarteter Name für einen Nullzähler-Schnappschuss
   * (`NullCounterRegistry::snapshot`, `harw-observe/src/null_counter.rs`).
   * Es existiert unter `harw-operations`/`harw-ops` keine Operation mit
   * diesem oder einem verwandten Namen (Stand dieses Knotens).
   */
  nullCounterSnapshot: "telemetry.null_counters.snapshot",
  /**
   * Erwarteter Name für einen Metrik-Schnappschuss
   * (`MetricKey`/`MetricValue`, `harw-observe/src/metric.rs`). Ebenfalls
   * ohne Entsprechung unter `harw-operations`/`harw-ops`.
   */
  metricsSnapshot: "telemetry.metrics.snapshot",
} as const;

/** Sucht die Nullzähler-Schnappschuss-Route in [`WEB_ROUTES`]. */
export function resolveNullCounterSnapshotRoute() {
  return findRoute(TELEMETRY_OPERATIONS.nullCounterSnapshot);
}

/** Sucht die Metrik-Schnappschuss-Route in [`WEB_ROUTES`]. */
export function resolveMetricsSnapshotRoute() {
  return findRoute(TELEMETRY_OPERATIONS.metricsSnapshot);
}

/**
 * Ruft die Nullzähler-Schnappschuss-Route auf, falls deklariert.
 *
 * # Returns
 * `undefined`, wenn die Route (noch) nicht existiert — der Aufrufer zeigt
 * dann eine Lücken-Meldung statt eines erfundenen Schnappschusses. Sonst
 * das [`OperationResult`] von `callOperation` (Rohtext, siehe
 * `OpOutput`-Vertrag in `@/lib/webClient`-Kopf — es gibt kein
 * strukturiertes JSON-Antwortschema).
 */
export async function fetchNullCounterSnapshot(): Promise<OperationResult | undefined> {
  const route = resolveNullCounterSnapshotRoute();
  if (route === undefined) {
    return undefined;
  }
  return callOperation(route);
}

/**
 * Ruft die Metrik-Schnappschuss-Route auf, falls deklariert.
 *
 * # Returns
 * `undefined`, wenn die Route (noch) nicht existiert. Sonst das
 * [`OperationResult`] von `callOperation`.
 */
export async function fetchMetricsSnapshot(): Promise<OperationResult | undefined> {
  const route = resolveMetricsSnapshotRoute();
  if (route === undefined) {
    return undefined;
  }
  return callOperation(route);
}
