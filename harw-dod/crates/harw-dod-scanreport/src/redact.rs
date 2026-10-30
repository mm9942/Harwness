//! Redaktion von Freitext aus fremden Berichten: kappen, entschärfen.
//!
//! # Verantwortungsbereich
//! Besitzt genau eine Funktion, [`sanitize_and_truncate`], und die Grenze
//! [`MAX_DETAIL_LEN`], die sie durchsetzt. Jeder Freitext, der aus einem
//! Scanner-Bericht in ein [`harw_dod_signals::EventKind::StructureDrift`]
//! wandert, geht durch diese Funktion — nirgendwo sonst in dieser Crate wird
//! Freitext ungefiltert weitergereicht.
//!
//! # Warum beides in einem Schritt (Steuerzeichen **und** Länge)
//! Ein Bericht ist angreiferkontrolliert (siehe `crate`-Moduldoku): wer den
//! gescannten Code kontrolliert, kontrolliert auch Beschreibungstexte,
//! Regelnamen und Pfade, die ein Scanner in seinen Bericht übernimmt. Zwei
//! unabhängige Angriffsflächen folgen daraus, und diese Funktion schließt
//! beide:
//!
//! - **Steuerzeichen** (`\n`, `\r`, ESC/ANSI-Einleiter, C0/C1-Steuerbereich):
//!   Ein eingebetteter Zeilenumbruch kann eine Logzeile fälschen, die diesen
//!   Text später neben anderen Log-Einträgen zeilenweise ausgibt. Diese
//!   Funktion ersetzt jedes Zeichen, für das `char::is_control` `true`
//!   liefert, durch ein einzelnes Leerzeichen. Das genügt, um sowohl
//!   Zeilenumbrüche als auch ANSI-Fluchtsequenzen zu entschärfen: eine
//!   ANSI-Sequenz beginnt immer mit dem Steuerzeichen `ESC` (`\u{1B}`) — ohne
//!   dieses Einleitungszeichen bleibt der Rest der Sequenz (z. B. `[31m`)
//!   bedeutungslose, harmlose Druckzeichen. Diese Funktion ersetzt bewusst
//!   nur das Steuerzeichen selbst, nicht die Folgezeichen — das genügt für
//!   die Entschärfung und verändert nicht mehr Text als nötig.
//! - **Länge**: siehe [`MAX_DETAIL_LEN`].
//!
//! # Reihenfolge
//! Erst Steuerzeichen ersetzen, dann kürzen — nicht umgekehrt. Würde zuerst
//! gekürzt, könnte der Schnitt mitten in einer mehrbytigen ANSI-Sequenz
//! liegen und deren Rest so aussehen lassen wie harmloser Text, der er nicht
//! ist; nach dem Ersetzen ist jedes potenziell gefährliche Zeichen bereits
//! neutralisiert, bevor überhaupt geschnitten wird.
//!
//! # Nebenläufigkeit
//! Zustandslose, reine Funktion; `Send + Sync`, von jedem Thread parallel
//! aufrufbar.
//!
//! # Fehler
//! Keine — [`sanitize_and_truncate`] ist total: jede `&str`-Eingabe liefert
//! ein wohlgeformtes `String`-Ergebnis, auch leere Eingaben oder Eingaben,
//! die ausschließlich aus Steuerzeichen bestehen.
//!
//! # Examples
//! ```rust,ignore
//! // Privates Modul: dieses Beispiel dokumentiert das Verhalten, ist aber
//! // nicht von außerhalb der Crate aufrufbar.
//! let sanitized = sanitize_and_truncate("Zeile eins\nZeile zwei\u{1b}[31m gefärbt");
//! assert!(!sanitized.contains('\n'));
//! assert!(!sanitized.contains('\u{1b}'));
//! ```

/// Obergrenze für einen redigierten Freitext, in Bytes: **2048 (2 KiB)**.
///
/// # Begründung
/// SARIF-`message.text`-Felder und `cargo-audit`-Advisory-Beschreibungen
/// sind in der Praxis wenige hundert Bytes lang — ein, zwei Absätze
/// Fließtext. 2 KiB liegt gut das Drei- bis Zehnfache darüber: großzügig
/// genug, um eine legitime, ausführliche Beschreibung nicht sichtbar zu
/// verstümmeln, aber klein genug, dass ein Bericht mit einem absichtlich
/// zehn Megabyte großen Beschreibungsfeld (ein Speicherangriff, siehe
/// `crate`-Moduldoku) pro Ereignis nur diese begrenzte Menge in ein
/// [`harw_dod_signals::EventKind::StructureDrift`] überführt — unabhängig
/// davon, wie groß das Feld im Bericht selbst war. Die Gesamtgröße einer
/// gelesenen Berichtsdatei ist zusätzlich bereits durch
/// `harw_dod_readfs::MAX_READ_BYTES` (1 MiB) begrenzt; diese Grenze wirkt
/// eine Ebene darunter, pro Freitextfeld.
pub(crate) const MAX_DETAIL_LEN: usize = 2048;

/// Sichtbarer Marker, der ein gekürztes Ergebnis kennzeichnet.
///
/// Ein Literal aus dem eigenen Quelltext dieser Crate, kein Berichtsinhalt —
/// darf deshalb uneingeschränkt an das (bereits gekürzte) Ergebnis angehängt
/// werden.
const TRUNCATION_MARKER: &str = " …[gekuerzt]";

