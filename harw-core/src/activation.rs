//! Session-level activation for tools, instructions, and context providers.
//!
//! The [`ExtensionRegistry`] remains static: providers register once at startup.
//! This module lets each [`crate::session::AgentSession`] further narrow what is
//! exposed to the model — inspired by OpenClaw's tool profiles and Codex'
//! `ToolExposure::Hidden`.
//!
//! # Key types
//! - [`ToolProfile`] — coarse-grained named surface (Minimal / Coding / Full)
//! - [`SessionActivation`] — per-session filter; combines a profile with
//!   per-tool and per-provider overrides
//!
//! # Concurrency
//! [`SessionActivation`] is `Send + Sync` (only `BTreeSet`/`HashSet` + primitive
//! field). It is owned exclusively by an `AgentSession`, so concurrent access
//! happens only when the session itself is behind a lock, which is the caller's
//! responsibility.
//!
//! # Examples
//! ```rust,no_run
//! use harw_core::activation::{SessionActivation, ToolProfile};
//! use harw_tools::ToolName;
//!
//! let mut act = SessionActivation::new(ToolProfile::Coding);
//! assert!(act.is_tool_enabled(&ToolName::new("fs.read")));
//! act.disable_tool(ToolName::new("fs.write"));
//! assert!(!act.is_tool_enabled(&ToolName::new("fs.write")));
//! ```

#![allow(clippy::module_name_repetitions)]

use harw_tools::ToolName;
use std::collections::{BTreeSet, HashSet};

// ---------------------------------------------------------------------------
// ToolProfile
// ---------------------------------------------------------------------------

/// Named, coarse-grained tool profile (OpenClaw-inspired).
///
/// # Description
/// A `ToolProfile` defines a canonical set of tools that should be visible to
/// the model for a given session type. Individual tools can be toggled further
/// through [`SessionActivation::disable_tool`] and
/// [`SessionActivation::enable_tool`].
///
/// # Concurrency
/// `Copy` — safe to share freely across threads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolProfile {
    /// No tools exposed. Model runs in pure-conversation mode.
    Minimal,
    /// Read + write filesystem + shell — the default coding-agent surface.
    Coding,
    /// Every registered tool. Use with authority checks in place.
    Full,
}

/// Per-turn tool exposure tier for progressive tool loading.
///
/// # Description
///
/// The hardening document (§14, §15) describes three exposure tiers plus a
/// denied state. The turn loop computes exposure per turn from the callable
/// tool set, the turn trigger, the task intent, and the model profile.
///
/// - `Resident` — tool schema is always present in the model's context.
/// - `Deferred` — tool is callable but its schema is loaded on demand.
/// - `Hidden` — tool is callable but not visible until explicitly discovered.
/// - `Denied` — tool is not callable for this turn under any circumstance.
///
/// A tool-search tool, if added, must only search within the already-admitted
/// callable set — it can never discover `Denied` tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToolExposure {
    /// Tool schema is always present in the model context.
    Resident,
    /// Tool is callable but its schema loads on demand.
    Deferred,
    /// Tool is callable but not visible until explicitly discovered.
    Hidden,
    /// Tool is not callable for this turn.
    Denied,
}

impl Default for ToolProfile {
    /// Returns [`ToolProfile::Full`] as the default: no filtering, all
    /// registered tools are exposed. This preserves backward compatibility —
    /// existing sessions that do not explicitly set an activation see no change
    /// in behavior. Callers that want a narrower surface construct a
    /// `SessionActivation::new(ToolProfile::Coding)` explicitly.
    fn default() -> Self {
        Self::Full
    }
}

impl ToolProfile {
    /// Returns the canonical tool-name allow-list for this profile.
    ///
    /// # Returns
    /// `None` means "no filter — everything registered is exposed".
    /// `Some(set)` means only the named tools are exposed by default.
    ///
    /// # Concurrency
    /// Pure computation; allocates a new `HashSet` on every call.
    #[must_use]
    pub fn allowlist(self) -> Option<HashSet<ToolName>> {
        match self {
            Self::Minimal => Some(HashSet::new()),
            Self::Coding => {
                let mut s = HashSet::new();
                for n in ["fs.read", "fs.write", "fs.list", "fs.search", "shell.exec"] {
                    s.insert(ToolName::new(n));
                }
                Some(s)
            }
            Self::Full => None,
        }
    }
}

// ---------------------------------------------------------------------------
// SessionActivation
// ---------------------------------------------------------------------------

