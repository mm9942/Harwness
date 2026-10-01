//! `PalaceToolProvider` — die lesenden Modell-Werkzeuge des Palace
//! (`docs/design/knowledge-surfaces.md` §2.2/§2.3, Runde 4 D4):
//! `palace.search` und `palace.recall`.
//!
//! # Verantwortung
//! Agenten dürfen den Palace **lesen** — und zwar nur geteiltes, durch den
//! Review-Gate gegangenes Wissen. Beide Werkzeuge sehen ausschließlich
//! Knoten, für die [`harw_knowledge::memory::palace::is_agent_readable`]
//! gilt: Art `PalaceNode`, Status `established`, Sichtbarkeit
//! `OperatorOnly` (die Sichtbarkeit des Operator-Promote-Pfads).
//! `provisional` und `superseded` Knoten, Themen, Diary, Workbench und alle
//! übrigen Flächen bleiben unsichtbar — auch über Backlink-Hops, weil die
//! Suche auf einem eigens gefilterten Teil-Index läuft
//! ([`agent_view`]), der nur solche Knoten enthält.
//!
//! - `palace.search {query, limit?}` — Stichwortsuche (BM25-lite aus
//!   `harw_knowledge::memory::recall`), ohne Hops; je Treffer Id, Titel,
//!   Score und ein kurzer Ausschnitt.
//! - `palace.recall {query, max_hops?, limit?}` — derselbe Recall mit
//!   begrenzter Backlink-Expansion (höchstens [`MAX_RECALL_HOPS`] Hops);
//!   liefert die Bodies (je gekürzt) samt Hop-Pfad und ausgehenden Links.
//!
//! # Rechteklasse: read-only
//! Keine Schreibwirkung, kein Netz, kein Prozess. Darum
//! [`Permission::ReadWorkspace`] und `parallel_safe`. Die Ausgabe ist
//! gedeckelt ([`MAX_OUTPUT_CHARS`], Treffer- und Hop-Obergrenzen), sodass
//! das Werkzeug nie zum unbegrenzten Kontext-Dump wird (§2.3).
//!
//! # Nebenläufigkeit
//! `Send + Sync`; hält nur ein `Arc<KnowledgeStore>`. Der Index kommt aus
//! dem prozessweiten Cache `KnowledgeIndex::cached` (mtime-Signatur).
//!
//! # Fehler
//! Kein Aufruf gibt `Err` zurück: ungültige Argumente und Speicherfehler
//! werden als [`ToolOutput::error`] gemeldet.

use std::collections::BTreeMap;
use std::sync::Arc;

use harw_extension_api::{
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolName, ToolOutput,
    ToolSpec,
};
use harw_knowledge::memory::palace::{TITLE_KEY, is_agent_readable, scan_wikilinks};
use harw_knowledge::memory::recall::search;
use harw_knowledge::{
    ArtifactKind, KnowledgeArtifact, KnowledgeIndex, KnowledgeStore, RecallQuery, VisibilityScope,
};
use harw_tools::{AdditionalProperties, FunctionToolSpec, JsonSchema, JsonSchemaType, Permission};
use serde::Deserialize;

/// Name des Such-Werkzeugs.
pub const PALACE_SEARCH: &str = "palace.search";

/// Name des Recall-Werkzeugs.
pub const PALACE_RECALL: &str = "palace.recall";

/// Vorgabe für `limit`.
const DEFAULT_LIMIT: usize = 5;

/// Obergrenze für `limit` bei `palace.search`.
const MAX_SEARCH_LIMIT: usize = 20;

/// Obergrenze für `limit` bei `palace.recall` (liefert Bodies, daher enger).
const MAX_RECALL_LIMIT: usize = 10;

/// Vorgabe für `max_hops` bei `palace.recall`.
const DEFAULT_RECALL_HOPS: u8 = 1;

/// Obergrenze für `max_hops` bei `palace.recall`.
pub const MAX_RECALL_HOPS: u8 = 2;

/// Länge eines Such-Ausschnitts (Unicode-Zeichen).
const SNIPPET_CHARS: usize = 240;

