//! `MemoryToolProvider` — die Fakt-Werkzeuge `memory.recall` und
//! `memory.record` (Global-Memory Punkt 4): der Agent liest und schreibt
//! Fakten mitten in der Sitzung, scope-bewusst (`project` | `global`).
//!
//! # Verantwortung
//! Beide Werkzeuge arbeiten direkt auf [`harw_memory::FactStore`] — derselbe
//! Speicher wie `/memory record|recall` in `harw-ops`, aber ohne Umweg über
//! die Operations-Schicht. Die Wurzeln werden bei der Montage gebunden und
//! nie aus Argumenten gelesen:
//! - `project`: `<projekt>/.harw/memories/`,
//! - `global`: `<root-space>/profiles/<profil>/memories/` oder `None`, wenn an
//!   die Sitzung kein Root-Space gebunden ist (dann schlägt jeder globale
//!   Zugriff geschlossen fehl, kein Rückfall auf `~/.harw`).
//!
//! - `memory.recall {query, scope?, limit?}` — Stichwortsuche
//!   (`FactStore::search`), Projekt zuerst; je Treffer Scope, Name,
//!   Beschreibung, Typ, Vertrauen und gekürzter Body. Nur lesend.
//! - `memory.record {text, scope?, title?, type?}` — legt einen Fakt an (oder
//!   aktualisiert den inhaltsgleichen). Vorgabe-Scope `project`.
//!
//! # Rechteklasse
//! - `memory.recall`: [`Permission::ReadWorkspace`], parallelsicher.
//! - `memory.record`: [`Permission::WriteWorkspace`] — schreibend, damit für
//!   read-only Rollen nicht gewährbar. Das Werkzeug steht bewusst **nicht** in
//!   `AUTO_APPROVED_TOOLS`: die Standardpolitik fragt (fail-closed) bei jedem
//!   Aufruf, insbesondere für `scope = "global"`, das projektübergreifend
//!   wirkt. Auch `memory.recall` wird nicht auto-freigegeben (wie Kanban).
//!
//! # Redaction
//! `FactStore::write` schwärzt Body, Beschreibung und Tags. Weil Name und
//! Beschreibung hier aus dem Eingabetext abgeleitet werden, läuft
//! [`harw_memory::redact`] **vor** der Ableitung — ein Geheimnis landet so
//! weder im Dateinamen noch im Index. Ausgaben von `memory.recall` werden
//! defensiv ein zweites Mal geschwärzt (handbearbeitete Dateien).
//!
//! # Deckel
//! `limit` höchstens [`MAX_LIMIT`], Body je Treffer [`BODY_CHARS`] Zeichen,
//! Gesamttext [`MAX_OUTPUT_CHARS`]; Eingabetext höchstens [`MAX_TEXT_CHARS`].
//!
//! # Fehler
//! Kein Aufruf gibt `Err` zurück: ungültige Argumente und Speicherfehler
//! werden als [`ToolOutput::error`] gemeldet.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use harw_extension_api::{
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolName, ToolOutput,
    ToolSpec,
};
use harw_memory::{Fact, FactScope, FactStore, FactType, redact, slugify};
use harw_tools::args::parse_args;
use harw_tools::schema_helpers::{property, strict_object_schema};
use harw_tools::{FunctionToolSpec, JsonSchema, JsonSchemaType, Permission};
use serde::Deserialize;

/// Name des Lese-Werkzeugs.
pub const MEMORY_RECALL: &str = "memory.recall";

/// Name des Schreib-Werkzeugs.
pub const MEMORY_RECORD: &str = "memory.record";

/// Vorgabe für `limit`.
const DEFAULT_LIMIT: usize = 5;

/// Obergrenze für `limit`.
pub const MAX_LIMIT: usize = 20;

/// Länge eines Treffer-Bodys (Unicode-Zeichen).
pub const BODY_CHARS: usize = 1_500;

/// Gesamtdeckel der gelieferten Body-Texte je Aufruf (Unicode-Zeichen).
pub const MAX_OUTPUT_CHARS: usize = 12_000;

/// Obergrenze der Suchanfrage (Unicode-Zeichen).
const MAX_QUERY_CHARS: usize = 500;

