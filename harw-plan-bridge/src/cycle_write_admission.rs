//! Trusted admission of a `ProposePatch` transition (W05 first slice).
//!
//! No executor writes here: a successful admission is a proof object that a
//! later write adapter must present; it is not a write grant by itself. The
//! library code of this module contains no file write of any kind (no create,
//! truncate, rename, remove or permission change). Its only file access is the
//! bounded, read-only preimage re-read of [`admit_patch`] through the
//! workspace-bound [`Scope`]. (The `#[cfg(test)]` fixtures write into
//! throwaway temp workspaces to set files up and to perturb them; that code
//! is not part of the library.)
//!
//! # Trust boundary
//!
//! ```text
//! model proposal      CycleProposal::ProposePatch { target ids, evidence id }
//!   -> admit_cycle        ids inside the write ceiling, evidence id known
//!   -> journaled step     InFlightStep, durable before anything runs
//!   -> PatchProposal::from_admitted
//!                         trusted: id -> path through a WriteTargetMap,
//!                         evidence must be observed repository evidence
//!                         whose locator is that very path
//!   -> admit_patch        WriteWorkspace permission + allowed write target
//!                         + scope-safe path + preimage re-read
//!   -> AdmittedPatch      proof object, not a grant
//! ```
//!
//! - The model only names *target ids*. Which workspace-relative path an id
//!   stands for is decided by the trusted runtime in a [`WriteTargetMap`],
//!   which is deliberately not `Deserialize`. A [`PatchProposal`] is likewise
//!   not `Deserialize` and is built from a *journaled, admitted* step, never
//!   from model JSON directly.
//! - The preimage digest is not taken from the model either: it is the digest
//!   of the trusted `Repository` evidence record the step referenced, and that
//!   record's locator must name the same file as the target.
//! - [`admit_patch`] checks, in this order: the authority holds
//!   [`Permission::WriteWorkspace`]; the proposal belongs to the intent of the
//!   admission and its target is in [`CycleAdmission::allowed_write_targets`];
//!   the trusted [`WriteTargetMap`] still maps that target to exactly the
//!   proposal's path; the owner worker id is sane; the [`Scope`] is the one of
//!   the authority's workspace; the path passes `Scope::rel_readable` (no
//!   `..`, no absolute path outside the root, no secret path such as `.env`
//!   or `*.pem`); and the file, re-read beneath the workspace root (regular
//!   files only, symlinks are never followed, bounded size), still has the
//!   digest the evidence recorded. Every refusal is fail-closed: nothing is
//!   created, changed or reserved.
//!
//! # What an [`AdmittedPatch`] is not
//!
//! It is a snapshot taken at `admit` time. It cannot be forged outside this
//! module (private fields), but it is `Clone`, so holding one grants nothing.
//! The write adapter that consumes it must still re-verify the preimage at
//! write time on its own exclusive open (the file may change after the
//! check), hold the per-file lease for the target, check its lease epoch, and
//! apply approval, worktree and rollback policy. Creating a *new* file is not
//! admitted in this slice: there is no preimage to compare, so the re-read
//! fails closed as [`PatchRefusal::Unreadable`].
//!
//! # Hand-built proposals
//!
//! [`PatchProposal`] has public fields (so callers can read it) and could be
//! built or edited by hand. [`admit_patch`] therefore does not trust the
//! `target id -> relative path` binding it carries: it receives the trusted
//! [`WriteTargetMap`] and re-derives the path from the target id, so a
//! proposal naming an allowed target but a different file is refused. The
//! intent binding, the target admission and the owner id are re-checked the
//! same way. What `admit_patch` cannot re-check is the *provenance* of
//! `evidence_id` and `expected_preimage`: it has no checkpoint. It only proves
//! that the file currently has the digest the proposal names. The link from
//! that digest to observed repository evidence is established by
//! [`PatchProposal::from_admitted`], which is the one constructor trusted code
//! should use.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path};

use harw_authority::{AuthorityContext, Permission};
use harw_tool_fsread::scope::Scope;
use harw_types::ContentDigest;

use crate::cycle_explorer::{DEFAULT_MAX_READ_BYTES, normalized, read_beneath};
use crate::cycle_runtime::InFlightStep;
use crate::intent_cycle::{
    CycleAdmission, CycleCheckpoint, CycleProposal, EvidenceSourceKind, EvidenceTrust,
    is_prompt_safe_char,
};

const MAX_TARGET_ID_BYTES: usize = 256;
const MAX_PATH_BYTES: usize = 4096;

/// Why a [`WriteTargetMap`] could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteTargetError {
    /// The target id is empty, too long or contains a character outside the
    /// prompt-safe set (the set `CycleAdmission::new` accepts for ids).
    InvalidId(String),
    /// The id is mapped twice.
    DuplicateId(String),
    /// The path is empty, absolute, contains `.`/`..`/NUL, or is too long.
    InvalidPath { id: String },
}

impl fmt::Display for WriteTargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId(id) => write!(f, "invalid write target id '{id}'"),
            Self::DuplicateId(id) => write!(f, "write target id '{id}' is mapped twice"),
            Self::InvalidPath { id } => {
                write!(f, "write target '{id}' maps to an invalid workspace path")
            }
        }
    }
}

