//! Agent-facing Obsidian vault tools.
//!
//! # Purpose
//! The vault is the project's long-form memory: code maps, architecture
//! notes, decision records, and learning summaries. These tools let an
//! agent explore and maintain it the same way it works with source code —
//! map the structure, read a note with its metadata, search across notes,
//! follow wikilinks in both directions, and write updates that preserve
//! frontmatter and links.
//!
//! # Memory wiring
//! A note whose frontmatter carries `memory: <palace-node-id>` is connected
//! to the project's shared memory: `obsidian.read` resolves that id against
//! the knowledge store and returns the palace node's title, status and
//! outgoing links alongside the note body (established nodes only, same
//! visibility rules as `palace.search`/`palace.recall`). This keeps the
//! code map in the vault and the distilled, agent-readable truth in the
//! palace pointing at each other without duplicating content.
//!
//! # Tool set
//! - `obsidian.map` — directory/structure overview of the vault.
//! - `obsidian.read` — one note: frontmatter, body, wikilinks, memory link.
//! - `obsidian.search` — full-text search with snippets and line numbers.
//! - `obsidian.links` — outgoing and backlinks of one note.
//! - `obsidian.write` — create or replace a note (preserving caller
//!   responsibility for frontmatter/links; the tool never rewrites content
//!   on its own).
//!
//! # Permissions
//! Read tools declare `ReadWorkspace`; `obsidian.write` declares
//! `WriteWorkspace`. All paths are resolved through the sandbox's
//! [`SandboxSpec::workspace`] binding (see [`vault`]).

pub mod provider;
pub mod vault;

pub use provider::ObsidianToolProvider;

use std::path::PathBuf;

use harw_knowledge::memory::palace::{TITLE_KEY, is_agent_readable, scan_wikilinks};
use harw_knowledge::{KnowledgeIndex, KnowledgeStore};
use harw_macros::Tool;
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::Deserialize;

use crate::vault::{VaultRoot, frontmatter_scalar, split_frontmatter};

/// Maximum number of notes listed or returned by map/search/links.
pub const MAX_RESULTS: usize = 200;

/// Maximum characters of body text returned per note.
pub const MAX_BODY_CHARS: usize = 8_000;

/// Frontmatter key linking a note to a palace node.
pub const MEMORY_KEY: &str = "memory";

/// Resolve the vault root for a call.
fn vault_of(
    context: &ToolExecutionContext,
    tool: &str,
) -> Result<VaultRoot, ToolOutput> {
    VaultRoot::resolve(context.sandbox()).map_err(|err| ToolOutput::error(format!("{tool}: {err}")))
}

/// Read a note file and return its parsed parts as JSON.
///
/// # Errors
/// Never returns `Err`; failures become `ToolOutput::error`.
fn read_note_parts(
    vault: &VaultRoot,
    note: &std::path::Path,
    memory: Option<&KnowledgeStore>,
) -> Result<serde_json::Value, String> {
    let raw = std::fs::read_to_string(note)
        .map_err(|error| format!("cannot read {}: {error}", note.display()))?;
    let (yaml, body) = split_frontmatter(&raw);
    let yaml_str = yaml.as_deref().unwrap_or("");
    let mut links = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for target in scan_wikilinks(body).into_iter().chain(scan_wikilinks(yaml_str)) {
        if seen.insert(target.clone()) {
            links.push(target);
        }
    }
    let mut value = serde_json::json!({
        "path": vault.display_path(note),
        "frontmatter": yaml.as_ref().map(|y| serde_json::json!({"raw": y})),
        "body": truncate(&body, MAX_BODY_CHARS),
        "wikilinks": links,
    });
    if let (Some(yaml), Some(store)) = (yaml.as_deref(), memory) {
        if let Some(node_id) = frontmatter_scalar(yaml, MEMORY_KEY) {
            value["memory"] = resolve_memory(store, node_id);
        }
    }
    Ok(value)
}

