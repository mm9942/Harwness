//! `sys.which` — `which` in reinem Rust.
//!
//! Sucht Befehlsnamen im `PATH` des Harness-Prozesses. Ein Treffer ist eine
//! reguläre Datei (Symlinks werden wie bei `which` aufgelöst) mit
//! Ausführrecht für den aktuellen Benutzer (`access(X_OK)`). Relative und
//! leere `PATH`-Einträge werden übersprungen (kein Treffer im
//! Arbeitsverzeichnis). Namen mit `/` sind nicht erlaubt: das Werkzeug
//! beantwortet „welches Programm heißt so“, nicht „existiert dieser Pfad“.

use harw_tool_fsread::blocking::run_blocking;
use harw_tool_fsread::budget::{fail, flag, ok};
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use rustix::fs::{Access, accessat};
use serde::Deserialize;
use serde_json::json;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Name des Werkzeugs.
pub const TOOL: &str = "sys.which";

/// Höchstzahl Namen.
pub const MAX_NAMES: usize = 16;

/// Argumente für `sys.which`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct WhichArgs {
    /// Command names to look up (1-16), plain names without '/'.
    pub names: Vec<String>,
    /// -a / --all: report every match in PATH order instead of only the first.
    #[serde(default)]
    pub all: Option<bool>,
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    meta.is_file()
        && accessat(
            rustix::fs::CWD,
            path,
            Access::EXEC_OK,
            rustix::fs::AtFlags::empty(),
        )
        .is_ok()
}

/// Führt `sys.which` gegen `path_var` aus.
#[must_use]
pub fn run(path_var: Option<OsString>, args: &WhichArgs) -> ToolOutput {
    if args.names.is_empty() || args.names.len() > MAX_NAMES {
        return fail(TOOL, format!("names must contain 1-{MAX_NAMES} entries"));
    }
    for name in &args.names {
        if name.is_empty()
            || name.len() > 255
            || name.contains('/')
            || name.contains('\0')
            || name.starts_with('-')
        {
            return fail(
                TOOL,
                format!(
                    "invalid command name '{}': plain names only, no '/'",
                    name.chars().take(32).collect::<String>()
                ),
            );
        }
    }
    let dirs: Vec<PathBuf> = path_var
        .map(|value| {
            std::env::split_paths(&value)
                .filter(|dir| dir.is_absolute())
                .collect()
        })
        .unwrap_or_default();
    let all = flag(args.all);
    let mut results = Vec::new();
    let mut missing = 0usize;
    for name in &args.names {
        let mut found: Vec<String> = Vec::new();
        for dir in &dirs {
            let candidate = dir.join(name);
            if is_executable_file(&candidate) {
                let text = candidate.to_string_lossy().into_owned();
                if !found.contains(&text) {
                    found.push(text);
                }
                if !all {
                    break;
                }
            }
        }
        if found.is_empty() {
            missing += 1;
        }
        results.push(json!({"name": name, "found": !found.is_empty(), "paths": found}));
    }
    ok(
        TOOL,
        format!("{} found, {missing} not found", results.len() - missing),
        json!({"results": results, "missing": missing, "path_dirs_searched": dirs.len(), "truncated": false}),
    )
}

