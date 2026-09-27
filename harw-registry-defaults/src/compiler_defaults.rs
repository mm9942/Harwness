//! The built-in defaults of the agent compiler
//! ([`harw_agent_compiler::builtins::BuiltinDefaults`]).
//!
//! # Responsibility
//! `harw-agent-compiler` (layer C) states what it reads from the composition
//! layer — the embedded role and base definitions, the context-program
//! library, the [capability catalog](crate::capability_catalog) and the
//! [roster](crate::roster)'s generic ceilings — as a trait, and this crate
//! implements it ([`RegistryCompilerDefaults`]). The dependency points from
//! here to the compiler, not back (§6, §35).
//!
//! # Use
//! Every program that compiles agents calls [`install`] once before the
//! first compile (the `harw` binary at start, tests before building a
//! compiler). Installing twice is harmless.
//!
//! # Concurrency
//! Static data, `Send + Sync`.

use std::collections::HashMap;

use harw_agent_compiler::builtins::{
    BuiltinDefaults, RoleDefaults, ToolCapability, install_builtin_defaults,
};
use harw_agent_dsl::AgentIr;
use harw_agent_dsl::bind::ContextProgramLibrary;
use harw_agent_dsl::classify::ToolClassifier;
use harw_agent_dsl::diagnostics::SourceFile;
use time::OffsetDateTime;

use crate::capability_catalog::{self, CapabilityCatalog, CapabilityClass, ToolPattern};
use crate::{embedded_agents, roster};

/// The compiler's built-in defaults from this crate's static data.
#[derive(Debug, Clone, Copy, Default)]
pub struct RegistryCompilerDefaults;

/// The one instance [`install`] registers.
pub static COMPILER_DEFAULTS: RegistryCompilerDefaults = RegistryCompilerDefaults;

/// Capability classes that write, run processes or act on the host.
const WRITING_CLASSES: &[&str] = &[
    CapabilityClass::Write.as_str(),
    CapabilityClass::WriteOther.as_str(),
    CapabilityClass::Shell.as_str(),
    CapabilityClass::Host.as_str(),
];

/// Installs [`COMPILER_DEFAULTS`] for this process (see the module docs).
pub fn install() {
    // `false` only means an installation already exists; every caller
    // installs this same data.
    let _installed_now = install_builtin_defaults(&COMPILER_DEFAULTS);
}

impl BuiltinDefaults for RegistryCompilerDefaults {
    fn capability(&self, tool: &str) -> Option<ToolCapability> {
        capability_catalog::lookup(tool).map(|entry| ToolCapability {
            provider_id: entry.provider.id,
            crate_name: entry.provider.crate_name,
            feature: entry.feature(),
            class: entry.class.as_str(),
            always_available: entry.always_available,
        })
    }

    fn catalog_labels(&self) -> Vec<String> {
        capability_catalog::CATALOG
            .iter()
            .map(|entry| entry.pattern.label())
            .collect()
    }

