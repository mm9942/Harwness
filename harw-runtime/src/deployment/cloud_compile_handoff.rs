//! Read-only preflight for a single registered owner-local compile template.
//!
//! Not a transport, deployment, lease, authorization issuer, or executor.
//! The host must supply the independently authenticated owner and locally
//! pinned source/toolchain bundle bytes. A successful check is a point-in-time
//! comparison, not a claim or a portable authority grant.

use std::fmt;
use std::io::Read;
use std::os::fd::AsFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use harw_fsutil::{OpenMode, ensure_private_regular, open_beneath, open_dir_nofollow};
use harw_job_core::{
    IdempotencyKey, JobKind, JobScope, JobScopeId, JobSpec, JobSpecEnvelope, JobState,
    ResourceRequest, SandboxProfileName, SandboxRequirement, StoredJob, spec::CpuBandwidth,
};
use harw_job_runtime::CoordinatorStore;
use harw_placement_model::UnixMillis;
use harw_session_store::ApprovalStore;
use harw_types::{
    ContentDigest, IngressSurface, ItemId, PermissionTier, Principal, PrincipalKind,
    ReviewDecision, SessionId, ToolCallId, WorkId,
};
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};

use super::cloud_placement::{
    CloudWorkload, CloudWorkloadPolicy, ObservedDeployment, WorkloadDecision,
};

mod bundle_plan;

pub use bundle_plan::{CompileBundlePlan, CompileFileMode, CompilePlannedFile};
use bundle_plan::BundleRole;

const APPROVAL_DOMAIN: &str = "harw.owner-local.compile.cargo-test-locked-offline.v3";
const MAX_APPROVAL_AGE: SignedDuration = SignedDuration::from_secs(300);

const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TOOLCHAIN_BYTES: u64 = 256 * 1024 * 1024;

/// Exact content pins for two trusted owner-local input bundles. Retains only
/// immutable copies; changes to the store after verification cannot substitute
/// bytes in this value. A pin includes a strict file-only plan, not proof
/// that a materializer, usable executable, or supported worker exists.
pub struct PinnedCompileInputs {
    source_digest: ContentDigest,
    toolchain_digest: ContentDigest,
    source_bytes: Box<[u8]>,
    toolchain_bytes: Box<[u8]>,
    source_plan: CompileBundlePlan,
    toolchain_plan: CompileBundlePlan,
}

impl PinnedCompileInputs {
    /// Read the two approved objects from a configured absolute owner-private
    /// directory. Only fixed, digest-derived object names are accepted;
    /// no symlinks in the store path or object names are followed. This is
    /// read-only and does not extract or run either object.
    ///
    /// # Errors
    /// Missing, unsafe, non-private, empty, oversized or altered objects.
    pub fn load_from_owner_store(
        root: &Path,
        source: ContentDigest,
        toolchain: ContentDigest,
    ) -> Result<Self, CompilePreflightError> {
        if source == toolchain {
            return Err(CompilePreflightError::InputPin);
        }
        let relative_root = root
            .strip_prefix("/")
            .map_err(|_| CompilePreflightError::InputPin)?;
        let system_root =
            open_dir_nofollow(Path::new("/")).map_err(|_| CompilePreflightError::InputPin)?;
        let store = open_beneath(system_root.as_fd(), relative_root, OpenMode::read_only())
            .map_err(|_| CompilePreflightError::InputPin)?;
        let meta = store.metadata().map_err(|_| CompilePreflightError::InputPin)?;
        if !meta.is_dir() || meta.mode() & 0o077 != 0 {
            return Err(CompilePreflightError::InputPin);
        }

        let source_bytes = read_pinned_asset(&store, meta.uid(), "source", source, MAX_SOURCE_BYTES)?;
        let toolchain_bytes =
            read_pinned_asset(&store, meta.uid(), "toolchain", toolchain, MAX_TOOLCHAIN_BYTES)?;
        let source_plan = CompileBundlePlan::validate(&source_bytes, BundleRole::Source)
            .ok_or(CompilePreflightError::InputManifest)?;
        let toolchain_plan = CompileBundlePlan::validate(&toolchain_bytes, BundleRole::Toolchain)
            .ok_or(CompilePreflightError::InputManifest)?;
        Ok(Self {
            source_digest: source,
            toolchain_digest: toolchain,
            source_bytes,
            toolchain_bytes,
            source_plan,
            toolchain_plan,
        })
    }

    /// Sizes of the immutable in-process source and toolchain snapshots.
    #[must_use]
    pub fn byte_lengths(&self) -> (usize, usize) {
        (self.source_bytes.len(), self.toolchain_bytes.len())
    }

