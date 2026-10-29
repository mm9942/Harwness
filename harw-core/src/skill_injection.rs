//! Triggered skill injection per model round (PL-95 S3, PL-97 B1).
//!
//! # Responsibility
//! [`SkillInjector`] wires the pure selection core of `harw-catalog`
//! ([`harw_catalog::select`], `triggers.toml` → [`harw_catalog::TriggerIndex`])
//! into the turn loop: before every model round it turns the conversation
//! items appended since the previous round into [`TriggerEvent`]s, selects the
//! matching skill sections under the configured budgets and returns them as
//! [`harw_context::Fragment`]s of section [`harw_catalog::FRAGMENT_SECTION`]
//! (`skills.triggered`, [`harw_context::Stability::Fresh`]).
//!
//! # Event feed
//! From the history items since the last round:
//! - user text → [`TriggerEvent::TaskText`] (keywords, error codes);
//! - tool results → [`TriggerEvent::ToolOutput`] (error codes; string leaves of
//!   the result value, so JSON escaping never glues tokens together; a long
//!   result keeps its head and its tail, where compiler and test errors land);
//! - the first call of a tool name in this session → [`TriggerEvent::ToolFirstUse`];
//! - path arguments of `fs.*`/`doc.*` calls → [`TriggerEvent::PathTouched`];
//! - command arguments of `shell.*`/`process.*`/`job.start` calls →
//!   [`TriggerEvent::Command`].
//!
//! The first round of a session (and the first round after a history
//! rewrite) starts at the latest user message: a resumed or compacted
//! conversation does not re-trigger on old text, and tools called before
//! that point count as already used.
//!
//! # Lifetime of an excerpt
//! Fragments are not part of the history, so a delivered excerpt stays
//! *active* and is re-emitted unchanged in every following round (append-only
//! between compactions, which keeps the request prefix stable). Dedupe and the
//! session cap come from [`harw_catalog::InjectionState`]: a skill section is
//! delivered at most once while it is active. An excerpt leaves the context
//! - after a compaction ([`SkillInjector::reset_after_compaction`], or a
//!   detected history rewrite): every skill may be delivered again and the
//!   session budget starts over;
//! - when newer excerpts need its room in the section share (oldest first,
//!   [`RoundInjection::evicted`]): it may be delivered again, its bytes stay
//!   counted against the session cap;
//! - when the admit predicate of [`SkillInjector::prepare_round_with`] or the
//!   request montage ([`SkillInjector::reconcile`]) drops it: it may be
//!   delivered again, and if it never reached the model its bytes are
//!   released.
//!
//! A selection only becomes a delivery once the model answered a request
//! carrying it ([`SkillInjector::mark_answered`]): from then on its bytes
//! stay charged, and it waits for its `injection` event
//! ([`SkillInjector::pending_deliveries`]) until that event is durable
//! ([`SkillInjector::confirm_recorded`]). A request rebuilt after an
//! emergency compaction, or retried after a context-length error, records
//! each excerpt exactly once; a failed append stays pending for the next
//! answered round, even when the excerpt left the context in the meantime
//! (only a compaction discards it, see
//! [`SkillInjector::reset_after_compaction`]).
//!
//! # Resume (PL-97 H7)
//! The state survives a restart of the same session: every answered
//! delivery is recorded with its framed excerpt, and
//! [`crate::session::AgentSession::hydrate_from_store`] hands the records of
//! the current context epoch (since the last compaction) to
//! [`SkillInjector::restore`] before the next turn. The active excerpts,
//! the dedupe state and the charged session budget are rebuilt from them;
//! a store without records yields a fresh state.
//!
//! # Budgets
//! Per round, for the whole section (active excerpts included): ~3 % of the
//! model window ([`harw_context::sources::skills_triggered`]), at least
//! [`MIN_PER_TURN_BYTES`], at most the configured cap (default 4 KiB). Per
//! session (until the next compaction): the configured cap (default 16 KiB),
//! at most [`SESSION_BUDGET_ROUNDS`] per-round budgets. The L0 catalog uses
//! [`SkillInjectionSettings::l0_bytes`].
//!
//! # Trust
//! Bundled and trusted-layer skills are framed as [`TrustClass::Instruction`],
//! imported skills as [`TrustClass::Evidence`] — and only when
//! `[skills] inject_imported = true`. A skill never grants rights; an excerpt
//! is text and nothing else. Every delivery is reported as a
//! [`DeliveredSkill`] for the session store's `injection` event.
//!
//! # Tool availability (PL-97 B2)
//! With [`SkillInjector::with_tool_availability`], a triggered skill that
//! declares tools of which this session registers none is skipped
//! ([`RoundInjection::unavailable`]): a session without `sys.*` that runs
//! `ps aux` through `shell.exec` gets no `sys-tools` excerpt telling it to
//! use tools it does not have. A skill declares the tools of its manifest
//! (`skill.toml` `tools = [...]`) and the tools its `triggers.toml` fires on
//! (`[triggers] tools`; `cargo-tools` fires on `cargo.test_one`, which its
//! manifest does not list). Only declared names the [`ToolAvailability`]
//! knows as tool names count; abstract capability names (`filesystem.read`)
//! never filter a skill.
//!
//! A skipped skill takes no room: it costs no session bytes, and when it
//! displaced other hits of the same round from the per-round budget the
//! selection runs again over the same events with the room it left, so
//! those hits are judged as if the skipped skill had never been a
//! candidate. The passes go on while each one skips at least one further
//! skill for its tools and other hits were dropped for the budget; every
//! triggered skill is skipped at most once, so the round needs at most one
//! pass per candidate plus one (four budget-filling unavailable skills
//! ahead of an available one take five passes). It stays
//! marked until the next compaction (the session's tools do not change), and
//! a later hit is reported as unavailable again, not as already delivered.
//! Without the filter (the default) every triggered skill is eligible.
//!
//! # Preview
//! [`SkillInjector::preview`] runs the selection of a fresh session's first
//! round over explicit [`SkillPreviewInput`]s instead of a history (PL-97 B3:
//! `harw skill preview`, `/skills preview`): same settings, budget, index and
//! framing as the turn loop, no model call, no session state.
//!
//! # Concurrency
//! `Send + Sync`; the per-session state sits behind a `Mutex` that is never
//! held across an `.await`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use harw_catalog::{
    DEFAULT_L0_BYTES, DropReason, Dropped, Injection, InjectionBudget, InjectionState, SkillIndex,
    SkillSource, TriggerEvent, TriggerKind, TriggerReason,
};
use harw_context::{Fragment, TrustClass};
use harw_protocol::{ContentPart, ToolCallResult, TurnItem};
use harw_types::ItemId;

use crate::history::ConversationHistory;

/// Lower bound of the per-round budget derived from the model window.
pub const MIN_PER_TURN_BYTES: usize = 1024;

/// The session cap is at most this many per-round budgets of the active
/// window (until the next compaction).
pub const SESSION_BUDGET_ROUNDS: usize = 4;

/// Upper bound of the text scanned per tool result (same as
/// [`harw_catalog::MAX_SCAN_BYTES`]): its first and its last half.
const MAX_RESULT_SCAN_BYTES: usize = harw_catalog::MAX_SCAN_BYTES;

/// Argument keys of `fs.*`/`doc.*` calls that name a path.
const PATH_KEYS: &[&str] = &[
    "path",
    "paths",
    "file",
    "files",
    "file_path",
    "dir",
    "directory",
];

/// Argument keys of command runners that carry the command line.
const COMMAND_KEYS: &[&str] = &["command", "cmd", "argv"];

/// Settings of one [`SkillInjector`] (from `[skills]`, see
/// [`harw_config::SkillsSection`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkillInjectionSettings {
    /// L1 injection on/off. Off: no selection, no fragment, no event.
    pub enabled: bool,
    /// Deliver imported skills (as [`TrustClass::Evidence`]).
    pub inject_imported: bool,
    /// Upper bound of the per-round budget in bytes.
    pub per_turn_cap_bytes: usize,
    /// Configured per-session cap in bytes (until the next compaction); see
    /// [`Self::session_cap_bytes`] for the effective value.
    pub per_session_bytes: usize,
    /// Budget of the L0 catalog lines in bytes.
    pub l0_bytes: usize,
}

impl Default for SkillInjectionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            inject_imported: false,
            per_turn_cap_bytes: harw_catalog::DEFAULT_PER_TURN_BYTES,
            per_session_bytes: harw_catalog::DEFAULT_PER_SESSION_BYTES,
            l0_bytes: DEFAULT_L0_BYTES,
        }
    }
}

impl SkillInjectionSettings {
    /// Settings from the resolved `[skills]` section.
    #[must_use]
    pub fn from_config(section: &harw_config::SkillsSection) -> Self {
        Self {
            enabled: section.triggered_injection,
            inject_imported: section.inject_imported,
            per_turn_cap_bytes: section.per_turn_bytes,
            per_session_bytes: section.per_session_bytes,
            l0_bytes: DEFAULT_L0_BYTES,
        }
    }

    /// The per-round budget of the `skills.triggered` section for a model
    /// window of `window_tokens`.
    ///
    /// # Description
    /// ~3 % of the window (4 bytes per token), at least [`MIN_PER_TURN_BYTES`],
    /// at most [`Self::per_turn_cap_bytes`] (the cap wins over the floor). An
    /// unknown window (`0`) yields the cap. It bounds the new deliveries of a
    /// round and the active excerpts every request carries.
    #[must_use]
    pub fn per_turn_bytes(&self, window_tokens: u64) -> usize {
        if window_tokens == 0 {
            return self.per_turn_cap_bytes;
        }
        let window_bytes = window_tokens.saturating_mul(4);
        let share = harw_context::sources::skills_triggered.share_of(window_bytes);
        usize::try_from(share)
            .unwrap_or(usize::MAX)
            .max(MIN_PER_TURN_BYTES)
            .min(self.per_turn_cap_bytes)
    }

    /// The effective session cap for a model window of `window_tokens`:
    /// [`Self::per_session_bytes`], at most [`SESSION_BUDGET_ROUNDS`] times
    /// [`Self::per_turn_bytes`] — a small window never pays for a large
    /// configured session cap.
    #[must_use]
    pub fn session_cap_bytes(&self, window_tokens: u64) -> usize {
        self.per_session_bytes.min(
            self.per_turn_bytes(window_tokens)
                .saturating_mul(SESSION_BUDGET_ROUNDS),
        )
    }

    /// The selection budget for a model window of `window_tokens`.
    #[must_use]
    pub fn budget(&self, window_tokens: u64) -> InjectionBudget {
        InjectionBudget {
            per_turn_bytes: self.per_turn_bytes(window_tokens),
            per_session_bytes: self.session_cap_bytes(window_tokens),
            l0_bytes: self.l0_bytes,
        }
    }
}

/// Where a skill comes from, for its trust class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillProvenance {
    /// Embedded in the `harw` binary.
    Bundled,
    /// From a trusted config layer.
    TrustedLayer,
    /// Marked as imported from outside (`harw skill import`, PL-95 S8).
    Imported,
}

impl SkillProvenance {
    /// The trust class of an excerpt of such a skill.
    #[must_use]
    pub fn trust(self) -> TrustClass {
        match self {
            Self::Bundled | Self::TrustedLayer => TrustClass::Instruction,
            Self::Imported => TrustClass::Evidence,
        }
    }
}

/// One excerpt selected for the first time in a round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveredSkill {
    /// Skill name.
    pub skill: String,
    /// Delivered `##` section; `None` = the whole skill.
    pub section: Option<String>,
    /// Fragment label (`skill:<name>` or `skill:<name>:<section>`).
    pub label: String,
    /// Trigger kind.
    pub kind: TriggerKind,
    /// The manifest entry that matched.
    pub key: String,
    /// Bytes of the framed excerpt.
    pub bytes: usize,
    /// SHA-256 of the full skill text.
    pub sha256: String,
    /// Trust class of the fragment.
    pub trust: TrustClass,
    /// The framed excerpt (the fragment body), recorded so that a resumed
    /// session can re-establish it ([`SkillInjector::restore`]).
    pub text: String,
}