/// Höchstzahl ausgewerteter Suchwörter.
const MAX_KEYWORDS: usize = 16;

/// Obergrenze des aufzuzeichnenden Textes (Unicode-Zeichen).
pub const MAX_TEXT_CHARS: usize = 4_000;

/// Länge der Beschreibung (Index-Zeile) in Unicode-Zeichen.
const DESCRIPTION_MAX_CHARS: usize = 120;

/// Versuche, einen freien Fakt-Namen zu finden.
const MAX_SLUG_ATTEMPTS: u32 = 50;

/// Die Fakt-Werkzeuge über den bei der Montage gebundenen Wurzeln.
#[derive(Debug, Clone)]
pub struct MemoryToolProvider {
    roots: Arc<MemoryRoots>,
}

/// Die gebundenen Speicherwurzeln (`.../memories`).
#[derive(Debug, Clone)]
struct MemoryRoots {
    project: PathBuf,
    global: Option<PathBuf>,
}

impl MemoryToolProvider {
    /// Baut den Provider.
    ///
    /// # Argumente
    /// - `project` (`PathBuf`): `<projekt>/.harw/memories`.
    /// - `global` (`Option<PathBuf>`): `<root-space>/profiles/<profil>/memories`
    ///   oder `None`, wenn kein Root-Space gebunden ist.
    #[must_use]
    pub fn new(project: PathBuf, global: Option<PathBuf>) -> Self {
        Self {
            roots: Arc::new(MemoryRoots { project, global }),
        }
    }
}

// `memory.recall` liest nur (parallelsicher); `memory.record` schreibt.
harw_tools::tool_provider! {
    impl for MemoryToolProvider as provider, parallel_safe: [MEMORY_RECALL] {
        MEMORY_RECALL => {
            spec: recall_spec(),
            permission: Permission::ReadWorkspace,
            executor: MemoryExecutor { roots: Arc::clone(&provider.roots), tool: MemoryTool::Recall },
        },
        MEMORY_RECORD => {
            spec: record_spec(),
            permission: Permission::WriteWorkspace,
            executor: MemoryExecutor { roots: Arc::clone(&provider.roots), tool: MemoryTool::Record },
        },
    }
}

fn enum_property(description: &str, values: &[&str]) -> JsonSchema {
    let mut schema = property(JsonSchemaType::String, description);
    schema.enum_values = Some(values.iter().map(|value| serde_json::json!(value)).collect());
    schema
}

fn recall_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    props.insert(
        "query".to_owned(),
        property(JsonSchemaType::String, "Stichwörter für die Suche."),
    );
    props.insert(
        "scope".to_owned(),
        enum_property(
            "Wo gesucht wird: project, global oder all (Vorgabe all, Projekt zuerst).",
            &["project", "global", "all"],
        ),
    );
    props.insert(
        "limit".to_owned(),
        property(
            JsonSchemaType::Integer,
            "Höchstzahl Treffer (1–20, Vorgabe 5).",
        ),
    );
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(MEMORY_RECALL),
        description: format!(
            "Ruft gespeicherte Fakten (Präferenzen, Entscheidungen, Fallen, Verweise) per \
             Stichwortsuche aus dem Projekt- und/oder dem globalen Gedächtnis ab. Liefert je \
             Treffer Scope, Name, Beschreibung, Typ und Inhalt (auf {BODY_CHARS} Zeichen \
             gekürzt). Höchstens {MAX_LIMIT} Treffer. Nur lesend."
        ),
        parameters: strict_object_schema(props, &["query"]),
        strict: true,
    })
}

