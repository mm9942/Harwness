// SSE-Anbindung mit Lückenerkennung — Gegenstück zu `harw_web::events`.
//
// `harw-web` liefert über `GET /events` einen Ereignisstrom, in dem jedes
// [`WebEvent`] (siehe `harw_web::events`-Moduldoku) eine **monoton
// steigende** `sequence: u64` trägt, und meldet einen zu langsamen
// Abonnenten explizit über ein benanntes SSE-Ereignis `event: lagged` mit
// `data: {"skipped": n}` (siehe `harw_web::server::sse_response`). Ein
// Client, der diese Lücke nicht erkennt, hält eine unvollständige Ansicht
// für vollständig — dieselbe Begründung, mit der
// `harw-lens-query::resolve_index` einen Fehler statt einer leeren
// Trefferliste liefert (siehe Auftrag dieses Knotens).
//
// Die eigentliche Lückenerkennung ([`trackSequence`]) ist bewusst als reine
// Funktion von der Transportschicht ([`connectWebEventStream`], die einen
// echten `EventSource` öffnet) getrennt: reine Funktionen lassen sich ohne
// Netzwerkverbindung testen (siehe `lib/__tests__/sse.test.ts`) — dieser
// Knoten darf in keinem Test eine echte Verbindung aufbauen.

/** Fachliche Nutzlast eines Ereignisses — Spiegelbild von `WebEventKind`. */
export type WebEventKind =
  | { readonly type: "operation_completed"; readonly operation: string; readonly ok: boolean }
  | { readonly type: "heartbeat" };

/** Ein einzelnes, sequenznummeriertes Ereignis — Spiegelbild von `WebEvent`. */
export interface WebEvent {
  readonly sequence: number;
  readonly kind: WebEventKind;
}

/** Ergebnis eines Sequenznummer-Checks — siehe [`trackSequence`]. */
export interface SequenceCheck {
  /** Aktualisierter Verfolgungszustand; an den nächsten Aufruf weiterreichen. */
  readonly state: SequenceState;
  /**
   * `null`, wenn keine Lücke erkannt wurde; sonst die Anzahl der zwischen
   * der zuletzt gesehenen und dieser Sequenznummer fehlenden Ereignisse.
   */
  readonly missing: number | null;
}

/** Verfolgungszustand der Sequenznummer — undurchsichtig für Aufrufer. */
export interface SequenceState {
  readonly lastSequence: number | null;
}

/** Der anfängliche Verfolgungszustand vor dem ersten empfangenen Ereignis. */
export const INITIAL_SEQUENCE_STATE: SequenceState = { lastSequence: null };

/**
 * Prüft eine neu eingetroffene Sequenznummer gegen die zuletzt gesehene.
 *
 * # Description
 * Reine Funktion, keine Ein-/Ausgabe. Das allererste Ereignis eines
 * Abonnements erzeugt nie eine Lücke — es gibt noch keinen Vorgänger, gegen
 * den verglichen werden könnte. Jede folgende Sequenznummer, die nicht
 * genau eins über der vorherigen liegt, meldet die Anzahl der dazwischen
 * fehlenden Ereignisse; ein `event: lagged` mit explizitem `skipped`-Wert
 * (siehe [`applyLaggedNotice`]) ist der Fall, in dem der Server diese Zahl
 * bereits kennt und nicht neu berechnet werden muss.
 *
 * # Arguments
 * - `state` (`SequenceState`): Zustand vor diesem Ereignis.
 * - `incoming` (`number`): die `sequence` des neu eingetroffenen Ereignisses.
 *
 * # Returns
 * Den aktualisierten Zustand plus `missing` (siehe [`SequenceCheck`]).
 *
 * # Examples
 * ```ts
 * let state = INITIAL_SEQUENCE_STATE;
 * ({ state } = trackSequence(state, 1)); // missing: null
 * const { missing } = trackSequence(state, 3); // missing: 1 (Ereignis 2 fehlt)
 * ```
 */
export function trackSequence(state: SequenceState, incoming: number): SequenceCheck {
  if (state.lastSequence === null) {
    return { state: { lastSequence: incoming }, missing: null };
  }
  const expected = state.lastSequence + 1;
  const missing = incoming > expected ? incoming - expected : null;
  return { state: { lastSequence: incoming }, missing };
}

