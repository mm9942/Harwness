// Telemetrieansicht — komponiert Nullzähler-Leiste, Metriktabelle und
// Ereignis-Feed (Knoten UI-04).
//
// Rein lesend, wie die übrigen Ansichten dieser Schale. Lädt zwei
// unabhängige Schnappschüsse (Nullzähler, Metriken) über `_lib/webRoutes.ts`
// und abonniert zusätzlich `GET /events` für Aktivität und Lücken. Beide
// Schnappschuss-Operationen existieren zum Zeitpunkt dieses Knotens nicht
// (siehe `_lib/webRoutes.ts`-Kopf) — diese Komponente zeigt in diesem Fall
// [`RouteMissingNotice`] statt eine [`NullCounterBar`]/[`MetricsTable`] mit
// erfundenem Inhalt zu rendern. Das ist eine bewusste Entscheidung: die
// Leiste selbst unterscheidet „alle null" von „keine Zähler registriert"
// (siehe `NullCounterBar`-Kopf) — „die Operation ist nicht erreichbar" ist
// ein dritter, wiederum eigener Zustand, der keinem der beiden gleichen
// darf.
"use client";

import { useEffect, useState } from "react";
import type { JSX } from "react";

import { DataBlock } from "@/components/ui/DataBlock";
import { connectWebEventStream } from "@/lib/sse";

import { appendTelemetryFeedEvent, appendTelemetryGapNotice } from "../_lib/eventGap";
import { classifyNullCounterSnapshot, parseMetricsSnapshotText, parseNullCounterSnapshotText } from "../_lib/nullCounters";
import type { MetricSnapshotEntry, NullCounterBarState, TelemetryFeedEvent, TelemetryGapNotice } from "../_lib/types";
import {
  TELEMETRY_OPERATIONS,
  fetchMetricsSnapshot,
  fetchNullCounterSnapshot,
  resolveMetricsSnapshotRoute,
  resolveNullCounterSnapshotRoute,
} from "../_lib/webRoutes";
import { EventFeed } from "./EventFeed";
import { GapBanner } from "./GapBanner";
import { MetricsTable } from "./MetricsTable";
import { NullCounterBar } from "./NullCounterBar";
import { RouteMissingNotice } from "./RouteMissingNotice";

/** Fester Ereignisstrom-Endpunkt aus `lib/sse.ts` — keine Operationsroute, siehe dessen Moduldoku. */
const EVENTS_URL = "/events";

type SnapshotState<T> =
  | { readonly kind: "route-missing" }
  | { readonly kind: "loading" }
  | { readonly kind: "loaded"; readonly data: T }
  | { readonly kind: "parse-error" }
  | { readonly kind: "error"; readonly message: string };

/**
 * Lädt und zeigt die Telemetrieansicht: Nullzähler-Leiste, Metriktabelle,
 * Ereignis-Feed.
 *
 * # Returns
 * Das gerenderte Telemetrie-Fragment.
 */