/// Länge eines Recall-Bodys je Knoten (Unicode-Zeichen).
const RECALL_BODY_CHARS: usize = 2_000;

/// Gesamtdeckel der Textmenge (Ausschnitte bzw. Bodies) je Aufruf.
pub const MAX_OUTPUT_CHARS: usize = 12_000;

/// Die Palace-Lesewerkzeuge über einem geteilten Wissensspeicher.
#[derive(Debug, Clone)]
pub struct PalaceToolProvider {
    store: Arc<KnowledgeStore>,
}

impl PalaceToolProvider {
    /// Baut den Provider über dem Speicher der Montage.
    ///
    /// # Argumente
    /// - `store` (`Arc<KnowledgeStore>`): derselbe Speicher, den
    ///   `RuntimeServices::knowledge_store` auf Slash/Modell-Werkzeug legt.
    #[must_use]
    pub fn new(store: Arc<KnowledgeStore>) -> Self {
        Self { store }
    }
}

// Beide Palace-Werkzeuge lesen nur (`ReadWorkspace`) und sind parallelsicher.
harw_tools::tool_provider! {
    impl for PalaceToolProvider as provider, parallel_safe: all {
        PALACE_SEARCH => {
            spec: search_spec(),
            permission: Permission::ReadWorkspace,
            executor: PalaceExecutor { store: Arc::clone(&provider.store), tool: PalaceTool::Search },
        },
        PALACE_RECALL => {
            spec: recall_spec(),
            permission: Permission::ReadWorkspace,
            executor: PalaceExecutor { store: Arc::clone(&provider.store), tool: PalaceTool::Recall },
        },
    }
}

fn typed(schema_type: JsonSchemaType, description: &str) -> JsonSchema {
    JsonSchema {
        schema_type: Some(schema_type),
        description: Some(description.to_owned()),
        ..Default::default()
    }
}

fn object_schema(props: BTreeMap<String, JsonSchema>, required: &[&str]) -> JsonSchema {
    JsonSchema {
        schema_type: Some(JsonSchemaType::Object),
        properties: Some(props),
        required: Some(required.iter().map(|name| (*name).to_owned()).collect()),
        additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
        ..Default::default()
    }
}

fn search_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    props.insert(
        "query".to_owned(),
        typed(JsonSchemaType::String, "Stichwörter für die Suche."),
    );
    props.insert(
        "limit".to_owned(),
        typed(
            JsonSchemaType::Integer,
            "Höchstzahl Treffer (1–20, Vorgabe 5).",
        ),
    );
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(PALACE_SEARCH),
        description: "Durchsucht den Palace — das geprüfte, geteilte Langzeitwissen \
             (nur Knoten mit Status established). Liefert Id, Titel, Score und einen \
             kurzen Ausschnitt je Treffer; den Inhalt holt palace.recall. Nur lesend."
            .to_owned(),
        parameters: object_schema(props, &["query"]),
        strict: true,
    })
}

fn recall_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    props.insert(
        "query".to_owned(),
        typed(JsonSchemaType::String, "Stichwörter für den Recall."),
    );
    props.insert(
        "max_hops".to_owned(),
        typed(
            JsonSchemaType::Integer,
            "Wie viele Backlink-Hops vom Treffer aus mitgenommen werden (0–2, Vorgabe 1).",
        ),
    );
    props.insert(
        "limit".to_owned(),
        typed(
            JsonSchemaType::Integer,
            "Höchstzahl Knoten insgesamt (1–10, Vorgabe 5).",
        ),
    );
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(PALACE_RECALL),
        description: "Ruft Wissen aus dem Palace ab (nur established): die passenden Knoten \
             mit Inhalt (gekürzt), ausgehenden Links und über Backlinks verbundene Knoten \
             bis zur Hop-Grenze. Nur lesend; Ausgabe gedeckelt."
            .to_owned(),
        parameters: object_schema(props, &["query"]),
        strict: true,
    })
}

/// Welches der beiden Werkzeuge ein Executor bedient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PalaceTool {
    Search,
    Recall,
}

