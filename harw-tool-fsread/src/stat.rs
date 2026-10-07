//! `fsread.stat` — `stat` in reinem Rust.
//!
//! Metadaten von bis zu [`MAX_PATHS`] Pfaden: Typ, Größe, Rechte (oktal und
//! `rwx`), uid/gid samt Namen, Zeiten, Inode, Links, Blöcke, Gerät. Mit
//! `follow` (`-L`) wird ein Symlink nur aufgelöst, wenn das Ziel innerhalb der
//! Workspace-Wurzel liegt; sonst meldet der Eintrag den Fehler. Ohne `follow`
//! beschreibt `stat` den Link selbst und nennt sein Ziel.
//!
//! Nur Metadaten, nie Inhalte: auch Geheimnis-Pfade dürfen hier „gestattet“
//! werden (Existenz, Größe), Inhalte liest dieses Werkzeug nie.

use crate::blocking::{run_blocking, scoped, workspace_root};
use crate::budget::{choice, flag, ok};
use crate::meta::{Kind, Meta, human_size};
use crate::scope::{RelPath, Scope, io_message};
use crate::users::UserDb;
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

/// Name des Werkzeugs.
pub const TOOL: &str = "fsread.stat";

/// Höchstzahl Pfade je Aufruf.
pub const MAX_PATHS: usize = 64;

/// Argumente für `fsread.stat`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct StatArgs {
    /// Paths relative to the workspace root (1-64).
    pub paths: Vec<String>,
    /// -L / --dereference: describe the symlink target (only if it stays inside the workspace).
    #[serde(default)]
    pub follow: Option<bool>,
    /// Output form: 'short' (type, size, mode, owner, mtime) or 'long' (all fields incl. atime/ctime, inode, links, blocks, device). Default long.
    #[serde(default)]
    pub format: Option<String>,
}

fn describe(scope: &Scope, db: &UserDb, input: &str, follow: bool, long: bool) -> Value {
    let rel = match scope.rel(input) {
        Ok(rel) => rel,
        Err(error) => return json!({"path": input, "ok": false, "error": error.to_string()}),
    };
    let shown = rel.display();
    let target_rel: RelPath = if follow {
        match scope.resolve_follow(&rel) {
            Ok(resolved) => resolved,
            Err(error) => return json!({"path": shown, "ok": false, "error": error.to_string()}),
        }
    } else {
        rel.clone()
    };
    let stat = match scope.lstat(&target_rel) {
        Ok(stat) => stat,
        Err(error) => {
            return json!({"path": shown, "ok": false, "error": io_message(&error)});
        }
    };
    let meta = Meta::from_stat(&stat);
    let mut value = json!({
        "path": shown,
        "ok": true,
        "type": meta.kind.name(),
        "size": meta.size,
        "size_human": human_size(meta.size),
        "mode": format!("{:04o}", meta.mode),
        "mode_string": meta.mode_string(),
        "uid": meta.uid,
        "gid": meta.gid,
        "owner": db.user_or_id(meta.uid),
        "group": db.group_or_id(meta.gid),
        "mtime": meta.mtime.iso(),
    });
    if meta.kind == Kind::Symlink {
        if let Ok(target) = scope.read_link(&target_rel) {
            value["link_target"] = json!(target.to_string_lossy());
        }
    }
    if follow && target_rel != rel {
        value["resolved_path"] = json!(target_rel.display());
    }
    if long {
        value["atime"] = meta.atime.json();
        value["mtime"] = meta.mtime.json();
        value["ctime"] = meta.ctime.json();
        value["inode"] = json!(meta.ino);
        value["nlink"] = json!(meta.nlink);
        value["blocks"] = json!(meta.blocks);
        value["block_size"] = json!(meta.blksize);
        value["device"] = json!(meta.dev);
        if matches!(meta.kind, Kind::CharDevice | Kind::BlockDevice) {
            value["rdev"] = json!(meta.rdev);
        }
    }
    value
}

/// Führt `fsread.stat` aus.
#[must_use]
pub fn run(root: &Path, args: &StatArgs) -> ToolOutput {
    scoped(TOOL, root, |scope| {
        if args.paths.is_empty() {
            return Err("paths must contain at least one path".to_owned());
        }
        if args.paths.len() > MAX_PATHS {
            return Err(format!("too many paths (max {MAX_PATHS})"));
        }
        let format = choice("format", args.format.as_deref(), &["short", "long"], "long")?;
        let follow = flag(args.follow);
        let db = UserDb::load();
        let results: Vec<Value> = args
            .paths
            .iter()
            .map(|input| describe(scope, &db, input, follow, format == "long"))
            .collect();
        let failed = results.iter().filter(|r| r["ok"] == false).count();
        Ok(ok(
            TOOL,
            format!("{} paths, {failed} failed", results.len()),
            json!({ "results": results, "failed": failed, "truncated": false }),
        ))
    })
}

