// Tests für Feed- und Lücken-Reducer (Knoten UI-04).
import { beforeEach, describe, expect, it } from "vitest";

import {
  appendTelemetryFeedEvent,
  appendTelemetryGapNotice,
  resetTelemetryIdsForTest,
  summarizeTelemetryEvent,
} from "../eventGap";

beforeEach(() => {
  resetTelemetryIdsForTest();
});

describe("appendTelemetryGapNotice", () => {
  it("hängt eine Lücke an, ohne vorherige zu verdrängen", () => {
    let notices = appendTelemetryGapNotice([], 3, "t1");
    notices = appendTelemetryGapNotice(notices, 5, "t2");
    expect(notices).toEqual([
      { id: 1, missingCount: 3, detectedAtIso: "t1" },
      { id: 2, missingCount: 5, detectedAtIso: "t2" },
    ]);
  });
});

describe("summarizeTelemetryEvent", () => {
  it("beschreibt ein erfolgreiches operation_completed-Ereignis", () => {
    const summary = summarizeTelemetryEvent({
      sequence: 7,
      kind: { type: "operation_completed", operation: "session.list", ok: true },
    });
    expect(summary).toContain("session.list");
    expect(summary).toContain("erfolgreich");
    expect(summary).toContain("7");
  });

  it("beschreibt ein fehlgeschlagenes operation_completed-Ereignis", () => {
    const summary = summarizeTelemetryEvent({
      sequence: 8,
      kind: { type: "operation_completed", operation: "x", ok: false },
    });
    expect(summary).toContain("fehlgeschlagen");
  });

  it("beschreibt einen heartbeat", () => {
    const summary = summarizeTelemetryEvent({ sequence: 9, kind: { type: "heartbeat" } });
    expect(summary).toContain("Heartbeat");
    expect(summary).toContain("9");
  });
});

describe("appendTelemetryFeedEvent", () => {
  it("hängt Ereignisse in Empfangsreihenfolge an", () => {
    let feed = appendTelemetryFeedEvent([], { sequence: 1, kind: { type: "heartbeat" } }, "t1");
    feed = appendTelemetryFeedEvent(
      feed,
      { sequence: 2, kind: { type: "operation_completed", operation: "a", ok: true } },
      "t2",
    );
    expect(feed).toHaveLength(2);
    expect(feed[0]?.sequence).toBe(1);
    expect(feed[1]?.summary).toContain("a");
  });
});
