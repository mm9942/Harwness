// Vier-Schichten-Karte für ein aufgelöstes Modell (Knoten UI-07, Teil 1).
//
// # Die tragende Auflage
// „Eine Ansicht, die die vier Schichten zu einer Zahl verrührt, nimmt dem
// Bediener genau die Information, für die es sie gibt." Diese Komponente
// rendert deshalb VIER eigenständige `<section>`-Blöcke — einen je Layer
// (`ProviderLayerView`, `DescriptorLayerView`, `RuntimeLayerView`,
// `ObservedLayerView`) — statt eines einzigen Scores. Es gibt in dieser
// Datei keine Funktion, die zwei oder mehr Layer-Felder addiert, mittelt
// oder sonst zu einem einzelnen Wert zusammenführt.
//
// Jeder angezeigte Wert (Modellname, Providername, Policy-Label) kommt aus
// Konfiguration/Provider-Dokumentation — potenziell von außen beeinflusst
// — und geht deshalb ausschließlich durch [`DataBlock`].
import type { JSX } from "react";

import { DataBlock } from "@/components/ui/DataBlock";

import type { ObservedScoreView, ResolvedModelView } from "../_lib/types";

/** Rendert einen einzelnen Layer-4-Score mit seiner Evidenzlage — nie als nackte Zahl ohne Kontext. */
function ScoreRow({ score }: { readonly score: ObservedScoreView }): JSX.Element {
  const value = score.measured
    ? `${score.value} (${score.evidenceCount} Beleg${score.evidenceCount === 1 ? "" : "e"})`
    : "nicht gemessen (Bootstrap-Default, kein Beleg)";
  return <DataBlock label={score.label} value={value} />;
}

export interface ModelLayersCardProps {
  readonly model: ResolvedModelView;
}

/**
 * Zeigt die vier Katalog-Layer eines Modells als vier getrennte Blöcke.
 *
 * # Description
 * Layer 1 (Provider) erscheint nur, wenn er aufgelöst werden konnte
 * (`model.provider !== null`) — sonst zeigt der Block selbst eine
 * „nicht aufgelöst"-Meldung statt zu verschweigen, dass diese Schicht fehlt.
 *
 * # Arguments
 * - `model` (`ResolvedModelView`): die vier aufgelösten Layer eines Modells.
 *
 * # Returns
 * Eine `<article>` mit vier `<section>`-Kindern, einem je Layer.
 */
export function ModelLayersCard({ model }: ModelLayersCardProps): JSX.Element {
  return (
    <article className="harw-model-layers" aria-label={`Modell ${model.descriptor.model}`}>
      <h3>
        <DataBlock value={`${model.descriptor.provider} / ${model.descriptor.model}`} />
      </h3>

      <section aria-label="Layer 1 — Provider">
        <h4>Layer 1 — Provider</h4>
        {model.provider === null ? (
          <DataBlock label="Provider" value="nicht aufgelöst — kein passender ProviderSpec im Katalog gefunden" />
        ) : (
          <>
            <DataBlock label="Anbieter" value={model.provider.name} />
            <DataBlock label="Basis-URL" value={model.provider.baseUrl} />
            <DataBlock label="API-Familie" value={model.provider.api} />
          </>
        )}
      </section>

      <section aria-label="Layer 2 — Deklarierte Fähigkeiten">
        <h4>Layer 2 — Deklarierte Fähigkeiten (Provider behauptet)</h4>
        <DataBlock label="Kontextfenster" value={`${model.descriptor.contextWindow} Tokens`} />
        <DataBlock
          label="Max. Output"
          value={model.descriptor.maxOutputTokens === null ? "nicht deklariert" : `${model.descriptor.maxOutputTokens} Tokens`}
        />
        <DataBlock label="Modalitäten" value={model.descriptor.modalities.join(", ") || "keine deklariert"} />
        <DataBlock label="Tool-Calling (Katalog)" value={model.descriptor.toolCalling} />
      </section>

      <section aria-label="Layer 3 — Harness-Policy">
        <h4>Layer 3 — Harness-Runtime-Policy (kein Provider-Anspruch)</h4>
        <DataBlock label="Kontext-Policy" value={model.runtime.contextPolicy} />
        <DataBlock label="Kompaktierungs-Policy" value={model.runtime.compactionPolicy} />
        <DataBlock label="Delegations-Policy" value={model.runtime.delegationPolicy} />
        <DataBlock label="Max. parallele Tools" value={String(model.runtime.maxParallelTools)} />
        <DataBlock label="Max. Child-Fanout" value={String(model.runtime.maxChildFanout)} />
      </section>

      <section aria-label="Layer 4 — Empirisch gemessen">
        <h4>Layer 4 — Empirisch gemessenes Verhalten</h4>
        <DataBlock
          label="Letzter Messzeitpunkt"
          value={model.observed.updatedAtIso ?? "nie gemessen (Bootstrap-Default)"}
        />
        {model.observed.scores.map((score) => (
          <ScoreRow key={score.label} score={score} />
        ))}
      </section>
    </article>
  );
}
