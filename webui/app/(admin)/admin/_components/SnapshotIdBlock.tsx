// Anzeige der `SnapshotId` samt Unterscheidbarkeits-Beweis (Knoten UI-07, Teil 2).
//
// # Zweck von `SnapshotId`
// „Ihr Zweck ist, dass zwei scheinbar gleiche Definitionen unterscheidbar
// sind" — `harw_agent_dsl::executable::SnapshotId` ist ein
// inhaltsadressierter BLAKE3-Digest der aufgelösten IR. Diese Komponente
// zeigt genau den Fall, für den es sie gibt: zwei Definitionen mit
// identischem Anzeigenamen (Rolle, Familie), aber unterschiedlichem
// `SnapshotId` — also tatsächlich unterschiedlichem Inhalt.
//
// # Bekannter Befund
// `SnapshotId` hat weder `Serialize`/`Deserialize` noch einen öffentlichen
// Konstruktor (das innere Feld ist privat) — siehe `_lib/webRoutes.ts`-Kopf
// für den vollständigen Befund. Diese Komponente behandelt einen fehlenden
// Wert deshalb nicht als Fehler, sondern als erwarteten, benannten Zustand.
import type { JSX } from "react";

import { DataBlock } from "@/components/ui/DataBlock";

export interface SnapshotIdBlockProps {
  readonly snapshotId: string | null;
}

/**
 * Zeigt einen einzelnen `SnapshotId`-Wert oder den bekannten Ausweich-Befund.
 *
 * # Returns
 * Ein [`DataBlock`] mit dem Hex-Digest, oder — wenn `snapshotId === null` —
 * eine Meldung, die den strukturellen Grund nennt statt eine Zeichenkette
 * zu erfinden.
 */
export function SnapshotIdBlock({ snapshotId }: SnapshotIdBlockProps): JSX.Element {
  if (snapshotId === null) {
    return (
      <DataBlock
        label="SnapshotId"
        value="nicht verfügbar — SnapshotId hat kein Serialize/Deserialize und keinen öffentlichen Konstruktor; diese Fläche kann sie nicht anzeigen, bis ein Knoten in harw-agent-dsl/harw-web einen Übertragungsweg baut."
      />
    );
  }
  return <DataBlock label="SnapshotId" value={snapshotId} />;
}

export interface SnapshotIdComparisonProps {
  readonly label: string;
  readonly left: { readonly name: string; readonly snapshotId: string | null };
  readonly right: { readonly name: string; readonly snapshotId: string | null };
}

/**
 * Vergleicht zwei Definitionen mit gleichem Anzeigenamen anhand ihrer
 * `SnapshotId` — der Fall, für den `SnapshotId` laut Auftrag existiert.
 *
 * # Returns
 * Eine Meldung, ob beide `SnapshotId`s übereinstimmen (dann sind beide
 * Definitionen tatsächlich inhaltsgleich) oder abweichen (dann sehen sie
 * nur gleich aus).
 */
export function SnapshotIdComparison({ label, left, right }: SnapshotIdComparisonProps): JSX.Element {
  const bothKnown = left.snapshotId !== null && right.snapshotId !== null;
  const identical = bothKnown && left.snapshotId === right.snapshotId;
  return (
    <section aria-label={label}>
      <DataBlock label={`${left.name} — SnapshotId`} value={left.snapshotId ?? "nicht verfügbar"} />
      <DataBlock label={`${right.name} — SnapshotId`} value={right.snapshotId ?? "nicht verfügbar"} />
      <DataBlock
        label="Ergebnis"
        value={
          !bothKnown
            ? "Vergleich nicht möglich — mindestens eine SnapshotId ist nicht verfügbar."
            : identical
              ? "Identisch — beide Definitionen sind trotz gleichem Anzeigenamen inhaltlich dieselbe Fassung."
              : "Unterschiedlich — beide sehen gleich aus, sind aber inhaltlich verschiedene Fassungen."
        }
      />
    </section>
  );
}
