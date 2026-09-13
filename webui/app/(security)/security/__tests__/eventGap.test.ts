// Tests für den Lücken-Reducer der Sicherheitszentrale (Knoten UI-05).
// Gleiches Muster wie `(sessions)/_lib/eventGap.ts` — reine Funktion, kein
// `EventSource`, kein Netzwerk.
import { beforeEach, describe, expect, it } from "vitest";

import { appendSecurityGapNotice, resetSecurityGapNoticeIdsForTest } from "../_lib/eventGap";
import type { SecurityGapNotice } from "../_lib/types";

describe("appendSecurityGapNotice", () => {
  beforeEach(() => {
    resetSecurityGapNoticeIdsForTest();
  });

  it("hängt die erste Lücke an eine leere Liste an", () => {
    const notices = appendSecurityGapNotice([], 3, "1970-01-01T00:00:00Z");
    expect(notices).toEqual([{ id: 1, missingCount: 3, detectedAtIso: "1970-01-01T00:00:00Z" }]);
  });

  it("überschreibt keine bestehende Lücke, sondern hängt an", () => {
    let notices = appendSecurityGapNotice([], 1, "1970-01-01T00:00:00Z");
    notices = appendSecurityGapNotice(notices, 2, "1970-01-01T00:00:01Z");
    expect(notices).toHaveLength(2);
    expect(notices[0]).toEqual({ id: 1, missingCount: 1, detectedAtIso: "1970-01-01T00:00:00Z" });
    expect(notices[1]).toEqual({ id: 2, missingCount: 2, detectedAtIso: "1970-01-01T00:00:01Z" });
  });

  it("vergibt für jede neue Lücke eine eigene, aufsteigende ID", () => {
    let notices: readonly SecurityGapNotice[] = [];
    for (let i = 0; i < 5; i += 1) {
      notices = appendSecurityGapNotice(notices, 1);
    }
    expect(notices.map((notice) => notice.id)).toEqual([1, 2, 3, 4, 5]);
  });
});
