//! Inline activity-ranked live cells in the main chat (PL-97 C5; PL-94 W01
//! M2a + M2c).
//!
//! # Responsibility
//! Every frame, [`LiveSlots::project`] turns the [`LiveActivityRanker`]
//! order of the streaming background runs (their [`ChildStreamBlock`]s from
//! the child stream glue) into a [`LiveSlotProjection`]: where in the tail of
//! the chat up to [`MAX_INLINE_LIVE`] **virtual** live cells are inserted and
//! how many further running runs collapse into one `+N weitere aktiv` row.
//! [`ChatApp::history_segments`] then builds the virtual cell list the history
//! renderer draws: the canonical cells, unchanged and in order, with the
//! virtual cells inserted at the projected slot.
//!
//! # Slot table (W01 §1, slots counted upward from the composer)
//! - UIA idle: the lead live cell sits directly before the latest message
//!   (the last committed answer is #1, the lead run #2).
//! - User message submitted / UIA answer in flight: the insertion point stays
//!   before the answer that preceded the new user message, so the foreground
//!   pair stays in front and the lead run is #3 (with no earlier answer it
//!   sits directly behind the new user message).
//! - UIA reply committed: the new answer is #1, the lead run #2 again.
//!
//! # Anchoring (M2c)
//! While the chat is scrolled away from the tail the projection is frozen:
//! the order, the insertion point and the `+N` count stay exactly as they
//! were, and only a `N neue Aktivitäten` row counts what changed meanwhile.
//! [`crate::chat_scroll::ChatScroll::sync_anchor`] keeps the top visible
//! canonical cell by identity ([`SegmentGeometry`]). Back at the tail, ranking
//! resumes.
//!
//! # Display only
//! The projection never touches the canonical cell vector, the export
//! (`export_capture.rs`, `export_entries`), session persistence, model
//! context or the final reply dedupe (`final_reply.rs`). The virtual cells
//! are read-only views of the existing bounded child blocks, keyed by the
//! child's own session id; scheduling, jobs, permissions and contexts are
//! unaffected.
//!
//! [`ChildStreamBlock`]: crate::child_stream::ChildStreamBlock

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::{ChatApp, Role};
use crate::agent_monitor::{AgentLive, AgentPhase};
use crate::chat_scroll::{CellAnchor, CellId};
use crate::child_stream::{ChildStreamMode, SharedChildBlock};
use crate::live_ranker::{ActivityTier, LiveActivityRanker, LiveCandidate, RankerConfig};
use crate::sanitize::sanitize_inline;
use crate::style;

/// Most virtual live cells shown inline at the same time (W01: one lead
/// background slot plus the next one; the rest collapse into `+N`).
pub(crate) const MAX_INLINE_LIVE: usize = 2;

/// Most tracked child blocks; the oldest entry is dropped beyond this bound.
const MAX_TRACKED_RUNS: usize = 64;

/// Below this many history rows only one live cell is shown inline, so the
/// chat keeps dominating short terminals (W01 acceptance 10).
const TWO_SLOT_MIN_ROWS: u16 = 20;

/// Upper bound of body rows of one virtual live cell.
const LIVE_CELL_MAX_BODY: usize = 8;

/// Lower bound of body rows of one virtual live cell.
const LIVE_CELL_MIN_BODY: usize = 2;

/// Marker of a pinned live cell (pin of the Agents / LIVE window, `p`).
pub(crate) const PINNED_MARKER: &str = "▲ pinned";

/// Snapshot of one tracked run for the ranking (plain data, so the mapping
/// stays testable without the monitor's wall clock).
#[derive(Debug, Clone)]
pub(super) struct RunSnapshot {
    /// Session id of the child run.
    pub(super) id: String,
    /// Role name of the run.
    pub(super) role: String,
    /// Phase per agent monitor; `None` while the monitor does not know it.
    pub(super) phase: Option<AgentPhase>,
    /// A failure the operator has not seen yet.
    pub(super) unseen_failure: bool,
    /// Last meaningful observation of the monitor.
    pub(super) last_event: Option<Instant>,
    /// End of the run per monitor.
    pub(super) finished_at: Option<Instant>,
    /// Last bounded progress message (`parent.message {kind: "info"}`).
    pub(super) info_at: Option<Instant>,
}

impl RunSnapshot {
    /// Latest meaningful instant (monitor event or progress message).
    fn meaningful(&self) -> Option<Instant> {
        [self.last_event, self.info_at].into_iter().flatten().max()
    }
}

/// Ranker tier of a run; same mapping as the status-line order
/// (`live_order_glue.rs`): an unseen failure needs the operator, a finished
/// run is terminal (even with a fresh message), a fresh progress message is a
/// milestone, otherwise the phase decides. Reasoning or token deltas never
/// raise the tier.
fn tier_of(run: &RunSnapshot, now: Instant) -> ActivityTier {
    let fresh_info = run
        .info_at
        .is_some_and(|at| super::child_info_visible(now.saturating_duration_since(at).as_secs()));
    match run.phase {
        _ if run.unseen_failure => ActivityTier::NeedsOperator,
        Some(AgentPhase::Done | AgentPhase::Failed | AgentPhase::Cancelled) => {
            ActivityTier::Terminal
        }
        _ if fresh_info => ActivityTier::Milestone,
        Some(AgentPhase::Admitted | AgentPhase::Thinking | AgentPhase::Tool) => {
            ActivityTier::Active
        }
        Some(AgentPhase::Waiting) | None => ActivityTier::Quiet,
    }
}

