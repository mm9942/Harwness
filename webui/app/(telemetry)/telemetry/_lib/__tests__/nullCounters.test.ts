// Tests für die Klassifikation der Nullzähler-Leiste (Knoten UI-04).
//
// Der wichtigste Test dieses Knotens steht hier zuerst: „alle Zähler null"
// und „keine Zähler registriert" müssen unterscheidbare Ergebnisse liefern.
import { describe, expect, it } from "vitest";

import {
  classifyNullCounterSnapshot,
  isRestrictedNamespaceMetric,
  parseMetricsSnapshotText,
  parseNullCounterSnapshotText,
} from "../nullCounters";
import type { NullCounterSnapshotEntry } from "../types";

describe("classifyNullCounterSnapshot", () => {
  it("unterscheidet eine leere Registrierung von lauter Nullständen", () => {
    const empty = classifyNullCounterSnapshot([]);
    const allZero = classifyNullCounterSnapshot([
      { name: "a_total", count: 0, invariant: "a hält" },
    ]);

    expect(empty.kind).toBe("empty");
    expect(allZero.kind).toBe("all-zero");
    expect(empty).not.toEqual(allZero);
  });

  it("meldet einen verletzten Zähler mitsamt Invariante", () => {
    const entries: NullCounterSnapshotEntry[] = [
      { name: "ok_total", count: 0, invariant: "ok hält" },
      { name: "bad_total", count: 3, invariant: "kein Fragment mit TrustClass::Data im Instruktionsblock" },
    ];
    const result = classifyNullCounterSnapshot(entries);

    expect(result.kind).toBe("violated");
    if (result.kind === "violated") {
      expect(result.violated).toEqual([
        { name: "bad_total", count: 3, invariant: "kein Fragment mit TrustClass::Data im Instruktionsblock" },
      ]);
      expect(result.entries).toHaveLength(2);
    }
  });

  it("behandelt mehrere gleichzeitig verletzte Zähler vollständig", () => {
    const entries: NullCounterSnapshotEntry[] = [
      { name: "a_total", count: 1, invariant: "a hält" },
      { name: "b_total", count: 2, invariant: "b hält" },
    ];
    const result = classifyNullCounterSnapshot(entries);
    expect(result.kind).toBe("violated");
    if (result.kind === "violated") {
      expect(result.violated).toHaveLength(2);
    }
  });
});

describe("isRestrictedNamespaceMetric", () => {
  it("erkennt security.*- und warden.*-Namen", () => {
    expect(isRestrictedNamespaceMetric("security.block_total")).toBe(true);
    expect(isRestrictedNamespaceMetric("warden.ceiling_total")).toBe(true);
  });

  it("lässt andere Namensräume unberührt", () => {
    expect(isRestrictedNamespaceMetric("jobs_completed_total")).toBe(false);
  });
});

describe("parseNullCounterSnapshotText", () => {
  it("parst ein wohlgeformtes JSON-Array", () => {
    const text = JSON.stringify([{ name: "a_total", count: 0, invariant: "a hält" }]);
    expect(parseNullCounterSnapshotText(text)).toEqual([
      { name: "a_total", count: 0, invariant: "a hält" },
    ]);
  });

  it("liefert null bei ungültigem JSON", () => {
    expect(parseNullCounterSnapshotText("kein json")).toBeNull();
  });

  it("liefert null bei fehlendem Feld", () => {
    expect(parseNullCounterSnapshotText(JSON.stringify([{ name: "a" }]))).toBeNull();
  });

  it("liefert null, wenn die Antwort kein Array ist", () => {
    expect(parseNullCounterSnapshotText(JSON.stringify({ name: "a" }))).toBeNull();
  });
});

describe("parseMetricsSnapshotText", () => {
  it("parst ein wohlgeformtes JSON-Array", () => {
    const text = JSON.stringify([
      { name: "jobs_completed_total", kind: "Counter", unit: "Count", value: "42" },
    ]);
    expect(parseMetricsSnapshotText(text)).toEqual([
      { name: "jobs_completed_total", kind: "Counter", unit: "Count", value: "42" },
    ]);
  });

  it("liefert null bei unbekannter kind", () => {
    const text = JSON.stringify([
      { name: "x", kind: "Nonsense", unit: "Count", value: "1" },
    ]);
    expect(parseMetricsSnapshotText(text)).toBeNull();
  });
});
