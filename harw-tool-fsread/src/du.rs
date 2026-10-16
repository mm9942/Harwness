//! `fsread.du` — `du` in reinem Rust.
//!
//! Summiert die Größe eines Verzeichnisbaums. Standard sind belegte Blöcke
//! (`st_blocks * 512`, wie `du`), mit `apparent_size` die scheinbare Größe
//! (`--apparent-size`, dabei zählen Verzeichniseinträge selbst 0 wie bei GNU `du`). Harte Links (`st_nlink > 1`) werden einmal gezählt.
//! `summarize` (`-s`) liefert nur die Gesamtsumme, `max_depth` (`-d`) begrenzt
//! die gemeldeten Ebenen, `all` (`-a`) meldet auch Dateien.
//!
//! Der Walk ist begrenzt (100 000 Einträge, 5 s, Tiefe 32); ist er nicht
//! vollständig, steht `complete: false` im Ergebnis und die Summe ist eine
//! untere Schranke.

use crate::blocking::{run_blocking, scoped, workspace_root};
use crate::budget::{Collector, flag, limit_or, ok};
use crate::meta::{Kind, human_size};
use crate::scope::io_message;
use crate::walk::{Flow, HARD_MAX_DEPTH, HARD_MAX_VISITED, WalkOpts, walk};
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::Deserialize;
use serde_json::json;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// Name des Werkzeugs.
pub const TOOL: &str = "fsread.du";

/// Standard-Limit gemeldeter Zeilen.
pub const DEFAULT_LIMIT: usize = 200;

/// Argumente für `fsread.du`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct DuArgs {
    /// Directory (or file) relative to the workspace root (default: the root).
    #[serde(default)]
    pub path: Option<String>,
    /// -s / --summarize: report only the total.
    #[serde(default)]
    pub summarize: Option<bool>,
    /// --apparent-size: count file lengths instead of allocated disk blocks.
    #[serde(default)]
    pub apparent_size: Option<bool>,
    /// -h: add human-readable sizes (1024-based).
    #[serde(default)]
    pub human: Option<bool>,
    /// -a / --all: report files as well as directories.
    #[serde(default)]
    pub all: Option<bool>,
    /// -d / --max-depth: only report entries up to this depth (0 = only the total). Default 1.
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub max_depth: Option<usize>,
    /// Maximum number of reported lines (default 200, hard 5000).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub limit: Option<usize>,
}

