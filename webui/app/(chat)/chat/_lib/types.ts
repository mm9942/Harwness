// Domänentypen für Master- und Session-Chat (Knoten UI-02).
//
// Bewusst schmal gehalten: `harw_operations::operation::OpOutput` ist nur
// `{ text: string }` (siehe DataBlock.tsx-Kopf, dort für UI-01 bereits
// gelesen) — der Server liefert keine strukturierten Chat-Nachrichten mit
// Rollen, IDs oder Zeitstempeln. Ein `ChatTurn` hier ist daher ein rein
// clientseitiges Konstrukt: ein Zug entsteht lokal, wenn der Nutzer sendet
// (`role: "user"`) und wenn die Sende-Operation synchron eine Antwort
// liefert (`role: "assistant"`, `text` = `OperationResult.text`). Es gibt
// keine serverseitige Nachrichten-ID — `id` wird lokal erzeugt und dient nur
// dazu, das Daumen-Runter-Signal (siehe `chatOperations.ts`) an genau diesen
// Zug zu binden.

/** Unterscheidet Master-Chat (kein Auftrag) von Session-Chat (an eine Sitzung gebunden). */
export type ChatScope =
  | { readonly kind: "master" }
  | { readonly kind: "session"; readonly sessionId: string };

/** Rolle eines Chat-Zugs. */
export type ChatTurnRole = "user" | "assistant";

/** Rückmeldung des Nutzers zu einem Chat-Zug — nie eine Gedächtnis-Schreibung selbst, nur ein Signal. */
export type ChatTurnFeedback = "negative" | null;

/** Ein einzelner, clientseitig verwalteter Chat-Zug. */
export interface ChatTurn {
  /** Lokal erzeugte, stabile Kennung — nicht vom Server vergeben. */
  readonly id: string;
  readonly role: ChatTurnRole;
  /** Roher, potenziell angreiferkontrollierter Text — wird nur über DataBlock angezeigt. */
  readonly text: string;
  /** `"negative"`, sobald der Nutzer für diesen (assistant-)Zug Daumen-Runter gegeben hat. */
  readonly feedback: ChatTurnFeedback;
}

/** Eine erkannte Lücke im Ereignisstrom, mit Zeitpunkt für die Anzeige. */
export interface ChatGapNotice {
  readonly id: string;
  readonly missingCount: number;
  readonly detectedAtIso: string;
}
