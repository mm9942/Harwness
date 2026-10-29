//! `tool.index` (PL-97 B2, PL-88): the read-only model tool over the tool
//! index, and the shared views behind `harw tool list|describe`.
//!
//! # Surfaces
//! - `tool.index` — read-only model tool without approval
//!   (`model_tool(readonly, approval = "none")`), args `{family?}`. Without
//!   `family` it returns the tool families with their tool counts; with a
//!   family one compact line per tool: name, capability class, first
//!   sentence of the real description, linked skill and the profiles that
//!   register it. The text is the table, `data` the same content as JSON.
//! - `harw tool list [FAMILY]` and `harw tool describe NAME`
//!   (`harw-cli/src/tool_cmd.rs`) render through [`index_view`] and
//!   [`describe_view`], so the terminal and the model see the same index.
//!
//! # Data
//! [`build_index`] is `ToolIndex::build` over the capability catalog (names,
//! families, providers, classes, profiles) with two derived inputs and
//! nothing maintained by hand here:
//! - the summaries: the first sentence of each tool's real `ToolSpec`
//!   description ([`known_specs`]: the specs of the profile providers,
//!   `harw_registry_defaults::profile::known_tool_specs`, then the operation
//!   model tools of the registry at hand);
//! - the tool → skill link (`ToolSkillMap::bundled`: the bundled skill
//!   manifests and the providers' `TOOL_SKILLS`).
//!
//! # Trust boundary
//! Read-only over static data of this binary: no workspace read, no network,
//! no session state, no configuration. Auto-approved
//! (`harw_registry_defaults::AUTO_APPROVED_TOOLS`) and without a sandbox
//! right (`tool_permission` is `None`, like `ps`). The index lists every tool
//! this binary knows, not the tools the calling session may use; it grants
//! nothing.

use std::collections::BTreeMap;
use std::sync::Arc;

use harw_macros::operation;
use harw_operations::adapter::ModelToolAdapter;
use harw_operations::adapter::model_tool::model_tool_schema_for;
use harw_operations::registry::OperationRegistry;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_registry_defaults::tool_index::{ToolIndex, ToolIndexEntry, ToolSkillMap};
use harw_tools::{FunctionToolSpec, JsonSchema, ToolName, ToolSpec};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Tool name of the model tool.
pub const TOOL_INDEX_TOOL: &str = "tool.index";

/// Arguments of `tool.index`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, harw_macros::OpArgs)]
#[serde(deny_unknown_fields)]
pub struct ToolIndexArgs {
    /// Tool family: the name prefix before the first `.` (e.g. `fs`, `git`, `dod`); omit it for the list of families.
    #[serde(default)]
    pub family: Option<String>,
}

impl harw_operations::FromRawArgs for ToolIndexArgs {
    fn from_raw_args(_tokens: &[String]) -> Result<Self, OpError> {
        Err(OpError::NotAvailable(format!(
            "`{TOOL_INDEX_TOOL}` has no command surface; it is a model tool (use `harw tool list`)"
        )))
    }
}

/// The compact tool index for the model.
#[operation(
    name = "tool.index",
    summary = "Returns the compact tool index of this Harwness build: without `family` the tool families with their tool counts, with `family` one line per tool of that family with its capability class, the first sentence of its description, the skill that explains it and the profiles that register it. Read-only; lists every known tool, not only the ones you may call.",
    domain = "catalog_config",
    permission = "observer",
    model_tool(readonly, approval = "none")
)]
async fn tool_index(ctx: &OpContext, args: ToolIndexArgs) -> Result<OpOutput, OpError> {
    let operations = ctx.service::<OperationRegistry>();
    let index = build_index(operations);
    let view = index_view(&index, args.family.as_deref()).map_err(OpError::InvalidArguments)?;
    Ok(OpOutput {
        text: view.text,
        data: Some(view.json),
    })
}

/// A rendered view: the text form and the JSON form of the same content.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexView {
    /// Text form (the model tool's result, the CLI's text output).
    pub text: String,
    /// JSON form (`data`, the CLI's `--json` output).
    pub json: Value,
}

