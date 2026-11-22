//! REASONING window of the portrait stack: one time-ordered, mixed stream
//! of all agents (reasoning text, tool starts and results, finished marks),
//! every row prefixed with the agent role. Bounded ring, newest rows at the
//! bottom; the render shows the last rows minus a scroll offset from the end.

use std::cell::{Ref, RefCell};
use std::collections::{HashMap, VecDeque};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::live_window::NO_VISIBLE_EVENTS;
use crate::export::redact_text;
use crate::sanitize::sanitize_inline;
use crate::status_line::{display_width, fit_width};
use crate::style::{self, Theme};

/// Most entries the feed keeps (oldest dropped first).
const MAX_ENTRIES: usize = 2000;

/// Most text bytes the feed keeps in total (oldest dropped first).
const MAX_BYTES: usize = 256 * 1024;

/// Size a single oversized entry is cut down to once it exceeds
/// [`MAX_BYTES`] (hysteresis: the next deltas do not trim again).
const TRIM_TO_BYTES: usize = MAX_BYTES / 4 * 3;

/// Display columns of the role column (without the separating space).
const ROLE_COLS: usize = 12;

/// Role column plus its separating space.
const PREFIX_COLS: usize = ROLE_COLS + 1;

/// Glyph in front of reasoning text (same as the LIVE window).
const REASONING_GLYPH: &str = "∴ ";

/// Stand-in for a grapheme wider than the text column.
const TOO_WIDE: &str = "…";

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

/// Wrapped text rows of an entry for one width (`width == None`: stale).
#[derive(Debug, Clone, Default)]
struct Wrapped {
    width: Option<u16>,
    rows: Vec<String>,
}

/// One feed entry.
#[derive(Debug, Clone)]
struct Entry {
    agent: String,
    role: String,
    kind: EntryKind,
    /// Raw text; reasoning is redacted and sanitized only when wrapped, so
    /// sequences split across deltas are judged as a whole.
    text: String,
    /// Wrap result for the last width asked for.
    wrapped: RefCell<Wrapped>,
}

impl Entry {
    /// Wrapped rows at `width`, wrapped at most once per width and text.
    fn rows(&self, width: u16) -> Ref<'_, [String]> {
        if self.wrapped.borrow().width != Some(width) {
            *self.wrapped.borrow_mut() = Wrapped {
                width: Some(width),
                rows: wrap_entry(self.kind, &self.text, width),
            };
        }
        Ref::map(self.wrapped.borrow(), |wrapped| wrapped.rows.as_slice())
    }

    /// Cached row count at `width`, without wrapping.
    fn cached_rows(&self, width: u16) -> Option<usize> {
        let wrapped = self.wrapped.borrow();
        (wrapped.width == Some(width)).then_some(wrapped.rows.len())
    }

    fn invalidate(&mut self) {
        self.wrapped.get_mut().width = None;
    }
}

/// Rows that left the top of the feed (eviction or front trim) at `width`
/// since the last [`ReasoningFeed::clear`] or width change.
#[derive(Debug, Default)]
struct Evicted {
    width: Option<u16>,
    all: usize,
    by_agent: HashMap<String, usize>,
}

impl Evicted {
    fn add(&mut self, agent: &str, rows: usize) {
        if rows == 0 {
            return;
        }
        self.all = self.all.saturating_add(rows);
        let count = self.by_agent.entry(agent.to_owned()).or_default();
        *count = count.saturating_add(rows);
    }

    fn rows(&self, pinned: Option<&str>) -> usize {
        match pinned {
            None => self.all,
            Some(id) => self.by_agent.get(id).copied().unwrap_or(0),
        }
    }
}

/// Memoized [`ReasoningFeed::line_count`] result.
#[derive(Debug)]
struct CountCache {
    generation: u64,
    width: u16,
    pinned: Option<String>,
    count: usize,
}

/// Bounded, time-ordered stream of all agents' visible events.
#[derive(Debug, Default)]
pub(crate) struct ReasoningFeed {
    entries: VecDeque<Entry>,
    bytes: usize,
    /// Bumped on every change, keys [`Self::count`].
    generation: u64,
    count: RefCell<Option<CountCache>>,
    evicted: RefCell<Evicted>,
}

