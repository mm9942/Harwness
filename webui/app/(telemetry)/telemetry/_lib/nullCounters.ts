// Reine Klassifikationsfunktion für die Nullzähler-Leiste (Knoten UI-04).
//
// Der wichtigste Test dieses Knotens hängt an dieser einen Funktion: „alle
// Zähler null" (`kind: "all-zero"`) und „keine Zähler registriert"
// (`kind: "empty"`) sehen in einer naiven Anzeige gleich aus — beide zeigen
// nichts Auffälliges. `NullCounterRegistry::snapshot()` (siehe
// `harw-observe/src/null_counter.rs`-Moduldoku) liefert bei einer leeren
// Registrierung genau dieselbe Form (eine leere Liste) wie eine Anzeige, die
// schlicht keine Zähler kennt — der Unterschied zwischen „geprüft und in
// Ordnung" und „nie geprüft" ist strukturell nicht aus der Zahl ablesbar,
// nur aus der Länge der Liste selbst. Diese Funktion ist deshalb bewusst von
// jeder Netzwerk-/Fetch-Logik getrennt: eine reine Funktion von
// `readonly NullCounterSnapshotEntry[]` auf `NullCounterBarState`, ohne
// Seiteneffekt, ohne `EventSource`.
import type {
  MetricSnapshotEntry,
  NullCounterBarState,
  NullCounterSnapshotEntry,
} from "./types";

/**
 * Klassifiziert einen Nullzähler-Schnappschuss für die Leiste.
 *
 * # Description
 * Drei Fälle, in fester Rangfolge geprüft:
 * 1. `entries` ist leer → `"empty"` — keine Registrierung, keine Aussage
 *    über irgendeine Invariante möglich. Das ist eine Meldung, kein grüner
 *    Haken (siehe Auftrag, Abschnitt 2).
 * 2. Kein Eintrag mit `count > 0` → `"all-zero"` — der Normalzustand.
 * 3. Mindestens ein Eintrag mit `count > 0` → `"violated"` — enthält
 *    zusätzlich `violated`, die Teilmenge der gebrochenen Zusagen, jede mit
 *    ihrem `invariant`-Text (Auftrag, Abschnitt 3: „zeig, welche").
 *
 * # Arguments
 * - `entries` (`readonly NullCounterSnapshotEntry[]`): der Schnappschuss,
 *   z. B. aus einer künftigen `telemetry.null_counters.snapshot`-Antwort.
 *
 * # Returns
 * Den klassifizierten [`NullCounterBarState`].
 *
 * # Examples
 * ```ts
 * classifyNullCounterSnapshot([]); // { kind: "empty" }
 * ```
 */
export function classifyNullCounterSnapshot(
  entries: readonly NullCounterSnapshotEntry[],
): NullCounterBarState {
  if (entries.length === 0) {
    return { kind: "empty" };
  }
  const violated = entries.filter((entry) => entry.count > 0);
  if (violated.length === 0) {
    return { kind: "all-zero", entries };
  }
  return { kind: "violated", entries, violated };
}

/** Namensräume, die laut AW5-06 (`harw-observe::routing`) nie über einen Web-Export laufen dürfen. */
const RESTRICTED_NAMESPACE_PREFIXES = ["security.", "warden."] as const;

/**
 * Prüft, ob ein Metrikname zu einem Namensraum gehört, der laut AW5-06
 * ausschließlich in den File-Sink gehört.
 *
 * # Description
 * `harw-observe` routet `security.*`/`warden.*` strukturell nicht nach
 * außen (siehe `harw-observe/src/routing.rs`); dass dieser Name hier
 * dennoch geprüft wird, ist eine zweite, UI-seitige Absicherung — begegnet
 * der Ansicht ein solcher Name trotzdem (z. B. weil eine künftige Operation
 * die Regel verletzt), ist das laut Auftrag ein Befund, kein Anzeigefall.
 *
 * # Arguments
 * - `metricName` (`string`): der zu prüfende Metrikname.
 *
 * # Returns
 * `true`, wenn `metricName` mit `"security."` oder `"warden."` beginnt.
 */
