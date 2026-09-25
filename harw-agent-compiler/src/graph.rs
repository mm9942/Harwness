//! `harw agent graph`: delegation, resolution and rights-flow graphs in
//! text, Graphviz dot, Mermaid and JSON.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::compiler::Compiled;
use crate::discovery::SourceSet;
use crate::passes::role_label;
use crate::rights::is_writing;
use crate::rights::RightsSet;

/// Output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphFormat {
    /// Indented text.
    Text,
    /// Graphviz dot.
    Dot,
    /// Mermaid flowchart.
    Mermaid,
    /// JSON (nodes and edges).
    Json,
}

impl GraphFormat {
    /// Parses `text|dot|mermaid|json`.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "text" => Some(Self::Text),
            "dot" => Some(Self::Dot),
            "mermaid" => Some(Self::Mermaid),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

/// Which graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphKind {
    /// Who can start whom.
    Delegation,
    /// `extends` → mixins → patches per layer.
    Resolution,
    /// Manifest ≤ base role ≤ author ceiling.
    Rights,
    /// All three.
    All,
}

impl GraphKind {
    /// Parses `delegation|resolution|rights|all`.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "delegation" => Some(Self::Delegation),
            "resolution" => Some(Self::Resolution),
            "rights" => Some(Self::Rights),
            "all" => Some(Self::All),
            _ => None,
        }
    }
}

/// A node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphNode {
    /// Unique id.
    pub id: String,
    /// Display label.
    pub label: String,
    /// `agent`, `definition`, `patch`, `rights`.
    pub kind: String,
    /// Extra attributes (role, depth, access, layer, …).
    pub attrs: BTreeMap<String, String>,
}

/// An edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphEdge {
    /// Source node id.
    pub from: String,
    /// Target node id.
    pub to: String,
    /// Label.
    pub label: String,
}

/// A graph.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Graph {
    /// Title.
    pub title: String,
    /// Nodes, insertion order.
    pub nodes: Vec<GraphNode>,
    /// Edges, insertion order.
    pub edges: Vec<GraphEdge>,
}

impl Graph {
    fn node(&mut self, id: &str, label: &str, kind: &str, attrs: &[(&str, String)]) {
        if self.nodes.iter().any(|node| node.id == id) {
            return;
        }
        self.nodes.push(GraphNode {
            id: id.to_owned(),
            label: label.to_owned(),
            kind: kind.to_owned(),
            attrs: attrs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.clone()))
                .collect(),
        });
    }

    fn edge(&mut self, from: &str, to: &str, label: &str) {
        let edge = GraphEdge {
            from: from.to_owned(),
            to: to.to_owned(),
            label: label.to_owned(),
        };
        if !self.edges.contains(&edge) {
            self.edges.push(edge);
        }
    }
}

fn access(read_only: bool) -> String {
    if read_only { "read-only" } else { "writing" }.to_owned()
}

/// The delegation graph of compiled agents: every compiled root and its
/// embedded children (depth, role, read-only or writing).
#[must_use]
pub fn delegation_graph(compiled: &[&Compiled]) -> Graph {
    let mut graph = Graph {
        title: "delegation".to_owned(),
        ..Graph::default()
    };
    for root in compiled {
        let unit = &root.unit;
        let rights = RightsSet::claimed_by(&unit.ir);
        graph.node(
            &unit.name,
            &unit.name,
            "agent",
            &[
                ("role", role_label(unit.ir.role).to_owned()),
                ("depth", "0".to_owned()),
                ("access", access(!is_writing(&rights))),
            ],
        );
        for child in &unit.children {
            graph.node(
                &child.name,
                &child.name,
                "agent",
                &[
                    ("role", role_label(child.role).to_owned()),
                    ("depth", child.depth.to_string()),
                    ("access", access(child.read_only)),
                ],
            );
            graph.edge(&child.parent, &child.name, &child.via);
        }
    }
    graph
}

/// The resolution graph of one agent: the applied definitions in order
/// (`base`, `mixin`, `patch` steps from the trace) with their layer and
/// file, and the target's own `[patch.*]` operations.
#[must_use]
pub fn resolution_graph(compiled: &Compiled, sources: &SourceSet) -> Graph {
    let unit = &compiled.unit;
    let mut graph = Graph {
        title: format!("resolution of {}", unit.name),
        ..Graph::default()
    };
    let mut previous: Option<String> = None;
    for step in &unit.ir.trace.steps {
        let entry = sources.find_id(&step.source);
        let attrs = [
            (
                "layer",
                entry
                    .map(|entry| format!("{:?}", entry.layer))
                    .unwrap_or_else(|| "?".to_owned()),
            ),
            (
                "file",
                entry
                    .map(|entry| entry.label.display().to_string())
                    .unwrap_or_default(),
            ),
        ];
        graph.node(&step.source, &step.source, "definition", &attrs);
        if let Some(from) = previous.as_ref().filter(|from| **from != step.source) {
            graph.edge(from, &step.source, &step.kind);
        }
        previous = Some(step.source.clone());
    }
    let target = unit.ir.id.to_string();
    graph.node(&target, &target, "definition", &[("layer", "target".to_owned())]);
    if let Some(from) = previous.filter(|from| from != &target) {
        graph.edge(&from, &target, "extends");
    }
    for (path, op) in own_patches(compiled, sources) {
        let id = format!("patch:{path}");
        graph.node(&id, &format!("{path} ({op})"), "patch", &[("op", op.clone())]);
        graph.edge(&id, &target, "patch");
    }
    graph
}

