//! `fsread.ls` — `ls` in reinem Rust.
//!
//! # Verantwortung
//! Listet ein Verzeichnis (oder einen einzelnen Eintrag) unterhalb der
//! Workspace-Wurzel. Unterstützt `-a`, `-A`, `-l`, `-h`, `-R`, `-S`, `-t`, `-r`,
//! `--sort`, `--group-directories-first`, `-L` sowie Tiefe und Limit.
//!
//! # Grenzen
//! Je Verzeichnis werden höchstens [`SCAN_CAP`] Namen betrachtet (die
//! kleinsten, deterministisch); insgesamt höchstens `limit` Einträge
//! (Standard 200, hart [`crate::budget::HARD_MAX_ITEMS`]), Tiefe höchstens
//! [`MAX_RECURSION_DEPTH`], Frist 5 s. Symlinks werden nicht betreten, auch
//! nicht mit `follow` (das ersetzt nur die angezeigten Metadaten durch die des
//! Ziels, sofern es innerhalb der Wurzel liegt).
//!
//! # Sortierung
//! Bytefolge der Namen (nicht locale-abhängig), Tiebreak immer der Name.

use crate::blocking::{run_blocking, scoped, workspace_root};
use crate::budget::{Collector, choice, flag, limit_or, ok};
use crate::meta::{Kind, Meta, human_size};
use crate::scope::{RelPath, Scope, io_message};
use crate::users::UserDb;
use crate::walk::{WALK_TIMEOUT, smallest_names};
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use rustix::fs::AtFlags;
use serde::Deserialize;
use serde_json::{Value, json};
use std::cmp::Ordering;
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Instant;

/// Name des Werkzeugs.
pub const TOOL: &str = "fsread.ls";

/// Höchstzahl betrachteter Namen je Verzeichnis.
pub const SCAN_CAP: usize = 20_000;

/// Größte Rekursionstiefe bei `-R`.
pub const MAX_RECURSION_DEPTH: usize = 16;

/// Standardtiefe bei `-R` ohne `max_depth`.
pub const DEFAULT_RECURSION_DEPTH: usize = 3;

/// Standard-Limit der Einträge.
pub const DEFAULT_LIMIT: usize = 200;

/// Argumente für `fsread.ls`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct LsArgs {
    /// Directory (or single entry) to list, relative to the workspace root. Default: the root.
    #[serde(default)]
    pub path: Option<String>,
    /// -a / --all: include entries starting with '.' (plus '.' and '..' where inside the workspace).
    #[serde(default)]
    pub all: Option<bool>,
    /// -A / --almost-all: include dot entries but not '.' and '..'.
    #[serde(default)]
    pub almost_all: Option<bool>,
    /// -l: long format (type/permissions, links, owner, group, size, mtime, symlink target).
    #[serde(default)]
    pub long: Option<bool>,
    /// -h / --human-readable: add size_human (1024-based) in long format.
    #[serde(default)]
    pub human: Option<bool>,
    /// -R / --recursive: descend into subdirectories (never through symlinks), bounded by max_depth.
    #[serde(default)]
    pub recursive: Option<bool>,
    /// -S: sort by size, largest first.
    #[serde(default)]
    pub sort_size: Option<bool>,
    /// -t: sort by modification time, newest first.
    #[serde(default)]
    pub sort_time: Option<bool>,
    /// -r / --reverse: reverse the sort order.
    #[serde(default)]
    pub reverse: Option<bool>,
    /// --sort=WORD: one of name, size, time, extension (default name). Conflicts with sort_size/sort_time.
    #[serde(default)]
    pub sort: Option<String>,
    /// --group-directories-first: list directories before other entries.
    #[serde(default)]
    pub group_directories_first: Option<bool>,
    /// -L / --dereference: show metadata of symlink targets that stay inside the workspace.
    #[serde(default)]
    pub follow: Option<bool>,
    /// Maximum depth with recursive (1-16, default 3). A value above 1 implies recursive.
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub max_depth: Option<usize>,
    /// Maximum number of entries returned (default 200, hard maximum 5000).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub limit: Option<usize>,
}

