//! Mission Control windows (PL-97 C2; PL-94 W02 U1/U2).
//!
//! Three status windows with separate identities -- Active Agents, Active
//! Jobs, Finished / Failed -- and the small read-only LIVE projection of one
//! selected agent. The geometry comes from [`harw_tui_layout::classify_mission`]
//! (converted in [`crate::panes::MissionAreas`]); this module owns the
//! presentation state of the windows ([`MissionUi`]) and their renderers.
//!
//! # State ownership
//! - The Agents window keeps the legacy selection and scroll offset of
//!   [`crate::agent_monitor::AgentMonitor`] (`selected`, `panel_scroll`), so
//!   every existing agent-list key keeps working unchanged.
//! - Jobs and Finished / Failed each own one [`WindowUi`] in [`MissionUi`].
//! - LIVE in the portrait stack is neither focusable nor scrollable; it only
//!   remembers the pinned agent and its ranking state.
//!
//! Everything here is presentation only: no model call, no job, no change to
//! scheduling, permissions or history (W02 "Reuse, no extra model or job").

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use crate::agent_monitor::{AgentLive, AgentMonitor, AgentPhase};
use crate::live_ranker::{ActivityTier, LiveActivityRanker, LiveCandidate, RankerConfig};
use crate::panes::PaneFocus;

pub(crate) mod agents_window;
pub(crate) mod finished_window;
pub(crate) mod jobs_window;
pub(crate) mod live_window;
pub(crate) mod projection;

/// Focus cycle order of `F4` (PL-97 §3.3.3): Chat → Agents → Jobs →
/// Finished → LIVE (only while focusable) → Explorer → Workbench → Chat.
/// Targets that are not on screen are skipped by
/// [`crate::panes::PanelState`].
pub(crate) const FOCUS_ORDER: [PaneFocus; 7] = [
    PaneFocus::Chat,
    PaneFocus::Agents,
    PaneFocus::Jobs,
    PaneFocus::Finished,
    PaneFocus::Live,
    PaneFocus::Explorer,
    PaneFocus::Workbench,
];

/// How long a pinned agent that just finished stays the LIVE source (its
/// honest terminal summary) before LIVE falls back to the default rules;
/// mirrors `RankerConfig::terminal_display_ms`.
const PINNED_TERMINAL_HOLD: Duration = Duration::from_millis(5000);

/// The Mission Control windows that can be hit by the mouse or focused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MissionWindow {
    /// Active agents (or the combined agents-and-jobs list).
    Agents,
    /// Active jobs.
    Jobs,
    /// Finished / failed aggregates.
    Finished,
    /// LIVE projection of the selected agent.
    Live,
}

/// Selection and scroll state of one list window.
///
/// The selection is anchored by row key (job id, Finished category): when
/// the rows reorder -- a running job finishes, a new one starts, a category
/// gets a new failure -- the selection stays on the same row instead of the
/// same index. The index is only the fallback once the anchored row is gone.
///
/// Rendering only has `&ChatApp`, so the values the renderer corrects
/// (selected index, anchor, scroll offset, follow flag, visible rows) live in
/// [`Cell`]s / a [`RefCell`]; keys change them through `&mut`.
#[derive(Debug, Default)]
pub(crate) struct WindowUi {
    /// Selected row index as of the last key or render (fallback when the
    /// anchored row disappears).
    selected: Cell<usize>,
    /// Key of the selected row; the selection follows this row.
    anchor: RefCell<Option<String>>,
    /// First visible row.
    scroll: Cell<usize>,
    /// Bring the selection into view on the next render.
    follow: Cell<bool>,
    /// Visible rows at the last render (page size for PageUp/PageDown).
    viewport_rows: Cell<usize>,
}

impl WindowUi {
    /// Index of the selected row among `keys` (the window's row keys in
    /// display order): the anchored row if it is still listed, otherwise the
    /// last index clamped to the rows, which then becomes the new anchor.
    #[must_use]
    pub(crate) fn selected(&self, keys: &[&str]) -> usize {
        let Some(last) = keys.len().checked_sub(1) else {
            return 0;
        };
        let anchored = self.anchor.try_borrow().ok().and_then(|anchor| {
            let anchor = anchor.as_deref()?;
            keys.iter().position(|key| *key == anchor)
        });
        let index = anchored.unwrap_or_else(|| self.selected.get().min(last));
        self.selected.set(index);
        if anchored.is_none()
            && let Ok(mut anchor) = self.anchor.try_borrow_mut()
        {
            *anchor = keys.get(index).map(|key| (*key).to_owned());
        }
        index
    }

