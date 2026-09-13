//! Terminal-sichere Aufbereitung nicht vertrauenswürdiger Texte.
//!
//! # Verantwortung
//! Einzige Stelle der Crate, die entscheidet, welche Zeichen aus Modell-,
//! Werkzeug-, Plan- oder Nutzertext in einen ratatui-`Span` gelangen dürfen
//! (W1-08, Register G-007/G-008). Alle Verlaufszellen in `history_cell.rs`
//! rendern Fremdtext ausschließlich über die Funktionen dieses Moduls.
//!
//! # Warum überhaupt
//! ratatui 0.29 filtert Steuerzeichen im `Paragraph`-Pfad **nicht**:
//! `Span::styled_graphemes` verwirft nur `"\n"`
//! (`ratatui-0.29.0/src/text/span.rs:318-328`), `Paragraph::render_text`
//! überspringt nur Grapheme der Breite 0
//! (`ratatui-0.29.0/src/widgets/paragraph.rs:467-477`), und `unicode-width`
//! 0.2.0 gibt jedem Zeichen `<= U+00A0` außer `\r` vor `\n` die Breite 1
//! (`unicode-width-0.2.0/src/tables.rs:216-221`). Ein `ESC` landet damit als
//! Zellsymbol im Buffer und wird vom Crossterm-Backend roh ausgegeben
//! (`ratatui-0.29.0/src/backend/crossterm.rs:190`, `Print(cell.symbol())`) —
//! der Terminal-Emulator führt die Sequenz aus (OSC 52 Zwischenablage,
//! OSC 0/2 Fenstertitel, CSI Cursor-Bewegung, `ESC c` Reset …).
//!
//! # Regeln (für alle Varianten)
//! - **ESC-Sequenzen** (CSI `ESC [`, OSC `ESC ]`, DCS `ESC P`, SOS `ESC X`,
//!   PM `ESC ^`, APC `ESC _` mit Terminator BEL / `ESC \` / U+009C, sowie
//!   zweistellige `ESC Fp/Fe/Fs` und `ESC nF`): Im *Strip*-Modus wird eine
//!   **vollständige** Sequenz samt Nutzlast durch [`ESC_SEQUENCE_MARKER`]
//!   ersetzt. Eine unvollständige Sequenz wird nie „bis zum Ende“ verschluckt
//!   — sonst ließe sich der Rest eines Textes verstecken; stattdessen wird
//!   nur das `ESC` markiert und der Rest bleibt als druckbarer Text stehen.
//!   Im *Reveal*-Modus wird **nichts** verschluckt: jedes `ESC` wird markiert,
//!   die Folgezeichen bleiben sichtbar.
//! - **C0/C1-Steuerzeichen** (`char::is_control`, also U+0000–U+001F und
//!   U+007F–U+009F): als `⟨U+XXXX⟩` sichtbar markiert. Ausnahmen: `\n` bleibt
//!   in den mehrzeiligen Varianten erhalten, `\t` wird immer zu vier
//!   Leerzeichen (ein rohes Tab verschöbe die Terminal-Spalte gegenüber
//!   ratatuis Zellmodell), `\r\n` wird zu `\n`.
//! - **Unsichtbare Formatzeichen** (Bidi-Steuerung, Zero-Width, Tag-Zeichen …,
//!   siehe [`is_invisible_format_char`]): als `⟨U+XXXX⟩` sichtbar markiert,
//!   damit eine umgedrehte oder versteckte Darstellung auffällt.
//!
//! Invariante jeder Ausgabe: kein Zeichen mit `char::is_control()` außer `\n`
//! (nur mehrzeilige Varianten) und kein [`is_invisible_format_char`]-Zeichen.
//!
//! Eine Markierung selbst ist gewöhnlicher Text: ein Angreifer kann `⟨U+202E⟩`
//! auch wörtlich schreiben. Das erzeugt höchstens eine sichtbare, harmlose
//! Verwirrung, aber keine unsichtbare Täuschung.
//!
//! # Varianten
//! | Funktion | Zeilen | ESC-Sequenzen | Einsatz |
//! |---|---|---|---|
//! | [`sanitize_display`] | `\n` bleibt | Strip | Fließtext (Assistent, Reasoning, Nutzer) |
//! | [`sanitize_inline`] | `\n` → Leerzeichen | Strip | einzeilige Felder (Tool-Name, IDs, Ziele) |
//! | [`sanitize_reveal`] | `\n` bleibt | Reveal | Freigabe-Argumente |
//! | [`sanitize_reveal_inline`] | `\n` markiert | Reveal | Freigabe-Schlüssel und einzeilige Werte |

