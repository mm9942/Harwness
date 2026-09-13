// Generische Listenfläche für eine Sicherheits-Datenquelle (Knoten UI-05).
//
// Trägt die zentrale Auflage dieses Knotens an einer einzigen Stelle:
// - **„keine Daten verfügbar" ≠ „keine Befunde".** Ein `route-missing`-
//   oder `error`-Ergebnis rendert [`SecurityRouteMissingNotice`]/eine
//   Fehlermeldung mit `data-testid="security-no-data"`; ein `ok`-Ergebnis
//   mit leerer Liste rendert eine eigene, textlich und per
//   `data-testid="security-no-findings"` unterscheidbare Meldung. Beide
//   sehen im DOM nicht gleich aus und tragen nicht denselben Text (siehe
//   Auftrag, Abschnitt „Tests").
// - **Jeder Wert geht durch `DataBlock`/`DataBlockList`.** `toLines`
//   liefert nur Zeichenketten; diese Datei selbst ruft nirgends
//   `dangerouslySetInnerHTML` und baut kein `<a href>` aus einem Datenwert.
import type { JSX } from "react";

import { DataBlock, DataBlockList } from "@/components/ui/DataBlock";

import type { SecurityFetchResult } from "../_lib/types";
import { SecurityRouteMissingNotice } from "./SecurityRouteMissingNotice";

export interface SecurityListPanelProps<T> {
  /** Überschrift der Fläche, z. B. "Befunde". Fest codiert, nicht aus Daten. */
  readonly title: string;
  /** Abrufergebnis dieser Datenquelle — siehe `SecurityFetchResult`. */
  readonly result: SecurityFetchResult<T>;
  /** Baut aus einem Element die Anzeigezeilen (siehe `_lib/present.ts`). */
  readonly toLines: (item: T) => readonly string[];
  /** Label für jedes einzelne Element in der Liste, z. B. "Befund". */
  readonly itemLabel: string;
}

/**
 * Rendert eine Sicherheits-Datenquelle nach dem einheitlichen
 * Drei-Wege-Schema: keine Route, Fehler, oder Ergebnis (leer oder gefüllt).
 *
 * # Returns
 * Ein `<section>` mit Überschrift und dem jeweils passenden Inhalt.
 */
export function SecurityListPanel<T>({
  title,
  result,
  toLines,
  itemLabel,
}: SecurityListPanelProps<T>): JSX.Element {
  return (
    <section className="harw-security-panel" aria-label={title}>
      <h2>{title}</h2>
      {result.status === "route-missing" ? (
        <SecurityRouteMissingNotice operation={result.operation} />
      ) : result.status === "error" ? (
        <div role="alert" data-testid="security-no-data">
          <DataBlock
            label={`Fehler bei "${result.operation}"`}
            value={result.message}
          />
        </div>
      ) : result.items.length === 0 ? (
        <div role="status" data-testid="security-no-findings">
          <DataBlock
            label="Keine Befunde"
            value={`Die Operation wurde erfolgreich aufgerufen und hat keine Einträge geliefert — dies ist eine erfolgreiche leere Antwort, kein fehlender Datenweg.`}
          />
        </div>
      ) : (
        <ul className="harw-security-panel-list" data-testid="security-findings-list">
          {result.items.map((item, index) => (
            // eslint-disable-next-line react/no-array-index-key -- Einträge
            // sind reiner Anzeigeinhalt ohne vom Server garantierte stabile
            // Identität an dieser Stelle; Index ist hier sicher.
            <li key={index}>
              <DataBlockList label={itemLabel} values={toLines(item)} />
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
