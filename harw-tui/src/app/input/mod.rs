//! Input classification and shell follow-up.

use super::*;

/// Entscheidung, was mit einer abgeschickten Eingabezeile geschehen soll.
///
/// # Beschreibung
/// TTY-freies Zwischenergebnis von [`classify_line`], damit die Zeilen-Logik
/// ohne Terminal getestet werden kann. Der Event-Loop in `run_loop` wertet
/// diese Variante aus und delegiert an den jeweils passenden Zweig.
///
/// # Beispiele
/// ```ignore
/// use harw_tui::app::{classify_line, LineAction};
/// assert_eq!(classify_line(""), LineAction::Ignore);
/// assert_eq!(classify_line("/quit"), LineAction::Quit);
/// assert!(matches!(classify_line("Hallo"), LineAction::Chat(_)));
/// assert!(matches!(classify_line("/status"), LineAction::Command(_)));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LineAction {
    /// Loop verlassen — ausgelöst durch `/quit` oder `/exit`.
    Quit,
    /// Nichts tun — ausgelöst durch eine leere oder rein-whitespace Eingabe.
    Ignore,
    /// Einen Chat-Turn mit dem enthaltenen Text treiben.
    Chat(String),
    /// Eine `/command`-Zeile (oder `!`-Shell/Note/Mention) asynchron über die
    /// Operation-Adapter-Pipeline ausführen (`HarwEvent::Command` →
    /// `crate::command_exec::execute_command_as`); enthält die unveränderte
    /// Rohzeile. Die lokale [`CommandRegistry`] wird davon unabhängig
    /// weiterhin für die Popup-Autocomplete-Anzeige verwendet.
    Command(String),
    /// Eine Systemzeile im Scrollback anzeigen (Hinweis oder abgelehnte Eingabe).
    System(String),
}

/// Klassifiziert eine abgeschickte Zeile in eine [`LineAction`].
///
/// # Beschreibung
/// Reine, TTY-freie Funktion: nutzt [`crate::classify_input`] und übersetzt das
/// Ergebnis in die vom Event-Loop ausführbare Aktion. `/quit` und `/exit`
/// beenden den Loop; ein leerer Chat-Text wird ignoriert.
///
/// # Argumente
/// - `line` (`&str`): die vom Benutzer abgeschickte Rohzeile (ohne Zeilenende).
///
/// # Rückgabe
/// Die passende [`LineAction`].
#[must_use]
pub fn classify_line(line: &str) -> LineAction {
    let trimmed = line.trim();
    // Leere Zeile beendet nicht — sie wird ignoriert (codex-Muster).
    // Beendet wird über Ctrl+C 2×, Ctrl+D oder `/quit`/`/exit`.
    if trimmed.is_empty() {
        return LineAction::Ignore;
    }
    if trimmed == "/quit" || trimmed == "/exit" {
        return LineAction::Quit;
    }

    match crate::classify_input(line) {
        Ok(crate::Invocation::Chat(text)) => {
            if text.trim().is_empty() {
                LineAction::Ignore
            } else {
                LineAction::Chat(text)
            }
        }
        // Alle Nicht-Chat-Invocations (Command/Shell/Note/Mention) werden über
        // die CommandRegistry ausgeführt; die Rohzeile wird durchgereicht.
        Ok(_) => LineAction::Command(line.to_owned()),
        Err(error) => LineAction::System(format!("Eingabe abgelehnt: {error}")),
    }
}

/// Erkennt, ob der aktuell getippte (noch nicht abgeschickte) Composer-Text
/// im Shell-Modus gerendert werden soll (Plan Teil F: `!`-Modus wie in
/// Claude Code).
///
/// # Beschreibung
/// Reine Prädikatsfunktion: `true` genau dann, wenn `text` mit `'!'`
/// beginnt (das schließt `"!"` und `"!!"` selbst ein). Entscheidet
/// ausschließlich über die Composer-**Darstellung** (Rahmenfarbe, Titel,
/// Prompt in [`draw_viewport`]); die tatsächliche Ausführungssemantik
/// (Escape via `\!`, Admission, Capability) bleibt unverändert bei
/// [`crate::classify_input`] und [`classify_line`].
///
/// # Argumente
/// - `text` (`&str`): der aktuelle, noch nicht abgeschickte Composer-Inhalt.
///
/// # Rückgabe
/// `true`, wenn `text` mit `'!'` beginnt.
///
/// # Beispiele
/// ```ignore
/// use harw_tui::app::is_shell_mode_input;
/// assert!(is_shell_mode_input("!ls -la"));
/// assert!(!is_shell_mode_input("ls -la"));
/// ```
#[must_use]
pub(super) fn is_shell_mode_input(text: &str) -> bool {
    text.starts_with('!')
}

/// Obergrenze der im automatischen Folge-Turn eingebetteten `!`-Ausgabe
/// (Plan Teil F), in Unicode-Zeichen.
pub(super) const SHELL_TURN_OUTPUT_MAX_CHARS: usize = 8000;