/// Entschärft Steuerzeichen und kappt auf [`MAX_DETAIL_LEN`] Bytes.
///
/// # Description
/// Ersetzt zunächst jedes Zeichen, für das `char::is_control` `true`
/// liefert (C0/C1-Steuerbereich inklusive `\n`, `\r`, Tabulator und dem
/// ANSI-Einleiter `ESC`), durch ein einzelnes Leerzeichen. Überschreitet das
/// Ergebnis danach [`MAX_DETAIL_LEN`] Bytes, wird es auf eine gültige
/// UTF-8-Zeichengrenze innerhalb der Grenze gekürzt und um
/// [`TRUNCATION_MARKER`] ergänzt — das Ergebnis kann dadurch geringfügig
/// länger als `MAX_DETAIL_LEN` sein, niemals aber die unbeschnittene Eingabe
/// überschreiten. Ein Ergebnis, das die Grenze nicht überschreitet, bleibt
/// unverändert (keine Zwangsanhängung des Markers).
///
/// # Arguments
/// - `input` (`&str`): der zu redigierende Freitext aus einem fremden
///   Bericht. Angreiferkontrolliert (siehe `crate`-Moduldoku) — diese
///   Funktion trifft keine Annahme über seinen Inhalt.
///
/// # Returns
/// Einen `String` ohne Steuerzeichen, dessen Byte-Länge höchstens
/// `MAX_DETAIL_LEN + TRUNCATION_MARKER.len()` beträgt.
///
/// # Concurrency
/// Zustandslos; von jedem Thread parallel aufrufbar.
pub(crate) fn sanitize_and_truncate(input: &str) -> String {
    let sanitized: String = input
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();

    if sanitized.len() <= MAX_DETAIL_LEN {
        return sanitized;
    }

    let budget = MAX_DETAIL_LEN.saturating_sub(TRUNCATION_MARKER.len());
    let mut end = budget.min(sanitized.len());
    while end > 0 && !sanitized.is_char_boundary(end) {
        end -= 1;
    }

    let mut truncated = sanitized[..end].to_owned();
    truncated.push_str(TRUNCATION_MARKER);
    truncated
}

#[cfg(test)]
mod tests {
    use super::{MAX_DETAIL_LEN, TRUNCATION_MARKER, sanitize_and_truncate};

    #[test]
    fn test_sanitize_and_truncate_leaves_short_plain_text_unchanged() {
        let input = "eine kurze, harmlose Beschreibung";
        assert_eq!(sanitize_and_truncate(input), input);
    }

    #[test]
    fn test_sanitize_and_truncate_replaces_newlines_with_spaces() {
        let input = "Zeile eins\nZeile zwei\r\nZeile drei";
        let out = sanitize_and_truncate(input);
        assert!(!out.contains('\n'));
        assert!(!out.contains('\r'));
        assert!(out.contains("Zeile eins"));
        assert!(out.contains("Zeile drei"));
    }

    #[test]
    fn test_sanitize_and_truncate_removes_ansi_escape_introducer() {
        let input = "gef\u{e4}rbt\u{1b}[31mrot\u{1b}[0m";
        let out = sanitize_and_truncate(input);
        assert!(!out.contains('\u{1b}'));
    }

    #[test]
    fn test_sanitize_and_truncate_caps_oversized_input_and_marks_it() {
        let huge = "A".repeat(MAX_DETAIL_LEN * 5);
        let out = sanitize_and_truncate(&huge);
        assert!(out.len() <= MAX_DETAIL_LEN + TRUNCATION_MARKER.len());
        assert!(out.len() < huge.len());
        assert!(out.ends_with(TRUNCATION_MARKER));
    }

    #[test]
    fn test_sanitize_and_truncate_does_not_fully_drop_oversized_input() {
        let huge = "B".repeat(MAX_DETAIL_LEN * 3);
        let out = sanitize_and_truncate(&huge);
        // Gekappt, nicht abgelehnt: das Ergebnis ist nicht leer und trägt
        // noch einen Teil des Originaltexts.
        assert!(!out.is_empty());
        assert!(out.starts_with('B'));
    }

    #[test]
    fn test_sanitize_and_truncate_handles_empty_input() {
        assert_eq!(sanitize_and_truncate(""), "");
    }

    #[test]
    fn test_sanitize_and_truncate_backs_off_from_multibyte_char_boundary() {
        // Ein mehrbytiges Zeichen (Euro-Zeichen, 3 Bytes in UTF-8) beginnt
        // genau ein Byte vor der naiven Kappgrenze `budget` -- die naive
        // Grenze läge damit mitten im Zeichen. Ohne Rückwärtssuche auf eine
        // gültige Zeichengrenze würde die anschließende Byte-Slice-Operation
        // auf einer ungültigen UTF-8-Grenze mit einer Panik abbrechen. Die
        // Rechnung ist exakt: `prefix` füllt die Grenze bis auf ein Byte,
        // das Euro-Zeichen beginnt dort und wird komplett verworfen.
        let budget = MAX_DETAIL_LEN - TRUNCATION_MARKER.len();
        let prefix = "x".repeat(budget - 1);
        let mut input = prefix.clone();
        input.push('€');
        input.push_str(&"y".repeat(100));

        let out = sanitize_and_truncate(&input);

        assert!(!out.contains('€'));
        assert_eq!(out, format!("{prefix}{TRUNCATION_MARKER}"));
    }
}
