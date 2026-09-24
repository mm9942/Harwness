//! Central command lookup and admission gates.
//!
//! Source: `harw-tui` interaction-contract spec.

use crate::command::CommandOrigin;
use crate::{
    CommandDomain, CommandError, CommandResult, CommandScope, CommandSpec, Invocation,
    OutputSurface, PermissionTier,
};

// ── TuiRegistryError ──────────────────────────────────────────────────────────

/// Error returned by [`CommandRegistry::from_operation_registry`] when a collision
/// or unsupported configuration is detected while building the TUI command catalog.
///
/// # Description
/// Covers three categories of failure:
/// - Duplicate canonical command names or paths across operations.
/// - Alias collisions between any two command specs.
/// - Multi-segment command paths are accepted (stored as-is after stripping the
///   leading `/`), but if a future caller needs to reject them this variant is
///   available.
///
/// # Variants
/// - [`Self::DuplicateCommandName`]: Two `Surface::Command` surfaces produce the same
///   canonical name after stripping the leading `/`.
/// - [`Self::DuplicateCommandPath`]: Two `Surface::Command` surfaces share the same
///   raw `path` string.
/// - [`Self::AliasCollision`]: An alias from one operation shadows the name or an
///   alias of another.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TuiRegistryError {
    /// Two operations map to the same canonical command name.
    DuplicateCommandName {
        /// Canonical name (without `/`) that collides.
        name: String,
        /// Operation whose command surface first claimed the name.
        first_owner: String,
        /// Operation that tried to claim the same name.
        second_owner: String,
    },
    /// Two operations declare a `Surface::Command` with the same `path`.
    DuplicateCommandPath {
        /// The raw path string (e.g. `"/model"`).
        path: String,
        /// Operation that first declared this path.
        first_owner: String,
        /// Operation that tried to declare the same path again.
        second_owner: String,
    },
    /// An alias from one operation collides with the name or an alias of another.
    AliasCollision {
        /// The alias string that collides.
        alias: String,
        /// Operation that first owned the name/alias being shadowed.
        first_owner: String,
        /// Operation whose alias provoked the collision.
        second_owner: String,
    },
}

impl std::fmt::Display for TuiRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateCommandName {
                name,
                first_owner,
                second_owner,
            } => {
                write!(
                    f,
                    "duplicate command name '{name}': claimed by '{first_owner}' and '{second_owner}'"
                )
            }
            Self::DuplicateCommandPath {
                path,
                first_owner,
                second_owner,
            } => {
                write!(
                    f,
                    "duplicate command path '{path}': claimed by '{first_owner}' and '{second_owner}'"
                )
            }
            Self::AliasCollision {
                alias,
                first_owner,
                second_owner,
            } => {
                write!(
                    f,
                    "alias '{alias}' from '{second_owner}' collides with '{first_owner}'"
                )
            }
        }
    }
}

impl std::error::Error for TuiRegistryError {}

/// Surface on which an invocation entered the system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvocationSurface {
    Tui,
    Channel,
}

/// Capability needed by a prefix shortcut that can perform a privileged action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellCapability {
    /// The local `commands.shell` capability flag.
    CommandsShell,
    /// Explicit per-channel shell opt-in in addition to `CommandsShell`.
    ChannelShell,
}

/// Capability grants resolved from configuration and channel policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CapabilitySet {
    commands_shell: bool,
    channel_shell: bool,
}

impl CapabilitySet {
    #[must_use]
    pub const fn with(capability: ShellCapability) -> Self {
        match capability {
            ShellCapability::CommandsShell => Self {
                commands_shell: true,
                channel_shell: false,
            },
            ShellCapability::ChannelShell => Self {
                commands_shell: true,
                channel_shell: true,
            },
        }
    }

    #[must_use]
    pub const fn allows(self, capability: ShellCapability) -> bool {
        match capability {
            ShellCapability::CommandsShell => self.commands_shell,
            ShellCapability::ChannelShell => self.commands_shell && self.channel_shell,
        }
    }
}

/// Context shared by all centralized admission checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DispatchContext {
    pub caller_tier: PermissionTier,
    pub surface: InvocationSurface,
    pub capabilities: CapabilitySet,
}

/// An admitted action for a renderer/runtime to execute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandAction {
    Command(CommandSpec, Vec<String>),
    Shell(String),
    ShellRepeat,
    Note(String),
    Mention { target: String, body: String },
    Chat(String),
}

/// Static command catalog plus centralized dispatch policy.
#[derive(Clone, Debug, Default)]
pub struct CommandRegistry {
    specs: Vec<CommandSpec>,
}

impl CommandRegistry {
    #[must_use]
    pub fn new(specs: Vec<CommandSpec>) -> Self {
        Self { specs }
    }

