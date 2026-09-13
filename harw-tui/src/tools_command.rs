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
//! as `&mut`. The [`harw_extension_api::ExtensionRegistry`] is read-only — only
//! the activation state is mutated. Spec source: harw-tui design, tools-command
//! section.
//!
//! # Concurrency
//! Pure synchronous function; no locking. The caller (TUI run-loop) owns both
//! `registry` and `activation` exclusively while this runs.
//!
//! # Examples
//! ```rust,no_run
//! use harw_tui::tools_command::{handle_tools_command, ToolsCommandOutcome};
//! use harw_core::activation::{SessionActivation, ToolProfile};
//! use harw_extension_api::registry::empty_extension_registry;
//!
//! let registry = empty_extension_registry();
//! let mut activation = SessionActivation::new(ToolProfile::Full);
//! let outcome = handle_tools_command("", &registry, &mut activation);
//! assert!(matches!(outcome, ToolsCommandOutcome::Listing(_)));
//! ```

use harw_core::activation::{SessionActivation, ToolProfile};
use harw_extension_api::{ExtensionRegistry, ToolName};

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

/// Parses the arg-string of a `/tools` command and dispatches to the
/// appropriate sub-handler, mutating `activation` in place for
/// state-changing sub-commands.
///
/// # Description
/// `args` is everything after `/tools` (i.e. the remainder of the input
/// line, already stripped of the leading `/tools` token). The function
/// trims whitespace and splits on ASCII whitespace to identify the
/// sub-command and any trailing argument.
///
/// Evaluation table:
///
/// | `args` (trimmed)         | Sub-command      |
/// |--------------------------|------------------|
/// | `""`                     | list all tools   |
/// | `"on <name>"`            | enable tool      |
/// | `"off <name>"`           | disable tool     |
/// | `"reset"`                | reset all        |
/// | `"reset <name>"`         | reset one tool   |
/// | `"profile <p>"`          | switch profile   |
/// | anything else            | usage error      |
///
/// # Arguments
/// - `args` (`&str`): the raw arg string after `/tools`, may be empty.
/// - `registry` (`&ExtensionRegistry`): read-only view of all registered
///   tool providers; used only for listing.
/// - `activation` (`&mut SessionActivation`): current session activation
///   state; mutated by `on`, `off`, `reset`, and `profile` sub-commands.
///
/// # Returns
/// A [`ToolsCommandOutcome`] describing what happened.
///
/// # Examples
/// ```rust,no_run
/// use harw_tui::tools_command::{handle_tools_command, ToolsCommandOutcome};
/// use harw_core::activation::{SessionActivation, ToolProfile};
/// use harw_extension_api::registry::empty_extension_registry;
///
/// let registry = empty_extension_registry();
/// let mut activation = SessionActivation::new(ToolProfile::Coding);
///
/// // List tools
/// let out = handle_tools_command("", &registry, &mut activation);
/// assert!(matches!(out, ToolsCommandOutcome::Listing(_)));
///
/// // Switch profile
/// let out = handle_tools_command("profile minimal", &registry, &mut activation);
/// assert_eq!(out, ToolsCommandOutcome::Confirmation("profile set to minimal".to_string()));
/// assert_eq!(activation.profile(), ToolProfile::Minimal);
/// ```
pub fn handle_tools_command(
    args: &str,
    registry: &ExtensionRegistry,
    activation: &mut SessionActivation,
) -> ToolsCommandOutcome {
    let trimmed = args.trim();
    let parts: Vec<&str> = if trimmed.is_empty() {
        Vec::new()
    } else {
        trimmed.split_whitespace().collect()
    };

    match parts.as_slice() {
        [] => list_tools(registry, activation),
        ["on", name] => set_tool(activation, name, true),
        ["off", name] => set_tool(activation, name, false),
        ["reset"] => reset_all(activation),
        ["reset", name] => reset_one(activation, name),
        ["profile", p] => set_profile(activation, p),
        _ => ToolsCommandOutcome::Error(
            "usage: /tools | /tools on <name> | /tools off <name> \
             | /tools reset [<name>] | /tools profile <minimal|coding|full>"
                .to_string(),
        ),
    }
}

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
/// For sub-commands that do not need the snapshot (`on`, `off`, `reset`,
/// `profile`), the snapshot is ignored. For the list sub-command (`args == ""`),
/// the snapshot is used to build the listing output.
///
/// Veraltet ab W2d-2: diese Funktion prüft keine Basis-Grenze — `on <tool>`
/// kann die Basis-Aktivierung der Session überschreiten und `profile <name>`
/// wird nicht auf die Basis geschnitten. Aufrufer wechseln zu
/// [`dispatch_tools_command_bounded`]. Kein `#[deprecated]`, da das in
/// `app.rs` clippy `-D warnings` brechen würde, solange die Migration (Welle
/// D5) dort nicht nachgezogen ist.
///
/// # Arguments
/// - `args` (`&str`): everything after `/tools` in the user's input (trimmed by caller).
/// - `tool_snapshot` (`&[(String, bool)]`): `(tool_name, is_enabled)` pairs
///   collected from the registry while holding a shared session borrow.
/// - `activation` (`&mut SessionActivation`): current session activation;
///   mutated by state-changing sub-commands.
///
/// # Returns
/// A [`ToolsCommandOutcome`] ready for rendering.
pub fn dispatch_tools_command(
    args: &str,
    tool_snapshot: &[(String, bool)],
    activation: &mut SessionActivation,
) -> ToolsCommandOutcome {
    let trimmed = args.trim();
    let parts: Vec<&str> = if trimmed.is_empty() {
        Vec::new()
    } else {
        trimmed.split_whitespace().collect()
    };

    match parts.as_slice() {
        [] => list_from_snapshot(tool_snapshot, activation),
        ["on", name] => set_tool(activation, name, true),
        ["off", name] => set_tool(activation, name, false),
        ["reset"] => reset_all(activation),
        ["reset", name] => reset_one(activation, name),
        ["profile", p] => set_profile(activation, p),
        _ => ToolsCommandOutcome::Error(
            "usage: /tools | /tools on <name> | /tools off <name> \
             | /tools reset [<name>] | /tools profile <minimal|coding|full>"
                .to_string(),
        ),
    }
}

