//! `/palace` — der verlinkte Langzeitgedächtnis-Graph
//! (`docs/design/knowledge-surfaces.md` §2.2/§2.5, `interaction-contract.md` §2.2).
//!
//! # Subcommands
//! - (bare) / `list` — alle sichtbaren Knoten; `OpOutput::data` =
//!   `{"nodes":[{"id","title","status","links","updated"}]}`.
//! - `show <ArtifactRef>` — Knoten mit Titel, Status, Tags, Body, ausgehenden
//!   Links (`[[wikilinks]]` + Frontmatter-`links`) und berechneten Backlinks;
//!   `OpOutput::data` = `{"node":{"id","title","status","links","updated",
//!   "tags","backlinks","body"}}`.
//! - `search <query> [--max-hops=n] [--max=n]` — graphbewusster Recall über
//!   Palace-Knoten; begrenzt durch `RecallQuery::validate` (§2.3).
//! - `promote <topic-ref>` — Thema → Palace-Knoten (§2.5).
//! - `supersede <alt> <neu> [--confirm]` — `alt` wird `superseded` und
//!   verweist über `extra.superseded_by` (plus Frontmatter-Link) auf `neu`.
//! - `edit <id> <text…> [--confirm]` — ersetzt den Body; `[[wikilinks]]`
//!   werden zu Frontmatter-Links.
//! - `link <a> <b> [--confirm]` — ergänzt in `a` einen `[[palace/b]]`-Verweis.
//!
//! # Review-Gate der Schreibpfade (D4)
//! `supersede`, `edit` und `link` sind nur für den Operator (Sicht
//! `OperatorOnly`) erreichbar; ein Agenten-Principal bekommt
//! [`OpError::NotAvailable`]. Ein `provisional` Knoten darf frei geändert
//! werden. Jede Änderung an einem `established` Knoten ist eine neue
//! Revision und verlangt die ausdrückliche Bestätigung `--confirm` (wie das
//! ausdrückliche `/palace promote`, §2.5); die Vorfassung wird dabei an
//! `palace/<slug>.history.jsonl` angehängt und `extra.revision` hochgezählt.
//! Ein `superseded` Knoten ist eingefroren. Alle Schreibpfade halten die
//! Dateisperre des Knotens ([`KnowledgeLock::for_target`]) über den ganzen
//! Read-Modify-Write-Zyklus. Knoten werden nie gelöscht.
//!
//! # Index
//! Lesepfade nutzen [`KnowledgeIndex::cached`] (prozessweiter Cache mit
//! mtime-Signatur) statt bei jedem Aufruf neu aufzubauen.
//!
//! # Promotion-Gate
//! `promote` ist nur als Operator-Kommando erreichbar (kein Modell-Werkzeug,
//! `permission = "operator"`). Ein ausdrückliches `/palace promote` ist laut
//! §2.5 genau die Autorisierung, die das strengste Gate verlangt
//! (`ensure_promotion_reviewed(.., reviewed = true)`); der Knoten landet
//! darum sofort als `established`. Die zweite Vertragsvariante — ein
//! wartender Review-Eintrag für Aufrufer ohne Direktrecht — entsteht erst mit
//! einer Modell-/Kanal-Fläche dieses Pfads und ist hier bewusst nicht gebaut.
//! Ein bestehender Knoten wird nie überschrieben (Knoten werden ersetzt,
//! nicht gelöscht).
//!
//! # Sichtbarkeit
//! Alle Lesepfade laufen über `harw_knowledge::memory::recall::search_with`
//! mit der Sicht des echten Aufrufers
//! ([`crate::knowledge_common::KnowledgeCaller`]: Operator → `OperatorOnly`,
//! Agent → `SelfOnly`; dieselbe Disziplin wie `/context-proposal`);
//! `KnowledgeIndex::get` wird nur mit Ids aufgerufen, die der Recall bereits
//! als sichtbar bestätigt hat. Backlinks werden auf sichtbare Quellen
//! gefiltert. Ein Thema wird nur promotet, wenn es für den Aufrufer sichtbar
//! ist; der Knoten erbt seine Sichtbarkeit (nie eine Ausweitung). Nach
//! `promote`, `supersede`, `edit` und `link` geht
//! `AgentEventKind::Knowledge { area: "palace", id: "palace/<slug>" }` über
//! den Hub.
//!
//! # Fehler
//! - [`OpError::NotAvailable`] — kein Knowledge-Store im Kontext.
//! - [`OpError::InvalidArguments`] — Grammatik, unbekannte/unsichtbare Id,
//!   bereits existierender Knoten.
//! - [`OpError::Execution`] — Index-/Ein-/Ausgabefehler.

use harw_knowledge::artifact::{
    ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact, MAX_RECALL_ARTIFACTS,
    MAX_RECALL_HOPS, RecallQuery,
};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_knowledge::index::KnowledgeIndex;
use harw_knowledge::memory::palace::{
    PalaceStatus, REVISION_KEY, SUPERSEDED_BY_KEY, artifact_status, ensure_promotion_reviewed,
    scan_wikilinks, set_status,
};
use harw_knowledge::memory::recall::{ListAllRanker, search, search_with};
use harw_knowledge::memory::topic;
use harw_knowledge::{AgentId, KnowledgeLock, KnowledgeStore, VisibilityScope};
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};

use crate::knowledge_args::{FlagSpec, KnowledgeArgs};
use crate::knowledge_common::{
    AREA_PALACE, KnowledgeCaller, knowledge_store, map_knowledge_error, publish_knowledge,
};

/// Flags von `search`.
const SEARCH_FLAGS: &[FlagSpec] = &[FlagSpec::value("max-hops"), FlagSpec::value("max")];

/// Flags der Schreib-Subcommands `supersede`/`edit`/`link`.
const WRITE_FLAGS: &[FlagSpec] = &[FlagSpec::switch("confirm")];

/// Vorgabe für `--max` bei `search`.
const DEFAULT_SEARCH_MAX: usize = 10;

