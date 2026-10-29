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
//! carrying it ([`SkillInjector::confirm_delivered`]): a request rebuilt after
//! an emergency compaction, or retried after a context-length error, records
//! each excerpt exactly once.
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
    SkillSource, TriggerEvent, TriggerKind,
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
        }
    }
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
    /// delivered (`injection` event) once [`SkillInjector::confirm_delivered`]
    /// reports them.
    pub delivered: Vec<DeliveredSkill>,
    /// Hits of this round that were not selected, with the reason.
    pub dropped: Vec<Dropped>,
    /// Imported skills withheld because `inject_imported` is off.
    pub suppressed: Vec<String>,
    /// Labels the admit predicate rejected (new or active excerpts).
    pub withheld: Vec<String>,
    /// Labels of older excerpts that made room for newer ones.
    pub evicted: Vec<String>,
}

/// Per-session triggered skill injector (see the module documentation).
#[derive(Debug)]
pub struct SkillInjector {
    index: Arc<SkillIndex>,
    settings: SkillInjectionSettings,
    imported: BTreeSet<String>,
    state: Mutex<InjectorState>,
}

/// An excerpt in the context of the session.
#[derive(Debug, Clone)]
struct ActiveExcerpt {
    fragment: Fragment,
    delivery: DeliveredSkill,
    /// Reported by [`SkillInjector::confirm_delivered`]: a request carrying
    /// it was answered by the model.
    confirmed: bool,
}

#[derive(Debug, Default)]
struct InjectorState {
    injection: InjectionState,
    /// Bytes `select` booked for excerpts that never reached the model; they
    /// widen the session budget again (see [`Self::session_bytes`]).
    released_bytes: usize,
    active: Vec<ActiveExcerpt>,
    seen_tools: BTreeSet<String>,
    cursor: Option<HistoryCursor>,
}

impl InjectorState {
    fn reset(&mut self) {
        self.injection.reset_after_compaction();
        self.released_bytes = 0;
        self.active.clear();
        self.seen_tools.clear();
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
    /// the bytes of an excerpt that never reached the model are released.
    /// (`forget_skill` also reopens sibling sections that are still active;
    /// [`SkillInjector::prepare_round_with`] skips such a re-selection.)
    fn drop_excerpt(&mut self, excerpt: &ActiveExcerpt) {
        self.injection.forget_skill(&excerpt.delivery.skill);
        if !excerpt.confirmed {
            self.release(excerpt.delivery.bytes);
        }
    }
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
            state: Mutex::new(InjectorState::default()),
        }
    }

    /// Marks skills as imported (trust class `Evidence`; delivered only with
    /// `inject_imported`). Nothing in the tree marks skills as imported yet;
    /// the importer of PL-95 S8 feeds this set.
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

    /// Provenance of `skill`: imported (explicitly marked), bundled or from a
    /// trusted layer.
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
            None => SkillProvenance::Imported,
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
    pub fn reset_after_compaction(&self) {
        self.lock().reset();
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
        let mut budget = self.settings.budget(window_tokens);
        budget.per_session_bytes = budget
            .per_session_bytes
            .saturating_add(state.released_bytes);
        let selection = self.index.select(&borrowed, &mut state.injection, &budget);
        round.dropped = selection.dropped;
        let produced_at = jiff::Timestamp::now();
        for injection in selection.injections {
            let provenance = self.provenance(&injection.skill);
            if provenance == SkillProvenance::Imported && !self.settings.inject_imported {
                // Stays marked (no re-selection until the next compaction),
                // but costs no session bytes.
                state.release(injection.bytes);
                round.suppressed.push(injection.skill);
                continue;
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
                    continue;
                }
            };
            fragment.trust = trust;
            delivery.label = fragment.label.as_str().to_owned();
            if state.is_active(&delivery.label) {
                // Reopened by `forget_skill` of a sibling section: still in
                // the context, nothing new to deliver.
                state.release(delivery.bytes);
                round.dropped.push(Dropped {
                    skill: delivery.skill,
                    section: delivery.section,
                    reason: DropReason::AlreadyDelivered,
                });
                continue;
            }
            if !admit(&fragment) {
                state.injection.forget_skill(&delivery.skill);
                state.release(delivery.bytes);
                round.withheld.push(delivery.label);
                continue;
            }
            round.delivered.push(delivery.clone());
            state.active.push(ActiveExcerpt {
                fragment,
                delivery,
                confirmed: false,
            });
        }
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

    /// The model answered a request carrying the active excerpts: returns
    /// every one not reported before (each delivery exactly once) and marks
    /// it confirmed.
    #[must_use]
    pub fn confirm_delivered(&self) -> Vec<DeliveredSkill> {
        let mut state = self.lock();
        state
            .active
            .iter_mut()
            .filter(|excerpt| !excerpt.confirmed)
            .map(|excerpt| {
                excerpt.confirmed = true;
                excerpt.delivery.clone()
            })
            .collect()
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
        ToolCallResult::Success { value } => {
            let mut leaves = Vec::new();
            collect_string_leaves(value, &mut leaves);
            head_and_tail(&leaves)
        }
    }
}

fn collect_string_leaves<'a>(value: &'a serde_json::Value, out: &mut Vec<&'a str>) {
    match value {
        serde_json::Value::String(text) => out.push(text),
        serde_json::Value::Array(values) => {
            for value in values {
                collect_string_leaves(value, out);
            }
        }
        serde_json::Value::Object(object) => {
            for value in object.values() {
                collect_string_leaves(value, out);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

/// `parts` joined with `\n`; above [`MAX_RESULT_SCAN_BYTES`] the first half
/// and the last half of that joined text (separated by `\n`), at most
/// [`MAX_RESULT_SCAN_BYTES`] bytes in all.
fn head_and_tail(parts: &[&str]) -> String {
    let total = parts
        .iter()
        .map(|part| part.len())
        .sum::<usize>()
        .saturating_add(parts.len().saturating_sub(1));
    if total <= MAX_RESULT_SCAN_BYTES {
        return parts.join("\n");
    }
    let head_max = MAX_RESULT_SCAN_BYTES / 2;
    let tail_max = MAX_RESULT_SCAN_BYTES - head_max - 1;

    let mut out = String::with_capacity(MAX_RESULT_SCAN_BYTES);
    for (position, part) in parts.iter().enumerate() {
        if position > 0 {
            if out.len() >= head_max {
                break;
            }
            out.push('\n');
        }
        let room = head_max.saturating_sub(out.len());
        if part.len() <= room {
            out.push_str(part);
        } else {
            out.push_str(
                part.get(..floor_char_boundary(part, room))
                    .unwrap_or_default(),
            );
            break;
        }
    }

    let mut tail: Vec<&str> = Vec::new();
    let mut tail_len = 0usize;
    for part in parts.iter().rev() {
        let separator = usize::from(!tail.is_empty());
        let room = tail_max.saturating_sub(tail_len.saturating_add(separator));
        if room == 0 {
            break;
        }
        if part.len() <= room {
            tail.push(part);
            tail_len = tail_len.saturating_add(part.len() + separator);
        } else {
            let start = ceil_char_boundary(part, part.len() - room);
            tail.push(part.get(start..).unwrap_or_default());
            break;
        }
    }
    tail.reverse();
    out.push('\n');
    out.push_str(&tail.join("\n"));
    out
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
