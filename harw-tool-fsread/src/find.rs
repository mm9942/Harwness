//! `fsread.find` — `find` in reinem Rust.
//!
//! Sucht unterhalb eines Startpfads nach Name (Glob), Pfad (Glob), Typ,
//! Größe, Änderungszeit und Tiefe. Entspricht `find -name/-iname/-path/-type/
//! -size/-mtime/-mindepth/-maxdepth/-empty`. Symlinks werden gemeldet, nie
//! betreten (`find -L` gibt es bewusst nicht). Die Ausgabe ist nach Pfad
//! sortiert (Vorordnung des Walks) und begrenzt.

use crate::blocking::{run_blocking, scoped, workspace_root};
use crate::budget::{Collector, choice, flag, limit_or, ok};
use crate::meta::Kind;
use crate::scope::io_message;
use crate::walk::{Flow, HARD_MAX_DEPTH, HARD_MAX_VISITED, StopReason, WalkOpts, walk};
use globset::{GlobBuilder, GlobMatcher};
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::Deserialize;
use serde_json::json;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Name des Werkzeugs.
pub const TOOL: &str = "fsread.find";

/// Standard-Limit der Treffer.
pub const DEFAULT_LIMIT: usize = 200;

/// Höchstlänge eines Glob-Musters.
pub const MAX_PATTERN_BYTES: usize = 512;

/// Argumente für `fsread.find`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct FindArgs {
    /// Start directory relative to the workspace root (default: the root).
    #[serde(default)]
    pub path: Option<String>,
    /// -name: glob matched against the file name, e.g. '*.rs'.
    #[serde(default)]
    pub name: Option<String>,
    /// -iname: like name but case-insensitive.
    #[serde(default)]
    pub iname: Option<String>,
    /// -path: glob matched against the whole path relative to the workspace root; '*' also matches '/'.
    #[serde(default)]
    pub path_glob: Option<String>,
    /// -type: 'f' (file), 'd' (directory), 'l' (symlink) or 'o' (other); the argument is named kind.
    #[serde(default)]
    pub kind: Option<String>,
    /// Only entries at least this many bytes (-size +N, files and symlinks only).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
    pub min_size: Option<u64>,
    /// Only entries at most this many bytes (-size -N).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
    pub max_size: Option<u64>,
    /// Only entries modified within the last N seconds (like -mmin, in seconds).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
    pub modified_within_secs: Option<u64>,
    /// Only entries modified more than N seconds ago.
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
    pub modified_before_secs: Option<u64>,
    /// -mindepth (default 0: the start directory's children are depth 1).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub min_depth: Option<usize>,
    /// -maxdepth (default and maximum 32).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub max_depth: Option<usize>,
    /// -empty: only empty files and empty directories.
    #[serde(default)]
    pub empty: Option<bool>,
    /// Maximum number of matches (default 200, hard 5000).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub limit: Option<usize>,
}

fn glob(
    pattern: &str,
    case_insensitive: bool,
    literal_separator: bool,
) -> Result<GlobMatcher, String> {
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(format!("pattern longer than {MAX_PATTERN_BYTES} bytes"));
    }
    GlobBuilder::new(pattern)
        .case_insensitive(case_insensitive)
        .literal_separator(literal_separator)
        .build()
        .map(|glob| glob.compile_matcher())
        .map_err(|error| {
            format!(
                "invalid glob '{}': {error}",
                pattern.chars().take(64).collect::<String>()
            )
        })
}

