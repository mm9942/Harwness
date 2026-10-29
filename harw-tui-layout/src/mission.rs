//! Mission Control geometry (PL-97 C1; PL-94 W02 plus the PL-94 landscape
//! anchors).
//!
//! [`classify_mission`] turns a terminal size plus a few visibility flags into
//! one [`MissionLayout`]: where the chat, the three status windows (active
//! agents, active jobs, finished/failed), the LIVE projection, the optional
//! explorer and workbench, the status rows and the composer go.
//!
//! Decision order (deterministic, first match wins):
//!
//! 1. `bottom = status_rows + composer_rows`, `body = rows - bottom`
//!    (saturating; the composer keeps its rows first when the terminal is
//!    lower than `bottom`).
//! 2. Not [`Orientation::Portrait`], `body >= LANDSCAPE_MIN_ROWS_BODY` and
//!    enough columns for the full (`64 + 38 + 30`) or the dense
//!    (`48 + 34 + 26`) profile: [`MissionPlacement::LandscapeThreeColumns`].
//!    Both profiles size the columns alike, `center = clamp(cols / 4, 34..=60)`
//!    and `right = clamp(cols / 5, 26..=44)`, and the chat takes the rest; the
//!    profile only sets the chat minimum (64 or 48 columns) the explorer has
//!    to leave.
//!
//!    ```text
//!    ┌ CHAT ──────────────┬ FINISHED / FAILED ┬ ACTIVE AGENTS ┐
//!    │                    ├ LIVE ─────────────┼ ACTIVE JOBS ──┤
//!    │                    │                   ├ WORKBENCH ────┤ (optional)
//!    └────────────────────┴───────────────────┴───────────────┘
//!    ```
//! 3. [`crate::classify`] says [`crate::Placement::WideSide`]:
//!    [`MissionPlacement::ShallowLandscape`] -- today's side-column geometry,
//!    agents stacked above jobs in the right column, and a one-row finished
//!    strip plus a header-only LIVE strip on top of the chat column (via
//!    [`crate::chat_first`] without its cards row).
//! 4. [`crate::classify`] says [`crate::Placement::PortraitDock`]:
//!    [`MissionPlacement::PortraitStack`] -- [`crate::chat_first`] over the
//!    whole body (cards row split by [`crate::split_dock`], finished strip,
//!    LIVE strip, chat).
//! 5. Otherwise [`MissionPlacement::CompactSummary`]: no status windows at all.
//!
//! Invariants (for every input, checked exhaustively in the tests): all rects
//! lie inside the terminal, are pairwise disjoint and together cover it
//! exactly; every `Some` rect is at least three columns wide and, except for
//! the header-led strips of the strip placements, at least three rows high.
//! `jobs` is `None` while `agents` is `Some` exactly when the agents rect holds
//! the combined agents-and-jobs list (see [`MissionLayout::agents`]).
//! The result depends on the input only, so a resize that flips the
//! orientation simply re-classifies.
//!
//! No I/O, no panics, saturating arithmetic only, `std` only.

use crate::Rect;
use crate::dock::DockSplit;
use crate::mini_live::{
    CARDS_ROWS, ChatFirstInput, LiveMode, MINI_LIVE_MAX_BODY_LINES, MINI_LIVE_MIN_CHAT_ROWS,
    chat_first,
};
use crate::placement::{LayoutInput, Placement, classify};

/// Chat minimum of the full three-column landscape profile, and its summand
/// in the full-profile width threshold (`64 + 38 + 30 = 132` columns).
pub const LANDSCAPE_CHAT_MIN_COLS: u16 = 64;
/// Centre-column summand of the full-profile width threshold
/// (`64 + 38 + 30`). The centre column itself is `clamp(cols / 4, 34..=60)`
/// in both profiles, so from 132 to 151 columns it is narrower than this
/// value and the chat keeps the difference.
pub const LANDSCAPE_CENTER_MIN_COLS: u16 = 38;
/// Right-column summand of the full-profile width threshold (`64 + 38 + 30`).
/// The right column itself is `clamp(cols / 5, 26..=44)` in both profiles, so
/// from 132 to 149 columns it is narrower than this value.
pub const LANDSCAPE_RIGHT_MIN_COLS: u16 = 30;
/// Chat minimum of the dense three-column landscape profile, and its summand
/// in the dense-profile width threshold (`48 + 34 + 26 = 108` columns).
pub const LANDSCAPE_DENSE_CHAT_MIN_COLS: u16 = 48;
/// Narrowest centre column in both profiles (lower bound of
/// `clamp(cols / 4, 34..=60)`), and the centre summand of the dense threshold.
pub const LANDSCAPE_DENSE_CENTER_MIN_COLS: u16 = 34;
/// Narrowest right column in both profiles (lower bound of
/// `clamp(cols / 5, 26..=44)`), and the right summand of the dense threshold.
pub const LANDSCAPE_DENSE_RIGHT_MIN_COLS: u16 = 26;
/// Minimum body height (rows above status and composer) for the
/// three-column landscape: agents 5, jobs 5, finished 3, LIVE 7.
pub const LANDSCAPE_MIN_ROWS_BODY: u16 = 5 + 5 + 3 + 7;
/// Rows of the collapsed finished/failed strip.
pub const FINISHED_COLLAPSED_ROWS: u16 = 1;
/// Fewest content rows of the expanded finished/failed window.
pub const FINISHED_EXPANDED_MIN_ROWS: u16 = 3;
/// Most content rows of the expanded finished/failed window.
pub const FINISHED_EXPANDED_MAX_ROWS: u16 = 8;

// `u16::clamp` panics for min > max; the bounds are constants, so prove the
// order at compile time instead of at run time.
const _: () = assert!(FINISHED_EXPANDED_MIN_ROWS <= FINISHED_EXPANDED_MAX_ROWS);
const _: () = assert!(LANDSCAPE_DENSE_CENTER_MIN_COLS <= LANDSCAPE_CENTER_MAX_COLS);
const _: () = assert!(LANDSCAPE_DENSE_RIGHT_MIN_COLS <= LANDSCAPE_RIGHT_MAX_COLS);

/// Terminal width from which the full landscape profile applies.
const LANDSCAPE_FULL_MIN_COLS: u16 =
    LANDSCAPE_CHAT_MIN_COLS + LANDSCAPE_CENTER_MIN_COLS + LANDSCAPE_RIGHT_MIN_COLS;
/// Terminal width from which the dense landscape profile applies.
const LANDSCAPE_DENSE_MIN_COLS: u16 = LANDSCAPE_DENSE_CHAT_MIN_COLS
    + LANDSCAPE_DENSE_CENTER_MIN_COLS
    + LANDSCAPE_DENSE_RIGHT_MIN_COLS;
/// Widest centre column (upper bound of `clamp(cols / 4, 34..=60)`).
const LANDSCAPE_CENTER_MAX_COLS: u16 = 60;
/// Widest right column (upper bound of `clamp(cols / 5, 26..=44)`).
const LANDSCAPE_RIGHT_MAX_COLS: u16 = 44;
/// Rows the agents and the jobs window each keep in the three-column landscape,
/// also when the workbench shares the right column.
const LANDSCAPE_STACK_MIN_ROWS: u16 = 5;
/// Top plus bottom border of a bordered window.
const BORDER_ROWS: u16 = 2;
/// Header row of a LIVE strip (the rows below it are the body lines).
const STRIP_HEADER_ROWS: u16 = 1;
/// Smallest bordered window: two border rows plus one content row.
const WINDOW_MIN_ROWS: u16 = 3;
/// Smallest width of any window or strip.
const WINDOW_MIN_COLS: u16 = 3;
/// Explorer width; deliberate mirror of `EXPLORER_WIDTH` in
/// `harw-tui/src/panes.rs` (this crate cannot depend on `harw-tui`).
const EXPLORER_COLS: u16 = 36;
/// Chat width kept next to the explorer outside the three-column landscape;
/// deliberate mirror of `MIN_CHAT_WIDTH` in `harw-tui/src/panes.rs`.
const SIDE_CHAT_MIN_COLS: u16 = 48;
/// Fewest workbench rows when it shares the side column (shallow landscape);
/// deliberate mirror of `WORKBENCH_MIN_ROWS` in `harw-tui/src/panes.rs`.
const SIDE_WORKBENCH_MIN_ROWS: u16 = 8;

/// Aspect of the terminal in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    /// More rows than columns.
    Portrait,
    /// More columns than rows.
    Landscape,
    /// As many rows as columns.
    Balanced,
}

/// Classifies `cols` x `rows` by aspect: rows > cols is portrait, cols > rows
/// is landscape, equal is balanced.
pub fn orientation(cols: u16, rows: u16) -> Orientation {
    match rows.cmp(&cols) {
        std::cmp::Ordering::Greater => Orientation::Portrait,
        std::cmp::Ordering::Less => Orientation::Landscape,
        std::cmp::Ordering::Equal => Orientation::Balanced,
    }
}

