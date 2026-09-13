//! Closed, server-authoritative durable-job admission.
//!
//! `JobIntent` is deliberately smaller than `StoredJob`: callers can name a
//! configured workspace and provide task data, but cannot provide identity,
//! scheduling, resource, sandbox, or credential authority.  Those values are
//! resolved by the trusted policy implementation at this boundary.

use std::fmt;
use std::sync::Arc;

use harw_job_runtime::{Budget, Job, JobKind, JobScope, RetryPolicy, StoredJob};
use harw_sandbox::{SandboxError, SandboxSpec, WorkspaceBinding, WorkspaceRegistry};
use harw_session_store::{JobStore, SessionStoreError};
use harw_types::{ApprovalActor, TenantId, WorkspaceId};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Untrusted request accepted at the admission edge. Unknown fields are
/// rejected so authority-bearing wire fields cannot be silently ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobIntent {
    pub workspace: String,
    pub task: Value,
}

/// Trusted context supplied by an authenticated ingress resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionContext {
    tenant: TenantId,
    submitter: ApprovalActor,
}

impl AdmissionContext {
    #[must_use]
    pub fn new(tenant: TenantId, submitter: ApprovalActor) -> Self {
        Self { tenant, submitter }
    }

    #[must_use]
    pub fn tenant(&self) -> &TenantId {
        &self.tenant
    }

    #[must_use]
    pub fn submitter(&self) -> &ApprovalActor {
        &self.submitter
    }
}

/// Server-resolved policy. No field can be supplied by [`JobIntent`].
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedAdmission {
    pub kind: JobKind,
    pub budget: Budget,
    pub retry: RetryPolicy,
    pub sandbox: SandboxSpec,
}

/// Policy/catalog/sandbox resolver owned by the trusted composition layer.
pub trait JobAdmissionPolicy: Send + Sync {
    fn resolve(
        &self,
        context: &AdmissionContext,
        workspace: &WorkspaceBinding,
        task: &Value,
    ) -> Result<ResolvedAdmission, JobAdmissionError>;
}

/// Durable admission facade. It generates the work identity and persists only
/// a server-resolved `StoredJob`.
pub struct JobAdmissionService<P> {
    store: Arc<JobStore>,
    workspaces: Arc<WorkspaceRegistry>,
    policy: P,
}

impl<P: JobAdmissionPolicy> JobAdmissionService<P> {
    #[must_use]
    pub fn new(store: Arc<JobStore>, workspaces: Arc<WorkspaceRegistry>, policy: P) -> Self {
        Self {
            store,
            workspaces,
            policy,
        }
    }

    /// Validate, resolve, generate identity, and durably admit one job.
    pub fn admit(
        &self,
        intent: JobIntent,
        context: &AdmissionContext,
        now: Timestamp,
    ) -> Result<StoredJob, JobAdmissionError> {
        let workspace_id = validate_workspace_alias(&intent.workspace)?;
        let binding = self.workspaces.resolve(context.tenant(), &workspace_id)?;
        let task = sanitize_task(intent.task)?;
        let resolved = self.policy.resolve(context, &binding, &task)?;
        if resolved.sandbox.workspace() != &binding {
            return Err(JobAdmissionError::SandboxBindingMismatch);
        }

        let id = harw_types::WorkId::new();
        let job = Job::new(
            id.clone(),
            resolved.kind,
            resolved.budget,
            resolved.retry,
            now,
        );
        let record = StoredJob {
            job,
            scope: JobScope::new(
                context.tenant().clone(),
                workspace_id,
                context.submitter().clone(),
            ),
            input: task,
            submitted_at: now,
            not_before: now,
            lease: None,
            lease_epoch: 0,
            completion: None,
            cancellation: None,
            revision: 0,
            // AdmissionContext (tenant + submitter) carries no trace context,
            // and JobAdmissionService is not yet wired to any caller that
            // holds one.
            trace: None,
        };
        self.store.admit(&record)?;
        Ok(record)
    }
}

fn validate_workspace_alias(raw: &str) -> Result<WorkspaceId, JobAdmissionError> {
    if raw.is_empty()
        || raw == "."
        || raw == ".."
        || raw.len() > 128
        || !raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(JobAdmissionError::InvalidWorkspaceAlias(raw.to_owned()));
    }
    Ok(WorkspaceId::from_str(raw))
}

const RESERVED: &[&str] = &[
    "tenant",
    "budget",
    "retry",
    "sandbox",
    "capabilities",
    "credentials",
    "work_id",
];

fn sanitize_task(value: Value) -> Result<Value, JobAdmissionError> {
    fn walk(value: Value, depth: usize) -> Result<Value, JobAdmissionError> {
        if depth > 32 {
            return Err(JobAdmissionError::TaskTooDeep);
        }
        match value {
            Value::Object(object) => {
                for key in object.keys() {
                    if RESERVED.contains(&key.as_str()) {
                        return Err(JobAdmissionError::ReservedTaskField(key.clone()));
                    }
                }
                let entries = object.into_iter().collect::<Vec<_>>();
                let mut out = serde_json::Map::new();
                for (key, value) in entries {
                    out.insert(key, walk(value, depth + 1)?);
                }
                Ok(Value::Object(out))
            }
            Value::Array(values) => values
                .into_iter()
                .map(|v| walk(v, depth + 1))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            other => Ok(other),
        }
    }
    walk(value, 0)
}

