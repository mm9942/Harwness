// Fachliche Typen für Sitzungsliste und Agentenbaum (Knoten UI-03).
//
// Diese Datei modelliert nur, was in dieser Ansicht tatsächlich gebraucht
// wird — keine Annahme über Felder, die `harw-web`/`harw-ops` (noch) nicht
// liefern (siehe Abschlussbericht dieses Knotens für die vollständige Liste
// der geprüften, aber nicht gefundenen Felder/Operationen).

/** Eine einzelne Sitzung, wie sie eine künftige `session.list`-Antwort liefern müsste. */
export interface SessionSummary {
  /** Stabile Sitzungs-ID. Angreiferkontrolliert wie jeder Serverwert — nur über `DataBlock` anzeigen. */
  readonly id: string;
  /** Menschenlesbarer Titel/Status-Text. Angreiferkontrolliert. */
  readonly label: string;
}

/**
 * Ein Knoten im Agentenbaum, wie ihn `harw-core::child_controller` intern
 * kennt (`ChildRecord`/`TraceContext`) — **sofern** eine Operation diese
 * Felder je exponiert. `traceId`/`spanId`/`parentSpanId` spiegeln
 * `TraceContext` (siehe `harw-core/src/child_controller.rs`,
 * `inherit_trace`): ein Kind erbt `traceId` seines Elternteils, bekommt eine
 * frische `spanId` und trägt die `spanId` des Elternteils als
 * `parentSpanId`. `contextCeiling`/`permissions` sind optional, weil noch
 * keine Operation belegt, dass sie geliefert werden (siehe Bericht,
 * „geschnittene Decke").
 */
export interface AgentNode {
  readonly spanId: string;
  readonly traceId: string;
  readonly parentSpanId: string | null;
  /** Menschenlesbarer Agentenname. Angreiferkontrolliert. */
  readonly label: string;
  /** Geschnittene Kontext-Decke, falls geliefert (`ContextCeiling`, `harw-context::ceiling`). */
  readonly contextCeiling: string | null;
  /** Geschnittene Permissions, falls geliefert. */
  readonly permissions: string | null;
}

/** Eine einzelne, im UI gesammelte Lücken-Meldung aus dem Ereignisstrom. */
export interface GapNotice {
  readonly id: number;
  readonly missingCount: number;
  readonly detectedAtIso: string;
}
