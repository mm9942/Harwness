//! `fsread.readlink` und `fsread.realpath` — Link-Ziele und kanonische Pfade.
//!
//! `readlink` zeigt das rohe Ziel eines Symlinks; mit `canonicalize` (`-f`)
//! zusätzlich den aufgelösten Pfad — aber **nur**, wenn jede Zwischenstation
//! innerhalb der Workspace-Wurzel bleibt (sonst Fehler, nicht das fremde
//! Ziel). `realpath` löst einen Pfad vollständig auf und verlangt, dass das
//! Ergebnis existiert und unter der Wurzel liegt.
//!
//! Das rohe Ziel eines Links kann ein Pfad außerhalb des Workspace sein; er
//! wird als Text gemeldet (`target_inside_workspace` sagt, ob er innerhalb
//! liegt), aber nie geöffnet.

use crate::blocking::{run_blocking, scoped, workspace_root};
use crate::budget::{flag, ok};
use crate::meta::{Kind, Meta};
use crate::scope::io_message;
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::Deserialize;
use serde_json::json;
use std::path::Path;

/// Name von `fsread.readlink`.
pub const READLINK_TOOL: &str = "fsread.readlink";

/// Name von `fsread.realpath`.
pub const REALPATH_TOOL: &str = "fsread.realpath";

/// Argumente für `fsread.readlink`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct ReadlinkArgs {
    /// Symlink relative to the workspace root.
    pub path: String,
    /// -f / --canonicalize: also resolve the full chain; fails if it leaves the workspace.
    #[serde(default)]
    pub canonicalize: Option<bool>,
}

/// Argumente für `fsread.realpath`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct RealpathArgs {
    /// Path relative to the workspace root (may contain symlinks and '..').
    pub path: String,
}

/// Führt `fsread.readlink` aus.
#[must_use]
pub fn run_readlink(root: &Path, args: &ReadlinkArgs) -> ToolOutput {
    scoped(READLINK_TOOL, root, |scope| {
        let rel = scope.rel(&args.path).map_err(|e| e.to_string())?;
        let stat = scope
            .lstat(&rel)
            .map_err(|e| format!("cannot access '{}': {}", rel.display(), io_message(&e)))?;
        if Meta::from_stat(&stat).kind != Kind::Symlink {
            return Err(format!("'{}' is not a symbolic link", rel.display()));
        }
        let target = scope
            .read_link(&rel)
            .map_err(|e| format!("cannot read link '{}': {}", rel.display(), io_message(&e)))?;
        let inside = if target.is_absolute() {
            target.starts_with(scope.root())
        } else {
            // Relative Ziele: lexikalisch ab dem Verzeichnis des Links prüfen.
            let mut depth = i64::try_from(rel.as_path().components().count()).unwrap_or(0) - 1;
            let mut inside = true;
            for component in target.components() {
                match component {
                    std::path::Component::ParentDir => {
                        depth -= 1;
                        if depth < 0 {
                            inside = false;
                            break;
                        }
                    }
                    std::path::Component::Normal(_) => depth += 1,
                    _ => {}
                }
            }
            inside
        };
        let mut data = json!({
            "path": rel.display(),
            "target": target.to_string_lossy(),
            "target_inside_workspace": inside,
            "truncated": false,
        });
        if flag(args.canonicalize) {
            let resolved = scope.resolve_follow(&rel).map_err(|e| e.to_string())?;
            scope.lstat(&resolved).map_err(|e| {
                format!(
                    "link target '{}' does not exist: {}",
                    resolved.display(),
                    io_message(&e)
                )
            })?;
            data["resolved"] = json!(resolved.display());
        }
        Ok(ok(
            READLINK_TOOL,
            format!("{} -> {}", rel.display(), target.display()),
            data,
        ))
    })
}

/// Führt `fsread.realpath` aus.
#[must_use]
pub fn run_realpath(root: &Path, args: &RealpathArgs) -> ToolOutput {
    scoped(REALPATH_TOOL, root, |scope| {
        let rel = scope.rel(&args.path).map_err(|e| e.to_string())?;
        let resolved = scope.resolve_follow(&rel).map_err(|e| e.to_string())?;
        scope
            .lstat(&resolved)
            .map_err(|e| format!("cannot access '{}': {}", resolved.display(), io_message(&e)))?;
        let absolute = scope.root().join(resolved.as_path());
        Ok(ok(
            REALPATH_TOOL,
            resolved.display(),
            json!({
                "path": rel.display(),
                "relative": resolved.display(),
                "absolute": absolute.to_string_lossy(),
                "truncated": false,
            }),
        ))
    })
}