    /// Source and toolchain plans in that order. The pin retains the exact
    /// bytes on which each plan was validated.
    #[must_use]
    pub const fn plans(&self) -> (&CompileBundlePlan, &CompileBundlePlan) {
        (&self.source_plan, &self.toolchain_plan)
    }

    fn matches(&self, intent: &OwnerCompileIntent) -> bool {
        self.source_digest == intent.source && self.toolchain_digest == intent.toolchain
    }
}

fn read_pinned_asset(
    root: &std::fs::File,
    owner_uid: u32,
    purpose: &str,
    expected: ContentDigest,
    max_bytes: u64,
) -> Result<Box<[u8]>, CompilePreflightError> {
    let name = format!("{purpose}-{expected}");
    let file = open_beneath(root.as_fd(), Path::new(&name), OpenMode::read_only())
        .map_err(|_| CompilePreflightError::InputPin)?;
    ensure_private_regular(&file).map_err(|_| CompilePreflightError::InputPin)?;
    let meta = file.metadata().map_err(|_| CompilePreflightError::InputPin)?;
    let len = meta.len();
    if meta.uid() != owner_uid || len == 0 || len > max_bytes {
        return Err(CompilePreflightError::InputPin);
    }
    let limit = len + 1;
    let capacity = usize::try_from(limit).map_err(|_| CompilePreflightError::InputPin)?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(capacity).map_err(|_| CompilePreflightError::InputPin)?;
    file.take(limit)
        .read_to_end(&mut bytes)
        .map_err(|_| CompilePreflightError::InputPin)?;
    if u64::try_from(bytes.len()).ok() != Some(len) || ContentDigest::of(&bytes) != expected {
        return Err(CompilePreflightError::InputPin);
    }
    Ok(bytes.into_boxed_slice())
}

/// Bindings resolved by the local job owner, not received in a client request.
#[derive(Debug)]
pub struct OwnerCompileIntent {
    /// Immutable durable authority of the job, including its authenticated submitter.
    pub scope: JobScope,
    /// Work ID chosen for this exact operation before issuing its approval.
    pub work: WorkId,
    /// Exact revision of the prepared Ready record accepted for preflight.
    pub record_revision: u64,
    /// Expected digest of the source bundle, not a branch name.
    pub source: ContentDigest,
    /// Expected digest of the complete offline toolchain input bundle.
    pub toolchain: ContentDigest,
    /// Owning session in the trusted approval store.
    pub approval_session: SessionId,
    /// Unique request ID in that session.
    pub approval_request: ItemId,
    /// Call identity associated with this exact receipt.
    pub approval_call: ToolCallId,
}

/// A trusted observation of the exact execution worker and current lane policy.
pub struct CompilePlacement<'a> {
    /// Current policy snapshot, not a lookalike replacement with the same number.
    pub policy: &'a CloudWorkloadPolicy,
    /// Advisory decision for an explicit observed target.
    pub advice: &'a WorkloadDecision,
    /// Independently verified current observation of that target.
    pub worker: &'a ObservedDeployment,
    /// Current server clock, not taken from the caller or worker lease.
    pub now: UnixMillis,
}

/// Refusal deliberately discloses no cross-tenant IDs or raw store data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompilePreflightError {
    Owner,
    InputPin,
    InputManifest,
    Placement,
    Binding,
    Receipt,
    Record,
}

impl fmt::Display for CompilePreflightError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Owner => "compile preflight requires the exact authenticated local owner",
            Self::InputPin => "compile preflight requires exact owner-private source and toolchain snapshots",
            Self::InputManifest => "compile input manifest failed closed validation",
            Self::Placement => "compile preflight placement is stale, unavailable, or not compile",
            Self::Binding => "compile preflight cannot construct the registered job contract",
            Self::Receipt => "compile preflight lacks a current exact owner approval receipt",
            Self::Record => "compile preflight durable job is missing, changed, or already claimed",
        })
    }
}

impl std::error::Error for CompilePreflightError {}

/// Canonical local values for one registered recipe; not signed or executable
/// on their own. Preparation does not write either store or create a job.
#[derive(Debug)]
pub struct CompileRegistration {
    intent: OwnerCompileIntent,
    approval_payload: Value,
    job_input: JobSpecEnvelope,
}

/// Exact in-process snapshot from one owner-local store. There is deliberately
/// no claim, work-ID-only dispatch, or getter exposing the runnable job.
///
/// An eventual launch path must consume this exact record through
/// CoordinatorStore::claim_existing_job after safely materializing these
/// exact snapshots and verifying the installed executable/toolchain binding
/// through launch. The byte pins themselves cannot supply that guarantee.
pub struct CompileClaimPrerequisite<'a, S: CoordinatorStore> {
    registration: &'a CompileRegistration,
    approvals: &'a ApprovalStore,
    jobs: &'a S,
    inputs: &'a PinnedCompileInputs,
    record: StoredJob,
    checked_at: UnixMillis,
}