    /// Key of the selected row, if any (tests and diagnostics).
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn selected_key<'a>(&self, keys: &[&'a str]) -> Option<&'a str> {
        keys.get(self.selected(keys)).copied()
    }

    /// First visible row as of the last render or scroll (tests and
    /// diagnostics).
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn scroll_offset(&self) -> usize {
        self.scroll.get()
    }

    /// Selects row `index` of `keys`, anchors it and brings it into view.
    fn select_index(&mut self, index: usize, keys: &[&str]) {
        self.selected.set(index);
        *self.anchor.get_mut() = keys.get(index).map(|key| (*key).to_owned());
        self.follow.set(true);
    }

    /// Moves the selection by one row, wrapping like the agent list.
    pub(crate) fn select_step(&mut self, forward: bool, keys: &[&str]) {
        let len = keys.len();
        if len == 0 {
            self.select_index(0, keys);
            return;
        }
        let current = self.selected(keys);
        let next = if forward {
            (current + 1) % len
        } else {
            (current + len - 1) % len
        };
        self.select_index(next, keys);
    }

    /// PageUp/PageDown: moves the selection by one page, stopping at the
    /// edges.
    pub(crate) fn select_page(&mut self, down: bool, keys: &[&str]) {
        let Some(last) = keys.len().checked_sub(1) else {
            return;
        };
        let page = self.viewport_rows.get().max(2) - 1;
        let current = self.selected(keys);
        let next = if down {
            current.saturating_add(page).min(last)
        } else {
            current.saturating_sub(page)
        };
        self.select_index(next, keys);
    }

    /// Home/End: selects the first or the last row.
    pub(crate) fn select_edge(&mut self, last: bool, keys: &[&str]) {
        let index = if last {
            keys.len().saturating_sub(1)
        } else {
            0
        };
        self.select_index(index, keys);
    }

    /// Scrolls without moving the selection (mouse wheel, `scroll_panel_*`);
    /// negative is up. The next render clamps the offset.
    pub(crate) fn scroll_by(&self, delta: isize) {
        let current = self.scroll.get();
        let next = if delta < 0 {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            current.saturating_add(delta.unsigned_abs())
        };
        self.scroll.set(next);
        self.follow.set(false);
    }

    /// First row to draw for the rows `keys` in a viewport of `rows` rows.
    ///
    /// Clamps the stored offset to the content, keeps a keyboard selection
    /// visible once (follow) and remembers the viewport for page keys.
    pub(crate) fn window_start(&self, keys: &[&str], rows: usize) -> usize {
        self.viewport_rows.set(rows);
        let len = keys.len();
        let max_offset = len.saturating_sub(rows);
        let mut offset = self.scroll.get().min(max_offset);
        if self.follow.get() && rows > 0 && len > 0 {
            let selected = self.selected(keys);
            if selected < offset {
                offset = selected;
            } else if selected >= offset + rows {
                offset = selected + 1 - rows;
            }
        }
        self.follow.set(false);
        self.scroll.set(offset);
        offset
    }
}

/// Ranking state for the LIVE fallback source: the pure
/// [`LiveActivityRanker`] plus the epoch its millisecond clock counts from.
/// A separate instance from the status-line order (`app/live_order_glue.rs`)
/// so the two projections never disturb each other's coalescing.
#[derive(Debug)]
struct LiveRank {
    ranker: LiveActivityRanker,
    epoch: Instant,
}

impl LiveRank {
    fn millis_since_epoch(&self, at: Instant) -> u64 {
        u64::try_from(at.saturating_duration_since(self.epoch).as_millis()).unwrap_or(u64::MAX)
    }

    /// Highest-ranked agent of `monitor` at `now`.
    fn top(&mut self, monitor: &AgentMonitor, now: Instant) -> Option<String> {
        let candidates: Vec<LiveCandidate> = monitor
            .rows()
            .into_iter()
            .map(|(_, live)| self.candidate(live))
            .collect();
        let now_ms = self.millis_since_epoch(now);
        self.ranker.rank(now_ms, &candidates).into_iter().next()
    }

