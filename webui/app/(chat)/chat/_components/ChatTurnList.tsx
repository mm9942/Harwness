// Rendert die Chat-Züge einer Sitzung (Knoten UI-02).
//
// Jeder Zug — egal ob vom Nutzer eingegeben oder vom Modell zurückgegeben —
// ist potenziell angreiferkontrollierter Text und geht ausschließlich durch
// [`DataBlock`]. Ein Modell-Zug `<img src=x onerror=...>` oder ein
// Markdown-Link `[klick](javascript:...)` erscheint dadurch als Text, nie
// als Markup (siehe Auftrag; siehe `__tests__/ChatTurnList.test.tsx`).
import type { JSX } from "react";

import { DataBlock } from "@/components/ui/DataBlock";

import type { ChatTurn } from "../_lib/types";

export interface ChatTurnListProps {
  readonly turns: readonly ChatTurn[];
  /**
   * Aufgerufen, wenn der Nutzer für einen assistant-Zug Daumen-Runter
   * gibt. `undefined`, solange keine Signal-Route deklariert ist — die
   * Schaltfläche wird dann als Lücke gemeldet, nicht ausgeblendet.
   */
  readonly onNegativeFeedback?: (turn: ChatTurn) => void;
  /** `true`, solange keine Route für das Daumen-Runter-Signal existiert. */
  readonly feedbackRouteMissing: boolean;
}

/**
 * Rendert die Liste der Chat-Züge in chronologischer Reihenfolge.
 *
 * # Returns
 * Eine geordnete Liste; jeder Zug trägt seine Rolle als Beschriftung und
 * seinen Text ausschließlich über [`DataBlock`].
 */
export function ChatTurnList({
  turns,
  onNegativeFeedback,
  feedbackRouteMissing,
}: ChatTurnListProps): JSX.Element {
  return (
    <ol className="harw-chat-turn-list" data-testid="chat-turn-list">
      {turns.map((turn) => (
        <li key={turn.id} className={`harw-chat-turn harw-chat-turn-${turn.role}`}>
          <DataBlock
            label={turn.role === "user" ? "Du" : "Harwness"}
            value={turn.text}
          />
          {turn.role === "assistant" ? (
            <button
              type="button"
              aria-label="Antwort als negativ bewerten"
              aria-pressed={turn.feedback === "negative"}
              disabled={turn.feedback === "negative" || feedbackRouteMissing}
              onClick={() => onNegativeFeedback?.(turn)}
              data-testid={`thumbs-down-${turn.id}`}
            >
              {turn.feedback === "negative" ? "Signal gemeldet" : "Daumen runter"}
            </button>
          ) : null}
        </li>
      ))}
    </ol>
  );
}
