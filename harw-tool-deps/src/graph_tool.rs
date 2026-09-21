//! `deps.graph` — Leaf-first-Abhängigkeitsgraph des eigenen Cargo-Workspace.
//!
//! # Verantwortung
//! Dieses Modul projiziert einen [`WorkspaceGraph`] aus `harw-code-graph` in
//! eine Tool-Antwort: entweder als JSON (`{crates, levels}`) für maschinelle
//! Weiterverarbeitung oder als kompakte Textprojektion für Prompts. Es liest
//! ausschließlich Manifeste **innerhalb** des Workspace; der Registry-Cache
//! wird hier nicht berührt (das ist `source_tool.rs`).
//!
//! # Sicherheitskontrakt
//! - Permission: [`harw_authority::Permission::ReadWorkspace`], geprüft vom
//!   Makro-Prolog vor der Deserialisierung der Argumente.
//! - Ein optionales `root`-Argument wird **nie** direkt verwendet, sondern über
//!   `WorkspaceBinding::resolve_existing` aufgelöst; damit sind `../`-Traversal
//!   und Symlinks aus dem Workspace heraus abgewehrt. Ohne `root` gilt
//!   `canonical_root()` der Sandbox.
//!
//! # Schlüsseltypen
//! - [`CrateSummary`] — eine Crate-Zeile der JSON-Antwort.
//! - [`DepsGraphTool`] — der von `#[harw_macros::tool]` erzeugte Executor.
//!
//! # Fehler
//! [`crate::DepsToolError`]; im Tool wird jeder Fehler zu `Ok(ToolOutput::Error)`.
//!
//! # Nebenläufigkeit
//! Zustandslos und rein lesend; das Tool ist `parallel_safe`.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_code_graph::WorkspaceGraph;
//! use harw_tool_deps::graph_tool::graph_as_json;
//! use std::path::Path;
//!
//! # fn demo() -> Result<(), harw_tool_deps::DepsToolError> {
//! let graph = WorkspaceGraph::load(Path::new("."))?;
//! let json = graph_as_json(&graph)?;
//! println!("{json}");
//! # Ok(())
//! # }
//! ```

use std::io;
use std::path::{Path, PathBuf};

use harw_code_graph::WorkspaceGraph;
use harw_tools::{ToolOutput, ToolsError, executor::ToolExecutionContext};
use serde::{Deserialize, Serialize};

use crate::error::{DepsToolError, DepsToolResult};
use crate::source_tool::{error_output, json_output};

/// Erlaubte Werte für das `format`-Argument.
pub const GRAPH_FORMATS: &[&str] = &["json", "text"];

/// Eine Crate-Zeile der `deps.graph`-JSON-Antwort.
///
/// # Description
/// `level` und `is_leaf` sind das eigentliche Nutzsignal für den Analyse-Modus:
/// sie sagen, wo ein Crate in der Bottom-up-Reihenfolge steht. `deps` sind die
/// workspace-internen, `external_deps` die externen Abhängigkeiten — letztere
/// sind die Brücke zu `deps.locked` und `deps.source_read`.
#[derive(Debug, Clone, Serialize)]
pub struct CrateSummary {
    /// Crate-Name aus `[package].name`.
    pub name: String,
    /// Version aus `[package].version` (oder `"0.0.0"`).
    pub version: String,
    /// Ebene von unten: `0` sind die Leaves.
    pub level: u32,
    /// `true`, wenn das Crate keine workspace-internen Abhängigkeiten hat.
    pub is_leaf: bool,
    /// Workspace-interne Abhängigkeiten, sortiert.
    pub deps: Vec<String>,
    /// Externe (Registry-)Abhängigkeiten, sortiert.
    pub external_deps: Vec<String>,
}

/// Argumente für `deps.graph`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
pub struct DepsGraphArgs {
    /// Workspace-Wurzel relativ zum Workspace-Root. Standard: der Workspace-Root selbst.
    pub root: Option<String>,
    /// Ausgabeformat: "json" (Standard) oder "text".
    pub format: Option<String>,
}