/// Vorgabe für `--max-hops` bei `search`.
const DEFAULT_SEARCH_HOPS: u8 = 2;

/// Frontmatter-`extra`-Schlüssel eines Palace-Knotens.
const TITLE_KEY: &str = "title";
const CONFIDENCE_KEY: &str = "confidence";
const PROMOTED_FROM_KEY: &str = "promoted_from";

/// Argument-Container für `/palace`: rohe Tokens.
#[derive(Debug, Default, serde::Deserialize)]
pub struct PalaceArgs {
    /// Alle Tokens nach `/palace`.
    #[serde(default)]
    pub tokens: Vec<String>,
}

impl harw_operations::FromRawArgs for PalaceArgs {
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        Ok(Self {
            tokens: tokens.to_vec(),
        })
    }
}

/// Führt `/palace` aus.
///
/// # Fehler
/// Siehe Moduldoku.
#[operation(
    name = "palace",
    summary = "Palace-Graph: show <id>, search <query> [--max-hops=n] [--max=n], promote <topic>, supersede <alt> <neu>, edit <id> <text>, link <a> <b> [--confirm].",
    domain = "knowledge",
    permission = "operator",
    command(
        path = "/palace",
        visibility = "channel_parity",
        busy_subcommands = "-=immediate, list=immediate, show=immediate, search=immediate"
    )
)]
async fn palace(ctx: &OpContext, args: PalaceArgs) -> Result<OpOutput, OpError> {
    let store = knowledge_store(ctx)?;
    let caller = KnowledgeCaller::from_context(ctx);
    let output = run_palace_as(&store, &caller, &args.tokens, jiff::Timestamp::now())?;
    if let Some(id) = written_node_id(&args.tokens) {
        publish_knowledge(ctx, AREA_PALACE, id);
    }
    Ok(output)
}

/// Für schreibende Subcommands die Id des geänderten Knotens
/// (`Some(Some(id))`), sonst `None` (nichts zu melden).
fn written_node_id(tokens: &[String]) -> Option<Option<String>> {
    let parsed = KnowledgeArgs::parse(tokens, WRITE_FLAGS).ok()?;
    let sub = parsed.subcommand()?;
    if !matches!(sub, "promote" | "supersede" | "edit" | "link") {
        return None;
    }
    Some(
        parsed
            .positionals()
            .get(1)
            .and_then(|raw| topic_slug(raw.trim_start_matches("palace/")).ok())
            .map(|slug| format!("palace/{slug}")),
    )
}

/// Der reine Kern von `/palace` aus Operator-Sicht (testbar ohne
/// `OpContext`); gleichbedeutend mit [`run_palace_as`] und
/// [`KnowledgeCaller::operator`].
///
/// # Fehler
/// Siehe Moduldoku.
pub fn run_palace(
    store: &KnowledgeStore,
    caller: &AgentId,
    tokens: &[String],
    now: jiff::Timestamp,
) -> Result<OpOutput, OpError> {
    run_palace_as(
        store,
        &KnowledgeCaller::operator(caller.clone()),
        tokens,
        now,
    )
}

/// Der reine Kern von `/palace` für einen beliebigen Aufrufer.
///
/// # Fehler
/// Siehe Moduldoku.
pub fn run_palace_as(
    store: &KnowledgeStore,
    caller: &KnowledgeCaller,
    tokens: &[String],
    now: jiff::Timestamp,
) -> Result<OpOutput, OpError> {
    let args = KnowledgeArgs::parse(tokens, &[])?;
    let tail = args.rest();
    let viewer = &caller.viewer;
    match args.subcommand() {
        None | Some("list") => list(store, viewer),
        Some("show") => show(store, viewer, tail),
        Some("search") => search_nodes(store, viewer, tail),
        Some("promote") => promote(store, caller, tail, now),
        Some("supersede") => supersede(store, caller, tail, now),
        Some("edit") => edit(store, caller, tail, now),
        Some("link") => link(store, caller, tail, now),
        Some(other) => Err(OpError::InvalidArguments(format!(
            "unbekannter /palace-Subcommand: {other} (list, show, search, promote, supersede, edit, link)"
        ))),
    }
}

/// Titel eines Knotens (`extra.title`, sonst die Id).
fn node_title(node: &KnowledgeArtifact) -> &str {
    node.frontmatter
        .extra
        .get(TITLE_KEY)
        .and_then(serde_json::Value::as_str)
        .unwrap_or(node.id.as_str())
}

/// Status eines Knotens (`extra.confidence`, sonst `provisional`).
fn node_status(node: &KnowledgeArtifact) -> &str {
    node.frontmatter
        .extra
        .get(CONFIDENCE_KEY)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("provisional")
}

/// Ausgehende Links: Frontmatter-`links` plus `[[wikilinks]]`, sortiert, eindeutig.
fn node_links(node: &KnowledgeArtifact) -> Vec<String> {
    let mut outgoing: Vec<String> = node
        .frontmatter
        .links
        .iter()
        .map(ToString::to_string)
        .chain(scan_wikilinks(&node.body))
        .collect();
    outgoing.sort();
    outgoing.dedup();
    outgoing
}

/// Kopf-Nutzlast eines Knotens (ohne Body/Backlinks).
fn node_summary(node: &KnowledgeArtifact) -> serde_json::Value {
    serde_json::json!({
        "id": node.id.as_str(),
        "title": node_title(node),
        "status": node_status(node),
        "links": node_links(node),
        "updated": node.frontmatter.updated_at.to_string(),
    })
}

fn list(store: &KnowledgeStore, viewer: &VisibilityScope) -> Result<OpOutput, OpError> {
    let index = rebuild(store)?;
    let nodes = visible_nodes(&index, viewer)?;
    let summaries: Vec<serde_json::Value> = nodes.iter().map(node_summary).collect();
    let data = serde_json::json!({ "nodes": summaries });
    if nodes.is_empty() {
        return Ok(OpOutput {
            text: "Keine sichtbaren Palace-Knoten.".to_owned(),
            data: Some(data),
        });
    }
    let mut out = format!("{} Palace-Knoten:\n", nodes.len());
    for node in &nodes {
        out.push_str(&format!(
            "· {} — {} [{}]\n",
            node.id,
            node_title(node),
            node_status(node)
        ));
    }
    Ok(OpOutput {
        text: out,
        data: Some(data),
    })
}

