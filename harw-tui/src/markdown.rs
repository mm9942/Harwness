//! Markdown-Darstellung für Assistenten-Nachrichten im Chat-Verlauf.
//!
//! # Verantwortung
//! Wandelt Markdown-Text (typischerweise eine gestreamte Modellantwort) in
//! gestylte ratatui-[`Line`]s um. Der Parser ist bewusst klein und
//! zeilenorientiert (keine zusätzliche Abhängigkeit wie `pulldown-cmark`):
//! jede Quellzeile ergibt höchstens eine Ausgabezeile, Fließtext wird nicht
//! umbrochen — das übernimmt der `Paragraph` des Aufrufers.
//!
//! # Unterstützte Konstrukte
//! - ATX-Überschriften (`#` bis `######`), fett in der Akzentfarbe.
//! - Aufzählungen (`-`, `*`, `+`) mit `• ` und Einrückung pro Ebene,
//!   nummerierte Listen (`1.` / `1)`).
//! - Zitate (`>`, auch verschachtelt) mit gedimmtem `│ `-Präfix.
//! - Umzäunte Code-Blöcke (```` ``` ```` / `~~~`) mit Sprach-Tag, gedimmter
//!   Randspalte `▏ ` und minimaler Hervorhebung für rust, python, js/ts, sh,
//!   toml und json. Ein nicht geschlossener Block (Streaming!) wird bis zum
//!   Textende als Code dargestellt.
//! - Inline: `` `code` ``, `**fett**`, `*kursiv*`/`_kursiv_`, `~~durch~~`,
//!   Links `[text](url)` → unterstrichener Text plus gedimmtes ` (url)`.
//! - Trennlinien (`---`, `***`, `___`) und Tabellen (`| a | b |`) mit
//!   ausgerichteten Spalten.
//!
//! # Sicherheit
//! Jeder Textbaustein läuft durch [`sanitize_inline`]; kein Steuerzeichen
//! aus Modelltext erreicht den Terminal-Buffer. Der Parser arbeitet
//! ausschließlich mit geprüften Indizes und kann bei beliebiger (auch
//! abgeschnittener) Eingabe nicht in Panik geraten.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::sanitize::sanitize_inline;
use crate::style::{self, Theme};

/// Breite einer horizontalen Trennlinie in Zellen.
const HR_WIDTH: usize = 40;
/// Randspalte vor jeder Code-Zeile.
const CODE_GUTTER: &str = "▏ ";
/// Präfix pro Zitat-Ebene.
const QUOTE_PREFIX: &str = "│ ";
/// Aufzählungszeichen ungeordneter Listen.
const BULLET: &str = "• ";
/// Spaltentrenner in Tabellen.
const TABLE_SEP: &str = " │ ";
/// Kreuzung von Spaltentrenner und Kopf-Unterstreichung.
const TABLE_CROSS: &str = "─┼─";
/// Einrückung pro Listenebene in der Ausgabe.
const LIST_INDENT: &str = "  ";

/// Rendert Markdown-Text in gestylte, terminal-sichere Zeilen.
///
/// # Argumente
/// - `text` (`&str`): nicht vertrauenswürdiger Markdown-Text, auch
///   unvollständig (z. B. mitten im Streaming abgeschnitten).
/// - `theme` ([`Theme`]): aktives Farbschema.
///
/// # Rückgabe
/// Eine Zeile pro Quellzeile (Zaunzeilen von Code-Blöcken entfallen,
/// Tabellen erhalten eine zusätzliche Kopf-Unterstreichung).
///
/// # Beispiele
/// ```ignore
/// let lines = render_markdown("# Titel\n- Punkt", Theme::Dark);
/// assert_eq!(lines.len(), 2);
/// ```
pub(crate) fn render_markdown(text: &str, theme: Theme) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut fence: Option<Fence> = None;
    let mut table: Vec<&str> = Vec::new();

    for raw in text.lines() {
        if let Some(open) = &fence {
            if open.is_closed_by(raw) {
                fence = None;
            } else {
                out.push(render_code_line(raw, open.lang, theme));
            }
            continue;
        }
        if is_table_line(raw) {
            table.push(raw);
            continue;
        }
        flush_table(&mut table, theme, &mut out);
        if let Some(opened) = Fence::open(raw) {
            fence = Some(opened);
            continue;
        }
        out.push(render_block_line(raw, theme));
    }
    flush_table(&mut table, theme, &mut out);
    out
}

// ---------------------------------------------------------------------------
// Blockebene
// ---------------------------------------------------------------------------

/// Rendert eine einzelne Nicht-Code-, Nicht-Tabellen-Zeile inklusive Zitat-Präfixen.
fn render_block_line(raw: &str, theme: Theme) -> Line<'static> {
    let (depth, inner) = strip_quote(raw);
    let mut spans = Vec::new();
    for _ in 0..depth {
        spans.push(Span::styled(QUOTE_PREFIX, style::dim_style(theme)));
    }
    spans.extend(render_simple(inner, theme));
    Line::from(spans)
}

/// Entfernt führende `>`-Marker und liefert deren Anzahl samt Restzeile.
fn strip_quote(raw: &str) -> (usize, &str) {
    let mut depth = 0;
    let mut rest = raw;
    while let Some(after) = rest.trim_start().strip_prefix('>') {
        depth += 1;
        rest = after.strip_prefix(' ').unwrap_or(after);
    }
    (depth, rest)
}