/// Resolve a `memory:` palace node id for the read tool.
///
/// Only established, agent-readable nodes are returned — the same rule the
/// palace tools apply (`is_agent_readable` over the cached index).
fn resolve_memory(store: &KnowledgeStore, node_id: &str) -> serde_json::Value {
    let view = match KnowledgeIndex::cached(store) {
        Ok(view) => view,
        Err(error) => return serde_json::json!({"error": error.to_string()}),
    };
    let artifact = match view.iter().find(|a| a.id.as_str() == node_id) {
        Some(artifact) => artifact,
        None => {
            return serde_json::json!({
                "error": format!("palace node '{node_id}' not found in the agent-readable view")
            })
        }
    };
    if !is_agent_readable(artifact) {
        return serde_json::json!({
            "error": format!("palace node '{node_id}' is not agent-readable (not established or hidden)")
        });
    }
    serde_json::json!({
        "id": artifact.id.as_str(),
        "title": artifact.frontmatter.extra.get(TITLE_KEY).and_then(|v| v.as_str()).unwrap_or(node_id),
        "status": "established",
        "body": truncate(&artifact.body, 1500),
        "outgoing_links": scan_wikilinks(&artifact.body),
    })
}

/// Truncate a string to `max` characters on a char boundary.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{cut}… [truncated]")
}

// ── obsidian.map ─────────────────────────────────────────────────────────────

/// Arguments of `obsidian.map`.
#[derive(Debug, Tool, Deserialize)]
#[serde(deny_unknown_fields)]
#[tool(
    name = "obsidian.map",
    description = "Lists the vault structure: folders and markdown notes below the vault root, with each note's title (frontmatter or filename) and memory link if present. Use it to see what the project's long-form memory covers before reading or searching.",
)]
pub struct ObsidianMapArgs {
    /// Optional subdirectory (vault-relative) to map instead of the whole vault.
    #[serde(default)]
    pub folder: Option<String>,
}

#[harw_macros::tool(
    name = "obsidian.map",
    description = "Lists the vault structure: folders and markdown notes below the vault root, with each note's title (frontmatter or filename) and memory link if present. Use it to see what the project's long-form memory covers before reading or searching.",
    permission = "read_workspace"
)]
async fn obsidian_map(
    context: &ToolExecutionContext,
    args: ObsidianMapArgs,
) -> Result<ToolOutput, ToolsError> {
    let tool = "obsidian.map";
    let vault = match vault_of(context, tool) {
        Ok(v) => v,
        Err(output) => return Ok(output),
    };
    obsidian_map_sync(&vault, args.folder.as_deref())
        .map(ToolOutput::json)
        .or_else(|err| Ok(ToolOutput::error(format!("{tool}: {err}"))))
}

/// Synchronous core of `obsidian.map`.
fn obsidian_map_sync(
    vault: &VaultRoot,
    folder: Option<&str>,
) -> Result<serde_json::Value, String> {
    let start = vault.existing_dir(folder.unwrap_or("."))?;
    let mut notes = Vec::new();
    let mut folders = Vec::new();
    collect_entries(vault, &start, &mut folders, &mut notes)?;
    notes.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    folders.sort();
    Ok(serde_json::json!({
        "root": vault.display_path(&start),
        "folders": folders,
        "notes": notes,
    }))
}

/// Walk `dir` collecting subfolders and markdown notes (title + memory link).
fn collect_entries(
    vault: &VaultRoot,
    dir: &std::path::Path,
    folders: &mut Vec<String>,
    notes: &mut Vec<serde_json::Value>,
) -> Result<(), String> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|error| format!("cannot list {}: {error}", dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("cannot list {}: {error}", dir.display()))?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            folders.push(vault.display_path(&path));
            collect_entries(vault, &path, folders, notes)?;
        } else if name.ends_with(".md") {
            let note = read_note_parts(vault, &path, None)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            notes.push(serde_json::json!({
                "path": note["path"],
                "title": first_heading(&note["body"].as_str().unwrap_or_default())
                    .unwrap_or_else(|| name.trim_end_matches(".md").to_owned()),
                "memory": note.get("memory").cloned(),
            }));
        }
    }
    Ok(())
}

