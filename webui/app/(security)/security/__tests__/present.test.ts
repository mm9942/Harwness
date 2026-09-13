// Tests für die reinen Formatierungsfunktionen der Sicherheitszentrale
// (Knoten UI-05). Prüft insbesondere, dass angreiferkontrollierte Werte
// (Dateipfade, Freitext) unverändert und vollständig in den Anzeigezeilen
// landen — die eigentliche Text-vs-Markup-Sicherheit liegt bei `DataBlock`
// (siehe `SecurityListPanel.test.tsx`), diese Tests sichern nur, dass hier
// kein Feld verloren geht oder verändert wird.
import { describe, expect, it } from "vitest";

import {
  findingToLines,
  freezeToLines,
  hostSampleToLines,
  securityEventToLines,
  verdictToLines,
} from "../_lib/present";
import type { Finding, FreezeRecord, HostSample, SecurityEvent, SecurityVerdict } from "../_lib/types";

describe("findingToLines", () => {
  it("baut alle gemeinsamen Felder für einen Finding<Raw>", () => {
    const finding: Finding = {
      state: "raw",
      ruleId: "egress-flow",
      kind: "rule-triggered",
      severity: "high",
      hardness: "correlated",
      summary: "verdächtiger Egress zu 10.0.0.1:4444",
      observedAt: "1970-01-01T00:00:00Z",
    };
    const lines = findingToLines(finding);
    expect(lines).toContain("Regel: egress-flow");
    expect(lines).toContain("Zustand: raw");
    expect(lines.some((line) => line.includes("Befunds-ID"))).toBe(false);
  });

  it("ergänzt die Befunds-ID für Finding<RuleChecked>", () => {
    const finding: Finding = {
      state: "rule-checked",
      ruleId: "egress-flow",
      kind: "rule-triggered",
      severity: "high",
      hardness: "correlated",
      summary: "verdächtiger Egress",
      observedAt: "1970-01-01T00:00:00Z",
      findingId: "finding-001",
    };
    expect(findingToLines(finding)).toContain("Befunds-ID: finding-001");
  });

  it("ergänzt Befunds-ID und Triage-Ergebnis für Finding<Triaged>, auch mit HTML-artiger Zusammenfassung", () => {
    const finding: Finding = {
      state: "triaged",
      ruleId: "egress-flow",
      kind: "anomaly",
      severity: "critical",
      hardness: "inferred",
      summary: '<img src=x onerror=alert(1)>',
      observedAt: "1970-01-01T00:00:00Z",
      findingId: "finding-002",
      verdict: "confirmed",
    };
    const lines = findingToLines(finding);
    expect(lines).toContain("Befunds-ID: finding-002");
    expect(lines).toContain("Triage-Ergebnis: confirmed");
    expect(lines.some((line) => line === "Zusammenfassung: <img src=x onerror=alert(1)>")).toBe(true);
  });
});

describe("hostSampleToLines", () => {
  it("baut alle Felder eines Hostmesswerts", () => {
    const sample: HostSample = {
      sensor: "thermal-0",
      observedAt: "1970-01-01T00:00:00Z",
      metric: "temperature_celsius",
      value: 42.5,
    };
    expect(hostSampleToLines(sample)).toEqual([
      "Sensor: thermal-0",
      "Beobachtet: 1970-01-01T00:00:00Z",
      "Messgröße: temperature_celsius",
      "Wert: 42.5",
    ]);
  });
});