/// Werkzeuge, die jede Agent-Sitzung unabhängig von `[tools].admitted` ihrer
/// Definition freischaltet (Plan R9, Teil A): der lesende Skill-Katalog
/// `skills.search`/`skills.load`.
///
/// # Beschreibung
/// Beide lesen nur den bei der Montage eingefrorenen Skill-Katalog im
/// Host-Prozess — kein Workspace, kein Netz, kein Prozess, keine
/// Rechteklasse. Damit kein Zuschnitt (z. B. das Klemmen einer eigenen
/// Definition auf ihre Basisrolle) sie versehentlich entfernt, schaltet
/// [`crate::session::AgentSession::with_executable_agent_ir`] sie nach den
/// admittierten Namen immer ein; ein ausdrückliches `forbidden` der
/// Definition gewinnt weiterhin. Ob ein Werkzeug überhaupt sichtbar ist,
/// entscheidet weiter die Registry: ohne montierten Provider bleibt der
/// Name wirkungslos.
pub const ALWAYS_AVAILABLE_TOOLS: &[&str] = &["skills.search", "skills.load"];

/// Session-scoped filter over the [`harw_extension_api::ExtensionRegistry`].
///
/// # Description
/// `SessionActivation` lets a session narrow what the model can see without
/// modifying the shared, static registry. The evaluation order for a given
/// tool `name` is:
/// 1. If `name` is in `tools_disabled` → hidden.
/// 2. If `name` is in `tools_enabled_extra` → visible.
/// 3. Fall back to the [`ToolProfile::allowlist`]; `None` (Full) means visible.
///
/// Instructions and context providers are filtered by their string label. A
/// missing or empty label is treated as `"unlabeled"`.
///
/// # Concurrency
/// `Send + Sync`. Owned exclusively by `AgentSession`; callers must not access
/// it concurrently without external synchronization.
///
/// # Examples
/// ```rust,no_run
/// use harw_core::activation::{SessionActivation, ToolProfile};
/// use harw_tools::ToolName;
///
/// let mut act = SessionActivation::new(ToolProfile::Full);
/// act.disable_tool(ToolName::new("shell.exec"));
/// assert!(!act.is_tool_enabled(&ToolName::new("shell.exec")));
/// act.enable_tool(ToolName::new("shell.exec"));
/// assert!(act.is_tool_enabled(&ToolName::new("shell.exec")));
/// ```
#[derive(Debug, Clone, Default)]
pub struct SessionActivation {
    tool_profile: ToolProfile,
    /// Tools explicitly toggled off by this session (overrides profile allowlist).
    tools_disabled: HashSet<ToolName>,
    /// Tools explicitly re-enabled beyond the profile allowlist.
    tools_enabled_extra: HashSet<ToolName>,
    /// Instruction-provider labels currently disabled for this session.
    instructions_disabled: BTreeSet<String>,
    /// Context-provider labels currently disabled for this session.
    context_disabled: BTreeSet<String>,
}

impl SessionActivation {
    /// Creates a new `SessionActivation` with the given profile and no overrides.
    ///
    /// # Arguments
    /// - `profile` (`ToolProfile`): coarse-grained tool surface for this session.
    ///
    /// # Returns
    /// A freshly initialized `SessionActivation`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_core::activation::{SessionActivation, ToolProfile};
    /// let act = SessionActivation::new(ToolProfile::Minimal);
    /// assert_eq!(act.profile(), ToolProfile::Minimal);
    /// ```
    #[must_use]
    pub fn new(profile: ToolProfile) -> Self {
        Self {
            tool_profile: profile,
            ..Self::default()
        }
    }

    /// Returns the active [`ToolProfile`].
    ///
    /// # Returns
    /// The current coarse-grained tool profile.
    #[must_use]
    pub fn profile(&self) -> ToolProfile {
        self.tool_profile
    }

    /// Replaces the active [`ToolProfile`] in-place.
    ///
    /// # Arguments
    /// - `p` (`ToolProfile`): the new profile. Existing per-tool overrides are
    ///   preserved; they are re-evaluated against the new profile on the next
    ///   [`is_tool_enabled`][Self::is_tool_enabled] call.
    pub fn set_profile(&mut self, p: ToolProfile) {
        self.tool_profile = p;
    }

    // -----------------------------------------------------------------------
    // Tool filtering
    // -----------------------------------------------------------------------

