//! Mission view of a session: agents, jobs, reasoning ring and finished tally.
//!
//! Fed by [`SessionState::observe`](crate::projection::SessionState) from the
//! frames of the shared session stream; projected into the UniFFI records of
//! `types.rs` for the phone's mission dock.

use std::collections::{HashMap, VecDeque};

use harw_protocol::TurnEvent;
use harw_protocol::items::TurnItem;
use harw_types::TurnId;

use crate::{
    FinishedCategory, MobileAgent, MobileAgentState, MobileJob, ReasoningKind, ReasoningLine,
};

pub(crate) const ROOT_ID: &str = "root";
const ROOT_ROLE: &str = "root";
const MAX_REASONING_LINES: usize = 200;
const MAX_LINE_BYTES: usize = 4096;
const MAX_AGENTS: usize = 64;
const MAX_JOBS: usize = 64;
const MAX_CALLS: usize = 256;
const MAX_FINISHED: usize = 32;
const MAX_LABEL_BYTES: usize = 64;
const OTHER_LABEL: &str = "other";

struct AgentRec {
    agent: MobileAgent,
    last_started_turn: Option<TurnId>,
    terminal: bool,
    outcome: Option<Outcome>,
}

struct JobRec {
    job: MobileJob,
    terminal: bool,
}

#[derive(Default)]
struct Tally {
    total: u32,
    ok: u32,
    failed: u32,
    cancelled: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Ok,
    Failed,
    Cancelled,
}

#[derive(Default)]
pub(crate) struct Mission {
    agents: Vec<AgentRec>,
    jobs: Vec<JobRec>,
    reasoning: VecDeque<ReasoningLine>,
    next_seq: u64,
    finished: Vec<(String, Tally)>,
    /// Open tool calls per call id: (agent id, tool name).
    calls: HashMap<String, (String, String)>,
    /// Recently started call ids (bounded), so durable replay of a call that
    /// already ended is not counted again.
    seen_calls: VecDeque<String>,
}

impl Mission {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Mark the root as waiting, or (when `waiting` is false) move it from
    /// Waiting back to Running; a Done/Failed root is never changed by `false`.
    pub fn root_waiting(&mut self, waiting: bool) {
        let root = self.agent_mut(ROOT_ID, None, ROOT_ROLE);
        if waiting {
            root.state = MobileAgentState::Waiting;
        } else if root.state == MobileAgentState::Waiting {
            root.state = MobileAgentState::Running;
        }
    }

    /// Set the root state directly (creating the record if missing); no tally.
    pub fn set_root_state(&mut self, state: MobileAgentState) {
        let root = self.agent_mut(ROOT_ID, None, ROOT_ROLE);
        root.state = state;
        if matches!(state, MobileAgentState::Done | MobileAgentState::Failed) {
            root.current_tool = None;
        }
    }

    /// Apply one turn event of the root agent.
    pub fn root_event(&mut self, event: &TurnEvent) {
        self.agent_event(ROOT_ID, None, ROOT_ROLE, event);
    }

    /// Apply one turn event of a child agent.
    pub fn child_event(
        &mut self,
        agent: &str,
        parent: Option<&str>,
        role: &str,
        event: &TurnEvent,
    ) {
        let parent = parent.unwrap_or(ROOT_ID);
        self.agent_event(agent, Some(parent), role, event);
    }

