//! Globale Jobs- und Agenten-Uebersicht (Wire-Feature `overview`): `jobs.*` und `agents.*` ohne offene Session.
//! Gilt fuer jede aktive Verbindung, Cloud-Gateway wie On-Device-Host; die Berechtigungen (observe/steer) und
//! die Mandantenpruefung liegen beim Host.

use harw_protocol::session_wire::{
    AgentDefinition, AgentStopParams, AgentsListParams, AgentsListResult, JobDetail,
    JobHistoryEntry, JobRow, JobTargetParams, JobsListParams, JobsListResult, RunningAgent,
    features,
};
use harw_types::SessionId;

use crate::manage::{check_client_msg_id, check_target_id, run_result};
use crate::{ManageRunResult, MobileCore, MobileError};

const NO_OVERVIEW_DETAIL: &str = "Server kennt keine Jobs- und Agenten-Übersicht (Feature overview fehlt)";

/// Ein dauerhafter Job des Profils.
#[derive(Clone, Debug, uniffi::Record)]
pub struct OverviewJob {
    pub work_id: String,
    pub kind: String,
    /// `queued|running|blocked|succeeded|failed|timed_out|cancelled`.
    pub state: String,
    pub title: String,
    pub summary: Option<String>,
    pub session_id: Option<String>,
    pub submitted_ms: u64,
    pub started_ms: Option<u64>,
    pub finished_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub attempts: u32,
    pub can_cancel: bool,
    pub can_retry: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OverviewJobs {
    pub jobs: Vec<OverviewJob>,
    pub truncated: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OverviewJobEvent {
    pub at_ms: u64,
    pub event: String,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OverviewJobDetail {
    pub job: OverviewJob,
    pub spec_summary: String,
    pub history: Vec<OverviewJobEvent>,
    pub holder: Option<String>,
    pub output_tail: Option<String>,
}

/// Ein laufender Kind-Agent einer gehosteten Session.
#[derive(Clone, Debug, uniffi::Record)]
pub struct OverviewAgent {
    pub agent_id: String,
    pub session_id: String,
    pub parent: Option<String>,
    pub role: String,
    pub model: Option<String>,
    /// `running|waiting|done|failed`.
    pub state: String,
    pub current_tool: Option<String>,
    pub started_ms: u64,
    pub tool_calls: u32,
    pub can_stop: bool,
}

/// Eine startbare Agentendefinition (Bibliothek).
#[derive(Clone, Debug, uniffi::Record)]
pub struct OverviewAgentDef {
    pub name: String,
    pub role: String,
    pub description: Option<String>,
    pub model: Option<String>,
    /// `builtin` oder `custom`.
    pub source: String,
    pub read_only: bool,
    pub active: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct OverviewAgents {
    pub running: Vec<OverviewAgent>,
    pub library: Vec<OverviewAgentDef>,
}

impl From<JobRow> for OverviewJob {
    fn from(row: JobRow) -> Self {
        Self {
            work_id: row.work_id,
            kind: row.kind,
            state: row.state,
            title: row.title,
            summary: row.summary,
            session_id: row.session_id,
            submitted_ms: row.submitted_ms,
            started_ms: row.started_ms,
            finished_ms: row.finished_ms,
            duration_ms: row.duration_ms,
            attempts: row.attempts,
            can_cancel: row.can_cancel,
            can_retry: row.can_retry,
        }
    }
}

impl From<JobsListResult> for OverviewJobs {
    fn from(result: JobsListResult) -> Self {
        Self {
            jobs: result.jobs.into_iter().map(Into::into).collect(),
            truncated: result.truncated,
        }
    }
}

impl From<JobHistoryEntry> for OverviewJobEvent {
    fn from(entry: JobHistoryEntry) -> Self {
        Self {
            at_ms: entry.at_ms,
            event: entry.event,
            detail: entry.detail,
        }
    }
}

impl From<JobDetail> for OverviewJobDetail {
    fn from(detail: JobDetail) -> Self {
        Self {
            job: detail.job.into(),
            spec_summary: detail.spec_summary,
            history: detail.history.into_iter().map(Into::into).collect(),
            holder: detail.holder,
            output_tail: detail.output_tail,
        }
    }
}

impl From<RunningAgent> for OverviewAgent {
    fn from(agent: RunningAgent) -> Self {
        Self {
            agent_id: agent.agent_id,
            session_id: agent.session_id.to_string(),
            parent: agent.parent,
            role: agent.role,
            model: agent.model,
            state: agent.state,
            current_tool: agent.current_tool,
            started_ms: agent.started_ms,
            tool_calls: agent.tool_calls,
            can_stop: agent.can_stop,
        }
    }
}

impl From<AgentDefinition> for OverviewAgentDef {
    fn from(def: AgentDefinition) -> Self {
        Self {
            name: def.name,
            role: def.role,
            description: def.description,
            model: def.model,
            source: def.source,
            read_only: def.read_only,
            active: def.active,
        }
    }
}

impl From<AgentsListResult> for OverviewAgents {
    fn from(result: AgentsListResult) -> Self {
        Self {
            running: result.running.into_iter().map(Into::into).collect(),
            library: result.library.into_iter().map(Into::into).collect(),
        }
    }
}

fn overview_call<T, F>(core: &MobileCore, call: F) -> Result<T, MobileError>
where
    T: Send + 'static,
    F: for<'a> FnOnce(
            &'a dyn harw_protocol::SessionPort,
        ) -> harw_protocol::PortFuture<'a, T>
        + Send
        + 'static,
{
    core.manage_call(features::OVERVIEW, call).map_err(|error| match error {
        MobileError::Denied { detail } if detail == crate::manage::NO_MANAGE_DETAIL => {
            MobileError::Denied {
                detail: NO_OVERVIEW_DETAIL.to_owned(),
            }
        }
        other => other,
    })
}

#[uniffi::export]
impl MobileCore {
    /// Bietet die aktive Verbindung die globale Jobs- und Agenten-Uebersicht an? Nie ein Fehler.
    pub fn overview_support(&self) -> bool {
        self.feature_granted(features::OVERVIEW)
    }

    /// `jobs.list`: alle Jobs des Profils, neueste zuerst; `states` leer = alle Zustaende.
    pub fn overview_jobs(
        &self,
        states: Vec<String>,
        limit: Option<u32>,
    ) -> Result<OverviewJobs, MobileError> {
        let params = JobsListParams { states, limit };
        overview_call(self, move |port| port.jobs_list(params)).map(Into::into)
    }

    /// `jobs.get`: Spezifikation, Statusverlauf und Ausgabe-Ende eines Jobs.
    pub fn overview_job(&self, work_id: String) -> Result<OverviewJobDetail, MobileError> {
        check_target_id(&work_id)?;
        let params = JobTargetParams {
            work_id,
            client_msg_id: String::new(),
        };
        overview_call(self, move |port| port.jobs_get(params)).map(Into::into)
    }

    /// `jobs.cancel` (Cap steer).
    pub fn overview_job_cancel(
        &self,
        work_id: String,
        client_msg_id: String,
    ) -> Result<ManageRunResult, MobileError> {
        check_target_id(&work_id)?;
        check_client_msg_id(&client_msg_id)?;
        let params = JobTargetParams {
            work_id,
            client_msg_id,
        };
        let result = overview_call(self, move |port| port.jobs_cancel(params))?;
        run_result(result, jiff::Timestamp::now())
    }

    /// `jobs.retry` (Cap steer): fordert einen gescheiterten oder abgebrochenen Job erneut an.
    pub fn overview_job_retry(
        &self,
        work_id: String,
        client_msg_id: String,
    ) -> Result<ManageRunResult, MobileError> {
        check_target_id(&work_id)?;
        check_client_msg_id(&client_msg_id)?;
        let params = JobTargetParams {
            work_id,
            client_msg_id,
        };
        let result = overview_call(self, move |port| port.jobs_retry(params))?;
        run_result(result, jiff::Timestamp::now())
    }

    /// `agents.list`: laufende Agenten ueber alle Sessions und die Agentenbibliothek.
    pub fn overview_agents(&self) -> Result<OverviewAgents, MobileError> {
        overview_call(self, move |port| port.agents_list(AgentsListParams {})).map(Into::into)
    }

    /// `agents.stop` (Cap steer).
    pub fn overview_agent_stop(
        &self,
        session_id: String,
        agent_id: String,
        client_msg_id: String,
    ) -> Result<ManageRunResult, MobileError> {
        check_target_id(&agent_id)?;
        check_client_msg_id(&client_msg_id)?;
        let session_id = SessionId::try_from_str(session_id).map_err(|_| MobileError::Session {
            detail: "Invalid session ID".to_owned(),
        })?;
        let params = AgentStopParams {
            session_id,
            agent_id,
            client_msg_id,
        };
        let result = overview_call(self, move |port| port.agents_stop(params))?;
        run_result(result, jiff::Timestamp::now())
    }
}
