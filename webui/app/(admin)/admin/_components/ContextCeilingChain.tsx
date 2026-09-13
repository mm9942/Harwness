// `ContextCeiling`-Ableitungskette mit sichtbarem Schnittpunkt (Knoten UI-07, Teil 2).
//
// # Halbverband-Disziplin
// `harw_context::ceiling::ContextCeiling::intersect` bildet ausschließlich
// den Schnitt zweier Decken — es gibt keine Vereinigung, keinen Weg, eine
// Decke anzuheben (siehe Moduldoku dort). Bei `extends` in
// `harw_agent_dsl::context_program` gilt seit Kurzem zusätzlich: die
// `TrustClass` einer geerbten Sektion wird beim Auflösen auf den Rang der
// `extends`-Basis gedeckelt ([`harw_context::fragment::TrustClass::trust_rank`]),
// während `detail`/`strength` additiv bleiben und deshalb hier nicht
// geprüft werden. Diese Komponente zeigt jede Stufe der Kette und markiert
// sichtbar, an welcher Stufe tatsächlich geschnitten wurde — nicht nur das
// Endergebnis.
import type { JSX } from "react";

import { DataBlock } from "@/components/ui/DataBlock";

import type { CeilingChainView, CeilingStepView } from "../_lib/types";

/** Rendert eine einzelne Stufe der Ableitungskette samt Schnitt-Markierung. */
function StepRow({ step }: { readonly step: CeilingStepView }): JSX.Element {
  return (
    <li className="harw-ceiling-step" data-cut={step.cut}>
      <DataBlock label="Kontextprogramm" value={step.programId} />
      <DataBlock label="TrustClass vor dieser Stufe" value={step.trustBefore} />
      <DataBlock label="TrustClass nach dieser Stufe" value={step.trustAfter} />
      <DataBlock
        label="Geschnitten?"
        value={step.cut ? `ja — TrustClass auf ${step.trustAfter} gesenkt` : "nein — unverändert übernommen"}
      />
    </li>
  );
}

export interface ContextCeilingChainProps {
  readonly chain: CeilingChainView;
}

/**
 * Zeigt eine vollständige `extends`-Kette bis zum aufgelösten Kontextprogramm.
 *
 * # Description
 * Jede Stufe erscheint einzeln, in Ableitungsreihenfolge (Basis zuerst).
 * `detail`/`strength` werden hier bewusst nicht dargestellt — sie sind
 * additiv, nicht geschnitten, und ihre Anzeige würde suggerieren, dass auch
 * sie ein Sicherheitsschnitt wären.
 *
 * # Arguments
 * - `chain` (`CeilingChainView`): die Ableitungskette eines Kontextprogramms.
 *
 * # Returns
 * Eine `<article>` mit einer geordneten Liste der Kettenstufen.
 */
export function ContextCeilingChain({ chain }: ContextCeilingChainProps): JSX.Element {
  const anyCut = chain.steps.some((step) => step.cut);
  return (
    <article className="harw-ceiling-chain" aria-label={`Decken-Kette für ${chain.leafProgramId}`}>
      <DataBlock label="Aufgelöstes Kontextprogramm" value={chain.leafProgramId} />
      <DataBlock
        label="Zusammenfassung"
        value={
          anyCut
            ? "Mindestens eine Stufe dieser Kette hat die TrustClass gegenüber ihrer Basis gesenkt."
            : "Keine Stufe dieser Kette senkt die TrustClass gegenüber ihrer Basis."
        }
      />
      {chain.steps.length === 0 ? (
        <DataBlock label="Kette" value="keine Ableitung — dieses Kontextprogramm hat kein extends" />
      ) : (
        <ol>
          {chain.steps.map((step) => (
            <StepRow key={step.programId} step={step} />
          ))}
        </ol>
      )}
    </article>
  );
}
