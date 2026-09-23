//! `ExplorerToolProvider` — die vier `explore.*`-Werkzeuge über
//! [`harw_explorer::ExplorerIndex`].
//!
//! # Verantwortung
//! Jeder Aufruf baut den Index **bei Bedarf** frisch ab der kanonischen
//! Workspace-Wurzel der Sandbox ([`ExplorerIndex::build`] mit
//! [`ExplorerOptions::default`]) und projiziert ihn in eine begrenzte
//! Tool-Antwort:
//!
//! - `explore.tree` — eingerückter Textbaum plus Zusammenfassungszeile.
//! - `explore.projects` — JSON-Liste der erkannten Projekte.
//! - `explore.relations` — JSON-Liste der gefundenen Beziehungen (filterbar).
//! - `explore.find` — Pfadsuche mit Art, Größe und umgebendem Projekt.
//!
//! # Sicherheitskontrakt
//! - Permission: [`harw_authority::Permission::ReadWorkspace`], vom Prolog
//!   von `#[harw_macros::tool]` geprüft, bevor Argumente deserialisiert werden.
//! - Vom Modell übergebene Pfade müssen relativ sein; absolute Pfade und
//!   `..`-Komponenten werden als Tool-Fehler abgewiesen. `explore.tree`
//!   prüft den Zielordner zusätzlich über
//!   `WorkspaceBinding::resolve_existing` (Symlink-Schutz).
//! - Rein lesend, kein Netz, kein Subprozess.
//! - Jede Antwort ist auf [`MAX_OUTPUT_BYTES`] begrenzt; Kürzungen werden
//!   im Ergebnis vermerkt.
//!
//! # Nebenläufigkeit
//! Zustandslos; alle vier Tools sind `parallel_safe`.

use std::path::{Component, Path, PathBuf};

use harw_explorer::{ExplorerIndex, ExplorerOptions, Node, Project, Relation, RelationKind};
use harw_tools::{ToolOutput, ToolsError, executor::ToolExecutionContext};
use serde::Deserialize;
use serde_json::{Value, json};

/// Namen aller vier Explorer-Werkzeuge (in Registrierungsreihenfolge).
pub const EXPLORER_TOOL_NAMES: [&str; 4] = [
    "explore.tree",
    "explore.projects",
    "explore.relations",
    "explore.find",
];

/// Obergrenze einer einzelnen Tool-Antwort (≈ 64 KiB).
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Standardtiefe für `explore.tree`.
pub const DEFAULT_TREE_DEPTH: usize = 3;

/// Harte Maximaltiefe für `explore.tree`.
pub const MAX_TREE_DEPTH: usize = 12;

/// Maximale Anzahl Einträge, die `explore.tree` rendert.
pub const MAX_TREE_ENTRIES: usize = 2_000;

/// Standard-Trefferlimit für `explore.find`.
pub const DEFAULT_FIND_LIMIT: usize = 50;

/// Hartes Trefferlimit für `explore.find`.
pub const MAX_FIND_LIMIT: usize = 500;

// ---------------------------------------------------------------------------
// Argumente
// ---------------------------------------------------------------------------

/// Argumente für `explore.tree`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
pub struct ExploreTreeArgs {
    /// Directory relative to the workspace root. Default: the workspace root.
    pub path: Option<String>,
    /// Maximum depth below `path` (default 3, max 12).
    pub depth: Option<usize>,
    /// Also show entries excluded by .gitignore/.ignore/hidden rules (default false).
    pub include_ignored: Option<bool>,
}

/// Argumente für `explore.projects` (keine).
#[derive(Debug, Deserialize, harw_macros::Tool)]
pub struct ExploreProjectsArgs {}

/// Argumente für `explore.relations`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
pub struct ExploreRelationsArgs {
    /// Only relations whose source or target lies at or below this path (relative to the workspace root).
    pub project: Option<String>,
    /// Only relations of this kind: member, path-dep, dep, nested, link (snake_case names are accepted too).
    pub kind: Option<String>,
}

/// Argumente für `explore.find`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
pub struct ExploreFindArgs {
    /// Case-insensitive substring or glob pattern matched against relative paths.
    pub query: String,
    /// Maximum number of matches (default 50, max 500).
    pub limit: Option<usize>,
}