/// Merge operators of a patch table.
pub const PATCH_OPS: &[&str] = &[
    "replace",
    "append",
    "prepend",
    "remove",
    "intersect",
    "min",
    "max-within-parent",
];

/// The `[patch.*]` operations of the target's own file: `(path, op)`.
#[must_use]
pub fn own_patches(compiled: &Compiled, sources: &SourceSet) -> Vec<(String, String)> {
    let target = compiled.unit.ir.id.to_string();
    let Some(entry) = sources.find_id(&target) else {
        return Vec::new();
    };
    let Some(file) = sources.file(&entry.label) else {
        return Vec::new();
    };
    let Ok(table) = toml::from_str::<toml::Table>(&file.text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(toml::Value::Table(patch)) = table.get("patch") {
        collect_patches(patch, "", &mut out);
    }
    out
}

fn collect_patches(table: &toml::Table, prefix: &str, out: &mut Vec<(String, String)>) {
    for (key, value) in table {
        if PATCH_OPS.contains(&key.as_str()) {
            out.push((prefix.to_owned(), key.clone()));
            continue;
        }
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            toml::Value::Table(inner) => collect_patches(inner, &path, out),
            _ => out.push((path, "value".to_owned())),
        }
    }
}

/// The rights flow of one agent: manifest → base role → author ceiling,
/// each edge labelled with its narrowings or widenings.
#[must_use]
pub fn rights_graph(compiled: &Compiled) -> Graph {
    let unit = &compiled.unit;
    let mut graph = Graph {
        title: format!("rights of {}", unit.name),
        ..Graph::default()
    };
    let Some(flow) = &unit.rights else {
        return graph;
    };
    let manifest = format!("manifest:{}", unit.name);
    graph.node(
        &manifest,
        &format!("{} manifest", unit.name),
        "rights",
        &[
            ("tools", flow.manifest.tools.len().to_string()),
            ("classes", join(flow.manifest.classes.iter())),
        ],
    );
    let mut upper = manifest.clone();
    if let Some(base) = &flow.base {
        let id = format!("base:{}", base.base_role);
        graph.node(
            &id,
            &format!("base role {} ({})", base.base_role, base.reason),
            "rights",
            &[
                ("tools", base.rights.tools.len().to_string()),
                ("classes", join(base.rights.classes.iter())),
            ],
        );
        let label = if flow.over_base.is_empty() {
            format!("≤ (narrowed: {} tools)", flow.narrowed_tools.len())
        } else {
            format!("WIDENS: {}", flow.over_base.lines().join("; "))
        };
        graph.edge(&manifest, &id, &label);
        upper = id;
    }
    if let Some(ceiling) = &flow.ceiling {
        let id = "ceiling:author".to_owned();
        graph.node(
            &id,
            "author ceiling",
            "rights",
            &[
                ("tools", ceiling.tools.len().to_string()),
                ("classes", join(ceiling.classes.iter())),
            ],
        );
        let delta = if flow.base.is_some() {
            &flow.base_over_ceiling
        } else {
            &flow.over_ceiling
        };
        let label = if delta.is_empty() {
            "≤".to_owned()
        } else {
            format!("narrowed by the ceiling: {}", delta.lines().join("; "))
        };
        graph.edge(&upper, &id, &label);
    }
    graph
}

fn join<'a>(items: impl Iterator<Item = &'a String>) -> String {
    items.cloned().collect::<Vec<_>>().join(",")
}

/// Renders graphs in a format.
#[must_use]
pub fn render(graphs: &[Graph], format: GraphFormat) -> String {
    match format {
        GraphFormat::Text => graphs.iter().map(render_text).collect::<Vec<_>>().join("\n"),
        GraphFormat::Dot => graphs.iter().map(render_dot).collect::<Vec<_>>().join("\n"),
        GraphFormat::Mermaid => graphs
            .iter()
            .map(render_mermaid)
            .collect::<Vec<_>>()
            .join("\n"),
        GraphFormat::Json => serde_json::to_string_pretty(graphs).unwrap_or_default(),
    }
}

fn attrs_text(node: &GraphNode) -> String {
    if node.attrs.is_empty() {
        return String::new();
    }
    let attrs: Vec<String> = node
        .attrs
        .iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    format!(" [{}]", attrs.join(", "))
}