/// A delivery the model answered whose `injection` event is not durable yet
/// ([`SkillInjector::pending_deliveries`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRecord {
    /// The delivery.
    pub delivery: DeliveredSkill,
    /// The turn whose answered request first carried the excerpt; a retried
    /// append keeps it, so the event names the turn the model received the
    /// excerpt in.
    pub turn_id: String,
}

impl PendingRecord {
    /// The `injection` record of this delivery.
    #[must_use]
    pub fn to_record(&self) -> harw_session_store::InjectionRecord {
        self.delivery.to_record(&self.turn_id)
    }
}

impl DeliveredSkill {
    fn of(injection: &Injection, label: String, trust: TrustClass) -> Self {
        Self {
            skill: injection.skill.clone(),
            section: injection.section.clone(),
            label,
            kind: injection.reason.kind,
            key: injection.reason.key.clone(),
            bytes: injection.bytes,
            sha256: injection.sha256.clone(),
            trust,
            text: injection.text.clone(),
        }
    }

    /// The session-store `injection` event of this delivery in `turn_id`.
    #[must_use]
    pub fn to_record(&self, turn_id: &str) -> harw_session_store::InjectionRecord {
        harw_session_store::InjectionRecord {
            skill: self.skill.clone(),
            section: self.section.clone(),
            trigger_kind: self.kind.as_str().to_owned(),
            trigger_key: self.key.clone(),
            bytes: u64::try_from(self.bytes).unwrap_or(u64::MAX),
            turn_id: turn_id.to_owned(),
            trust: trust_label(self.trust).to_owned(),
            sha256: self.sha256.clone(),
            text: Some(self.text.clone()),
        }
    }
}

/// [`TriggerKind`] of the stable name a record stores
/// ([`TriggerKind::as_str`]).
fn trigger_kind_of(name: &str) -> Option<TriggerKind> {
    [
        TriggerKind::ErrorCode,
        TriggerKind::Command,
        TriggerKind::Tool,
        TriggerKind::Path,
        TriggerKind::Keyword,
    ]
    .into_iter()
    .find(|kind| kind.as_str() == name)
}

fn trust_label(trust: TrustClass) -> &'static str {
    match trust {
        TrustClass::Instruction => "instruction",
        TrustClass::Evidence => "evidence",
        TrustClass::Data => "data",
    }
}

/// Result of [`SkillInjector::prepare_round_with`].
#[derive(Debug, Clone, Default)]
pub struct RoundInjection {
    /// Every active excerpt of the session, in delivery order (this round's
    /// new ones last), together at most one per-round budget. Empty when
    /// nothing is active.
    pub fragments: Vec<Fragment>,
    /// Excerpts selected for the first time in this round. They count as
    /// delivered once the model answered a request carrying them
    /// ([`SkillInjector::mark_answered`]); their `injection` event follows
    /// ([`SkillInjector::confirm_recorded`]).
    pub delivered: Vec<DeliveredSkill>,
    /// Hits of this round that were not selected, with the reason.
    pub dropped: Vec<Dropped>,
    /// Imported skills withheld because `inject_imported` is off.
    pub suppressed: Vec<String>,
    /// Hits of skills skipped because this session registers none of the
    /// tools they declare (see [`ToolAvailability`]); once per skill and
    /// round, also for a skill skipped in an earlier round.
    pub unavailable: Vec<String>,
    /// Labels the admit predicate rejected (new or active excerpts).
    pub withheld: Vec<String>,
    /// Labels of older excerpts that made room for newer ones.
    pub evicted: Vec<String>,
}

/// The tool names of one session for the availability filter of the
/// injector (PL-97 B2, see the module documentation).
///
/// # Description
/// `registered` are the tool names the session registers; `known` every tool
/// name a session of this binary can register (typically the capability
/// catalog; the registered names are added). A skill is admitted when its
/// manifest declares no known tool name, or at least one of them is
/// registered. Declared names outside `known` (abstract capabilities such as
/// `filesystem.read`) say nothing about availability. Descriptive only: an
/// admitted skill gains no tool and no right.
///
/// # Examples
/// ```rust
/// use harw_core::skill_injection::ToolAvailability;
///
/// let tools = ToolAvailability::new(["shell.exec", "fs.read"], ["sys.ps", "sys.free"]);
/// assert!(!tools.admits(&["sys.ps".to_owned(), "sys.free".to_owned()]));
/// assert!(tools.admits(&["sys.ps".to_owned(), "fs.read".to_owned()]));
/// assert!(tools.admits(&["filesystem.read".to_owned()]));
/// assert!(tools.admits(&[]));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolAvailability {
    registered: BTreeSet<String>,
    known: BTreeSet<String>,
}

impl ToolAvailability {
    /// The availability of a session registering `registered`, out of the
    /// tool names `known` to this binary.
    #[must_use]
    pub fn new<R, S, K, T>(registered: R, known: K) -> Self
    where
        R: IntoIterator<Item = S>,
        S: Into<String>,
        K: IntoIterator<Item = T>,
        T: Into<String>,
    {
        let registered: BTreeSet<String> = registered.into_iter().map(Into::into).collect();
        let mut known: BTreeSet<String> = known.into_iter().map(Into::into).collect();
        known.extend(registered.iter().cloned());
        Self { registered, known }
    }

    /// Whether a skill declaring `tools` fits the session: it declares no
    /// known tool name, or the session registers at least one it declares.
    #[must_use]
    pub fn admits(&self, tools: &[String]) -> bool {
        let mut declared = tools
            .iter()
            .filter(|tool| self.known.contains(tool.as_str()))
            .peekable();
        if declared.peek().is_none() {
            return true;
        }
        declared.any(|tool| self.registered.contains(tool.as_str()))
    }
}

/// Per-session triggered skill injector (see the module documentation).
#[derive(Debug)]
pub struct SkillInjector {
    index: Arc<SkillIndex>,
    settings: SkillInjectionSettings,
    imported: BTreeSet<String>,
    /// The availability filter; `None` admits every triggered skill.
    tools: Option<ToolAvailability>,
    state: Mutex<InjectorState>,
}

/// An excerpt in the context of the session.
#[derive(Debug, Clone)]
struct ActiveExcerpt {
    fragment: Fragment,
    delivery: DeliveredSkill,
    /// Set by [`SkillInjector::mark_answered`]: a request carrying it was
    /// answered by the model, so its bytes stay charged when it leaves the
    /// context. Whether its `injection` event is durable is tracked
    /// separately ([`InjectorState::unrecorded`]).
    answered: bool,
}

#[derive(Debug, Default)]
struct InjectorState {
    injection: InjectionState,
    /// Bytes `select` booked for excerpts that never reached the model; they
    /// widen the session budget again (see [`Self::session_bytes`]).
    released_bytes: usize,
    active: Vec<ActiveExcerpt>,
    /// Answered deliveries whose `injection` event is not durable yet, in
    /// answer order. Independent of `active`: an excerpt that leaves the
    /// context keeps its pending record (the model did see it).
    unrecorded: Vec<PendingRecord>,
    seen_tools: BTreeSet<String>,
    /// Skills skipped for their tools since the last compaction; they stay
    /// marked in `injection`, so a later hit reaches `select` as "already
    /// delivered" and is reported as unavailable instead.
    unavailable: BTreeSet<String>,
    cursor: Option<HistoryCursor>,
}

impl InjectorState {
    fn reset(&mut self) {
        self.injection.reset_after_compaction();
        self.released_bytes = 0;
        self.active.clear();
        if !self.unrecorded.is_empty() {
            // Appended after the compaction, the record would count as a
            // delivery of the new context epoch (restore would bring the
            // excerpt back): the pending records are dropped instead.
            tracing::warn!(
                count = self.unrecorded.len(),
                "skill_injection.unrecorded_dropped_on_compaction"
            );
            self.unrecorded.clear();
        }
        self.seen_tools.clear();
        self.unavailable.clear();
        self.cursor = None;
    }

    /// Bytes delivered since the session start or the last compaction.
    fn session_bytes(&self) -> usize {
        self.injection
            .session_bytes()
            .saturating_sub(self.released_bytes)
    }

    fn active_bytes(&self) -> usize {
        self.active
            .iter()
            .map(|excerpt| excerpt.delivery.bytes)
            .sum()
    }

    fn is_active(&self, label: &str) -> bool {
        self.active
            .iter()
            .any(|excerpt| excerpt.delivery.label == label)
    }

    fn release(&mut self, bytes: usize) {
        self.released_bytes = self.released_bytes.saturating_add(bytes);
    }

    /// The excerpt left the context: its skill may be selected again, and
    /// the bytes of an excerpt that never reached the model (not answered)
    /// are released. A pending record of an answered excerpt is kept.
    /// (`forget_skill` also reopens sibling sections that are still active;
    /// [`SkillInjector::prepare_round_with`] skips such a re-selection.)
    fn drop_excerpt(&mut self, excerpt: &ActiveExcerpt) {
        self.injection.forget_skill(&excerpt.delivery.skill);
        if !excerpt.answered {
            self.release(excerpt.delivery.bytes);
        }
    }
}

/// Per-round bookkeeping of the selection passes
/// ([`SkillInjector::prepare_round_with`]).
#[derive(Debug, Default)]
struct RoundScratch {
    /// Skills to reopen once every pass of the round is done (withheld, or
    /// no room left): reopened earlier, a repeated pass would select them
    /// again.
    forget: Vec<String>,
    /// Drops the injector decides itself, reported after those of `select`.
    dropped: Vec<Dropped>,
}

/// How far the injector has read the history: its length and the id of the
/// last item read. A shorter history or another id at that position means
/// the history was rewritten (compaction, truncation).
#[derive(Debug, Clone, PartialEq, Eq)]
struct HistoryCursor {
    len: usize,
    last: Option<ItemId>,
}

impl HistoryCursor {
    fn at_end(items: &[TurnItem]) -> Self {
        Self {
            len: items.len(),
            last: items.last().map(|item| item_id(item).clone()),
        }
    }

    fn still_valid(&self, items: &[TurnItem]) -> bool {
        match self.len.checked_sub(1) {
            None => true,
            Some(last_index) => items.get(last_index).map(item_id) == self.last.as_ref(),
        }
    }
}

/// The fragment label of an excerpt (as [`Injection::into_fragment`] forms
/// it).
fn excerpt_label(skill: &str, section: Option<&str>) -> String {
    match section {
        Some(section) => format!("skill:{skill}:{section}"),
        None => format!("skill:{skill}"),
    }
}

fn item_id(item: &TurnItem) -> &ItemId {
    match item {
        TurnItem::UserMessage(item) => &item.id,
        TurnItem::AssistantMessage(item) => &item.id,
        TurnItem::ToolCall(item) => &item.id,
        TurnItem::ToolResult(item) => &item.id,
        TurnItem::Reasoning(item) => &item.id,
        TurnItem::Error(item) => &item.id,
    }
}

impl SkillInjector {
    /// An injector over the assembly's skill index (its trigger index is
    /// built once with the index) with a fresh session state.
    #[must_use]
    pub fn new(index: Arc<SkillIndex>, settings: SkillInjectionSettings) -> Self {
        Self {
            index,
            settings,
            imported: BTreeSet::new(),
            tools: None,
            state: Mutex::new(InjectorState::default()),
        }
    }

    /// Enables the tool-availability filter (PL-97 B2): a triggered skill
    /// whose manifest declares only tools `tools` does not register is
    /// skipped ([`RoundInjection::unavailable`]).
    #[must_use]
    pub fn with_tool_availability(mut self, tools: ToolAvailability) -> Self {
        self.tools = Some(tools);
        self
    }

