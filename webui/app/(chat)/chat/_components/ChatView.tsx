"use client";

// Gemeinsame Ansicht für Master- und Session-Chat (Knoten UI-02).
//
// # Warum eine Komponente für beide
// Master-Chat (kein Auftrag) und Session-Chat (an eine Sitzung gebunden)
// unterscheiden sich in der Autoritätslage — welche Operation aufgerufen
// wird und ob eine `sessionId` mitgeschickt wird — nicht aber in Aufbau
// oder Verhalten: beide zeigen einen Verlauf, ein Eingabefeld, eine
// Lücken-Anzeige und Daumen-Runter je Antwort. Zwei fast identische
// Komponenten nebeneinander wären hier teurer als eine mit einem
// `scope`-Parameter, der die Operationsnamen und den Sende-Rumpf bestimmt
// (siehe `_lib/chatOperations.ts`). Die Verzweigung ist an der Stelle
// abgebildet, an der die Autoritätslage tatsächlich verschieden ist — den
// Operationsnamen und dem Rumpf —, nicht in der Darstellung.
//
// # Live-Ereignisse
// `EventSource` ist nicht in jeder Laufzeitumgebung vorhanden (SSR, jsdom
// in Tests) — die Verbindung wird deshalb nur aufgebaut, wenn
// `typeof EventSource !== "undefined"`. Das ist zugleich der Grund, warum
// dieser Knoten in keinem Test eine echte Verbindung öffnet: Tests laufen
// unter jsdom ohne `EventSource`.
import { useEffect, useMemo, useState, type JSX } from "react";

import { connectWebEventStream, type WebEventStreamHandle } from "@/lib/sse";
import { DataBlock } from "@/components/ui/DataBlock";

import { ChatComposer } from "./ChatComposer";
import { ChatTurnList } from "./ChatTurnList";
import { GapBanner } from "./GapBanner";
import { RouteMissingNotice } from "./RouteMissingNotice";
import { appendGapNotice } from "../_lib/gapLog";
import { nextLocalId } from "../_lib/ids";
import {
  CHAT_OPERATIONS,
  fetchHistory,
  historyOperationName,
  reportNegativeFeedback,
  resolveHistoryRoute,
  resolveNegativeFeedbackRoute,
  resolveSendRoute,
  sendChatMessage,
  sendOperationName,
} from "../_lib/chatOperations";
import type { ChatGapNotice, ChatScope, ChatTurn } from "../_lib/types";

/** Fester SSE-Endpunkt von `harw-web` — kein Operationsname, siehe `sse.ts`-Kopf. */
const EVENTS_URL = "/events";

export interface ChatViewProps {
  readonly scope: ChatScope;
  readonly title: string;
}

/**
 * Trägt Verlaufsanzeige, Eingabe, Lücken-Anzeige und Daumen-Runter für
 * einen Chat-Bereich (Master oder Session).
 *
 * # Description
 * Lädt beim Mount den Verlauf über die passende History-Route (falls
 * deklariert), verbindet sich mit dem Ereignisstrom für Lücken-Erkennung,
 * und hängt jeden gesendeten/empfangenen Zug lokal an. Da `OpOutput` nur
 * `{ text: string }` trägt, wird die Sende-Antwort direkt als
 * Modell-Antwort-Zug übernommen (siehe `_lib/types.ts`-Kopf für die
 * Begründung dieser Annahme).
 *
 * # Arguments
 * - `scope` (`ChatScope`): Master oder eine konkrete Sitzung.
 * - `title` (`string`): vom Aufrufer fest codierte Überschrift.
 *
 * # Returns
 * Die vollständige Chat-Ansicht.
 */