fn record_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    props.insert(
        "text".to_owned(),
        property(
            JsonSchemaType::String,
            "Der festzuhaltende Fakt als Freitext. Keine Geheimnisse; sie werden geschwärzt.",
        ),
    );
    props.insert(
        "scope".to_owned(),
        enum_property(
            "project (Vorgabe, bleibt im Repo) oder global (projektübergreifend, braucht \
             Freigabe).",
            &["project", "global"],
        ),
    );
    props.insert(
        "title".to_owned(),
        property(
            JsonSchemaType::String,
            "Kurztitel; bestimmt Namen und Beschreibung. Ohne Angabe aus dem Text abgeleitet.",
        ),
    );
    props.insert(
        "type".to_owned(),
        enum_property(
            "Art des Fakts (Vorgabe fact).",
            &["fact", "decision", "preference", "pitfall", "reference"],
        ),
    );
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(MEMORY_RECORD),
        description: format!(
            "Hält einen Fakt im Gedächtnis fest (Projekt oder global); derselbe Text \
             aktualisiert den vorhandenen Fakt. Text höchstens {MAX_TEXT_CHARS} Zeichen; \
             Geheimnisse werden vor dem Speichern geschwärzt. Schreibend, erfordert Freigabe."
        ),
        parameters: strict_object_schema(props, &["text"]),
        strict: true,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemoryTool {
    Recall,
    Record,
}

struct MemoryExecutor {
    roots: Arc<MemoryRoots>,
    tool: MemoryTool,
}

