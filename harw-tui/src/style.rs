//! Theme-Erkennung und zentrale Stil-Definitionen für `harw-tui`.
//!
//! Spec-Quelle: `docs/design/tui-architecture.md`
//! Abschnitt 2.10 + SLICE 8 sowie
//! `docs/design/tui-architecture.md`.
//!
//! # Verantwortung
//! Dieses Modul ist die einzige Stelle, die `ratatui`-Stile und Farben
//! erzeugt. Es erkennt das Terminal-Farbschema zur Laufzeit und stellt
//! konfigurierte Stile für alle anderen Rendering-Module bereit.
//!
//! # Exportierte Typen
//! - [`Theme`] — Dark / Light Diskriminator.
//! - [`detect_theme`] — liest `COLORFGBG` und klassifiziert das Schema.
//! - [`is_light`], [`accent_color`], [`selected_style`], [`dim_style`] —
//!   theme-abhängige Stil-Helfer.
//! - [`luminance`] — gewichtete Helligkeit für spätere RGB-Hintergrund-Erkennung.
//!
//! # Concurrency
//! Alle Funktionen sind rein (keine inneren Locks, kein Shared State).
//! Sie können aus beliebigen Threads aufgerufen werden.
//!
//! # Fehler
//! Dieses Modul erzeugt keine Fehler; unbekannte Werte fallen auf sichere
//! Standardwerte zurück.
//!
//! # Examples
//! ```ignore
//! use harw_tui::style::{detect_theme, selected_style};
//!
//! let theme = detect_theme();
//! let style = selected_style(theme);
//! ```

use ratatui::style::{Color, Modifier, Style};

/// Diskriminator des Terminal-Farbschemas.
///
/// # Description
/// Wird von [`detect_theme`] zurückgegeben und steuert alle
/// theme-abhängigen Stil-Entscheidungen im Rendering-Layer.
/// Spec-Quelle: Abschnitt 2.10.
///
/// # Concurrency
/// `Copy`-Wert; uneingeschränkt thread-sicher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Theme {
    /// Dunkles Terminal (helle Vordergrundfarben auf dunklem Hintergrund).
    Dark,
    /// Helles Terminal (dunkle Vordergrundfarben auf hellem Hintergrund).
    Light,
}

/// Erkennt das aktive Terminal-Farbschema anhand der Umgebungsvariable
/// `COLORFGBG`.
///
/// # Description
/// `COLORFGBG` wird von vielen Terminals (rxvt, xterm, konsole, …) im
/// Format `"<fg>;<bg>"` gesetzt, wobei `<bg>` ein ANSI-Farbindex (0–15)
/// oder ein xterm-256-Farbindex ist.
///
/// **Heuristik**:
/// - `bg` ≥ 7 → überwiegend heller Hintergrund → [`Theme::Light`].
/// - `bg` < 7 oder nicht parsbar → dunkler Hintergrund → [`Theme::Dark`].
/// - Kein `COLORFGBG` gesetzt → [`Theme::Dark`] (sicherer Standardwert,
///   da die Mehrheit professioneller Terminals dunkel konfiguriert ist).
///
/// Diese Heuristik ist bewusst konservativ: Falsch-Positiv-Klassifikation
/// als Dark schadet weniger als invertierte Kontraste auf hellem Grund.
///
/// # Returns
/// [`Theme::Light`] wenn der Hintergrundindex ≥ 7, sonst [`Theme::Dark`].
///
/// # Concurrency
/// Rein; liest nur die Prozessumgebung, schreibt nichts.
///
/// # Examples
/// ```ignore
/// use harw_tui::style::detect_theme;
/// let theme = detect_theme();
/// ```
pub(crate) fn detect_theme() -> Theme {
    let Some(value) = std::env::var("COLORFGBG").ok() else {
        return Theme::Dark;
    };

    // Format: "fg;bg" oder seltener "fg;extra;bg" (z. B. „0;default;15").
    // Der bg-Index steht immer am Ende.
    let bg_str = value.rsplit(';').next().unwrap_or("");
    let bg_index: u32 = bg_str.trim().parse().unwrap_or(0);

    if bg_index >= 7 {
        Theme::Light
    } else {
        Theme::Dark
    }
}