    fn agent_event(&mut self, id: &str, parent: Option<&str>, role: &str, event: &TurnEvent) {
        let is_root = id == ROOT_ID;
        // The root agent exists from the first event on, and sorts first.
        self.agent_mut(ROOT_ID, None, ROOT_ROLE);
        match event {
            TurnEvent::TurnStarted { turn_id, .. } => {
                // A new run of a finished agent counts (and finishes) again.
                let index = self.agent_index(id, parent, role);
                let rec = &mut self.agents[index];
                if rec.last_started_turn.as_ref() != Some(turn_id) {
                    rec.last_started_turn = Some(turn_id.clone());
                    rec.agent.started_ms = now_ms();
                }
                rec.terminal = false;
                rec.outcome = None;
                rec.agent.state = MobileAgentState::Running;
                rec.agent.current_tool = None;
            }
            TurnEvent::ReasoningDelta { text, .. } => {
                self.touch_running(id, parent, role);
                self.push_thinking(id, role, text);
            }
            TurnEvent::ToolCallRequested {
                call_id, tool_name, ..
            } => self.tool_start(id, parent, role, call_id.as_str(), tool_name),
            TurnEvent::ItemAdded {
                item: TurnItem::ToolCall(call),
                ..
            } => {
                if !self.calls.contains_key(call.call_id.as_str())
                    && !self
                        .seen_calls
                        .iter()
                        .any(|seen| seen == call.call_id.as_str())
                {
                    self.tool_start(id, parent, role, call.call_id.as_str(), &call.tool_name);
                }
            }
            TurnEvent::ToolCallCompleted {
                call_id, result, ..
            } => self.tool_end(id, call_id.as_str(), result.is_success()),
            TurnEvent::ItemAdded {
                item: TurnItem::ToolResult(result),
                ..
            } => self.tool_end(id, result.call_id.as_str(), result.result.is_success()),
            TurnEvent::TurnCompleted { .. } | TurnEvent::TurnAborted { .. } => {
                let cancelled = matches!(event, TurnEvent::TurnAborted { .. });
                if is_root {
                    let agent = self.agent_mut(id, parent, role);
                    agent.state = MobileAgentState::Done;
                    agent.current_tool = None;
                } else {
                    let outcome = if cancelled {
                        Outcome::Cancelled
                    } else {
                        Outcome::Ok
                    };
                    self.finish_agent(id, parent, role, outcome, false);
                }
                let (ok, text) = if cancelled {
                    (None, "cancelled")
                } else {
                    (Some(true), "done")
                };
                self.push_line(id, role, ReasoningKind::Finished, ok, text.into());
            }
            TurnEvent::TurnFailed { reason, .. } => {
                if is_root {
                    let agent = self.agent_mut(id, parent, role);
                    agent.state = MobileAgentState::Failed;
                    agent.current_tool = None;
                } else {
                    self.finish_agent(id, parent, role, Outcome::Failed, false);
                }
                self.push_line(
                    id,
                    role,
                    ReasoningKind::Finished,
                    Some(false),
                    reason.clone(),
                );
            }
            TurnEvent::ChildSpawned { child, role, .. } => {
                self.agent_mut(child.as_str(), Some(id), role);
            }
            TurnEvent::ChildProgress {
                child, tool_calls, ..
            } => {
                let agent = self.agent_mut(child.as_str(), Some(id), "agent");
                agent.tool_calls = agent.tool_calls.max(*tool_calls);
            }
            TurnEvent::ChildCompleted { child, outcome, .. } => {
                let outcome = match outcome.as_str() {
                    "completed" => Outcome::Ok,
                    "cancelled" => Outcome::Cancelled,
                    _ => Outcome::Failed,
                };
                self.finish_agent(child.as_str(), Some(id), "agent", outcome, false);
            }
            _ => {}
        }
    }