    /// Builds a registry from all operations registered in an [`harw_operations::registry::OperationRegistry`].
    ///
    /// # Description
    /// Iterates over every [`harw_operations::operation::Operation`] held in `ops`. For each
    /// operation that declares at least one [`harw_operations::operation::Surface::Command`]
    /// surface, a corresponding [`CommandSpec`] is derived:
    ///
    /// - `name`: taken from the command `path` with the leading `/` stripped. Multi-segment
    ///   paths (e.g. `/session/list`) whose stripped form contains a `/` are **accepted** —
    ///   the full stripped path is stored in the spec. Paths that fail [`crate::CommandName`]
    ///   validation for other reasons (length, character set) are silently skipped.
    /// - `scope`: mapped from [`harw_operations::operation::CommandVisibility`].
    /// - `permission`: mapped from [`harw_operations::operation::PermissionTier`] (shared type via re-export).
    /// - `domain`: mapped from [`harw_operations::operation::OperationDomain`].
    /// - `output`: defaults to [`OutputSurface::Inline`].
    /// - `aliases`: pulled from [`harw_operations::operation::OperationMeta::aliases`] so that
    ///   short aliases like `"m"`, `"p"`, and `"reasoning"` are wired into the TUI catalog.
    /// - `busy`: copied from [`harw_operations::operation::OperationMeta::busy`] so that
    ///   [`CommandSpec::busy`] reflects whether the command may run immediately during a
    ///   busy turn or must wait for turn end (the default).
    ///
    /// # Collision detection
    /// Returns `Err(TuiRegistryError)` when:
    /// - Two surfaces produce the same canonical name (`DuplicateCommandName`).
    /// - Two surfaces share the same raw `path` string (`DuplicateCommandPath`).
    /// - An alias from one operation collides with the name or alias of another (`AliasCollision`).
    ///
    /// # Arguments
    /// - `ops` (`&harw_operations::registry::OperationRegistry`): A populated operation registry,
    ///   typically obtained by calling [`harw_ops::register_all`].
    ///
    /// # Returns
    /// - `Ok(CommandRegistry)`: One [`CommandSpec`] per `Surface::Command` declaration.
    /// - `Err(TuiRegistryError)`: A collision was detected in the operation set.
    ///
    /// # Errors
    /// - [`TuiRegistryError::DuplicateCommandName`]: two ops map to the same command name.
    /// - [`TuiRegistryError::DuplicateCommandPath`]: two ops declare the same surface path.
    /// - [`TuiRegistryError::AliasCollision`]: an alias collides with an existing name or alias.
    ///
    /// # Concurrency
    /// Requires only a shared borrow of `ops`; safe to call from any thread.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_operations::registry::OperationRegistry;
    /// use harw_tui::CommandRegistry;
    ///
    /// let mut ops = OperationRegistry::new();
    /// harw_ops::register_all(&mut ops);
    /// let registry = CommandRegistry::from_operation_registry(&ops).unwrap();
    /// assert!(registry.specs().len() >= 15);
    /// ```
    pub fn from_operation_registry(
        ops: &harw_operations::registry::OperationRegistry,
    ) -> Result<Self, TuiRegistryError> {
        let mut specs: Vec<CommandSpec> = Vec::new();
        // Track (canonical_name_lowercase, op_name) and (alias_lowercase, op_name)
        // so collision detection is O(n*m) but deterministic.
        // name_index: canonical_name_lowercase → op_meta_name
        let mut name_index: Vec<(String, String)> = Vec::new();
        // alias_index: alias_lowercase → op_meta_name
        let mut alias_index: Vec<(String, String)> = Vec::new();
        // path_index: raw path → op_meta_name
        let mut path_index: Vec<(String, String)> = Vec::new();

        for op in ops.iter() {
            let meta = op.meta();
            for surface in &meta.surfaces {
                if let harw_operations::operation::Surface::Command { path, visibility } = surface {
                    let raw_path = (*path).to_owned();
                    let name = path.trim_start_matches('/');
                    // Multi-segment paths (e.g. "session/list") are accepted; only
                    // truly invalid CommandName characters/length are skipped.
                    // We need to check if CommandName::parse accepts it; if not, skip.
                    let Ok(cmd_name) = crate::CommandName::parse(name.to_owned()) else {
                        continue;
                    };
                    let canonical = cmd_name.as_str().to_ascii_lowercase();
                    let op_name = meta.name.to_owned();

                    // Duplicate path check.
                    if let Some((_, prior_owner)) = path_index.iter().find(|(p, _)| *p == raw_path)
                    {
                        return Err(TuiRegistryError::DuplicateCommandPath {
                            path: raw_path.clone(),
                            first_owner: prior_owner.clone(),
                            second_owner: op_name,
                        });
                    }

                    // Duplicate canonical name check.
                    if let Some((_, prior_owner)) = name_index.iter().find(|(n, _)| *n == canonical)
                    {
                        return Err(TuiRegistryError::DuplicateCommandName {
                            name: cmd_name.as_str().to_owned(),
                            first_owner: prior_owner.clone(),
                            second_owner: op_name,
                        });
                    }

                    // Collect this op's aliases for collision checking.
                    let op_aliases: Vec<String> = meta
                        .aliases
                        .iter()
                        .map(|a| a.to_ascii_lowercase())
                        .collect();

                    // Check new canonical name against existing aliases.
                    if let Some((_, prior_owner)) =
                        alias_index.iter().find(|(a, _)| *a == canonical)
                    {
                        return Err(TuiRegistryError::AliasCollision {
                            alias: canonical.clone(),
                            first_owner: prior_owner.clone(),
                            second_owner: op_name,
                        });
                    }

                    // Check new aliases against existing names and aliases.
                    for alias in &op_aliases {
                        if let Some((_, prior_owner)) = name_index.iter().find(|(n, _)| n == alias)
                        {
                            return Err(TuiRegistryError::AliasCollision {
                                alias: alias.clone(),
                                first_owner: prior_owner.clone(),
                                second_owner: op_name.clone(),
                            });
                        }
                        if let Some((_, prior_owner)) = alias_index.iter().find(|(a, _)| a == alias)
                        {
                            return Err(TuiRegistryError::AliasCollision {
                                alias: alias.clone(),
                                first_owner: prior_owner.clone(),
                                second_owner: op_name.clone(),
                            });
                        }
                    }

                    let scope = map_visibility(*visibility);
                    let permission = meta.permission;
                    let domain = map_domain(meta.domain);
                    let output = OutputSurface::Inline;

                    if let Ok(mut cmd_spec) = CommandSpec::new(
                        cmd_name.as_str(),
                        meta.aliases.iter().copied(),
                        scope,
                        permission,
                        output,
                        domain,
                    ) {
                        cmd_spec.busy = meta.busy;
                        cmd_spec.summary = meta.summary.to_owned();
                        crate::command_catalog::enrich(&mut cmd_spec);
                        // Record in indexes after successful spec construction.
                        path_index.push((raw_path.clone(), op_name.clone()));
                        name_index.push((canonical.clone(), op_name.clone()));
                        for alias in &op_aliases {
                            alias_index.push((alias.clone(), op_name.clone()));
                        }
                        specs.push(cmd_spec);
                    }
                }
            }
        }

        Ok(Self::new(specs))
    }