/// The real specs of every tool the index can describe, by name.
///
/// The profile providers' specs
/// (`harw_registry_defaults::profile::known_tool_specs`) first, then the
/// operation model tools of `operations` (description = the operation
/// summary, parameters = the schema the model sees).
#[must_use]
pub fn known_specs(operations: Option<&OperationRegistry>) -> BTreeMap<String, ToolSpec> {
    let mut specs: BTreeMap<String, ToolSpec> = harw_registry_defaults::profile::known_tool_specs()
        .iter()
        .map(|spec| (spec.name().to_owned(), spec.clone()))
        .collect();
    for op in operations.into_iter().flat_map(OperationRegistry::iter) {
        let Some(adapter) = ModelToolAdapter::from_operation(Arc::clone(op)) else {
            continue;
        };
        let name = adapter.tool_name().to_owned();
        if specs.contains_key(&name) {
            continue;
        }
        // Dasselbe Schema, das `ModelToolProvider::tools` dem Modell zeigt.
        let derived = op.meta().args_schema.map(|build_schema| build_schema());
        let spec = ToolSpec::Function(FunctionToolSpec {
            name: ToolName::new(name.clone()),
            description: adapter.description().to_owned(),
            parameters: model_tool_schema_for(&name, derived),
            strict: false,
        });
        specs.insert(name, spec);
    }
    specs
}

/// The description of one tool: the profile spec, otherwise the summary of
/// an operation model tool in `operations`. Builds no schema.
fn description_of(name: &str, operations: Option<&OperationRegistry>) -> Option<String> {
    if let Some(spec) = harw_registry_defaults::profile::known_tool_specs()
        .iter()
        .find(|spec| spec.name() == name)
    {
        let ToolSpec::Function(function) = spec;
        return Some(function.description.clone());
    }
    let op = operations?.find_by_name(name)?;
    ModelToolAdapter::from_operation(Arc::clone(op)).map(|adapter| adapter.description().to_owned())
}

/// The tool index with summaries and skill links (see the module
/// documentation).
#[must_use]
pub fn build_index(operations: Option<&OperationRegistry>) -> ToolIndex {
    ToolIndex::build(&|name| description_of(name, operations)).with_skills(ToolSkillMap::bundled())
}

/// The operations whose model tools `harw tool` describes outside a
/// session: the base set (`register_all`) plus the gated planning,
/// WorkDriver, intent-cycle and gateway surfaces, as a root session with all
/// of them enabled would carry them.
#[must_use]
pub fn cli_operations() -> OperationRegistry {
    let config = harw_plan::config::PlanToolConfig::enabled_defaults();
    let mut registry = OperationRegistry::new();
    crate::register_all(&mut registry);
    crate::register_plan_tools(&mut registry, &config);
    crate::register_work_driver_tools(&mut registry, &config);
    crate::register_intent_cycle_tools(&mut registry, &config);
    // Doppelte Registrierung ist hier ausgeschlossen (frische Registry); ein
    // Fehler ließe nur Zusammenfassungen fehlen, nie Rechte entstehen.
    if let Err(error) = crate::gateway_ops::register_gateway(&mut registry) {
        tracing::debug!(%error, "tool_index.gateway_ops_skipped");
    }
    if let Err(error) = crate::gateway_ops::register_gateway_diagnostics(&mut registry) {
        tracing::debug!(%error, "tool_index.gateway_diagnostics_skipped");
    }
    registry
}

/// The JSON form of one index entry.
#[must_use]
pub fn entry_json(entry: &ToolIndexEntry) -> Value {
    json!({
        "name": entry.name,
        "family": entry.family,
        "provider": entry.provider,
        "crate": entry.crate_name,
        "class": entry.class.as_str(),
        "always_available": entry.always_available,
        "profiles": entry.profiles,
        "skill": entry.skill,
        "summary": entry.summary,
    })
}

/// The overview (`family = None`) or the tools of one family.
///
/// # Errors
/// An English message naming the known families when `family` is not one
/// of them (`tool.index` reports it as invalid arguments, `harw tool list`
/// exits with 1).
pub fn index_view(index: &ToolIndex, family: Option<&str>) -> Result<IndexView, String> {
    let family = family.map(str::trim).filter(|family| !family.is_empty());
    match family {
        None => {
            let families = index.families();
            let json = json!({
                "families": families
                    .iter()
                    .map(|(family, tools)| json!({ "family": family, "tools": tools }))
                    .collect::<Vec<_>>(),
                "tools": index.entries().len(),
            });
            Ok(IndexView {
                text: index.render(None),
                json,
            })
        }
        Some(family) => {
            let rows = index.family(family);
            if rows.is_empty() {
                let known: Vec<&str> = index.families().iter().map(|&(name, _)| name).collect();
                return Err(format!(
                    "unknown tool family {family:?}; known families: {}",
                    known.join(", ")
                ));
            }
            let json = json!({
                "family": family,
                "tools": rows.iter().map(|entry| entry_json(entry)).collect::<Vec<_>>(),
            });
            Ok(IndexView {
                text: index.render(Some(family)),
                json,
            })
        }
    }
}

