//! Point-in-time snapshot of the running agent tree, plus the pure helpers
//! needed to build, validate and display it.
//!
//! The snapshot is a flat, bounded, user-safe list of nodes in pre-order
//! (parents always before their children). It carries topology and short
//! status text only, never a child's private history. Producers (a later wave:
//! the spawner projection) fill it from a non-blocking read of live state;
//! consumers (tools, TUI, channel adapters) render it.
//!
//! This module has no I/O, no clocks and no runtime dependencies.
//!
//! # Activity invariant
//!
//! A parent may only be [`AgentActivity::WaitingOnChildren`] when its own turn
//! has ended and every live (non-terminal) child is itself
//! `WaitingOnChildren` or running detached in the background. If any
//! foreground child is still working, the parent is not "waiting": somebody in
//! its subtree is actively working. [`derive_activity`] computes this bottom-up.

use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};

/// Current schema version of [`AgentTreeSnapshot`].
pub const AGENT_TREE_SCHEMA: u32 = 1;

/// Maximum number of characters kept in free-text fields (`task`,
/// `last_milestone`, failure `reason`). Longer text is cut on a char boundary
/// and ends with `…` (the ellipsis counts towards the cap).
pub const MAX_TEXT_CHARS: usize = 200;

/// Maximum characters of the milestone shown on one rendered tree line.
pub const RENDER_MILESTONE_CHARS: usize = 60;

/// Number of leading characters of an id shown in rendered lines.
const SHORT_ID_CHARS: usize = 8;

/// Bounds `text` to at most `max_chars` characters. When it is cut, the last
/// kept character is `…`. Never splits a multi-byte character.
#[must_use]
pub fn bound_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    if max_chars == 0 {
        return String::new();
    }
    let mut out: String = text.chars().take(max_chars - 1).collect();
    out.push('…');
    out
}

/// How an agent was started relative to its parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunKind {
    /// The parent blocks on this child's result.
    Foreground,
    /// Detached: runs on its own while the parent continues or ends its turn.
    Background,
    /// Backed by a durable job that survives the parent session.
    Durable,
}

impl AgentRunKind {
    fn label(self) -> &'static str {
        match self {
            Self::Foreground => "foreground",
            Self::Background => "background",
            Self::Durable => "durable",
        }
    }
}

/// Per-node lifecycle as shown to the user.
///
/// Invariant: a node is [`Self::WaitingOnChildren`] only when its own turn has
/// ended and every live child is itself `WaitingOnChildren` or running in the
/// background. See [`derive_activity`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentActivity {
    /// The node's own turn is active, or a foreground descendant is working.
    Working,
    /// Own turn ended; blocked only on the listed live children (ids), each of
    /// which is waiting or detached.
    WaitingOnChildren {
        /// Ids of the node's live children.
        children: Vec<String>,
    },
    /// Alive, but no own turn and no live children.
    Idle,
    /// Finished successfully.
    Completed,
    /// Finished with an error.
    Failed {
        /// Short, user-safe reason (bounded to [`MAX_TEXT_CHARS`]).
        reason: String,
    },
    /// Stopped on request.
    Cancelled,
}

impl AgentActivity {
    /// True for `Completed`, `Failed` and `Cancelled`.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed { .. } | Self::Cancelled
        )
    }
}

/// One agent in the tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTreeNode {
    /// Session id of the agent.
    pub id: String,
    /// Parent session id; `None` only for the root.
    pub parent: Option<String>,
    /// Distance from the root (root = 0).
    pub depth: u32,
    /// Role name of the agent.
    pub role: String,
    /// Foreground, background or durable.
    pub kind: AgentRunKind,
    /// Current lifecycle state.
    pub activity: AgentActivity,
    /// Short task description (bounded).
    pub task: Option<String>,
    /// Start time, if known.
    pub started_at: Option<jiff::Timestamp>,
    /// Id of the backing job for durable agents.
    pub job_work_id: Option<String>,
    /// Latest milestone text (bounded).
    pub last_milestone: Option<String>,
    /// Tokens used so far.
    pub tokens: Option<u64>,
    /// Tool calls completed so far.
    pub tool_calls: Option<u32>,
    /// Model the agent talks to.
    pub model: Option<String>,
    /// Number of this node's direct children dropped from `nodes` by
    /// truncation. Must be 0 unless the snapshot is `truncated`. For a
    /// `WaitingOnChildren` node, every omitted child is covered by the
    /// producer's guarantee that it is waiting or detached (see
    /// [`AgentTreeSnapshot::bounded`]). Absent in older payloads (= 0).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted_children: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl AgentTreeNode {
    /// Bounds every free-text field to [`MAX_TEXT_CHARS`].
    pub fn bound_texts(&mut self) {
        for text in [&mut self.task, &mut self.last_milestone]
            .into_iter()
            .flatten()
        {
            *text = bound_text(text, MAX_TEXT_CHARS);
        }
        if let AgentActivity::Failed { reason } = &mut self.activity {
            *reason = bound_text(reason, MAX_TEXT_CHARS);
        }
    }
}

/// A bounded, point-in-time view of the whole agent tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTreeSnapshot {
    /// Always [`AGENT_TREE_SCHEMA`] for snapshots built by this crate.
    pub schema_version: u32,
    /// When the snapshot was taken.
    pub generated_at: jiff::Timestamp,
    /// Session id of the root agent.
    pub root: String,
    /// Nodes in pre-order: every parent precedes its children.
    pub nodes: Vec<AgentTreeNode>,
    /// True when nodes were dropped to respect the node cap.
    pub truncated: bool,
    /// Number of nodes dropped (0 unless `truncated`).
    pub omitted: usize,
}

