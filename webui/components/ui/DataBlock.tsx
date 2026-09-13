// Datenblock-Renderer — zeigt angreiferkontrollierten Text als Text.
//
// # Sicherheitsauflage (nicht Stilfrage)
// `harw-web` liefert Sensorfelder, Dateinamen, Prozessnamen und Log-Zeilen
// unverändert als `OpOutput::text` weiter (siehe `harw-web::server`-
// Moduldoku: „Kein Markdown-Rendering, keine aktiven Links."). Diese Werte
// sind vollständig angreiferkontrolliert. `DataBlock` ist die einzige
// vorgesehene Stelle, an der die UI solche Werte anzeigt — und sie ist so
// gebaut, dass ein Fehler hier **nicht ausdrückbar** ist, nicht nur
// unwahrscheinlich:
//
// - `value` ist als `string` typisiert, nicht als `ReactNode` oder
//   `{ __html: string }`. Es gibt keinen Prop, über den Markup
//   hereinkäme — der Typ selbst schließt es aus.
// - Diese Datei ruft `dangerouslySetInnerHTML` nirgends auf und bindet
//   keine Markdown-Bibliothek ein.
// - `value` wird ausschließlich als JSX-Text-Kind gerendert
//   (`<pre>{value}</pre>`) — React escapt Text-Kinder immer; es gibt in
//   diesem Baustein keinen Pfad, der `value` als Markup interpretiert.
// - Es gibt keine automatische Verlinkung: kein Regex, der aus einem
//   Text-Fragment ein `<a href>` baut. Ein `href` entsteht in dieser Datei
//   nur aus der optionalen, vom **Aufrufer** (nicht aus `value`) fest
//   codierten `label`-Prop — nie aus dem angezeigten Wert selbst — und
//   selbst dafür gibt es hier keinen `<a>`, nur eine Beschriftung.
//
// # Vertrauensklasse (AW4-01, Zwei-Block-Rendering)
// `harw_web::events::WebEventKind::OperationCompleted` und der JSON-Rumpf
// von `POST`/`GET`-Antworten (`harw_web::server::op_result_response`)
// liefern inzwischen ein `trust`-Feld — der veraltete Stand dieses
// Kommentars ("er fehlt hier bewusst, weil kein Signal existiert") gilt
// nicht mehr; siehe `harw-web/src/events.rs`, Abschnitt „Woher die
// `TrustClass` kommt". Diese Komponente liest das Feld jetzt über die
// optionale `trust`-Prop.
//
// Die drei Vertrauensklassen (identisch zu `harw_context::TrustClass`,
// absteigend vertrauenswürdig):
// - `"instruction"`: eine vom System selbst formulierte Anweisung.
// - `"evidence"`: ein belegtes Beweisstück mit bekannter, geprüfter
//   Herkunft (z. B. ein `EvidenceRef` mit gesetztem Digest).
// - `"data"`: angreiferkontrollierte Nutzlast ohne geprüfte Herkunft.
//
// **In der Praxis ist heute fast alles `"data"`**: `OpOutput` selbst trägt
// keine Klasse, `harw-web` setzt sie deshalb serverseitig auf `Data`
// (die niedrigste Klasse), solange keine Quelle mit bekannter Herkunft
// existiert — und von den `EvidenceRef`-Konstruktionsstellen setzen nur
// wenige einen Digest. Das ist kein Fehler dieser Komponente, sondern ihr
// Zweck: der Bediener soll sehen, dass praktisch alles `Data` ist, statt
// eine höhere Klasse vorzutäuschen. `DataBlock` erfindet deshalb **keine**
// Klasse, wenn `trust` fehlt — die Vorgabe ist `"data"`, dieselbe Vorgabe,
// die `harw-web` serverseitig verwendet (siehe `default_trust_class` dort).
//
// Die CSS-Token in `app/globals.css` decken bislang `--harw-trust-data-*`
// und `--harw-trust-evidence-*` ab, `instruction` (noch) nicht — diese
// Datei liegt außerhalb des Schreibbereichs dieses Knotens. `DataBlock`
// setzt das `data-harw-trust`-Attribut für alle drei Werte bereits jetzt,
// damit ein späterer Knoten nur noch CSS ergänzen muss, nicht diese
// Komponente.
import type { JSX } from "react";

/**
 * Vertrauensklasse eines angezeigten Textwerts — identisch zu
 * `harw_context::TrustClass`, wie sie `trust` auf
 * `WebEventKind::OperationCompleted` und im JSON-Antwortrumpf trägt
 * (kebab-case-Serialisierung: `"instruction"` | `"evidence"` | `"data"`).
 */
export type TrustClass = "instruction" | "evidence" | "data";

/** Vorgabeklasse, wenn der Aufrufer keine `trust`-Prop mitgibt — dieselbe
 * Vorgabe, die `harw-web` serverseitig für eine Quelle ohne bekannte
 * Herkunft verwendet (siehe Dateikopf). */