/// Dispatches a `/tools` command the same way as [`dispatch_tools_command`],
/// but never lets the session's activation exceed `base` (Welle W2d-1/B,
/// Befund `w4-tui-control.md` §5).
///
/// # Description
/// Parses `args` identically to [`dispatch_tools_command`]. Before applying
/// `on <tool>` or `off <tool>`, the toggle is checked with
/// [`crate::runtime_commands::validate_tool_toggle`] against `base` and
/// `tool_snapshot`: an unknown tool name, or an attempt to enable a tool the
/// base activation forbids, is rejected with
/// [`ToolsCommandOutcome::Error`] and `activation` is left untouched. After
/// `profile <name>` applies the new profile, the resulting activation is cut
/// down to `base` via [`SessionActivation::intersect`] so a profile switch
/// can never grant more than the session's base activation allows.
///
/// `reset` and `reset <name>` behave exactly as in [`dispatch_tools_command`]
/// — they only revert per-tool overrides made through this same command and
/// can never move `activation` beyond `base`, because every prior mutation
/// through this function was already bounded by `base`.
///
/// # Arguments
/// - `args` (`&str`): everything after `/tools` in the user's input.
/// - `tool_snapshot` (`&[(String, bool)]`): `(tool_name, is_enabled)` pairs
///   collected from the registry while holding a shared session borrow; also
///   used as the known-tool set for toggle validation.
/// - `activation` (`&mut SessionActivation`): current session activation;
///   mutated only when the requested change is within `base`.
/// - `base` (`&SessionActivation`): the session's base activation
///   (`AgentSession::base_activation`), the ceiling `activation` may never
///   exceed.
///
/// # Returns
/// A [`ToolsCommandOutcome`] ready for rendering.
///
/// # Errors
/// Returns [`ToolsCommandOutcome::Error`] (rendered via `into_lines`) when
/// `on`/`off` names an unregistered tool, when `on` would enable a tool
/// beyond `base`, when `profile` names an unknown profile, or when the
/// sub-command is unrecognized. No mutation occurs in the error case.
#[must_use]
pub fn dispatch_tools_command_bounded(
    args: &str,
    tool_snapshot: &[(String, bool)],
    activation: &mut SessionActivation,
    base: &SessionActivation,
) -> ToolsCommandOutcome {
    let trimmed = args.trim();
    let parts: Vec<&str> = if trimmed.is_empty() {
        Vec::new()
    } else {
        trimmed.split_whitespace().collect()
    };

    match parts.as_slice() {
        [] => list_from_snapshot(tool_snapshot, activation),
        ["on", name] => bounded_set_tool(activation, base, tool_snapshot, name, true),
        ["off", name] => bounded_set_tool(activation, base, tool_snapshot, name, false),
        ["reset"] => reset_all(activation),
        ["reset", name] => reset_one(activation, name),
        ["profile", p] => bounded_set_profile(activation, base, p),
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

/// Lists all registered tools with their current on/off state.
///
/// # Description
/// Iterates over every [`ToolProvider`] in `registry`, calls `.tools()` on
/// each, extracts the tool name from the [`ToolSpec`], and checks
/// `activation.is_tool_enabled` to format the status marker. A profile
/// header line is always prepended.
///
/// # Returns
/// [`ToolsCommandOutcome::Listing`] with one entry per tool (plus header).
fn list_tools(registry: &ExtensionRegistry, activation: &SessionActivation) -> ToolsCommandOutcome {
    let mut lines = Vec::new();
    lines.push(format!("profile: {:?}", activation.profile()));

    let mut any = false;
    for provider in registry.tool_providers() {
        for spec in provider.tools() {
            any = true;
            let name = spec.name();
            let mark = if activation.is_tool_enabled(&ToolName::new(name)) {
                "on "
            } else {
                "off"
            };
            lines.push(format!("  [{}] {}", mark, name));
        }
    }

    if !any {
        lines.push("  (no tools registered)".to_string());
    }

    ToolsCommandOutcome::Listing(lines)
}

/// Lists tools from a pre-collected `(name, enabled)` snapshot.
///
/// # Description
/// Used by [`dispatch_tools_command`] when the caller cannot hold a shared
/// registry borrow alongside a mutable activation borrow (split-borrow
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

/// Validates and applies a single `on <tool>` / `off <tool>` toggle against a
/// base activation ceiling.
///
/// # Description
/// Delegates the check to
/// [`crate::runtime_commands::validate_tool_toggle`]. On `Err`, `activation`
/// is left untouched and the error's `Display` message is returned as
/// [`ToolsCommandOutcome::Error`]. On `Ok`, delegates to [`set_tool`] to apply
/// the change and build the confirmation message.
///
/// # Arguments
/// - `activation` — session activation to mutate on success.
/// - `base` — ceiling the toggle may never exceed.
/// - `known_tools` — `(name, enabled)` pairs used to validate `name`.
/// - `name` — raw tool-name string from the command line.
/// - `enable` — `true` for `on`, `false` for `off`.
fn bounded_set_tool(
    activation: &mut SessionActivation,
    base: &SessionActivation,
    known_tools: &[(String, bool)],
    name: &str,
    enable: bool,
) -> ToolsCommandOutcome {
    if let Err(err) = crate::runtime_commands::validate_tool_toggle(base, known_tools, name, enable) {
        return ToolsCommandOutcome::Error(err.to_string());
    }
    set_tool(activation, name, enable)
}

/// Applies a `profile <name>` switch, then cuts the resulting activation down
/// to `base` so the new profile can never grant more than the session's base
/// activation allows.
///
/// # Description
/// Parses `p` exactly like [`set_profile`]. On success, applies the profile
/// to `activation` and replaces `activation` with
/// `activation.intersect(base)` ([`SessionActivation::intersect`]) before
/// returning the confirmation — an unknown profile name leaves `activation`
/// untouched.
///
/// # Arguments
/// - `activation` — session activation to mutate.
/// - `base` — ceiling the resulting activation is intersected with.
/// - `p` — profile name: `"minimal"`, `"coding"`, or `"full"`.
///
/// # Errors
/// Returns [`ToolsCommandOutcome::Error`] when `p` does not match any known
/// profile name; `activation` is not mutated in that case.
fn bounded_set_profile(
    activation: &mut SessionActivation,
    base: &SessionActivation,
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
    activation.set_profile(profile);
    *activation = activation.intersect(base);
    ToolsCommandOutcome::Confirmation(format!("profile set to {p}"))
}

/// Resets all per-tool overrides by replacing the activation with a fresh
/// one that has the same profile.
///
/// # Description
/// Constructs a new [`SessionActivation`] from the current profile, which
/// clears all `tools_disabled` and `tools_enabled_extra` entries. Also
/// clears instruction and context overrides (they were empty per-tool-only
/// use). For a full reset the profile itself is preserved.
///
/// # Arguments
/// - `activation` — session activation to replace in-place.
fn reset_all(activation: &mut SessionActivation) -> ToolsCommandOutcome {
    let profile = activation.profile();
    *activation = SessionActivation::new(profile);
    ToolsCommandOutcome::Confirmation("reset all tool overrides".to_string())
}

/// Resets overrides for a single named tool.
///
/// # Description
/// Calls [`SessionActivation::reset_tool`] for the named tool. After the
/// call the tool reverts to profile-only evaluation.
///
/// # Arguments
/// - `activation` — session activation to mutate.
/// - `name` — raw tool-name string from the command line.
fn reset_one(activation: &mut SessionActivation, name: &str) -> ToolsCommandOutcome {
    activation.reset_tool(&ToolName::new(name.to_string()));
    ToolsCommandOutcome::Confirmation(format!("reset {name}"))
}

/// Switches the active tool profile.
///
/// # Description
/// Parses `p` (case-insensitive) into a [`ToolProfile`] variant and calls
/// [`SessionActivation::set_profile`]. Per-tool overrides that were set
/// before the profile change are preserved — they are re-evaluated against
/// the new profile on the next [`SessionActivation::is_tool_enabled`] call.
///
/// # Arguments
/// - `activation` — session activation to mutate.
/// - `p` — profile name: `"minimal"`, `"coding"`, or `"full"`.
///
/// # Errors
/// Returns [`ToolsCommandOutcome::Error`] when `p` does not match any known
/// profile name.
fn set_profile(activation: &mut SessionActivation, p: &str) -> ToolsCommandOutcome {
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
    activation.set_profile(profile);
    ToolsCommandOutcome::Confirmation(format!("profile set to {p}"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use harw_extension_api::contributors::ToolProvider;
    use harw_extension_api::registry::ExtensionRegistryBuilder;
    use harw_extension_api::{ToolExecutor, ToolName, ToolSpec};
    // `harw_tools` is a [dev-dependencies] entry added specifically for tests;
    // it is not a production dependency of harw-tui.
    use harw_tools::{FunctionToolSpec, JsonSchema};
    use std::sync::Arc;

    // ── Minimal in-test ToolProvider ─────────────────────────────────────────

    /// Fake provider with two tools: `"test.alpha"` and `"test.beta"`.
    ///
    /// Used to exercise the listing and toggle sub-commands without depending
    /// on any real filesystem or shell provider.
    struct FakeToolProvider;

    /// Constructs a `ToolSpec::Function` for the given tool name with an empty
    /// parameter schema — sufficient for listing and activation tests.
    fn make_tool_spec(name: &str) -> ToolSpec {
        ToolSpec::Function(FunctionToolSpec {
            name: ToolName::new(name),
            description: format!("{name} description"),
            parameters: JsonSchema::default(),
            strict: false,
        })
    }

    impl ToolProvider for FakeToolProvider {
        fn tools(&self) -> Vec<ToolSpec> {
            vec![make_tool_spec("test.alpha"), make_tool_spec("test.beta")]
        }

        fn executor(&self, _name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
            None
        }
    }

    /// Builds a registry containing the two fake tools.
    fn registry_with_tools() -> ExtensionRegistry {
        ExtensionRegistryBuilder::default()
            .tool_provider(Arc::new(FakeToolProvider))
            .build()
    }

    /// Builds an empty registry (no tool providers).
    fn empty_registry() -> ExtensionRegistry {
        ExtensionRegistryBuilder::default().build()
    }

    fn full_activation() -> SessionActivation {
        SessionActivation::new(ToolProfile::Full)
    }

    /// Base activation for the `_bounded` tests: `Minimal` profile with only
    /// `test.alpha` explicitly enabled — `test.beta` stays forbidden.
    fn bounded_base() -> SessionActivation {
        let mut base = SessionActivation::new(ToolProfile::Minimal);
        base.enable_tool(ToolName::new("test.alpha"));
        base
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    /// `/tools` with no registered tools yields the profile line + "(no tools registered)".
    #[test]
    fn list_returns_profile_line_when_no_tools() {
        let registry = empty_registry();
        let mut activation = full_activation();

        let outcome = handle_tools_command("", &registry, &mut activation);

        let ToolsCommandOutcome::Listing(lines) = outcome else {
            panic!("expected Listing");
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
    }

    /// `/tools` with two registered tools lists both with on/off status markers.
    #[test]
    fn list_shows_registered_tools_with_status() {
        let registry = registry_with_tools();
        let mut activation = full_activation();

        let outcome = handle_tools_command("", &registry, &mut activation);

        let ToolsCommandOutcome::Listing(lines) = outcome else {
            panic!("expected Listing");
        };
        assert!(
            lines.iter().any(|l| l.contains("test.alpha")),
            "alpha should appear"
        );
        assert!(
            lines.iter().any(|l| l.contains("test.beta")),
            "beta should appear"
        );
        // Full profile → both tools enabled
        assert!(
            lines.iter().filter(|l| l.contains("[on ]")).count() == 2,
            "both tools should be on under Full profile"
        );
    }

    /// `/tools on <name>` enables a tool that was disabled.
    #[test]
    fn on_enables_disabled_tool() {
        let registry = registry_with_tools();
        let mut activation = full_activation();
        activation.disable_tool(ToolName::new("test.alpha"));
        assert!(!activation.is_tool_enabled(&ToolName::new("test.alpha")));

        let outcome = handle_tools_command("on test.alpha", &registry, &mut activation);

        assert_eq!(
            outcome,
            ToolsCommandOutcome::Confirmation("enabled test.alpha".to_string())
        );
        assert!(activation.is_tool_enabled(&ToolName::new("test.alpha")));
    }

    /// `/tools off <name>` disables a tool that was enabled.
    #[test]
    fn off_disables_tool() {
        let registry = registry_with_tools();
        let mut activation = full_activation();
        assert!(activation.is_tool_enabled(&ToolName::new("test.beta")));

        let outcome = handle_tools_command("off test.beta", &registry, &mut activation);

        assert_eq!(
            outcome,
            ToolsCommandOutcome::Confirmation("disabled test.beta".to_string())
        );
        assert!(!activation.is_tool_enabled(&ToolName::new("test.beta")));
    }

    /// `/tools reset` clears all overrides for all tools.
    #[test]
    fn reset_all_reverts_overrides() {
        let registry = registry_with_tools();
        let mut activation = full_activation();
        activation.disable_tool(ToolName::new("test.alpha"));
        activation.disable_tool(ToolName::new("test.beta"));
        assert!(!activation.is_tool_enabled(&ToolName::new("test.alpha")));

        let outcome = handle_tools_command("reset", &registry, &mut activation);

        assert_eq!(
            outcome,
            ToolsCommandOutcome::Confirmation("reset all tool overrides".to_string())
        );
        // Full profile restored → tools enabled again
        assert!(activation.is_tool_enabled(&ToolName::new("test.alpha")));
        assert!(activation.is_tool_enabled(&ToolName::new("test.beta")));
        // Profile itself must be preserved
        assert_eq!(activation.profile(), ToolProfile::Full);
    }

    /// `/tools reset <name>` reverts only the named tool's override.
    #[test]
    fn reset_one_reverts_single_tool() {
        let registry = registry_with_tools();
        let mut activation = full_activation();
        activation.disable_tool(ToolName::new("test.alpha"));
        activation.disable_tool(ToolName::new("test.beta"));

        let outcome = handle_tools_command("reset test.alpha", &registry, &mut activation);

        assert_eq!(
            outcome,
            ToolsCommandOutcome::Confirmation("reset test.alpha".to_string())
        );
        // alpha reverted to profile default (Full → enabled)
        assert!(activation.is_tool_enabled(&ToolName::new("test.alpha")));
        // beta still explicitly disabled
        assert!(!activation.is_tool_enabled(&ToolName::new("test.beta")));
    }

    /// `/tools profile coding` changes the profile to Coding.
    #[test]
    fn profile_switch_changes_activation_profile() {
        let registry = empty_registry();
        let mut activation = full_activation();
        assert_eq!(activation.profile(), ToolProfile::Full);

        let outcome = handle_tools_command("profile coding", &registry, &mut activation);

        assert_eq!(
            outcome,
            ToolsCommandOutcome::Confirmation("profile set to coding".to_string())
        );
        assert_eq!(activation.profile(), ToolProfile::Coding);
    }

    /// `/tools profile <unknown>` returns an error variant.
    #[test]
    fn profile_switch_rejects_unknown_name() {
        let registry = empty_registry();
        let mut activation = full_activation();

        let outcome = handle_tools_command("profile turbo", &registry, &mut activation);

        assert!(
            matches!(outcome, ToolsCommandOutcome::Error(ref msg) if msg.contains("turbo")),
            "error should name the unknown profile"
        );
        // Profile must not have changed
        assert_eq!(activation.profile(), ToolProfile::Full);
    }

    /// An unrecognised sub-command returns a usage error.
    #[test]
    fn unknown_subcommand_returns_usage_error() {
        let registry = empty_registry();
        let mut activation = full_activation();

        let outcome = handle_tools_command("frobnicate", &registry, &mut activation);

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
        let base = bounded_base();
        let ghost = ToolName::new("test.ghost");
        assert!(activation.is_tool_enabled(&ghost), "Full profile defaults to visible");

        let outcome =
            dispatch_tools_command_bounded("off test.ghost", &known, &mut activation, &base);

        assert!(
            matches!(outcome, ToolsCommandOutcome::Error(ref msg) if msg.contains("test.ghost")),
            "unknown tool must be rejected by name"
        );
        assert!(
            activation.is_tool_enabled(&ghost),
            "rejected toggle must not mutate activation"
        );
    }

    /// `on <tool>` beyond what `base` allows is rejected, even though the
    /// tool is registered.
    #[test]
    fn test_dispatch_tools_command_bounded_enable_beyond_base_is_rejected() {
        let known = vec![
            ("test.alpha".to_string(), false),
            ("test.beta".to_string(), false),
        ];
        let mut activation = SessionActivation::new(ToolProfile::Minimal);
        let base = bounded_base();
        let beta = ToolName::new("test.beta");
        assert!(!activation.is_tool_enabled(&beta));

        let outcome =
            dispatch_tools_command_bounded("on test.beta", &known, &mut activation, &base);

        assert!(
            matches!(outcome, ToolsCommandOutcome::Error(ref msg) if msg.contains("test.beta")),
            "toggle beyond base must be rejected by name"
        );
        assert!(
            !activation.is_tool_enabled(&beta),
            "rejected enable must not mutate activation"
        );
    }

    /// Disabling a known tool always applies, regardless of `base` — turning
    /// a tool off only narrows, never exceeds, any ceiling.
    #[test]
    fn test_dispatch_tools_command_bounded_disable_known_tool_applies() {
        let known = vec![("test.alpha".to_string(), true)];
        let mut activation = full_activation();
        let base = bounded_base();
        let alpha = ToolName::new("test.alpha");
        assert!(activation.is_tool_enabled(&alpha));

        let outcome =
            dispatch_tools_command_bounded("off test.alpha", &known, &mut activation, &base);

        assert_eq!(
            outcome,
            ToolsCommandOutcome::Confirmation("disabled test.alpha".to_string())
        );
        assert!(!activation.is_tool_enabled(&alpha));
    }

    /// `profile full` would normally allow every tool, but the resulting
    /// activation must be cut down to `base` — only `test.alpha` (allowed by
    /// `base`) stays visible, `test.beta` does not.
    #[test]
    fn test_dispatch_tools_command_bounded_profile_is_cut_to_base() {
        let known: Vec<(String, bool)> = Vec::new();
        let mut activation = SessionActivation::new(ToolProfile::Minimal);
        let base = bounded_base();

        let outcome =
            dispatch_tools_command_bounded("profile full", &known, &mut activation, &base);

        assert_eq!(
            outcome,
            ToolsCommandOutcome::Confirmation("profile set to full".to_string())
        );
        assert!(
            activation.is_tool_enabled(&ToolName::new("test.alpha")),
            "base allows test.alpha, so the cut activation must still allow it"
        );
        assert!(
            !activation.is_tool_enabled(&ToolName::new("test.beta")),
            "base forbids test.beta, so `profile full` must not resurrect it"
        );
    }
}
