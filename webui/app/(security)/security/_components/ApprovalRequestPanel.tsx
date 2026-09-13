"use client";

// Die Bestätigungsfläche selbst (Knoten UI-06) — der einzige Ort, an dem
// ein Mensch eine irreversible Aktion freigibt.
//
// # Kein zweiter Autoritätspfad
// Diese Komponente **zeigt** eine Anfrage und nimmt eine menschliche
// Entscheidung entgegen; sie führt selbst nichts aus. Jede Entscheidung
// geht ausschließlich über `submitApprovalDecision` →
// `findRoute`/`callOperation` (`_lib/approvalOperations.ts`) — es gibt in
// dieser Datei keinen Knopf, der eine cgroup einfriert oder einen
// Prozessbaum beendet, und keinen Pfad, der eine Aktion ohne eine über
// `harw-web` deklarierte Route auslöst (fehlt die Route, zeigt diese
// Komponente [`ApprovalRouteMissingNotice`] statt einen Ersatzweg zu
// erfinden). Die Autorisierung selbst entsteht weiterhin ausschließlich in
// `authorize` (`harw-dod-escalate`, `pub(crate)`) — siehe Auftrag,
// Abschnitt „Die Regel, die dieser Knoten durchsetzt".
//
// # Umkehrbarkeit ist keine Stilfrage
// `view.irreversible` bestimmt, welcher der beiden — textlich und über
// `role`/`data-testid` unterscheidbaren — Warnblöcke gerendert wird.
// „Prozessbaum beenden" und „cgroup einfrieren" sehen als Knopf gleich aus
// und sind es nicht (siehe Auftrag, Abschnitt „Was sieht der Mensch?").
//
// # Jeder angezeigte Wert geht durch `DataBlock`
// `view.rationale` stammt aus Sensordaten/Modellausgabe und ist
// angreiferkontrolliert — der Text, mit dem jemand einen Bediener zu einer
// Freigabe bewegen möchte. Diese Datei ruft nirgends
// `dangerouslySetInnerHTML` auf, bindet kein Markdown ein und baut kein
// `<a href>` aus einem angezeigten Wert.
//
// # Woher der Anzeigeinhalt kommt — noch offen
// `view` (ein [`PendingApprovalView`]) wird als Prop entgegengenommen,
// nicht selbst geladen — es gibt (Stand dieses Knotens) keine Route, die
// eine einzelne anstehende Anfrage nach `session`/`request` parametrisiert
// nachschlägt (siehe `_lib/approvalOperations.ts`-Kopf). Ein künftiger
// Aufrufer (z. B. ein Ereignis im SSE-Strom, sobald `WebEventKind` eine
// Genehmigungs-Nutzlast kennt) liefert `view`; diese Komponente erfindet
// dafür keinen eigenen Abrufweg.
import { useState, type JSX } from "react";

import { DataBlock } from "@/components/ui/DataBlock";

import { submitApprovalDecision } from "../_lib/approvalOperations";
import type { PendingApprovalView, ReviewDecision } from "../_lib/approvalTypes";
import { ApprovalRouteMissingNotice } from "./ApprovalRouteMissingNotice";

export interface ApprovalRequestPanelProps {
  /** Anzeigeinhalt der anstehenden Anfrage (siehe Dateikopf, „noch offen"). */
  readonly view: PendingApprovalView;
  /**
   * Wird nach einer erfolgreich übermittelten Entscheidung aufgerufen —
   * z. B. damit ein Aufrufer die Anfrage aus einer Liste entfernt. Löst
   * selbst keine Aktion aus.
   */
  readonly onResolved?: (decision: ReviewDecision) => void;
}

const DECISION_LABELS: Record<ReviewDecision, string> = {
  approved: "Genehmigen",
  approved_once: "Einmalig genehmigen",
  rejected: "Ablehnen",
};

