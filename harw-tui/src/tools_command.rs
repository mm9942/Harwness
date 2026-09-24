//! `/tools` slash-command handler for the harw TUI.
//!
//! Lists the currently-registered tools and their session-level activation
//! state, and lets the user toggle them or switch tool profiles at runtime.
//!
//! # Commands
//! - `/tools`                    — list all registered tools and their on/off state
//! - `/tools on <name>`          — enable one tool (overrides profile filter)
//! - `/tools off <name>`         — disable one tool (overrides profile filter)
//! - `/tools reset [<name>]`     — clear overrides (all overrides, or one by name)
//! - `/tools profile <p>`        — switch profile: `minimal`, `coding`, or `full`
//!
//! # Design
//! Backed by [`harw_core::activation::SessionActivation`] which the caller holds
//! as `&mut`, never letting it exceed a session ceiling (base ∩ mode) passed in
//! alongside it. Tool listings and toggle validation work off a pre-collected
//! `(name, enabled)` snapshot rather than a live
//! [`harw_extension_api::ExtensionRegistry`] borrow — the TUI run-loop cannot
//! hold both a shared registry borrow and a mutable activation borrow at once.
//! Spec source: harw-tui design, tools-command section.
//!
//! # Concurrency
//! Pure synchronous function; no locking. The caller (TUI run-loop) owns both
//! the tool snapshot and `activation` exclusively while this runs.
//!
//! # Examples
//! ```rust,no_run
//! use harw_tui::tools_command::{dispatch_tools_command_bounded, ToolsCommandOutcome};
//! use harw_core::activation::{SessionActivation, ToolProfile};
//!
//! let snapshot: Vec<(String, bool)> = Vec::new();
//! let mut activation = SessionActivation::new(ToolProfile::Full);
//! let ceiling = SessionActivation::new(ToolProfile::Full);
//! let outcome = dispatch_tools_command_bounded("", &snapshot, &mut activation, &ceiling);
//! assert!(matches!(outcome, ToolsCommandOutcome::Listing(_)));
//! ```

use harw_core::activation::{SessionActivation, ToolProfile};
use harw_extension_api::ToolName;

// ---------------------------------------------------------------------------
// Public outcome type
// ---------------------------------------------------------------------------

/// Structured outcome of a `/tools` invocation, rendered by the TUI as chat
/// lines.
///
/// # Description
/// The TUI run-loop pattern-matches on this enum and converts it to one or
/// more [`ratatui::text::Line`] entries in the chat history. Each variant
/// carries all information required for rendering — the caller does not need
/// to inspect any additional state.
///
/// # Variants
/// - [`Listing`](Self::Listing) — human-readable status listing, one string
///   per tool plus a profile header.
/// - [`Confirmation`](Self::Confirmation) — single-line success message for
///   a mutation sub-command (`on`/`off`/`reset`/`profile`).
/// - [`Error`](Self::Error) — user-facing error: unknown sub-command, unknown
///   tool name, or invalid profile string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolsCommandOutcome {
    /// Human-readable status listing (one line per registered tool, plus a
    /// profile header line). Empty when no tools are registered.
    Listing(Vec<String>),
    /// Confirmation of a successful mutation, e.g. `"enabled fs.read"`.
    Confirmation(String),
    /// User-facing error message — displayed in the chat history.
    Error(String),
}

