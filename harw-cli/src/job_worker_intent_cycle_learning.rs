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
//! Every pending entry is decided once: learned, refused by the gate, or not
//! learnable at all (admission refusals are a normal part of a run, not a
//! lesson).
//!
//! # Ordering: decide, materialise, then acknowledge (two-phase delivery)
//!
//! 1. **Read** the pending entries under the lease (fenced, lease held).
//! 2. **Decide** every entry in memory, then commit the decision in one
//!    fenced write held under the live lease
//!    ([`CycleStore::decide_observations`]): screened and non-learnable
//!    entries are acknowledged directly; learnable ones are marked
//!    [`DeliveryState::Accepted`] and stay in the outbox. That write is the
//!    commit of the learning decision — the job lease cannot be replaced
//!    between its verification and the write, so an expired, cancelled or
//!    reclaimed lease never accepts anything.
//! 3. **Materialise**: open the project [`FactStore`] and write each
//!    accepted fact. The fact name is a pure function of the observation's
//!    dedupe key, so a repeated write produces the same file (idempotent).
//! 4. **Acknowledge** exactly the entries whose fact write succeeded
//!    ([`CycleStore::ack_observations`], lease held again). An entry whose
//!    fact is not durable yet — a crash after the decision, project
//!    memories that cannot be opened, a failed write, a refused final
//!    acknowledgement — stays accepted and is materialised again by the next
//!    drain (this claim's or the one that takes over): delivery is at least
//!    once, and never lost.
//!
//! An accepted entry is a committed decision and is never re-made: a later
//! drain — possibly a newer binary with a changed classification or gate —
//! skips [`is_learnable`] and the gate for it and rebuilds its fact exactly
//! as it was accepted (the fact text depends only on the typed observation).
//! An accepted entry whose fact cannot be rebuilt stays accepted and is
//! reported, never acknowledged without its fact.
//!
//! # Binding to the job
//!
//! Both halves act on the data of exactly one job:
//!
//! - `learning_root` is bound to the job by its trusted input
//!   (`harw_ops::intent_cycle::IntentCycleJobInput::learning_root`). Only
//!   `intent_cycle.start` writes that field, from the operator's resolved
//!   home context (the project memories of the bound home); it is never a
//!   tool argument, and without it nothing is learned or recalled.
//! - The observations are bound to the job by the store's lease fence:
//!   [`CycleStore::pending_observations`] takes the claim's [`LeaseToken`]
//!   and refuses, *before* any payload leaves the store, unless the record
//!   belongs to `token.work_id` and is fenced under `token.epoch`. On a
//!   refusal [`drain_cycle_observations`] returns without opening the
//!   project memories, writing or acknowledging anything, so a superseded
//!   epoch or another job's lease can never copy the current owner's
//!   observations into memory.
//! - The epoch fence alone cannot refuse the *same* lease after its job
//!   claim expired but before a reclaim rebinds the cycle. The worker's
//!   store therefore holds the job lease around every fenced operation
//!   ([`CycleStore::with_lease_liveness`], backed by
//!   `JobStore::hold_live_lease`): the read, the decision and the
//!   acknowledgement are refused for a token that is no longer the current,
//!   non-expired claim, and the lease cannot be replaced while one of them
//!   runs. Facts are only written for entries accepted under such a held
//!   lease (see the ordering above), so a lease that is lost after the read
//!   and before the decision writes nothing. A fact written after the
//!   decision materialises a lesson the lease was entitled to; if its lease
//!   is lost before the acknowledgement, the entry stays accepted and the
//!   claim that takes over acknowledges it after rewriting the same file.
//! - A pending observation that names another cycle than the drained one is
//!   never learned (screened out and acknowledged at the decision).
//!
//! # Recall (W09b)
//!
//! [`lesson_lines`] lists the same project root, keeps only the lessons this
//! module wrote (tag [`LESSON_TAG`], project scope) *before* any limit, ranks
//! them by the objective's keywords and returns at most
//! [`MAX_RECALL_TOP_K`](harw_knowledge::learning_recall::MAX_RECALL_TOP_K)
//! single, sanitized lines within the token ceiling of
//! [`select_lessons`]. Ordinary notes never compete with lessons for a slot.
//! The proposer renders the lines as low-trust data.

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
use harw_plan_bridge::{CycleStore, DeliveryState, ObservationCategory, RunObservationV1};

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

/// Distinct lesson lines (eligible, ranked, de-duplicated) handed to the
/// recall budget.
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
    /// Outbox entries acknowledged (removed): screened and non-learnable ones
    /// at the decision, learned ones once their fact was written.
    pub acked: usize,
    /// Pitfall facts written (durable) by this drain.
    pub written: usize,
    /// Learnable observations the gate (or the id check) kept out of memory.
    pub screened_out: usize,
    /// Accepted lessons whose fact did not become durable in this drain
    /// (the write failed, or the fact cannot be rebuilt); they stay accepted
    /// and are never acknowledged unlearned.
    pub pending_materialization: usize,
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
    accepted_fact(observation, now)
}