/// Gibt zurück, ob das Theme hell ist.
///
/// # Description
/// Dünnschichtige Abfrage-Funktion; vermeidet direkte `match`-Verzweigungen
/// in Rendering-Code und wird von [`accent_color`] genutzt.
/// Spec-Quelle: `docs/design/tui-architecture.md`.
///
/// # Arguments
/// - `theme` (`Theme`): das zu prüfende Farbschema.
///
/// # Returns
/// `true` genau dann, wenn `theme == Theme::Light`.
///
/// # Concurrency
/// Rein; kein Shared State.
///
/// # Examples
/// ```ignore
/// use harw_tui::style::{Theme, is_light};
/// assert!(!is_light(Theme::Dark));
/// ```
pub(crate) fn is_light(theme: Theme) -> bool {
    theme == Theme::Light
}

/// Gibt die primäre Akzentfarbe für das Theme zurück.
///
/// # Description
/// Dark-Themes verwenden Cyan (hoher Kontrast auf dunklem Grund); Light-Themes
/// verwenden Blue (ausreichend Kontrast auf hellem Grund ohne Übersteuerung).
/// Nutzt [`is_light`] intern, um den Branch auszudrücken.
/// Spec-Quelle: Abschnitt 2.10.
///
/// # Arguments
/// - `theme` (`Theme`): das aktive Farbschema.
///
/// # Returns
/// [`Color::Cyan`] für [`Theme::Dark`], [`Color::Blue`] für [`Theme::Light`].
///
/// # Concurrency
/// Rein; kein Shared State.
///
/// # Examples
/// ```ignore
/// use harw_tui::style::{Theme, accent_color};
/// use ratatui::style::Color;
/// assert_eq!(accent_color(Theme::Dark), Color::Cyan);
/// ```
pub(crate) fn accent_color(theme: Theme) -> Color {
    if is_light(theme) {
        Color::Blue
    } else {
        Color::Cyan
    }
}

/// Baut den Stil für das aktuell markierte Listen-Element.
///
/// # Description
/// Kombiniert die theme-spezifische Akzentfarbe mit `BOLD`-Modifikator.
/// Ersetzt direkte `Style::default().fg(Color::Cyan).add_modifier(…)`-
/// Aufrufe im Rendering-Code. Spec-Quelle: Abschnitt 2.10.
///
/// # Arguments
/// - `theme` (`Theme`): das aktive Farbschema.
///
/// # Returns
/// [`Style`] mit Akzentfarbe als Vordergrund und `Modifier::BOLD`.
///
/// # Concurrency
/// Rein; kein Shared State.
///
/// # Examples
/// ```ignore
/// use harw_tui::style::{Theme, selected_style};
/// let style = selected_style(Theme::Dark);
/// ```
pub(crate) fn selected_style(theme: Theme) -> Style {
    Style::default()
        .fg(accent_color(theme))
        .add_modifier(Modifier::BOLD)
}

/// Gibt die Farbe für Nutzer-Nachrichten zurück (semantische Palette).
pub(crate) fn user_color(theme: Theme) -> Color {
    if is_light(theme) {
        Color::Rgb(0xB4, 0x53, 0x09)
    } else {
        Color::Rgb(0xF6, 0xC4, 0x53)
    }
}

/// Gibt die Farbe für Assistenten-Nachrichten zurück (semantische Palette).
pub(crate) fn assistant_color(theme: Theme) -> Color {
    if is_light(theme) {
        Color::Rgb(0x1D, 0x4E, 0xD8)
    } else {
        Color::Rgb(0x8C, 0xC8, 0xFF)
    }
}

