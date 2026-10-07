//! `ObsidianToolProvider` — bundles the five tools of this crate.
//!
//! # Responsibility
//! Contains no logic of its own: the provider structure with `tools()`,
//! `executor(name)` and `TOOL_PERMISSIONS` comes from
//! [`harw_tools::tool_provider!`] over the generated executors in
//! `lib.rs`. Names, specs and permissions come exclusively from the
//! `#[harw_macros::tool]` attributes there — no second source of truth.
//!
//! # Permissions
//! `obsidian.map`, `obsidian.read`, `obsidian.search`, `obsidian.links`
//! declare `ReadWorkspace`; `obsidian.write` declares `WriteWorkspace`.
//! The permission registry (`harw-registry-defaults` `authority.rs`) picks
//! both up through `TOOL_NAMES`/`TOOL_PERMISSIONS`, exactly like the other
//! providers.

use crate::{
    ObsidianLinksTool, ObsidianMapTool, ObsidianReadTool, ObsidianSearchTool, ObsidianWriteTool,
};

harw_tools::tool_provider! {
    /// Provides the five Obsidian vault tools.
    pub struct ObsidianToolProvider {
        ObsidianMapTool,
        ObsidianReadTool,
        ObsidianSearchTool,
        ObsidianLinksTool,
        ObsidianWriteTool,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_authority::Permission;
    use harw_extension_api::contributors::ToolProvider as _;
    use harw_tools::ToolName;

    #[test]
    fn provider_lists_exactly_five_tools() {
        let provider = ObsidianToolProvider::new();
        let tools = provider.tools();
        let names: Vec<&str> = tools.iter().map(|s| s.name()).collect();
        assert_eq!(
            names,
            vec![
                "obsidian.map",
                "obsidian.read",
                "obsidian.search",
                "obsidian.links",
                "obsidian.write",
            ]
        );
    }

    #[test]
    fn provider_declares_expected_permissions() {
        let permissions: Vec<Option<Permission>> = ObsidianToolProvider::TOOL_PERMISSIONS.to_vec();
        assert_eq!(
            permissions,
            vec![
                Some(Permission::ReadWorkspace),
                Some(Permission::ReadWorkspace),
                Some(Permission::ReadWorkspace),
                Some(Permission::ReadWorkspace),
                Some(Permission::WriteWorkspace),
            ]
        );
    }

    #[test]
    fn provider_resolves_known_and_rejects_unknown() {
        let provider = ObsidianToolProvider::new();
        assert!(provider.executor(&ToolName::new("obsidian.read")).is_some());
        assert!(provider.executor(&ToolName::new("obsidian.write")).is_some());
        assert!(provider.executor(&ToolName::new("obsidian.delete")).is_none());
    }
}