// ---------------------------------------------------------------------------
// Hilfsfunktionen
// ---------------------------------------------------------------------------

/// Prüft einen vom Modell übergebenen Pfad lexikalisch und normalisiert ihn.
///
/// Leere Eingabe und `.` bedeuten die Wurzel (leerer `PathBuf`). Absolute
/// Pfade, Präfixe und `..` werden abgewiesen.
fn validate_relative(raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    let mut out = PathBuf::new();
    if trimmed.is_empty() {
        return Ok(out);
    }
    let path = Path::new(trimmed);
    if path.is_absolute() || path.has_root() {
        return Err(format!(
            "path must be relative to the workspace root: {trimmed}"
        ));
    }
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!(
                    "path must not contain '..' (escapes the workspace root): {trimmed}"
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "path must be relative to the workspace root: {trimmed}"
                ));
            }
        }
    }
    Ok(out)
}

/// Anzeigeform eines relativen Pfads (`.` für die Wurzel, `/` als Trenner).
fn rel_display(path: &Path) -> String {
    if path.as_os_str().is_empty() {
        return ".".to_owned();
    }
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Kürzt einen Text auf höchstens `limit` Bytes (an einer Zeichengrenze)
/// und hängt einen Hinweis an.
fn truncate_text(mut text: String, limit: usize) -> String {
    if text.len() <= limit {
        return text;
    }
    let mut cut = limit;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    text.push_str(&format!(
        "\n… [output truncated at {} KiB]",
        limit / 1024
    ));
    text
}

/// Nimmt Elemente auf, solange ihre serialisierte Größe in das Budget passt.
///
/// Liefert die aufgenommenen Werte und die Zahl der ausgelassenen Elemente.
fn bounded_items(items: impl IntoIterator<Item = Value>, budget: usize) -> (Vec<Value>, usize) {
    let mut used = 0usize;
    let mut kept = Vec::new();
    let mut omitted = 0usize;
    for item in items {
        if omitted > 0 {
            omitted += 1;
            continue;
        }
        let size = item.to_string().len() + 1;
        if used + size > budget {
            omitted += 1;
            continue;
        }
        used += size;
        kept.push(item);
    }
    (kept, omitted)
}

/// Baut den Index ab der Workspace-Wurzel der Sandbox.
fn build_index(
    context: &ToolExecutionContext,
    include_ignored: bool,
) -> Result<ExplorerIndex, String> {
    let root = context.sandbox().workspace().canonical_root().to_path_buf();
    let options = ExplorerOptions {
        include_ignored,
        ..ExplorerOptions::default()
    };
    ExplorerIndex::build(&root, &options).map_err(|error| error.to_string())
}

/// Einheitliche Fehlerausgabe mit strukturiertem Log.
fn error_output(tool: &str, message: &str) -> ToolOutput {
    tracing::warn!(tool, error = %message, "explore-Werkzeug abgebrochen");
    ToolOutput::error(format!("{tool}: {message}"))
}

/// Budget für Listenelemente: Gesamtbudget abzüglich Reserve für Kopfdaten.
const ITEM_BUDGET: usize = MAX_OUTPUT_BYTES - 4 * 1024;

/// JSON-Form eines Projekts.
fn project_json(project: &Project) -> Value {
    json!({
        "root": rel_display(&project.root),
        "kind": project.kind.label(),
        "name": project.name,
        "manifest": project.manifest.as_deref().map(rel_display),
        "members": project.members.iter().map(|m| rel_display(m)).collect::<Vec<_>>(),
    })
}

/// JSON-Form einer Relation.
fn relation_json(relation: &Relation) -> Value {
    json!({
        "from": rel_display(&relation.from),
        "to": rel_display(&relation.to),
        "kind": relation.kind.label(),
        "label": relation.label,
    })
}

/// Löst einen `kind`-Filter (Label oder snake_case-Name) auf.
fn parse_relation_kind(raw: &str) -> Option<RelationKind> {
    const ALL: [RelationKind; 5] = [
        RelationKind::WorkspaceMember,
        RelationKind::PathDependency,
        RelationKind::CrateDependency,
        RelationKind::NestedProject,
        RelationKind::DocLink,
    ];
    let wanted = raw.trim().to_ascii_lowercase();
    ALL.into_iter().find(|kind| {
        kind.label() == wanted
            || serde_json::to_value(kind)
                .ok()
                .and_then(|value| value.as_str().map(|s| s == wanted))
                .unwrap_or(false)
    })
}

/// JSON-Form eines Suchtreffers.
fn find_match_json(index: &ExplorerIndex, node: &Node) -> Value {
    let project = index.project_at(&node.path);
    json!({
        "path": rel_display(&node.path),
        "kind": node.kind.label(),
        "size": node.size,
        "project": project.map(|p| rel_display(&p.root)),
        "project_name": project.map(|p| p.name.clone()),
    })
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

/// Rendert einen eingerückten Verzeichnisbaum ab einem relativen Pfad.
#[harw_macros::tool(
    name = "explore.tree",
    description = "Shows the directory tree of the workspace (any project type) as an indented \
                   text tree, followed by a summary line (counts per file kind and detected \
                   projects). 'path' is optional and relative to the workspace root; absolute \
                   paths and '..' are rejected. 'depth' limits the depth below 'path' \
                   (default 3, max 12). 'include_ignored' also lists entries hidden by \
                   .gitignore/.ignore rules. Output is capped at ~64 KiB. Read-only.",
    permission = "read_workspace",
    parallel_safe
)]
async fn explore_tree(
    context: &ToolExecutionContext,
    args: ExploreTreeArgs,
) -> Result<ToolOutput, ToolsError> {
    const TOOL: &str = "explore.tree";

    let relative = match validate_relative(args.path.as_deref().unwrap_or("")) {
        Ok(relative) => relative,
        Err(message) => return Ok(error_output(TOOL, &message)),
    };
    if !relative.as_os_str().is_empty() {
        match context.sandbox().workspace().resolve_existing(&relative) {
            Ok(resolved) if resolved.is_dir() => {}
            Ok(_) => {
                return Ok(error_output(
                    TOOL,
                    &format!("not a directory: {}", rel_display(&relative)),
                ));
            }
            Err(error) => return Ok(error_output(TOOL, &error.to_string())),
        }
    }

    let depth = args.depth.unwrap_or(DEFAULT_TREE_DEPTH).clamp(1, MAX_TREE_DEPTH);
    let index = match build_index(context, args.include_ignored.unwrap_or(false)) {
        Ok(index) => index,
        Err(message) => return Ok(error_output(TOOL, &message)),
    };

    tracing::info!(
        path = %rel_display(&relative),
        depth,
        nodes = index.nodes.len(),
        "explore.tree"
    );

    let mut text = index.tree(&relative, depth, MAX_TREE_ENTRIES);
    if !text.ends_with('\n') {
        text.push('\n');
    }
    // Zusammenfassung vor dem Kürzen sichern, damit sie immer sichtbar bleibt.
    let mut summary = index.summary();
    if index.truncated {
        summary.push_str(" (index truncated: node limit reached)");
    }
    let budget = MAX_OUTPUT_BYTES.saturating_sub(summary.len() + 64);
    let mut out = truncate_text(text, budget);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&summary);
    Ok(ToolOutput::text(out))
}