/// Gibt die Farbe für Tool-Aufrufe zurück (semantische Palette).
pub(crate) fn tool_color(theme: Theme) -> Color {
    if is_light(theme) {
        Color::Rgb(0xC2, 0x41, 0x0C)
    } else {
        Color::Rgb(0xF2, 0xA6, 0x5A)
    }
}

/// Gibt die Farbe für Erfolgs-Zustände zurück (semantische Palette).
pub(crate) fn success_color(theme: Theme) -> Color {
    if is_light(theme) {
        Color::Rgb(0x04, 0x78, 0x57)
    } else {
        Color::Rgb(0x7D, 0xD3, 0xA5)
    }
}

/// Gibt die Farbe für Fehler-Zustände zurück (semantische Palette).
pub(crate) fn error_color(theme: Theme) -> Color {
    if is_light(theme) {
        Color::Rgb(0xDC, 0x26, 0x26)
    } else {
        Color::Rgb(0xF9, 0x70, 0x66)
    }
}

/// Gibt die Farbe für Warn-Zustände (nicht-fatal, aber Aufmerksamkeit erforderlich) zurück (semantische Palette).
pub(crate) fn warning_color(theme: Theme) -> Color {
    if is_light(theme) {
        Color::Rgb(0xB4, 0x53, 0x09)
    } else {
        Color::Rgb(0xE5, 0xB5, 0x67)
    }
}

/// Gibt die Akzentfarbe für den Composer-Shell-Modus zurück (Plan Teil F:
/// `!`-Modus wie in Claude Code).
///
/// # Description
/// Magenta/Pink, angelehnt an Claude Codes Bash-Modus-Akzent — bewusst
/// getrennt von [`accent_color`] (Cyan/Blue), damit der Shell-Modus im
/// Composer auf den ersten Blick von der normalen Chat-Eingabe zu
/// unterscheiden ist. Theme-abhängig für ausreichenden Kontrast auf hellem
/// wie dunklem Grund, analog zu den übrigen semantischen Farben in diesem
/// Modul (`user_color`, `tool_color`, …).
///
/// # Arguments
/// - `theme` (`Theme`): das aktive Farbschema.
///
/// # Returns
/// Eine Magenta/Pink-`Color`, dunkler auf [`Theme::Light`], heller auf
/// [`Theme::Dark`].
///
/// # Concurrency
/// Rein; kein Shared State.
pub(crate) fn shell_mode_color(theme: Theme) -> Color {
    if is_light(theme) {
        Color::Rgb(0xA3, 0x1D, 0x8C)
    } else {
        Color::Rgb(0xF2, 0x7B, 0xE0)
    }
}

/// Baut den Stil für den Composer-Shell-Modus (Rahmen, Titel, Prompt).
///
/// # Description
/// Nutzt [`shell_mode_color`] mit `BOLD`-Modifikator, analog zu
/// [`selected_style`]. Vom Rendering-Code (`app.rs::draw_viewport`) für
/// Rahmen-Farbe, Titel und den `!`-Prompt des Composers verwendet, sobald
/// der getippte Text mit `!` beginnt (`is_shell_mode_input`).
///
/// # Arguments
/// - `theme` (`Theme`): das aktive Farbschema.
///
/// # Returns
/// [`Style`] mit Shell-Modus-Akzentfarbe als Vordergrund und
/// `Modifier::BOLD`.
///
/// # Concurrency
/// Rein; kein Shared State.
pub(crate) fn shell_mode_style(theme: Theme) -> Style {
    Style::default()
        .fg(shell_mode_color(theme))
        .add_modifier(Modifier::BOLD)
}

/// Gibt die Rahmenfarbe zurück (semantische Palette).
pub(crate) fn border_color(theme: Theme) -> Color {
    if is_light(theme) {
        Color::Rgb(0x5B, 0x64, 0x72)
    } else {
        Color::Rgb(0x3C, 0x41, 0x4B)
    }
}