impl<S: CoordinatorStore> CompileClaimPrerequisite<'_, S> {
    /// Re-read the receipt and exact Ready record from the same owner stores.
    /// Refuses changes to the full stored record, not just its revision or
    /// executable fields. This remains a read-only snapshot comparison.
    ///
    /// # Errors
    /// Stale time, changed job, or any current preflight refusal.
    pub fn revalidate(
        &self,
        owner: &Principal,
        placement: CompilePlacement<'_>,
    ) -> Result<(), CompilePreflightError> {
        if placement.now.0 < self.checked_at.0 {
            return Err(CompilePreflightError::Placement);
        }
        let current = self.registration.verified_ready_snapshot(
            owner,
            self.inputs,
            placement,
            self.approvals,
            self.jobs,
        )?;
        if current != self.record {
            return Err(CompilePreflightError::Record);
        }
        Ok(())
    }
}

impl CompileRegistration {
    /// Build the fixed template contract using only host-resolved context.
    /// The caller must have independently authenticated the local owner and
    /// selected the exact work record, checkout, approval and lane.
    ///
    /// # Errors
    /// A non-owner principal, non-compile decision, or invalid typed binding.
    pub fn prepare(
        intent: OwnerCompileIntent,
        owner: &Principal,
        inputs: &PinnedCompileInputs,
        placement: CompilePlacement<'_>,
    ) -> Result<Self, CompilePreflightError> {
        if !owner_matches(owner, &intent.scope) {
            return Err(CompilePreflightError::Owner);
        }
        if !inputs.matches(&intent) {
            return Err(CompilePreflightError::InputPin);
        }
        let payload = bound_payload(&intent, owner, placement)?;
        let job_input = expected_job_input(&intent.scope, &payload)?;
        Ok(Self {
            intent,
            approval_payload: payload,
            job_input,
        })
    }

    /// Exact payload the trusted approval issuer must attach to this request.
    #[must_use]
    pub const fn approval_payload(&self) -> &Value {
        &self.approval_payload
    }

    /// Expected process record payload for the trusted local job owner.
    #[must_use]
    pub const fn job_input(&self) -> &JobSpecEnvelope {
        &self.job_input
    }

    /// Refuse unless the exact stored owner receipt and untouched Ready record
    /// still match the pinned inputs and current worker policy.
    /// Reads the owner-local stores without writing, claiming or launching.
    ///
    /// Before a future dispatch, the owner still needs an executable/toolchain
    /// pin, stable source materialization, worker re-admission and an atomic
    /// exact-record claim. Passing this check does not make those optional.
    ///
    /// # Errors
    /// Missing, mismatched, expired, scheduled, cancelled or used evidence.
    pub fn preflight<'a, S: CoordinatorStore>(
        &'a self,
        owner: &Principal,
        inputs: &'a PinnedCompileInputs,
        placement: CompilePlacement<'_>,
        approvals: &'a ApprovalStore,
        jobs: &'a S,
    ) -> Result<CompileClaimPrerequisite<'a, S>, CompilePreflightError> {
        let checked_at = placement.now;
        let record = self.verified_ready_snapshot(
            owner,
            inputs,
            placement,
            approvals,
            jobs,
        )?;
        Ok(CompileClaimPrerequisite {
            registration: self,
            approvals,
            jobs,
            inputs,
            record,
            checked_at,
        })
    }

    fn verified_ready_snapshot(
        &self,
        owner: &Principal,
        inputs: &PinnedCompileInputs,
        placement: CompilePlacement<'_>,
        approvals: &ApprovalStore,
        jobs: &impl CoordinatorStore,
    ) -> Result<StoredJob, CompilePreflightError> {
        let intent = &self.intent;
        if !owner_matches(owner, &intent.scope) {
            return Err(CompilePreflightError::Owner);
        }
        if !inputs.matches(intent) {
            return Err(CompilePreflightError::InputPin);
        }
        let now = timestamp(placement.now)?;
        if bound_payload(intent, owner, placement)? != self.approval_payload {
            return Err(CompilePreflightError::Placement);
        }

        let resolution = approvals
            .resolution(&intent.approval_session, &intent.approval_request)
            .map_err(|_| CompilePreflightError::Receipt)?
            .ok_or(CompilePreflightError::Receipt)?;
        let expires_at = resolution
            .resolved_at
            .checked_add(MAX_APPROVAL_AGE)
            .map_err(|_| CompilePreflightError::Receipt)?;
        if resolution.session != intent.approval_session
            || resolution.request != intent.approval_request
            || resolution.call_id != intent.approval_call
            || resolution.tenant.as_ref() != Some(intent.scope.tenant())
            || &resolution.actor != intent.scope.submitter()
            || resolution.decision != ReviewDecision::ApprovedOnce
            || now < resolution.resolved_at
            || now >= expires_at
        {
            return Err(CompilePreflightError::Receipt);
        }
        let stored_payload = approvals
            .payload(&intent.approval_session, &intent.approval_request)
            .map_err(|_| CompilePreflightError::Receipt)?;
        if stored_payload.as_ref() != Some(&self.approval_payload) {
            return Err(CompilePreflightError::Receipt);
        }

        let record = jobs
            .load_job(&intent.work)
            .map_err(|_| CompilePreflightError::Record)?;
        let input =
            serde_json::to_value(&self.job_input).map_err(|_| CompilePreflightError::Binding)?;
        if record.job.id != intent.work
            || record.scope != intent.scope
            || record.revision != intent.record_revision
            || record.job.kind != JobKind::Custom("process".to_owned())
            || record.job.state != JobState::Ready
            || now < record.not_before
            || record.job.attempts != 0
            || record.lease_epoch != 0
            || record.lease.is_some()
            || record.cancellation.is_some()
            || record.completion.is_some()
            || record.input != input
        {
            return Err(CompilePreflightError::Record);
        }
        Ok(record)
    }
}

