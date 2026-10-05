//! Capability catalog: tool name → provider, capability class and the cargo
//! feature that links the provider into a compiled agent (#22 wave 2B,
//! composition contract §6).
//!
//! # Responsibility
//! One static table that answers, for every tool a harw agent can be
//! admitted, three questions the agent compiler (`harw-agent-compiler`)
//! needs:
//! - **Which provider serves it?** [`ToolProvider`]: a stable provider id,
//!   the crate that implements it and the cargo feature of
//!   `harw-agent-runner` that links it. `ReachableTools` collects the
//!   providers of a manifest; the native backend enables exactly their
//!   features.
//! - **What does it do to the rights manifest?** [`CapabilityClass`], which
//!   maps to [`harw_agent_dsl::classify::ToolClasses`] through
//!   [`CapabilityCatalog`]'s [`ToolClassifier`] implementation. The
//!   compiler's `RightsCheck` re-classifies the IR's permission manifest from
//!   here instead of from the static label lists of the lowering.
//! - **Is it always there?** `always_available` marks tools every compiled
//!   runner serves without a provider feature of its own (the agent core:
//!   delegation, child messaging, the skill catalog).
//!
//! The catalog covers every tool of [`RegistryProfile::registered_tool_names`]
//! for every profile plus the tools the composition root synthesizes outside
//! the profiles (`transfer_to_*`, `agents.*`, `agent.*`, `skills.*`,
//! `job.*`, `parent.message`, `delegate_wave`, knowledge, plan, matrix,
//! sudo). The test `test_every_registered_tool_is_in_the_catalog` keeps it
//! complete; a new tool without a catalog row fails it.
//!
//! # Feature names
//! The feature strings are the contract with `harw-agent-runner` (wave 3):
//! the runner crate must define one cargo feature per distinct
//! [`ToolProvider::feature`] (see [`PROVIDER_FEATURES`]). A feature name is
//! never renamed without a matching runner change.
//!
//! # Concurrency
//! Static data, `Send + Sync`.

use harw_agent_dsl::classify::{ToolClasses, ToolClassifier};

/// What a tool does, as one class per tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CapabilityClass {
    /// Reads the workspace (files, documents, dependency sources, code graph).
    Read,
    /// Writes the workspace file tree.
    Write,
    /// Writes outside the workspace file tree (proposals, build outputs).
    WriteOther,
    /// Reaches the network.
    Network,
    /// Starts or controls processes inside the sandbox.
    Shell,
    /// Acts on the host outside the sandbox.
    Host,
    /// Delegation and communication with parent or child agents.
    Agent,
    /// Reads or writes the session's knowledge stores (workbench, diary,
    /// palace, kanban).
    Knowledge,
    /// Planning and questions to the user.
    Interaction,
    /// Reads catalog metadata (skills, definitions) without workspace access.
    Meta,
}

impl CapabilityClass {
    /// Lowercase label (`"read"`, `"write"`, …).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::WriteOther => "write-other",
            Self::Network => "network",
            Self::Shell => "shell",
            Self::Host => "host",
            Self::Agent => "agent",
            Self::Knowledge => "knowledge",
            Self::Interaction => "interaction",
            Self::Meta => "meta",
        }
    }

    /// The manifest classes of this capability class.
    #[must_use]
    pub const fn tool_classes(self) -> ToolClasses {
        let none = ToolClasses {
            read: false,
            write_workspace: false,
            write_other: false,
            network: false,
            shell: false,
            host: false,
        };
        match self {
            Self::Read => ToolClasses { read: true, ..none },
            Self::Write => ToolClasses {
                write_workspace: true,
                ..none
            },
            Self::WriteOther => ToolClasses {
                write_other: true,
                ..none
            },
            Self::Network => ToolClasses {
                network: true,
                ..none
            },
            Self::Shell => ToolClasses {
                shell: true,
                ..none
            },
            Self::Host => ToolClasses { host: true, ..none },
            Self::Agent | Self::Knowledge | Self::Interaction | Self::Meta => none,
        }
    }
}