    /// Builds the built-in command registry driven by [`harw_ops::register_all`].
    ///
    /// # Description
    /// Creates a fresh [`harw_operations::registry::OperationRegistry`], populates it
    /// via [`harw_ops::register_all`] (17 operations as of the initial op-set), and
    /// delegates to [`Self::from_operation_registry`] to derive [`CommandSpec`] entries
    /// for every `Surface::Command` declaration found. This replaces the former
    /// hard-coded list of 8 specs and keeps the TUI's known-command set in sync with
    /// the runtime's dispatch table automatically.
    ///
    /// Callers receive a `Result`: collisions in the built-in op-set are reported as
    /// [`TuiRegistryError`] instead of panicking (Bible R087/R165 — no panics on a
    /// data-dependent failure path, even one that "must not" occur by construction).
    ///
    /// # Returns
    /// - `Ok(CommandRegistry)`: one [`CommandSpec`] per `Surface::Command` declaration
    ///   across all built-in operations, with aliases wired in.
    /// - `Err(TuiRegistryError)`: a collision was detected in the built-in op-set — by
    ///   construction this must not happen, and an `Err` here indicates a programming
    ///   error in `harw-ops`.
    ///
    /// # Errors
    /// - [`TuiRegistryError::DuplicateCommandName`] / [`TuiRegistryError::DuplicateCommandPath`] /
    ///   [`TuiRegistryError::AliasCollision`]: propagated unchanged from
    ///   [`Self::from_operation_registry`].
    ///
    /// # Concurrency
    /// The underlying [`harw_operations::registry::OperationRegistry`] is a short-lived
    /// local value; this function is safe to call from any thread.
    ///
    /// # Examples
    /// ```rust
    /// use harw_tui::CommandRegistry;
    ///
    /// let registry = CommandRegistry::built_in()?;
    /// assert!(registry.specs().len() >= 15);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn built_in() -> Result<Self, TuiRegistryError> {
        let mut ops = harw_operations::registry::OperationRegistry::new();
        harw_ops::register_all(&mut ops);
        Self::from_operation_registry(&ops)
    }

    /// Builds the dispatch catalog directly from the TUI's command adapters.
    ///
    /// # Description
    /// Used by `command_exec::execute_command_as` so that admission (tier, scope,
    /// shell capability, unknown command with suggestion) runs exclusively through
    /// [`Self::dispatch`] (Befund w4-tui-control C1). One [`CommandSpec`] is derived
    /// per adapter:
    ///
    /// - `name`: the adapter `path` without its single leading `/`. Paths without a
    ///   leading `/` or with an invalid [`crate::CommandName`] are skipped, because
    ///   the former path lookup (`adapter.path() == "/{name}"`) could never reach them.
    /// - `permission`: [`harw_operations::adapter::CommandAdapter::permission`], i.e. the
    ///   tier copied from the operation when the adapter was registered.
    /// - `scope`: mapped from [`harw_operations::adapter::CommandAdapter::visibility`].
    /// - `aliases` / `domain` / `busy`: read from the operation metadata exactly once per
    ///   adapter; aliases that are not valid command names are dropped (they were
    ///   unreachable).
    ///
    /// Unlike [`Self::from_operation_registry`], this constructor does not reject
    /// collisions: the first adapter claiming a canonical name wins, mirroring the
    /// former first-match adapter lookup. [`Self::find`] resolves canonical names
    /// before aliases, so an alias can never shadow a canonical command.
    ///
    /// # Arguments
    /// - `adapters` (`&[CommandAdapter]`): the `/`-command adapters of the TUI.
    ///
    /// # Returns
    /// A [`CommandRegistry`] with at most one spec per adapter, in adapter order.
    ///
    /// # Concurrency
    /// Requires only shared borrows; safe to call from any thread.
    #[must_use]
    pub(crate) fn from_command_adapters(
        adapters: &[harw_operations::adapter::CommandAdapter],
    ) -> Self {
        let mut specs: Vec<CommandSpec> = Vec::with_capacity(adapters.len());
        for adapter in adapters {
            let Some(stripped) = adapter.path().strip_prefix('/') else {
                tracing::debug!(
                    path = adapter.path(),
                    operation = adapter.operation_name(),
                    "command adapter path has no leading '/'; skipped"
                );
                continue;
            };
            let name = match crate::CommandName::parse(stripped) {
                Ok(name) => name,
                Err(error) => {
                    tracing::debug!(
                        path = adapter.path(),
                        operation = adapter.operation_name(),
                        %error,
                        "command adapter path is not a valid command name; skipped"
                    );
                    continue;
                }
            };
            if specs.iter().any(|spec| spec.name == name) {
                tracing::debug!(
                    path = adapter.path(),
                    operation = adapter.operation_name(),
                    "duplicate command adapter path; first adapter wins"
                );
                continue;
            }
            let meta = adapter.operation().meta();
            let aliases: Vec<String> = meta
                .aliases
                .iter()
                .filter(|alias| crate::CommandName::parse(**alias).is_ok())
                .map(|alias| (*alias).to_owned())
                .collect();
            let mut spec = CommandSpec {
                name,
                aliases,
                scope: map_visibility(adapter.visibility()),
                permission: adapter.permission(),
                output: OutputSurface::Inline,
                domain: map_domain(meta.domain),
                busy: meta.busy,
                summary: meta.summary.to_owned(),
                usage: String::new(),
                subcommands: Vec::new(),
                origin: CommandOrigin::Operation,
            };
            crate::command_catalog::enrich(&mut spec);
            specs.push(spec);
        }
        Self::new(specs)
    }

    /// Mischt TUI-lokale Spezifikationen in den Katalog ein.
    ///
    /// # Beschreibung
    /// Jede lokale Spezifikation wird hinten angehängt, sofern weder ihr Name
    /// noch einer ihrer Aliase mit dem Namen oder einem Alias eines bereits
    /// vorhandenen Eintrags kollidiert (Vergleich ohne Groß-/Kleinschreibung).
    /// Bei Kollision gewinnt der vorhandene Eintrag — Operationen haben also
    /// immer Vorrang, und von zwei gleichnamigen lokalen Einträgen bleibt der
    /// erste. Verworfene Einträge werden nur per `tracing::debug!` gemeldet.
    ///
    /// # Argumente
    /// - `locals`: typischerweise `command_catalog::local_command_specs()`.
    ///
    /// # Rückgabe
    /// Der erweiterte Katalog (Builder-Stil).
    #[must_use]
    pub(crate) fn with_local_specs(mut self, locals: Vec<CommandSpec>) -> Self {
        for local in locals {
            let mut keys = std::iter::once(local.name.as_str())
                .chain(local.aliases.iter().map(String::as_str));
            let collides = keys.any(|key| {
                self.specs.iter().any(|existing| {
                    existing.name.as_str().eq_ignore_ascii_case(key)
                        || existing
                            .aliases
                            .iter()
                            .any(|alias| alias.eq_ignore_ascii_case(key))
                })
            });
            if collides {
                tracing::debug!(
                    command = local.name.as_str(),
                    "local command spec collides with an existing command; existing wins"
                );
                continue;
            }
            self.specs.push(local);
        }
        self
    }

    /// Looks up a command by canonical name, falling back to aliases.
    ///
    /// Canonical names take precedence over aliases across the whole catalog, so a
    /// malformed catalog cannot let an earlier alias shadow a later canonical command
    /// and thereby change its permission boundary.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&CommandSpec> {
        self.specs
            .iter()
            .find(|spec| spec.name.as_str() == name)
            .or_else(|| {
                self.specs
                    .iter()
                    .find(|spec| spec.aliases.iter().any(|alias| alias == name))
            })
    }

    /// Returns all registered command specifications, in registration order.
    #[must_use]
    pub fn specs(&self) -> &[CommandSpec] {
        &self.specs
    }

    /// Performs all admission checks before handing the action to a runtime.
    ///
    /// # Description
    /// Single admission point for TUI and channel invocations:
    /// - unknown command → [`CommandError::UnknownCommand`] with a typo suggestion;
    /// - caller tier below the command's [`CommandSpec::permission`] (the operation's
    ///   declared tier) → [`CommandError::PermissionDenied`];
    /// - `TuiOnly` command from a channel → [`CommandError::TuiOnlyCommand`];
    /// - shell shortcuts below `Operator` or without the surface-appropriate
    ///   capability → [`CommandError::PermissionDenied`] / [`CommandError::CapabilityDenied`].
    ///
    /// # Errors
    /// See the variants listed above.
    pub fn dispatch(
        &self,
        context: DispatchContext,
        invocation: Invocation,
    ) -> CommandResult<CommandAction> {
        match invocation {
            Invocation::Command { name, raw_args } => {
                let spec = self
                    .find(&name)
                    .ok_or_else(|| CommandError::UnknownCommand {
                        suggestion: self.suggestion(&name),
                        input: name,
                    })?;
                self.check_command(context, spec)?;
                Ok(CommandAction::Command(spec.clone(), raw_args))
            }
            Invocation::Shell(command) => {
                self.check_shell(context)?;
                Ok(CommandAction::Shell(command))
            }
            Invocation::ShellRepeat => {
                self.check_shell(context)?;
                Ok(CommandAction::ShellRepeat)
            }
            Invocation::Note(note) => Ok(CommandAction::Note(note)),
            Invocation::Mention { target, body } => Ok(CommandAction::Mention { target, body }),
            Invocation::Chat(text) => Ok(CommandAction::Chat(text)),
        }
    }

    fn check_command(&self, context: DispatchContext, spec: &CommandSpec) -> CommandResult<()> {
        if context.caller_tier < spec.permission {
            return Err(CommandError::PermissionDenied {
                command: spec.name.as_str().to_owned(),
                required: spec.permission,
                actual: context.caller_tier,
            });
        }

        if context.surface == InvocationSurface::Channel {
            match spec.scope {
                CommandScope::TuiOnly => {
                    return Err(CommandError::TuiOnlyCommand {
                        command: spec.name.as_str().to_owned(),
                    });
                }
                CommandScope::ChannelReduced | CommandScope::ChannelParity => {}
            }
        }
        Ok(())
    }

    fn check_shell(&self, context: DispatchContext) -> CommandResult<()> {
        if context.caller_tier < PermissionTier::Operator {
            return Err(CommandError::PermissionDenied {
                command: "!".to_owned(),
                required: PermissionTier::Operator,
                actual: context.caller_tier,
            });
        }

        let capability = match context.surface {
            InvocationSurface::Tui => ShellCapability::CommandsShell,
            InvocationSurface::Channel => ShellCapability::ChannelShell,
        };
        if context.capabilities.allows(capability) {
            Ok(())
        } else {
            Err(CommandError::CapabilityDenied {
                capability: match capability {
                    ShellCapability::CommandsShell => "commands.shell",
                    ShellCapability::ChannelShell => "channel.allow_shell",
                },
            })
        }
    }

    fn suggestion(&self, input: &str) -> Option<String> {
        let first = input.chars().next()?;
        self.specs
            .iter()
            .map(|spec| spec.name.as_str())
            .filter(|name| name.starts_with(first))
            .min_by_key(|name| name.len().abs_diff(input.len()))
            .map(str::to_owned)
    }
}