/// Which Mission Control arrangement a terminal gets (see the module docs for
/// the decision order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissionPlacement {
    /// Cards row (agents | jobs, combined below
    /// [`crate::PHONE_DOCK_SPLIT_MIN_COLS`]), finished strip, LIVE strip and
    /// chat stacked over the full width.
    PortraitStack,
    /// Chat left and widest, finished/failed top-centre with LIVE below it,
    /// agents above jobs far right.
    LandscapeThreeColumns,
    /// Today's wide side column (agents above jobs, or one combined list when
    /// the column is too low for two windows) next to the chat, with a
    /// one-row finished strip and a header-only LIVE strip above the chat.
    /// Also chosen for portrait terminals that are at least
    /// [`crate::WIDE_AGENT_PANEL_MIN_COLS`] wide.
    ShallowLandscape,
    /// Today's one-line summary or compact fallback: no status windows.
    CompactSummary,
}

/// Input of [`classify_mission`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissionInput {
    /// Terminal width in cells.
    pub cols: u16,
    /// Terminal height in cells.
    pub rows: u16,
    /// Rows reserved for the status line(s) above the composer.
    pub status_rows: u16,
    /// Rows reserved for the composer at the bottom.
    pub composer_rows: u16,
    /// Wanted LIVE body lines below its header in the portrait stack
    /// (clamped to [`crate::MINI_LIVE_MAX_BODY_LINES`]; 0 means header only).
    pub live_body_lines_wanted: u16,
    /// Wanted finished/failed rows: [`FINISHED_COLLAPSED_ROWS`] (or 0) is
    /// collapsed, larger values are expanded and clamped to
    /// [`FINISHED_EXPANDED_MIN_ROWS`]..=[`FINISHED_EXPANDED_MAX_ROWS`].
    pub finished_rows_wanted: u16,
    /// Whether the Mission Control windows (agents, jobs, finished, LIVE) are
    /// shown; hidden windows leave their space to the chat.
    pub agents_visible: bool,
    /// Whether the workbench is shown (side column only; hidden elsewhere).
    pub workbench_visible: bool,
    /// Whether the explorer is shown left of the chat (only when the chat
    /// keeps its minimum width next to it).
    pub explorer_visible: bool,
}

/// Result of [`classify_mission`]; all rects in terminal cells with the
/// origin at the top left of the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissionLayout {
    /// The chosen arrangement.
    pub placement: MissionPlacement,
    /// Aspect of the terminal (see [`orientation`]).
    pub orientation: Orientation,
    /// Chat area: always present, possibly empty on degenerate terminals.
    pub chat: Rect,
    /// Active-agents window.
    ///
    /// One rule holds in every placement: `jobs == None` while
    /// `agents == Some` means this rect holds the combined agents-and-jobs
    /// list (one window, like today's wide side panel and the legacy combined
    /// dock), so a renderer must draw both lists into it. That happens in the
    /// portrait stack below [`crate::PHONE_DOCK_SPLIT_MIN_COLS`] (combined
    /// cards row) and in the shallow landscape when the side stack is lower
    /// than two bordered windows (six rows), e.g. on a low terminal or when a
    /// visible workbench takes most of the side column. The three-column
    /// landscape always places both windows.
    pub agents: Option<Rect>,
    /// Active-jobs window; `None` next to `Some` agents means the jobs are
    /// listed in the combined agents rect (see [`MissionLayout::agents`]).
    pub jobs: Option<Rect>,
    /// Finished/failed window (three-column landscape, bordered) or strip
    /// (portrait stack and shallow landscape, header-led).
    pub finished: Option<Rect>,
    /// LIVE window (three-column landscape, bordered and scrollable) or strip
    /// (portrait stack and shallow landscape, header plus body lines).
    pub live: Option<Rect>,
    /// Stage of the LIVE area in [`LiveMode`]'s own terms, in every
    /// placement: `Hidden` exactly when `live` is `None`, otherwise `Header`
    /// or `Body { lines }` with `lines` in
    /// `1..=`[`crate::MINI_LIVE_MAX_BODY_LINES`].
    ///
    /// In the strip placements this is the [`crate::chat_first`] stage and
    /// the strip is exactly `lines + 1` rows (header plus body). In the
    /// three-column landscape the LIVE area is a bordered window instead: its
    /// unscrolled view shows the full body (`lines` is
    /// `min(content rows, MINI_LIVE_MAX_BODY_LINES)`), and its content rows
    /// are [`MissionLayout::live_content_rows`] (`live.height - 2`), which
    /// can be more than `lines` because that window scrolls.
    pub live_mode: LiveMode,
    /// Explorer column left of the chat.
    pub explorer: Option<Rect>,
    /// Workbench below the jobs window in the right column.
    pub workbench: Option<Rect>,
    /// Status row(s) below the body.
    pub status: Rect,
    /// Composer at the very bottom.
    pub composer: Rect,
}

impl MissionLayout {
    /// Rows the LIVE content gets, in every placement: below the header in
    /// the strip placements (`live.height - 1`, so the `lines` of
    /// [`MissionLayout::live_mode`] and 0 for a header-only strip), inside
    /// both borders in the three-column landscape (`live.height - 2`, which
    /// can exceed [`crate::MINI_LIVE_MAX_BODY_LINES`] because that window
    /// scrolls), and 0 without a LIVE area.
    pub fn live_content_rows(&self) -> u16 {
        match (self.placement, self.live) {
            (_, None) => 0,
            (MissionPlacement::LandscapeThreeColumns, Some(live)) => {
                live.height.saturating_sub(BORDER_ROWS)
            }
            (_, Some(live)) => live.height.saturating_sub(STRIP_HEADER_ROWS),
        }
    }
}

/// Classifies a terminal into a [`MissionLayout`] (see the module docs for
/// the decision order and the invariants).
///
/// Pure function: no I/O, no panics, saturating arithmetic only; it reuses
/// [`crate::classify`], [`crate::split_dock`] (through the cards row) and
/// [`crate::chat_first`].
pub fn classify_mission(input: MissionInput) -> MissionLayout {
    let cols = input.cols;
    let rows = input.rows;

    // 1. Bottom rows: the composer first, then the status rows, the rest is
    //    the body. Equal to `rows - (status_rows + composer_rows)` whenever the
    //    terminal is high enough, and never overlapping when it is not.
    let composer_rows = input.composer_rows.min(rows);
    let status_rows = input.status_rows.min(rows.saturating_sub(composer_rows));
    let body = rows
        .saturating_sub(composer_rows)
        .saturating_sub(status_rows);
    let base = MissionLayout {
        placement: MissionPlacement::CompactSummary,
        orientation: orientation(cols, rows),
        chat: Rect {
            x: 0,
            y: 0,
            width: cols,
            height: body,
        },
        agents: None,
        jobs: None,
        finished: None,
        live: None,
        live_mode: LiveMode::Hidden,
        explorer: None,
        workbench: None,
        status: Rect {
            x: 0,
            y: body,
            width: cols,
            height: status_rows,
        },
        composer: Rect {
            x: 0,
            y: body.saturating_add(status_rows),
            width: cols,
            height: composer_rows,
        },
    };

    // 2. Three columns.
    if base.orientation != Orientation::Portrait
        && body >= LANDSCAPE_MIN_ROWS_BODY
        && let Some(chat_min) = landscape_chat_min(cols)
    {
        return landscape_three_columns(base, input, chat_min);
    }

    // 3.-5. Today's classification decides the rest.
    let screen = classify(LayoutInput {
        cols,
        rows,
        status_rows: input.status_rows,
        composer_rows: input.composer_rows,
    });
    match screen.placement {
        Placement::WideSide => shallow_landscape(base, input, screen.chat, screen.agents),
        Placement::PortraitDock { .. } => portrait_stack(base, input),
        Placement::Summary | Placement::Compact => compact_summary(base, input),
    }
}

/// Chat minimum of the three-column profile that `cols` reaches: the full
/// profile from `64 + 38 + 30` columns, the dense one from `48 + 34 + 26`,
/// none below. The profile changes nothing else: both size the centre and
/// the right column with the same clamp, so the columns grow smoothly across
/// the switch from dense to full.
fn landscape_chat_min(cols: u16) -> Option<u16> {
    if cols >= LANDSCAPE_FULL_MIN_COLS {
        Some(LANDSCAPE_CHAT_MIN_COLS)
    } else if cols >= LANDSCAPE_DENSE_MIN_COLS {
        Some(LANDSCAPE_DENSE_CHAT_MIN_COLS)
    } else {
        None
    }
}

