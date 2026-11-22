//! REASONING window of the portrait stack: one time-ordered, mixed stream
//! of all agents (reasoning text, tool starts and results, finished marks),
//! every row prefixed with the agent role. Bounded ring, newest rows at the
//! bottom; the render shows the last rows minus a scroll offset from the end.

use std::collections::{HashMap, VecDeque};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::live_window::NO_VISIBLE_EVENTS;
use crate::sanitize::sanitize_inline;
use crate::status_line::{display_width, fit_width};
use crate::style::{self, Theme};

/// Most entries the feed keeps (oldest dropped first).
const MAX_ENTRIES: usize = 2000;

/// Most text bytes the feed keeps in total (oldest dropped first).
const MAX_BYTES: usize = 256 * 1024;

/// Display columns of the role column (without the separating space).
const ROLE_COLS: usize = 12;

/// Role column plus its separating space.
const PREFIX_COLS: usize = ROLE_COLS + 1;

/// Glyph in front of reasoning text (same as the LIVE window).
const REASONING_GLYPH: &str = "∴ ";

/// What an entry shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    /// Reasoning text the provider exposed.
    Reasoning,
    /// A tool call started.
    ToolStart,
    /// A tool call ended.
    ToolEnd { ok: bool },
    /// The agent run ended.
    Finished { ok: bool },
}

/// One feed entry.
#[derive(Debug, Clone)]
struct Entry {
    agent: String,
    role: String,
    kind: EntryKind,
    text: String,
}

/// Bounded, time-ordered stream of all agents' visible events.
#[derive(Debug, Default)]
pub(crate) struct ReasoningFeed {
    entries: VecDeque<Entry>,
    bytes: usize,
}

impl ReasoningFeed {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    /// Appends reasoning text; consecutive deltas of the same agent coalesce.
    pub(crate) fn push_reasoning(&mut self, agent: &str, role: &str, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if let Some(last) = self.entries.back_mut()
            && last.kind == EntryKind::Reasoning
            && last.agent == agent
        {
            last.text.push_str(delta);
            self.bytes += delta.len();
            self.enforce_bounds();
            return;
        }
        self.push(agent, role, EntryKind::Reasoning, delta.to_owned());
    }

    pub(crate) fn push_tool_start(&mut self, agent: &str, role: &str, tool: &str) {
        let text = format!("● {} · running", sanitize_inline(tool));
        self.push(agent, role, EntryKind::ToolStart, text);
    }

    pub(crate) fn push_tool_end(
        &mut self,
        agent: &str,
        role: &str,
        tool: &str,
        ok: bool,
        summary: &str,
    ) {
        let mark = if ok { '✓' } else { '✗' };
        let tool = sanitize_inline(tool);
        let summary = collapse(&sanitize_inline(summary));
        let text = if summary.is_empty() {
            format!("{mark} {tool}")
        } else {
            format!("{mark} {tool} · {summary}")
        };
        self.push(agent, role, EntryKind::ToolEnd { ok }, text);
    }

    pub(crate) fn push_finished(&mut self, agent: &str, role: &str, ok: bool) {
        let text = if ok { "✓ done" } else { "✗ failed" };
        self.push(agent, role, EntryKind::Finished { ok }, text.to_owned());
    }

    /// Agents whose newest entry is not a finished mark.
    pub(crate) fn active_agents(&self) -> usize {
        let mut active: HashMap<&str, bool> = HashMap::new();
        for entry in &self.entries {
            active.insert(
                entry.agent.as_str(),
                !matches!(entry.kind, EntryKind::Finished { .. }),
            );
        }
        active.values().filter(|running| **running).count()
    }

    /// Number of rows [`Self::lines`] can produce at `width`.
    pub(crate) fn line_count(&self, width: u16, pinned: Option<&str>) -> usize {
        self.visible(pinned)
            .map(|entry| wrap_entry(entry, width).len())
            .sum()
    }