/// Rendert Überschrift, Trennlinie, Listenpunkt oder Absatz einer Zeile.
fn render_simple(line: &str, theme: Theme) -> Vec<Span<'static>> {
    let trimmed = line.trim_start();
    if trimmed.trim_end().is_empty() {
        return Vec::new();
    }
    if let Some(title) = heading_text(trimmed) {
        let base = Style::default()
            .fg(style::accent_color(theme))
            .add_modifier(Modifier::BOLD);
        return inline_spans(title, base, theme);
    }
    if is_hr(trimmed) {
        return vec![Span::styled("─".repeat(HR_WIDTH), style::dim_style(theme))];
    }

    let indent = indent_width(line);
    let marker_style = Style::default().fg(style::accent_color(theme));
    if let Some(content) = bullet_content(trimmed) {
        let prefix = format!("{}{BULLET}", LIST_INDENT.repeat(indent / 2));
        let mut spans = vec![Span::styled(prefix, marker_style)];
        spans.extend(inline_spans(content, Style::default(), theme));
        return spans;
    }
    if let Some((number, content)) = ordered_item(trimmed) {
        let prefix = format!("{}{number}. ", LIST_INDENT.repeat(indent / 2));
        let mut spans = vec![Span::styled(prefix, marker_style)];
        spans.extend(inline_spans(content, Style::default(), theme));
        return spans;
    }

    let mut spans = Vec::new();
    if indent > 0 {
        spans.push(Span::raw(" ".repeat(indent)));
    }
    spans.extend(inline_spans(trimmed.trim_end(), Style::default(), theme));
    spans
}

/// Breite der führenden Einrückung (Leerzeichen = 1, Tab = 4).
fn indent_width(line: &str) -> usize {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

/// Liefert den Titeltext einer ATX-Überschrift (ohne `#`-Marker).
fn heading_text(trimmed: &str) -> Option<&str> {
    let level = trimmed.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = trimmed.get(level..)?;
    if !(rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t')) {
        return None;
    }
    let body = rest.trim();
    // Optionale schließende `#`-Folge entfernen, wenn sie durch Leerraum abgetrennt ist.
    let without_closing = body.trim_end_matches('#');
    if without_closing.is_empty() {
        return Some("");
    }
    if without_closing.len() != body.len() && without_closing.ends_with([' ', '\t']) {
        return Some(without_closing.trim_end());
    }
    Some(body)
}

/// Prüft auf eine Trennlinie aus mindestens drei gleichen `-`, `*` oder `_`.
fn is_hr(trimmed: &str) -> bool {
    let compact: Vec<char> = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    match compact.first() {
        Some(&first) if matches!(first, '-' | '*' | '_') => {
            compact.len() >= 3 && compact.iter().all(|c| *c == first)
        }
        _ => false,
    }
}

/// Liefert den Inhalt eines ungeordneten Listenpunkts.
fn bullet_content(trimmed: &str) -> Option<&str> {
    ["- ", "* ", "+ "]
        .iter()
        .find_map(|marker| trimmed.strip_prefix(marker))
        .map(str::trim)
}

/// Liefert Nummer und Inhalt eines geordneten Listenpunkts (`1.` oder `1)`).
fn ordered_item(trimmed: &str) -> Option<(&str, &str)> {
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 9 {
        return None;
    }
    let number = trimmed.get(..digits)?;
    let rest = trimmed.get(digits..)?;
    let content = rest
        .strip_prefix(". ")
        .or_else(|| rest.strip_prefix(") "))?;
    Some((number, content.trim()))
}

// ---------------------------------------------------------------------------
// Code-Blöcke
// ---------------------------------------------------------------------------

/// Ein geöffneter Code-Zaun.
struct Fence {
    /// Zaunzeichen (`` ` `` oder `~`).
    marker: char,
    /// Länge der öffnenden Zeichenfolge.
    count: usize,
    /// Erkannte Sprache für die Hervorhebung.
    lang: Lang,
}

impl Fence {
    /// Erkennt eine öffnende Zaunzeile.
    fn open(raw: &str) -> Option<Self> {
        let trimmed = raw.trim_start();
        let marker = trimmed.chars().next()?;
        if marker != '`' && marker != '~' {
            return None;
        }
        let count = trimmed.chars().take_while(|c| *c == marker).count();
        if count < 3 {
            return None;
        }
        let info = trimmed.get(count..)?.trim();
        if marker == '`' && info.contains('`') {
            return None;
        }
        let tag = info.split_whitespace().next().unwrap_or("");
        Some(Self {
            marker,
            count,
            lang: Lang::from_tag(tag),
        })
    }

    /// Prüft, ob `raw` diesen Zaun schließt.
    fn is_closed_by(&self, raw: &str) -> bool {
        let trimmed = raw.trim();
        let run = trimmed.chars().take_while(|c| *c == self.marker).count();
        run >= self.count && run == trimmed.chars().count()
    }
}

/// Sprachen mit minimaler Syntaxhervorhebung.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lang {
    /// Rust.
    Rust,
    /// Python.
    Python,
    /// JavaScript / TypeScript.
    Js,
    /// POSIX-Shell / Bash.
    Shell,
    /// TOML.
    Toml,
    /// JSON.
    Json,
    /// Unbekannt: keine Hervorhebung.
    Plain,
}

