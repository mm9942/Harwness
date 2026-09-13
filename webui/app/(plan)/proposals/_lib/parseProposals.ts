// Reiner Parser für die Textausgabe von `/context-proposal list` (Knoten UI-03).
//
// `render_list` in `harw-ops/src/context_proposal.rs` gibt pro Zeile
// `"· {id} [{status}]"` aus (siehe dortiger Quelltext). Dieser Parser ist
// best-effort: erkennt er das Format nicht, liefert er den Rohtext als
// `ProposalSummary` mit `id`/`status: null` — nie eine erfundene Id.
import type { ProposalSummary } from "../../_lib/types";

const LINE_PATTERN = /^·\s*(\S+)\s*\[(\w+)]/u;

/**
 * Zerlegt den Text einer `list`-Antwort in einzelne [`ProposalSummary`].
 *
 * # Arguments
 * - `text` (`string`): der rohe `OperationResult.text`-Wert von
 *   `context-proposal list` (oder einer analogen `model-behavior-proposal`-
 *   Antwort im selben Format).
 *
 * # Returns
 * Eine Zeile pro nicht-leerer Textzeile, ohne die zusammenfassende erste
 * Kopfzeile herauszufiltern — die Anzeige zeigt jede Zeile unverändert über
 * `DataBlock`, das Erkennen von `id`/`status` ist nur eine Zusatzangabe.
 */
export function parseProposalListText(text: string): readonly ProposalSummary[] {
  return text
    .split("\n")
    .filter((line) => line.length > 0)
    .map((line) => {
      const match = LINE_PATTERN.exec(line);
      return {
        raw: line,
        id: match?.[1] ?? null,
        status: match?.[2] ?? null,
      };
    });
}