/// Sortierschlüssel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Name,
    Size,
    Time,
    Extension,
}

/// Geprüfte Optionen.
#[derive(Debug, Clone, Copy)]
struct Opts {
    all: bool,
    almost_all: bool,
    long: bool,
    human: bool,
    recursive: bool,
    key: SortKey,
    reverse: bool,
    group_dirs: bool,
    follow: bool,
    max_depth: usize,
    limit: usize,
}

/// Ein Eintrag mit Metadaten.
#[derive(Debug)]
struct Item {
    name: String,
    raw_name: OsString,
    rel: RelPath,
    meta: Meta,
    target: Option<String>,
    follow_error: Option<String>,
}

fn parse(args: &LsArgs) -> Result<Opts, String> {
    let sort = args
        .sort
        .as_deref()
        .map(|value| {
            choice(
                "sort",
                Some(value),
                &["name", "size", "time", "extension"],
                "name",
            )
        })
        .transpose()?;
    let by_flag = match (flag(args.sort_size), flag(args.sort_time)) {
        (true, true) => return Err("sort_size and sort_time conflict; pick one".to_owned()),
        (true, false) => Some("size"),
        (false, true) => Some("time"),
        (false, false) => None,
    };
    let word = match (sort, by_flag) {
        (Some(word), Some(flagged)) if word != flagged => {
            return Err(format!("sort='{word}' conflicts with the {flagged} flag"));
        }
        (Some(word), _) => word,
        (None, Some(flagged)) => flagged,
        (None, None) => "name",
    };
    let key = match word {
        "size" => SortKey::Size,
        "time" => SortKey::Time,
        "extension" => SortKey::Extension,
        _ => SortKey::Name,
    };
    if args.max_depth == Some(0) {
        return Err("max_depth must be at least 1".to_owned());
    }
    let recursive = flag(args.recursive) || args.max_depth.is_some_and(|depth| depth > 1);
    let max_depth = if recursive {
        limit_or(args.max_depth, DEFAULT_RECURSION_DEPTH, MAX_RECURSION_DEPTH)
    } else {
        1
    };
    Ok(Opts {
        all: flag(args.all),
        almost_all: flag(args.almost_all),
        long: flag(args.long),
        human: flag(args.human),
        recursive,
        key,
        reverse: flag(args.reverse),
        group_dirs: flag(args.group_directories_first),
        follow: flag(args.follow),
        max_depth,
        limit: limit_or(args.limit, DEFAULT_LIMIT, crate::budget::HARD_MAX_ITEMS),
    })
}

/// Zustand eines Laufs.
struct Run<'a> {
    scope: &'a Scope,
    opts: Opts,
    deadline: Instant,
    db: Option<UserDb>,
    out: Collector,
    scan_truncated: bool,
    skipped_errors: usize,
    stopped_deadline: bool,
}

/// Erweiterung eines Dateinamens für `--sort=extension`.
fn extension_of(name: &str) -> &str {
    match name.rfind('.') {
        Some(index) if index > 0 => &name[index + 1..],
        _ => "",
    }
}