export function ChatView({ scope, title }: ChatViewProps): JSX.Element {
  const [turns, setTurns] = useState<readonly ChatTurn[]>([]);
  const [gapNotices, setGapNotices] = useState<readonly ChatGapNotice[]>([]);
  const [historyText, setHistoryText] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);

  const historyRoute = useMemo(() => resolveHistoryRoute(scope), [scope]);
  const sendRoute = useMemo(() => resolveSendRoute(scope), [scope]);
  const feedbackRoute = useMemo(() => resolveNegativeFeedbackRoute(), []);

  // Verlauf einmalig laden, wenn eine Route existiert.
  useEffect(() => {
    let cancelled = false;
    if (historyRoute === undefined) {
      return () => {
        cancelled = true;
      };
    }
    fetchHistory(scope).then((result) => {
      if (cancelled || result === undefined) {
        return;
      }
      if (result.ok) {
        setHistoryText(result.text);
      } else {
        setSendError(result.error);
      }
    });
    return () => {
      cancelled = true;
    };
    // `scope` wechselt nur zwischen Master- und Session-Chat-Mounts, nicht
    // innerhalb einer laufenden Ansicht — bewusst nicht auf jede Änderung
    // von `historyRoute` erneut ausgeführt, um Doppel-Lädungen zu vermeiden.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scope]);

  // Ereignisstrom nur verbinden, wenn EventSource in dieser Umgebung existiert.
  useEffect(() => {
    if (typeof EventSource === "undefined") {
      return;
    }
    const expectedSendOp = sendOperationName(scope);
    const handle: WebEventStreamHandle = connectWebEventStream(EVENTS_URL, {
      onGap: (missingCount) => {
        setGapNotices((current) => appendGapNotice(current, missingCount));
      },
      onEvent: (event) => {
        if (event.kind.type === "operation_completed" && event.kind.operation === expectedSendOp) {
          // Nur ein Hinweis, kein automatisches Neuladen ohne Rückmeldung
          // an den Nutzer — ein Verlauf, der sich unbemerkt unter dem
          // Nutzer verändert, wäre dieselbe Art stiller Unvollständigkeit,
          // die die Lücken-Anzeige verhindern soll.
        }
      },
      onError: () => {
        // Verbindungsfehler werden nicht separat anders behandelt als eine
        // Lücke — ohne Sequenznummer lässt sich ihr Ausmaß nicht beziffern.
      },
    });
    return () => {
      handle.close();
    };
  }, [scope]);

  function handleSend(text: string): void {
    if (sendRoute === undefined) {
      return;
    }
    const userTurn: ChatTurn = { id: nextLocalId(), role: "user", text, feedback: null };
    setTurns((current) => [...current, userTurn]);
    setPending(true);
    setSendError(null);
    sendChatMessage(scope, text).then((result) => {
      setPending(false);
      if (result === undefined) {
        return;
      }
      if (result.ok) {
        const assistantTurn: ChatTurn = {
          id: nextLocalId(),
          role: "assistant",
          text: result.text,
          feedback: null,
        };
        setTurns((current) => [...current, assistantTurn]);
      } else {
        setSendError(result.error);
      }
    });
  }

  function handleNegativeFeedback(turn: ChatTurn): void {
    if (feedbackRoute === undefined) {
      return;
    }
    reportNegativeFeedback(scope, turn.id, turn.text).then((result) => {
      if (result?.ok) {
        setTurns((current) =>
          current.map((candidate) =>
            candidate.id === turn.id ? { ...candidate, feedback: "negative" } : candidate,
          ),
        );
      }
    });
  }

  return (
    <section className="harw-chat-view" aria-label={title}>
      <h1>{title}</h1>
      <GapBanner notices={gapNotices} />
      {historyRoute === undefined ? (
        <RouteMissingNotice operation={historyOperationName(scope)} />
      ) : historyText !== null ? (
        <div className="harw-chat-history" data-testid="chat-history">
          <DataBlock label="Bisheriger Verlauf" value={historyText} />
        </div>
      ) : null}
      <ChatTurnList
        turns={turns}
        onNegativeFeedback={handleNegativeFeedback}
        feedbackRouteMissing={feedbackRoute === undefined}
      />
      {feedbackRoute === undefined && turns.some((turn) => turn.role === "assistant") ? (
        <RouteMissingNotice operation={CHAT_OPERATIONS.negativeFeedbackSignal} />
      ) : null}
      {sendError !== null ? (
        <div role="alert" className="harw-chat-send-error" data-testid="chat-send-error">
          <DataBlock label="Fehler" value={sendError} />
        </div>
      ) : null}
      {sendRoute === undefined ? (
        <RouteMissingNotice operation={sendOperationName(scope)} />
      ) : null}
      <ChatComposer onSubmit={handleSend} disabled={sendRoute === undefined} pending={pending} />
    </section>
  );
}
