"use client";

// Sicherheitszentrale — die lesende Übersicht über Befunde, Hostmesswerte,
// Sicherheitsereignisse, Agenten-Urteile und Freeze-Datensätze (Knoten UI-05).
//
// # Lesend, ausdrücklich
// Diese Komponente führt nichts aus: kein Knopf hier ruft eine
// Durchsetzungsaktion auf. Der Durchsetzer ist `harw-warden`, und der Weg
// dorthin geht über die Eskalationsleiter (`harw_dod_escalate::authorize`,
// `pub(crate)`) — Autorisierung entsteht an genau einer Stelle, und die ist
// nicht diese UI (siehe Auftrag, Abschnitt 2). Ein irreversibler Vorgang
// wird an anderer Stelle (UI-06) zu einem `ApprovalRequest`, nie zu einem
// Knopf hier.
//
// # Live-Ereignisse
// `EventSource` ist nicht in jeder Laufzeitumgebung vorhanden (SSR, jsdom
// in Tests) — die Verbindung wird deshalb nur aufgebaut, wenn
// `typeof EventSource !== "undefined"` (gleiches Muster wie
// `(chat)/chat/_components/ChatView.tsx`). Dieser Knoten öffnet in keinem
// Test eine echte Verbindung — nur die reine Funktion
// `appendSecurityGapNotice` wird getestet.
import { useEffect, useState, type JSX } from "react";

import { connectWebEventStream, type WebEventStreamHandle } from "@/lib/sse";

import { appendSecurityGapNotice } from "../_lib/eventGap";
import {
  findingToLines,
  freezeToLines,
  hostSampleToLines,
  securityEventToLines,
  verdictToLines,
} from "../_lib/present";
import {
  fetchFindings,
  fetchFreezeRecords,
  fetchHostSamples,
  fetchSecurityEvents,
  fetchSecurityVerdicts,
} from "../_lib/securityOperations";
import type {
  Finding,
  FreezeRecord,
  HostSample,
  SecurityEvent,
  SecurityFetchResult,
  SecurityGapNotice,
  SecurityVerdict,
} from "../_lib/types";
import { SecurityGapBanner } from "./SecurityGapBanner";
import { SecurityListPanel } from "./SecurityListPanel";

/** Fester SSE-Endpunkt von `harw-web` — kein Operationsname, siehe `lib/sse.ts`-Kopf. */
const EVENTS_URL = "/events";

/** Anfangszustand vor dem ersten Abrufversuch: „keine Route" ist der sichere Default. */
function initialResult<T>(operation: string): SecurityFetchResult<T> {
  return { status: "route-missing", operation };
}

/**
 * Die vollständige Sicherheitszentrale: fünf Datenquellen-Flächen plus
 * Lücken-Anzeige des Ereignisstroms.
 *
 * # Description
 * Lädt jede Datenquelle unabhängig beim Mount (jede Operation kann fehlen,
 * ohne die anderen zu blockieren) und verbindet sich mit dem
 * Ereignisstrom ausschließlich zur Lücken-Erkennung — diese Fläche
 * verarbeitet keine einzelnen Ereignis-Nutzlasten aus dem Strom weiter,
 * weil `WebEventKind` (Stand dieses Knotens, `lib/sse.ts`) keine
 * Sicherheits-Nutzlast kennt, nur `operation_completed`/`heartbeat`.
 *
 * # Returns
 * Die vollständige Ansicht unter `/security`.
 */
export function SecurityView(): JSX.Element {
  const [findings, setFindings] = useState<SecurityFetchResult<Finding>>(() =>
    initialResult<Finding>("security.findings.list"),
  );
  const [samples, setSamples] = useState<SecurityFetchResult<HostSample>>(() =>
    initialResult<HostSample>("security.samples.list"),
  );
  const [events, setEvents] = useState<SecurityFetchResult<SecurityEvent>>(() =>
    initialResult<SecurityEvent>("security.events.list"),
  );
  const [verdicts, setVerdicts] = useState<SecurityFetchResult<SecurityVerdict>>(() =>
    initialResult<SecurityVerdict>("security.verdicts.list"),
  );
  const [freezes, setFreezes] = useState<SecurityFetchResult<FreezeRecord>>(() =>
    initialResult<FreezeRecord>("security.freeze.list"),
  );
  const [gapNotices, setGapNotices] = useState<readonly SecurityGapNotice[]>([]);

  useEffect(() => {
    let cancelled = false;
    fetchFindings().then((result) => {
      if (!cancelled) setFindings(result);
    });
    fetchHostSamples().then((result) => {
      if (!cancelled) setSamples(result);
    });
    fetchSecurityEvents().then((result) => {
      if (!cancelled) setEvents(result);
    });
    fetchSecurityVerdicts().then((result) => {
      if (!cancelled) setVerdicts(result);
    });
    fetchFreezeRecords().then((result) => {
      if (!cancelled) setFreezes(result);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (typeof EventSource === "undefined") {
      return;
    }
    const handle: WebEventStreamHandle = connectWebEventStream(EVENTS_URL, {
      onGap: (missingCount) => {
        setGapNotices((current) => appendSecurityGapNotice(current, missingCount));
      },
      onError: () => {
        // Verbindungsfehler werden nicht separat anders behandelt als eine
        // Lücke — ohne Sequenznummer lässt sich ihr Ausmaß nicht beziffern
        // (gleiches Verhalten wie `(chat)/chat/_components/ChatView.tsx`).
      },
    });
    return () => {
      handle.close();
    };
  }, []);

  return (
    <section className="harw-security-view" aria-label="Sicherheitszentrale">
      <h1>Sicherheitszentrale</h1>
      <p className="harw-security-readonly-notice" role="note">
        Diese Fläche ist lesend. Kein Element hier friert eine cgroup ein,
        hebt einen Freeze auf oder beendet einen Prozess — Durchsetzung läuft
        ausschließlich über harw-warden und die Eskalationsleiter.
      </p>
      <SecurityGapBanner notices={gapNotices} />
      <SecurityListPanel
        title="Befunde"
        result={findings}
        toLines={findingToLines}
        itemLabel="Befund"
      />
      <SecurityListPanel
        title="Agenten-Urteile"
        result={verdicts}
        toLines={verdictToLines}
        itemLabel="Urteil"
      />
      <SecurityListPanel
        title="Sicherheitsereignisse"
        result={events}
        toLines={securityEventToLines}
        itemLabel="Ereignis"
      />
      <SecurityListPanel
        title="Hostmesswerte"
        result={samples}
        toLines={hostSampleToLines}
        itemLabel="Messwert"
      />
      <SecurityListPanel
        title="Freeze-Datensätze"
        result={freezes}
        toLines={freezeToLines}
        itemLabel="Freeze"
      />
    </section>
  );
}