impl std::error::Error for WriteTargetError {}

/// Trusted mapping `write target id -> workspace-relative path`.
///
/// Built by the trusted runtime (not by model output); intentionally neither
/// `Deserialize` nor constructible from admission data. Mirrors
/// `ReadTargetMap`: secret-path policy is not duplicated here, it is applied
/// by `Scope::rel_readable` in [`admit_patch`], the single source of that
/// rule.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WriteTargetMap {
    paths: BTreeMap<String, String>,
}

impl WriteTargetMap {
    /// Builds the map from `(id, relative path)` pairs.
    ///
    /// # Errors
    /// [`WriteTargetError`] for an empty/oversized id or one with a character
    /// outside the prompt-safe set, a duplicate id, or a path that is empty,
    /// absolute, longer than 4096 bytes, or contains NUL, `.` or `..`
    /// components.
    pub fn new<I, K, V>(entries: I) -> Result<Self, WriteTargetError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let mut paths = BTreeMap::new();
        for (id, path) in entries {
            let (id, path) = (id.into(), path.into());
            if id.is_empty()
                || id.len() > MAX_TARGET_ID_BYTES
                || !id.chars().all(is_prompt_safe_char)
            {
                return Err(WriteTargetError::InvalidId(id));
            }
            if !is_plain_relative(&path) {
                return Err(WriteTargetError::InvalidPath { id });
            }
            if paths.insert(id.clone(), path).is_some() {
                return Err(WriteTargetError::DuplicateId(id));
            }
        }
        Ok(Self { paths })
    }

    /// The workspace-relative path for `id`, if the runtime mapped it.
    #[must_use]
    pub fn path(&self, id: &str) -> Option<&str> {
        self.paths.get(id).map(String::as_str)
    }

    /// Mapped target ids, e.g. to derive `CycleTargets::write`.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.paths.keys().map(String::as_str)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

/// Only plain, normal path components: no root, prefix, `.` or `..`.
///
/// Same rule as the read side (`ReadTargetMap`); kept private there, so it is
/// restated here.
fn is_plain_relative(path: &str) -> bool {
    if path.is_empty() || path.len() > MAX_PATH_BYTES || path.contains('\0') {
        return false;
    }
    let mut components = Path::new(path).components().peekable();
    components.peek().is_some() && components.all(|c| matches!(c, Component::Normal(_)))
}

/// An owner worker id is usable when it is not blank and has no control
/// characters (it ends up in lease records and logs).
fn is_valid_owner(owner: &str) -> bool {
    !owner.trim().is_empty() && !owner.chars().any(char::is_control)
}

/// One single-file patch proposal derived from an admitted step.
///
/// Not `Deserialize`: it is built by [`PatchProposal::from_admitted`] from
/// trusted state. The fields are public for reading; see the module docs
/// ("Hand-built proposals") for why they must not be edited before
/// [`admit_patch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchProposal {
    /// Intent the cycle was admitted for.
    pub intent_id: String,
    /// Revision of that intent.
    pub intent_revision: u64,
    /// Write target id (as admitted), not a path.
    pub target: String,
    /// Normalized workspace-relative path the trusted map assigned to `target`.
    pub relative_path: String,
    /// Digest the file must still have when the patch is admitted, taken from
    /// the observed repository evidence the step referenced.
    pub expected_preimage: ContentDigest,
    /// Id of that evidence record.
    pub evidence_id: String,
    /// Worker that will own the file lease and the write.
    pub owner_worker: String,
    /// The journaled step's idempotency key; the write adapter deduplicates
    /// the external effect with it.
    pub idempotency_key: String,
}

/// Why a patch was not admitted. Every variant is a fail-closed refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchRefusal {
    /// The step is not a `ProposePatch`.
    NotAPatch,
    /// Not exactly one target (this slice admits single-file patches only;
    /// an empty target set is refused here too).
    MultiFile,
    /// The target id is not in the trusted [`WriteTargetMap`] (checked by
    /// both [`PatchProposal::from_admitted`] and [`admit_patch`]).
    UnknownTarget,
    /// The target (or the intent) is not admitted by the [`CycleAdmission`].
    TargetNotAdmitted,
    /// The referenced evidence id is not in the checkpoint.
    MissingEvidence,
    /// The evidence is not `Repository` evidence of trust `Observed` or
    /// `Verified`.
    EvidenceNotObserved,
    /// The evidence locator names a different file than the target path, or
    /// (in [`admit_patch`]) the proposal's path is not the one the trusted
    /// map assigns to its target.
    EvidenceLocatorMismatch,
    /// The path is outside the workspace, a protected (secret) path, or the
    /// [`Scope`] is not the one of the authority's workspace.
    PathEscapesWorkspace,
    /// The authority lacks [`Permission::WriteWorkspace`].
    WritePermissionMissing,
    /// The file no longer has the digest the evidence recorded.
    PreimageMismatch,
    /// The file could not be re-read beneath the workspace root (missing,
    /// not a regular file, symlink, over the size cap, I/O error).
    Unreadable,
    /// The owner worker id is empty, blank or contains control characters.
    InvalidOwner,
}