/// One virtual live cell of the projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiveSlot {
    /// Session id of the child run (the cell's UI-only identity).
    pub(crate) run_id: String,
    /// Role name of the run.
    pub(crate) role: String,
    /// The run is the pinned agent of the Agents / LIVE window.
    pub(crate) pinned: bool,
    /// The run is terminal (bounded final display).
    pub(crate) terminal: bool,
}

/// Result of one projection: where the virtual cells go and what they show.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LiveSlotProjection {
    /// Index of the canonical cell the virtual cells are inserted before
    /// (`== cell count`: at the very end).
    pub(crate) insert_at: usize,
    /// Virtual live cells, lead first (the lead is the one nearest to the
    /// composer).
    pub(crate) live: Vec<LiveSlot>,
    /// Further running runs collapsed into the `+N weitere aktiv` row.
    pub(crate) more: usize,
    /// Activity observed while frozen (`N neue Aktivitäten`).
    pub(crate) new_activity: usize,
    /// The projection is frozen because the chat is scrolled away.
    pub(crate) frozen: bool,
}

impl LiveSlotProjection {
    /// Visual slot of `run_id` counted upward from the composer, when every
    /// canonical cell after the insertion point counts as one slot (true for
    /// a chat of plain messages, as in the W01 table).
    #[must_use]
    pub(crate) fn slot_of(&self, cell_count: usize, run_id: &str) -> Option<usize> {
        let rank = self.live.iter().position(|slot| slot.run_id == run_id)?;
        Some(cell_count.saturating_sub(self.insert_at) + rank + 1)
    }
}

/// Positions of the latest user and assistant messages in the canonical
/// cell vector (recorded when they are appended).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MessageMarks {
    /// Index of the latest user message.
    last_user: Option<usize>,
    /// Index of the latest assistant answer.
    last_assistant: Option<usize>,
    /// Latest assistant answer before the latest user message.
    answer_before_user: Option<usize>,
}

impl MessageMarks {
    /// Records a message appended at `index`.
    fn note(&mut self, role: Role, index: usize) {
        match role {
            Role::User => {
                self.answer_before_user = self.last_assistant;
                self.last_user = Some(index);
            }
            Role::Assistant => self.last_assistant = Some(index),
            Role::System => {}
        }
    }

    /// Insertion point of the virtual cells per the W01 slot table (see the
    /// module documentation). Never beyond `cell_count`.
    fn insertion_point(&self, cell_count: usize, busy: bool) -> usize {
        let valid = |index: Option<usize>| index.filter(|index| *index < cell_count);
        let last_user = valid(self.last_user);
        let point = if busy {
            // Hinter das Vordergrund-Paar: vor die Antwort, die der neuen
            // Nutzernachricht voranging, sonst direkt vor die Nutzernachricht.
            valid(self.answer_before_user)
                .filter(|answer| last_user.is_none_or(|user| *answer < user))
                .or(last_user)
        } else {
            // Leerlauf: direkt vor die jüngste Nachricht (meist die Antwort).
            last_user.max(valid(self.last_assistant))
        };
        point.unwrap_or(cell_count).min(cell_count)
    }
}

/// One tracked child block: its run, its canonical home cell and the shared
/// block the virtual cell renders.
#[derive(Debug)]
struct TrackedRun {
    id: String,
    role: String,
    /// Index of the canonical cell holding the block (`None` after `/clear`).
    home: Option<usize>,
    block: SharedChildBlock,
}

/// Projection frozen while the chat is scrolled away from the tail.
#[derive(Debug, Clone)]
struct Frozen {
    projection: LiveSlotProjection,
    since: Instant,
    known: HashSet<String>,
}

/// Inputs of one projection that come from the foreground state.
#[derive(Debug, Clone, Copy)]
pub(super) struct Foreground {
    /// Number of canonical cells.
    pub(super) cell_count: usize,
    /// A UIA turn is in flight (user message submitted, answer pending).
    pub(super) busy: bool,
    /// The chat is scrolled away from the tail (identity-anchored).
    pub(super) anchored: bool,
    /// Live cells that fit (1 on short terminals, else [`MAX_INLINE_LIVE`]).
    pub(super) max_inline: usize,
}

/// Live slot state of [`ChatApp`] (field `live_slots`, behind a `RefCell`
/// because drawing only has `&ChatApp`).
#[derive(Debug)]
pub(crate) struct LiveSlots {
    ranker: LiveActivityRanker,
    epoch: Instant,
    marks: MessageMarks,
    runs: Vec<TrackedRun>,
    last: Option<LiveSlotProjection>,
    frozen: Option<Frozen>,
}

impl LiveSlots {
    /// Fresh state; `epoch` anchors the ranker's millisecond clock. A
    /// separate ranker instance from the status line and the LIVE window,
    /// so the projections never disturb each other's coalescing.
    pub(crate) fn new(epoch: Instant) -> Self {
        Self {
            ranker: LiveActivityRanker::new(RankerConfig::default()),
            epoch,
            marks: MessageMarks::default(),
            runs: Vec::new(),
            last: None,
            frozen: None,
        }
    }

    /// Records a user/assistant message appended at canonical `index`.
    pub(crate) fn note_message(&mut self, role: Role, index: usize) {
        self.marks.note(role, index);
    }

    /// `/clear`: the canonical cells are gone, so are their positions. The
    /// runs stay tracked (they keep running) but lose their home cell.
    pub(crate) fn clear_cells(&mut self) {
        self.marks = MessageMarks::default();
        for run in &mut self.runs {
            run.home = None;
        }
        self.last = None;
        self.frozen = None;
    }

    /// Tracks the block of a streaming child whose canonical home is `home`.
    pub(crate) fn track(&mut self, id: &str, role: &str, home: usize, block: SharedChildBlock) {
        self.runs.retain(|run| run.id != id);
        self.runs.push(TrackedRun {
            id: id.to_owned(),
            role: role.to_owned(),
            home: Some(home),
            block,
        });
        while self.runs.len() > MAX_TRACKED_RUNS {
            self.runs.remove(0);
        }
    }

