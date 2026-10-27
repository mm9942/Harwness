//! W09a/W09b: job-local, idempotent consumer of the cycle outbox and the
//! bounded lesson recall for the next proposer prompt. Only project scope;
//! promotion to global stays a reviewed, manual step (W09c).
//!
//! # Learning (W09a)
//!
//! The intent-cycle driver commits typed, text-free
//! [`RunObservationV1`]s into the record's outbox together with the step's
//! checkpoint. [`drain_cycle_observations`] reads the pending ones and turns
//! every *learnable* failure category into one deterministic project
//! [`FactType::Pitfall`] fact. The fact text is built from enum names and
//! ids only (never from anything a model or an executor wrote), runs through
//! the learning gate, and is written under the project memories root the job
//! was started with. The global scope is never opened.
//!
//! Every pending entry is acknowledged once it has been dealt with:
//! learned, refused by the gate, or not learnable at all (admission refusals
//! are a normal part of a run, not a lesson). The fact name is a pure
//! function of the observation's dedupe key, so a crash between the write and
//! the acknowledgement only rewrites the same file on the next drain.
//!
//! # Recall (W09b)
//!
//! [`lesson_lines`] searches the same project root for lessons matching the
//! objective's keywords and returns at most
//! [`MAX_RECALL_TOP_K`](harw_knowledge::learning_recall::MAX_RECALL_TOP_K)
//! single, sanitized lines within the token ceiling of
//! [`select_lessons`]. The proposer renders them as low-trust data.

use std::collections::BTreeSet;
use std::fmt::Debug;
use std::path::Path;

use harw_job_runtime::LeaseToken;
use harw_knowledge::learning_recall::{
    LearningRecallRequest, LessonCandidate, MAX_RECALL_TOKENS, MAX_RECALL_TOP_K, select_lessons,
};
use harw_memory::facts::{Fact, FactScope, FactStore, FactType, slugify};
use harw_memory::learning_gate::{GateVerdict, screen};
use harw_memory::types::Timestamp;
use harw_plan_bridge::intent_cycle::is_prompt_safe_char;
use harw_plan_bridge::{CycleStore, ObservationCategory, RunObservationV1};

/// Tag carried by every fact this module writes; recall reads only these.
pub(super) const LESSON_TAG: &str = "pl90";

/// Scope label of a project lesson in the recall request.
const PROJECT_SCOPE_LABEL: &str = "project";

/// Confidence of a lesson: one observation is a hint, not an established fact.
const LESSON_CONFIDENCE: f32 = 0.3;

/// Keywords taken from the objective.
const MAX_KEYWORDS: usize = 8;

/// Shortest word that counts as a keyword.
const MIN_KEYWORD_CHARS: usize = 4;

/// Facts fetched by the keyword search before the tag filter and the budget.
const SEARCH_LIMIT: usize = 32;

/// Longest recalled lesson line, in characters.
const MAX_LESSON_CHARS: usize = 160;

/// Longest id (cycle or evidence) copied into a fact source.
const MAX_ID_CHARS: usize = 128;

/// Longest transition label copied into a fact.
const MAX_LABEL_CHARS: usize = 32;

/// Longest fact name `FactStore` accepts.
const MAX_FACT_NAME_CHARS: usize = 60;

/// Hex characters of the dedupe-key hash that end every fact name.
const NAME_HASH_HEX_CHARS: usize = 12;

/// What one [`drain_cycle_observations`] call did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct DrainReport {
    /// Outbox entries acknowledged (removed from the pending list).
    pub acked: usize,
    /// Pitfall facts written.
    pub written: usize,
    /// Learnable observations the gate (or the id check) kept out of memory.
    pub screened_out: usize,
}

/// Categories that become a lesson.
///
/// `AdmissionRefused` is the normal feedback loop of a run (the proposer is
/// told and retries), `Recovered` is not a failure, and `ValidatedFix` is
/// never produced without a verifier: those are acknowledged without a fact.
/// The match is exhaustive on purpose: a new category must be classified here.
pub(super) const fn is_learnable(category: ObservationCategory) -> bool {
    match category {
        ObservationCategory::ToolFailedNoEffect
        | ObservationCategory::CapabilityUnavailable
        | ObservationCategory::ProviderUnavailable
        | ObservationCategory::VerificationFailed
        | ObservationCategory::UncertainEffect
        | ObservationCategory::Contradicted => true,
        ObservationCategory::AdmissionRefused
        | ObservationCategory::Recovered
        | ObservationCategory::ValidatedFix => false,
    }
}

