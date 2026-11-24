//! Cloud workload placement and advisory selection. The configured profile is
//! a ceiling; identity, lifecycle, mounted services, and backend enforcement
//! are independently observed facts. No candidate creates a job or grants
//! authority to execute one.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::num::NonZeroU64;
use std::sync::Arc;

use harw_placement_model::{
    NodeOffer, RequiredDimensions, RuntimeBackend, RuntimeOffer, RuntimeRejection,
    RuntimeRequirements, UnixMillis, filter_runtime,
};
use harw_types::NodeId;
use serde::Deserialize;

use super::{DeploymentProfile, Service};

const MAX_NODES: usize = 128;
const MAX_RUNTIME_OFFERS: usize = 16;
const MAX_WORKER_OBSERVATIONS: usize = 128;
const MAX_SERVICES: usize = 16;
const MAX_OBSERVATION_AGE_MS: u64 = 30_000;
const MAX_DECISION_AGE_MS: u64 = 5_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "kebab-case")]
pub enum CloudWorkload {
    /// Build/compile/test/lint on the job-owned Compile lane.
    Compile,
    /// Constrained agent/process orchestration jobs, not generic tooling.
    AgentOrchestration,
    /// Typed build on the Compile lane and a rootless Podman service.
    PodmanBuild,
    /// Typed build with Docker; requires additional rootful approval.
    DockerBuild,
}

impl CloudWorkload {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compile => "compile",
            Self::AgentOrchestration => "agent-orchestration",
            Self::PodmanBuild => "podman-build",
            Self::DockerBuild => "docker-build",
        }
    }

    const fn profiles(self) -> &'static [DeploymentProfile] {
        use DeploymentProfile::{CompileHub, ContainerHub, WorkerHub};
        match self {
            Self::Compile => &[CompileHub],
            Self::AgentOrchestration => &[WorkerHub],
            Self::PodmanBuild | Self::DockerBuild => &[CompileHub, ContainerHub],
        }
    }

    const fn matches_backend(self, backend: RuntimeBackend) -> bool {
        use RuntimeBackend::{Bwrap, Darwin, Landlock, OciDocker, OciPodmanRootless};
        match self {
            Self::Compile | Self::AgentOrchestration => matches!(backend, Landlock | Bwrap | Darwin),
            Self::PodmanBuild => matches!(backend, OciPodmanRootless),
            Self::DockerBuild => matches!(backend, OciDocker),
        }
    }
}

/// Operator-side errors. Do not expose another tenant's node or offer details
/// from an untrusted submit path.
#[derive(Debug, Eq, PartialEq)]
pub enum CloudPlacementError {
    InvalidPolicy,
    InvalidAdvertisement,
    NodeNotConfigured,
    ProfileNotConfigured,
    WorkloadNotConfigured,
    ProfileNotMounted,
    BackendMismatch,
    Runtime(Vec<RuntimeRejection>),
    TooManyObservations,
    DuplicateObservation,
    NoEligibleCandidate,
    AmbiguousCandidate,
    StaleDecision,
}

impl fmt::Display for CloudPlacementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidPolicy => "invalid cloud workload policy",
            Self::InvalidAdvertisement => "invalid worker deployment observation",
            Self::NodeNotConfigured => "node is not configured for cloud workloads",
            Self::ProfileNotConfigured => "worker profile is outside configured node policy",
            Self::WorkloadNotConfigured => "workload is not configured for this node",
            Self::ProfileNotMounted => "worker has not mounted the workload profiles",
            Self::BackendMismatch => "requested backend does not match the workload",
            Self::Runtime(_) => "worker failed the required runtime eligibility checks",
            Self::TooManyObservations => "placement observation set exceeds its bound",
            Self::DuplicateObservation => "duplicate observation for the same worker process",
            Self::NoEligibleCandidate => "no eligible worker for the selected placement",
            Self::AmbiguousCandidate => "more than one worker is eligible; choose an exact target",
            Self::StaleDecision => "placement advice is stale or from a different authority",
        })
    }
}

impl std::error::Error for CloudPlacementError {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyV1 {
    version: u8,
    revision: NonZeroU64,
    nodes: Vec<PolicyNode>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyNode {
    node: String,
    profiles: Vec<String>,
    workloads: Vec<CloudWorkload>,
}

/// Explicit profiles and workload ceilings for an operator-selected node.
#[derive(Debug)]
pub struct ConfiguredWorkloads {
    node: NodeId,
    profiles: BTreeSet<DeploymentProfile>,
    workloads: BTreeSet<CloudWorkload>,
}

impl ConfiguredWorkloads {
    #[must_use]
    pub const fn node(&self) -> &NodeId {
        &self.node
    }

    pub fn profiles(&self) -> impl Iterator<Item = DeploymentProfile> + '_ {
        self.profiles.iter().copied()
    }