/// Projiziert einen [`WorkspaceGraph`] in die JSON-Antwortform
/// `{crates: [...], levels: [[name, ...], ...]}`.
///
/// # Arguments
/// - `graph` (`&WorkspaceGraph`): der geladene Workspace-Graph.
///
/// # Returns
/// Ein [`serde_json::Value`] mit den nach Name sortierten `crates` und den
/// Ebenen `levels` (Index `0` = Leaves).
///
/// # Errors
/// - [`DepsToolError::CodeGraph`] mit `CycleDetected`, wenn der Graph zyklisch ist.
/// - [`DepsToolError::Json`], wenn die Projektion nicht serialisierbar ist.
///
/// # Panics
/// Nie.
///
/// # Concurrency
/// Reine Funktion; sicher aus mehreren Threads.
///
/// # Examples
/// ```rust,no_run
/// use harw_code_graph::WorkspaceGraph;
/// use harw_tool_deps::graph_tool::graph_as_json;
/// use std::path::Path;
///
/// # fn demo() -> Result<(), harw_tool_deps::DepsToolError> {
/// let graph = WorkspaceGraph::load(Path::new("."))?;
/// let value = graph_as_json(&graph)?;
/// assert!(value.get("levels").is_some());
/// # Ok(())
/// # }
/// ```
pub fn graph_as_json(graph: &WorkspaceGraph) -> DepsToolResult<serde_json::Value> {
    let levels: Vec<Vec<String>> = graph
        .topological_levels()?
        .iter()
        .map(|level| level.iter().map(|node| node.name.clone()).collect())
        .collect();

    let mut crates: Vec<CrateSummary> = graph
        .crates
        .iter()
        .map(|node| CrateSummary {
            name: node.name.clone(),
            version: node.version.clone(),
            level: node.level,
            is_leaf: node.is_leaf,
            deps: node.deps.clone(),
            external_deps: node.external_deps.clone(),
        })
        .collect();
    crates.sort_by(|left, right| left.name.cmp(&right.name));

    // Explizit über `to_value` statt als `json!`-Interpolation: so wird ein
    // Serialisierungsfehler zu einem `Result`, nicht zu einem Panic im Makro.
    let crates_value = serde_json::to_value(&crates)?;

    Ok(serde_json::json!({
        "root": graph.root.display().to_string(),
        "crates": crates_value,
        "levels": levels,
    }))
}

/// Löst die zu ladende Workspace-Wurzel auf.
///
/// # Description
/// Ohne `root` gilt `canonical_root()` der Sandbox. Mit `root` wird der Pfad
/// **ausschließlich** über `WorkspaceBinding::resolve_existing` aufgelöst, das
/// `../`-Traversal und Symlinks aus dem Workspace heraus abweist.
///
/// # Errors
/// - [`DepsToolError::Io`] mit `ErrorKind::InvalidInput`, wenn die Sandbox den
///   Pfad ablehnt; die Meldung der Sandbox wird unverändert übernommen.
fn resolve_root(context: &ToolExecutionContext, root: Option<&str>) -> DepsToolResult<PathBuf> {
    let binding = context.sandbox().workspace();
    match root {
        None => Ok(binding.canonical_root().to_path_buf()),
        Some(relative) => binding
            .resolve_existing(Path::new(relative))
            .map_err(|error| {
                DepsToolError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    error.to_string(),
                ))
            }),
    }
}

