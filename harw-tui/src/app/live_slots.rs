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

use crate::i18n::{tr, tr_args};

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::live_order_glue::{self, ChildActivity};
use super::{ChatApp, Role, ToolCellHandle};
use crate::agent_monitor::{AgentLive, AgentPhase};
use crate::chat_scroll::{CellAnchor, CellId};
use crate::child_stream::{ChildStreamMode, SharedChildBlock};
use crate::history_cell::{HistoryCell, ToolVerbosity};
use crate::live_ranker::{ActivityTier, LiveActivityRanker, LiveCandidate, RankerConfig};
use crate::sanitize::sanitize_inline;
use crate::style;

/// Most virtual live cells shown inline at the same time (W01: one lead
/// background slot plus the next one; the rest collapse into `+N`).
pub(crate) const MAX_INLINE_LIVE: usize = 2;

/// Most tracked child blocks; the oldest entry is dropped beyond this bound.
const MAX_TRACKED_RUNS: usize = 64;

/// Most remembered expandable cells of child blocks (Ctrl+O detail check);
/// the oldest entry is dropped beyond this bound.
const MAX_DETAIL_HANDLES: usize = 4096;

/// Below this many history rows only one live cell is shown inline, so the
/// chat keeps dominating short terminals (W01 acceptance 10).
const TWO_SLOT_MIN_ROWS: u16 = 20;

/// Upper bound of body rows of one virtual live cell.
const LIVE_CELL_MAX_BODY: usize = 8;

/// Lower bound of body rows of one virtual live cell.
const LIVE_CELL_MIN_BODY: usize = 2;

/// Marker of a pinned live cell (pin of the Agents / LIVE window, `p`).
#[cfg(test)]
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
    /// A child approval question of this run is open or queued.
    pub(super) pending_question: bool,
}

impl RunSnapshot {
    /// Latest meaningful instant (monitor event or progress message).
    fn meaningful(&self) -> Option<Instant> {
        [self.last_event, self.info_at].into_iter().flatten().max()
    }
}

