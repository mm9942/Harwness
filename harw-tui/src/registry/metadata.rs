//! Presentation adapter for concrete built-in operation metadata.
//!
//! The operation macro stores one immutable `OperationMeta` per concrete type in
//! a static `OnceLock`. Compare that identity, never supplied prose or a name
//! alone: extensions may legitimately reuse any built-in command name/path.
//! The base registry also includes the private unavailable-compaction wrapper.
//! Building this reference registry only constructs metadata and unit operations;
//! it does not run operations, inspect configuration, or connect to services.

use std::sync::OnceLock;

use harw_operations::registry::OperationRegistry;
use harw_operations::{Operation, OperationMeta};

use crate::{CommandSpec, i18n::tr};

fn base_operations() -> &'static OperationRegistry {
    static BASE: OnceLock<OperationRegistry> = OnceLock::new();
    BASE.get_or_init(|| {
        let mut operations = OperationRegistry::new();
        harw_ops::register_all(&mut operations);
        operations
    })
}

// Optional runtime contributors must not become available merely because the
// presentation adapter knows them. These references establish identity only.
const OPTIONAL_OPERATIONS: &[&dyn Operation] = &[
    &harw_ops::plan::PlanOperation,
    &harw_ops::goal::GoalOperation,
    &harw_ops::explore::ExploreOperation,
    &harw_ops::research::ResearchOperation,
    &harw_ops::research::ResearchDepsOperation,
    &harw_ops::research::ResearchWebOperation,
    &harw_ops::analyze::AnalyzeOperation,
    &harw_ops::intent_cycle::IntentCycleCommandOperation,
    &harw_ops::infra::InfraStatusOperation,
    &harw_ops::infra::InfraHealthOperation,
    &harw_ops::infra::InfraAuthKeysDescribeOperation,
    &harw_ops::infra::InfraAuthKeysRotateOperation,
    &harw_ops::gateway_ops::GatewayStatusOperation,
    &harw_ops::gateway_ops::GatewayConnectionsListOperation,
    &harw_ops::gateway_ops::GatewayListenersListOperation,
    &harw_ops::gateway_ops::GatewayToolsListOperation,
    &harw_ops::gateway_ops::GatewayChannelsListOperation,
    &harw_ops::gateway_ops::GatewayConnectionsRevokeOperation,
    &harw_ops::gateway_ops::GatewayDrainOperation,
    &harw_ops::gateway_ops::GatewayHealthOperation,
    &harw_ops::gateway_ops::GatewayLogsOperation,
    &harw_ops::compact::CompactOperation,
];

fn is_builtin(meta: &OperationMeta) -> bool {
    base_operations()
        .iter()
        .any(|operation| std::ptr::eq(operation.meta(), meta))
        || OPTIONAL_OPERATIONS
            .iter()
            .any(|operation| std::ptr::eq(operation.meta(), meta))
}

/// Preserve unknown/external text and grammar, including built-in name collisions.
/// Only concrete built-ins receive the owned usage/hints and complete summaries.
pub(super) fn enrich(spec: &mut CommandSpec, meta: &OperationMeta) {
    let owns_path = meta.surfaces.iter().any(|surface| {
        matches!(surface,
        harw_operations::Surface::Command { path, .. }
            if path.strip_prefix('/') == Some(spec.name.as_str()))
    });
    if let Some(summary) = summary(meta).filter(|_| owns_path) {
        crate::command_catalog::enrich(spec);
        // Reuse the owned grammar/overrides, then supply this complete message.
        spec.summary = summary.to_owned();
    } else if spec.usage.is_empty() {
        spec.usage = format!("/{}", spec.name);
    }
}