    pub fn workloads(&self) -> impl Iterator<Item = CloudWorkload> + '_ {
        self.workloads.iter().copied()
    }
}

#[derive(Debug)]
pub struct CloudWorkloadPolicy {
    authority: Arc<()>,
    revision: NonZeroU64,
    nodes: BTreeMap<String, ConfiguredWorkloads>,
}

impl CloudWorkloadPolicy {
    /// Input cap shared by the trusted loaders and the local operator CLI.
    pub const MAX_BYTES: usize = 64 * 1024;

    /// Parse a bounded v1 TOML file selected by the trusted operator. There
    /// are no inferred profiles, runtime defaults, or endpoint addresses.
    /// Empty node/workload lists authorize nothing; duplicates are rejected.
    ///
    /// # Errors
    /// Unknown fields, wrong versions, malformed IDs and conflicts between
    /// authorized workloads and allowed profiles are refused.
    pub fn load_from_str(text: &str) -> Result<Self, CloudPlacementError> {
        if text.is_empty() || text.len() > Self::MAX_BYTES {
            return Err(CloudPlacementError::InvalidPolicy);
        }
        let raw: PolicyV1 = toml::from_str(text).map_err(|_| CloudPlacementError::InvalidPolicy)?;
        if raw.version != 1 || raw.nodes.len() > MAX_NODES {
            return Err(CloudPlacementError::InvalidPolicy);
        }
        let mut nodes = BTreeMap::new();
        for row in raw.nodes {
            if !valid_id(&row.node) || row.profiles.len() > DeploymentProfile::ALL.len()
                || row.workloads.len() > 4
            {
                return Err(CloudPlacementError::InvalidPolicy);
            }
            let node = NodeId::parse(&row.node).map_err(|_| CloudPlacementError::InvalidPolicy)?;
            let profiles: BTreeSet<_> = row.profiles.iter()
                .map(|profile| DeploymentProfile::parse(profile).ok_or(CloudPlacementError::InvalidPolicy))
                .collect::<Result<_, _>>()?;
            let workload_count = row.workloads.len();
            let workloads: BTreeSet<_> = row.workloads.into_iter().collect();
            if profiles.len() != row.profiles.len() || workloads.len() != workload_count
                || workloads.iter().any(|work| !work.profiles().iter().all(|p| profiles.contains(p)))
            {
                return Err(CloudPlacementError::InvalidPolicy);
            }
            if nodes.insert(row.node, ConfiguredWorkloads { node, profiles, workloads }).is_some() {
                return Err(CloudPlacementError::InvalidPolicy);
            }
        }
        Ok(Self { authority: Arc::new(()), revision: raw.revision, nodes })
    }

    #[must_use]
    pub const fn revision(&self) -> NonZeroU64 {
        self.revision
    }

    /// Inspection returns configuration, never a claim about live nodes.
    pub fn configured_nodes(&self) -> impl Iterator<Item = &ConfiguredWorkloads> {
        self.nodes.values()
    }

    /// Filter one explicit, independently observed worker against the
    /// operator's node policy. The trusted request chooses one exact backend.
    /// No ranking, fallback, lifecycle mutation, reservations or dispatch.
    ///
    /// # Errors
    /// A wrong lane, unconfigured or unmounted profile, or an ineligible
    /// runtime fails without returning any candidate.
    pub fn candidate(
        &self,
        workload: CloudWorkload,
        worker: &ObservedDeployment,
        requirements: RuntimeRequirements,
        now: UnixMillis,
    ) -> Result<WorkloadCandidate, CloudPlacementError> {
        let policy = self.nodes.get(worker.offer.node.as_str())
            .ok_or(CloudPlacementError::NodeNotConfigured)?;
        if !worker.profiles.is_subset(&policy.profiles) {
            return Err(CloudPlacementError::ProfileNotConfigured);
        }
        if !policy.workloads.contains(&workload) {
            return Err(CloudPlacementError::WorkloadNotConfigured);
        }
        if !workload.profiles().iter().all(|profile| worker.profiles.contains(profile)) {
            return Err(CloudPlacementError::ProfileNotMounted);
        }
        if !workload.matches_backend(requirements.backend) {
            return Err(CloudPlacementError::BackendMismatch);
        }

        // Cloud placement never widens a weaker caller-selected profile.
        // A rootful backend also needs the separate trusted request opt-in.
        let strict = RuntimeRequirements {
            backend: requirements.backend,
            required: RequiredDimensions {
                filesystem: true,
                network: true,
                no_new_privs: true,
                capabilities: true,
                resource_limits: true,
            },
            allow_rootful: workload == CloudWorkload::DockerBuild && requirements.allow_rootful,
            max_silence_ms: requirements.max_silence_ms.min(MAX_OBSERVATION_AGE_MS),
        };
        let runtime = filter_runtime(&worker.offer, &strict, now).map_err(CloudPlacementError::Runtime)?;
        if !now.0.checked_sub(runtime.observed.0).is_some_and(|age| age <= MAX_OBSERVATION_AGE_MS) {
            return Err(CloudPlacementError::Runtime(vec![RuntimeRejection::RuntimeStale]));
        }
        let node_until = worker.offer.last_seen.0.saturating_add(strict.max_silence_ms);
        let runtime_until = runtime.observed.0
            .saturating_add(runtime.ttl_ms.min(MAX_OBSERVATION_AGE_MS));
        let observed_until = UnixMillis(node_until.min(runtime_until));
        if now >= observed_until {
            let reason = if node_until <= runtime_until {
                RuntimeRejection::NodeStale
            } else {
                RuntimeRejection::RuntimeStale
            };
            return Err(CloudPlacementError::Runtime(vec![reason]));
        }
        Ok(WorkloadCandidate {
            node: policy.node.clone(),
            instance: worker.instance.clone(),
            activation_epoch: worker.activation_epoch,
            workload,
            runtime,
            policy_revision: self.revision,
            observed_until,
        })
    }

