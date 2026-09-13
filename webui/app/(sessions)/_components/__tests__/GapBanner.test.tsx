// Belegt, dass eine erkannte Lücke im Ereignisstrom sichtbar angezeigt wird
// (Knoten UI-03, Auflage „Der Ereignisstrom").
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { appendGapNotice, resetGapNoticeIdsForTest } from "../../_lib/eventGap";
import { GapBanner } from "../GapBanner";

describe("GapBanner", () => {
  it("zeigt nichts an, solange keine Lücke erkannt wurde", () => {
    const html = renderToStaticMarkup(<GapBanner notices={[]} />);
    expect(html).toBe("");
  });

  it("zeigt eine erkannte Lücke sichtbar mit der fehlenden Anzahl an", () => {
    resetGapNoticeIdsForTest();
    const notices = appendGapNotice([], 5, "2026-01-01T00:00:00.000Z");
    const html = renderToStaticMarkup(<GapBanner notices={notices} />);
    expect(html).toContain("5 Ereignis(se) fehlen");
    expect(html).toContain('role="alert"');
  });

  it("hängt mehrere Lücken an, statt sie zu ersetzen", () => {
    resetGapNoticeIdsForTest();
    let notices = appendGapNotice([], 1, "t0");
    notices = appendGapNotice(notices, 2, "t1");
    const html = renderToStaticMarkup(<GapBanner notices={notices} />);
    expect(html).toContain("1 Ereignis(se)");
    expect(html).toContain("2 Ereignis(se)");
  });
});
