// Reine Formatierungsfunktionen für die Sicherheitszentrale (Knoten UI-05).
//
// Jede Funktion baut aus einem Fachobjekt eine Liste von Textzeilen
// (`"Feldname: Wert"`), die ausschließlich über `DataBlockList` angezeigt
// werden (siehe `_components/SecurityListPanel.tsx`). Es entsteht an keiner
// Stelle Markup — jede Zeile bleibt eine einzelne Zeichenkette, auch wenn
// `Wert` selbst wie ein Pfad oder ein HTML-Fragment aussieht (siehe
// `DataBlock.tsx`-Kopf: `value` ist ausnahmslos `string`, nie `ReactNode`).
// Diese Trennung von Formatierung (hier, reine Funktionen) und Rendering
// (dort, React) macht die Formatierung ohne DOM testbar.
import type {
  Finding,
  FreezeRecord,
  HostSample,
  SecurityEvent,
  SecurityVerdict,
} from "./types";

/** Baut die Anzeigezeilen für einen einzelnen Befund (`Finding<S>`). */
export function findingToLines(finding: Finding): readonly string[] {
  const lines = [
    `Regel: ${finding.ruleId}`,
    `Art: ${finding.kind}`,
    `Schwere: ${finding.severity}`,
    `Nachweishärte: ${finding.hardness}`,
    `Zustand: ${finding.state}`,
    `Beobachtet: ${finding.observedAt}`,
    `Zusammenfassung: ${finding.summary}`,
  ];
  if (finding.state === "rule-checked" || finding.state === "triaged") {
    lines.push(`Befunds-ID: ${finding.findingId}`);
  }
  if (finding.state === "triaged") {
    lines.push(`Triage-Ergebnis: ${finding.verdict}`);
  }
  return lines;
}

/** Baut die Anzeigezeilen für einen einzelnen Hostmesswert. */
export function hostSampleToLines(sample: HostSample): readonly string[] {
  return [
    `Sensor: ${sample.sensor}`,
    `Beobachtet: ${sample.observedAt}`,
    `Messgröße: ${sample.metric}`,
    `Wert: ${sample.value}`,
  ];
}

/** Baut die Anzeigezeilen für ein einzelnes Sicherheitsereignis. */
export function securityEventToLines(event: SecurityEvent): readonly string[] {
  const actor = event.actor;
  const auidText = actor !== null && actor.auid !== null ? String(actor.auid) : "unbekannt";
  const lines = [
    `Sensor: ${event.sensor}`,
    `Beobachtet: ${event.observedAt}`,
    `Akteur (uid): ${actor !== null ? String(actor.uid) : "unbekannt"}`,
    `Akteur (auid): ${auidText}`,
    `Art: ${event.kind.kind}`,
  ];
  switch (event.kind.kind) {
    case "process-exec":
      lines.push(`Pfad: ${event.kind.path}`, `Argv-Digest: ${event.kind.argvDigest}`);
      break;
    case "file-write":
      lines.push(`Pfad: ${event.kind.path}`);
      break;
    case "egress-flow":
      lines.push(`Ziel: ${event.kind.destination}`, `Port: ${event.kind.port}`);
      break;
    case "listener-opened":
      lines.push(`Port: ${event.kind.port}`);
      break;
    case "auth-event":
      lines.push(`Ausgang: ${event.kind.outcome}`);
      break;
    case "structure-drift":
      lines.push(`Schwere: ${event.kind.severity}`, `Detail: ${event.kind.detail}`);
      break;
    case "sensor-degraded":
      lines.push(`Betroffener Sensor: ${event.kind.sensor}`);
      break;
  }
  return lines;
}

/** Baut die Anzeigezeilen für ein einzelnes Agenten-Urteil (`SecurityVerdict`). */
export function verdictToLines(verdict: SecurityVerdict): readonly string[] {
  return [
    `Befunds-ID: ${verdict.findingId}`,
    `Gebundener Beleg (Digest): ${verdict.boundEvidenceDigest}`,
    `Einstufung: ${verdict.classification}`,
    `Schwere: ${verdict.severity}`,
    `Begründung: ${verdict.rationale}`,
    `Vorschlag: ${verdict.suggestedResponse ?? "keiner"}`,
    `Ausgestellt von: ${verdict.issuedBy}`,
    `Ausgestellt am: ${verdict.issuedAt}`,
  ];
}

/** Baut die Anzeigezeilen für einen einzelnen Freeze-Datensatz. Rein anzeigend. */
export function freezeToLines(freeze: FreezeRecord): readonly string[] {
  return [
    `Cgroup: ${freeze.cgroup}`,
    `Befund: ${freeze.finding}`,
    `Eingefroren am: ${freeze.frozenAt}`,
    `Läuft ab am: ${freeze.expiresAt ?? "nie (kein Ablaufdatum gesetzt)"}`,
    `Aufgelöst am: ${freeze.resolvedAt ?? "noch nicht aufgelöst"}`,
    `Auflösungsgrund: ${freeze.resolutionOutcome ?? "—"}`,
  ];
}