    fn candidate(&self, live: &AgentLive) -> LiveCandidate {
        let tier = live_tier(live);
        LiveCandidate {
            run_id: live.id.clone(),
            // Der Monitor enthält nur Läufe dieser Sitzung.
            session_visible: true,
            tier,
            last_meaningful_ms: self.millis_since_epoch(live.last_meaningful),
            reasoning_deltas_since_last_eval: 0,
            terminal_since_ms: (tier == ActivityTier::Terminal)
                .then(|| live.finished.map(|at| self.millis_since_epoch(at)))
                .flatten(),
        }
    }
}

/// Ranker tier of one agent (same mapping as the status-line order).
fn live_tier(live: &AgentLive) -> ActivityTier {
    if live.is_unseen_failure() {
        return ActivityTier::NeedsOperator;
    }
    match live.phase {
        AgentPhase::Done | AgentPhase::Failed | AgentPhase::Cancelled => ActivityTier::Terminal,
        AgentPhase::Admitted | AgentPhase::Thinking | AgentPhase::Tool => ActivityTier::Active,
        AgentPhase::Waiting => ActivityTier::Quiet,
    }
}

/// When the TUI first saw one tool call of one run (LIVE `● name · läuft ·
/// elapsed`). The monitor has no tool-start instant; `last_meaningful` moves
/// with every later event of the run, so the start is taken once, when the
/// call (run, tool, call count) first shows up, and kept until it changes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolClock {
    run: String,
    tool: String,
    calls: u32,
    started: Instant,
}

/// Presentation state of the Mission Control windows (field
/// `ChatApp::mission`).
#[derive(Debug)]
pub(crate) struct MissionUi {
    /// Selection and scroll of the Active Jobs window.
    pub(crate) jobs: WindowUi,
    /// Selection and scroll of the expanded Finished / Failed window.
    pub(crate) finished: WindowUi,
    /// Finished / Failed shows one row per category instead of the
    /// one-row strip (`Enter`/`f` in the focused window).
    pub(crate) finished_expanded: bool,
    /// LIVE detail requested (`Alt+L`): in the portrait stack it opens the
    /// existing agent detail view of the LIVE source; the landscape centre
    /// window (C3) shows it in place and becomes focusable.
    pub(crate) live_expanded: bool,
    /// Agent pinned as the LIVE source (`p` in the Agents window).
    pub(crate) pinned_agent: Option<String>,
    /// Ranking state of the LIVE fallback source.
    live_rank: RefCell<LiveRank>,
    /// Start of the tool call LIVE shows (see [`ToolClock`]).
    tool_clock: RefCell<Option<ToolClock>>,
}

impl MissionUi {
    /// Fresh state; `epoch` anchors the ranker's millisecond clock.
    pub(crate) fn new(epoch: Instant) -> Self {
        Self {
            jobs: WindowUi::default(),
            finished: WindowUi::default(),
            finished_expanded: false,
            live_expanded: false,
            pinned_agent: None,
            live_rank: RefCell::new(LiveRank {
                ranker: LiveActivityRanker::new(RankerConfig::default()),
                epoch,
            }),
            tool_clock: RefCell::new(None),
        }
    }

    /// Start of the tool call `live` is running, if it is in a tool: the
    /// instant the call first showed up (its `last_meaningful` at that
    /// moment), kept while run, tool name and call count stay the same.
    pub(crate) fn tool_started(&self, live: &AgentLive) -> Option<Instant> {
        if live.phase != AgentPhase::Tool {
            return None;
        }
        let tool = live.current_tool.as_deref()?;
        // Ein gleichzeitiger Zugriff (nie im Zeichenpfad) zählt ab dem
        // letzten Ereignis.
        let Ok(mut clock) = self.tool_clock.try_borrow_mut() else {
            return Some(live.last_meaningful);
        };
        let same_call = clock.as_ref().is_some_and(|clock| {
            clock.run == live.id && clock.tool == tool && clock.calls == live.tool_calls
        });
        if !same_call {
            *clock = Some(ToolClock {
                run: live.id.clone(),
                tool: tool.to_owned(),
                calls: live.tool_calls,
                started: live.last_meaningful,
            });
        }
        clock.as_ref().map(|clock| clock.started)
    }

    /// `p`: pins `agent` as the LIVE source, or unpins it when it already is.
    pub(crate) fn toggle_pin(&mut self, agent: &str) {
        if self.pinned_agent.as_deref() == Some(agent) {
            self.pinned_agent = None;
        } else {
            self.pinned_agent = Some(agent.to_owned());
        }
    }