/// Zeigt Metadaten von Pfaden wie `stat`.
#[harw_macros::tool(
    name = "fsread.stat",
    description = "Shows file metadata like stat: type, size, permissions (octal and rwx), uid/gid with names, atime/mtime/ctime, inode, link count, blocks, device; follow=true is -L (only targets inside the workspace). Use when you need exact metadata of up to 64 paths instead of running stat. Returns JSON {results:[{path, ok, type, size, mode, owner, mtime, ...}], failed}. Symlinks are described, not followed, unless follow is set.",
    permission = "read_workspace",
    parallel_safe
)]
async fn fsread_stat(
    context: &ToolExecutionContext,
    args: StatArgs,
) -> Result<ToolOutput, ToolsError> {
    let root = workspace_root(context);
    run_blocking(TOOL, move || run(&root, &args)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, TestResult, error_of, json_of};

    fn call(fx: &Fixture, args: Value) -> TestResult<ToolOutput> {
        Ok(run(&fx.ws, &serde_json::from_value(args)?))
    }

    #[test]
    fn long_form_has_all_fields() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("a.txt", b"hello")?;
        let value = json_of(call(&fx, json!({"paths": ["a.txt"]}))?)?;
        let r = &value["results"][0];
        assert_eq!(r["ok"], true);
        assert_eq!(r["type"], "file");
        assert_eq!(r["size"], 5);
        assert!(r["mode"].as_str().is_some_and(|m| m.len() == 4));
        for key in [
            "atime", "mtime", "ctime", "inode", "nlink", "blocks", "device", "uid", "gid", "owner",
            "group",
        ] {
            assert!(!r[key].is_null(), "missing {key}: {r}");
        }
        assert!(r["mtime"]["iso"].as_str().is_some_and(|s| s.ends_with('Z')));
        Ok(())
    }

    #[test]
    fn short_form_omits_long_fields() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("a.txt", b"x")?;
        let value = json_of(call(&fx, json!({"paths": ["a.txt"], "format": "short"}))?)?;
        let r = &value["results"][0];
        assert!(r["inode"].is_null() && r["atime"].is_null());
        assert!(r["mtime"].is_string());
        Ok(())
    }

    #[test]
    fn symlink_described_not_followed_and_follow_inside_only() -> TestResult {
        let fx = Fixture::new()?;
        fx.plant_escapes()?;
        fx.write("real.txt", b"12345")?;
        std::os::unix::fs::symlink("real.txt", fx.ws.join("alias"))?;
        let value = json_of(call(&fx, json!({"paths": ["alias", "link_file"]}))?)?;
        assert_eq!(value["results"][0]["type"], "symlink");
        assert_eq!(value["results"][0]["link_target"], "real.txt");
        let value = json_of(call(
            &fx,
            json!({"paths": ["alias", "link_file", "link_dir/secret.txt"], "follow": true}),
        )?)?;
        assert_eq!(value["results"][0]["type"], "file");
        assert_eq!(value["results"][0]["size"], 5);
        assert_eq!(value["results"][0]["resolved_path"], "real.txt");
        for index in [1, 2] {
            assert_eq!(
                value["results"][index]["ok"], false,
                "{}",
                value["results"][index]
            );
        }
        assert_eq!(value["failed"], 2);
        Ok(())
    }

    #[test]
    fn escape_through_symlink_and_dotdot_is_an_error_entry() -> TestResult {
        let fx = Fixture::new()?;
        fx.plant_escapes()?;
        let value = json_of(call(
            &fx,
            json!({"paths": ["../outside/secret.txt", "link_dir/secret.txt", "/etc/passwd", "missing"]}),
        )?)?;
        for index in 0..4 {
            assert_eq!(
                value["results"][index]["ok"], false,
                "{}",
                value["results"][index]
            );
        }
        Ok(())
    }

    #[test]
    fn bad_arguments_are_rejected() -> TestResult {
        let fx = Fixture::new()?;
        error_of(call(&fx, json!({"paths": []}))?)?;
        error_of(call(&fx, json!({"paths": ["a"], "format": "weird"}))?)?;
        let many: Vec<String> = (0..=MAX_PATHS).map(|i| format!("f{i}")).collect();
        error_of(call(&fx, json!({"paths": many}))?)?;
        let parsed: Result<StatArgs, _> =
            serde_json::from_value(json!({"paths": ["a"], "bogus": 1}));
        assert!(parsed.is_err());
        let missing: Result<StatArgs, _> = serde_json::from_value(json!({}));
        assert!(missing.is_err());
        Ok(())
    }
}
