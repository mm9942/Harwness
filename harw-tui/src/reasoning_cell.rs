//! Renderer for the model's reasoning output in the chat history (PL-97 R1).
//!
//! # Responsibility
//! Turns a reasoning summary (`ReasoningItem::summary_text`, joined with
//! blank lines so paragraphs survive) into history lines that read like a
//! message instead of a dimmed single-line note: a header `∴ Reasoning`
//! (optionally followed by the dimmed `[role]` origin of a child agent) and
//! a body rendered through [`crate::markdown::render_markdown`] exactly like
//! assistant messages (headings, lists, code fences, tables). The body keeps
//! the theme's normal text colour; a dim left gutter (`│ `) marks every body
//! line as reasoning.
//!
//! The same module renders the transient live reasoning of the running turn
//! (`TurnEvent::ReasoningDelta`) in `message` mode: a header `∴ Reasoning …`
//! and the last few wrapped lines of the streamed tail with paragraph breaks
//! preserved.
//!
//! # Terminal safety (W1-08)
//! Every text that comes from the model runs through
//! [`crate::sanitize::sanitize_display`] (body) or
//! [`crate::sanitize::sanitize_inline`] (origin) before it reaches the
//! renderer; the origin is truncated char-safely **before** sanitising so no
//! `⟨U+XXXX⟩` marker is cut in half. The markdown renderer sanitises every
//! inline piece a second time, which is harmless.
//!
//! # Callers
//! [`crate::history_cell::ReasoningHistoryCell::display_lines`] and the live
//! reasoning preview in `app.rs` (`live_reasoning_lines`).

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::history_cell::{truncate_chars, wrap_plain, wrap_styled};
use crate::sanitize::{sanitize_display, sanitize_inline};
use crate::style;

/// Header of every reasoning block.
pub(crate) const REASONING_HEADER: &str = "∴ Reasoning";

/// Suffix of the live header while reasoning is still streaming.
const LIVE_SUFFIX: &str = " …";

/// Dim left gutter in front of every body line.
pub(crate) const REASONING_GUTTER: &str = "│ ";

/// Upper bound (chars) of the `[role]` origin before it is truncated.
const ORIGIN_MAX_CHARS: usize = 24;

/// Builds the header line: `∴ Reasoning`, the optional dimmed `[role]`
/// origin and, for the live preview, a trailing ` …`.
///
/// # Arguments
/// - `origin` (`Option<&str>`): raw role of the originating child agent;
///   truncated to [`ORIGIN_MAX_CHARS`] and sanitised. `None` for the root.
/// - `live` (`bool`): `true` appends ` …` (reasoning still streaming).
/// - `theme` ([`style::Theme`]): colour scheme.
#[must_use]
pub(crate) fn header_line(origin: Option<&str>, live: bool, theme: style::Theme) -> Line<'static> {
    let dim = style::dim_style(theme);
    let mut spans = vec![Span::styled(
        REASONING_HEADER,
        dim.add_modifier(Modifier::BOLD),
    )];
    if let Some(role) = origin {
        // Truncate before sanitising (module docs, W1-08).
        let shown = sanitize_inline(&truncate_chars(role, ORIGIN_MAX_CHARS));
        spans.push(Span::styled(format!(" [{shown}]"), dim));
    }
    if live {
        spans.push(Span::styled(LIVE_SUFFIX, dim));
    }
    Line::from(spans)
}

/// Prefixes one rendered body line with the dim gutter.
fn with_gutter(line: Line<'static>, theme: style::Theme) -> Line<'static> {
    let mut spans = Vec::with_capacity(line.spans.len() + 1);
    spans.push(Span::styled(REASONING_GUTTER, style::dim_style(theme)));
    spans.extend(line.spans);
    Line::from(spans).style(line.style)
}

/// Width available for body text next to the gutter (at least one column).
fn body_width(width: u16) -> usize {
    usize::from(width.saturating_sub(2).max(1))
}

/// Renders the body of a reasoning summary: sanitised, parsed as markdown,
/// wrapped like an assistant message and prefixed with the gutter.
///
/// # Arguments
/// - `summary` (`&str`): raw summary text (paragraphs separated by blank lines).
/// - `width` (`u16`): total width in columns including the gutter.
/// - `theme` ([`style::Theme`]): colour scheme.
///
/// # Returns
/// One line per rendered row; empty for an empty summary.
#[must_use]
pub(crate) fn body_lines(summary: &str, width: u16, theme: style::Theme) -> Vec<Line<'static>> {
    let sanitized = sanitize_display(summary);
    let body = sanitized.trim_matches('\n');
    if body.trim().is_empty() {
        return Vec::new();
    }
    let text_width = body_width(width);
    crate::markdown::render_markdown(body, theme)
        .into_iter()
        .flat_map(|line| wrap_styled(line, text_width))
        .map(|line| with_gutter(line, theme))
        .collect()
}