/// A tool provider: who implements a group of tools and which runner feature
/// links it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ToolProvider {
    /// Stable provider id (`"fs"`, `"web"`, …).
    pub id: &'static str,
    /// Implementing crate.
    pub crate_name: &'static str,
    /// Cargo feature of `harw-agent-runner` that links the provider.
    pub feature: &'static str,
}

/// How a catalog row matches tool names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPattern {
    /// Exactly this name.
    Exact(&'static str),
    /// Every name with this prefix (`transfer_to_`).
    Prefix(&'static str),
}

impl ToolPattern {
    /// `true` if `tool` matches.
    #[must_use]
    pub fn matches(self, tool: &str) -> bool {
        match self {
            Self::Exact(name) => tool == name,
            Self::Prefix(prefix) => tool.len() > prefix.len() && tool.starts_with(prefix),
        }
    }

    /// The name or the prefix followed by `*`.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Exact(name) => name.to_owned(),
            Self::Prefix(prefix) => format!("{prefix}*"),
        }
    }
}

/// One catalog row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityEntry {
    /// Which tools the row describes.
    pub pattern: ToolPattern,
    /// The provider.
    pub provider: &'static ToolProvider,
    /// The capability class.
    pub class: CapabilityClass,
    /// Served by every runner without a provider feature of its own.
    pub always_available: bool,
}

impl CapabilityEntry {
    /// The cargo feature that links this tool's provider.
    #[must_use]
    pub fn feature(&self) -> &'static str {
        self.provider.feature
    }
}

macro_rules! provider {
    ($name:ident, $id:literal, $krate:literal, $feature:literal) => {
        #[doc = concat!("Provider `", $id, "` (`", $krate, "`, feature `", $feature, "`).")]
        pub const $name: ToolProvider = ToolProvider {
            id: $id,
            crate_name: $krate,
            feature: $feature,
        };
    };
}

/// The providers.
pub mod providers {
    use super::ToolProvider;

    provider!(FS, "fs", "harw-tool-fs", "tool-fs");
    provider!(DOC, "doc", "harw-tool-doc", "tool-doc");
    provider!(EXPLORE, "explore", "harw-tool-explorer", "tool-explorer");
    provider!(DEPS, "deps", "harw-tool-deps", "tool-deps");
    provider!(LENS, "lens", "harw-tool-lens", "tool-lens");
    provider!(OBSIDIAN, "obsidian", "harw-tool-obsidian", "tool-obsidian");
    provider!(WEB, "web", "harw-tool-web", "tool-web");
    provider!(BROWSER, "browser", "harw-tool-browser", "tool-browser");
    provider!(SHELL, "shell", "harw-tool-shell", "tool-shell");
    provider!(LATEX, "latex", "harw-tool-shell", "tool-latex");
    provider!(SUDO, "sudo", "harw-tool-shell", "tool-sudo");
    provider!(JOB, "job", "harw-tool-job", "tool-job");
    provider!(PROCESS, "process", "harw-tool-process", "tool-process");
    provider!(PLAN, "plan", "harw-tool-plan", "tool-plan");
    provider!(TUNNEL, "tunnel", "harw-tool-tunnel", "tool-tunnel");
    provider!(
        KNOWLEDGE,
        "knowledge",
        "harw-registry-defaults",
        "knowledge"
    );
    provider!(
        AUTHORING,
        "authoring",
        "harw-registry-defaults",
        "authoring"
    );
    provider!(MATRIX, "matrix", "harw-matrix-game", "matrix");
    provider!(SKILLS, "skills", "harw-registry-defaults", "core");
    provider!(AGENTS, "agents", "harw-core", "core");
}

/// Every distinct runner feature a provider needs, sorted. `core` is always
/// on in a runner (it carries the agent loop itself).
pub const PROVIDER_FEATURES: &[&str] = &[
    "authoring",
    "core",
    "knowledge",
    "matrix",
    "tool-browser",
    "tool-deps",
    "tool-doc",
    "tool-explorer",
    "tool-fs",
    "tool-job",
    "tool-latex",
    "tool-lens",
    "tool-obsidian",
    "tool-plan",
    "tool-process",
    "tool-shell",
    "tool-sudo",
    "tool-web",
];