/// Zeigt das Ziel eines Symlinks wie `readlink`.
#[harw_macros::tool(
    name = "fsread.readlink",
    description = "Shows the raw target of a symbolic link inside the workspace like readlink, and with canonicalize=true (-f) the fully resolved path if the whole chain stays inside the workspace. Use when you need to know where a link points instead of running readlink. Returns JSON {path, target, target_inside_workspace, resolved}. Links pointing outside are reported as text and never opened.",
    permission = "read_workspace",
    parallel_safe
)]
async fn fsread_readlink(
    context: &ToolExecutionContext,
    args: ReadlinkArgs,
) -> Result<ToolOutput, ToolsError> {
    let root = workspace_root(context);
    run_blocking(READLINK_TOOL, move || run_readlink(&root, &args)).await
}

/// Löst einen Pfad kanonisch auf wie `realpath`.
#[harw_macros::tool(
    name = "fsread.realpath",
    description = "Resolves a workspace path to its canonical form like realpath: symlinks and '..' are resolved, the result must exist and stay inside the workspace. Use when you need the canonical location of a path instead of running realpath. Returns JSON {relative, absolute}; any chain leaving the workspace is an error.",
    permission = "read_workspace",
    parallel_safe
)]
async fn fsread_realpath(
    context: &ToolExecutionContext,
    args: RealpathArgs,
) -> Result<ToolOutput, ToolsError> {
    let root = workspace_root(context);
    run_blocking(REALPATH_TOOL, move || run_realpath(&root, &args)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, TestResult, error_of, json_of};
    use serde_json::Value;
    use std::os::unix::fs::symlink;

    fn rl(fx: &Fixture, args: Value) -> ToolOutput {
        match serde_json::from_value(args) {
            Ok(parsed) => run_readlink(&fx.ws, &parsed),
            Err(error) => ToolOutput::error(error.to_string()),
        }
    }

    fn rp(fx: &Fixture, path: &str) -> ToolOutput {
        run_realpath(
            &fx.ws,
            &RealpathArgs {
                path: path.to_owned(),
            },
        )
    }

    #[test]
    fn readlink_reports_target_and_containment() -> TestResult {
        let fx = Fixture::new()?;
        fx.plant_escapes()?;
        fx.write("real/x", b"1")?;
        symlink("real/x", fx.ws.join("inner"))?;
        symlink("../outside/secret.txt", fx.ws.join("rel_out"))?;
        let inner = json_of(rl(&fx, json!({"path": "inner"})))?;
        assert_eq!(inner["target"], "real/x");
        assert_eq!(inner["target_inside_workspace"], true);
        let out = json_of(rl(&fx, json!({"path": "rel_out"})))?;
        assert_eq!(out["target_inside_workspace"], false);
        let abs = json_of(rl(&fx, json!({"path": "link_file"})))?;
        assert_eq!(abs["target_inside_workspace"], false);
        Ok(())
    }

    #[test]
    fn readlink_canonicalize_refuses_outside_chains() -> TestResult {
        let fx = Fixture::new()?;
        fx.plant_escapes()?;
        fx.write("real/x", b"1")?;
        symlink("real/x", fx.ws.join("inner"))?;
        let ok_value = json_of(rl(&fx, json!({"path": "inner", "canonicalize": true})))?;
        assert_eq!(ok_value["resolved"], "real/x");
        error_of(rl(&fx, json!({"path": "link_file", "canonicalize": true})))?;
        error_of(rl(&fx, json!({"path": "real/x"})))?;
        error_of(rl(&fx, json!({"path": "missing"})))?;
        symlink("nowhere", fx.ws.join("dangling"))?;
        error_of(rl(&fx, json!({"path": "dangling", "canonicalize": true})))?;
        assert_eq!(
            json_of(rl(&fx, json!({"path": "dangling"})))?["target"],
            "nowhere"
        );
        Ok(())
    }

    #[test]
    fn realpath_resolves_inside_and_rejects_outside() -> TestResult {
        let fx = Fixture::new()?;
        fx.plant_escapes()?;
        fx.write("real/x", b"1")?;
        symlink("real", fx.ws.join("alias"))?;
        let value = json_of(rp(&fx, "alias/x"))?;
        assert_eq!(value["relative"], "real/x");
        assert_eq!(
            value["absolute"],
            fx.ws.join("real/x").to_string_lossy().as_ref()
        );
        assert_eq!(json_of(rp(&fx, "real/../real/x"))?["relative"], "real/x");
        assert_eq!(json_of(rp(&fx, "."))?["relative"], ".");
        for bad in [
            "link_file",
            "link_dir/secret.txt",
            "../outside",
            "/etc/passwd",
            "missing",
            "loop/loop/nope",
        ] {
            error_of(rp(&fx, bad))
                .map_err(|e| crate::test_support::TestError::Unexpected(format!("{bad}: {e}")))?;
        }
        Ok(())
    }
}