/// Listet alle erkannten Projekte.
#[harw_macros::tool(
    name = "explore.projects",
    description = "Lists all projects detected anywhere in the workspace tree (Cargo \
                   workspaces/crates, npm/pnpm, Python, Go, Git repositories, document \
                   collections), including nested ones. Returns JSON {projects:[{root, kind, \
                   name, manifest, members}], summary, truncated}. Paths are relative to the \
                   workspace root ('.' = root). Output is capped at ~64 KiB. Read-only.",
    permission = "read_workspace",
    parallel_safe
)]
async fn explore_projects(
    context: &ToolExecutionContext,
    _args: ExploreProjectsArgs,
) -> Result<ToolOutput, ToolsError> {
    const TOOL: &str = "explore.projects";

    let index = match build_index(context, false) {
        Ok(index) => index,
        Err(message) => return Ok(error_output(TOOL, &message)),
    };
    tracing::info!(projects = index.projects.len(), "explore.projects");

    let total = index.projects.len();
    let (projects, omitted) = bounded_items(index.projects.iter().map(project_json), ITEM_BUDGET);
    let mut value = json!({
        "projects": projects,
        "total": total,
        "summary": index.summary(),
        "truncated": index.truncated || omitted > 0,
    });
    if omitted > 0 {
        value["note"] = json!(format!("{omitted} projects omitted (output limit ~64 KiB)"));
    }
    Ok(ToolOutput::json(value))
}