/// The runner feature that is always enabled.
pub const CORE_FEATURE: &str = "core";

/// `work_driver.enqueue` (R14, `harw-ops/src/work_driver.rs`): starts a
/// durable WorkDriver run.
pub const WORK_DRIVER_ENQUEUE_TOOL: &str = "work_driver.enqueue";
/// `work_driver.status`: reads a WorkDriver job and its state sidecar.
pub const WORK_DRIVER_STATUS_TOOL: &str = "work_driver.status";
/// `work_driver.stop`: cancels a WorkDriver job through the job cancel path.
pub const WORK_DRIVER_STOP_TOOL: &str = "work_driver.stop";
/// The three WorkDriver tools.
pub const WORK_DRIVER_TOOLS: &[&str] = &[
    WORK_DRIVER_ENQUEUE_TOOL,
    WORK_DRIVER_STATUS_TOOL,
    WORK_DRIVER_STOP_TOOL,
];

macro_rules! row {
    ($tool:literal, $provider:ident, $class:ident) => {
        CapabilityEntry {
            pattern: ToolPattern::Exact($tool),
            provider: &providers::$provider,
            class: CapabilityClass::$class,
            always_available: false,
        }
    };
    ($tool:literal, $provider:ident, $class:ident, always) => {
        CapabilityEntry {
            pattern: ToolPattern::Exact($tool),
            provider: &providers::$provider,
            class: CapabilityClass::$class,
            always_available: true,
        }
    };
}