    /// Upsert a job by work id; terminal states are tallied once.
    pub fn job(&mut self, work_id: &str, kind: &str, state: &str, summary: Option<&str>) {
        let now = now_ms();
        let outcome = job_outcome(state);
        let position = self.jobs.iter().position(|rec| rec.job.work_id == work_id);
        let newly_terminal = match position {
            Some(index) => {
                let rec = &mut self.jobs[index];
                rec.job.kind = kind.into();
                rec.job.state = state.into();
                if summary.is_some() {
                    rec.job.summary = summary.map(str::to_owned);
                }
                rec.job.updated_ms = now;
                let fresh = outcome.is_some() && !rec.terminal;
                rec.terminal = outcome.is_some();
                fresh
            }
            None => {
                self.jobs.push(JobRec {
                    job: MobileJob {
                        work_id: work_id.into(),
                        kind: kind.into(),
                        state: state.into(),
                        summary: summary.map(str::to_owned),
                        updated_ms: now,
                    },
                    terminal: outcome.is_some(),
                });
                if self.jobs.len() > MAX_JOBS {
                    let old = self.jobs.iter().position(|rec| rec.terminal).unwrap_or(0);
                    self.jobs.remove(old);
                }
                outcome.is_some()
            }
        };
        if newly_terminal && let Some(outcome) = outcome {
            if kind == "agent" {
                // The orchestration event of a child is the authoritative
                // outcome of that agent; it is tallied once under its role.
                if self.agents.iter().any(|rec| rec.agent.id == work_id) {
                    self.finish_agent(work_id, None, "agent", outcome, true);
                }
            } else {
                self.tally(&format!("job:{kind}"), outcome);
            }
        }
    }

    pub fn agents(&self) -> Vec<MobileAgent> {
        self.agents.iter().map(|rec| rec.agent.clone()).collect()
    }

    pub fn jobs(&self) -> Vec<MobileJob> {
        self.jobs.iter().map(|rec| rec.job.clone()).collect()
    }

    pub fn reasoning(&self) -> Vec<ReasoningLine> {
        self.reasoning.iter().cloned().collect()
    }

    pub fn finished(&self) -> Vec<FinishedCategory> {
        self.finished
            .iter()
            .map(|(label, tally)| FinishedCategory {
                label: label.clone(),
                total: tally.total,
                ok: tally.ok,
                failed: tally.failed,
                cancelled: tally.cancelled,
            })
            .collect()
    }

    fn agent_mut(&mut self, id: &str, parent: Option<&str>, role: &str) -> &mut MobileAgent {
        let index = self.agent_index(id, parent, role);
        &mut self.agents[index].agent
    }

    fn agent_index(&mut self, id: &str, parent: Option<&str>, role: &str) -> usize {
        match self.agents.iter().position(|rec| rec.agent.id == id) {
            Some(index) => index,
            None => {
                self.agents.push(AgentRec {
                    agent: MobileAgent {
                        id: id.into(),
                        parent: parent.map(str::to_owned),
                        role: role.into(),
                        model: None,
                        state: MobileAgentState::Running,
                        current_tool: None,
                        started_ms: now_ms(),
                        tool_calls: 0,
                    },
                    last_started_turn: None,
                    terminal: false,
                    outcome: None,
                });
                if self.agents.len() > MAX_AGENTS {
                    let evictable = |rec: &AgentRec| rec.agent.id != ROOT_ID;
                    let old = self
                        .agents
                        .iter()
                        .position(|rec| rec.terminal && evictable(rec))
                        .or_else(|| self.agents.iter().position(evictable));
                    if let Some(old) = old {
                        self.agents.remove(old);
                    }
                }
                self.agents
                    .iter()
                    .position(|rec| rec.agent.id == id)
                    .unwrap_or(self.agents.len() - 1)
            }
        }
    }

    fn touch_running(&mut self, id: &str, parent: Option<&str>, role: &str) {
        let agent = self.agent_mut(id, parent, role);
        if agent.state == MobileAgentState::Waiting {
            agent.state = MobileAgentState::Running;
        }
    }

    fn tool_start(
        &mut self,
        id: &str,
        parent: Option<&str>,
        role: &str,
        call_id: &str,
        name: &str,
    ) {
        if self.calls.len() >= MAX_CALLS {
            self.calls.clear();
        }
        self.calls.insert(call_id.into(), (id.into(), name.into()));
        if self.seen_calls.len() >= MAX_CALLS {
            self.seen_calls.pop_front();
        }
        self.seen_calls.push_back(call_id.into());
        let agent = self.agent_mut(id, parent, role);
        agent.state = MobileAgentState::Running;
        agent.current_tool = Some(name.into());
        agent.tool_calls = agent.tool_calls.saturating_add(1);
        let role = agent.role.clone();
        self.push_line(id, &role, ReasoningKind::ToolStart, None, name.into());
    }