/// Snake-case name of a unit enum variant (`ToolFailedNoEffect` becomes
/// `tool_failed_no_effect`), identical to its serde representation.
fn snake_name<T: Debug>(value: T) -> String {
    let debug = format!("{value:?}");
    let mut out = String::with_capacity(debug.len() + 4);
    for (index, c) in debug.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// `text` if it is a non-empty, bounded id made of prompt-safe characters.
fn safe_id(text: &str) -> Option<&str> {
    let ok = !text.is_empty()
        && text.chars().count() <= MAX_ID_CHARS
        && text.chars().all(is_prompt_safe_char);
    ok.then_some(text)
}

/// Lowercase `[a-z0-9_]` label of at most [`MAX_LABEL_CHARS`] characters;
/// `unknown` when nothing is left.
fn safe_label(text: &str) -> String {
    let label: String = text
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
        .take(MAX_LABEL_CHARS)
        .collect();
    if label.is_empty() {
        "unknown".to_owned()
    } else {
        label
    }
}

/// Fact name for one observation: `pl90-<category>-<cause>-<hash>`.
///
/// `FactStore` caps names at 60 characters and `slugify` cuts longer titles,
/// which would drop exactly the part that tells observations apart (cycle id,
/// sequence, index). The readable stem is therefore followed by a fixed-length
/// hash of the observation's dedupe key: the name stays a pure function of
/// that key (re-writing the same observation rewrites the same file) and two
/// distinct observations never share a name because of truncation.
fn fact_name(category: &str, cause: &str, dedupe_key: &str) -> String {
    let hex = blake3::hash(dedupe_key.as_bytes()).to_hex();
    let suffix: String = hex.chars().take(NAME_HASH_HEX_CHARS).collect();
    let budget = MAX_FACT_NAME_CHARS.saturating_sub(NAME_HASH_HEX_CHARS + 1);
    let mut stem: String = slugify(&format!("{LESSON_TAG}-{category}-{cause}"))
        .chars()
        .take(budget)
        .collect();
    while stem.ends_with('-') {
        stem.pop();
    }
    format!("{stem}-{suffix}")
}

/// Deterministic project pitfall for one typed observation.
///
/// `None` for a category that is not learnable and for an observation whose
/// cycle id is not a safe id. Nothing but enum names, ids and the (sanitized)
/// transition label ends up in the fact; evidence ids that are not safe ids
/// are left out.
pub(super) fn lesson_fact(observation: &RunObservationV1, now: Timestamp) -> Option<Fact> {
    if !is_learnable(observation.category) {
        return None;
    }
    let event = &observation.event_id;
    let cycle_id = safe_id(&event.cycle_id)?;
    let category = snake_name(observation.category);
    let cause = snake_name(observation.cause);
    let transition = safe_label(&observation.transition);
    let description =
        format!("Intent cycle observed {category}/{cause} on transition {transition}");
    let body = format!("{description}\nOutcome: {:?}.", observation.outcome);
    let mut sources = vec![format!(
        "cycle:{cycle_id}#{}:{}",
        event.sequence, event.index
    )];
    sources.extend(
        observation
            .evidence_ids
            .iter()
            .filter_map(|id| safe_id(id))
            .map(|id| format!("evidence:{id}")),
    );
    Some(Fact {
        name: fact_name(&category, &cause, &event.dedupe_key()),
        description,
        fact_type: FactType::Pitfall,
        scope: FactScope::Project,
        created: now,
        updated: now,
        confidence: LESSON_CONFIDENCE,
        sources,
        tags: vec![LESSON_TAG.to_owned(), category, cause, transition],
        body,
    })
}

/// Opens the project fact store on first use and keeps it for the drain, so a
/// drain without a learnable observation creates no directory.
fn project_store<'a>(
    slot: &'a mut Option<FactStore>,
    learning_root: &Path,
) -> Result<&'a FactStore, String> {
    if slot.is_none() {
        let opened = FactStore::open(learning_root, FactScope::Project)
            .map_err(|error| format!("open project memories: {error}"))?;
        *slot = Some(opened);
    }
    slot.as_ref()
        .ok_or_else(|| "project memories are not open".to_owned())
}