/// How the default approval policy treats a call of `name`.
fn approval_label(name: &str) -> &'static str {
    if harw_registry_defaults::ALWAYS_ASK_TOOLS.contains(&name) {
        "always asks"
    } else if harw_registry_defaults::AUTO_APPROVED_TOOLS.contains(&name) {
        "auto-approved"
    } else {
        "asks unless the approval mode or an allow rule approves it"
    }
}

/// The type of a parameter schema (`string`, `integer|null`, ...).
fn type_label(schema: &JsonSchema) -> String {
    if let Some(kind) = &schema.schema_type {
        return format!("{kind:?}").to_lowercase();
    }
    match &schema.any_of {
        Some(variants) if !variants.is_empty() => variants
            .iter()
            .map(type_label)
            .collect::<Vec<_>>()
            .join("|"),
        _ => "any".to_owned(),
    }
}

/// Everything about one tool: its index entry (if the catalog names it),
/// its approval treatment, its full description and its parameters (if its
/// spec is known). `None` when neither the catalog nor a spec knows `name`.
#[must_use]
pub fn describe_view(name: &str, operations: Option<&OperationRegistry>) -> Option<IndexView> {
    use std::fmt::Write as _;

    let name = name.trim();
    let index = build_index(operations);
    let entry = index.get(name);
    let specs = known_specs(operations);
    let function = specs.get(name).map(|spec| {
        let ToolSpec::Function(function) = spec;
        function
    });
    if entry.is_none() && function.is_none() {
        return None;
    }

    let mut text = String::new();
    let _ = writeln!(text, "{name}");
    if let Some(entry) = entry {
        let _ = writeln!(text, "  family:    {}", entry.family);
        let _ = writeln!(
            text,
            "  provider:  {} ({})",
            entry.provider, entry.crate_name
        );
        let _ = writeln!(text, "  class:     {}", entry.class.as_str());
        let profiles = if entry.always_available {
            "always available".to_owned()
        } else if entry.profiles.is_empty() {
            "none (added by the composition root)".to_owned()
        } else {
            entry.profiles.join(", ")
        };
        let _ = writeln!(text, "  profiles:  {profiles}");
        let _ = writeln!(
            text,
            "  skill:     {}",
            entry.skill.as_deref().unwrap_or("-")
        );
    }
    let approval = approval_label(name);
    let _ = writeln!(text, "  approval:  {approval}");

    let mut parameters: Vec<Value> = Vec::new();
    if let Some(function) = function {
        let _ = write!(text, "\n{}\n", function.description.trim());
        let required = function.parameters.required.clone().unwrap_or_default();
        let properties = function.parameters.properties.clone().unwrap_or_default();
        if !properties.is_empty() {
            text.push_str("\nParameters:\n");
        }
        for (field, schema) in &properties {
            let kind = type_label(schema);
            let is_required = required.contains(field);
            let description = schema.description.as_deref().unwrap_or("").trim();
            let _ = writeln!(
                text,
                "  {field} ({kind}{}): {description}",
                if is_required { ", required" } else { "" }
            );
            parameters.push(json!({
                "name": field,
                "type": kind,
                "required": is_required,
                "description": schema.description,
            }));
        }
    }

    let json = json!({
        "name": name,
        "entry": entry.map(entry_json),
        "approval": approval,
        "description": function.map(|function| function.description.clone()),
        "parameters": parameters,
        "schema": function.and_then(|function| serde_json::to_value(&function.parameters).ok()),
    });
    Some(IndexView {
        text: text.trim_end().to_owned(),
        json,
    })
}

#[cfg(test)]
mod tests {
    use harw_operations::context::ServiceMap;
    use harw_operations::{ApprovalPolicy, OpInput, Operation, PermissionTier, Surface};
    use harw_registry_defaults::capability_catalog::CapabilityClass;

    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn ctx_err(context: &'static str) -> impl FnOnce(OpError) -> TestError {
        move |error| TestError::Unexpected(format!("{context}: {error:?}"))
    }

    async fn call(context: &OpContext, args: Value) -> Result<OpOutput, OpError> {
        ToolIndexOperation
            .run(context, OpInput::model_tool(args))
            .await
    }

    fn context_with_operations() -> TestResult<OpContext> {
        let mut services = ServiceMap::new();
        // Wie die Laufzeit: die Registry selbst (nicht im `Arc`), siehe `/help`.
        services.insert(cli_operations());
        crate::knowledge_test_support::op_context(services)
    }