use std::fmt::Write as _;

/// Ersatztext für eine vollständig entfernte ESC-Sequenz (Strip-Modus).
pub(crate) const ESC_SEQUENCE_MARKER: &str = "⟨ESC⟩";

/// Ersatz für ein Tabulatorzeichen.
const TAB_REPLACEMENT: &str = "    ";

/// Umgang mit Zeilenumbrüchen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineMode {
    /// `\n` bleibt erhalten, `\r\n` wird zu `\n`.
    Multiline,
    /// `\n`/`\r` werden zu einem Leerzeichen.
    Inline,
    /// `\n`/`\r` werden sichtbar markiert (nichts darf eine Zeile vortäuschen).
    Marked,
}

/// Umgang mit ESC-Sequenzen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EscapeMode {
    /// Vollständige Sequenzen werden entfernt und durch [`ESC_SEQUENCE_MARKER`] ersetzt.
    Strip,
    /// Nichts wird entfernt; jedes Steuerzeichen wird markiert.
    Reveal,
}

/// Mehrzeiliger Fließtext: ESC-Sequenzen entfernt, Steuer-/Formatzeichen markiert.
///
/// # Argumente
/// - `text` (`&str`): nicht vertrauenswürdiger Text.
///
/// # Rückgabe
/// Terminal-sicherer Text; enthält als einziges Steuerzeichen `\n`.
///
/// # Beispiele
/// ```ignore
/// assert_eq!(sanitize_display("\u{1b}[31mROT\u{1b}[0m\nx"), "⟨ESC⟩ROT⟨ESC⟩\nx");
/// ```
pub(crate) fn sanitize_display(text: &str) -> String {
    sanitize_with(text, LineMode::Multiline, EscapeMode::Strip)
}

/// Einzeiliger Text: wie [`sanitize_display`], aber `\n`/`\r` werden zu Leerzeichen.
///
/// # Argumente
/// - `text` (`&str`): nicht vertrauenswürdiger Text.
///
/// # Rückgabe
/// Terminal-sicherer Text ohne jedes Steuerzeichen.
pub(crate) fn sanitize_inline(text: &str) -> String {
    sanitize_with(text, LineMode::Inline, EscapeMode::Strip)
}

/// Mehrzeilige Offenlegung für Freigabefragen: nichts wird verschluckt.
///
/// # Beschreibung
/// Was der Nutzer freigibt, muss er vollständig sehen. Deshalb entfernt diese
/// Variante auch keine ESC-Sequenz-Nutzlast (etwa eine OSC-52-Zeichenkette in
/// einem `shell.exec`-Befehl), sondern markiert nur das `ESC` selbst.
/// Ein alleinstehendes `\r` wird markiert statt umgebrochen.
///
/// # Rückgabe
/// Terminal-sicherer Text; enthält als einziges Steuerzeichen `\n`.
pub(crate) fn sanitize_reveal(text: &str) -> String {
    sanitize_with(text, LineMode::Multiline, EscapeMode::Reveal)
}

/// Einzeilige Offenlegung: wie [`sanitize_reveal`], aber auch `\n`/`\r` werden markiert.
///
/// # Beschreibung
/// Für Stellen, an denen ein Zeilenumbruch eine gefälschte Zeile erzeugen
/// könnte (Argument-Schlüssel, Werkzeugname in der Freigabefrage).
///
/// # Rückgabe
/// Terminal-sicherer Text ohne jedes Steuerzeichen.
pub(crate) fn sanitize_reveal_inline(text: &str) -> String {
    sanitize_with(text, LineMode::Marked, EscapeMode::Reveal)
}