const DEFAULT_TRUST: TrustClass = "data";

export interface DataBlockProps {
  /** Der anzuzeigende, angreiferkontrollierte Rohtext. Niemals HTML/Markdown. */
  readonly value: string;
  /** Optionale, vom Aufrufer fest codierte Beschriftung (z. B. Feldname). */
  readonly label?: string;
  /**
   * Vertrauensklasse von `value`, z. B. aus `trust` auf
   * `WebEventKind::OperationCompleted` oder dem JSON-Antwortrumpf.
   * Vorgabe `"data"` (siehe Dateikopf), wenn der Aufrufer keine Klasse
   * kennt — niemals eine höhere Klasse erfinden.
   */
  readonly trust?: TrustClass;
  /** Zusätzliche CSS-Klasse für den äußeren Container. */
  readonly className?: string;
}

/**
 * Zeigt einen einzelnen Datenwert unverändert als Text an.
 *
 * # Description
 * Rendert `value` innerhalb eines `<pre>`-Elements als reinen Text-Knoten.
 * Zeilenumbrüche und Leerraum in `value` bleiben erhalten (CSS
 * `white-space: pre-wrap`); es findet keinerlei Interpretation als
 * Markdown, HTML oder Link statt (siehe Dateikopf für die vollständige
 * Begründung).
 *
 * # Arguments
 * - `value` (`string`): der Rohtext, z. B. ein Dateiname, eine Log-Zeile
 *   oder ein Sensorfeld aus `OpOutput::text`.
 * - `label` (`string`, optional): eine vom Aufrufer fest codierte
 *   Beschriftung, niemals aus `value` abgeleitet.
 * - `className` (`string`, optional): zusätzliche CSS-Klasse.
 *
 * # Returns
 * Ein `<div>` mit optionaler Beschriftung und dem Textinhalt.
 *
 * # Examples
 * ```tsx
 * <DataBlock label="Dateiname" value={untrustedFilename} />
 * ```
 */
export function DataBlock({ value, label, trust, className }: DataBlockProps): JSX.Element {
  const classes = ["harw-data-block", className].filter(Boolean).join(" ");
  return (
    <div className={classes} data-harw-trust={trust ?? DEFAULT_TRUST}>
      {label !== undefined ? (
        <span className="harw-data-block-label">{label}</span>
      ) : null}
      <pre className="harw-data-block-text">{value}</pre>
    </div>
  );
}

export interface DataBlockListProps {
  /** Mehrere angreiferkontrollierte Zeilen, z. B. ein Log-Ausschnitt. */
  readonly values: readonly string[];
  /** Optionale, vom Aufrufer fest codierte Beschriftung. */
  readonly label?: string;
  /**
   * Vertrauensklasse aller Zeilen in `values` (siehe [`DataBlockProps.trust`]).
   * Vorgabe `"data"`, wenn der Aufrufer keine Klasse kennt.
   */
  readonly trust?: TrustClass;
  /** Zusätzliche CSS-Klasse für den äußeren Container. */
  readonly className?: string;
}

/**
 * Zeigt mehrere Textzeilen (z. B. Log-Ausschnitt) unverändert als Text an.
 *
 * # Description
 * Verwendet [`DataBlock`] für den eigentlichen Text — jede Zeile bleibt ein
 * eigener Text-Knoten, keine Zeile wird zusammengeführt oder als Markup
 * interpretiert. Alle Zeilen teilen dieselbe `trust`-Klasse, weil sie
 * derselben Quelle entstammen (z. B. demselben `OpOutput::text`).
 *
 * # Arguments
 * - `values` (`readonly string[]`): die anzuzeigenden Zeilen.
 * - `label` (`string`, optional): Beschriftung des gesamten Blocks.
 * - `trust` (`TrustClass`, optional): Vertrauensklasse aller Zeilen; Vorgabe
 *   `"data"`.
 * - `className` (`string`, optional): zusätzliche CSS-Klasse.
 *
 * # Returns
 * Ein `<div>` mit einer Liste von [`DataBlock`]-Zeilen.
 */
export function DataBlockList({ values, label, trust, className }: DataBlockListProps): JSX.Element {
  const classes = ["harw-data-block-list", className].filter(Boolean).join(" ");
  return (
    <div className={classes} data-harw-trust={trust ?? DEFAULT_TRUST}>
      {label !== undefined ? (
        <span className="harw-data-block-label">{label}</span>
      ) : null}
      {values.map((line, index) => (
        // eslint-disable-next-line react/no-array-index-key -- Zeilen sind
        // reiner Anzeigetext ohne stabile Identität; Index ist hier sicher.
        <DataBlock key={index} value={line} trust={trust} />
      ))}
    </div>
  );
}