/// Gibt die abgedunkelte Farbe für sekundäre/inaktive Elemente zurück (semantische Palette).
pub(crate) fn dim_color(theme: Theme) -> Color {
    if is_light(theme) {
        Color::Rgb(0x5B, 0x64, 0x72)
    } else {
        Color::Rgb(0x7B, 0x7F, 0x87)
    }
}

/// Gibt den abgedunkelten Stil für nicht ausgewählte oder sekundäre Elemente.
///
/// # Description
/// Nutzt [`dim_color`], damit der Kontrast theme-abhängig angepasst wird
/// (statt eines festen [`Color::DarkGray`]). Spec-Quelle: Abschnitt 2.10.
///
/// # Arguments
/// - `theme` (`Theme`): das aktive Farbschema.
///
/// # Returns
/// [`Style`] mit theme-abhängiger gedimmter Vordergrundfarbe.
///
/// # Concurrency
/// Rein; kein Shared State.
///
/// # Examples
/// ```ignore
/// use harw_tui::style::{Theme, dim_style};
/// let style = dim_style(Theme::Dark);
/// ```
pub(crate) fn dim_style(theme: Theme) -> Style {
    Style::default().fg(dim_color(theme))
}

/// Baut den Stil für Nutzer-Nachrichten (semantische Palette).
pub(crate) fn user_style(theme: Theme) -> Style {
    Style::default().fg(user_color(theme))
}

/// Baut den Stil für Assistenten-Nachrichten (semantische Palette).
pub(crate) fn assistant_style(theme: Theme) -> Style {
    Style::default().fg(assistant_color(theme))
}

/// Baut den Stil für Tool-Aufrufe (semantische Palette).
pub(crate) fn tool_style(theme: Theme) -> Style {
    Style::default().fg(tool_color(theme))
}

/// Baut den Stil für Erfolgs-Zustände (semantische Palette).
pub(crate) fn success_style(theme: Theme) -> Style {
    Style::default().fg(success_color(theme))
}

/// Baut den Stil für Fehler-Zustände (semantische Palette).
pub(crate) fn error_style(theme: Theme) -> Style {
    Style::default().fg(error_color(theme))
}

/// Baut den Stil für Warn-Zustände (semantische Palette).
pub(crate) fn warning_style(theme: Theme) -> Style {
    Style::default().fg(warning_color(theme))
}

