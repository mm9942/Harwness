// Operationsanbindung der Bestätigungsfläche (Knoten UI-06).
//
// # Auflage
// Jeder Aufruf geht ausschließlich über `findRoute`/`callOperation` aus
// `@/lib/webClient` — es gibt in dieser Datei keine Funktion, die einen
// rohen Pfad-String entgegennimmt (dasselbe Muster wie
// `_lib/securityOperations.ts`, Knoten UI-05). `APPROVAL_OPERATIONS.*` sind
// **erwartete**, keine garantierten Namen: `WEB_ROUTES` ist zum Zeitpunkt
// dieses Knotens leer (`lib/generated/operations.ts` → `[] as const`) —
// jeder Aufruf meldet deshalb aktuell `{ status: "route-missing" }` statt
// eine Route zu erfinden.
//
// # Warum es hier keine `fetchPendingApproval`-Funktion gibt
// Eine anstehende Anfrage über HTTP nachzuschlagen bräuchte eine Route, die
// nach `session`/`request` parametrisiert ist. Das aktuelle Modell
// (`harw_web::router::WebMethod::expected_for`: `GET` ⇒ kein Rumpf, fester
// Pfad je Operation) hat dafür keinen Mechanismus — `callOperation` sendet
// bei `GET` nie einen Rumpf, und `findRoute` löst nur nach Operationsname
// auf, nicht nach Pfadparametern. Diese Lücke ist im Abschlussbericht
// dieses Knotens gemeldet, statt sie hier mit einer erfundenen
// Query-String-Konvention zu überbrücken. `ApprovalRequestPanel` nimmt den
// Anzeigeinhalt deshalb als Prop entgegen (siehe dortiger Kopf), statt ihn
// selbst zu laden.
//
// # Der Actor geht nie im Anfragerumpf mit
// `submitApprovalDecision` sendet bewusst **keinen** `actor` im
// POST-Rumpf — `harw-web` leitet ihn serverseitig aus den
// Peer-Credentials der Verbindung ab (siehe `harw-web/src/security.rs`,
// `ApprovalActorResolver`). Ein `actor`-Feld hier wäre wertlos: der
// Absender könnte es sich selbst aussuchen.
import { callOperation, findRoute, type WebRouteDescriptor } from "@/lib/webClient";

import type {
  ApprovalResolutionRecord,
  ApprovalSubmitResult,
  ReviewDecision,
} from "./approvalTypes";

/** Erwartete, aber (Stand dieses Knotens) nicht deklarierte Operationsnamen. */
export const APPROVAL_OPERATIONS = {
  /**
   * Erwarteter Name für die Auflösung einer Genehmigungsanfrage — siehe
   * `harw-web/src/security.rs`-Kopf, Abschnitt „Warum dieses Modul (noch)
   * nicht erreichbar ist". Es existiert unter `harw-ops` (Stand dieses
   * Knotens) keine Operation mit diesem oder einem verwandten Namen.
   */
  resolve: "approval.resolve",
} as const;

/** Sucht die Auflösungs-Route in `WEB_ROUTES`. */
function resolveRoute(operation: string): WebRouteDescriptor | undefined {
  return findRoute(operation);
}

/**
 * Löst eine anstehende Genehmigungsanfrage über `harw-web` auf.
 *
 * # Description
 * Sendet ausschließlich `session`, `request`, `decision` und `comment` —
 * niemals einen `actor` (siehe Dateikopf). Eine fehlende Route wird nie
 * stillschweigend als Erfolg oder als „keine Anfrage" behandelt.
 *
 * # Arguments
 * - `session` (`string`): die betroffene Sitzung.
 * - `request` (`string`): die Kennung der Genehmigungsanfrage.
 * - `decision` (`ReviewDecision`): die menschliche Entscheidung.
 * - `comment` (`string | null`): optionaler Freitextkommentar.
 *
 * # Returns
 * Ein [`ApprovalSubmitResult`] — `route-missing`, solange
 * `APPROVAL_OPERATIONS.resolve` nicht in `WEB_ROUTES` deklariert ist.
 */
export async function submitApprovalDecision(
  session: string,
  request: string,
  decision: ReviewDecision,
  comment: string | null,
): Promise<ApprovalSubmitResult> {
  const route = resolveRoute(APPROVAL_OPERATIONS.resolve);
  if (route === undefined) {
    return { status: "route-missing", operation: APPROVAL_OPERATIONS.resolve };
  }
  const result = await callOperation(route, { session, request, decision, comment });
  if (!result.ok) {
    return { status: "error", operation: APPROVAL_OPERATIONS.resolve, message: result.error };
  }
  try {
    return {
      status: "ok",
      resolution: JSON.parse(result.text) as ApprovalResolutionRecord,
    };
  } catch (error) {
    return {
      status: "error",
      operation: APPROVAL_OPERATIONS.resolve,
      message: error instanceof Error ? error.message : "Antwort ließ sich nicht lesen",
    };
  }
}