    /// Whether `skill` fits the session's tools ([`ToolAvailability::admits`]
    /// over its declared tools: the manifest's `tools` plus its `[triggers]
    /// tools`); always `true` without the filter or for a name the index
    /// does not know.
    #[must_use]
    pub fn tools_admit(&self, skill: &str) -> bool {
        match (&self.tools, self.index.get(skill)) {
            (Some(tools), Some(entry)) => {
                let declared: Vec<String> = entry
                    .tools
                    .iter()
                    .chain(&entry.triggers.tools)
                    .cloned()
                    .collect();
                tools.admits(&declared)
            }
            _ => true,
        }
    }

    /// Marks additional skills as imported (trust class `Evidence`; delivered
    /// only with `inject_imported`). In production the provenance comes from
    /// the index: a skill whose directory carries
    /// [`harw_catalog::IMPORTED_MARKER_FILE`] is [`SkillSource::Imported`]
    /// (PL-97 H8); this set only adds names on top of that.
    #[must_use]
    pub fn with_imported_skills<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.imported.extend(names.into_iter().map(Into::into));
        self
    }

    /// The settings of this injector.
    #[must_use]
    pub fn settings(&self) -> &SkillInjectionSettings {
        &self.settings
    }

    /// The skill index this injector selects from.
    #[must_use]
    pub fn index(&self) -> &Arc<SkillIndex> {
        &self.index
    }

    /// Provenance of `skill`: imported (marked by the index as
    /// [`SkillSource::Imported`], PL-97 H8, or via
    /// [`Self::with_imported_skills`]), bundled or from a trusted layer.
    ///
    /// A name the index does not know is treated as imported (trust class
    /// `Evidence`): the only trust a skill can carry is the trust of the
    /// layer it was loaded from, and an unknown name has none. `select` only
    /// yields indexed skills, so this arm is a fail-closed default, not a
    /// path taken in normal operation.
    #[must_use]
    pub fn provenance(&self, skill: &str) -> SkillProvenance {
        if self.imported.contains(skill) {
            return SkillProvenance::Imported;
        }
        match self.index.get(skill).map(|entry| &entry.source) {
            Some(SkillSource::Bundled) => SkillProvenance::Bundled,
            Some(SkillSource::Layer(_)) => SkillProvenance::TrustedLayer,
            Some(SkillSource::Imported(_)) | None => SkillProvenance::Imported,
        }
    }

    /// Bytes delivered since the session start or the last compaction
    /// (released selections excluded).
    #[must_use]
    pub fn session_bytes(&self) -> usize {
        self.lock().session_bytes()
    }

    /// Number of active excerpts.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.lock().active.len()
    }

    /// Bytes of the active excerpts (what every request carries).
    #[must_use]
    pub fn active_bytes(&self) -> usize {
        self.lock().active_bytes()
    }

    /// Drops every active excerpt and the dedupe state: after a compaction
    /// every skill may be delivered again and the session budget starts
    /// over. The next round re-reads the history from its latest user
    /// message.
    ///
    /// Pending records ([`Self::pending_deliveries`]) are dropped as well
    /// (logged as a warning): appended after the compaction, they would count
    /// as deliveries of the new context epoch on a resume.
    pub fn reset_after_compaction(&self) {
        self.lock().reset();
    }

    /// Rebuilds the state of a resumed session (same `SessionId`, new
    /// process or assembly) from its injection records (PL-97 H7).
    ///
    /// # Description
    /// Deterministic reconstruction: `records` are the deliveries of the
    /// current context epoch in append order
    /// ([`crate::state_store::StateStore::load_active_skill_injections`]).
    /// Every record charges its bytes against the session budget and marks
    /// its skill section as delivered, exactly as the original selection did.
    /// A record whose excerpt is known ([`harw_session_store::InjectionRecord::text`])
    /// is active again, unchanged and answered (it reached the model once, so
    /// it is never recorded twice); a later delivery of the same label
    /// replaces the earlier one. A record without text (older binaries), of a
    /// skill the index no longer knows, whose tools the session lacks, or an
    /// imported skill while `inject_imported` is off, counts as an excerpt
    /// that left the context: its bytes stay charged and the skill may be
    /// delivered again. Rounds after the restore apply the usual rules
    /// (section share, admit predicate, request montage) to the restored
    /// excerpts; the next round reads the history from its latest user
    /// message.
    ///
    /// A no-op (returns `0`) when injection is off, `records` is empty (an
    /// old store: fresh state) or this injector already has session state.
    ///
    /// # Returns
    /// The number of active excerpts after the restore.
    pub fn restore(&self, records: &[harw_session_store::InjectionRecord]) -> usize {
        if !self.settings.enabled || records.is_empty() {
            return 0;
        }
        let mut state = self.lock();
        if state.cursor.is_some() || !state.active.is_empty() || state.injection.session_bytes() > 0
        {
            tracing::debug!("skill_injection.restore_skipped_live_state");
            return 0;
        }
        let produced_at = jiff::Timestamp::now();
        for record in records {
            let bytes = usize::try_from(record.bytes).unwrap_or(usize::MAX);
            state
                .injection
                .restore_delivered(&record.skill, record.section.as_deref(), bytes);
            let label = excerpt_label(&record.skill, record.section.as_deref());
            state
                .active
                .retain(|excerpt| excerpt.delivery.label != label);
            match self.restored_excerpt(record, produced_at) {
                Some(excerpt) => state.active.push(excerpt),
                None => state.injection.forget_skill(&record.skill),
            }
        }
        tracing::debug!(
            records = records.len(),
            active = state.active.len(),
            session_bytes = state.session_bytes(),
            "skill_injection.restored"
        );
        state.active.len()
    }

    /// The active excerpt of a restored record, or `None` when it cannot or
    /// must not be re-established (see [`Self::restore`]).
    fn restored_excerpt(
        &self,
        record: &harw_session_store::InjectionRecord,
        produced_at: jiff::Timestamp,
    ) -> Option<ActiveExcerpt> {
        let text = record.text.as_deref()?;
        let kind = trigger_kind_of(&record.trigger_kind)?;
        // Bindung an den aktuellen Index: ein seither geänderter Skill
        // (neue Bundle-Version, editierte Trusted-Layer-Datei) gilt als
        // Auszug, der den Kontext verlassen hat — die aktuelle Fassung darf
        // neu geliefert werden. Ebenso muss der Text den Kopf genau dieser
        // Fassung tragen, sonst wird Plattentext nie zur Instruktion.
        let entry = self.index.get(&record.skill)?;
        if entry.snapshot().sha256 != record.sha256
            || !text.starts_with(&format!(
                "# Skill: {} (sha256 {})",
                record.skill, record.sha256
            ))
        {
            tracing::debug!(skill = %record.skill, "skill_injection.restore_stale_excerpt");
            return None;
        }
        let text = text.to_owned();
        if !self.tools_admit(&record.skill) {
            return None;
        }
        let provenance = self.provenance(&record.skill);
        if provenance == SkillProvenance::Imported && !self.settings.inject_imported {
            return None;
        }
        let trust = provenance.trust();
        let injection = Injection {
            skill: record.skill.clone(),
            section: record.section.clone(),
            bytes: text.len(),
            text,
            reason: TriggerReason {
                kind,
                key: record.trigger_key.clone(),
            },
            sha256: record.sha256.clone(),
        };
        let mut delivery = DeliveredSkill::of(&injection, String::new(), trust);
        let mut fragment = match injection.into_fragment(produced_at) {
            Ok(fragment) => fragment,
            Err(error) => {
                tracing::warn!(skill = %record.skill, %error, "skill_injection.restore_rejected");
                return None;
            }
        };
        fragment.trust = trust;
        delivery.label = fragment.label.as_str().to_owned();
        Some(ActiveExcerpt {
            fragment,
            delivery,
            answered: true,
        })
    }

    /// [`Self::prepare_round_with`] without an admit predicate.
    #[must_use]
    pub fn prepare_round(
        &self,
        history: &ConversationHistory,
        window_tokens: u64,
    ) -> RoundInjection {
        self.prepare_round_with(history, window_tokens, |_| true)
    }

    /// Feeds the history items since the previous round into the selection
    /// and returns the active excerpts for this round's request.
    ///
    /// # Arguments
    /// - `history`: the session history as it will be sent.
    /// - `window_tokens`: the active model window (`0` = unknown).
    /// - `admit`: whether a fragment may reach the request at all (e.g. the
    ///   session's activation of its label). A rejected selection is
    ///   released (no dedupe, no session bytes); a rejected active excerpt
    ///   leaves the context.
    #[must_use]
    pub fn prepare_round_with(
        &self,
        history: &ConversationHistory,
        window_tokens: u64,
        admit: impl Fn(&Fragment) -> bool,
    ) -> RoundInjection {
        let mut round = RoundInjection::default();
        if !self.settings.enabled {
            return round;
        }
        let mut state = self.lock();
        let items = history.items();
        let resume_at = state
            .cursor
            .as_ref()
            .map(|cursor| cursor.still_valid(items).then_some(cursor.len));
        let start = match resume_at {
            Some(Some(len)) => len,
            Some(None) => {
                tracing::debug!("skill_injection.history_rewritten");
                state.reset();
                anchor(&mut state.seen_tools, items)
            }
            None => anchor(&mut state.seen_tools, items),
        };
        state.cursor = Some(HistoryCursor::at_end(items));

        // Active excerpts the predicate no longer admits leave the context.
        let (kept, rejected): (Vec<_>, Vec<_>) = std::mem::take(&mut state.active)
            .into_iter()
            .partition(|excerpt| admit(&excerpt.fragment));
        state.active = kept;
        for excerpt in &rejected {
            state.drop_excerpt(excerpt);
            round.withheld.push(excerpt.delivery.label.clone());
        }

        let new_items = items.get(start..).unwrap_or_default();
        let events = collect_events(new_items, &mut state.seen_tools);
        if !events.is_empty() {
            self.select_into(&mut state, &events, window_tokens, &admit, &mut round);
        }

        // The whole section stays within one per-round budget: the oldest
        // excerpts make room for newer ones.
        let section_cap = self.settings.per_turn_bytes(window_tokens);
        while state.active_bytes() > section_cap && !state.active.is_empty() {
            let oldest = state.active.remove(0);
            state.drop_excerpt(&oldest);
            round.evicted.push(oldest.delivery.label);
        }
        round
            .delivered
            .retain(|delivered| state.is_active(&delivered.label));
        round.fragments = state
            .active
            .iter()
            .map(|excerpt| excerpt.fragment.clone())
            .collect();
        round
    }

    fn select_into(
        &self,
        state: &mut InjectorState,
        events: &[OwnedEvent],
        window_tokens: u64,
        admit: &impl Fn(&Fragment) -> bool,
        round: &mut RoundInjection,
    ) {
        let borrowed: Vec<TriggerEvent<'_>> = events.iter().map(OwnedEvent::as_event).collect();
        let produced_at = jiff::Timestamp::now();
        // Platz der Runde: das Pro-Runden-Budget abzüglich jeder Auswahl
        // außer den mangels Werkzeug übersprungenen Skills.
        let mut room = self.settings.per_turn_bytes(window_tokens);
        // Skills mit einem Ergebnis in dieser Runde (in irgendeinem Durchlauf).
        let mut decided: BTreeSet<String> = BTreeSet::new();
        let mut scratch = RoundScratch::default();
        let mut select_drops: Vec<Dropped> = Vec::new();
        // A pass after the first runs only when the previous one skipped a
        // skill not decided before; `select` never returns a skipped skill
        // again (it stays marked), so the loop ends after at most one pass
        // per skill of the index plus one. The bound is that count, never a
        // fixed number that could stop before the last available hit.
        let max_passes = self.index.len().saturating_add(1);
        for _ in 0..max_passes {
            let mut budget = self.settings.budget(window_tokens);
            budget.per_session_bytes = budget
                .per_session_bytes
                .saturating_add(state.released_bytes);
            let selection = self.index.select(&borrowed, &mut state.injection, &budget);
            select_drops = selection.dropped;
            let mut skipped_for_tools = false;
            for injection in selection.injections {
                if !decided.insert(injection.skill.clone()) {
                    // Schon entschieden und seither wieder geöffnet: das
                    // Ergebnis des früheren Durchlaufs bleibt.
                    state.release(injection.bytes);
                    scratch.forget.push(injection.skill);
                    continue;
                }
                if !self.tools_admit(&injection.skill) {
                    // The session has none of the tools the skill explains:
                    // stays marked (no re-selection until the next
                    // compaction), costs no bytes and takes no room.
                    tracing::debug!(skill = %injection.skill, "skill_injection.tools_unavailable");
                    state.release(injection.bytes);
                    state.unavailable.insert(injection.skill.clone());
                    round.unavailable.push(injection.skill);
                    skipped_for_tools = true;
                    continue;
                }
                if injection.bytes > room {
                    // Only in a repeated pass: `select` judged the hit
                    // against the whole per-round budget, the round has less
                    // left (exactly the check of a single pass).
                    state.release(injection.bytes);
                    scratch.forget.push(injection.skill.clone());
                    scratch.dropped.push(Dropped {
                        skill: injection.skill,
                        section: injection.section,
                        reason: DropReason::TurnBudget,
                    });
                    continue;
                }
                room -= injection.bytes;
                self.deliver(state, injection, produced_at, admit, round, &mut scratch);
            }
            let dropped_for_budget = select_drops.iter().any(|dropped| {
                matches!(
                    dropped.reason,
                    DropReason::TurnBudget | DropReason::SessionBudget
                )
            });
            // Done once a pass skips no further skill for its tools (no room
            // freed) or nothing waits for room.
            if !(skipped_for_tools && dropped_for_budget) {
                break;
            }
        }
        for skill in &scratch.forget {
            state.injection.forget_skill(skill);
        }
        // Die Verwürfe des letzten Durchlaufs beschreiben jeden Treffer, den
        // er nicht ausgewählt hat. „Schon geliefert“ eines in dieser Runde
        // entschiedenen Skills ist ein Artefakt der Wiederholung; das eines
        // früher mangels Werkzeug übersprungenen Skills heißt „nicht
        // verfügbar“.
        for dropped in select_drops {
            if dropped.reason == DropReason::AlreadyDelivered {
                if decided.contains(&dropped.skill) {
                    continue;
                }
                if state.unavailable.contains(&dropped.skill) {
                    if !round.unavailable.contains(&dropped.skill) {
                        round.unavailable.push(dropped.skill);
                    }
                    continue;
                }
            }
            round.dropped.push(dropped);
        }
        round.dropped.append(&mut scratch.dropped);
    }

    /// The path of one selected injection that fits the round: imported
    /// skills withheld without `inject_imported`, framing, the "still
    /// active" check, the admit predicate, delivery. Skills to reopen after
    /// the round go to `scratch.forget`, drops the injector decides itself
    /// to `scratch.dropped`.
    fn deliver(
        &self,
        state: &mut InjectorState,
        injection: Injection,
        produced_at: jiff::Timestamp,
        admit: &impl Fn(&Fragment) -> bool,
        round: &mut RoundInjection,
        scratch: &mut RoundScratch,
    ) {
        let provenance = self.provenance(&injection.skill);
        if provenance == SkillProvenance::Imported && !self.settings.inject_imported {
            // Stays marked (no re-selection until the next compaction), but
            // costs no session bytes.
            state.release(injection.bytes);
            round.suppressed.push(injection.skill);
            return;
        }
        let trust = provenance.trust();
        let mut delivery = DeliveredSkill::of(&injection, String::new(), trust);
        let mut fragment = match injection.into_fragment(produced_at) {
            Ok(fragment) => fragment,
            Err(error) => {
                tracing::warn!(
                    skill = %delivery.skill,
                    %error,
                    "skill_injection.fragment_rejected"
                );
                state.release(delivery.bytes);
                return;
            }
        };
        fragment.trust = trust;
        delivery.label = fragment.label.as_str().to_owned();
        if state.is_active(&delivery.label) {
            // Reopened by `forget_skill` of a sibling section: still in the
            // context, nothing new to deliver.
            state.release(delivery.bytes);
            scratch.dropped.push(Dropped {
                skill: delivery.skill,
                section: delivery.section,
                reason: DropReason::AlreadyDelivered,
            });
            return;
        }
        if !admit(&fragment) {
            state.release(delivery.bytes);
            scratch.forget.push(delivery.skill);
            round.withheld.push(delivery.label);
            return;
        }
        round.delivered.push(delivery.clone());
        state.active.push(ActiveExcerpt {
            fragment,
            delivery,
            answered: false,
        });
    }

    /// Keeps only the active excerpts whose label the built request
    /// actually carries (`included`); every other one leaves the context
    /// (program `exclude`, ceiling, budget). Returns the labels removed.
    pub fn reconcile(&self, included: &[String]) -> Vec<String> {
        let mut state = self.lock();
        let (kept, omitted): (Vec<_>, Vec<_>) = std::mem::take(&mut state.active)
            .into_iter()
            .partition(|excerpt| included.contains(&excerpt.delivery.label));
        state.active = kept;
        omitted
            .into_iter()
            .map(|excerpt| {
                state.drop_excerpt(&excerpt);
                excerpt.delivery.label
            })
            .collect()
    }

    /// The model answered a request carrying the active excerpts in turn
    /// `turn_id`: every one not answered before becomes a delivery. Its bytes
    /// stay charged from now on, and it is queued for its `injection` event
    /// ([`Self::pending_deliveries`]) with `turn_id` as its provenance.
    pub fn mark_answered(&self, turn_id: &str) {
        let mut state = self.lock();
        let InjectorState {
            active, unrecorded, ..
        } = &mut *state;
        for excerpt in active.iter_mut().filter(|excerpt| !excerpt.answered) {
            excerpt.answered = true;
            unrecorded.push(PendingRecord {
                delivery: excerpt.delivery.clone(),
                turn_id: turn_id.to_owned(),
            });
        }
    }

    /// Every answered delivery whose `injection` event is not recorded yet,
    /// in answer order, without changing any state.
    ///
    /// # Description
    /// The caller appends one event per returned record
    /// ([`PendingRecord::to_record`]) and reports each successful append
    /// with [`Self::confirm_recorded`]. A record whose append failed stays
    /// pending, also when its excerpt leaves the context (eviction, admit
    /// predicate, [`Self::reconcile`]), and is returned again; a recorded one
    /// is never returned again. Only a compaction or a history rewrite drops
    /// pending records ([`Self::reset_after_compaction`]). A retried record
    /// keeps the turn of its first answer ([`PendingRecord::turn_id`]).
    #[must_use]
    pub fn pending_deliveries(&self) -> Vec<PendingRecord> {
        self.lock().unrecorded.clone()
    }

    /// The `injection` event of `pending` is durable: removes it from the
    /// pending records.
    ///
    /// # Returns
    /// `true` when the record was pending; `false` when it was confirmed
    /// already or dropped by a compaction in the meantime.
    pub fn confirm_recorded(&self, pending: &PendingRecord) -> bool {
        let mut state = self.lock();
        match state.unrecorded.iter().position(|open| open == pending) {
            Some(position) => {
                state.unrecorded.remove(position);
                true
            }
            None => false,
        }
    }

    /// Test shorthand: marks the active excerpts answered and confirms every
    /// pending record at once, as if each append had succeeded.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn confirm_delivered(&self) -> Vec<DeliveredSkill> {
        self.mark_answered("test-turn");
        let pending = self.pending_deliveries();
        for record in &pending {
            self.confirm_recorded(record);
        }
        pending.into_iter().map(|record| record.delivery).collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, InjectorState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Previews the first round of a fresh session for `input`
    /// (`harw skill preview`, `/skills preview`, PL-97 B3) — without a model
    /// call and without touching this injector's session state.
    ///
    /// # Description
    /// The selection path of [`Self::prepare_round_with`] without an admit
    /// predicate: the budget from [`SkillInjectionSettings::budget`],
    /// [`harw_catalog::select`] over this injector's index (and its
    /// `TriggerIndex`), provenance, trust and the fragment framing. A fresh
    /// round never exceeds the section share (the selection is bounded by the
    /// same per-round budget), so nothing is evicted. A session whose
    /// `[skills] triggered_injection` is off delivers nothing; the preview
    /// selects anyway and reports the switch in [`SkillPreview::enabled`].
    ///
    /// # Arguments
    /// - `input`: the trigger inputs (see [`SkillPreviewInput`]).
    /// - `window_tokens`: the model window (`0` = unknown, the per-round cap
    ///   applies).
    #[must_use]
    pub fn preview(&self, input: &SkillPreviewInput, window_tokens: u64) -> SkillPreview {
        let mut state = InjectorState::default();
        let mut round = RoundInjection::default();
        let events = input.events();
        if !events.is_empty() {
            self.select_into(
                &mut state,
                &events,
                window_tokens,
                &|_: &Fragment| true,
                &mut round,
            );
        }
        round.fragments = state
            .active
            .iter()
            .map(|excerpt| excerpt.fragment.clone())
            .collect();
        SkillPreview {
            enabled: self.settings.enabled,
            budget: self.settings.budget(window_tokens),
            round,
        }
    }
}