/// Führt `fsread.find` aus.
#[must_use]
pub fn run(root: &Path, args: &FindArgs) -> ToolOutput {
    scoped(TOOL, root, |scope| {
        let kind = args
            .kind
            .as_deref()
            .map(|value| choice("kind", Some(value), &["f", "d", "l", "o"], "f"))
            .transpose()?;
        let name = args
            .name
            .as_deref()
            .map(|p| glob(p, false, false))
            .transpose()?;
        let iname = args
            .iname
            .as_deref()
            .map(|p| glob(p, true, false))
            .transpose()?;
        let path_glob = args
            .path_glob
            .as_deref()
            .map(|p| glob(p, false, false))
            .transpose()?;
        if let (Some(min), Some(max)) = (args.min_size, args.max_size) {
            if min > max {
                return Err(format!("min_size {min} is larger than max_size {max}"));
            }
        }
        let max_depth = limit_or(
            args.max_depth.map(|d| d.max(1)),
            HARD_MAX_DEPTH,
            HARD_MAX_DEPTH,
        );
        let min_depth = args.min_depth.unwrap_or(0);
        if min_depth > max_depth {
            return Err(format!(
                "min_depth {min_depth} exceeds max_depth {max_depth}"
            ));
        }
        let limit = limit_or(args.limit, DEFAULT_LIMIT, crate::budget::HARD_MAX_ITEMS);
        let want_empty = flag(args.empty);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
            .unwrap_or(0);

        let input = args
            .path
            .as_deref()
            .filter(|p| !p.is_empty())
            .unwrap_or(".");
        let start = scope.rel(input).map_err(|e| e.to_string())?;
        let mut out = Collector::new(limit);
        let mut visited_total = 0usize;
        let report = walk(
            scope,
            &start,
            WalkOpts::bounded(max_depth, HARD_MAX_VISITED),
            |entry| {
                visited_total += 1;
                let meta = entry.meta;
                if entry.depth < min_depth {
                    return Flow::Continue;
                }
                let type_ok = kind.is_none_or(|k| match k {
                    "f" => meta.kind == Kind::File,
                    "d" => meta.kind == Kind::Dir,
                    "l" => meta.kind == Kind::Symlink,
                    _ => !matches!(meta.kind, Kind::File | Kind::Dir | Kind::Symlink),
                });
                if !type_ok {
                    return Flow::Continue;
                }
                let file_name = entry.name.to_string_lossy();
                if name
                    .as_ref()
                    .is_some_and(|m| !m.is_match(file_name.as_ref()))
                {
                    return Flow::Continue;
                }
                if iname
                    .as_ref()
                    .is_some_and(|m| !m.is_match(file_name.as_ref()))
                {
                    return Flow::Continue;
                }
                if path_glob.as_ref().is_some_and(|m| !m.is_match(entry.rel)) {
                    return Flow::Continue;
                }
                if args.min_size.is_some_and(|min| meta.size < min)
                    || args.max_size.is_some_and(|max| meta.size > max)
                {
                    return Flow::Continue;
                }
                let age = now.saturating_sub(meta.mtime.secs);
                if args
                    .modified_within_secs
                    .is_some_and(|secs| age > i64::try_from(secs).unwrap_or(i64::MAX))
                {
                    return Flow::Continue;
                }
                if args
                    .modified_before_secs
                    .is_some_and(|secs| age <= i64::try_from(secs).unwrap_or(i64::MAX))
                {
                    return Flow::Continue;
                }
                if want_empty {
                    let is_empty = match meta.kind {
                        Kind::File => meta.size == 0,
                        Kind::Dir => scope
                            .rel(&entry.rel.to_string_lossy())
                            .ok()
                            .and_then(|rel| scope.open_dir(&rel).ok())
                            .and_then(|fd| {
                                crate::walk::smallest_names(
                                    &fd,
                                    1,
                                    std::time::Instant::now() + std::time::Duration::from_secs(1),
                                )
                                .ok()
                                .flatten()
                            })
                            .is_some_and(|names| names.is_empty()),
                        _ => false,
                    };
                    if !is_empty {
                        return Flow::Continue;
                    }
                }
                let value = json!({
                    "path": entry.rel.to_string_lossy(),
                    "type": meta.kind.name(),
                    "size": meta.size,
                    "mtime": meta.mtime.iso(),
                });
                if out.push(value) {
                    Flow::Continue
                } else {
                    Flow::Stop
                }
            },
        )
        .map_err(|e| format!("cannot search '{}': {}", start.display(), io_message(&e)))?;

        let walk_stop = match report.stopped {
            Some(StopReason::Caller) | None => None,
            Some(reason) => Some(reason.as_str()),
        };
        let stopped = out.stop_reason().or(walk_stop);
        let truncated = out.truncated() || walk_stop.is_some();
        let count = out.len();
        Ok(ok(
            TOOL,
            format!(
                "{count} matches under {} ({visited_total} entries scanned){}",
                start.display(),
                if truncated { ", truncated" } else { "" }
            ),
            json!({
                "start": start.display(),
                "matches": out.into_items(),
                "count": count,
                "scanned": visited_total,
                "truncated": truncated,
                "stopped": stopped,
                "depth_limited": report.depth_limited,
                "denied_dirs": report.denied_dirs,
                "unreadable_entries": report.errors,
            }),
        ))
    })
}