describe("securityEventToLines", () => {
  it("übernimmt einen angreiferkontrollierten Dateipfad unverändert bei file-write", () => {
    const event: SecurityEvent = {
      sensor: "fs-watch-0",
      observedAt: "1970-01-01T00:00:00Z",
      actor: { uid: 0, auid: 1000, cgroup: null },
      kind: { kind: "file-write", path: "../../etc/passwd" },
    };
    const lines = securityEventToLines(event);
    expect(lines).toContain("Pfad: ../../etc/passwd");
    expect(lines).toContain("Akteur (uid): 0");
    expect(lines).toContain("Akteur (auid): 1000");
  });

  it("zeigt 'unbekannt' für einen fehlenden Akteur, statt ihn zu erfinden", () => {
    const event: SecurityEvent = {
      sensor: "audit-0",
      observedAt: "1970-01-01T00:00:00Z",
      actor: null,
      kind: { kind: "sensor-degraded", sensor: "thermal-0" },
    };
    const lines = securityEventToLines(event);
    expect(lines).toContain("Akteur (uid): unbekannt");
    expect(lines).toContain("Akteur (auid): unbekannt");
    expect(lines).toContain("Betroffener Sensor: thermal-0");
  });

  it("baut Ziel und Port für egress-flow", () => {
    const event: SecurityEvent = {
      sensor: "netmon-0",
      observedAt: "1970-01-01T00:00:00Z",
      actor: null,
      kind: { kind: "egress-flow", destination: "10.0.0.1", port: 4444 },
    };
    const lines = securityEventToLines(event);
    expect(lines).toContain("Ziel: 10.0.0.1");
    expect(lines).toContain("Port: 4444");
  });
});

describe("verdictToLines", () => {
  it("baut alle Felder eines Agenten-Urteils, inklusive Freitext-Begründung", () => {
    const verdict: SecurityVerdict = {
      findingId: "finding-002",
      boundEvidenceDigest: "sha256:abc",
      classification: "confirmed",
      severity: "critical",
      rationale: "Kommandozeile deckt sich mit bekanntem Exfiltrationsmuster.",
      suggestedResponse: "contain",
      issuedBy: "family/security#0",
      issuedAt: "1970-01-01T00:00:00Z",
    };
    const lines = verdictToLines(verdict);
    expect(lines).toContain("Einstufung: confirmed");
    expect(lines).toContain("Vorschlag: contain");
    expect(lines).toContain(
      "Begründung: Kommandozeile deckt sich mit bekanntem Exfiltrationsmuster.",
    );
  });

  it("zeigt 'keiner' wenn kein Vorschlag vorliegt", () => {
    const verdict: SecurityVerdict = {
      findingId: "finding-003",
      boundEvidenceDigest: "sha256:def",
      classification: "benign",
      severity: "info",
      rationale: "Kein Handlungsbedarf.",
      suggestedResponse: null,
      issuedBy: "family/security#0",
      issuedAt: "1970-01-01T00:00:00Z",
    };
    expect(verdictToLines(verdict)).toContain("Vorschlag: keiner");
  });
});

describe("freezeToLines", () => {
  it("zeigt 'nie' für einen Freeze ohne Ablaufdatum, nicht 'null'", () => {
    const freeze: FreezeRecord = {
      cgroup: "cgroup-123",
      finding: "finding-002",
      frozenAt: "1970-01-01T00:00:00Z",
      expiresAt: null,
      resolvedAt: null,
      resolutionOutcome: null,
    };
    const lines = freezeToLines(freeze);
    expect(lines).toContain("Läuft ab am: nie (kein Ablaufdatum gesetzt)");
    expect(lines).toContain("Aufgelöst am: noch nicht aufgelöst");
  });

  it("zeigt den Auflösungsgrund für einen aufgelösten Freeze", () => {
    const freeze: FreezeRecord = {
      cgroup: "cgroup-123",
      finding: "finding-002",
      frozenAt: "1970-01-01T00:00:00Z",
      expiresAt: "1970-01-02T00:00:00Z",
      resolvedAt: "1970-01-01T12:00:00Z",
      resolutionOutcome: "lifted",
    };
    const lines = freezeToLines(freeze);
    expect(lines).toContain("Auflösungsgrund: lifted");
    expect(lines).toContain("Aufgelöst am: 1970-01-01T12:00:00Z");
  });
});