impl Lang {
    /// Ordnet ein Sprach-Tag (```` ```rust ````) einer Sprache zu.
    fn from_tag(tag: &str) -> Self {
        match tag.to_ascii_lowercase().as_str() {
            "rust" | "rs" => Self::Rust,
            "python" | "py" | "python3" => Self::Python,
            "js" | "javascript" | "jsx" | "mjs" | "cjs" | "ts" | "typescript" | "tsx" => Self::Js,
            "sh" | "bash" | "shell" | "zsh" | "console" => Self::Shell,
            "toml" => Self::Toml,
            "json" | "jsonc" => Self::Json,
            _ => Self::Plain,
        }
    }

    /// Schlüsselwörter der Sprache.
    fn keywords(self) -> &'static [&'static str] {
        match self {
            Self::Rust => &[
                "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else",
                "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match",
                "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct",
                "super", "trait", "true", "type", "unsafe", "use", "where", "while",
            ],
            Self::Python => &[
                "and", "as", "assert", "async", "await", "break", "class", "continue", "def",
                "del", "elif", "else", "except", "False", "finally", "for", "from", "global", "if",
                "import", "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise",
                "return", "True", "try", "while", "with", "yield",
            ],
            Self::Js => &[
                "async",
                "await",
                "break",
                "case",
                "catch",
                "class",
                "const",
                "continue",
                "default",
                "delete",
                "do",
                "else",
                "enum",
                "export",
                "extends",
                "false",
                "finally",
                "for",
                "from",
                "function",
                "if",
                "implements",
                "import",
                "in",
                "instanceof",
                "interface",
                "let",
                "new",
                "null",
                "private",
                "public",
                "readonly",
                "return",
                "static",
                "super",
                "switch",
                "this",
                "throw",
                "true",
                "try",
                "type",
                "typeof",
                "undefined",
                "var",
                "void",
                "while",
                "yield",
            ],
            Self::Shell => &[
                "case", "do", "done", "echo", "elif", "else", "esac", "exit", "export", "fi",
                "for", "function", "if", "in", "local", "return", "then", "while",
            ],
            Self::Toml => &["true", "false"],
            Self::Json => &["true", "false", "null"],
            Self::Plain => &[],
        }
    }

    /// Zeichen, die ein String-Literal eröffnen.
    fn quotes(self) -> &'static [char] {
        match self {
            Self::Rust | Self::Json => &['"'],
            Self::Python | Self::Shell | Self::Toml => &['"', '\''],
            Self::Js => &['"', '\'', '`'],
            Self::Plain => &[],
        }
    }

    /// Prüft, ob an Position `i` ein Zeilenkommentar beginnt.
    fn starts_comment(self, chars: &[char], i: usize) -> bool {
        let current = chars.get(i).copied();
        match self {
            Self::Rust | Self::Js => current == Some('/') && chars.get(i + 1) == Some(&'/'),
            Self::Python | Self::Toml => current == Some('#'),
            Self::Shell => {
                current == Some('#')
                    && (i == 0 || chars.get(i - 1).is_some_and(|c| c.is_whitespace()))
            }
            Self::Json | Self::Plain => false,
        }
    }
}

/// Stil für Schlüsselwörter in Code-Blöcken.
fn keyword_style(theme: Theme) -> Style {
    Style::default()
        .fg(style::accent_color(theme))
        .add_modifier(Modifier::BOLD)
}

/// Stil für String-Literale in Code-Blöcken.
fn string_style(theme: Theme) -> Style {
    style::success_style(theme)
}

/// Stil für Zahlen in Code-Blöcken.
fn number_style(theme: Theme) -> Style {
    style::warning_style(theme)
}

/// Stil für Kommentare in Code-Blöcken.
fn comment_style(theme: Theme) -> Style {
    style::dim_style(theme).add_modifier(Modifier::ITALIC)
}

/// Stil für Inline-Code.
fn inline_code_style(theme: Theme) -> Style {
    style::tool_style(theme)
}

/// Rendert eine Code-Zeile mit Randspalte und Hervorhebung.
fn render_code_line(raw: &str, lang: Lang, theme: Theme) -> Line<'static> {
    let mut spans = vec![Span::styled(CODE_GUTTER, style::dim_style(theme))];
    // Erst die ganze Zeile sanieren, damit eine ESC-Sequenz nicht auf mehrere
    // Tokens verteilt und dadurch nur teilweise entfernt wird.
    spans.extend(highlight_code(&sanitize_inline(raw), lang, theme));
    Line::from(spans)
}

/// Zerlegt eine Code-Zeile in hervorgehobene Tokens.
fn highlight_code(line: &str, lang: Lang, theme: Theme) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    if line.is_empty() {
        return spans;
    }
    if lang == Lang::Plain {
        spans.push(Span::raw(sanitize_inline(line)));
        return spans;
    }
    let chars: Vec<char> = line.chars().collect();
    let keywords = lang.keywords();
    let quotes = lang.quotes();
    let mut plain = String::new();
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        if lang.starts_comment(&chars, i) {
            flush_plain(&mut plain, Style::default(), &mut spans);
            push_token(&chars[i..], comment_style(theme), &mut spans);
            break;
        }
        if quotes.contains(&c) {
            flush_plain(&mut plain, Style::default(), &mut spans);
            let end = scan_string(&chars, i);
            push_token(&chars[i..end], string_style(theme), &mut spans);
            i = end;
            continue;
        }
        let prev_is_ident = i > 0 && chars.get(i - 1).is_some_and(|p| is_ident_char(*p));
        if c.is_ascii_digit() && !prev_is_ident {
            flush_plain(&mut plain, Style::default(), &mut spans);
            let end = scan_while(&chars, i, |ch| {
                ch.is_ascii_alphanumeric() || ch == '_' || ch == '.'
            });
            push_token(&chars[i..end], number_style(theme), &mut spans);
            i = end;
            continue;
        }
        if (c.is_alphabetic() || c == '_') && !prev_is_ident {
            let end = scan_while(&chars, i, is_ident_char);
            let word: String = chars[i..end].iter().collect();
            if keywords.contains(&word.as_str()) {
                flush_plain(&mut plain, Style::default(), &mut spans);
                spans.push(Span::styled(sanitize_inline(&word), keyword_style(theme)));
            } else {
                plain.push_str(&word);
            }
            i = end;
            continue;
        }
        plain.push(c);
        i += 1;
    }
    flush_plain(&mut plain, Style::default(), &mut spans);
    spans
}

