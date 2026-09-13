// Layer-4-Rückkanal — Anzeige von `ModelBehaviorProposal` (Knoten UI-07, Teil 1).
//
// # Schlägt vor, committet nie
// `harw_knowledge::model_behavior_proposal::ModelBehaviorProposal` trägt
// laut seiner Moduldoku strukturell KEINE `apply`-Methode — nur `accept`/
// `reject`, die ausschließlich den Prüfstatus umschreiben. Eine komplette
// Baumsuche nach einer Funktion, die einen `ModelDescriptor` anhand eines
// Vorschlags tatsächlich verändert, ergibt in der ganzen Codebasis keinen
// Treffer (siehe Moduldoku dort, Abschnitt „Warum es keine `apply`-Funktion
// gibt"). Diese Komponente bildet das ab:
// - Es gibt hier keinen Knopf. `ProposalActionsHint` zeigt nur Text.
// - Der Text benennt die Handlung nach dem, was sie tut: "als geprüft
//   markieren", niemals "übernehmen"/"anwenden" — ein Knopf mit dem Label
//   "Übernehmen" würde über die Wirkung der eigenen Handlung lügen.
// - Auch bei einem klickbaren Aufruf (sobald die Route existiert) würde
//   dieser ausschließlich `accept`/`reject` (Statuswechsel) auslösen —
//   niemals eine Katalogänderung.
import type { JSX } from "react";

import { DataBlock } from "@/components/ui/DataBlock";

import type { ModelBehaviorProposalView, ResolvedModelView } from "../_lib/types";

/**
 * Erklärt in einem Satz, was ein Statuswechsel bewirkt (und was nicht) —
 * ohne einen Knopf anzubieten, weil diese Fläche keine Web-Route dafür hat.
 *
 * # Returns
 * Ein erklärender [`DataBlock`]-Text, exportiert für den Negativtest
 * „kein Knopf, der eine Definition/einen Katalogeintrag ändert".
 */
export function ProposalActionsHint(): JSX.Element {
  return (
    <DataBlock
      label="Wirkung eines Prüfstatus"
      value="Ein Vorschlag lässt sich als geprüft markieren (angenommen/abgelehnt) — das schreibt ausschließlich seinen eigenen Prüfstatus. Keine Funktion in diesem Baum ändert dadurch einen Katalogeintrag; eine Katalogänderung bleibt manuelle Pflege der Vendor-Module durch einen Menschen."
    />
  );
}

/** Findet den `tool_schema_reliability`-Score im Observed-Layer und rendert ihn als Text. Kein erfundener Wert, wenn er fehlt. */
function renderToolSchemaScore(model: ResolvedModelView): string {
  const score = model.observed.scores.find((entry) => entry.label === "tool_schema_reliability");
  if (score === undefined) {
    return "kein Score mit diesem Namen im geladenen Observed-Layer";
  }
  return score.measured
    ? `${score.value} (${score.evidenceCount} Beleg${score.evidenceCount === 1 ? "" : "e"})`
    : "nicht gemessen (Bootstrap-Default, kein Beleg)";
}

export interface ModelBehaviorProposalPanelProps {
  readonly proposal: ModelBehaviorProposalView;
  /** Das betroffene Modell, falls im geladenen Katalog auffindbar — für den Katalog/Beobachtung/Vorschlag-Bezug. */
  readonly targetModel: ResolvedModelView | null;
}

/**
 * Zeigt einen einzelnen `ModelBehaviorProposal` samt dem Bezug „Katalog
 * sagt X, Beobachtung sagt Y, ein Vorschlag liegt vor".
 *
 * # Description
 * Wenn `targetModel` bekannt ist, stellt diese Komponente die
 * Katalogbehauptung (Layer 2, z. B. `descriptor.toolCalling`) neben die
 * vorgeschlagene Änderung — beide als eigene [`DataBlock`]s, nie
 * zusammengeführt. Ist `targetModel === null` (Katalog nicht geladen oder
 * Modell darin nicht gefunden), wird das ausdrücklich gemeldet statt eines
 * erfundenen Vergleichs.
 *
 * # Arguments
 * - `proposal` (`ModelBehaviorProposalView`): der anzuzeigende Vorschlag.
 * - `targetModel` (`ResolvedModelView | null`): das betroffene Modell,
 *   falls bekannt.
 *
 * # Returns
 * Eine `<article>` mit Titel, Bezug, Status und Herkunft.
 */
export function ModelBehaviorProposalPanel({
  proposal,
  targetModel,
}: ModelBehaviorProposalPanelProps): JSX.Element {
  return (
    <article className="harw-model-behavior-proposal" aria-label={`Vorschlag ${proposal.id}`}>
      <DataBlock label="Titel" value={proposal.title} />
      <DataBlock label="Betrifft" value={`${proposal.targetProvider} / ${proposal.targetModel}`} />
      <DataBlock label="Status" value={proposal.status} />
      <DataBlock label="Herkunft" value={proposal.producedBy} />

      <section aria-label="Katalog vs. Beobachtung vs. Vorschlag">
        <h4>Katalog sagt … Beobachtung sagt … Vorschlag liegt vor</h4>
        {targetModel === null ? (
          <DataBlock
            label="Bezug"
            value="Das betroffene Modell ist im geladenen Katalog nicht auffindbar — kein Vergleich möglich, statt eines erfundenen."
          />
        ) : (
          <>
            <DataBlock label="Katalog behauptet (Layer 2)" value={targetModel.descriptor.toolCalling} />
            <DataBlock label="Beobachtung zeigt (Layer 4)" value={renderToolSchemaScore(targetModel)} />
          </>
        )}
        {proposal.changes.map((change, index) => (
          // eslint-disable-next-line react/no-array-index-key -- Änderungen sind reine Anzeigedaten ohne stabile Id.
          <DataBlock key={index} label="Vorgeschlagene Änderung" value={`${change.kind} → ${change.to}`} />
        ))}
      </section>

      <ProposalActionsHint />
    </article>
  );
}
