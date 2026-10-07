//! `fsread.tree` — `tree` in reinem Rust.
//!
//! Rendert einen Verzeichnisbaum (`├── `, `└── `, `│   `) und liefert dieselben
//! Einträge als JSON. Symlinks werden angezeigt (`name -> ziel`), nie betreten.
//! Begrenzt auf `limit` Einträge (Standard 500, hart 5000), Tiefe höchstens 16
//! und die Walk-Grenzen von [`crate::walk`]. Versteckte Einträge nur mit `all`.

use crate::blocking::{run_blocking, scoped, workspace_root};
use crate::budget::{MAX_TEXT_BYTES, clip_text, flag, limit_or, ok};
use crate::meta::Kind;
use crate::scope::io_message;
use crate::walk::{Flow, HARD_MAX_VISITED, StopReason, WalkOpts, walk};
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::Deserialize;
use serde_json::json;
use std::path::Path;

/// Name des Werkzeugs.
pub const TOOL: &str = "fsread.tree";

/// Standard-Tiefe.
pub const DEFAULT_DEPTH: usize = 3;

/// Größte Tiefe.
pub const MAX_DEPTH: usize = 16;

/// Standard-Limit der Einträge.
pub const DEFAULT_LIMIT: usize = 500;

/// Argumente für `fsread.tree`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct TreeArgs {
    /// Start directory relative to the workspace root (default: the root).
    #[serde(default)]
    pub path: Option<String>,
    /// -L: maximum depth (default 3, maximum 16).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub max_depth: Option<usize>,
    /// -d: list directories only.
    #[serde(default)]
    pub dirs_only: Option<bool>,
    /// -a: include entries starting with '.'.
    #[serde(default)]
    pub all: Option<bool>,
    /// Maximum number of entries (default 500, hard 5000).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub limit: Option<usize>,
}

struct Node {
    depth: usize,
    name: String,
    kind: Kind,
    target: Option<String>,
    path: String,
}

/// Rendert die Knoten (Vorordnung) als Baumtext.
fn render(root_label: &str, nodes: &[Node]) -> String {
    let max_depth = nodes.iter().map(|n| n.depth).max().unwrap_or(0);
    // `is_last[i]`: letztes Kind seines Elternteils (rückwärts bestimmt).
    let mut is_last = vec![false; nodes.len()];
    let mut seen = vec![false; max_depth + 2];
    for index in (0..nodes.len()).rev() {
        let depth = nodes[index].depth;
        is_last[index] = !seen[depth];
        seen[depth] = true;
        for deeper in seen.iter_mut().skip(depth + 1) {
            *deeper = false;
        }
    }
    let mut out = String::new();
    out.push_str(root_label);
    out.push('\n');
    // Für jede Ebene: ob der Vorfahre auf dieser Ebene der letzte war.
    let mut ancestors_last: Vec<bool> = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        ancestors_last.truncate(node.depth.saturating_sub(1));
        for last in &ancestors_last {
            out.push_str(if *last { "    " } else { "│   " });
        }
        out.push_str(if is_last[index] {
            "└── "
        } else {
            "├── "
        });
        out.push_str(&node.name);
        if let Some(target) = &node.target {
            out.push_str(" -> ");
            out.push_str(target);
        } else if node.kind == Kind::Dir {
            out.push('/');
        }
        out.push('\n');
        ancestors_last.push(is_last[index]);
    }
    out
}

/// Führt `fsread.tree` aus.
#[must_use]
pub fn run(root: &Path, args: &TreeArgs) -> ToolOutput {
    scoped(TOOL, root, |scope| {
        let depth = limit_or(args.max_depth, DEFAULT_DEPTH, MAX_DEPTH);
        let limit = limit_or(args.limit, DEFAULT_LIMIT, crate::budget::HARD_MAX_ITEMS);
        let dirs_only = flag(args.dirs_only);
        let all = flag(args.all);
        let input = args
            .path
            .as_deref()
            .filter(|p| !p.is_empty())
            .unwrap_or(".");
        let start = scope.rel(input).map_err(|e| e.to_string())?;

        let mut nodes: Vec<Node> = Vec::new();
        let mut capped = false;
        let report = walk(
            scope,
            &start,
            WalkOpts::bounded(depth, HARD_MAX_VISITED),
            |entry| {
                let hidden = entry.name.as_encoded_bytes().first() == Some(&b'.');
                if hidden && !all {
                    return Flow::SkipDir;
                }
                if dirs_only && entry.meta.kind != Kind::Dir {
                    return Flow::Continue;
                }
                if nodes.len() >= limit {
                    capped = true;
                    return Flow::Stop;
                }
                let target = (entry.meta.kind == Kind::Symlink)
                    .then(|| {
                        scope
                            .rel(&entry.rel.to_string_lossy())
                            .ok()
                            .and_then(|rel| scope.read_link(&rel).ok())
                            .map(|t| t.to_string_lossy().into_owned())
                    })
                    .flatten();
                nodes.push(Node {
                    depth: entry.depth,
                    name: entry.name.to_string_lossy().into_owned(),
                    kind: entry.meta.kind,
                    target,
                    path: entry.rel.to_string_lossy().into_owned(),
                });
                Flow::Continue
            },
        )
        .map_err(|e| format!("cannot read '{}': {}", start.display(), io_message(&e)))?;

        let dirs = nodes.iter().filter(|n| n.kind == Kind::Dir).count();
        let files = nodes.len() - dirs;
        let label = start.display();
        let text = render(&label, &nodes);
        let (text, text_clipped) = clip_text(&text, MAX_TEXT_BYTES);
        let walk_stop = match report.stopped {
            Some(StopReason::Caller) | None => None,
            Some(reason) => Some(reason.as_str()),
        };
        let stopped = if capped {
            Some("entry_limit")
        } else if text_clipped {
            Some("output_limit")
        } else {
            walk_stop
        };
        let truncated = capped || text_clipped || walk_stop.is_some();
        let entries: Vec<_> = nodes
            .iter()
            .take(2000)
            .map(|n| json!({"path": n.path, "depth": n.depth, "type": n.kind.name()}))
            .collect();
        Ok(ok(
            TOOL,
            format!(
                "{dirs} directories, {files} files under {label}{}",
                if truncated { " (truncated)" } else { "" }
            ),
            json!({
                "path": label,
                "tree": text,
                "entries": entries,
                "directories": dirs,
                "files": files,
                "truncated": truncated,
                "stopped": stopped,
                "depth_limited": report.depth_limited,
                "denied_dirs": report.denied_dirs,
            }),
        ))
    })
}