/// Zeichen, die in Bezeichnern vorkommen dürfen.
fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Liefert das Ende (exklusiv) eines bei `start` beginnenden String-Literals.
fn scan_string(chars: &[char], start: usize) -> usize {
    let Some(&quote) = chars.get(start) else {
        return chars.len();
    };
    let mut j = start + 1;
    while let Some(&c) = chars.get(j) {
        if c == '\\' {
            j += 2;
            continue;
        }
        if c == quote {
            return j + 1;
        }
        j += 1;
    }
    chars.len()
}

/// Liefert das Ende (exklusiv) der Zeichenfolge ab `start`, die `pred` erfüllt.
fn scan_while(chars: &[char], start: usize, pred: impl Fn(char) -> bool) -> usize {
    let mut j = start;
    while chars.get(j).is_some_and(|c| pred(*c)) {
        j += 1;
    }
    j
}

/// Hängt ein Token saniert und gestylt an.
fn push_token(chars: &[char], style: Style, spans: &mut Vec<Span<'static>>) {
    if chars.is_empty() {
        return;
    }
    let text: String = chars.iter().collect();
    spans.push(Span::styled(sanitize_inline(&text), style));
}

/// Schreibt den gepufferten Text als Span und leert den Puffer.
fn flush_plain(buf: &mut String, style: Style, spans: &mut Vec<Span<'static>>) {
    if !buf.is_empty() {
        spans.push(Span::styled(sanitize_inline(buf), style));
        buf.clear();
    }
}

// ---------------------------------------------------------------------------
// Inline-Formatierung
// ---------------------------------------------------------------------------

/// Rendert Inline-Markdown einer Zeile mit Grundstil `base`.
fn inline_spans(text: &str, base: Style, theme: Theme) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    parse_inline(&chars, base, theme, &mut out);
    out
}

/// Rekursiver Inline-Parser; nicht geschlossene Marker bleiben wörtlich stehen.
fn parse_inline(chars: &[char], base: Style, theme: Theme, out: &mut Vec<Span<'static>>) {
    let mut buf = String::new();
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        let next = chars.get(i + 1).copied();
        match c {
            '\\' if next.is_some_and(|n| n.is_ascii_punctuation()) => {
                if let Some(n) = next {
                    buf.push(n);
                }
                i += 2;
                continue;
            }
            '`' => {
                let run = scan_while(chars, i, |ch| ch == '`') - i;
                if let Some(close) = find_backtick_close(chars, i + run, run) {
                    flush_plain(&mut buf, base, out);
                    let inner: String = chars[i + run..close].iter().collect();
                    let inner = strip_code_padding(&inner);
                    out.push(Span::styled(
                        sanitize_inline(inner),
                        inline_code_style(theme),
                    ));
                    i = close + run;
                } else {
                    buf.extend(&chars[i..i + run]);
                    i += run;
                }
                continue;
            }
            '~' if next == Some('~') => {
                if let Some(close) = find_double_close(chars, i + 2, '~') {
                    flush_plain(&mut buf, base, out);
                    let style = base.add_modifier(Modifier::CROSSED_OUT);
                    parse_inline(&chars[i + 2..close], style, theme, out);
                    i = close + 2;
                    continue;
                }
            }
            '*' | '_' if next == Some(c) => {
                if opens_emphasis(chars, i, 2) {
                    if let Some(close) = find_double_close(chars, i + 2, c) {
                        flush_plain(&mut buf, base, out);
                        let style = base.add_modifier(Modifier::BOLD);
                        parse_inline(&chars[i + 2..close], style, theme, out);
                        i = close + 2;
                        continue;
                    }
                }
                // Nicht geschlossen: beide Marker wörtlich übernehmen.
                buf.push(c);
                buf.push(c);
                i += 2;
                continue;
            }
            '*' | '_' => {
                if opens_emphasis(chars, i, 1) {
                    if let Some(close) = find_single_close(chars, i + 1, c) {
                        flush_plain(&mut buf, base, out);
                        let style = base.add_modifier(Modifier::ITALIC);
                        parse_inline(&chars[i + 1..close], style, theme, out);
                        i = close + 1;
                        continue;
                    }
                }
            }
            '[' => {
                if let Some((text_end, url_end)) = find_link(chars, i) {
                    flush_plain(&mut buf, base, out);
                    let text = &chars[i + 1..text_end];
                    let url: String = chars[text_end + 2..url_end].iter().collect();
                    let url = url.trim();
                    parse_inline(text, base.add_modifier(Modifier::UNDERLINED), theme, out);
                    let text_str: String = text.iter().collect();
                    if !url.is_empty() && url != text_str.trim() {
                        out.push(Span::styled(
                            format!(" ({})", sanitize_inline(url)),
                            style::dim_style(theme),
                        ));
                    }
                    i = url_end + 1;
                    continue;
                }
            }
            _ => {}
        }
        buf.push(c);
        i += 1;
    }
    flush_plain(&mut buf, base, out);
}