/// `palace/<slug>` aus `palace/<slug>` oder `<slug>`.
fn node_id(raw: &str) -> ArtifactId {
    let raw = raw.trim();
    if raw.starts_with("palace/") {
        ArtifactId::new(raw)
    } else {
        ArtifactId::new(format!("palace/{raw}"))
    }
}

fn rebuild(store: &KnowledgeStore) -> Result<Arc<KnowledgeIndex>, OpError> {
    KnowledgeIndex::cached(store).map_err(|error| {
        OpError::Execution(format!("Knowledge-Index-Aufbau fehlgeschlagen: {error}"))
    })
}

/// Alle für `viewer` sichtbaren Palace-Knoten (einziger Lesepfad).
fn visible_nodes(
    index: &KnowledgeIndex,
    viewer: &VisibilityScope,
) -> Result<Vec<KnowledgeArtifact>, OpError> {
    let mut query = RecallQuery::new(String::new(), viewer.clone());
    query.kinds = vec![ArtifactKind::PalaceNode];
    query.max_hops = 0;
    let result = search_with(index, &query, &ListAllRanker).map_err(map_knowledge_error)?;
    Ok(result
        .hits
        .iter()
        .filter_map(|hit| index.get(&hit.artifact.id).cloned())
        .collect())
}

fn show(
    store: &KnowledgeStore,
    viewer: &VisibilityScope,
    tail: &[String],
) -> Result<OpOutput, OpError> {
    let raw = tail
        .first()
        .ok_or_else(|| OpError::InvalidArguments("Aufruf: /palace show <id>".to_owned()))?;
    let target = node_id(raw);
    let index = rebuild(store)?;
    let node = visible_nodes(&index, viewer)?
        .into_iter()
        .find(|artifact| artifact.id == target)
        .ok_or_else(|| {
            OpError::InvalidArguments(format!("kein sichtbarer Palace-Knoten '{target}'"))
        })?;

    let title = node_title(&node);
    let status = node_status(&node);
    let outgoing = node_links(&node);
    let backlinks: Vec<String> = index
        .backlinks(&node.id)
        .into_iter()
        .filter(|source| source.visibility.visible_to_caller(viewer))
        .map(|source| source.id.to_string())
        .collect();

    let mut out = format!("{} — {title}\nStatus: {status}\n", node.id);
    if !node.frontmatter.tags.is_empty() {
        out.push_str(&format!("Tags: {}\n", node.frontmatter.tags.join(", ")));
    }
    out.push_str(&format!("\n{}\n\n", node.body.trim()));
    out.push_str(&format!("Links ({}):\n", outgoing.len()));
    for link in &outgoing {
        out.push_str(&format!("  → {link}\n"));
    }
    out.push_str(&format!("Backlinks ({}):\n", backlinks.len()));
    for link in &backlinks {
        out.push_str(&format!("  ← {link}\n"));
    }
    let mut data = node_summary(&node);
    if let Some(fields) = data.as_object_mut() {
        fields.insert("tags".to_owned(), serde_json::json!(node.frontmatter.tags));
        fields.insert("backlinks".to_owned(), serde_json::json!(backlinks));
        fields.insert("body".to_owned(), serde_json::json!(node.body.trim()));
    }
    Ok(OpOutput {
        text: out,
        data: Some(serde_json::json!({ "node": data })),
    })
}