export function TelemetryView(): JSX.Element {
  const [nullCounterState, setNullCounterState] = useState<SnapshotState<NullCounterBarState>>({
    kind: "route-missing",
  });
  const [metricsState, setMetricsState] = useState<SnapshotState<readonly MetricSnapshotEntry[]>>({
    kind: "route-missing",
  });
  const [gaps, setGaps] = useState<readonly TelemetryGapNotice[]>([]);
  const [feed, setFeed] = useState<readonly TelemetryFeedEvent[]>([]);

  const nullCounterRoute = resolveNullCounterSnapshotRoute();
  const metricsRoute = resolveMetricsSnapshotRoute();

  useEffect(() => {
    if (nullCounterRoute === undefined) {
      setNullCounterState({ kind: "route-missing" });
      return;
    }
    let cancelled = false;
    setNullCounterState({ kind: "loading" });
    fetchNullCounterSnapshot()
      .then((result) => {
        if (cancelled || result === undefined) {
          return;
        }
        if (!result.ok) {
          setNullCounterState({ kind: "error", message: result.error });
          return;
        }
        const entries = parseNullCounterSnapshotText(result.text);
        if (entries === null) {
          setNullCounterState({ kind: "parse-error" });
          return;
        }
        setNullCounterState({ kind: "loaded", data: classifyNullCounterSnapshot(entries) });
      })
      .catch((error: unknown) => {
        if (!cancelled) {
          setNullCounterState({
            kind: "error",
            message: error instanceof Error ? error.message : String(error),
          });
        }
      });
    return () => {
      cancelled = true;
    };
    // `nullCounterRoute` wird nur zur Existenzprüfung gelesen.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nullCounterRoute === undefined]);

  useEffect(() => {
    if (metricsRoute === undefined) {
      setMetricsState({ kind: "route-missing" });
      return;
    }
    let cancelled = false;
    setMetricsState({ kind: "loading" });
    fetchMetricsSnapshot()
      .then((result) => {
        if (cancelled || result === undefined) {
          return;
        }
        if (!result.ok) {
          setMetricsState({ kind: "error", message: result.error });
          return;
        }
        const entries = parseMetricsSnapshotText(result.text);
        if (entries === null) {
          setMetricsState({ kind: "parse-error" });
          return;
        }
        setMetricsState({ kind: "loaded", data: entries });
      })
      .catch((error: unknown) => {
        if (!cancelled) {
          setMetricsState({
            kind: "error",
            message: error instanceof Error ? error.message : String(error),
          });
        }
      });
    return () => {
      cancelled = true;
    };
    // `metricsRoute` wird nur zur Existenzprüfung gelesen.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [metricsRoute === undefined]);

  useEffect(() => {
    const handle = connectWebEventStream(EVENTS_URL, {
      onEvent: (event) => {
        setFeed((previous) => appendTelemetryFeedEvent(previous, event));
      },
      onGap: (missingCount) => {
        setGaps((previous) => appendTelemetryGapNotice(previous, missingCount));
      },
    });
    return () => handle.close();
  }, []);

  return (
    <section className="harw-telemetry-view" aria-label="Telemetrie">
      <h2>Telemetrie</h2>
      <GapBanner notices={gaps} />

      <h3>Nullzähler</h3>
      {renderNullCounterSection(nullCounterState)}

      <h3>Metriken</h3>
      {renderMetricsSection(metricsState)}

      <h3>Ereignisstrom</h3>
      <EventFeed events={feed} />
    </section>
  );
}

function renderNullCounterSection(state: SnapshotState<NullCounterBarState>): JSX.Element {
  switch (state.kind) {
    case "route-missing":
      return (
        <RouteMissingNotice
          operation={TELEMETRY_OPERATIONS.nullCounterSnapshot}
          detail="Es gibt unter harw-operations derzeit keine Operation, die harw-observe referenziert — die Nullzähler-Registrierung ist ohne Web-Fläche gebaut."
        />
      );
    case "loading":
      return <DataBlock label="Status" value="Lade Nullzähler-Schnappschuss…" />;
    case "parse-error":
      return (
        <DataBlock
          label="Antwort nicht auswertbar"
          value="Die Antwort der Nullzähler-Operation ist kein JSON-Array der erwarteten Form (name, count, invariant). Siehe _lib/nullCounters.ts für die angenommene Form."
        />
      );
    case "error":
      return <DataBlock label="Fehler" value={state.message} />;
    case "loaded":
      return <NullCounterBar state={state.data} />;
  }
}

function renderMetricsSection(state: SnapshotState<readonly MetricSnapshotEntry[]>): JSX.Element {
  switch (state.kind) {
    case "route-missing":
      return (
        <RouteMissingNotice
          operation={TELEMETRY_OPERATIONS.metricsSnapshot}
          detail="Es gibt unter harw-operations derzeit keine Operation, die einen Metrik-Schnappschuss liefert."
        />
      );
    case "loading":
      return <DataBlock label="Status" value="Lade Metrik-Schnappschuss…" />;
    case "parse-error":
      return (
        <DataBlock
          label="Antwort nicht auswertbar"
          value="Die Antwort der Metrik-Operation ist kein JSON-Array der erwarteten Form (name, kind, unit, value). Siehe _lib/nullCounters.ts für die angenommene Form."
        />
      );
    case "error":
      return <DataBlock label="Fehler" value={state.message} />;
    case "loaded":
      return <MetricsTable entries={state.data} />;
  }
}