struct PalaceExecutor {
    store: Arc<KnowledgeStore>,
    tool: PalaceTool,
}

impl ToolExecutor for PalaceExecutor {
    fn execute<'a>(
        &'a self,
        _context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        let arguments = call.arguments.clone();
        Box::pin(async move {
            Ok(match self.tool {
                PalaceTool::Search => execute_search(&self.store, arguments),
                PalaceTool::Recall => execute_recall(&self.store, arguments),
            })
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    query: String,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecallArgs {
    query: String,
    #[serde(default)]
    max_hops: Option<u8>,
    #[serde(default)]
    limit: Option<usize>,
}

/// Der Teil-Index, den Agenten sehen: nur [`is_agent_readable`]-Knoten.
///
/// # Beschreibung
/// Weil der Teil-Index ausschließlich freigegebene Knoten enthält, kann
/// weder die direkte Suche noch die Backlink-Expansion einen `provisional`
/// oder `superseded` Knoten (oder eine andere Fläche) erreichen. Alle
/// enthaltenen Knoten sind `OperatorOnly`; der Recall läuft darum mit
/// diesem Scope — die eigentliche Zugriffsentscheidung ist der Filter hier.
fn agent_view(store: &KnowledgeStore) -> Result<KnowledgeIndex, String> {
    let full = KnowledgeIndex::cached(store).map_err(|error| error.to_string())?;
    let mut view = KnowledgeIndex::new();
    for artifact in full.iter().filter(|artifact| is_agent_readable(artifact)) {
        view.insert(artifact.clone());
    }
    Ok(view)
}

/// Titel eines Knotens (`extra.title`, sonst die Id).
fn node_title(node: &KnowledgeArtifact) -> &str {
    node.frontmatter
        .extra
        .get(TITLE_KEY)
        .and_then(serde_json::Value::as_str)
        .unwrap_or(node.id.as_str())
}

/// Kürzt `text` auf höchstens `max` Unicode-Zeichen (mit `…`).
fn clamp_chars(text: &str, max: usize) -> (String, bool) {
    let trimmed = text.trim();
    match trimmed.char_indices().nth(max) {
        None => (trimmed.to_owned(), false),
        Some((cut, _)) => (format!("{}…", &trimmed[..cut]), true),
    }
}

/// Prüft eine optionale Schranke gegen `1..=max`.
fn bounded(value: Option<usize>, default: usize, max: usize, field: &str) -> Result<usize, String> {
    let value = value.unwrap_or(default);
    if value == 0 || value > max {
        return Err(format!("{field} muss zwischen 1 und {max} liegen"));
    }
    Ok(value)
}

/// Baut die Recall-Anfrage über dem Agenten-Teil-Index.
fn palace_query(text: &str, max_hops: u8, limit: usize) -> RecallQuery {
    let mut query = RecallQuery::new(text.trim(), VisibilityScope::OperatorOnly);
    query.kinds = vec![ArtifactKind::PalaceNode];
    query.max_hops = max_hops;
    query.max_artifacts = limit;
    query
}

/// Kern von `palace.search` (testbar ohne Sandbox-Kontext).
fn execute_search(store: &KnowledgeStore, arguments: serde_json::Value) -> ToolOutput {
    let args: SearchArgs = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(error) => {
            return ToolOutput::error(format!("{PALACE_SEARCH}: ungültige Argumente: {error}"));
        }
    };
    if args.query.trim().is_empty() {
        return ToolOutput::error(format!("{PALACE_SEARCH}: query ist leer"));
    }
    let limit = match bounded(args.limit, DEFAULT_LIMIT, MAX_SEARCH_LIMIT, "limit") {
        Ok(limit) => limit,
        Err(error) => return ToolOutput::error(format!("{PALACE_SEARCH}: {error}")),
    };
    let view = match agent_view(store) {
        Ok(view) => view,
        Err(error) => return ToolOutput::error(format!("{PALACE_SEARCH}: {error}")),
    };
    let result = match search(&view, &palace_query(&args.query, 0, limit)) {
        Ok(result) => result,
        Err(error) => return ToolOutput::error(format!("{PALACE_SEARCH}: {error}")),
    };
    let mut budget = MAX_OUTPUT_CHARS;
    let mut truncated = result.truncated;
    let mut hits = Vec::new();
    for hit in &result.hits {
        let Some(node) = view.get(&hit.artifact.id) else {
            continue;
        };
        if budget == 0 {
            truncated = true;
            break;
        }
        let (snippet, _) = clamp_chars(&node.body, SNIPPET_CHARS.min(budget));
        budget = budget.saturating_sub(snippet.chars().count());
        hits.push(serde_json::json!({
            "id": node.id.as_str(),
            "title": node_title(node),
            "score": hit.score,
            "snippet": snippet,
        }));
    }
    ToolOutput::json(serde_json::json!({ "hits": hits, "truncated": truncated }))
}

/// Kern von `palace.recall` (testbar ohne Sandbox-Kontext).
fn execute_recall(store: &KnowledgeStore, arguments: serde_json::Value) -> ToolOutput {
    let args: RecallArgs = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(error) => {
            return ToolOutput::error(format!("{PALACE_RECALL}: ungültige Argumente: {error}"));
        }
    };
    if args.query.trim().is_empty() {
        return ToolOutput::error(format!("{PALACE_RECALL}: query ist leer"));
    }
    let max_hops = args.max_hops.unwrap_or(DEFAULT_RECALL_HOPS);
    if max_hops > MAX_RECALL_HOPS {
        return ToolOutput::error(format!(
            "{PALACE_RECALL}: max_hops muss zwischen 0 und {MAX_RECALL_HOPS} liegen"
        ));
    }
    let limit = match bounded(args.limit, DEFAULT_LIMIT, MAX_RECALL_LIMIT, "limit") {
        Ok(limit) => limit,
        Err(error) => return ToolOutput::error(format!("{PALACE_RECALL}: {error}")),
    };
    let view = match agent_view(store) {
        Ok(view) => view,
        Err(error) => return ToolOutput::error(format!("{PALACE_RECALL}: {error}")),
    };
    let result = match search(&view, &palace_query(&args.query, max_hops, limit)) {
        Ok(result) => result,
        Err(error) => return ToolOutput::error(format!("{PALACE_RECALL}: {error}")),
    };
    let mut budget = MAX_OUTPUT_CHARS;
    let mut truncated = result.truncated;
    let mut nodes = Vec::new();
    for hit in &result.hits {
        let Some(node) = view.get(&hit.artifact.id) else {
            continue;
        };
        if budget == 0 {
            truncated = true;
            break;
        }
        let (body, clipped) = clamp_chars(&node.body, RECALL_BODY_CHARS.min(budget));
        truncated |= clipped;
        budget = budget.saturating_sub(body.chars().count());
        // Ausgehende Links nur auf ebenfalls freigegebene Knoten.
        let mut links: Vec<String> = node
            .frontmatter
            .links
            .iter()
            .map(ToString::to_string)
            .chain(scan_wikilinks(&node.body))
            .filter(|target| {
                view.get(&harw_knowledge::ArtifactId::new(target.as_str()))
                    .is_some()
            })
            .collect();
        links.sort();
        links.dedup();
        let via: Vec<&str> = hit.hop_path.iter().map(|id| id.as_str()).collect();
        nodes.push(serde_json::json!({
            "id": node.id.as_str(),
            "title": node_title(node),
            "score": hit.score,
            "via": via,
            "links": links,
            "body": body,
        }));
    }
    ToolOutput::json(serde_json::json!({
        "nodes": nodes,
        "max_hops": max_hops,
        "truncated": truncated,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_extension_api::contributors::ToolProvider;
    use harw_knowledge::memory::palace::{PalaceStatus, set_status};
    use harw_knowledge::memory::topic;
    use harw_knowledge::{AgentId, ArtifactId, Frontmatter};

    fn temporary_store(label: &str) -> TestResult<Arc<KnowledgeStore>> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(ctx("system clock is after epoch"))?
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "harw-registry-palace-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).map_err(ctx("create root"))?;
        Ok(Arc::new(KnowledgeStore::new(&root)))
    }

    fn write_node(
        store: &KnowledgeStore,
        slug: &str,
        status: PalaceStatus,
        visibility: VisibilityScope,
        body: &str,
    ) -> TestResult {
        let mut frontmatter = Frontmatter::new(
            AgentId::new("operator"),
            visibility,
            jiff::Timestamp::UNIX_EPOCH,
        );
        set_status(&mut frontmatter, status);
        frontmatter.links = scan_wikilinks(body)
            .into_iter()
            .map(ArtifactId::new)
            .collect();
        frontmatter
            .extra
            .insert(TITLE_KEY.to_owned(), serde_json::json!(format!("T {slug}")));
        let artifact = KnowledgeArtifact::new(
            ArtifactId::new(format!("palace/{slug}")),
            ArtifactKind::PalaceNode,
            frontmatter,
            body,
        );
        store
            .write_artifact(&store.palace_path(&ArtifactId::new(slug)), &artifact)
            .map_err(ctx("write node"))
    }

    fn json(output: ToolOutput) -> TestResult<serde_json::Value> {
        match output {
            ToolOutput::Json { content } => Ok(content),
            other => Err(TestError::Unexpected(format!(
                "expected JSON output, got {other:?}"
            ))),
        }
    }

    fn ids(value: &serde_json::Value, key: &str) -> Vec<String> {
        value[key]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item["id"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Ein Speicher mit je einem Knoten pro Sichtbarkeitsfall, alle zum
    /// Stichwort „canary“; `hub` ist established und wird von allen verlinkt.
    fn seeded(label: &str) -> TestResult<Arc<KnowledgeStore>> {
        let store = temporary_store(label)?;
        write_node(
            &store,
            "hub",
            PalaceStatus::Established,
            VisibilityScope::OperatorOnly,
            "Canary Deploy Grundlagen.",
        )?;
        write_node(
            &store,
            "shared",
            PalaceStatus::Established,
            VisibilityScope::OperatorOnly,
            "Canary Rollback, siehe [[palace/hub]].",
        )?;
        write_node(
            &store,
            "draft",
            PalaceStatus::Provisional,
            VisibilityScope::OperatorOnly,
            "Canary Entwurf, siehe [[palace/hub]].",
        )?;
        write_node(
            &store,
            "retired",
            PalaceStatus::Superseded,
            VisibilityScope::OperatorOnly,
            "Canary alt, siehe [[palace/hub]].",
        )?;
        write_node(
            &store,
            "private",
            PalaceStatus::Established,
            VisibilityScope::SelfOnly,
            "Canary privat, siehe [[palace/hub]].",
        )?;
        let mut frontmatter = Frontmatter::new(
            AgentId::new("operator"),
            VisibilityScope::OperatorOnly,
            jiff::Timestamp::UNIX_EPOCH,
        );
        set_status(&mut frontmatter, PalaceStatus::Established);
        topic::write(
            &store,
            "topic-canary",
            frontmatter,
            "Canary Thema [[palace/hub]]",
        )
        .map_err(ctx("write topic"))?;
        Ok(store)
    }

    #[test]
    fn provider_lists_exactly_the_two_read_only_tools() {
        let provider =
            PalaceToolProvider::new(Arc::new(KnowledgeStore::new(std::path::Path::new("/x"))));
        let names: Vec<String> = provider
            .tools()
            .iter()
            .map(|spec| {
                let ToolSpec::Function(function) = spec;
                function.name.as_str().to_owned()
            })
            .collect();
        assert_eq!(
            names,
            vec![PALACE_SEARCH.to_owned(), PALACE_RECALL.to_owned()]
        );
        for name in PalaceToolProvider::TOOL_NAMES {
            assert!(provider.executor(&ToolName::new(*name)).is_some(), "{name}");
            assert!(provider.parallel_safe(&ToolName::new(*name)), "{name}");
        }
        assert!(
            provider
                .executor(&ToolName::new("palace.promote"))
                .is_none()
        );
        assert_eq!(
            PalaceToolProvider::TOOL_NAMES.len(),
            PalaceToolProvider::TOOL_PERMISSIONS.len()
        );
        for permission in PalaceToolProvider::TOOL_PERMISSIONS {
            assert_eq!(*permission, Some(Permission::ReadWorkspace));
        }
    }

    #[test]
    fn search_sees_only_established_shared_nodes() -> TestResult {
        let store = seeded("search")?;
        let found = json(execute_search(
            &store,
            serde_json::json!({ "query": "canary", "limit": 20 }),
        ))?;
        let mut hits = ids(&found, "hits");
        hits.sort();
        assert_eq!(hits, vec!["palace/hub", "palace/shared"], "{found}");
        assert_eq!(
            found["hits"][0]["title"]
                .as_str()
                .map(|t| t.starts_with("T ")),
            Some(true)
        );
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn recall_hops_never_reach_provisional_or_superseded_nodes() -> TestResult {
        let store = seeded("recall")?;
        // „Grundlagen“ trifft nur `hub` direkt; alle anderen verlinken `hub`
        // und wären über einen Backlink-Hop erreichbar.
        let recalled = json(execute_recall(
            &store,
            serde_json::json!({ "query": "Grundlagen", "max_hops": 2, "limit": 10 }),
        ))?;
        let nodes = ids(&recalled, "nodes");
        assert_eq!(nodes, vec!["palace/hub", "palace/shared"], "{recalled}");
        assert_eq!(recalled["nodes"][1]["via"][0], "palace/hub");
        assert_eq!(recalled["nodes"][1]["links"][0], "palace/hub");
        assert!(
            recalled["nodes"][0]["body"]
                .as_str()
                .is_some_and(|body| body.contains("Grundlagen"))
        );

        let direct = json(execute_recall(
            &store,
            serde_json::json!({ "query": "Grundlagen", "max_hops": 0 }),
        ))?;
        assert_eq!(ids(&direct, "nodes"), vec!["palace/hub"]);
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn bounds_and_arguments_are_enforced() -> TestResult {
        let store = seeded("bounds")?;
        let is_error = |output: ToolOutput| matches!(output, ToolOutput::Error { .. });
        assert!(is_error(execute_recall(
            &store,
            serde_json::json!({ "query": "canary", "max_hops": 3 })
        )));
        assert!(is_error(execute_recall(
            &store,
            serde_json::json!({ "query": "canary", "limit": 11 })
        )));
        assert!(is_error(execute_search(
            &store,
            serde_json::json!({ "query": "canary", "limit": 0 })
        )));
        assert!(is_error(execute_search(
            &store,
            serde_json::json!({ "query": "  " })
        )));
        assert!(is_error(execute_search(
            &store,
            serde_json::json!({ "query": "canary", "scope": "all" })
        )));
        // strict-Modus: ausgelassene Felder kommen als null.
        let nulls = json(execute_recall(
            &store,
            serde_json::json!({ "query": "canary", "max_hops": null, "limit": null }),
        ))?;
        assert_eq!(nulls["max_hops"], 1);
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn output_is_capped() -> TestResult {
        let store = temporary_store("cap")?;
        let long = format!("canary {}", "x".repeat(RECALL_BODY_CHARS * 2));
        for index in 0..8 {
            write_node(
                &store,
                &format!("n{index}"),
                PalaceStatus::Established,
                VisibilityScope::OperatorOnly,
                &long,
            )?;
        }
        let recalled = json(execute_recall(
            &store,
            serde_json::json!({ "query": "canary", "limit": 10, "max_hops": 0 }),
        ))?;
        assert_eq!(recalled["truncated"], true);
        let total: usize = recalled["nodes"]
            .as_array()
            .map(|nodes| {
                nodes
                    .iter()
                    .filter_map(|node| node["body"].as_str())
                    .map(|body| body.chars().count())
                    .sum()
            })
            .unwrap_or(0);
        // Je Body höchstens RECALL_BODY_CHARS plus Auslassungszeichen.
        assert!(total <= MAX_OUTPUT_CHARS + 8, "{total}");
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }
}