/// Kappt `text` zeichengrenzen-sicher (nie mitten in einem UTF-8-Codepunkt)
/// auf höchstens `max_chars` Zeichen und hängt bei tatsächlicher Kappung den
/// Hinweis `"\n[gekürzt]"` an (Plan Teil F).
///
/// # Argumente
/// - `text` (`&str`): der zu kappende Text.
/// - `max_chars` (`usize`): Obergrenze in Unicode-Zeichen (nicht Bytes).
///
/// # Rückgabe
/// `text` unverändert, wenn er höchstens `max_chars` Zeichen hat; sonst die
/// ersten `max_chars` Zeichen plus `"\n[gekürzt]"`.
#[must_use]
pub(super) fn truncate_chars_with_marker(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut truncated: String = text.chars().take(max_chars).collect();
    truncated.push_str("\n[gekürzt]");
    truncated
}

/// Baut die Nutzereingabe für den automatischen Folge-Turn nach einem
/// `!`/`!!`-Befehl (Plan Teil F).
///
/// # Beschreibung
/// Reine Funktion, unabhängig vom Renderer-Zustand testbar. `output` wird
/// über [`truncate_chars_with_marker`] auf höchstens
/// [`SHELL_TURN_OUTPUT_MAX_CHARS`] Zeichen gekappt.
///
/// # Argumente
/// - `shell` (`&ShellRunOutcome`): Befehl (ohne führendes `!`), Exit-Code,
///   zusammengeführte Ausgabe, cwd und Endhinweis.
///
/// # Rückgabe
/// Die vollständige Nutzereingabe-Nachricht für den Folge-Turn, im Format
/// „Ich habe `! <command>` auf dem Host ausgeführt (cwd `<cwd>`, Exit <n>):"
/// (Runde 6, Teil B: „auf dem Host“ und cwd, damit der Agent nicht rät,
/// wo eine Datei gelandet ist), ggf. mit Endhinweis (Zeitlimit, Kappung),
/// gefolgt von einem Markdown-Codeblock (Sprache `text`) mit der (ggf.
/// gekappten) Ausgabe.
#[must_use]
pub(super) fn build_shell_turn_message(shell: &ShellRunOutcome) -> String {
    let truncated = truncate_chars_with_marker(&shell.combined_output, SHELL_TURN_OUTPUT_MAX_CHARS);
    let note = shell
        .note
        .as_deref()
        .map(|note| format!("; {note}"))
        .unwrap_or_default();
    format!(
        "Ich habe `! {}` auf dem Host ausgeführt (cwd `{}`, Exit {}{note}):\n```text\n{truncated}\n```",
        shell.command,
        shell.cwd.display(),
        shell.exit_code,
    )
}

/// Reiht nach einem tatsächlich gelaufenen `!`/`!!`-Befehl den automatischen
/// Folge-Turn ein (Plan Teil F).
///
/// # Beschreibung
/// Merkt `shell.command` als [`ChatApp::last_shell_command`] für den
/// nächsten `!!`-Aufruf, hinterlegt eine kompakte
/// [`ChatApp::pending_turn_user_cell_override`] (vermeidet doppelte Anzeige
/// der bereits in der Shell-Ergebniszelle sichtbaren Ausgabe — pragmatische
/// Entscheidung nach Plan Teil F, siehe Feld-Doku) und reicht die volle,
/// gekappte Turn-Nachricht ([`build_shell_turn_message`]) über denselben
/// Pfad weiter wie eine normal abgeschickte Nachricht: frei (`submitted ==
/// None`) wird sie sofort zum nächsten zu treibenden Turn; belegt
/// (`submitted.is_some()`) wird sie ans Ende von [`ChatApp::pending_turns`]
/// gehängt — identisch zu [`HarwEvent::Submit`] in `run_loop`.
///
/// # Argumente
/// - `app` (`&mut ChatApp`): nimmt `last_shell_command` und die
///   Anzeige-Überschreibung auf.
/// - `submitted` (`&mut Option<String>`): dieselbe lokale Variable, die
///   `run_loop` für den nächsten zu treibenden Turn verwendet.
/// - `shell` ([`ShellRunOutcome`]): das strukturierte Ergebnis des soeben
///   gelaufenen `!`/`!!`-Befehls.
pub(super) fn queue_shell_follow_up_turn(
    app: &mut ChatApp,
    submitted: &mut Option<String>,
    shell: ShellRunOutcome,
) {
    app.pending_turn_user_cell_override = Some(format!(
        "↳ Ausgabe von ! {} an den Agenten übergeben",
        shell.command
    ));
    // Runde 6, Teil B: Nachricht nennt Host, cwd und Exit-Code.
    let message = build_shell_turn_message(&shell);
    app.last_shell_command = Some(shell.command);
    if submitted.is_some() {
        app.pending_turns.push_back(message);
    } else {
        *submitted = Some(message);
    }
}