    /// Returns `true` when `name` is visible to the model under the current
    /// activation state.
    ///
    /// # Description
    /// Evaluation order:
    /// 1. `tools_disabled` wins (hidden even if allowlisted).
    /// 2. `tools_enabled_extra` overrides the profile (visible even if not
    ///    in the profile's allowlist).
    /// 3. Profile allowlist: `None` (Full) → visible; `Some(set)` → only
    ///    names in the set are visible.
    ///
    /// # Arguments
    /// - `name` (`&ToolName`): the tool to query.
    ///
    /// # Returns
    /// `true` if the tool should be included in the model-visible tool list.
    ///
    /// # Concurrency
    /// Read-only; safe to call from any thread while holding a shared reference.
    #[must_use]
    pub fn is_tool_enabled(&self, name: &ToolName) -> bool {
        if self.tools_disabled.contains(name) {
            return false;
        }
        if self.tools_enabled_extra.contains(name) {
            return true;
        }
        match self.tool_profile.allowlist() {
            None => true,
            Some(set) => set.contains(name),
        }
    }

    /// Disables a specific tool for this session, overriding the profile.
    ///
    /// # Arguments
    /// - `name` (`ToolName`): tool to hide from the model. Any entry in
    ///   `tools_enabled_extra` for this name is removed first.
    pub fn disable_tool(&mut self, name: ToolName) {
        self.tools_enabled_extra.remove(&name);
        self.tools_disabled.insert(name);
    }

    /// Explicitly enables a tool, overriding the profile allowlist.
    ///
    /// # Arguments
    /// - `name` (`ToolName`): tool to expose to the model. Any entry in
    ///   `tools_disabled` for this name is removed first.
    pub fn enable_tool(&mut self, name: ToolName) {
        self.tools_disabled.remove(&name);
        self.tools_enabled_extra.insert(name);
    }

    /// Removes all per-tool overrides for `name`, reverting to profile-only
    /// evaluation.
    ///
    /// # Arguments
    /// - `name` (`&ToolName`): tool whose overrides are cleared.
    pub fn reset_tool(&mut self, name: &ToolName) {
        self.tools_disabled.remove(name);
        self.tools_enabled_extra.remove(name);
    }

    // -----------------------------------------------------------------------
    // Instructions filtering
    // -----------------------------------------------------------------------

    /// Returns `true` when the instructions provider identified by `label` is
    /// active for this session.
    ///
    /// # Arguments
    /// - `label` (`&str`): provider label; use `"baseline"` for the first
    ///   non-empty system prompt, `"unlabeled"` for providers without a label.
    ///
    /// # Returns
    /// `true` unless the label has been explicitly disabled via
    /// [`disable_instructions`][Self::disable_instructions].
    #[must_use]
    pub fn is_instructions_enabled(&self, label: &str) -> bool {
        !self.instructions_disabled.contains(label)
    }

    /// Disables the instructions provider identified by `label`.
    ///
    /// # Arguments
    /// - `label` (`impl Into<String>`): label of the provider to suppress.
    pub fn disable_instructions(&mut self, label: impl Into<String>) {
        self.instructions_disabled.insert(label.into());
    }

    /// Re-enables a previously disabled instructions provider.
    ///
    /// # Arguments
    /// - `label` (`&str`): label of the provider to restore.
    pub fn enable_instructions(&mut self, label: &str) {
        self.instructions_disabled.remove(label);
    }

    // -----------------------------------------------------------------------
    // Context filtering
    // -----------------------------------------------------------------------

    /// Returns `true` when the context provider identified by `label` is active
    /// for this session.
    ///
    /// # Arguments
    /// - `label` (`&str`): fragment label; empty string is treated as
    ///   `"unlabeled"` by the turn-loop filter, not here.
    ///
    /// # Returns
    /// `true` unless the label has been explicitly disabled via
    /// [`disable_context`][Self::disable_context].
    #[must_use]
    pub fn is_context_enabled(&self, label: &str) -> bool {
        !self.context_disabled.contains(label)
    }

    /// Disables the context provider identified by `label`.
    ///
    /// # Arguments
    /// - `label` (`impl Into<String>`): label of the fragment to suppress.
    pub fn disable_context(&mut self, label: impl Into<String>) {
        self.context_disabled.insert(label.into());
    }

    /// Re-enables a previously disabled context provider.
    ///
    /// # Arguments
    /// - `label` (`&str`): label of the fragment to restore.
    pub fn enable_context(&mut self, label: &str) {
        self.context_disabled.remove(label);
    }