/// Führt `fsread.du` aus.
#[must_use]
pub fn run(root: &Path, args: &DuArgs) -> ToolOutput {
    scoped(TOOL, root, |scope| {
        let apparent = flag(args.apparent_size);
        let human = flag(args.human);
        let all = flag(args.all);
        let summarize = flag(args.summarize);
        let report_depth = if summarize {
            0
        } else {
            args.max_depth.unwrap_or(1).min(HARD_MAX_DEPTH)
        };
        let limit = limit_or(args.limit, DEFAULT_LIMIT, crate::budget::HARD_MAX_ITEMS);
        let input = args
            .path
            .as_deref()
            .filter(|p| !p.is_empty())
            .unwrap_or(".");
        let start = scope.rel(input).map_err(|e| e.to_string())?;
        let own = scope
            .lstat(&start)
            .map_err(|e| format!("cannot access '{}': {}", start.display(), io_message(&e)))?;
        let own_meta = crate::meta::Meta::from_stat(&own);
        // Wie GNU `du --apparent-size`: Verzeichnisse selbst zählen dort nicht mit
        // (nur Dateigrößen); ohne die Option zählen belegte Blöcke inkl. Verzeichnisse.
        let size_of = |meta: &crate::meta::Meta| {
            if !apparent {
                meta.disk_bytes()
            } else if meta.kind == Kind::Dir {
                0
            } else {
                meta.size
            }
        };

        if own_meta.kind != Kind::Dir {
            let bytes = size_of(&own_meta);
            let mut line = json!({"path": start.display(), "bytes": bytes});
            if human {
                line["human"] = json!(human_size(bytes));
            }
            return Ok(ok(
                TOOL,
                format!("{} {}", human_size(bytes), start.display()),
                json!({"entries": [line], "total_bytes": bytes, "complete": true, "truncated": false}),
            ));
        }

        // Summen je Verzeichnis (relativ zum Start; "" = Start selbst).
        let mut dir_totals: BTreeMap<PathBuf, u64> = BTreeMap::new();
        let mut file_sizes: BTreeMap<PathBuf, u64> = BTreeMap::new();
        let start_total_init = size_of(&own_meta);
        dir_totals.insert(PathBuf::new(), start_total_init);
        let mut seen_links: HashSet<(u64, u64)> = HashSet::new();
        let start_path = start.as_path().to_path_buf();
        let report = walk(
            scope,
            &start,
            WalkOpts::bounded(HARD_MAX_DEPTH, HARD_MAX_VISITED),
            |entry| {
                let meta = entry.meta;
                if meta.kind != Kind::Dir
                    && meta.nlink > 1
                    && !seen_links.insert((meta.dev, meta.ino))
                {
                    return Flow::Continue;
                }
                let size = size_of(meta);
                let local = entry
                    .rel
                    .strip_prefix(&start_path)
                    .unwrap_or(entry.rel)
                    .to_path_buf();
                if meta.kind == Kind::Dir {
                    dir_totals.entry(local.clone()).or_insert(0);
                    *dir_totals.entry(local.clone()).or_insert(0) += size;
                } else if all {
                    file_sizes.insert(local.clone(), size);
                }
                // Auf alle Vorfahren (inkl. Start) aufaddieren.
                let mut ancestor = local.parent().map(Path::to_path_buf);
                while let Some(dir) = ancestor {
                    *dir_totals.entry(dir.clone()).or_insert(0) += size;
                    ancestor = if dir.as_os_str().is_empty() {
                        None
                    } else {
                        dir.parent().map(Path::to_path_buf)
                    };
                }
                Flow::Continue
            },
        )
        .map_err(|e| format!("cannot read '{}': {}", start.display(), io_message(&e)))?;

        let total = dir_totals.get(Path::new("")).copied().unwrap_or(0);
        let mut lines: Vec<(PathBuf, u64)> = Vec::new();
        for (path, bytes) in &dir_totals {
            if path.components().count() <= report_depth {
                lines.push((path.clone(), *bytes));
            }
        }
        if all && !summarize {
            for (path, bytes) in &file_sizes {
                if path.components().count() <= report_depth {
                    lines.push((path.clone(), *bytes));
                }
            }
        }
        lines.sort_by(|a, b| a.0.cmp(&b.0));
        let mut out = Collector::new(limit);
        for (path, bytes) in lines {
            let shown = start.as_path().join(&path);
            let display = if shown.as_os_str().is_empty() {
                ".".to_owned()
            } else {
                shown.to_string_lossy().into_owned()
            };
            let mut line = json!({"path": display, "bytes": bytes});
            if human {
                line["human"] = json!(human_size(bytes));
            }
            if !out.push(line) {
                break;
            }
        }
        let complete = report.stopped.is_none() && !report.depth_limited && report.errors == 0;
        let truncated = out.truncated() || report.stopped.is_some();
        let stopped = report
            .stopped
            .map(|reason| reason.as_str())
            .or(out.stop_reason());
        let count = out.len();
        Ok(ok(
            TOOL,
            format!(
                "{} {}{}",
                human_size(total),
                start.display(),
                if complete {
                    ""
                } else {
                    " (incomplete: lower bound)"
                }
            ),
            json!({
                "path": start.display(),
                "entries": out.into_items(),
                "count": count,
                "total_bytes": total,
                "total_human": human_size(total),
                "mode": if apparent { "apparent" } else { "disk_blocks" },
                "complete": complete,
                "truncated": truncated,
                "stopped": stopped,
                "denied_dirs": report.denied_dirs,
                "unreadable_entries": report.errors,
                "scanned": report.visited,
            }),
        ))
    })
}