/// Step 2: chat | finished over LIVE | agents over jobs (over workbench).
///
/// Column widths exactly as in §3.3.2 and the same for both profiles:
/// `center = clamp(cols / 4, 34..=60)`, `right = clamp(cols / 5, 26..=44)`,
/// the chat takes the rest. That rest never drops below the profile's
/// `chat_min` (48 at 108 columns, 72 at 132; the tests check every width),
/// so `chat_min` only decides whether the explorer fits. Hidden mission
/// windows drop the centre column; the right column stays only for a
/// visible workbench.
fn landscape_three_columns(
    base: MissionLayout,
    input: MissionInput,
    chat_min: u16,
) -> MissionLayout {
    let cols = input.cols;
    let body = base.chat.height;
    let mission = input.agents_visible;
    let center_width = if mission {
        (cols / 4).clamp(LANDSCAPE_DENSE_CENTER_MIN_COLS, LANDSCAPE_CENTER_MAX_COLS)
    } else {
        0
    };
    let right_width = if mission || input.workbench_visible {
        (cols / 5).clamp(LANDSCAPE_DENSE_RIGHT_MIN_COLS, LANDSCAPE_RIGHT_MAX_COLS)
    } else {
        0
    };
    let chat_width = cols
        .saturating_sub(center_width)
        .saturating_sub(right_width);
    let (explorer, chat) = carve_explorer(
        Rect {
            x: 0,
            y: 0,
            width: chat_width,
            height: body,
        },
        input.explorer_visible,
        chat_min,
    );
    let mut layout = MissionLayout {
        placement: MissionPlacement::LandscapeThreeColumns,
        chat,
        explorer,
        ..base
    };

    let center_x = chat_width;
    let right_x = chat_width.saturating_add(center_width);
    if mission {
        let finished_height = input
            .finished_rows_wanted
            .clamp(FINISHED_EXPANDED_MIN_ROWS, FINISHED_EXPANDED_MAX_ROWS)
            .saturating_add(BORDER_ROWS);
        let live_height = body.saturating_sub(finished_height);
        layout.finished = Some(Rect {
            x: center_x,
            y: 0,
            width: center_width,
            height: finished_height,
        });
        layout.live = Some(Rect {
            x: center_x,
            y: finished_height,
            width: center_width,
            height: live_height,
        });
        layout.live_mode = window_live_mode(live_height.saturating_sub(BORDER_ROWS));
    }

    let right = Rect {
        x: right_x,
        y: 0,
        width: right_width,
        height: body,
    };
    match (mission, input.workbench_visible) {
        (true, true) => {
            // Workbench: the lower 40 % of the column, agents and jobs keep
            // their minimum rows each.
            let share = u16::try_from(u32::from(body) * 2 / 5).unwrap_or(body);
            let workbench_height =
                share.min(body.saturating_sub(LANDSCAPE_STACK_MIN_ROWS.saturating_mul(2)));
            let stack_height = body.saturating_sub(workbench_height);
            let (agents, jobs) = halve(Rect {
                height: stack_height,
                ..right
            });
            layout.agents = Some(agents);
            layout.jobs = Some(jobs);
            layout.workbench = Some(Rect {
                y: stack_height,
                height: workbench_height,
                ..right
            });
        }
        (true, false) => {
            let (agents, jobs) = halve(right);
            layout.agents = Some(agents);
            layout.jobs = Some(jobs);
        }
        (false, true) => layout.workbench = Some(right),
        (false, false) => {}
    }
    layout
}

/// Stage of the bordered landscape LIVE window in [`LiveMode`]'s own terms:
/// its unscrolled view shows the full LIVE body, at most
/// [`MINI_LIVE_MAX_BODY_LINES`] lines as in the fullest strip; the window's
/// further content rows are scroll room (see
/// [`MissionLayout::live_content_rows`]).
fn window_live_mode(content_rows: u16) -> LiveMode {
    match content_rows.min(MINI_LIVE_MAX_BODY_LINES) {
        0 => LiveMode::Header,
        lines => LiveMode::Body { lines },
    }
}

/// Step 3: today's side-column geometry from [`crate::classify`] (`chat_column`
/// left, `side_column` right), agents stacked above jobs in the side column
/// (with the workbench below them when visible) and the header strips on top
/// of the chat column.
fn shallow_landscape(
    base: MissionLayout,
    input: MissionInput,
    chat_column: Rect,
    side_column: Option<Rect>,
) -> MissionLayout {
    let body_rect = base.chat;
    let mission = input.agents_visible;
    // A side column needs a visible panel and room for one bordered window;
    // otherwise the chat keeps the full width.
    let side = side_column.filter(|column| {
        (mission || input.workbench_visible)
            && column.height >= WINDOW_MIN_ROWS
            && column.width >= WINDOW_MIN_COLS
    });
    let left = if side.is_some() {
        chat_column
    } else {
        body_rect
    };

    let mut layout = MissionLayout {
        placement: MissionPlacement::ShallowLandscape,
        ..base
    };
    if let Some(column) = side {
        let (stack, workbench) = split_side_column(column, mission, input.workbench_visible);
        layout.workbench = workbench;
        if let Some(stack) = stack {
            let (agents, jobs) = stack_agents_jobs(stack);
            layout.agents = Some(agents);
            layout.jobs = jobs;
        }
    }

    let (explorer, chat) = carve_explorer(left, input.explorer_visible, SIDE_CHAT_MIN_COLS);
    layout.explorer = explorer;
    layout.chat = chat;
    if mission {
        let strips = header_strips(chat);
        layout.finished = strips.finished;
        layout.live = strips.live;
        layout.live_mode = strips.live_mode;
        layout.chat = strips.chat;
    }
    layout
}

/// Step 4: [`crate::chat_first`] over the whole body, then the expanded
/// finished strip (taken from the chat, which keeps
/// [`MINI_LIVE_MIN_CHAT_ROWS`]), then the explorer left of the chat.
fn portrait_stack(base: MissionLayout, input: MissionInput) -> MissionLayout {
    let mut layout = MissionLayout {
        placement: MissionPlacement::PortraitStack,
        ..base
    };
    if input.agents_visible {
        let stack = chat_first(ChatFirstInput {
            area: base.chat,
            live_body_lines_wanted: input.live_body_lines_wanted,
            min_chat_rows: MINI_LIVE_MIN_CHAT_ROWS,
        });
        match stack.cards {
            Some(DockSplit::Split { agents, jobs }) => {
                layout.agents = Some(agents);
                layout.jobs = Some(jobs);
            }
            Some(DockSplit::Combined(cards)) => layout.agents = Some(cards),
            None => {}
        }
        layout.finished = stack.finished;
        layout.live = stack.live;
        layout.live_mode = stack.live_mode;
        layout.chat = stack.chat;

        if let Some(strip) = stack.finished {
            let target = finished_strip_rows(input.finished_rows_wanted);
            let spare = stack.chat.height.saturating_sub(MINI_LIVE_MIN_CHAT_ROWS);
            let granted = target.min(FINISHED_COLLAPSED_ROWS.saturating_add(spare));
            let extra = if granted >= FINISHED_EXPANDED_MIN_ROWS {
                granted.saturating_sub(FINISHED_COLLAPSED_ROWS)
            } else {
                0
            };
            if extra > 0 {
                layout.finished = Some(Rect {
                    height: strip.height.saturating_add(extra),
                    ..strip
                });
                layout.live = stack.live.map(|live| Rect {
                    y: live.y.saturating_add(extra),
                    ..live
                });
                layout.chat = Rect {
                    y: stack.chat.y.saturating_add(extra),
                    height: stack.chat.height.saturating_sub(extra),
                    ..stack.chat
                };
            }
        }
    }
    let (explorer, chat) = carve_explorer(layout.chat, input.explorer_visible, SIDE_CHAT_MIN_COLS);
    layout.explorer = explorer;
    layout.chat = chat;
    layout
}

/// Step 5: no status windows; the explorer follows today's side-panel rule.
fn compact_summary(base: MissionLayout, input: MissionInput) -> MissionLayout {
    let (explorer, chat) = carve_explorer(base.chat, input.explorer_visible, SIDE_CHAT_MIN_COLS);
    MissionLayout {
        placement: MissionPlacement::CompactSummary,
        chat,
        explorer,
        ..base
    }
}

/// Finished strip rows in the portrait stack: collapsed for 0 and 1,
/// otherwise clamped to the expanded range.
fn finished_strip_rows(wanted: u16) -> u16 {
    if wanted <= FINISHED_COLLAPSED_ROWS {
        FINISHED_COLLAPSED_ROWS
    } else {
        wanted.clamp(FINISHED_EXPANDED_MIN_ROWS, FINISHED_EXPANDED_MAX_ROWS)
    }
}

/// Splits `column` into an upper part with `ceil(height / 2)` rows and a
/// lower part with the rest.
fn halve(column: Rect) -> (Rect, Rect) {
    let upper = column.height.saturating_sub(column.height / 2);
    (
        Rect {
            height: upper,
            ..column
        },
        Rect {
            y: column.y.saturating_add(upper),
            height: column.height.saturating_sub(upper),
            ..column
        },
    )
}

/// Agents above jobs in a side column: both halves when each keeps a bordered
/// window, otherwise the agents window takes the whole column and holds the
/// combined list (`jobs == None`, see [`MissionLayout::agents`]).
fn stack_agents_jobs(column: Rect) -> (Rect, Option<Rect>) {
    if column.height >= WINDOW_MIN_ROWS.saturating_mul(2) {
        let (agents, jobs) = halve(column);
        (agents, Some(jobs))
    } else {
        (column, None)
    }
}