/// The catalog rows. Exact rows come first; prefix rows are consulted only
/// when no exact row matches.
pub const CATALOG: &[CapabilityEntry] = &[
    // fs
    row!("fs.read", FS, Read),
    row!("fs.list", FS, Read),
    row!("fs.search", FS, Read),
    row!("fs.glob", FS, Read),
    row!("fs.grep", FS, Read),
    row!("fs.write", FS, Write),
    row!("fs.edit", FS, Write),
    // doc
    row!("doc.read_pdf", DOC, Read),
    // explore
    row!("explore.tree", EXPLORE, Read),
    row!("explore.projects", EXPLORE, Read),
    row!("explore.relations", EXPLORE, Read),
    row!("explore.find", EXPLORE, Read),
    // deps
    row!("deps.graph", DEPS, Read),
    row!("deps.locked", DEPS, Read),
    row!("deps.source_read", DEPS, Read),
    row!("deps.source_search", DEPS, Read),
    row!("deps.source_list", DEPS, Read),
    // lens
    row!("lens.ask", LENS, Read),
    // obsidian
    row!("obsidian.map", OBSIDIAN, Read),
    row!("obsidian.read", OBSIDIAN, Read),
    row!("obsidian.search", OBSIDIAN, Read),
    row!("obsidian.links", OBSIDIAN, Read),
    row!("obsidian.write", OBSIDIAN, Write),
    // web
    row!("web.fetch", WEB, Network),
    row!("web.docs_rs", WEB, Network),
    row!("web.crates_io", WEB, Network),
    row!("web.search", WEB, Network),
    // browser
    row!("browser.open", BROWSER, Network),
    row!("browser.observe", BROWSER, Network),
    row!("browser.find", BROWSER, Network),
    row!("browser.act", BROWSER, Network),
    row!("browser.wait", BROWSER, Network),
    row!("browser.events", BROWSER, Network),
    row!("browser.close", BROWSER, Network),
    // shell, latex, sudo
    row!("shell.exec", SHELL, Shell),
    row!("latex.build", LATEX, Shell),
    row!("latex.check", LATEX, Shell),
    row!("latex.template", LATEX, WriteOther),
    row!("host.sudo_exec", SUDO, Host),
    // jobs
    row!("job.start", JOB, Shell),
    row!("job.status", JOB, Meta),
    row!("job.logs", JOB, Meta),
    row!("job.stop", JOB, Meta),
    row!("job.list", JOB, Meta),
    row!("job.wait", JOB, Meta),
    // agent compiler (#22 wave 2B): compiles an agent definition as a
    // background job, same launch path as `job.start`/`shell.exec`. Never
    // granted to a built-in role by default (no `RegistryProfile` registers
    // `crate::agent_definition_tools::AgentBuildToolProvider`); only an
    // explicit agent definition that lists `agents.build` in
    // `tools.admitted` can carry it (gate:
    // `crate::agent_definition_tools::agent_build_provider_for`).
    row!("agents.build", JOB, Shell),
    // WorkDriver (R14): `work_driver.enqueue` admits a durable background
    // job that runs the orchestrator's `[work_driver] verify` commands and
    // drives worker agents — the same launch class as `job.start` and
    // `agents.build` (`Shell`), so a manifest admitting it must carry the
    // process right. Status and stop only read or cancel a job record
    // (`Meta`, like `job.status`/`job.stop`). Provider: the agent core — the
    // operations live in `harw-ops`, the job kind in the CLI job worker; no
    // tool provider crate of its own. Not `always`: only an orchestrator
    // whose definition has `[work_driver]` can use it.
    row!("work_driver.enqueue", AGENTS, Shell),
    row!("work_driver.status", AGENTS, Meta),
    row!("work_driver.stop", AGENTS, Meta),
    // Only WorkDriver worker turns get it; it records the worker's report.
    row!("work_driver.report", AGENTS, Meta),
    // gateway (R18): operations in `harw-ops`, served to the UIA root like
    // `work_driver.*`. Reads inspect the gateway; mutations act on it outside
    // the sandbox and always ask.
    row!("gateway.status", AGENTS, Meta),
    row!("gateway.connections.list", AGENTS, Meta),
    row!("gateway.sessions.list", AGENTS, Meta),
    row!("gateway.listeners.list", AGENTS, Meta),
    row!("gateway.tools.list", AGENTS, Meta),
    row!("gateway.channels.list", AGENTS, Meta),
    row!("gateway.channels.connect_info", AGENTS, Meta),
    row!("gateway.health", AGENTS, Meta),
    row!("gateway.logs", AGENTS, Meta),
    row!("gateway.connections.revoke", AGENTS, Host),
    row!("gateway.drain", AGENTS, Host),
    row!("gateway.listeners.set", AGENTS, Host),
    row!("gateway.tools.grant", AGENTS, Host),
    row!("gateway.tools.narrow", AGENTS, Host),
    // processes
    row!("process.list", PROCESS, Shell),
    row!("process.kill", PROCESS, Shell),
    // plan mode
    row!("plan.write", PLAN, WriteOther),
    row!("plan.exit", PLAN, Interaction),
    row!("plan.enter", PLAN, Interaction),
    row!("ask_user", PLAN, Interaction),
    // knowledge stores
    row!("workbench.show", KNOWLEDGE, Knowledge),
    row!("workbench.note", KNOWLEDGE, Knowledge),
    row!("workbench.hypothesis", KNOWLEDGE, Knowledge),
    row!("diary.read", KNOWLEDGE, Knowledge),
    row!("palace.search", KNOWLEDGE, Knowledge),
    row!("palace.recall", KNOWLEDGE, Knowledge),
    row!("kanban.list", KNOWLEDGE, Knowledge),
    row!("kanban.show", KNOWLEDGE, Knowledge),
    // definition and skill authoring
    row!("agents.validate", AUTHORING, Meta),
    row!("agents.list_proposals", AUTHORING, Meta),
    row!("agents.write_definition", AUTHORING, WriteOther),
    row!("agents.write_uia", AUTHORING, WriteOther),
    row!("agents.commit_proposal", AUTHORING, WriteOther),
    row!("agents.reject_proposal", AUTHORING, WriteOther),
    row!("skills.validate", AUTHORING, Meta),
    row!("skills.list_proposals", AUTHORING, Meta),
    row!("skills.propose", AUTHORING, WriteOther),
    row!("skills.commit_proposal", AUTHORING, WriteOther),
    row!("skills.reject_proposal", AUTHORING, WriteOther),
    // matrix game
    row!("matrix.draft_scenario", MATRIX, Knowledge),
    row!("matrix.status", MATRIX, Knowledge),
    row!("matrix.add_fact", MATRIX, Knowledge),
    row!("matrix.start", MATRIX, Agent),
    row!("matrix.run", MATRIX, Agent),
    row!("matrix.finish", MATRIX, Agent),
    // skill catalog (always there)
    row!("skills.search", SKILLS, Meta, always),
    row!("skills.load", SKILLS, Meta, always),
    // agent core: delegation and messaging (always there)
    row!("delegate_wave", AGENTS, Agent, always),
    row!("agents.catalog", AGENTS, Agent, always),
    row!("agents.delegate", AGENTS, Agent, always),
    row!("agent.status", AGENTS, Agent, always),
    row!("agent.result", AGENTS, Agent, always),
    row!("agent.message", AGENTS, Agent, always),
    row!("agent.cancel", AGENTS, Agent, always),
    row!("parent.message", AGENTS, Agent, always),
    CapabilityEntry {
        pattern: ToolPattern::Prefix("transfer_to_"),
        provider: &providers::AGENTS,
        class: CapabilityClass::Agent,
        always_available: true,
    },
];