/// Prüft, ob `c` ein unsichtbares bzw. darstellungsveränderndes Formatzeichen ist.
///
/// # Beschreibung
/// Pflichtumfang (W1-08): Bidi-Steuerung U+202A–U+202E, U+2066–U+2069,
/// U+200E/U+200F, U+061C; Zero-Width U+200B–U+200D, U+2060, U+FEFF.
/// Zusätzlich, weil sie dieselbe Täuschung erlauben:
/// - U+2028/U+2029 (Zeilen-/Absatztrenner, erzeugen in manchen Terminals Umbrüche),
/// - U+2061–U+2064 und U+206A–U+206F (unsichtbare Operatoren, veraltete Formatzeichen),
/// - U+00AD (weiches Trennzeichen), U+034F (Grapheme Joiner), U+180E,
/// - U+115F, U+1160, U+3164, U+FFA0 (Hangul-Füllzeichen, sehen wie Leerraum aus),
/// - U+FFF9–U+FFFB (Interlinear-Annotation),
/// - U+E0000–U+E007F (Tag-Zeichen; „ASCII-Schmuggel“ unsichtbarer Anweisungen).
///
/// Nebenwirkung: Emoji-ZWJ-Sequenzen (👨‍👩‍👧) werden in ihre Bestandteile mit
/// `⟨U+200D⟩` zerlegt angezeigt — bewusst, der Pflichtumfang verlangt U+200D.
///
/// # Rückgabe
/// `true`, wenn `c` markiert werden muss.
pub(crate) fn is_invisible_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{E0000}'..='\u{E007F}'
    )
}

/// Gemeinsamer Kern aller Varianten.
fn sanitize_with(text: &str, lines: LineMode, escapes: EscapeMode) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;

    while let Some(&c) = chars.get(index) {
        match c {
            '\u{1b}' => {
                if escapes == EscapeMode::Strip {
                    if let Some(len) = escape_sequence_len(&chars[index..]) {
                        out.push_str(ESC_SEQUENCE_MARKER);
                        index += len;
                        continue;
                    }
                }
                push_marker(&mut out, c);
            }
            '\n' => match lines {
                LineMode::Multiline => out.push('\n'),
                LineMode::Inline => out.push(' '),
                LineMode::Marked => push_marker(&mut out, c),
            },
            '\r' => {
                let crlf = chars.get(index + 1) == Some(&'\n');
                match (lines, escapes) {
                    (LineMode::Multiline, EscapeMode::Strip) => {
                        out.push('\n');
                        if crlf {
                            index += 1;
                        }
                    }
                    (LineMode::Multiline, EscapeMode::Reveal) if crlf => {
                        out.push('\n');
                        index += 1;
                    }
                    (LineMode::Inline, _) => {
                        out.push(' ');
                        if crlf {
                            index += 1;
                        }
                    }
                    // Offenlegung: ein alleinstehendes `\r` (Zeilenanfang
                    // überschreiben) wird immer markiert.
                    (LineMode::Multiline, EscapeMode::Reveal) | (LineMode::Marked, _) => {
                        push_marker(&mut out, c);
                    }
                }
            }
            '\t' => out.push_str(TAB_REPLACEMENT),
            c if c.is_control() || is_invisible_format_char(c) => push_marker(&mut out, c),
            c => out.push(c),
        }
        index += 1;
    }

    out
}

/// Hängt die sichtbare Markierung `⟨U+XXXX⟩` für `c` an.
fn push_marker(out: &mut String, c: char) {
    // `fmt::Write` für `String` kann nicht fehlschlagen.
    let _ = write!(out, "⟨U+{:04X}⟩", u32::from(c));
}