    /// The LIVE source at `now` (W02 source rules) and whether it is pinned.
    ///
    /// 1. the pinned agent while it runs, has an unseen failure or finished
    ///    less than [`PINNED_TERMINAL_HOLD`] ago;
    /// 2. otherwise the active root orchestrator (most recent activity);
    /// 3. otherwise the highest-ranked [`LiveActivityRanker`] candidate.
    pub(crate) fn live_source<'a>(
        &self,
        monitor: &'a AgentMonitor,
        now: Instant,
    ) -> Option<(&'a AgentLive, bool)> {
        if let Some(pinned) = self
            .pinned_agent
            .as_deref()
            .and_then(|id| monitor.agent(id))
            .filter(|live| {
                projection::is_active(live.phase)
                    || live.is_unseen_failure()
                    || live
                        .finished
                        .is_some_and(|at| now.saturating_duration_since(at) < PINNED_TERMINAL_HOLD)
            })
        {
            return Some((pinned, true));
        }
        let orchestrator = monitor
            .rows()
            .into_iter()
            .map(|(_, live)| live)
            .filter(|live| {
                projection::is_active(live.phase)
                    && live.role.trim()
                        == harw_registry_defaults::profile::role_names::ROOT_ORCHESTRATOR
            })
            .max_by_key(|live| live.last_meaningful);
        if let Some(live) = orchestrator {
            return Some((live, false));
        }
        // Ein gleichzeitiger Zugriff (nie im Zeichenpfad) fällt still auf
        // „keine Quelle“ zurück.
        let top = self
            .live_rank
            .try_borrow_mut()
            .ok()
            .and_then(|mut rank| rank.top(monitor, now))?;
        monitor.agent(&top).map(|live| (live, false))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use harw_core::{AgentEvent, AgentEventKind};
    use harw_protocol::TurnEvent;
    use harw_types::{SessionId, TurnId};

    /// Row `index` of a rendered screen (no panicking index in tests).
    pub(crate) fn screen_row(rows: &[String], index: usize) -> TestResult<&str> {
        rows.get(index)
            .map(String::as_str)
            .ok_or(TestError::Missing("screen row"))
    }

    pub(crate) fn turn_event(id: &str, role: &str, turn: TurnEvent) -> AgentEvent {
        AgentEvent {
            agent: SessionId::from_str(id),
            parent: Some(SessionId::from_str("root-session")),
            role: role.to_owned(),
            kind: AgentEventKind::Turn(turn),
        }
    }

    /// Puts `id` into the thinking phase with `reasoning` as its preview.
    pub(crate) fn thinking(monitor: &mut AgentMonitor, id: &str, role: &str, reasoning: &str) {
        monitor.apply(&turn_event(
            id,
            role,
            TurnEvent::ReasoningDelta {
                turn_id: TurnId::new(),
                text: reasoning.to_owned(),
            },
        ));
    }

    #[test]
    fn window_ui_wraps_pages_and_follows_the_selection() {
        const FIVE: [&str; 5] = ["a", "b", "c", "d", "e"];
        let mut ui = WindowUi::default();
        ui.select_step(false, &FIVE);
        assert_eq!(ui.selected(&FIVE), 4, "up from the first row wraps");
        assert_eq!(
            ui.window_start(&FIVE, 2),
            3,
            "follow brings row 4 into view"
        );
        ui.select_step(true, &FIVE);
        assert_eq!(ui.selected(&FIVE), 0);
        assert_eq!(ui.window_start(&FIVE, 2), 0);
        ui.select_page(true, &FIVE);
        assert_eq!(ui.selected(&FIVE), 1, "page = viewport - 1");
        ui.select_edge(true, &FIVE);
        assert_eq!(ui.selected(&FIVE), 4);
        ui.scroll_by(-10);
        assert_eq!(ui.window_start(&FIVE, 2), 0, "scrolling does not follow");
        ui.scroll_by(99);
        assert_eq!(ui.window_start(&FIVE, 2), 3, "the offset is clamped");
        assert_eq!(
            ui.selected(&["a", "b"]),
            1,
            "the selection is clamped to the rows once its row is gone"
        );
        ui.select_step(true, &[]);
        assert_eq!(ui.selected(&[]), 0);
    }

    /// The selection follows its row (by key) when the rows reorder and only
    /// falls back to the clamped index once the row is gone.
    #[test]
    fn window_ui_selection_follows_its_row_when_rows_reorder() {
        let mut ui = WindowUi::default();
        ui.select_step(true, &["j1", "j2", "j3"]);
        assert_eq!(ui.selected_key(&["j1", "j2", "j3"]), Some("j2"));
        assert_eq!(ui.selected(&["j1", "j3", "j2"]), 2, "j2 moved to the end");
        assert_eq!(ui.selected_key(&["j0", "j1", "j3", "j2"]), Some("j2"));
        assert_eq!(
            ui.selected_key(&["j1", "j3"]),
            Some("j3"),
            "j2 is gone: the clamped index wins and becomes the anchor"
        );
        assert_eq!(ui.selected_key(&["j3", "j1"]), Some("j3"));
    }

    /// The LIVE tool clock keeps the start of one call although later events
    /// move `last_meaningful`; a new call (or another run) restarts it.
    #[test]
    fn tool_clock_keeps_the_start_of_one_call() -> TestResult {
        let mut monitor = AgentMonitor::default();
        let ui = MissionUi::new(Instant::now());
        monitor.apply(&turn_event(
            "w1",
            "uia-worker",
            TurnEvent::ToolCallRequested {
                turn_id: TurnId::new(),
                call_id: harw_types::ToolCallId::new(),
                tool_name: "fs.read".to_owned(),
                arguments: serde_json::json!({}),
            },
        ));
        let live = monitor.agent("w1").ok_or(TestError::Missing("agent"))?;
        let start = ui
            .tool_started(live)
            .ok_or(TestError::Missing("tool start"))?;
        assert_eq!(start, live.last_meaningful);
        let mut later = live.clone();
        later.last_meaningful = start + Duration::from_secs(5);
        assert_eq!(ui.tool_started(&later), Some(start), "same call");
        later.tool_calls = later.tool_calls.saturating_add(1);
        assert_eq!(
            ui.tool_started(&later),
            Some(later.last_meaningful),
            "the next call starts a new clock"
        );
        later.phase = AgentPhase::Thinking;
        assert_eq!(ui.tool_started(&later), None, "no tool, no clock");
        Ok(())
    }

    #[test]
    fn focus_order_matches_the_plan() {
        assert_eq!(
            FOCUS_ORDER,
            [
                PaneFocus::Chat,
                PaneFocus::Agents,
                PaneFocus::Jobs,
                PaneFocus::Finished,
                PaneFocus::Live,
                PaneFocus::Explorer,
                PaneFocus::Workbench,
            ]
        );
    }

    #[test]
    fn live_source_prefers_pin_then_root_orchestrator_then_ranking() -> TestResult {
        let now = Instant::now();
        let mut monitor = AgentMonitor::default();
        let mut ui = MissionUi::new(now);
        assert!(
            ui.live_source(&monitor, now).is_none(),
            "no agents, no source"
        );

        thinking(&mut monitor, "w1", "uia-worker", "");
        let (live, pinned) = ui
            .live_source(&monitor, Instant::now())
            .ok_or(TestError::Missing("ranked source"))?;
        assert_eq!((live.id.as_str(), pinned), ("w1", false));

        thinking(
            &mut monitor,
            "o1",
            harw_registry_defaults::profile::role_names::ROOT_ORCHESTRATOR,
            "",
        );
        let (live, _) = ui
            .live_source(&monitor, Instant::now())
            .ok_or(TestError::Missing("orchestrator source"))?;
        assert_eq!(live.id, "o1", "the active root orchestrator wins");

        ui.toggle_pin("w1");
        let (live, pinned) = ui
            .live_source(&monitor, Instant::now())
            .ok_or(TestError::Missing("pinned source"))?;
        assert_eq!((live.id.as_str(), pinned), ("w1", true));
        ui.toggle_pin("w1");
        assert!(ui.pinned_agent.is_none(), "p again unpins");

        ui.toggle_pin("gone");
        let (live, pinned) = ui
            .live_source(&monitor, Instant::now())
            .ok_or(TestError::Missing("fallback source"))?;
        assert_eq!(
            (live.id.as_str(), pinned),
            ("o1", false),
            "an unknown pin falls back"
        );
        Ok(())
    }
}
