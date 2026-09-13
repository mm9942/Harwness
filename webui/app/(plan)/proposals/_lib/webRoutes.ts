// Operationsanbindung für die Vorschlagswarteschlange (Knoten UI-03).
//
// # Auflage
// Ausschließlich `findRoute`/`callOperation` — kein roher Pfad-String.
//
// # Geprüft, nicht erfunden — die zentrale Meldung dieses Unterknotens
// - `/context-proposal` (`harw-ops::context_proposal`) ist explizit
//   `Surface::Command` (`command(path = "/context-proposal", visibility =
//   "channel_parity")`) — **kein** `Surface::Web`. Über HTTP ist die
//   Operation damit nicht erreichbar, unabhängig davon, dass `WEB_ROUTES`
//   ohnehin leer ist. `findRoute("context-proposal")` liefert `undefined`,
//   und wird es auch nach jeder reinen `WEB_ROUTES`-Befüllung weiterhin
//   tun, solange niemand diese Operation explizit um `Surface::Web`
//   erweitert — das ist eine bewusste Entscheidung der Datei
//   `context_proposal.rs`, kein Versehen dieses Knotens.
// - Für `ModelBehaviorProposal` (AW6-06) existiert unter `harw-ops`
//   **überhaupt keine** Operation — nur der Datentyp in
//   `harw-knowledge/src/model_behavior_proposal.rs`. Der hier verwendete
//   Name ist rein spekulativ, analog zu `"context-proposal"` benannt.
import { callOperation, findRoute, type OperationResult } from "@/lib/webClient";

/** Erwartete Operationsnamen für beide Vorschlagsarten. */
export const PROPOSAL_OPERATIONS = {
  /** Existiert als Operation, aber nur `Surface::Command` — siehe Dateikopf. */
  contextProposalList: "context-proposal",
  /** Existiert unter `harw-ops` überhaupt nicht — rein spekulativer Name. */
  modelBehaviorProposalList: "model-behavior-proposal",
} as const;

/** Sucht die `ContextProposal`-Listen-Route in [`WEB_ROUTES`]. */
export function resolveContextProposalListRoute() {
  return findRoute(PROPOSAL_OPERATIONS.contextProposalList);
}

/** Sucht die `ModelBehaviorProposal`-Listen-Route in [`WEB_ROUTES`]. */
export function resolveModelBehaviorProposalListRoute() {
  return findRoute(PROPOSAL_OPERATIONS.modelBehaviorProposalList);
}

/**
 * Ruft die `ContextProposal`-Liste auf (`list`), falls deklariert.
 *
 * # Returns
 * `undefined`, wenn die Route (noch) nicht existiert — was für diese
 * Operation zum Zeitpunkt dieses Knotens strukturell zutrifft (`Surface::
 * Command`, nicht `Surface::Web`).
 */
export async function fetchContextProposalList(): Promise<OperationResult | undefined> {
  const route = resolveContextProposalListRoute();
  if (route === undefined) {
    return undefined;
  }
  return callOperation(route, { sub: "list" });
}

/**
 * Ruft die `ModelBehaviorProposal`-Liste auf, falls deklariert.
 *
 * # Returns
 * `undefined`, wenn die Route (noch) nicht existiert — was zum Zeitpunkt
 * dieses Knotens immer zutrifft, da die Operation überhaupt nicht existiert.
 */
export async function fetchModelBehaviorProposalList(): Promise<OperationResult | undefined> {
  const route = resolveModelBehaviorProposalListRoute();
  if (route === undefined) {
    return undefined;
  }
  return callOperation(route, { sub: "list" });
}