    #[test]
    fn tool_index_is_one_read_only_auto_approved_model_tool() {
        let meta = ToolIndexOperation.meta();
        assert_eq!(meta.name, TOOL_INDEX_TOOL);
        assert_eq!(meta.permission, PermissionTier::Observer);
        assert_eq!(
            meta.surfaces,
            vec![Surface::ModelTool {
                readonly: true,
                approval: ApprovalPolicy::None,
            }]
        );
        assert!(meta.summary.len() >= 60, "{}", meta.summary);
    }

    /// Rechte-Parität: Operation, Allowlist, Rechtetabelle und Katalog sagen
    /// dasselbe — lesend, ohne Rückfrage, ohne Sandbox-Recht, `Meta`-Zeile.
    #[test]
    fn tool_index_permission_parity_with_the_registry_defaults() {
        assert_eq!(
            harw_registry_defaults::profile::TOOL_INDEX_TOOLS,
            [TOOL_INDEX_TOOL]
        );
        assert!(harw_registry_defaults::AUTO_APPROVED_TOOLS.contains(&TOOL_INDEX_TOOL));
        assert!(!harw_registry_defaults::ALWAYS_ASK_TOOLS.contains(&TOOL_INDEX_TOOL));
        assert!(
            !harw_registry_defaults::DefaultApprovalPolicy::requires_explicit_approval(
                &harw_extension_api::ToolCall {
                    id: Default::default(),
                    name: harw_extension_api::ToolName::new(TOOL_INDEX_TOOL),
                    arguments: Default::default(),
                }
            )
        );
        assert_eq!(
            harw_registry_defaults::authority::tool_permission(TOOL_INDEX_TOOL),
            None
        );
        assert_eq!(
            harw_registry_defaults::authority::tool_permission(TOOL_INDEX_TOOL),
            harw_registry_defaults::authority::tool_permission("ps")
        );
        assert_eq!(
            harw_registry_defaults::capability_catalog::lookup(TOOL_INDEX_TOOL)
                .map(|entry| entry.class),
            Some(CapabilityClass::Meta)
        );
        let mut registry = OperationRegistry::new();
        crate::register_all(&mut registry);
        assert!(registry.find_by_name(TOOL_INDEX_TOOL).is_some());
    }