impl Run<'_> {
    fn compare(&self, a: &Item, b: &Item) -> Ordering {
        if self.opts.group_dirs {
            let a_dir = a.meta.kind == Kind::Dir;
            let b_dir = b.meta.kind == Kind::Dir;
            if a_dir != b_dir {
                return if a_dir {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }
        }
        let by_name = a.raw_name.as_bytes().cmp(b.raw_name.as_bytes());
        let primary = match self.opts.key {
            SortKey::Name => by_name,
            // Größte / neueste zuerst, wie `ls -S` und `ls -t`.
            SortKey::Size => b.meta.size.cmp(&a.meta.size),
            SortKey::Time => b.meta.mtime.cmp(&a.meta.mtime),
            SortKey::Extension => extension_of(&a.name).cmp(extension_of(&b.name)),
        };
        let ordering = primary.then(by_name);
        if self.opts.reverse {
            ordering.reverse()
        } else {
            ordering
        }
    }

    fn make_item(&self, rel: RelPath, raw_name: OsString, meta: Meta) -> Item {
        let name = raw_name.to_string_lossy().into_owned();
        let mut item = Item {
            name,
            raw_name,
            rel,
            meta,
            target: None,
            follow_error: None,
        };
        if meta.kind == Kind::Symlink {
            item.target = self
                .scope
                .read_link(&item.rel)
                .ok()
                .map(|target| target.to_string_lossy().into_owned());
            if self.opts.follow {
                match self.scope.resolve_follow(&item.rel) {
                    Ok(resolved) => match self.scope.lstat(&resolved) {
                        Ok(stat) => item.meta = Meta::from_stat(&stat),
                        Err(error) => item.follow_error = Some(io_message(&error)),
                    },
                    Err(error) => item.follow_error = Some(error.to_string()),
                }
            }
        }
        item
    }

    fn json(&self, item: &Item) -> Value {
        let mut value = json!({
            "path": item.rel.display(),
            "name": item.name,
            "type": item.meta.kind.name(),
        });
        if self.opts.long {
            let meta = &item.meta;
            let db = self.db.as_ref();
            value["mode"] = json!(meta.mode_string());
            value["nlink"] = json!(meta.nlink);
            value["owner"] =
                json!(db.map_or_else(|| meta.uid.to_string(), |db| db.user_or_id(meta.uid)));
            value["group"] =
                json!(db.map_or_else(|| meta.gid.to_string(), |db| db.group_or_id(meta.gid)));
            value["size"] = json!(meta.size);
            if self.opts.human {
                value["size_human"] = json!(human_size(meta.size));
            }
            value["mtime"] = json!(meta.mtime.iso());
        }
        if let Some(target) = &item.target {
            value["target"] = json!(target);
        }
        if let Some(error) = &item.follow_error {
            value["follow_error"] = json!(error);
        }
        value
    }

    /// Listet `rel`; `false` = alles beenden (Limit/Frist).
    fn list_dir(&mut self, rel: &RelPath, depth: usize) -> bool {
        let fd = match self.scope.open_dir(rel) {
            Ok(fd) => fd,
            Err(_) => {
                self.skipped_errors += 1;
                return true;
            }
        };
        let names = match smallest_names(&fd, SCAN_CAP + 1, self.deadline) {
            Ok(Some(names)) => names,
            Ok(None) => {
                self.stopped_deadline = true;
                return false;
            }
            Err(_) => {
                self.skipped_errors += 1;
                return true;
            }
        };
        let mut names = names;
        if names.len() > SCAN_CAP {
            names.truncate(SCAN_CAP);
            self.scan_truncated = true;
        }
        let mut items: Vec<Item> = Vec::new();
        if self.opts.all {
            if let Ok(stat) = self.scope.lstat(rel) {
                items.push(self.make_item(
                    rel.clone(),
                    OsString::from("."),
                    Meta::from_stat(&stat),
                ));
            }
            if !rel.is_root() {
                let parent = parent_of(rel);
                if let Ok(stat) = self.scope.lstat(&parent) {
                    items.push(self.make_item(
                        parent,
                        OsString::from(".."),
                        Meta::from_stat(&stat),
                    ));
                }
            }
        }
        for name in names {
            let hidden = name.as_bytes().first() == Some(&b'.');
            if hidden && !(self.opts.all || self.opts.almost_all) {
                continue;
            }
            let stat = match rustix::fs::statat(&fd, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => stat,
                Err(_) => {
                    self.skipped_errors += 1;
                    continue;
                }
            };
            let child = rel.join(&name);
            items.push(self.make_item(child, name, Meta::from_stat(&stat)));
        }
        items.sort_by(|a, b| self.compare(a, b));
        let mut subdirs: Vec<RelPath> = Vec::new();
        for item in &items {
            let value = self.json(item);
            if !self.out.push(value) {
                return false;
            }
            let dot = item.name == "." || item.name == "..";
            if self.opts.recursive
                && depth < self.opts.max_depth
                && !dot
                && item.meta.kind == Kind::Dir
                // `follow` kann einen Link als Verzeichnis zeigen; betreten wird er nie.
                && item.target.is_none()
            {
                subdirs.push(item.rel.clone());
            }
        }
        for sub in subdirs {
            if Instant::now() >= self.deadline {
                self.stopped_deadline = true;
                return false;
            }
            if !self.list_dir(&sub, depth + 1) {
                return false;
            }
        }
        true
    }
}