/// Sucht Dateien und Verzeichnisse wie `find`.
#[harw_macros::tool(
    name = "fsread.find",
    description = "Finds files and directories under a workspace path like find: -name/-iname/-path globs, kind (-type) f|d|l|o, size range, modification age, -mindepth/-maxdepth, -empty. Use when you need to locate entries by name, type, size or age instead of running find. Returns JSON {matches:[{path,type,size,mtime}], count, truncated, stopped} sorted by path, bounded (default 200, hard 5000, 100000 entries scanned, 5 s). Never follows symlinks.",
    permission = "read_workspace",
    parallel_safe
)]
async fn fsread_find(
    context: &ToolExecutionContext,
    args: FindArgs,
) -> Result<ToolOutput, ToolsError> {
    let root = workspace_root(context);
    run_blocking(TOOL, move || run(&root, &args)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, TestError, TestResult, error_of, json_of};
    use serde_json::Value;

    fn find(fx: &Fixture, args: Value) -> TestResult<Value> {
        json_of(run(&fx.ws, &serde_json::from_value(args)?))
    }

    fn paths(value: &Value) -> TestResult<Vec<String>> {
        Ok(value["matches"]
            .as_array()
            .ok_or(TestError::Missing("matches"))?
            .iter()
            .filter_map(|m| m["path"].as_str().map(str::to_owned))
            .collect())
    }

    fn tree() -> TestResult<Fixture> {
        let fx = Fixture::new()?;
        fx.write("src/main.rs", b"fn main() {}")?;
        fx.write("src/lib.RS", b"")?;
        fx.write("src/deep/mod.rs", b"mod")?;
        fx.write("README.md", b"# readme")?;
        fx.write("empty.txt", b"")?;
        std::fs::create_dir_all(fx.ws.join("emptydir"))?;
        Ok(fx)
    }

    #[test]
    fn name_iname_type_and_path_glob() -> TestResult {
        let fx = tree()?;
        assert_eq!(
            paths(&find(&fx, json!({"name": "*.rs"}))?)?,
            vec!["src/deep/mod.rs", "src/main.rs"]
        );
        assert_eq!(
            paths(&find(&fx, json!({"iname": "*.rs"}))?)?,
            vec!["src/deep/mod.rs", "src/lib.RS", "src/main.rs"]
        );
        assert_eq!(
            paths(&find(&fx, json!({"kind": "d"}))?)?,
            vec!["emptydir", "src", "src/deep"]
        );
        assert_eq!(
            paths(&find(&fx, json!({"path_glob": "src/*/mod.rs"}))?)?,
            vec!["src/deep/mod.rs"]
        );
        Ok(())
    }

    #[test]
    fn size_depth_and_empty_filters() -> TestResult {
        let fx = tree()?;
        assert_eq!(
            paths(&find(&fx, json!({"kind": "f", "min_size": 5}))?)?,
            vec!["README.md", "src/main.rs"]
        );
        assert_eq!(
            paths(&find(&fx, json!({"kind": "f", "max_size": 0}))?)?,
            vec!["empty.txt", "src/lib.RS"]
        );
        assert_eq!(
            paths(&find(&fx, json!({"max_depth": 1, "kind": "f"}))?)?,
            vec!["README.md", "empty.txt"]
        );
        assert_eq!(
            paths(&find(&fx, json!({"min_depth": 3, "kind": "f"}))?)?,
            vec!["src/deep/mod.rs"]
        );
        assert_eq!(
            paths(&find(&fx, json!({"empty": true}))?)?,
            vec!["empty.txt", "emptydir", "src/lib.RS"]
        );
        Ok(())
    }

    #[test]
    fn mtime_filters() -> TestResult {
        let fx = tree()?;
        let old = std::fs::File::options()
            .write(true)
            .open(fx.ws.join("README.md"))?;
        old.set_modified(UNIX_EPOCH + std::time::Duration::from_secs(1_000_000))?;
        assert_eq!(
            paths(&find(
                &fx,
                json!({"kind": "f", "modified_before_secs": 3600})
            )?)?,
            vec!["README.md"]
        );
        let recent = paths(&find(
            &fx,
            json!({"kind": "f", "modified_within_secs": 3600}),
        )?)?;
        assert!(!recent.contains(&"README.md".to_owned()));
        assert!(recent.contains(&"src/main.rs".to_owned()));
        Ok(())
    }

    #[test]
    fn limit_and_start_path() -> TestResult {
        let fx = tree()?;
        let value = find(&fx, json!({"limit": 1}))?;
        assert_eq!(value["count"], 1);
        assert_eq!(value["truncated"], true);
        assert_eq!(
            paths(&find(&fx, json!({"path": "src", "kind": "f"}))?)?,
            vec!["src/deep/mod.rs", "src/lib.RS", "src/main.rs"]
        );
        Ok(())
    }

    #[test]
    fn never_follows_symlinks_and_rejects_escapes() -> TestResult {
        let fx = Fixture::new()?;
        fx.plant_escapes()?;
        let value = find(&fx, json!({"name": "*.txt"}))?;
        assert!(paths(&value)?.is_empty(), "{value}");
        let links = paths(&find(&fx, json!({"kind": "l"}))?)?;
        assert_eq!(links, vec!["link_dir", "link_file", "loop", "nested/up"]);
        for bad in [
            json!({"path": ".."}),
            json!({"path": "link_dir"}),
            json!({"path": "/etc"}),
        ] {
            error_of(run(&fx.ws, &serde_json::from_value(bad)?))?;
        }
        Ok(())
    }

    #[test]
    fn invalid_arguments_are_rejected() -> TestResult {
        let fx = tree()?;
        for bad in [
            json!({"kind": "x"}),
            json!({"name": "[unclosed"}),
            json!({"min_size": 10, "max_size": 1}),
            json!({"min_depth": 5, "max_depth": 2}),
            json!({"name": "a".repeat(MAX_PATTERN_BYTES + 1)}),
        ] {
            error_of(run(&fx.ws, &serde_json::from_value(bad.clone())?))
                .map_err(|e| TestError::Unexpected(format!("{bad}: {e}")))?;
        }
        let parsed: Result<FindArgs, _> = serde_json::from_value(json!({"follow": true}));
        assert!(parsed.is_err(), "find -L must not exist");
        Ok(())
    }
}