    fn millis_since_epoch(&self, at: Instant) -> u64 {
        u64::try_from(at.saturating_duration_since(self.epoch).as_millis()).unwrap_or(u64::MAX)
    }

    fn candidate(&self, run: &RunSnapshot, now: Instant) -> LiveCandidate {
        let tier = tier_of(run, now);
        let terminal_since_ms = if tier == ActivityTier::Terminal {
            run.finished_at.map(|at| self.millis_since_epoch(at))
        } else {
            None
        };
        LiveCandidate {
            run_id: run.id.clone(),
            // Nur Blöcke direkter Kinder dieser Sitzung werden verfolgt.
            session_visible: true,
            tier,
            last_meaningful_ms: run
                .meaningful()
                .map_or(0, |at| self.millis_since_epoch(at)),
            reasoning_deltas_since_last_eval: 0,
            terminal_since_ms,
        }
    }

    /// Projection at `now` (see the module documentation).
    ///
    /// # Arguments
    /// - `now`: the frame's instant.
    /// - `fg`: foreground state (cell count, busy, anchored, room).
    /// - `runs`: snapshots of the tracked runs.
    /// - `pin`: pinned agent of the Agents / LIVE window.
    pub(super) fn project(
        &mut self,
        now: Instant,
        fg: Foreground,
        runs: &[RunSnapshot],
        pin: Option<&str>,
    ) -> LiveSlotProjection {
        if fg.anchored {
            if self.frozen.is_none() {
                let base = match self.last.clone() {
                    Some(last) => last,
                    None => self.fresh(now, fg, runs, pin),
                };
                let known = base.live.iter().map(|slot| slot.run_id.clone()).collect();
                self.frozen = Some(Frozen {
                    projection: base,
                    since: now,
                    known,
                });
            }
            if let Some(frozen) = self.frozen.as_ref() {
                let mut projection = frozen.projection.clone();
                projection.insert_at = projection.insert_at.min(fg.cell_count);
                projection.new_activity = runs
                    .iter()
                    .filter(|run| {
                        run.meaningful().is_some_and(|at| at > frozen.since)
                            || (!frozen.known.contains(&run.id)
                                && tier_of(run, now) != ActivityTier::Terminal)
                    })
                    .count();
                projection.frozen = true;
                return projection;
            }
        }
        self.frozen = None;
        let projection = self.fresh(now, fg, runs, pin);
        self.last = Some(projection.clone());
        projection
    }

    /// Unfrozen projection: rank, take the lead cells, count the rest.
    fn fresh(
        &mut self,
        now: Instant,
        fg: Foreground,
        runs: &[RunSnapshot],
        pin: Option<&str>,
    ) -> LiveSlotProjection {
        let candidates: Vec<LiveCandidate> =
            runs.iter().map(|run| self.candidate(run, now)).collect();
        let tiers: HashMap<&str, ActivityTier> = candidates
            .iter()
            .map(|candidate| (candidate.run_id.as_str(), candidate.tier))
            .collect();
        self.ranker.pin(pin.map(str::to_owned));
        let ranked = self.ranker.rank(self.millis_since_epoch(now), &candidates);
        let is_terminal =
            |id: &str| tiers.get(id).copied() == Some(ActivityTier::Terminal);
        let live = ranked
            .iter()
            .take(fg.max_inline)
            .map(|id| LiveSlot {
                run_id: id.clone(),
                role: runs
                    .iter()
                    .find(|run| run.id == *id)
                    .map(|run| run.role.clone())
                    .unwrap_or_default(),
                pinned: pin == Some(id.as_str()) && !is_terminal(id),
                terminal: is_terminal(id),
            })
            .collect();
        let more = ranked
            .iter()
            .skip(fg.max_inline)
            .filter(|id| !is_terminal(id))
            .count();
        LiveSlotProjection {
            insert_at: self.marks.insertion_point(fg.cell_count, fg.busy),
            live,
            more,
            new_activity: 0,
            frozen: false,
        }
    }
}

/// Identity of one segment of the virtual cell list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SegmentId {
    /// A canonical cell, unchanged.
    Cell(CellId),
    /// The virtual live cell of a run.
    Live(String),
    /// The `+N weitere aktiv` row.
    More,
}

/// One segment of the virtual cell list with its rendered lines.
#[derive(Debug, Clone)]
pub(crate) struct HistorySegment {
    pub(crate) id: SegmentId,
    pub(crate) lines: Vec<Line<'static>>,
}

/// Wrapped-row geometry of the virtual cell list, for the identity anchor.
#[derive(Debug, Clone, Default)]
pub(crate) struct SegmentGeometry {
    /// (`CellId` of canonical segments, first row, rows) in display order.
    rows: Vec<(Option<CellId>, usize, usize)>,
}

impl SegmentGeometry {
    /// Measures `segments` wrapped at `width` (same wrapping as the history
    /// paragraph, so the rows add up to its line count).
    pub(crate) fn measure(segments: &[HistorySegment], width: u16) -> Self {
        let mut start = 0usize;
        let rows = segments
            .iter()
            .map(|segment| {
                let height = Paragraph::new(segment.lines.clone())
                    .wrap(Wrap { trim: false })
                    .line_count(width);
                let cell = match segment.id {
                    SegmentId::Cell(cell) => Some(cell),
                    SegmentId::Live(_) | SegmentId::More => None,
                };
                let entry = (cell, start, height);
                start = start.saturating_add(height);
                entry
            })
            .collect();
        Self { rows }
    }