fn parent_of(rel: &RelPath) -> RelPath {
    let parent = rel
        .as_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    // Über `rel("")` normalisiert, damit nur gültige RelPath entstehen.
    let mut out = RelPath::root();
    for component in parent.components() {
        out = out.join(component.as_os_str());
    }
    out
}

/// Führt `fsread.ls` aus.
#[must_use]
pub fn run(root: &Path, args: &LsArgs) -> ToolOutput {
    scoped(TOOL, root, |scope| {
        let opts = parse(args)?;
        let input = args
            .path
            .as_deref()
            .filter(|p| !p.is_empty())
            .unwrap_or(".");
        let mut rel = scope.rel(input).map_err(|e| e.to_string())?;
        if opts.follow {
            rel = scope.resolve_follow(&rel).map_err(|e| e.to_string())?;
        }
        let stat = scope
            .lstat(&rel)
            .map_err(|e| format!("cannot access '{}': {}", rel.display(), io_message(&e)))?;
        let meta = Meta::from_stat(&stat);
        let mut run = Run {
            scope,
            opts,
            deadline: Instant::now() + WALK_TIMEOUT,
            db: opts.long.then(UserDb::load),
            out: Collector::new(opts.limit),
            scan_truncated: false,
            skipped_errors: 0,
            stopped_deadline: false,
        };
        if meta.kind == Kind::Dir {
            run.list_dir(&rel, 1);
        } else {
            let name = rel.file_name().map(OsString::from).unwrap_or_default();
            let item = run.make_item(rel.clone(), name, meta);
            let value = run.json(&item);
            run.out.push(value);
        }
        let truncated = run.out.truncated() || run.scan_truncated || run.stopped_deadline;
        let stopped = if run.stopped_deadline {
            Some("deadline")
        } else if run.out.truncated() {
            run.out.stop_reason()
        } else if run.scan_truncated {
            Some("directory_scan_limit")
        } else {
            None
        };
        let count = run.out.len();
        let skipped = run.skipped_errors;
        let summary = format!(
            "{count} entries in {}{}",
            rel.display(),
            if truncated { " (truncated)" } else { "" }
        );
        Ok(ok(
            TOOL,
            summary,
            json!({
                "path": rel.display(),
                "entries": run.out.into_items(),
                "count": count,
                "truncated": truncated,
                "stopped": stopped,
                "unreadable_entries": skipped,
            }),
        ))
    })
}

