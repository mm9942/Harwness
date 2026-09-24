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

/// Verfügbarkeit eines Befehls während eines laufenden ("busy") Turns.
///
/// Re-exported from `harw_operations` — mirrors [`OperationMeta::busy`] so that
/// [`CommandSpec`] carries the same busy-availability truth as the operation it
/// was derived from, instead of a second, silently-diverging copy.
///
/// [`OperationMeta::busy`]: harw_operations::operation::OperationMeta::busy
pub use harw_operations::operation::BusyAvailability;

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

/// Herkunft eines Befehls im TUI-Katalog.
///
/// # Varianten
/// - [`Self::Operation`]: aus einer registrierten `harw-ops`-Operation
///   abgeleitet; die Ausführung läuft über den Operations-Dispatch.
/// - [`Self::TuiLocal`]: rein TUI-lokaler Befehl (z. B. `/tools`, `/exit`),
///   der vor dem Operations-Dispatch abgefangen wird. Er erscheint im Popup
///   und in der Hilfe, hat aber keine Operation hinter sich.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CommandOrigin {
    /// Aus einer registrierten Operation abgeleitet (Standard).
    #[default]
    Operation,
    /// Rein TUI-lokal abgefangener Befehl.
    TuiLocal,
}

/// Statischer Hinweis auf ein Unterkommando für Popup, Hilfe und Vervollständigung.
///
/// # Felder
/// - `name`: das Unterkommando-Token (z. B. `"switch"`, `"--format"`).
/// - `args`: Argument-Grammatik hinter dem Token, leer wenn keine
///   (z. B. `"<modell-id>"`).
/// - `summary`: kurze deutsche Beschreibung (eine Zeile).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SubcommandHint {
    /// Unterkommando-Token ohne führendes `/`.
    pub name: &'static str,
    /// Argument-Grammatik hinter dem Token; leer, wenn keine Argumente folgen.
    pub args: &'static str,
    /// Kurze deutsche Beschreibung.
    pub summary: &'static str,
}

impl SubcommandHint {
    /// Erzeugt einen Hinweis (in `const`-Tabellen verwendbar).
    #[must_use]
    pub const fn new(name: &'static str, args: &'static str, summary: &'static str) -> Self {
        Self {
            name,
            args,
            summary,
        }
    }
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
    pub busy: harw_operations::operation::BusyAvailability,
    /// Kurze deutsche Beschreibung (eine Zeile); leer, wenn unbekannt.
    pub summary: String,
    /// Nutzungszeile, z. B. `"/model [show|list|switch <modell-id>]"`; leer,
    /// wenn unbekannt.
    pub usage: String,
    /// Bekannte Unterkommandos in Anzeigereihenfolge; leer, wenn keine.
    pub subcommands: Vec<SubcommandHint>,
    /// Herkunft: Operation oder TUI-lokal.
    pub origin: CommandOrigin,
}