/// Trigger inputs of a selection preview ([`SkillInjector::preview`]): what
/// the event feed of the turn loop would read from the history of a fresh
/// session. Each field becomes the [`TriggerEvent`] the feed derives from the
/// matching history item (see the module documentation); blank entries are
/// ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillPreviewInput {
    /// Text of a user message → [`TriggerEvent::TaskText`] (keywords, error
    /// codes).
    pub task: String,
    /// Tools called for the first time → [`TriggerEvent::ToolFirstUse`].
    pub tools: Vec<String>,
    /// Text of tool results → [`TriggerEvent::ToolOutput`] (error codes,
    /// e.g. `E0505`).
    pub tool_output: Vec<String>,
    /// Path arguments of `fs.*`/`doc.*` calls → [`TriggerEvent::PathTouched`].
    pub paths: Vec<String>,
    /// Command lines of `shell.*`/`process.*`/`job.start` calls →
    /// [`TriggerEvent::Command`].
    pub commands: Vec<String>,
}

impl SkillPreviewInput {
    /// Whether no entry yields a trigger event (blank task text, every list
    /// empty or blank).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events().is_empty()
    }

    /// The events of these inputs, framed like the history feed
    /// ([`collect_events`]): tool output is scanned through [`result_text`]'s
    /// head-and-tail window, a tool counts once.
    fn events(&self) -> Vec<OwnedEvent> {
        let mut events = Vec::new();
        if !self.task.trim().is_empty() {
            events.push(OwnedEvent::Task(self.task.clone()));
        }
        let mut seen_tools = BTreeSet::new();
        for tool in &self.tools {
            let tool = tool.trim();
            if !tool.is_empty() && seen_tools.insert(tool) {
                events.push(OwnedEvent::FirstUse(tool.to_owned()));
            }
        }
        for text in &self.tool_output {
            if !text.trim().is_empty() {
                events.push(OwnedEvent::Output {
                    tool: String::new(),
                    text: head_and_tail(&[text.as_str()]),
                });
            }
        }
        for path in &self.paths {
            if !path.trim().is_empty() {
                events.push(OwnedEvent::Path(path.clone()));
            }
        }
        for command in &self.commands {
            if !command.trim().is_empty() {
                events.push(OwnedEvent::Command(command.clone()));
            }
        }
        events
    }
}

