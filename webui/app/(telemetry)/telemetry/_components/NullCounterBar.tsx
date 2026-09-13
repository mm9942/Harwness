// Die Nullzähler-Leiste — Kern dieses Knotens (UI-04).
//
// # Die Denkarbeit hinter der Bauart
// Ein Nullzähler (`harw-observe::NullCounter`, siehe dessen Moduldoku) ist
// im Normalfall null. Drei Anforderungen bestimmen diese Komponente:
//
// 1. **Null ist der Normalzustand, darf aber nicht unsichtbar sein.** Der
//    `"all-zero"`-Fall rendert deshalb eine einzeilige, aber immer
//    sichtbare Zeile mit der Anzahl geprüfter Zähler — nicht nichts (das
//    würde niemand ansehen), nicht blinkend (das würde ignoriert). Details
//    zu jedem einzelnen Zähler stehen hinter einem `<details>`-Element,
//    damit die Ruhe des Normalzustands erhalten bleibt, ohne die
//    Information zu verstecken.
// 2. **„Alle null" und „keine Zähler registriert" sehen gleich aus und
//    sind es nicht.** `classifyNullCounterSnapshot` (`_lib/nullCounters.ts`)
//    trennt beide Fälle bereits vor dem Rendern; diese Komponente rendert
//    sie mit sichtbar unterschiedlicher Semantik (`role="status"` mit
//    neutralem Text vs. `role="alert"` mit Warntext) — der leere Fall ist
//    eine Meldung „nichts geprüft", kein grüner Haken.
// 3. **Ein Zähler über null ist ein Ereignis, keine Zahl.** Jeder verletzte
//    Zähler bekommt einen eigenen, hervorgehobenen Block mit Name, Stand
//    *und* dem `invariant`-Text — welche Zusage gebrochen wurde, nicht nur
//    wie oft.
//
// Namen und Invarianten-Texte laufen durch [`DataBlock`], obwohl sie in
// `harw-observe` `&'static str`-Konstanten sind (keine Laufzeit-Nutzereingabe)
// — Konsistenz mit der Auflage „Metriknamen und Labelwerte gehen durch
// DataBlock" ist billiger als eine Ausnahme für den heutigen Fall zu
// begründen, und schützt automatisch, falls ein künftiger Zähler seinen
// Namen doch einmal aus Konfigurationsdaten bezieht.
import type { JSX } from "react";

import { DataBlock } from "@/components/ui/DataBlock";

import { isRestrictedNamespaceMetric } from "../_lib/nullCounters";
import type { NullCounterBarState } from "../_lib/types";

export interface NullCounterBarProps {
  readonly state: NullCounterBarState;
}

/**
 * Rendert die Nullzähler-Leiste für den gegebenen klassifizierten Zustand.
 *
 * # Returns
 * Ein `<section>` — Inhalt und Semantik hängen von `state.kind` ab (siehe
 * Dateikopf).
 */
export function NullCounterBar({ state }: NullCounterBarProps): JSX.Element {
  if (state.kind === "empty") {
    return (
      <section
        className="harw-null-counter-bar harw-null-counter-bar--empty"
        role="status"
        data-testid="null-counter-bar"
        data-state="empty"
        aria-label="Nullzähler-Leiste"
      >
        <DataBlock
          label="Nullzähler"
          value="Keine Nullzähler registriert. Das ist keine Bestätigung, dass alle Invarianten halten — es wurde schlicht keine geprüft."
        />
      </section>
    );
  }

  const restricted = state.entries.filter((entry) => isRestrictedNamespaceMetric(entry.name));

  if (state.kind === "all-zero") {
    return (
      <section
        className="harw-null-counter-bar harw-null-counter-bar--ok"
        role="status"
        data-testid="null-counter-bar"
        data-state="all-zero"
        aria-label="Nullzähler-Leiste"
      >
        <p className="harw-null-counter-summary">
          {state.entries.length} Nullzähler geprüft, alle bei 0.
        </p>
        <details>
          <summary>Einzelne Zähler anzeigen</summary>
          {state.entries.map((entry) => (
            <DataBlock key={entry.name} label={entry.name} value={`Stand 0 — ${entry.invariant}`} />
          ))}
        </details>
        {restricted.length > 0 ? (
          <RestrictedNamespaceNotice entries={restricted} />
        ) : null}
      </section>
    );
  }

  return (
    <section
      className="harw-null-counter-bar harw-null-counter-bar--violated"
      role="alert"
      data-testid="null-counter-bar"
      data-state="violated"
      aria-label="Nullzähler-Leiste"
    >
      <p className="harw-null-counter-summary">
        {state.violated.length} von {state.entries.length} Nullzählern über 0 — mindestens eine
        Invariante wurde verletzt.
      </p>
      {state.violated.map((entry) => (
        <DataBlock
          key={entry.name}
          label={`Verletzung: ${entry.name}`}
          value={`Stand ${entry.count} (erwartet: 0). Verletzte Zusage: ${entry.invariant}`}
        />
      ))}
      {restricted.length > 0 ? <RestrictedNamespaceNotice entries={restricted} /> : null}
    </section>
  );
}

/**
 * Meldet Nullzähler aus einem laut AW5-06 gesperrten Namensraum
 * (`security.*`/`warden.*`), die dieser Ansicht dennoch begegnet sind.
 *
 * # Description
 * Laut Auftrag ist ein solcher Fund ein Befund, kein Anzeigefall — diese
 * Komponente zeigt ihn deshalb als eigenen, unübersehbaren Block statt ihn
 * stillschweigend wie jeden anderen Zähler zu rendern.
 */
function RestrictedNamespaceNotice({
  entries,
}: {
  readonly entries: readonly { readonly name: string }[];
}): JSX.Element {
  return (
    <div className="harw-null-counter-restricted-notice" role="alert" data-testid="restricted-namespace-notice">
      <DataBlock
        label="Befund: gesperrter Namensraum über Web erreicht"
        value={`Folgende Zähler stammen laut Namen aus "security."/"warden." und dürften laut AW5-06 nicht über einen Web-Export sichtbar sein: ${entries
          .map((entry) => entry.name)
          .join(", ")}.`}
      />
    </div>
  );
}