/// Entfernt je ein umschließendes Leerzeichen eines Inline-Code-Inhalts.
fn strip_code_padding(inner: &str) -> &str {
    if inner.len() >= 2
        && inner.starts_with(' ')
        && inner.ends_with(' ')
        && !inner.trim().is_empty()
    {
        inner.get(1..inner.len() - 1).unwrap_or(inner)
    } else {
        inner
    }
}

/// Sucht eine schließende Backtick-Folge exakt der Länge `run` ab `from`.
fn find_backtick_close(chars: &[char], from: usize, run: usize) -> Option<usize> {
    let mut j = from;
    while j < chars.len() {
        if chars.get(j) == Some(&'`') {
            let len = scan_while(chars, j, |ch| ch == '`') - j;
            if len == run {
                return Some(j);
            }
            j += len;
        } else {
            j += 1;
        }
    }
    None
}

/// Prüft, ob der Marker der Länge `len` bei `i` Hervorhebung eröffnen darf.
fn opens_emphasis(chars: &[char], i: usize, len: usize) -> bool {
    let Some(&marker) = chars.get(i) else {
        return false;
    };
    let after = chars.get(i + len);
    if after.is_none_or(|c| c.is_whitespace()) {
        return false;
    }
    // `_` innerhalb eines Wortes (snake_case) ist keine Hervorhebung.
    if marker == '_' && i > 0 && chars.get(i - 1).is_some_and(|p| p.is_alphanumeric()) {
        return false;
    }
    true
}

/// Sucht einen schließenden Doppelmarker (`**`, `__`, `~~`) ab `from`.
fn find_double_close(chars: &[char], from: usize, marker: char) -> Option<usize> {
    let mut j = from + 1;
    while j + 1 < chars.len() {
        if chars.get(j) == Some(&marker)
            && chars.get(j + 1) == Some(&marker)
            && chars.get(j - 1).is_some_and(|p| !p.is_whitespace())
        {
            // Bei längeren Folgen (`***`) schließen die letzten beiden Marker.
            let run_end = scan_while(chars, j, |ch| ch == marker);
            let close = run_end - 2;
            if closes_word(chars, run_end, marker) {
                return Some(close);
            }
            j = run_end;
            continue;
        }
        j += 1;
    }
    None
}

/// Sucht einen schließenden Einzelmarker (`*`, `_`) ab `from`; Doppelmarker werden übersprungen.
fn find_single_close(chars: &[char], from: usize, marker: char) -> Option<usize> {
    let mut j = from;
    while let Some(&c) = chars.get(j) {
        if c == '`' {
            // Inline-Code innerhalb der Hervorhebung überspringen.
            let run = scan_while(chars, j, |ch| ch == '`') - j;
            j = find_backtick_close(chars, j + run, run).map_or(j + run, |close| close + run);
            continue;
        }
        if c == marker {
            if chars.get(j + 1) == Some(&marker) {
                j += 2;
                continue;
            }
            let prev_ok = j > from && chars.get(j - 1).is_some_and(|p| !p.is_whitespace());
            if prev_ok && closes_word(chars, j + 1, marker) {
                return Some(j);
            }
        }
        j += 1;
    }
    None
}

/// `_` schließt nur am Wortende (kein alphanumerisches Zeichen dahinter).
fn closes_word(chars: &[char], after: usize, marker: char) -> bool {
    marker != '_' || !chars.get(after).is_some_and(|c| c.is_alphanumeric())
}

/// Erkennt `[text](url)` ab `start`; liefert Position von `]` und `)`.
fn find_link(chars: &[char], start: usize) -> Option<(usize, usize)> {
    let text_end = (start + 1..chars.len()).find(|&j| chars.get(j) == Some(&']'))?;
    if chars.get(text_end + 1) != Some(&'(') {
        return None;
    }
    let url_end = (text_end + 2..chars.len()).find(|&j| chars.get(j) == Some(&')'))?;
    Some((text_end, url_end))
}

// ---------------------------------------------------------------------------
// Tabellen
// ---------------------------------------------------------------------------

/// Spaltenausrichtung aus der Trennzeile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Align {
    /// Linksbündig (Standard).
    Left,
    /// Zentriert (`:---:`).
    Center,
    /// Rechtsbündig (`---:`).
    Right,
}

/// Zeilen, die mit `|` beginnen, gehören zu einem Tabellen-Kandidaten.
fn is_table_line(raw: &str) -> bool {
    raw.trim_start().starts_with('|')
}

/// Zerlegt eine Tabellenzeile in Zellen (`\|` bleibt wörtlich erhalten).
fn parse_row(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    let inner = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let inner = if inner.ends_with('|') && !inner.ends_with("\\|") {
        inner.get(..inner.len() - 1).unwrap_or(inner)
    } else {
        inner
    };
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cell.push('|');
                chars.next();
            }
            '|' => cells.push(std::mem::take(&mut cell).trim().to_string()),
            _ => cell.push(c),
        }
    }
    cells.push(cell.trim().to_string());
    cells
}

