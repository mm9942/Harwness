// Allgemeine Metrikansicht (Knoten UI-04).
//
// Zeigt Messgrößen, wie `harw-observe::MetricKey`/`MetricValue`
// (`harw-observe/src/metric.rs`) sie beschreiben. Bewusst **ohne**
// Beschreibungsspalte: `MetricKey` trägt kein `description`-Feld — der vom
// Prometheus-Knoten (AW3-04) gemeldete Befund wurde für diesen Knoten
// erneut geprüft (`harw-observe/src/metric.rs`, `MetricKey`-Struct-Felder:
// `name`, `kind`, `unit`, `labels`, `cardinality` — kein Text) und
// bestätigt. Diese Ansicht dichtet die Lücke nicht mit einem erfundenen
// Text zu, sondern meldet sie sichtbar.
import type { JSX } from "react";

import { DataBlock } from "@/components/ui/DataBlock";

import { isRestrictedNamespaceMetric } from "../_lib/nullCounters";
import type { MetricSnapshotEntry } from "../_lib/types";

export interface MetricsTableProps {
  readonly entries: readonly MetricSnapshotEntry[];
}

/**
 * Rendert eine Zeile pro Messgröße.
 *
 * # Returns
 * Ein `<section>`; bei leerer Liste ein Hinweis, dass keine Metriken
 * angekommen sind (nicht: dass keine existieren — siehe Auftrag,
 * Unterscheidung „geprüft" vs. „nicht erreicht").
 */
export function MetricsTable({ entries }: MetricsTableProps): JSX.Element {
  const restricted = entries.filter((entry) => isRestrictedNamespaceMetric(entry.name));

  if (entries.length === 0) {
    return (
      <section className="harw-metrics-table" data-testid="metrics-table" data-state="empty">
        <DataBlock
          label="Metriken"
          value="Keine Metrikwerte angekommen. Das bedeutet nicht, dass harw-observe keine Metriken führt — nur, dass hier keine ankamen."
        />
      </section>
    );
  }

  return (
    <section className="harw-metrics-table" data-testid="metrics-table" data-state="loaded">
      <DataBlock
        label="Hinweis"
        value="MetricKey trägt keinen Beschreibungstext (bestätigter Befund, siehe Dateikopf) — diese Tabelle zeigt deshalb nur Name, Art, Einheit und Wert, keine Beschreibung."
      />
      {entries.map((entry) => (
        <DataBlock
          key={entry.name}
          label={entry.name}
          value={`Art: ${entry.kind} · Einheit: ${entry.unit} · Wert: ${entry.value}`}
        />
      ))}
      {restricted.length > 0 ? (
        <DataBlock
          label="Befund: gesperrter Namensraum über Web erreicht"
          value={`Folgende Metriken stammen laut Namen aus "security."/"warden." und dürften laut AW5-06 nicht über einen Web-Export sichtbar sein: ${restricted
            .map((entry) => entry.name)
            .join(", ")}.`}
        />
      ) : null}
    </section>
  );
}