/// Zeigt einen Verzeichnisbaum wie `tree`.
#[harw_macros::tool(
    name = "fsread.tree",
    description = "Renders a directory tree of the workspace like tree: -L max_depth (default 3, max 16), -d dirs_only, -a all, bounded by limit (default 500, hard 5000). Use when you need the layout of a project instead of running tree or many ls calls. Returns JSON {tree: text, entries:[{path,depth,type}], directories, files, truncated}. Symlinks are shown as 'name -> target' and never entered; secret directories (.ssh, ...) are not descended.",
    permission = "read_workspace",
    parallel_safe
)]
async fn fsread_tree(
    context: &ToolExecutionContext,
    args: TreeArgs,
) -> Result<ToolOutput, ToolsError> {
    let root = workspace_root(context);
    run_blocking(TOOL, move || run(&root, &args)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, TestError, TestResult, error_of, json_of};
    use serde_json::Value;

    fn tree(fx: &Fixture, args: Value) -> TestResult<Value> {
        json_of(run(&fx.ws, &serde_json::from_value(args)?))
    }

    fn text(value: &Value) -> TestResult<&str> {
        value["tree"].as_str().ok_or(TestError::Missing("tree"))
    }

    #[test]
    fn renders_connectors_like_tree() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("a/b/c.txt", b"")?;
        fx.write("a/d.txt", b"")?;
        fx.write("e.txt", b"")?;
        let value = tree(&fx, json!({}))?;
        let expected = ".\n├── a/\n│   ├── b/\n│   │   └── c.txt\n│   └── d.txt\n└── e.txt\n";
        assert_eq!(text(&value)?, expected);
        assert_eq!(value["directories"], 2);
        assert_eq!(value["files"], 3);
        Ok(())
    }

    #[test]
    fn depth_dirs_only_hidden_and_limit() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("a/b/c.txt", b"")?;
        fx.write(".hidden/x", b"")?;
        fx.write("z.txt", b"")?;
        let shallow = tree(&fx, json!({"max_depth": 1}))?;
        assert_eq!(text(&shallow)?, ".\n├── a/\n└── z.txt\n");
        assert_eq!(shallow["depth_limited"], true);
        let dirs = tree(&fx, json!({"dirs_only": true}))?;
        assert_eq!(text(&dirs)?, ".\n└── a/\n    └── b/\n");
        let all = tree(&fx, json!({"all": true, "max_depth": 1}))?;
        assert!(text(&all)?.contains(".hidden/"));
        let limited = tree(&fx, json!({"limit": 1}))?;
        assert_eq!(limited["truncated"], true);
        assert_eq!(limited["stopped"], "entry_limit");
        Ok(())
    }

    #[test]
    fn symlinks_are_shown_not_entered() -> TestResult {
        let fx = Fixture::new()?;
        fx.plant_escapes()?;
        let value = tree(&fx, json!({}))?;
        let text = text(&value)?;
        assert!(text.contains("link_dir -> "), "{text}");
        // Der Linkziel-Text darf erscheinen, der Inhalt des Ziels nie.
        assert!(!text.contains("more.txt"), "{text}");
        assert!(!text.contains("deep"), "{text}");
        let paths: Vec<&str> = value["entries"]
            .as_array()
            .map(|e| e.iter().filter_map(|x| x["path"].as_str()).collect())
            .unwrap_or_default();
        assert!(
            paths
                .iter()
                .all(|p| !p.starts_with("link_dir/") && !p.starts_with("loop/")),
            "{paths:?}"
        );
        Ok(())
    }

    #[test]
    fn secret_directories_are_not_descended() -> TestResult {
        let fx = Fixture::new()?;
        fx.write(".ssh/id_ed25519", b"KEY")?;
        let value = tree(&fx, json!({"all": true}))?;
        let text = text(&value)?;
        assert!(text.contains(".ssh/"));
        assert!(!text.contains("id_ed25519"));
        assert_eq!(value["denied_dirs"], 1);
        Ok(())
    }

    #[test]
    fn escapes_and_bad_paths_rejected() -> TestResult {
        let fx = Fixture::new()?;
        fx.plant_escapes()?;
        fx.write("f", b"")?;
        for path in ["..", "link_dir", "f", "missing", "/etc"] {
            error_of(run(&fx.ws, &serde_json::from_value(json!({"path": path}))?))?;
        }
        Ok(())
    }

    #[test]
    fn huge_directory_is_bounded() -> TestResult {
        let fx = Fixture::new()?;
        for index in 0..800 {
            fx.write(&format!("d/f{index:04}"), b"")?;
        }
        let value = tree(&fx, json!({"limit": 50}))?;
        assert_eq!(value["truncated"], true);
        assert!(value["entries"].as_array().is_some_and(|e| e.len() <= 50));
        Ok(())
    }
}