export function isRestrictedNamespaceMetric(metricName: string): boolean {
  return RESTRICTED_NAMESPACE_PREFIXES.some((prefix) => metricName.startsWith(prefix));
}

/**
 * Parst den Rohtext einer (künftigen) Nullzähler-Schnappschuss-Antwort.
 *
 * # Description
 * `OpOutput` liefert laut Vertrag ausschließlich `{ text: string }` — es
 * gibt kein dediziertes JSON-Antwortschema (siehe `@/lib/webClient`-Kopf).
 * Diese Funktion nimmt an, dass eine künftige `telemetry.null_counters.
 * snapshot`-Operation ihre Antwort als JSON-Array in genau diesem Textfeld
 * kodiert — die einzig plausible Form für strukturierte Daten in einem
 * reinen Textfeld. Diese Annahme ist **nicht** durch eine bestehende
 * Operation belegt (es gibt keine); sie ist eine Interpretation, an die
 * sich eine künftige Operation halten müsste, damit diese Ansicht sie ohne
 * Änderung konsumieren kann. Jede strukturelle Abweichung führt zu `null`,
 * nie zu einem stillschweigend falschen Ergebnis.
 *
 * # Arguments
 * - `text` (`string`): der Rohtext aus `OperationResult.text`.
 *
 * # Returns
 * Die geparste Liste, oder `null`, wenn `text` kein JSON-Array aus
 * wohlgeformten Einträgen ist.
 */
export function parseNullCounterSnapshotText(text: string): readonly NullCounterSnapshotEntry[] | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return null;
  }
  if (!Array.isArray(parsed)) {
    return null;
  }
  const entries: NullCounterSnapshotEntry[] = [];
  for (const item of parsed) {
    if (
      typeof item !== "object" ||
      item === null ||
      typeof (item as { name?: unknown }).name !== "string" ||
      typeof (item as { count?: unknown }).count !== "number" ||
      typeof (item as { invariant?: unknown }).invariant !== "string"
    ) {
      return null;
    }
    const record = item as { name: string; count: number; invariant: string };
    entries.push({ name: record.name, count: record.count, invariant: record.invariant });
  }
  return entries;
}

/**
 * Parst den Rohtext einer (künftigen) Metrik-Schnappschuss-Antwort.
 *
 * # Description
 * Dieselbe Annahme wie [`parseNullCounterSnapshotText`] — JSON-Array im
 * Textfeld, hier ohne `description` (siehe `MetricSnapshotEntry`-Kommentar
 * für die Begründung).
 *
 * # Arguments
 * - `text` (`string`): der Rohtext aus `OperationResult.text`.
 *
 * # Returns
 * Die geparste Liste, oder `null`, wenn `text` kein JSON-Array aus
 * wohlgeformten Einträgen ist.
 */
export function parseMetricsSnapshotText(text: string): readonly MetricSnapshotEntry[] | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return null;
  }
  if (!Array.isArray(parsed)) {
    return null;
  }
  const kinds = new Set(["Counter", "Gauge", "Histogram"]);
  const units = new Set(["Count", "Bytes", "Seconds", "Ratio", "Tokens", "Celsius"]);
  const entries: MetricSnapshotEntry[] = [];
  for (const item of parsed) {
    if (typeof item !== "object" || item === null) {
      return null;
    }
    const record = item as { name?: unknown; kind?: unknown; unit?: unknown; value?: unknown };
    if (
      typeof record.name !== "string" ||
      typeof record.kind !== "string" ||
      !kinds.has(record.kind) ||
      typeof record.unit !== "string" ||
      !units.has(record.unit) ||
      typeof record.value !== "string"
    ) {
      return null;
    }
    entries.push({
      name: record.name,
      kind: record.kind as MetricSnapshotEntry["kind"],
      unit: record.unit as MetricSnapshotEntry["unit"],
      value: record.value,
    });
  }
  return entries;
}
