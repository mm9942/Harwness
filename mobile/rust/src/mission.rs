//! Mission view of a session: agents, jobs, reasoning ring and finished tally.
//!
//! Fed by [`SessionState::observe`](crate::projection::SessionState) from the
//! frames of the shared session stream; projected into the UniFFI records of
//! `types.rs` for the phone's mission dock.

use std::collections::{HashMap, VecDeque};

use harw_protocol::TurnEvent;
use harw_protocol::items::TurnItem;

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

struct AgentRec {
    agent: MobileAgent,
    terminal: bool,
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
}

impl Mission {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn set_root_model(&mut self, model: Option<String>) {
        let root = self.agent_mut(ROOT_ID, None, ROOT_ROLE);
        root.model = model;
    }

    pub fn root_waiting(&mut self, waiting: bool) {
        let root = self.agent_mut(ROOT_ID, None, ROOT_ROLE);
        root.state = if waiting {
            MobileAgentState::Waiting
        } else {
            MobileAgentState::Running
        };
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
        match event {
            TurnEvent::TurnStarted { .. } => {
                let agent = self.agent_mut(id, parent, role);
                agent.state = MobileAgentState::Running;
                agent.current_tool = None;
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
                if !self.calls.contains_key(call.call_id.as_str()) {
                    self.tool_start(id, parent, role, call.call_id.as_str(), &call.tool_name);
                }
            }
            TurnEvent::ToolCallCompleted {
                call_id, result, ..
            } => self.tool_end(id, call_id.as_str(), result.is_success(), is_root),
            TurnEvent::ItemAdded {
                item: TurnItem::ToolResult(result),
                ..
            } => self.tool_end(
                id,
                result.call_id.as_str(),
                result.result.is_success(),
                is_root,
            ),
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
                    self.finish_agent(id, parent, role, outcome);
                }
                self.push_line(id, role, ReasoningKind::Finished, None, "done".into());
            }
            TurnEvent::TurnFailed { reason, .. } => {
                if is_root {
                    let agent = self.agent_mut(id, parent, role);
                    agent.state = MobileAgentState::Failed;
                    agent.current_tool = None;
                } else {
                    self.finish_agent(id, parent, role, Outcome::Failed);
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
                self.finish_agent(child.as_str(), Some(id), "agent", outcome);
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
                if self.jobs.len() > MAX_JOBS
                    && let Some(old) = self.jobs.iter().position(|rec| rec.terminal)
                {
                    self.jobs.remove(old);
                }
                outcome.is_some()
            }
        };
        if newly_terminal && let Some(outcome) = outcome {
            self.tally(&format!("job:{kind}"), outcome);
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
        let index = match self.agents.iter().position(|rec| rec.agent.id == id) {
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
                    terminal: false,
                });
                if self.agents.len() > MAX_AGENTS
                    && let Some(old) = self
                        .agents
                        .iter()
                        .position(|rec| rec.terminal && rec.agent.id != ROOT_ID)
                {
                    self.agents.remove(old);
                }
                self.agents
                    .iter()
                    .position(|rec| rec.agent.id == id)
                    .unwrap_or(self.agents.len() - 1)
            }
        };
        &mut self.agents[index].agent
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
        let agent = self.agent_mut(id, parent, role);
        agent.state = MobileAgentState::Running;
        agent.current_tool = Some(name.into());
        agent.tool_calls = agent.tool_calls.saturating_add(1);
        let role = agent.role.clone();
        self.push_line(id, &role, ReasoningKind::ToolStart, None, name.into());
    }

    fn tool_end(&mut self, id: &str, call_id: &str, ok: bool, is_root: bool) {
        let Some((agent_id, name)) = self.calls.remove(call_id) else {
            return;
        };
        let role = {
            let agent = self.agent_mut(&agent_id, None, "agent");
            agent.current_tool = None;
            agent.role.clone()
        };
        self.push_line(id, &role, ReasoningKind::ToolEnd, Some(ok), name.clone());
        if is_root {
            let outcome = if ok { Outcome::Ok } else { Outcome::Failed };
            self.tally(&format!("tool:{name}"), outcome);
        }
    }

    fn finish_agent(&mut self, id: &str, parent: Option<&str>, role: &str, outcome: Outcome) {
        let index = self
            .agents
            .iter()
            .position(|rec| rec.agent.id == id)
            .unwrap_or_else(|| {
                self.agent_mut(id, parent, role);
                self.agents.len() - 1
            });
        let rec = &mut self.agents[index];
        if rec.terminal {
            return;
        }
        rec.terminal = true;
        rec.agent.current_tool = None;
        rec.agent.state = match outcome {
            Outcome::Ok => MobileAgentState::Done,
            Outcome::Failed | Outcome::Cancelled => MobileAgentState::Failed,
        };
        let label = rec.agent.role.clone();
        self.tally(&label, outcome);
    }

    fn tally(&mut self, label: &str, outcome: Outcome) {
        let index = match self.finished.iter().position(|(name, _)| name == label) {
            Some(index) => index,
            None => {
                self.finished.push((label.into(), Tally::default()));
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
            if last.text.len() > MAX_LINE_BYTES {
                let keep_from = last
                    .text
                    .ceil_char_boundary(last.text.len() - MAX_LINE_BYTES);
                last.text.drain(..keep_from);
            }
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
        text: String,
    ) {
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

fn job_outcome(state: &str) -> Option<Outcome> {
    match state.to_ascii_lowercase().as_str() {
        "completed" | "complete" | "done" | "succeeded" | "success" | "ok" => Some(Outcome::Ok),
        "failed" | "error" | "timed_out" | "timeout" | "dead" => Some(Outcome::Failed),
        "cancelled" | "canceled" | "aborted" => Some(Outcome::Cancelled),
        _ => None,
    }
}

fn now_ms() -> u64 {
    u64::try_from(jiff::Timestamp::now().as_millisecond()).unwrap_or(0)
}
