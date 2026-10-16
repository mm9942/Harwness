//! `fsread.diff` — `diff -u` zweier Workspace-Dateien (eigene Myers-Implementierung).
//!
//! `context` (`-U`), `ignore_whitespace` (`-w`), `ignore_case` (`-i`),
//! `brief` (`-q`). Dateien bis 1 MiB und 20 000 Zeilen; Binärdateien werden
//! nur auf Gleichheit verglichen. Übersteigt die Editdistanz
//! [`crate::textdiff::DEFAULT_MAX_D`], meldet das Werkzeug `differ: true` mit
//! `diff_omitted` statt einen riesigen Diff zu bauen.

use crate::blocking::{run_blocking, scoped, workspace_root};
use crate::budget::{MAX_TEXT_BYTES, clip_text, flag, ok};
use crate::io::{looks_binary, open_text, read_prefix};
use crate::textdiff::{DEFAULT_MAX_D, diff_lines, split_lines, unified};
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::Deserialize;
use serde_json::json;
use std::path::Path;

/// Name des Werkzeugs.
pub const TOOL: &str = "fsread.diff";

/// Höchstgröße je Datei.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// Höchstzahl Zeilen je Datei.
pub const MAX_LINES: usize = 20_000;

/// Größter Kontext.
pub const MAX_CONTEXT: usize = 50;

/// Argumente für `fsread.diff`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct DiffArgs {
    /// Left file (old), relative to the workspace root.
    pub a: String,
    /// Right file (new), relative to the workspace root.
    pub b: String,
    /// -U / --unified: lines of context (default 3, maximum 50).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub context: Option<usize>,
    /// -w / --ignore-all-space: ignore all whitespace when comparing.
    #[serde(default)]
    pub ignore_whitespace: Option<bool>,
    /// -i / --ignore-case: ignore case when comparing.
    #[serde(default)]
    pub ignore_case: Option<bool>,
    /// -q / --brief: only report whether the files differ.
    #[serde(default)]
    pub brief: Option<bool>,
}

fn key(line: &str, ignore_ws: bool, ignore_case: bool) -> String {
    let mut out: String = if ignore_ws {
        line.chars().filter(|c| !c.is_whitespace()).collect()
    } else {
        line.to_owned()
    };
    if ignore_case {
        out = out.to_lowercase();
    }
    out
}

/// Führt `fsread.diff` aus.
#[must_use]
pub fn run(root: &Path, args: &DiffArgs) -> ToolOutput {
    scoped(TOOL, root, |scope| {
        let context = args.context.unwrap_or(3).min(MAX_CONTEXT);
        let ignore_ws = flag(args.ignore_whitespace);
        let ignore_case = flag(args.ignore_case);
        let brief = flag(args.brief);
        let (a_rel, mut a_file) = open_text(scope, &args.a)?;
        let (b_rel, mut b_file) = open_text(scope, &args.b)?;
        let (a_bytes, a_more) =
            read_prefix(&mut a_file, MAX_FILE_BYTES).map_err(|e| e.to_string())?;
        let (b_bytes, b_more) =
            read_prefix(&mut b_file, MAX_FILE_BYTES).map_err(|e| e.to_string())?;
        if a_more || b_more {
            return Err(format!("files must be at most {MAX_FILE_BYTES} bytes each"));
        }
        let (a_name, b_name) = (a_rel.display(), b_rel.display());
        let base = |differ: bool| {
            json!({
                "a": a_name, "b": b_name, "differ": differ, "identical": !differ,
                "truncated": false,
            })
        };
        if looks_binary(&a_bytes) || looks_binary(&b_bytes) {
            let differ = a_bytes != b_bytes;
            let mut data = base(differ);
            data["binary"] = json!(true);
            let summary = if differ {
                "binary files differ"
            } else {
                "files are identical"
            };
            return Ok(ok(TOOL, summary, data));
        }
        let a_text = String::from_utf8_lossy(&a_bytes).into_owned();
        let b_text = String::from_utf8_lossy(&b_bytes).into_owned();
        let a_lines = split_lines(&a_text);
        let b_lines = split_lines(&b_text);
        if a_lines.len() > MAX_LINES || b_lines.len() > MAX_LINES {
            return Err(format!("files must have at most {MAX_LINES} lines each"));
        }
        let a_keys: Vec<String> = a_lines
            .iter()
            .map(|l| key(l, ignore_ws, ignore_case))
            .collect();
        let b_keys: Vec<String> = b_lines
            .iter()
            .map(|l| key(l, ignore_ws, ignore_case))
            .collect();
        let Some(ops) = diff_lines(&a_keys, &b_keys, DEFAULT_MAX_D) else {
            let mut data = base(true);
            data["diff_omitted"] =
                json!(format!("files differ by more than {DEFAULT_MAX_D} lines"));
            return Ok(ok(TOOL, "files differ (diff too large to render)", data));
        };
        let (text, stats) = unified(
            &format!("a/{a_name}"),
            &format!("b/{b_name}"),
            &a_lines,
            &b_lines,
            &ops,
            context,
        );
        let differ = stats.additions + stats.deletions > 0;
        let mut data = base(differ);
        data["additions"] = json!(stats.additions);
        data["deletions"] = json!(stats.deletions);
        data["hunks"] = json!(stats.hunks);
        let summary = if differ {
            format!(
                "+{} -{} in {} hunks",
                stats.additions, stats.deletions, stats.hunks
            )
        } else {
            "files are identical".to_owned()
        };
        if !brief && differ {
            let (clipped, was_clipped) = clip_text(&text, MAX_TEXT_BYTES);
            data["diff"] = json!(clipped);
            data["truncated"] = json!(was_clipped);
        }
        Ok(ok(TOOL, summary, data))
    })
}