/// Länge einer **vollständigen** ESC-Sequenz ab `seq[0] == ESC`, sonst `None`.
///
/// # Beschreibung
/// Grammatik nach ECMA-48 / ISO 2022:
/// - `ESC [` Parameter (0x30–0x3F)* Zwischenbytes (0x20–0x2F)* Endbyte (0x40–0x7E),
/// - `ESC ] | P | X | ^ | _` Nutzlast bis BEL, U+009C oder `ESC \`
///   (ein anderes `ESC` beendet die Zeichenkette vor sich, wie in xterm),
/// - `ESC` Zwischenbytes (0x20–0x2F)+ Endbyte (0x30–0x7E) (`ESC ( B` …),
/// - `ESC` Endbyte (0x30–0x7E) (`ESC c`, `ESC 7`, `ESC \` …).
///
/// Jeder Abbruch (Textende, unzulässiges Byte) liefert `None`: dann wird nur
/// das `ESC` markiert, der Rest bleibt sichtbarer Text.
fn escape_sequence_len(seq: &[char]) -> Option<usize> {
    let introducer = *seq.get(1)?;
    match introducer {
        '[' => {
            let mut j = 2;
            while let Some(&c) = seq.get(j) {
                match c {
                    '\u{20}'..='\u{3F}' => j += 1,
                    '\u{40}'..='\u{7E}' => return Some(j + 1),
                    _ => return None,
                }
            }
            None
        }
        ']' | 'P' | 'X' | '^' | '_' => {
            let mut j = 2;
            while let Some(&c) = seq.get(j) {
                match c {
                    '\u{07}' | '\u{9C}' => return Some(j + 1),
                    '\u{1b}' if seq.get(j + 1) == Some(&'\\') => return Some(j + 2),
                    '\u{1b}' => return Some(j),
                    _ => j += 1,
                }
            }
            None
        }
        '\u{20}'..='\u{2F}' => {
            let mut j = 2;
            while let Some(&c) = seq.get(j) {
                if ('\u{20}'..='\u{2F}').contains(&c) {
                    j += 1;
                } else {
                    break;
                }
            }
            match seq.get(j) {
                Some(c) if ('\u{30}'..='\u{7E}').contains(c) => Some(j + 1),
                _ => None,
            }
        }
        '\u{30}'..='\u{7E}' => Some(2),
        _ => None,
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Prüft die Ausgabe-Invariante: kein Steuerzeichen außer (optional) `\n`,
    /// kein unsichtbares Formatzeichen.
    fn assert_terminal_safe(output: &str, newline_allowed: bool) {
        for c in output.chars() {
            if c == '\n' && newline_allowed {
                continue;
            }
            assert!(
                !c.is_control(),
                "Steuerzeichen U+{:04X} in Ausgabe {output:?}",
                u32::from(c)
            );
            assert!(
                !is_invisible_format_char(c),
                "Formatzeichen U+{:04X} in Ausgabe {output:?}",
                u32::from(c)
            );
        }
    }

    /// Tabelle: (Beschreibung, Eingabe, erwartete `sanitize_display`-Ausgabe).
    const DISPLAY_TABLE: &[(&str, &str, &str)] = &[
        ("CSI Farbe", "\u{1b}[31mROT\u{1b}[0m normal", "⟨ESC⟩ROT⟨ESC⟩ normal"),
        ("CSI Cursor", "a\u{1b}[2J\u{1b}[1;1Hb", "a⟨ESC⟩⟨ESC⟩b"),
        ("CSI privat", "\u{1b}[?1049hx", "⟨ESC⟩x"),
        ("OSC 52 mit BEL", "a\u{1b}]52;c;ZXZpbA==\u{07}b", "a⟨ESC⟩b"),
        ("OSC 52 mit ST", "a\u{1b}]52;c;ZXZpbA==\u{1b}\\b", "a⟨ESC⟩b"),
        ("OSC 52 mit C1-ST", "a\u{1b}]52;c;eA==\u{9c}b", "a⟨ESC⟩b"),
        ("OSC 0 Titel", "\u{1b}]0;pwned\u{07}ok", "⟨ESC⟩ok"),
        (
            "OSC 8 Hyperlink",
            "\u{1b}]8;;https://evil\u{1b}\\klick\u{1b}]8;;\u{1b}\\",
            "⟨ESC⟩klick⟨ESC⟩",
        ),
        ("DCS", "\u{1b}Pq#0;2;0;0;0\u{1b}\\x", "⟨ESC⟩x"),
        ("APC", "\u{1b}_Gf=100;AAAA\u{1b}\\y", "⟨ESC⟩y"),
        ("ESC c Reset", "x\u{1b}cy", "x⟨ESC⟩y"),
        ("ESC nF Zeichensatz", "\u{1b}(Bz", "⟨ESC⟩z"),
        // Unvollständige Sequenzen verstecken nie den Rest des Textes.
        ("OSC ohne Terminator", "a\u{1b}]0;rest bleibt", "a⟨U+001B⟩]0;rest bleibt"),
        ("CSI am Ende", "a\u{1b}[", "a⟨U+001B⟩["),
        ("CSI mit Nicht-ASCII", "\u{1b}[3ä", "⟨U+001B⟩[3ä"),
        ("ESC am Ende", "z\u{1b}", "z⟨U+001B⟩"),
        // C0 / C1
        ("BEL", "a\u{07}b", "a⟨U+0007⟩b"),
        ("NUL", "a\u{0}b", "a⟨U+0000⟩b"),
        ("Backspace", "abc\u{08}\u{08}", "abc⟨U+0008⟩⟨U+0008⟩"),
        ("DEL", "a\u{7f}", "a⟨U+007F⟩"),
        ("C1 CSI", "\u{9b}31mx", "⟨U+009B⟩31mx"),
        ("C1 NEL", "a\u{85}b", "a⟨U+0085⟩b"),
        // Bidi
        ("RLO", "rm -rf \u{202e}txt.exe", "rm -rf ⟨U+202E⟩txt.exe"),
        ("LRE/RLE/PDF/LRO", "\u{202a}\u{202b}\u{202c}\u{202d}", "⟨U+202A⟩⟨U+202B⟩⟨U+202C⟩⟨U+202D⟩"),
        ("Isolates", "\u{2066}a\u{2067}b\u{2068}c\u{2069}", "⟨U+2066⟩a⟨U+2067⟩b⟨U+2068⟩c⟨U+2069⟩"),
        ("LRM/RLM", "a\u{200e}b\u{200f}", "a⟨U+200E⟩b⟨U+200F⟩"),
        ("ALM", "a\u{061c}b", "a⟨U+061C⟩b"),
        // Zero-Width
        ("ZWSP/ZWNJ/ZWJ", "s\u{200b}u\u{200c}d\u{200d}o", "s⟨U+200B⟩u⟨U+200C⟩d⟨U+200D⟩o"),
        ("Word Joiner", "a\u{2060}b", "a⟨U+2060⟩b"),
        ("BOM", "\u{feff}text", "⟨U+FEFF⟩text"),
        ("Tag-Zeichen", "ok\u{e0041}\u{e007f}", "ok⟨U+E0041⟩⟨U+E007F⟩"),
        ("Zeilentrenner", "a\u{2028}b", "a⟨U+2028⟩b"),
        // Erlaubtes
        ("Zeilenumbruch bleibt", "a\nb", "a\nb"),
        ("CRLF", "a\r\nb", "a\nb"),
        ("CR allein", "a\rb", "a\nb"),
        ("Tab", "a\tb", "a    b"),
        ("Unicode unverändert", "Grüße 🎉 日本語 — ✓", "Grüße 🎉 日本語 — ✓"),
    ];

    #[test]
    fn test_sanitize_display_table() {
        for (name, input, expected) in DISPLAY_TABLE {
            let output = sanitize_display(input);
            assert_eq!(&output, expected, "Fall {name:?}");
            assert_terminal_safe(&output, true);
        }
    }

    #[test]
    fn test_sanitize_inline_folds_line_breaks_and_stays_safe() {
        assert_eq!(sanitize_inline("a\nb\r\nc\rd"), "a b c d");
        assert_eq!(sanitize_inline("x\u{1b}]52;c;AA==\u{07}y"), "x⟨ESC⟩y");
        for (_, input, _) in DISPLAY_TABLE {
            assert_terminal_safe(&sanitize_inline(input), false);
        }
    }

    #[test]
    fn test_sanitize_reveal_never_swallows_payload() {
        // Die OSC-52-Nutzlast bleibt vollständig lesbar.
        assert_eq!(
            sanitize_reveal("printf '\u{1b}]52;c;ZXZpbA==\u{07}'"),
            "printf '⟨U+001B⟩]52;c;ZXZpbA==⟨U+0007⟩'"
        );
        assert_eq!(sanitize_reveal("\u{1b}[31mX"), "⟨U+001B⟩[31mX");
        // Alleinstehendes CR wird markiert, CRLF bleibt ein Umbruch.
        assert_eq!(sanitize_reveal("safe\rrm -rf /"), "safe⟨U+000D⟩rm -rf /");
        assert_eq!(sanitize_reveal("a\r\nb"), "a\nb");
        assert_eq!(sanitize_reveal("\u{202e}x"), "⟨U+202E⟩x");
        for (_, input, _) in DISPLAY_TABLE {
            assert_terminal_safe(&sanitize_reveal(input), true);
        }
    }

    #[test]
    fn test_sanitize_reveal_inline_marks_line_breaks() {
        assert_eq!(
            sanitize_reveal_inline("path\n[y] freigeben"),
            "path⟨U+000A⟩[y] freigeben"
        );
        assert_eq!(sanitize_reveal_inline("a\r\nb"), "a⟨U+000D⟩⟨U+000A⟩b");
        for (_, input, _) in DISPLAY_TABLE {
            assert_terminal_safe(&sanitize_reveal_inline(input), false);
        }
    }

    #[test]
    fn test_every_c0_c1_and_format_char_is_neutralised() {
        let all: String = ('\u{0}'..='\u{a0}')
            .chain('\u{2000}'..='\u{206f}')
            .chain(['\u{feff}', '\u{e0001}', '\u{e0020}'])
            .collect();
        assert_terminal_safe(&sanitize_display(&all), true);
        assert_terminal_safe(&sanitize_inline(&all), false);
        assert_terminal_safe(&sanitize_reveal(&all), true);
        assert_terminal_safe(&sanitize_reveal_inline(&all), false);
    }
}