/// Liefert die Ausrichtungen, falls `cells` eine Trennzeile (`|---|:--:|`) ist.
fn separator_alignments(cells: &[String]) -> Option<Vec<Align>> {
    if cells.is_empty() {
        return None;
    }
    cells
        .iter()
        .map(|cell| {
            let left = cell.starts_with(':');
            let right = cell.ends_with(':') && cell.len() > 1;
            let dashes = cell.trim_matches(':');
            if dashes.is_empty() || !dashes.chars().all(|c| c == '-') {
                return None;
            }
            Some(match (left, right) {
                (true, true) => Align::Center,
                (false, true) => Align::Right,
                _ => Align::Left,
            })
        })
        .collect()
}

/// Rendert gepufferte Tabellenzeilen; ohne gültige Trennzeile als normale Zeilen.
fn flush_table(buffer: &mut Vec<&str>, theme: Theme, out: &mut Vec<Line<'static>>) {
    if buffer.is_empty() {
        return;
    }
    let rows: Vec<Vec<String>> = buffer.iter().map(|raw| parse_row(raw)).collect();
    let aligns = rows.get(1).and_then(|row| separator_alignments(row));
    let (Some(aligns), Some(header)) = (aligns, rows.first()) else {
        // Noch keine vollständige Tabelle (z. B. Streaming nach der Kopfzeile).
        for raw in buffer.iter() {
            out.push(render_block_line(raw, theme));
        }
        buffer.clear();
        return;
    };

    let header_style = Style::default().add_modifier(Modifier::BOLD);
    let mut rendered: Vec<Vec<Vec<Span<'static>>>> = Vec::new();
    rendered.push(
        header
            .iter()
            .map(|cell| inline_spans(cell, header_style, theme))
            .collect(),
    );
    for row in rows.iter().skip(2) {
        rendered.push(
            row.iter()
                .map(|cell| inline_spans(cell, Style::default(), theme))
                .collect(),
        );
    }

    let columns = rendered.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0usize; columns];
    for row in &rendered {
        for (col, cell) in row.iter().enumerate() {
            if let Some(w) = widths.get_mut(col) {
                *w = (*w).max(spans_width(cell));
            }
        }
    }

    for (index, row) in rendered.into_iter().enumerate() {
        out.push(table_row_line(row, &widths, &aligns, theme));
        if index == 0 {
            let underline = widths
                .iter()
                .map(|w| "─".repeat(*w))
                .collect::<Vec<_>>()
                .join(TABLE_CROSS);
            out.push(Line::from(Span::styled(underline, style::dim_style(theme))));
        }
    }
    buffer.clear();
}

/// Anzeigebreite einer Span-Folge.
fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.content.as_ref().width()).sum()
}