impl CommandSpec {
    /// Creates a command spec after validating its canonical name and aliases.
    ///
    /// `busy` defaults to [`BusyAvailability::DeferredUntilTurnEnd`] (the same
    /// default as [`harw_operations::operation::OperationMeta::busy`]); callers
    /// that need to construct a spec with an explicit busy-availability set the
    /// field directly on the returned value.
    ///
    /// `summary`/`usage` starten leer, `subcommands` ohne Einträge und
    /// `origin` als [`CommandOrigin::Operation`]; siehe [`Self::with_help`]
    /// und [`Self::local`].
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
            busy: BusyAvailability::default(),
            summary: String::new(),
            usage: String::new(),
            subcommands: Vec::new(),
            origin: CommandOrigin::Operation,
        })
    }

    /// Setzt Beschreibung, Nutzungszeile und Unterkommando-Hinweise.
    ///
    /// # Argumente
    /// - `summary`: kurze deutsche Beschreibung.
    /// - `usage`: Nutzungszeile (mit führendem `/`).
    /// - `subs`: Unterkommando-Hinweise; ersetzt vorhandene vollständig.
    ///
    /// # Rückgabe
    /// Die geänderte Spezifikation (Builder-Stil).
    #[must_use]
    pub fn with_help(
        mut self,
        summary: impl Into<String>,
        usage: impl Into<String>,
        subs: &[SubcommandHint],
    ) -> Self {
        self.summary = summary.into();
        self.usage = usage.into();
        self.subcommands = subs.to_vec();
        self
    }

    /// Markiert die Spezifikation als TUI-lokal ([`CommandOrigin::TuiLocal`]).
    #[must_use]
    pub fn local(mut self) -> Self {
        self.origin = CommandOrigin::TuiLocal;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BusyAvailability, CommandDomain, CommandName, CommandOrigin, CommandScope, CommandSpec,
        OutputSurface, SubcommandHint,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::{CommandError, PermissionTier};

    #[test]
    fn command_name_parse_retains_the_invalid_input() -> TestResult {
        let Err(error) = CommandName::parse("Uppercase") else {
            return Err(TestError::Unexpected(
                "uppercase names must fail".to_owned(),
            ));
        };

        assert_eq!(
            error,
            CommandError::InvalidCommandName {
                input: "Uppercase".to_owned(),
            }
        );
        Ok(())
    }

    #[test]
    fn command_spec_rejects_an_invalid_alias_with_a_typed_error() -> TestResult {
        let Err(error) = CommandSpec::new(
            "status",
            ["not_an_alias"],
            CommandScope::TuiOnly,
            PermissionTier::Observer,
            OutputSurface::Inline,
            CommandDomain::SessionLifecycle,
        ) else {
            return Err(TestError::Unexpected(
                "underscores are not valid command aliases".to_owned(),
            ));
        };

        assert_eq!(
            error,
            CommandError::InvalidCommandName {
                input: "not_an_alias".to_owned(),
            }
        );
        Ok(())
    }

    #[test]
    fn command_spec_new_defaults_busy_to_deferred_until_turn_end() -> TestResult {
        let spec = CommandSpec::new(
            "status",
            Vec::<String>::new(),
            CommandScope::TuiOnly,
            PermissionTier::Observer,
            OutputSurface::Inline,
            CommandDomain::SessionLifecycle,
        )
        .map_err(ctx("valid spec must construct"))?;

        assert_eq!(
            spec.busy,
            BusyAvailability::DeferredUntilTurnEnd,
            "CommandSpec::new must default busy to DeferredUntilTurnEnd, matching \
             OperationMeta::busy's default"
        );
        Ok(())
    }

    #[test]
    fn command_spec_busy_field_can_be_set_to_immediate() -> TestResult {
        let mut spec = CommandSpec::new(
            "status",
            Vec::<String>::new(),
            CommandScope::TuiOnly,
            PermissionTier::Observer,
            OutputSurface::Inline,
            CommandDomain::SessionLifecycle,
        )
        .map_err(ctx("valid spec must construct"))?;
        spec.busy = BusyAvailability::Immediate;

        assert_eq!(spec.busy, BusyAvailability::Immediate);
        Ok(())
    }
    #[test]
    fn command_spec_new_defaults_help_fields_to_empty_operation() -> TestResult {
        let spec = CommandSpec::new(
            "status",
            Vec::<String>::new(),
            CommandScope::TuiOnly,
            PermissionTier::Observer,
            OutputSurface::Inline,
            CommandDomain::SessionLifecycle,
        )
        .map_err(ctx("valid spec must construct"))?;

        assert!(spec.summary.is_empty());
        assert!(spec.usage.is_empty());
        assert!(spec.subcommands.is_empty());
        assert_eq!(spec.origin, CommandOrigin::Operation);
        Ok(())
    }

    #[test]
    fn with_help_and_local_set_the_help_fields_and_origin() -> TestResult {
        const SUBS: &[SubcommandHint] = &[
            SubcommandHint::new("on", "<name>", "Werkzeug einschalten"),
            SubcommandHint::new("off", "<name>", "Werkzeug ausschalten"),
        ];
        let spec = CommandSpec::new(
            "tools",
            Vec::<String>::new(),
            CommandScope::TuiOnly,
            PermissionTier::Operator,
            OutputSurface::Inline,
            CommandDomain::Execution,
        )
        .map_err(ctx("valid spec must construct"))?
        .with_help("Werkzeuge verwalten", "/tools [on|off <name>]", SUBS)
        .local();

        assert_eq!(spec.summary, "Werkzeuge verwalten");
        assert_eq!(spec.usage, "/tools [on|off <name>]");
        assert_eq!(spec.subcommands, SUBS.to_vec());
        assert_eq!(spec.origin, CommandOrigin::TuiLocal);

        let replaced = spec.with_help("neu", "", &[]);
        assert!(
            replaced.subcommands.is_empty(),
            "with_help replaces subcommands"
        );
        assert_eq!(
            replaced.origin,
            CommandOrigin::TuiLocal,
            "with_help keeps the origin"
        );
        Ok(())
    }
}
