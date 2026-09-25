//! Re-classification of the permission manifest from a tool catalog
//! (#22 wave 2B).
//!
//! [`crate::lower_v2::lower_v2`] derives [`Permissions`] from static label
//! lists (`fs.` reads, `web.` reaches the network, …), because this crate
//! does not know which provider serves a tool. The compiler knows: it has the
//! capability catalog (`harw_registry_defaults::capability_catalog`). It
//! passes a [`ToolClassifier`] to [`reclassify_permissions`], which rebuilds
//! the tool-derived parts of the manifest from the catalog's classes and
//! leaves everything else (hosts, capabilities, spawn limits, budget,
//! required environment variables) untouched.
//!
//! A tool the classifier does not know is classified by the static label
//! lists ([`LabelClassifier`]), exactly as lowering did, and reported back to
//! the caller, which decides whether an unknown tool is an error.
//!
//! # Concurrency
//! Pure functions over owned data.

use std::collections::BTreeSet;

use crate::ir_v2::{AgentIr, NetworkMode, Permissions, WORKSPACE_WRITE_PATH};
use crate::lower_v2::{
    FS_WRITE_TOOLS, HOST_TOOL_PREFIXES, NETWORK_TOOL_PREFIXES, OTHER_WRITE_TOOLS,
    READ_TOOL_PREFIXES, SHELL_TOOL_PREFIXES, SHELL_TOOLS,
};

/// What a single tool does, as far as the rights manifest is concerned.
///
/// # Description
/// A tool may have several classes (none of the built-in tools does today).
/// All `false` means "no manifest-relevant effect" (for example
/// `parent.message` or `skills.load`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToolClasses {
    /// Reads the workspace file tree.
    pub read: bool,
    /// Writes into the workspace file tree (`fs.write`, `fs.edit`).
    pub write_workspace: bool,
    /// Writes outside the workspace file tree (proposals, build outputs).
    pub write_other: bool,
    /// Reaches the network.
    pub network: bool,
    /// Starts processes.
    pub shell: bool,
    /// Acts on the host outside the sandbox.
    pub host: bool,
}

/// Maps a tool name to its [`ToolClasses`].
pub trait ToolClassifier {
    /// The classes of `tool`, or `None` if the classifier does not know it.
    fn classify(&self, tool: &str) -> Option<ToolClasses>;
}

/// The static label lists lowering uses (the fallback classifier).
#[derive(Debug, Clone, Copy, Default)]
pub struct LabelClassifier;

impl ToolClassifier for LabelClassifier {
    fn classify(&self, tool: &str) -> Option<ToolClasses> {
        let has_prefix = |prefixes: &[&str]| prefixes.iter().any(|p| tool.starts_with(p));
        let write_workspace = FS_WRITE_TOOLS.contains(&tool);
        Some(ToolClasses {
            read: has_prefix(READ_TOOL_PREFIXES) && !write_workspace,
            write_workspace,
            write_other: OTHER_WRITE_TOOLS.contains(&tool),
            network: has_prefix(NETWORK_TOOL_PREFIXES),
            shell: SHELL_TOOLS.contains(&tool) || has_prefix(SHELL_TOOL_PREFIXES),
            host: has_prefix(HOST_TOOL_PREFIXES),
        })
    }
}

/// Rebuilds the tool-derived parts of `permissions` for `effective_tools`.
///
/// # Description
/// `tools`, `filesystem.{read,write,write_paths,other_write_tools}`,
/// `network.{mode,tools}`, `shell` and `host` are recomputed; `network.hosts`,
/// `capabilities`, `spawn`, `budget`, `required_env` and `forbidden_tools`
/// stay as they are. `workspace_hint` is the spawn workspace hint (the write
/// path when a workspace-writing tool is admitted).
///
/// # Returns
/// The tools the classifier did not know (sorted); they were classified by
/// [`LabelClassifier`].
pub fn reclassify(
    permissions: &mut Permissions,
    effective_tools: &[String],
    workspace_hint: Option<&str>,
    classifier: &dyn ToolClassifier,
) -> Vec<String> {
    let tools: Vec<String> = effective_tools
        .iter()
        .cloned()
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect();
    let mut unknown = Vec::new();
    let mut read = false;
    let mut write = false;
    let mut other_write_tools = Vec::new();
    let mut network_tools = Vec::new();
    let mut shell = false;
    let mut host = false;
    for tool in &tools {
        let classes = classifier.classify(tool).unwrap_or_else(|| {
            unknown.push(tool.clone());
            LabelClassifier.classify(tool).unwrap_or_default()
        });
        read |= classes.read;
        write |= classes.write_workspace;
        if classes.write_other {
            other_write_tools.push(tool.clone());
        }
        if classes.network {
            network_tools.push(tool.clone());
        }
        shell |= classes.shell;
        host |= classes.host;
    }
    permissions.filesystem.read = read;
    permissions.filesystem.write = write;
    permissions.filesystem.write_paths = if write {
        vec![workspace_hint.unwrap_or(WORKSPACE_WRITE_PATH).to_owned()]
    } else {
        Vec::new()
    };
    permissions.filesystem.other_write_tools = other_write_tools;
    permissions.network.mode = if network_tools.is_empty() {
        NetworkMode::Off
    } else {
        NetworkMode::Allowlist
    };
    permissions.network.tools = network_tools;
    permissions.shell = shell;
    permissions.host = host;
    permissions.tools = tools;
    unknown
}