/// Result of [`SkillInjector::preview`].
#[derive(Debug, Clone, Default)]
pub struct SkillPreview {
    /// `[skills] triggered_injection`. `false`: a session delivers none of
    /// this (the preview selects anyway).
    pub enabled: bool,
    /// The budget of the round, from the settings and the model window.
    pub budget: InjectionBudget,
    /// The round: [`RoundInjection::delivered`] is what the first round of a
    /// fresh session delivers (in rank order), [`RoundInjection::dropped`]
    /// the hits not selected with their typed reason,
    /// [`RoundInjection::suppressed`] the imported skills withheld, and
    /// [`RoundInjection::fragments`] the framed excerpts.
    pub round: RoundInjection,
}

/// Start of the first read: the latest user message (or the start). Tools
/// called before it count as already used.
fn anchor(seen_tools: &mut BTreeSet<String>, items: &[TurnItem]) -> usize {
    let start = items
        .iter()
        .rposition(|item| matches!(item, TurnItem::UserMessage(_)))
        .unwrap_or(0);
    for item in items.get(..start).unwrap_or_default() {
        if let TurnItem::ToolCall(call) = item {
            seen_tools.insert(call.tool_name.trim().to_owned());
        }
    }
    start
}

/// A [`TriggerEvent`] with owned text.
#[derive(Debug, Clone, PartialEq, Eq)]
enum OwnedEvent {
    Task(String),
    Output { tool: String, text: String },
    FirstUse(String),
    Path(String),
    Command(String),
}

impl OwnedEvent {
    fn as_event(&self) -> TriggerEvent<'_> {
        match self {
            Self::Task(text) => TriggerEvent::TaskText(text),
            Self::Output { tool, text } => TriggerEvent::ToolOutput { tool, text },
            Self::FirstUse(tool) => TriggerEvent::ToolFirstUse(tool),
            Self::Path(path) => TriggerEvent::PathTouched(path),
            Self::Command(command) => TriggerEvent::Command(command),
        }
    }
}

fn collect_events(items: &[TurnItem], seen_tools: &mut BTreeSet<String>) -> Vec<OwnedEvent> {
    let mut events = Vec::new();
    let mut tool_of_call: BTreeMap<&str, &str> = BTreeMap::new();
    for item in items {
        match item {
            TurnItem::UserMessage(message) => {
                let text = message
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        ContentPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.trim().is_empty() {
                    events.push(OwnedEvent::Task(text));
                }
            }
            TurnItem::ToolCall(call) => {
                let tool = call.tool_name.trim();
                tool_of_call.insert(call.call_id.as_str(), tool);
                if seen_tools.insert(tool.to_owned()) {
                    events.push(OwnedEvent::FirstUse(tool.to_owned()));
                }
                if is_path_tool(tool) {
                    for path in string_arguments(&call.arguments, PATH_KEYS) {
                        events.push(OwnedEvent::Path(path));
                    }
                }
                if is_command_tool(tool) {
                    for command in command_arguments(&call.arguments) {
                        events.push(OwnedEvent::Command(command));
                    }
                }
            }
            TurnItem::ToolResult(result) => {
                let text = result_text(&result.result);
                if !text.trim().is_empty() {
                    let tool = tool_of_call
                        .get(result.call_id.as_str())
                        .copied()
                        .unwrap_or_default()
                        .to_owned();
                    events.push(OwnedEvent::Output { tool, text });
                }
            }
            TurnItem::AssistantMessage(_) | TurnItem::Reasoning(_) | TurnItem::Error(_) => {}
        }
    }
    events
}

fn is_path_tool(tool: &str) -> bool {
    tool.starts_with("fs.") || tool.starts_with("doc.")
}

fn is_command_tool(tool: &str) -> bool {
    tool.starts_with("shell.") || tool.starts_with("process.") || tool == "job.start"
}

/// String values (or string array elements) of `keys` in an argument object.
fn string_arguments(arguments: &serde_json::Value, keys: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let Some(object) = arguments.as_object() else {
        return out;
    };
    for key in keys {
        match object.get(*key) {
            Some(serde_json::Value::String(value)) if !value.trim().is_empty() => {
                out.push(value.clone());
            }
            Some(serde_json::Value::Array(values)) => out.extend(
                values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .map(str::to_owned),
            ),
            _ => {}
        }
    }
    out
}

/// The command line of a runner call: `command`/`cmd` as given, `argv`
/// joined with spaces.
fn command_arguments(arguments: &serde_json::Value) -> Vec<String> {
    let Some(object) = arguments.as_object() else {
        return Vec::new();
    };
    COMMAND_KEYS
        .iter()
        .filter_map(|key| match object.get(*key) {
            Some(serde_json::Value::String(command)) => Some(command.clone()),
            Some(serde_json::Value::Array(argv)) => Some(
                argv.iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            _ => None,
        })
        .filter(|command| !command.trim().is_empty())
        .collect()
}

/// The scanned text of a tool result: the error message, or every string
/// leaf of the success value (newline-separated). Above
/// [`MAX_RESULT_SCAN_BYTES`] only the first and the last half are kept —
/// compiler and test errors usually come at the end of a long output — and
/// the middle is never copied.
fn result_text(result: &ToolCallResult) -> String {
    match result {
        ToolCallResult::Error { message } => head_and_tail(&[message.as_str()]),
        ToolCallResult::Success { value } => scan_value(value).0,
    }
}

/// Upper bound of the JSON nodes one traversal of a tool result visits
/// ([`scan_value`] runs at most two: forward for the head, backward for the
/// tail). An untrusted result with millions of tiny leaves costs at most
/// this many steps per direction.
const MAX_RESULT_SCAN_NODES: usize = 65_536;

/// Upper bound of the container nesting [`scan_value`] descends into;
/// deeper arrays and objects are skipped (their leaves are not scanned).
const MAX_RESULT_SCAN_DEPTH: usize = 128;

/// Work counters of one [`scan_value`] call (the regression tests assert
/// them instead of wall time).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ScanStats {
    /// JSON nodes visited over both traversals.
    visited: usize,
    /// Deepest traversal stack (open containers) reached.
    max_depth: usize,
    /// Containers skipped because they sat below [`MAX_RESULT_SCAN_DEPTH`].
    depth_skipped: usize,
    /// The forward traversal ran out of [`MAX_RESULT_SCAN_NODES`] and the
    /// tail came from a separate backward traversal.
    exhausted: bool,
}

/// Traversal order of [`walk_leaves`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Forward,
    Backward,
}

/// The children of an open container on the traversal stack.
enum Children<'a> {
    Array(std::slice::Iter<'a, serde_json::Value>),
    Object(serde_json::map::Values<'a>),
}

impl<'a> Children<'a> {
    fn next(&mut self, direction: Direction) -> Option<&'a serde_json::Value> {
        match (self, direction) {
            (Self::Array(values), Direction::Forward) => values.next(),
            (Self::Array(values), Direction::Backward) => values.next_back(),
            (Self::Object(values), Direction::Forward) => values.next(),
            (Self::Object(values), Direction::Backward) => values.next_back(),
        }
    }
}

/// How a traversal of [`walk_leaves`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WalkEnd {
    /// Every node (above the depth limit) was visited.
    Complete,
    /// `on_leaf` asked to stop.
    Stopped,
    /// [`MAX_RESULT_SCAN_NODES`] nodes were visited before the end.
    Exhausted,
}

/// Iterative traversal of the string leaves of `root` in document order
/// (or its reverse), bounded by [`MAX_RESULT_SCAN_NODES`] visited nodes and
/// [`MAX_RESULT_SCAN_DEPTH`] open containers. `stats` accumulates the work.
fn walk_leaves<'a>(
    root: &'a serde_json::Value,
    direction: Direction,
    stats: &mut ScanStats,
    mut on_leaf: impl FnMut(&'a str) -> std::ops::ControlFlow<()>,
) -> WalkEnd {
    let mut stack: Vec<Children<'a>> = Vec::new();
    let mut next = Some(root);
    let mut visited = 0usize;
    loop {
        let value = match next.take() {
            Some(value) => value,
            None => match stack.last_mut() {
                None => return WalkEnd::Complete,
                Some(children) => match children.next(direction) {
                    Some(value) => value,
                    None => {
                        stack.pop();
                        continue;
                    }
                },
            },
        };
        if visited >= MAX_RESULT_SCAN_NODES {
            return WalkEnd::Exhausted;
        }
        visited += 1;
        stats.visited = stats.visited.saturating_add(1);
        let children = match value {
            serde_json::Value::String(text) => {
                if on_leaf(text).is_break() {
                    return WalkEnd::Stopped;
                }
                None
            }
            serde_json::Value::Array(values) => Some(Children::Array(values.iter())),
            serde_json::Value::Object(object) => Some(Children::Object(object.values())),
            serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
                None
            }
        };
        if let Some(children) = children {
            if stack.len() < MAX_RESULT_SCAN_DEPTH {
                stack.push(children);
                stats.max_depth = stats.max_depth.max(stack.len());
            } else {
                stats.depth_skipped = stats.depth_skipped.saturating_add(1);
            }
        }
    }
}

/// The scan text of a success value ([`result_text`]) with its work
/// counters.
///
/// The string leaves stream through a bounded [`ScanWindow`] during one
/// forward traversal; nothing proportional to the number of leaves is
/// collected. When that traversal visits the whole value, the text equals
/// [`head_and_tail`] over all leaves. When it runs out of nodes, the head is
/// what it saw and the tail comes from a backward traversal with its own node
/// budget (the middle is skipped; on values just above the budget the two
/// ends may share leaves).
fn scan_value(value: &serde_json::Value) -> (String, ScanStats) {
    let mut stats = ScanStats::default();
    let mut window = ScanWindow::new();
    let end = walk_leaves(value, Direction::Forward, &mut stats, |leaf| {
        window.push(leaf);
        std::ops::ControlFlow::Continue(())
    });
    if end != WalkEnd::Exhausted {
        return (window.finish(), stats);
    }
    stats.exhausted = true;
    let mut tail = TailWindow::new();
    walk_leaves(value, Direction::Backward, &mut stats, |leaf| {
        tail.push(leaf)
    });
    tracing::debug!(
        visited = stats.visited,
        "skill_injection.result_scan_node_limit"
    );
    (window.finish_with_tail(tail.finish()), stats)
}

/// Bytes of the head half of a truncated scan text.
const HEAD_MAX: usize = MAX_RESULT_SCAN_BYTES / 2;

/// Bytes of the tail half of a truncated scan text (one byte goes to the
/// `\n` between the halves).
const TAIL_MAX: usize = MAX_RESULT_SCAN_BYTES - HEAD_MAX - 1;

/// Streaming form of [`head_and_tail`]: parts arrive one by one and the
/// memory stays bounded — the prefix of the joined text up to
/// [`MAX_RESULT_SCAN_BYTES`] and the shortest suffix of parts that covers
/// [`TAIL_MAX`] bytes (each retained part costs at least its separator).
struct ScanWindow<'a> {
    /// Prefix of the joined text, at most [`MAX_RESULT_SCAN_BYTES`] bytes.
    head: String,
    /// A part did not fit into `head` any more.
    head_full: bool,
    /// Length of the whole joined text.
    total: usize,
    /// Number of parts pushed.
    parts: usize,
    /// The last parts, as few as cover [`TAIL_MAX`] bytes.
    tail: std::collections::VecDeque<&'a str>,
    /// Joined length of `tail`.
    tail_len: usize,
}

impl<'a> ScanWindow<'a> {
    fn new() -> Self {
        Self {
            head: String::new(),
            head_full: false,
            total: 0,
            parts: 0,
            tail: std::collections::VecDeque::new(),
            tail_len: 0,
        }
    }