/**
 * Zeigt eine anstehende Genehmigungsanfrage und nimmt eine menschliche
 * Entscheidung entgegen.
 *
 * # Description
 * Rendert Umkehrbarkeit, Aktionsart, Befundtext (über `DataBlock`) und drei
 * Entscheidungsknöpfe. Nach dem ersten erfolgreichen Absenden verschwinden
 * die Knöpfe lokal — eine zweite Bestätigung derselben Anfrage weist
 * `harw-web`/`ApprovalStore::resolve` ohnehin serverseitig ab
 * (`ApprovalAlreadyResolved`); dieses lokale Ausblenden ist nur eine
 * Bedienhilfe, keine Sicherheitsgrenze.
 *
 * # Arguments
 * - `view` (`PendingApprovalView`): der anzuzeigende Anfrageinhalt.
 * - `onResolved` (`(decision: ReviewDecision) => void`, optional): Rückruf
 *   nach erfolgreicher Auflösung.
 *
 * # Returns
 * Ein `<section>` mit Warnblock, Befundtext und (solange unaufgelöst)
 * Entscheidungsknöpfen.
 */
export function ApprovalRequestPanel({
  view,
  onResolved,
}: ApprovalRequestPanelProps): JSX.Element {
  const [comment, setComment] = useState("");
  const [submitting, setSubmitting] = useState<ReviewDecision | null>(null);
  const [routeMissingOperation, setRouteMissingOperation] = useState<string | null>(null);
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const [resolvedDecision, setResolvedDecision] = useState<ReviewDecision | null>(null);

  async function handleDecision(decision: ReviewDecision): Promise<void> {
    setSubmitting(decision);
    setErrorMessage(null);
    const result = await submitApprovalDecision(
      view.record.session,
      view.record.request,
      decision,
      comment.trim().length > 0 ? comment.trim() : null,
    );
    setSubmitting(null);
    if (result.status === "route-missing") {
      setRouteMissingOperation(result.operation);
      return;
    }
    if (result.status === "error") {
      setErrorMessage(result.message);
      return;
    }
    setResolvedDecision(decision);
    onResolved?.(decision);
  }

  return (
    <section className="harw-approval-panel" aria-label="Genehmigungsanfrage">
      <h2>Genehmigungsanfrage</h2>

      {view.irreversible ? (
        <div
          className="harw-approval-irreversible"
          role="alert"
          data-testid="approval-irreversible-warning"
        >
          <DataBlock
            label="Nicht umkehrbar"
            value={`Diese Aktion (${view.actionKind}) lässt sich nach Ausführung nicht rückgängig machen.`}
          />
        </div>
      ) : (
        <div
          className="harw-approval-reversible"
          role="status"
          data-testid="approval-reversible-notice"
        >
          <DataBlock
            label="Umkehrbar"
            value={`Diese Aktion (${view.actionKind}) lässt sich nach Ausführung wieder aufheben.`}
          />
        </div>
      )}

      <DataBlock label="Begründung" value={view.rationale} />
      <DataBlock label="Anfrage-ID" value={view.record.request} />
      <DataBlock label="Sitzung" value={view.record.session} />
      <DataBlock label="Ausgestellt am" value={view.record.issuedAt} />

      {resolvedDecision !== null ? (
        <div role="status" data-testid="approval-resolved-notice">
          <DataBlock
            label="Entschieden"
            value={`Diese Anfrage wurde als "${DECISION_LABELS[resolvedDecision]}" übermittelt.`}
          />
        </div>
      ) : routeMissingOperation !== null ? (
        <ApprovalRouteMissingNotice operation={routeMissingOperation} />
      ) : (
        <>
          <label className="harw-approval-comment-label" htmlFor="approval-comment">
            Kommentar (optional)
          </label>
          <textarea
            id="approval-comment"
            className="harw-approval-comment"
            value={comment}
            onChange={(event) => setComment(event.target.value)}
            disabled={submitting !== null}
          />
          {errorMessage !== null ? (
            <div role="alert" data-testid="approval-submit-error">
              <DataBlock label="Fehler bei der Übermittlung" value={errorMessage} />
            </div>
          ) : null}
          <div className="harw-approval-actions">
            {(Object.keys(DECISION_LABELS) as readonly ReviewDecision[]).map((decision) => (
              <button
                key={decision}
                type="button"
                data-testid={`approval-decision-${decision}`}
                disabled={submitting !== null}
                onClick={() => {
                  void handleDecision(decision);
                }}
              >
                {DECISION_LABELS[decision]}
              </button>
            ))}
          </div>
        </>
      )}
    </section>
  );
}