impl ReasoningFeed {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
        self.touch();
        let evicted = self.evicted.get_mut();
        evicted.all = 0;
        evicted.by_agent.clear();
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
            last.invalidate();
            self.bytes += delta.len();
            self.touch();
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
        let summary = collapse(&sanitize_inline(&redact_text(summary)));
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

    /// Number of rows [`Self::lines`] can produce at `width`. Memoized until
    /// the next change; only entries changed since their last wrap at this
    /// width are wrapped again.
    pub(crate) fn line_count(&self, width: u16, pinned: Option<&str>) -> usize {
        let cached = self
            .count
            .borrow()
            .as_ref()
            .filter(|cache| {
                cache.generation == self.generation
                    && cache.width == width
                    && cache.pinned.as_deref() == pinned
            })
            .map(|cache| cache.count);
        if let Some(count) = cached {
            return count;
        }
        let count = self
            .visible(pinned)
            .map(|entry| entry.rows(width).len())
            .sum();
        *self.count.borrow_mut() = Some(CountCache {
            generation: self.generation,
            width,
            pinned: pinned.map(str::to_owned),
            count,
        });
        count
    }

    /// Scroll extent for a bottom-anchored scroller: [`Self::line_count`]
    /// plus the rows that left the top at `width` since the last
    /// [`Self::clear`] or width change. Grows with every row added at the
    /// bottom even when the full ring drops as many at the top, so a
    /// scrolled-up offset that grows by the difference keeps its rows.
    pub(crate) fn scroll_total(&self, width: u16, pinned: Option<&str>) -> usize {
        let evicted = {
            let mut evicted = self.evicted.borrow_mut();
            if evicted.width != Some(width) {
                *evicted = Evicted {
                    width: Some(width),
                    ..Evicted::default()
                };
            }
            evicted.rows(pinned)
        };
        self.line_count(width, pinned).saturating_add(evicted)
    }