/// Reasons a snapshot is structurally invalid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentTreeError {
    /// `schema_version` is not [`AGENT_TREE_SCHEMA`].
    UnsupportedSchema(u32),
    /// Two nodes share an id.
    DuplicateId(String),
    /// The root node is missing, has a parent, or has a non-zero depth; or a
    /// second parentless node exists.
    InvalidRoot(String),
    /// A node names a parent that is not in the snapshot.
    Orphan {
        /// The orphaned node.
        id: String,
        /// The missing parent.
        parent: String,
    },
    /// Following parents from this node loops.
    Cycle(String),
    /// `depth` is not parent depth + 1.
    WrongDepth {
        /// The offending node.
        id: String,
        /// Depth implied by the parent.
        expected: u32,
        /// Depth found on the node.
        found: u32,
    },
    /// A `WaitingOnChildren` entry is not a live child of that node.
    BadWaitingChild {
        /// The waiting node.
        id: String,
        /// The offending child reference.
        child: String,
    },
    /// `WaitingOnChildren` with no children in a non-truncated snapshot.
    EmptyWaiting(String),
    /// A `WaitingOnChildren` list names the same child twice.
    DuplicateWaitingChild {
        /// The waiting node.
        id: String,
        /// The repeated child reference.
        child: String,
    },
    /// A live child present in the snapshot is missing from its parent's
    /// `WaitingOnChildren` list.
    UnlistedLiveChild {
        /// The waiting node.
        id: String,
        /// The live child that is not listed.
        child: String,
    },
    /// A foreground child of a `WaitingOnChildren` node is not itself waiting,
    /// so somebody in the subtree is still working.
    ForegroundChildNotWaiting {
        /// The waiting node.
        id: String,
        /// The foreground child that is not waiting.
        child: String,
    },
    /// `omitted_children` is inconsistent with `truncated` or with the
    /// children the node's waiting list names but the snapshot lacks.
    BadOmission(String),
}

impl fmt::Display for AgentTreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchema(v) => write!(f, "unsupported agent tree schema {v}"),
            Self::DuplicateId(id) => write!(f, "duplicate node id {id}"),
            Self::InvalidRoot(why) => write!(f, "invalid root: {why}"),
            Self::Orphan { id, parent } => write!(f, "node {id} has unknown parent {parent}"),
            Self::Cycle(id) => write!(f, "parent cycle through node {id}"),
            Self::WrongDepth {
                id,
                expected,
                found,
            } => write!(f, "node {id} has depth {found}, expected {expected}"),
            Self::BadWaitingChild { id, child } => {
                write!(f, "node {id} waits on {child}, which is not a live child")
            }
            Self::EmptyWaiting(id) => write!(f, "node {id} is waiting on no children"),
            Self::DuplicateWaitingChild { id, child } => {
                write!(f, "node {id} lists waiting child {child} more than once")
            }
            Self::UnlistedLiveChild { id, child } => {
                write!(
                    f,
                    "node {id} is waiting but does not list live child {child}"
                )
            }
            Self::ForegroundChildNotWaiting { id, child } => write!(
                f,
                "node {id} is waiting but foreground child {child} is not waiting"
            ),
            Self::BadOmission(id) => write!(f, "node {id} has inconsistent omitted_children"),
        }
    }
}

impl std::error::Error for AgentTreeError {}

impl AgentTreeSnapshot {
    /// Builds a bounded snapshot from an arbitrary node list.
    ///
    /// Nodes are re-ordered into pre-order from `root` (siblings keep their
    /// input order), nodes unreachable from the root are dropped, and at most
    /// `max_nodes` (at least 1) leading nodes are kept. A pre-order prefix is
    /// always connected, so parents stay before their children. Dropped nodes
    /// are counted in `omitted` and `truncated` is set; each kept node records
    /// how many of its direct children were dropped in `omitted_children`. A
    /// `WaitingOnChildren` node with a dropped live foreground child that is not
    /// itself waiting cannot be proven waiting, so it is demoted to `Working`.
    /// Free-text fields are bounded to [`MAX_TEXT_CHARS`].
    #[must_use]
    pub fn bounded(
        generated_at: jiff::Timestamp,
        root: String,
        nodes: Vec<AgentTreeNode>,
        max_nodes: usize,
    ) -> Self {
        let total = nodes.len();
        let mut by_parent: HashMap<Option<String>, Vec<usize>> = HashMap::new();
        for (i, node) in nodes.iter().enumerate() {
            by_parent.entry(node.parent.clone()).or_default().push(i);
        }
        let start = nodes
            .iter()
            .position(|n| n.id == root && n.parent.is_none());
        let mut order: Vec<usize> = Vec::new();
        let mut seen: HashSet<usize> = HashSet::new();
        let mut stack: Vec<usize> = start.into_iter().collect();
        while let Some(i) = stack.pop() {
            if !seen.insert(i) {
                continue;
            }
            order.push(i);
            if let Some(children) = by_parent.get(&Some(nodes[i].id.clone())) {
                stack.extend(children.iter().rev().copied());
            }
        }
        order.truncate(max_nodes.max(1));
        let kept: HashSet<usize> = order.iter().copied().collect();
        let kept_ids: HashSet<&str> = order.iter().map(|&i| nodes[i].id.as_str()).collect();
        let mut dropped: HashMap<String, (u32, bool)> = HashMap::new();
        for (i, node) in nodes.iter().enumerate() {
            let Some(parent) = node.parent.as_deref() else {
                continue;
            };
            if kept.contains(&i) || !kept_ids.contains(parent) {
                continue;
            }
            let entry = dropped.entry(parent.to_owned()).or_default();
            entry.0 = entry.0.saturating_add(1);
            let blocking = node.kind == AgentRunKind::Foreground
                && !node.activity.is_terminal()
                && !matches!(node.activity, AgentActivity::WaitingOnChildren { .. });
            entry.1 |= blocking;
        }
        let mut slots: Vec<Option<AgentTreeNode>> = nodes.into_iter().map(Some).collect();
        let mut out: Vec<AgentTreeNode> = order.iter().filter_map(|&i| slots[i].take()).collect();
        for node in &mut out {
            node.bound_texts();
            if let Some(&(count, blocking)) = dropped.get(&node.id) {
                node.omitted_children = count;
                if blocking && matches!(node.activity, AgentActivity::WaitingOnChildren { .. }) {
                    node.activity = AgentActivity::Working;
                }
            }
        }
        let omitted = total - out.len();
        Self {
            schema_version: AGENT_TREE_SCHEMA,
            generated_at,
            root,
            nodes: out,
            truncated: omitted > 0,
            omitted,
        }
    }