/// First `# ` heading of a markdown body, as a cheap title fallback.
fn first_heading(body: &str) -> Option<String> {
    body.lines()
        .find_map(|line| line.strip_prefix("# "))
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
}

// ── obsidian.read ────────────────────────────────────────────────────────────

/// Arguments of `obsidian.read`.
#[derive(Debug, Tool, Deserialize)]
#[serde(deny_unknown_fields)]
#[tool(
    name = "obsidian.read",
    description = "Reads one vault note: path, frontmatter, body, wikilinks and — when the frontmatter carries `memory: <palace-node-id>` — the linked established palace node. Use `obsidian.map` or `obsidian.search` first to find the path.",
)]
pub struct ObsidianReadArgs {
    /// Vault-relative path of the note, e.g. `70-decisions/README.md`.
    pub path: String,
}

#[harw_macros::tool(
    name = "obsidian.read",
    description = "Reads one vault note: path, frontmatter, body, wikilinks and — when the frontmatter carries `memory: <palace-node-id>` — the linked established palace node. Use `obsidian.map` or `obsidian.search` first to find the path.",
    permission = "read_workspace"
)]
async fn obsidian_read(
    context: &ToolExecutionContext,
    args: ObsidianReadArgs,
) -> Result<ToolOutput, ToolsError> {
    let tool = "obsidian.read";
    let vault = match vault_of(context, tool) {
        Ok(v) => v,
        Err(output) => return Ok(output),
    };
    let note = match vault.existing_note(&args.path) {
        Ok(p) => p,
        Err(err) => return Ok(ToolOutput::error(format!("{tool}: {err}"))),
    };
    // The knowledge store lives in the process' service map only in the full
    // runtime; without one the memory link is simply not resolved.
    obsidian_read_sync(&vault, &note, None)
        .map(ToolOutput::json)
        .or_else(|err| Ok(ToolOutput::error(format!("{tool}: {err}"))))
}

/// Synchronous core of `obsidian.read`, separated so tests can run it
/// without an async runtime or service map.
fn obsidian_read_sync(
    vault: &VaultRoot,
    note: &std::path::Path,
    store: Option<&KnowledgeStore>,
) -> Result<serde_json::Value, String> {
    read_note_parts(vault, note, store)
}

// ── obsidian.search ──────────────────────────────────────────────────────────

/// Arguments of `obsidian.search`.
#[derive(Debug, Tool, Deserialize)]
#[serde(deny_unknown_fields)]
#[tool(
    name = "obsidian.search",
    description = "Full-text search across all vault notes. Returns matching notes with line numbers and a snippet each. Case-insensitive; plain substring, not regex.",
)]
pub struct ObsidianSearchArgs {
    /// The text to find (case-insensitive substring).
    pub query: String,
    /// Optional subdirectory (vault-relative) to restrict the search to.
    #[serde(default)]
    pub folder: Option<String>,
    /// Maximum number of matches (default 20, hard cap 100).
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Default and cap for `obsidian.search`'s `limit`.
pub const SEARCH_DEFAULT_LIMIT: usize = 20;
pub const SEARCH_MAX_LIMIT: usize = 100;

#[harw_macros::tool(
    name = "obsidian.search",
    description = "Full-text search across all vault notes. Returns matching notes with line numbers and a snippet each. Case-insensitive; plain substring, not regex.",
    permission = "read_workspace"
)]
async fn obsidian_search(
    context: &ToolExecutionContext,
    args: ObsidianSearchArgs,
) -> Result<ToolOutput, ToolsError> {
    let tool = "obsidian.search";
    let vault = match vault_of(context, tool) {
        Ok(v) => v,
        Err(output) => return Ok(output),
    };
    obsidian_search_sync(&vault, &args.query, args.folder.as_deref(), args.limit)
        .map(ToolOutput::json)
        .or_else(|err| Ok(ToolOutput::error(format!("{tool}: {err}"))))
}