#[derive(Debug)]
pub enum JobAdmissionError {
    InvalidWorkspaceAlias(String),
    ReservedTaskField(String),
    TaskTooDeep,
    SandboxBindingMismatch,
    PolicyRejected(String),
    Sandbox(SandboxError),
    Store(SessionStoreError),
}

impl fmt::Display for JobAdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidWorkspaceAlias(v) => write!(f, "invalid workspace alias '{v}'"),
            Self::ReservedTaskField(v) => write!(f, "reserved task field '{v}' is not admissible"),
            Self::TaskTooDeep => f.write_str("task nesting exceeds admission limit"),
            Self::SandboxBindingMismatch => {
                f.write_str("policy sandbox is not bound to resolved workspace")
            }
            Self::PolicyRejected(v) => write!(f, "admission policy rejected task: {v}"),
            Self::Sandbox(v) => write!(f, "workspace resolution failed: {v}"),
            Self::Store(v) => write!(f, "job admission persistence failed: {v}"),
        }
    }
}
impl std::error::Error for JobAdmissionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sandbox(v) => Some(v),
            Self::Store(v) => Some(v),
            _ => None,
        }
    }
}
impl From<SandboxError> for JobAdmissionError {
    fn from(v: SandboxError) -> Self {
        Self::Sandbox(v)
    }
}
impl From<SessionStoreError> for JobAdmissionError {
    fn from(v: SessionStoreError) -> Self {
        Self::Store(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_sandbox::{PermissionSet, WorkspaceRegistration};
    use harw_types::{ChannelId, PeerId};
    use std::path::Path;
    use tempfile::tempdir;

    struct Policy;
    impl JobAdmissionPolicy for Policy {
        fn resolve(
            &self,
            _: &AdmissionContext,
            workspace: &WorkspaceBinding,
            _: &Value,
        ) -> Result<ResolvedAdmission, JobAdmissionError> {
            Ok(ResolvedAdmission {
                kind: JobKind::Worker,
                budget: Budget::unbounded(),
                retry: RetryPolicy {
                    max_attempts: 2,
                    base_delay: jiff::SignedDuration::ZERO,
                    factor: 1.0,
                    max_delay: jiff::SignedDuration::ZERO,
                },
                sandbox: SandboxSpec::from_resolved(workspace.clone(), PermissionSet::empty()),
            })
        }
    }

    fn service(root: &Path) -> JobAdmissionService<Policy> {
        let tenant = TenantId::from_str("tenant");
        let workspace = WorkspaceId::from_str("safe");
        let registry = WorkspaceRegistry::build(
            root,
            [WorkspaceRegistration {
                tenant,
                workspace,
                root: root.to_path_buf(),
            }],
        )
        .expect("registry");
        JobAdmissionService::new(Arc::new(JobStore::new(root)), Arc::new(registry), Policy)
    }

    fn context() -> AdmissionContext {
        AdmissionContext::new(
            TenantId::from_str("tenant"),
            ApprovalActor::ChannelPeer {
                channel: ChannelId::from_str("telegram"),
                peer: PeerId::from_str("42"),
            },
        )
    }

    #[test]
    fn closed_intent_rejects_authority_fields() {
        let parsed = serde_json::from_value::<JobIntent>(
            serde_json::json!({"workspace":"safe","task":{},"budget":{}}),
        );
        assert!(parsed.is_err());
    }

    #[test]
    fn admission_generates_identity_and_scope() {
        let root = tempdir().expect("tempdir");
        let record = service(root.path())
            .admit(
                JobIntent {
                    workspace: "safe".into(),
                    task: serde_json::json!({"prompt":"hi"}),
                },
                &context(),
                Timestamp::now(),
            )
            .expect("admit");
        assert!(!record.job.id.as_str().is_empty());
        assert_eq!(record.scope.tenant().as_str(), "tenant");
        assert_eq!(record.scope.workspace().as_str(), "safe");
    }

    #[test]
    fn traversal_and_nested_authority_are_rejected() {
        let root = tempdir().expect("tempdir");
        let svc = service(root.path());
        assert!(matches!(
            svc.admit(
                JobIntent {
                    workspace: "../safe".into(),
                    task: Value::Null
                },
                &context(),
                Timestamp::now()
            ),
            Err(JobAdmissionError::InvalidWorkspaceAlias(_))
        ));
        assert!(matches!(
            svc.admit(
                JobIntent {
                    workspace: "safe".into(),
                    task: serde_json::json!({"nested":{"credentials":"x"}})
                },
                &context(),
                Timestamp::now()
            ),
            Err(JobAdmissionError::ReservedTaskField(_))
        ));
        assert!(matches!(
            svc.admit(
                JobIntent {
                    workspace: "unknown".into(),
                    task: Value::Null
                },
                &context(),
                Timestamp::now()
            ),
            Err(JobAdmissionError::Sandbox(_))
        ));
    }
}
