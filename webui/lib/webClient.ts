// Einziger Datenzugriffspfad der Control-Plane-Schale — kein zweiter
// Autoritätspfad.
//
// # Auflage
// Die UI ruft ausschließlich Routen auf, die `harw-web` aus der Registry
// gebaut hat (siehe `harw_web::router::WebRouteTable::from_registry`-
// Moduldoku: „es gibt keine Methode, die einen rohen Pfad-String … entgegen-
// nimmt"). Diese Datei ist die eine Stelle in `webui/`, die `fetch` gegen
// einen Operationspfad aufruft, und sie tut das ausschließlich über
// [`WEB_ROUTES`] — die aus dem Rust-Quelltext erzeugte Liste (siehe
// `lib/generated/operations.ts`). Es gibt hier keine Funktion, die einen
// rohen Pfad-String entgegennimmt: [`callOperation`] verlangt einen
// `WebRouteDescriptor`-Wert aus `WEB_ROUTES`, kein `string` — ein Aufruf
// eines nicht deklarierten Pfads ist damit ein Typfehler, kein Laufzeitpfad.
import { WEB_ROUTES, type WebRouteDescriptor } from "./generated/operations";

export { WEB_ROUTES };
export type { WebRouteDescriptor };

/** Ergebnis eines Operationsaufrufs — Spiegelbild von `harw_web::server::op_result_response`. */
export type OperationResult =
  | { readonly ok: true; readonly text: string }
  | { readonly ok: false; readonly status: number; readonly error: string };

/**
 * Findet eine Route in [`WEB_ROUTES`] über ihren `operation`-Namen.
 *
 * # Description
 * Der bevorzugte Weg, eine Route zu referenzieren: über den stabilen,
 * maschinenlesbaren Operationsnamen statt über den HTTP-Pfad direkt —
 * derselbe Name, den `harw_operations::adapter::WebAdapter::operation_name`
 * liefert.
 *
 * # Arguments
 * - `operation` (`string`): `OperationMeta::name`, z. B. `"session.list"`.
 *
 * # Returns
 * Die passende Route, oder `undefined`, wenn kein `Surface::Web` diesen
 * Operationsnamen deklariert.
 */
export function findRoute(operation: string): WebRouteDescriptor | undefined {
  return WEB_ROUTES.find((route) => route.operation === operation);
}

/**
 * Ruft eine deklarierte `harw-web`-Route auf.
 *
 * # Description
 * Baut die HTTP-Methode aus `route.method` (siehe `WebMethod::expected_for`
 * in `harw_web::router`) und sendet `body` nur bei `POST` — bei `GET` bleibt
 * der Rumpf leer, exakt wie `harw_web::server::read_json_body` es für einen
 * leeren `GET`-Rumpf erwartet (`Value::Null`).
 *
 * # Arguments
 * - `route` (`WebRouteDescriptor`): eine Route aus [`WEB_ROUTES`] — niemals
 *   ein roher Pfad-String.
 * - `body` (`unknown`, optional): JSON-Argumente für eine `POST`-Route.
 *
 * # Returns
 * Ein [`OperationResult`]: `{ ok: true, text }` bei Erfolg,
 * `{ ok: false, status, error }` bei einer Fehlerantwort — niemals ein
 * geworfener Fehler für eine reguläre HTTP-Fehlerantwort (nur ein
 * Netzwerk-/Parsing-Fehler wird geworfen).
 *
 * # Errors
 * Wirft, wenn `fetch` selbst fehlschlägt (Netzwerk) oder die Antwort kein
 * gültiges JSON ist — beides Fälle, die keine sinnvolle
 * `OperationResult`-Form haben.
 */
export async function callOperation(
  route: WebRouteDescriptor,
  body?: unknown,
): Promise<OperationResult> {
  const init: RequestInit =
    route.method === "POST"
      ? {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify(body ?? null),
        }
      : { method: "GET" };

  const response = await fetch(route.path, init);
  const payload = (await response.json()) as { text?: string; error?: string };

  if (response.ok && typeof payload.text === "string") {
    return { ok: true, text: payload.text };
  }
  return {
    ok: false,
    status: response.status,
    error: payload.error ?? `unerwartete Antwort (Status ${response.status})`,
  };
}