impl ToolExecutor for MemoryExecutor {
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        let arguments = call.arguments.clone();
        let session = context.session_id().to_string();
        Box::pin(async move {
            Ok(match self.tool {
                MemoryTool::Recall => execute_recall(&self.roots, arguments),
                MemoryTool::Record => execute_record(&self.roots, arguments, &session),
            })
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecallArgs {
    query: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordArgs {
    text: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default, rename = "type")]
    fact_type: Option<String>,
}

/// Kürzt `text` auf höchstens `max` Unicode-Zeichen (mit `…`).
fn clamp_chars(text: &str, max: usize) -> (String, bool) {
    let trimmed = text.trim();
    match trimmed.char_indices().nth(max) {
        None => (trimmed.to_owned(), false),
        Some((cut, _)) => (format!("{}…", &trimmed[..cut]), true),
    }
}

/// `Ok(None)` steht für `all`/keine Angabe.
fn parse_scope(value: Option<&str>) -> Result<Option<FactScope>, String> {
    match value {
        None | Some("all") => Ok(None),
        Some("project") => Ok(Some(FactScope::Project)),
        Some("global") => Ok(Some(FactScope::Global)),
        Some(other) => Err(format!("unbekannter scope '{other}' (project|global|all)")),
    }
}

impl MemoryRoots {
    fn open(&self, scope: FactScope) -> Result<FactStore, String> {
        let root = match scope {
            FactScope::Project => Some(&self.project),
            FactScope::Global => self.global.as_ref(),
        }
        .ok_or_else(|| {
            "kein globaler Speicher: an die Sitzung ist kein Root-Space gebunden".to_owned()
        })?;
        FactStore::open(root, scope)
            .map_err(|error| format!("Fakt-Speicher ({scope}) öffnen fehlgeschlagen: {error}"))
    }
}

/// Kern von `memory.recall` (testbar ohne Sandbox-Kontext).
fn execute_recall(roots: &MemoryRoots, arguments: serde_json::Value) -> ToolOutput {
    let args: RecallArgs = match parse_args(MEMORY_RECALL, &arguments) {
        Ok(args) => args,
        Err(out) => return out,
    };
    let fail = |detail: String| ToolOutput::error(format!("{MEMORY_RECALL}: {detail}"));
    let query = args.query.trim();
    if query.is_empty() {
        return fail("query ist leer".to_owned());
    }
    if query.chars().count() > MAX_QUERY_CHARS {
        return fail(format!("query länger als {MAX_QUERY_CHARS} Zeichen"));
    }
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 || limit > MAX_LIMIT {
        return fail(format!("limit muss zwischen 1 und {MAX_LIMIT} liegen"));
    }
    let requested = match parse_scope(args.scope.as_deref()) {
        Ok(scope) => scope,
        Err(error) => return fail(error),
    };
    let scopes = match requested {
        Some(one) => vec![one],
        None => vec![FactScope::Project, FactScope::Global],
    };
    let keywords: Vec<&str> = query.split_whitespace().take(MAX_KEYWORDS).collect();

    let mut found: Vec<(FactScope, Fact)> = Vec::new();
    let mut global_unavailable = false;
    for scope in scopes {
        // Ohne Bindung wird Global bei `all` übersprungen, explizit nicht.
        if scope == FactScope::Global && requested.is_none() && roots.global.is_none() {
            global_unavailable = true;
            continue;
        }
        let store = match roots.open(scope) {
            Ok(store) => store,
            Err(error) => return fail(error),
        };
        match store.search(&keywords, limit) {
            Ok(facts) => found.extend(facts.into_iter().map(|fact| (scope, fact))),
            Err(error) => return fail(format!("Suche ({scope}) fehlgeschlagen: {error}")),
        }
    }

    let mut truncated = found.len() > limit;
    found.truncate(limit);
    let mut budget = MAX_OUTPUT_CHARS;
    let mut hits = Vec::new();
    for (scope, fact) in &found {
        if budget == 0 {
            truncated = true;
            break;
        }
        let (body, body_truncated) = clamp_chars(&redact(&fact.body), BODY_CHARS.min(budget));
        budget = budget.saturating_sub(body.chars().count());
        truncated |= body_truncated;
        hits.push(serde_json::json!({
            "scope": scope.as_str(),
            "name": fact.name,
            "description": redact(&fact.description),
            "type": fact.fact_type.as_str(),
            "confidence": fact.confidence,
            "tags": fact.tags.iter().map(|tag| redact(tag)).collect::<Vec<_>>(),
            "body": body,
            "body_truncated": body_truncated,
        }));
    }
    ToolOutput::json(serde_json::json!({
        "hits": hits,
        "truncated": truncated,
        "global_unavailable": global_unavailable,
    }))
}

/// Findet einen freien oder inhaltlich identischen Fakt-Namen ab `base`.
fn unique_slug(store: &FactStore, base: &str, body: &str) -> Result<String, String> {
    let mut candidate = base.to_owned();
    for suffix in 2..=MAX_SLUG_ATTEMPTS {
        match store
            .read(&candidate)
            .map_err(|error| format!("Fakt lesen fehlgeschlagen: {error}"))?
        {
            None => return Ok(candidate),
            // `FactStore::write` hängt einen abschließenden Zeilenumbruch an;
            // der Vergleich löst ihn, damit derselbe Text idempotent bleibt.
            Some(existing)
                if existing.body.trim_end_matches('\n') == body.trim_end_matches('\n') =>
            {
                return Ok(candidate);
            }
            Some(_) => candidate = format!("{base}-{suffix}"),
        }
    }
    Err(format!(
        "kein freier Fakt-Name für '{base}' gefunden ({MAX_SLUG_ATTEMPTS} Kollisionen)"
    ))
}

/// Kern von `memory.record` (testbar ohne Sandbox-Kontext). Die Freigabe für
/// den Aufruf holt die Standardpolitik vorher (nicht auto-freigegeben).
fn execute_record(roots: &MemoryRoots, arguments: serde_json::Value, session: &str) -> ToolOutput {
    let args: RecordArgs = match parse_args(MEMORY_RECORD, &arguments) {
        Ok(args) => args,
        Err(out) => return out,
    };
    let fail = |detail: String| ToolOutput::error(format!("{MEMORY_RECORD}: {detail}"));
    if args.text.chars().count() > MAX_TEXT_CHARS {
        return fail(format!("text länger als {MAX_TEXT_CHARS} Zeichen"));
    }
    // Redaction vor jeder Ableitung (Name, Beschreibung, Vergleich).
    let text = redact(args.text.trim());
    if text.trim().is_empty() {
        return fail("text ist leer".to_owned());
    }
    let scope = match parse_scope(args.scope.as_deref()) {
        Ok(Some(scope)) => scope,
        Ok(None) if args.scope.is_some() => {
            return fail("scope muss project oder global sein".to_owned());
        }
        Ok(None) => FactScope::Project,
        Err(error) => return fail(error),
    };
    let fact_type = match args.fact_type.as_deref() {
        None => FactType::Fact,
        Some(label) => match label.parse::<FactType>() {
            Ok(fact_type) => fact_type,
            Err(_) => return fail(format!("unbekannter type '{label}'")),
        },
    };
    let store = match roots.open(scope) {
        Ok(store) => store,
        Err(error) => return fail(error),
    };
    let title = args
        .title
        .as_deref()
        .map(|title| redact(title.trim()))
        .filter(|title| !title.is_empty());
    let label = title.as_deref().unwrap_or(&text);
    let name = match unique_slug(&store, &slugify(label), &text) {
        Ok(name) => name,
        Err(error) => return fail(error),
    };
    let now = time::OffsetDateTime::now_utc();
    let fact = Fact {
        name: name.clone(),
        description: clamp_chars(label, DESCRIPTION_MAX_CHARS).0,
        fact_type,
        scope,
        created: now,
        updated: now,
        confidence: 1.0,
        sources: vec![format!("session:{session}")],
        tags: Vec::new(),
        body: text,
    };
    if let Err(error) = store.write(&fact) {
        return fail(format!("Fakt schreiben fehlgeschlagen: {error}"));
    }
    ToolOutput::json(serde_json::json!({
        "recorded": true,
        "name": name,
        "scope": scope.as_str(),
        "type": fact_type.as_str(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::golden::{assert_golden, surface};
    use crate::test_support::{TestError, TestResult, ctx};
    use serde_json::json;

    fn roots(global: bool) -> TestResult<(tempfile::TempDir, MemoryRoots)> {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let roots = MemoryRoots {
            project: dir.path().join("project"),
            global: global.then(|| dir.path().join("global")),
        };
        Ok((dir, roots))
    }

    fn json_of(output: ToolOutput) -> TestResult<serde_json::Value> {
        match output {
            ToolOutput::Json { content } => Ok(content),
            other => Err(TestError::Unexpected(format!("kein JSON: {other:?}"))),
        }
    }

    fn error_of(output: ToolOutput) -> TestResult<String> {
        match output {
            ToolOutput::Error { message } => Ok(message),
            other => Err(TestError::Unexpected(format!("kein Fehler: {other:?}"))),
        }
    }

    #[test]
    fn test_memory_provider_surface_matches_golden() -> TestResult {
        let provider = MemoryToolProvider::new(PathBuf::from("/x/p"), Some(PathBuf::from("/x/g")));
        assert_golden(
            "memory.json",
            &surface(&provider, Some(MemoryToolProvider::TOOL_PERMISSIONS))?,
        )
    }

    #[test]
    fn test_permissions_and_approval_class() {
        use crate::{AUTO_APPROVED_TOOLS, DefaultApprovalPolicy, tool_permission};
        assert_eq!(
            MemoryToolProvider::TOOL_PERMISSIONS,
            &[
                Some(Permission::ReadWorkspace),
                Some(Permission::WriteWorkspace)
            ]
        );
        assert_eq!(
            tool_permission(MEMORY_RECORD),
            Some(harw_authority::Permission::WriteWorkspace)
        );
        assert_eq!(
            tool_permission(MEMORY_RECALL),
            Some(harw_authority::Permission::ReadWorkspace)
        );
        for tool in MemoryToolProvider::TOOL_NAMES {
            assert!(!AUTO_APPROVED_TOOLS.contains(tool), "{tool}");
            let call = ToolCall {
                id: Default::default(),
                name: ToolName::new(*tool),
                arguments: json!({"scope": "global"}),
            };
            assert!(DefaultApprovalPolicy::requires_explicit_approval(&call));
        }
    }

    #[test]
    fn test_global_record_without_bound_root_is_denied() -> TestResult {
        let (dir, roots) = roots(false)?;
        let message = error_of(execute_record(
            &roots,
            json!({"text": "x", "scope": "global"}),
            "s",
        ))?;
        assert!(message.contains("kein globaler Speicher"), "{message}");
        assert!(!dir.path().join("global").exists());
        Ok(())
    }

    #[test]
    fn test_record_then_recall_round_trip_and_idempotence() -> TestResult {
        let (_dir, roots) = roots(true)?;
        for _ in 0..2 {
            let out = json_of(execute_record(
                &roots,
                json!({"text": "Nutze cargo nextest", "type": "preference"}),
                "s1",
            ))?;
            assert_eq!(out["scope"], "project");
            assert_eq!(out["name"], "nutze-cargo-nextest");
        }
        json_of(execute_record(
            &roots,
            json!({"text": "Globale Vorliebe nextest", "scope": "global"}),
            "s1",
        ))?;
        let out = json_of(execute_recall(&roots, json!({"query": "nextest"})))?;
        let hits = out["hits"].as_array().ok_or(TestError::Missing("hits"))?;
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0]["scope"], "project");
        assert_eq!(hits[1]["scope"], "global");
        let only_global = json_of(execute_recall(
            &roots,
            json!({"query": "nextest", "scope": "global"}),
        ))?;
        assert_eq!(only_global["hits"].as_array().map(Vec::len), Some(1));
        Ok(())
    }

    #[test]
    fn test_record_redacts_text_name_and_description() -> TestResult {
        let (_dir, roots) = roots(false)?;
        let secret = "sk-abcdefghijklmnopqrstuvwxyz0123456789";
        let out = json_of(execute_record(
            &roots,
            json!({"text": format!("API key ist {secret}")}),
            "s",
        ))?;
        let name = out["name"].as_str().ok_or(TestError::Missing("name"))?;
        assert!(!name.contains("abcdef"), "{name}");
        let store = FactStore::open(&roots.project, FactScope::Project).map_err(ctx("open"))?;
        let fact = store
            .read(name)
            .map_err(ctx("read"))?
            .ok_or(TestError::Missing("fact"))?;
        assert!(!fact.body.contains(secret) && !fact.description.contains(secret));
        let recalled = json_of(execute_recall(&roots, json!({"query": "API key"})))?;
        assert!(!recalled.to_string().contains(secret));
        Ok(())
    }

    #[test]
    fn test_recall_limits_results_and_truncates_body() -> TestResult {
        let (_dir, roots) = roots(false)?;
        for index in 0..8 {
            let body = format!("marker{index} {}", "x".repeat(BODY_CHARS * 2));
            json_of(execute_record(
                &roots,
                json!({"text": body, "title": format!("marker {index}")}),
                "s",
            ))?;
        }
        let out = json_of(execute_recall(
            &roots,
            json!({"query": "marker", "limit": 3}),
        ))?;
        let hits = out["hits"].as_array().ok_or(TestError::Missing("hits"))?;
        assert_eq!(hits.len(), 3);
        assert_eq!(out["truncated"], true);
        for hit in hits {
            assert_eq!(hit["body_truncated"], true);
            let body = hit["body"].as_str().ok_or(TestError::Missing("body"))?;
            assert!(body.chars().count() <= BODY_CHARS + 1);
        }
        let message = error_of(execute_recall(
            &roots,
            json!({"query": "marker", "limit": MAX_LIMIT + 1}),
        ))?;
        assert!(message.contains("limit"), "{message}");
        Ok(())
    }

    #[test]
    fn test_invalid_arguments_are_errors_not_panics() -> TestResult {
        let (_dir, roots) = roots(false)?;
        error_of(execute_recall(&roots, json!({"query": "  "})))?;
        error_of(execute_recall(
            &roots,
            json!({"query": "a", "scope": "nope"}),
        ))?;
        error_of(execute_recall(
            &roots,
            json!({"query": "a", "scope": "global"}),
        ))?;
        error_of(execute_recall(&roots, json!({"bogus": 1})))?;
        error_of(execute_record(&roots, json!({"text": ""}), "s"))?;
        error_of(execute_record(
            &roots,
            json!({"text": "a", "type": "zzz"}),
            "s",
        ))?;
        error_of(execute_record(
            &roots,
            json!({"text": "a", "scope": "all"}),
            "s",
        ))?;
        error_of(execute_record(
            &roots,
            json!({"text": "y".repeat(MAX_TEXT_CHARS + 1)}),
            "s",
        ))?;
        // `all` ohne globale Bindung überspringt Global statt zu scheitern.
        let out = json_of(execute_recall(&roots, json!({"query": "a"})))?;
        assert_eq!(out["global_unavailable"], true);
        Ok(())
    }
}