    /// The newest rows: at most `max_rows`, after skipping
    /// `scroll_from_bottom` rows from the end. Builds only the returned rows.
    pub(crate) fn lines(
        &self,
        width: u16,
        max_rows: usize,
        scroll_from_bottom: usize,
        pinned: Option<&str>,
        theme: Theme,
    ) -> Vec<Line<'static>> {
        let mut skip = scroll_from_bottom;
        let mut rows: Vec<Line<'static>> = Vec::new();
        for entry in self.visible(pinned).rev() {
            if rows.len() >= max_rows {
                break;
            }
            let wrapped = entry.rows(width);
            if skip >= wrapped.len() {
                skip -= wrapped.len();
                continue;
            }
            let end = wrapped.len() - skip;
            skip = 0;
            let start = end.saturating_sub(max_rows - rows.len());
            let text_style = entry_style(entry.kind, theme);
            for index in (start..end).rev() {
                let prefix = if index == 0 {
                    Span::styled(role_column(&entry.role), style::dim_style(theme))
                } else {
                    Span::raw(" ".repeat(PREFIX_COLS))
                };
                rows.push(Line::from(vec![
                    prefix,
                    Span::styled(wrapped[index].clone(), text_style),
                ]));
            }
        }
        rows.reverse();
        rows
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
            wrapped: RefCell::new(Wrapped::default()),
        });
        self.touch();
        self.enforce_bounds();
    }

    fn touch(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// Drops the oldest entries over the bounds, or trims the front of a
    /// single oversized entry down to [`TRIM_TO_BYTES`], and records the
    /// rows that left the top at the tracked width.
    fn enforce_bounds(&mut self) {
        let tracked = self.evicted.get_mut().width;
        while self.entries.len() > MAX_ENTRIES || (self.bytes > MAX_BYTES && self.entries.len() > 1)
        {
            if let Some(old) = self.entries.pop_front() {
                self.bytes = self.bytes.saturating_sub(old.text.len());
                if let Some(rows) = tracked.and_then(|width| old.cached_rows(width)) {
                    self.evicted.get_mut().add(&old.agent, rows);
                }
            }
        }
        if self.bytes > MAX_BYTES
            && let Some(only) = self.entries.front_mut()
        {
            let before = tracked.map(|width| only.rows(width).len());
            let mut cut = only.text.len().saturating_sub(TRIM_TO_BYTES);
            while !only.text.is_char_boundary(cut) {
                cut += 1;
            }
            only.text.drain(..cut);
            only.invalidate();
            self.bytes = only.text.len();
            if let (Some(width), Some(before)) = (tracked, before) {
                let after = only.rows(width).len();
                self.evicted
                    .get_mut()
                    .add(&only.agent, before.saturating_sub(after));
            }
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
    // Memoized by the feed (the caller's `scroll_total` already counted).
    // The scroll extent includes rows evicted from the top; an offset into
    // them shows the oldest kept rows.
    let height = usize::from(inner.height);
    let total = feed.line_count(inner.width, pinned);
    let scroll_from_bottom = scroll_from_bottom.min(total.saturating_sub(height));
    let mut lines = feed.lines(inner.width, height, scroll_from_bottom, pinned, theme);
    if total == 0 {
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

/// Wrapped text rows of one entry (empty for blank reasoning). Reasoning
/// is stored raw and redacted before sanitizing, like child-stream lines.
fn wrap_entry(kind: EntryKind, text: &str, width: u16) -> Vec<String> {
    let cols = usize::from(width).saturating_sub(PREFIX_COLS).max(1);
    if kind == EntryKind::Reasoning {
        let text = collapse(&sanitize_inline(&redact_text(text)));
        if text.is_empty() {
            return Vec::new();
        }
        wrap(&format!("{REASONING_GLYPH}{text}"), cols)
    } else {
        let text = collapse(&sanitize_inline(text));
        if text.is_empty() {
            return Vec::new();
        }
        wrap(&text, cols)
    }
}

/// Greedy word wrap by display columns; over-long words split by grapheme,
/// a grapheme wider than `cols` shows as [`TOO_WIDE`].
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
                let (grapheme, width) = match grapheme.width() {
                    width if width > cols => (TOO_WIDE, display_width(TOO_WIDE)),
                    width => (grapheme, width),
                };
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
    use crate::chat_scroll::ChatScroll;
    use crate::sanitize::is_invisible_format_char;
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
        assert!(feed.bytes <= MAX_BYTES);
        assert_eq!(feed.bytes, TRIM_TO_BYTES);
        // Hysteresis: the next delta is appended without another trim.
        feed.push_reasoning("a", "r", "z");
        assert_eq!(feed.bytes, TRIM_TO_BYTES + 1);
        assert_eq!(
            feed.entries.front().map(|entry| entry.text.len()),
            Some(feed.bytes)
        );
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
    fn line_count_matches_rows_after_coalescing_and_eviction() {
        let mut feed = ReasoningFeed::new();
        feed.push_reasoning("a1", "worker", &"word ".repeat(40));
        let before = feed.line_count(30, None);
        feed.push_reasoning("a1", "worker", &"more ".repeat(40));
        let after = feed.line_count(30, None);
        assert!(after > before);
        assert_eq!(
            feed.lines(30, usize::MAX, 0, None, Theme::Dark).len(),
            after
        );
        for index in 0..MAX_ENTRIES {
            feed.push_tool_start("a2", "coder", &format!("t{index}"));
            if index == MAX_ENTRIES / 2 {
                assert_eq!(
                    feed.lines(30, usize::MAX, 0, None, Theme::Dark).len(),
                    feed.line_count(30, None)
                );
            }
        }
        assert_eq!(feed.entries.len(), MAX_ENTRIES);
        let total = feed.line_count(30, None);
        assert_eq!(total, MAX_ENTRIES);
        assert_eq!(
            feed.lines(30, usize::MAX, 0, None, Theme::Dark).len(),
            total
        );
        assert_eq!(feed.line_count(30, Some("a1")), 0);
    }

    #[test]
    fn scrolled_up_view_stays_put_when_the_ring_is_full() {
        let mut feed = ReasoningFeed::new();
        for index in 0..MAX_ENTRIES {
            feed.push_tool_start("a", "r", &format!("t{index}"));
        }
        let mut scroll = ChatScroll::new();
        let total = feed.scroll_total(40, None);
        assert_eq!(scroll.sync_layout(total, 3, 40), 0);
        scroll.scroll_up(5, total, 3);
        let offset = scroll.sync_layout(total, 3, 40);
        assert_eq!(offset, 5);
        let shown = rows(&feed, 40, 3, offset);
        for index in MAX_ENTRIES..(MAX_ENTRIES + 10) {
            feed.push_tool_start("a", "r", &format!("t{index}"));
        }
        assert_eq!(feed.line_count(40, None), MAX_ENTRIES);
        let grown = feed.scroll_total(40, None);
        assert_eq!(grown, total + 10);
        let offset = scroll.sync_layout(grown, 3, 40);
        assert_eq!(offset, 15);
        assert_eq!(rows(&feed, 40, 3, offset), shown);
        // A width change or clear() resets the evicted rows.
        assert_eq!(feed.scroll_total(30, None), feed.line_count(30, None));
        feed.clear();
        assert_eq!(feed.scroll_total(30, None), 0);
    }

    #[test]
    fn front_trim_of_one_entry_keeps_scroll_total_monotonic() {
        let mut feed = ReasoningFeed::new();
        feed.push_reasoning("a", "r", &"word ".repeat(MAX_BYTES / 5 - 100));
        let before = feed.scroll_total(40, None);
        feed.push_reasoning("a", "r", &"next ".repeat(1000));
        assert_eq!(feed.bytes, TRIM_TO_BYTES);
        assert!(feed.line_count(40, None) < before);
        assert!(feed.scroll_total(40, None) >= before);
    }

    #[test]
    fn tool_errors_and_reasoning_are_redacted() {
        let mut feed = ReasoningFeed::new();
        feed.push_tool_end(
            "a",
            "worker",
            "http",
            false,
            "401 Authorization: Bearer abc123xyz\nretry",
        );
        feed.push_reasoning("b", "planner", "the api_key=");
        feed.push_reasoning("b", "planner", "abc123xyz leaked");
        let all = rows(&feed, 80, 8, 0).join("\n");
        assert!(!all.contains("abc123xyz"), "{all}");
        assert!(all.contains("[REDACTED]"), "{all}");
    }

    #[test]
    fn control_and_bidi_characters_are_neutralised() {
        let mut feed = ReasoningFeed::new();
        feed.push_reasoning("a", "worker", "copy \u{1b}]52;c;SGVsbG8=");
        feed.push_reasoning("a", "worker", "\u{7} done \u{9b}31m");
        feed.push_tool_start("b", "evil\u{202e}role", "x\u{1b}[2J");
        feed.push_tool_end(
            "b",
            "evil\u{202e}role",
            "x\u{1b}[2J",
            false,
            "bad\u{85}\u{2066}",
        );
        let lines = feed.lines(40, usize::MAX, 0, None, Theme::Dark);
        assert_eq!(lines.len(), feed.line_count(40, None));
        for line in &lines {
            for span in &line.spans {
                assert!(
                    !span
                        .content
                        .chars()
                        .any(|c| c.is_control() || is_invisible_format_char(c)),
                    "{:?}",
                    span.content
                );
            }
        }
        let all = lines.iter().map(text).collect::<Vec<_>>().join("\n");
        assert!(!all.contains("SGVsbG8"), "{all}");
    }

    #[test]
    fn wide_and_combining_text_fits_role_column_and_width() -> TestResult {
        let mut feed = ReasoningFeed::new();
        let role = "漢字漢字漢字漢";
        feed.push_reasoning("a", role, "漢字漢字漢字漢字漢字 かなかな ");
        feed.push_reasoning(
            "a",
            role,
            "e\u{301}e\u{301}e\u{301} 長い長い長い長い長い長い長い",
        );
        feed.push_tool_end("a", role, "道具", true, "結果は良好です");
        for width in [30, 14] {
            let lines = feed.lines(width, usize::MAX, 0, None, Theme::Dark);
            assert!(!lines.is_empty());
            assert_eq!(lines.len(), feed.line_count(width, None));
            for line in &lines {
                let prefix = line.spans.first().ok_or(TestError::Missing("prefix"))?;
                assert_eq!(display_width(&prefix.content), PREFIX_COLS);
                let row = text(line);
                assert!(display_width(&row) <= usize::from(width), "{width}: {row}");
            }
        }
        Ok(())
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
