//! Static command metadata used by the registry and a future renderer.

use std::fmt;

use crate::{CommandError, CommandResult};

/// Canonical, validated command name without a leading slash.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommandName(String);

impl CommandName {
    /// Validates the command-name grammar from the interaction contract.
    pub fn parse(input: impl Into<String>) -> CommandResult<Self> {
        let input = input.into();
        let valid = !input.is_empty()
            && input.len() <= 32
            && input.as_bytes()[0].is_ascii_lowercase()
            && input
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');

        if valid {
            Ok(Self(input))
        } else {
            Err(CommandError::InvalidCommandName { input })
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommandName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a command is available.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandScope {
    TuiOnly,
    ChannelParity,
    ChannelReduced,
}

/// Minimum permission required to invoke a command.
///
/// Re-exported from `harw_operations` — `harw-tui` used to define its own,
/// structurally identical copy of this enum. A single shared type is used
/// so that `CommandAdapter` (from `harw-operations`) and the TUI's own
/// `CommandRegistry` gate the exact same permission values instead of two
/// silently-diverging truths.
pub use harw_operations::operation::PermissionTier;

/// Surface on which a future renderer presents a command result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputSurface {
    Inline,
    Panel,
    Pager,
    Toast,
}

/// Help and completion grouping for commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandDomain {
    SessionLifecycle,
    AgentTopology,
    WorkGovernance,
    Execution,
    CatalogConfig,
    Knowledge,
    Channels,
    Misc,
}

/// Static description of a command accepted by the command shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    pub name: CommandName,
    pub aliases: Vec<String>,
    pub scope: CommandScope,
    pub permission: PermissionTier,
    pub output: OutputSurface,
    pub domain: CommandDomain,
}

impl CommandSpec {
    /// Creates a command spec after validating its canonical name and aliases.
    pub fn new(
        name: impl Into<String>,
        aliases: impl IntoIterator<Item = impl Into<String>>,
        scope: CommandScope,
        permission: PermissionTier,
        output: OutputSurface,
        domain: CommandDomain,
    ) -> CommandResult<Self> {
        let name = CommandName::parse(name)?;
        let aliases = aliases.into_iter().map(Into::into).collect::<Vec<String>>();

        for alias in &aliases {
            CommandName::parse(alias.clone())?;
        }

        Ok(Self {
            name,
            aliases,
            scope,
            permission,
            output,
            domain,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{CommandDomain, CommandName, CommandScope, CommandSpec, OutputSurface};
    use crate::{CommandError, PermissionTier};

    #[test]
    fn command_name_parse_retains_the_invalid_input() {
        let error = CommandName::parse("Uppercase").expect_err("uppercase names must fail");

        assert_eq!(
            error,
            CommandError::InvalidCommandName {
                input: "Uppercase".to_owned(),
            }
        );
    }

    #[test]
    fn command_spec_rejects_an_invalid_alias_with_a_typed_error() {
        let error = CommandSpec::new(
            "status",
            ["not_an_alias"],
            CommandScope::TuiOnly,
            PermissionTier::Observer,
            OutputSurface::Inline,
            CommandDomain::SessionLifecycle,
        )
        .expect_err("underscores are not valid command aliases");

        assert_eq!(
            error,
            CommandError::InvalidCommandName {
                input: "not_an_alias".to_owned(),
            }
        );
    }
}
