//! `/palace` — der verlinkte Langzeitgedächtnis-Graph
//! (`docs/design/knowledge-surfaces.md` §2.2/§2.5, `interaction-contract.md` §2.2).
//!
//! # Subcommands
//! - `show <ArtifactRef>` — Knoten mit Titel, Status, Tags, Body, ausgehenden
//!   Links (`[[wikilinks]]` + Frontmatter-`links`) und berechneten Backlinks.
//! - `search <query> [--max-hops=n] [--max=n]` — graphbewusster Recall über
//!   Palace-Knoten; begrenzt durch `RecallQuery::validate` (§2.3).
//! - `promote <topic-ref>` — Thema → Palace-Knoten (§2.5).
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
//! mit `VisibilityScope::OperatorOnly` (dieselbe Disziplin wie
//! `/context-proposal`); `KnowledgeIndex::get` wird nur mit Ids aufgerufen,
//! die der Recall bereits als sichtbar bestätigt hat. Backlinks werden auf
//! sichtbare Quellen gefiltert. Ein Thema wird nur promotet, wenn es für
//! einen `OperatorOnly`-Aufrufer sichtbar ist; der Knoten erbt seine
//! Sichtbarkeit (nie eine Ausweitung).
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
use harw_knowledge::index::KnowledgeIndex;
use harw_knowledge::memory::palace::{PalaceStatus, ensure_promotion_reviewed, scan_wikilinks};
use harw_knowledge::memory::recall::{ListAllRanker, search, search_with};
use harw_knowledge::memory::topic;
use harw_knowledge::{AgentId, KnowledgeStore, VisibilityScope};
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};

use crate::workbench::{caller_agent, knowledge_store, map_knowledge_error, split_flag};

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
    summary = "Palace-Graph: show <id>, search <query> [--max-hops=n] [--max=n], promote <topic>.",
    domain = "knowledge",
    permission = "operator",
    command(path = "/palace", visibility = "channel_parity")
)]
async fn palace(ctx: &OpContext, args: PalaceArgs) -> Result<OpOutput, OpError> {
    let store = knowledge_store(ctx)?;
    run_palace(
        &store,
        &caller_agent(ctx),
        &args.tokens,
        jiff::Timestamp::now(),
    )
}