/// Listet die Beziehungen zwischen Projekten und Dokumenten.
#[harw_macros::tool(
    name = "explore.relations",
    description = "Lists relations found between projects and files in the workspace: \
                   workspace membership ('member'), path dependencies ('path-dep'), named \
                   dependencies on projects in the tree ('dep'), nesting ('nested') and local \
                   Markdown links ('link'). Optional 'project' (path relative to the workspace \
                   root) keeps only relations whose source or target lies at or below it; \
                   optional 'kind' filters by relation kind. Returns JSON {relations:[{from, \
                   to, kind, label}], total, truncated}. Output is capped at ~64 KiB. Read-only.",
    permission = "read_workspace",
    parallel_safe
)]
async fn explore_relations(
    context: &ToolExecutionContext,
    args: ExploreRelationsArgs,
) -> Result<ToolOutput, ToolsError> {
    const TOOL: &str = "explore.relations";

    let filter_path = match args.project.as_deref().map(validate_relative).transpose() {
        Ok(path) => path.filter(|p| !p.as_os_str().is_empty()),
        Err(message) => return Ok(error_output(TOOL, &message)),
    };
    let filter_kind = match args.kind.as_deref() {
        None => None,
        Some(raw) => match parse_relation_kind(raw) {
            Some(kind) => Some(kind),
            None => {
                return Ok(error_output(
                    TOOL,
                    &format!(
                        "unknown relation kind '{raw}' (allowed: member, path-dep, dep, nested, link)"
                    ),
                ));
            }
        },
    };

    let index = match build_index(context, false) {
        Ok(index) => index,
        Err(message) => return Ok(error_output(TOOL, &message)),
    };

    let matching: Vec<&Relation> = index
        .relations
        .iter()
        .filter(|relation| filter_kind.is_none_or(|kind| relation.kind == kind))
        .filter(|relation| {
            filter_path
                .as_deref()
                .is_none_or(|p| relation.from.starts_with(p) || relation.to.starts_with(p))
        })
        .collect();
    tracing::info!(
        relations = matching.len(),
        total = index.relations.len(),
        "explore.relations"
    );

    let total = matching.len();
    let (relations, omitted) =
        bounded_items(matching.into_iter().map(relation_json), ITEM_BUDGET);
    let mut value = json!({
        "relations": relations,
        "total": total,
        "truncated": index.truncated || omitted > 0,
    });
    if omitted > 0 {
        value["note"] = json!(format!("{omitted} relations omitted (output limit ~64 KiB)"));
    }
    Ok(ToolOutput::json(value))
}

/// Sucht Einträge nach Pfad.
#[harw_macros::tool(
    name = "explore.find",
    description = "Finds files and directories in the workspace whose relative path matches \
                   'query' (case-insensitive substring, or a glob such as '*.pdf' or \
                   'src/**/*.rs'). Returns JSON {matches:[{path, kind, size, project, \
                   project_name}], count, truncated}; 'project' is the innermost project \
                   containing the match. 'limit' defaults to 50 (max 500). Output is capped \
                   at ~64 KiB. Read-only.",
    permission = "read_workspace",
    parallel_safe
)]
async fn explore_find(
    context: &ToolExecutionContext,
    args: ExploreFindArgs,
) -> Result<ToolOutput, ToolsError> {
    const TOOL: &str = "explore.find";

    let query = args.query.trim();
    if query.is_empty() {
        return Ok(error_output(TOOL, "query must not be empty"));
    }
    let limit = args.limit.unwrap_or(DEFAULT_FIND_LIMIT).clamp(1, MAX_FIND_LIMIT);

    let index = match build_index(context, false) {
        Ok(index) => index,
        Err(message) => return Ok(error_output(TOOL, &message)),
    };

    // Einen Treffer mehr anfordern, um ein erreichtes Limit zu erkennen.
    let found = index.find(query, limit + 1);
    let hit_limit = found.len() > limit;
    tracing::info!(query, matches = found.len().min(limit), "explore.find");

    let (matches, omitted) = bounded_items(
        found
            .into_iter()
            .take(limit)
            .map(|node| find_match_json(&index, node)),
        ITEM_BUDGET,
    );
    let mut value = json!({
        "count": matches.len(),
        "matches": matches,
        "truncated": hit_limit || omitted > 0 || index.truncated,
    });
    if hit_limit {
        value["note"] = json!(format!("limit of {limit} matches reached; refine the query"));
    } else if omitted > 0 {
        value["note"] = json!(format!("{omitted} matches omitted (output limit ~64 KiB)"));
    }
    Ok(ToolOutput::json(value))
}

