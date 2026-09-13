// Gemeinsame Zustandsanzeige für Listen dieser Fläche (Knoten UI-07).
//
// # Warum diese Datei existiert
// Der Auftrag verlangt ausdrücklich, dass „keine Daten verfügbar" (die
// Operation wurde aufgerufen, lieferte aber einen Fehler oder ist noch
// unterwegs) von „nichts konfiguriert" (die Operation antwortete explizit
// mit einer leeren Liste) unterscheidbar bleibt — und beide wiederum von
// „Route nicht deklariert" ([`RouteMissingNotice`], eine dritte, noch
// grundlegendere Lücke). Eine leere Liste, die wie „nichts konfiguriert"
// aussieht, obwohl in Wahrheit nie eine Antwort ankam, wäre exakt die
// Verwechslung, die dieses Programm mit `GateReport::checked` an anderer
// Stelle bereits bekämpft.
import type { JSX } from "react";

import { DataBlock } from "@/components/ui/DataBlock";

import type { ListLoadState } from "../_lib/types";
import { RouteMissingNotice } from "./RouteMissingNotice";

export interface ListStateNoticeProps<T> {
  readonly state: ListLoadState<T>;
  readonly operation: string;
  readonly detail: string;
  readonly emptyLabel: string;
}

/**
 * Rendert die Nicht-Erfolgs-Zustände eines [`ListLoadState`] — `loaded` mit
 * Einträgen wird bewusst NICHT hier gerendert, das bleibt Sache des
 * Aufrufers (der die Einträge fachlich darstellt).
 *
 * # Returns
 * `null`, wenn `state.kind === "loaded"` und `items.length > 0` — sonst die
 * passende Meldung.
 */
export function ListStateNotice<T>({
  state,
  operation,
  detail,
  emptyLabel,
}: ListStateNoticeProps<T>): JSX.Element | null {
  switch (state.kind) {
    case "route-missing":
      return <RouteMissingNotice operation={operation} detail={detail} />;
    case "loading":
      return <DataBlock label="Status" value="Lädt…" />;
    case "error":
      return <DataBlock label="Keine Daten verfügbar" value={`Die Operation antwortete mit einem Fehler: ${state.message}`} />;
    case "empty":
      return <DataBlock label="Nichts konfiguriert" value={emptyLabel} />;
    case "loaded":
      return state.items.length > 0 ? null : <DataBlock label="Nichts konfiguriert" value={emptyLabel} />;
    default:
      return null;
  }
}
