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
}