    fn push(&mut self, part: &'a str) {
        if self.parts > 0 {
            self.total = self.total.saturating_add(1);
            if !self.head_full {
                if self.head.len() < MAX_RESULT_SCAN_BYTES {
                    self.head.push('\n');
                } else {
                    self.head_full = true;
                }
            }
        }
        self.parts = self.parts.saturating_add(1);
        self.total = self.total.saturating_add(part.len());
        if !self.head_full {
            let room = MAX_RESULT_SCAN_BYTES.saturating_sub(self.head.len());
            if part.len() <= room {
                self.head.push_str(part);
            } else {
                self.head.push_str(
                    part.get(..floor_char_boundary(part, room))
                        .unwrap_or_default(),
                );
                self.head_full = true;
            }
        }

        if !self.tail.is_empty() {
            self.tail_len = self.tail_len.saturating_add(1);
        }
        self.tail.push_back(part);
        self.tail_len = self.tail_len.saturating_add(part.len());
        // Drop the oldest part while the rest alone fills the tail.
        while self.tail.len() > 1 {
            let Some(front) = self.tail.front() else {
                break;
            };
            let rest = self.tail_len.saturating_sub(front.len()).saturating_sub(1);
            if rest < TAIL_MAX {
                break;
            }
            self.tail.pop_front();
            self.tail_len = rest;
        }
    }

    /// The joined text, or its head and tail above
    /// [`MAX_RESULT_SCAN_BYTES`].
    fn finish(self) -> String {
        if self.total <= MAX_RESULT_SCAN_BYTES {
            return self.head;
        }
        let mut tail = TailWindow::new();
        for part in self.tail.iter().rev() {
            if tail.push(part).is_break() {
                break;
            }
        }
        let tail = tail.finish();
        self.finish_with_tail(tail)
    }

    /// The head of the pushed parts, `\n` and `tail`.
    fn finish_with_tail(self, tail: String) -> String {
        let mut out = self.head;
        out.truncate(floor_char_boundary(&out, HEAD_MAX));
        out.push('\n');
        out.push_str(&tail);
        out
    }
}

/// The tail half of a truncated scan text, fed with parts from the end
/// backwards: at most [`TAIL_MAX`] bytes of the joined text, the first part
/// cut at a char boundary.
struct TailWindow<'a> {
    /// The parts in reverse order.
    parts: Vec<&'a str>,
    /// Joined length of `parts`.
    len: usize,
}

impl<'a> TailWindow<'a> {
    fn new() -> Self {
        Self {
            parts: Vec::new(),
            len: 0,
        }
    }

    /// Adds the part before the ones pushed so far; `Break` once the window
    /// is full.
    fn push(&mut self, part: &'a str) -> std::ops::ControlFlow<()> {
        let separator = usize::from(!self.parts.is_empty());
        let room = TAIL_MAX.saturating_sub(self.len.saturating_add(separator));
        if room == 0 {
            return std::ops::ControlFlow::Break(());
        }
        if part.len() <= room {
            self.parts.push(part);
            self.len = self.len.saturating_add(part.len() + separator);
            std::ops::ControlFlow::Continue(())
        } else {
            let start = ceil_char_boundary(part, part.len() - room);
            self.parts.push(part.get(start..).unwrap_or_default());
            self.len = TAIL_MAX;
            std::ops::ControlFlow::Break(())
        }
    }

    fn finish(mut self) -> String {
        self.parts.reverse();
        self.parts.join("\n")
    }
}

/// `parts` joined with `\n`; above [`MAX_RESULT_SCAN_BYTES`] the first half
/// and the last half of that joined text (separated by `\n`), at most
/// [`MAX_RESULT_SCAN_BYTES`] bytes in all.
fn head_and_tail(parts: &[&str]) -> String {
    let mut window = ScanWindow::new();
    for part in parts {
        window.push(part);
    }
    window.finish()
}

fn floor_char_boundary(text: &str, max: usize) -> usize {
    let mut cut = max.min(text.len());
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
}

fn ceil_char_boundary(text: &str, min: usize) -> usize {
    let mut cut = min.min(text.len());
    while cut < text.len() && !text.is_char_boundary(cut) {
        cut += 1;
    }
    cut
}

#[cfg(test)]
mod tests;

/// Parity of [`SkillInjector::preview`] with the first round of the turn
/// loop (PL-97 B3).
#[cfg(test)]
mod preview_tests {
    use std::sync::Arc;

    use harw_catalog::{DropReason, SkillIndex, TriggerKind};
    use harw_protocol::ToolCallResult;
    use harw_types::ToolCallId;

    use super::{SkillInjectionSettings, SkillInjector, SkillPreviewInput};
    use crate::history::ConversationHistory;
    use crate::test_support::{TestError, TestResult};

    const BORROW_BODY: &str = "# Borrow\n\nIntro.\n\n## When to use\n\nWhenever the borrow checker complains.\n\n## Cause E0505\n\nThe value is still borrowed.\n\n## Fix\n\nOwn the value first.\n";
    const BORROW_TRIGGERS: &str = "[triggers]\nerror_codes = [\"E0505\"]\npaths = [\"**/*.rs\"]\ncommands = [\"cargo test\"]\npriority = 70\n";
    const LINT_BODY: &str = "# Lint\n\n## Clippy\n\nRun clippy with -D warnings and fix every lint before the commit.\n";
    const LINT_TRIGGERS: &str =
        "[triggers]\nkeywords = [\"clippy\"]\ntools = [\"shell.exec\"]\npriority = 40\n";

    /// An index over an in-memory bundle: `(name, triggers.toml, body)`.
    fn index_of(skills: &[(&str, &str, &str)]) -> Arc<SkillIndex> {
        let mut files: Vec<(String, String)> = Vec::new();
        for (name, triggers, body) in skills {
            files.push((
                format!("skills/{name}/skill.toml"),
                format!("name = \"{name}\"\ndescription = \"Use when {name} matters.\"\n"),
            ));
            files.push((format!("skills/{name}/instructions.md"), (*body).to_owned()));
            files.push((
                format!("skills/{name}/triggers.toml"),
                (*triggers).to_owned(),
            ));
        }
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(path, content)| (path.as_str(), content.as_str()))
            .collect();
        Arc::new(SkillIndex::build_with_bundle(&[], &refs))
    }

    fn two_skills() -> Arc<SkillIndex> {
        index_of(&[
            ("borrow", BORROW_TRIGGERS, BORROW_BODY),
            ("lint", LINT_TRIGGERS, LINT_BODY),
        ])
    }

    /// The history the preview inputs stand for: a user message, a `fs.read`
    /// of `src/lib.rs`, a `shell.exec` of `cargo test` failing with E0505.
    fn history() -> ConversationHistory {
        let mut history = ConversationHistory::new();
        history.push_user_text("cargo test fails, clippy too");
        history.push_tool_call(
            ToolCallId::from_str("c1"),
            "fs.read",
            serde_json::json!({ "path": "src/lib.rs" }),
        );
        history.push_tool_result(
            ToolCallId::from_str("c1"),
            ToolCallResult::success(serde_json::json!("fn main() {}")),
            1,
        );
        history.push_tool_call(
            ToolCallId::from_str("c2"),
            "shell.exec",
            serde_json::json!({ "command": "cargo test" }),
        );
        history.push_tool_result(
            ToolCallId::from_str("c2"),
            ToolCallResult::error("error[E0505]: cannot move out of `v`"),
            1,
        );
        history
    }

    fn input() -> SkillPreviewInput {
        SkillPreviewInput {
            task: "cargo test fails, clippy too".to_owned(),
            tools: vec!["fs.read".to_owned(), "shell.exec".to_owned()],
            tool_output: vec![
                "fn main() {}".to_owned(),
                "error[E0505]: cannot move out of `v`".to_owned(),
            ],
            paths: vec!["src/lib.rs".to_owned()],
            commands: vec!["cargo test".to_owned()],
        }
    }

    fn assert_same_round(settings: SkillInjectionSettings, window_tokens: u64) -> TestResult {
        let index = two_skills();
        let turn = SkillInjector::new(Arc::clone(&index), settings)
            .prepare_round(&history(), window_tokens);
        let preview = SkillInjector::new(index, settings).preview(&input(), window_tokens);
        assert_eq!(preview.round.delivered, turn.delivered);
        assert_eq!(preview.round.dropped, turn.dropped);
        assert_eq!(preview.round.suppressed, turn.suppressed);
        let bodies = |fragments: &[harw_context::Fragment]| -> Vec<(String, String)> {
            fragments
                .iter()
                .map(|fragment| (fragment.label.as_str().to_owned(), fragment.body.clone()))
                .collect()
        };
        assert_eq!(bodies(&preview.round.fragments), bodies(&turn.fragments));
        assert_eq!(preview.budget, settings.budget(window_tokens));
        Ok(())
    }

    #[test]
    fn preview_delivers_what_the_first_round_of_the_injector_delivers() -> TestResult {
        assert_same_round(SkillInjectionSettings::default(), 0)?;
        let preview = SkillInjector::new(two_skills(), SkillInjectionSettings::default())
            .preview(&input(), 0);
        let kinds: Vec<(&str, TriggerKind, &str)> = preview
            .round
            .delivered
            .iter()
            .map(|delivered| {
                (
                    delivered.skill.as_str(),
                    delivered.kind,
                    delivered.key.as_str(),
                )
            })
            .collect();
        // Rank order: priority 70 before 40; per skill the first reason
        // (error code before command, path and keyword; tool before keyword).
        assert_eq!(
            kinds,
            vec![
                ("borrow", TriggerKind::ErrorCode, "E0505"),
                ("lint", TriggerKind::Tool, "shell.exec"),
            ]
        );
        assert!(preview.enabled);
        Ok(())
    }

    #[test]
    fn preview_reports_the_same_budget_drops_as_the_injector() -> TestResult {
        // Room for one excerpt only: the lower-priority skill is dropped.
        let settings = SkillInjectionSettings {
            per_turn_cap_bytes: 260,
            ..SkillInjectionSettings::default()
        };
        assert_same_round(settings, 0)?;
        let preview = SkillInjector::new(two_skills(), settings).preview(&input(), 0);
        let dropped = preview
            .round
            .dropped
            .first()
            .ok_or(TestError::Missing("dropped hit"))?;
        assert_eq!(dropped.skill, "lint");
        assert_eq!(dropped.reason, DropReason::TurnBudget);
        Ok(())
    }

    /// The preview takes the trust class from the injector's own provenance,
    /// like the turn loop (anything not known as bundled or from a trusted
    /// layer is imported, PR #156): an imported skill is withheld without
    /// `inject_imported` and delivered as evidence with it — in the preview
    /// exactly as in the first round.
    #[test]
    fn preview_follows_the_injectors_provenance_for_imported_skills() -> TestResult {
        for inject_imported in [false, true] {
            let settings = SkillInjectionSettings {
                inject_imported,
                ..SkillInjectionSettings::default()
            };
            let injector =
                || SkillInjector::new(two_skills(), settings).with_imported_skills(["lint"]);
            let turn = injector().prepare_round(&history(), 0);
            let preview = injector().preview(&input(), 0);
            assert_eq!(preview.round.delivered, turn.delivered);
            assert_eq!(preview.round.dropped, turn.dropped);
            assert_eq!(preview.round.suppressed, turn.suppressed);
            let lint = preview
                .round
                .delivered
                .iter()
                .find(|delivered| delivered.skill == "lint");
            if inject_imported {
                let lint = lint.ok_or(TestError::Missing("imported skill delivered"))?;
                assert_eq!(lint.trust, harw_context::TrustClass::Evidence);
            } else {
                assert!(lint.is_none());
                assert_eq!(preview.round.suppressed, vec!["lint".to_owned()]);
            }
            let borrow = preview
                .round
                .delivered
                .iter()
                .find(|delivered| delivered.skill == "borrow")
                .ok_or(TestError::Missing("bundled skill delivered"))?;
            assert_eq!(borrow.trust, harw_context::TrustClass::Instruction);
        }
        Ok(())
    }

    #[test]
    fn preview_selects_even_when_injection_is_off() {
        let settings = SkillInjectionSettings {
            enabled: false,
            ..SkillInjectionSettings::default()
        };
        let turn = SkillInjector::new(two_skills(), settings).prepare_round(&history(), 0);
        assert!(turn.delivered.is_empty());
        let preview = SkillInjector::new(two_skills(), settings).preview(&input(), 0);
        assert!(!preview.enabled);
        assert_eq!(preview.round.delivered.len(), 2);
    }

    #[test]
    fn preview_leaves_the_session_state_untouched() {
        let injector = SkillInjector::new(two_skills(), SkillInjectionSettings::default());
        let preview = injector.preview(&input(), 0);
        assert!(!preview.round.delivered.is_empty());
        assert_eq!(injector.session_bytes(), 0);
        assert_eq!(injector.active_count(), 0);
        // The session still delivers on its first round.
        assert_eq!(
            injector.prepare_round(&history(), 0).delivered,
            preview.round.delivered
        );
    }

    #[test]
    fn blank_preview_input_is_empty() {
        assert!(SkillPreviewInput::default().is_empty());
        let blank = SkillPreviewInput {
            task: "  ".to_owned(),
            tools: vec![String::new()],
            ..SkillPreviewInput::default()
        };
        assert!(blank.is_empty());
        let task = SkillPreviewInput {
            task: "x".to_owned(),
            ..SkillPreviewInput::default()
        };
        assert!(!task.is_empty());
    }
}