/// The fact of an observation whose delivery is already committed
/// ([`DeliveryState::Accepted`]): built exactly like [`lesson_fact`], but
/// without asking today's classification whether it is learnable — an
/// accepted decision is never re-made. `None` when the fact cannot be
/// reconstructed (no safe cycle id).
fn accepted_fact(observation: &RunObservationV1, now: Timestamp) -> Option<Fact> {
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

/// Turns the pending observations of `cycle_id` into project pitfall facts in
/// two phases under `token` (see the module docs, "Ordering").
///
/// 1. The pending list (pending and already accepted entries) is read under
///    the lease: unless the record belongs to `token.work_id`, is fenced
///    under `token.epoch` and (with a lease guard) `token` is the live job
///    claim, the store refuses before any payload is returned.
/// 2. Every entry that is still [`DeliveryState::Pending`] is decided in
///    memory:
///    - not learnable (see [`is_learnable`]): acknowledged, no fact;
///    - the gate does not accept the fact, the observation has no safe id or
///      it names another cycle: counted in `screened_out`, acknowledged;
///    - accepted: marked [`DeliveryState::Accepted`], to be materialised.
///
///    An entry that is already accepted is never decided again: today's
///    classification and gate are not consulted, its fact is rebuilt as it
///    was accepted. If that is impossible (no safe id, another cycle), it
///    stays accepted (`pending_materialization`, logged) and is never
///    acknowledged unlearned.
///
///    The decision is committed in one fenced, lease-held write
///    ([`CycleStore::decide_observations`]); a refusal ends the drain with
///    nothing accepted, acknowledged or written.
/// 3. If there is something to materialise, [`FactScope::Project`] under
///    `learning_root` is opened and every accepted fact is written
///    (idempotently: the name derives from the dedupe key). A write that
///    fails is logged; its entry stays accepted (`pending_materialization`).
/// 4. Exactly the entries whose fact was written are acknowledged.
///
/// The global scope is never opened.
///
/// # Errors
/// The lease does not own the record (stale epoch, another job, not yet
/// rebound), is not live, or the pending list cannot be read, or the
/// decision is refused: nothing is accepted, acknowledged or written. The
/// project memories cannot be opened: the accepted entries stay accepted,
/// no fact is written. The final acknowledgement is refused or fails: the
/// facts are durable, their entries stay accepted and the next drain
/// rewrites the same files before acknowledging them.
pub(super) fn drain_cycle_observations(
    store: &CycleStore,
    cycle_id: &str,
    token: &LeaseToken,
    learning_root: &Path,
) -> Result<DrainReport, String> {
    // Read under the lease: a stale, foreign or dead lease gets an error,
    // never a payload.
    let pending = store
        .pending_observations(cycle_id, token)
        .map_err(|error| format!("read pending observations: {error}"))?;
    let mut report = DrainReport::default();
    if pending.is_empty() {
        return Ok(report);
    }
    // Decide every entry in memory; nothing leaves the process yet.
    let now = Timestamp::now_utc();
    let mut lessons: Vec<(String, Fact)> = Vec::new();
    let mut accept: Vec<String> = Vec::new();
    let mut ack: Vec<String> = Vec::new();
    let mut unreconstructable = 0_usize;
    for entry in &pending {
        let observation = &entry.payload;
        if entry.state == DeliveryState::Accepted {
            // A committed decision: materialise it, never re-screen it. Only
            // the drained cycle's own observations are ever materialised.
            let fact = if observation.event_id.cycle_id == cycle_id {
                accepted_fact(observation, now)
            } else {
                None
            };
            match fact {
                Some(fact) => lessons.push((entry.key.clone(), fact)),
                // Never acknowledged unlearned: it stays accepted.
                None => unreconstructable += 1,
            }
            continue;
        }
        if !is_learnable(observation.category) {
            ack.push(entry.key.clone());
            continue;
        }
        // Only observations of the drained cycle are learned.
        let verdict = if observation.event_id.cycle_id == cycle_id {
            lesson_fact(observation, now).map(screen)
        } else {
            None
        };
        match verdict {
            Some(GateVerdict::Accept(fact)) => {
                accept.push(entry.key.clone());
                lessons.push((entry.key.clone(), fact));
            }
            Some(GateVerdict::Review(_) | GateVerdict::Reject(_)) | None => {
                report.screened_out += 1;
                ack.push(entry.key.clone());
            }
        }
    }
    // Phase 1, the commit of the decision: lease held across the write.
    if !accept.is_empty() || !ack.is_empty() {
        let (_accepted, acked) = store
            .decide_observations(cycle_id, token, &accept, &ack)
            .map_err(|error| format!("decide observations: {error}"))?;
        report.acked += acked;
    }
    if unreconstructable > 0 {
        tracing::warn!(
            cycle_id,
            unreconstructable,
            "accepted intent cycle lessons cannot be rebuilt; they stay accepted, never acknowledged unlearned"
        );
    }
    report.pending_materialization = unreconstructable;
    if lessons.is_empty() {
        // Nothing to write: the project memories are never created.
        return Ok(report);
    }
    // Phase 2, materialise the accepted lessons idempotently.
    let project = FactStore::open(learning_root, FactScope::Project).map_err(|error| {
        format!(
            "open project memories: {error}; {} accepted lessons stay pending",
            lessons.len()
        )
    })?;
    let mut durable: Vec<String> = Vec::with_capacity(lessons.len());
    for (key, fact) in &lessons {
        match project.write(fact) {
            Ok(()) => durable.push(key.clone()),
            Err(error) => tracing::warn!(
                cycle_id,
                error = %error,
                "intent cycle lesson could not be written; it stays accepted for the next drain"
            ),
        }
    }
    report.written = durable.len();
    report.pending_materialization += lessons.len().saturating_sub(durable.len());
    if durable.is_empty() {
        return Ok(report);
    }
    // Phase 3, acknowledge exactly the durable lessons.
    report.acked += store
        .ack_observations(cycle_id, token, &durable)
        .map_err(|error| {
            format!(
                "acknowledge {} written lessons (they stay accepted): {error}",
                durable.len()
            )
        })?;
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

/// Relevance of the `index`-th ranked lesson: 1000 for the best, 10 less per
/// position, never below 0.
fn relevance_milli(index: usize) -> u32 {
    let penalty = u32::try_from(index).unwrap_or(u32::MAX).saturating_mul(10);
    1_000u32.saturating_sub(penalty)
}

/// How many of `words` (lowercase) occur in the name, the description or a
/// tag of `fact`, compared in lowercase.
fn keyword_hits(fact: &Fact, words: &[String]) -> usize {
    let name = fact.name.to_lowercase();
    let description = fact.description.to_lowercase();
    let tags: Vec<String> = fact.tags.iter().map(|tag| tag.to_lowercase()).collect();
    words
        .iter()
        .filter(|word| {
            let word = word.as_str();
            name.contains(word)
                || description.contains(word)
                || tags.iter().any(|tag| tag.contains(word))
        })
        .count()
}

/// The lessons among `facts` that match at least one of `words`, best first:
/// more matching keywords first, ties by name (deterministic).
///
/// Only project-scoped facts tagged [`LESSON_TAG`] are eligible. That is
/// decided here, before any limit, so ordinary notes that match the keywords
/// better can never push a lesson out of the recall.
fn ranked_lessons(facts: Vec<Fact>, words: &[String]) -> Vec<Fact> {
    let mut scored: Vec<(usize, Fact)> = facts
        .into_iter()
        .filter(|fact| {
            fact.scope == FactScope::Project && fact.tags.iter().any(|tag| tag == LESSON_TAG)
        })
        .filter_map(|fact| {
            let hits = keyword_hits(&fact, words);
            (hits > 0).then_some((hits, fact))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    scored.into_iter().map(|(_, fact)| fact).collect()
}

/// Lesson lines for the next proposer prompt: project pitfalls written by
/// [`drain_cycle_observations`] that match the keywords of `objective`.
///
/// At most [`MAX_RECALL_TOP_K`] lines within [`MAX_RECALL_TOKENS`] estimated
/// tokens, best match first, each a single printable line of at most
/// [`MAX_LESSON_CHARS`] characters; identical lines appear once. Only facts
/// that carry [`LESSON_TAG`] and are project-scoped qualify, and that is
/// decided over every fact of the root before the [`SEARCH_LIMIT`] cut (see
/// [`ranked_lessons`]). Empty when the objective has no keyword, when
/// `learning_root` has no `facts` directory (nothing is created) or when the
/// store cannot be read.
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
    // Every fact of the root: eligibility is decided before any limit (the
    // store's keyword search would cut the ranking of *all* facts first).
    let facts = match store.list() {
        Ok(facts) => facts,
        Err(error) => {
            tracing::warn!(error = %error, "intent cycle lessons: project memories unreadable");
            return Vec::new();
        }
    };

    // (fact name, cleaned line) in rank order, one entry per distinct line;
    // duplicates do not use up the limit either.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut lines: Vec<(String, String)> = Vec::new();
    for fact in ranked_lessons(facts, &words) {
        if lines.len() >= SEARCH_LIMIT {
            break;
        }
        let line = clean_line(&fact.description);
        if line.is_empty() || !seen.insert(line.clone()) {
            continue;
        }
        lines.push((fact.name, line));
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, mpsc};

    use harw_authority::{
        AuthorityContext, Permission, PermissionSet, SandboxSpec, WorkspaceRegistration,
        WorkspaceRegistry,
    };
    use harw_core::cancel::{CancelReason, CancelToken};
    use harw_job_runtime::{Budget, Job, JobKind, JobScope, RetryPolicy, StoredJob};
    use harw_plan_bridge::{
        CauseCode, CeilingReissuer, CycleAdmission, CycleCheckpoint, CycleDriver, CycleLimits,
        CycleObservations, CycleProposal, CycleProposer, CycleRecord, CycleRefusal,
        CycleRunOutcome, CycleStepExecutor, CycleStoreError, CycleTargets, INTENT_CYCLE_JOB_KIND,
        InFlightStep, IntentBinding, LeaseHold, LeaseLivenessGuard, StepFailure,
        StepReconciliation, admit_cycle, apply_observations, build_observation, resume_admission,
    };
    use harw_session_store::{ClaimRequest, JobStore, SessionStoreError};
    use harw_types::{ApprovalActor, ContentDigest, TenantId, WorkId, WorkspaceId};
    use jiff::SignedDuration;

    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    const CYCLE_ID: &str = "job-learning";

    /// A job root with one cycle record whose outbox holds the observations,
    /// plus the lease that owns it. `_temp` keeps the directory alive;
    /// `admission` and `authority` let a test take the cycle over under a
    /// newer lease.
    struct Fixture {
        _temp: tempfile::TempDir,
        base: PathBuf,
        store: CycleStore,
        token: LeaseToken,
        learning_root: PathBuf,
        admission: CycleAdmission,
        authority: AuthorityContext,
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
            admission,
            authority,
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
            .pending_observations(CYCLE_ID, &fx.token)
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
            .pending_observations(CYCLE_ID, &fx.token)
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
                screened_out: 0,
                pending_materialization: 0
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
    fn stale_token_reads_nothing_and_writes_no_facts() -> TestResult {
        let fx = fixture(&[
            (
                ObservationCategory::ToolFailedNoEffect,
                CauseCode::ExecutorFailed,
            ),
            (
                ObservationCategory::VerificationFailed,
                CauseCode::VerificationUnmet,
            ),
        ])?;
        // A newer lease takes the cycle over; the fixture's lease is a zombie.
        let owner = LeaseToken {
            work_id: fx.token.work_id.clone(),
            epoch: fx.token.epoch.saturating_add(1),
            nonce: "nonce-1".to_owned(),
        };
        let record = fx.store.load(CYCLE_ID).map_err(ctx("load record"))?;
        let (_, resumed) = resume_admission(
            &record.checkpoint,
            &fx.admission,
            &fx.authority,
            owner.epoch,
        )
        .map_err(|_| TestError::Missing("resumed checkpoint"))?;
        fx.store
            .rebind(CYCLE_ID, &owner, &resumed)
            .map_err(ctx("take over"))?;
        assert!(fx.token.epoch < resumed.fence.epoch);

        // The stale lease gets no payload and the drain writes nothing.
        assert!(fx.store.pending_observations(CYCLE_ID, &fx.token).is_err());
        let stale = drain_cycle_observations(&fx.store, CYCLE_ID, &fx.token, &fx.learning_root);
        assert!(stale.is_err(), "{stale:?}");
        // Another job's lease under the current epoch is refused the same way.
        let foreign = LeaseToken {
            work_id: WorkId::from_str("another-job"),
            epoch: owner.epoch,
            nonce: "nonce-foreign".to_owned(),
        };
        let foreign_drain =
            drain_cycle_observations(&fx.store, CYCLE_ID, &foreign, &fx.learning_root);
        assert!(foreign_drain.is_err(), "{foreign_drain:?}");
        assert!(
            !fx.learning_root.join("facts").exists(),
            "a refused drain must not open the project memories"
        );
        assert!(!fx.learning_root.exists());

        // The owner still sees and learns every entry.
        let pending = fx
            .store
            .pending_observations(CYCLE_ID, &owner)
            .map_err(ctx("pending under the owner's lease"))?;
        assert_eq!(pending.len(), 2);
        let report = drain_cycle_observations(&fx.store, CYCLE_ID, &owner, &fx.learning_root)
            .map_err(TestError::Unexpected)?;
        assert_eq!(
            report,
            DrainReport {
                acked: 2,
                written: 2,
                screened_out: 0,
                pending_materialization: 0
            }
        );
        let files = fact_files(&fx.learning_root)?;
        assert_eq!(files.len(), 2);
        assert!(files.keys().all(|name| name.starts_with("pl90-")));
        Ok(())
    }

    #[test]
    fn observations_of_another_cycle_are_never_learned() -> TestResult {
        let fx = fixture_with(
            &[(
                ObservationCategory::ToolFailedNoEffect,
                CauseCode::ExecutorFailed,
            )],
            |observations| {
                for observation in observations.iter_mut() {
                    observation.event_id.cycle_id = "job-other".to_owned();
                }
            },
        )?;
        let report = drain(&fx)?;
        assert_eq!(
            report,
            DrainReport {
                acked: 1,
                written: 0,
                screened_out: 1,
                pending_materialization: 0
            }
        );
        assert!(pending_keys(&fx)?.is_empty());
        assert!(!fx.learning_root.join("facts").exists());
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
                screened_out: 0,
                pending_materialization: 0
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

    /// A learnable observation of the fixture's cycle, its cycle id replaced.
    fn observation_with_cycle_id(cycle_id: &str) -> TestResult<RunObservationV1> {
        let fx = fixture(&[(
            ObservationCategory::ToolFailedNoEffect,
            CauseCode::ExecutorFailed,
        )])?;
        let mut observation = first_payload(&fx)?;
        observation.event_id.cycle_id = cycle_id.to_owned();
        Ok(observation)
    }

    #[test]
    fn unsafe_cycle_ids_never_become_a_lesson() -> TestResult {
        // Pins `lesson_fact`'s own `safe_id` guard; the drain screens another
        // cycle's id out before it ever gets here (see
        // `observations_of_another_cycle_are_never_learned`).
        let now = Timestamp::now_utc();
        assert!(
            lesson_fact(&observation_with_cycle_id(CYCLE_ID)?, now).is_some(),
            "control: a safe id becomes a lesson"
        );
        let too_long = "c".repeat(MAX_ID_CHARS + 1);
        for unsafe_id in [
            "cycle with\nspaces",
            "",
            "cycle\u{202E}id",
            too_long.as_str(),
        ] {
            assert!(
                lesson_fact(&observation_with_cycle_id(unsafe_id)?, now).is_none(),
                "{unsafe_id:?}"
            );
        }
        Ok(())
    }

    /// Learnable observations every lease test below starts from.
    const TWO_LEARNABLE: [(ObservationCategory, CauseCode); 2] = [
        (
            ObservationCategory::ToolFailedNoEffect,
            CauseCode::ExecutorFailed,
        ),
        (
            ObservationCategory::VerificationFailed,
            CauseCode::VerificationUnmet,
        ),
    ];

    /// A job record for the fixture's cycle in the job store of the
    /// fixture's job root, claimed at `claimed_at` for `ttl`. The cycle is
    /// rebound to the claim's epoch through the unguarded store — the
    /// claim's own rebind while it was live; no reclaim follows.
    fn claimed_job(
        fx: &Fixture,
        claimed_at: jiff::Timestamp,
        ttl: SignedDuration,
    ) -> TestResult<(Arc<JobStore>, LeaseToken)> {
        let jobs = Arc::new(JobStore::new(&fx.base));
        let work_id = fx.token.work_id.clone();
        let mut job = Job::new(
            work_id.clone(),
            JobKind::Custom(INTENT_CYCLE_JOB_KIND.to_owned()),
            Budget::unbounded(),
            RetryPolicy {
                max_attempts: 2,
                base_delay: SignedDuration::from_secs(1),
                factor: 2.0,
                max_delay: SignedDuration::from_secs(10),
                jitter: 0.0,
            },
            claimed_at,
        );
        job.mark_ready(claimed_at).map_err(ctx("job is ready"))?;
        jobs.admit(&StoredJob {
            job,
            scope: JobScope::new(
                TenantId::from_str("tenant"),
                WorkspaceId::from_str("workspace"),
                ApprovalActor::Operator {
                    id: "operator".to_owned(),
                },
            ),
            input: serde_json::json!({}),
            submitted_at: claimed_at,
            not_before: claimed_at,
            lease: None,
            lease_epoch: 0,
            completion: None,
            cancellation: None,
            revision: 0,
            trace: None,
        })
        .map_err(ctx("admit job"))?;
        let claim = jobs
            .claim(
                &work_id,
                &ClaimRequest {
                    worker_id: "worker-a".to_owned(),
                    lease_ttl: ttl,
                    now: claimed_at,
                },
            )
            .map_err(ctx("claim job"))?;
        let record = fx.store.load(CYCLE_ID).map_err(ctx("load record"))?;
        let (_, resumed) = resume_admission(
            &record.checkpoint,
            &fx.admission,
            &fx.authority,
            claim.token.epoch,
        )
        .map_err(|_| TestError::Missing("resumed checkpoint"))?;
        fx.store
            .rebind(CYCLE_ID, &claim.token, &resumed)
            .map_err(ctx("rebind to the claim"))?;
        Ok((jobs, claim.token))
    }

    /// The fixture's cycle store as the worker opens it: fenced to the live
    /// claim through the production check.
    fn worker_store(fx: &Fixture, jobs: &Arc<JobStore>) -> TestResult<CycleStore> {
        Ok(CycleStore::open_in_job_root(&fx.base.join("jobs"))
            .map_err(ctx("cycle store"))?
            .with_lease_liveness(super::super::intent_cycle_job::lease_liveness(jobs)))
    }

    fn two_minutes_ago() -> TestResult<jiff::Timestamp> {
        jiff::Timestamp::now()
            .checked_sub(SignedDuration::from_secs(120))
            .map_err(ctx("two minutes ago"))
    }

    #[test]
    fn live_claim_learns_through_the_liveness_check() -> TestResult {
        let fx = fixture(&TWO_LEARNABLE)?;
        let (jobs, lease) =
            claimed_job(&fx, jiff::Timestamp::now(), SignedDuration::from_secs(60))?;
        let guarded = worker_store(&fx, &jobs)?;
        let before = jobs.get(&lease.work_id).map_err(ctx("job record"))?;

        let report = drain_cycle_observations(&guarded, CYCLE_ID, &lease, &fx.learning_root)
            .map_err(TestError::Unexpected)?;

        assert_eq!(
            report,
            DrainReport {
                acked: 2,
                written: 2,
                screened_out: 0,
                pending_materialization: 0
            }
        );
        assert_eq!(fact_files(&fx.learning_root)?.len(), 2);
        // Every hold renewed the lease like a heartbeat: same claim, later
        // expiry, nothing else changed.
        let after = jobs.get(&lease.work_id).map_err(ctx("job record after"))?;
        assert_eq!(after.job.state, before.job.state);
        assert_eq!(after.lease_epoch, before.lease_epoch);
        let (held_before, held_after) = match (before.lease.as_ref(), after.lease.as_ref()) {
            (Some(held_before), Some(held_after)) => (held_before, held_after),
            _ => return Err(TestError::Missing("the claim's lease")),
        };
        assert_eq!(held_after.token(), lease);
        assert!(held_after.expires_at >= held_before.expires_at);
        assert!(after.revision > before.revision, "renewals are durable");
        Ok(())
    }

    #[test]
    fn accepted_entries_are_materialised_even_if_they_would_now_be_screened_out() -> TestResult {
        // Accepted by an earlier drain whose classification still learned
        // admission refusals; today's drain would acknowledge them unlearned.
        let fx = fixture(&[(
            ObservationCategory::AdmissionRefused,
            CauseCode::ReadNotAllowed,
        )])?;
        let keys = pending_keys(&fx)?;
        assert!(!is_learnable(ObservationCategory::AdmissionRefused));
        assert_eq!(
            fx.store
                .decide_observations(CYCLE_ID, &fx.token, &keys, &[])
                .map_err(ctx("an earlier decision"))?,
            (1, 0)
        );

        let report = drain(&fx)?;
        assert_eq!(
            report,
            DrainReport {
                acked: 1,
                written: 1,
                screened_out: 0,
                pending_materialization: 0
            }
        );
        assert!(pending_keys(&fx)?.is_empty());
        let facts = FactStore::open(&fx.learning_root, FactScope::Project)
            .map_err(ctx("open facts"))?
            .list()
            .map_err(ctx("list facts"))?;
        let descriptions: Vec<&str> = facts.iter().map(|fact| fact.description.as_str()).collect();
        assert_eq!(
            descriptions,
            vec!["Intent cycle observed admission_refused/read_not_allowed on transition wait"],
            "the committed decision is materialised, not re-screened"
        );
        Ok(())
    }

    #[test]
    fn accepted_entry_that_cannot_be_reconstructed_stays_accepted() -> TestResult {
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
        let keys = pending_keys(&fx)?;
        fx.store
            .decide_observations(CYCLE_ID, &fx.token, &keys, &[])
            .map_err(ctx("an earlier decision"))?;

        // Drained twice: never acknowledged without its fact.
        for _ in 0..2 {
            let report = drain(&fx)?;
            assert_eq!(
                report,
                DrainReport {
                    acked: 0,
                    written: 0,
                    screened_out: 0,
                    pending_materialization: 1
                }
            );
            assert_eq!(pending_keys(&fx)?, keys);
            assert_eq!(entry_states(&fx)?, vec![DeliveryState::Accepted]);
        }
        assert!(
            !fx.learning_root.exists(),
            "nothing to write, so the project memories are never created"
        );
        Ok(())
    }

    #[test]
    fn expired_claim_cannot_learn_even_before_a_rebind() -> TestResult {
        let fx = fixture(&TWO_LEARNABLE)?;
        // Claimed two minutes ago for one second: the lease expired long ago,
        // and neither a reclaim nor a rebind followed.
        let (jobs, lease) = claimed_job(&fx, two_minutes_ago()?, SignedDuration::from_secs(1))?;
        let record = fx.store.load(CYCLE_ID).map_err(ctx("load record"))?;
        assert_eq!(
            record.checkpoint.fence.epoch, lease.epoch,
            "same epoch: nothing rebound the cycle after the expiry"
        );
        let job = jobs.get(&lease.work_id).map_err(ctx("job record"))?;
        assert_eq!(job.lease_epoch, lease.epoch, "no reclaim");
        assert_eq!(
            job.lease.as_ref().map(|held| held.token()),
            Some(lease.clone())
        );
        // The epoch fence alone would still hand the expired lease the payloads.
        assert_eq!(
            fx.store
                .pending_observations(CYCLE_ID, &lease)
                .map_err(ctx("unguarded read"))?
                .len(),
            2
        );

        let guarded = worker_store(&fx, &jobs)?;
        let reason = match drain_cycle_observations(&guarded, CYCLE_ID, &lease, &fx.learning_root) {
            Err(reason) => reason,
            Ok(report) => {
                return Err(TestError::Unexpected(format!(
                    "an expired claim drained: {report:?}"
                )));
            }
        };
        assert!(reason.contains("job lease is not live"), "{reason}");
        assert!(reason.contains("expired"), "{reason}");
        assert!(
            !fx.learning_root.join("facts").exists(),
            "no fact file may exist"
        );
        assert!(
            !fx.learning_root.exists(),
            "the project memories are never opened"
        );
        // Both entries stay pending for the claim that takes over.
        assert_eq!(
            fx.store
                .pending_observations(CYCLE_ID, &lease)
                .map_err(ctx("pending after the refusal"))?
                .len(),
            2
        );
        Ok(())
    }

    /// A lease guard that is live for the first `live_for` holds and refuses
    /// every later one; `holds` counts the attempts.
    struct CountingGuard {
        live_for: usize,
        holds: Arc<AtomicUsize>,
    }

    /// A hold that never passes its deadline.
    struct LiveHold;

    impl LeaseHold for LiveHold {
        fn still_live(&self) -> Result<(), String> {
            Ok(())
        }
    }

    impl LeaseLivenessGuard for CountingGuard {
        fn hold(&self, _token: &LeaseToken) -> Result<Box<dyn LeaseHold>, String> {
            if self.holds.fetch_add(1, Ordering::SeqCst) < self.live_for {
                Ok(Box::new(LiveHold))
            } else {
                Err(format!("lease lost after {} operations", self.live_for))
            }
        }
    }

    /// The fixture's cycle store behind a [`CountingGuard`].
    fn counting_store(fx: &Fixture, live_for: usize) -> TestResult<(CycleStore, Arc<AtomicUsize>)> {
        let holds = Arc::new(AtomicUsize::new(0));
        let store = CycleStore::open_in_job_root(&fx.base.join("jobs"))
            .map_err(ctx("cycle store"))?
            .with_lease_liveness(Arc::new(CountingGuard {
                live_for,
                holds: Arc::clone(&holds),
            }));
        Ok((store, holds))
    }

    /// Delivery states of the pending entries, in outbox order.
    fn entry_states(fx: &Fixture) -> TestResult<Vec<DeliveryState>> {
        Ok(fx
            .store
            .load(CYCLE_ID)
            .map_err(ctx("load record"))?
            .outbox
            .pending()
            .iter()
            .map(|entry| entry.state)
            .collect())
    }

    #[test]
    fn lease_lost_between_read_and_decision_writes_no_fact() -> TestResult {
        let fx = fixture(&TWO_LEARNABLE)?;
        // Live for the read, gone at the decision: nothing may be accepted,
        // acknowledged or written.
        let (guarded, holds) = counting_store(&fx, 1)?;

        let reason =
            match drain_cycle_observations(&guarded, CYCLE_ID, &fx.token, &fx.learning_root) {
                Err(reason) => reason,
                Ok(report) => {
                    return Err(TestError::Unexpected(format!(
                        "a lost lease drained: {report:?}"
                    )));
                }
            };
        assert_eq!(
            reason,
            "decide observations: job lease is not live: lease lost after 1 operations"
        );
        assert_eq!(
            holds.load(Ordering::SeqCst),
            2,
            "the read and the decision each held the lease"
        );
        assert!(
            !fx.learning_root.exists(),
            "no fact without a decision committed under the lease"
        );
        assert_eq!(
            entry_states(&fx)?,
            vec![DeliveryState::Pending; 2],
            "nothing was accepted"
        );
        Ok(())
    }

    #[test]
    fn lease_lost_before_the_final_ack_keeps_lessons_accepted() -> TestResult {
        let fx = fixture(&TWO_LEARNABLE)?;
        // Live for the read and the decision; gone before the facts are
        // acknowledged. The facts materialise the committed decision, the
        // entries stay accepted until a live lease acknowledges them.
        let (guarded, holds) = counting_store(&fx, 2)?;

        let reason =
            match drain_cycle_observations(&guarded, CYCLE_ID, &fx.token, &fx.learning_root) {
                Err(reason) => reason,
                Ok(report) => {
                    return Err(TestError::Unexpected(format!(
                        "a lost lease acknowledged: {report:?}"
                    )));
                }
            };
        assert!(
            reason.starts_with("acknowledge 2 written lessons (they stay accepted)"),
            "{reason}"
        );
        assert_eq!(holds.load(Ordering::SeqCst), 3);
        let written = fact_files(&fx.learning_root)?;
        assert_eq!(written.len(), 2);
        assert_eq!(entry_states(&fx)?, vec![DeliveryState::Accepted; 2]);

        // The next drain under a live lease rewrites the same files and
        // acknowledges them.
        let report = drain(&fx)?;
        assert_eq!(
            report,
            DrainReport {
                acked: 2,
                written: 2,
                screened_out: 0,
                pending_materialization: 0
            }
        );
        assert!(pending_keys(&fx)?.is_empty());
        let rewritten = fact_files(&fx.learning_root)?;
        assert_eq!(
            rewritten.keys().collect::<Vec<_>>(),
            written.keys().collect::<Vec<_>>(),
            "idempotent: the same fact files"
        );
        Ok(())
    }

    /// Makes `FactStore::open` fail: a regular file where `facts/` belongs.
    fn block_project_memories(fx: &Fixture) -> TestResult {
        std::fs::create_dir_all(&fx.learning_root).map_err(ctx("create learning root"))?;
        std::fs::write(fx.learning_root.join("facts"), b"not a directory")
            .map_err(ctx("block the facts directory"))
    }

    fn unblock_project_memories(fx: &Fixture) -> TestResult {
        std::fs::remove_file(fx.learning_root.join("facts")).map_err(ctx("unblock facts"))
    }

    #[test]
    fn crash_after_accept_before_write_is_recovered_on_the_next_drain() -> TestResult {
        let fx = fixture(&TWO_LEARNABLE)?;
        let keys = pending_keys(&fx)?;
        // A drain committed its decision and the process died before any
        // fact was written.
        assert_eq!(
            fx.store
                .decide_observations(CYCLE_ID, &fx.token, &keys, &[])
                .map_err(ctx("decision before the crash"))?,
            (2, 0)
        );
        assert_eq!(entry_states(&fx)?, vec![DeliveryState::Accepted; 2]);

        // The next drain cannot open the project memories: nothing is lost,
        // nothing is written, the entries stay accepted.
        block_project_memories(&fx)?;
        let blocked = drain(&fx);
        let Err(TestError::Unexpected(reason)) = blocked else {
            return Err(TestError::Missing("a drain with blocked memories fails"));
        };
        assert!(reason.contains("open project memories"), "{reason}");
        assert!(
            reason.contains("2 accepted lessons stay pending"),
            "{reason}"
        );
        assert!(fx.learning_root.join("facts").is_file(), "no fact written");
        assert_eq!(entry_states(&fx)?, vec![DeliveryState::Accepted; 2]);

        // Repaired: the accepted lessons are materialised and acknowledged.
        unblock_project_memories(&fx)?;
        let report = drain(&fx)?;
        assert_eq!(
            report,
            DrainReport {
                acked: 2,
                written: 2,
                screened_out: 0,
                pending_materialization: 0
            }
        );
        assert_eq!(fact_files(&fx.learning_root)?.len(), 2);
        assert!(pending_keys(&fx)?.is_empty());
        Ok(())
    }

    #[test]
    fn partial_write_failure_keeps_the_failed_entry_accepted() -> TestResult {
        let fx = fixture(&TWO_LEARNABLE)?;
        let keys = pending_keys(&fx)?;
        let (first_key, second_key) = match keys.as_slice() {
            [first, second] => (first.clone(), second.clone()),
            _ => return Err(TestError::Missing("two pending keys")),
        };
        // The first lesson's fact path is occupied by a directory: its write
        // fails, the second one succeeds.
        let first = lesson_fact(&first_payload(&fx)?, Timestamp::now_utc())
            .ok_or(TestError::Missing("first lesson"))?;
        let occupied = fx
            .learning_root
            .join("facts")
            .join(format!("{}.md", first.name));
        std::fs::create_dir_all(&occupied).map_err(ctx("occupy the first fact path"))?;

        let report = drain(&fx)?;
        assert_eq!(
            report,
            DrainReport {
                acked: 1,
                written: 1,
                screened_out: 0,
                pending_materialization: 1
            }
        );
        assert_eq!(pending_keys(&fx)?, vec![first_key.clone()]);
        assert_eq!(entry_states(&fx)?, vec![DeliveryState::Accepted]);
        assert!(!pending_keys(&fx)?.contains(&second_key));

        // Repaired: the failed lesson is written and acknowledged.
        std::fs::remove_dir(&occupied).map_err(ctx("free the first fact path"))?;
        let report = drain(&fx)?;
        assert_eq!(
            report,
            DrainReport {
                acked: 1,
                written: 1,
                screened_out: 0,
                pending_materialization: 0
            }
        );
        assert!(pending_keys(&fx)?.is_empty());
        let files = fact_files(&fx.learning_root)?;
        assert_eq!(files.len(), 2);
        assert!(files.contains_key(&format!("{}.md", first.name)));
        Ok(())
    }

    #[test]
    fn screened_entries_are_acked_directly_without_materialization() -> TestResult {
        let fx = fixture_with(
            &[
                (
                    ObservationCategory::ToolFailedNoEffect,
                    CauseCode::ExecutorFailed,
                ),
                (
                    ObservationCategory::AdmissionRefused,
                    CauseCode::ReadNotAllowed,
                ),
                (
                    ObservationCategory::VerificationFailed,
                    CauseCode::VerificationUnmet,
                ),
            ],
            |observations| {
                if let Some(other) = observations.get_mut(2) {
                    other.event_id.cycle_id = "job-other".to_owned();
                }
            },
        )?;
        let keys = pending_keys(&fx)?;
        let learnable = keys
            .first()
            .cloned()
            .ok_or(TestError::Missing("learnable key"))?;
        // Materialisation is impossible, yet the decision still settles the
        // entries that need none.
        block_project_memories(&fx)?;
        let blocked = drain(&fx);
        assert!(blocked.is_err(), "{blocked:?}");
        assert_eq!(
            pending_keys(&fx)?,
            vec![learnable],
            "the non-learnable and the foreign entry are acknowledged"
        );
        assert_eq!(entry_states(&fx)?, vec![DeliveryState::Accepted]);

        unblock_project_memories(&fx)?;
        let report = drain(&fx)?;
        assert_eq!(
            report,
            DrainReport {
                acked: 1,
                written: 1,
                screened_out: 0,
                pending_materialization: 0
            }
        );
        assert!(pending_keys(&fx)?.is_empty());
        Ok(())
    }

    /// Wraps a lease guard and runs `between` once, right after the lease was
    /// verified and is held: the window between the lease check and the
    /// cycle commit.
    struct PausedGuard {
        inner: Arc<dyn LeaseLivenessGuard>,
        between: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    }

    impl LeaseLivenessGuard for PausedGuard {
        fn hold(&self, token: &LeaseToken) -> Result<Box<dyn LeaseHold>, String> {
            let held = self.inner.hold(token)?;
            let between = self.between.lock().ok().and_then(|mut slot| slot.take());
            if let Some(between) = between {
                between();
            }
            Ok(held)
        }
    }

    /// What the reclaimer reports: attempts until it won, the delivery
    /// states it found right after winning, and when the job is claimable
    /// again.
    type Reclaimed = (usize, Vec<DeliveryState>, Option<jiff::Timestamp>);

    #[test]
    fn reclaim_cannot_slip_between_lease_check_and_cycle_commit() -> TestResult {
        let fx = fixture(&TWO_LEARNABLE)?;
        let (jobs, lease) =
            claimed_job(&fx, jiff::Timestamp::now(), SignedDuration::from_secs(60))?;
        let keys = pending_keys(&fx)?;

        // An operator reclaim (revoke the lease, requeue the job) that starts
        // inside the window and retries until it wins.
        let (go, go_seen) = mpsc::channel::<()>();
        let (tried, tried_seen) = mpsc::channel::<()>();
        let reclaimer = {
            let jobs = Arc::clone(&jobs);
            let observer =
                CycleStore::open_in_job_root(&fx.base.join("jobs")).map_err(ctx("observer"))?;
            let work_id = lease.work_id.clone();
            std::thread::spawn(move || -> Result<Reclaimed, String> {
                go_seen.recv().map_err(|error| error.to_string())?;
                for attempt in 1..=2_000_usize {
                    let result = jobs.reclaim(
                        &work_id,
                        jiff::Timestamp::now(),
                        ApprovalActor::Operator {
                            id: "operator".to_owned(),
                        },
                    );
                    if attempt == 1 {
                        tried.send(()).map_err(|error| error.to_string())?;
                    }
                    match result {
                        Ok(expired) => {
                            let states = observer
                                .load(CYCLE_ID)
                                .map_err(|error| error.to_string())?
                                .outbox
                                .pending()
                                .iter()
                                .map(|entry| entry.state)
                                .collect();
                            return Ok((attempt, states, expired.retry_scheduled_for));
                        }
                        Err(SessionStoreError::JobLockContended { .. }) => {
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                        Err(other) => return Err(other.to_string()),
                    }
                }
                Err("the reclaim never got the job lock".to_owned())
            })
        };
        let between: Box<dyn FnOnce() + Send> = Box::new(move || {
            // Inside the window: the production guard holds the job lock.
            assert!(go.send(()).is_ok(), "the reclaimer waits for the window");
            assert!(tried_seen.recv().is_ok(), "the reclaimer tried once");
        });
        let guarded = CycleStore::open_in_job_root(&fx.base.join("jobs"))
            .map_err(ctx("cycle store"))?
            .with_lease_liveness(Arc::new(PausedGuard {
                inner: super::super::intent_cycle_job::lease_liveness(&jobs),
                between: Mutex::new(Some(between)),
            }));

        // The commit under the held lease: the drain's decision.
        let decided = guarded
            .decide_observations(CYCLE_ID, &lease, &keys, &[])
            .map_err(ctx("decision under the held lease"))?;
        assert_eq!(decided, (2, 0));
        let (attempts, states_when_reclaimed, requeued_at) = reclaimer
            .join()
            .map_err(|_| TestError::Missing("reclaimer thread"))?
            .map_err(TestError::Unexpected)?;
        assert!(
            attempts > 1,
            "the reclaim was refused while the lease was held"
        );
        assert_eq!(
            states_when_reclaimed,
            vec![DeliveryState::Accepted; 2],
            "the reclaim won only after the commit was durable"
        );

        // The reclaim replaced the lease: the old token gets nothing more.
        let job = jobs.get(&lease.work_id).map_err(ctx("job record"))?;
        assert!(job.lease_epoch > lease.epoch);
        assert!(job.lease.is_none());
        let late = guarded.ack_observations(CYCLE_ID, &lease, &keys);
        assert!(
            matches!(late, Err(CycleStoreError::LeaseNotLive(_))),
            "{late:?}"
        );
        let late_drain = drain_cycle_observations(&guarded, CYCLE_ID, &lease, &fx.learning_root);
        assert!(late_drain.is_err(), "{late_drain:?}");
        assert!(!fx.learning_root.exists(), "the old lease learned nothing");
        assert_eq!(entry_states(&fx)?, vec![DeliveryState::Accepted; 2]);

        // The claim that takes over materialises the accepted lessons.
        let requeued_at = requeued_at.ok_or(TestError::Missing("a retry is scheduled"))?;
        let next = jobs
            .claim(
                &lease.work_id,
                &ClaimRequest {
                    worker_id: "worker-b".to_owned(),
                    lease_ttl: SignedDuration::from_secs(60),
                    now: requeued_at,
                },
            )
            .map_err(ctx("the next claim"))?;
        let record = fx.store.load(CYCLE_ID).map_err(ctx("load record"))?;
        let (_, resumed) = resume_admission(
            &record.checkpoint,
            &fx.admission,
            &fx.authority,
            next.token.epoch,
        )
        .map_err(|_| TestError::Missing("resumed checkpoint"))?;
        fx.store
            .rebind(CYCLE_ID, &next.token, &resumed)
            .map_err(ctx("rebind to the next claim"))?;
        let report = drain_cycle_observations(
            &worker_store(&fx, &jobs)?,
            CYCLE_ID,
            &next.token,
            &fx.learning_root,
        )
        .map_err(TestError::Unexpected)?;
        assert_eq!(
            report,
            DrainReport {
                acked: 2,
                written: 2,
                screened_out: 0,
                pending_materialization: 0
            }
        );
        Ok(())
    }

    /// Proposer and executor of a run that is cancelled before its first
    /// step; any call is a test failure surfaced as a failed step.
    struct NoStep;

    impl CycleProposer for NoStep {
        async fn propose(
            &mut self,
            _admission: &CycleAdmission,
            _state: &CycleCheckpoint,
            _last_refusal: Option<CycleRefusal>,
            _cancel: &CancelToken,
        ) -> Result<CycleProposal, StepFailure> {
            Err(StepFailure::new("no proposal expected after cancellation"))
        }
    }

    impl CycleStepExecutor for NoStep {
        async fn execute(
            &mut self,
            _step: &InFlightStep,
            _admission: &CycleAdmission,
            _state: &CycleCheckpoint,
            _cancel: &CancelToken,
        ) -> Result<CycleObservations, StepFailure> {
            Err(StepFailure::new("no step expected after cancellation"))
        }

        async fn reconcile(&mut self, _step: &InFlightStep) -> StepReconciliation {
            StepReconciliation::Uncertain
        }
    }

    #[tokio::test]
    async fn cancellation_after_expiry_writes_no_fact() -> TestResult {
        let fx = fixture(&TWO_LEARNABLE)?;
        let (jobs, lease) = claimed_job(&fx, two_minutes_ago()?, SignedDuration::from_secs(1))?;
        let guarded = Arc::new(worker_store(&fx, &jobs)?);
        let before = fx.store.load(CYCLE_ID).map_err(ctx("load record"))?;

        // The lease loss cancelled the claim (job control → cancel token)
        // before the next step; the driver returns `Cancelled`.
        let cancel = CancelToken::new();
        cancel.cancel(CancelReason::User);
        let mut driver = CycleDriver::new(
            Arc::clone(&guarded),
            NoStep,
            NoStep,
            Arc::new(CeilingReissuer::new(fx.authority.clone())),
        );
        let outcome = driver
            .run(CYCLE_ID, &lease, &fx.admission, &cancel)
            .await
            .map_err(ctx("cancelled run"))?;
        assert!(
            matches!(outcome, CycleRunOutcome::Cancelled { .. }),
            "{outcome:?}"
        );
        assert_eq!(
            fx.store.load(CYCLE_ID).map_err(ctx("reload"))?,
            before,
            "the cancelled run wrote nothing"
        );

        // What the worker does next (`drain_learning`): drain under the claim.
        let drained = drain_cycle_observations(&guarded, CYCLE_ID, &lease, &fx.learning_root);
        assert!(
            drained
                .as_ref()
                .is_err_and(|reason| reason.contains("job lease is not live")),
            "{drained:?}"
        );
        assert!(!fx.learning_root.exists(), "no fact file may exist");
        assert_eq!(
            fx.store
                .pending_observations(CYCLE_ID, &lease)
                .map_err(ctx("pending after the refusal"))?
                .len(),
            2
        );
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
            CauseCode::ProposerFailed,
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
    fn lessons_are_not_crowded_out_by_ordinary_facts() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("temp directory"))?;
        let root = temp.path().join("memories");
        let store = FactStore::open(&root, FactScope::Project).map_err(ctx("open facts"))?;
        // Ordinary notes that match every keyword in name, description and tags.
        for index in 0..40 {
            store
                .write(&lesson(
                    &format!("advance-parser-error-handling-{index:02}"),
                    "advance the parser error handling: parser error handling notes",
                    FactScope::Project,
                    &["advance", "parser", "error", "handling"],
                ))
                .map_err(ctx("write note"))?;
        }
        let pitfall =
            "Intent cycle observed tool_failed_no_effect/executor_failed on transition advance";
        store
            .write(&lesson(
                "pl90-tool-failed-no-effect-executor-failed-0123456789ab",
                pitfall,
                FactScope::Project,
                &[
                    LESSON_TAG,
                    "tool_failed_no_effect",
                    "executor_failed",
                    "advance",
                ],
            ))
            .map_err(ctx("write lesson"))?;
        // Precondition: the store's own keyword search ranks and cuts all
        // facts first, so its top results hold no lesson at all.
        let searched = store
            .search(&["advance", "parser", "error", "handling"], SEARCH_LIMIT)
            .map_err(ctx("search"))?;
        assert_eq!(searched.len(), SEARCH_LIMIT);
        assert!(
            searched
                .iter()
                .all(|fact| !fact.tags.iter().any(|tag| tag == LESSON_TAG))
        );

        let lines = lesson_lines(&root, "Advance the parser error handling", 0);
        assert_eq!(lines, vec![pitfall.to_owned()]);
        Ok(())
    }

    #[test]
    fn ranking_counts_keywords_and_breaks_ties_by_name() {
        let words = keywords("parser cache loader");
        let make = |name: &str, description: &str| {
            lesson(name, description, FactScope::Project, &[LESSON_TAG])
        };
        let ranked: Vec<String> = ranked_lessons(
            vec![
                make("pl90-b", "parser problem"),
                make("pl90-a", "parser problem"),
                make("pl90-c", "parser and cache problem"),
                make("pl90-d", "unrelated problem"),
                lesson(
                    "pl90-e",
                    "parser cache loader",
                    FactScope::Global,
                    &[LESSON_TAG],
                ),
            ],
            &words,
        )
        .into_iter()
        .map(|fact| fact.name)
        .collect();
        assert_eq!(ranked, ["pl90-c", "pl90-a", "pl90-b"].map(str::to_owned));
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