harw_tools::tool_provider! {
    /// Stellt die vier Explorer-Werkzeuge bereit: `explore.tree`,
    /// `explore.projects`, `explore.relations` und `explore.find`.
    pub struct ExplorerToolProvider {
        ExploreTreeTool,
        ExploreProjectsTool,
        ExploreRelationsTool,
        ExploreFindTool,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_extension_api::contributors::ToolProvider as _;
    use harw_tools::{ToolCall, ToolName};
    use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
    use std::fmt;
    use std::fs;

    /// Test-Fehlertyp: ersetzt panic!/unwrap/expect in Tests.
    enum TestError {
        Missing(&'static str),
        Unexpected(String),
        Context {
            context: &'static str,
            source: String,
        },
    }

    type TestResult<T = ()> = Result<T, TestError>;

    impl fmt::Debug for TestError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Missing(what) => write!(f, "erwarteter Wert fehlt: {what}"),
                Self::Unexpected(message) => write!(f, "unerwartetes Ergebnis: {message}"),
                Self::Context { context, source } => write!(f, "{context}: {source}"),
            }
        }
    }

    /// Übersetzt `.expect("…")` in `.map_err(ctx("…"))?`.
    fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
        move |error| TestError::Context {
            context,
            source: error.to_string(),
        }
    }

    /// Ausführungskontext mit Workspace `<harness>/ws`.
    fn sandbox_context(
        harness_root: &Path,
        permissions: Vec<Permission>,
    ) -> TestResult<ToolExecutionContext> {
        fs::create_dir_all(harness_root.join("ws")).map_err(ctx("Workspace anlegen"))?;
        let registry = WorkspaceRegistry::build(
            harness_root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("t"),
                workspace: WorkspaceId::from_str("w"),
                root: PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("Workspace-Registry bauen"))?;
        let binding = registry
            .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("w"))
            .map_err(ctx("Workspace auflösen"))?;
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::from_policy(permissions));
        Ok(ToolExecutionContext::new(
            SessionId::new(),
            TurnId::new(),
            sandbox,
        ))
    }

    /// Legt einen kleinen gemischten Workspace an: Cargo-Workspace mit
    /// Mitglied `a`, eine README mit Link und einen `docs/`-Ordner.
    fn fixture() -> TestResult<(tempfile::TempDir, ToolExecutionContext)> {
        let harness = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let ws = harness.path().join("ws");
        fs::create_dir_all(ws.join("a/src")).map_err(ctx("a/src anlegen"))?;
        fs::create_dir_all(ws.join("docs")).map_err(ctx("docs anlegen"))?;
        fs::write(
            ws.join("Cargo.toml"),
            "[workspace]\nmembers = [\"a\"]\n",
        )
        .map_err(ctx("Wurzel-Manifest"))?;
        fs::write(
            ws.join("a/Cargo.toml"),
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
        )
        .map_err(ctx("Member-Manifest"))?;
        fs::write(ws.join("a/src/lib.rs"), "pub fn a() {}\n").map_err(ctx("lib.rs"))?;
        fs::write(ws.join("README.md"), "# Demo\n\nSee [guide](docs/guide.md).\n")
            .map_err(ctx("README"))?;
        fs::write(ws.join("docs/guide.md"), "# Guide\n").map_err(ctx("guide"))?;
        let context = sandbox_context(harness.path(), vec![Permission::ReadWorkspace])?;
        Ok((harness, context))
    }

    /// Führt ein Tool über den Provider aus.
    fn run(context: &ToolExecutionContext, name: &str, arguments: Value) -> TestResult<ToolOutput> {
        let provider = ExplorerToolProvider::new();
        let executor = provider
            .executor(&ToolName::new(name))
            .ok_or(TestError::Missing("Executor vorhanden"))?;
        let call = ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(name),
            arguments,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(ctx("Tokio-Runtime bauen"))?;
        runtime
            .block_on(executor.execute(context, &call))
            .map_err(ctx("Tool läuft"))
    }

    fn expect_json(output: ToolOutput) -> TestResult<Value> {
        match output {
            ToolOutput::Json { content } => Ok(content),
            other => Err(TestError::Unexpected(format!("JSON erwartet: {other:?}"))),
        }
    }

    fn expect_text(output: ToolOutput) -> TestResult<String> {
        match output {
            ToolOutput::Text { content } => Ok(content),
            other => Err(TestError::Unexpected(format!("Text erwartet: {other:?}"))),
        }
    }

    fn expect_error(output: ToolOutput) -> TestResult<String> {
        match output {
            ToolOutput::Error { message } => Ok(message),
            other => Err(TestError::Unexpected(format!("Fehler erwartet: {other:?}"))),
        }
    }

    #[test]
    fn test_provider_lists_all_four_tools_in_order() {
        assert_eq!(ExplorerToolProvider::TOOL_NAMES, EXPLORER_TOOL_NAMES.as_slice());
        let provider = ExplorerToolProvider::new();
        let tools = provider.tools();
        let names: Vec<&str> = tools
            .iter()
            .map(harw_tools::ToolSpec::name)
            .collect();
        for expected in EXPLORER_TOOL_NAMES {
            assert!(names.contains(&expected), "{expected} fehlt");
        }
        assert_eq!(names.len(), 4);
    }

    #[test]
    fn test_provider_tools_are_parallel_safe_and_read_workspace() {
        let provider = ExplorerToolProvider::new();
        for (name, permission) in ExplorerToolProvider::TOOL_NAMES
            .iter()
            .zip(ExplorerToolProvider::TOOL_PERMISSIONS)
        {
            assert!(provider.parallel_safe(&ToolName::new(*name)), "{name}");
            assert_eq!(*permission, Some(Permission::ReadWorkspace), "{name}");
        }
        assert!(!provider.parallel_safe(&ToolName::new("explore.unknown")));
        assert!(provider.executor(&ToolName::new("explore.unknown")).is_none());
    }

    #[test]
    fn test_validate_relative_rejects_escapes() -> TestResult {
        assert_eq!(validate_relative("").map_err(ctx("leer"))?, PathBuf::new());
        assert_eq!(validate_relative("./").map_err(ctx("punkt"))?, PathBuf::new());
        assert_eq!(
            validate_relative("./a/b").map_err(ctx("a/b"))?,
            PathBuf::from("a/b")
        );
        assert!(validate_relative("../x").is_err());
        assert!(validate_relative("a/../../x").is_err());
        assert!(validate_relative("/etc").is_err());
        Ok(())
    }

    #[test]
    fn test_truncate_text_respects_limit_and_char_boundary() {
        let text = "ä".repeat(3000);
        let cut = truncate_text(text, 1025);
        assert!(cut.contains("output truncated"));
        assert!(cut.len() < 1100);
        assert_eq!(truncate_text("kurz".to_owned(), 1024), "kurz");
    }

    #[test]
    fn test_bounded_items_counts_omitted() {
        let items = (0..100).map(|i| json!({ "i": i, "pad": "x".repeat(50) }));
        let (kept, omitted) = bounded_items(items, 700);
        assert!(!kept.is_empty());
        assert_eq!(kept.len() + omitted, 100);
        assert!(omitted > 0);
    }

    #[test]
    fn test_parse_relation_kind_accepts_label_and_snake_case() {
        assert_eq!(parse_relation_kind("member"), Some(RelationKind::WorkspaceMember));
        assert_eq!(
            parse_relation_kind("workspace_member"),
            Some(RelationKind::WorkspaceMember)
        );
        assert_eq!(parse_relation_kind("LINK"), Some(RelationKind::DocLink));
        assert_eq!(parse_relation_kind("bogus"), None);
    }

    #[test]
    fn test_tree_renders_tree_and_summary() -> TestResult {
        let (_harness, context) = fixture()?;
        let text = expect_text(run(&context, "explore.tree", json!({}))?)?;
        assert!(text.contains("README.md"), "{text}");
        assert!(text.contains("docs"), "{text}");
        Ok(())
    }

    #[test]
    fn test_tree_subpath_and_escape_rejection() -> TestResult {
        let (_harness, context) = fixture()?;
        let text = expect_text(run(&context, "explore.tree", json!({ "path": "a", "depth": 5 }))?)?;
        assert!(text.contains("lib.rs"), "{text}");

        let message = expect_error(run(&context, "explore.tree", json!({ "path": "../" }))?)?;
        assert!(message.contains(".."), "{message}");
        let message = expect_error(run(&context, "explore.tree", json!({ "path": "/etc" }))?)?;
        assert!(message.contains("relative"), "{message}");
        let message =
            expect_error(run(&context, "explore.tree", json!({ "path": "README.md" }))?)?;
        assert!(message.contains("not a directory"), "{message}");
        Ok(())
    }

    #[test]
    fn test_projects_lists_cargo_workspace_and_member() -> TestResult {
        let (_harness, context) = fixture()?;
        let value = expect_json(run(&context, "explore.projects", json!({}))?)?;
        let projects = value["projects"]
            .as_array()
            .ok_or(TestError::Missing("projects-Array"))?;
        assert!(
            projects
                .iter()
                .any(|p| p["kind"] == "cargo-workspace" && p["root"] == "."),
            "{value}"
        );
        assert!(
            projects
                .iter()
                .any(|p| p["kind"] == "cargo-crate" && p["name"] == "a"),
            "{value}"
        );
        assert!(value["summary"].is_string());
        Ok(())
    }

    #[test]
    fn test_relations_filter_by_kind() -> TestResult {
        let (_harness, context) = fixture()?;
        let value = expect_json(run(
            &context,
            "explore.relations",
            json!({ "kind": "member" }),
        )?)?;
        let relations = value["relations"]
            .as_array()
            .ok_or(TestError::Missing("relations-Array"))?;
        assert!(
            relations.iter().all(|r| r["kind"] == "member"),
            "{value}"
        );
        assert!(
            relations.iter().any(|r| r["from"] == "." && r["to"] == "a"),
            "{value}"
        );

        let message = expect_error(run(
            &context,
            "explore.relations",
            json!({ "kind": "bogus" }),
        )?)?;
        assert!(message.contains("unknown relation kind"), "{message}");
        let message = expect_error(run(
            &context,
            "explore.relations",
            json!({ "project": "../elsewhere" }),
        )?)?;
        assert!(message.contains(".."), "{message}");
        Ok(())
    }

    #[test]
    fn test_find_reports_kind_size_and_project() -> TestResult {
        let (_harness, context) = fixture()?;
        let value = expect_json(run(&context, "explore.find", json!({ "query": "lib.rs" }))?)?;
        let matches = value["matches"]
            .as_array()
            .ok_or(TestError::Missing("matches-Array"))?;
        let hit = matches
            .iter()
            .find(|m| m["path"] == "a/src/lib.rs")
            .ok_or(TestError::Missing("Treffer a/src/lib.rs"))?;
        assert_eq!(hit["kind"], "rust");
        assert_eq!(hit["size"], 14);
        assert_eq!(hit["project"], "a");

        let message = expect_error(run(&context, "explore.find", json!({ "query": "  " }))?)?;
        assert!(message.contains("empty"), "{message}");
        Ok(())
    }

    #[test]
    fn test_find_limit_marks_truncation() -> TestResult {
        let (harness, context) = fixture()?;
        for i in 0..5 {
            fs::write(harness.path().join(format!("ws/docs/note{i}.md")), "x")
                .map_err(ctx("note"))?;
        }
        let value = expect_json(run(
            &context,
            "explore.find",
            json!({ "query": "note", "limit": 2 }),
        )?)?;
        assert_eq!(value["count"], 2);
        assert_eq!(value["truncated"], true);
        Ok(())
    }

    #[test]
    fn test_tools_deny_without_read_workspace() -> TestResult {
        let harness = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let context = sandbox_context(harness.path(), Vec::new())?;
        for name in EXPLORER_TOOL_NAMES {
            let message = expect_error(run(&context, name, json!({ "query": "x" }))?)?;
            assert!(message.contains("ReadWorkspace"), "{name}: {message}");
        }
        Ok(())
    }
}