/// The tool-availability filter (PL-97 B2): a triggered skill whose declared
/// tools the session does not register is not injected.
#[cfg(test)]
mod availability_tests {
    use std::sync::Arc;

    use harw_catalog::{DropReason, SkillIndex};
    use harw_protocol::ToolCallResult;
    use harw_types::ToolCallId;

    use super::{SkillInjectionSettings, SkillInjector, ToolAvailability};
    use crate::history::ConversationHistory;
    use crate::test_support::{TestError, TestResult};

    /// The bundled skills (`sys-tools` with its `triggers.toml` included).
    fn bundled() -> Arc<SkillIndex> {
        Arc::new(SkillIndex::build(&[]))
    }

    /// A session that runs `ps aux` through `shell.exec`.
    fn ps_aux_history() -> ConversationHistory {
        let mut history = ConversationHistory::new();
        history.push_user_text("which processes are running?");
        history.push_tool_call(
            ToolCallId::from_str("c1"),
            "shell.exec",
            serde_json::json!({ "command": "ps aux" }),
        );
        history.push_tool_result(
            ToolCallId::from_str("c1"),
            ToolCallResult::success(serde_json::json!("PID TTY TIME CMD")),
            1,
        );
        history
    }

    /// Every tool name the bundled `sys-tools` manifest declares.
    fn sys_tools(index: &SkillIndex) -> TestResult<Vec<String>> {
        let entry = index
            .get("sys-tools")
            .ok_or(TestError::Missing("sys-tools bundled"))?;
        Ok(entry.tools.clone())
    }

    fn delivered_skills(injector: &SkillInjector) -> Vec<String> {
        injector
            .prepare_round(&ps_aux_history(), 0)
            .delivered
            .into_iter()
            .map(|delivered| delivered.skill)
            .collect()
    }

    #[test]
    fn a_session_without_sys_tools_running_ps_aux_gets_no_sys_tools_injection() -> TestResult {
        let index = bundled();
        let sys = sys_tools(&index)?;
        // Without the filter (as before) `ps aux` pulls in `sys-tools`.
        let unfiltered = SkillInjector::new(Arc::clone(&index), SkillInjectionSettings::default());
        assert!(
            delivered_skills(&unfiltered).contains(&"sys-tools".to_owned()),
            "control: the command trigger fires without the filter"
        );

        // A session with `shell.exec` but without a single `sys.*` tool.
        let without_sys = SkillInjector::new(Arc::clone(&index), SkillInjectionSettings::default())
            .with_tool_availability(ToolAvailability::new(
                ["shell.exec", "fs.read"],
                sys.iter().cloned(),
            ));
        let mut history = ps_aux_history();
        let round = without_sys.prepare_round(&history, 0);
        assert!(
            !round
                .delivered
                .iter()
                .any(|delivered| delivered.skill == "sys-tools"),
            "{:?}",
            round.delivered
        );
        assert_eq!(round.unavailable, vec!["sys-tools".to_owned()]);
        assert!(
            !round
                .fragments
                .iter()
                .any(|fragment| fragment.label.as_str().starts_with("skill:sys-tools")),
            "no fragment reaches the request"
        );
        assert_eq!(without_sys.session_bytes(), 0, "costs no session bytes");
        // Stays marked until the next compaction: a second `ps` call does not
        // select it again.
        history.push_tool_call(
            ToolCallId::from_str("c2"),
            "shell.exec",
            serde_json::json!({ "command": "ps -ef" }),
        );
        history.push_tool_result(
            ToolCallId::from_str("c2"),
            ToolCallResult::success(serde_json::json!("PID")),
            1,
        );
        let again = without_sys.prepare_round(&history, 0);
        // A later hit is reported as unavailable again, never as "already
        // delivered" (it never was).
        assert_eq!(again.unavailable, vec!["sys-tools".to_owned()]);
        assert!(
            !again
                .dropped
                .iter()
                .any(|dropped| dropped.skill == "sys-tools"),
            "{:?}",
            again.dropped
        );
        assert!(
            !again
                .delivered
                .iter()
                .any(|delivered| delivered.skill == "sys-tools")
        );
        assert_eq!(without_sys.session_bytes(), 0);

        // With one registered `sys.*` tool the skill is delivered again.
        let with_sys =
            SkillInjector::new(index, SkillInjectionSettings::default()).with_tool_availability(
                ToolAvailability::new(["shell.exec", "sys.ps"], sys.iter().cloned()),
            );
        assert!(delivered_skills(&with_sys).contains(&"sys-tools".to_owned()));
        Ok(())
    }

    #[test]
    fn availability_ignores_declared_names_that_are_no_tool_names() {
        let tools = ToolAvailability::new(["shell.exec"], ["sys.ps"]);
        // Abstract capability names of the bundled manifests never filter.
        assert!(tools.admits(&["filesystem.read".to_owned()]));
        assert!(tools.admits(&[]));
        assert!(!tools.admits(&["sys.ps".to_owned(), "filesystem.read".to_owned()]));
        // Registered names always count as known.
        assert!(tools.admits(&["shell.exec".to_owned()]));
        // Without the filter, and for unknown skills: admitted.
        let plain = SkillInjector::new(
            Arc::new(SkillIndex::build(&[])),
            SkillInjectionSettings::default(),
        );
        assert!(plain.tools_admit("sys-tools"));
        let filtered = plain.with_tool_availability(tools);
        assert!(!filtered.tools_admit("sys-tools"));
        assert!(filtered.tools_admit("no-such-skill"));
    }

    /// Every tool a bundled skill's `triggers.toml` fires on admits that
    /// skill on its own: a session whose only matching tool is
    /// `cargo.test_one` (listed in `cargo-tools/triggers.toml`, not in its
    /// manifest) gets `cargo-tools`.
    #[test]
    fn every_trigger_tool_of_a_bundled_skill_admits_it() -> TestResult {
        let index = bundled();
        let known: Vec<String> = index
            .entries()
            .flat_map(|entry| entry.tools.iter().chain(&entry.triggers.tools))
            .cloned()
            .collect();
        let mut checked = 0;
        for entry in index.entries() {
            for tool in &entry.triggers.tools {
                let injector =
                    SkillInjector::new(Arc::clone(&index), SkillInjectionSettings::default())
                        .with_tool_availability(ToolAvailability::new(
                            [tool.as_str()],
                            known.iter().cloned(),
                        ));
                assert!(
                    injector.tools_admit(&entry.name),
                    "{} must be admitted by its trigger tool {tool}",
                    entry.name
                );
                checked += 1;
            }
        }
        assert!(checked > 30, "only {checked} trigger tools checked");
        let cargo = SkillInjector::new(index, SkillInjectionSettings::default())
            .with_tool_availability(ToolAvailability::new(["cargo.test_one"], known));
        assert!(cargo.tools_admit("cargo-tools"));
        assert!(!cargo.tools_admit("sys-tools"));
        Ok(())
    }

    /// Bundle files of a fixture skill: `(name, skill.toml tools,
    /// triggers.toml, body)`.
    fn fixture_files(
        name: &str,
        tools: &[&str],
        triggers: &str,
        body: &str,
    ) -> Vec<(String, String)> {
        let tools = tools
            .iter()
            .map(|tool| format!("\"{tool}\""))
            .collect::<Vec<_>>()
            .join(", ");
        vec![
            (
                format!("skills/{name}/skill.toml"),
                format!(
                    "name = \"{name}\"\ndescription = \"Use when {name} matters.\"\ntools = [{tools}]\n"
                ),
            ),
            (format!("skills/{name}/instructions.md"), body.to_owned()),
            (format!("skills/{name}/triggers.toml"), triggers.to_owned()),
        ]
    }

    fn index_of(files: &[(String, String)], with_bundled: bool) -> Arc<SkillIndex> {
        let mut refs: Vec<(&str, &str)> = if with_bundled {
            harw_catalog::bundled_skill_files()
        } else {
            Vec::new()
        };
        refs.extend(
            files
                .iter()
                .map(|(path, content)| (path.as_str(), content.as_str())),
        );
        Arc::new(SkillIndex::build_with_bundle(&[], &refs))
    }

    const LOW_BODY: &str = "# Low\n\nRun the low-priority check first.\n";

    /// The bytes the `low` fixture skill takes when it is delivered alone.
    fn low_bytes(index: &Arc<SkillIndex>, history: &ConversationHistory) -> TestResult<usize> {
        let alone = SkillInjector::new(Arc::clone(index), SkillInjectionSettings::default());
        let round = alone.prepare_round(history, 0);
        round
            .delivered
            .iter()
            .find(|delivered| delivered.skill == "low")
            .map(|delivered| delivered.bytes)
            .ok_or(TestError::Missing("low delivered with the full budget"))
    }

    /// The per-round budget is charged only for skills that reach the
    /// request: an unavailable high-priority skill and an available
    /// lower-priority one, with room for one of them, deliver the available
    /// one.
    #[test]
    fn an_unavailable_skill_leaves_its_room_to_an_available_one() -> TestResult {
        let high_body = format!("# High\n\n{}", "Use sys.ps for processes.\n".repeat(40));
        let mut files = fixture_files(
            "high",
            &["sys.ps"],
            "[triggers]\nkeywords = [\"zebrafoo\"]\npriority = 90\n",
            &high_body,
        );
        files.extend(fixture_files(
            "low",
            &[],
            "[triggers]\nkeywords = [\"zebrafoo\"]\npriority = 10\n",
            LOW_BODY,
        ));
        let index = index_of(&files, false);
        let mut history = ConversationHistory::new();
        history.push_user_text("zebrafoo");
        let low = low_bytes(&index, &history)?;
        let settings = SkillInjectionSettings {
            per_turn_cap_bytes: low + 40,
            ..SkillInjectionSettings::default()
        };

        // Control: without the filter the high-priority skill takes the room.
        let unfiltered =
            SkillInjector::new(Arc::clone(&index), settings).prepare_round(&history, 0);
        let delivered: Vec<&str> = unfiltered
            .delivered
            .iter()
            .map(|delivered| delivered.skill.as_str())
            .collect();
        assert_eq!(delivered, ["high"]);
        assert!(
            unfiltered
                .dropped
                .iter()
                .any(|dropped| dropped.skill == "low" && dropped.reason == DropReason::TurnBudget),
            "{:?}",
            unfiltered.dropped
        );

        // Without `sys.ps` the high-priority skill is skipped and the
        // low-priority one gets the room.
        let filtered = SkillInjector::new(index, settings)
            .with_tool_availability(ToolAvailability::new(["shell.exec"], ["sys.ps"]));
        let round = filtered.prepare_round(&history, 0);
        let delivered: Vec<&str> = round
            .delivered
            .iter()
            .map(|delivered| delivered.skill.as_str())
            .collect();
        assert_eq!(delivered, ["low"]);
        assert_eq!(round.unavailable, vec!["high".to_owned()]);
        assert!(round.dropped.is_empty(), "{:?}", round.dropped);
        assert_eq!(filtered.session_bytes(), low);
        Ok(())
    }

