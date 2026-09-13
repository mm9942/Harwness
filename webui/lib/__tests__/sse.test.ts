// Lückenerkennungs-Tests — keine echte EventSource-Verbindung (siehe
// `sse.ts`-Kopf und die harte Regel dieses Knotens: kein Netzwerk in Tests).
import { describe, expect, it } from "vitest";

import {
  INITIAL_SEQUENCE_STATE,
  applyLaggedNotice,
  trackSequence,
} from "../sse";

describe("trackSequence", () => {
  it("meldet keine Lücke für das allererste Ereignis", () => {
    const { state, missing } = trackSequence(INITIAL_SEQUENCE_STATE, 1);
    expect(missing).toBeNull();
    expect(state.lastSequence).toBe(1);
  });

  it("meldet keine Lücke bei fortlaufenden Sequenznummern", () => {
    let state = INITIAL_SEQUENCE_STATE;
    ({ state } = trackSequence(state, 1));
    const result = trackSequence(state, 2);
    expect(result.missing).toBeNull();
  });

  it("erkennt eine fehlende Sequenznummer und zählt sie korrekt", () => {
    let state = INITIAL_SEQUENCE_STATE;
    ({ state } = trackSequence(state, 1));
    const result = trackSequence(state, 3);
    expect(result.missing).toBe(1);
    expect(result.state.lastSequence).toBe(3);
  });

  it("erkennt mehrere fehlende Sequenznummern auf einmal", () => {
    let state = INITIAL_SEQUENCE_STATE;
    ({ state } = trackSequence(state, 10));
    const result = trackSequence(state, 15);
    expect(result.missing).toBe(4);
  });

  it("bleibt lückenfrei über mehrere aufeinanderfolgende Aufrufe", () => {
    let state = INITIAL_SEQUENCE_STATE;
    const gaps: Array<number | null> = [];
    for (const sequence of [1, 2, 3, 4, 5]) {
      const result = trackSequence(state, sequence);
      state = result.state;
      gaps.push(result.missing);
    }
    expect(gaps).toEqual([null, null, null, null, null]);
  });
});

describe("applyLaggedNotice", () => {
  it("übernimmt die vom Server gemeldete Anzahl übersprungener Ereignisse", () => {
    const { missing, state } = applyLaggedNotice(7);
    expect(missing).toBe(7);
    expect(state.lastSequence).toBeNull();
  });

  it("setzt den Verfolgungszustand zurück, damit keine Folgelücke entsteht", () => {
    let state = INITIAL_SEQUENCE_STATE;
    ({ state } = trackSequence(state, 5));
    ({ state } = applyLaggedNotice(3));
    // Nach einer Lagged-Meldung ist die nächste Sequenznummer nicht
    // vorhersagbar — das erste danach empfangene Ereignis darf keine
    // zusätzliche, unechte Lücke erzeugen.
    const result = trackSequence(state, 42);
    expect(result.missing).toBeNull();
  });
});