    // -----------------------------------------------------------------------
    // Intersection
    // -----------------------------------------------------------------------

    /// Computes the intersection of `self` and `other`: a tool or label is
    /// visible in the result only if it would be visible under **both**
    /// activations. Monotone — the result never permits more than either
    /// input alone, only ever less or equal.
    ///
    /// # Instructions and context labels
    /// For these, "visible" is the default state — only `disable_*` narrows
    /// it, for an unbounded space of possible labels. The intersection is
    /// therefore exactly the union of the two disabled-label sets: a label
    /// ends up disabled in the result iff it was disabled on at least one
    /// side, which is precisely "enabled on both" negated.
    ///
    /// # Tools
    /// For tools, the default depends on [`ToolProfile`]: `Full` means "every
    /// name, including ones neither side has ever heard of", while
    /// `Coding`/`Minimal` mean "only a fixed, finite allow-list (or none)".
    /// Because [`ToolName`] is an open-ended space, the result cannot just
    /// combine the two sets of overrides — it has to also account for what
    /// each side does with a name *neither side mentions explicitly*.
    ///
    /// The construction: first, collect every name that appears in either
    /// side's `tools_disabled`, `tools_enabled_extra`, or profile allow-list
    /// into a finite candidate set. For every other, unmentioned name, both
    /// sides fall back to their profile's default, and "both allow it" is
    /// possible only when both profiles are `Full` (a finite `Coding`
    /// allow-list, by construction, never allows a name outside itself, and
    /// `Minimal` allows nothing outside overrides). So:
    /// - If both sides are `Full`: the result profile is `Full` too, with an
    ///   explicit `tools_disabled` entry for every candidate name that is not
    ///   allowed by both sides (everything else stays allowed by the `Full`
    ///   default, correctly).
    /// - Otherwise: the result profile is `Minimal` (nothing allowed by
    ///   default), with an explicit `tools_enabled_extra` entry for every
    ///   candidate name that *is* allowed by both sides.
    ///
    /// Either way, [`Self::is_tool_enabled`] on the result agrees with
    /// `self.is_tool_enabled(name) && other.is_tool_enabled(name)` for every
    /// possible `name`, not just the ones either side mentions.
    ///
    /// # Arguments
    /// - `other` (`&Self`): the activation to intersect with.
    ///
    /// # Returns
    /// A new `SessionActivation` representing the intersection.
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Self {
        let mut candidates: HashSet<ToolName> = HashSet::new();
        candidates.extend(self.tools_disabled.iter().cloned());
        candidates.extend(self.tools_enabled_extra.iter().cloned());
        candidates.extend(other.tools_disabled.iter().cloned());
        candidates.extend(other.tools_enabled_extra.iter().cloned());
        if let Some(set) = self.tool_profile.allowlist() {
            candidates.extend(set);
        }
        if let Some(set) = other.tool_profile.allowlist() {
            candidates.extend(set);
        }

        let both_full =
            self.tool_profile == ToolProfile::Full && other.tool_profile == ToolProfile::Full;
        let mut result = if both_full {
            Self::new(ToolProfile::Full)
        } else {
            Self::new(ToolProfile::Minimal)
        };

        for name in candidates {
            let allowed_by_both = self.is_tool_enabled(&name) && other.is_tool_enabled(&name);
            if both_full {
                if !allowed_by_both {
                    result.disable_tool(name);
                }
            } else if allowed_by_both {
                result.enable_tool(name);
            }
        }

        result.instructions_disabled = self
            .instructions_disabled
            .union(&other.instructions_disabled)
            .cloned()
            .collect();
        result.context_disabled = self
            .context_disabled
            .union(&other.context_disabled)
            .cloned()
            .collect();

        result
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str) -> ToolName {
        ToolName::new(name)
    }

    #[test]
    fn test_default_is_full_profile() {
        let act = SessionActivation::default();
        assert_eq!(act.profile(), ToolProfile::Full);
    }

    #[test]
    fn test_full_profile_allows_everything() {
        let act = SessionActivation::new(ToolProfile::Full);
        assert!(act.is_tool_enabled(&tool("absolutely.anything")));
        assert!(act.is_tool_enabled(&tool("custom.tool")));
    }

    #[test]
    fn test_minimal_profile_denies_all() {
        let act = SessionActivation::new(ToolProfile::Minimal);
        assert!(!act.is_tool_enabled(&tool("fs.read")));
        assert!(!act.is_tool_enabled(&tool("shell.exec")));
        assert!(!act.is_tool_enabled(&tool("anything")));
    }

