//! [`BuiltinDefaults`]: the contract through which the compiler reads the
//! built-in agent definitions, the capability catalog and the generic role
//! ceilings.
//!
//! # Responsibility
//! The compiler (layer C) needs data that lives in the composition layer
//! (A, `harw-registry-defaults`): the embedded role and base definitions,
//! the context-program library, the capability catalog (tool → provider,
//! class, runner feature) and the roster's generic ceilings. It must not
//! depend on that crate (§6, §35 of the workspace architecture). So the
//! compiler states here what it needs, and the composition layer implements
//! it and installs it once per process ([`install_builtin_defaults`]):
//! `harw_registry_defaults::compiler_defaults::install` does both.
//!
//! # Why a process-wide installation
//! Compiler entry points are reached from several callers that only hold a
//! [`crate::CompilerEnv`] (the CLI, the `/agent` op, the automatic UIA
//! build). The built-in data is static and identical for every caller, so
//! the composition root installs it once at start instead of threading it
//! through every signature.
//!
//! # Without an installation
//! [`require_builtin_defaults`] fails with a [`CompileError::Other`] that
//! names the fix; [`crate::discovery::SourceSet::discover`] and
//! [`crate::rights::BuiltinCeilings::load`] (and therefore
//! [`crate::Compiler::new`]) call it, so no compile runs without the data.
//! Lookups that cannot fail ([`builtin_defaults`]) see an empty catalog.
//!
//! # Concurrency
//! The installation is a [`OnceLock`]; the first installation wins and is
//! `Send + Sync` for the rest of the process.

use std::collections::HashMap;
use std::sync::OnceLock;

use harw_agent_dsl::bind::ContextProgramLibrary;
use harw_agent_dsl::classify::{ToolClasses, ToolClassifier};
use harw_agent_dsl::diagnostics::SourceFile;
use harw_agent_dsl::ir_v2::AgentIr;
use time::OffsetDateTime;

use crate::error::CompileError;

/// What the capability catalog says about one tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolCapability {
    /// Stable provider id (`"fs"`, `"web"`, …).
    pub provider_id: &'static str,
    /// The crate that implements the provider.
    pub crate_name: &'static str,
    /// Cargo feature of `harw-agent-runner` that links the provider.
    pub feature: &'static str,
    /// Capability class label (`"read"`, `"write"`, …).
    pub class: &'static str,
    /// Served by every runner without a provider feature of its own.
    pub always_available: bool,
}

/// Names of the built-in bases and the generic role ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoleDefaults {
    /// Built-in base layers: not compilable on their own.
    pub base_definition_names: &'static [&'static str],
    /// The worker base layer.
    pub worker_base_name: &'static str,
    /// The child-orchestrator base layer.
    pub child_orchestrator_base_name: &'static str,
    /// Built-in role that bounds a generic worker.
    pub generic_worker_base: &'static str,
    /// Maximum spawn depth of a generic worker.
    pub generic_worker_max_depth: u32,
    /// Label of the generic child-orchestrator ceiling.
    pub generic_child_orchestrator_base: &'static str,
    /// Built-in role that bounds a generic UIA worker.
    pub generic_uia_worker_base: &'static str,
    /// Write tools a generic writing worker may admit in addition.
    pub generic_worker_write_tools: &'static [&'static str],
}

impl RoleDefaults {
    /// No bases and no generic ceilings (the state without an installation).
    pub const EMPTY: Self = Self {
        base_definition_names: &[],
        worker_base_name: "",
        child_orchestrator_base_name: "",
        generic_worker_base: "",
        generic_worker_max_depth: 0,
        generic_child_orchestrator_base: "",
        generic_uia_worker_base: "",
        generic_worker_write_tools: &[],
    };
}

/// The built-in data the compiler reads (see the module docs).
///
/// # Errors
/// The fallible methods return the implementation's error as text; the
/// compiler wraps it in [`CompileError::Other`] (a broken built-in
/// definition is a build defect, not a user error).
pub trait BuiltinDefaults: Send + Sync {
    /// The catalog row of `tool` (exact rows before prefix rows), or `None`
    /// if no provider serves it.
    fn capability(&self, tool: &str) -> Option<ToolCapability>;

    /// Every catalog row as a label: the tool name, or a prefix followed by
    /// `*` (suggestions for unknown tools).
    fn catalog_labels(&self) -> Vec<String>;

    /// Every tool named by an exact catalog row (the user-interface
    /// ceiling).
    fn catalog_tools(&self) -> Vec<&'static str>;

    /// The runner feature that is always enabled.
    fn core_feature(&self) -> &'static str;