    /// First row of the anchored row, `None` once the cell is gone.
    pub(crate) fn resolve(&self, anchor: CellAnchor) -> Option<usize> {
        self.rows
            .iter()
            .find(|(cell, _, _)| *cell == Some(anchor.cell))
            .map(|(_, start, rows)| start + anchor.line.min(rows.saturating_sub(1)))
    }

    /// Anchor of `top`: the last canonical cell starting at or before it.
    pub(crate) fn locate(&self, top: usize) -> Option<CellAnchor> {
        self.rows
            .iter()
            .rev()
            .find_map(|(cell, start, _)| match cell {
                Some(cell) if *start <= top => Some(CellAnchor {
                    cell: *cell,
                    line: top - start,
                }),
                _ => None,
            })
    }
}

/// Label of the frozen-activity row (`None` when nothing happened).
#[must_use]
pub(crate) fn new_activity_label(count: usize) -> Option<String> {
    match count {
        0 => None,
        1 => Some("↓ 1 neue Aktivität".to_owned()),
        n => Some(format!("↓ {n} neue Aktivitäten")),
    }
}

/// Body rows of one virtual live cell for `history_rows` visible rows.
fn body_rows(history_rows: u16) -> usize {
    (usize::from(history_rows) / 4).clamp(LIVE_CELL_MIN_BODY, LIVE_CELL_MAX_BODY)
}

/// Lines of one virtual live cell: header (`◆ live · role · phase`, plus the
/// pinned marker) and the bounded tail of the run's child block.
fn live_cell_lines(
    slot: &LiveSlot,
    block: Option<&SharedChildBlock>,
    live: Option<&AgentLive>,
    width: u16,
    theme: style::Theme,
    max_body: usize,
) -> Vec<Line<'static>> {
    let accent = ratatui::style::Style::default()
        .fg(style::accent_color(theme))
        .add_modifier(Modifier::BOLD);
    let dim = style::dim_style(theme);
    let mut header = vec![
        Span::styled("◆ ", accent),
        Span::styled(format!("live · {}", sanitize_inline(&slot.role)), accent),
    ];
    if let Some(live) = live {
        header.push(Span::styled(format!(" · {}", live.phase.label()), dim));
    }
    if slot.pinned {
        header.push(Span::styled(
            format!(" {PINNED_MARKER}"),
            style::warning_style(theme),
        ));
    }
    let mut lines = vec![Line::from(header)];
    let body = match block.map(|block| block.lock()) {
        Some(Ok(guard)) => crate::history_cell::HistoryCell::display_lines(&*guard, width, theme),
        Some(Err(_)) => vec![Line::styled(
            "  │ ⚠ Live-Block nicht lesbar (Sperre vergiftet)".to_owned(),
            style::warning_style(theme),
        )],
        None => Vec::new(),
    };
    if body.len() > max_body {
        let keep = max_body.saturating_sub(1).max(1);
        let hidden = body.len() - keep;
        lines.push(Line::styled(format!("  │ … {hidden} Zeilen darüber"), dim));
        lines.extend(body.into_iter().skip(hidden));
    } else {
        lines.extend(body);
    }
    lines
}

impl ChatApp {
    /// Records a user/assistant message about to be appended (history hook
    /// of `push_line`).
    pub(super) fn note_live_slot_message(&mut self, role: Role) {
        let index = self.cells.len();
        self.live_slots.get_mut().note_message(role, index);
    }

    /// `/clear` hook: forget canonical positions.
    pub(super) fn clear_live_slot_cells(&mut self) {
        self.live_slots.get_mut().clear_cells();
    }

    /// Tracks a child block about to be appended as canonical cell (hook of
    /// the child stream glue).
    pub(super) fn track_live_child_block(
        &mut self,
        child_id: &str,
        role: &str,
        block: SharedChildBlock,
    ) {
        let home = self.cells.len();
        self.live_slots
            .get_mut()
            .track(child_id, role, home, block);
    }

    /// Snapshots of the tracked runs (none while the child stream is off).
    fn live_run_snapshots(&self, slots: &LiveSlots) -> Vec<RunSnapshot> {
        if self.child_stream.mode() == ChildStreamMode::Off {
            return Vec::new();
        }
        slots
            .runs
            .iter()
            .map(|run| {
                let live = self.agent_monitor.agent(&run.id);
                RunSnapshot {
                    id: run.id.clone(),
                    role: live.map_or_else(|| run.role.clone(), |live| live.role.clone()),
                    phase: live.map(|live| live.phase),
                    unseen_failure: live.is_some_and(AgentLive::is_unseen_failure),
                    last_event: live.map(|live| live.last_meaningful),
                    finished_at: live.and_then(|live| live.finished),
                    info_at: self.child_infos.get(&run.id).map(|(_, at)| *at),
                }
            })
            .collect()
    }

    /// Live slot projection of this frame (history rendering hook).
    ///
    /// # Arguments
    /// - `now`: the frame's instant.
    /// - `history_rows`: visible rows of the chat history.
    pub(super) fn live_slot_projection(&self, now: Instant, history_rows: u16) -> LiveSlotProjection {
        let Ok(mut slots) = self.live_slots.try_borrow_mut() else {
            return LiveSlotProjection {
                insert_at: self.cells.len(),
                ..LiveSlotProjection::default()
            };
        };
        let runs = self.live_run_snapshots(&slots);
        let fg = Foreground {
            cell_count: self.cells.len(),
            busy: self.active_cancel.is_some(),
            anchored: !self.scroll.is_following(),
            max_inline: if history_rows >= TWO_SLOT_MIN_ROWS {
                MAX_INLINE_LIVE
            } else {
                1
            },
        };
        slots.project(now, fg, &runs, self.mission.pinned_agent.as_deref())
    }