fn render_text(graph: &Graph) -> String {
    let mut out = format!("{}:\n", graph.title);
    for node in &graph.nodes {
        out.push_str(&format!("  {}{}\n", node.label, attrs_text(node)));
        for edge in graph.edges.iter().filter(|edge| edge.from == node.id) {
            let label = graph
                .nodes
                .iter()
                .find(|candidate| candidate.id == edge.to)
                .map_or(edge.to.as_str(), |candidate| candidate.label.as_str());
            out.push_str(&format!("    └─{}→ {label}\n", edge.label));
        }
    }
    out
}

fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn render_dot(graph: &Graph) -> String {
    let mut out = format!("digraph {} {{\n  rankdir=LR;\n", quote(&graph.title));
    for node in &graph.nodes {
        let shape = if node.kind == "agent" && node.attrs.get("access").map(String::as_str) == Some("writing") {
            "box, style=bold"
        } else {
            "box"
        };
        out.push_str(&format!(
            "  {} [label={}, shape={shape}];\n",
            quote(&node.id),
            quote(&format!("{}{}", node.label, attrs_text(node)))
        ));
    }
    for edge in &graph.edges {
        out.push_str(&format!(
            "  {} -> {} [label={}];\n",
            quote(&edge.from),
            quote(&edge.to),
            quote(&edge.label)
        ));
    }
    out.push_str("}\n");
    out
}

fn mermaid_id(id: &str) -> String {
    let mut out: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    out.insert(0, 'n');
    out
}

fn mermaid_text(text: &str) -> String {
    text.replace('"', "'")
}

fn render_mermaid(graph: &Graph) -> String {
    let mut out = format!("%% {}\ngraph LR\n", graph.title);
    for node in &graph.nodes {
        out.push_str(&format!(
            "  {}[\"{}\"]\n",
            mermaid_id(&node.id),
            mermaid_text(&format!("{}{}", node.label, attrs_text(node)))
        ));
    }
    for edge in &graph.edges {
        out.push_str(&format!(
            "  {} -->|\"{}\"| {}\n",
            mermaid_id(&edge.from),
            mermaid_text(&edge.label),
            mermaid_id(&edge.to)
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn family() -> Graph {
        let mut graph = Graph {
            title: "delegation".to_owned(),
            ..Graph::default()
        };
        graph.node(
            "lead",
            "lead",
            "agent",
            &[("role", "child-orchestrator".to_owned()), ("access", "read-only".to_owned())],
        );
        graph.node(
            "reader",
            "reader",
            "agent",
            &[("depth", "1".to_owned()), ("access", "read-only".to_owned())],
        );
        graph.node(
            "writer",
            "writer",
            "agent",
            &[("depth", "1".to_owned()), ("access", "writing".to_owned())],
        );
        graph.edge("lead", "reader", "delegation");
        graph.edge("lead", "writer", "delegation");
        graph
    }

    #[test]
    fn test_text_dot_and_mermaid_render_a_small_family() {
        let graphs = [family()];
        let text = render(&graphs, GraphFormat::Text);
        assert!(text.contains("lead [access=read-only, role=child-orchestrator]"), "{text}");
        assert!(text.contains("└─delegation→ writer"), "{text}");
        let dot = render(&graphs, GraphFormat::Dot);
        assert!(dot.starts_with("digraph \"delegation\" {"), "{dot}");
        assert!(dot.contains("\"lead\" -> \"writer\" [label=\"delegation\"];"), "{dot}");
        assert!(dot.contains("style=bold"), "writers are bold: {dot}");
        let mermaid = render(&graphs, GraphFormat::Mermaid);
        assert!(mermaid.contains("graph LR"), "{mermaid}");
        assert!(mermaid.contains("nlead -->|\"delegation\"| nreader"), "{mermaid}");
        let json = render(&graphs, GraphFormat::Json);
        assert!(json.contains("\"from\": \"lead\""), "{json}");
    }

    #[test]
    fn test_formats_and_kinds_parse() {
        assert_eq!(GraphFormat::parse("dot"), Some(GraphFormat::Dot));
        assert_eq!(GraphFormat::parse("svg"), None);
        assert_eq!(GraphKind::parse("rights"), Some(GraphKind::Rights));
        assert_eq!(GraphKind::parse("x"), None);
    }

    #[test]
    fn test_patch_collection_finds_operators_and_values() {
        let table: toml::Table = toml::from_str(
            "[tools.admitted]\nappend = [\"x\"]\n[limits]\nmax_tool_calls = { min = 4 }\n",
        )
        .unwrap_or_default();
        let mut out = Vec::new();
        collect_patches(&table, "", &mut out);
        assert!(out.contains(&("tools.admitted".to_owned(), "append".to_owned())));
        assert!(out.contains(&("limits.max_tool_calls".to_owned(), "min".to_owned())));
    }
}