/// Synchronous core of `obsidian.search`.
fn obsidian_search_sync(
    vault: &VaultRoot,
    query: &str,
    folder: Option<&str>,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    if query.trim().is_empty() {
        return Err("'query' must not be empty".to_owned());
    }
    let limit = limit.unwrap_or(SEARCH_DEFAULT_LIMIT).clamp(1, SEARCH_MAX_LIMIT);
    let needle = query.to_lowercase();
    let mut matches = Vec::new();
    for path in markdown_files(vault, folder)? {
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(_) => continue,
        };
        for (idx, line) in raw.lines().enumerate() {
            if line.to_lowercase().contains(&needle) {
                let display = vault.display_path(&path);
                matches.push(serde_json::json!({
                    "path": display,
                    "line": idx + 1,
                    "snippet": truncate(line.trim(), 240),
                }));
                if matches.len() >= limit {
                    return Ok(serde_json::json!({ "matches": matches, "truncated": matches.len() >= limit }));
                }
            }
        }
    }
    Ok(serde_json::json!({ "matches": matches, "truncated": false }))
}

/// Collect all markdown files below the vault root (or a subfolder).
fn markdown_files(vault: &VaultRoot, folder: Option<&str>) -> Result<Vec<PathBuf>, String> {
    let start = match folder {
        Some(sub) => {
            let p = vault.existing_note(sub)?;
            if !p.is_dir() {
                return Err(format!("'{sub}' is not a folder"));
            }
            p
        }
        None => vault.existing_dir(".")?,
    };
    let mut out = Vec::new();
    walk_markdown(&start, &mut out);
    Ok(out)
}

fn walk_markdown(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            walk_markdown(&path, out);
        } else if name.ends_with(".md") {
            out.push(path);
        }
    }
}

// ── obsidian.links ───────────────────────────────────────────────────────────

/// Arguments of `obsidian.links`.
#[derive(Debug, Tool, Deserialize)]
#[serde(deny_unknown_fields)]
#[tool(
    name = "obsidian.links",
    description = "Shows the link neighbourhood of one note: outgoing wikilinks (with the note names they resolve to inside the vault) and backlinks (other notes linking to it). Use it to follow the knowledge graph.",
)]
pub struct ObsidianLinksArgs {
    /// Vault-relative path of the note.
    pub path: String,
}

#[harw_macros::tool(
    name = "obsidian.links",
    description = "Shows the link neighbourhood of one note: outgoing wikilinks (with the note names they resolve to inside the vault) and backlinks (other notes linking to it). Use it to follow the knowledge graph.",
    permission = "read_workspace"
)]
async fn obsidian_links(
    context: &ToolExecutionContext,
    args: ObsidianLinksArgs,
) -> Result<ToolOutput, ToolsError> {
    let tool = "obsidian.links";
    let vault = match vault_of(context, tool) {
        Ok(v) => v,
        Err(output) => return Ok(output),
    };
    let note = match vault.existing_note(&args.path) {
        Ok(p) => p,
        Err(err) => return Ok(ToolOutput::error(format!("{tool}: {err}"))),
    };
    obsidian_links_sync(&vault, &note)
        .map(ToolOutput::json)
        .or_else(|err| Ok(ToolOutput::error(format!("{tool}: {err}"))))
}

