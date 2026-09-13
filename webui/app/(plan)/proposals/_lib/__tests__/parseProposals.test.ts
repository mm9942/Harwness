// Tests für den reinen Vorschlagsliste-Parser (Knoten UI-03).
import { describe, expect, it } from "vitest";

import { parseProposalListText } from "../parseProposals";

describe("parseProposalListText", () => {
  it("erkennt id und status im dokumentierten Format", () => {
    const text = "1 Kontextprogramm-Vorschlag/Vorschläge:\n· context-proposal/example [Pending]\n";
    const result = parseProposalListText(text);
    expect(result).toHaveLength(2);
    expect(result[1]).toEqual({ raw: "· context-proposal/example [Pending]", id: "context-proposal/example", status: "Pending" });
  });

  it("liefert id/status als null, wenn das Format nicht erkennbar ist, statt zu raten", () => {
    const result = parseProposalListText("Keine Kontextprogramm-Vorschläge.");
    expect(result).toEqual([{ raw: "Keine Kontextprogramm-Vorschläge.", id: null, status: null }]);
  });

  it("ignoriert leere Zeilen", () => {
    const result = parseProposalListText("a\n\nb\n");
    expect(result).toHaveLength(2);
  });
});
