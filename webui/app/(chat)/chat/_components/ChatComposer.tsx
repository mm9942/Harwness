// Eingabefeld für neue Chat-Nachrichten (Knoten UI-02).
import { useState, type FormEvent, type JSX } from "react";

export interface ChatComposerProps {
  readonly onSubmit: (text: string) => void;
  readonly disabled: boolean;
  readonly pending: boolean;
}

/**
 * Formular zum Verfassen und Absenden einer Chat-Nachricht.
 *
 * # Description
 * Hält den Eingabetext lokal (`useState`) und ruft `onSubmit` mit dem
 * getrimmten Text auf. Sendet nichts direkt — die eigentliche
 * Netzwerk-/Routen-Entscheidung trifft der Aufrufer (`ChatView`), damit
 * dieses Formular unabhängig von `WEB_ROUTES` testbar bleibt.
 *
 * # Arguments
 * - `onSubmit` (`(text: string) => void`): Rückruf mit dem eingegebenen Text.
 * - `disabled` (`boolean`): deaktiviert das Formular vollständig, z. B. wenn
 *   keine Sende-Route existiert.
 * - `pending` (`boolean`): zeigt an, dass eine vorherige Sendung noch läuft.
 *
 * # Returns
 * Ein `<form>` mit Textfeld und Sende-Schaltfläche.
 */
export function ChatComposer({ onSubmit, disabled, pending }: ChatComposerProps): JSX.Element {
  const [text, setText] = useState("");

  function handleSubmit(event: FormEvent<HTMLFormElement>): void {
    event.preventDefault();
    const trimmed = text.trim();
    if (trimmed.length === 0) {
      return;
    }
    onSubmit(trimmed);
    setText("");
  }

  return (
    <form className="harw-chat-composer" onSubmit={handleSubmit} data-testid="chat-composer">
      <label htmlFor="harw-chat-input" className="harw-chat-composer-label">
        Nachricht
      </label>
      <textarea
        id="harw-chat-input"
        value={text}
        disabled={disabled || pending}
        onChange={(event) => setText(event.target.value)}
        rows={3}
      />
      <button type="submit" disabled={disabled || pending || text.trim().length === 0}>
        {pending ? "Wird gesendet…" : "Senden"}
      </button>
    </form>
  );
}