impl fmt::Display for PatchRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::NotAPatch => "the step is not a patch proposal",
            Self::MultiFile => "only single-file patch proposals are admitted in this slice",
            Self::UnknownTarget => "the patch target is not in the trusted write target map",
            Self::TargetNotAdmitted => "the patch target is not admitted for this cycle",
            Self::MissingEvidence => "the patch proposal references unknown evidence",
            Self::EvidenceNotObserved => {
                "the patch evidence is not observed or verified repository evidence"
            }
            Self::EvidenceLocatorMismatch => {
                "the patch evidence locator does not match the target path"
            }
            Self::PathEscapesWorkspace => "the patch path is outside the workspace or protected",
            Self::WritePermissionMissing => "WriteWorkspace permission missing",
            Self::PreimageMismatch => "the file changed since the evidence was recorded",
            Self::Unreadable => "the patch target could not be re-read beneath the workspace",
            Self::InvalidOwner => "owner worker id is empty or unsafe",
        };
        f.write_str(text)
    }
}

impl std::error::Error for PatchRefusal {}

impl PatchProposal {
    /// Builds the proposal from a journaled, admitted step (never from model
    /// JSON directly).
    ///
    /// `step.proposal` must be a [`CycleProposal::ProposePatch`] with exactly
    /// one target that `targets` maps to a path, and its evidence record in
    /// `state` must be `Repository` evidence of trust `Observed` or
    /// `Verified` whose locator names that same file (compared in normalized
    /// form, so `a//b` and `a/b` are one file). The expected preimage is that
    /// record's digest. This function reads no file and does not consult the
    /// [`CycleAdmission`]; [`admit_patch`] does.
    ///
    /// # Errors
    /// [`PatchRefusal::NotAPatch`], [`PatchRefusal::InvalidOwner`] (`owner_worker`
    /// is empty, blank or contains control characters),
    /// [`PatchRefusal::MultiFile`] (zero or more than one target),
    /// [`PatchRefusal::UnknownTarget`],
    /// [`PatchRefusal::MissingEvidence`], [`PatchRefusal::EvidenceNotObserved`]
    /// or [`PatchRefusal::EvidenceLocatorMismatch`].
    pub fn from_admitted(
        step: &InFlightStep,
        state: &CycleCheckpoint,
        targets: &WriteTargetMap,
        owner_worker: &str,
    ) -> Result<Self, PatchRefusal> {
        let CycleProposal::ProposePatch {
            targets: named,
            evidence_id,
        } = &step.proposal
        else {
            return Err(PatchRefusal::NotAPatch);
        };
        if !is_valid_owner(owner_worker) {
            return Err(PatchRefusal::InvalidOwner);
        }
        if named.len() != 1 {
            return Err(PatchRefusal::MultiFile);
        }
        let Some(target) = named.iter().next() else {
            return Err(PatchRefusal::MultiFile);
        };
        let path = targets.path(target).ok_or(PatchRefusal::UnknownTarget)?;
        let record = state
            .evidence
            .get(evidence_id)
            .ok_or(PatchRefusal::MissingEvidence)?;
        if record.source != EvidenceSourceKind::Repository {
            return Err(PatchRefusal::EvidenceNotObserved);
        }
        match record.trust {
            EvidenceTrust::Observed | EvidenceTrust::Verified => {}
            EvidenceTrust::Reported => return Err(PatchRefusal::EvidenceNotObserved),
        }
        let relative_path = normalized(path);
        if normalized(&record.locator) != relative_path {
            return Err(PatchRefusal::EvidenceLocatorMismatch);
        }
        Ok(Self {
            intent_id: state.intent_id.clone(),
            intent_revision: state.intent_revision,
            target: target.clone(),
            relative_path,
            expected_preimage: record.digest,
            evidence_id: evidence_id.clone(),
            owner_worker: owner_worker.to_owned(),
            idempotency_key: step.idempotency_key.clone(),
        })
    }
}

/// Proof that a patch passed every trusted check at [`admit_patch`] time.
///
/// Fields are private: the only way to obtain one is a successful
/// [`admit_patch`]. It is a proof object, not a grant; see the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedPatch {
    proposal: PatchProposal,
    checked_digest: ContentDigest,
}

impl AdmittedPatch {
    /// The proposal that was admitted.
    #[must_use]
    pub fn proposal(&self) -> &PatchProposal {
        &self.proposal
    }

    /// The digest of the file as re-read at admission time. Equal to the
    /// proposal's expected preimage by construction.
    #[must_use]
    pub fn checked_digest(&self) -> ContentDigest {
        self.checked_digest
    }
}