/// Turns the pending observations of `cycle_id` into project pitfall facts and
/// acknowledges them under `token`.
///
/// Only [`FactScope::Project`] under `learning_root` is ever opened. Per
/// pending entry:
///
/// - not learnable (see [`is_learnable`]): acknowledged, no fact;
/// - the gate does not accept the fact (or the observation has no safe id):
///   counted in `screened_out` and acknowledged, so it is not retried forever;
/// - accepted: written, then acknowledged.
///
/// A fact that cannot be written stops the drain; what was handled before is
/// still acknowledged and the failing entry stays pending for the next drain.
///
/// # Errors
/// The pending list cannot be read, the project memories cannot be opened
/// (nothing is acknowledged then), or the acknowledgement fails (the facts are
/// already written; the next drain rewrites the same files).
pub(super) fn drain_cycle_observations(
    store: &CycleStore,
    cycle_id: &str,
    token: &LeaseToken,
    learning_root: &Path,
) -> Result<DrainReport, String> {
    let pending = store
        .pending_observations(cycle_id)
        .map_err(|error| format!("read pending observations: {error}"))?;
    let mut report = DrainReport::default();
    if pending.is_empty() {
        return Ok(report);
    }
    let now = Timestamp::now_utc();
    let mut facts: Option<FactStore> = None;
    let mut to_ack: Vec<String> = Vec::with_capacity(pending.len());
    for entry in &pending {
        let observation = &entry.payload;
        if !is_learnable(observation.category) {
            to_ack.push(entry.key.clone());
            continue;
        }
        let verdict = lesson_fact(observation, now).map(screen);
        let fact = match verdict {
            Some(GateVerdict::Accept(fact)) => fact,
            Some(GateVerdict::Review(_) | GateVerdict::Reject(_)) | None => {
                report.screened_out += 1;
                to_ack.push(entry.key.clone());
                continue;
            }
        };
        let project = project_store(&mut facts, learning_root)?;
        if let Err(error) = project.write(&fact) {
            tracing::warn!(
                cycle_id,
                error = %error,
                "intent cycle lesson could not be written; leaving it pending"
            );
            break;
        }
        report.written += 1;
        to_ack.push(entry.key.clone());
    }
    if to_ack.is_empty() {
        return Ok(report);
    }
    report.acked = store
        .ack_observations(cycle_id, token, &to_ack)
        .map_err(|error| format!("acknowledge observations: {error}"))?;
    Ok(report)
}

/// Lowercase words of at least [`MIN_KEYWORD_CHARS`] characters, de-duplicated,
/// the first [`MAX_KEYWORDS`] in order of appearance.
pub(super) fn keywords(objective: &str) -> Vec<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<String> = Vec::new();
    for word in objective.split(|c: char| !c.is_alphanumeric()) {
        if out.len() >= MAX_KEYWORDS {
            break;
        }
        let lowered = word.to_lowercase();
        if lowered.chars().count() < MIN_KEYWORD_CHARS || !seen.insert(lowered.clone()) {
            continue;
        }
        out.push(lowered);
    }
    out
}

/// Characters that never belong in a one-line prompt datum: control
/// characters, zero-width and bidirectional formatting characters, and the
/// Unicode line and paragraph separators.
fn is_unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{2069}'
                | '\u{FEFF}'
        )
}

/// Single-line, printable, bounded rendering of a fact description.
fn clean_line(text: &str) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let printable: String = joined
        .chars()
        .filter(|c| !is_unsafe_char(*c))
        .take(MAX_LESSON_CHARS)
        .collect();
    printable.trim().to_owned()
}

/// Relevance of the `index`-th search hit: 1000 for the best, 10 less per
/// position, never below 0.
fn relevance_milli(index: usize) -> u32 {
    let penalty = u32::try_from(index).unwrap_or(u32::MAX).saturating_mul(10);
    1_000u32.saturating_sub(penalty)
}