fn owner_matches(owner: &Principal, scope: &JobScope) -> bool {
    owner.kind() == PrincipalKind::Human
        && matches!(owner.surface(), IngressSurface::Cli | IngressSurface::Tui)
        && owner.tier() == PermissionTier::Owner
        && !owner.id().is_empty()
        && owner.approval_actor().as_ref() == Some(scope.submitter())
}

fn timestamp(now: UnixMillis) -> Result<Timestamp, CompilePreflightError> {
    let millis = i64::try_from(now.0).map_err(|_| CompilePreflightError::Placement)?;
    Timestamp::from_millisecond(millis).map_err(|_| CompilePreflightError::Placement)
}

fn bound_payload(
    intent: &OwnerCompileIntent,
    owner: &Principal,
    placements: CompilePlacement<'_>,
) -> Result<Value, CompilePreflightError> {
    let candidate = placements.advice.candidate();
    if candidate.workload() != CloudWorkload::Compile {
        return Err(CompilePreflightError::Placement);
    }
    placements
        .policy
        .revalidate_advice(placements.advice, placements.worker, placements.now)
        .map_err(|_| CompilePreflightError::Placement)?;
    Ok(json!({
        "domain": APPROVAL_DOMAIN,
        "owner_id": owner.id(),
        "scope": intent.scope,
        "work": intent.work,
        "record_revision": intent.record_revision,
        "source": intent.source,
        "toolchain": intent.toolchain,
        "approval_session": intent.approval_session,
        "approval_request": intent.approval_request,
        "approval_call": intent.approval_call,
        "node": candidate.node(),
        "instance": candidate.instance(),
        "policy_revision": candidate.policy_revision().get(),
        "activation_epoch": candidate.activation_epoch().get(),
        "backend": candidate.runtime().backend.as_str(),
    }))
}

fn expected_job_input(
    scope: &JobScope,
    payload: &Value,
) -> Result<JobSpecEnvelope, CompilePreflightError> {
    let scope_bytes = serde_json::to_vec(scope).map_err(|_| CompilePreflightError::Binding)?;
    let scope_id = JobScopeId::new(format!("cloud-compile:{}", ContentDigest::of(&scope_bytes)))
        .map_err(|_| CompilePreflightError::Binding)?;
    let payload_bytes = serde_json::to_vec(payload).map_err(|_| CompilePreflightError::Binding)?;
    let receipt_key =
        IdempotencyKey::new(format!("cloud-compile-v3:{}", ContentDigest::of(&payload_bytes)))
            .map_err(|_| CompilePreflightError::Binding)?;
    let spec = JobSpec::command("cargo")
        .args(["test", "--locked", "--offline"])
        .resources(ResourceRequest {
            cpu_max: Some(CpuBandwidth::cpus(2)),
            memory_max: Some(8 * 1024 * 1024 * 1024),
            pids_max: Some(256),
            wall_timeout: Some(SignedDuration::from_secs(600)),
            ..ResourceRequest::default()
        })
        .sandbox(SandboxProfileName::NoNetwork)
        .sandbox_requirement(SandboxRequirement::Required)
        .scope(scope_id)
        .idempotency_key(receipt_key)
        .build()
        .map_err(|_| CompilePreflightError::Binding)?;
    JobSpecEnvelope::new(spec).map_err(|_| CompilePreflightError::Binding)
}
