// Tests für die reine Lücken-Reducer-Funktion (Knoten UI-02).
import { describe, expect, it } from "vitest";

import { appendGapNotice } from "../_lib/gapLog";

describe("appendGapNotice", () => {
  it("hängt eine Lücke an eine leere Liste an", () => {
    const result = appendGapNotice([], 3, "2026-09-01T00:00:00.000Z");
    expect(result).toHaveLength(1);
    expect(result[0]?.missingCount).toBe(3);
    expect(result[0]?.detectedAtIso).toBe("2026-09-01T00:00:00.000Z");
  });

  it("überschreibt keine bestehende Lücke, sondern hängt an", () => {
    const first = appendGapNotice([], 1, "t1");
    const second = appendGapNotice(first, 2, "t2");
    expect(second).toHaveLength(2);
    expect(second[0]?.missingCount).toBe(1);
    expect(second[1]?.missingCount).toBe(2);
  });

  it("vergibt jeder Lücke eine eigene id", () => {
    const first = appendGapNotice([], 1, "t1");
    const second = appendGapNotice(first, 1, "t1");
    expect(second[0]?.id).not.toBe(second[1]?.id);
  });
});