/// Lesson lines for the next proposer prompt: project pitfalls written by
/// [`drain_cycle_observations`] that match the keywords of `objective`.
///
/// At most [`MAX_RECALL_TOP_K`] lines within [`MAX_RECALL_TOKENS`] estimated
/// tokens, best match first, each a single printable line of at most
/// [`MAX_LESSON_CHARS`] characters; identical lines appear once. Only facts
/// that carry [`LESSON_TAG`] and are project-scoped qualify. Empty when the
/// objective has no keyword, when `learning_root` has no `facts` directory
/// (nothing is created) or when the store cannot be read.
pub(super) fn lesson_lines(learning_root: &Path, objective: &str, now_unix: i64) -> Vec<String> {
    let words = keywords(objective);
    if words.is_empty() || !learning_root.join("facts").is_dir() {
        return Vec::new();
    }
    let store = match FactStore::open(learning_root, FactScope::Project) {
        Ok(store) => store,
        Err(error) => {
            tracing::warn!(error = %error, "intent cycle lessons: project memories unavailable");
            return Vec::new();
        }
    };
    let terms: Vec<&str> = words.iter().map(String::as_str).collect();
    let found = match store.search(&terms, SEARCH_LIMIT) {
        Ok(found) => found,
        Err(error) => {
            tracing::warn!(error = %error, "intent cycle lessons: search failed");
            return Vec::new();
        }
    };

    // (fact name, cleaned line) in search order, one entry per distinct line.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut lines: Vec<(String, String)> = Vec::new();
    for fact in &found {
        if fact.scope != FactScope::Project || !fact.tags.iter().any(|tag| tag == LESSON_TAG) {
            continue;
        }
        let line = clean_line(&fact.description);
        if line.is_empty() || !seen.insert(line.clone()) {
            continue;
        }
        lines.push((fact.name.clone(), line));
    }
    let candidates: Vec<LessonCandidate> = lines
        .iter()
        .enumerate()
        .map(|(index, (id, line))| LessonCandidate {
            id: id.clone(),
            scope_label: PROJECT_SCOPE_LABEL.to_owned(),
            relevance_milli: relevance_milli(index),
            est_tokens: line.chars().count() / 4 + 1,
            trust_verified: false,
            expires_at: None,
            superseded: false,
            revoked: false,
        })
        .collect();
    let request = LearningRecallRequest {
        top_k: MAX_RECALL_TOP_K,
        token_ceiling: MAX_RECALL_TOKENS,
        now_unix,
        allowed_scopes: vec![PROJECT_SCOPE_LABEL.to_owned()],
        require_verified: false,
    };
    select_lessons(&request, &candidates, &|_| true)
        .into_iter()
        .filter_map(|id| {
            lines
                .iter()
                .find(|(candidate, _)| *candidate == id)
                .map(|(_, line)| line.clone())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use harw_authority::{
        AuthorityContext, Permission, PermissionSet, SandboxSpec, WorkspaceRegistration,
        WorkspaceRegistry,
    };
    use harw_plan_bridge::{
        CauseCode, CycleAdmission, CycleCheckpoint, CycleLimits, CycleObservations, CycleProposal,
        CycleRecord, CycleTargets, IntentBinding, admit_cycle, apply_observations,
        build_observation,
    };
    use harw_types::{ContentDigest, TenantId, WorkId, WorkspaceId};

    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    const CYCLE_ID: &str = "job-learning";

    /// A job root with one cycle record whose outbox holds the observations,
    /// plus the lease that owns it. `_temp` keeps the directory alive.
    struct Fixture {
        _temp: tempfile::TempDir,
        base: PathBuf,
        store: CycleStore,
        token: LeaseToken,
        learning_root: PathBuf,
    }

    fn authority(base: &Path) -> TestResult<AuthorityContext> {
        std::fs::create_dir_all(base.join("workspace")).map_err(ctx("create workspace"))?;
        let tenant = TenantId::from_str("tenant");
        let workspace = WorkspaceId::from_str("workspace");
        let registry = WorkspaceRegistry::build(
            base,
            [WorkspaceRegistration {
                tenant: tenant.clone(),
                workspace: workspace.clone(),
                root: PathBuf::from("workspace"),
            }],
        )
        .map_err(ctx("workspace registry"))?;
        let binding = registry
            .resolve(&tenant, &workspace)
            .map_err(ctx("resolve workspace"))?;
        Ok(SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        )
        .authority()
        .clone())
    }

    fn fixture_with(
        observed: &[(ObservationCategory, CauseCode)],
        adjust: impl FnOnce(&mut Vec<RunObservationV1>),
    ) -> TestResult<Fixture> {
        let temp = tempfile::tempdir().map_err(ctx("temp directory"))?;
        let base = temp.path().to_path_buf();
        let authority = authority(&base)?;
        let admission = CycleAdmission::new(
            IntentBinding {
                id: "intent-learning".to_owned(),
                revision: 1,
                digest: ContentDigest::of(b"intent-learning@1"),
                predecessor: None,
                acceptance: BTreeSet::from(["done".to_owned()]),
            },
            CycleLimits {
                max_transitions: 8,
                max_chain_depth: 1,
                max_spawn_depth: 1,
                max_parallel_segments: 2,
                max_stall_transitions: 3,
            },
            CycleTargets::default(),
        )
        .map_err(|_| TestError::Missing("admission"))?;
        let initial = CycleCheckpoint::initial(&admission, &authority, 0);
        let work_id = WorkId::from_str(CYCLE_ID);
        let record = CycleRecord::new(CYCLE_ID, work_id.clone(), initial.clone())
            .map_err(ctx("cycle record"))?;
        let jobs = base.join("jobs");
        std::fs::create_dir_all(&jobs).map_err(ctx("create job root"))?;
        let store = CycleStore::open_in_job_root(&jobs).map_err(ctx("cycle store"))?;
        store.create(&record).map_err(ctx("create record"))?;
        let token = LeaseToken {
            work_id,
            epoch: 0,
            nonce: "nonce-0".to_owned(),
        };

        // Journal one step and commit it together with the observations.
        let admitted = admit_cycle(
            &admission,
            &initial,
            CycleProposal::Wait {
                reason: "waiting for the owner".to_owned(),
            },
        )
        .map_err(|_| TestError::Missing("admitted wait"))?;
        store
            .begin_step(CYCLE_ID, &token, &admitted)
            .map_err(ctx("begin step"))?;
        let next = apply_observations(
            admission.intent(),
            &initial,
            &admitted,
            CycleObservations::default(),
        )
        .map_err(|_| TestError::Missing("next checkpoint"))?;
        let mut observations: Vec<RunObservationV1> = observed
            .iter()
            .enumerate()
            .map(|(index, (category, cause))| {
                build_observation(
                    CYCLE_ID,
                    initial.fence,
                    u32::try_from(index).unwrap_or(u32::MAX),
                    &initial,
                    admitted.proposal(),
                    *category,
                    *cause,
                    &[],
                )
            })
            .collect();
        adjust(&mut observations);
        store
            .commit_step_observed(CYCLE_ID, &token, &next, &observations)
            .map_err(ctx("commit step"))?;

        let learning_root = base.join("project").join("memories");
        Ok(Fixture {
            _temp: temp,
            base,
            store,
            token,
            learning_root,
        })
    }

    fn fixture(observed: &[(ObservationCategory, CauseCode)]) -> TestResult<Fixture> {
        fixture_with(observed, |_| {})
    }

    fn drain(fx: &Fixture) -> TestResult<DrainReport> {
        drain_cycle_observations(&fx.store, CYCLE_ID, &fx.token, &fx.learning_root)
            .map_err(TestError::Unexpected)
    }

    fn pending_keys(fx: &Fixture) -> TestResult<Vec<String>> {
        Ok(fx
            .store
            .pending_observations(CYCLE_ID)
            .map_err(ctx("pending observations"))?
            .into_iter()
            .map(|entry| entry.key)
            .collect())
    }

    /// File name to bytes for everything under `<root>/facts`.
    fn fact_files(root: &Path) -> TestResult<BTreeMap<String, Vec<u8>>> {
        let mut files = BTreeMap::new();
        let entries = std::fs::read_dir(root.join("facts")).map_err(ctx("read facts dir"))?;
        for entry in entries {
            let entry = entry.map_err(ctx("facts dir entry"))?;
            let bytes = std::fs::read(entry.path()).map_err(ctx("read fact file"))?;
            files.insert(entry.file_name().to_string_lossy().into_owned(), bytes);
        }
        Ok(files)
    }

    fn entry_names(dir: &Path) -> TestResult<BTreeSet<String>> {
        let mut names = BTreeSet::new();
        for entry in std::fs::read_dir(dir).map_err(ctx("read dir"))? {
            let entry = entry.map_err(ctx("dir entry"))?;
            names.insert(entry.file_name().to_string_lossy().into_owned());
        }
        Ok(names)
    }

    fn first_payload(fx: &Fixture) -> TestResult<RunObservationV1> {
        fx.store
            .pending_observations(CYCLE_ID)
            .map_err(ctx("pending observations"))?
            .into_iter()
            .next()
            .map(|entry| entry.payload)
            .ok_or(TestError::Missing("pending observation"))
    }

    fn lesson(name: &str, description: &str, scope: FactScope, tags: &[&str]) -> Fact {
        let now = Timestamp::now_utc();
        Fact {
            name: name.to_owned(),
            description: description.to_owned(),
            fact_type: FactType::Pitfall,
            scope,
            created: now,
            updated: now,
            confidence: LESSON_CONFIDENCE,
            sources: Vec::new(),
            tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            body: String::new(),
        }
    }

    #[test]
    fn drain_writes_project_pitfalls_and_acks_exactly_once() -> TestResult {
        let fx = fixture(&[
            (
                ObservationCategory::ToolFailedNoEffect,
                CauseCode::ExecutorFailed,
            ),
            (
                ObservationCategory::VerificationFailed,
                CauseCode::VerificationUnmet,
            ),
            (
                ObservationCategory::AdmissionRefused,
                CauseCode::ReadNotAllowed,
            ),
        ])?;
        assert_eq!(pending_keys(&fx)?.len(), 3);

        let first = drain(&fx)?;
        assert_eq!(
            first,
            DrainReport {
                acked: 3,
                written: 2,
                screened_out: 0
            }
        );
        assert!(pending_keys(&fx)?.is_empty());
        let files_after_first = fact_files(&fx.learning_root)?;
        assert_eq!(files_after_first.len(), 2);
        assert!(
            files_after_first
                .keys()
                .all(|name| name.starts_with("pl90-") && name.ends_with(".md")),
            "{:?}",
            files_after_first.keys()
        );

        let facts = FactStore::open(&fx.learning_root, FactScope::Project)
            .map_err(ctx("open facts"))?
            .list()
            .map_err(ctx("list facts"))?;
        let descriptions: BTreeSet<String> =
            facts.iter().map(|fact| fact.description.clone()).collect();
        assert_eq!(
            descriptions,
            BTreeSet::from([
                "Intent cycle observed tool_failed_no_effect/executor_failed on transition wait"
                    .to_owned(),
                "Intent cycle observed verification_failed/verification_unmet on transition wait"
                    .to_owned(),
            ])
        );
        for fact in &facts {
            assert_eq!(fact.fact_type, FactType::Pitfall);
            assert_eq!(fact.scope, FactScope::Project);
            assert!(fact.tags.iter().any(|tag| tag == LESSON_TAG));
            assert!((fact.confidence - LESSON_CONFIDENCE).abs() < 0.01);
        }

        // Delivered again: nothing is pending, so nothing is acked or written.
        let second = drain(&fx)?;
        assert_eq!(second, DrainReport::default());
        assert_eq!(fact_files(&fx.learning_root)?, files_after_first);
        Ok(())
    }

    #[test]
    fn admission_refusals_are_acked_but_never_learned() -> TestResult {
        let fx = fixture(&[
            (
                ObservationCategory::AdmissionRefused,
                CauseCode::ReadNotAllowed,
            ),
            (ObservationCategory::AdmissionRefused, CauseCode::Stalled),
            (ObservationCategory::Recovered, CauseCode::Unclassified),
            (ObservationCategory::ValidatedFix, CauseCode::Unclassified),
        ])?;
        let report = drain(&fx)?;
        assert_eq!(
            report,
            DrainReport {
                acked: 4,
                written: 0,
                screened_out: 0
            }
        );
        assert!(pending_keys(&fx)?.is_empty());
        assert!(
            !fx.learning_root.join("facts").exists(),
            "a drain without a learnable observation must not create memories"
        );
        Ok(())
    }

    #[test]
    fn drain_never_opens_global_scope() -> TestResult {
        let fx = fixture(&[(
            ObservationCategory::CapabilityUnavailable,
            CauseCode::Unclassified,
        )])?;
        let observation = first_payload(&fx)?;
        let fact = lesson_fact(&observation, Timestamp::now_utc())
            .ok_or(TestError::Missing("lesson fact"))?;
        assert_eq!(fact.scope, FactScope::Project);
        assert!(matches!(screen(fact), GateVerdict::Accept(_)));

        assert_eq!(drain(&fx)?.written, 1);

        // Everything lives under the project root; no other directory appeared
        // next to the job root, the workspace and the project.
        assert_eq!(
            entry_names(&fx.base)?,
            BTreeSet::from([
                "jobs".to_owned(),
                "project".to_owned(),
                "workspace".to_owned()
            ])
        );
        assert!(
            entry_names(&fx.learning_root)?
                .iter()
                .all(|name| name == "facts" || name == "MEMORY.md"),
            "{:?}",
            entry_names(&fx.learning_root)
        );
        for (name, bytes) in fact_files(&fx.learning_root)? {
            let text = String::from_utf8_lossy(&bytes);
            assert!(text.contains("scope: project"), "{name}: {text}");
            assert!(!text.contains("scope: global"), "{name}: {text}");
        }
        Ok(())
    }

    #[test]
    fn unsafe_observation_ids_are_screened_out_and_acked() -> TestResult {
        let fx = fixture_with(
            &[(
                ObservationCategory::ToolFailedNoEffect,
                CauseCode::ExecutorFailed,
            )],
            |observations| {
                for observation in observations.iter_mut() {
                    observation.event_id.cycle_id = "cycle with\nspaces".to_owned();
                }
            },
        )?;
        let report = drain(&fx)?;
        assert_eq!(
            report,
            DrainReport {
                acked: 1,
                written: 0,
                screened_out: 1
            }
        );
        assert!(pending_keys(&fx)?.is_empty());
        assert!(!fx.learning_root.join("facts").exists());
        Ok(())
    }

    #[test]
    fn learnable_categories_are_the_failures() {
        let learnable = [
            ObservationCategory::ToolFailedNoEffect,
            ObservationCategory::CapabilityUnavailable,
            ObservationCategory::ProviderUnavailable,
            ObservationCategory::VerificationFailed,
            ObservationCategory::UncertainEffect,
            ObservationCategory::Contradicted,
        ];
        let not_learnable = [
            ObservationCategory::AdmissionRefused,
            ObservationCategory::Recovered,
            ObservationCategory::ValidatedFix,
        ];
        assert!(learnable.into_iter().all(is_learnable));
        assert!(
            not_learnable
                .into_iter()
                .all(|category| !is_learnable(category))
        );
    }

    #[test]
    fn lesson_fact_is_deterministic_typed_and_text_free() -> TestResult {
        let fx = fixture(&[(
            ObservationCategory::ToolFailedNoEffect,
            CauseCode::ExecutorFailed,
        )])?;
        let mut observation = first_payload(&fx)?;
        observation.evidence_ids = vec!["ev-1".to_owned(), "not safe".to_owned()];
        let now = Timestamp::now_utc();

        let fact = lesson_fact(&observation, now).ok_or(TestError::Missing("lesson fact"))?;
        assert_eq!(lesson_fact(&observation, now), Some(fact.clone()));
        assert!(
            fact.name
                .starts_with("pl90-tool-failed-no-effect-executor-failed-"),
            "{}",
            fact.name
        );
        assert!(fact.name.len() <= MAX_FACT_NAME_CHARS, "{}", fact.name);
        assert_eq!(fact.fact_type, FactType::Pitfall);
        assert_eq!(fact.scope, FactScope::Project);
        assert_eq!(
            fact.description,
            "Intent cycle observed tool_failed_no_effect/executor_failed on transition wait"
        );
        assert_eq!(
            fact.body,
            format!("{}\nOutcome: Pending.", fact.description)
        );
        assert_eq!(
            fact.sources,
            vec![format!("cycle:{CYCLE_ID}#0:0"), "evidence:ev-1".to_owned()]
        );
        assert_eq!(
            fact.tags,
            vec![
                "pl90".to_owned(),
                "tool_failed_no_effect".to_owned(),
                "executor_failed".to_owned(),
                "wait".to_owned()
            ]
        );

        // Another event of the same cycle gets its own, equally short name.
        let mut other = observation.clone();
        other.event_id.index = 1;
        let other_fact =
            lesson_fact(&other, now).ok_or(TestError::Missing("second lesson fact"))?;
        assert_ne!(other_fact.name, fact.name);
        assert!(other_fact.name.len() <= MAX_FACT_NAME_CHARS);

        // A long cycle id must not make two events collide.
        let mut long_a = observation.clone();
        long_a.event_id.cycle_id = format!("job-{}-a", "0123456789".repeat(8));
        let mut long_b = long_a.clone();
        long_b.event_id.cycle_id = format!("job-{}-b", "0123456789".repeat(8));
        let name_a = lesson_fact(&long_a, now)
            .map(|fact| fact.name)
            .ok_or(TestError::Missing("long id lesson"))?;
        let name_b = lesson_fact(&long_b, now)
            .map(|fact| fact.name)
            .ok_or(TestError::Missing("long id lesson"))?;
        assert_ne!(name_a, name_b);
        assert!(name_a.len() <= MAX_FACT_NAME_CHARS && name_b.len() <= MAX_FACT_NAME_CHARS);
        Ok(())
    }

    #[test]
    fn snake_names_match_the_serde_names() -> TestResult {
        fn serde_name<T: serde::Serialize>(value: &T) -> TestResult<String> {
            match serde_json::to_value(value).map_err(ctx("serialize"))? {
                serde_json::Value::String(name) => Ok(name),
                other => Err(TestError::Unexpected(other.to_string())),
            }
        }
        for category in [
            ObservationCategory::AdmissionRefused,
            ObservationCategory::ToolFailedNoEffect,
            ObservationCategory::CapabilityUnavailable,
            ObservationCategory::ProviderUnavailable,
            ObservationCategory::VerificationFailed,
            ObservationCategory::Contradicted,
            ObservationCategory::UncertainEffect,
            ObservationCategory::Recovered,
            ObservationCategory::ValidatedFix,
        ] {
            assert_eq!(snake_name(category), serde_name(&category)?);
        }
        for cause in [
            CauseCode::InvalidIntent,
            CauseCode::EmptyOrOversizedGraph,
            CauseCode::ReadNotAllowed,
            CauseCode::CeilingMismatch,
            CauseCode::ExecutorFailed,
            CauseCode::ExecutorRetryable,
            CauseCode::ReconcileUncertain,
            CauseCode::VerificationUnmet,
            CauseCode::VerificationStale,
            CauseCode::StaleEpoch,
            CauseCode::Unclassified,
        ] {
            assert_eq!(snake_name(cause), serde_name(&cause)?);
        }
        Ok(())
    }

    #[test]
    fn lesson_lines_are_bounded_to_eight_and_sanitized() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("temp directory"))?;
        let root = temp.path().join("memories");
        let store = FactStore::open(&root, FactScope::Project).map_err(ctx("open facts"))?;
        for index in 0..12 {
            let mut description = format!(
                "parser pitfall number {index:02}\u{7} with\u{1b}[31m escape\u{200B} codes"
            );
            if index == 0 {
                description.push(' ');
                description.push_str(&"long pitfall text ".repeat(14));
            }
            store
                .write(&lesson(
                    &format!("pl90-lesson-{index:02}"),
                    &description,
                    FactScope::Project,
                    &[LESSON_TAG, "tool_failed_no_effect"],
                ))
                .map_err(ctx("write lesson"))?;
        }
        // Neither of these may be recalled: no pl90 tag, and a global-scoped one.
        store
            .write(&lesson(
                "note-parser-hint",
                "parser note about the parser module",
                FactScope::Project,
                &["note"],
            ))
            .map_err(ctx("write note"))?;
        store
            .write(&lesson(
                "pl90-global-leak",
                "parser leak that must not be recalled",
                FactScope::Global,
                &[LESSON_TAG],
            ))
            .map_err(ctx("write global lookalike"))?;

        let lines = lesson_lines(&root, "Improve the parser error handling", 0);
        assert_eq!(lines.len(), MAX_RECALL_TOP_K, "{lines:?}");
        let distinct: BTreeSet<&String> = lines.iter().collect();
        assert_eq!(distinct.len(), lines.len(), "{lines:?}");
        for line in &lines {
            assert!(!line.chars().any(char::is_control), "{line:?}");
            assert!(!line.chars().any(is_unsafe_char), "{line:?}");
            assert!(line.chars().count() <= MAX_LESSON_CHARS, "{line:?}");
            assert!(line.starts_with("parser pitfall number"), "{line:?}");
            assert!(!line.contains("note about"), "{line:?}");
            assert!(!line.contains("leak"), "{line:?}");
        }
        Ok(())
    }

    #[test]
    fn identical_lessons_are_recalled_once() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("temp directory"))?;
        let root = temp.path().join("memories");
        let store = FactStore::open(&root, FactScope::Project).map_err(ctx("open facts"))?;
        for index in 0..3 {
            store
                .write(&lesson(
                    &format!("pl90-same-{index}"),
                    "Intent cycle observed tool_failed_no_effect/executor_failed on transition advance",
                    FactScope::Project,
                    &[LESSON_TAG],
                ))
                .map_err(ctx("write lesson"))?;
        }
        let lines = lesson_lines(&root, "advance the cycle", 0);
        assert_eq!(
            lines,
            vec![
                "Intent cycle observed tool_failed_no_effect/executor_failed on transition advance"
                    .to_owned()
            ]
        );
        Ok(())
    }

    #[test]
    fn recall_without_memories_or_keywords_is_empty_and_creates_nothing() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("temp directory"))?;
        let missing = temp.path().join("missing");
        assert!(lesson_lines(&missing, "improve the parser", 0).is_empty());
        assert!(!missing.exists());

        let root = temp.path().join("memories");
        FactStore::open(&root, FactScope::Project).map_err(ctx("open facts"))?;
        assert!(lesson_lines(&root, "do it", 0).is_empty());
        assert!(lesson_lines(&root, "improve the parser", 0).is_empty());
        Ok(())
    }

    #[test]
    fn keywords_are_lowercase_deduped_and_capped() {
        assert_eq!(
            keywords(
                "Fix the PARSER, fix the Parser; and refactor module-loading of Config files, \
                 Cache layers, Network stack, Render pipeline"
            ),
            [
                "parser", "refactor", "module", "loading", "config", "files", "cache", "layers"
            ]
            .map(str::to_owned)
        );
        assert!(keywords("a bc def").is_empty());
        assert!(keywords("").is_empty());
        assert_eq!(keywords("Äpfel äpfel ÄPFEL"), vec!["äpfel".to_owned()]);
    }

    #[test]
    fn relevance_never_goes_below_zero() {
        assert_eq!(relevance_milli(0), 1_000);
        assert_eq!(relevance_milli(31), 690);
        assert_eq!(relevance_milli(100), 0);
        assert_eq!(relevance_milli(usize::MAX), 0);
    }
}