/// Tools that only make sense when the agent can spawn children
/// (`PruneUnusedTools` drops them from agents that cannot).
pub const SPAWN_TOOLS: &[&str] = &[
    "delegate_wave",
    "agents.delegate",
    "agent.status",
    "agent.result",
    "agent.message",
    "agent.cancel",
];

/// Looks up a tool: exact rows first, then prefix rows.
#[must_use]
pub fn lookup(tool: &str) -> Option<&'static CapabilityEntry> {
    CATALOG
        .iter()
        .find(|entry| matches!(entry.pattern, ToolPattern::Exact(name) if name == tool))
        .or_else(|| {
            CATALOG.iter().find(|entry| {
                matches!(entry.pattern, ToolPattern::Prefix(_)) && entry.pattern.matches(tool)
            })
        })
}

/// `true` for a delegation handoff tool (`transfer_to_<agent>`).
#[must_use]
pub fn is_handoff_tool(tool: &str) -> bool {
    ToolPattern::Prefix("transfer_to_").matches(tool)
}

/// The catalog as a [`ToolClassifier`] for
/// [`harw_agent_dsl::classify::reclassify_permissions`].
#[derive(Debug, Clone, Copy, Default)]
pub struct CapabilityCatalog;

impl ToolClassifier for CapabilityCatalog {
    fn classify(&self, tool: &str) -> Option<ToolClasses> {
        lookup(tool).map(|entry| entry.class.tool_classes())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::profile::{
        CHILD_MESSAGE_TOOLS, CHILD_RESULT_TOOLS, JOB_CONTROL_TOOLS, JOB_TOOLS, KANBAN_READ_TOOLS,
        KNOWLEDGE_READ_TOOLS, MATRIX_GAME_MASTER_TOOLS, ORCHESTRATION_TOOLS, PARENT_MESSAGE_TOOLS,
        RegistryProfile, SKILL_CATALOG_TOOLS, SUDO_TOOLS,
    };
    use crate::{
        ALWAYS_ASK_TOOLS, DiaryToolProvider, KanbanReadToolProvider, PalaceToolProvider,
        WorkbenchReadToolProvider, WorkbenchToolProvider,
    };

    /// Everything the composition root can register or synthesize.
    fn every_known_tool() -> BTreeSet<&'static str> {
        let mut tools: BTreeSet<&'static str> = BTreeSet::new();
        for profile in RegistryProfile::ALL {
            tools.extend(profile.registered_tool_names());
        }
        for list in [
            ORCHESTRATION_TOOLS,
            CHILD_RESULT_TOOLS,
            SKILL_CATALOG_TOOLS,
            CHILD_MESSAGE_TOOLS,
            PARENT_MESSAGE_TOOLS,
            SUDO_TOOLS,
            MATRIX_GAME_MASTER_TOOLS,
            KNOWLEDGE_READ_TOOLS,
            KANBAN_READ_TOOLS,
            JOB_TOOLS,
            JOB_CONTROL_TOOLS,
            crate::roster::ALWAYS_AVAILABLE_TOOLS,
            crate::profile::BROWSER_TOOLS,
            crate::profile::LATEX_TOOLS,
            crate::profile::WEB_TOOLS,
            crate::profile::AGENT_DEFINITION_WRITE_TOOLS,
            crate::skill_proposal_tools::SKILL_PROPOSAL_READ_TOOLS,
            crate::skill_proposal_tools::SKILL_PROPOSAL_PROPOSE_TOOLS,
            crate::skill_proposal_tools::SKILL_PROPOSAL_DECIDE_TOOLS,
            WorkbenchToolProvider::TOOL_NAMES,
            WorkbenchReadToolProvider::TOOL_NAMES,
            DiaryToolProvider::TOOL_NAMES,
            PalaceToolProvider::TOOL_NAMES,
            KanbanReadToolProvider::TOOL_NAMES,
            harw_tool_plan::PlanToolProvider::TOOL_NAMES,
            ALWAYS_ASK_TOOLS,
        ] {
            tools.extend(list.iter().copied());
        }
        tools.insert("agents.catalog");
        tools.insert("agents.delegate");
        tools.insert("agent.status");
        tools.insert("agent.cancel");
        tools
    }