    #[test]
    fn test_coding_profile_allows_only_allowlisted() {
        let act = SessionActivation::new(ToolProfile::Coding);
        assert!(act.is_tool_enabled(&tool("fs.read")));
        assert!(act.is_tool_enabled(&tool("fs.write")));
        assert!(act.is_tool_enabled(&tool("shell.exec")));
        assert!(!act.is_tool_enabled(&tool("custom.tool")));
        assert!(!act.is_tool_enabled(&tool("db.query")));
    }

    #[test]
    fn test_disable_overrides_profile() {
        let mut act = SessionActivation::new(ToolProfile::Full);
        act.disable_tool(tool("shell.exec"));
        assert!(!act.is_tool_enabled(&tool("shell.exec")));
        // other tools still visible
        assert!(act.is_tool_enabled(&tool("any.other")));
    }

    #[test]
    fn test_enable_extra_beyond_profile() {
        let mut act = SessionActivation::new(ToolProfile::Minimal);
        act.enable_tool(tool("custom.tool"));
        assert!(act.is_tool_enabled(&tool("custom.tool")));
        // other tools still blocked
        assert!(!act.is_tool_enabled(&tool("fs.read")));
    }

    #[test]
    fn test_reset_clears_overrides() {
        let mut act = SessionActivation::new(ToolProfile::Full);
        act.disable_tool(tool("shell.exec"));
        assert!(!act.is_tool_enabled(&tool("shell.exec")));
        act.reset_tool(&tool("shell.exec"));
        // back to profile default (Full → allowed)
        assert!(act.is_tool_enabled(&tool("shell.exec")));
    }

    #[test]
    fn test_instructions_toggle() {
        let mut act = SessionActivation::default();
        assert!(act.is_instructions_enabled("baseline"));
        act.disable_instructions("baseline");
        assert!(!act.is_instructions_enabled("baseline"));
        act.enable_instructions("baseline");
        assert!(act.is_instructions_enabled("baseline"));
    }

    #[test]
    fn test_context_toggle() {
        let mut act = SessionActivation::default();
        assert!(act.is_context_enabled("workspace_files"));
        act.disable_context("workspace_files");
        assert!(!act.is_context_enabled("workspace_files"));
        act.enable_context("workspace_files");
        assert!(act.is_context_enabled("workspace_files"));
    }

    // -----------------------------------------------------------------------
    // intersect
    // -----------------------------------------------------------------------

    /// Checks the defining property directly: for every probed name/label,
    /// the intersection agrees with the logical AND of the two inputs.
    fn assert_is_and_of(a: &SessionActivation, b: &SessionActivation, probes: &[&str]) {
        let i = a.intersect(b);
        for name in probes {
            let t = tool(name);
            assert_eq!(
                i.is_tool_enabled(&t),
                a.is_tool_enabled(&t) && b.is_tool_enabled(&t),
                "tool {name:?}: intersection must equal a AND b"
            );
            assert_eq!(
                i.is_instructions_enabled(name),
                a.is_instructions_enabled(name) && b.is_instructions_enabled(name),
                "instructions {name:?}: intersection must equal a AND b"
            );
            assert_eq!(
                i.is_context_enabled(name),
                a.is_context_enabled(name) && b.is_context_enabled(name),
                "context {name:?}: intersection must equal a AND b"
            );
        }
    }

    const PROBE_NAMES: &[&str] = &[
        "fs.read",
        "fs.write",
        "fs.list",
        "fs.search",
        "shell.exec",
        "custom.tool",
        "db.query",
        "baseline",
        "unlabeled",
        "workspace_files",
    ];

    #[test]
    fn intersect_of_full_and_full_is_full() {
        let a = SessionActivation::new(ToolProfile::Full);
        let b = SessionActivation::new(ToolProfile::Full);
        assert_is_and_of(&a, &b, PROBE_NAMES);
        let i = a.intersect(&b);
        assert!(i.is_tool_enabled(&tool("anything.at.all")));
    }

    #[test]
    fn intersect_of_full_and_minimal_denies_everything() {
        let a = SessionActivation::new(ToolProfile::Full);
        let b = SessionActivation::new(ToolProfile::Minimal);
        assert_is_and_of(&a, &b, PROBE_NAMES);
        let i = a.intersect(&b);
        assert!(!i.is_tool_enabled(&tool("fs.read")));
        assert!(!i.is_tool_enabled(&tool("anything.at.all")));
    }