    /// List eligible observations for one exact workload/backend combination.
    /// A duplicate node/instance invalidates the set even if one copy is
    /// ineligible: freshness is not a way to break conflicting provenance.
    /// Output order is for inspection only; it is not ranking.
    ///
    /// # Errors
    /// Refuses oversized or duplicate observation sets and a wrong backend.
    pub fn eligible_candidates(
        &self,
        workload: CloudWorkload,
        workers: &[ObservedDeployment],
        requirements: RuntimeRequirements,
        now: UnixMillis,
    ) -> Result<Vec<WorkloadCandidate>, CloudPlacementError> {
        if workers.len() > MAX_WORKER_OBSERVATIONS {
            return Err(CloudPlacementError::TooManyObservations);
        }
        if !workload.matches_backend(requirements.backend) {
            return Err(CloudPlacementError::BackendMismatch);
        }
        let mut seen = BTreeSet::new();
        let mut eligible = Vec::new();
        for worker in workers {
            if !seen.insert((worker.offer.node.as_str(), worker.instance.as_str())) {
                return Err(CloudPlacementError::DuplicateObservation);
            }
            if let Ok(candidate) = self.candidate(workload, worker, requirements, now)
                && now < candidate.observed_until
            {
                eligible.push(candidate);
            }
        }
        eligible.sort_by(|a, b| {
            a.node.as_str().cmp(b.node.as_str()).then_with(|| a.instance.cmp(&b.instance))
        });
        Ok(eligible)
    }

    /// Create in-process advice for an exact selected worker or the sole
    /// eligible worker. No score or alternate target is inferred from result
    /// order, and an unavailable explicit target cannot select another.
    ///
    /// # Errors
    /// Inherits the candidate-set refusals. Refuses absent targets and a
    /// non-unique result when no exact target was selected.
    pub fn advise(
        &self,
        workload: CloudWorkload,
        workers: &[ObservedDeployment],
        requirements: RuntimeRequirements,
        selection: CandidateSelection<'_>,
        now: UnixMillis,
    ) -> Result<WorkloadDecision, CloudPlacementError> {
        let mut eligible = self.eligible_candidates(workload, workers, requirements, now)?;
        let candidate = match selection {
            CandidateSelection::RequireUnique => match eligible.len() {
                0 => return Err(CloudPlacementError::NoEligibleCandidate),
                1 => eligible.pop().ok_or(CloudPlacementError::NoEligibleCandidate)?,
                _ => return Err(CloudPlacementError::AmbiguousCandidate),
            },
            CandidateSelection::Exact { node, instance } => eligible.into_iter()
                .find(|candidate| &candidate.node == node && candidate.instance == instance)
                .ok_or(CloudPlacementError::NoEligibleCandidate)?,
        };
        let expires_at = candidate.observed_until.min(UnixMillis(now.0.saturating_add(MAX_DECISION_AGE_MS)));
        Ok(WorkloadDecision {
            candidate,
            authority: Arc::clone(&self.authority),
            requirements,
            issued_at: now,
            expires_at,
        })
    }