/// Berechnet die wahrgenommene Helligkeit eines RGB-Farbwerts.
///
/// # Description
/// Verwendet die standardisierte Luma-Formel (ITU-R BT.601):
/// `Y = 0.299·R + 0.587·G + 0.114·B`, gerundet auf den nächsten
/// ganzzahligen Wert. Das Ergebnis liegt im Bereich `[0, 255]`.
///
/// Diese Funktion ist für die spätere RGB-Hintergrunderkennung vorgesehen
/// (z. B. wenn das Terminal echte RGB-Hintergrundfarben meldet).
/// Spec-Quelle: `docs/design/tui-architecture.md` (blend-Muster).
///
/// # Arguments
/// - `r` (`u8`): Rotanteil (0–255).
/// - `g` (`u8`): Grünanteil (0–255).
/// - `b` (`u8`): Blauanteil (0–255).
///
/// # Returns
/// Gerundete Luma als [`u16`] im Bereich `[0, 255]`.
///
/// # Concurrency
/// Rein; keine Seiteneffekte.
///
/// # Examples
/// ```ignore
/// use harw_tui::style::luminance;
/// assert_eq!(luminance(0, 0, 0), 0);
/// assert_eq!(luminance(255, 255, 255), 255);
/// ```
// Vorgesehen für die RGB-Hintergrunderkennung in einem späteren Rendering-Slice
// (`docs/design/tui-architecture.md`). Bis dahin nur intern in Tests genutzt.
#[allow(dead_code)]
pub(crate) fn luminance(r: u8, g: u8, b: u8) -> u16 {
    // Multiplikation mit Skalierungsfaktor 1000, dann durch 1000 dividieren,
    // um Ganzzahl-Arithmetik ohne Gleitkomma zu verwenden.
    let luma = u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114;
    // Division durch 1000 + Rundung: (luma + 500) / 1000
    ((luma + 500) / 1000) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Prüft Helligkeit für Schwarz (untere Grenze).
    #[test]
    fn test_luminance_black_is_zero() {
        assert_eq!(luminance(0, 0, 0), 0);
    }

    /// Prüft Helligkeit für Weiß (obere Grenze, ~255).
    #[test]
    fn test_luminance_white_is_255() {
        // 0.299*255 + 0.587*255 + 0.114*255 = 255
        assert_eq!(luminance(255, 255, 255), 255);
    }

    /// Prüft, dass Cyan die Akzentfarbe für Dark-Themes ist.
    #[test]
    fn test_accent_color_dark_is_cyan() {
        assert_eq!(accent_color(Theme::Dark), Color::Cyan);
    }

    /// Prüft, dass Blue die Akzentfarbe für Light-Themes ist.
    #[test]
    fn test_accent_color_light_is_blue() {
        assert_eq!(accent_color(Theme::Light), Color::Blue);
    }

    /// Prüft, dass `is_light` für Light korrekt `true` zurückgibt.
    #[test]
    fn test_is_light_returns_true_for_light() {
        assert!(is_light(Theme::Light));
    }

    /// Prüft, dass `is_light` für Dark `false` zurückgibt.
    #[test]
    fn test_is_light_returns_false_for_dark() {
        assert!(!is_light(Theme::Dark));
    }

    /// Prüft, dass `selected_style` BOLD-Modifikator enthält.
    #[test]
    fn test_selected_style_contains_bold() {
        let style = selected_style(Theme::Dark);
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    /// Prüft, dass der Shell-Modus-Akzent (Plan Teil F) sich von der
    /// normalen Akzentfarbe unterscheidet — sonst wäre der Composer-Modus
    /// visuell nicht von normaler Chat-Eingabe zu unterscheiden.
    #[test]
    fn test_shell_mode_color_differs_from_accent_color() {
        assert_ne!(shell_mode_color(Theme::Dark), accent_color(Theme::Dark));
        assert_ne!(shell_mode_color(Theme::Light), accent_color(Theme::Light));
    }

    /// Prüft, dass `shell_mode_style` BOLD-Modifikator und die
    /// Shell-Modus-Akzentfarbe trägt.
    #[test]
    fn test_shell_mode_style_uses_shell_mode_color_and_bold() {
        let style = shell_mode_style(Theme::Dark);
        assert_eq!(style.fg, Some(shell_mode_color(Theme::Dark)));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    /// Prüft, dass `dim_style` die theme-abhängige gedimmte Farbe setzt.
    #[test]
    fn test_dim_style_is_dark_gray() {
        let style = dim_style(Theme::Dark);
        assert_eq!(style.fg, Some(Color::Rgb(0x7B, 0x7F, 0x87)));
    }

    /// Prüft die Luminanz-Heuristik für einen mittleren Grauwert.
    #[test]
    fn test_luminance_midgray() {
        // 128·(0.299+0.587+0.114) ≈ 128
        let l = luminance(128, 128, 128);
        assert!((120..=136).contains(&l), "erwartet ~128, erhalten {l}");
    }

    // Hinweis: Der Test für detect_theme ohne COLORFGBG wurde entfernt, da
    // std::env::remove_var und set_var in Edition 2024 unsafe sind und
    // #![forbid(unsafe_code)] in diesem Crate gilt. Ein Umgebungs-Mutations-Test
    // ist hier nicht darstellbar ohne unsafe; die Logik ist durch detect_theme()
    // selbst ausreichend abgesichert (parse-Fehler → Dark-Fallback).
}
