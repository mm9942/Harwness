// Fachliche Typen der Telemetrieansicht (Knoten UI-04).
//
// Diese Datei modelliert die Nullzähler- und Metrikformen so, wie sie aus
// `harw-observe` (`harw-observe/src/null_counter.rs`, `harw-observe/src/
// metric.rs`) tatsächlich hervorgehen könnten — **kein** erfundenes
// Wire-Format. Zum Zeitpunkt dieses Knotens existiert keine Operation, die
// diese Werte über HTTP liefert (`WEB_ROUTES` ist leer, siehe
// `_lib/webRoutes.ts`); diese Typen legen fest, was eine künftige Operation
// liefern müsste, damit die hier gebaute Ansicht ihre Auflage erfüllen kann.

/**
 * Ein einzelner Nullzähler mit Stand und Invarianten-Text.
 *
 * Spiegelt `NullCounter::name()`, `NullCounter::count()` und
 * `NullCounter::invariant()` (`harw-observe/src/null_counter.rs`). Die
 * Registry-`snapshot()`-Methode liefert nur `(name, count)`-Paare — der
 * Invarianten-Text müsste eine künftige Operation zusätzlich mitschicken,
 * sonst kann die Leiste Auflage 3 („zeig, welche [Zusage gebrochen wurde]")
 * nicht erfüllen.
 */
export interface NullCounterSnapshotEntry {
  /** `MetricKey::name` des Zählers, z. B. `"trust_block_violation_total"`. */
  readonly name: string;
  /** Aktueller Stand — erwartungsgemäß `0`. */
  readonly count: number;
  /** Die bewachte Invariante im Klartext (`NullCounter::invariant()`). */
  readonly invariant: string;
}

/** Klassifikation eines Nullzähler-Schnappschusses — siehe `_lib/nullCounters.ts`. */
export type NullCounterBarState =
  | { readonly kind: "empty" }
  | { readonly kind: "all-zero"; readonly entries: readonly NullCounterSnapshotEntry[] }
  | {
      readonly kind: "violated";
      readonly entries: readonly NullCounterSnapshotEntry[];
      readonly violated: readonly NullCounterSnapshotEntry[];
    };

/**
 * Eine einzelne Messgröße, wie `harw-observe::MetricKey`/`MetricValue` sie
 * beschreiben (`harw-observe/src/metric.rs`). **Kein** `description`-Feld:
 * `MetricKey` trägt keinen Beschreibungstext (bestätigter Befund aus AW3-04,
 * siehe Abschlussbericht dieses Knotens) — ein solches Feld hier zu
 * ergänzen hieße, einen Text zu erfinden, der im Code nicht existiert.
 */
export interface MetricSnapshotEntry {
  readonly name: string;
  readonly kind: "Counter" | "Gauge" | "Histogram";
  readonly unit: "Count" | "Bytes" | "Seconds" | "Ratio" | "Tokens" | "Celsius";
  /** Formatierter Wert — `Count(u64)`, `Gauge(f64)` oder `Observation(f64)` als Text. */
  readonly value: string;
}

/** Ein einzelnes, im UI gesammeltes Ereignis aus `GET /events`. */
export interface TelemetryFeedEvent {
  readonly id: number;
  readonly sequence: number;
  readonly summary: string;
  readonly receivedAtIso: string;
}

/** Eine im UI gesammelte Lücken-Meldung aus dem Ereignisstrom. */
export interface TelemetryGapNotice {
  readonly id: number;
  readonly missingCount: number;
  readonly detectedAtIso: string;
}