    fn tool_end(&mut self, id: &str, call_id: &str, ok: bool) {
        let Some((agent_id, name)) = self.calls.remove(call_id) else {
            return;
        };
        let role = {
            let agent = self.agent_mut(&agent_id, None, "agent");
            agent.current_tool = None;
            agent.role.clone()
        };
        self.push_line(id, &role, ReasoningKind::ToolEnd, Some(ok), name.clone());
        let outcome = if ok { Outcome::Ok } else { Outcome::Failed };
        self.tally(&format!("tool:{name}"), outcome);
    }

    /// Finish an agent once. An `authoritative` outcome (the orchestration
    /// event) may revise an earlier, non-authoritative one without recounting.
    fn finish_agent(
        &mut self,
        id: &str,
        parent: Option<&str>,
        role: &str,
        outcome: Outcome,
        authoritative: bool,
    ) {
        let index = self.agent_index(id, parent, role);
        let rec = &mut self.agents[index];
        let previous = if rec.terminal { rec.outcome } else { None };
        if rec.terminal && (!authoritative || previous == Some(outcome)) {
            return;
        }
        rec.terminal = true;
        rec.outcome = Some(outcome);
        rec.agent.current_tool = None;
        rec.agent.state = match outcome {
            Outcome::Ok => MobileAgentState::Done,
            Outcome::Failed | Outcome::Cancelled => MobileAgentState::Failed,
        };
        let label = rec.agent.role.clone();
        if let Some(previous) = previous {
            self.untally(&label, previous);
        }
        self.tally(&label, outcome);
    }

    fn untally(&mut self, label: &str, outcome: Outcome) {
        let label = bounded_label(label);
        let index = self
            .finished
            .iter()
            .position(|(name, _)| *name == label)
            .or_else(|| {
                self.finished
                    .iter()
                    .position(|(name, _)| name == OTHER_LABEL)
            });
        if let Some(index) = index {
            let tally = &mut self.finished[index].1;
            tally.total = tally.total.saturating_sub(1);
            match outcome {
                Outcome::Ok => tally.ok = tally.ok.saturating_sub(1),
                Outcome::Failed => tally.failed = tally.failed.saturating_sub(1),
                Outcome::Cancelled => tally.cancelled = tally.cancelled.saturating_sub(1),
            }
        }
    }

    fn tally(&mut self, label: &str, outcome: Outcome) {
        let mut label = bounded_label(label);
        if !self.finished.iter().any(|(name, _)| *name == label)
            && self.finished.len() + 1 >= MAX_FINISHED
            && label != OTHER_LABEL
        {
            // Keep the last slot for the overflow bucket.
            label = OTHER_LABEL.into();
        }
        let index = match self.finished.iter().position(|(name, _)| *name == label) {
            Some(index) => index,
            None => {
                self.finished.push((label, Tally::default()));
                self.finished.len() - 1
            }
        };
        let tally = &mut self.finished[index].1;
        tally.total = tally.total.saturating_add(1);
        match outcome {
            Outcome::Ok => tally.ok = tally.ok.saturating_add(1),
            Outcome::Failed => tally.failed = tally.failed.saturating_add(1),
            Outcome::Cancelled => tally.cancelled = tally.cancelled.saturating_add(1),
        }
    }

    fn push_thinking(&mut self, agent: &str, role: &str, text: &str) {
        if let Some(last) = self.reasoning.back_mut()
            && last.agent == agent
            && last.kind == ReasoningKind::Thinking
        {
            last.text.push_str(text);
            bound_line(&mut last.text);
            return;
        }
        self.push_line(agent, role, ReasoningKind::Thinking, None, text.into());
    }