/// Synchronous core of `obsidian.links`.
fn obsidian_links_sync(
    vault: &VaultRoot,
    note: &std::path::Path,
) -> Result<serde_json::Value, String> {
    let raw = std::fs::read_to_string(note)
        .map_err(|error| format!("cannot read {}: {error}", note.display()))?;
    let (_, body) = split_frontmatter(&raw);
    let outgoing: Vec<String> = scan_wikilinks(body);
    let target_stem = note
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut backlinks = Vec::new();
    for path in markdown_files(vault, None)? {
        if path == note {
            continue;
        }
        let Ok(other) = std::fs::read_to_string(&path) else {
            continue;
        };
        if other.to_lowercase().contains(&target_stem) {
            let hits: Vec<usize> = other
                .lines()
                .enumerate()
                .filter(|(_, l)| l.to_lowercase().contains(&target_stem))
                .map(|(i, _)| i + 1)
                .take(5)
                .collect();
            backlinks.push(serde_json::json!({
                "path": vault.display_path(&path),
                "lines": hits,
            }));
        }
        if backlinks.len() >= MAX_RESULTS {
            break;
        }
    }
    Ok(serde_json::json!({
        "path": vault.display_path(note),
        "outgoing": outgoing,
        "backlinks": backlinks,
    }))
}

// ── obsidian.write ───────────────────────────────────────────────────────────

/// Arguments of `obsidian.write`.
#[derive(Debug, Tool, Deserialize)]
#[serde(deny_unknown_fields)]
#[tool(
    name = "obsidian.write",
    description = "Creates or fully replaces one vault note. The content must be complete markdown (frontmatter included when wanted); the tool never merges or rewrites. For small edits prefer reading the note and writing the full new text. Set `memory` to a palace node id (e.g. `palace/code-map`) to wire the note into the agent-readable knowledge graph.",
)]
pub struct ObsidianWriteArgs {
    /// Vault-relative path of the note (`.md` recommended).
    pub path: String,
    /// Full note content to write.
    pub content: String,
}

#[harw_macros::tool(
    name = "obsidian.write",
    description = "Creates or fully replaces one vault note. The content must be complete markdown (frontmatter included when wanted); the tool never merges or rewrites. For small edits prefer reading the note and writing the full new text. Set `memory` to a palace node id (e.g. `palace/code-map`) to wire the note into the agent-readable knowledge graph.",
    permission = "write_workspace"
)]
async fn obsidian_write(
    context: &ToolExecutionContext,
    args: ObsidianWriteArgs,
) -> Result<ToolOutput, ToolsError> {
    let tool = "obsidian.write";
    let vault = match vault_of(context, tool) {
        Ok(v) => v,
        Err(output) => return Ok(output),
    };
    let note = match vault.writable_note(&args.path) {
        Ok(p) => p,
        Err(err) => return Ok(ToolOutput::error(format!("{tool}: {err}"))),
    };
    obsidian_write_sync(&vault, &note, &args.content)
        .map(ToolOutput::json)
        .or_else(|err| Ok(ToolOutput::error(format!("{tool}: {err}"))))
}