/// Lädt den Workspace-Graphen und liefert ihn als JSON oder Textprojektion.
///
/// # Description
/// Der Einstieg für den Analyse-Modus: "wo fange ich an?" beantwortet die
/// Ebenen-Projektion (Leaves zuerst). Siehe [`graph_as_json`] für die
/// JSON-Form und `WorkspaceGraph::render_levels` für die Textform.
///
/// # Errors
/// Liefert nie `Err`; jeder Fehler wird als `Ok(ToolOutput::Error)` gemeldet.
#[harw_macros::tool(
    name = "deps.graph",
    description = "Liefert den Abhängigkeitsgraphen des eigenen Cargo-Workspace in \
                   Leaf-first-Ebenen — die Grundlage, um eine Codebasis bottom-up zu \
                   verstehen. format='json' (Standard) liefert {root, crates:[{name, version, \
                   level, is_leaf, deps, external_deps}], levels:[[name,...]]}; format='text' \
                   liefert die kompakte Ebenenprojektion 'Ebene N: crate-a, crate-b'. 'root' \
                   ist optional und relativ zum Workspace-Root; Pfade außerhalb des Workspace \
                   werden abgelehnt. Nur lesend.",
    permission = "read_workspace",
    parallel_safe
)]
async fn deps_graph(
    context: &ToolExecutionContext,
    args: DepsGraphArgs,
) -> Result<ToolOutput, ToolsError> {
    const TOOL: &str = "deps.graph";

    let format = args.format.as_deref().unwrap_or("json");
    if !GRAPH_FORMATS.contains(&format) {
        return Ok(ToolOutput::error(format!(
            "{TOOL}: unbekanntes Format '{format}' (erlaubt: json, text)"
        )));
    }

    let root = match resolve_root(context, args.root.as_deref()) {
        Ok(root) => root,
        Err(error) => return Ok(error_output(TOOL, &error)),
    };

    let graph = match WorkspaceGraph::load(&root) {
        Ok(graph) => graph,
        Err(error) => return Ok(error_output(TOOL, &DepsToolError::CodeGraph(error))),
    };

    tracing::info!(
        root = %root.display(),
        crates = graph.crates.len(),
        format,
        "deps.graph"
    );

    if format == "text" {
        return match graph.render_levels() {
            Ok(text) => Ok(ToolOutput::text(text)),
            Err(error) => Ok(error_output(TOOL, &DepsToolError::CodeGraph(error))),
        };
    }

    match graph_as_json(&graph) {
        Ok(value) => Ok(json_output(TOOL, &value)),
        Err(error) => Ok(error_output(TOOL, &error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{block_on, mini_workspace, sandbox_context, tool_call};
    use harw_authority::Permission;
    use harw_tools::ToolExecutor as _;
    use std::fs;

    #[test]
    fn test_graph_as_json_reports_levels_and_leaf_flags() {
        let (harness, workspace) = mini_workspace("graph-json");
        let graph = WorkspaceGraph::load(&workspace).expect("Workspace laden");

        let value = graph_as_json(&graph).expect("JSON-Projektion");

        let levels = value["levels"].as_array().expect("levels ist ein Array");
        assert_eq!(levels.len(), 3, "a -> b -> c ergibt drei Ebenen");
        assert_eq!(levels[0][0], "a");
        assert_eq!(levels[2][0], "c");

        let crates = value["crates"].as_array().expect("crates ist ein Array");
        assert_eq!(crates.len(), 3);
        assert_eq!(crates[0]["name"], "a", "crates sind nach Name sortiert");
        assert_eq!(crates[0]["is_leaf"], true);
        assert_eq!(crates[0]["level"], 0);
        assert_eq!(crates[1]["deps"][0], "a");
        assert_eq!(crates[2]["level"], 2);

        fs::remove_dir_all(&harness).ok();
    }

    #[test]
    fn test_graph_as_json_lists_external_deps() {
        let (harness, workspace) = mini_workspace("graph-external");
        let graph = WorkspaceGraph::load(&workspace).expect("Workspace laden");

        let value = graph_as_json(&graph).expect("JSON-Projektion");
        let crates = value["crates"].as_array().expect("crates ist ein Array");

        assert_eq!(
            crates[0]["external_deps"][0], "serde",
            "externe Abhängigkeiten müssen sichtbar sein"
        );

        fs::remove_dir_all(&harness).ok();
    }

    #[test]
    fn test_deps_graph_tool_returns_json_by_default() {
        let (harness, _) = mini_workspace("graph-tool-json");
        let context = sandbox_context(&harness, vec![Permission::ReadWorkspace]);
        let call = tool_call("deps.graph", serde_json::json!({}));

        let output = block_on(DepsGraphTool.execute(&context, &call)).expect("Tool läuft");

        match output {
            ToolOutput::Json { content } => {
                assert!(content["levels"].is_array());
                assert!(content["crates"].is_array());
            }
            other => panic!("JSON-Ausgabe erwartet, war: {other:?}"),
        }

        fs::remove_dir_all(&harness).ok();
    }

    #[test]
    fn test_deps_graph_tool_returns_text_projection() {
        let (harness, _) = mini_workspace("graph-tool-text");
        let context = sandbox_context(&harness, vec![Permission::ReadWorkspace]);
        let call = tool_call("deps.graph", serde_json::json!({ "format": "text" }));

        let output = block_on(DepsGraphTool.execute(&context, &call)).expect("Tool läuft");

        match output {
            ToolOutput::Text { content } => {
                assert!(content.starts_with("Ebene 0: a"), "war: {content}");
                assert!(content.contains("Ebene 2: c"), "war: {content}");
            }
            other => panic!("Text-Ausgabe erwartet, war: {other:?}"),
        }

        fs::remove_dir_all(&harness).ok();
    }

    #[test]
    fn test_deps_graph_tool_rejects_unknown_format() {
        let (harness, _) = mini_workspace("graph-tool-format");
        let context = sandbox_context(&harness, vec![Permission::ReadWorkspace]);
        let call = tool_call("deps.graph", serde_json::json!({ "format": "yaml" }));

        let output = block_on(DepsGraphTool.execute(&context, &call)).expect("Tool läuft");

        match output {
            ToolOutput::Error { message } => assert!(message.contains("yaml"), "war: {message}"),
            other => panic!("Fehlerausgabe erwartet, war: {other:?}"),
        }

        fs::remove_dir_all(&harness).ok();
    }

    #[test]
    fn test_deps_graph_tool_rejects_root_outside_workspace() {
        let (harness, _) = mini_workspace("graph-tool-escape");
        let context = sandbox_context(&harness, vec![Permission::ReadWorkspace]);
        let call = tool_call("deps.graph", serde_json::json!({ "root": "../.." }));

        let output = block_on(DepsGraphTool.execute(&context, &call)).expect("Tool läuft");

        assert!(
            matches!(output, ToolOutput::Error { .. }),
            "'../'-Wurzel muss abgelehnt werden, war: {output:?}"
        );

        fs::remove_dir_all(&harness).ok();
    }

    #[test]
    fn test_deps_graph_tool_denies_without_read_workspace() {
        let (harness, _) = mini_workspace("graph-tool-denied");
        let context = sandbox_context(&harness, Vec::new());
        let call = tool_call("deps.graph", serde_json::json!({}));

        let output = block_on(DepsGraphTool.execute(&context, &call)).expect("Tool läuft");

        match output {
            ToolOutput::Error { message } => assert!(
                message.contains("ReadWorkspace"),
                "fehlende Permission muss benannt werden, war: {message}"
            ),
            other => panic!("Fehlerausgabe erwartet, war: {other:?}"),
        }

        fs::remove_dir_all(&harness).ok();
    }

    #[test]
    fn test_deps_graph_tool_spec_advertises_its_name() {
        let spec = DepsGraphTool::spec();
        assert_eq!(spec.name(), "deps.graph");
        assert_eq!(DepsGraphTool::NAME, "deps.graph");
        assert_eq!(
            DepsGraphTool::PERMISSION,
            Some(harw_tools::Permission::ReadWorkspace)
        );
    }

    // `PARALLEL_SAFE` is a macro-generated `const bool` (see
    // `#[harw_macros::tool]`), so any `assert!` on it is compile-time-constant
    // by construction — exactly what clippy's `assertions_on_constants` flags
    // as checking nothing at runtime. A `const` assertion embraces that fact
    // instead of fighting it: it fails to *compile* the moment `DepsGraphTool`
    // stops declaring itself parallel-safe (other tools in the workspace do
    // declare `parallel_safe = false`, see harw-tools/src/provider_macro.rs,
    // so this is a real per-tool fact, not a type-level tautology), which is a
    // strictly stronger guarantee than the runtime assertion it replaces.
    const _: () = assert!(DepsGraphTool::PARALLEL_SAFE);
}