    /// The newest rows: at most `max_rows`, after skipping
    /// `scroll_from_bottom` rows from the end.
    pub(crate) fn lines(
        &self,
        width: u16,
        max_rows: usize,
        scroll_from_bottom: usize,
        pinned: Option<&str>,
        theme: Theme,
    ) -> Vec<Line<'static>> {
        let wanted = scroll_from_bottom.saturating_add(max_rows);
        let mut rows: Vec<Line<'static>> = Vec::new();
        for entry in self.visible(pinned).rev() {
            if rows.len() >= wanted {
                break;
            }
            let wrapped = wrap_entry(entry, width);
            let text_style = entry_style(entry.kind, theme);
            let role = role_column(&entry.role);
            let indent = " ".repeat(PREFIX_COLS);
            for (index, text) in wrapped.into_iter().enumerate().rev() {
                let prefix = if index == 0 {
                    Span::styled(role.clone(), style::dim_style(theme))
                } else {
                    Span::raw(indent.clone())
                };
                rows.push(Line::from(vec![prefix, Span::styled(text, text_style)]));
            }
        }
        rows.reverse();
        let end = rows.len().saturating_sub(scroll_from_bottom);
        let start = end.saturating_sub(max_rows);
        rows[start..end].to_vec()
    }

    fn visible<'a>(
        &'a self,
        pinned: Option<&'a str>,
    ) -> impl DoubleEndedIterator<Item = &'a Entry> + 'a {
        self.entries
            .iter()
            .filter(move |entry| pinned.is_none_or(|id| entry.agent == id))
    }

    fn push(&mut self, agent: &str, role: &str, kind: EntryKind, text: String) {
        self.bytes += text.len();
        self.entries.push_back(Entry {
            agent: agent.to_owned(),
            role: sanitize_inline(role.trim()),
            kind,
            text,
        });
        self.enforce_bounds();
    }

    fn enforce_bounds(&mut self) {
        while self.entries.len() > MAX_ENTRIES || (self.bytes > MAX_BYTES && self.entries.len() > 1)
        {
            if let Some(old) = self.entries.pop_front() {
                self.bytes = self.bytes.saturating_sub(old.text.len());
            }
        }
        if self.bytes > MAX_BYTES
            && let Some(only) = self.entries.front_mut()
        {
            let mut cut = only.text.len() - MAX_BYTES;
            while !only.text.is_char_boundary(cut) {
                cut += 1;
            }
            only.text.drain(..cut);
            self.bytes = only.text.len();
        }
    }
}