    /// Checks structural invariants: supported schema, unique ids, a single
    /// root (parentless, depth 0), existing parents, no cycles,
    /// `depth == parent depth + 1`, and that every `WaitingOnChildren` entry is
    /// a live (non-terminal) child of that node.
    ///
    /// A `WaitingOnChildren` list must be exactly the node's live direct
    /// children in the snapshot (no duplicates, none missing) and every
    /// foreground live child must itself be waiting; background and durable
    /// children are detached. `omitted_children` must be 0 in a non-truncated
    /// snapshot. In a truncated snapshot, listed ids absent from `nodes` are
    /// tolerated only up to the node's `omitted_children`.
    ///
    /// # Errors
    /// Returns the first violated invariant as an [`AgentTreeError`].
    pub fn validate(&self) -> Result<(), AgentTreeError> {
        if self.schema_version != AGENT_TREE_SCHEMA {
            return Err(AgentTreeError::UnsupportedSchema(self.schema_version));
        }
        let mut index: HashMap<&str, &AgentTreeNode> = HashMap::new();
        for node in &self.nodes {
            if index.insert(node.id.as_str(), node).is_some() {
                return Err(AgentTreeError::DuplicateId(node.id.clone()));
            }
        }
        let Some(root) = index.get(self.root.as_str()) else {
            return Err(AgentTreeError::InvalidRoot(format!(
                "root {} not in snapshot",
                self.root
            )));
        };
        if root.parent.is_some() || root.depth != 0 {
            return Err(AgentTreeError::InvalidRoot(format!(
                "root {} must have no parent and depth 0",
                self.root
            )));
        }
        for node in &self.nodes {
            match &node.parent {
                None if node.id != self.root => {
                    return Err(AgentTreeError::InvalidRoot(format!(
                        "second parentless node {}",
                        node.id
                    )));
                }
                Some(parent) if !index.contains_key(parent.as_str()) => {
                    return Err(AgentTreeError::Orphan {
                        id: node.id.clone(),
                        parent: parent.clone(),
                    });
                }
                _ => {}
            }
        }
        for node in &self.nodes {
            let mut current = node;
            let mut steps = 0usize;
            while let Some(parent) = current.parent.as_deref() {
                steps += 1;
                if steps > self.nodes.len() {
                    return Err(AgentTreeError::Cycle(node.id.clone()));
                }
                match index.get(parent) {
                    Some(next) => current = next,
                    None => break,
                }
            }
        }
        for node in &self.nodes {
            let Some(parent) = node.parent.as_deref().and_then(|p| index.get(p)) else {
                continue;
            };
            let expected = parent.depth.saturating_add(1);
            if node.depth != expected {
                return Err(AgentTreeError::WrongDepth {
                    id: node.id.clone(),
                    expected,
                    found: node.depth,
                });
            }
        }
        let mut live_children: HashMap<&str, Vec<&AgentTreeNode>> = HashMap::new();
        for node in &self.nodes {
            if node.omitted_children > 0 && !self.truncated {
                return Err(AgentTreeError::BadOmission(node.id.clone()));
            }
            if let Some(parent) = node.parent.as_deref()
                && !node.activity.is_terminal()
            {
                live_children.entry(parent).or_default().push(node);
            }
        }
        for node in &self.nodes {
            let AgentActivity::WaitingOnChildren { children } = &node.activity else {
                continue;
            };
            if children.is_empty() && !self.truncated {
                return Err(AgentTreeError::EmptyWaiting(node.id.clone()));
            }
            let mut listed: HashSet<&str> = HashSet::new();
            let mut missing = 0u32;
            for child in children {
                if !listed.insert(child.as_str()) {
                    return Err(AgentTreeError::DuplicateWaitingChild {
                        id: node.id.clone(),
                        child: child.clone(),
                    });
                }
                match index.get(child.as_str()) {
                    Some(c)
                        if c.parent.as_deref() == Some(node.id.as_str())
                            && !c.activity.is_terminal() => {}
                    None if self.truncated => missing = missing.saturating_add(1),
                    _ => {
                        return Err(AgentTreeError::BadWaitingChild {
                            id: node.id.clone(),
                            child: child.clone(),
                        });
                    }
                }
            }
            if missing > node.omitted_children {
                return Err(AgentTreeError::BadOmission(node.id.clone()));
            }
            for live in live_children.get(node.id.as_str()).into_iter().flatten() {
                if !listed.contains(live.id.as_str()) {
                    return Err(AgentTreeError::UnlistedLiveChild {
                        id: node.id.clone(),
                        child: live.id.clone(),
                    });
                }
                if live.kind == AgentRunKind::Foreground
                    && !matches!(live.activity, AgentActivity::WaitingOnChildren { .. })
                {
                    return Err(AgentTreeError::ForegroundChildNotWaiting {
                        id: node.id.clone(),
                        child: live.id.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

/// Terminal outcome of an agent, as input to [`derive_activity`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentTerminal {
    /// Finished successfully.
    Completed,
    /// Finished with an error.
    Failed(String),
    /// Stopped on request.
    Cancelled,
}

/// Raw per-node facts from which [`derive_activity`] computes activity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeFacts {
    /// Session id.
    pub id: String,
    /// Parent session id; `None` for the root. Unknown parents are treated as
    /// "no parent".
    pub parent: Option<String>,
    /// True while the node's own turn is running.
    pub own_turn_active: bool,
    /// `Some` once the node has finished.
    pub terminal: Option<AgentTerminal>,
    /// True when the node runs detached (background/durable), so its parent
    /// need not be considered blocked on it.
    pub detached: bool,
}

/// Derives each node's [`AgentActivity`] bottom-up. Returns one entry per input
/// fact, in input order.
///
/// Rules, applied per node after its children are resolved:
/// 1. A terminal node reports its terminal state (`Completed`, `Failed`
///    with the reason bounded to [`MAX_TEXT_CHARS`], `Cancelled`), whatever its
///    children do.
/// 2. Own turn active: `Working`.
/// 3. Own turn ended and no live (non-terminal) child: `Idle`.
/// 4. Own turn ended and every live child is `WaitingOnChildren` or
///    `detached`: `WaitingOnChildren` listing all live children.
/// 5. Otherwise some live non-detached child is not waiting: `Working` if any
///    such child is `Working`, else (they are all `Idle`) `Idle`.
///
/// Parent cycles in the input do not hang: a node reached again while it is
/// being resolved counts as `Working`. Recursion depth equals tree depth.
#[must_use]
pub fn derive_activity(facts: &[NodeFacts]) -> Vec<(String, AgentActivity)> {
    let mut first: HashMap<&str, usize> = HashMap::new();
    for (i, f) in facts.iter().enumerate() {
        first.entry(f.id.as_str()).or_insert(i);
    }
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); facts.len()];
    for (i, f) in facts.iter().enumerate() {
        if let Some(&p) = f.parent.as_deref().and_then(|p| first.get(p))
            && p != i
            && first.get(f.id.as_str()) == Some(&i)
        {
            children[p].push(i);
        }
    }
    let mut memo: Vec<Option<AgentActivity>> = vec![None; facts.len()];
    let mut visiting = vec![false; facts.len()];
    for i in 0..facts.len() {
        resolve(i, facts, &children, &mut memo, &mut visiting);
    }
    facts
        .iter()
        .zip(memo)
        .map(|(f, a)| (f.id.clone(), a.unwrap_or(AgentActivity::Working)))
        .collect()
}

fn resolve(
    i: usize,
    facts: &[NodeFacts],
    children: &[Vec<usize>],
    memo: &mut [Option<AgentActivity>],
    visiting: &mut [bool],
) -> AgentActivity {
    if let Some(done) = &memo[i] {
        return done.clone();
    }
    if visiting[i] {
        return AgentActivity::Working;
    }
    visiting[i] = true;
    let fact = &facts[i];
    let result = if let Some(t) = &fact.terminal {
        for &c in &children[i] {
            resolve(c, facts, children, memo, visiting);
        }
        match t {
            AgentTerminal::Completed => AgentActivity::Completed,
            AgentTerminal::Failed(r) => AgentActivity::Failed {
                reason: bound_text(r, MAX_TEXT_CHARS),
            },
            AgentTerminal::Cancelled => AgentActivity::Cancelled,
        }
    } else {
        let mut live: Vec<(usize, AgentActivity)> = Vec::new();
        for &c in &children[i] {
            let act = resolve(c, facts, children, memo, visiting);
            if facts[c].terminal.is_none() {
                live.push((c, act));
            }
        }
        if fact.own_turn_active {
            AgentActivity::Working
        } else if live.is_empty() {
            AgentActivity::Idle
        } else {
            let satisfied = |c: usize, a: &AgentActivity| {
                facts[c].detached || matches!(a, AgentActivity::WaitingOnChildren { .. })
            };
            if live.iter().all(|(c, a)| satisfied(*c, a)) {
                AgentActivity::WaitingOnChildren {
                    children: live.iter().map(|(c, _)| facts[*c].id.clone()).collect(),
                }
            } else if live
                .iter()
                .any(|(c, a)| !satisfied(*c, a) && matches!(a, AgentActivity::Working))
            {
                AgentActivity::Working
            } else {
                AgentActivity::Idle
            }
        }
    };
    visiting[i] = false;
    memo[i] = Some(result.clone());
    result
}

/// Glyph set and connectors for the renderers.
struct Style {
    working: &'static str,
    waiting: &'static str,
    idle: &'static str,
    completed: &'static str,
    failed: &'static str,
    cancelled: &'static str,
    branch: &'static str,
    last: &'static str,
    pipe: &'static str,
    blank: &'static str,
    ellipsis: &'static str,
}

const UNICODE: Style = Style {
    working: "⏳",
    waiting: "⏸",
    idle: "💤",
    completed: "✅",
    failed: "❌",
    cancelled: "⛔",
    branch: "├─ ",
    last: "└─ ",
    pipe: "│  ",
    blank: "   ",
    ellipsis: "…",
};

const ASCII: Style = Style {
    working: "[*]",
    waiting: "[~]",
    idle: "[z]",
    completed: "[+]",
    failed: "[!]",
    cancelled: "[x]",
    branch: "|- ",
    last: "`- ",
    pipe: "|  ",
    blank: "   ",
    ellipsis: "...",
};

/// Renders the tree as plain text with emoji glyphs, one line per node:
/// `<glyph> <role> <short id> [<kind>] <detail>`, plus a final
/// `… N more agents not shown` line when the snapshot is truncated.
#[must_use]
pub fn render_tree_text(snapshot: &AgentTreeSnapshot) -> String {
    render_tree(snapshot, &UNICODE)
}

/// Like [`render_tree_text`] but ASCII-only, for channels that cannot show
/// emoji or box-drawing characters.
#[must_use]
pub fn render_tree_ascii(snapshot: &AgentTreeSnapshot) -> String {
    render_tree(snapshot, &ASCII)
}

fn one_line(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    bound_text(flat.trim(), max)
}

fn render_tree(snapshot: &AgentTreeSnapshot, style: &Style) -> String {
    let mut children: HashMap<&str, Vec<&AgentTreeNode>> = HashMap::new();
    for node in &snapshot.nodes {
        if let Some(p) = node.parent.as_deref() {
            children.entry(p).or_default().push(node);
        }
    }
    let mut out = String::new();
    let mut seen: HashSet<&str> = HashSet::new();
    // (node, prefix for its own line, prefix for its children)
    let mut stack: Vec<(&AgentTreeNode, String, String)> = snapshot
        .nodes
        .iter()
        .filter(|n| n.id == snapshot.root)
        .take(1)
        .map(|n| (n, String::new(), String::new()))
        .collect();
    while let Some((node, line_prefix, child_prefix)) = stack.pop() {
        if !seen.insert(node.id.as_str()) {
            continue;
        }
        out.push_str(&line_prefix);
        out.push_str(&node_line(node, style));
        out.push('\n');
        if let Some(kids) = children.get(node.id.as_str()) {
            let n = kids.len();
            for (k, kid) in kids.iter().enumerate().rev() {
                let (conn, cont) = if k + 1 == n {
                    (style.last, style.blank)
                } else {
                    (style.branch, style.pipe)
                };
                stack.push((
                    kid,
                    format!("{child_prefix}{conn}"),
                    format!("{child_prefix}{cont}"),
                ));
            }
        }
    }
    if snapshot.truncated {
        let noun = if snapshot.omitted == 1 {
            "agent"
        } else {
            "agents"
        };
        out.push_str(&format!(
            "{} {} more {noun} not shown\n",
            style.ellipsis, snapshot.omitted
        ));
    }
    out
}

fn node_line(node: &AgentTreeNode, style: &Style) -> String {
    let glyph = match &node.activity {
        AgentActivity::Working => style.working,
        AgentActivity::WaitingOnChildren { .. } => style.waiting,
        AgentActivity::Idle => style.idle,
        AgentActivity::Completed => style.completed,
        AgentActivity::Failed { .. } => style.failed,
        AgentActivity::Cancelled => style.cancelled,
    };
    let short: String = node.id.chars().take(SHORT_ID_CHARS).collect();
    let mut line = format!(
        "{glyph} {} {short} [{}]",
        one_line(&node.role, 40),
        node.kind.label()
    );
    match &node.activity {
        AgentActivity::WaitingOnChildren { children } => {
            line.push_str(&format!(" waiting on {}", children.len()));
        }
        AgentActivity::Failed { reason } => {
            line.push_str(&format!(
                " failed: {}",
                one_line(reason, RENDER_MILESTONE_CHARS)
            ));
        }
        _ => {}
    }
    if let Some(m) = &node.last_milestone {
        let m = one_line(m, RENDER_MILESTONE_CHARS);
        if !m.is_empty() {
            line.push_str(" - ");
            line.push_str(&m);
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn ts() -> jiff::Timestamp {
        jiff::Timestamp::constant(1_700_000_000, 0)
    }

    fn node(id: &str, parent: Option<&str>, depth: u32, activity: AgentActivity) -> AgentTreeNode {
        AgentTreeNode {
            id: id.to_owned(),
            parent: parent.map(str::to_owned),
            depth,
            role: format!("role-{id}"),
            kind: AgentRunKind::Foreground,
            activity,
            task: None,
            started_at: None,
            job_work_id: None,
            last_milestone: None,
            tokens: None,
            tool_calls: None,
            model: None,
            omitted_children: 0,
        }
    }

    fn snap(nodes: Vec<AgentTreeNode>) -> AgentTreeSnapshot {
        AgentTreeSnapshot {
            schema_version: AGENT_TREE_SCHEMA,
            generated_at: ts(),
            root: "root".to_owned(),
            nodes,
            truncated: false,
            omitted: 0,
        }
    }

    fn waiting(ids: &[&str]) -> AgentActivity {
        AgentActivity::WaitingOnChildren {
            children: ids.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn valid_nodes() -> Vec<AgentTreeNode> {
        vec![
            node("root", None, 0, waiting(&["a", "b"])),
            bg(node("a", Some("root"), 1, AgentActivity::Working)),
            bg(node("b", Some("root"), 1, AgentActivity::Idle)),
        ]
    }

    fn bg(mut n: AgentTreeNode) -> AgentTreeNode {
        n.kind = AgentRunKind::Background;
        n
    }

    #[test]
    fn validate_rejects_omitted_working_foreground_child() {
        // root lists only the background child; foreground "fg" works.
        let nodes = vec![
            node("root", None, 0, waiting(&["bg"])),
            bg(node("bg", Some("root"), 1, AgentActivity::Working)),
            node("fg", Some("root"), 1, AgentActivity::Working),
        ];
        assert!(is_err_matching(snap(nodes).validate(), |e| matches!(
            e,
            AgentTreeError::UnlistedLiveChild { child, .. } if child == "fg"
        )));
        // Listed, but working foreground child.
        let nodes = vec![
            node("root", None, 0, waiting(&["bg", "fg"])),
            bg(node("bg", Some("root"), 1, AgentActivity::Working)),
            node("fg", Some("root"), 1, AgentActivity::Working),
        ];
        assert!(is_err_matching(snap(nodes).validate(), |e| matches!(
            e,
            AgentTreeError::ForegroundChildNotWaiting { child, .. } if child == "fg"
        )));
    }

    #[test]
    fn validate_rejects_duplicate_waiting_child() {
        let mut nodes = valid_nodes();
        nodes[0].activity = waiting(&["a", "b", "a"]);
        assert!(is_err_matching(snap(nodes).validate(), |e| matches!(
            e,
            AgentTreeError::DuplicateWaitingChild { child, .. } if child == "a"
        )));
    }

    #[test]
    fn validate_omission_semantics() {
        // Non-truncated: omitted_children must be 0.
        let mut nodes = valid_nodes();
        nodes[0].omitted_children = 1;
        assert!(is_err_matching(snap(nodes).validate(), |e| matches!(
            e,
            AgentTreeError::BadOmission(_)
        )));
        // Truncated: a listed-but-absent child needs omitted_children cover.
        let mk = |omitted| {
            let mut root = node("root", None, 0, waiting(&["a", "gone"]));
            root.omitted_children = omitted;
            let mut s = snap(vec![
                root,
                bg(node("a", Some("root"), 1, AgentActivity::Working)),
            ]);
            s.truncated = true;
            s.omitted = 1;
            s
        };
        assert!(mk(1).validate().is_ok());
        assert!(is_err_matching(mk(0).validate(), |e| matches!(
            e,
            AgentTreeError::BadOmission(_)
        )));
    }

    #[test]
    fn bounded_demotes_waiting_when_foreground_child_omitted() -> TestResult {
        let nodes = vec![
            node("root", None, 0, waiting(&["fg", "bg"])),
            bg(node("bg", Some("root"), 1, AgentActivity::Working)),
            node("fg", Some("root"), 1, AgentActivity::Working),
        ];
        let s = AgentTreeSnapshot::bounded(ts(), "root".to_owned(), nodes, 2);
        assert!(s.truncated);
        assert_eq!(s.nodes[0].omitted_children, 1);
        assert_eq!(s.nodes[0].activity, AgentActivity::Working);
        s.validate().map_err(ctx("demoted snapshot"))?;
        // Omitted background child keeps the waiting state.
        let nodes = vec![
            node("root", None, 0, waiting(&["fg", "bg"])),
            node("fg", Some("root"), 1, waiting(&["x"])),
            bg(node("x", Some("fg"), 2, AgentActivity::Working)),
            bg(node("bg", Some("root"), 1, AgentActivity::Working)),
        ];
        let s = AgentTreeSnapshot::bounded(ts(), "root".to_owned(), nodes, 3);
        assert_eq!(s.nodes[0].omitted_children, 1);
        assert!(matches!(
            s.nodes[0].activity,
            AgentActivity::WaitingOnChildren { .. }
        ));
        s.validate().map_err(ctx("kept waiting"))
    }

    #[test]
    fn node_without_omitted_children_deserializes_to_zero() -> TestResult {
        let n = node("x", None, 0, AgentActivity::Idle);
        let mut v = serde_json::to_value(&n).map_err(ctx("ser"))?;
        assert!(v.get("omitted_children").is_none());
        v.as_object_mut().map(|o| o.remove("omitted_children"));
        let back: AgentTreeNode = serde_json::from_value(v).map_err(ctx("de"))?;
        assert_eq!(back.omitted_children, 0);
        Ok(())
    }

    fn is_err_matching(r: Result<(), AgentTreeError>, f: impl Fn(&AgentTreeError) -> bool) -> bool {
        matches!(r, Err(ref e) if f(e))
    }

    #[test]
    fn valid_snapshot_passes() -> TestResult {
        // validate checks references only; it does not re-derive activity.
        snap(valid_nodes())
            .validate()
            .map_err(ctx("valid snapshot"))
    }

    #[test]
    fn validate_rejects_duplicate_id() {
        let mut nodes = valid_nodes();
        nodes.push(node("a", Some("root"), 1, AgentActivity::Idle));
        assert!(is_err_matching(snap(nodes).validate(), |e| matches!(
            e,
            AgentTreeError::DuplicateId(id) if id == "a"
        )));
    }

    #[test]
    fn validate_rejects_missing_root_and_second_root() {
        let mut s = snap(valid_nodes());
        s.root = "nope".to_owned();
        assert!(is_err_matching(s.validate(), |e| matches!(
            e,
            AgentTreeError::InvalidRoot(_)
        )));
        let mut nodes = valid_nodes();
        nodes.push(node("x", None, 0, AgentActivity::Idle));
        assert!(is_err_matching(snap(nodes).validate(), |e| matches!(
            e,
            AgentTreeError::InvalidRoot(_)
        )));
    }

    #[test]
    fn validate_rejects_orphan() {
        let mut nodes = valid_nodes();
        nodes.push(node("c", Some("ghost"), 1, AgentActivity::Idle));
        assert!(is_err_matching(snap(nodes).validate(), |e| matches!(
            e,
            AgentTreeError::Orphan { id, parent } if id == "c" && parent == "ghost"
        )));
    }

    #[test]
    fn validate_rejects_wrong_depth() {
        let mut nodes = valid_nodes();
        nodes[1].depth = 3;
        assert!(is_err_matching(snap(nodes).validate(), |e| matches!(
            e,
            AgentTreeError::WrongDepth { id, expected: 1, found: 3 } if id == "a"
        )));
    }

    #[test]
    fn validate_rejects_cycle() {
        let nodes = vec![
            node("root", None, 0, AgentActivity::Idle),
            node("x", Some("y"), 1, AgentActivity::Idle),
            node("y", Some("x"), 2, AgentActivity::Idle),
        ];
        assert!(is_err_matching(snap(nodes).validate(), |e| matches!(
            e,
            AgentTreeError::Cycle(_)
        )));
    }

    #[test]
    fn validate_rejects_bad_waiting_children() {
        // Unknown child.
        let mut nodes = valid_nodes();
        nodes[0].activity = waiting(&["zzz"]);
        assert!(is_err_matching(snap(nodes).validate(), |e| matches!(
            e,
            AgentTreeError::BadWaitingChild { child, .. } if child == "zzz"
        )));
        // Terminal child is not live.
        let mut nodes = valid_nodes();
        nodes[2].activity = AgentActivity::Completed;
        assert!(is_err_matching(snap(nodes).validate(), |e| matches!(
            e,
            AgentTreeError::BadWaitingChild { child, .. } if child == "b"
        )));
        // Grandchild is not a child of this node.
        let nodes = vec![
            node("root", None, 0, waiting(&["g"])),
            node("a", Some("root"), 1, AgentActivity::Working),
            node("g", Some("a"), 2, AgentActivity::Working),
        ];
        assert!(is_err_matching(snap(nodes).validate(), |e| matches!(
            e,
            AgentTreeError::BadWaitingChild { child, .. } if child == "g"
        )));
        // Empty list.
        let nodes = vec![node("root", None, 0, waiting(&[]))];
        assert!(is_err_matching(snap(nodes).validate(), |e| matches!(
            e,
            AgentTreeError::EmptyWaiting(_)
        )));
    }

    #[test]
    fn validate_rejects_unsupported_schema() {
        let mut s = snap(valid_nodes());
        s.schema_version = 99;
        assert_eq!(s.validate(), Err(AgentTreeError::UnsupportedSchema(99)));
    }

    fn wide_tree() -> Vec<AgentTreeNode> {
        // Input deliberately not in pre-order.
        vec![
            node("root", None, 0, AgentActivity::Working),
            node("a", Some("root"), 1, AgentActivity::Working),
            node("b", Some("root"), 1, AgentActivity::Working),
            node("b1", Some("b"), 2, AgentActivity::Working),
            node("a1", Some("a"), 2, AgentActivity::Working),
            node("a2", Some("a"), 2, AgentActivity::Working),
        ]
    }

    #[test]
    fn bounded_orders_preorder_and_keeps_connected_prefix() -> TestResult {
        let s = AgentTreeSnapshot::bounded(ts(), "root".to_owned(), wide_tree(), 4);
        let ids: Vec<&str> = s.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["root", "a", "a1", "a2"]);
        assert!(s.truncated);
        assert_eq!(s.omitted, 2);
        s.validate().map_err(ctx("truncated snapshot"))?;
        Ok(())
    }

    #[test]
    fn bounded_without_truncation_and_with_unreachable_nodes() {
        let s = AgentTreeSnapshot::bounded(ts(), "root".to_owned(), wide_tree(), 100);
        assert!(!s.truncated);
        assert_eq!(s.omitted, 0);
        assert_eq!(s.nodes.len(), 6);
        let mut nodes = wide_tree();
        nodes.push(node("lost", Some("ghost"), 1, AgentActivity::Idle));
        let s = AgentTreeSnapshot::bounded(ts(), "root".to_owned(), nodes, 100);
        assert_eq!(s.nodes.len(), 6);
        assert_eq!(s.omitted, 1);
        assert!(s.truncated);
    }

    #[test]
    fn bounded_always_keeps_root() {
        let s = AgentTreeSnapshot::bounded(ts(), "root".to_owned(), wide_tree(), 0);
        assert_eq!(s.nodes.len(), 1);
        assert_eq!(s.nodes[0].id, "root");
        assert_eq!(s.omitted, 5);
    }

    #[test]
    fn bound_text_respects_char_boundaries() {
        assert_eq!(bound_text("short", 10), "short");
        assert_eq!(bound_text("abcdef", 6), "abcdef");
        assert_eq!(bound_text("abcdefg", 6), "abcde…");
        let cut = bound_text("äöü😀日本語テキスト", 5);
        assert_eq!(cut, "äöü😀…");
        assert_eq!(cut.chars().count(), 5);
        assert_eq!(bound_text("abc", 0), "");
    }

    #[test]
    fn bounded_caps_free_text_fields() {
        let long = "日".repeat(MAX_TEXT_CHARS + 50);
        let mut nodes = wide_tree();
        nodes[0].task = Some(long.clone());
        nodes[0].last_milestone = Some(long.clone());
        nodes[1].activity = AgentActivity::Failed { reason: long };
        let s = AgentTreeSnapshot::bounded(ts(), "root".to_owned(), nodes, 100);
        assert_eq!(
            s.nodes[0].task.as_deref().map(|t| t.chars().count()),
            Some(MAX_TEXT_CHARS)
        );
        assert!(
            s.nodes[0]
                .last_milestone
                .as_deref()
                .is_some_and(|t| t.ends_with('…'))
        );
        assert!(matches!(
            &s.nodes[1].activity,
            AgentActivity::Failed { reason } if reason.chars().count() == MAX_TEXT_CHARS
        ));
    }

    fn fact(id: &str, parent: Option<&str>, own: bool) -> NodeFacts {
        NodeFacts {
            id: id.to_owned(),
            parent: parent.map(str::to_owned),
            own_turn_active: own,
            terminal: None,
            detached: false,
        }
    }

    fn detached(mut f: NodeFacts) -> NodeFacts {
        f.detached = true;
        f
    }

    fn act(out: &[(String, AgentActivity)], id: &str) -> Result<AgentActivity, TestError> {
        out.iter()
            .find(|(i, _)| i == id)
            .map(|(_, a)| a.clone())
            .ok_or(TestError::Missing("derived activity"))
    }

    #[test]
    fn derive_no_children_is_idle_or_working() -> TestResult {
        let out = derive_activity(&[fact("r", None, false), fact("s", None, true)]);
        assert_eq!(act(&out, "r")?, AgentActivity::Idle);
        assert_eq!(act(&out, "s")?, AgentActivity::Working);
        Ok(())
    }

    #[test]
    fn derive_working_child_prevents_waiting() -> TestResult {
        let out = derive_activity(&[fact("r", None, false), fact("c", Some("r"), true)]);
        assert_eq!(act(&out, "r")?, AgentActivity::Working);
        Ok(())
    }

    #[test]
    fn derive_three_levels_all_waiting() -> TestResult {
        let out = derive_activity(&[
            fact("r", None, false),
            fact("a", Some("r"), false),
            fact("b", Some("r"), false),
            fact("a1", Some("a"), false),
            fact("b1", Some("b"), false),
            detached(fact("a1x", Some("a1"), true)),
            detached(fact("b1x", Some("b1"), true)),
        ]);
        assert_eq!(act(&out, "a1")?, waiting(&["a1x"]));
        assert_eq!(act(&out, "a")?, waiting(&["a1"]));
        assert_eq!(act(&out, "r")?, waiting(&["a", "b"]));
        Ok(())
    }

    #[test]
    fn derive_working_grandchild_propagates_up() -> TestResult {
        let out = derive_activity(&[
            fact("r", None, false),
            fact("a", Some("r"), false),
            fact("a1", Some("a"), true),
        ]);
        assert_eq!(act(&out, "a")?, AgentActivity::Working);
        assert_eq!(act(&out, "r")?, AgentActivity::Working);
        Ok(())
    }

    #[test]
    fn derive_mixed_detached_and_foreground() -> TestResult {
        let bg = detached(fact("bg", Some("r"), true));
        let out = derive_activity(&[fact("r", None, false), bg.clone()]);
        assert_eq!(act(&out, "r")?, waiting(&["bg"]));
        let out = derive_activity(&[fact("r", None, false), bg, fact("fg", Some("r"), true)]);
        assert_eq!(act(&out, "r")?, AgentActivity::Working);
        Ok(())
    }

    #[test]
    fn derive_idle_foreground_child_gives_idle_parent() -> TestResult {
        let out = derive_activity(&[fact("r", None, false), fact("c", Some("r"), false)]);
        assert_eq!(act(&out, "r")?, AgentActivity::Idle);
        Ok(())
    }

    #[test]
    fn derive_terminal_passthrough_and_ignored_for_liveness() -> TestResult {
        let mut done = fact("done", Some("r"), false);
        done.terminal = Some(AgentTerminal::Completed);
        let mut bad = fact("bad", Some("r"), true);
        bad.terminal = Some(AgentTerminal::Failed("boom".to_owned()));
        let mut gone = fact("gone", Some("r"), false);
        gone.terminal = Some(AgentTerminal::Cancelled);
        let out = derive_activity(&[fact("r", None, false), done, bad, gone]);
        assert_eq!(act(&out, "done")?, AgentActivity::Completed);
        assert_eq!(
            act(&out, "bad")?,
            AgentActivity::Failed {
                reason: "boom".to_owned()
            }
        );
        assert_eq!(act(&out, "gone")?, AgentActivity::Cancelled);
        // Only terminal children: the parent has no live children.
        assert_eq!(act(&out, "r")?, AgentActivity::Idle);
        Ok(())
    }

    #[test]
    fn derive_survives_parent_cycle() {
        let out = derive_activity(&[fact("x", Some("y"), false), fact("y", Some("x"), false)]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn derived_activity_validates_in_a_snapshot() -> TestResult {
        let facts = [
            fact("root", None, false),
            fact("a", Some("root"), false),
            detached(fact("a1", Some("a"), true)),
        ];
        let derived = derive_activity(&facts);
        let nodes = vec![
            node("root", None, 0, act(&derived, "root")?),
            node("a", Some("root"), 1, act(&derived, "a")?),
            bg(node("a1", Some("a"), 2, act(&derived, "a1")?)),
        ];
        snap(nodes).validate().map_err(ctx("derived snapshot"))
    }

    fn render_fixture() -> AgentTreeSnapshot {
        let mut root = node("rootsession1", None, 0, waiting(&["alpha000", "beta0000"]));
        root.role = "uia".to_owned();
        let mut a = node("alpha000", Some("rootsession1"), 1, AgentActivity::Working);
        a.role = "coder".to_owned();
        a.last_milestone = Some("editing main.rs".to_owned());
        let mut a1 = node("alpha111", Some("alpha000"), 2, AgentActivity::Completed);
        a1.role = "tester".to_owned();
        a1.kind = AgentRunKind::Background;
        let failed = AgentActivity::Failed {
            reason: "out of budget".to_owned(),
        };
        let mut b = node("beta0000", Some("rootsession1"), 1, failed);
        b.role = "reviewer".to_owned();
        b.kind = AgentRunKind::Durable;
        let mut s = snap(vec![root, a, a1, b]);
        s.root = "rootsession1".to_owned();
        s
    }

    #[test]
    fn render_text_golden() {
        let expected = "\
⏸ uia rootsess [foreground] waiting on 2
├─ ⏳ coder alpha000 [foreground] - editing main.rs
│  └─ ✅ tester alpha111 [background]
└─ ❌ reviewer beta0000 [durable] failed: out of budget
";
        assert_eq!(render_tree_text(&render_fixture()), expected);
    }

    #[test]
    fn render_ascii_golden_with_truncation_line() {
        let mut s = render_fixture();
        s.truncated = true;
        s.omitted = 3;
        let expected = "\
[~] uia rootsess [foreground] waiting on 2
|- [*] coder alpha000 [foreground] - editing main.rs
|  `- [+] tester alpha111 [background]
`- [!] reviewer beta0000 [durable] failed: out of budget
... 3 more agents not shown
";
        let out = render_tree_ascii(&s);
        assert_eq!(out, expected);
        assert!(out.is_ascii());
        assert!(render_tree_text(&s).ends_with("… 3 more agents not shown\n"));
    }

    #[test]
    fn render_flattens_and_bounds_milestone() {
        let mut s = snap(vec![node("root", None, 0, AgentActivity::Working)]);
        s.nodes[0].last_milestone = Some(format!("line1\nline2 {}", "ä".repeat(100)));
        let out = render_tree_text(&s);
        assert_eq!(out.lines().count(), 1);
        let milestone = out.rsplit(" - ").next().unwrap_or_default().trim_end();
        assert_eq!(milestone.chars().count(), RENDER_MILESTONE_CHARS);
    }

    #[test]
    fn serde_round_trip() -> TestResult {
        let mut s = render_fixture();
        s.nodes[1].started_at = Some(ts());
        s.nodes[1].tokens = Some(1234);
        s.nodes[1].tool_calls = Some(7);
        s.nodes[1].model = Some("m".to_owned());
        let json = serde_json::to_string(&s).map_err(ctx("serialize"))?;
        let back: AgentTreeSnapshot = serde_json::from_str(&json).map_err(ctx("deserialize"))?;
        assert_eq!(back, s);
        assert!(json.contains("\"state\":\"waiting_on_children\""));
        assert!(json.contains("\"kind\":\"durable\""));
        Ok(())
    }

    #[test]
    fn serde_rejects_unknown_fields() -> TestResult {
        let s = snap(valid_nodes());
        let mut value = serde_json::to_value(&s).map_err(ctx("to_value"))?;
        value["surprise"] = serde_json::json!(1);
        assert!(serde_json::from_value::<AgentTreeSnapshot>(value).is_err());
        let mut value = serde_json::to_value(&s).map_err(ctx("to_value"))?;
        value["nodes"][0]["surprise"] = serde_json::json!(1);
        assert!(serde_json::from_value::<AgentTreeSnapshot>(value).is_err());
        let activity = serde_json::json!({"state": "failed", "reason": "x", "extra": 1});
        assert!(serde_json::from_value::<AgentActivity>(activity).is_err());
        Ok(())
    }
}