/// Summiert Größen wie `du`.
#[harw_macros::tool(
    name = "fsread.du",
    description = "Sums disk usage of a workspace directory like du: allocated blocks (default) or --apparent-size, -s, -d/max_depth, -a, -h; hard links are counted once. Use when you need to know what takes space instead of running du. Returns JSON {entries:[{path,bytes}], total_bytes, complete, truncated}; the walk is bounded (100000 entries, 5 s) and complete=false means the total is a lower bound.",
    permission = "read_workspace",
    parallel_safe
)]
async fn fsread_du(context: &ToolExecutionContext, args: DuArgs) -> Result<ToolOutput, ToolsError> {
    let root = workspace_root(context);
    run_blocking(TOOL, move || run(&root, &args)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, TestError, TestResult, error_of, json_of};
    use serde_json::Value;

    fn du(fx: &Fixture, args: Value) -> TestResult<Value> {
        json_of(run(&fx.ws, &serde_json::from_value(args)?))
    }

    fn line<'a>(value: &'a Value, path: &str) -> TestResult<&'a Value> {
        value["entries"]
            .as_array()
            .ok_or(TestError::Missing("entries"))?
            .iter()
            .find(|e| e["path"] == path)
            .ok_or_else(|| TestError::Unexpected(format!("no line for {path}: {value}")))
    }

    #[test]
    fn apparent_sizes_add_up_per_directory() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("a/one", &[0u8; 100])?;
        fx.write("a/b/two", &[0u8; 250])?;
        fx.write("c", &[0u8; 7])?;
        let value = du(&fx, json!({"apparent_size": true, "max_depth": 5}))?;
        // Wie GNU `du --apparent-size`: nur Dateigrößen, Verzeichnisse zählen 0.
        assert_eq!(line(&value, "a/b")?["bytes"], 250);
        assert_eq!(line(&value, "a")?["bytes"], 100 + 250);
        assert_eq!(value["total_bytes"], 100 + 250 + 7);
        assert_eq!(value["complete"], true);
        Ok(())
    }

    #[test]
    fn summarize_all_and_depth() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("a/one", &[0u8; 10])?;
        fx.write("a/b/two", &[0u8; 20])?;
        let summary = du(&fx, json!({"summarize": true, "apparent_size": true}))?;
        assert_eq!(summary["count"], 1);
        assert_eq!(summary["entries"][0]["path"], ".");
        let depth0 = du(&fx, json!({"max_depth": 0, "apparent_size": true}))?;
        assert_eq!(depth0["count"], 1);
        let with_files = du(
            &fx,
            json!({"all": true, "max_depth": 3, "apparent_size": true}),
        )?;
        assert_eq!(line(&with_files, "a/one")?["bytes"], 10);
        assert_eq!(line(&with_files, "a/b/two")?["bytes"], 20);
        let no_files = du(&fx, json!({"max_depth": 3, "apparent_size": true}))?;
        assert!(
            no_files["entries"]
                .as_array()
                .is_some_and(|e| e.iter().all(|x| x["path"] != "a/one"))
        );
        Ok(())
    }

    #[test]
    fn hard_links_are_counted_once() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("x", &[0u8; 1000])?;
        std::fs::hard_link(fx.ws.join("x"), fx.ws.join("y"))?;
        let single = du(&fx, json!({"summarize": true, "apparent_size": true}))?;
        assert_eq!(single["total_bytes"], 1000);
        Ok(())
    }

    #[test]
    fn single_file_path_and_errors() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("f", &[0u8; 33])?;
        let value = du(
            &fx,
            json!({"path": "f", "apparent_size": true, "human": true}),
        )?;
        assert_eq!(value["total_bytes"], 33);
        error_of(run(&fx.ws, &serde_json::from_value(json!({"path": ".."}))?))?;
        error_of(run(
            &fx.ws,
            &serde_json::from_value(json!({"path": "missing"}))?,
        ))?;
        Ok(())
    }

    #[test]
    fn symlinks_are_not_followed_out_of_the_workspace() -> TestResult {
        use std::os::unix::fs::MetadataExt;
        let fx = Fixture::new()?;
        fx.plant_escapes()?;
        let value = du(&fx, json!({"summarize": true, "apparent_size": true}))?;
        // Erwartet: nur die Größen der Links selbst (Verzeichnisse zählen 0); die
        // Geheimnisse in `outside/` und die Schleifenziele zählen nie mit.
        let mut expected = 0u64;
        for path in ["link_dir", "link_file", "loop", "nested/up"] {
            expected += std::fs::symlink_metadata(fx.ws.join(path))?.size();
        }
        assert_eq!(value["total_bytes"], expected);
        assert_eq!(value["complete"], true);
        Ok(())
    }
}