    fn catalog_tools(&self) -> Vec<&'static str> {
        capability_catalog::CATALOG
            .iter()
            .filter_map(|entry| match entry.pattern {
                ToolPattern::Exact(tool) => Some(tool),
                ToolPattern::Prefix(_) => None,
            })
            .collect()
    }

    fn core_feature(&self) -> &'static str {
        capability_catalog::CORE_FEATURE
    }

    fn spawn_tools(&self) -> &'static [&'static str] {
        capability_catalog::SPAWN_TOOLS
    }

    fn is_handoff_tool(&self, tool: &str) -> bool {
        capability_catalog::is_handoff_tool(tool)
    }

    fn writing_classes(&self) -> &'static [&'static str] {
        WRITING_CLASSES
    }

    fn tool_classifier(&self) -> &dyn ToolClassifier {
        &CapabilityCatalog
    }

    fn source_files(&self) -> Vec<SourceFile> {
        embedded_agents::builtin_source_files()
    }

    fn context_program_library(&self) -> Result<ContextProgramLibrary, String> {
        embedded_agents::builtin_context_program_library().map_err(|error| error.to_string())
    }

    fn agent_irs(&self, now: OffsetDateTime) -> Result<HashMap<String, AgentIr>, String> {
        embedded_agents::builtin_agent_irs(now).map_err(|error| error.to_string())
    }

    fn base_irs(&self, now: OffsetDateTime) -> Result<HashMap<String, AgentIr>, String> {
        embedded_agents::builtin_base_irs(now).map_err(|error| error.to_string())
    }

    fn roles(&self) -> RoleDefaults {
        RoleDefaults {
            base_definition_names: embedded_agents::BASE_DEFINITION_NAMES,
            worker_base_name: embedded_agents::WORKER_BASE_NAME,
            child_orchestrator_base_name: embedded_agents::CHILD_ORCHESTRATOR_BASE_NAME,
            generic_worker_base: roster::GENERIC_WORKER_BASE,
            generic_worker_max_depth: roster::GENERIC_WORKER_MAX_DEPTH,
            generic_child_orchestrator_base: roster::GENERIC_CHILD_ORCHESTRATOR_BASE,
            generic_uia_worker_base: roster::GENERIC_UIA_WORKER_BASE,
            generic_worker_write_tools: roster::GENERIC_WORKER_WRITE_TOOLS,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn test_capability_mirrors_the_catalog_row() -> TestResult {
        let defaults = RegistryCompilerDefaults;
        let entry = capability_catalog::lookup("fs.read").ok_or("fs.read is catalogued")?;
        let capability = defaults.capability("fs.read").ok_or("fs.read capability")?;
        assert_eq!(capability.provider_id, entry.provider.id);
        assert_eq!(capability.crate_name, entry.provider.crate_name);
        assert_eq!(capability.feature, entry.feature());
        assert_eq!(capability.class, entry.class.as_str());
        assert_eq!(capability.always_available, entry.always_available);
        assert!(
            defaults.capability("transfer_to_explorer").is_some(),
            "prefix row"
        );
        assert!(defaults.capability("no.such.tool").is_none());
        Ok(())
    }

    #[test]
    fn test_catalog_views_cover_every_row() {
        let defaults = RegistryCompilerDefaults;
        assert_eq!(
            defaults.catalog_labels().len(),
            capability_catalog::CATALOG.len()
        );
        assert!(
            defaults
                .catalog_labels()
                .contains(&"transfer_to_*".to_owned())
        );
        let tools = defaults.catalog_tools();
        assert!(tools.contains(&"fs.read"));
        assert!(!tools.iter().any(|tool| tool.starts_with("transfer_to_")));
        assert_eq!(defaults.core_feature(), capability_catalog::CORE_FEATURE);
        assert!(defaults.is_handoff_tool("transfer_to_explorer"));
        assert!(!defaults.is_handoff_tool("fs.read"));
        assert_eq!(
            defaults.writing_classes(),
            ["write", "write-other", "shell", "host"]
        );
        assert_eq!(
            defaults.tool_classifier().classify("fs.write"),
            CapabilityCatalog.classify("fs.write")
        );
    }

    #[test]
    fn test_definitions_and_role_names_come_from_the_embedded_tree() -> TestResult {
        let defaults = RegistryCompilerDefaults;
        assert_eq!(
            defaults.source_files().len(),
            embedded_agents::builtin_source_files().len()
        );
        let roles = defaults.agent_irs(OffsetDateTime::UNIX_EPOCH)?;
        let names = defaults.roles();
        assert!(roles.contains_key(names.generic_worker_base));
        assert!(roles.contains_key(names.generic_uia_worker_base));
        let bases = defaults.base_irs(OffsetDateTime::UNIX_EPOCH)?;
        assert!(bases.contains_key(names.worker_base_name));
        assert!(bases.contains_key(names.child_orchestrator_base_name));
        assert_eq!(
            names.base_definition_names,
            embedded_agents::BASE_DEFINITION_NAMES
        );
        defaults.context_program_library()?;
        Ok(())
    }
}