    /// Tools that only make sense when the agent can spawn children.
    fn spawn_tools(&self) -> &'static [&'static str];

    /// `true` for a delegation handoff tool (`transfer_to_<agent>`).
    fn is_handoff_tool(&self, tool: &str) -> bool;

    /// Capability class labels that write, run processes or act on the host.
    fn writing_classes(&self) -> &'static [&'static str];

    /// The catalog as a classifier for
    /// [`harw_agent_dsl::classify::reclassify_permissions`].
    fn tool_classifier(&self) -> &dyn ToolClassifier;

    /// The embedded role and base definitions as source files.
    fn source_files(&self) -> Vec<SourceFile>;

    /// The built-in context-program library.
    ///
    /// # Errors
    /// A built-in context program is broken.
    fn context_program_library(&self) -> Result<ContextProgramLibrary, String>;

    /// The built-in roles, lowered at `now`, by name.
    ///
    /// # Errors
    /// A built-in role is broken.
    fn agent_irs(&self, now: OffsetDateTime) -> Result<HashMap<String, AgentIr>, String>;

    /// The built-in base layers, lowered at `now`, by name.
    ///
    /// # Errors
    /// A built-in base is broken.
    fn base_irs(&self, now: OffsetDateTime) -> Result<HashMap<String, AgentIr>, String>;

    /// Base names and generic ceilings.
    fn roles(&self) -> RoleDefaults;
}

static INSTALLED: OnceLock<&'static dyn BuiltinDefaults> = OnceLock::new();

/// Installs the built-in defaults for this process.
///
/// Returns `true` if this call installed them, `false` if an installation
/// already existed (it stays; installing the same data twice is harmless).
pub fn install_builtin_defaults(defaults: &'static dyn BuiltinDefaults) -> bool {
    INSTALLED.set(defaults).is_ok()
}

/// The installed built-in defaults.
///
/// # Errors
/// [`CompileError::Other`] if nothing is installed (a composition defect of
/// the calling program).
pub fn require_builtin_defaults() -> Result<&'static dyn BuiltinDefaults, CompileError> {
    INSTALLED.get().copied().ok_or_else(|| {
        CompileError::Other(
            "no built-in agent defaults installed; the program must call \
             `harw_registry_defaults::compiler_defaults::install()` before compiling"
                .to_owned(),
        )
    })
}

/// The installed built-in defaults, or an empty catalog without an
/// installation (for lookups that cannot fail).
#[must_use]
pub fn builtin_defaults() -> &'static dyn BuiltinDefaults {
    INSTALLED.get().copied().unwrap_or(&NoDefaults)
}

/// The state without an installation: no tools, no definitions.
struct NoDefaults;

impl ToolClassifier for NoDefaults {
    fn classify(&self, _tool: &str) -> Option<ToolClasses> {
        None
    }
}

impl BuiltinDefaults for NoDefaults {
    fn capability(&self, _tool: &str) -> Option<ToolCapability> {
        None
    }

    fn catalog_labels(&self) -> Vec<String> {
        Vec::new()
    }

    fn catalog_tools(&self) -> Vec<&'static str> {
        Vec::new()
    }

    fn core_feature(&self) -> &'static str {
        "core"
    }

    fn spawn_tools(&self) -> &'static [&'static str] {
        &[]
    }

    fn is_handoff_tool(&self, _tool: &str) -> bool {
        false
    }

    fn writing_classes(&self) -> &'static [&'static str] {
        &[]
    }

    fn tool_classifier(&self) -> &dyn ToolClassifier {
        self
    }

    fn source_files(&self) -> Vec<SourceFile> {
        Vec::new()
    }

    fn context_program_library(&self) -> Result<ContextProgramLibrary, String> {
        Ok(ContextProgramLibrary::new())
    }

    fn agent_irs(&self, _now: OffsetDateTime) -> Result<HashMap<String, AgentIr>, String> {
        Ok(HashMap::new())
    }

    fn base_irs(&self, _now: OffsetDateTime) -> Result<HashMap<String, AgentIr>, String> {
        Ok(HashMap::new())
    }

    fn roles(&self) -> RoleDefaults {
        RoleDefaults::EMPTY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_without_installation_the_catalog_is_empty() {
        let empty = NoDefaults;
        assert_eq!(empty.capability("fs.read"), None);
        assert!(empty.catalog_labels().is_empty());
        assert!(empty.tool_classifier().classify("fs.read").is_none());
        assert_eq!(empty.roles(), RoleDefaults::EMPTY);
    }
}
