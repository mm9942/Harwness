// Operationsanbindung für Master- und Session-Chat (Knoten UI-02).
//
// # Auflage
// Jeder Aufruf geht ausschließlich über [`findRoute`]/[`callOperation`] aus
// `webClient.ts` — es gibt in dieser Datei keine Funktion, die einen rohen
// Pfad-String entgegennimmt. Die hier genannten Operationsnamen
// (`CHAT_OPERATIONS.*`) sind **erwartete** Namen, keine garantierten: zum
// Zeitpunkt dieses Knotens ist `WEB_ROUTES` leer (`lib/generated/
// operations.ts` enthält `[] as const`) — es gibt noch KEINE einzige
// deklarierte Route, für Chat so wenig wie für alles andere. `resolveRoute`
// liefert deshalb für jede dieser Operationen aktuell `undefined`; die UI
// zeigt in diesem Fall eine Lücken-Meldung mit dem Operationsnamen statt
// eine Route zu erfinden (siehe Abschlussbericht dieses Knotens für die
// vollständige Liste als Meldung an den Registry-Eigentümer).
import { callOperation, findRoute, type OperationResult } from "@/lib/webClient";

import type { ChatScope } from "./types";

/** Erwartete, aber (Stand dieses Knotens) nicht deklarierte Operationsnamen. */
export const CHAT_OPERATIONS = {
  masterHistory: "chat.master.history",
  masterSend: "chat.master.send",
  sessionHistory: "chat.session.history",
  sessionSend: "chat.session.send",
  /**
   * Erwarteter Name für das Daumen-Runter-Signal. Schreibt laut Auftrag
   * **nichts** ins Gedächtnis — erzeugt nur einen Vorschlag, der an anderer
   * Stelle bewertet wird (dieselbe Doktrin wie `ContextProposal` /
   * `ModelBehaviorProposal`). Der Name ist an diese Doktrin angelehnt,
   * aber nicht verifiziert — siehe Abschlussbericht.
   */
  negativeFeedbackSignal: "chat.feedback.negative_signal",
} as const;

/** Wählt den History-Operationsnamen für den gegebenen Chat-Bereich. */
export function historyOperationName(scope: ChatScope): string {
  return scope.kind === "master" ? CHAT_OPERATIONS.masterHistory : CHAT_OPERATIONS.sessionHistory;
}

/** Wählt den Sende-Operationsnamen für den gegebenen Chat-Bereich. */
export function sendOperationName(scope: ChatScope): string {
  return scope.kind === "master" ? CHAT_OPERATIONS.masterSend : CHAT_OPERATIONS.sessionSend;
}

/**
 * Sucht die History-Route für den gegebenen Bereich in [`WEB_ROUTES`].
 *
 * # Returns
 * Die Route, oder `undefined`, wenn `harw-web` diese Operation (noch)
 * nicht deklariert.
 */
export function resolveHistoryRoute(scope: ChatScope) {
  return findRoute(historyOperationName(scope));
}

/** Sucht die Sende-Route für den gegebenen Bereich in [`WEB_ROUTES`]. */
export function resolveSendRoute(scope: ChatScope) {
  return findRoute(sendOperationName(scope));
}

/** Sucht die Daumen-Runter-Signal-Route in [`WEB_ROUTES`]. */
export function resolveNegativeFeedbackRoute() {
  return findRoute(CHAT_OPERATIONS.negativeFeedbackSignal);
}

/** Baut den JSON-Rumpf für eine Sende-Operation — reine Funktion, ohne Netzwerk. */
export function buildSendBody(scope: ChatScope, text: string): unknown {
  return scope.kind === "master"
    ? { text }
    : { sessionId: scope.sessionId, text };
}

/** Baut den JSON-Rumpf für das Daumen-Runter-Signal — reine Funktion, ohne Netzwerk. */
export function buildNegativeFeedbackBody(scope: ChatScope, turnId: string, text: string): unknown {
  const base = { turnId, text, reason: "negative" as const };
  return scope.kind === "master" ? base : { ...base, sessionId: scope.sessionId };
}

/**
 * Ruft die History-Route für den Bereich auf, falls deklariert.
 *
 * # Returns
 * `undefined`, wenn die Route (noch) nicht existiert — der Aufrufer
 * entscheidet dann, eine Lücken-Meldung statt eines erfundenen Verlaufs zu
 * zeigen. Sonst das [`OperationResult`] von `callOperation`.
 */
export async function fetchHistory(scope: ChatScope): Promise<OperationResult | undefined> {
  const route = resolveHistoryRoute(scope);
  if (route === undefined) {
    return undefined;
  }
  return callOperation(route, scope.kind === "session" ? { sessionId: scope.sessionId } : undefined);
}

/**
 * Sendet eine Chat-Nachricht über die deklarierte Sende-Route, falls
 * vorhanden.
 *
 * # Returns
 * `undefined`, wenn die Route (noch) nicht existiert. Sonst das
 * [`OperationResult`] — bei Erfolg trägt `text` die (synchrone)
 * Modellantwort, siehe `_lib/types.ts` für die Begründung dieser Annahme.
 */
export async function sendChatMessage(scope: ChatScope, text: string): Promise<OperationResult | undefined> {
  const route = resolveSendRoute(scope);
  if (route === undefined) {
    return undefined;
  }
  return callOperation(route, buildSendBody(scope, text));
}

/**
 * Meldet ein Daumen-Runter als Signal, falls die Route existiert.
 *
 * # Description
 * Schreibt laut Auftrag ausdrücklich **nichts** ins Gedächtnis — die
 * Bewertung, ob daraus ein `ModelBehaviorProposal`/`ContextProposal`
 * entsteht, findet server-/gedächtnisseitig statt, nie hier.
 *
 * # Returns
 * `undefined`, wenn die Route (noch) nicht existiert.
 */
export async function reportNegativeFeedback(
  scope: ChatScope,
  turnId: string,
  turnText: string,
): Promise<OperationResult | undefined> {
  const route = resolveNegativeFeedbackRoute();
  if (route === undefined) {
    return undefined;
  }
  return callOperation(route, buildNegativeFeedbackBody(scope, turnId, turnText));
}