/// Renders a complete reasoning cell: header plus body, or — collapsed —
/// header, first body line and the hint `… (N Zeilen · Ctrl+O)` where `N`
/// is the number of body lines at this width. A body that fits in one line
/// never shows the hint.
///
/// # Arguments
/// - `summary` (`&str`): raw summary text.
/// - `origin` (`Option<&str>`): raw role of the originating child agent.
/// - `expanded` (`bool`): user toggle (Ctrl+O).
/// - `width` (`u16`): total width in columns.
/// - `theme` ([`style::Theme`]): colour scheme.
#[must_use]
pub(crate) fn display_lines(
    summary: &str,
    origin: Option<&str>,
    expanded: bool,
    width: u16,
    theme: style::Theme,
) -> Vec<Line<'static>> {
    let mut out = vec![header_line(origin, false, theme)];
    let body = body_lines(summary, width, theme);
    let total = body.len();
    if expanded || total <= 1 {
        out.extend(body);
        return out;
    }
    out.extend(body.into_iter().take(1));
    let hint = format!("… ({total} Zeilen · Ctrl+O)");
    let dim = style::dim_style(theme);
    let hint_width = u16::try_from(body_width(width)).unwrap_or(u16::MAX);
    for piece in wrap_plain(&hint, hint_width) {
        let raw: String = piece.spans.iter().map(|s| s.content.as_ref()).collect();
        out.push(with_gutter(Line::from(Span::styled(raw, dim)), theme));
    }
    out
}

