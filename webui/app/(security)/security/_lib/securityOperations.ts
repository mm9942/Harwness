// Operationsanbindung der Sicherheitszentrale (Knoten UI-05).
//
// # Auflage
// Jeder Aufruf geht ausschließlich über `findRoute`/`callOperation` aus
// `webClient.ts` — es gibt in dieser Datei keine Funktion, die einen rohen
// Pfad-String entgegennimmt (siehe `_lib/types.ts`-Kopf und
// `webClient.ts`-Kopf für dieselbe Auflage). Die hier genannten
// Operationsnamen (`SECURITY_OPERATIONS.*`) sind **erwartete**, keine
// garantierten Namen: `WEB_ROUTES` ist zum Zeitpunkt dieses Knotens leer
// (`lib/generated/operations.ts` enthält `[] as const`) — für Sicherheit so
// wenig wie für alles andere. `resolveRoute` liefert deshalb für jede
// dieser Operationen aktuell `undefined`; jede Abrufunktion gibt in diesem
// Fall `{ status: "route-missing", operation }` zurück statt eine Route zu
// erfinden — das ist der zweite Autoritätspfad, den UI-00 ausdrücklich
// verhindert (siehe Auftrag, Abschnitt „Was zu bauen ist").
//
// Für das Warden-Audit gibt es überhaupt keinen erwarteten Operationsnamen:
// das Protokoll lebt ausschließlich im Prozessspeicher des Warden (siehe
// `_lib/types.ts`, `WardenAuditEvent`-Kopf) — es existiert (Stand des
// gelesenen Quelltexts) keine plausible künftige HTTP-Operation, die
// geraten werden könnte, ohne sich eine auszudenken.
import { callOperation, findRoute, type WebRouteDescriptor } from "@/lib/webClient";

import type {
  Finding,
  FreezeRecord,
  HostSample,
  SecurityEvent,
  SecurityFetchResult,
  SecurityVerdict,
} from "./types";

/** Erwartete, aber (Stand dieses Knotens) nicht deklarierte Operationsnamen. */
export const SECURITY_OPERATIONS = {
  findingsList: "security.findings.list",
  hostSamplesList: "security.samples.list",
  eventsList: "security.events.list",
  verdictsList: "security.verdicts.list",
  freezeList: "security.freeze.list",
} as const;

/** Sucht eine Sicherheits-Route in `WEB_ROUTES` über ihren Operationsnamen. */
function resolveRoute(operation: string): WebRouteDescriptor | undefined {
  return findRoute(operation);
}

/**
 * Ruft eine deklarierte Sicherheits-Operation auf und bildet das Ergebnis
 * auf [`SecurityFetchResult`] ab.
 *
 * # Description
 * Reine Verzweigung ohne eigene Fehlerinterpretation: eine fehlende Route
 * wird nie stillschweigend als „keine Befunde" behandelt (siehe
 * `_lib/types.ts`, `SecurityFetchResult`-Kopf für die Begründung dieser
 * Drei-Wege-Unterscheidung).
 *
 * # Arguments
 * - `operation` (`string`): der erwartete Operationsname aus
 *   [`SECURITY_OPERATIONS`].
 * - `parseItems` (`(text: string) => readonly T[]`): parst den `text`-Rumpf
 *   einer erfolgreichen Antwort in eine Liste. Wirft diese Funktion, wird
 *   der Aufruf als `error` gemeldet — kein Absturz der Ansicht.
 *
 * # Returns
 * Ein [`SecurityFetchResult<T>`] — niemals eine erfundene leere Liste für
 * eine fehlende Route.
 */
async function fetchSecurityList<T>(
  operation: string,
  parseItems: (text: string) => readonly T[],
): Promise<SecurityFetchResult<T>> {
  const route = resolveRoute(operation);
  if (route === undefined) {
    return { status: "route-missing", operation };
  }
  const result = await callOperation(route);
  if (!result.ok) {
    return { status: "error", operation, message: result.error };
  }
  try {
    return { status: "ok", items: parseItems(result.text) };
  } catch (error) {
    return {
      status: "error",
      operation,
      message: error instanceof Error ? error.message : "Antwort ließ sich nicht lesen",
    };
  }
}

/** Ruft die Befundliste ab, falls `SECURITY_OPERATIONS.findingsList` deklariert ist. */
export function fetchFindings(): Promise<SecurityFetchResult<Finding>> {
  return fetchSecurityList<Finding>(SECURITY_OPERATIONS.findingsList, (text) =>
    JSON.parse(text) as readonly Finding[],
  );
}

/** Ruft die Hostmesswerte ab, falls `SECURITY_OPERATIONS.hostSamplesList` deklariert ist. */
export function fetchHostSamples(): Promise<SecurityFetchResult<HostSample>> {
  return fetchSecurityList<HostSample>(SECURITY_OPERATIONS.hostSamplesList, (text) =>
    JSON.parse(text) as readonly HostSample[],
  );
}

/** Ruft die Sicherheitsereignisse ab, falls `SECURITY_OPERATIONS.eventsList` deklariert ist. */
export function fetchSecurityEvents(): Promise<SecurityFetchResult<SecurityEvent>> {
  return fetchSecurityList<SecurityEvent>(SECURITY_OPERATIONS.eventsList, (text) =>
    JSON.parse(text) as readonly SecurityEvent[],
  );
}

/** Ruft die Agenten-Urteile ab, falls `SECURITY_OPERATIONS.verdictsList` deklariert ist. */
export function fetchSecurityVerdicts(): Promise<SecurityFetchResult<SecurityVerdict>> {
  return fetchSecurityList<SecurityVerdict>(SECURITY_OPERATIONS.verdictsList, (text) =>
    JSON.parse(text) as readonly SecurityVerdict[],
  );
}

/** Ruft die Freeze-Datensätze ab, falls `SECURITY_OPERATIONS.freezeList` deklariert ist. */
export function fetchFreezeRecords(): Promise<SecurityFetchResult<FreezeRecord>> {
  return fetchSecurityList<FreezeRecord>(SECURITY_OPERATIONS.freezeList, (text) =>
    JSON.parse(text) as readonly FreezeRecord[],
  );
}