impl ToolsCommandOutcome {
    /// Converts the outcome to a multi-line string, one entry per line, for
    /// display in the chat history widget.
    ///
    /// # Returns
    /// A `Vec<String>` where each element is one chat line. Callers that
    /// need a flat `String` can join with `'\n'`.
    #[must_use]
    pub fn into_lines(self) -> Vec<String> {
        match self {
            Self::Listing(lines) => lines,
            Self::Confirmation(msg) => vec![msg],
            Self::Error(msg) => vec![format!("[tools error] {msg}")],
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Dispatches a `/tools` command using pre-collected tool state, avoiding
/// split-borrow conflicts in the TUI run-loop.
///
/// # Description
/// The TUI run-loop holds `&mut gateway` and needs to read the registry's tool
/// list (requiring `&session`) AND mutate the activation (requiring
/// `&mut session`) — both through `gateway.session_mut()`. Rust forbids holding
/// both borrows simultaneously.
///
/// This function accepts `tool_snapshot`, a pre-collected `Vec<(name, enabled)>`
/// gathered while only a shared `&session` borrow was held. The caller drops
/// that borrow before calling this function with `&mut activation`.
///
/// Dispatches a `/tools` command, never letting the session's activation
/// exceed `ceiling`.
///
/// Entfernt W2d-2/CE (E8): der frühere unbegrenzte `dispatch_tools_command`
/// (ohne Deckenprüfung) hatte in `tools_command.rs` keine eigenen Tests und
/// wurde ersatzlos gestrichen. Entfernt W2d-2/F-TOOLS (Befund R6): der
/// ebenfalls unbegrenzte `handle_tools_command` (kein Prod-Aufrufer im
/// Workspace) ist aus demselben Grund gestrichen — jeder Aufrufer verwendet
/// ab jetzt diese begrenzte Variante. Von den früheren Sub-Handlern bleibt
/// nur `set_tool` bestehen (gemeinsam mit `bounded_set_tool` genutzt);
/// `list_tools`, `reset_all`, `reset_one` und `set_profile` sind mit
/// `handle_tools_command` entfallen.
///
/// # Description
/// `ceiling` is the session's tool ceiling: base activation ∩ mode activation
/// (`AgentSession::mode_ceiling`). Every sub-command is bounded by it:
///
/// - `on <tool>` / `off <tool>`: checked first with
///   `crate::runtime_commands::validate_tool_toggle` against `ceiling` and
///   `tool_snapshot`. An unknown tool name, or an attempt to enable a tool the
///   ceiling forbids, is rejected with [`ToolsCommandOutcome::Error`] and
///   `activation` is left untouched. `off` only narrows and always applies to
///   a known tool.
/// - `reset`: replaces `activation` with a copy of `ceiling` — the widest
///   state this command may ever restore.
/// - `reset <name>`: sets the named tool to exactly the state `ceiling` gives
///   it (enabled iff `ceiling.is_tool_enabled(name)`); other tools are
///   unchanged.
/// - `profile <p>`: applies the profile, then cuts the result down to
///   `ceiling` via [`SessionActivation::intersect`]. The confirmation names the
///   effective state: registered tools the profile would have enabled but the
///   ceiling withholds are listed together with the effective profile.
///
/// # Arguments
/// - `args` (`&str`): everything after `/tools` in the user's input.
/// - `tool_snapshot` (`&[(String, bool)]`): `(tool_name, is_enabled)` pairs
///   collected from the registry while holding a shared session borrow; also
///   used as the known-tool set for toggle validation and for reporting
///   withheld tools after `profile`.
/// - `activation` (`&mut SessionActivation`): current session activation;
///   mutated only within `ceiling`.
/// - `ceiling` (`&SessionActivation`): the session ceiling (base ∩ mode),
///   which `activation` may never exceed.
///
/// # Returns
/// A [`ToolsCommandOutcome`] ready for rendering.
///
/// # Errors
/// Returns [`ToolsCommandOutcome::Error`] (rendered via `into_lines`) when
/// `on`/`off` names an unregistered tool, when `on` would enable a tool
/// beyond `ceiling`, when `profile` names an unknown profile, or when the
/// sub-command is unrecognized. No mutation occurs in the error case.
///
/// # Concurrency
/// Pure synchronous function; no locking. The caller owns `activation`
/// exclusively while this runs.
#[must_use]
pub fn dispatch_tools_command_bounded(
    args: &str,
    tool_snapshot: &[(String, bool)],
    activation: &mut SessionActivation,
    ceiling: &SessionActivation,
) -> ToolsCommandOutcome {
    let trimmed = args.trim();
    let parts: Vec<&str> = if trimmed.is_empty() {
        Vec::new()
    } else {
        trimmed.split_whitespace().collect()
    };

    match parts.as_slice() {
        [] => list_from_snapshot(tool_snapshot, activation),
        ["on", name] => bounded_set_tool(activation, ceiling, tool_snapshot, name, true),
        ["off", name] => bounded_set_tool(activation, ceiling, tool_snapshot, name, false),
        ["reset"] => bounded_reset_all(activation, ceiling),
        ["reset", name] => bounded_reset_one(activation, ceiling, name),
        ["profile", p] => bounded_set_profile(activation, ceiling, tool_snapshot, p),
        _ => ToolsCommandOutcome::Error(
            "usage: /tools | /tools on <name> | /tools off <name> \
             | /tools reset [<name>] | /tools profile <minimal|coding|full>"
                .to_string(),
        ),
    }
}

// ---------------------------------------------------------------------------
// Sub-handlers (private)
// ---------------------------------------------------------------------------

/// Lists tools from a pre-collected `(name, enabled)` snapshot.
///
/// # Description
/// Used by [`dispatch_tools_command_bounded`] when the caller cannot hold a
/// shared registry borrow alongside a mutable activation borrow (split-borrow
/// constraint in the TUI run-loop). The snapshot captures `(name, is_enabled)`
/// pairs before any mutation occurs; the profile header is read freshly from
/// `activation` (only `profile()` is needed, which is always safe to call).
///
/// # Arguments
/// - `snapshot` (`&[(String, bool)]`): `(tool_name, is_enabled)` collected
///   from the registry while holding a shared session borrow.
/// - `activation` (`&SessionActivation`): used only to read the current
///   profile for the header line.
///
/// # Returns
/// [`ToolsCommandOutcome::Listing`] with profile header + one entry per tool.
fn list_from_snapshot(
    snapshot: &[(String, bool)],
    activation: &SessionActivation,
) -> ToolsCommandOutcome {
    let mut lines = Vec::new();
    lines.push(format!("profile: {:?}", activation.profile()));

    if snapshot.is_empty() {
        lines.push("  (no tools registered)".to_string());
    } else {
        for (name, enabled) in snapshot {
            let mark = if *enabled { "on " } else { "off" };
            lines.push(format!("  [{}] {}", mark, name));
        }
    }

    ToolsCommandOutcome::Listing(lines)
}

/// Enables or disables a single tool by name.
///
/// # Description
/// Converts `name` to a [`ToolName`] and calls either
/// [`SessionActivation::enable_tool`] or [`SessionActivation::disable_tool`]
/// based on `on`. Returns a [`ToolsCommandOutcome::Confirmation`] in either
/// case; the registry is not consulted (unknown names are allowed — they
/// simply have no effect on exposed tools).
///
/// # Arguments
/// - `activation` — session activation to mutate.
/// - `name` — raw tool-name string from the command line.
/// - `on` — `true` to enable, `false` to disable.
fn set_tool(activation: &mut SessionActivation, name: &str, on: bool) -> ToolsCommandOutcome {
    let tool_name = ToolName::new(name.to_string());
    if on {
        activation.enable_tool(tool_name);
        ToolsCommandOutcome::Confirmation(format!("enabled {name}"))
    } else {
        activation.disable_tool(tool_name);
        ToolsCommandOutcome::Confirmation(format!("disabled {name}"))
    }
}

/// Validates and applies a single `on <tool>` / `off <tool>` toggle against
/// the session ceiling (base ∩ mode).
///
/// # Description
/// Delegates the check to `crate::runtime_commands::validate_tool_toggle`.
/// On `Err`, `activation` is left untouched and the error's `Display` message
/// is returned as [`ToolsCommandOutcome::Error`]. On `Ok`, delegates to
/// [`set_tool`] to apply the change and build the confirmation message.
///
/// # Arguments
/// - `activation` — session activation to mutate on success.
/// - `ceiling` — session ceiling (base ∩ mode) the toggle may never exceed.
/// - `known_tools` — `(name, enabled)` pairs used to validate `name`.
/// - `name` — raw tool-name string from the command line.
/// - `enable` — `true` for `on`, `false` for `off`.
fn bounded_set_tool(
    activation: &mut SessionActivation,
    ceiling: &SessionActivation,
    known_tools: &[(String, bool)],
    name: &str,
    enable: bool,
) -> ToolsCommandOutcome {
    if let Err(err) =
        crate::runtime_commands::validate_tool_toggle(ceiling, known_tools, name, enable)
    {
        return ToolsCommandOutcome::Error(err.to_string());
    }
    set_tool(activation, name, enable)
}

/// Resets the whole activation to the session ceiling (Befund T2).
///
/// # Description
/// Replaces `activation` with a copy of `ceiling` (base ∩ mode). Unlike a
/// plain reset to a fresh activation of the current profile, this never
/// widens beyond the ceiling: a fresh activation of the current profile could
/// re-enable tools the base or mode forbids.
///
/// # Arguments
/// - `activation` — session activation to replace in-place.
/// - `ceiling` — session ceiling to restore.
fn bounded_reset_all(
    activation: &mut SessionActivation,
    ceiling: &SessionActivation,
) -> ToolsCommandOutcome {
    *activation = ceiling.clone();
    ToolsCommandOutcome::Confirmation("reset all tool overrides to the session ceiling".to_string())
}

/// Resets one named tool to the state the session ceiling gives it
/// (Befund T2).
///
/// # Description
/// Clears the tool's overrides with [`SessionActivation::reset_tool`]; if the
/// resulting profile-only state differs from `ceiling.is_tool_enabled(name)`,
/// an explicit override is set so the tool ends up enabled exactly when the
/// ceiling enables it. Other tools are unchanged.
///
/// # Arguments
/// - `activation` — session activation to mutate.
/// - `ceiling` — session ceiling whose state for `name` is adopted.
/// - `name` — raw tool-name string from the command line.
fn bounded_reset_one(
    activation: &mut SessionActivation,
    ceiling: &SessionActivation,
    name: &str,
) -> ToolsCommandOutcome {
    let tool_name = ToolName::new(name.to_string());
    let allowed = ceiling.is_tool_enabled(&tool_name);
    activation.reset_tool(&tool_name);
    if activation.is_tool_enabled(&tool_name) != allowed {
        if allowed {
            activation.enable_tool(tool_name);
        } else {
            activation.disable_tool(tool_name);
        }
    }
    let state = if allowed { "on" } else { "off" };
    ToolsCommandOutcome::Confirmation(format!("reset {name} to session ceiling ({state})"))
}

/// Applies a `profile <name>` switch, then cuts the resulting activation down
/// to the session ceiling (Befunde T3/T7).
///
/// # Description
/// Parses `p` case-insensitively into a [`ToolProfile`] variant. On success, applies the profile
/// to a copy of `activation`, stores `requested.intersect(ceiling)`
/// ([`SessionActivation::intersect`]) back into `activation`, and reports the
/// effective result: when no registered tool (from `known_tools`) and no name
/// on the requested profile's allow-list was withheld by the ceiling, the
/// message is `profile set to <p>`; otherwise it names the effective profile
/// and the withheld tools in sorted order. An unknown profile name leaves
/// `activation` untouched.
///
/// # Arguments
/// - `activation` — session activation to mutate.
/// - `ceiling` — session ceiling (base ∩ mode) the result is intersected with.
/// - `known_tools` — `(name, enabled)` pairs of registered tools, used to
///   report withheld tools.
/// - `p` — profile name: `"minimal"`, `"coding"`, or `"full"`.
///
/// # Errors
/// Returns [`ToolsCommandOutcome::Error`] when `p` does not match any known
/// profile name; `activation` is not mutated in that case.
fn bounded_set_profile(
    activation: &mut SessionActivation,
    ceiling: &SessionActivation,
    known_tools: &[(String, bool)],
    p: &str,
) -> ToolsCommandOutcome {
    let profile = match p.to_ascii_lowercase().as_str() {
        "minimal" => ToolProfile::Minimal,
        "coding" => ToolProfile::Coding,
        "full" => ToolProfile::Full,
        other => {
            return ToolsCommandOutcome::Error(format!(
                "unknown profile '{other}' — choose: minimal, coding, full"
            ));
        }
    };
    let mut requested = activation.clone();
    requested.set_profile(profile);
    let effective = requested.intersect(ceiling);

    let mut candidates: std::collections::BTreeSet<String> = known_tools
        .iter()
        .map(|(name, _)| name.to_owned())
        .collect();
    if let Some(allowlist) = profile.allowlist() {
        candidates.extend(allowlist.into_iter().map(|name| name.0));
    }
    let withheld: Vec<String> = candidates
        .into_iter()
        .filter(|name| {
            let tool_name = ToolName::new(name.as_str());
            requested.is_tool_enabled(&tool_name) && !effective.is_tool_enabled(&tool_name)
        })
        .collect();

    *activation = effective;
    if withheld.is_empty() {
        ToolsCommandOutcome::Confirmation(format!("profile set to {p}"))
    } else {
        ToolsCommandOutcome::Confirmation(format!(
            "profile {p} limited by session ceiling: effective profile {:?}, withheld: {}",
            activation.profile(),
            withheld.join(", ")
        ))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn full_activation() -> SessionActivation {
        SessionActivation::new(ToolProfile::Full)
    }

    /// Ceiling for the `_bounded` tests: `Minimal` profile with only
    /// `test.alpha` explicitly enabled (an allowlist) — `test.beta` stays
    /// forbidden.
    fn bounded_ceiling() -> SessionActivation {
        let mut ceiling = SessionActivation::new(ToolProfile::Minimal);
        ceiling.enable_tool(ToolName::new("test.alpha"));
        ceiling
    }

    /// Ceiling for the reset tests: `Full` base with `test.beta` disabled.
    fn full_ceiling_without_beta() -> SessionActivation {
        let mut ceiling = SessionActivation::new(ToolProfile::Full);
        ceiling.disable_tool(ToolName::new("test.beta"));
        ceiling
    }

    /// `(name, enabled)` snapshot with both fake tools registered.
    fn both_known() -> Vec<(String, bool)> {
        vec![
            ("test.alpha".to_string(), true),
            ("test.beta".to_string(), false),
        ]
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    /// `/tools` with no registered tools yields the profile line + "(no tools registered)".
    ///
    /// Migrated from the removed `handle_tools_command` (W2d-2/F-TOOLS, R6):
    /// same output contract, driven through `dispatch_tools_command_bounded`
    /// with an empty snapshot instead of a registry.
    #[test]
    fn test_dispatch_tools_command_bounded_list_returns_profile_line_when_no_tools() -> TestResult {
        let known: Vec<(String, bool)> = Vec::new();
        let ceiling = full_activation();
        let mut activation = full_activation();

        let outcome = dispatch_tools_command_bounded("", &known, &mut activation, &ceiling);

        let ToolsCommandOutcome::Listing(lines) = outcome else {
            return Err(TestError::Unexpected("expected Listing".into()));
        };
        assert!(!lines.is_empty(), "should have at least a profile line");
        assert!(
            lines[0].contains("profile:"),
            "first line should be profile header"
        );
        assert!(
            lines.iter().any(|l| l.contains("no tools registered")),
            "should mention no tools"
        );
        Ok(())
    }

    /// `/tools` with two known tools lists both with on/off status markers
    /// taken directly from the snapshot.
    ///
    /// Migrated from the removed `handle_tools_command` (W2d-2/F-TOOLS, R6).
    #[test]
    fn test_dispatch_tools_command_bounded_list_shows_known_tools_with_snapshot_status()
    -> TestResult {
        let known = both_known(); // test.alpha=true, test.beta=false
        let ceiling = full_activation();
        let mut activation = full_activation();

        let outcome = dispatch_tools_command_bounded("", &known, &mut activation, &ceiling);

        let ToolsCommandOutcome::Listing(lines) = outcome else {
            return Err(TestError::Unexpected("expected Listing".into()));
        };
        assert!(
            lines
                .iter()
                .any(|l| l.contains("[on ]") && l.contains("test.alpha")),
            "alpha is enabled in the snapshot, so it should show as on"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("[off]") && l.contains("test.beta")),
            "beta is disabled in the snapshot, so it should show as off"
        );
        Ok(())
    }

    /// `/tools on <name>` enables a tool that was disabled, when the ceiling
    /// allows it — the removed `handle_tools_command` had no ceiling, so this
    /// case (unlike `off`, already covered by
    /// `test_dispatch_tools_command_bounded_disable_known_tool_applies`) had
    /// no bounded-path equivalent yet.
    #[test]
    fn test_dispatch_tools_command_bounded_on_enables_disabled_tool_within_ceiling() {
        let known = both_known();
        let ceiling = full_activation();
        let mut activation = full_activation();
        activation.disable_tool(ToolName::new("test.alpha"));
        assert!(!activation.is_tool_enabled(&ToolName::new("test.alpha")));

        let outcome =
            dispatch_tools_command_bounded("on test.alpha", &known, &mut activation, &ceiling);

        assert_eq!(
            outcome,
            ToolsCommandOutcome::Confirmation("enabled test.alpha".to_string())
        );
        assert!(activation.is_tool_enabled(&ToolName::new("test.alpha")));
    }

    // `off_disables_tool` (removed `handle_tools_command` test) is a pure
    // duplicate of `test_dispatch_tools_command_bounded_disable_known_tool_applies`
    // below — deleted rather than migrated, per Ledger W2d2/F-TOOLS.md.

    // `reset_all_reverts_overrides` (removed `handle_tools_command` test) is
    // covered at the parsing level by
    // `test_dispatch_tools_command_bounded_reset_restores_ceiling` — deleted,
    // per Ledger W2d2/F-TOOLS.md.

    // `reset_one_reverts_single_tool` (removed `handle_tools_command` test)
    // is covered at the parsing level by
    // `test_dispatch_tools_command_bounded_reset_one_keeps_ceiling_disabled_tool_off`
    // and `..._reset_then_reset_one_stays_within_ceiling` — deleted, per
    // Ledger W2d2/F-TOOLS.md.

    /// `/tools profile coding` under an unrestricted (`Full`) ceiling reports
    /// the plain confirmation and exposes exactly the `Coding` allowlist.
    ///
    /// Note: unlike the removed `handle_tools_command`, the bounded path
    /// always intersects with `ceiling`
    /// ([`harw_core::activation::SessionActivation::intersect`]), whose
    /// result profile is `Minimal` unless *both* sides are `Full` — so
    /// `activation.profile()` is not asserted here; tool visibility is.
    #[test]
    fn test_dispatch_tools_command_bounded_profile_switch_reports_plain_confirmation_when_unrestricted()
     {
        let known: Vec<(String, bool)> = Vec::new();
        let ceiling = full_activation();
        let mut activation = full_activation();

        let outcome =
            dispatch_tools_command_bounded("profile coding", &known, &mut activation, &ceiling);

        assert_eq!(
            outcome,
            ToolsCommandOutcome::Confirmation("profile set to coding".to_string())
        );
        assert!(
            activation.is_tool_enabled(&ToolName::new("fs.read")),
            "Coding allowlist tool must be enabled under an unrestricted ceiling"
        );
        assert!(
            !activation.is_tool_enabled(&ToolName::new("custom.tool")),
            "tool outside the Coding allowlist must stay hidden"
        );
    }

    /// `/tools profile <unknown>` returns an error variant and leaves
    /// `activation` untouched.
    ///
    /// Migrated from the removed `handle_tools_command` (W2d-2/F-TOOLS, R6).
    #[test]
    fn test_dispatch_tools_command_bounded_profile_switch_rejects_unknown_name() {
        let known: Vec<(String, bool)> = Vec::new();
        let ceiling = full_activation();
        let mut activation = full_activation();

        let outcome =
            dispatch_tools_command_bounded("profile turbo", &known, &mut activation, &ceiling);

        assert!(
            matches!(outcome, ToolsCommandOutcome::Error(ref msg) if msg.contains("turbo")),
            "error should name the unknown profile"
        );
        assert_eq!(activation.profile(), ToolProfile::Full);
    }

    /// An unrecognised sub-command returns a usage error.
    ///
    /// Migrated from the removed `handle_tools_command` (W2d-2/F-TOOLS, R6).
    #[test]
    fn test_dispatch_tools_command_bounded_unknown_subcommand_returns_usage_error() {
        let known: Vec<(String, bool)> = Vec::new();
        let ceiling = full_activation();
        let mut activation = full_activation();

        let outcome =
            dispatch_tools_command_bounded("frobnicate", &known, &mut activation, &ceiling);

        assert!(matches!(outcome, ToolsCommandOutcome::Error(_)));
    }

    /// `into_lines` on a `Listing` returns lines unchanged.
    #[test]
    fn into_lines_listing_passthrough() {
        let lines = vec!["a".to_string(), "b".to_string()];
        let outcome = ToolsCommandOutcome::Listing(lines.clone());
        assert_eq!(outcome.into_lines(), lines);
    }

    /// `into_lines` on a `Confirmation` returns a single-element vec.
    #[test]
    fn into_lines_confirmation_single() {
        let outcome = ToolsCommandOutcome::Confirmation("ok".to_string());
        assert_eq!(outcome.into_lines(), vec!["ok".to_string()]);
    }

    /// `into_lines` on an `Error` prefixes the message.
    #[test]
    fn into_lines_error_prefixed() {
        let outcome = ToolsCommandOutcome::Error("bad".to_string());
        let lines = outcome.into_lines();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("[tools error]") && lines[0].contains("bad"));
    }

    // ── dispatch_tools_command_bounded ──────────────────────────────────────

    /// `off <unknown>` is rejected and leaves the activation untouched — the
    /// tool's (Full-profile default) visibility must be unchanged, not just
    /// "still visible by coincidence".
    #[test]
    fn test_dispatch_tools_command_bounded_unknown_tool_changes_nothing() {
        let known = vec![("test.alpha".to_string(), true)];
        let mut activation = full_activation();
        let ceiling = bounded_ceiling();
        let ghost = ToolName::new("test.ghost");
        assert!(
            activation.is_tool_enabled(&ghost),
            "Full profile defaults to visible"
        );

        let outcome =
            dispatch_tools_command_bounded("off test.ghost", &known, &mut activation, &ceiling);

        assert!(
            matches!(outcome, ToolsCommandOutcome::Error(ref msg) if msg.contains("test.ghost")),
            "unknown tool must be rejected by name"
        );
        assert!(
            activation.is_tool_enabled(&ghost),
            "rejected toggle must not mutate activation"
        );
    }

    /// `on <tool>` beyond what `ceiling` allows is rejected, even though the
    /// tool is registered.
    #[test]
    fn test_dispatch_tools_command_bounded_enable_beyond_ceiling_is_rejected() {
        let known = vec![
            ("test.alpha".to_string(), false),
            ("test.beta".to_string(), false),
        ];
        let mut activation = SessionActivation::new(ToolProfile::Minimal);
        let ceiling = bounded_ceiling();
        let beta = ToolName::new("test.beta");
        assert!(!activation.is_tool_enabled(&beta));

        let outcome =
            dispatch_tools_command_bounded("on test.beta", &known, &mut activation, &ceiling);

        assert!(
            matches!(outcome, ToolsCommandOutcome::Error(ref msg) if msg.contains("test.beta")),
            "toggle beyond ceiling must be rejected by name"
        );
        assert!(
            !activation.is_tool_enabled(&beta),
            "rejected enable must not mutate activation"
        );
    }

    /// Disabling a known tool always applies, regardless of `ceiling` — turning
    /// a tool off only narrows, never exceeds, any ceiling.
    #[test]
    fn test_dispatch_tools_command_bounded_disable_known_tool_applies() {
        let known = vec![("test.alpha".to_string(), true)];
        let mut activation = full_activation();
        let ceiling = bounded_ceiling();
        let alpha = ToolName::new("test.alpha");
        assert!(activation.is_tool_enabled(&alpha));

        let outcome =
            dispatch_tools_command_bounded("off test.alpha", &known, &mut activation, &ceiling);

        assert_eq!(
            outcome,
            ToolsCommandOutcome::Confirmation("disabled test.alpha".to_string())
        );
        assert!(!activation.is_tool_enabled(&alpha));
    }

    /// `profile full` would normally allow every tool, but the resulting
    /// activation must be cut down to `ceiling` — only `test.alpha` (allowed by
    /// `ceiling`) stays visible, `test.beta` does not. With no registered tools
    /// and no allow-list names withheld, the plain confirmation is honest.
    #[test]
    fn test_dispatch_tools_command_bounded_profile_is_cut_to_ceiling() {
        let known: Vec<(String, bool)> = Vec::new();
        let mut activation = SessionActivation::new(ToolProfile::Minimal);
        let ceiling = bounded_ceiling();

        let outcome =
            dispatch_tools_command_bounded("profile full", &known, &mut activation, &ceiling);

        assert_eq!(
            outcome,
            ToolsCommandOutcome::Confirmation("profile set to full".to_string())
        );
        assert!(
            activation.is_tool_enabled(&ToolName::new("test.alpha")),
            "ceiling allows test.alpha, so the cut activation must still allow it"
        );
        assert!(
            !activation.is_tool_enabled(&ToolName::new("test.beta")),
            "ceiling forbids test.beta, so `profile full` must not resurrect it"
        );
    }

    /// T2: `reset` restores the ceiling, not a fresh `Full` profile — session
    /// overrides are dropped, and `test.beta`, disabled by the ceiling, stays
    /// off.
    #[test]
    fn test_dispatch_tools_command_bounded_reset_restores_ceiling() {
        let known = both_known();
        let ceiling = full_ceiling_without_beta();
        let alpha = ToolName::new("test.alpha");
        let beta = ToolName::new("test.beta");
        let mut activation = ceiling.clone();
        activation.disable_tool(ToolName::new("test.alpha"));

        let outcome = dispatch_tools_command_bounded("reset", &known, &mut activation, &ceiling);

        assert_eq!(
            outcome,
            ToolsCommandOutcome::Confirmation(
                "reset all tool overrides to the session ceiling".to_string()
            )
        );
        assert!(
            activation.is_tool_enabled(&alpha),
            "ceiling allows test.alpha"
        );
        assert!(
            !activation.is_tool_enabled(&beta),
            "reset must not re-enable test.beta beyond the ceiling"
        );
    }

    /// T2: `reset test.beta` adopts the ceiling's state for that tool — the
    /// override that disables it (from the ceiling itself) must not be cleared
    /// into the `Full` profile default.
    #[test]
    fn test_dispatch_tools_command_bounded_reset_one_keeps_ceiling_disabled_tool_off() {
        let known = both_known();
        let ceiling = full_ceiling_without_beta();
        let beta = ToolName::new("test.beta");
        let mut activation = ceiling.clone();

        let outcome =
            dispatch_tools_command_bounded("reset test.beta", &known, &mut activation, &ceiling);

        assert_eq!(
            outcome,
            ToolsCommandOutcome::Confirmation(
                "reset test.beta to session ceiling (off)".to_string()
            )
        );
        assert!(
            !activation.is_tool_enabled(&beta),
            "reset <name> must not re-enable test.beta beyond the ceiling"
        );
        assert!(activation.is_tool_enabled(&ToolName::new("test.alpha")));
    }

    /// T2: after a full `reset` followed by `reset test.beta`, `test.beta`
    /// is still off, and `reset test.alpha` restores a disabled but allowed
    /// tool to on.
    #[test]
    fn test_dispatch_tools_command_bounded_reset_then_reset_one_stays_within_ceiling() {
        let known = both_known();
        let ceiling = full_ceiling_without_beta();
        let alpha = ToolName::new("test.alpha");
        let beta = ToolName::new("test.beta");
        let mut activation = SessionActivation::new(ToolProfile::Full);
        activation.disable_tool(ToolName::new("test.alpha"));

        let first = dispatch_tools_command_bounded("reset", &known, &mut activation, &ceiling);
        let second =
            dispatch_tools_command_bounded("reset test.beta", &known, &mut activation, &ceiling);
        activation.disable_tool(ToolName::new("test.alpha"));
        let third =
            dispatch_tools_command_bounded("reset test.alpha", &known, &mut activation, &ceiling);

        assert!(matches!(first, ToolsCommandOutcome::Confirmation(_)));
        assert!(matches!(second, ToolsCommandOutcome::Confirmation(_)));
        assert_eq!(
            third,
            ToolsCommandOutcome::Confirmation(
                "reset test.alpha to session ceiling (on)".to_string()
            )
        );
        assert!(!activation.is_tool_enabled(&beta), "test.beta stays off");
        assert!(
            activation.is_tool_enabled(&alpha),
            "test.alpha restored to on"
        );
    }

    /// T3/T9(b): with a `Minimal` + allowlist ceiling, `on <tool outside>` is
    /// rejected with the `BeyondCeiling` message.
    #[test]
    fn test_dispatch_tools_command_bounded_on_outside_ceiling_is_beyond_ceiling() {
        let known = both_known();
        let ceiling = bounded_ceiling();
        let mut activation = ceiling.clone();
        let expected = crate::runtime_commands::ToolToggleError::BeyondCeiling {
            name: "test.beta".to_string(),
        }
        .to_string();

        let outcome =
            dispatch_tools_command_bounded("on test.beta", &known, &mut activation, &ceiling);

        assert_eq!(outcome, ToolsCommandOutcome::Error(expected));
        assert!(!activation.is_tool_enabled(&ToolName::new("test.beta")));
    }

    /// T7/T9(b): `profile full` under a `Minimal` + allowlist ceiling stays a
    /// subset of the ceiling for every registered tool, and the confirmation
    /// names the effective profile and the withheld tool instead of claiming
    /// "profile set to full".
    #[test]
    fn test_dispatch_tools_command_bounded_profile_full_reports_withheld_tools() {
        let known = both_known();
        let ceiling = bounded_ceiling();
        let mut activation = SessionActivation::new(ToolProfile::Minimal);

        let outcome =
            dispatch_tools_command_bounded("profile full", &known, &mut activation, &ceiling);

        assert_eq!(
            outcome,
            ToolsCommandOutcome::Confirmation(
                "profile full limited by session ceiling: effective profile Minimal, \
                 withheld: test.beta"
                    .to_string()
            )
        );
        for (name, _) in &known {
            let tool = ToolName::new(name.as_str());
            assert!(
                !activation.is_tool_enabled(&tool) || ceiling.is_tool_enabled(&tool),
                "{name} must not be enabled beyond the ceiling"
            );
        }
        assert!(activation.is_tool_enabled(&ToolName::new("test.alpha")));
    }
}