/// Shares the shallow-landscape side column between the mission windows
/// (upper part) and the workbench (lower part), like `split_right_column` in
/// `harw-tui/src/panes.rs`: with both visible the workbench gets 40 % of the
/// height, at least [`SIDE_WORKBENCH_MIN_ROWS`], and the upper part keeps a
/// bordered window; when that does not fit the mission windows get the
/// whole column (this crate knows no focus).
fn split_side_column(
    column: Rect,
    mission: bool,
    workbench_visible: bool,
) -> (Option<Rect>, Option<Rect>) {
    match (mission, workbench_visible) {
        (true, false) => (Some(column), None),
        (false, true) => (None, Some(column)),
        (false, false) => (None, None),
        (true, true) => {
            let height = column.height;
            if height < SIDE_WORKBENCH_MIN_ROWS.saturating_add(WINDOW_MIN_ROWS) {
                return (Some(column), None);
            }
            let share = u16::try_from(u32::from(height) * 2 / 5).unwrap_or(height);
            let workbench_height = share
                .max(SIDE_WORKBENCH_MIN_ROWS)
                .min(height.saturating_sub(WINDOW_MIN_ROWS));
            let upper = height.saturating_sub(workbench_height);
            (
                Some(Rect {
                    height: upper,
                    ..column
                }),
                Some(Rect {
                    y: column.y.saturating_add(upper),
                    height: workbench_height,
                    ..column
                }),
            )
        }
    }
}

/// Carves the explorer off the left edge of `region` when it is visible, has
/// room for a bordered window and the chat keeps `chat_min` columns next to
/// it; otherwise the whole region stays chat.
fn carve_explorer(region: Rect, visible: bool, chat_min: u16) -> (Option<Rect>, Rect) {
    if visible
        && region.height >= WINDOW_MIN_ROWS
        && region.width >= EXPLORER_COLS.saturating_add(chat_min)
    {
        let explorer = Rect {
            width: EXPLORER_COLS,
            ..region
        };
        let chat = Rect {
            x: region.x.saturating_add(EXPLORER_COLS),
            width: region.width.saturating_sub(EXPLORER_COLS),
            ..region
        };
        (Some(explorer), chat)
    } else {
        (None, region)
    }
}

/// Header strips of the shallow landscape on top of the chat column.
#[derive(Debug, Clone, Copy)]
struct HeaderStrips {
    finished: Option<Rect>,
    live: Option<Rect>,
    live_mode: LiveMode,
    chat: Rect,
}