/// Listet Verzeichnisse und Dateien wie `ls`.
#[harw_macros::tool(
    name = "fsread.ls",
    description = "Lists a directory or entry inside the workspace like ls: -a/-A, -l (type, permissions, owner, size, mtime), -h, -R with max_depth, -S/-t/--sort, -r, --group-directories-first, -L. Use when you need names, sizes or timestamps of directory contents instead of running ls. Returns JSON {entries:[{path,name,type,...}], count, truncated}; output is bounded (default 200 entries, hard 5000) and sorted by name unless told otherwise. Never leaves the workspace and never follows symlinks out of it.",
    permission = "read_workspace",
    parallel_safe
)]
async fn fsread_ls(context: &ToolExecutionContext, args: LsArgs) -> Result<ToolOutput, ToolsError> {
    let root = workspace_root(context);
    run_blocking(TOOL, move || run(&root, &args)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, TestError, TestResult, error_of, json_of, run as exec};
    use harw_authority::Permission;

    fn args(value: Value) -> TestResult<LsArgs> {
        Ok(serde_json::from_value(value)?)
    }

    fn names(output: ToolOutput) -> TestResult<Vec<String>> {
        let value = json_of(output)?;
        let entries = value["entries"]
            .as_array()
            .ok_or(TestError::Missing("entries"))?;
        Ok(entries
            .iter()
            .filter_map(|e| e["path"].as_str().map(str::to_owned))
            .collect())
    }

    fn field_list(output: ToolOutput, field: &str) -> TestResult<Vec<String>> {
        let value = json_of(output)?;
        let entries = value["entries"]
            .as_array()
            .ok_or(TestError::Missing("entries"))?;
        Ok(entries
            .iter()
            .filter_map(|e| e[field].as_str().map(str::to_owned))
            .collect())
    }

    fn sample() -> TestResult<Fixture> {
        let fx = Fixture::new()?;
        fx.write("b.txt", b"12345")?;
        fx.write("a.rs", b"1")?;
        fx.write(".hidden", b"h")?;
        fx.write("dir/inner.txt", b"123456789")?;
        fx.write("dir/sub/deep.txt", b"")?;
        Ok(fx)
    }

    #[test]
    fn default_listing_hides_dotfiles_and_sorts_by_name() -> TestResult {
        let fx = sample()?;
        let out = run(&fx.ws, &args(json!({}))?);
        assert_eq!(names(out)?, vec!["a.rs", "b.txt", "dir"]);
        Ok(())
    }

    #[test]
    fn all_and_almost_all() -> TestResult {
        let fx = sample()?;
        let all = names(run(&fx.ws, &args(json!({"all": true}))?))?;
        assert_eq!(all, vec![".", ".hidden", "a.rs", "b.txt", "dir"]);
        let almost = names(run(&fx.ws, &args(json!({"almost_all": true}))?))?;
        assert_eq!(almost, vec![".hidden", "a.rs", "b.txt", "dir"]);
        let sub = field_list(
            run(&fx.ws, &args(json!({"path": "dir", "all": true}))?),
            "name",
        )?;
        assert_eq!(sub, vec![".", "..", "inner.txt", "sub"]);
        let root_all = field_list(run(&fx.ws, &args(json!({"all": true}))?), "name")?;
        assert!(
            !root_all.contains(&"..".to_owned()),
            "root must not expose its parent"
        );
        Ok(())
    }

    #[test]
    fn long_format_has_permissions_owner_size_and_mtime() -> TestResult {
        let fx = sample()?;
        let value = json_of(run(&fx.ws, &args(json!({"long": true, "human": true}))?))?;
        let first = &value["entries"][0];
        assert_eq!(first["name"], "a.rs");
        assert_eq!(first["type"], "file");
        assert_eq!(first["size"], 1);
        assert_eq!(first["size_human"], "1");
        assert!(
            first["mode"]
                .as_str()
                .is_some_and(|m| m.len() == 10 && m.starts_with('-'))
        );
        assert!(first["mtime"].as_str().is_some_and(|m| m.ends_with('Z')));
        assert!(first["owner"].is_string());
        Ok(())
    }

    #[test]
    fn sort_by_size_time_extension_and_reverse_with_name_tiebreak() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("f1.zz", b"1")?;
        fx.write("f2.aa", b"12345")?;
        fx.write("f3.mm", b"123")?;
        fx.write("f4.mm", b"123")?;
        let set = |name: &str, secs: u64| -> TestResult {
            let file = std::fs::File::options()
                .write(true)
                .open(fx.ws.join(name))?;
            file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))?;
            Ok(())
        };
        set("f1.zz", 300)?;
        set("f2.aa", 100)?;
        set("f3.mm", 200)?;
        set("f4.mm", 200)?;
        let by_size = names(run(&fx.ws, &args(json!({"sort_size": true}))?))?;
        assert_eq!(by_size, vec!["f2.aa", "f3.mm", "f4.mm", "f1.zz"]);
        let by_time = names(run(&fx.ws, &args(json!({"sort_time": true}))?))?;
        assert_eq!(by_time, vec!["f1.zz", "f3.mm", "f4.mm", "f2.aa"]);
        let rev = names(run(
            &fx.ws,
            &args(json!({"sort_time": true, "reverse": true}))?,
        ))?;
        assert_eq!(rev, vec!["f2.aa", "f4.mm", "f3.mm", "f1.zz"]);
        let ext = names(run(&fx.ws, &args(json!({"sort": "extension"}))?))?;
        assert_eq!(ext, vec!["f2.aa", "f3.mm", "f4.mm", "f1.zz"]);
        let plain_reverse = names(run(&fx.ws, &args(json!({"reverse": true}))?))?;
        assert_eq!(plain_reverse, vec!["f4.mm", "f3.mm", "f2.aa", "f1.zz"]);
        Ok(())
    }

    #[test]
    fn group_directories_first() -> TestResult {
        let fx = sample()?;
        let grouped = names(run(
            &fx.ws,
            &args(json!({"group_directories_first": true}))?,
        ))?;
        assert_eq!(grouped, vec!["dir", "a.rs", "b.txt"]);
        let grouped_rev = names(run(
            &fx.ws,
            &args(json!({"group_directories_first": true, "reverse": true}))?,
        ))?;
        assert_eq!(grouped_rev, vec!["dir", "b.txt", "a.rs"]);
        Ok(())
    }

    #[test]
    fn recursive_respects_depth() -> TestResult {
        let fx = sample()?;
        let two = names(run(
            &fx.ws,
            &args(json!({"recursive": true, "max_depth": 2}))?,
        ))?;
        assert_eq!(
            two,
            vec!["a.rs", "b.txt", "dir", "dir/inner.txt", "dir/sub"]
        );
        let three = names(run(&fx.ws, &args(json!({"recursive": true}))?))?;
        assert!(three.contains(&"dir/sub/deep.txt".to_owned()));
        Ok(())
    }

    #[test]
    fn limit_truncates_and_flags() -> TestResult {
        let fx = sample()?;
        let value = json_of(run(&fx.ws, &args(json!({"limit": 2}))?))?;
        assert_eq!(value["count"], 2);
        assert_eq!(value["truncated"], true);
        assert_eq!(value["stopped"], "entry_limit");
        Ok(())
    }

    #[test]
    fn huge_directory_is_bounded_and_deterministic() -> TestResult {
        let fx = Fixture::new()?;
        for index in 0..2500 {
            fx.write(&format!("big/f{index:05}"), b"")?;
        }
        let value = json_of(run(&fx.ws, &args(json!({"path": "big", "limit": 3}))?))?;
        let first: Vec<&str> = value["entries"]
            .as_array()
            .ok_or(TestError::Missing("entries"))?
            .iter()
            .filter_map(|e| e["name"].as_str())
            .collect();
        assert_eq!(first, vec!["f00000", "f00001", "f00002"]);
        Ok(())
    }

    #[test]
    fn symlinks_show_target_and_are_not_entered() -> TestResult {
        let fx = Fixture::new()?;
        fx.plant_escapes()?;
        fx.write("real/x", b"1")?;
        let value = json_of(run(
            &fx.ws,
            &args(json!({"recursive": true, "long": true}))?,
        ))?;
        let entries = value["entries"]
            .as_array()
            .ok_or(TestError::Missing("entries"))?;
        let link = entries
            .iter()
            .find(|e| e["name"] == "link_dir")
            .ok_or(TestError::Missing("link_dir"))?;
        assert_eq!(link["type"], "symlink");
        assert!(link["target"].is_string());
        assert!(
            !entries.iter().any(|e| e["path"]
                .as_str()
                .is_some_and(|p| p.starts_with("link_dir/"))),
            "symlink dir must not be entered"
        );
        assert!(
            !entries
                .iter()
                .any(|e| e["path"].as_str().is_some_and(|p| p.starts_with("loop/")))
        );
        Ok(())
    }

    #[test]
    fn follow_with_recursion_never_enters_a_linked_directory() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("real/inner.txt", b"1")?;
        std::os::unix::fs::symlink("real", fx.ws.join("alias"))?;
        let raw = json_of(run(
            &fx.ws,
            &args(json!({"recursive": true, "follow": true, "max_depth": 3}))?,
        ))?;
        // Auch kein stiller Versuch (`unreadable_entries`), den Link zu öffnen.
        assert_eq!(raw["unreadable_entries"], 0);
        let value: Vec<String> = raw["entries"]
            .as_array()
            .map(|e| {
                e.iter()
                    .filter_map(|x| x["path"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        assert!(value.contains(&"alias".to_owned()));
        assert!(value.contains(&"real/inner.txt".to_owned()));
        assert!(
            !value.iter().any(|p| p.starts_with("alias/")),
            "linked directory must not be entered: {value:?}"
        );
        Ok(())
    }

    #[test]
    fn follow_shows_inside_targets_and_reports_outside() -> TestResult {
        let fx = Fixture::new()?;
        fx.plant_escapes()?;
        fx.write("real.txt", b"12345")?;
        std::os::unix::fs::symlink("real.txt", fx.ws.join("alias"))?;
        let value = json_of(run(&fx.ws, &args(json!({"follow": true, "long": true}))?))?;
        let entries = value["entries"]
            .as_array()
            .ok_or(TestError::Missing("entries"))?;
        let alias = entries
            .iter()
            .find(|e| e["name"] == "alias")
            .ok_or(TestError::Missing("alias"))?;
        assert_eq!(alias["type"], "file");
        assert_eq!(alias["size"], 5);
        let out = entries
            .iter()
            .find(|e| e["name"] == "link_file")
            .ok_or(TestError::Missing("link_file"))?;
        assert_eq!(out["type"], "symlink");
        assert!(out["follow_error"].is_string(), "{out}");
        Ok(())
    }

    #[test]
    fn escape_and_missing_and_conflicts_are_rejected() -> TestResult {
        let fx = sample()?;
        for bad in [
            json!({"path": ".."}),
            json!({"path": "/etc"}),
            json!({"path": "missing"}),
            json!({"sort": "bogus"}),
            json!({"sort_size": true, "sort_time": true}),
            json!({"sort": "time", "sort_size": true}),
            json!({"max_depth": 0}),
        ] {
            let result = run(&fx.ws, &args(bad.clone())?);
            error_of(result).map_err(|e| TestError::Unexpected(format!("{bad}: {e}")))?;
        }
        Ok(())
    }

    #[test]
    fn single_file_and_symlink_path() -> TestResult {
        let fx = sample()?;
        fx.plant_escapes()?;
        let single = names(run(&fx.ws, &args(json!({"path": "b.txt"}))?))?;
        assert_eq!(single, vec!["b.txt"]);
        let link = names(run(&fx.ws, &args(json!({"path": "link_dir"}))?))?;
        assert_eq!(link, vec!["link_dir"]);
        // Durch einen Symlink hindurch wird nichts gelistet.
        assert!(error_of(run(&fx.ws, &args(json!({"path": "link_dir/deep"}))?)).is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn unknown_option_is_rejected_and_permission_is_required() -> TestResult {
        let fx = sample()?;
        let tool = FsreadLsTool;
        let ctx = fx.read_ctx()?;
        let result = exec(&tool, &ctx, TOOL, json!({"bogus": true})).await;
        match result {
            Err(_) => {}
            Ok(output) => {
                error_of(output)?;
            }
        }
        let no_perm = fx.ctx(vec![Permission::WriteWorkspace])?;
        let denied = exec(&tool, &no_perm, TOOL, json!({})).await;
        match denied {
            Err(_) => {}
            Ok(output) => {
                error_of(output)?;
            }
        }
        let good = exec(&tool, &ctx, TOOL, json!({"path": "."})).await?;
        json_of(good)?;
        Ok(())
    }
}