/// Admits `proposal` against the trusted `admission`, `targets`, `authority`
/// and `scope`, or refuses it. Performs no write.
///
/// Checks, in order, all before the file is opened:
/// 1. `authority` holds [`Permission::WriteWorkspace`]
///    ([`PatchRefusal::WritePermissionMissing`]);
/// 2. the proposal's intent id and revision equal the admission's, and the
///    target is in [`CycleAdmission::allowed_write_targets`]
///    ([`PatchRefusal::TargetNotAdmitted`]);
/// 3. `targets` maps the proposal's target ([`PatchRefusal::UnknownTarget`])
///    to a path whose normalized form equals `proposal.relative_path`
///    ([`PatchRefusal::EvidenceLocatorMismatch`]); a hand-built proposal
///    cannot pair an allowed target id with a different file;
/// 4. `owner_worker` is not blank and has no control characters
///    ([`PatchRefusal::InvalidOwner`]);
/// 5. `scope` is the scope of the authority's workspace root, and the path
///    passes `Scope::rel_readable` ([`PatchRefusal::PathEscapesWorkspace`]).
///
/// Then the file is re-read beneath `scope` (bounded by
/// `DEFAULT_MAX_READ_BYTES`; a larger file is refused, never truncated) and
/// its digest must equal `expected_preimage` ([`PatchRefusal::Unreadable`],
/// [`PatchRefusal::PreimageMismatch`]).
///
/// The proposal should come from [`PatchProposal::from_admitted`]; what this
/// function cannot re-check about a hand-built one is described in the module
/// docs ("Hand-built proposals"). Blocking file read: call from
/// `spawn_blocking`.
///
/// # Errors
/// A [`PatchRefusal`]; nothing has been created or changed in that case.
pub fn admit_patch(
    proposal: &PatchProposal,
    admission: &CycleAdmission,
    targets: &WriteTargetMap,
    authority: &AuthorityContext,
    scope: &Scope,
) -> Result<AdmittedPatch, PatchRefusal> {
    if !authority.permissions().contains(Permission::WriteWorkspace) {
        return Err(PatchRefusal::WritePermissionMissing);
    }
    // A proposal minted for another intent (or revision) must not be admitted
    // under this admission, even when the target id happens to coincide.
    let intent = admission.intent();
    if proposal.intent_id != intent.id || proposal.intent_revision != intent.revision {
        return Err(PatchRefusal::TargetNotAdmitted);
    }
    if !admission.allowed_write_targets().contains(&proposal.target) {
        return Err(PatchRefusal::TargetNotAdmitted);
    }
    // The target id -> path binding is the trusted map's, not the proposal's.
    let Some(mapped) = targets.path(&proposal.target) else {
        return Err(PatchRefusal::UnknownTarget);
    };
    if normalized(mapped) != proposal.relative_path {
        return Err(PatchRefusal::EvidenceLocatorMismatch);
    }
    if !is_valid_owner(&proposal.owner_worker) {
        return Err(PatchRefusal::InvalidOwner);
    }
    // The permission above was checked on `authority`; the file must be
    // looked at in that same workspace and nowhere else.
    if scope.root() != authority.workspace().canonical_root() {
        return Err(PatchRefusal::PathEscapesWorkspace);
    }
    if scope.rel_readable(&proposal.relative_path).is_err() {
        return Err(PatchRefusal::PathEscapesWorkspace);
    }
    let outcome = read_beneath(
        scope,
        &proposal.relative_path,
        DEFAULT_MAX_READ_BYTES,
        DEFAULT_MAX_READ_BYTES,
    )
    .map_err(|_| PatchRefusal::Unreadable)?;
    if outcome.digest != proposal.expected_preimage {
        return Err(PatchRefusal::PreimageMismatch);
    }
    Ok(AdmittedPatch {
        proposal: proposal.clone(),
        checked_digest: outcome.digest,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{TenantId, WorkspaceId};

    use super::*;
    use crate::intent_cycle::{CycleLimits, CycleTargets, IntentBinding};
    use crate::test_support::{TestError, TestResult, ctx};

    /// Content the evidence vouches for.
    const OLD: &[u8] = b"pub fn old() {}\n";

    /// A real workspace directory below the harness root (the registry
    /// rejects roots outside it) that is removed on drop.
    struct Workspace {
        dir: tempfile::TempDir,
    }

    impl Workspace {
        fn new() -> TestResult<Self> {
            let dir = tempfile::Builder::new()
                .prefix("write-admission-ws-")
                .tempdir_in(env!("CARGO_MANIFEST_DIR"))
                .map_err(ctx("workspace dir"))?;
            Ok(Self { dir })
        }

        fn write(&self, relative: &str, content: &[u8]) -> TestResult {
            let path = self.dir.path().join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(ctx("mkdir"))?;
            }
            std::fs::write(path, content).map_err(ctx("write file"))
        }

        fn authority(&self, permissions: &[Permission]) -> TestResult<AuthorityContext> {
            let harness_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .ok_or(TestError::Missing("crate has a workspace parent"))?
                .to_path_buf();
            let name = self
                .dir
                .path()
                .file_name()
                .ok_or(TestError::Missing("workspace dir name"))?;
            let tenant = TenantId::from_str("write-admission-tenant");
            let workspace = WorkspaceId::from_str("write-admission-tests");
            let binding = WorkspaceRegistry::build(
                &harness_root,
                [WorkspaceRegistration {
                    tenant: tenant.clone(),
                    workspace: workspace.clone(),
                    root: PathBuf::from("harw-plan-bridge").join(name),
                }],
            )
            .map_err(ctx("workspace registers"))?
            .resolve(&tenant, &workspace)
            .map_err(ctx("workspace resolves"))?;
            Ok(SandboxSpec::from_resolved(
                binding,
                PermissionSet::from_policy(permissions.iter().copied()),
            )
            .authority()
            .clone())
        }

        /// Digest of every regular file below the workspace, by relative path.
        fn snapshot(&self) -> TestResult<BTreeMap<String, ContentDigest>> {
            let mut files = BTreeMap::new();
            collect(self.dir.path(), self.dir.path(), &mut files)?;
            Ok(files)
        }
    }

    fn collect(root: &Path, dir: &Path, files: &mut BTreeMap<String, ContentDigest>) -> TestResult {
        for entry in std::fs::read_dir(dir).map_err(ctx("read dir"))? {
            let entry = entry.map_err(ctx("dir entry"))?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(ctx("file type"))?;
            if file_type.is_dir() {
                collect(root, &path, files)?;
            } else if file_type.is_file() {
                let bytes = std::fs::read(&path).map_err(ctx("read file"))?;
                let relative = path.strip_prefix(root).map_err(ctx("relative path"))?;
                files.insert(
                    relative.to_string_lossy().into_owned(),
                    ContentDigest::of(&bytes),
                );
            }
        }
        Ok(())
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    fn write_rights() -> [Permission; 2] {
        [Permission::ReadWorkspace, Permission::WriteWorkspace]
    }

    fn admission_with(write_ids: &[&str]) -> TestResult<CycleAdmission> {
        CycleAdmission::new(
            IntentBinding {
                id: "intent-w05".to_owned(),
                revision: 1,
                digest: ContentDigest::of(b"intent-w05@1"),
                predecessor: None,
                acceptance: set(&["patched"]),
            },
            CycleLimits {
                max_transitions: 10,
                max_chain_depth: 1,
                max_spawn_depth: 1,
                max_parallel_segments: 4,
                max_stall_transitions: 3,
            },
            CycleTargets {
                read: set(&["source"]),
                write: set(write_ids),
                ..CycleTargets::default()
            },
        )
        .map_err(|_| TestError::Missing("admission builds"))
    }

    fn evidence(
        id: &str,
        locator: &str,
        digest: ContentDigest,
        source: EvidenceSourceKind,
        trust: EvidenceTrust,
    ) -> crate::intent_cycle::EvidenceRecord {
        crate::intent_cycle::EvidenceRecord {
            id: id.to_owned(),
            source,
            locator: locator.to_owned(),
            digest,
            trust,
        }
    }

    fn patch_step(targets: &[&str], evidence_id: &str) -> InFlightStep {
        step(CycleProposal::ProposePatch {
            targets: set(targets),
            evidence_id: evidence_id.to_owned(),
        })
    }

    fn step(proposal: CycleProposal) -> InFlightStep {
        InFlightStep {
            epoch: 1,
            sequence: 0,
            idempotency_key: "cyc-0".to_owned(),
            proposal,
        }
    }

    fn map(entries: &[(&str, &str)]) -> TestResult<WriteTargetMap> {
        WriteTargetMap::new(entries.iter().copied()).map_err(ctx("map builds"))
    }

    /// A workspace holding `src/lib.rs` (= [`OLD`]) with admission, authority,
    /// scope, a checkpoint that observed that file as `ev-1`, and a target map
    /// `code -> src/lib.rs`, `docs -> docs/guide.md`.
    struct Case {
        ws: Workspace,
        authority: AuthorityContext,
        admission: CycleAdmission,
        state: CycleCheckpoint,
        targets: WriteTargetMap,
        scope: Scope,
    }

    impl Case {
        fn new(permissions: &[Permission], write_ids: &[&str]) -> TestResult<Self> {
            let ws = Workspace::new()?;
            ws.write("src/lib.rs", OLD)?;
            let authority = ws.authority(permissions)?;
            let admission = admission_with(write_ids)?;
            let mut state = CycleCheckpoint::initial(&admission, &authority, 0);
            let observed = evidence(
                "ev-1",
                "src/lib.rs",
                ContentDigest::of(OLD),
                EvidenceSourceKind::Repository,
                EvidenceTrust::Observed,
            );
            state.evidence.insert(observed.id.clone(), observed);
            let scope = Scope::new(authority.workspace().canonical_root()).map_err(ctx("scope"))?;
            Ok(Self {
                ws,
                authority,
                admission,
                state,
                targets: map(&[("code", "src/lib.rs"), ("docs", "docs/guide.md")])?,
                scope,
            })
        }

        fn record(&mut self, record: crate::intent_cycle::EvidenceRecord) {
            self.state.evidence.insert(record.id.clone(), record);
        }

        fn proposal(&self, targets: &[&str], evidence_id: &str) -> TestResult<PatchProposal> {
            PatchProposal::from_admitted(
                &patch_step(targets, evidence_id),
                &self.state,
                &self.targets,
                "worker-1",
            )
            .map_err(ctx("proposal builds"))
        }

        fn admit(&self, proposal: &PatchProposal) -> Result<AdmittedPatch, PatchRefusal> {
            admit_patch(
                proposal,
                &self.admission,
                &self.targets,
                &self.authority,
                &self.scope,
            )
        }
    }

    #[test]
    fn target_map_validates_ids_and_paths() -> TestResult {
        assert!(WriteTargetMap::new([("a", "src/lib.rs")]).is_ok());
        for bad in ["", "/etc/passwd", "../x", "a/../b", "./a", "a\0b"] {
            assert_eq!(
                WriteTargetMap::new([("a", bad)]),
                Err(WriteTargetError::InvalidPath { id: "a".to_owned() }),
                "path {bad:?}"
            );
        }
        for bad in ["", " ", "a b", "a\nb", "ä"] {
            assert_eq!(
                WriteTargetMap::new([(bad, "x")]),
                Err(WriteTargetError::InvalidId(bad.to_owned())),
                "id {bad:?}"
            );
        }
        assert_eq!(
            WriteTargetMap::new([("a".repeat(MAX_TARGET_ID_BYTES + 1), "x".to_owned())]),
            Err(WriteTargetError::InvalidId(
                "a".repeat(MAX_TARGET_ID_BYTES + 1)
            ))
        );
        assert_eq!(
            WriteTargetMap::new([("a", "x"), ("a", "y")]),
            Err(WriteTargetError::DuplicateId("a".to_owned()))
        );
        let targets = map(&[("a", "x"), ("b", "y")])?;
        assert_eq!(targets.ids().collect::<Vec<_>>(), vec!["a", "b"]);
        assert_eq!(targets.path("a"), Some("x"));
        assert_eq!(targets.path("c"), None);
        assert_eq!(targets.len(), 2);
        assert!(!targets.is_empty());
        assert!(WriteTargetMap::default().is_empty());
        Ok(())
    }

    #[test]
    fn refusals_render_distinct_messages() {
        let all = [
            PatchRefusal::NotAPatch,
            PatchRefusal::MultiFile,
            PatchRefusal::UnknownTarget,
            PatchRefusal::TargetNotAdmitted,
            PatchRefusal::MissingEvidence,
            PatchRefusal::EvidenceNotObserved,
            PatchRefusal::EvidenceLocatorMismatch,
            PatchRefusal::PathEscapesWorkspace,
            PatchRefusal::WritePermissionMissing,
            PatchRefusal::PreimageMismatch,
            PatchRefusal::Unreadable,
            PatchRefusal::InvalidOwner,
        ];
        let messages: BTreeSet<String> = all.iter().map(ToString::to_string).collect();
        assert_eq!(messages.len(), all.len());
        assert!(messages.iter().all(|message| !message.is_empty()));
    }

    #[test]
    fn admitted_patch_carries_the_derived_proposal_and_checked_digest() -> TestResult {
        let case = Case::new(&write_rights(), &["code"])?;
        let proposal = case.proposal(&["code"], "ev-1")?;
        assert_eq!(
            proposal,
            PatchProposal {
                intent_id: "intent-w05".to_owned(),
                intent_revision: 1,
                target: "code".to_owned(),
                relative_path: "src/lib.rs".to_owned(),
                expected_preimage: ContentDigest::of(OLD),
                evidence_id: "ev-1".to_owned(),
                owner_worker: "worker-1".to_owned(),
                idempotency_key: "cyc-0".to_owned(),
            }
        );
        let before = case.ws.snapshot()?;
        let admitted = case.admit(&proposal).map_err(ctx("admits"))?;
        assert_eq!(admitted.proposal(), &proposal);
        assert_eq!(admitted.checked_digest(), ContentDigest::of(OLD));
        assert_eq!(case.ws.snapshot()?, before, "admission is read-only");
        Ok(())
    }

    #[test]
    fn unauthorized_path_fails_admission_before_any_write() -> TestResult {
        // `code` maps to a real file but is not in the admission's write set.
        let case = Case::new(&write_rights(), &["docs"])?;
        let before = case.ws.snapshot()?;
        assert_eq!(
            before.get("src/lib.rs"),
            Some(&ContentDigest::of(OLD)),
            "fixture file present"
        );
        let proposal = case.proposal(&["code"], "ev-1")?;
        assert_eq!(case.admit(&proposal), Err(PatchRefusal::TargetNotAdmitted));
        assert_eq!(case.ws.snapshot()?, before, "nothing on disk changed");
        Ok(())
    }

    #[test]
    fn foreign_modification_fails_preimage_match() -> TestResult {
        let case = Case::new(&write_rights(), &["code"])?;
        let proposal = case.proposal(&["code"], "ev-1")?;
        assert!(case.admit(&proposal).is_ok(), "unchanged file is admitted");
        // Someone else edits the file after the evidence was recorded.
        case.ws.write("src/lib.rs", b"pub fn foreign() {}\n")?;
        assert_eq!(case.admit(&proposal), Err(PatchRefusal::PreimageMismatch));
        Ok(())
    }

    #[test]
    fn multi_file_proposal_is_refused_in_first_slice() -> TestResult {
        let case = Case::new(&write_rights(), &["code", "docs"])?;
        let refused = PatchProposal::from_admitted(
            &patch_step(&["code", "docs"], "ev-1"),
            &case.state,
            &case.targets,
            "worker-1",
        );
        assert_eq!(refused, Err(PatchRefusal::MultiFile));
        let empty = PatchProposal::from_admitted(
            &patch_step(&[], "ev-1"),
            &case.state,
            &case.targets,
            "worker-1",
        );
        assert_eq!(empty, Err(PatchRefusal::MultiFile));
        Ok(())
    }

    #[test]
    fn symlink_escape_is_refused() -> TestResult {
        let mut case = Case::new(&write_rights(), &["code", "docs"])?;
        let outside = tempfile::tempdir().map_err(ctx("outside dir"))?;
        std::fs::write(outside.path().join("loot.rs"), b"outside").map_err(ctx("outside file"))?;
        std::os::unix::fs::symlink(
            outside.path().join("loot.rs"),
            case.ws.dir.path().join("link.rs"),
        )
        .map_err(ctx("file symlink"))?;
        std::os::unix::fs::symlink(outside.path(), case.ws.dir.path().join("dirlink"))
            .map_err(ctx("dir symlink"))?;
        case.targets = map(&[("code", "link.rs"), ("docs", "dirlink/loot.rs")])?;
        for (target, locator) in [("code", "link.rs"), ("docs", "dirlink/loot.rs")] {
            // The evidence even carries the digest of the outside content.
            case.record(evidence(
                &format!("ev-{target}"),
                locator,
                ContentDigest::of(b"outside"),
                EvidenceSourceKind::Repository,
                EvidenceTrust::Observed,
            ));
        }
        for target in ["code", "docs"] {
            let proposal = case.proposal(&[target], &format!("ev-{target}"))?;
            let refused = case.admit(&proposal);
            assert!(
                matches!(
                    refused,
                    Err(PatchRefusal::PathEscapesWorkspace | PatchRefusal::Unreadable)
                ),
                "{target}: {refused:?}"
            );
        }
        let loot = std::fs::read(outside.path().join("loot.rs")).map_err(ctx("read outside"))?;
        assert_eq!(loot, b"outside", "outside file untouched");
        Ok(())
    }

    #[test]
    fn missing_write_permission_fails_closed() -> TestResult {
        let case = Case::new(&[Permission::ReadWorkspace], &["code"])?;
        let before = case.ws.snapshot()?;
        let proposal = case.proposal(&["code"], "ev-1")?;
        assert_eq!(
            case.admit(&proposal),
            Err(PatchRefusal::WritePermissionMissing)
        );
        assert_eq!(case.ws.snapshot()?, before);
        Ok(())
    }

    #[test]
    fn evidence_locator_must_match_target_path() -> TestResult {
        let mut case = Case::new(&write_rights(), &["code"])?;
        case.record(evidence(
            "ev-other",
            "src/other.rs",
            ContentDigest::of(OLD),
            EvidenceSourceKind::Repository,
            EvidenceTrust::Observed,
        ));
        let refused = PatchProposal::from_admitted(
            &patch_step(&["code"], "ev-other"),
            &case.state,
            &case.targets,
            "worker-1",
        );
        assert_eq!(refused, Err(PatchRefusal::EvidenceLocatorMismatch));
        // The same file spelled with a doubled separator is the same file.
        case.record(evidence(
            "ev-spelled",
            "src//lib.rs",
            ContentDigest::of(OLD),
            EvidenceSourceKind::Repository,
            EvidenceTrust::Observed,
        ));
        let proposal = case.proposal(&["code"], "ev-spelled")?;
        assert_eq!(proposal.relative_path, "src/lib.rs");
        assert!(case.admit(&proposal).is_ok());
        Ok(())
    }

    #[test]
    fn evidence_must_be_observed_repository_evidence() -> TestResult {
        let mut case = Case::new(&write_rights(), &["code"])?;
        let digest = ContentDigest::of(OLD);
        for (id, source, trust) in [
            (
                "ev-reported",
                EvidenceSourceKind::Repository,
                EvidenceTrust::Reported,
            ),
            ("ev-tool", EvidenceSourceKind::Tool, EvidenceTrust::Observed),
            (
                "ev-human",
                EvidenceSourceKind::Human,
                EvidenceTrust::Verified,
            ),
        ] {
            case.record(evidence(id, "src/lib.rs", digest, source, trust));
            let refused = PatchProposal::from_admitted(
                &patch_step(&["code"], id),
                &case.state,
                &case.targets,
                "worker-1",
            );
            assert_eq!(refused, Err(PatchRefusal::EvidenceNotObserved), "{id}");
        }
        let missing = PatchProposal::from_admitted(
            &patch_step(&["code"], "ev-nope"),
            &case.state,
            &case.targets,
            "worker-1",
        );
        assert_eq!(missing, Err(PatchRefusal::MissingEvidence));
        case.record(evidence(
            "ev-verified",
            "src/lib.rs",
            digest,
            EvidenceSourceKind::Repository,
            EvidenceTrust::Verified,
        ));
        assert!(case.proposal(&["code"], "ev-verified").is_ok());
        Ok(())
    }

    #[test]
    fn non_patch_step_and_unmapped_target_are_refused() -> TestResult {
        let case = Case::new(&write_rights(), &["code"])?;
        let verify = step(CycleProposal::Verify {
            evidence_id: "ev-1".to_owned(),
        });
        assert_eq!(
            PatchProposal::from_admitted(&verify, &case.state, &case.targets, "worker-1"),
            Err(PatchRefusal::NotAPatch)
        );
        assert_eq!(
            PatchProposal::from_admitted(
                &patch_step(&["ghost"], "ev-1"),
                &case.state,
                &case.targets,
                "worker-1"
            ),
            Err(PatchRefusal::UnknownTarget)
        );
        Ok(())
    }

    #[test]
    fn proposal_of_another_intent_is_not_admitted() -> TestResult {
        let case = Case::new(&write_rights(), &["code"])?;
        let proposal = case.proposal(&["code"], "ev-1")?;
        let mut other_id = proposal.clone();
        other_id.intent_id = "intent-other".to_owned();
        assert_eq!(case.admit(&other_id), Err(PatchRefusal::TargetNotAdmitted));
        let mut other_revision = proposal;
        other_revision.intent_revision += 1;
        assert_eq!(
            case.admit(&other_revision),
            Err(PatchRefusal::TargetNotAdmitted)
        );
        Ok(())
    }

    #[test]
    fn scope_of_another_workspace_is_refused() -> TestResult {
        let case = Case::new(&write_rights(), &["code"])?;
        let proposal = case.proposal(&["code"], "ev-1")?;
        let elsewhere = tempfile::tempdir().map_err(ctx("other workspace"))?;
        std::fs::write(elsewhere.path().join("lib.rs"), OLD).map_err(ctx("other file"))?;
        let foreign = Scope::new(elsewhere.path()).map_err(ctx("foreign scope"))?;
        assert_eq!(
            admit_patch(
                &proposal,
                &case.admission,
                &case.targets,
                &case.authority,
                &foreign
            ),
            Err(PatchRefusal::PathEscapesWorkspace)
        );
        Ok(())
    }

    #[test]
    fn secret_path_and_missing_file_are_refused() -> TestResult {
        let mut case = Case::new(&write_rights(), &["code", "docs"])?;
        case.ws.write(".env", b"TOKEN=1")?;
        case.targets = map(&[("code", ".env"), ("docs", "src/new.rs")])?;
        case.record(evidence(
            "ev-env",
            ".env",
            ContentDigest::of(b"TOKEN=1"),
            EvidenceSourceKind::Repository,
            EvidenceTrust::Observed,
        ));
        case.record(evidence(
            "ev-new",
            "src/new.rs",
            ContentDigest::of(b""),
            EvidenceSourceKind::Repository,
            EvidenceTrust::Observed,
        ));
        let before = case.ws.snapshot()?;
        let secret = case.proposal(&["code"], "ev-env")?;
        assert_eq!(case.admit(&secret), Err(PatchRefusal::PathEscapesWorkspace));
        // Creating a new file has no preimage to compare: not admitted.
        let create = case.proposal(&["docs"], "ev-new")?;
        assert_eq!(case.admit(&create), Err(PatchRefusal::Unreadable));
        assert_eq!(case.ws.snapshot()?, before, "no file was created");
        Ok(())
    }

    #[test]
    fn hand_built_proposal_with_foreign_path_is_refused() -> TestResult {
        let case = Case::new(&write_rights(), &["code"])?;
        // A second file with the very same content, so only the target ->
        // path binding can stop the forged proposal.
        case.ws.write("src/other.rs", OLD)?;
        let proposal = case.proposal(&["code"], "ev-1")?;
        let mut forged = proposal.clone();
        forged.relative_path = "src/other.rs".to_owned();
        assert_eq!(
            case.admit(&forged),
            Err(PatchRefusal::EvidenceLocatorMismatch)
        );
        // The same file spelled differently is still the mapped file.
        let mut respelled = proposal;
        respelled.relative_path = "src//lib.rs".to_owned();
        assert_eq!(
            case.admit(&respelled),
            Err(PatchRefusal::EvidenceLocatorMismatch),
            "relative_path must be the normalized form the map yields"
        );
        Ok(())
    }

    #[test]
    fn hand_built_proposal_with_unmapped_target_is_refused() -> TestResult {
        // `ghost` is admitted by the cycle but the trusted map does not know it.
        let case = Case::new(&write_rights(), &["code", "ghost"])?;
        let mut forged = case.proposal(&["code"], "ev-1")?;
        forged.target = "ghost".to_owned();
        assert_eq!(case.admit(&forged), Err(PatchRefusal::UnknownTarget));
        Ok(())
    }

    #[test]
    fn owner_worker_must_be_non_empty_and_free_of_control_characters() -> TestResult {
        let case = Case::new(&write_rights(), &["code"])?;
        for bad in ["", "   ", "worker\n1", "wor\0ker"] {
            let refused = PatchProposal::from_admitted(
                &patch_step(&["code"], "ev-1"),
                &case.state,
                &case.targets,
                bad,
            );
            assert_eq!(refused, Err(PatchRefusal::InvalidOwner), "owner {bad:?}");
        }
        // A hand-built proposal cannot smuggle in a bad owner either.
        let mut forged = case.proposal(&["code"], "ev-1")?;
        forged.owner_worker = String::new();
        assert_eq!(case.admit(&forged), Err(PatchRefusal::InvalidOwner));
        Ok(())
    }
}
