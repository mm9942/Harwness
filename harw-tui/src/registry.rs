//! Central command lookup and admission gates.
//!
//! Source: `harw-tui` interaction-contract spec.

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

                    if let Ok(cmd_spec) = CommandSpec::new(
                        cmd_name.as_str(),
                        meta.aliases.iter().copied(),
                        scope,
                        permission,
                        output,
                        domain,
                    ) {
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
    /// Backward-compatible: callers continue to use `CommandRegistry::built_in()` with
    /// the same return type and semantics.
    ///
    /// # Returns
    /// A [`CommandRegistry`] containing one [`CommandSpec`] per `Surface::Command`
    /// declaration across all 17 built-in operations.
    ///
    /// # Concurrency
    /// The underlying [`harw_operations::registry::OperationRegistry`] is a short-lived
    /// local value; this function is safe to call from any thread.
    ///
    /// # Examples
    /// ```rust
    /// use harw_tui::CommandRegistry;
    ///
    /// let registry = CommandRegistry::built_in();
    /// assert!(registry.specs().len() >= 15);
    /// ```
    /// Builds the built-in command registry driven by [`harw_ops::register_all`].
    ///
    /// # Description
    /// Creates a fresh [`harw_operations::registry::OperationRegistry`], populates it
    /// via [`harw_ops::register_all`], and delegates to [`Self::from_operation_registry`].
    /// Panics if the built-in op-set contains any name or alias collision — by construction
    /// it must not, and a panic here indicates a programming error in `harw-ops`.
    ///
    /// # Returns
    /// A [`CommandRegistry`] containing one [`CommandSpec`] per `Surface::Command`
    /// declaration across all built-in operations, with aliases wired in.
    ///
    /// # Panics
    /// Panics if [`Self::from_operation_registry`] returns an error (collision in built-in ops).
    ///
    /// # Concurrency
    /// The underlying [`harw_operations::registry::OperationRegistry`] is a short-lived
    /// local value; this function is safe to call from any thread.
    ///
    /// # Examples
    /// ```rust
    /// use harw_tui::CommandRegistry;
    ///
    /// let registry = CommandRegistry::built_in();
    /// assert!(registry.specs().len() >= 15);
    /// ```
    #[must_use]
    pub fn built_in() -> Self {
        let mut ops = harw_operations::registry::OperationRegistry::new();
        harw_ops::register_all(&mut ops);
        Self::from_operation_registry(&ops)
            .expect("built-in op-set must have no name or alias collisions")
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
    /// - `aliases` / `domain`: read from the operation metadata exactly once per adapter;
    ///   aliases that are not valid command names are dropped (they were unreachable).
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
            specs.push(CommandSpec {
                name,
                aliases,
                scope: map_visibility(adapter.visibility()),
                permission: adapter.permission(),
                output: OutputSurface::Inline,
                domain: map_domain(meta.domain),
            });
        }
        Self::new(specs)
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
    fn canonical_command_resolves_by_name() {
        // The "h" alias was present only in the old hard-coded built_in(). The
        // new implementation derives specs from the operation contract, which
        // carries no alias declarations. Verify the canonical name still resolves.
        let registry = CommandRegistry::built_in();
        assert_eq!(
            registry.find("help").map(|spec| spec.name.as_str()),
            Some("help"),
            "help command must be findable by its canonical name"
        );
    }

    #[test]
    fn registry_enforces_permission_before_dispatch() {
        let registry = CommandRegistry::built_in();
        let invocation = classify_input("/new").unwrap();
        let error = registry
            .dispatch(
                context(
                    PermissionTier::Observer,
                    InvocationSurface::Tui,
                    CapabilitySet::default(),
                ),
                invocation,
            )
            .unwrap_err();

        assert!(matches!(
            error,
            CommandError::PermissionDenied { command, required: PermissionTier::Operator, .. }
                if command == "new"
        ));
    }

    fn protected_spec(name: &str, alias: &str, permission: PermissionTier) -> CommandSpec {
        CommandSpec::new(
            name,
            [alias],
            CommandScope::TuiOnly,
            permission,
            OutputSurface::Inline,
            CommandDomain::Misc,
        )
        .expect("test spec must be valid")
    }

    #[test]
    fn test_dispatch_rejects_tier_below_operation_requirement() {
        let registry = CommandRegistry::new(vec![protected_spec(
            "maint",
            "mt",
            PermissionTier::Maintainer,
        )]);
        let invocation = Invocation::Command {
            name: "maint".to_owned(),
            raw_args: vec![],
        };

        let denied = registry
            .dispatch(
                context(
                    PermissionTier::Operator,
                    InvocationSurface::Tui,
                    CapabilitySet::default(),
                ),
                invocation.clone(),
            )
            .unwrap_err();
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
            .expect("caller at exactly the required tier must be admitted");
        let CommandAction::Command(spec, args) = &admitted else {
            panic!("expected an admitted command action, got {admitted:?}");
        };
        assert_eq!(spec.name.as_str(), "maint");
        assert!(args.is_empty());
    }

    #[test]
    fn test_find_prefers_canonical_name_over_earlier_alias() {
        let registry = CommandRegistry::new(vec![
            protected_spec("first", "second", PermissionTier::Observer),
            protected_spec("second", "sec", PermissionTier::Owner),
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
    }

    #[test]
    fn test_from_command_adapters_carries_adapter_permission_and_aliases() {
        let ops = ops_registry();
        let adapters: Vec<CommandAdapter> = ops
            .iter()
            .flat_map(|op| CommandAdapter::from_operation(std::sync::Arc::clone(op)))
            .collect();
        let registry = CommandRegistry::from_command_adapters(&adapters);

        let model = registry.find("m").expect("alias 'm' must resolve");
        assert_eq!(model.name.as_str(), "model");
        assert_eq!(model.permission, PermissionTier::Operator);
        assert_eq!(model.scope, CommandScope::TuiOnly);
        assert!(CommandRegistry::from_command_adapters(&[]).specs().is_empty());
    }

    #[test]
    fn registry_rejects_tui_only_commands_from_channels() {
        let registry = CommandRegistry::built_in();
        let invocation = classify_input("/attach p:42").unwrap();
        // attach declares permission = operator; use Operator so the permission
        // check passes and the TuiOnly scope check is reached.
        let error = registry
            .dispatch(
                context(
                    PermissionTier::Operator,
                    InvocationSurface::Channel,
                    CapabilitySet::default(),
                ),
                invocation,
            )
            .unwrap_err();

        assert!(matches!(error, CommandError::TuiOnlyCommand { command } if command == "attach"));
    }

    #[test]
    fn shell_needs_operator_and_surface_appropriate_capability() {
        let registry = CommandRegistry::built_in();
        let invocation = classify_input("! rg TODO").unwrap();

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
    }

    // ── from_operation_registry tests ────────────────────────────────────────

    /// Test 1: after register_all, at least 15 command-surface specs are produced.
    ///
    /// All 18 ops have a `Surface::Command`; this test uses ≥15 to tolerate any
    /// future ops that expose only a `ModelTool` surface.
    #[test]
    fn test_from_operation_registry_produces_expected_command_count() {
        let ops = ops_registry();
        let registry = CommandRegistry::from_operation_registry(&ops)
            .expect("built-in ops must produce a collision-free registry");
        assert!(
            registry.specs().len() >= 15,
            "expected at least 15 command specs from 18 ops, got {}",
            registry.specs().len()
        );
    }

    /// Test 2: no spec name starts with '/'.
    #[test]
    fn test_from_operation_registry_names_have_no_leading_slash() {
        let ops = ops_registry();
        let registry = CommandRegistry::from_operation_registry(&ops)
            .expect("built-in ops must produce a collision-free registry");
        for spec in registry.specs() {
            assert!(
                !spec.name.as_str().starts_with('/'),
                "spec name '{}' must not start with '/'",
                spec.name.as_str()
            );
        }
    }

    /// Test 3: /model and /provider produce specs named "model" and "provider".
    #[test]
    fn test_from_operation_registry_includes_model_and_provider() {
        let ops = ops_registry();
        let registry = CommandRegistry::from_operation_registry(&ops)
            .expect("built-in ops must produce a collision-free registry");
        assert!(
            registry.find("model").is_some(),
            "expected a 'model' command spec"
        );
        assert!(
            registry.find("provider").is_some(),
            "expected a 'provider' command spec"
        );
    }

    /// Test 4: `built_in()` produces the same specs as `from_operation_registry` on a
    /// freshly-populated registry.
    #[test]
    fn test_built_in_matches_from_operation_registry() {
        let via_built_in = CommandRegistry::built_in();
        let ops = ops_registry();
        let via_from = CommandRegistry::from_operation_registry(&ops)
            .expect("built-in ops must produce a collision-free registry");

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
    }

    /// Test 5: /model operation is declared TuiOnly → its spec must have TuiOnly scope.
    #[test]
    fn test_from_operation_registry_maps_tui_only_visibility() {
        let ops = ops_registry();
        let registry = CommandRegistry::from_operation_registry(&ops)
            .expect("built-in ops must produce a collision-free registry");
        let model_spec = registry.find("model").expect("'model' spec must exist");
        assert_eq!(
            model_spec.scope,
            CommandScope::TuiOnly,
            "'/model' declares tui_only visibility; scope must be TuiOnly"
        );
    }

    /// Test 6: /model operation declares permission = operator → its spec must reflect that.
    #[test]
    fn test_from_operation_registry_maps_permission_tier_operator() {
        let ops = ops_registry();
        let registry = CommandRegistry::from_operation_registry(&ops)
            .expect("built-in ops must produce a collision-free registry");
        let model_spec = registry.find("model").expect("'model' spec must exist");
        assert_eq!(
            model_spec.permission,
            PermissionTier::Operator,
            "'/model' declares permission=operator; spec must require Operator tier"
        );
    }

    /// Test 7: aliases from OperationMeta reach CommandSpec and are findable via find().
    ///
    /// The `/model` op declares `aliases = ["m"]`, `/provider` declares `aliases = ["p"]`,
    /// and `/effort` declares `aliases = ["reasoning"]`. All three must be discoverable
    /// via the short alias, and the returned spec must carry the canonical name.
    #[test]
    fn aliases_from_operation_meta_reach_commandspec() {
        let ops = ops_registry();
        let registry = CommandRegistry::from_operation_registry(&ops)
            .expect("built-in ops must produce a collision-free registry");

        // "m" must resolve and its canonical name must be "model".
        let m_spec = registry.find("m");
        assert!(
            m_spec.is_some(),
            "alias 'm' must be findable in the command registry"
        );
        assert_eq!(
            m_spec.unwrap().name.as_str(),
            "model",
            "alias 'm' must map to the canonical 'model' command"
        );

        // "p" must resolve and its canonical name must be "provider".
        let p_spec = registry.find("p");
        assert!(
            p_spec.is_some(),
            "alias 'p' must be findable in the command registry"
        );
        assert_eq!(
            p_spec.unwrap().name.as_str(),
            "provider",
            "alias 'p' must map to the canonical 'provider' command"
        );

        // "reasoning" must resolve and its canonical name must be "effort".
        let r_spec = registry.find("reasoning");
        assert!(
            r_spec.is_some(),
            "alias 'reasoning' must be findable in the command registry"
        );
        assert_eq!(
            r_spec.unwrap().name.as_str(),
            "effort",
            "alias 'reasoning' must map to the canonical 'effort' command"
        );
    }
}