    #[test]
    fn intersect_of_full_and_coding_matches_coding_allowlist() {
        let a = SessionActivation::new(ToolProfile::Full);
        let b = SessionActivation::new(ToolProfile::Coding);
        assert_is_and_of(&a, &b, PROBE_NAMES);
        let i = a.intersect(&b);
        assert!(i.is_tool_enabled(&tool("fs.read")));
        assert!(i.is_tool_enabled(&tool("shell.exec")));
        assert!(!i.is_tool_enabled(&tool("custom.tool")));
    }

    #[test]
    fn intersect_respects_disabled_override_on_either_side() {
        let mut a = SessionActivation::new(ToolProfile::Full);
        a.disable_tool(tool("shell.exec"));
        let b = SessionActivation::new(ToolProfile::Full);
        assert_is_and_of(&a, &b, PROBE_NAMES);
        let i = a.intersect(&b);
        assert!(!i.is_tool_enabled(&tool("shell.exec")));
        assert!(i.is_tool_enabled(&tool("fs.read")));
    }

    #[test]
    fn intersect_respects_enabled_extra_beyond_minimal_profile() {
        let mut a = SessionActivation::new(ToolProfile::Minimal);
        a.enable_tool(tool("custom.tool"));
        let mut b = SessionActivation::new(ToolProfile::Minimal);
        b.enable_tool(tool("custom.tool"));
        assert_is_and_of(&a, &b, PROBE_NAMES);
        let i = a.intersect(&b);
        assert!(i.is_tool_enabled(&tool("custom.tool")));
        assert!(!i.is_tool_enabled(&tool("fs.read")));
    }

    #[test]
    fn intersect_instructions_and_context_union_disabled_labels() {
        let mut a = SessionActivation::default();
        a.disable_instructions("baseline");
        a.disable_context("workspace_files");
        let mut b = SessionActivation::default();
        b.disable_instructions("other");
        assert_is_and_of(&a, &b, PROBE_NAMES);
        let i = a.intersect(&b);
        assert!(!i.is_instructions_enabled("baseline"));
        assert!(!i.is_instructions_enabled("other"));
        assert!(!i.is_context_enabled("workspace_files"));
    }

    #[test]
    fn intersect_is_subset_of_both_inputs() {
        let mut a = SessionActivation::new(ToolProfile::Full);
        a.disable_tool(tool("shell.exec"));
        let mut b = SessionActivation::new(ToolProfile::Coding);
        b.disable_tool(tool("fs.write"));
        let i = a.intersect(&b);
        for name in PROBE_NAMES {
            let t = tool(name);
            if i.is_tool_enabled(&t) {
                assert!(
                    a.is_tool_enabled(&t),
                    "result allows {name:?} but a does not"
                );
                assert!(
                    b.is_tool_enabled(&t),
                    "result allows {name:?} but b does not"
                );
            }
        }
    }

    #[test]
    fn intersect_is_commutative_in_permission() {
        let mut a = SessionActivation::new(ToolProfile::Full);
        a.disable_tool(tool("shell.exec"));
        let mut b = SessionActivation::new(ToolProfile::Coding);
        b.enable_tool(tool("custom.tool"));
        b.disable_instructions("baseline");

        let ab = a.intersect(&b);
        let ba = b.intersect(&a);

        for name in PROBE_NAMES {
            let t = tool(name);
            assert_eq!(ab.is_tool_enabled(&t), ba.is_tool_enabled(&t));
            assert_eq!(
                ab.is_instructions_enabled(name),
                ba.is_instructions_enabled(name)
            );
            assert_eq!(ab.is_context_enabled(name), ba.is_context_enabled(name));
        }
    }

    #[test]
    fn intersect_is_idempotent() {
        let mut a = SessionActivation::new(ToolProfile::Coding);
        a.enable_tool(tool("custom.tool"));
        a.disable_tool(tool("fs.write"));
        a.disable_context("workspace_files");

        let once = a.intersect(&a);
        let twice = once.intersect(&a);

        for name in PROBE_NAMES {
            let t = tool(name);
            assert_eq!(once.is_tool_enabled(&t), a.is_tool_enabled(&t));
            assert_eq!(twice.is_tool_enabled(&t), a.is_tool_enabled(&t));
            assert_eq!(
                once.is_instructions_enabled(name),
                a.is_instructions_enabled(name)
            );
            assert_eq!(once.is_context_enabled(name), a.is_context_enabled(name));
        }
    }
}