/// Synchronous core of `obsidian.write`.
fn obsidian_write_sync(
    vault: &VaultRoot,
    note: &std::path::Path,
    content: &str,
) -> Result<serde_json::Value, String> {
    if content.trim().is_empty() {
        return Err("'content' must not be empty".to_owned());
    }
    if !note.parent().map(|p| p.is_dir()).unwrap_or(false) {
        return Err(format!(
            "parent folder of {} does not exist; create it first",
            note.display()
        ));
    }
    let existed = note.exists();
    std::fs::write(note, content)
        .map_err(|error| format!("cannot write {}: {error}", note.display()))?;
    // Post-conditions worth reporting: does the note carry valid frontmatter
    // and a memory link? Purely informational, never fails the write.
    let (yaml, _) = split_frontmatter(content);
    let memory = yaml
        .as_deref()
        .and_then(|y| frontmatter_scalar(y, MEMORY_KEY))
        .map(str::to_owned);
    Ok(serde_json::json!({
        "path": vault.display_path(note),
        "created": !existed,
        "has_frontmatter": yaml.is_some(),
        "memory": memory,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a vault root over a temp workspace: `<tmp>/docs/planning`,
    /// resolved through a real SandboxSpec (same pattern as the lens tests).
    fn test_vault() -> (tempfile::TempDir, VaultRoot) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("docs/planning/sub")).expect("mkdir");
        std::fs::write(
            dir.path().join("docs/planning/code-map.md"),
            "---\ntitle: Code Map\nmemory: palace/code-map\n---\n\n# Code Map\nSee [[architecture]].\n",
        )
        .expect("write");
        std::fs::write(
            dir.path().join("docs/planning/sub/architecture.md"),
            "# Architecture\nThe system is layered.\n",
        )
        .expect("write");
        let sandbox = crate::vault::tests::test_sandbox_at(dir.path(), "obsidian-test");
        let vault = VaultRoot::resolve(&sandbox).expect("vault");
        (dir, vault)
    }

    #[test]
    fn map_lists_folders_and_notes_with_titles() {
        let (_dir, vault) = test_vault();
        let out = obsidian_map_sync(&vault, None).expect("map");
        let notes = out["notes"].as_array().expect("notes");
        assert_eq!(notes.len(), 2);
        assert!(out["folders"].as_array().expect("folders").iter().any(|f| f.as_str().unwrap_or("").contains("sub")));
        assert!(notes.iter().any(|n| n["title"] == "Code Map"));
    }

    #[test]
    fn read_extracts_frontmatter_links_and_memory_id() {
        let (_dir, vault) = test_vault();
        let note = vault.existing_note("code-map.md").expect("note");
        let out = obsidian_read_sync(&vault, &note, None).expect("read");
        assert_eq!(out["wikilinks"].as_array().expect("links")[0], "architecture");
        let yaml = out["frontmatter"]["raw"].as_str().expect("yaml");
        assert!(yaml.contains("memory: palace/code-map"));
    }

    #[test]
    fn search_finds_case_insensitive_matches_with_lines() {
        let (_dir, vault) = test_vault();
        let out = obsidian_search_sync(&vault, "LAYERED", None, None).expect("search");
        let matches = out["matches"].as_array().expect("matches");
        assert_eq!(matches.len(), 1);
        // "The system is layered." is the second line of architecture.md.
        assert_eq!(matches[0]["line"], 2);
        assert!(matches[0]["path"].as_str().unwrap_or("").contains("architecture"));
    }

    #[test]
    fn search_rejects_empty_query() {
        let (_dir, vault) = test_vault();
        assert!(obsidian_search_sync(&vault, "  ", None, None).is_err());
    }

    #[test]
    fn links_shows_outgoing_and_backlinks() {
        let (_dir, vault) = test_vault();
        let note = vault.existing_note("code-map.md").expect("note");
        let out = obsidian_links_sync(&vault, &note).expect("links");
        assert_eq!(out["outgoing"].as_array().expect("out")[0], "architecture");
        assert!(out["backlinks"].as_array().expect("back").is_empty());
    }

    #[test]
    fn write_creates_and_reports_memory() {
        let (_dir, vault) = test_vault();
        let note = vault.writable_note("sub/new-note.md").expect("writable");
        let out = obsidian_write_sync(&vault, &note, "---\ntitle: New\n---\nBody\n").expect("write");
        assert_eq!(out["created"], true);
        assert_eq!(out["has_frontmatter"], true);
        let written = std::fs::read_to_string(vault.existing_note("sub/new-note.md").expect("exists")).expect("read back");
        assert!(written.contains("# New") || written.contains("title: New"));
    }

    #[test]
    fn write_rejects_empty_content() {
        let (_dir, vault) = test_vault();
        let note = vault.writable_note("empty.md").expect("writable");
        assert!(obsidian_write_sync(&vault, &note, "   ").is_err());
    }

    #[test]
    fn first_heading_extracts_h1() {
        assert_eq!(first_heading("# Title\n"), Some("Title".to_owned()));
        assert_eq!(first_heading("no heading\n"), None);
    }
}