/**
 * Verarbeitet eine explizite `event: lagged`-Meldung des Servers.
 *
 * # Description
 * `harw-web` kennt bei einem `Lagged`-Fehler die genaue Anzahl
 * übersprungener Ereignisse bereits (siehe `harw_web::events::WebEventReceiveError::Lagged`)
 * und schickt sie im Rumpf mit — dieser Pfad muss die Sequenznummer nicht
 * selbst rekonstruieren, sondern übernimmt `skipped` unverändert als Lücke.
 * Der Verfolgungszustand wird dabei bewusst **zurückgesetzt**
 * (`lastSequence: null`): welche Sequenznummer als nächstes eintrifft, ist
 * nach einer Lagged-Meldung nicht vorhersagbar, und ein falscher
 * Erwartungswert würde eine zweite, unechte Lücke erzeugen.
 *
 * # Arguments
 * - `skipped` (`number`): Anzahl der laut Server übersprungenen Ereignisse.
 *
 * # Returns
 * Den zurückgesetzten Zustand plus `missing: skipped`.
 */
export function applyLaggedNotice(skipped: number): SequenceCheck {
  return { state: { lastSequence: null }, missing: skipped };
}

/** Vom Aufrufer bereitgestellte Rückrufe für [`connectWebEventStream`]. */
export interface WebEventStreamHandlers {
  /** Aufgerufen für jedes erfolgreich dekodierte, lückenlos geprüfte Ereignis. */
  readonly onEvent?: (event: WebEvent) => void;
  /**
   * Aufgerufen, sobald eine Lücke erkannt wurde — vor oder anstelle von
   * `onEvent` für das auslösende Ereignis. `missingCount` ist niemals `0`.
   */
  readonly onGap?: (missingCount: number) => void;
  /** Aufgerufen bei einem nicht dekodierbaren Ereignis oder Verbindungsfehler. */
  readonly onError?: (error: unknown) => void;
}

/** Von [`connectWebEventStream`] zurückgegebenes Handle zum Schließen. */
export interface WebEventStreamHandle {
  /** Schließt die zugrunde liegende `EventSource`-Verbindung. */
  close(): void;
}

/**
 * Öffnet eine `EventSource`-Verbindung zu `GET /events` und meldet jede
 * erkannte Lücke über `handlers.onGap`.
 *
 * # Description
 * Dünner Adapter zwischen dem Browser-`EventSource`-API und den reinen
 * Funktionen [`trackSequence`]/[`applyLaggedNotice`] — die eigentliche
 * Lückenlogik bleibt dadurch ohne echte Verbindung testbar. Nur diese
 * Funktion berührt `EventSource`; sie wird in keinem Test dieses Knotens
 * aufgerufen (siehe Modultests: sie testen ausschließlich die reinen
 * Funktionen).
 *
 * # Arguments
 * - `url` (`string`): der `/events`-Endpunkt von `harw-web`.
 * - `handlers` (`WebEventStreamHandlers`): Rückrufe für Ereignis, Lücke, Fehler.
 *
 * # Returns
 * Ein [`WebEventStreamHandle`] zum Schließen der Verbindung.
 *
 * # Errors
 * Dekodier- und Verbindungsfehler werden nie geworfen, sondern über
 * `handlers.onError` gemeldet — ein Streaming-Client hat keinen sinnvollen
 * synchronen Rückgabewert für einen asynchronen Fehler.
 */
export function connectWebEventStream(
  url: string,
  handlers: WebEventStreamHandlers,
): WebEventStreamHandle {
  let state = INITIAL_SEQUENCE_STATE;
  const source = new EventSource(url);

  source.addEventListener("message", (raw: MessageEvent<string>) => {
    try {
      const event = JSON.parse(raw.data) as WebEvent;
      const check = trackSequence(state, event.sequence);
      state = check.state;
      if (check.missing !== null && handlers.onGap) {
        handlers.onGap(check.missing);
      }
      handlers.onEvent?.(event);
    } catch (error) {
      handlers.onError?.(error);
    }
  });

  source.addEventListener("lagged", (raw: MessageEvent<string>) => {
    try {
      const body = JSON.parse(raw.data) as { skipped: number };
      const check = applyLaggedNotice(body.skipped);
      state = check.state;
      handlers.onGap?.(body.skipped);
    } catch (error) {
      handlers.onError?.(error);
    }
  });

  source.addEventListener("error", (event: Event) => {
    handlers.onError?.(event);
  });

  return {
    close(): void {
      source.close();
    },
  };
}