/// Findet Programme im PATH wie `which`.
#[harw_macros::tool(
    name = "sys.which",
    description = "Locates executables in the agent's PATH like which: for each plain command name the first match (or every match with all=true, -a) that is an executable regular file. Use when you need to know whether and where a tool is installed instead of running which or command -v. Returns JSON {results:[{name, found, paths}], missing}. Relative PATH entries are skipped; names with '/' are rejected. Read-only.",
    permission = "execute_process",
    parallel_safe
)]
async fn sys_which(
    _context: &ToolExecutionContext,
    args: WhichArgs,
) -> Result<ToolOutput, ToolsError> {
    run_blocking(TOOL, move || run(std::env::var_os("PATH"), &args)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, error_of, json_of};
    use serde_json::Value;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn make(dir: &Path, name: &str, mode: u32) -> TestResult {
        fs::create_dir_all(dir)?;
        fs::write(dir.join(name), "#!/bin/sh\n")?;
        fs::set_permissions(dir.join(name), fs::Permissions::from_mode(mode))?;
        Ok(())
    }

    fn which(path: &[&Path], args: Value) -> TestResult<Value> {
        let joined =
            std::env::join_paths(path).map_err(|e| TestError::Unexpected(e.to_string()))?;
        json_of(run(Some(joined), &serde_json::from_value(args)?))
    }

    #[test]
    fn first_match_and_all_matches_in_path_order() -> TestResult {
        let dir = tempfile::tempdir()?;
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        make(&a, "tool", 0o755)?;
        make(&b, "tool", 0o755)?;
        make(&b, "only_b", 0o755)?;
        let first = which(&[&a, &b], json!({"names": ["tool", "only_b", "nothing"]}))?;
        assert_eq!(
            first["results"][0]["paths"],
            json!([a.join("tool").to_string_lossy()])
        );
        assert_eq!(
            first["results"][1]["paths"],
            json!([b.join("only_b").to_string_lossy()])
        );
        assert_eq!(first["results"][2]["found"], false);
        assert_eq!(first["missing"], 1);
        let every = which(&[&a, &b], json!({"names": ["tool"], "all": true}))?;
        assert_eq!(
            every["results"][0]["paths"].as_array().map(Vec::len),
            Some(2)
        );
        Ok(())
    }

    #[test]
    fn non_executable_directories_and_symlinks() -> TestResult {
        let dir = tempfile::tempdir()?;
        let bin = dir.path().join("bin");
        make(&bin, "plain", 0o644)?;
        make(&bin, "real", 0o755)?;
        fs::create_dir_all(bin.join("adir"))?;
        symlink("real", bin.join("link"))?;
        symlink("missing", bin.join("dangling"))?;
        let value = which(
            &[&bin],
            json!({"names": ["plain", "adir", "link", "dangling", "real"]}),
        )?;
        let found: Vec<bool> = value["results"]
            .as_array()
            .map(|r| r.iter().map(|x| x["found"] == true).collect())
            .unwrap_or_default();
        assert_eq!(found, vec![false, false, true, false, true]);
        Ok(())
    }

    #[test]
    fn relative_and_empty_path_entries_are_skipped() -> TestResult {
        let dir = tempfile::tempdir()?;
        make(dir.path(), "here", 0o755)?;
        let previous = std::env::current_dir()?;
        let joined =
            std::env::join_paths([Path::new(""), Path::new("."), Path::new("relative/bin")])
                .map_err(|e| TestError::Unexpected(e.to_string()))?;
        let value = json_of(run(
            Some(joined),
            &serde_json::from_value(json!({"names": ["here", "ls"]}))?,
        ))?;
        assert_eq!(previous, std::env::current_dir()?);
        assert_eq!(value["path_dirs_searched"], 0);
        assert_eq!(value["missing"], 2);
        let none = json_of(run(
            None,
            &serde_json::from_value(json!({"names": ["ls"]}))?,
        ))?;
        assert_eq!(none["missing"], 1);
        Ok(())
    }

    #[test]
    fn invalid_names_are_rejected() -> TestResult {
        for bad in [
            json!({"names": []}),
            json!({"names": ["a/b"]}),
            json!({"names": ["/bin/sh"]}),
            json!({"names": [""]}),
            json!({"names": ["-a"]}),
            json!({"names": ["a\u{0}"]}),
            json!({"names": (0..=MAX_NAMES).map(|i| format!("n{i}")).collect::<Vec<_>>()}),
        ] {
            error_of(run(None, &serde_json::from_value(bad.clone())?))
                .map_err(|e| TestError::Unexpected(format!("{bad}: {e}")))?;
        }
        assert!(
            serde_json::from_value::<WhichArgs>(json!({"names": ["a"], "silent": true})).is_err()
        );
        Ok(())
    }

    #[test]
    fn finds_a_real_system_program() -> TestResult {
        let value = json_of(run(
            std::env::var_os("PATH"),
            &serde_json::from_value(json!({"names": ["sh"]}))?,
        ))?;
        assert_eq!(value["results"][0]["found"], true);
        Ok(())
    }
}