    /// Recheck advice against the same exact policy snapshot and a fresh
    /// independent observation of the same worker. This does not extend the
    /// deadline or replace job-owner authorization/claim.
    ///
    /// # Errors
    /// Refuses an expired, foreign, replaced, or now-ineligible target.
    pub fn revalidate_advice(
        &self,
        decision: &WorkloadDecision,
        current: &ObservedDeployment,
        now: UnixMillis,
    ) -> Result<(), CloudPlacementError> {
        let target = &decision.candidate;
        if !Arc::ptr_eq(&self.authority, &decision.authority)
            || self.revision != target.policy_revision
            || now < decision.issued_at
            || now >= decision.expires_at
            || current.offer.node != target.node.as_str()
            || current.instance != target.instance
            || current.activation_epoch != target.activation_epoch
        {
            return Err(CloudPlacementError::StaleDecision);
        }
        let candidate = self.candidate(target.workload, current, decision.requirements, now)?;
        if now >= candidate.observed_until {
            return Err(CloudPlacementError::StaleDecision);
        }
        Ok(())
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.starts_with('.')
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// One process's verified mount/offer observation. Non-serializable; never
/// construct from submission fields or from an observation's asserted node ID
/// alone. The composition supplies the authenticated node, observed instance,
/// actual mounted services, and independently resolved lifecycle/activation.
/// Sample the activation on both sides of collecting the observation and
/// never construct this value if that authority is absent or changed.
#[derive(Debug)]
pub struct ObservedDeployment {
    instance: String,
    activation_epoch: NonZeroU64,
    profiles: BTreeSet<DeploymentProfile>,
    offer: NodeOffer,
}

impl ObservedDeployment {
    /// Bind a runtime offer to one authenticated worker process and its real
    /// mount table. Profiles on separate processes of a node may not be pooled.
    ///
    /// # Errors
    /// Refuses a mismatched node, duplicate/forbidden mounts and inconsistent
    /// composition. In particular, CloudHub cannot share an execution process.
    pub fn from_trusted_mount(
        authenticated_node: &NodeId,
        instance: &str,
        activation_epoch: NonZeroU64,
        profiles: &[DeploymentProfile],
        services: &[Service],
        offer: NodeOffer,
    ) -> Result<Self, CloudPlacementError> {
        if offer.node != authenticated_node.as_str() || !valid_id(authenticated_node.as_str())
            || !valid_id(instance) || profiles.is_empty() || profiles.len() > DeploymentProfile::ALL.len()
            || services.is_empty() || services.len() > MAX_SERVICES
            || offer.runtimes.len() > MAX_RUNTIME_OFFERS
        {
            return Err(CloudPlacementError::InvalidAdvertisement);
        }
        let declared: BTreeSet<_> = profiles.iter().copied().collect();
        let mounted: BTreeSet<_> = services.iter().copied().collect();
        let expected: BTreeSet<_> = profiles.iter().flat_map(|p| p.mounts().iter().copied()).collect();
        if declared.len() != profiles.len() || mounted.len() != services.len() || mounted != expected
            || profiles.iter().enumerate().any(|(idx, left)| {
                profiles[idx + 1..].iter().any(|right| !left.compatible_with(*right))
            })
        {
            return Err(CloudPlacementError::InvalidAdvertisement);
        }
        Ok(Self { instance: instance.to_owned(), activation_epoch, profiles: declared, offer })
    }
}

/// A point-in-time eligibility decision for inspection/composition. Not a
/// portable credential, reservation, or proof that a future job will run.
#[derive(Debug)]
pub struct WorkloadCandidate {
    node: NodeId,
    instance: String,
    activation_epoch: NonZeroU64,
    workload: CloudWorkload,
    runtime: RuntimeOffer,
    policy_revision: NonZeroU64,
    observed_until: UnixMillis,
}

impl WorkloadCandidate {
    #[must_use]
    pub const fn node(&self) -> &NodeId { &self.node }

    #[must_use]
    pub fn instance(&self) -> &str { &self.instance }

    /// Activation that was independently observed, not a portable grant.
    #[must_use]
    pub const fn activation_epoch(&self) -> NonZeroU64 { self.activation_epoch }

    #[must_use]
    pub const fn workload(&self) -> CloudWorkload { self.workload }

    #[must_use]
    pub const fn runtime(&self) -> RuntimeOffer { self.runtime }

    #[must_use]
    pub const fn policy_revision(&self) -> NonZeroU64 { self.policy_revision }
}

/// The caller chooses an exact worker or explicitly permits the only
/// eligible worker. There is no silent choice between multiple workers.
pub enum CandidateSelection<'a> {
    RequireUnique,
    Exact { node: &'a NodeId, instance: &'a str },
}

/// Expiring advisory placement, not a serialized grant or a reservation.
#[derive(Debug)]
pub struct WorkloadDecision {
    candidate: WorkloadCandidate,
    authority: Arc<()>,
    requirements: RuntimeRequirements,
    issued_at: UnixMillis,
    expires_at: UnixMillis,
}

impl WorkloadDecision {
    #[must_use]
    pub const fn candidate(&self) -> &WorkloadCandidate {
        &self.candidate
    }

    /// The exclusive bound shared with the original observations.
    #[must_use]
    pub const fn expires_at(&self) -> UnixMillis {
        self.expires_at
    }
}