/// Ranker tier of a run: a pending approval question needs the operator
/// (W01 §2 tier 1); otherwise the shared status-line mapping
/// ([`live_order_glue::tier_of`]: unseen failure, terminal, fresh progress
/// message, phase), so status line, LIVE order and chat slots agree.
/// Reasoning or token deltas never raise the tier.
fn tier_of(run: &RunSnapshot, now: Instant) -> ActivityTier {
    if run.pending_question {
        return ActivityTier::NeedsOperator;
    }
    let activity = ChildActivity {
        id: run.id.clone(),
        phase: run.phase,
        unseen_failure: run.unseen_failure,
        last_event: run.last_event,
        finished_at: run.finished_at,
        info_at: run.info_at,
    };
    live_order_glue::tier_of(&activity, now)
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
    #[cfg(test)]
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
    /// First frame the background registry reported the run as finished
    /// (fallback end time when the monitor has none).
    finished_seen: Option<Instant>,
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
    /// Expandable cells inside child blocks (Ctrl+O / `/verbose`).
    detail: Vec<ToolCellHandle>,
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
            detail: Vec::new(),
            last: None,
            frozen: None,
        }
    }

    /// Remembers an expandable cell of a child block (Ctrl+O detail check).
    pub(super) fn note_detail_handle(&mut self, handle: ToolCellHandle) {
        self.detail.push(handle);
        if self.detail.len() > MAX_DETAIL_HANDLES {
            let excess = self.detail.len() - MAX_DETAIL_HANDLES;
            self.detail.drain(..excess);
        }
    }

    /// Whether any cell inside a child block is expanded (Ctrl+O): then the
    /// full blocks stay at their canonical home instead of a pointer.
    fn detail_expanded(&self) -> bool {
        self.detail.iter().any(ToolCellHandle::is_expanded)
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
            finished_seen: None,
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
            last_meaningful_ms: run.meaningful().map_or(0, |at| self.millis_since_epoch(at)),
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
                // Alle bekannten Läufe, auch die im `+N`: nur wirklich neue
                // zählen als neue Aktivität.
                let known = runs.iter().map(|run| run.id.clone()).collect();
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
        let is_terminal = |id: &str| tiers.get(id).copied() == Some(ActivityTier::Terminal);
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

    /// Row of the anchor, `None` once the cell is gone. The line is not
    /// clamped to the cell: a top row inside the virtual segments after the
    /// cell (live cells, `+N`) is anchored to the preceding canonical cell
    /// with a line past its end, and must resolve to exactly that row again.
    pub(crate) fn resolve(&self, anchor: CellAnchor) -> Option<usize> {
        self.rows
            .iter()
            .find(|(cell, _, _)| *cell == Some(anchor.cell))
            .map(|(_, start, _)| start.saturating_add(anchor.line))
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
        1 => Some(tr("chrome.new_activity", "↓ 1 new activity").to_owned()),
        n => Some(tr_args(
            "chrome.new_activities",
            "↓ {count} new activities",
            &[("count", &n.to_string())],
        )),
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
        Span::styled(
            tr_args(
                "chrome.live_role",
                "live · {role}",
                &[("role", &sanitize_inline(&slot.role))],
            ),
            accent,
        ),
    ];
    if let Some(live) = live {
        header.push(Span::styled(format!(" · {}", live.phase.label()), dim));
    }
    if slot.pinned {
        header.push(Span::styled(
            format!(" {}", tr("chrome.pinned", "▲ pinned")),
            style::warning_style(theme),
        ));
    }
    let mut lines = vec![Line::from(header)];
    let body = match block.map(|block| block.lock()) {
        Some(Ok(guard)) => guard.display_lines(width, theme),
        Some(Err(_)) => vec![Line::styled(
            tr(
                "chrome.live_unreadable",
                "  │ ⚠ Live block unreadable (poisoned lock)",
            )
            .to_owned(),
            style::warning_style(theme),
        )],
        None => Vec::new(),
    };
    if body.len() > max_body {
        let keep = max_body.saturating_sub(1).max(1);
        let hidden = body.len() - keep;
        lines.push(Line::styled(
            tr_args(
                "chrome.hidden_lines",
                "  │ … {count} lines above",
                &[("count", &hidden.to_string())],
            ),
            dim,
        ));
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
        self.live_slots.get_mut().track(child_id, role, home, block);
    }

    /// Remembers an expandable cell of a child block (hook of the child
    /// stream glue, Ctrl+O detail preservation).
    pub(super) fn note_live_detail_handle(&mut self, handle: ToolCellHandle) {
        self.live_slots.get_mut().note_detail_handle(handle);
    }

    /// Snapshots of the tracked **background** runs of this session (none
    /// while the child stream is off).
    ///
    /// # Description
    /// Only runs of the session's background registry
    /// ([`harw_core::BackgroundChildren::runs_for`]) are candidates: the
    /// block of a synchronous child (worker, `background:false` handoff)
    /// belongs to the foreground turn and stays at its canonical home. A run
    /// the registry reports as finished counts as terminal even when a stale
    /// or late bus event made the monitor look active again (W01 §3: never
    /// resurrect a terminal child); it then leaves the ranking after the
    /// ranker's terminal display time.
    fn live_run_snapshots(&self, slots: &mut LiveSlots, now: Instant) -> Vec<RunSnapshot> {
        if self.child_stream.mode() == ChildStreamMode::Off {
            return Vec::new();
        }
        let Some(spawner) = self.managed_spawner() else {
            return Vec::new();
        };
        let background = spawner.background_children().runs_for(self.session_id());
        slots
            .runs
            .iter_mut()
            .filter_map(|run| {
                let registered = background
                    .iter()
                    .find(|entry| entry.child.as_str() == run.id)?;
                let live = self.agent_monitor.agent(&run.id);
                let mut phase = live.map(|live| live.phase);
                let mut finished_at = live.and_then(|live| live.finished);
                if registered.is_running() {
                    run.finished_seen = None;
                } else {
                    let seen = *run.finished_seen.get_or_insert(now);
                    if phase.is_none_or(AgentPhase::is_active) {
                        phase = Some(match registered.status {
                            harw_core::BackgroundStatus::Failed => AgentPhase::Failed,
                            harw_core::BackgroundStatus::Cancelled => AgentPhase::Cancelled,
                            harw_core::BackgroundStatus::Running
                            | harw_core::BackgroundStatus::Completed => AgentPhase::Done,
                        });
                    }
                    finished_at = finished_at.or(Some(seen));
                }
                Some(RunSnapshot {
                    id: run.id.clone(),
                    role: live.map_or_else(|| run.role.clone(), |live| live.role.clone()),
                    phase,
                    unseen_failure: live.is_some_and(AgentLive::is_unseen_failure),
                    last_event: live.map(|live| live.last_meaningful),
                    finished_at,
                    info_at: self.child_infos.get(&run.id).map(|(_, at)| *at),
                    pending_question: self.child_approvals.is_pending_for(&run.id),
                })
            })
            .collect()
    }

    /// Live slot projection of this frame (history rendering hook).
    ///
    /// # Arguments
    /// - `now`: the frame's instant.
    /// - `history_rows`: visible rows of the chat history.
    pub(super) fn live_slot_projection(
        &self,
        now: Instant,
        history_rows: u16,
    ) -> LiveSlotProjection {
        let Ok(mut slots) = self.live_slots.try_borrow_mut() else {
            return LiveSlotProjection {
                insert_at: self.cells.len(),
                ..LiveSlotProjection::default()
            };
        };
        let runs = self.live_run_snapshots(&mut slots, now);
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
        // Ctrl+O / `/verbose`: aufgeklappte Details der Kind-Blöcke bleiben
        // an ihrer kanonischen Stelle vollständig sichtbar (W01 M2c), statt
        // hinter dem einzeiligen Verweis zu verschwinden.
        let detail = self.tool_verbosity == ToolVerbosity::Verbose
            || slots.as_ref().is_some_and(|slots| slots.detail_expanded());
        let homes: HashSet<usize> = if detail {
            HashSet::new()
        } else {
            projection
                .live
                .iter()
                .filter_map(|slot| block_of(&slot.run_id).and_then(|run| run.home))
                .collect()
        };
        let dim = style::dim_style(theme);
        let insert_at = projection.insert_at.min(self.cells.len());
        let mut virtual_cells: Vec<HistorySegment> = Vec::new();
        if projection.more > 0 {
            virtual_cells.push(HistorySegment {
                id: SegmentId::More,
                lines: vec![Line::styled(
                    tr_args(
                        "chrome.more_active",
                        "◇ +{count} more active",
                        &[("count", &projection.more.to_string())],
                    ),
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
                    tr("chrome.live_below", "↓ live further below")
                } else {
                    tr("chrome.live_above", "↑ live further above")
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
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use harw_core::child_approval::{
        ChildApprovalAnswer, ChildApprovalBroker, ChildApprovalRequest,
    };
    use harw_core::{
        AgentEvent, AgentEventKind, BackgroundStatus, ChildLimits, ManagedAgentSpawner,
        SessionManager,
    };
    use harw_protocol::events::TurnEvent;
    use harw_protocol::items::{AssistantMessageItem, ContentPart, TurnItem};
    use harw_registry_defaults::profile::role_names;
    use harw_tools::{ToolCall, ToolName};
    use harw_types::{ItemId, SessionId, ToolCallId, TurnId};
    use tokio::sync::oneshot;

    use super::super::child_approvals::{self, ChildApprovalUi};
    use super::super::{
        Spinner, TurnEventState, handle_turn_event, render_viewport, tests::test_chat_app,
    };
    use super::*;
    use crate::chat_scroll::ChatScroll;
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
            pending_question: false,
        }
    }

    /// Test-App mit Spawner, damit Läufe im Hintergrund-Register der Sitzung
    /// stehen (nur diese bekommen Live-Zellen).
    fn bg_app() -> TestResult<ChatApp> {
        let (events, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let spawner = Arc::new(ManagedAgentSpawner::new(
            manager,
            ChildLimits::conservative(),
        ));
        Ok(test_chat_app()?.with_managed_spawner(Some(spawner)))
    }

    fn spawner(app: &ChatApp) -> TestResult<Arc<ManagedAgentSpawner>> {
        app.managed_spawner()
            .cloned()
            .ok_or(TestError::Missing("Spawner der Test-App"))
    }

    /// Beendet einen Hintergrund-Lauf wie der echte Pfad: Abschluss im Bus
    /// (Monitor) und im Hintergrund-Register.
    fn finish(app: &mut ChatApp, id: &str) -> TestResult {
        bus(
            app,
            id,
            TurnEvent::TurnCompleted {
                turn_id: TurnId::new(),
                usage: None,
            },
        );
        spawner(app)?
            .background_children()
            .finish(
                &SessionId::from_str(id),
                BackgroundStatus::Completed,
                "fertig".to_owned(),
            )
            .ok_or(TestError::Missing("laufender Hintergrund-Lauf"))?;
        Ok(())
    }

    /// Stellt eine Freigabe-Frage des Kindes `child` und holt sie ab; der
    /// Empfänger muss leben, solange die Frage wartet.
    fn ask_approval(
        app: &mut ChatApp,
        child: &str,
    ) -> TestResult<oneshot::Receiver<ChildApprovalAnswer>> {
        let (ui, broker) = ChildApprovalUi::channel();
        app.child_approvals = ui;
        let answer = broker
            .submit(ChildApprovalRequest {
                child: SessionId::from_str(child),
                role: role_names::ROOT_ORCHESTRATOR.to_owned(),
                tree_path: vec![role_names::ROOT_ORCHESTRATOR.to_owned()],
                call: ToolCall {
                    id: ToolCallId::new(),
                    name: ToolName::new("fs.write"),
                    arguments: serde_json::json!({ "path": "src/lib.rs", "content": "x" }),
                },
                timeout: Duration::from_secs(600),
            })
            .ok_or(TestError::Missing("Frage zugestellt"))?;
        child_approvals::poll(app);
        Ok(answer)
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
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
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
            app.child_stream
                .apply(&event.agent, event.parent.as_ref(), &event.role, turn);
        }
    }

    /// Startet ein Hintergrund-Orchestrator-Kind wie der echte Pfad:
    /// Eintrag im Hintergrund-Register, `ChildSpawned` der Wurzel
    /// (Agent-Zeile + Block), dann ein erstes Bus-Ereignis.
    fn spawn(app: &mut ChatApp, id: &str, text: &str) -> TestResult {
        let registered = spawner(app)?.background_children().register(
            &SessionId::from_str(id),
            app.session_id(),
            role_names::ROOT_ORCHESTRATOR,
            Some("Baue X"),
        );
        if !registered {
            return Err(TestError::Missing("neuer Hintergrund-Lauf"));
        }
        spawn_foreground(app, id, text)
    }

    /// Startet ein synchrones Kind (Worker, `background:false`-Übergabe):
    /// Block im Verlauf, aber kein Eintrag im Hintergrund-Register.
    fn spawn_foreground(app: &mut ChatApp, id: &str, text: &str) -> TestResult {
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
        let mut app = bg_app()?;
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

        // Antwort im Flug: die vorläufige Antwort steht dem Composer am
        // nächsten, davor die neue Frage, dahinter (weiter oben) der Root.
        app.live_stream = "vorlaeufige-antwort".to_owned();
        let rows = render_rows(&app, 100, 40)?;
        let row_of = |needle: &str| {
            rows.iter()
                .position(|row| row.contains(needle))
                .ok_or(TestError::Missing("Zeile im Bild"))
        };
        let live_row = row_of("◆ live")?;
        let question_row = row_of("Nächste Frage")?;
        let provisional_row = row_of("vorlaeufige-antwort")?;
        assert!(
            live_row < question_row && question_row < provisional_row,
            "{rows:#?}"
        );
        app.live_stream.clear();

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
        let mut app = bg_app()?;
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

    /// W01 Abnahme 3 (zweite Hälfte) + §2 Stufe 1: eine wartende
    /// Freigabe-Frage hebt einen ruhigen Lauf vor einen aktiven, der 500
    /// Reasoning-Deltas sendet.
    #[test]
    fn pending_question_outranks_a_chatty_active_run() -> TestResult {
        let mut app = bg_app()?;
        app.push_line(Role::User, "los");
        spawn(&mut app, "asking", "brauche Freigabe")?;
        spawn(&mut app, "chatty", "rede")?;
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
        let _answer = ask_approval(&mut app, "asking")?;
        assert!(app.child_approvals.is_pending_for("asking"));
        // Nach dem Coalescing-Fenster des Rankers (keine Haltezeit für
        // `NeedsOperator`).
        let lead = projection(&app, at(Instant::now(), 2000)?);
        assert_eq!(
            lead.live.first().map(|slot| slot.run_id.as_str()),
            Some("asking"),
            "{lead:?}"
        );
        Ok(())
    }

    /// Unit-Ebene: ohne Frage führt der jüngere aktive Lauf, mit Frage der
    /// wartende (Frage schlägt Phase und Aktualität).
    #[test]
    fn pending_question_maps_to_needs_operator() -> TestResult {
        let base = Instant::now();
        let mut asking = snapshot("asking", AgentPhase::Waiting, base);
        let chatty = snapshot("chatty", AgentPhase::Thinking, at(base, 5000)?);
        assert_eq!(tier_of(&chatty, base), ActivityTier::Active);
        let lead = |runs: &[RunSnapshot]| -> TestResult<Option<String>> {
            let mut slots = LiveSlots::new(base);
            let p = slots.project(at(base, 6000)?, fg(4), runs, None);
            Ok(p.live.first().map(|slot| slot.run_id.clone()))
        };
        assert_eq!(
            lead(&[asking.clone(), chatty.clone()])?.as_deref(),
            Some("chatty")
        );
        asking.pending_question = true;
        assert_eq!(tier_of(&asking, base), ActivityTier::NeedsOperator);
        assert_eq!(lead(&[asking, chatty])?.as_deref(), Some("asking"));
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
    /// oberste Zelle und zeigt `N new activitäten`; zurück am Ende wird
    /// wieder gerankt.
    #[test]
    fn scroll_up_freezes_order_keeps_top_cell_and_counts_new_activity() -> TestResult {
        let mut app = bg_app()?;
        for index in 0..80 {
            app.push_line(Role::System, format!("verlauf-zeile-{index:02}"));
        }
        app.push_line(Role::User, "Frage");
        spawn(&mut app, "run-a", "A arbeitet")?;
        spawn(&mut app, "run-b", "B arbeitet")?;
        spawn(&mut app, "run-c", "C arbeitet")?;
        app.push_line(Role::Assistant, "Antwort");
        let now = Instant::now();
        let tail = projection(&app, now);
        let order: Vec<String> = tail.live.iter().map(|slot| slot.run_id.clone()).collect();
        assert_eq!(order.len(), 2);
        assert_eq!(tail.more, 1);

        render_rows(&app, 120, 60)?;
        app.scroll.scroll_up(10, 500, 20);
        let rows = render_rows(&app, 120, 60)?;
        let top = app.scroll.anchor().ok_or(TestError::Missing("Anker"))?;
        // Nur Hochscrollen ist keine Aktivität, auch nicht für den Lauf im `+N`.
        let quiet = projection(&app, Instant::now());
        assert!(quiet.frozen);
        assert_eq!(quiet.new_activity, 0, "{quiet:?}");
        assert!(!rows.iter().any(|row| row.contains("new activit")));
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
        let rows = render_rows(&app, 120, 60)?;
        assert_eq!(app.scroll.anchor(), Some(top), "gleiche oberste Zelle");
        assert!(rows.contains(&first_visible), "{rows:#?}");
        let frozen = projection(&app, at(now, 10_000)?);
        assert!(frozen.frozen);
        let frozen_order: Vec<String> =
            frozen.live.iter().map(|slot| slot.run_id.clone()).collect();
        assert_eq!(frozen_order, order, "eingefroren");
        assert_eq!(frozen.new_activity, 1, "{frozen:?}");
        assert!(
            rows.iter().any(|row| row.contains("new activit")),
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
        let mut app = bg_app()?;
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
        // Die Freigabe-Frage eines anderen Agenten erscheint trotz Pin als
        // Dialog; der Pin bleibt sichtbar.
        let _answer = ask_approval(&mut app, "busy-orch")?;
        assert!(app.pending_approval_dialog.is_some(), "Dialog offen");
        let asked = projection(&app, at(now, 500)?);
        assert!(
            asked
                .live
                .iter()
                .any(|slot| slot.run_id == "pinned-root" && slot.pinned),
            "{asked:?}"
        );
        assert!(asked.live.iter().any(|slot| slot.run_id == "busy-orch"));
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
        let mut app = bg_app()?;
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
                let other = if id == "run-x" {
                    "nur-y-text"
                } else {
                    "nur-x-text"
                };
                assert!(
                    segment
                        .lines
                        .iter()
                        .all(|line| !line_text(line).contains(other)),
                    "keine Vermischung in {id}"
                );
            }
        }
        assert!(current.is_some());
        assert!(text.iter().any(|line| line.contains("live further below")));
        Ok(())
    }

    /// W01 §3 / PL-97 C5: Projektion und Zeichnen ändern weder die
    /// kanonischen Zellen noch den Export, auch nicht über einen Turn mit
    /// Schlussantwort und Hochscrollen hinweg.
    #[test]
    fn canonical_cells_and_export_are_unchanged_by_the_projection() -> TestResult {
        let mut app = bg_app()?;
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
        let mut app = bg_app()?;
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
        let mut app = bg_app()?;
        app.push_line(Role::User, "los");
        spawn(&mut app, "run-1", "eins")?;
        app.apply_child_stream_command("none");
        let p = projection(&app, Instant::now());
        assert!(p.live.is_empty());
        assert_eq!(p.more, 0);
        Ok(())
    }

    /// W01 Abnahme 7 + 9 am echten Pfad: nach `terminal_display_ms` verlässt
    /// der beendete Lauf die Live-Zellen, sein Block steht wieder vollständig
    /// an der kanonischen Stelle (Ergebnis bleibt sichtbar), und ein
    /// verspätetes Ereignis holt ihn nicht zurück.
    #[test]
    fn finished_run_returns_to_its_home_and_stale_events_do_not_revive_it() -> TestResult {
        let mut app = bg_app()?;
        app.push_line(Role::User, "los");
        spawn(&mut app, "done-run", "arbeite")?;
        // Bleibende Schlussnachricht des Kindes im Block.
        bus(
            &mut app,
            "done-run",
            TurnEvent::ItemAdded {
                turn_id: TurnId::new(),
                item: TurnItem::AssistantMessage(AssistantMessageItem {
                    id: ItemId::new(),
                    content: vec![ContentPart::Text {
                        text: "ergebnis-text".to_owned(),
                    }],
                    phase: None,
                }),
            },
        );
        app.push_line(Role::Assistant, "läuft");
        let now = Instant::now();
        let running = projection(&app, now);
        assert_eq!(running.live.len(), 1);
        let text = segment_text(&app.history_segments(&running, 100, style::Theme::Dark, 40));
        assert!(
            text.iter().any(|line| line.contains("live further")),
            "{text:?}"
        );

        finish(&mut app, "done-run")?;
        let display = RankerConfig::default().terminal_display_ms;
        let later = projection(&app, at(now, display + 2000)?);
        assert!(later.live.is_empty(), "{later:?}");
        assert_eq!(later.more, 0);
        let text = segment_text(&app.history_segments(&later, 100, style::Theme::Dark, 40));
        assert!(
            !text.iter().any(|line| line.contains("live further")),
            "{text:?}"
        );
        assert_eq!(
            text.iter()
                .filter(|line| line.contains("ergebnis-text"))
                .count(),
            1,
            "{text:?}"
        );

        // Verspätetes Bus-Ereignis nach dem Ende (Lag, Wiederholung).
        bus(
            &mut app,
            "done-run",
            TurnEvent::AssistantDelta {
                turn_id: TurnId::new(),
                text: " spät".to_owned(),
            },
        );
        let stale = projection(&app, at(now, display + 4000)?);
        assert!(stale.live.is_empty(), "{stale:?}");
        assert_eq!(stale.more, 0);
        Ok(())
    }

    /// W01 §1/§3.3.4: nur Hintergrund-Läufe bekommen Live-Zellen. Der Block
    /// eines synchronen Kindes (Worker, `background:false`) bleibt während
    /// des Turns an seiner Stelle, ohne Verweis.
    #[test]
    fn synchronous_child_block_stays_in_place_without_a_live_cell() -> TestResult {
        let mut app = bg_app()?;
        app.push_line(Role::Assistant, "vorher");
        app.push_line(Role::User, "Frage");
        app.active_cancel = Some(harw_core::cancel::CancelToken::new());
        spawn_foreground(&mut app, "worker-1", "sync-text")?;
        let p = projection(&app, Instant::now());
        assert!(p.live.is_empty(), "{p:?}");
        assert_eq!(p.more, 0);
        let text = segment_text(&app.history_segments(&p, 100, style::Theme::Dark, 40));
        assert!(
            text.iter().any(|line| line.contains("sync-text")),
            "{text:?}"
        );
        assert!(
            !text.iter().any(|line| line.contains("live further")),
            "{text:?}"
        );
        Ok(())
    }

    /// W01 M2c „Ctrl+O/detail preservation“: ist eine Zelle im Kind-Block
    /// aufgeklappt, bleibt der ganze Block an seiner kanonischen Stelle
    /// sichtbar statt hinter dem Verweis.
    #[test]
    fn expanded_child_detail_stays_reachable_at_its_home() -> TestResult {
        let mut app = bg_app()?;
        app.push_line(Role::User, "los");
        spawn(&mut app, "run-d", "detail-text")?;
        bus(
            &mut app,
            "run-d",
            TurnEvent::ToolCallRequested {
                turn_id: TurnId::new(),
                call_id: ToolCallId::new(),
                tool_name: "fs.write".to_owned(),
                arguments: serde_json::json!({ "path": "a.rs", "content": "x" }),
            },
        );
        app.absorb_child_stream_handles();
        app.push_line(Role::Assistant, "läuft");
        let home = app
            .live_slots
            .borrow()
            .runs
            .iter()
            .find(|run| run.id == "run-d")
            .and_then(|run| run.home)
            .ok_or(TestError::Missing("Heimat des Blocks"))?;
        let home_lines = |app: &ChatApp| -> TestResult<Vec<String>> {
            let p = projection(app, Instant::now());
            let segments = app.history_segments(&p, 100, style::Theme::Dark, 40);
            let segment = segments
                .iter()
                .find(|segment| segment.id == SegmentId::Cell(CellId(home)))
                .ok_or(TestError::Missing("Heimat-Segment"))?;
            Ok(segment.lines.iter().map(line_text).collect())
        };
        let collapsed = home_lines(&app)?;
        assert_eq!(collapsed.len(), 1, "Verweis: {collapsed:?}");
        assert!(collapsed.iter().any(|line| line.contains("live further")));

        if app.tool_cells.is_empty() {
            return Err(TestError::Missing("aufklappbare Kind-Zelle"));
        }
        for handle in &app.tool_cells {
            handle.set_expanded(true);
        }
        let expanded = home_lines(&app)?;
        let canonical_home: Vec<String> = app
            .cells
            .get(home)
            .ok_or(TestError::Missing("Heimat-Zelle"))?
            .display_lines(100, style::Theme::Dark)
            .iter()
            .map(line_text)
            .collect();
        assert_eq!(expanded, canonical_home);
        assert!(
            expanded.iter().any(|line| line.contains("a.rs"))
                && !expanded.iter().any(|line| line.contains("live further")),
            "voller Block: {expanded:?}"
        );
        Ok(())
    }

    /// W01 M2c: steht die oberste Zeile in einem virtuellen Segment (Live-
    /// Zelle), schnappt der Anker nicht zurück, und ein Schritt nach unten
    /// bewegt die Ansicht um genau eine Zeile.
    #[test]
    fn anchor_inside_a_virtual_segment_scrolls_line_by_line() {
        let rows = |prefix: &str, count: usize| -> Vec<Line<'static>> {
            (0..count)
                .map(|index| Line::from(format!("{prefix}-{index}")))
                .collect()
        };
        let segments = vec![
            HistorySegment {
                id: SegmentId::Cell(CellId(0)),
                lines: rows("a", 10),
            },
            HistorySegment {
                id: SegmentId::Live("run".to_owned()),
                lines: rows("live", 9),
            },
            HistorySegment {
                id: SegmentId::Cell(CellId(1)),
                lines: rows("b", 10),
            },
        ];
        let geometry = SegmentGeometry::measure(&segments, 80);
        let (total, viewport) = (29usize, 5usize);
        let mut scroll = ChatScroll::new();
        let sync = |scroll: &ChatScroll| {
            scroll.sync_layout(total, viewport, 80);
            scroll.sync_anchor(
                total,
                viewport,
                |anchor| geometry.resolve(anchor),
                |top| geometry.locate(top),
            )
        };
        // oberste Zeile 12 = dritte Zeile der Live-Zelle (Start 10).
        scroll.scroll_up(12, total, viewport);
        let first = sync(&scroll);
        assert_eq!(first, 12);
        assert_eq!(sync(&scroll), 12, "kein Zurückschnappen");
        scroll.scroll_down(1);
        assert_eq!(sync(&scroll), 11);
        assert_eq!(sync(&scroll), 11, "stabil nach dem Schritt");
        scroll.scroll_down(1);
        assert_eq!(sync(&scroll), 10);
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
        assert_eq!(
            marks.insertion_point(3, false),
            2,
            "abgebrochen: vor die Frage"
        );
        marks.note(Role::Assistant, 3);
        assert_eq!(marks.insertion_point(4, false), 3, "wieder #2");
    }
}