/// Baut eine ausgerichtete Tabellenzeile.
fn table_row_line(
    mut row: Vec<Vec<Span<'static>>>,
    widths: &[usize],
    aligns: &[Align],
    theme: Theme,
) -> Line<'static> {
    let columns = widths.len();
    row.resize_with(columns, Vec::new);
    let mut spans = Vec::new();
    for (col, cell) in row.into_iter().enumerate() {
        if col > 0 {
            spans.push(Span::styled(TABLE_SEP, style::dim_style(theme)));
        }
        let width = widths.get(col).copied().unwrap_or(0);
        let gap = width.saturating_sub(spans_width(&cell));
        let align = aligns.get(col).copied().unwrap_or(Align::Left);
        let (left, right) = match align {
            Align::Left => (0, gap),
            Align::Right => (gap, 0),
            Align::Center => (gap / 2, gap - gap / 2),
        };
        if left > 0 {
            spans.push(Span::raw(" ".repeat(left)));
        }
        spans.extend(cell);
        // Rechte Auffüllung der letzten Spalte entfällt (kein unnötiger Leerraum).
        if right > 0 && col + 1 < columns {
            spans.push(Span::raw(" ".repeat(right)));
        }
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    const T: Theme = Theme::Dark;

    fn text_of(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn span<'a>(line: &'a Line<'static>, content: &str) -> &'a Span<'static> {
        line.spans
            .iter()
            .find(|s| s.content == content)
            .unwrap_or_else(|| panic!("span {content:?} fehlt in {line:?}"))
    }

    fn has(style: Style, m: Modifier) -> bool {
        style.add_modifier.contains(m)
    }

    #[test]
    fn heading_is_bold_accent_without_markers() {
        let lines = render_markdown("## Titel ##", T);
        assert_eq!(lines.len(), 1);
        assert_eq!(text_of(&lines[0]), "Titel");
        let s = span(&lines[0], "Titel");
        assert_eq!(s.style.fg, Some(style::accent_color(T)));
        assert!(has(s.style, Modifier::BOLD));
        // Kein Leerzeichen nach `#` → kein Heading.
        assert_eq!(text_of(&render_markdown("#tag", T)[0]), "#tag");
        // ####### (7) ist kein Heading.
        assert_eq!(text_of(&render_markdown("####### x", T)[0]), "####### x");
    }

    #[test]
    fn bullets_and_nesting() {
        let lines = render_markdown("- eins\n  * zwei\n    + drei", T);
        assert_eq!(text_of(&lines[0]), "• eins");
        assert_eq!(text_of(&lines[1]), "  • zwei");
        assert_eq!(text_of(&lines[2]), "    • drei");
        assert_eq!(lines[0].spans[0].style.fg, Some(style::accent_color(T)));
    }

    #[test]
    fn ordered_lists() {
        let lines = render_markdown("1. a\n2) b\n   10. c", T);
        assert_eq!(text_of(&lines[0]), "1. a");
        assert_eq!(text_of(&lines[1]), "2. b");
        assert_eq!(text_of(&lines[2]), "  10. c");
    }

    #[test]
    fn blockquote_has_dim_prefix() {
        let lines = render_markdown("> zitat **fett**\n>> tief", T);
        assert_eq!(text_of(&lines[0]), "│ zitat fett");
        assert_eq!(lines[0].spans[0].style, style::dim_style(T));
        assert!(has(span(&lines[0], "fett").style, Modifier::BOLD));
        assert_eq!(text_of(&lines[1]), "│ │ tief");
    }

    #[test]
    fn code_block_with_rust_highlighting() {
        let md = "```rust\nfn main() { let s = \"hi\"; // c\n    let n = 42;\n```\nnach";
        let lines = render_markdown(md, T);
        assert_eq!(lines.len(), 3);
        assert_eq!(text_of(&lines[0]), "▏ fn main() { let s = \"hi\"; // c");
        assert_eq!(lines[0].spans[0].style, style::dim_style(T));
        assert_eq!(span(&lines[0], "fn").style, keyword_style(T));
        assert_eq!(span(&lines[0], "let").style, keyword_style(T));
        assert_eq!(span(&lines[0], "\"hi\"").style, string_style(T));
        assert_eq!(span(&lines[0], "// c").style, comment_style(T));
        assert_eq!(text_of(&lines[1]), "▏     let n = 42;");
        assert_eq!(span(&lines[1], "42").style, number_style(T));
        assert_eq!(text_of(&lines[2]), "nach");
        // Markdown im Code wird nicht interpretiert.
        let lines = render_markdown("```\n# kein **heading**\n```", T);
        assert_eq!(text_of(&lines[0]), "▏ # kein **heading**");
    }

    #[test]
    fn code_highlighting_other_languages() {
        let py = render_markdown("```python\ndef f(): return 'x'  # k\n```", T);
        assert_eq!(span(&py[0], "def").style, keyword_style(T));
        assert_eq!(span(&py[0], "'x'").style, string_style(T));
        assert_eq!(span(&py[0], "# k").style, comment_style(T));

        let sh = render_markdown("```sh\necho a#b # c\n```", T);
        assert_eq!(span(&sh[0], "echo").style, keyword_style(T));
        assert_eq!(span(&sh[0], "# c").style, comment_style(T));
        assert!(text_of(&sh[0]).contains("a#b"));

        let json = render_markdown("```json\n{\"a\": true, \"b\": 1.5}\n```", T);
        assert_eq!(span(&json[0], "true").style, keyword_style(T));
        assert_eq!(span(&json[0], "1.5").style, number_style(T));
        assert_eq!(span(&json[0], "\"a\"").style, string_style(T));

        let ts = render_markdown("```ts\nconst x = `t`;\n```", T);
        assert_eq!(span(&ts[0], "const").style, keyword_style(T));
        assert_eq!(span(&ts[0], "`t`").style, string_style(T));

        let toml = render_markdown("```toml\nk = false # c\n```", T);
        assert_eq!(span(&toml[0], "false").style, keyword_style(T));

        // Bezeichner mit Ziffern sind keine Zahlen, Keywords in Wörtern keine Keywords.
        let r = render_markdown("```rs\nlet v2 = format_fn;\n```", T);
        assert!(
            r[0].spans
                .iter()
                .all(|s| s.content != "2" && s.content != "fn")
        );

        // Unbekannte Sprache: keine Hervorhebung.
        let plain = render_markdown("```text\nfn x\n```", T);
        assert_eq!(plain[0].spans.len(), 2);
        assert_eq!(plain[0].spans[1].style, Style::default());
    }

    #[test]
    fn unclosed_fence_while_streaming() {
        let lines = render_markdown("Text\n```rust\nfn a() {\n    \"offen", T);
        assert_eq!(lines.len(), 3);
        assert_eq!(text_of(&lines[1]), "▏ fn a() {");
        assert_eq!(span(&lines[2], "\"offen").style, string_style(T));
        // Nur die Zaunzeile.
        assert!(render_markdown("```", T).is_empty());
        assert!(render_markdown("```py", T).is_empty());
        // Längerer Zaun wird nur durch mindestens gleich langen geschlossen.
        let lines = render_markdown("````\n```\n````\nx", T);
        assert_eq!(text_of(&lines[0]), "▏ ```");
        assert_eq!(text_of(&lines[1]), "x");
    }

    #[test]
    fn inline_styles() {
        let lines = render_markdown("a `c` **b** *i* _j_ ~~s~~ ***x***", T);
        let l = &lines[0];
        assert_eq!(text_of(l), "a c b i j s x");
        assert_eq!(span(l, "c").style, inline_code_style(T));
        assert!(has(span(l, "b").style, Modifier::BOLD));
        assert!(has(span(l, "i").style, Modifier::ITALIC));
        assert!(has(span(l, "j").style, Modifier::ITALIC));
        assert!(has(span(l, "s").style, Modifier::CROSSED_OUT));
        let x = span(l, "x").style;
        assert!(has(x, Modifier::BOLD) && has(x, Modifier::ITALIC));
    }

    #[test]
    fn inline_nesting_and_literals() {
        let lines = render_markdown("**fett *kursiv* `code`**", T);
        let l = &lines[0];
        assert!(has(span(l, "fett ").style, Modifier::BOLD));
        let k = span(l, "kursiv").style;
        assert!(has(k, Modifier::BOLD) && has(k, Modifier::ITALIC));
        assert_eq!(span(l, "code").style, inline_code_style(T));

        // snake_case, Multiplikation und Escapes bleiben wörtlich.
        let l = &render_markdown("snake_case_name 2 * 3 * 4 \\*nicht\\*", T)[0];
        assert_eq!(text_of(l), "snake_case_name 2 * 3 * 4 *nicht*");
        assert!(l.spans.iter().all(|s| s.style == Style::default()));

        // Markdown im Inline-Code wird nicht interpretiert.
        let l = &render_markdown("`**x**` und ``a`b``", T)[0];
        assert_eq!(span(l, "**x**").style, inline_code_style(T));
        assert_eq!(span(l, "a`b").style, inline_code_style(T));
    }

    #[test]
    fn links_render_text_and_url() {
        let l = &render_markdown("siehe [Doku](https://x.y) hier", T)[0];
        assert_eq!(text_of(l), "siehe Doku (https://x.y) hier");
        assert!(has(span(l, "Doku").style, Modifier::UNDERLINED));
        assert_eq!(span(l, " (https://x.y)").style, style::dim_style(T));
        // Kein Link ohne Klammerteil.
        assert_eq!(text_of(&render_markdown("[a] b", T)[0]), "[a] b");
    }

    #[test]
    fn horizontal_rules() {
        for md in ["---", "***", "_ _ _", "-----"] {
            let lines = render_markdown(md, T);
            assert_eq!(text_of(&lines[0]), "─".repeat(HR_WIDTH), "{md}");
            assert_eq!(lines[0].spans[0].style, style::dim_style(T));
        }
    }

    #[test]
    fn tables_align_columns() {
        let md = "| Name | Wert |\n|:-----|----:|\n| a | 1 |\n| lang | 100 |";
        let lines = render_markdown(md, T);
        assert_eq!(lines.len(), 4);
        assert_eq!(text_of(&lines[0]), "Name │ Wert");
        assert_eq!(text_of(&lines[1]), "─────┼─────");
        assert_eq!(text_of(&lines[2]), "a    │    1");
        assert_eq!(text_of(&lines[3]), "lang │  100");
        assert!(has(span(&lines[0], "Name").style, Modifier::BOLD));
        assert_eq!(span(&lines[0], TABLE_SEP).style, style::dim_style(T));
        assert_eq!(lines[1].spans[0].style, style::dim_style(T));
    }

    #[test]
    fn table_inline_markup_and_ragged_rows() {
        let md = "| a | b |\n|---|:-:|\n| `x` | \\| |\n| nur |";
        let lines = render_markdown(md, T);
        assert_eq!(lines.len(), 4);
        assert_eq!(span(&lines[2], "x").style, inline_code_style(T));
        assert_eq!(text_of(&lines[2]), "x   │ |");
        assert_eq!(text_of(&lines[3]), "nur │ ");
    }

    #[test]
    fn partial_table_while_streaming() {
        let lines = render_markdown("| a | b |", T);
        assert_eq!(lines.len(), 1);
        assert_eq!(text_of(&lines[0]), "| a | b |");
        // Kopf plus Trennzeile ergeben Kopfzeile und Unterstreichung.
        let lines = render_markdown("| a | b |\n|--", T);
        assert_eq!(lines.len(), 2);
        assert_eq!(text_of(&lines[1]), "──┼──");
        assert_eq!(text_of(&lines[0]), "a │ b");
    }

    #[test]
    fn streaming_partials_never_panic() {
        let full = "# H\n> q *i\n- [l](u\n1. **b\n| x |\n|-:|\n```rust\nlet s = \"a\\\n";
        let chars: Vec<char> = full.chars().collect();
        for n in 0..=chars.len() {
            let prefix: String = chars[..n].iter().collect();
            let _ = render_markdown(&prefix, T);
            let _ = render_markdown(&prefix, Theme::Light);
        }
        // Nicht geschlossene Marker bleiben wörtlich.
        let l = &render_markdown("**fett und `code", T)[0];
        assert_eq!(text_of(l), "**fett und `code");
        let l = &render_markdown("[link](http", T)[0];
        assert_eq!(text_of(l), "[link](http");
    }

    #[test]
    fn control_chars_are_sanitized() {
        let md = "a\u{1b}[31mrot **\u{7}b**\n```sh\necho \u{1b}]52;c;x\u{7}\n```\n| \u{202e}x | y |\n|---|---|";
        let lines = render_markdown(md, T);
        for line in &lines {
            for s in &line.spans {
                assert!(
                    !s.content.chars().any(|c| c.is_control() || c == '\u{202e}'),
                    "unsanitiert: {:?}",
                    s.content
                );
            }
        }
        assert!(text_of(&lines[0]).contains(crate::sanitize::ESC_SEQUENCE_MARKER));
    }

    #[test]
    fn blank_lines_and_paragraph_indent() {
        let lines = render_markdown("a\n\n  b\r\n", T);
        assert_eq!(lines.len(), 3);
        assert!(lines[1].spans.is_empty());
        assert_eq!(text_of(&lines[2]), "  b");
        assert_eq!(lines[0].spans[0].style.fg, None::<Color>);
    }
}