/// Maps a [`harw_operations::operation::CommandVisibility`] to the TUI-side [`CommandScope`].
///
/// # Description
/// Both enums carry identical semantics; this function converts between the two
/// without loss of information. See spec section "Surface mapping" in the TUI design doc.
///
/// # Arguments
/// - `v` (`harw_operations::operation::CommandVisibility`): Visibility declared by an operation.
///
/// # Returns
/// The matching [`CommandScope`] variant.
fn map_visibility(v: harw_operations::operation::CommandVisibility) -> CommandScope {
    use harw_operations::operation::CommandVisibility;
    match v {
        CommandVisibility::TuiOnly => CommandScope::TuiOnly,
        CommandVisibility::ChannelParity => CommandScope::ChannelParity,
        CommandVisibility::ChannelReduced => CommandScope::ChannelReduced,
    }
}

/// Maps an [`harw_operations::operation::OperationDomain`] to the TUI-side [`CommandDomain`].
///
/// # Description
/// The operation contract's `OperationDomain` is a superset-aligned enum. Variants that
/// have a direct counterpart are mapped 1-to-1. `OperationDomain::Session` maps to
/// `CommandDomain::SessionLifecycle` (the TUI uses the more descriptive name).
/// `OperationDomain::Agents` maps to `CommandDomain::AgentTopology` for the same reason.
/// Both mappings are documented in `assumptions_made` in the implementation report.
///
/// # Arguments
/// - `d` (`harw_operations::operation::OperationDomain`): Domain declared by an operation.
///
/// # Returns
/// The semantically closest [`CommandDomain`] variant.
fn map_domain(d: harw_operations::operation::OperationDomain) -> CommandDomain {
    use harw_operations::operation::OperationDomain;
    match d {
        OperationDomain::Session => CommandDomain::SessionLifecycle,
        OperationDomain::Agents => CommandDomain::AgentTopology,
        OperationDomain::Execution => CommandDomain::Execution,
        OperationDomain::CatalogConfig => CommandDomain::CatalogConfig,
        OperationDomain::Knowledge => CommandDomain::Knowledge,
        OperationDomain::Misc => CommandDomain::Misc,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify_input;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_operations::adapter::CommandAdapter;

    fn context(
        caller_tier: PermissionTier,
        surface: InvocationSurface,
        capabilities: CapabilitySet,
    ) -> DispatchContext {
        DispatchContext {
            caller_tier,
            surface,
            capabilities,
        }
    }

    /// Helper: build an OperationRegistry populated by `harw_ops::register_all`.
    fn ops_registry() -> harw_operations::registry::OperationRegistry {
        let mut ops = harw_operations::registry::OperationRegistry::new();
        harw_ops::register_all(&mut ops);
        ops
    }

    #[test]
    fn canonical_command_resolves_by_name() -> TestResult {
        // The "h" alias was present only in the old hard-coded built_in(). The
        // new implementation derives specs from the operation contract, which
        // carries no alias declarations. Verify the canonical name still resolves.
        let registry = CommandRegistry::built_in().map_err(ctx("built_in"))?;
        assert_eq!(
            registry.find("help").map(|spec| spec.name.as_str()),
            Some("help"),
            "help command must be findable by its canonical name"
        );
        Ok(())
    }

    #[test]
    fn registry_enforces_permission_before_dispatch() -> TestResult {
        let registry = CommandRegistry::built_in().map_err(ctx("built_in"))?;
        let invocation = classify_input("/new").map_err(ctx("classify_input"))?;
        let Err(error) = registry.dispatch(
            context(
                PermissionTier::Observer,
                InvocationSurface::Tui,
                CapabilitySet::default(),
            ),
            invocation,
        ) else {
            return Err(TestError::Unexpected(
                "dispatch below required tier must be rejected".into(),
            ));
        };

        assert!(matches!(
            error,
            CommandError::PermissionDenied { command, required: PermissionTier::Operator, .. }
                if command == "new"
        ));
        Ok(())
    }

    fn protected_spec(
        name: &str,
        alias: &str,
        permission: PermissionTier,
    ) -> TestResult<CommandSpec> {
        CommandSpec::new(
            name,
            [alias],
            CommandScope::TuiOnly,
            permission,
            OutputSurface::Inline,
            CommandDomain::Misc,
        )
        .map_err(ctx("test spec must be valid"))
    }

    #[test]
    fn test_dispatch_rejects_tier_below_operation_requirement() -> TestResult {
        let registry = CommandRegistry::new(vec![protected_spec(
            "maint",
            "mt",
            PermissionTier::Maintainer,
        )?]);
        let invocation = Invocation::Command {
            name: "maint".to_owned(),
            raw_args: vec![],
        };

        let Err(denied) = registry.dispatch(
            context(
                PermissionTier::Operator,
                InvocationSurface::Tui,
                CapabilitySet::default(),
            ),
            invocation.clone(),
        ) else {
            return Err(TestError::Unexpected(
                "dispatch below required tier must be rejected".into(),
            ));
        };
        assert_eq!(
            denied,
            CommandError::PermissionDenied {
                command: "maint".to_owned(),
                required: PermissionTier::Maintainer,
                actual: PermissionTier::Operator,
            }
        );

        let admitted = registry
            .dispatch(
                context(
                    PermissionTier::Maintainer,
                    InvocationSurface::Tui,
                    CapabilitySet::default(),
                ),
                invocation,
            )
            .map_err(ctx("caller at exactly the required tier must be admitted"))?;
        let CommandAction::Command(spec, args) = &admitted else {
            return Err(TestError::Unexpected(format!(
                "expected an admitted command action, got {admitted:?}"
            )));
        };
        assert_eq!(spec.name.as_str(), "maint");
        assert!(args.is_empty());
        Ok(())
    }

    #[test]
    fn test_find_prefers_canonical_name_over_earlier_alias() -> TestResult {
        let registry = CommandRegistry::new(vec![
            protected_spec("first", "second", PermissionTier::Observer)?,
            protected_spec("second", "sec", PermissionTier::Owner)?,
        ]);
        assert_eq!(
            registry.find("second").map(|spec| spec.permission),
            Some(PermissionTier::Owner),
            "an earlier alias must not shadow a later canonical command"
        );
        assert_eq!(
            registry.find("sec").map(|spec| spec.name.as_str()),
            Some("second")
        );
        Ok(())
    }

    #[test]
    fn test_from_command_adapters_carries_adapter_permission_and_aliases() -> TestResult {
        let ops = ops_registry();
        let adapters: Vec<CommandAdapter> = ops
            .iter()
            .flat_map(|op| CommandAdapter::from_operation(std::sync::Arc::clone(op)))
            .collect();
        let registry = CommandRegistry::from_command_adapters(&adapters);

        let model = registry
            .find("m")
            .ok_or(TestError::Missing("alias 'm' must resolve"))?;
        assert_eq!(model.name.as_str(), "model");
        assert_eq!(model.permission, PermissionTier::Operator);
        assert_eq!(model.scope, CommandScope::TuiOnly);
        assert!(
            CommandRegistry::from_command_adapters(&[])
                .specs()
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn registry_rejects_tui_only_commands_from_channels() -> TestResult {
        let registry = CommandRegistry::built_in().map_err(ctx("built_in"))?;
        let invocation = classify_input("/attach p:42").map_err(ctx("classify_input"))?;
        // attach declares permission = operator; use Operator so the permission
        // check passes and the TuiOnly scope check is reached.
        let Err(error) = registry.dispatch(
            context(
                PermissionTier::Operator,
                InvocationSurface::Channel,
                CapabilitySet::default(),
            ),
            invocation,
        ) else {
            return Err(TestError::Unexpected(
                "tui-only command from a channel must be rejected".into(),
            ));
        };

        assert!(matches!(error, CommandError::TuiOnlyCommand { command } if command == "attach"));
        Ok(())
    }

    #[test]
    fn shell_needs_operator_and_surface_appropriate_capability() -> TestResult {
        let registry = CommandRegistry::built_in().map_err(ctx("built_in"))?;
        let invocation = classify_input("! rg TODO").map_err(ctx("classify_input"))?;

        let local = registry.dispatch(
            context(
                PermissionTier::Operator,
                InvocationSurface::Tui,
                CapabilitySet::with(ShellCapability::CommandsShell),
            ),
            invocation.clone(),
        );
        assert!(matches!(local, Ok(CommandAction::Shell(command)) if command == "rg TODO"));

        let remote = registry.dispatch(
            context(
                PermissionTier::Operator,
                InvocationSurface::Channel,
                CapabilitySet::with(ShellCapability::CommandsShell),
            ),
            invocation,
        );
        assert!(matches!(
            remote,
            Err(CommandError::CapabilityDenied {
                capability: "channel.allow_shell"
            })
        ));
        Ok(())
    }

    // ── from_operation_registry tests ────────────────────────────────────────

    /// Test 1: after register_all, at least 15 command-surface specs are produced.
    ///
    /// All 18 ops have a `Surface::Command`; this test uses ≥15 to tolerate any
    /// future ops that expose only a `ModelTool` surface.
    #[test]
    fn test_from_operation_registry_produces_expected_command_count() -> TestResult {
        let ops = ops_registry();
        let registry = CommandRegistry::from_operation_registry(&ops)
            .map_err(ctx("built-in ops must produce a collision-free registry"))?;
        assert!(
            registry.specs().len() >= 15,
            "expected at least 15 command specs from 18 ops, got {}",
            registry.specs().len()
        );
        Ok(())
    }

    /// Test 2: no spec name starts with '/'.
    #[test]
    fn test_from_operation_registry_names_have_no_leading_slash() -> TestResult {
        let ops = ops_registry();
        let registry = CommandRegistry::from_operation_registry(&ops)
            .map_err(ctx("built-in ops must produce a collision-free registry"))?;
        for spec in registry.specs() {
            assert!(
                !spec.name.as_str().starts_with('/'),
                "spec name '{}' must not start with '/'",
                spec.name.as_str()
            );
        }
        Ok(())
    }

    /// Test 3: /model and /provider produce specs named "model" and "provider".
    #[test]
    fn test_from_operation_registry_includes_model_and_provider() -> TestResult {
        let ops = ops_registry();
        let registry = CommandRegistry::from_operation_registry(&ops)
            .map_err(ctx("built-in ops must produce a collision-free registry"))?;
        assert!(
            registry.find("model").is_some(),
            "expected a 'model' command spec"
        );
        assert!(
            registry.find("provider").is_some(),
            "expected a 'provider' command spec"
        );
        Ok(())
    }

    /// Test 4: `built_in()` produces the same specs as `from_operation_registry` on a
    /// freshly-populated registry.
    #[test]
    fn test_built_in_matches_from_operation_registry() -> TestResult {
        let via_built_in = CommandRegistry::built_in().map_err(ctx("built_in"))?;
        let ops = ops_registry();
        let via_from = CommandRegistry::from_operation_registry(&ops)
            .map_err(ctx("built-in ops must produce a collision-free registry"))?;

        assert_eq!(
            via_built_in.specs().len(),
            via_from.specs().len(),
            "built_in() and from_operation_registry() must produce the same number of specs"
        );

        // Verify every name present in one registry is present in the other.
        for spec in via_built_in.specs() {
            assert!(
                via_from.find(spec.name.as_str()).is_some(),
                "spec '{}' from built_in() not found in from_operation_registry()",
                spec.name.as_str()
            );
        }
        Ok(())
    }

    /// Test 5: /model operation is declared TuiOnly → its spec must have TuiOnly scope.
    #[test]
    fn test_from_operation_registry_maps_tui_only_visibility() -> TestResult {
        let ops = ops_registry();
        let registry = CommandRegistry::from_operation_registry(&ops)
            .map_err(ctx("built-in ops must produce a collision-free registry"))?;
        let model_spec = registry
            .find("model")
            .ok_or(TestError::Missing("'model' spec must exist"))?;
        assert_eq!(
            model_spec.scope,
            CommandScope::TuiOnly,
            "'/model' declares tui_only visibility; scope must be TuiOnly"
        );
        Ok(())
    }

    /// Test 6: /model operation declares permission = operator → its spec must reflect that.
    #[test]
    fn test_from_operation_registry_maps_permission_tier_operator() -> TestResult {
        let ops = ops_registry();
        let registry = CommandRegistry::from_operation_registry(&ops)
            .map_err(ctx("built-in ops must produce a collision-free registry"))?;
        let model_spec = registry
            .find("model")
            .ok_or(TestError::Missing("'model' spec must exist"))?;
        assert_eq!(
            model_spec.permission,
            PermissionTier::Operator,
            "'/model' declares permission=operator; spec must require Operator tier"
        );
        Ok(())
    }

    /// Test 7: aliases from OperationMeta reach CommandSpec and are findable via find().
    ///
    /// The `/model` op declares `aliases = ["m"]`, `/provider` declares `aliases = ["p"]`,
    /// and `/effort` declares `aliases = ["reasoning"]`. All three must be discoverable
    /// via the short alias, and the returned spec must carry the canonical name.
    #[test]
    fn aliases_from_operation_meta_reach_commandspec() -> TestResult {
        let ops = ops_registry();
        let registry = CommandRegistry::from_operation_registry(&ops)
            .map_err(ctx("built-in ops must produce a collision-free registry"))?;

        // "m" must resolve and its canonical name must be "model".
        let m_spec = registry.find("m").ok_or(TestError::Missing(
            "alias 'm' must be findable in the command registry",
        ))?;
        assert_eq!(
            m_spec.name.as_str(),
            "model",
            "alias 'm' must map to the canonical 'model' command"
        );

        // "p" must resolve and its canonical name must be "provider".
        let p_spec = registry.find("p").ok_or(TestError::Missing(
            "alias 'p' must be findable in the command registry",
        ))?;
        assert_eq!(
            p_spec.name.as_str(),
            "provider",
            "alias 'p' must map to the canonical 'provider' command"
        );

        // "reasoning" must resolve and its canonical name must be "effort".
        let r_spec = registry.find("reasoning").ok_or(TestError::Missing(
            "alias 'reasoning' must be findable in the command registry",
        ))?;
        assert_eq!(
            r_spec.name.as_str(),
            "effort",
            "alias 'reasoning' must map to the canonical 'effort' command"
        );
        Ok(())
    }

    /// Test 8: `from_operation_registry` copies `OperationMeta::busy` into
    /// `CommandSpec::busy` — both for an operation that opts into `Immediate`
    /// (`/model`) and one that keeps the `DeferredUntilTurnEnd` default (`/new`).
    #[test]
    fn test_from_operation_registry_maps_busy_availability() -> TestResult {
        let ops = ops_registry();
        let registry = CommandRegistry::from_operation_registry(&ops)
            .map_err(ctx("built-in ops must produce a collision-free registry"))?;

        let model_spec = registry
            .find("model")
            .ok_or(TestError::Missing("'model' spec must exist"))?;
        assert_eq!(
            model_spec.busy,
            harw_operations::operation::BusyAvailability::Immediate,
            "'/model' declares busy=\"immediate\"; spec must carry Immediate"
        );

        let new_spec = registry
            .find("new")
            .ok_or(TestError::Missing("'new' spec must exist"))?;
        assert_eq!(
            new_spec.busy,
            harw_operations::operation::BusyAvailability::DeferredUntilTurnEnd,
            "'/new' does not declare busy; spec must carry the DeferredUntilTurnEnd default"
        );
        Ok(())
    }

    /// Test 9: `from_command_adapters` copies `OperationMeta::busy` into
    /// `CommandSpec::busy` — mirrors the `from_operation_registry` coverage above
    /// for the adapter-driven constructor used by `command_exec`.
    #[test]
    fn test_from_command_adapters_maps_busy_availability() -> TestResult {
        let ops = ops_registry();
        let adapters: Vec<CommandAdapter> = ops
            .iter()
            .flat_map(|op| CommandAdapter::from_operation(std::sync::Arc::clone(op)))
            .collect();
        let registry = CommandRegistry::from_command_adapters(&adapters);

        let model_spec = registry
            .find("model")
            .ok_or(TestError::Missing("'model' spec must exist"))?;
        assert_eq!(
            model_spec.busy,
            harw_operations::operation::BusyAvailability::Immediate,
            "'/model' declares busy=\"immediate\"; adapter-derived spec must carry Immediate"
        );

        let new_spec = registry
            .find("new")
            .ok_or(TestError::Missing("'new' spec must exist"))?;
        assert_eq!(
            new_spec.busy,
            harw_operations::operation::BusyAvailability::DeferredUntilTurnEnd,
            "'/new' does not declare busy; adapter-derived spec must carry the \
             DeferredUntilTurnEnd default"
        );
        Ok(())
    }
    // ── Hilfe-Felder und lokale Spezifikationen ─────────────────────────────

    fn adapters() -> Vec<CommandAdapter> {
        ops_registry()
            .iter()
            .flat_map(|op| CommandAdapter::from_operation(std::sync::Arc::clone(op)))
            .collect()
    }

    /// Beide Konstruktoren übernehmen `OperationMeta::summary` und reichern
    /// über `command_catalog::enrich` an.
    #[test]
    fn constructors_set_summary_usage_subcommands_and_origin() -> TestResult {
        let via_ops = CommandRegistry::from_operation_registry(&ops_registry())
            .map_err(ctx("built-in ops must produce a collision-free registry"))?;
        let via_adapters = CommandRegistry::from_command_adapters(&adapters());
        for registry in [&via_ops, &via_adapters] {
            for spec in registry.specs() {
                assert_eq!(spec.origin, CommandOrigin::Operation, "{}", spec.name);
                assert!(!spec.summary.is_empty(), "{} lacks a summary", spec.name);
                assert!(
                    spec.usage.starts_with(&format!("/{}", spec.name)),
                    "{} usage {:?}",
                    spec.name,
                    spec.usage
                );
            }
            let model = registry
                .find("model")
                .ok_or(TestError::Missing("'model' spec must exist"))?;
            assert!(model.summary.contains("Modell"), "{:?}", model.summary);
            let names: Vec<&str> = model.subcommands.iter().map(|h| h.name).collect();
            assert_eq!(names, ["show", "list", "switch"]);
        }
        Ok(())
    }

    #[test]
    fn with_local_specs_appends_locals_and_lets_operations_win() -> TestResult {
        let ops = CommandRegistry::built_in().map_err(ctx("built_in"))?;
        let op_mode = ops.find("mode").cloned();
        let before = ops.specs().len();
        let merged = ops.with_local_specs(crate::command_catalog::local_command_specs());

        let tools = merged
            .find("tools")
            .ok_or(TestError::Missing("local '/tools' must be merged"))?;
        assert_eq!(tools.origin, CommandOrigin::TuiLocal);
        assert!(merged.specs().len() > before);

        // `/mode` ist eine Operation: der lokale Ersatz entfällt.
        if let Some(op_mode) = op_mode {
            let hits = merged
                .specs()
                .iter()
                .filter(|spec| spec.name.as_str() == "mode")
                .count();
            assert_eq!(hits, 1, "only one '/mode' may survive");
            let mode = merged.find("mode").ok_or(TestError::Missing("mode"))?;
            assert_eq!(mode.origin, CommandOrigin::Operation);
            assert_eq!(mode.permission, op_mode.permission);
        }
        Ok(())
    }

    #[test]
    fn with_local_specs_drops_locals_whose_alias_or_name_collides() -> TestResult {
        let base = CommandRegistry::new(vec![protected_spec(
            "status",
            "st",
            PermissionTier::Observer,
        )?]);
        let by_name = protected_spec("st", "zz", PermissionTier::Owner)?.local();
        let by_alias = protected_spec("other", "status", PermissionTier::Owner)?.local();
        let fresh = protected_spec("fresh", "fr", PermissionTier::Observer)?.local();
        let duplicate = protected_spec("fresh", "fr2", PermissionTier::Owner)?.local();
        let merged = base.with_local_specs(vec![by_name, by_alias, fresh, duplicate]);

        let names: Vec<&str> = merged.specs().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["status", "fresh"]);
        assert_eq!(
            merged.find("st").map(|spec| spec.name.as_str()),
            Some("status"),
            "the operation alias must keep resolving to the operation"
        );
        assert_eq!(
            merged.find("fresh").map(|spec| spec.permission),
            Some(PermissionTier::Observer),
            "the first of two same-named locals wins"
        );
        Ok(())
    }
}