    /// The virtual cell list: canonical cells in order, with the projected
    /// live cells (and the `+N` row) inserted before `insert_at`. A run shown
    /// as a live cell renders a one-line pointer at its canonical home
    /// instead of the same block twice; the canonical cell itself is not
    /// touched.
    pub(super) fn history_segments(
        &self,
        projection: &LiveSlotProjection,
        width: u16,
        theme: style::Theme,
        history_rows: u16,
    ) -> Vec<HistorySegment> {
        let slots = self.live_slots.try_borrow().ok();
        let block_of = |id: &str| {
            slots
                .as_ref()
                .and_then(|slots| slots.runs.iter().find(|run| run.id == id))
        };
        let homes: HashSet<usize> = projection
            .live
            .iter()
            .filter_map(|slot| block_of(&slot.run_id).and_then(|run| run.home))
            .collect();
        let dim = style::dim_style(theme);
        let insert_at = projection.insert_at.min(self.cells.len());
        let mut virtual_cells: Vec<HistorySegment> = Vec::new();
        if projection.more > 0 {
            virtual_cells.push(HistorySegment {
                id: SegmentId::More,
                lines: vec![Line::styled(
                    format!("◇ +{} weitere aktiv", projection.more),
                    dim,
                )],
            });
        }
        // Der Lead steht dem Composer am nächsten, also zuletzt.
        for slot in projection.live.iter().rev() {
            let block = block_of(&slot.run_id).map(|run| &run.block);
            virtual_cells.push(HistorySegment {
                id: SegmentId::Live(slot.run_id.clone()),
                lines: live_cell_lines(
                    slot,
                    block,
                    self.agent_monitor.agent(&slot.run_id),
                    width,
                    theme,
                    body_rows(history_rows),
                ),
            });
        }
        let mut segments = Vec::with_capacity(self.cells.len() + virtual_cells.len());
        let mut pending = Some(virtual_cells);
        for (index, cell) in self.cells.iter().enumerate() {
            if index == insert_at
                && let Some(cells) = pending.take()
            {
                segments.extend(cells);
            }
            let lines = if homes.contains(&index) {
                let arrow = if index < insert_at {
                    "↓ live weiter unten"
                } else {
                    "↑ live weiter oben"
                };
                vec![Line::styled(format!("  │ {arrow}"), dim)]
            } else {
                cell.display_lines(width, theme)
            };
            segments.push(HistorySegment {
                id: SegmentId::Cell(CellId(index)),
                lines,
            });
        }
        if let Some(cells) = pending {
            segments.extend(cells);
        }
        segments
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use harw_core::{AgentEvent, AgentEventKind};
    use harw_protocol::events::TurnEvent;
    use harw_registry_defaults::profile::role_names;
    use harw_types::{SessionId, TurnId};

    use super::super::{Spinner, TurnEventState, handle_turn_event, render_viewport, tests::test_chat_app};
    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn at(base: Instant, ms: u64) -> TestResult<Instant> {
        base.checked_add(Duration::from_millis(ms))
            .ok_or(TestError::Missing("Zeitpunkt nach der Basis"))
    }

    fn snapshot(id: &str, phase: AgentPhase, last: Instant) -> RunSnapshot {
        RunSnapshot {
            id: id.to_owned(),
            role: role_names::ROOT_ORCHESTRATOR.to_owned(),
            phase: Some(phase),
            unseen_failure: false,
            last_event: Some(last),
            finished_at: None,
            info_at: None,
        }
    }

    fn fg(cell_count: usize) -> Foreground {
        Foreground {
            cell_count,
            busy: false,
            anchored: false,
            max_inline: MAX_INLINE_LIVE,
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|span| span.content.as_ref()).collect()
    }

    /// Text der Segmente in Anzeigereihenfolge.
    fn segment_text(segments: &[HistorySegment]) -> Vec<String> {
        segments
            .iter()
            .flat_map(|segment| segment.lines.iter().map(line_text))
            .collect()
    }

    /// Kanonische Zellen (gerendert) und Export als Vergleichswert.
    fn canonical(app: &ChatApp) -> (Vec<String>, String) {
        let cells = app
            .cells
            .iter()
            .flat_map(|cell| cell.display_lines(100, style::Theme::Dark))
            .map(|line| line_text(&line))
            .collect();
        (cells, format!("{:?}", app.export_entries))
    }

    fn bus(app: &mut ChatApp, id: &str, event: TurnEvent) {
        let event = AgentEvent {
            agent: SessionId::from_str(id),
            parent: Some(app.session_id().clone()),
            role: role_names::ROOT_ORCHESTRATOR.to_owned(),
            kind: AgentEventKind::Turn(event),
        };
        app.agent_monitor.apply(&event);
        if let AgentEventKind::Turn(turn) = &event.kind {
            app.child_stream.apply(
                &event.agent,
                event.parent.as_ref(),
                &event.role,
                turn,
            );
        }
    }

    /// Startet ein Orchestrator-Kind wie der echte Pfad: `ChildSpawned` der
    /// Wurzel (Agent-Zeile + Block), dann ein erstes Bus-Ereignis.
    fn spawn(app: &mut ChatApp, id: &str, text: &str) -> TestResult {
        let mut state = TurnEventState::default();
        if !handle_turn_event(
            app,
            &mut state,
            TurnEvent::ChildSpawned {
                turn_id: TurnId::new(),
                child: SessionId::from_str(id),
                role: role_names::ROOT_ORCHESTRATOR.to_owned(),
                question: Some("Baue X".to_owned()),
            },
        ) {
            return Err(TestError::Missing("ChildSpawned behandelt"));
        }
        bus(
            app,
            id,
            TurnEvent::AssistantDelta {
                turn_id: TurnId::new(),
                text: text.to_owned(),
            },
        );
        Ok(())
    }

    fn projection(app: &ChatApp, now: Instant) -> LiveSlotProjection {
        app.live_slot_projection(now, 40)
    }

    /// W01 Abnahme 1: ein Hintergrund-Root steht im Leerlauf auf #2, nach
    /// einer neuen Nutzernachricht (UIA arbeitet) auf #3 und nach der
    /// Schlussantwort wieder auf #2 — ohne Identitätswechsel und ohne dass
    /// sich kanonische Zellen oder Export ändern.
    #[test]
    fn root_moves_between_slot_two_and_three_around_a_uia_turn() -> TestResult {
        let mut app = test_chat_app()?;
        app.push_line(Role::User, "Starte den Root");
        spawn(&mut app, "root-1", "Plane Welle 1")?;
        app.push_line(Role::Assistant, "Root läuft im Hintergrund");
        let now = Instant::now();

        let idle = projection(&app, now);
        assert_eq!(idle.slot_of(app.cells_len(), "root-1"), Some(2), "{idle:?}");

        let before = canonical(&app);
        app.push_line(Role::User, "Nächste Frage");
        app.active_cancel = Some(harw_core::cancel::CancelToken::new());
        let busy = projection(&app, at(now, 1000)?);
        assert_eq!(busy.slot_of(app.cells_len(), "root-1"), Some(3), "{busy:?}");
        // Die Ansicht ändert nichts an den kanonischen Zellen davor.
        let (cells_now, _) = canonical(&app);
        assert!(cells_now.starts_with(&before.0));

        app.active_cancel = None;
        super::super::final_reply::commit_final_reply(&mut app, "Antwort");
        let done = projection(&app, at(now, 2000)?);
        assert_eq!(done.slot_of(app.cells_len(), "root-1"), Some(2), "{done:?}");
        assert_eq!(done.live.len(), 1);

        // Segmentreihenfolge: … Frage, live-Zelle, Antwort.
        let text = segment_text(&app.history_segments(&done, 100, style::Theme::Dark, 40));
        let live = text
            .iter()
            .position(|line| line.starts_with("◆ live"))
            .ok_or(TestError::Missing("Live-Zelle"))?;
        let answer = text
            .iter()
            .rposition(|line| line.contains("Antwort"))
            .ok_or(TestError::Missing("Antwort"))?;
        assert!(live < answer, "{text:?}");
        Ok(())
    }

    /// W01 Abnahme 2 + 10: drei gleichzeitige Läufe ergeben zwei Live-Zellen
    /// und genau eine `+1 weitere aktiv`-Zeile; frische bedeutsame Aktivität
    /// befördert einen anderen Lauf an die Spitze.
    #[test]
    fn three_concurrent_runs_show_two_cells_and_one_more_row() -> TestResult {
        let epoch = Instant::now();
        let mut slots = LiveSlots::new(epoch);
        let runs = [
            snapshot("a-root", AgentPhase::Thinking, at(epoch, 1000)?),
            snapshot("b-orch", AgentPhase::Thinking, at(epoch, 2000)?),
            snapshot("c-orch", AgentPhase::Waiting, at(epoch, 3000)?),
        ];
        let first = slots.project(at(epoch, 4000)?, fg(4), &runs, None);
        let ids: Vec<&str> = first.live.iter().map(|slot| slot.run_id.as_str()).collect();
        assert_eq!(ids, ["b-orch", "a-root"]);
        assert_eq!(first.more, 1);

        // Später meldet der älteste Root einen Meilenstein.
        let mut promoted = runs.clone();
        if let Some(root) = promoted.first_mut() {
            root.info_at = Some(at(epoch, 9000)?);
        }
        let second = slots.project(at(epoch, 10_000)?, fg(4), &promoted, None);
        assert_eq!(
            second.live.first().map(|slot| slot.run_id.as_str()),
            Some("a-root")
        );
        assert_eq!(second.more, 1);

        // Kurzes Terminal: nur eine Live-Zelle, der Rest im `+N`.
        let short = slots.project(
            at(epoch, 20_000)?,
            Foreground {
                max_inline: 1,
                ..fg(4)
            },
            &promoted,
            None,
        );
        assert_eq!(short.live.len(), 1);
        assert_eq!(short.more, 2);
        Ok(())
    }

    /// W01 Abnahme 3: 500 Reasoning-Deltas eines geschwätzigen Laufs stehlen
    /// einem Lauf mit Meilenstein nicht die Spitze.
    #[test]
    fn reasoning_spam_cannot_outrank_a_milestone() -> TestResult {
        let mut app = test_chat_app()?;
        app.push_line(Role::User, "los");
        spawn(&mut app, "quiet-milestone", "Meilenstein erreicht")?;
        spawn(&mut app, "chatty", "rede")?;
        app.note_child_info("quiet-milestone".to_owned(), "Welle 1 fertig".to_owned());
        for index in 0..500 {
            bus(
                &mut app,
                "chatty",
                TurnEvent::ReasoningDelta {
                    turn_id: TurnId::new(),
                    text: format!("denke {index} "),
                },
            );
        }
        let lead = projection(&app, Instant::now());
        assert_eq!(
            lead.live.first().map(|slot| slot.run_id.as_str()),
            Some("quiet-milestone"),
            "{lead:?}"
        );
        Ok(())
    }

    fn render_rows(app: &ChatApp, width: u16, height: u16) -> TestResult<Vec<String>> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                .map_err(|_| TestError::Missing("Testterminal"))?;
        terminal
            .draw(|frame| render_viewport(frame, app, &Spinner::new(), None))
            .map_err(|_| TestError::Missing("Zeichnen"))?;
        let buffer = terminal.backend().buffer();
        Ok((0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect())
    }

    /// W01 Abnahme 5: Hochscrollen friert die Reihenfolge ein, hält dieselbe
    /// oberste Zelle und zeigt `N neue Aktivitäten`; zurück am Ende wird
    /// wieder gerankt.
    #[test]
    fn scroll_up_freezes_order_keeps_top_cell_and_counts_new_activity() -> TestResult {
        let mut app = test_chat_app()?;
        for index in 0..40 {
            app.push_line(Role::System, format!("verlauf-zeile-{index:02}"));
        }
        app.push_line(Role::User, "Frage");
        spawn(&mut app, "run-a", "A arbeitet")?;
        spawn(&mut app, "run-b", "B arbeitet")?;
        app.push_line(Role::Assistant, "Antwort");
        let now = Instant::now();
        let tail = projection(&app, now);
        let order: Vec<String> = tail.live.iter().map(|slot| slot.run_id.clone()).collect();
        assert_eq!(order.len(), 2);

        render_rows(&app, 100, 30)?;
        app.scroll.scroll_up(10, 200, 20);
        let rows = render_rows(&app, 100, 30)?;
        let top = app.scroll.anchor().ok_or(TestError::Missing("Anker"))?;
        let first_visible = rows
            .iter()
            .find(|row| row.contains("verlauf-zeile-"))
            .cloned()
            .ok_or(TestError::Missing("sichtbare Verlaufszeile"))?;

        // Der bisher zweite Lauf meldet einen Meilenstein; der Verlauf wächst.
        let second = order.get(1).cloned().ok_or(TestError::Missing("zweiter"))?;
        app.note_child_info(second.clone(), "Meilenstein".to_owned());
        bus(
            &mut app,
            &second,
            TurnEvent::AssistantDelta {
                turn_id: TurnId::new(),
                text: " und mehr".to_owned(),
            },
        );
        app.push_line(Role::System, "neu unten");
        let rows = render_rows(&app, 100, 30)?;
        assert_eq!(app.scroll.anchor(), Some(top), "gleiche oberste Zelle");
        assert!(rows.contains(&first_visible), "{rows:#?}");
        let frozen = projection(&app, at(now, 10_000)?);
        assert!(frozen.frozen);
        let frozen_order: Vec<String> =
            frozen.live.iter().map(|slot| slot.run_id.clone()).collect();
        assert_eq!(frozen_order, order, "eingefroren");
        assert!(frozen.new_activity >= 1, "{frozen:?}");
        assert!(
            rows.iter().any(|row| row.contains("neue Aktivit")),
            "{rows:#?}"
        );

        app.scroll.jump_to_bottom();
        let resumed = projection(&app, at(now, 20_000)?);
        assert!(!resumed.frozen);
        assert_eq!(resumed.new_activity, 0);
        assert_eq!(
            resumed.live.first().map(|slot| slot.run_id.as_str()),
            Some(second.as_str()),
            "Rangfolge läuft wieder"
        );
        Ok(())
    }

    /// W01 Abnahme 6: der Pin des Agenten-/LIVE-Fensters hält den gewählten
    /// Lauf vorn und markiert ihn mit `▲ pinned`; ohne Pin entscheidet
    /// wieder die Aktivität.
    #[test]
    fn pin_keeps_the_selected_run_in_front_with_a_marker() -> TestResult {
        let mut app = test_chat_app()?;
        app.push_line(Role::User, "los");
        spawn(&mut app, "pinned-root", "still")?;
        spawn(&mut app, "busy-orch", "laut")?;
        app.note_child_info("busy-orch".to_owned(), "Meilenstein".to_owned());
        app.mission.toggle_pin("pinned-root");
        let now = Instant::now();
        let pinned = projection(&app, now);
        let lead = pinned.live.first().ok_or(TestError::Missing("Lead"))?;
        assert_eq!(lead.run_id, "pinned-root");
        assert!(lead.pinned);
        let text = segment_text(&app.history_segments(&pinned, 100, style::Theme::Dark, 40));
        assert!(
            text.iter()
                .any(|line| line.starts_with("◆ live") && line.contains(PINNED_MARKER)),
            "{text:?}"
        );
        app.mission.toggle_pin("pinned-root");
        let released = projection(&app, at(now, 10_000)?);
        assert_eq!(
            released.live.first().map(|slot| slot.run_id.as_str()),
            Some("busy-orch")
        );
        assert!(released.live.iter().all(|slot| !slot.pinned));
        Ok(())
    }

    /// W01 Abnahme 7: ein beendeter Lauf bleibt `terminal_display_ms` lang
    /// sichtbar und verlässt danach die Live-Rangliste; der Verlauf behält
    /// seine Zellen.
    #[test]
    fn terminal_run_leaves_live_ranking_after_terminal_display_ms() -> TestResult {
        let epoch = Instant::now();
        let mut slots = LiveSlots::new(epoch);
        let display = RankerConfig::default().terminal_display_ms;
        let mut done = snapshot("done-run", AgentPhase::Done, at(epoch, 1000)?);
        done.finished_at = Some(at(epoch, 1000)?);
        let running = snapshot("running", AgentPhase::Thinking, at(epoch, 500)?);
        let runs = [done, running];
        let shortly = slots.project(at(epoch, 1000 + display - 1)?, fg(2), &runs, None);
        assert!(
            shortly
                .live
                .iter()
                .any(|slot| slot.run_id == "done-run" && slot.terminal),
            "{shortly:?}"
        );
        assert_eq!(shortly.more, 0, "terminal zählt nicht als aktiv");
        let later = slots.project(at(epoch, 1000 + display + 1000)?, fg(2), &runs, None);
        let ids: Vec<&str> = later.live.iter().map(|slot| slot.run_id.as_str()).collect();
        assert_eq!(ids, ["running"]);
        Ok(())
    }

    /// W01 Abnahme 8 + 9: jede Live-Zelle zeigt nur den Block ihres eigenen
    /// Laufs; die kanonische Heimat des Blocks zeigt einen Verweis statt
    /// derselben Zeilen doppelt, und wiederholte Projektionen legen keine
    /// Zellen an.
    #[test]
    fn live_cells_show_only_their_own_block_without_duplicates() -> TestResult {
        let mut app = test_chat_app()?;
        app.push_line(Role::User, "los");
        spawn(&mut app, "run-x", "nur-x-text")?;
        spawn(&mut app, "run-y", "nur-y-text")?;
        app.push_line(Role::Assistant, "fertig");
        let cells = app.cells_len();
        let mut segments = Vec::new();
        for step in 0..5u64 {
            let p = projection(&app, at(Instant::now(), step * 1000)?);
            segments = app.history_segments(&p, 100, style::Theme::Dark, 40);
        }
        assert_eq!(app.cells_len(), cells, "keine neuen kanonischen Zellen");
        let text = segment_text(&segments);
        for marker in ["nur-x-text", "nur-y-text"] {
            assert_eq!(
                text.iter().filter(|line| line.contains(marker)).count(),
                1,
                "{marker} genau einmal: {text:?}"
            );
        }
        let mut current: Option<&str> = None;
        for segment in &segments {
            if let SegmentId::Live(id) = &segment.id {
                current = Some(id.as_str());
                let other = if id == "run-x" { "nur-y-text" } else { "nur-x-text" };
                assert!(
                    segment.lines.iter().all(|line| !line_text(line).contains(other)),
                    "keine Vermischung in {id}"
                );
            }
        }
        assert!(current.is_some());
        assert!(text.iter().any(|line| line.contains("live weiter unten")));
        Ok(())
    }

    /// W01 §3 / PL-97 C5: Projektion und Zeichnen ändern weder die
    /// kanonischen Zellen noch den Export, auch nicht über einen Turn mit
    /// Schlussantwort und Hochscrollen hinweg.
    #[test]
    fn canonical_cells_and_export_are_unchanged_by_the_projection() -> TestResult {
        let mut app = test_chat_app()?;
        app.push_line(Role::User, "Frage");
        spawn(&mut app, "root-1", "arbeite")?;
        super::super::final_reply::commit_final_reply(&mut app, "Antwort");
        let before = canonical(&app);
        let cells = app.cells_len();
        render_rows(&app, 100, 30)?;
        projection(&app, Instant::now());
        app.scroll.scroll_up(3, 100, 10);
        render_rows(&app, 100, 30)?;
        app.scroll.jump_to_bottom();
        render_rows(&app, 60, 20)?;
        assert_eq!(app.cells_len(), cells);
        assert_eq!(canonical(&app), before);
        Ok(())
    }

    /// W01 Abnahme 10: auf kurzen Terminals passt nur eine Live-Zelle, und
    /// ihr Körper ist begrenzt, damit der Chat dominiert.
    #[test]
    fn short_terminals_get_one_bounded_live_cell() -> TestResult {
        let mut app = test_chat_app()?;
        app.push_line(Role::User, "los");
        spawn(&mut app, "run-1", "eins")?;
        spawn(&mut app, "run-2", "zwei")?;
        let long: String = (0..60).map(|index| format!("zeile {index}\n")).collect();
        bus(
            &mut app,
            "run-1",
            TurnEvent::AssistantDelta {
                turn_id: TurnId::new(),
                text: long,
            },
        );
        let p = app.live_slot_projection(Instant::now(), 12);
        assert_eq!(p.live.len(), 1);
        assert_eq!(p.more, 1);
        let segments = app.history_segments(&p, 100, style::Theme::Dark, 12);
        for segment in &segments {
            if matches!(segment.id, SegmentId::Live(_)) {
                assert!(
                    segment.lines.len() <= 1 + LIVE_CELL_MAX_BODY,
                    "{:?}",
                    segment.lines
                );
            }
        }
        Ok(())
    }

    /// Ohne Live-Stream (`/agent stream none`) gibt es keine Live-Zellen.
    #[test]
    fn child_stream_off_shows_no_live_cells() -> TestResult {
        let mut app = test_chat_app()?;
        app.push_line(Role::User, "los");
        spawn(&mut app, "run-1", "eins")?;
        app.apply_child_stream_command("none");
        let p = projection(&app, Instant::now());
        assert!(p.live.is_empty());
        assert_eq!(p.more, 0);
        Ok(())
    }

    /// Einfügepunkt laut Slot-Tabelle, ohne App.
    #[test]
    fn insertion_point_follows_the_slot_table() {
        let mut marks = MessageMarks::default();
        assert_eq!(marks.insertion_point(0, false), 0);
        marks.note(Role::User, 0);
        assert_eq!(marks.insertion_point(1, true), 0, "erste Frage: dahinter");
        marks.note(Role::Assistant, 1);
        assert_eq!(marks.insertion_point(2, false), 1, "vor die Antwort");
        marks.note(Role::User, 2);
        assert_eq!(marks.insertion_point(3, true), 1, "#3 hinter dem Paar");
        assert_eq!(marks.insertion_point(3, false), 2, "abgebrochen: vor die Frage");
        marks.note(Role::Assistant, 3);
        assert_eq!(marks.insertion_point(4, false), 3, "wieder #2");
    }
}