    fn push_line(
        &mut self,
        agent: &str,
        role: &str,
        kind: ReasoningKind,
        ok: Option<bool>,
        mut text: String,
    ) {
        bound_line(&mut text);
        self.next_seq += 1;
        self.reasoning.push_back(ReasoningLine {
            seq: self.next_seq,
            agent: agent.into(),
            role: role.into(),
            kind,
            ok,
            text,
        });
        while self.reasoning.len() > MAX_REASONING_LINES {
            self.reasoning.pop_front();
        }
    }
}

/// Keep the newest `MAX_LINE_BYTES` of a line, cut on a char boundary.
fn bound_line(text: &mut String) {
    if text.len() > MAX_LINE_BYTES {
        let keep_from = text.ceil_char_boundary(text.len() - MAX_LINE_BYTES);
        text.drain(..keep_from);
    }
}

/// Cut a tally label to `MAX_LABEL_BYTES` on a char boundary.
fn bounded_label(label: &str) -> String {
    let end = label.floor_char_boundary(MAX_LABEL_BYTES);
    label[..end].to_owned()
}

fn job_outcome(state: &str) -> Option<Outcome> {
    match state.to_ascii_lowercase().as_str() {
        "completed" | "complete" | "done" | "succeeded" | "success" | "ok" => Some(Outcome::Ok),
        "failed" | "error" | "timed_out" | "timeout" | "dead" => Some(Outcome::Failed),
        "cancelled" | "canceled" | "aborted" | "stopped" => Some(Outcome::Cancelled),
        _ => None,
    }
}