/// Re-classifies the manifest of `ir` from its tool surface.
///
/// # Description
/// The effective tools are `tools.admitted` minus `tools.forbidden` (as in
/// lowering); `forbidden_tools` is refreshed from `tools.forbidden`. The v7
/// snapshot is **not** recomputed; the caller does that once all changes to
/// the IR are done ([`AgentIr::with_snapshot`]).
///
/// # Returns
/// The tools the classifier did not know (sorted).
pub fn reclassify_permissions(ir: &mut AgentIr, classifier: &dyn ToolClassifier) -> Vec<String> {
    let effective: Vec<String> = ir
        .tools
        .admitted
        .iter()
        .filter(|tool| !ir.tools.forbidden.contains(tool))
        .cloned()
        .collect();
    ir.permissions.forbidden_tools = ir
        .tools
        .forbidden
        .iter()
        .cloned()
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect();
    let hint = ir.spawn.workspace_hint.clone();
    reclassify(
        &mut ir.permissions,
        &effective,
        hint.as_deref(),
        classifier,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir_v2::{FilesystemPermissions, NetworkPermissions, SpawnPermissions};

    fn empty() -> Permissions {
        Permissions {
            tools: Vec::new(),
            forbidden_tools: Vec::new(),
            capabilities: Vec::new(),
            filesystem: FilesystemPermissions::default(),
            network: NetworkPermissions {
                mode: NetworkMode::Off,
                tools: Vec::new(),
                hosts: vec!["docs.rs".to_owned()],
            },
            shell: false,
            host: false,
            spawn: SpawnPermissions::default(),
            budget: None,
            required_env: vec!["KEY".to_owned()],
        }
    }

    struct OnlyShell;
    impl ToolClassifier for OnlyShell {
        fn classify(&self, tool: &str) -> Option<ToolClasses> {
            (tool == "custom.run").then_some(ToolClasses {
                shell: true,
                ..ToolClasses::default()
            })
        }
    }

    #[test]
    fn test_label_classifier_matches_lowering_lists() {
        let classes = LabelClassifier.classify("fs.write").unwrap_or_default();
        assert!(classes.write_workspace && !classes.read);
        assert!(LabelClassifier.classify("fs.read").unwrap_or_default().read);
        assert!(LabelClassifier.classify("web.fetch").unwrap_or_default().network);
        assert!(LabelClassifier.classify("job.start").unwrap_or_default().shell);
        assert!(LabelClassifier.classify("host.sudo_exec").unwrap_or_default().host);
        assert_eq!(
            LabelClassifier.classify("parent.message"),
            Some(ToolClasses::default())
        );
    }

    #[test]
    fn test_reclassify_uses_the_classifier_and_reports_unknown_tools() {
        let mut permissions = empty();
        let tools = vec!["custom.run".to_owned(), "fs.write".to_owned()];
        let unknown = reclassify(&mut permissions, &tools, None, &OnlyShell);
        assert_eq!(unknown, ["fs.write"]);
        assert!(permissions.shell, "classified by the catalog");
        assert!(permissions.filesystem.write, "fallback to the label lists");
        assert_eq!(permissions.filesystem.write_paths, [WORKSPACE_WRITE_PATH]);
        assert_eq!(permissions.tools, ["custom.run", "fs.write"]);
        assert_eq!(permissions.network.hosts, ["docs.rs"], "hosts untouched");
        assert_eq!(permissions.required_env, ["KEY"], "env untouched");
    }
}