    #[test]
    fn test_every_registered_tool_is_in_the_catalog() {
        let missing: Vec<&str> = every_known_tool()
            .into_iter()
            .filter(|tool| lookup(tool).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "tools without a catalog row: {missing:?}"
        );
    }

    /// Parity: for every catalog provider that is backed by a Rust tool
    /// provider exposing `TOOL_NAMES`, the exact catalog rows of that provider
    /// are the union of those names (no row without a tool, no tool without a
    /// row). Providers whose tools are synthesized by the composition root
    /// (agents, authoring, sudo, latex, matrix, ...) are not listed here.
    #[test]
    fn test_catalog_rows_match_provider_tool_names() -> Result<(), String> {
        let parity: [(&ToolProvider, &[&[&str]]); 11] = [
            (
                &providers::DOC,
                &[harw_tool_doc::DocToolProvider::TOOL_NAMES],
            ),
            (
                &providers::EXPLORE,
                &[harw_tool_explorer::ExplorerToolProvider::TOOL_NAMES],
            ),
            (
                &providers::DEPS,
                &[harw_tool_deps::DepsToolProvider::TOOL_NAMES],
            ),
            (
                &providers::LENS,
                &[harw_tool_lens::LensToolProvider::TOOL_NAMES],
            ),
            (
                &providers::OBSIDIAN,
                &[harw_tool_obsidian::ObsidianToolProvider::TOOL_NAMES],
            ),
            (
                &providers::WEB,
                &[harw_tool_web::WebToolProvider::TOOL_NAMES],
            ),
            (&providers::JOB, &[&harw_tool_job::JOB_TOOL_NAMES]),
            (
                &providers::PROCESS,
                &[harw_tool_process::ProcessToolProvider::TOOL_NAMES],
            ),
            (
                &providers::PLAN,
                &[harw_tool_plan::PlanToolProvider::TOOL_NAMES],
            ),
            (
                &providers::KNOWLEDGE,
                &[
                    WorkbenchToolProvider::TOOL_NAMES,
                    WorkbenchReadToolProvider::TOOL_NAMES,
                    DiaryToolProvider::TOOL_NAMES,
                    PalaceToolProvider::TOOL_NAMES,
                    KanbanReadToolProvider::TOOL_NAMES,
                ],
            ),
            (
                &providers::SKILLS,
                &[crate::SkillCatalogToolProvider::TOOL_NAMES],
            ),
        ];
        // Rows that name a provider's feature but are synthesized by the
        // composition root, not served by that provider's `TOOL_NAMES`.
        // `agents.build` runs a build process, hence the `job` feature.
        const SYNTHESIZED_ROWS: &[&str] = &["agents.build"];
        let mut problems = Vec::new();
        for tool in SYNTHESIZED_ROWS {
            if lookup(tool).is_none() {
                problems.push(format!("stale SYNTHESIZED_ROWS entry {tool}"));
            }
        }
        for (provider, name_lists) in parity {
            let expected: BTreeSet<&str> = name_lists
                .iter()
                .flat_map(|names| names.iter().copied())
                .collect();
            let rows: BTreeSet<&str> = CATALOG
                .iter()
                .filter(|entry| entry.provider.id == provider.id)
                .filter_map(|entry| match entry.pattern {
                    ToolPattern::Exact(name) if !SYNTHESIZED_ROWS.contains(&name) => Some(name),
                    ToolPattern::Exact(_) | ToolPattern::Prefix(_) => None,
                })
                .collect();
            let missing_row: Vec<&&str> = expected.difference(&rows).collect();
            let missing_tool: Vec<&&str> = rows.difference(&expected).collect();
            if !missing_row.is_empty() || !missing_tool.is_empty() {
                problems.push(format!(
                    "provider {}: tools without catalog row {missing_row:?}, \
                     catalog rows without tool {missing_tool:?}",
                    provider.id
                ));
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("; "))
        }
    }