/// Renders the live reasoning preview in `message` mode: the header
/// `∴ Reasoning …` followed by the last (up to) `max_lines` wrapped lines
/// of the streamed tail. Paragraph breaks are preserved as one empty gutter
/// line; runs of blank lines collapse to one.
///
/// # Arguments
/// - `text` (`&str`): raw streamed reasoning tail (sanitised here).
/// - `width` (`u16`): width of the history area.
/// - `theme` ([`style::Theme`]): colour scheme.
/// - `max_lines` (`usize`): upper bound of body lines after the header.
///
/// # Returns
/// Empty when the sanitised text is blank; otherwise header plus body.
#[must_use]
pub(crate) fn live_message_lines(
    text: &str,
    width: u16,
    theme: style::Theme,
    max_lines: usize,
) -> Vec<Line<'static>> {
    let sanitized = sanitize_display(text);
    let trimmed = sanitized.trim_matches('\n');
    if trimmed.trim().is_empty() {
        return Vec::new();
    }
    let text_width = u16::try_from(body_width(width)).unwrap_or(u16::MAX);
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut pending_break = false;
    for raw in trimmed.lines() {
        if raw.trim().is_empty() {
            pending_break = !rows.is_empty();
            continue;
        }
        if pending_break {
            rows.push(Line::from(Span::raw(String::new())));
            pending_break = false;
        }
        for piece in wrap_plain(raw.trim_end(), text_width) {
            let content: String = piece.spans.iter().map(|s| s.content.as_ref()).collect();
            rows.push(Line::from(Span::styled(content, Style::default())));
        }
    }
    let skip = rows.len().saturating_sub(max_lines);
    let mut out = vec![header_line(None, true, theme)];
    out.extend(
        rows.into_iter()
            .skip(skip)
            .map(|line| with_gutter(line, theme)),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn text_of(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn texts(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(text_of).collect()
    }

    /// Two paragraphs stay two paragraphs: a blank gutter line separates them.
    #[test]
    fn two_paragraphs_render_with_a_blank_line_between() -> TestResult {
        let lines = texts(&display_lines(
            "Erster Absatz.\n\nZweiter Absatz.",
            None,
            true,
            80,
            style::Theme::Dark,
        ));
        let expected = vec![
            "∴ Reasoning".to_owned(),
            "│ Erster Absatz.".to_owned(),
            "│ ".to_owned(),
            "│ Zweiter Absatz.".to_owned(),
        ];
        if lines != expected {
            return Err(TestError::Unexpected(format!("war: {lines:?}")));
        }
        Ok(())
    }

    /// Bullet lists and code fences go through the markdown renderer: the
    /// list bullet and the code gutter are visible behind the reasoning gutter.
    #[test]
    fn markdown_list_and_code_fence_render_through_markdown() -> TestResult {
        let summary = "Plan:\n- erstens\n- zweitens\n\n```rust\nlet x = 1;\n```";
        let lines = texts(&display_lines(summary, None, true, 80, style::Theme::Dark));
        let has_bullet = lines.iter().any(|l| l.starts_with("│ • erstens"));
        let has_code = lines.iter().any(|l| l.starts_with("│ ▏ let x = 1;"));
        if !has_bullet || !has_code {
            return Err(TestError::Unexpected(format!("war: {lines:?}")));
        }
        Ok(())
    }

    /// Collapsed: header, first body line and the hint with the body line count.
    #[test]
    fn collapsed_shows_header_first_line_and_hint() -> TestResult {
        let lines = texts(&display_lines(
            "Erste Zeile\nZweite Zeile\nDritte Zeile",
            None,
            false,
            80,
            style::Theme::Dark,
        ));
        let expected = vec![
            "∴ Reasoning".to_owned(),
            "│ Erste Zeile".to_owned(),
            "│ … (3 Zeilen · Ctrl+O)".to_owned(),
        ];
        if lines != expected {
            return Err(TestError::Unexpected(format!("war: {lines:?}")));
        }
        Ok(())
    }

    /// The body keeps the normal text colour (no dim, no italic); only the
    /// gutter is dimmed.
    #[test]
    fn body_text_is_not_dim_or_italic() -> TestResult {
        let lines = display_lines("Lesbar", None, true, 80, style::Theme::Dark);
        let body = lines.get(1).ok_or(TestError::Missing("Body-Zeile"))?;
        let gutter = body.spans.first().ok_or(TestError::Missing("Gutter"))?;
        let text = body.spans.last().ok_or(TestError::Missing("Text"))?;
        if gutter.style.fg != Some(style::dim_color(style::Theme::Dark)) {
            return Err(TestError::Unexpected(format!("Gutter: {:?}", gutter.style)));
        }
        if text.style.fg.is_some() || text.style.add_modifier.contains(Modifier::ITALIC) {
            return Err(TestError::Unexpected(format!("Text: {:?}", text.style)));
        }
        Ok(())
    }

    /// The origin is truncated, sanitised and shown dimmed in the header.
    #[test]
    fn header_shows_truncated_sanitised_origin() -> TestResult {
        let role = "e\u{1b}[31mxplorer-mit-sehr-langem-namen-xyz";
        let line = header_line(Some(role), false, style::Theme::Dark);
        let text = text_of(&line);
        if !text.starts_with("∴ Reasoning [") || text.contains('\u{1b}') {
            return Err(TestError::Unexpected(text));
        }
        let origin = line.spans.get(1).ok_or(TestError::Missing("Origin-Span"))?;
        if origin.style.fg != Some(style::dim_color(style::Theme::Dark)) {
            return Err(TestError::Unexpected(format!("{:?}", origin.style)));
        }
        if origin.content.chars().count() > ORIGIN_MAX_CHARS + " []".len() + 8 {
            return Err(TestError::Unexpected(format!(
                "nicht gekürzt: {}",
                origin.content
            )));
        }
        Ok(())
    }

    /// Live preview: header `∴ Reasoning …`, paragraph breaks preserved, at
    /// most `max_lines` body lines, the newest.
    #[test]
    fn live_message_lines_keep_paragraph_breaks_and_tail() -> TestResult {
        let text = "a\n\n\nb\nc";
        let lines = texts(&live_message_lines(text, 80, style::Theme::Dark, 8));
        let expected = vec![
            "∴ Reasoning …".to_owned(),
            "│ a".to_owned(),
            "│ ".to_owned(),
            "│ b".to_owned(),
            "│ c".to_owned(),
        ];
        if lines != expected {
            return Err(TestError::Unexpected(format!("war: {lines:?}")));
        }
        let long = (0..40)
            .map(|i| format!("wort{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let tail = live_message_lines(&long, 24, style::Theme::Dark, 8);
        if tail.len() != 9 || !text_of(&tail[8]).contains("wort39") {
            return Err(TestError::Unexpected(format!("war: {:?}", texts(&tail))));
        }
        Ok(())
    }

    /// Narrow widths (and width 0) never panic.
    #[test]
    fn narrow_widths_do_not_panic() {
        let summary = "# Titel\n\n- ein sehr langer Aufzählungspunkt ohne Ende\n\n```\ncode\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |";
        for width in [0u16, 1, 2, 3, 24] {
            let _ = display_lines(summary, Some("explorer"), true, width, style::Theme::Dark);
            let _ = display_lines(summary, None, false, width, style::Theme::Light);
            let _ = live_message_lines(summary, width, style::Theme::Dark, 8);
        }
    }
}
