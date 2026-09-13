// Sitzungsliste — rein lesende Ansicht (Knoten UI-03).
//
// Ruft ausschließlich `fetchSessionList` aus `_lib/webRoutes.ts` auf, die
// ihrerseits nur über `findRoute`/`callOperation` geht. Die Antwort einer
// Operation ist laut `OpOutput`/`OperationResult` ein einziger, freier
// Text-Block (`{ text: string }`) — es gibt in `harw-web`/`harw-operations`
// kein strukturiertes JSON-Schema für eine Sitzungsliste (siehe
// Abschlussbericht). Diese Ansicht zerlegt den Text daher zeilenweise und
// zeigt jede Zeile über [`DataBlockList`] an, statt eine Struktur zu
// erfinden, die es nicht gibt.
"use client";

import { useEffect, useState } from "react";
import type { JSX } from "react";

import { DataBlock, DataBlockList } from "@/components/ui/DataBlock";
import { connectWebEventStream } from "@/lib/sse";

import { appendGapNotice } from "../_lib/eventGap";
import type { GapNotice } from "../_lib/types";
import { SESSION_OPERATIONS, fetchSessionList, resolveSessionListRoute } from "../_lib/webRoutes";
import { GapBanner } from "./GapBanner";
import { RouteMissingNotice } from "./RouteMissingNotice";

/** Fester Ereignisstrom-Endpunkt aus `lib/sse.ts` — keine Operationsroute, siehe dessen Moduldoku. */
const EVENTS_URL = "/events";

type LoadState =
  | { readonly kind: "idle" }
  | { readonly kind: "loading" }
  | { readonly kind: "loaded"; readonly lines: readonly string[] }
  | { readonly kind: "error"; readonly message: string };

/**
 * Lädt und zeigt die Sitzungsliste; abonniert zusätzlich den Ereignisstrom,
 * um Lücken sichtbar zu machen.
 *
 * # Description
 * Führt keine Aktion aus, die etwas verändert — reine Anzeige. Wenn
 * `session.list` nicht deklariert ist, zeigt diese Komponente
 * [`RouteMissingNotice`] statt einer erfundenen Liste.
 *
 * # Returns
 * Das gerenderte Sitzungslisten-Fragment.
 */
export function SessionList(): JSX.Element {
  const [state, setState] = useState<LoadState>({ kind: "idle" });
  const [gaps, setGaps] = useState<readonly GapNotice[]>([]);
  const route = resolveSessionListRoute();

  useEffect(() => {
    if (route === undefined) {
      return;
    }
    let cancelled = false;
    setState({ kind: "loading" });
    fetchSessionList()
      .then((result) => {
        if (cancelled || result === undefined) {
          return;
        }
        if (result.ok) {
          setState({ kind: "loaded", lines: result.text.split("\n").filter((line) => line.length > 0) });
        } else {
          setState({ kind: "error", message: result.error });
        }
      })
      .catch((error: unknown) => {
        if (!cancelled) {
          setState({ kind: "error", message: error instanceof Error ? error.message : String(error) });
        }
      });
    return () => {
      cancelled = true;
    };
    // `route` wird nur zur Existenzprüfung gelesen; ihr Inhalt ändert sich
    // nicht zur Laufzeit einer Sitzung dieser Ansicht.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [route === undefined]);

  useEffect(() => {
    const handle = connectWebEventStream(EVENTS_URL, {
      onGap: (missingCount) => {
        setGaps((previous) => appendGapNotice(previous, missingCount));
      },
    });
    return () => handle.close();
  }, []);

  return (
    <section className="harw-session-list" aria-label="Sitzungsliste">
      <h2>Sitzungen</h2>
      <GapBanner notices={gaps} />
      {route === undefined ? (
        <RouteMissingNotice
          operation={SESSION_OPERATIONS.sessionList}
          detail={'Es gibt unter harw-ops derzeit keine Operation für Sitzungen; die nächstliegende bestehende Operation ist „ps" (Jobs, keine Sitzungen).'}
        />
      ) : null}
      {state.kind === "loading" ? <DataBlock label="Status" value="Lade Sitzungsliste…" /> : null}
      {state.kind === "error" ? <DataBlock label="Fehler" value={state.message} /> : null}
      {state.kind === "loaded" ? <DataBlockList label="Sitzungen" values={state.lines} /> : null}
    </section>
  );
}
