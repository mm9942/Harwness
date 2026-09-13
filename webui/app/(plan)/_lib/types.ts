// Fachliche Typen für Plan, Ziel und Vorschlagswarteschlange (Knoten UI-03).
//
// `harw-ops::plan`/`harw-ops::goal` liefern beide nur menschenlesbaren Text
// (`OpOutput { text: String }`) — es gibt kein strukturiertes JSON-Schema
// für einen Plan-Baum oder ein Ziel-Objekt im Web-Transport (siehe
// Abschlussbericht). Diese Datei modelliert deshalb nur, was tatsächlich
// zeilenweise angezeigt wird.

/** Eine einzelne, angezeigte Textzeile aus einer Plan-/Ziel-Antwort. Angreiferkontrolliert. */
export type PlanLine = string;

/** Status eines Vorschlags — Spiegelbild von `ContextProposal`/`ModelBehaviorProposal`. */
export type ProposalStatus = "Pending" | "Accepted" | "Rejected" | string;

/**
 * Ein einzelner Vorschlag in der Warteschlange, wie ihn `/context-proposal
 * list` als Text meldet. Da die Operation nur Text liefert, ist `raw` immer
 * gesetzt; `id`/`status` werden best-effort aus dem Text herausgelesen,
 * wenn erkennbar (siehe `_lib/parseProposals.ts`).
 */
export interface ProposalSummary {
  /** Vollständige, unveränderte Textzeile — immer über `DataBlock` anzeigen. */
  readonly raw: string;
  /** Vorschlag-Id, falls aus `raw` erkennbar. */
  readonly id: string | null;
  /** Status, falls aus `raw` erkennbar. */
  readonly status: ProposalStatus | null;
}