/// Applies [`crate::chat_first`] to `column` with "no cards row" semantics: a
/// one-row finished strip and a header-only LIVE strip, both only while the
/// chat keeps [`MINI_LIVE_MIN_CHAT_ROWS`].
///
/// `chat_first` shows the finished strip only below its cards row, so it gets
/// an area extended by exactly [`CARDS_ROWS`] (the same technique as
/// `portrait_live_strip` in `harw-tui/src/panes.rs`); only the strips are
/// taken over, shifted back by those rows, and the chat is the rest of
/// `column`.
fn header_strips(column: Rect) -> HeaderStrips {
    let none = HeaderStrips {
        finished: None,
        live: None,
        live_mode: LiveMode::Hidden,
        chat: column,
    };
    if column.width == 0 || column.height == 0 {
        return none;
    }
    let stack = chat_first(ChatFirstInput {
        area: Rect {
            height: column.height.saturating_add(CARDS_ROWS),
            ..column
        },
        live_body_lines_wanted: 0,
        min_chat_rows: MINI_LIVE_MIN_CHAT_ROWS,
    });
    let shift = |rect: Rect| Rect {
        y: rect.y.saturating_sub(CARDS_ROWS),
        ..rect
    };
    let finished = stack.finished.map(shift);
    let live = stack.live.map(shift);
    let used = finished
        .map_or(0, |rect| rect.height)
        .saturating_add(live.map_or(0, |rect| rect.height));
    if used == 0 || used >= column.height {
        return none;
    }
    HeaderStrips {
        finished,
        live,
        live_mode: stack.live_mode,
        chat: Rect {
            y: column.y.saturating_add(used),
            height: column.height.saturating_sub(used),
            ..column
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Reference input: one status row, three composer rows, LIVE body
    /// wanted, finished collapsed, only the mission windows visible.
    fn input(cols: u16, rows: u16) -> MissionInput {
        MissionInput {
            cols,
            rows,
            status_rows: 1,
            composer_rows: 3,
            live_body_lines_wanted: 4,
            finished_rows_wanted: 1,
            agents_visible: true,
            workbench_visible: false,
            explorer_visible: false,
        }
    }

    fn rect(x: u16, y: u16, width: u16, height: u16) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn is_empty(rect: Rect) -> bool {
        rect.width == 0 || rect.height == 0
    }

    fn overlaps(a: Rect, b: Rect) -> bool {
        if is_empty(a) || is_empty(b) {
            return false;
        }
        let (ax, ay) = (u32::from(a.x), u32::from(a.y));
        let (bx, by) = (u32::from(b.x), u32::from(b.y));
        ax < bx + u32::from(b.width)
            && bx < ax + u32::from(a.width)
            && ay < by + u32::from(b.height)
            && by < ay + u32::from(a.height)
    }

    /// Every rect of `layout` with its name; `None` marks an absent window.
    fn named(layout: &MissionLayout) -> Vec<(&'static str, Option<Rect>)> {
        vec![
            ("agents", layout.agents),
            ("jobs", layout.jobs),
            ("finished", layout.finished),
            ("live", layout.live),
            ("explorer", layout.explorer),
            ("workbench", layout.workbench),
        ]
    }

    /// Placement and orientation oracle: §3.3.1/§3.3.2 restated with their
    /// literal numbers, independent of the module's constants and helpers.
    fn spec_placement(input: MissionInput) -> (Orientation, MissionPlacement) {
        let (cols, rows) = (input.cols, input.rows);
        let orientation = if rows > cols {
            Orientation::Portrait
        } else if cols > rows {
            Orientation::Landscape
        } else {
            Orientation::Balanced
        };
        let body = rows.saturating_sub(input.status_rows.saturating_add(input.composer_rows));
        let placement = if orientation != Orientation::Portrait && body >= 20 && cols >= 108 {
            MissionPlacement::LandscapeThreeColumns
        } else if cols >= 100 {
            MissionPlacement::ShallowLandscape
        } else if cols >= 40 && rows >= 28 {
            MissionPlacement::PortraitStack
        } else {
            MissionPlacement::CompactSummary
        };
        (orientation, placement)
    }

    /// Column oracle of the three-column landscape (§3.3.2, literal numbers):
    /// `(chat column incl. explorer, centre, right, chat minimum)`.
    fn spec_columns(input: MissionInput) -> (u16, u16, u16, u16) {
        let cols = input.cols;
        let chat_min = if cols >= 132 { 64 } else { 48 };
        let center = if input.agents_visible {
            (cols / 4).clamp(34, 60)
        } else {
            0
        };
        let right = if input.agents_visible || input.workbench_visible {
            (cols / 5).clamp(26, 44)
        } else {
            0
        };
        (cols - center - right, center, right, chat_min)
    }

    /// Checks every invariant of the module docs for one input, plus the
    /// spec's placement decision and, in the three-column landscape, its
    /// column widths.
    fn check_invariants(input: MissionInput, layout: &MissionLayout) -> Result<(), String> {
        let ctx = format!("{}x{} {input:?}", input.cols, input.rows);
        let (cols, rows) = (input.cols, input.rows);

        // The spec's decision order 1-5.
        let (orientation, placement) = spec_placement(input);
        if layout.orientation != orientation || layout.placement != placement {
            return Err(format!(
                "{ctx}: got {:?}/{:?}, the spec says {orientation:?}/{placement:?}",
                layout.orientation, layout.placement
            ));
        }
        let body = rows.saturating_sub(input.status_rows.saturating_add(input.composer_rows));
        if layout.status.y != body {
            return Err(format!("{ctx}: body is not rows - status - composer"));
        }

        let mut all: Vec<(&'static str, Rect)> = vec![
            ("chat", layout.chat),
            ("status", layout.status),
            ("composer", layout.composer),
        ];
        let strip_placement = matches!(
            layout.placement,
            MissionPlacement::PortraitStack | MissionPlacement::ShallowLandscape
        );
        for (name, slot) in named(layout) {
            let Some(rect) = slot else { continue };
            let strip = strip_placement && matches!(name, "finished" | "live");
            let min_rows = if strip { 1 } else { WINDOW_MIN_ROWS };
            if rect.width < WINDOW_MIN_COLS || rect.height < min_rows {
                return Err(format!("{ctx}: {name} too small: {rect:?}"));
            }
            all.push((name, rect));
        }

        // Inside the terminal.
        for (name, rect) in &all {
            if u32::from(rect.x) + u32::from(rect.width) > u32::from(cols)
                || u32::from(rect.y) + u32::from(rect.height) > u32::from(rows)
            {
                return Err(format!("{ctx}: {name} leaves the terminal: {rect:?}"));
            }
        }
        // Pairwise disjoint.
        for (index, (name_a, a)) in all.iter().enumerate() {
            for (name_b, b) in all.iter().skip(index + 1) {
                if overlaps(*a, *b) {
                    return Err(format!("{ctx}: {name_a} {a:?} overlaps {name_b} {b:?}"));
                }
            }
        }
        // Disjoint and inside, so equal total area means an exact cover.
        let covered: u64 = all
            .iter()
            .map(|(_, rect)| u64::from(rect.width) * u64::from(rect.height))
            .sum();
        if covered != u64::from(cols) * u64::from(rows) {
            return Err(format!("{ctx}: rects cover {covered} cells"));
        }

        // LIVE stage in `LiveMode`'s own terms, consistent with its rect.
        match (layout.live, layout.live_mode) {
            (None, LiveMode::Hidden) => {}
            (Some(live), LiveMode::Header) if strip_placement && live.height == 1 => {}
            (Some(live), LiveMode::Body { lines })
                if strip_placement
                    && (1..=MINI_LIVE_MAX_BODY_LINES).contains(&lines)
                    && live.height == lines + 1 => {}
            (Some(live), LiveMode::Body { lines })
                if !strip_placement
                    && live.height > BORDER_ROWS
                    && lines == (live.height - BORDER_ROWS).min(MINI_LIVE_MAX_BODY_LINES) => {}
            other => return Err(format!("{ctx}: inconsistent LIVE state {other:?}")),
        }
        let content_rows = match (layout.placement, layout.live) {
            (_, None) => 0,
            (MissionPlacement::LandscapeThreeColumns, Some(live)) => live.height - BORDER_ROWS,
            (_, Some(live)) => live.height - 1,
        };
        if layout.live_content_rows() != content_rows {
            return Err(format!("{ctx}: LIVE content rows are not {content_rows}"));
        }

        // `jobs == None` next to `Some` agents means the combined list, and
        // only where the docs allow it.
        match (layout.agents, layout.jobs) {
            (None, None) => {}
            (None, Some(_)) => return Err(format!("{ctx}: jobs without agents")),
            (Some(agents), None) => {
                let combined = match layout.placement {
                    MissionPlacement::PortraitStack => cols < 68,
                    MissionPlacement::ShallowLandscape => agents.height < 6,
                    _ => false,
                };
                if !combined {
                    return Err(format!("{ctx}: agents without jobs is not a combined case"));
                }
            }
            (Some(agents), Some(jobs)) => {
                let split = match layout.placement {
                    MissionPlacement::PortraitStack => {
                        cols >= 68
                            && (jobs.y, jobs.height) == (agents.y, agents.height)
                            && jobs.x == agents.x + agents.width
                    }
                    MissionPlacement::ShallowLandscape
                    | MissionPlacement::LandscapeThreeColumns => {
                        (jobs.x, jobs.width) == (agents.x, agents.width)
                            && jobs.y == agents.y + agents.height
                    }
                    MissionPlacement::CompactSummary => false,
                };
                if !split {
                    return Err(format!(
                        "{ctx}: agents {agents:?} and jobs {jobs:?} not split"
                    ));
                }
            }
        }

        let mission_windows = [layout.agents, layout.jobs, layout.finished, layout.live];
        if !input.agents_visible && mission_windows.iter().any(Option::is_some) {
            return Err(format!("{ctx}: hidden mission windows were placed"));
        }
        match layout.placement {
            MissionPlacement::CompactSummary => {
                if mission_windows.iter().any(Option::is_some) || layout.workbench.is_some() {
                    return Err(format!("{ctx}: compact summary placed a window"));
                }
            }
            MissionPlacement::LandscapeThreeColumns => {
                check_columns(&ctx, input, layout)?;
            }
            MissionPlacement::PortraitStack | MissionPlacement::ShallowLandscape => {}
        }
        // Chat ends on the body's last row, status and composer follow.
        if u32::from(layout.chat.y) + u32::from(layout.chat.height) != u32::from(layout.status.y)
            || u32::from(layout.status.y) + u32::from(layout.status.height)
                != u32::from(layout.composer.y)
            || u32::from(layout.composer.y) + u32::from(layout.composer.height) != u32::from(rows)
        {
            return Err(format!("{ctx}: chat, status and composer are not stacked"));
        }
        Ok(())
    }

    /// Three-column widths, positions and the explorer rule against
    /// [`spec_columns`].
    fn check_columns(ctx: &str, input: MissionInput, layout: &MissionLayout) -> Result<(), String> {
        let (chat_column, center, right, chat_min) = spec_columns(input);
        let explorer_width = layout.explorer.map_or(0, |rect| rect.width);
        if layout.chat.width + explorer_width != chat_column {
            return Err(format!("{ctx}: chat column is not {chat_column} wide"));
        }
        let explorer_expected = input.explorer_visible && chat_column >= 36 + chat_min;
        if layout.explorer.is_some() != explorer_expected {
            return Err(format!(
                "{ctx}: explorer placement is not {explorer_expected}"
            ));
        }
        if layout.chat.width < chat_min {
            return Err(format!("{ctx}: chat below the profile minimum {chat_min}"));
        }
        let centre_windows = [("finished", layout.finished), ("live", layout.live)];
        for (name, slot) in centre_windows {
            if slot.is_some_and(|rect| (rect.x, rect.width) != (chat_column, center)) {
                return Err(format!("{ctx}: {name} is not the centre column {center}"));
            }
        }
        let right_windows = [
            ("agents", layout.agents),
            ("jobs", layout.jobs),
            ("workbench", layout.workbench),
        ];
        for (name, slot) in right_windows {
            if slot.is_some_and(|rect| (rect.x, rect.width) != (chat_column + center, right)) {
                return Err(format!("{ctx}: {name} is not the right column {right}"));
            }
        }
        let all_four = [layout.agents, layout.jobs, layout.finished, layout.live];
        if input.agents_visible && all_four.iter().any(Option::is_none) {
            return Err(format!("{ctx}: three columns without all four windows"));
        }
        for (name, slot) in named(layout) {
            if slot.is_some_and(|rect| rect.width >= layout.chat.width) {
                return Err(format!("{ctx}: {name} is not narrower than the chat"));
            }
        }
        Ok(())
    }

    /// One row of the size matrix: placement plus every window rect.
    struct Expected {
        cols: u16,
        rows: u16,
        orientation: Orientation,
        placement: MissionPlacement,
        chat: Rect,
        agents: Option<Rect>,
        jobs: Option<Rect>,
        finished: Option<Rect>,
        live: Option<Rect>,
        live_mode: LiveMode,
        live_content_rows: u16,
    }

    /// Classifies every row of `table` with the reference input and checks
    /// the invariants and every expected rect.
    fn assert_matrix(table: &[Expected]) -> TestResult {
        for row in table {
            let inp = input(row.cols, row.rows);
            let layout = classify_mission(inp);
            let ctx = format!("{}x{}", row.cols, row.rows);
            check_invariants(inp, &layout)?;
            assert_eq!(layout.orientation, row.orientation, "{ctx}");
            assert_eq!(layout.placement, row.placement, "{ctx}");
            assert_eq!(layout.chat, row.chat, "{ctx} chat");
            assert_eq!(layout.agents, row.agents, "{ctx} agents");
            assert_eq!(layout.jobs, row.jobs, "{ctx} jobs");
            assert_eq!(layout.finished, row.finished, "{ctx} finished");
            assert_eq!(layout.live, row.live, "{ctx} live");
            assert_eq!(layout.live_mode, row.live_mode, "{ctx} live_mode");
            assert_eq!(
                layout.live_content_rows(),
                row.live_content_rows,
                "{ctx} live content rows"
            );
            assert!(layout.explorer.is_none(), "{ctx} explorer");
            assert!(layout.workbench.is_none(), "{ctx} workbench");
            // Bottom rows of the reference input (1 status, 3 composer).
            if row.rows >= 4 {
                assert_eq!(layout.status, rect(0, row.rows - 4, row.cols, 1), "{ctx}");
                assert_eq!(layout.composer, rect(0, row.rows - 3, row.cols, 3), "{ctx}");
            }
        }
        Ok(())
    }

    #[test]
    fn portrait_stack_at_46x78_48x65_70x90_60x60_80x40() -> TestResult {
        use MissionPlacement::PortraitStack;
        use Orientation::{Balanced, Landscape, Portrait};
        assert_matrix(&[
            // Combined cards below 68 columns.
            Expected {
                cols: 46,
                rows: 78,
                orientation: Portrait,
                placement: PortraitStack,
                chat: rect(0, 10, 46, 64),
                agents: Some(rect(0, 0, 46, 4)),
                jobs: None,
                finished: Some(rect(0, 4, 46, 1)),
                live: Some(rect(0, 5, 46, 5)),
                live_mode: LiveMode::Body { lines: 4 },
                live_content_rows: 4,
            },
            Expected {
                cols: 48,
                rows: 65,
                orientation: Portrait,
                placement: PortraitStack,
                chat: rect(0, 10, 48, 51),
                agents: Some(rect(0, 0, 48, 4)),
                jobs: None,
                finished: Some(rect(0, 4, 48, 1)),
                live: Some(rect(0, 5, 48, 5)),
                live_mode: LiveMode::Body { lines: 4 },
                live_content_rows: 4,
            },
            // Agents | jobs from 68 columns.
            Expected {
                cols: 70,
                rows: 90,
                orientation: Portrait,
                placement: PortraitStack,
                chat: rect(0, 10, 70, 76),
                agents: Some(rect(0, 0, 35, 4)),
                jobs: Some(rect(35, 0, 35, 4)),
                finished: Some(rect(0, 4, 70, 1)),
                live: Some(rect(0, 5, 70, 5)),
                live_mode: LiveMode::Body { lines: 4 },
                live_content_rows: 4,
            },
            Expected {
                cols: 60,
                rows: 60,
                orientation: Balanced,
                placement: PortraitStack,
                chat: rect(0, 10, 60, 46),
                agents: Some(rect(0, 0, 60, 4)),
                jobs: None,
                finished: Some(rect(0, 4, 60, 1)),
                live: Some(rect(0, 5, 60, 5)),
                live_mode: LiveMode::Body { lines: 4 },
                live_content_rows: 4,
            },
            // Landscape by aspect, but portrait stack by rule (< 100 columns).
            Expected {
                cols: 80,
                rows: 40,
                orientation: Landscape,
                placement: PortraitStack,
                chat: rect(0, 10, 80, 26),
                agents: Some(rect(0, 0, 40, 4)),
                jobs: Some(rect(40, 0, 40, 4)),
                finished: Some(rect(0, 4, 80, 1)),
                live: Some(rect(0, 5, 80, 5)),
                live_mode: LiveMode::Body { lines: 4 },
                live_content_rows: 4,
            },
        ])
    }

    #[test]
    fn shallow_landscape_at_100x30() -> TestResult {
        // Side column 44 wide, strips above the chat.
        assert_matrix(&[Expected {
            cols: 100,
            rows: 30,
            orientation: Orientation::Landscape,
            placement: MissionPlacement::ShallowLandscape,
            chat: rect(0, 2, 56, 24),
            agents: Some(rect(56, 0, 44, 13)),
            jobs: Some(rect(56, 13, 44, 13)),
            finished: Some(rect(0, 0, 56, 1)),
            live: Some(rect(0, 1, 56, 1)),
            live_mode: LiveMode::Header,
            live_content_rows: 0,
        }])
    }

    #[test]
    fn three_columns_at_120x42_160x45_200x50() -> TestResult {
        use MissionPlacement::LandscapeThreeColumns;
        use Orientation::Landscape;
        assert_matrix(&[
            // Dense profile: 60 | 34 | 26.
            Expected {
                cols: 120,
                rows: 42,
                orientation: Landscape,
                placement: LandscapeThreeColumns,
                chat: rect(0, 0, 60, 38),
                agents: Some(rect(94, 0, 26, 19)),
                jobs: Some(rect(94, 19, 26, 19)),
                finished: Some(rect(60, 0, 34, 5)),
                live: Some(rect(60, 5, 34, 33)),
                live_mode: LiveMode::Body { lines: 4 },
                live_content_rows: 31,
            },
            // Full profile: 88 | 40 | 32.
            Expected {
                cols: 160,
                rows: 45,
                orientation: Landscape,
                placement: LandscapeThreeColumns,
                chat: rect(0, 0, 88, 41),
                agents: Some(rect(128, 0, 32, 21)),
                jobs: Some(rect(128, 21, 32, 20)),
                finished: Some(rect(88, 0, 40, 5)),
                live: Some(rect(88, 5, 40, 36)),
                live_mode: LiveMode::Body { lines: 4 },
                live_content_rows: 34,
            },
            // Full profile: 110 | 50 | 40.
            Expected {
                cols: 200,
                rows: 50,
                orientation: Landscape,
                placement: LandscapeThreeColumns,
                chat: rect(0, 0, 110, 46),
                agents: Some(rect(160, 0, 40, 23)),
                jobs: Some(rect(160, 23, 40, 23)),
                finished: Some(rect(110, 0, 50, 5)),
                live: Some(rect(110, 5, 50, 41)),
                live_mode: LiveMode::Body { lines: 4 },
                live_content_rows: 39,
            },
        ])
    }

    #[test]
    fn compact_summary_at_39x60_48x19_1x1_0x0() -> TestResult {
        use MissionPlacement::CompactSummary;
        use Orientation::{Balanced, Landscape, Portrait};
        let empty = |cols, rows, orientation, chat| Expected {
            cols,
            rows,
            orientation,
            placement: CompactSummary,
            chat,
            agents: None,
            jobs: None,
            finished: None,
            live: None,
            live_mode: LiveMode::Hidden,
            live_content_rows: 0,
        };
        // Too narrow, too low, degenerate.
        assert_matrix(&[
            empty(39, 60, Portrait, rect(0, 0, 39, 56)),
            empty(48, 19, Landscape, rect(0, 0, 48, 15)),
            empty(1, 1, Balanced, rect(0, 0, 1, 0)),
            empty(0, 0, Balanced, rect(0, 0, 0, 0)),
        ])?;
        // 1x1: the composer keeps its row first.
        let tiny = classify_mission(input(1, 1));
        assert_eq!(tiny.composer, rect(0, 0, 1, 1));
        assert_eq!(tiny.status, rect(0, 0, 1, 0));
        let zero = classify_mission(input(0, 0));
        assert_eq!(zero.status, rect(0, 0, 0, 0));
        assert_eq!(zero.composer, rect(0, 0, 0, 0));
        Ok(())
    }

    #[test]
    fn placement_flips_exactly_at_cols_99_100_107_108_131_132_39_40_rows_27_28_body_19_20()
    -> TestResult {
        use MissionPlacement::{
            CompactSummary, LandscapeThreeColumns, PortraitStack, ShallowLandscape,
        };
        // (cols, rows, placement, three-column widths chat | centre | right).
        let table = [
            // Wide side column from 100 columns.
            (99, 30, PortraitStack, None),
            (100, 30, ShallowLandscape, None),
            // Dense three columns from 108 columns.
            (107, 45, ShallowLandscape, None),
            (108, 45, LandscapeThreeColumns, Some((48, 34, 26))),
            // Dense to full profile: the columns do not jump.
            (131, 45, LandscapeThreeColumns, Some((71, 34, 26))),
            (132, 45, LandscapeThreeColumns, Some((72, 34, 26))),
            (151, 45, LandscapeThreeColumns, Some((84, 37, 30))),
            (152, 45, LandscapeThreeColumns, Some((84, 38, 30))),
            // Body 19 / 20 (one status row, three composer rows).
            (108, 23, ShallowLandscape, None),
            (108, 24, LandscapeThreeColumns, Some((48, 34, 26))),
            (160, 23, ShallowLandscape, None),
            (160, 24, LandscapeThreeColumns, Some((88, 40, 32))),
            // Portrait dock from 40 columns and 28 rows.
            (60, 27, CompactSummary, None),
            (60, 28, PortraitStack, None),
            (39, 60, CompactSummary, None),
            (40, 60, PortraitStack, None),
            // Balanced gets three columns, portrait never.
            (120, 120, LandscapeThreeColumns, Some((60, 34, 26))),
            (120, 121, ShallowLandscape, None),
            (132, 132, LandscapeThreeColumns, Some((72, 34, 26))),
            (132, 133, ShallowLandscape, None),
        ];
        for (cols, rows, placement, widths) in table {
            let inp = input(cols, rows);
            let layout = classify_mission(inp);
            let ctx = format!("{cols}x{rows}");
            check_invariants(inp, &layout)?;
            assert_eq!(layout.placement, placement, "{ctx}");
            if let Some((chat, center, right)) = widths {
                let live = layout.live.ok_or("live missing")?;
                let agents = layout.agents.ok_or("agents missing")?;
                assert_eq!(
                    (layout.chat.width, live.width, agents.width),
                    (chat, center, right),
                    "{ctx}"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn landscape_columns_grow_smoothly_from_108_to_400_cols() -> TestResult {
        let variants = [
            input(0, 45),
            MissionInput {
                workbench_visible: true,
                ..input(0, 45)
            },
            MissionInput {
                explorer_visible: true,
                ..input(0, 45)
            },
            MissionInput {
                agents_visible: false,
                workbench_visible: true,
                explorer_visible: true,
                ..input(0, 45)
            },
        ];
        for variant in variants {
            let mut previous: Option<(u16, u16, u16)> = None;
            for cols in 108..=400u16 {
                let inp = MissionInput { cols, ..variant };
                let layout = classify_mission(inp);
                check_invariants(inp, &layout)?;
                let ctx = format!("{cols}x45 {variant:?}");
                assert_eq!(
                    layout.placement,
                    MissionPlacement::LandscapeThreeColumns,
                    "{ctx}"
                );
                let chat_column = layout.chat.width + layout.explorer.map_or(0, |r| r.width);
                let center = layout.live.map_or(0, |r| r.width);
                let right = layout.agents.or(layout.workbench).map_or(0, |r| r.width);
                if let Some((last_chat, last_center, last_right)) = previous {
                    // One more column never narrows the chat column by more
                    // than one column, nor the centre or the right column.
                    assert!(chat_column + 1 >= last_chat, "{ctx}: chat jumps");
                    assert!(center >= last_center, "{ctx}: centre shrinks");
                    assert!(right >= last_right, "{ctx}: right column shrinks");
                }
                previous = Some((chat_column, center, right));
            }
        }
        Ok(())
    }

    #[test]
    fn landscape_anchors_at_120x42_160x45_200x50_chat_left_finished_top_centre_live_below_agents_over_jobs_right()
    -> TestResult {
        for (cols, rows) in [(120u16, 42u16), (160, 45), (200, 50)] {
            let layout = classify_mission(input(cols, rows));
            let ctx = format!("{cols}x{rows}");
            assert_eq!(
                layout.placement,
                MissionPlacement::LandscapeThreeColumns,
                "{ctx}"
            );
            let body = rows - 4;
            let finished = layout.finished.ok_or("finished missing")?;
            let live = layout.live.ok_or("live missing")?;
            let agents = layout.agents.ok_or("agents missing")?;
            let jobs = layout.jobs.ok_or("jobs missing")?;
            // Chat left and widest.
            assert_eq!(layout.chat.x, 0, "{ctx}");
            assert_eq!(layout.chat.height, body, "{ctx}");
            for other in [finished, live, agents, jobs] {
                assert!(layout.chat.width > other.width, "{ctx}: chat not widest");
            }
            // Finished top-centre, LIVE directly below it.
            assert_eq!(finished.x, layout.chat.width, "{ctx}");
            assert_eq!(finished.y, 0, "{ctx}");
            assert_eq!((live.x, live.width), (finished.x, finished.width), "{ctx}");
            assert_eq!(live.y, finished.height, "{ctx}");
            assert_eq!(live.y + live.height, body, "{ctx}");
            // Agents above jobs, far right.
            assert_eq!(agents.x + agents.width, cols, "{ctx}");
            assert_eq!(agents.x, finished.x + finished.width, "{ctx}");
            assert_eq!(agents.y, 0, "{ctx}");
            assert_eq!((jobs.x, jobs.width), (agents.x, agents.width), "{ctx}");
            assert_eq!(jobs.y, agents.height, "{ctx}");
            assert_eq!(jobs.y + jobs.height, body, "{ctx}");
            assert!(agents.height >= LANDSCAPE_STACK_MIN_ROWS, "{ctx}");
            assert!(jobs.height >= LANDSCAPE_STACK_MIN_ROWS, "{ctx}");
            // LIVE stage in `LiveMode`'s range; the window scrolls beyond it.
            assert_eq!(
                layout.live_mode,
                LiveMode::Body {
                    lines: MINI_LIVE_MAX_BODY_LINES
                },
                "{ctx}"
            );
            assert_eq!(layout.live_content_rows(), live.height - 2, "{ctx}");
        }
        Ok(())
    }

    #[test]
    fn exhaustive_0x0_to_250x120_keeps_every_invariant_and_the_spec_placement() -> TestResult {
        let variants = [
            input(0, 0),
            MissionInput {
                status_rows: 2,
                live_body_lines_wanted: 2,
                finished_rows_wanted: 8,
                workbench_visible: true,
                explorer_visible: true,
                ..input(0, 0)
            },
            MissionInput {
                live_body_lines_wanted: 0,
                finished_rows_wanted: 5,
                agents_visible: false,
                workbench_visible: true,
                explorer_visible: true,
                ..input(0, 0)
            },
            MissionInput {
                status_rows: 0,
                composer_rows: 0,
                live_body_lines_wanted: 99,
                finished_rows_wanted: 2,
                workbench_visible: true,
                ..input(0, 0)
            },
            MissionInput {
                status_rows: 40,
                composer_rows: 70,
                finished_rows_wanted: u16::MAX,
                agents_visible: false,
                ..input(0, 0)
            },
        ];
        for variant in variants {
            for cols in 0..=250u16 {
                for rows in 0..=120u16 {
                    let inp = MissionInput {
                        cols,
                        rows,
                        ..variant
                    };
                    check_invariants(inp, &classify_mission(inp))?;
                }
            }
        }
        // Extreme sizes never panic and still partition the terminal.
        for (cols, rows) in [(u16::MAX, u16::MAX), (u16::MAX, 1), (1, u16::MAX)] {
            let inp = MissionInput {
                cols,
                rows,
                status_rows: u16::MAX,
                composer_rows: u16::MAX,
                ..input(0, 0)
            };
            let layout = classify_mission(inp);
            check_invariants(inp, &layout)?;
            assert_eq!(layout.composer.height, rows);
            assert_eq!(layout.chat.height, 0);
            let roomy = MissionInput {
                cols,
                rows,
                ..input(0, 0)
            };
            check_invariants(roomy, &classify_mission(roomy))?;
        }
        Ok(())
    }

    #[test]
    fn orientation_flip_reclassifies_without_state() -> TestResult {
        assert_eq!(orientation(46, 78), Orientation::Portrait);
        assert_eq!(orientation(78, 46), Orientation::Landscape);
        assert_eq!(orientation(60, 60), Orientation::Balanced);
        assert_eq!(orientation(0, 0), Orientation::Balanced);

        let wide = classify_mission(input(160, 45));
        let tall = classify_mission(input(45, 160));
        assert_eq!(wide.orientation, Orientation::Landscape);
        assert_eq!(wide.placement, MissionPlacement::LandscapeThreeColumns);
        assert_eq!(tall.orientation, Orientation::Portrait);
        assert_eq!(tall.placement, MissionPlacement::PortraitStack);
        // Back to the first size: exactly the first layout again.
        assert_eq!(classify_mission(input(160, 45)), wide);

        // A resize sequence through every placement and back.
        let sizes = [
            (46u16, 78u16),
            (78, 46),
            (120, 42),
            (42, 120),
            (100, 30),
            (30, 100),
            (200, 50),
            (50, 200),
            (60, 60),
            (48, 19),
        ];
        let first: Vec<MissionLayout> = sizes
            .iter()
            .map(|&(cols, rows)| classify_mission(input(cols, rows)))
            .collect();
        for (&(cols, rows), layout) in sizes.iter().zip(&first).rev() {
            let flipped = classify_mission(input(rows, cols));
            check_invariants(input(rows, cols), &flipped)?;
            if cols != rows {
                assert_ne!(flipped.orientation, layout.orientation, "{cols}x{rows}");
            }
            assert_eq!(
                classify_mission(input(cols, rows)),
                *layout,
                "{cols}x{rows}"
            );
        }
        // Portrait never gets three columns, however wide.
        let portrait_wide = classify_mission(input(140, 150));
        assert_eq!(portrait_wide.orientation, Orientation::Portrait);
        assert_eq!(portrait_wide.placement, MissionPlacement::ShallowLandscape);
        Ok(())
    }

    #[test]
    fn landscape_workbench_at_200x50_and_160x24_takes_the_lower_forty_percent_of_the_right_column()
    -> TestResult {
        let layout = classify_mission(MissionInput {
            workbench_visible: true,
            ..input(200, 50)
        });
        assert_eq!(layout.placement, MissionPlacement::LandscapeThreeColumns);
        assert_eq!(layout.agents, Some(rect(160, 0, 40, 14)));
        assert_eq!(layout.jobs, Some(rect(160, 14, 40, 14)));
        assert_eq!(layout.workbench, Some(rect(160, 28, 40, 18)));
        // Lowest landscape body: agents and jobs keep five rows each.
        let low = classify_mission(MissionInput {
            workbench_visible: true,
            ..input(160, 24)
        });
        assert_eq!(low.placement, MissionPlacement::LandscapeThreeColumns);
        let agents = low.agents.ok_or("agents missing")?;
        let jobs = low.jobs.ok_or("jobs missing")?;
        let workbench = low.workbench.ok_or("workbench missing")?;
        assert!(agents.height >= LANDSCAPE_STACK_MIN_ROWS);
        assert!(jobs.height >= LANDSCAPE_STACK_MIN_ROWS);
        assert_eq!(agents.height + jobs.height + workbench.height, 20);
        Ok(())
    }

    #[test]
    fn landscape_explorer_at_181x45_and_200x50_only_where_the_chat_keeps_its_minimum() -> TestResult
    {
        let wide = classify_mission(MissionInput {
            explorer_visible: true,
            ..input(200, 50)
        });
        assert_eq!(wide.explorer, Some(rect(0, 0, 36, 46)));
        assert_eq!(wide.chat, rect(36, 0, 74, 46));
        // Full profile: the chat column must keep 36 + 64 columns.
        let first = classify_mission(MissionInput {
            explorer_visible: true,
            ..input(181, 45)
        });
        assert_eq!(first.explorer, Some(rect(0, 0, 36, 41)));
        assert_eq!(first.chat, rect(36, 0, LANDSCAPE_CHAT_MIN_COLS, 41));
        for (cols, rows) in [(180u16, 45u16), (160, 45), (132, 45), (131, 45), (120, 42)] {
            let layout = classify_mission(MissionInput {
                explorer_visible: true,
                ..input(cols, rows)
            });
            assert!(layout.explorer.is_none(), "{cols}x{rows}");
            assert_eq!(layout.chat.x, 0, "{cols}x{rows}");
        }
        Ok(())
    }

    #[test]
    fn landscape_finished_window_at_160x45_follows_the_wanted_rows_plus_borders() -> TestResult {
        for (wanted, height) in [(0u16, 5u16), (1, 5), (3, 5), (5, 7), (8, 10), (40, 10)] {
            let layout = classify_mission(MissionInput {
                finished_rows_wanted: wanted,
                ..input(160, 45)
            });
            let finished = layout.finished.ok_or("finished missing")?;
            let live = layout.live.ok_or("live missing")?;
            assert_eq!(finished.height, height, "wanted {wanted}");
            assert_eq!(live.y, height, "wanted {wanted}");
            assert_eq!(live.height, 41 - height, "wanted {wanted}");
            assert_eq!(layout.live_content_rows(), 39 - height, "wanted {wanted}");
        }
        Ok(())
    }

    #[test]
    fn hidden_mission_windows_give_their_space_to_the_chat() -> TestResult {
        let hidden = |cols, rows| {
            classify_mission(MissionInput {
                agents_visible: false,
                ..input(cols, rows)
            })
        };
        let landscape = hidden(160, 45);
        assert_eq!(landscape.placement, MissionPlacement::LandscapeThreeColumns);
        assert_eq!(landscape.chat, rect(0, 0, 160, 41));
        let portrait = hidden(46, 78);
        assert_eq!(portrait.placement, MissionPlacement::PortraitStack);
        assert_eq!(portrait.chat, rect(0, 0, 46, 74));
        let shallow = hidden(100, 30);
        assert_eq!(shallow.placement, MissionPlacement::ShallowLandscape);
        assert_eq!(shallow.chat, rect(0, 0, 100, 26));
        // Only the workbench keeps the right column.
        let workbench = classify_mission(MissionInput {
            agents_visible: false,
            workbench_visible: true,
            ..input(160, 45)
        });
        assert_eq!(workbench.workbench, Some(rect(128, 0, 32, 41)));
        assert_eq!(workbench.chat, rect(0, 0, 128, 41));
        Ok(())
    }

    #[test]
    fn portrait_finished_strip_expands_while_the_chat_keeps_its_floor() -> TestResult {
        let expanded = classify_mission(MissionInput {
            finished_rows_wanted: 5,
            ..input(46, 78)
        });
        assert_eq!(expanded.finished, Some(rect(0, 4, 46, 5)));
        assert_eq!(expanded.live, Some(rect(0, 9, 46, 5)));
        assert_eq!(expanded.chat, rect(0, 14, 46, 60));
        // Two wanted rows round up to the expanded minimum.
        let two = classify_mission(MissionInput {
            finished_rows_wanted: 2,
            ..input(46, 78)
        });
        assert_eq!(
            two.finished.map(|r| r.height),
            Some(FINISHED_EXPANDED_MIN_ROWS)
        );
        // 46x28: body 24 = cards 4 + strip 1 + LIVE 5 + chat 14, so six spare
        // rows let the strip grow to seven rows and the chat keeps eight.
        let tight = classify_mission(MissionInput {
            finished_rows_wanted: 8,
            ..input(46, 28)
        });
        assert_eq!(tight.finished, Some(rect(0, 4, 46, 7)));
        assert_eq!(tight.chat.height, MINI_LIVE_MIN_CHAT_ROWS);
        // One spare row is not enough for an expanded window: stays collapsed.
        // Body 19 = cards 4 + strip 1 + LIVE 5 + chat 9.
        let collapsed = classify_mission(MissionInput {
            finished_rows_wanted: 8,
            composer_rows: 8,
            ..input(46, 28)
        });
        assert_eq!(collapsed.finished, Some(rect(0, 4, 46, 1)));
        assert_eq!(collapsed.chat.height, MINI_LIVE_MIN_CHAT_ROWS + 1);
        Ok(())
    }

    #[test]
    fn shallow_landscape_reuses_the_side_column_and_the_header_strips() -> TestResult {
        // Workbench below agents and jobs (40 %, at least eight rows).
        let workbench = classify_mission(MissionInput {
            workbench_visible: true,
            ..input(100, 30)
        });
        assert_eq!(workbench.agents, Some(rect(56, 0, 44, 8)));
        assert_eq!(workbench.jobs, Some(rect(56, 8, 44, 8)));
        assert_eq!(workbench.workbench, Some(rect(56, 16, 44, 10)));
        // Too low for three columns, wide enough for the explorer.
        let low = classify_mission(MissionInput {
            explorer_visible: true,
            ..input(140, 22)
        });
        assert_eq!(low.placement, MissionPlacement::ShallowLandscape);
        assert_eq!(low.explorer, Some(rect(0, 0, 36, 18)));
        assert_eq!(low.finished, Some(rect(36, 0, 60, 1)));
        assert_eq!(low.live, Some(rect(36, 1, 60, 1)));
        assert_eq!(low.chat, rect(36, 2, 60, 16));
        assert_eq!(low.agents, Some(rect(96, 0, 44, 9)));
        assert_eq!(low.jobs, Some(rect(96, 9, 44, 9)));
        // Short chat column: the strips go before the chat floor.
        let short = classify_mission(input(100, 12));
        assert_eq!(short.placement, MissionPlacement::ShallowLandscape);
        assert!(short.finished.is_none());
        assert!(short.live.is_none());
        assert_eq!(short.chat, rect(0, 0, 56, 8));
        assert_eq!(short.agents, Some(rect(56, 0, 44, 4)));
        assert_eq!(short.jobs, Some(rect(56, 4, 44, 4)));
        let header_only = classify_mission(input(100, 13));
        assert_eq!(header_only.finished, Some(rect(0, 0, 56, 1)));
        assert!(header_only.live.is_none());
        assert_eq!(header_only.live_mode, LiveMode::Hidden);
        Ok(())
    }

    #[test]
    fn shallow_landscape_at_100x8_100x15_100x17_puts_the_combined_list_into_agents() -> TestResult {
        // Body 4: the side stack is lower than two bordered windows, so the
        // agents rect holds agents and jobs together.
        let low = classify_mission(input(100, 8));
        check_invariants(input(100, 8), &low)?;
        assert_eq!(low.placement, MissionPlacement::ShallowLandscape);
        assert_eq!(low.agents, Some(rect(56, 0, 44, 4)));
        assert_eq!(low.jobs, None);
        assert_eq!(low.chat, rect(0, 0, 56, 4));
        assert!(low.finished.is_none() && low.live.is_none());
        // A visible workbench takes eight of the eleven side rows: three are
        // left for one combined window, and the chat keeps its strips.
        let with_workbench = |rows| MissionInput {
            workbench_visible: true,
            ..input(100, rows)
        };
        let workbench = classify_mission(with_workbench(15));
        check_invariants(with_workbench(15), &workbench)?;
        assert_eq!(workbench.placement, MissionPlacement::ShallowLandscape);
        assert_eq!(workbench.agents, Some(rect(56, 0, 44, 3)));
        assert_eq!(workbench.jobs, None);
        assert_eq!(workbench.workbench, Some(rect(56, 3, 44, 8)));
        assert_eq!(workbench.finished, Some(rect(0, 0, 56, 1)));
        assert_eq!(workbench.live, Some(rect(0, 1, 56, 1)));
        assert_eq!(workbench.chat, rect(0, 2, 56, 9));
        // Five rows above the workbench are still one combined window, six
        // split into agents above jobs.
        let five = classify_mission(with_workbench(17));
        check_invariants(with_workbench(17), &five)?;
        assert_eq!(five.agents, Some(rect(56, 0, 44, 5)));
        assert_eq!(five.jobs, None);
        let six = classify_mission(with_workbench(18));
        check_invariants(with_workbench(18), &six)?;
        assert_eq!(six.agents, Some(rect(56, 0, 44, 3)));
        assert_eq!(six.jobs, Some(rect(56, 3, 44, 3)));
        assert_eq!(six.workbench, Some(rect(56, 6, 44, 8)));
        Ok(())
    }
}