    /// PR #154 (P2): the reselection does not stop after a fixed number of
    /// skipped skills. Four higher-priority unavailable skills whose
    /// excerpts each fill the per-round budget, ahead of one available
    /// lower-priority skill: each pass releases one unavailable skill, and a
    /// fifth pass still delivers the available one instead of reporting it
    /// as `TurnBudget`.
    #[test]
    fn four_budget_filling_unavailable_skills_leave_their_room_to_an_available_one() -> TestResult {
        let unavailable = ["u1", "u2", "u3", "u4"];
        let mut files = Vec::new();
        for (name, priority) in unavailable.iter().zip([90, 80, 70, 60]) {
            let body = format!("# {name}\n\n{}", "Use sys.ps for processes.\n".repeat(40));
            files.extend(fixture_files(
                name,
                &["sys.ps"],
                &format!("[triggers]\nkeywords = [\"zebrafoo\"]\npriority = {priority}\n"),
                &body,
            ));
        }
        files.extend(fixture_files(
            "low",
            &[],
            "[triggers]\nkeywords = [\"zebrafoo\"]\npriority = 10\n",
            LOW_BODY,
        ));
        let index = index_of(&files, false);
        let mut history = ConversationHistory::new();
        history.push_user_text("zebrafoo");
        let low = low_bytes(&index, &history)?;
        let settings = SkillInjectionSettings {
            per_turn_cap_bytes: low + 40,
            ..SkillInjectionSettings::default()
        };

        // Control: without the filter the first unavailable skill fills the
        // round and every other hit, `low` included, is dropped for the
        // budget.
        let unfiltered =
            SkillInjector::new(Arc::clone(&index), settings).prepare_round(&history, 0);
        let delivered: Vec<&str> = unfiltered
            .delivered
            .iter()
            .map(|delivered| delivered.skill.as_str())
            .collect();
        assert_eq!(delivered, ["u1"]);
        for name in ["u2", "u3", "u4", "low"] {
            assert!(
                unfiltered.dropped.iter().any(
                    |dropped| dropped.skill == name && dropped.reason == DropReason::TurnBudget
                ),
                "{name}: {:?}",
                unfiltered.dropped
            );
        }

        let filtered = SkillInjector::new(index, settings)
            .with_tool_availability(ToolAvailability::new(["shell.exec"], ["sys.ps"]));
        let round = filtered.prepare_round(&history, 0);
        let delivered: Vec<&str> = round
            .delivered
            .iter()
            .map(|delivered| delivered.skill.as_str())
            .collect();
        assert_eq!(delivered, ["low"], "{:?}", round.dropped);
        assert_eq!(round.unavailable, unavailable.map(str::to_owned));
        assert!(round.dropped.is_empty(), "{:?}", round.dropped);
        assert_eq!(filtered.session_bytes(), low);
        Ok(())
    }

    /// The item-6 scenario under a tight per-round budget: `ps aux` (the
    /// bundled `sys-tools`, skipped without `sys.*`) and a keyword skill in
    /// the same round — the keyword skill is delivered, not dropped.
    #[test]
    fn ps_aux_and_a_keyword_in_one_round_deliver_the_keyword_skill() -> TestResult {
        let files = fixture_files(
            "low",
            &[],
            "[triggers]\nkeywords = [\"zebrafoo\"]\npriority = 10\n",
            LOW_BODY,
        );
        let index = index_of(&files, true);
        let mut keyword_only = ConversationHistory::new();
        keyword_only.push_user_text("zebrafoo");
        let low = low_bytes(&index, &keyword_only)?;
        let settings = SkillInjectionSettings {
            per_turn_cap_bytes: low + 40,
            ..SkillInjectionSettings::default()
        };
        let mut history = ConversationHistory::new();
        history.push_user_text("zebrafoo");
        history.push_tool_call(
            ToolCallId::from_str("c1"),
            "shell.exec",
            serde_json::json!({ "command": "ps aux" }),
        );
        history.push_tool_result(
            ToolCallId::from_str("c1"),
            ToolCallResult::success(serde_json::json!("PID TTY TIME CMD")),
            1,
        );

        // Control: `sys-tools` (higher priority) takes the room, the keyword
        // skill is dropped for the budget.
        let unfiltered =
            SkillInjector::new(Arc::clone(&index), settings).prepare_round(&history, 0);
        assert!(
            unfiltered
                .delivered
                .iter()
                .any(|delivered| delivered.skill == "sys-tools"),
            "{:?}",
            unfiltered.delivered
        );
        assert!(
            unfiltered
                .dropped
                .iter()
                .any(|dropped| dropped.skill == "low" && dropped.reason == DropReason::TurnBudget),
            "{:?}",
            unfiltered.dropped
        );

        let sys = sys_tools(&index)?;
        let without_sys = SkillInjector::new(index, settings).with_tool_availability(
            ToolAvailability::new(["shell.exec", "fs.read"], sys.iter().cloned()),
        );
        let round = without_sys.prepare_round(&history, 0);
        let delivered: Vec<&str> = round
            .delivered
            .iter()
            .map(|delivered| delivered.skill.as_str())
            .collect();
        assert_eq!(delivered, ["low"], "{:?}", round.dropped);
        assert_eq!(round.unavailable, vec!["sys-tools".to_owned()]);
        assert!(
            !round.dropped.iter().any(|dropped| dropped.skill == "low"),
            "{:?}",
            round.dropped
        );
        Ok(())
    }
}

/// Bounded traversal of tool results (PR #154 review: the scan limit holds
/// while traversing, not only after collecting).
#[cfg(test)]
mod scan_limit_tests {
    use super::{
        MAX_RESULT_SCAN_BYTES, MAX_RESULT_SCAN_DEPTH, MAX_RESULT_SCAN_NODES, ScanWindow,
        head_and_tail, scan_value,
    };
    use crate::test_support::TestResult;

    /// The collect-then-cut reference of the former implementation.
    fn reference(parts: &[&str]) -> String {
        let joined = parts.join("\n");
        if joined.len() <= MAX_RESULT_SCAN_BYTES {
            return joined;
        }
        let head_max = MAX_RESULT_SCAN_BYTES / 2;
        let tail_max = MAX_RESULT_SCAN_BYTES - head_max - 1;
        let head = super::floor_char_boundary(&joined, head_max);
        let mut tail: Vec<&str> = Vec::new();
        let mut tail_len = 0usize;
        for part in parts.iter().rev() {
            let separator = usize::from(!tail.is_empty());
            let room = tail_max.saturating_sub(tail_len + separator);
            if room == 0 {
                break;
            }
            if part.len() <= room {
                tail.push(part);
                tail_len += part.len() + separator;
            } else {
                let start = super::ceil_char_boundary(part, part.len() - room);
                tail.push(part.get(start..).unwrap_or_default());
                break;
            }
        }
        tail.reverse();
        format!(
            "{}\n{}",
            joined.get(..head).unwrap_or_default(),
            tail.join("\n")
        )
    }

    #[test]
    fn streaming_window_matches_the_collect_then_cut_reference() {
        let big = "x".repeat(MAX_RESULT_SCAN_BYTES);
        let umlauts = "ä".repeat(MAX_RESULT_SCAN_BYTES / 3);
        let half = "y".repeat(MAX_RESULT_SCAN_BYTES / 2);
        let cases: Vec<Vec<&str>> = vec![
            vec![],
            vec![""],
            vec!["a", "b"],
            vec!["", "", ""],
            vec![big.as_str()],
            vec!["HEAD", big.as_str(), "TAIL"],
            vec![umlauts.as_str(), "mid", umlauts.as_str()],
            vec![half.as_str(), half.as_str()],
            vec![half.as_str(), "", half.as_str(), ""],
            vec![half.as_str(), "z", half.as_str()],
        ];
        for parts in cases {
            assert_eq!(head_and_tail(&parts), reference(&parts));
        }
        let many: Vec<String> = (0..200_000).map(|n| format!("{n}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(head_and_tail(&many), reference(&many));
    }

    #[test]
    fn small_values_scan_every_leaf_in_document_order() {
        let value = serde_json::json!({
            "exit_code": 101,
            "stderr": "error[E0505]",
            "stdout": ["a", {"nested": "b"}, null, true],
        });
        let (text, stats) = scan_value(&value);
        assert!(!stats.exhausted);
        assert_eq!(text, "error[E0505]\na\nb");
    }

    #[test]
    fn millions_of_one_byte_leaves_cost_bounded_work() -> TestResult {
        const LEAVES: usize = 2_000_000;
        let mut values = Vec::with_capacity(LEAVES);
        values.push(serde_json::Value::String("H".to_owned()));
        values.extend((2..LEAVES).map(|_| serde_json::Value::String("a".to_owned())));
        values.push(serde_json::Value::String("T".to_owned()));
        let value = serde_json::Value::Array(values);

        let (text, stats) = scan_value(&value);
        assert!(stats.exhausted);
        // Root plus at most the node budget per direction.
        assert!(stats.visited <= 2 * MAX_RESULT_SCAN_NODES);
        assert!(stats.max_depth <= 1);
        assert!(text.len() <= MAX_RESULT_SCAN_BYTES);
        assert!(text.starts_with("H\na"));
        assert!(text.ends_with("a\nT"));

        // The streaming window keeps a bounded tail, whatever it is fed.
        let mut window = ScanWindow::new();
        for _ in 0..LEAVES {
            window.push("a");
        }
        assert!(window.tail.len() <= MAX_RESULT_SCAN_BYTES / 2 + 1);
        assert!(window.head.len() <= MAX_RESULT_SCAN_BYTES);
        Ok(())
    }

    /// Takes a deeply nested array apart iteratively (the derived `Drop`
    /// of `serde_json::Value` recurses).
    fn dismantle(mut value: serde_json::Value) {
        while let serde_json::Value::Array(values) = &mut value {
            let Some(inner) = values.pop() else {
                break;
            };
            value = inner;
        }
    }

    #[test]
    fn deeply_nested_values_stop_at_the_depth_limit() -> TestResult {
        const DEPTH: usize = 100_000;
        let mut value = serde_json::Value::String("E0505 deep".to_owned());
        for _ in 0..DEPTH {
            value = serde_json::Value::Array(vec![value]);
        }
        let (text, stats) = scan_value(&value);
        assert!(!stats.exhausted);
        assert_eq!(stats.max_depth, MAX_RESULT_SCAN_DEPTH);
        assert_eq!(stats.depth_skipped, 1);
        assert_eq!(stats.visited, MAX_RESULT_SCAN_DEPTH + 1);
        assert!(text.is_empty());
        dismantle(value);

        // Nesting within the limit is scanned completely.
        let mut shallow = serde_json::Value::String("E0505".to_owned());
        for _ in 0..MAX_RESULT_SCAN_DEPTH {
            shallow = serde_json::Value::Array(vec![shallow]);
        }
        let (text, stats) = scan_value(&shallow);
        assert_eq!(text, "E0505");
        assert_eq!(stats.depth_skipped, 0);
        Ok(())
    }
}