/// Vergleicht zwei Dateien wie `diff -u`.
#[harw_macros::tool(
    name = "fsread.diff",
    description = "Compares two workspace text files and returns a unified diff like diff -u: -U/context (default 3), -w ignore_whitespace, -i ignore_case, -q brief. Use when you need to see how two files differ instead of running diff. Returns JSON {differ, additions, deletions, hunks, diff}; files up to 1 MiB / 20000 lines, binary files only compared for equality, diff text capped at 48 KiB.",
    permission = "read_workspace",
    parallel_safe
)]
async fn fsread_diff(
    context: &ToolExecutionContext,
    args: DiffArgs,
) -> Result<ToolOutput, ToolsError> {
    let root = workspace_root(context);
    run_blocking(TOOL, move || run(&root, &args)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, TestResult, error_of, json_of};
    use serde_json::Value;

    fn diff(fx: &Fixture, args: Value) -> TestResult<Value> {
        json_of(run(&fx.ws, &serde_json::from_value(args)?))
    }

    #[test]
    fn unified_diff_of_two_files() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("old.txt", b"one\ntwo\nthree\n")?;
        fx.write("new.txt", b"one\n2\nthree\nfour\n")?;
        let value = diff(&fx, json!({"a": "old.txt", "b": "new.txt", "context": 1}))?;
        assert_eq!(value["differ"], true);
        assert_eq!(value["additions"], 2);
        assert_eq!(value["deletions"], 1);
        assert_eq!(
            value["diff"],
            "--- a/old.txt\n+++ b/new.txt\n@@ -1,3 +1,4 @@\n one\n-two\n+2\n three\n+four\n"
        );
        Ok(())
    }

    #[test]
    fn identical_ignore_whitespace_and_case() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("a", b"Hello  World\nx\n")?;
        fx.write("b", b"hello world\nx\n")?;
        assert_eq!(diff(&fx, json!({"a": "a", "b": "a"}))?["identical"], true);
        assert_eq!(diff(&fx, json!({"a": "a", "b": "b"}))?["differ"], true);
        assert_eq!(
            diff(
                &fx,
                json!({"a": "a", "b": "b", "ignore_whitespace": true, "ignore_case": true})
            )?["identical"],
            true
        );
        assert_eq!(
            diff(&fx, json!({"a": "a", "b": "b", "ignore_whitespace": true}))?["differ"],
            true
        );
        let brief = diff(&fx, json!({"a": "a", "b": "b", "brief": true}))?;
        assert_eq!(brief["differ"], true);
        assert!(brief["diff"].is_null());
        Ok(())
    }

    #[test]
    fn binary_files_compare_by_content() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("x", &[0, 1, 2])?;
        fx.write("y", &[0, 1, 3])?;
        fx.write("z", &[0, 1, 2])?;
        let value = diff(&fx, json!({"a": "x", "b": "y"}))?;
        assert_eq!(
            (value["binary"].clone(), value["differ"].clone()),
            (json!(true), json!(true))
        );
        assert_eq!(diff(&fx, json!({"a": "x", "b": "z"}))?["identical"], true);
        Ok(())
    }

    #[test]
    fn huge_edit_distance_is_omitted_not_exploded() -> TestResult {
        let fx = Fixture::new()?;
        let a: String = (0..4000).map(|i| format!("a{i}\n")).collect();
        let b: String = (0..4000).map(|i| format!("b{i}\n")).collect();
        fx.write("a", a.as_bytes())?;
        fx.write("b", b.as_bytes())?;
        let value = diff(&fx, json!({"a": "a", "b": "b"}))?;
        assert_eq!(value["differ"], true);
        assert!(value["diff_omitted"].is_string());
        Ok(())
    }

    #[test]
    fn limits_escapes_and_secrets_are_enforced() -> TestResult {
        let fx = Fixture::new()?;
        fx.plant_escapes()?;
        fx.write("big", &vec![b'x'; 1024 * 1024 + 1])?;
        fx.write("ok", b"a\n")?;
        fx.write(".env", b"S=1")?;
        let lines: String = "x\n".repeat(MAX_LINES + 1);
        fx.write("lines", lines.as_bytes())?;
        for (a, b) in [
            ("big", "ok"),
            ("ok", "../outside/secret.txt"),
            ("ok", "link_file"),
            ("ok", ".env"),
            ("lines", "ok"),
            ("ok", "missing"),
        ] {
            error_of(run(
                &fx.ws,
                &serde_json::from_value(json!({"a": a, "b": b}))?,
            ))?;
        }
        Ok(())
    }
}