fn search_nodes(
    store: &KnowledgeStore,
    viewer: &VisibilityScope,
    tail: &[String],
) -> Result<OpOutput, OpError> {
    let args = KnowledgeArgs::parse(tail, SEARCH_FLAGS)?;
    let hops_flag = args.value("max-hops");
    let max_flag = args.value("max");
    let text = args.positionals().join(" ");
    if text.trim().is_empty() {
        return Err(OpError::InvalidArguments(
            "Aufruf: /palace search <query> [--max-hops=n] [--max=n]".to_owned(),
        ));
    }
    let max_hops = match hops_flag {
        Some(raw) => parse_bound::<u8>(raw, "--max-hops", MAX_RECALL_HOPS)?,
        None => DEFAULT_SEARCH_HOPS,
    };
    let max_artifacts = match max_flag {
        Some(raw) => parse_bound::<usize>(raw, "--max", MAX_RECALL_ARTIFACTS)?,
        None => DEFAULT_SEARCH_MAX,
    };

    let index = rebuild(store)?;
    let mut query = RecallQuery::new(text.trim(), viewer.clone());
    query.kinds = vec![ArtifactKind::PalaceNode];
    query.max_hops = max_hops;
    query.max_artifacts = max_artifacts;
    let result = search(&index, &query).map_err(map_knowledge_error)?;
    // Strukturierte Nutzlast für den Wissensbrowser der TUI: Titel und
    // Status kommen aus dem vollen Knoten des Index (Treffer tragen nur die
    // leichte `ArtifactRef`).
    let hits: Vec<serde_json::Value> = result
        .hits
        .iter()
        .map(|hit| {
            let node = index.get(&hit.artifact.id);
            serde_json::json!({
                "id": hit.artifact.id.as_str(),
                "title": node.map_or(hit.artifact.id.as_str(), node_title),
                "status": node.map_or("provisional", node_status),
                "score": hit.score,
                "hop_path": hit
                    .hop_path
                    .iter()
                    .map(|id| id.as_str())
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    let data = serde_json::json!({
        "query": text.trim(),
        "truncated": result.truncated,
        "hits": hits,
    });
    if result.hits.is_empty() {
        return Ok(OpOutput {
            text: format!("Keine Palace-Treffer für '{}'.", text.trim()),
            data: Some(data),
        });
    }
    let mut out = format!("{} Palace-Treffer:\n", result.hits.len());
    for hit in &result.hits {
        out.push_str(&format!("· {} ({:.2})", hit.artifact.id, hit.score));
        if !hit.hop_path.is_empty() {
            let path: Vec<String> = hit.hop_path.iter().map(ToString::to_string).collect();
            out.push_str(&format!(" via {}", path.join(" → ")));
        }
        out.push('\n');
    }
    if result.truncated {
        out.push_str("(gekürzt — --max erhöhen)\n");
    }
    Ok(OpOutput {
        text: out,
        data: Some(data),
    })
}

/// Parst eine Schranke und prüft sie gegen die harte Obergrenze.
fn parse_bound<T>(raw: &str, flag: &str, max: T) -> Result<T, OpError>
where
    T: std::str::FromStr + PartialOrd + std::fmt::Display + Copy,
{
    let value: T = raw
        .trim()
        .parse()
        .map_err(|_| OpError::InvalidArguments(format!("{flag} erwartet eine Zahl: '{raw}'")))?;
    if value > max {
        return Err(OpError::InvalidArguments(format!(
            "{flag}={value} überschreitet die Obergrenze {max}"
        )));
    }
    Ok(value)
}

/// `topic/<slug>` oder `<slug>` → `<slug>`, als sichere Pfadkomponente.
fn topic_slug(raw: &str) -> Result<String, OpError> {
    let raw = raw.trim();
    let slug = raw.strip_prefix("topic/").unwrap_or(raw);
    let unsafe_slug = slug.is_empty()
        || slug == "."
        || slug == ".."
        || slug
            .chars()
            .any(|c| c == '/' || c == '\\' || c.is_control());
    if unsafe_slug {
        return Err(OpError::InvalidArguments(format!(
            "ungültige Themen-Id '{raw}'"
        )));
    }
    Ok(slug.to_owned())
}

fn promote(
    store: &KnowledgeStore,
    caller: &KnowledgeCaller,
    tail: &[String],
    now: jiff::Timestamp,
) -> Result<OpOutput, OpError> {
    let raw = tail
        .first()
        .ok_or_else(|| OpError::InvalidArguments("Aufruf: /palace promote <topic>".to_owned()))?;
    let slug = topic_slug(raw)?;
    let node = ArtifactId::new(format!("palace/{slug}"));
    let node_path = store.palace_path(&ArtifactId::new(slug.as_str()));
    if node_path.exists() {
        return Err(OpError::InvalidArguments(format!(
            "Palace-Knoten '{node}' existiert bereits; Knoten werden nie überschrieben"
        )));
    }
    if !store.topic_path(&slug).is_file() {
        return Err(OpError::InvalidArguments(format!(
            "kein sichtbares Thema 'topic/{slug}'"
        )));
    }
    let topic_artifact = topic::read(store, &slug).map_err(map_knowledge_error)?;
    if !caller.can_read(&topic_artifact.frontmatter.visibility) {
        return Err(OpError::InvalidArguments(format!(
            "kein sichtbares Thema 'topic/{slug}'"
        )));
    }
    // Ein ausdrückliches Operator-Kommando ist die Review-Autorisierung (§2.5).
    ensure_promotion_reviewed(&slug, &node, true).map_err(map_knowledge_error)?;

    let title = topic_artifact
        .frontmatter
        .extra
        .get(TITLE_KEY)
        .and_then(serde_json::Value::as_str)
        .map_or_else(|| slug.clone(), str::to_owned);
    let mut frontmatter = Frontmatter::new(
        caller.agent.clone(),
        topic_artifact.frontmatter.visibility.clone(),
        now,
    );
    frontmatter.tags = topic_artifact.frontmatter.tags.clone();
    frontmatter.links = scan_wikilinks(&topic_artifact.body)
        .into_iter()
        .map(ArtifactId::new)
        .collect();
    frontmatter.source_session_id = topic_artifact.frontmatter.source_session_id.clone();
    frontmatter
        .extra
        .insert(TITLE_KEY.to_owned(), serde_json::Value::String(title));
    frontmatter.extra.insert(
        CONFIDENCE_KEY.to_owned(),
        serde_json::to_value(PalaceStatus::Established)
            .map_err(|error| OpError::Execution(error.to_string()))?,
    );
    frontmatter.extra.insert(
        PROMOTED_FROM_KEY.to_owned(),
        serde_json::Value::String(format!("topic/{slug}")),
    );
    let artifact = KnowledgeArtifact::new(
        node.clone(),
        ArtifactKind::PalaceNode,
        frontmatter,
        topic_artifact.body.clone(),
    );
    store
        .write_artifact(&node_path, &artifact)
        .map_err(map_knowledge_error)?;
    Ok(OpOutput::from(format!(
        "topic/{slug} → {node} promotet (established)."
    )))
}

// ── Schreibpfade supersede / edit / link (D4) ──────────────────────────────

/// Ein geladener Knoten samt Dateipfad.
struct LoadedNode {
    id: ArtifactId,
    path: PathBuf,
    artifact: KnowledgeArtifact,
}

/// Review-Gate der Schreibpfade: nur der Operator darf Palace-Knoten ändern.
fn require_operator(caller: &KnowledgeCaller, sub: &str) -> Result<(), OpError> {
    if caller.viewer.is_operator_only() {
        Ok(())
    } else {
        Err(OpError::NotAvailable(format!(
            "/palace {sub} ist dem Operator vorbehalten"
        )))
    }
}

/// Lädt einen für den Aufrufer sichtbaren Knoten (`palace/<slug>` oder `<slug>`).
fn load_node(
    store: &KnowledgeStore,
    caller: &KnowledgeCaller,
    raw: &str,
) -> Result<LoadedNode, OpError> {
    let slug = topic_slug(raw.trim().trim_start_matches("palace/"))?;
    let id = ArtifactId::new(format!("palace/{slug}"));
    let path = store.palace_path(&ArtifactId::new(slug.as_str()));
    if !path.is_file() {
        return Err(OpError::InvalidArguments(format!(
            "kein sichtbarer Palace-Knoten '{id}'"
        )));
    }
    let artifact = store
        .read_artifact(&path, id.clone(), ArtifactKind::PalaceNode)
        .map_err(map_knowledge_error)?;
    if !caller.can_read(&artifact.frontmatter.visibility) {
        return Err(OpError::InvalidArguments(format!(
            "kein sichtbarer Palace-Knoten '{id}'"
        )));
    }
    Ok(LoadedNode { id, path, artifact })
}

/// Prüft, ob der Knoten geändert werden darf, und liefert, ob die Änderung
/// eine neue Revision ist (`established`).
fn ensure_editable(node: &LoadedNode, confirmed: bool, sub: &str) -> Result<bool, OpError> {
    match artifact_status(&node.artifact) {
        PalaceStatus::Provisional => Ok(false),
        PalaceStatus::Superseded => Err(OpError::InvalidArguments(format!(
            "{} ist superseded und eingefroren; Änderungen gehören an den Nachfolger",
            node.id
        ))),
        PalaceStatus::Established if confirmed => Ok(true),
        PalaceStatus::Established => Err(OpError::InvalidArguments(format!(
            "{} ist established: /palace {sub} erzeugt eine neue Revision und braucht \
             die ausdrückliche Bestätigung --confirm",
            node.id
        ))),
    }
}

/// Sidecar mit den Vorfassungen eines Knotens (`palace/<slug>.history.jsonl`;
/// kein `.md`, also nie im Index).
fn history_path(node_path: &Path) -> PathBuf {
    node_path.with_extension("history.jsonl")
}

/// Hängt die aktuelle Fassung an die History und zählt `extra.revision` hoch.
fn start_revision(
    node: &mut LoadedNode,
    reason: &str,
    now: jiff::Timestamp,
) -> Result<u64, OpError> {
    let current = node
        .artifact
        .frontmatter
        .extra
        .get(REVISION_KEY)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1);
    let entry = serde_json::json!({
        "revision": current,
        "replaced_at": now.to_string(),
        "reason": reason,
        "status": artifact_status(&node.artifact).label(),
        "updated_at": node.artifact.frontmatter.updated_at.to_string(),
        "links": node.artifact.frontmatter.links,
        "body": node.artifact.body,
    });
    let mut line = serde_json::to_string(&entry)
        .map_err(|error| OpError::Execution(format!("History kodieren: {error}")))?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(history_path(&node.path))
        .map_err(|error| OpError::Execution(format!("History öffnen: {error}")))?;
    file.write_all(line.as_bytes())
        .map_err(|error| OpError::Execution(format!("History schreiben: {error}")))?;
    let next = current.saturating_add(1);
    node.artifact
        .frontmatter
        .extra
        .insert(REVISION_KEY.to_owned(), serde_json::Value::from(next));
    Ok(next)
}

/// Schreibt den geänderten Knoten zurück (`updated_at` = `now`).
fn store_node(
    store: &KnowledgeStore,
    node: &mut LoadedNode,
    now: jiff::Timestamp,
) -> Result<(), OpError> {
    node.artifact.frontmatter.touch(now);
    store
        .write_artifact(&node.path, &node.artifact)
        .map_err(map_knowledge_error)
}

/// Fügt `target` den Frontmatter-Links hinzu, falls noch nicht vorhanden.
fn push_link(frontmatter: &mut Frontmatter, target: &ArtifactId) {
    if !frontmatter.links.contains(target) {
        frontmatter.links.push(target.clone());
    }
}

/// Schreib-Argumente: Positionale plus `--confirm`.
fn write_args(tail: &[String]) -> Result<(Vec<String>, bool), OpError> {
    let args = KnowledgeArgs::parse(tail, WRITE_FLAGS)?;
    Ok((args.positionals().to_vec(), args.switch("confirm")))
}

fn supersede(
    store: &KnowledgeStore,
    caller: &KnowledgeCaller,
    tail: &[String],
    now: jiff::Timestamp,
) -> Result<OpOutput, OpError> {
    require_operator(caller, "supersede")?;
    let (positionals, confirmed) = write_args(tail)?;
    let [old_raw, new_raw] = positionals.as_slice() else {
        return Err(OpError::InvalidArguments(
            "Aufruf: /palace supersede <alt> <neu> [--confirm]".to_owned(),
        ));
    };
    let replacement = load_node(store, caller, new_raw)?;
    if artifact_status(&replacement.artifact) == PalaceStatus::Superseded {
        return Err(OpError::InvalidArguments(format!(
            "Nachfolger {} ist selbst superseded",
            replacement.id
        )));
    }
    let probe = load_node(store, caller, old_raw)?;
    if probe.id == replacement.id {
        return Err(OpError::InvalidArguments(
            "ein Knoten kann sich nicht selbst ersetzen".to_owned(),
        ));
    }
    let _lock = KnowledgeLock::for_target(&probe.path).map_err(map_knowledge_error)?;
    // Unter der Sperre frisch lesen: der Stand vor der Sperre kann veraltet sein.
    let mut node = load_node(store, caller, old_raw)?;
    let revision = ensure_editable(&node, confirmed, "supersede")?;
    if revision {
        start_revision(&mut node, "supersede", now)?;
    }
    set_status(&mut node.artifact.frontmatter, PalaceStatus::Superseded);
    node.artifact.frontmatter.extra.insert(
        SUPERSEDED_BY_KEY.to_owned(),
        serde_json::Value::String(replacement.id.to_string()),
    );
    push_link(&mut node.artifact.frontmatter, &replacement.id);
    store_node(store, &mut node, now)?;
    Ok(OpOutput {
        text: format!(
            "{} → superseded, ersetzt durch {}.",
            node.id, replacement.id
        ),
        data: Some(serde_json::json!({
            "id": node.id.as_str(),
            "status": PalaceStatus::Superseded.label(),
            "superseded_by": replacement.id.as_str(),
        })),
    })
}

fn edit(
    store: &KnowledgeStore,
    caller: &KnowledgeCaller,
    tail: &[String],
    now: jiff::Timestamp,
) -> Result<OpOutput, OpError> {
    require_operator(caller, "edit")?;
    let (positionals, confirmed) = write_args(tail)?;
    let Some((raw, words)) = positionals.split_first() else {
        return Err(OpError::InvalidArguments(
            "Aufruf: /palace edit <id> <text…> [--confirm]".to_owned(),
        ));
    };
    let text = words.join(" ");
    if text.trim().is_empty() {
        return Err(OpError::InvalidArguments(
            "Aufruf: /palace edit <id> <text…> [--confirm] (Text fehlt)".to_owned(),
        ));
    }
    let probe = load_node(store, caller, raw)?;
    let _lock = KnowledgeLock::for_target(&probe.path).map_err(map_knowledge_error)?;
    let mut node = load_node(store, caller, raw)?;
    let revision = ensure_editable(&node, confirmed, "edit")?;
    let number = if revision {
        Some(start_revision(&mut node, "edit", now)?)
    } else {
        None
    };
    // Frontmatter-Links: ausdrückliche Links bleiben, Wikilinks folgen dem Body.
    let old_wikilinks: Vec<ArtifactId> = scan_wikilinks(&node.artifact.body)
        .into_iter()
        .map(ArtifactId::new)
        .collect();
    let mut links: Vec<ArtifactId> = node
        .artifact
        .frontmatter
        .links
        .iter()
        .filter(|link| !old_wikilinks.contains(link))
        .cloned()
        .collect();
    for link in scan_wikilinks(&text).into_iter().map(ArtifactId::new) {
        if !links.contains(&link) {
            links.push(link);
        }
    }
    node.artifact.frontmatter.links = links;
    node.artifact.body = format!("{}\n", text.trim());
    store_node(store, &mut node, now)?;
    let text = match number {
        Some(number) => format!("{} bearbeitet (neue Revision {number}).", node.id),
        None => format!("{} bearbeitet (provisional).", node.id),
    };
    Ok(OpOutput {
        text,
        data: Some(serde_json::json!({
            "id": node.id.as_str(),
            "status": artifact_status(&node.artifact).label(),
            "revision": number,
        })),
    })
}

fn link(
    store: &KnowledgeStore,
    caller: &KnowledgeCaller,
    tail: &[String],
    now: jiff::Timestamp,
) -> Result<OpOutput, OpError> {
    require_operator(caller, "link")?;
    let (positionals, confirmed) = write_args(tail)?;
    let [from_raw, to_raw] = positionals.as_slice() else {
        return Err(OpError::InvalidArguments(
            "Aufruf: /palace link <a> <b> [--confirm]".to_owned(),
        ));
    };
    let target = load_node(store, caller, to_raw)?;
    let probe = load_node(store, caller, from_raw)?;
    if probe.id == target.id {
        return Err(OpError::InvalidArguments(
            "ein Knoten kann nicht auf sich selbst verlinken".to_owned(),
        ));
    }
    let _lock = KnowledgeLock::for_target(&probe.path).map_err(map_knowledge_error)?;
    let mut node = load_node(store, caller, from_raw)?;
    let already = scan_wikilinks(&node.artifact.body)
        .iter()
        .any(|existing| existing == target.id.as_str());
    if already {
        return Ok(OpOutput::from(format!(
            "{} verlinkt bereits auf {}.",
            node.id, target.id
        )));
    }
    let revision = ensure_editable(&node, confirmed, "link")?;
    if revision {
        start_revision(&mut node, "link", now)?;
    }
    let mut body = node.artifact.body.trim_end().to_owned();
    body.push_str(&format!("\n\nSiehe auch: [[{}]]\n", target.id));
    node.artifact.body = body;
    push_link(&mut node.artifact.frontmatter, &target.id);
    store_node(store, &mut node, now)?;
    Ok(OpOutput {
        text: format!("{} → {} verlinkt.", node.id, target.id),
        data: Some(serde_json::json!({
            "id": node.id.as_str(),
            "link": target.id.as_str(),
        })),
    })
}

#[cfg(test)]
mod tests {
    use super::{run_palace, run_palace_as, written_node_id};
    use crate::knowledge_common::KnowledgeCaller;
    use crate::knowledge_test_support::temporary_store;
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_knowledge::memory::palace::{PalaceStatus, artifact_status, set_status};
    use harw_knowledge::memory::topic;
    use harw_knowledge::{
        AgentId, ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact, KnowledgeStore,
        VisibilityScope,
    };
    use harw_operations::operation::Surface;
    use harw_operations::{OpError, Operation};

    fn write_topic(
        store: &KnowledgeStore,
        slug: &str,
        body: &str,
        visibility: VisibilityScope,
    ) -> TestResult {
        let mut frontmatter = Frontmatter::new(
            AgentId::new("agent-1"),
            visibility,
            jiff::Timestamp::UNIX_EPOCH,
        );
        frontmatter.tags = vec!["ops".to_owned()];
        topic::write(store, slug, frontmatter, body).map_err(ctx("write topic"))
    }

    fn run(store: &KnowledgeStore, tokens: &[&str]) -> Result<String, OpError> {
        run_palace(
            store,
            &AgentId::new("operator"),
            &toks(tokens),
            jiff::Timestamp::UNIX_EPOCH,
        )
        .map(|output| output.text)
    }

    #[test]
    fn palace_is_command_only() {
        let surfaces = &super::PalaceOperation.meta().surfaces;
        assert!(
            !surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::ModelTool { .. })),
            "promote must never be reachable as a model tool"
        );
    }

    #[test]
    fn promote_then_show_and_search() -> TestResult {
        let store = temporary_store("promote")?;
        write_topic(
            &store,
            "deploy-pipeline",
            "Deploy läuft über [[palace/on-call]] und Canary.",
            VisibilityScope::OperatorOnly,
        )?;
        write_topic(
            &store,
            "on-call",
            "Rufbereitschaft für den Deploy.",
            VisibilityScope::OperatorOnly,
        )?;

        let promoted =
            run(&store, &["promote", "topic/deploy-pipeline"]).map_err(ctx("promote"))?;
        assert!(promoted.contains("palace/deploy-pipeline"));
        run(&store, &["promote", "on-call"]).map_err(ctx("promote second"))?;

        let shown = run(&store, &["show", "deploy-pipeline"]).map_err(ctx("show"))?;
        assert!(shown.contains("Status: established"), "{shown}");
        assert!(shown.contains("→ palace/on-call"), "{shown}");

        let target = run(&store, &["show", "palace/on-call"]).map_err(ctx("show target"))?;
        assert!(target.contains("← palace/deploy-pipeline"), "{target}");

        let hits = run(&store, &["search", "canary", "--max=5"]).map_err(ctx("search"))?;
        assert!(hits.contains("palace/deploy-pipeline"), "{hits}");

        // Strukturierte Treffer für den Wissensbrowser der TUI.
        let searched = run_palace(
            &store,
            &AgentId::new("operator"),
            &toks(&["search", "canary", "--max=5"]),
            jiff::Timestamp::UNIX_EPOCH,
        )
        .map_err(ctx("search data"))?;
        let data = searched.data.ok_or(TestError::Missing("search data"))?;
        assert_eq!(data["query"], "canary");
        assert_eq!(data["truncated"], false);
        assert_eq!(data["hits"][0]["id"], "palace/deploy-pipeline");
        assert_eq!(data["hits"][0]["status"], "established");
        assert!(data["hits"][0]["title"].is_string());
        assert!(data["hits"][0]["score"].is_number());
        assert!(data["hits"][0]["hop_path"].is_array());
        let empty = run_palace(
            &store,
            &AgentId::new("operator"),
            &toks(&["search", "gibtesnicht"]),
            jiff::Timestamp::UNIX_EPOCH,
        )
        .map_err(ctx("empty search"))?;
        let data = empty.data.ok_or(TestError::Missing("empty search data"))?;
        assert_eq!(data["hits"].as_array().map(Vec::len), Some(0));

        let listed = run_palace(
            &store,
            &AgentId::new("operator"),
            &toks(&["list"]),
            jiff::Timestamp::UNIX_EPOCH,
        )
        .map_err(ctx("list"))?;
        let data = listed.data.ok_or(TestError::Missing("list data"))?;
        assert_eq!(data["nodes"].as_array().map(Vec::len), Some(2));
        assert_eq!(data["nodes"][0]["id"], "palace/deploy-pipeline");
        assert_eq!(data["nodes"][0]["status"], "established");

        let shown = run_palace(
            &store,
            &AgentId::new("operator"),
            &toks(&["show", "on-call"]),
            jiff::Timestamp::UNIX_EPOCH,
        )
        .map_err(ctx("show data"))?;
        let node = shown.data.ok_or(TestError::Missing("show data"))?;
        assert_eq!(node["node"]["backlinks"][0], "palace/deploy-pipeline");
        assert_eq!(node["node"]["body"], "Rufbereitschaft für den Deploy.");
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn promote_never_overwrites_and_refuses_invisible_topics() -> TestResult {
        let store = temporary_store("guards")?;
        write_topic(&store, "a", "body", VisibilityScope::OperatorOnly)?;
        write_topic(&store, "secret", "body", VisibilityScope::SelfOnly)?;
        run(&store, &["promote", "a"]).map_err(ctx("first promote"))?;
        for tokens in [
            vec!["promote", "a"],
            vec!["promote", "secret"],
            vec!["promote", "missing"],
            vec!["promote", "../x"],
            vec!["show", "nope"],
            vec!["search"],
            vec!["search", "x", "--max-hops=99"],
            vec!["frobnicate"],
            vec!["show"],
        ] {
            match run(&store, &tokens) {
                Err(OpError::InvalidArguments(_)) => {}
                other => {
                    return Err(TestError::Unexpected(format!(
                        "{tokens:?} must be InvalidArguments, got {other:?}"
                    )));
                }
            }
        }
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    fn write_node(
        store: &KnowledgeStore,
        slug: &str,
        status: PalaceStatus,
        body: &str,
    ) -> TestResult {
        let mut frontmatter = Frontmatter::new(
            AgentId::new("operator"),
            VisibilityScope::OperatorOnly,
            jiff::Timestamp::UNIX_EPOCH,
        );
        set_status(&mut frontmatter, status);
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

    fn read_node(store: &KnowledgeStore, slug: &str) -> TestResult<KnowledgeArtifact> {
        store
            .read_artifact(
                &store.palace_path(&ArtifactId::new(slug)),
                ArtifactId::new(format!("palace/{slug}")),
                ArtifactKind::PalaceNode,
            )
            .map_err(ctx("read node"))
    }

    fn expect_invalid(result: Result<String, OpError>, what: &str) -> TestResult {
        match result {
            Err(OpError::InvalidArguments(_)) => Ok(()),
            other => Err(TestError::Unexpected(format!(
                "{what} must be InvalidArguments, got {other:?}"
            ))),
        }
    }

    #[test]
    fn edit_is_free_for_provisional_and_needs_confirm_for_established() -> TestResult {
        let store = temporary_store("edit")?;
        write_node(&store, "draft", PalaceStatus::Provisional, "alt\n")?;
        write_node(
            &store,
            "truth",
            PalaceStatus::Established,
            "alt [[palace/x]]\n",
        )?;

        run(&store, &["edit", "draft", "neu", "mit", "[[palace/truth]]"])
            .map_err(ctx("edit provisional"))?;
        let draft = read_node(&store, "draft")?;
        assert_eq!(draft.body.trim(), "neu mit [[palace/truth]]");
        assert_eq!(
            draft.frontmatter.links,
            vec![ArtifactId::new("palace/truth")]
        );
        assert_eq!(artifact_status(&draft), PalaceStatus::Provisional);
        assert!(!store.root().join("palace/draft.history.jsonl").exists());

        expect_invalid(
            run(&store, &["edit", "truth", "anders"]),
            "unconfirmed edit",
        )?;
        assert_eq!(read_node(&store, "truth")?.body.trim(), "alt [[palace/x]]");

        let out = run(&store, &["edit", "truth", "anders", "--confirm"])
            .map_err(ctx("confirmed edit"))?;
        assert!(out.contains("Revision 2"), "{out}");
        let truth = read_node(&store, "truth")?;
        assert_eq!(truth.body.trim(), "anders");
        assert!(truth.frontmatter.links.is_empty(), "stale wikilink dropped");
        assert_eq!(truth.frontmatter.extra["revision"], 2);
        assert_eq!(artifact_status(&truth), PalaceStatus::Established);
        let history = std::fs::read_to_string(store.root().join("palace/truth.history.jsonl"))
            .map_err(ctx("read history"))?;
        assert_eq!(history.lines().count(), 1);
        assert!(history.contains("alt [[palace/x]]"), "{history}");

        // Die History-Datei ist kein Palace-Knoten.
        let listed = run(&store, &["list"]).map_err(ctx("list"))?;
        assert!(listed.starts_with("2 Palace-Knoten"), "{listed}");
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn supersede_retires_the_old_node_and_points_to_the_new_one() -> TestResult {
        let store = temporary_store("supersede")?;
        write_node(&store, "old", PalaceStatus::Established, "alt\n")?;
        write_node(&store, "new", PalaceStatus::Provisional, "neu\n")?;
        write_node(&store, "draft", PalaceStatus::Provisional, "entwurf\n")?;

        expect_invalid(run(&store, &["supersede", "old", "new"]), "unconfirmed")?;
        expect_invalid(
            run(&store, &["supersede", "old", "old", "--confirm"]),
            "self",
        )?;
        expect_invalid(
            run(&store, &["supersede", "old", "missing", "--confirm"]),
            "missing",
        )?;
        expect_invalid(run(&store, &["supersede", "old"]), "arity")?;

        run(&store, &["supersede", "palace/old", "new", "--confirm"]).map_err(ctx("supersede"))?;
        let old = read_node(&store, "old")?;
        assert_eq!(artifact_status(&old), PalaceStatus::Superseded);
        assert_eq!(old.frontmatter.extra["superseded_by"], "palace/new");
        assert!(
            old.frontmatter
                .links
                .contains(&ArtifactId::new("palace/new"))
        );
        assert_eq!(old.body.trim(), "alt", "history stays readable");

        // Eingefroren: weder edit noch link noch ein zweites supersede.
        expect_invalid(
            run(&store, &["edit", "old", "x", "--confirm"]),
            "edit superseded",
        )?;
        expect_invalid(
            run(&store, &["link", "old", "new", "--confirm"]),
            "link superseded",
        )?;
        // Ein superseded Knoten taugt nicht als Nachfolger.
        expect_invalid(
            run(&store, &["supersede", "draft", "old"]),
            "superseded successor",
        )?;
        // Provisional ohne --confirm.
        run(&store, &["supersede", "draft", "new"]).map_err(ctx("supersede provisional"))?;

        let shown = run(&store, &["show", "new"]).map_err(ctx("show new"))?;
        assert!(shown.contains("← palace/old"), "{shown}");
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn link_appends_a_wikilink_once() -> TestResult {
        let store = temporary_store("link")?;
        write_node(&store, "a", PalaceStatus::Provisional, "A\n")?;
        write_node(&store, "b", PalaceStatus::Established, "B\n")?;

        run(&store, &["link", "a", "b"]).map_err(ctx("link"))?;
        let a = read_node(&store, "a")?;
        assert!(a.body.contains("[[palace/b]]"), "{}", a.body);
        assert_eq!(a.frontmatter.links, vec![ArtifactId::new("palace/b")]);
        let again = run(&store, &["link", "a", "palace/b"]).map_err(ctx("link again"))?;
        assert!(again.contains("bereits"), "{again}");
        assert_eq!(
            read_node(&store, "a")?.body.matches("[[palace/b]]").count(),
            1
        );

        let shown = run(&store, &["show", "b"]).map_err(ctx("show b"))?;
        assert!(shown.contains("← palace/a"), "{shown}");

        expect_invalid(
            run(&store, &["link", "b", "a"]),
            "established link unconfirmed",
        )?;
        run(&store, &["link", "b", "a", "--confirm"]).map_err(ctx("confirmed link"))?;
        expect_invalid(run(&store, &["link", "a", "a"]), "self link")?;
        expect_invalid(run(&store, &["link", "a", "nope"]), "missing target")?;
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn write_subcommands_are_operator_only() -> TestResult {
        let store = temporary_store("gate")?;
        write_node(&store, "a", PalaceStatus::Provisional, "A\n")?;
        write_node(&store, "b", PalaceStatus::Provisional, "B\n")?;
        let agent = KnowledgeCaller {
            agent: AgentId::new("root-explorer"),
            viewer: VisibilityScope::SelfOnly,
        };
        for tokens in [
            vec!["edit", "a", "x"],
            vec!["link", "a", "b"],
            vec!["supersede", "a", "b"],
        ] {
            match run_palace_as(&store, &agent, &toks(&tokens), jiff::Timestamp::UNIX_EPOCH) {
                Err(OpError::NotAvailable(_)) => {}
                other => {
                    return Err(TestError::Unexpected(format!(
                        "{tokens:?} must be NotAvailable for an agent, got {other:?}"
                    )));
                }
            }
        }
        assert_eq!(read_node(&store, "a")?.body.trim(), "A");
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn written_node_id_names_the_changed_node() {
        assert_eq!(
            written_node_id(&toks(&["edit", "--confirm", "palace/x", "t"])),
            Some(Some("palace/x".to_owned()))
        );
        assert_eq!(
            written_node_id(&toks(&["promote", "topic/y"])),
            Some(Some("palace/y".to_owned()))
        );
        assert_eq!(
            written_node_id(&toks(&["supersede", "a", "b"])),
            Some(Some("palace/a".to_owned()))
        );
        assert_eq!(written_node_id(&toks(&["show", "x"])), None);
        assert_eq!(written_node_id(&toks(&[])), None);
    }
}