fn now_ms() -> u64 {
    u64::try_from(jiff::Timestamp::now().as_millisecond()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use harw_protocol::items::{ToolCallItem, ToolCallResult, TurnItem};
    use harw_types::{ItemId, SessionId, ThreadId, ToolCallId, TurnId};

    use super::*;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    fn turn() -> TurnId {
        TurnId::from_str("t1")
    }

    fn thinking(text: &str) -> TurnEvent {
        TurnEvent::ReasoningDelta {
            turn_id: turn(),
            text: text.into(),
        }
    }

    fn requested(call: &str, tool: &str) -> TurnEvent {
        TurnEvent::ToolCallRequested {
            turn_id: turn(),
            call_id: ToolCallId::from_str(call),
            tool_name: tool.into(),
            arguments: serde_json::Value::Null,
        }
    }

    fn completed(call: &str, ok: bool) -> TurnEvent {
        TurnEvent::ToolCallCompleted {
            turn_id: turn(),
            call_id: ToolCallId::from_str(call),
            result: if ok {
                ToolCallResult::success(serde_json::Value::Null)
            } else {
                ToolCallResult::error("boom")
            },
            duration_ms: 1,
            placement: None,
        }
    }

    fn spawned(child: &str, role: &str) -> TurnEvent {
        TurnEvent::ChildSpawned {
            turn_id: turn(),
            child: SessionId::from_str(child),
            role: role.into(),
            question: None,
        }
    }

    fn child_done(child: &str, outcome: &str) -> TurnEvent {
        TurnEvent::ChildCompleted {
            turn_id: turn(),
            child: SessionId::from_str(child),
            outcome: outcome.into(),
            duration_ms: 1,
        }
    }

    fn find<'a>(finished: &'a [FinishedCategory], label: &str) -> TestResult<&'a FinishedCategory> {
        finished
            .iter()
            .find(|category| category.label == label)
            .ok_or_else(|| format!("missing category {label}").into())
    }

    #[test]
    fn new_turn_restarts_clock_but_replayed_start_does_not() -> TestResult {
        let mut mission = Mission::default();
        mission.set_root_state(MobileAgentState::Done);
        mission.agents[0].agent.started_ms = 0;
        let start = TurnEvent::TurnStarted {
            turn_id: TurnId::try_from_str("first")?,
            thread_id: ThreadId::try_from_str("thread")?,
        };
        mission.root_event(&start);
        assert_ne!(mission.agents()[0].started_ms, 0);

        mission.agents[0].agent.started_ms = 1;
        mission.root_event(&start);
        assert_eq!(mission.agents()[0].started_ms, 1);

        mission.root_event(&TurnEvent::TurnStarted {
            turn_id: TurnId::try_from_str("second")?,
            thread_id: ThreadId::try_from_str("thread")?,
        });
        assert_ne!(mission.agents()[0].started_ms, 1);
        Ok(())
    }

    #[test]
    fn thinking_coalesces_per_agent_and_interleaves_in_order() -> TestResult {
        let mut mission = Mission::default();
        mission.root_event(&thinking("a"));
        mission.root_event(&thinking("b"));
        mission.child_event("c1", Some("root"), "explorer", &thinking("x"));
        mission.root_event(&thinking("c"));
        let lines = mission.reasoning();
        let texts: Vec<&str> = lines.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(texts, ["ab", "x", "c"]);
        let agents: Vec<&str> = lines.iter().map(|line| line.agent.as_str()).collect();
        assert_eq!(agents, ["root", "c1", "root"]);
        assert!(lines.windows(2).all(|pair| pair[0].seq < pair[1].seq));
        assert!(
            lines
                .iter()
                .all(|line| line.kind == ReasoningKind::Thinking)
        );
        Ok(())
    }

    #[test]
    fn tool_lines_and_tally() -> TestResult {
        let mut mission = Mission::default();
        mission.root_event(&requested("k1", "shell"));
        let agent = mission.agents().remove(0);
        assert_eq!(agent.current_tool.as_deref(), Some("shell"));
        assert_eq!(agent.tool_calls, 1);
        mission.root_event(&completed("k1", true));
        mission.root_event(&requested("k2", "shell"));
        mission.root_event(&completed("k2", false));
        let agent = mission.agents().remove(0);
        assert_eq!(agent.current_tool, None);
        let kinds: Vec<(ReasoningKind, Option<bool>)> = mission
            .reasoning()
            .iter()
            .map(|line| (line.kind, line.ok))
            .collect();
        assert_eq!(
            kinds,
            [
                (ReasoningKind::ToolStart, None),
                (ReasoningKind::ToolEnd, Some(true)),
                (ReasoningKind::ToolStart, None),
                (ReasoningKind::ToolEnd, Some(false)),
            ]
        );
        let finished = mission.finished();
        let tool = find(&finished, "tool:shell")?;
        assert_eq!((tool.total, tool.ok, tool.failed), (2, 1, 1));
        Ok(())
    }

    #[test]
    fn tool_call_items_count_once_alongside_events() -> TestResult {
        let mut mission = Mission::default();
        let item = TurnItem::ToolCall(ToolCallItem {
            id: ItemId::from_str("i1"),
            call_id: ToolCallId::from_str("k1"),
            tool_name: "read".into(),
            arguments: serde_json::Value::Null,
        });
        let added = TurnEvent::ItemAdded {
            turn_id: turn(),
            item,
        };
        mission.root_event(&requested("k1", "read"));
        mission.root_event(&added);
        assert_eq!(mission.agents()[0].tool_calls, 1);
        mission.root_event(&completed("k1", true));
        mission.root_event(&added);
        assert_eq!(mission.agents()[0].tool_calls, 1);
        Ok(())
    }

    #[test]
    fn child_lifecycle_and_tally_by_role() -> TestResult {
        let mut mission = Mission::default();
        mission.root_event(&spawned("c1", "explorer"));
        mission.root_event(&spawned("c2", "explorer"));
        mission.root_event(&spawned("c3", "worker"));
        mission.root_event(&TurnEvent::ChildProgress {
            turn_id: turn(),
            child: SessionId::from_str("c1"),
            tool_calls: 4,
            tokens: 10,
        });
        let agents = mission.agents();
        assert_eq!(agents.len(), 4);
        let c1 = &agents[1];
        assert_eq!(c1.parent.as_deref(), Some("root"));
        assert_eq!(c1.state, MobileAgentState::Running);
        assert_eq!(c1.tool_calls, 4);
        mission.root_event(&child_done("c1", "completed"));
        mission.root_event(&child_done("c1", "failed"));
        mission.root_event(&child_done("c2", "failed"));
        mission.root_event(&child_done("c3", "cancelled"));
        let agents = mission.agents();
        assert_eq!(agents[1].state, MobileAgentState::Done);
        assert_eq!(agents[2].state, MobileAgentState::Failed);
        let finished = mission.finished();
        let explorer = find(&finished, "explorer")?;
        assert_eq!((explorer.total, explorer.ok, explorer.failed), (2, 1, 1));
        let worker = find(&finished, "worker")?;
        assert_eq!((worker.total, worker.cancelled), (1, 1));
        Ok(())
    }

    #[test]
    fn root_turn_states_and_finished_lines() -> TestResult {
        let mut mission = Mission::default();
        mission.root_event(&TurnEvent::TurnStarted {
            turn_id: turn(),
            thread_id: ThreadId::from_str("th"),
        });
        assert_eq!(mission.agents()[0].state, MobileAgentState::Running);
        mission.root_waiting(true);
        assert_eq!(mission.agents()[0].state, MobileAgentState::Waiting);
        mission.root_event(&TurnEvent::TurnFailed {
            turn_id: turn(),
            reason: "bad".into(),
            retryable: false,
        });
        assert_eq!(mission.agents()[0].state, MobileAgentState::Failed);
        let lines = mission.reasoning();
        let last = lines.last().ok_or("no line")?;
        assert_eq!((last.kind, last.ok), (ReasoningKind::Finished, Some(false)));
        Ok(())
    }

    #[test]
    fn job_upsert_and_single_terminal_tally() -> TestResult {
        let mut mission = Mission::default();
        mission.job("w1", "build", "running", None);
        mission.job("w1", "build", "running", Some("half"));
        assert_eq!(mission.jobs().len(), 1);
        assert_eq!(mission.jobs()[0].summary.as_deref(), Some("half"));
        assert!(mission.finished().is_empty());
        mission.job("w1", "build", "succeeded", None);
        mission.job("w1", "build", "succeeded", None);
        let jobs = mission.jobs();
        assert_eq!(jobs[0].state, "succeeded");
        assert_eq!(jobs[0].summary.as_deref(), Some("half"));
        let finished = mission.finished();
        let build = find(&finished, "job:build")?;
        assert_eq!((build.total, build.ok), (1, 1));
        Ok(())
    }

    #[test]
    fn collections_are_bounded() -> TestResult {
        let mut mission = Mission::default();
        for index in 0..(MAX_AGENTS + 10) {
            mission.root_event(&spawned(&format!("c{index}"), "worker"));
        }
        let agents = mission.agents();
        assert_eq!(agents.len(), MAX_AGENTS);
        assert_eq!(agents[0].id, ROOT_ID);
        for index in 0..(MAX_JOBS + 10) {
            mission.job(&format!("w{index}"), "build", "running", None);
        }
        assert_eq!(mission.jobs().len(), MAX_JOBS);
        for _ in 0..(MAX_REASONING_LINES + 20) {
            mission.root_event(&requested("k", "shell"));
        }
        assert_eq!(mission.reasoning().len(), MAX_REASONING_LINES);
        mission.root_event(&thinking(&"ä".repeat(MAX_LINE_BYTES)));
        let lines = mission.reasoning();
        let last = lines.last().ok_or("no line")?;
        assert!(last.text.len() <= MAX_LINE_BYTES);
        assert!(last.text.chars().all(|c| c == 'ä'));
        Ok(())
    }

    #[test]
    fn finished_tally_is_bounded_and_labels_truncated() -> TestResult {
        let mut mission = Mission::default();
        for index in 0..(MAX_FINISHED + 10) {
            let call = format!("k{index}");
            mission.root_event(&requested(&call, &format!("tool{index}")));
            mission.root_event(&completed(&call, true));
        }
        let finished = mission.finished();
        assert_eq!(finished.len(), MAX_FINISHED);
        let other = find(&finished, OTHER_LABEL)?;
        assert_eq!(other.total, 11);
        let long = "é".repeat(MAX_LABEL_BYTES);
        mission.job("w", &long, "done", None);
        let finished = mission.finished();
        assert_eq!(finished.len(), MAX_FINISHED);
        assert!(finished.iter().all(|c| c.label.len() <= MAX_LABEL_BYTES));
        assert_eq!(find(&finished, OTHER_LABEL)?.total, 12);
        Ok(())
    }

    #[test]
    fn child_counts_once_across_frames_and_job() -> TestResult {
        let mut mission = Mission::default();
        mission.root_event(&spawned("c1", "explorer"));
        let started = TurnEvent::TurnStarted {
            turn_id: turn(),
            thread_id: ThreadId::from_str("th"),
        };
        mission.child_event("c1", Some("root"), "explorer", &started);
        mission.child_event(
            "c1",
            Some("root"),
            "explorer",
            &TurnEvent::TurnCompleted {
                turn_id: turn(),
                usage: None,
            },
        );
        mission.root_event(&child_done("c1", "completed"));
        mission.job("c1", "agent", "failed", None);
        mission.job("c1", "agent", "failed", None);
        let finished = mission.finished();
        let total: u32 = finished.iter().map(|c| c.total).sum();
        assert_eq!(total, 1);
        let explorer = find(&finished, "explorer")?;
        assert_eq!((explorer.ok, explorer.failed), (0, 1));
        assert_eq!(mission.agents()[1].state, MobileAgentState::Failed);
        mission.job("unknown", "agent", "done", None);
        assert_eq!(mission.finished().iter().map(|c| c.total).sum::<u32>(), 1);
        Ok(())
    }

    #[test]
    fn restarted_child_finishes_again() -> TestResult {
        let mut mission = Mission::default();
        let started = TurnEvent::TurnStarted {
            turn_id: turn(),
            thread_id: ThreadId::from_str("th"),
        };
        let done = TurnEvent::TurnCompleted {
            turn_id: turn(),
            usage: None,
        };
        mission.child_event("c1", Some("root"), "worker", &done);
        assert_eq!(mission.agents()[1].state, MobileAgentState::Done);
        mission.child_event("c1", Some("root"), "worker", &started);
        assert_eq!(mission.agents()[1].state, MobileAgentState::Running);
        mission.child_event("c1", Some("root"), "worker", &done);
        assert_eq!(mission.agents()[1].state, MobileAgentState::Done);
        let finished = mission.finished();
        assert_eq!(find(&finished, "worker")?.total, 2);
        Ok(())
    }

    #[test]
    fn root_waiting_false_keeps_terminal_states_and_set_root_state() -> TestResult {
        let mut mission = Mission::default();
        mission.set_root_state(MobileAgentState::Done);
        assert_eq!(mission.agents()[0].state, MobileAgentState::Done);
        mission.root_waiting(false);
        assert_eq!(mission.agents()[0].state, MobileAgentState::Done);
        mission.set_root_state(MobileAgentState::Failed);
        mission.root_waiting(false);
        assert_eq!(mission.agents()[0].state, MobileAgentState::Failed);
        mission.set_root_state(MobileAgentState::Waiting);
        mission.root_waiting(false);
        assert_eq!(mission.agents()[0].state, MobileAgentState::Running);
        mission.root_waiting(true);
        assert_eq!(mission.agents()[0].state, MobileAgentState::Waiting);
        assert!(mission.finished().is_empty());
        Ok(())
    }

    #[test]
    fn clear_resets_everything() -> TestResult {
        let mut mission = Mission::default();
        mission.root_event(&requested("k1", "shell"));
        mission.root_event(&completed("k1", true));
        mission.job("w1", "build", "done", None);
        mission.clear();
        assert!(mission.agents().is_empty());
        assert!(mission.jobs().is_empty());
        assert!(mission.reasoning().is_empty());
        assert!(mission.finished().is_empty());
        mission.root_event(&thinking("x"));
        assert_eq!(mission.reasoning()[0].seq, 1);
        Ok(())
    }
}