fn summary(meta: &OperationMeta) -> Option<&'static str> {
    if !is_builtin(meta) {
        return None;
    }
    Some(match meta.name {
        "add-workdir" => tr(
            "operations.summary.add-workdir",
            "Allow an additional working directory for this session, optionally saved for the project",
        ),
        "agent" => tr(
            "commands.summary.agent",
            "Child agents: tree, list, stop, budget, stream",
        ),
        "analyze" => tr(
            "operations.summary.analyze",
            "Analyze the workspace using read-only analyst child agents",
        ),
        "approve" => tr(
            "operations.summary.approve",
            "Approve a job blocked on approval and return it to Ready",
        ),
        "attach" => tr(
            "operations.summary.attach",
            "Inspect a persistently stored job",
        ),
        "bug-report" => tr(
            "operations.summary.bug-report",
            "Save a local bug report under ~/.harw/bug-report/",
        ),
        "cancel" => tr(
            "operations.summary.cancel",
            "Cancel a running governance job in a controlled manner",
        ),
        "compact" => tr(
            "operations.summary.compact",
            "Session compaction is unavailable in this runtime",
        ),
        "context-proposal" => tr(
            "operations.summary.context-proposal",
            "Review context proposals: list, view, accept, reject; never apply them",
        ),
        "deny" => tr(
            "operations.summary.deny",
            "Permanently reject a job by cancelling it, including a Blocked job",
        ),
        "diary" => tr(
            "operations.summary.diary",
            "Read, search, and add agent diary entries",
        ),
        "diff" => tr(
            "operations.summary.diff",
            "Show the workspace's current git diff",
        ),
        "dod" => tr(
            "operations.summary.dod",
            "Inspect Harw-DoD status, findings, sensors, diagnostics, and configuration",
        ),
        "dream" => tr(
            "operations.summary.dream",
            "List, inspect, and run dreams; review their proposals",
        ),
        "effort" => tr(
            "operations.summary.effort",
            "Show or set reasoning effort for upcoming turns",
        ),
        "explore" => tr(
            "operations.summary.explore",
            "Answer a scoped question using a read-only explorer child agent",
        ),
        "export" => tr(
            "operations.summary.export",
            "Export session history through the TUI dialog with format and content options",
        ),
        "gateway.channels.list" => tr(
            "operations.summary.gateway.channels.list",
            "List configured channels and their transport and tenant binding; never show tokens",
        ),
        "gateway.connections.list" => tr(
            "operations.summary.gateway.connections.list",
            "List gateway connections and their principals, tenants, capabilities, and sessions",
        ),
        "gateway.connections.revoke" => tr(
            "operations.summary.gateway.connections.revoke",
            "Revoke a gateway connection and its running tool calls; always asks for approval",
        ),
        "gateway.drain" => tr(
            "operations.summary.gateway.drain",
            "Drain the gateway, rejecting new turns and tool calls; always asks for approval",
        ),
        "gateway.health" => tr(
            "operations.summary.gateway.health",
            "Diagnose the gateway daemon, sockets, connections, and log sizes",
        ),
        "gateway.listeners.list" => tr(
            "operations.summary.gateway.listeners.list",
            "List gateway listeners with their type, address, and enabled state",
        ),
        "gateway.logs" => tr(
            "operations.summary.gateway.logs",
            "Show filtered recent gateway logs with repeated lines grouped and secrets redacted",
        ),
        "gateway.status" => tr(
            "operations.summary.gateway.status",
            "Show gateway sessions, connections, listeners, tools, sandbox, and drain status",
        ),
        "gateway.tools.list" => tr(
            "operations.summary.gateway.tools.list",
            "List gateway tools and connected agents' tool grants",
        ),
        "goal" => tr(
            "operations.summary.goal",
            "Manage goals, criteria, and invariants; evaluate them against the plan",
        ),
        "help" => tr(
            "operations.summary.help",
            "List available slash commands with summaries",
        ),
        "infra.auth.keys.describe" => tr(
            "operations.summary.infra.auth.keys.describe",
            "Show AuthHub key metadata, never key material",
        ),
        "infra.auth.keys.rotate" => tr(
            "operations.summary.infra.auth.keys.rotate",
            "Rotate an AuthHub key irreversibly; Owner only",
        ),
        "infra.health" => tr(
            "operations.summary.infra.health",
            "Show health and versions of configured infrastructure daemons",
        ),
        "infra.status" => tr(
            "operations.summary.infra.status",
            "Show which infrastructure daemons are configured and reachable",
        ),
        "intent_cycle" => tr(
            "operations.summary.intent_cycle",
            "List intent cycles, inspect their status, or stop them",
        ),
        "jobs" => tr(
            "operations.summary.jobs",
            "List session background jobs, inspect status and logs, or stop them",
        ),
        "kanban" => tr(
            "operations.summary.kanban",
            "Manage Kanban boards, cards, progress, evidence, and worker approvals",
        ),
        "learn" => tr(
            "operations.summary.learn",
            "Create and review learning proposals from the session; never adopt them automatically",
        ),
        "matrix" => tr(
            "operations.summary.matrix",
            "View the Matrix Game; the Game Master starts and controls it",
        ),
        "memory" => tr(
            "commands.summary.memory",
            "Long-term memory: list, search, record, forget",
        ),
        "mode" => tr(
            "commands.summary.mode",
            "Show or change the interaction mode",
        ),
        "model" => tr(
            "operations.summary.model",
            "Show the current model or switch it with switch <id>",
        ),
        "models" => tr(
            "operations.summary.models",
            "Show or set models by role; takes effect next session",
        ),
        "new" => tr("operations.summary.new", "Start a new session in the TUI"),
        "palace" => tr(
            "operations.summary.palace",
            "Inspect and search the Palace graph; promote, revise, replace, or link nodes",
        ),
        "permissions" => tr(
            "operations.summary.permissions",
            "Show or change approval mode, allow/deny rules, and working directories",
        ),
        "plan" => tr(
            "operations.summary.plan",
            "Manage plans, nodes, waves, readiness, and reconciliation",
        ),
        "plugins" => tr(
            "operations.summary.plugins",
            "List or inspect plugins from the loaded configuration",
        ),
        "provider" => tr(
            "operations.summary.provider",
            "Show the active provider; list or test configured providers",
        ),
        "provider-concurrency" => tr(
            "operations.summary.provider-concurrency",
            "Show or change provider concurrency live; reduce it after repeated HTTP 429 responses",
        ),
        "ps" => tr(
            "operations.summary.ps",
            "List running jobs and child processes",
        ),
        "quit" => tr("operations.summary.quit", "End the current TUI session"),
        "research" => tr(
            "operations.summary.research",
            "Research a general question using a read-only child agent",
        ),
        "research_deps" => tr(
            "operations.summary.research_deps",
            "Research dependency versions, MSRV, and features using a read-only child agent",
        ),
        "research_web" => tr(
            "operations.summary.research_web",
            "Research a scoped question on the web using a read-only child agent",
        ),
        "retry" => tr(
            "operations.summary.retry",
            "Retry a Failed or Cancelled job according to the retry policy",
        ),
        "review" => tr(
            "operations.summary.review",
            "Inspect a governance job's approval context, budget, lease, and outcome",
        ),
        "sandbox-lease" => tr(
            "commands.summary.sandbox_lease",
            "Show or revoke the host permit for shell.exec",
        ),
        "settings" => tr("settings.title", "Settings"),
        "skills" => tr(
            "operations.summary.skills",
            "Manage the skill catalog and review skill proposals",
        ),
        "status" => tr(
            "operations.summary.status",
            "Show the current session and turn status",
        ),
        "stop" => tr(
            "operations.summary.stop",
            "Stop a running job in a controlled manner",
        ),
        "uia-effort" => tr(
            "operations.summary.uia-effort",
            "Show or pin UIA reasoning effort; takes effect next session",
        ),
        "uia-model" => tr(
            "operations.summary.uia-model",
            "Show or change the pinned UIA model independently of the default",
        ),
        "uia-provider" => tr(
            "operations.summary.uia-provider",
            "Show, list, or test the pinned UIA provider independently of the default",
        ),
        "uia-worker-model" => tr(
            "operations.summary.uia-worker-model",
            "Show or pin the UIA worker model; takes effect next session",
        ),
        "usage" => tr(
            "operations.summary.usage",
            "Show recorded token usage for the current session",
        ),
        "work" => tr(
            "operations.summary.work",
            "Show a persistent job summary; approval and diff data are unavailable locally",
        ),
        "workbench" => tr(
            "operations.summary.workbench",
            "Manage pinned files, notes, hypotheses, and retention; opens the panel without arguments",
        ),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CommandRegistry;
    use crate::i18n::{tr_for, with_language};
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_config::UiLanguage;
    use harw_operations::adapter::CommandAdapter;
    use harw_operations::{OpContext, OpFuture, OpInput, Surface};
    use std::sync::Arc;

    fn all_runtime_operations() -> TestResult<OperationRegistry> {
        let mut operations = OperationRegistry::new();
        harw_ops::register_all(&mut operations);
        let config = harw_plan::config::PlanToolConfig::enabled_defaults();
        harw_ops::register_plan_tools(&mut operations, &config);
        harw_ops::register_intent_cycle_tools(&mut operations, &config);
        harw_ops::register_work_driver_tools(&mut operations, &config);
        harw_ops::infra::register_infrastructure(&mut operations).map_err(ctx("infra"))?;
        harw_ops::gateway_ops::register_gateway(&mut operations).map_err(ctx("gateway"))?;
        harw_ops::gateway_ops::register_gateway_diagnostics(&mut operations)
            .map_err(ctx("diagnostics"))?;
        Ok(operations)
    }

    #[test]
    fn deny_summary_includes_blocked_jobs_in_cancellation() -> TestResult {
        // deny.rs selects deny_blocked for Blocked jobs and cancel otherwise;
        // both successful transitions persist Cancelled. Do not copy the
        // outdated shared summary's claim that Blocked jobs are excluded.
        let inclusion = [
            (UiLanguage::English, "including a Blocked job"),
            (UiLanguage::German, "auch einen Job im Zustand Blocked"),
            (UiLanguage::Spanish, "incluido uno en estado Blocked"),
            (UiLanguage::French, "y compris une tâche à l’état Blocked"),
            (UiLanguage::Portuguese, "inclusive um no estado Blocked"),
            (UiLanguage::Japanese, "Blocked 状態のジョブも含め"),
            (UiLanguage::ChineseSimplified, "包括处于 Blocked 状态的任务"),
            (UiLanguage::Korean, "Blocked 상태의 작업도 포함"),
            (
                UiLanguage::Vietnamese,
                "kể cả công việc ở trạng thái Blocked",
            ),
            (UiLanguage::Hindi, "Blocked स्थिति वाले कार्य सहित"),
        ];
        assert_eq!(inclusion.len(), UiLanguage::ALL.len());
        for (language, blocked_inclusion) in inclusion {
            with_language(language, || -> TestResult {
                let registry = CommandRegistry::built_in().map_err(ctx("deny registry"))?;
                let deny = registry.find("deny").ok_or(TestError::Missing("deny"))?;
                assert!(
                    deny.summary.contains(blocked_inclusion),
                    "Blocked jobs must be included in cancellation ({language:?}): {}",
                    deny.summary
                );
                Ok(())
            })?;
        }
        Ok(())
    }

    #[test]
    fn actual_builtin_contributors_have_complete_localized_summaries() -> TestResult {
        let operations = all_runtime_operations()?;
        let english = CommandRegistry::from_operation_registry(&operations)
            .map_err(ctx("English registry"))?;
        // The inventory's 95 declarations include web routes, which must never
        // appear as TUI commands. Derive coverage from the real constructors.
        let command_count = operations
            .iter()
            .flat_map(|op| &op.meta().surfaces)
            .filter(|surface| matches!(surface, Surface::Command { .. }))
            .count();
        assert_eq!(command_count, 67);
        assert_eq!(english.specs().len(), command_count);
        let catalog: serde_json::Value =
            serde_json::from_str(include_str!("../locales/operations.json"))
                .map_err(ctx("operation catalog"))?;
        assert_eq!(catalog.as_object().map(|values| values.len()), Some(62));
        let adapters: Vec<_> = operations
            .iter()
            .flat_map(|op| CommandAdapter::from_operation(Arc::clone(op)))
            .collect();
        for language in UiLanguage::ALL {
            with_language(language, || -> TestResult {
                let registry = CommandRegistry::from_operation_registry(&operations)
                    .map_err(ctx("localized registry"))?;
                let adapted = CommandRegistry::from_command_adapters(&adapters);
                assert_eq!(registry.specs(), adapted.specs());
                for operation in operations.iter() {
                    let meta = operation.meta();
                    for surface in &meta.surfaces {
                        if let Surface::Command { path, .. } = surface {
                            let spec = registry
                                .find(path.trim_start_matches('/'))
                                .ok_or(TestError::Missing("actual command"))?;
                            let baseline = english
                                .find(spec.name.as_str())
                                .ok_or(TestError::Missing("English command"))?;
                            let expected = summary(meta)
                                .ok_or(TestError::Missing("built-in summary identity"))?;
                            assert_eq!(spec.summary, expected, "{} {language:?}", meta.name);
                            assert!(!expected.is_empty());
                            let mut enriched_again = spec.clone();
                            crate::command_catalog::enrich(&mut enriched_again);
                            assert_eq!(
                                &enriched_again, spec,
                                "complete metadata must survive re-enrichment: {} {language:?}",
                                meta.name
                            );
                            if language != UiLanguage::English {
                                assert_ne!(
                                    expected, baseline.summary,
                                    "{} {language:?}",
                                    meta.name
                                );
                            }
                            if let Some(translations) =
                                catalog.get(format!("operations.summary.{}", meta.name))
                            {
                                assert_eq!(
                                    expected,
                                    translations[language.as_str()].as_str().unwrap_or_default()
                                );
                            }
                            assert_eq!(spec.aliases, baseline.aliases);
                            assert_eq!(spec.permission, baseline.permission);
                            assert_eq!(spec.scope, baseline.scope);
                            assert_eq!(spec.domain, baseline.domain);
                            assert_eq!(spec.busy, baseline.busy);
                            assert_eq!(spec.busy_subcommands, baseline.busy_subcommands);
                            // Only /matrix owns a localized explanation after its grammar.
                            assert_eq!(
                                spec.usage.split(" — ").next(),
                                baseline.usage.split(" — ").next()
                            );
                            let grammar = |spec: &CommandSpec| {
                                spec.subcommands
                                    .iter()
                                    .map(|hint| (hint.name, hint.args))
                                    .collect::<Vec<_>>()
                            };
                            assert_eq!(grammar(spec), grammar(baseline));
                            for alias in &spec.aliases {
                                assert_eq!(registry.find(alias), Some(spec));
                            }
                        }
                    }
                }
                Ok(())
            })?;
        }
        // Knowing an optional operation must not change ordinary availability.
        let base = CommandRegistry::built_in().map_err(ctx("base registry"))?;
        assert_eq!(base.specs().len(), 46);
        for optional in [
            "plan",
            "goal",
            "analyze",
            "cycle",
            "gateway-status",
            "infra-status",
        ] {
            assert!(base.find(optional).is_none(), "{optional}");
        }
        assert_eq!(
            base.find("model").map(|spec| spec.summary.as_str()),
            Some("Show the current model or switch it with switch <id>")
        );
        Ok(())
    }

    // Extensions may reuse names, command paths, and even clone metadata.
    // Their independent metadata object is not the concrete built-in identity.
    struct External(OperationMeta);
    impl Operation for External {
        fn meta(&self) -> &OperationMeta {
            &self.0
        }
        fn run<'a>(&'a self, _: &'a OpContext, _: OpInput) -> OpFuture<'a> {
            Box::pin(async {
                Err(harw_operations::OpError::NotAvailable(
                    "test operation".into(),
                ))
            })
        }
    }

    #[test]
    fn external_operations_keep_supplied_text_even_with_builtin_identity_collisions() -> TestResult
    {
        let builtin = base_operations();
        for name in [
            "model",
            "mode",
            "memory",
            "agent",
            "sandbox-lease",
            "settings",
            "compact",
        ] {
            let genuine = builtin
                .find_by_name(name)
                .ok_or(TestError::Missing("built-in"))?;
            for supplied_summary in [
                genuine.meta().summary,
                "Externe Beschreibung: Modell {payload} — bytegenau, nicht übersetzen. Noch ein sehr langer Satz mit eigenem Inhalt, unveränderter Benutzersyntax, /model, <pfad> und Zeichen aus 日本語.",
            ] {
                for (identity, path) in [
                    (name, genuine.meta().surfaces.clone()),
                    ("external-owned", genuine.meta().surfaces.clone()),
                    (
                        name,
                        vec![Surface::Command {
                            path: "/external",
                            visibility: harw_operations::CommandVisibility::TuiOnly,
                        }],
                    ),
                    (
                        "external-owned",
                        vec![Surface::Command {
                            path: "/external",
                            visibility: harw_operations::CommandVisibility::TuiOnly,
                        }],
                    ),
                ] {
                    let mut meta = genuine.meta().clone();
                    meta.name = identity;
                    meta.summary = supplied_summary;
                    meta.surfaces = path;
                    let original = meta.summary;
                    let mut operations = OperationRegistry::new();
                    operations.register(Arc::new(External(meta)));
                    let adapters: Vec<_> = operations
                        .iter()
                        .flat_map(|op| CommandAdapter::from_operation(Arc::clone(op)))
                        .collect();
                    for language in UiLanguage::ALL {
                        with_language(language, || -> TestResult {
                            let registry = CommandRegistry::from_operation_registry(&operations)
                                .map_err(ctx("external registry"))?;
                            let adapted = CommandRegistry::from_command_adapters(&adapters);
                            assert_eq!(registry.specs(), adapted.specs());
                            for spec in registry.specs() {
                                assert_eq!(
                                    spec.summary, original,
                                    "{name} {identity} {language:?}"
                                );
                                assert_eq!(spec.usage, format!("/{}", spec.name));
                                assert!(spec.subcommands.is_empty());
                            }
                            Ok(())
                        })?;
                    }
                }
            }
            // CommandSpec::new is caller-supplied, regardless of default origin.
            let supplied = crate::CommandSpec::new(
                name,
                Vec::<String>::new(),
                crate::CommandScope::TuiOnly,
                crate::PermissionTier::Operator,
                crate::OutputSurface::Inline,
                crate::CommandDomain::Misc,
            )
            .map_err(ctx("custom spec"))?
            .with_help(
                "Modell {user} unverändert",
                "/custom <pfad>",
                &[crate::command::SubcommandHint::new(
                    "show",
                    "<wert>",
                    "Fremder Hinweis",
                )],
            );
            for language in UiLanguage::ALL {
                let registry =
                    with_language(language, || CommandRegistry::new(vec![supplied.clone()]));
                assert_eq!(registry.specs(), &[supplied.clone()]);
            }
        }
        Ok(())
    }

    #[test]
    fn concrete_builtin_metadata_and_private_compact_identity_are_stable() -> TestResult {
        let first = CommandRegistry::built_in().map_err(ctx("first built-in"))?;
        let second = CommandRegistry::built_in().map_err(ctx("second built-in"))?;
        assert_eq!(first.specs(), second.specs());
        for language in UiLanguage::ALL {
            with_language(language, || -> TestResult {
                let registry = CommandRegistry::built_in().map_err(ctx("localized built-in"))?;
                let compact = registry
                    .find("compact")
                    .ok_or(TestError::Missing("compact"))?;
                assert_eq!(
                    compact.summary,
                    tr_for(
                        language,
                        "operations.summary.compact",
                        "Session compaction is unavailable in this runtime"
                    )
                );
                assert_eq!(
                    summary(harw_ops::model::ModelOperation.meta()),
                    Some(tr_for(
                        language,
                        "operations.summary.model",
                        "Show the current model or switch it with switch <id>"
                    ))
                );
                assert_eq!(
                    summary(harw_ops::compact::CompactOperation.meta()),
                    Some(compact.summary.as_str())
                );
                Ok(())
            })?;
        }
        Ok(())
    }
}