    #[test]
    fn test_every_builtin_definition_tool_is_in_the_catalog() -> Result<(), String> {
        let irs = crate::embedded_agents::builtin_agent_irs(time::OffsetDateTime::UNIX_EPOCH)
            .map_err(|error| error.to_string())?;
        for (name, ir) in &irs {
            for tool in ir.tools.admitted.iter().chain(&ir.tools.forbidden) {
                assert!(lookup(tool).is_some(), "{name}: {tool} has no catalog row");
            }
        }
        Ok(())
    }

    #[test]
    fn test_catalog_rows_are_unique_and_features_are_declared() {
        let mut seen = BTreeSet::new();
        for entry in CATALOG {
            assert!(
                seen.insert(entry.pattern.label()),
                "duplicate row {}",
                entry.pattern.label()
            );
            assert!(
                PROVIDER_FEATURES.contains(&entry.feature()),
                "{}: feature {} not in PROVIDER_FEATURES",
                entry.pattern.label(),
                entry.feature()
            );
        }
        let mut sorted = PROVIDER_FEATURES.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, PROVIDER_FEATURES, "PROVIDER_FEATURES stays sorted");
    }

    #[test]
    fn test_prefix_rows_match_handoffs_only() {
        assert_eq!(
            lookup("transfer_to_explorer").map(|entry| entry.provider.id),
            Some("agents")
        );
        assert!(
            lookup("transfer_to_").is_none(),
            "an empty target is no tool"
        );
        assert!(is_handoff_tool("transfer_to_executor"));
        assert!(!is_handoff_tool("fs.read"));
        assert!(lookup("fs.unknown").is_none());
    }

    #[test]
    fn test_classes_agree_with_tool_permission_for_rights_bearing_tools() {
        use harw_authority::Permission;
        for entry in CATALOG {
            let ToolPattern::Exact(tool) = entry.pattern else {
                continue;
            };
            match crate::tool_permission(tool) {
                Some(Permission::WriteWorkspace) => assert!(
                    matches!(
                        entry.class,
                        CapabilityClass::Write | CapabilityClass::WriteOther
                    ),
                    "{tool}: write permission but class {:?}",
                    entry.class
                ),
                Some(Permission::ExecuteProcess) => assert!(
                    matches!(entry.class, CapabilityClass::Shell | CapabilityClass::Host),
                    "{tool}: process permission but class {:?}",
                    entry.class
                ),
                Some(Permission::NetworkAccess) => assert_eq!(
                    entry.class,
                    CapabilityClass::Network,
                    "{tool}: network permission"
                ),
                _ => {}
            }
        }
    }

    #[test]
    fn test_catalog_classifier_feeds_the_dsl_reclassification() {
        let classes = CapabilityCatalog.classify("job.start").unwrap_or_default();
        assert!(classes.shell);
        let classes = CapabilityCatalog
            .classify("latex.template")
            .unwrap_or_default();
        assert!(classes.write_other && !classes.write_workspace);
        assert_eq!(CapabilityCatalog.classify("nope.tool"), None);
    }

    /// #22 Welle 2B: `agents.build` hat eine Katalogzeile, deren Klasse
    /// mindestens so streng ist wie die von `shell.exec` (beide `Shell` —
    /// `Ord`-Reihenfolge in [`CapabilityClass`]: `Shell` vor `Host`, danach
    /// nur noch rechtlose Klassen).
    #[test]
    fn test_agents_build_is_at_least_as_strict_as_shell_exec() {
        let shell_exec_class = lookup("shell.exec").map(|entry| entry.class);
        let agents_build_class = lookup("agents.build").map(|entry| entry.class);
        assert_eq!(shell_exec_class, Some(CapabilityClass::Shell));
        assert!(
            agents_build_class.is_some_and(|class| class >= CapabilityClass::Shell),
            "agents.build: Klasse {agents_build_class:?} ist weniger streng als shell.exec"
        );
    }

    /// #22 Welle 2B: kein eingebautes Profil darf `agents.build` von sich aus
    /// bewerben — nur eine ausdrückliche Agentendefinition, die es in
    /// `tools.admitted` aufführt, darf es bekommen.
    #[test]
    fn test_agents_build_is_not_granted_to_any_builtin_role_by_default() {
        for profile in RegistryProfile::ALL {
            assert!(
                !profile.registered_tool_names().contains(&"agents.build"),
                "{profile:?} bewirbt agents.build von sich aus"
            );
        }
    }

    /// R14: the WorkDriver tools have catalog rows; `enqueue` starts durable
    /// work and is at least as strict as `job.start`, status/stop are
    /// rights-free `Meta` like `job.status`/`job.stop`; none is auto-approved
    /// or served by every runner by default.
    #[test]
    fn test_work_driver_tools_are_classified() {
        let class = |tool: &str| lookup(tool).map(|entry| entry.class);
        assert_eq!(WORK_DRIVER_TOOLS.len(), 3);
        assert_eq!(
            class(WORK_DRIVER_ENQUEUE_TOOL),
            Some(CapabilityClass::Shell)
        );
        assert!(
            class(WORK_DRIVER_ENQUEUE_TOOL) >= class("job.start"),
            "work_driver.enqueue must be at least as strict as job.start"
        );
        assert_eq!(class(WORK_DRIVER_STATUS_TOOL), Some(CapabilityClass::Meta));
        assert_eq!(class(WORK_DRIVER_STOP_TOOL), Some(CapabilityClass::Meta));

        let enqueue = CapabilityCatalog
            .classify(WORK_DRIVER_ENQUEUE_TOOL)
            .unwrap_or_default();
        assert!(
            enqueue.shell,
            "enqueue needs the process right in a manifest"
        );
        assert!(!enqueue.write_workspace && !enqueue.network && !enqueue.host);
        for tool in [WORK_DRIVER_STATUS_TOOL, WORK_DRIVER_STOP_TOOL] {
            assert_eq!(
                CapabilityCatalog.classify(tool),
                Some(CapabilityClass::Meta.tool_classes()),
                "{tool} carries no manifest right"
            );
        }
        for tool in WORK_DRIVER_TOOLS {
            let entry = lookup(tool);
            assert!(
                entry.is_some_and(|entry| !entry.always_available),
                "{tool}: only orchestrators with [work_driver] get it"
            );
            assert_eq!(entry.map(CapabilityEntry::feature), Some(CORE_FEATURE));
            assert!(
                !crate::AUTO_APPROVED_TOOLS.contains(tool),
                "{tool} must not be auto-approved"
            );
        }
    }

    /// #22 Welle 2B: `agents.build` ist nicht in [`crate::AUTO_APPROVED_TOOLS`]
    /// — es fragt wie `shell.exec` immer nach Freigabe.
    #[test]
    fn test_agents_build_requires_approval_like_shell_exec() {
        assert!(!crate::AUTO_APPROVED_TOOLS.contains(&"shell.exec"));
        assert!(!crate::AUTO_APPROVED_TOOLS.contains(&"agents.build"));
    }
}