    #[test]
    fn tool_index_args_schema_round_trips() -> TestResult {
        let args: ToolIndexArgs =
            serde_json::from_value(json!({ "family": "dod" })).map_err(ctx("args parse"))?;
        assert_eq!(args.family.as_deref(), Some("dod"));
        let back = serde_json::to_value(&args).map_err(ctx("args encode"))?;
        assert_eq!(back, json!({ "family": "dod" }));
        assert!(serde_json::from_value::<ToolIndexArgs>(json!({ "nope": 1 })).is_err());
        let schema = <ToolIndexArgs as harw_operations::OpArgsSchema>::json_schema();
        let family = schema
            .properties
            .as_ref()
            .and_then(|properties| properties.get("family"))
            .ok_or(TestError::Missing("family property"))?;
        assert!(
            family
                .description
                .as_deref()
                .is_some_and(|text| text.contains("prefix"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn tool_index_without_family_lists_the_families_with_counts() -> TestResult {
        let context = context_with_operations()?;
        let output = call(&context, json!({}))
            .await
            .map_err(ctx_err("overview"))?;
        assert!(output.text.starts_with("Tool families"), "{}", output.text);
        assert!(output.text.contains("\n- dod (3)\n"), "{}", output.text);
        assert!(output.text.contains("\n- fs ("), "{}", output.text);
        let data = output.data.ok_or(TestError::Missing("data"))?;
        let families = data["families"]
            .as_array()
            .ok_or(TestError::Missing("families"))?;
        assert!(
            families
                .iter()
                .any(|family| family["family"] == "dod" && family["tools"] == 3)
        );
        Ok(())
    }

    /// PL-97 B2 (Plan-Test): `tool.index` für `family = "dod"`.
    #[tokio::test]
    async fn tool_index_for_family_dod_lists_the_three_read_only_tools() -> TestResult {
        let context = context_with_operations()?;
        let output = call(&context, json!({ "family": "dod" }))
            .await
            .map_err(ctx_err("dod family"))?;
        let lines: Vec<&str> = output.text.lines().collect();
        assert_eq!(lines.len(), 3, "{}", output.text);
        for (line, tool) in lines
            .iter()
            .zip(["dod.findings", "dod.sensors", "dod.status"])
        {
            assert!(
                line.starts_with(&format!("{tool} [meta] Read-only")),
                "{line}"
            );
        }
        let data = output.data.ok_or(TestError::Missing("data"))?;
        assert_eq!(data["family"], "dod");
        let tools = data["tools"]
            .as_array()
            .ok_or(TestError::Missing("tools"))?;
        assert_eq!(tools.len(), 3);
        assert!(tools.iter().all(|tool| {
            tool["provider"] == "dod"
                && tool["class"] == "meta"
                && tool["summary"]
                    .as_str()
                    .is_some_and(|summary| summary.starts_with("Read-only"))
        }));
        Ok(())
    }

    #[tokio::test]
    async fn tool_index_family_shows_skill_links_and_profiles() -> TestResult {
        let context = context_with_operations()?;
        let output = call(&context, json!({ "family": "sys" }))
            .await
            .map_err(ctx_err("sys family"))?;
        let ps = output
            .text
            .lines()
            .find(|line| line.starts_with("sys.ps ["))
            .ok_or(TestError::Missing("sys.ps line"))?;
        assert!(ps.contains("skill: sys-tools"), "{ps}");
        assert!(ps.contains("profiles: full"), "{ps}");
        Ok(())
    }

    #[tokio::test]
    async fn tool_index_rejects_an_unknown_family_with_the_known_ones() -> TestResult {
        let context = context_with_operations()?;
        match call(&context, json!({ "family": "nope" })).await {
            Err(OpError::InvalidArguments(message)) => {
                assert!(message.contains("unknown tool family"), "{message}");
                assert!(
                    message.contains("dod") && message.contains("fs"),
                    "{message}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected invalid arguments, got {other:?}"
                )));
            }
        }
        assert!(call(&context, json!({ "family": 3 })).await.is_err());
        Ok(())
    }

    #[test]
    fn summaries_come_from_the_real_specs() -> TestResult {
        let operations = cli_operations();
        let index = build_index(Some(&operations));
        let read = index.get("fs.read").ok_or(TestError::Missing("fs.read"))?;
        let spec = harw_registry_defaults::profile::known_tool_specs()
            .iter()
            .find(|spec| spec.name() == "fs.read")
            .ok_or(TestError::Missing("fs.read spec"))?;
        let ToolSpec::Function(function) = spec;
        assert_eq!(
            read.summary.as_deref(),
            Some(
                harw_registry_defaults::tool_index::first_sentence(&function.description).as_str()
            )
        );
        let status = index
            .get("dod.status")
            .ok_or(TestError::Missing("dod.status"))?;
        assert!(
            status
                .summary
                .as_deref()
                .is_some_and(|summary| summary.starts_with("Read-only Harw-DoD posture"))
        );
        // Ohne Operations-Registry fehlen nur die Zusammenfassungen der
        // Operationswerkzeuge.
        let bare = build_index(None);
        assert!(
            bare.get("dod.status")
                .is_some_and(|entry| entry.summary.is_none())
        );
        assert_eq!(
            bare.get("fs.read").map(|entry| &entry.summary),
            Some(&read.summary)
        );
        Ok(())
    }

    #[test]
    fn describe_shows_entry_approval_description_and_parameters() -> TestResult {
        let operations = cli_operations();
        let view =
            describe_view("fs.read", Some(&operations)).ok_or(TestError::Missing("fs.read"))?;
        assert!(
            view.text.starts_with("fs.read\n  family:    fs\n"),
            "{}",
            view.text
        );
        assert!(
            view.text.contains("  approval:  auto-approved"),
            "{}",
            view.text
        );
        assert!(view.text.contains("\nParameters:\n  "), "{}", view.text);
        assert!(view.text.contains("path ("), "{}", view.text);
        assert_eq!(view.json["entry"]["class"], "read");
        assert!(
            view.json["description"]
                .as_str()
                .is_some_and(|text| text.len() > 60)
        );
        let parameters = view.json["parameters"]
            .as_array()
            .ok_or(TestError::Missing("parameters"))?;
        assert!(
            parameters
                .iter()
                .any(|parameter| parameter["name"] == "path" && parameter["required"] == true)
        );

        let index_tool = describe_view(TOOL_INDEX_TOOL, Some(&operations))
            .ok_or(TestError::Missing("tool.index"))?;
        assert!(
            index_tool.text.contains("family (string"),
            "{}",
            index_tool.text
        );

        let shell =
            describe_view("shell.exec", Some(&operations)).ok_or(TestError::Missing("shell"))?;
        assert!(!shell.text.contains("auto-approved"), "{}", shell.text);
        assert!(describe_view("no.such_tool", Some(&operations)).is_none());
        Ok(())
    }
}