/// Renders the boxed REASONING window.
pub(crate) fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    feed: &ReasoningFeed,
    scroll_from_bottom: usize,
    focused: bool,
    pinned: Option<&str>,
    theme: Theme,
) {
    let area = area.intersection(frame.area());
    if area.width < 3 || area.height < 3 {
        return;
    }
    let inner_width = usize::from(area.width.saturating_sub(2));
    let title = fit_width(
        &crate::i18n::tr_args(
            "mission.reasoning_title",
            " Reasoning · {count} active ",
            &[("count", &(feed.active_agents()).to_string())],
        ),
        inner_width,
    );
    let border = if focused {
        Style::default().fg(style::accent_color(theme))
    } else {
        Style::default().fg(style::border_color(theme))
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let mut lines = feed.lines(
        inner.width,
        usize::from(inner.height),
        scroll_from_bottom,
        pinned,
        theme,
    );
    if lines.is_empty() && feed.line_count(inner.width, pinned) == 0 {
        lines.push(Line::styled(
            fit_width(
                crate::i18n::tr("mission.no_events", NO_VISIBLE_EVENTS),
                usize::from(inner.width),
            ),
            style::dim_style(theme),
        ));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn entry_style(kind: EntryKind, theme: Theme) -> Style {
    match kind {
        EntryKind::Reasoning => style::dim_style(theme).add_modifier(Modifier::ITALIC),
        EntryKind::ToolStart | EntryKind::ToolEnd { ok: true } => style::tool_style(theme),
        EntryKind::ToolEnd { ok: false } | EntryKind::Finished { ok: false } => {
            style::error_style(theme)
        }
        EntryKind::Finished { ok: true } => style::dim_style(theme),
    }
}

/// Role truncated to [`ROLE_COLS`] columns, padded, plus one space.
fn role_column(role: &str) -> String {
    let fitted = fit_width(role, ROLE_COLS);
    let pad = ROLE_COLS.saturating_sub(display_width(&fitted));
    format!("{fitted}{} ", " ".repeat(pad))
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Wrapped text rows of one entry (empty for blank reasoning).
fn wrap_entry(entry: &Entry, width: u16) -> Vec<String> {
    let cols = usize::from(width).saturating_sub(PREFIX_COLS).max(1);
    let text = collapse(&sanitize_inline(&entry.text));
    if text.is_empty() {
        return Vec::new();
    }
    if entry.kind == EntryKind::Reasoning {
        wrap(&format!("{REASONING_GLYPH}{text}"), cols)
    } else {
        wrap(&text, cols)
    }
}

/// Greedy word wrap by display columns; over-long words split by grapheme.
fn wrap(text: &str, cols: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut current = String::new();
    let mut used = 0;
    for word in text.split(' ') {
        let word_width = display_width(word);
        if word_width > cols {
            if !current.is_empty() {
                rows.push(std::mem::take(&mut current));
            }
            used = 0;
            for grapheme in word.graphemes(true) {
                let width = grapheme.width();
                if used + width > cols && !current.is_empty() {
                    rows.push(std::mem::take(&mut current));
                    used = 0;
                }
                current.push_str(grapheme);
                used += width;
            }
        } else if current.is_empty() {
            current.push_str(word);
            used = word_width;
        } else if used + 1 + word_width <= cols {
            current.push(' ');
            current.push_str(word);
            used += 1 + word_width;
        } else {
            rows.push(std::mem::replace(&mut current, word.to_owned()));
            used = word_width;
        }
    }
    if !current.is_empty() {
        rows.push(current);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn rows(feed: &ReasoningFeed, width: u16, max: usize, scroll: usize) -> Vec<String> {
        feed.lines(width, max, scroll, None, Theme::Dark)
            .iter()
            .map(text)
            .collect()
    }

    #[test]
    fn consecutive_reasoning_of_one_agent_coalesces() {
        let mut feed = ReasoningFeed::new();
        feed.push_reasoning("a1", "worker", "hel");
        feed.push_reasoning("a1", "worker", "lo");
        assert_eq!(feed.entries.len(), 1);
        assert_eq!(rows(&feed, 40, 8, 0), vec!["worker       ∴ hello"]);
        feed.push_tool_start("a1", "worker", "bash");
        feed.push_reasoning("a1", "worker", "more");
        assert_eq!(feed.entries.len(), 3);
    }

    #[test]
    fn interleaved_agents_keep_time_order_with_roles() {
        let mut feed = ReasoningFeed::new();
        feed.push_reasoning("a1", "planner", "one");
        feed.push_reasoning("a2", "coder", "two");
        feed.push_reasoning("a1", "planner", "three");
        feed.push_tool_end("a2", "coder", "bash", false, "exit 1");
        feed.push_finished("a1", "planner", true);
        let all = rows(&feed, 40, 8, 0);
        assert_eq!(
            all,
            vec![
                "planner      ∴ one",
                "coder        ∴ two",
                "planner      ∴ three",
                "coder        ✗ bash · exit 1",
                "planner      ✓ done",
            ]
        );
        assert_eq!(feed.active_agents(), 1);
    }

    #[test]
    fn entry_and_byte_bounds_drop_oldest() {
        let mut feed = ReasoningFeed::new();
        for index in 0..(MAX_ENTRIES + 50) {
            feed.push_tool_start("a", "r", &format!("t{index}"));
        }
        assert_eq!(feed.entries.len(), MAX_ENTRIES);
        feed.clear();
        let big = "x".repeat(10 * 1024);
        for index in 0..60 {
            let agent = if index % 2 == 0 { "a" } else { "b" };
            feed.push_reasoning(agent, "r", &big);
        }
        assert!(feed.bytes <= MAX_BYTES);
        assert!(feed.entries.len() < 60);
        feed.clear();
        feed.push_reasoning("a", "r", &"y".repeat(MAX_BYTES + 100));
        assert_eq!(feed.bytes, MAX_BYTES);
    }

    #[test]
    fn pinned_shows_only_that_agent() {
        let mut feed = ReasoningFeed::new();
        feed.push_reasoning("a1", "planner", "one");
        feed.push_reasoning("a2", "coder", "two");
        let only = feed.lines(40, 8, 0, Some("a2"), Theme::Dark);
        assert_eq!(only.len(), 1);
        assert!(text(&only[0]).starts_with("coder"));
        assert_eq!(feed.line_count(40, Some("a2")), 1);
        assert_eq!(feed.line_count(40, Some("none")), 0);
    }

    #[test]
    fn role_is_truncated_to_twelve_columns() -> TestResult {
        let mut feed = ReasoningFeed::new();
        feed.push_tool_start("a1", "a-very-long-role-name", "x");
        let all = rows(&feed, 40, 8, 0);
        let first = all.first().ok_or(TestError::Missing("row"))?;
        let prefix: String = first.chars().take(PREFIX_COLS).collect();
        assert_eq!(prefix, "a-very-long… ");
        assert_eq!(display_width(&prefix), PREFIX_COLS);
        Ok(())
    }

    #[test]
    fn wraps_at_width_30_with_indented_continuations() -> TestResult {
        let mut feed = ReasoningFeed::new();
        feed.push_reasoning(
            "a1",
            "worker",
            "alpha beta gamma delta epsilon zeta eta theta",
        );
        let all = rows(&feed, 30, 8, 0);
        assert!(all.len() > 1);
        for row in &all {
            assert!(display_width(row) <= 30, "{row}");
        }
        let second = all.get(1).ok_or(TestError::Missing("row"))?;
        assert!(second.starts_with(&" ".repeat(PREFIX_COLS)));
        Ok(())
    }

    #[test]
    fn scroll_from_bottom_skips_newest_rows() {
        let mut feed = ReasoningFeed::new();
        for index in 0..5 {
            feed.push_tool_start("a", "r", &format!("t{index}"));
        }
        let newest = rows(&feed, 40, 2, 0);
        assert!(newest[1].contains("t4") && newest[0].contains("t3"));
        let scrolled = rows(&feed, 40, 2, 2);
        assert!(scrolled[1].contains("t2") && scrolled[0].contains("t1"));
        assert!(rows(&feed, 40, 2, 99).is_empty());
    }

    #[test]
    fn line_count_matches_producible_rows() {
        let mut feed = ReasoningFeed::new();
        feed.push_reasoning("a1", "worker", &"word ".repeat(40));
        feed.push_tool_end("a2", "coder", "bash", true, "ok");
        feed.push_reasoning("a1", "worker", "");
        for width in [14, 30, 80] {
            let total = feed.line_count(width, None);
            assert_eq!(
                feed.lines(width, usize::MAX, 0, None, Theme::Dark).len(),
                total
            );
        }
    }

    #[test]
    fn render_shows_title_and_empty_hint() -> TestResult {
        let feed = ReasoningFeed::new();
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 5))
            .map_err(ctx("test terminal"))?;
        terminal
            .draw(|frame| {
                let area = frame.area();
                render(frame, area, &feed, 0, false, None, Theme::Dark);
            })
            .map_err(ctx("draw"))?;
        let buffer = terminal.backend().buffer();
        let row = |y: u16| -> String {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect()
        };
        assert!(row(0).contains("Reasoning"));
        assert!(row(1).contains(NO_VISIBLE_EVENTS));
        Ok(())
    }
}