/// Der reine Kern von `/palace` (testbar ohne `OpContext`).
///
/// # Fehler
/// Siehe Moduldoku.
pub fn run_palace(
    store: &KnowledgeStore,
    caller: &AgentId,
    tokens: &[String],
    now: jiff::Timestamp,
) -> Result<OpOutput, OpError> {
    let tail = tokens.get(1..).unwrap_or_default();
    match tokens.first().map(String::as_str) {
        Some("show") => show(store, tail),
        Some("search") => search_nodes(store, tail),
        Some("promote") => promote(store, caller, tail, now),
        Some(other) => Err(OpError::InvalidArguments(format!(
            "unbekannter /palace-Subcommand: {other} (show, search, promote)"
        ))),
        None => Err(OpError::InvalidArguments(
            "Aufruf: /palace show <id> | search <query> | promote <topic>".to_owned(),
        )),
    }
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

fn rebuild(store: &KnowledgeStore) -> Result<KnowledgeIndex, OpError> {
    KnowledgeIndex::rebuild(store).map_err(|error| {
        OpError::Execution(format!("Knowledge-Index-Aufbau fehlgeschlagen: {error}"))
    })
}

/// Alle für einen Operator sichtbaren Palace-Knoten (einziger Lesepfad).
fn visible_nodes(index: &KnowledgeIndex) -> Result<Vec<KnowledgeArtifact>, OpError> {
    let mut query = RecallQuery::new(String::new(), VisibilityScope::OperatorOnly);
    query.kinds = vec![ArtifactKind::PalaceNode];
    query.max_hops = 0;
    let result = search_with(index, &query, &ListAllRanker).map_err(map_knowledge_error)?;
    Ok(result
        .hits
        .iter()
        .filter_map(|hit| index.get(&hit.artifact.id).cloned())
        .collect())
}

fn show(store: &KnowledgeStore, tail: &[String]) -> Result<OpOutput, OpError> {
    let raw = tail
        .first()
        .ok_or_else(|| OpError::InvalidArguments("Aufruf: /palace show <id>".to_owned()))?;
    let target = node_id(raw);
    let index = rebuild(store)?;
    let node = visible_nodes(&index)?
        .into_iter()
        .find(|artifact| artifact.id == target)
        .ok_or_else(|| {
            OpError::InvalidArguments(format!("kein sichtbarer Palace-Knoten '{target}'"))
        })?;

    let extra = &node.frontmatter.extra;
    let title = extra
        .get(TITLE_KEY)
        .and_then(serde_json::Value::as_str)
        .unwrap_or(node.id.as_str());
    let status = extra
        .get(CONFIDENCE_KEY)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("provisional");
    let mut outgoing: Vec<String> = node
        .frontmatter
        .links
        .iter()
        .map(ToString::to_string)
        .chain(scan_wikilinks(&node.body))
        .collect();
    outgoing.sort();
    outgoing.dedup();
    let backlinks: Vec<String> = index
        .backlinks(&node.id)
        .into_iter()
        .filter(|source| {
            source
                .visibility
                .visible_to_caller(&VisibilityScope::OperatorOnly)
        })
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
    Ok(OpOutput::from(out))
}

fn search_nodes(store: &KnowledgeStore, tail: &[String]) -> Result<OpOutput, OpError> {
    let (hops_flag, rest) = split_flag(tail, "--max-hops=");
    let (max_flag, rest) = split_flag(&rest, "--max=");
    let text = rest.join(" ");
    if text.trim().is_empty() {
        return Err(OpError::InvalidArguments(
            "Aufruf: /palace search <query> [--max-hops=n] [--max=n]".to_owned(),
        ));
    }
    let max_hops = match hops_flag {
        Some(raw) => parse_bound::<u8>(&raw, "--max-hops", MAX_RECALL_HOPS)?,
        None => DEFAULT_SEARCH_HOPS,
    };
    let max_artifacts = match max_flag {
        Some(raw) => parse_bound::<usize>(&raw, "--max", MAX_RECALL_ARTIFACTS)?,
        None => DEFAULT_SEARCH_MAX,
    };

    let index = rebuild(store)?;
    let mut query = RecallQuery::new(text.trim(), VisibilityScope::OperatorOnly);
    query.kinds = vec![ArtifactKind::PalaceNode];
    query.max_hops = max_hops;
    query.max_artifacts = max_artifacts;
    let result = search(&index, &query).map_err(map_knowledge_error)?;
    if result.hits.is_empty() {
        return Ok(OpOutput::from(format!(
            "Keine Palace-Treffer für '{}'.",
            text.trim()
        )));
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
    Ok(OpOutput::from(out))
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
    caller: &AgentId,
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
    if !topic_artifact
        .frontmatter
        .visibility
        .visible_to_caller(&VisibilityScope::OperatorOnly)
    {
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
        caller.clone(),
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

#[cfg(test)]
mod tests {
    use super::run_palace;
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_knowledge::memory::topic;
    use harw_knowledge::{AgentId, Frontmatter, KnowledgeStore, VisibilityScope};
    use harw_operations::operation::Surface;
    use harw_operations::{OpError, Operation};

    fn temporary_store(label: &str) -> TestResult<KnowledgeStore> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(ctx("system clock is after epoch"))?
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "harw-ops-palace-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).map_err(ctx("create temporary knowledge root"))?;
        Ok(KnowledgeStore::new(&root))
    }

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

        let promoted = run(&store, &["promote", "topic/deploy-pipeline"]).map_err(ctx("promote"))?;
        assert!(promoted.contains("palace/deploy-pipeline"));
        run(&store, &["promote", "on-call"]).map_err(ctx("promote second"))?;

        let shown = run(&store, &["show", "deploy-pipeline"]).map_err(ctx("show"))?;
        assert!(shown.contains("Status: established"), "{shown}");
        assert!(shown.contains("→ palace/on-call"), "{shown}");

        let target = run(&store, &["show", "palace/on-call"]).map_err(ctx("show target"))?;
        assert!(target.contains("← palace/deploy-pipeline"), "{target}");

        let hits = run(&store, &["search", "canary", "--max=5"]).map_err(ctx("search"))?;
        assert!(hits.contains("palace/deploy-pipeline"), "{hits}");
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
}
