// Tests für den reinen Lücken-Reducer (Knoten UI-03).
import { beforeEach, describe, expect, it } from "vitest";

import { appendGapNotice, resetGapNoticeIdsForTest } from "../eventGap";

describe("appendGapNotice", () => {
  beforeEach(() => {
    resetGapNoticeIdsForTest();
  });

  it("hängt eine Lücke an eine leere Liste an", () => {
    const result = appendGapNotice([], 3, "2026-01-01T00:00:00.000Z");
    expect(result).toHaveLength(1);
    expect(result[0]).toEqual({ id: 1, missingCount: 3, detectedAtIso: "2026-01-01T00:00:00.000Z" });
  });

  it("überschreibt vorherige Lücken nie, sondern hängt an", () => {
    let notices = appendGapNotice([], 1, "t0");
    notices = appendGapNotice(notices, 2, "t1");
    expect(notices.map((n) => n.missingCount)).toEqual([1, 2]);
  });

  it("vergibt für jede Lücke eine eigene, stabile Id", () => {
    let notices = appendGapNotice([], 1, "t0");
    notices = appendGapNotice(notices, 1, "t1");
    expect(notices[0]?.id).not.toBe(notices[1]?.id);
  });
});
