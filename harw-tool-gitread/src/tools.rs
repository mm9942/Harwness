//! Die sechs `git.*`-Werkzeuge: Argumente, Ausführung und Provider.
//!
//! Alle Werkzeuge deklarieren `ReadWorkspace`, sind `parallel_safe` und
//! rein lesend; keines startet einen Fremdprozess. Das Git-Verzeichnis wird
//! ausschließlich über den symlinkfreien [`harw_tool_fsread::scope::Scope`]
//! gelesen und muss innerhalb der Workspace-Wurzel liegen.

use crate::blame::{self, BlameOpts};
use crate::branch::{self, BranchOpts};
use crate::config::Config;
use crate::diffcore::{DEFAULT_MAX_FILES, Format, MAX_CONTEXT, Reader, RenderOpts, render};
use crate::fmtutil::{deadline, parse_when};
use crate::graph::{ahead_behind, read_commit};
use crate::index::read as read_index;
use crate::log::{self, DEFAULT_MAX_COUNT, LogOpts};
use crate::odb::Odb;
use crate::pathspec::Pathspec;
use crate::refs::{Head, Refs};
use crate::repo::Repo;
use crate::rev::Revs;
use crate::show::{self, ShowOpts, tree_report};
use crate::snapshot::{WorktreeCtx, changes, from_flat, from_index, index_mtime, worktree};
use crate::status::{self, StatusOpts, Untracked};
use crate::tree::flatten;
use harw_tool_fsread::blocking::{run_blocking, workspace_root};
use harw_tool_fsread::budget::{Collector, choice, fail, flag, limit_or, ok};
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

/// Standardzahl Status-Einträge.
pub const DEFAULT_STATUS_LIMIT: usize = 500;
/// Obergrenze Status-Einträge.
pub const HARD_STATUS_LIMIT: usize = 5000;

fn with_repo<F>(tool: &str, root: &Path, body: F) -> ToolOutput
where
    F: FnOnce(&Repo, &Odb<'_>, &Refs<'_>) -> Result<ToolOutput, String>,
{
    let repo = match Repo::open(root) {
        Ok(repo) => repo,
        Err(message) => return fail(tool, message),
    };
    let odb = Odb::new(&repo);
    let refs = Refs::new(&repo);
    match body(&repo, &odb, &refs) {
        Ok(output) => output,
        Err(message) => fail(tool, message),
    }
}

fn spec_of(paths: Option<&Vec<String>>) -> Result<Pathspec, String> {
    paths.map_or_else(|| Ok(Pathspec::all()), |p| Pathspec::parse(p))
}

// ---------------------------------------------------------------- status

/// Name von `git.status`.
pub const STATUS: &str = "git.status";

/// Argumente für `git.status`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct StatusArgs {
    /// Untracked files: "normal" (default, untracked directories as dir/), "all" (every file) or "no".
    #[serde(default)]
    pub untracked: Option<String>,
    /// Also list ignored files and directories (default false).
    #[serde(default)]
    pub ignored: Option<bool>,
    /// Only report these workspace-relative literal paths (files or directories, no patterns).
    #[serde(default)]
    pub paths: Option<Vec<String>>,
    /// Maximum number of entries returned (default 500, maximum 5000).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub limit: Option<usize>,
}

fn head_json(repo: &Repo, odb: &Odb<'_>, refs: &Refs<'_>, head: &Head) -> Value {
    let (branch, oid) = match head {
        Head::Branch { name, oid } => (Some(name.clone()), *oid),
        Head::Detached(oid) => (None, Some(*oid)),
    };
    let mut value =
        json!({"branch": branch, "detached": branch.is_none(), "oid": oid.map(|o| o.hex())});
    if let Some(oid) = oid {
        if let Ok(commit) = read_commit(odb, &oid) {
            value["subject"] = json!(commit.subject().chars().take(200).collect::<String>());
        }
    }
    if let (Some(name), Some(tip)) = (&branch, oid) {
        if let Some((upstream_ref, upstream_short)) = Config::load(repo).upstream(name) {
            value["upstream"] = json!(upstream_short);
            let upstream = refs.resolve(&upstream_ref).ok().flatten();
            let counts = upstream.and_then(|up| ahead_behind(odb, &tip, &up, deadline()));
            value["upstream_gone"] = json!(upstream.is_none());
            value["ahead"] = counts.map_or(Value::Null, |(a, _)| json!(a));
            value["behind"] = counts.map_or(Value::Null, |(_, b)| json!(b));
        }
    }
    value
}

/// Führt `git.status` aus.
#[must_use]
pub fn run_status(root: &Path, args: &StatusArgs) -> ToolOutput {
    with_repo(STATUS, root, |repo, odb, refs| {
        let untracked = Untracked::from_name(choice(
            "untracked",
            args.untracked.as_deref(),
            Untracked::NAMES,
            "normal",
        )?)
        .ok_or("invalid untracked")?;
        let spec = spec_of(args.paths.as_ref())?;
        let limit = limit_or(args.limit, DEFAULT_STATUS_LIMIT, HARD_STATUS_LIMIT);
        let result = status::compute(
            repo,
            odb,
            refs,
            &StatusOpts {
                untracked,
                ignored: flag(args.ignored),
                spec,
                deadline: deadline(),
            },
        )?;
        let name = |p: &[u8]| String::from_utf8_lossy(p).into_owned();
        let mut out = Collector::new(limit);
        for change in &result.staged {
            out.push(
                json!({"area": "staged", "status": change.kind.word(), "path": name(&change.path)}),
            );
        }
        for (path, stages) in &result.conflicts {
            out.push(json!({"area": "conflict", "status": "unmerged", "path": name(path), "stages": stages}));
        }
        for change in &result.unstaged {
            out.push(json!({"area": "unstaged", "status": change.kind.word(), "path": name(&change.path)}));
        }
        for path in &result.untracked {
            out.push(json!({"area": "untracked", "path": name(path)}));
        }
        for path in &result.ignored {
            out.push(json!({"area": "ignored", "path": name(path)}));
        }
        let counts = json!({
            "staged": result.staged.len(), "unstaged": result.unstaged.len(), "conflicts": result.conflicts.len(),
            "untracked": result.untracked.len(), "ignored": result.ignored.len(),
        });
        let clean = result.staged.is_empty()
            && result.unstaged.is_empty()
            && result.conflicts.is_empty()
            && result.untracked.is_empty();
        let head = head_json(repo, odb, refs, &result.head);
        let summary = format!(
            "{}: {} staged, {} unstaged, {} untracked{}",
            head["branch"]
                .as_str()
                .map_or_else(|| "detached HEAD".to_owned(), |b| format!("on {b}")),
            result.staged.len(),
            result.unstaged.len(),
            result.untracked.len(),
            if result.conflicts.is_empty() {
                String::new()
            } else {
                format!(", {} conflicted", result.conflicts.len())
            }
        );
        let truncated = out.truncated();
        let reason = out.stop_reason();
        let data = json!({
            "head": head, "operation": result.operation, "clean": clean, "counts": counts,
            "entries": out.into_items(), "truncated": truncated, "stop_reason": reason,
            "incomplete": result.incomplete,
        });
        Ok(ok(STATUS, summary, data))
    })
}

/// Zeigt den Zustand des Repositories (`git status`).
#[harw_macros::tool(
    name = "git.status",
    description = "Reports the git status of the workspace repository like git status --porcelain: current branch (with upstream ahead/behind), staged, unstaged, conflicted, untracked and optionally ignored paths. Use when you need to know what changed or whether the tree is clean instead of running git status. Options: untracked (normal|all|no), ignored, paths (literal paths), limit. Returns JSON {head, operation, clean, counts, entries[{area,status,path}], truncated}. Read-only, pure Rust (no git process); .git must be inside the workspace; no core.autocrlf/attribute filters.",
    permission = "read_workspace",
    parallel_safe
)]
async fn git_status(
    context: &ToolExecutionContext,
    args: StatusArgs,
) -> Result<ToolOutput, ToolsError> {
    let root = workspace_root(context);
    run_blocking(STATUS, move || run_status(&root, &args)).await
}

// ---------------------------------------------------------------- diff

/// Name von `git.diff`.
pub const DIFF: &str = "git.diff";

/// Argumente für `git.diff`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct DiffArgs {
    /// Base revision (commit, branch, tag, HEAD~2, ...). Without it the index is compared with the working tree (or HEAD with the index when staged is true).
    #[serde(default)]
    pub base: Option<String>,
    /// Target revision; with base this compares two commits (git diff base target).
    #[serde(default)]
    pub target: Option<String>,
    /// Compare against the index instead of the working tree (git diff --cached).
    #[serde(default)]
    pub staged: Option<bool>,
    /// Only these workspace-relative literal paths (files or directories, no patterns).
    #[serde(default)]
    pub paths: Option<Vec<String>>,
    /// Output: "patch" (default), "stat" (per-file counts), "name_only" or "name_status".
    #[serde(default)]
    pub format: Option<String>,
    /// Lines of context around changes (default 3, maximum 50).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub context: Option<usize>,
    /// Ignore all whitespace when comparing lines (-w).
    #[serde(default)]
    pub ignore_whitespace: Option<bool>,
    /// Maximum number of files listed (default 200, maximum 2000).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub max_files: Option<usize>,
}

/// Führt `git.diff` aus.
#[must_use]
pub fn run_diff(root: &Path, args: &DiffArgs) -> ToolOutput {
    with_repo(DIFF, root, |repo, odb, refs| {
        let format = Format::from_name(choice(
            "format",
            args.format.as_deref(),
            Format::NAMES,
            "patch",
        )?)
        .ok_or("invalid format")?;
        let spec = spec_of(args.paths.as_ref())?;
        let staged = flag(args.staged);
        let opts = RenderOpts {
            format,
            context: args.context.unwrap_or(3).min(MAX_CONTEXT),
            ignore_whitespace: flag(args.ignore_whitespace),
            max_files: limit_or(
                args.max_files,
                DEFAULT_MAX_FILES,
                crate::diffcore::HARD_MAX_FILES,
            ),
        };
        let revs = Revs::new(odb, refs);
        if args.target.is_some() && args.base.is_none() {
            return Err("target requires base".to_owned());
        }
        if args.target.is_some() && staged {
            return Err("staged cannot be combined with target".to_owned());
        }
        let tree_of = |spec_text: &str| -> Result<crate::oid::Oid, String> {
            revs.peel_tree(revs.object(spec_text)?)
        };
        let (mode, report) = if let (Some(base), Some(target)) = (&args.base, &args.target) {
            let (old, new) = (tree_of(base)?, tree_of(target)?);
            (
                "tree_vs_tree",
                tree_report(odb, &repo.scope, Some(&old), Some(&new), &spec, &opts)?,
            )
        } else {
            let base_snapshot = match &args.base {
                Some(base) => Some(from_flat(flatten(odb, &tree_of(base)?, &spec)?)),
                None if staged => match refs.head()?.oid() {
                    Some(oid) => Some(from_flat(flatten(
                        odb,
                        &read_commit(odb, &oid)?.tree,
                        &spec,
                    )?)),
                    None => Some(Default::default()),
                },
                None => None,
            };
            let index = read_index(repo)?.unwrap_or_default();
            let view = from_index(&index, &spec);
            let reader = Reader {
                odb,
                scope: &repo.scope,
            };
            if staged {
                let mut old = base_snapshot.unwrap_or_default();
                for path in view.conflicts.keys() {
                    old.remove(path);
                }
                (
                    if args.base.is_some() {
                        "base_vs_staged"
                    } else {
                        "staged"
                    },
                    render(&reader, &changes(&old, &view.snapshot), &opts)?,
                )
            } else {
                let ctx = WorktreeCtx {
                    repo,
                    deadline: deadline(),
                    index_mtime: index_mtime(repo),
                };
                let mut paths: std::collections::BTreeSet<&[u8]> =
                    view.entries.keys().copied().collect();
                if let Some(snapshot) = &base_snapshot {
                    paths.extend(
                        snapshot
                            .keys()
                            .map(Vec::as_slice)
                            .filter(|p| !view.conflicts.contains_key(*p)),
                    );
                }
                let wt = worktree(
                    &ctx,
                    paths.into_iter().map(|p| (p, view.entries.get(p).copied())),
                )?;
                let mut old = base_snapshot
                    .clone()
                    .unwrap_or_else(|| view.snapshot.clone());
                for path in view.conflicts.keys() {
                    old.remove(path);
                }
                (
                    if args.base.is_some() {
                        "base_vs_worktree"
                    } else {
                        "unstaged"
                    },
                    render(&reader, &changes(&old, &wt), &opts)?,
                )
            }
        };
        let mut data = json!({"comparison": mode});
        let summary = format!(
            "{} file(s) changed (+{} -{})",
            report.total_files, report.additions, report.deletions
        );
        let truncated = report.files_truncated || report.patch_truncated;
        report.into_json(format, &mut data);
        data["truncated"] = json!(truncated);
        Ok(ok(DIFF, summary, data))
    })
}

/// Vergleicht Stände (`git diff`).
#[harw_macros::tool(
    name = "git.diff",
    description = "Shows changes like git diff, read-only and in pure Rust. Default: index vs working tree (unstaged); staged=true: HEAD vs index (git diff --cached); base: that commit vs working tree/index; base+target: two commits. Options: paths (literal), format (patch|stat|name_only|name_status), context, ignore_whitespace, max_files. Returns JSON {comparison, files[{path,status,additions,deletions,binary}], patch, truncated}. The patch is capped at 40 KiB; secret files (.env, keys) are listed but their contents are never shown; binary files only as 'Binary files differ'; no rename detection.",
    permission = "read_workspace",
    parallel_safe
)]
async fn git_diff(
    context: &ToolExecutionContext,
    args: DiffArgs,
) -> Result<ToolOutput, ToolsError> {
    let root = workspace_root(context);
    run_blocking(DIFF, move || run_diff(&root, &args)).await
}

// ---------------------------------------------------------------- log

/// Name von `git.log`.
pub const LOG: &str = "git.log";

/// Argumente für `git.log`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct LogArgs {
    /// Revision to start from (default HEAD), or a range "A..B".
    #[serde(default)]
    pub rev: Option<String>,
    /// Maximum number of commits (default 20, maximum 500).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub max_count: Option<usize>,
    /// Skip this many matching commits first.
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub skip: Option<usize>,
    /// Only commits whose author name or email contains this text (case-insensitive).
    #[serde(default)]
    pub author: Option<String>,
    /// Only commits whose message contains this text (case-insensitive).
    #[serde(default)]
    pub grep: Option<String>,
    /// Only commits on or after this date (YYYY-MM-DD, YYYY-MM-DDTHH:MM:SS or unix seconds, UTC).
    #[serde(default)]
    pub since: Option<String>,
    /// Only commits on or before this date (same formats as since).
    #[serde(default)]
    pub until: Option<String>,
    /// Only commits that change these workspace-relative literal paths.
    #[serde(default)]
    pub paths: Option<Vec<String>>,
    /// Follow only the first parent of merges.
    #[serde(default)]
    pub first_parent: Option<bool>,
    /// Leave out merge commits.
    #[serde(default)]
    pub no_merges: Option<bool>,
}

fn date_arg(field: &str, value: Option<&String>) -> Result<Option<i64>, String> {
    value.map(|text| parse_when(text).ok_or_else(|| format!("invalid {field} date '{}': use YYYY-MM-DD, YYYY-MM-DDTHH:MM:SS or unix seconds", text.chars().take(40).collect::<String>()))).transpose()
}

/// Führt `git.log` aus.
#[must_use]
pub fn run_log(root: &Path, args: &LogArgs) -> ToolOutput {
    with_repo(LOG, root, |_, odb, refs| {
        let opts = LogOpts {
            rev: args
                .rev
                .clone()
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| "HEAD".to_owned()),
            max_count: limit_or(args.max_count, DEFAULT_MAX_COUNT, log::HARD_MAX_COUNT),
            skip: args.skip.unwrap_or(0).min(1_000_000),
            author: args.author.clone().filter(|a| !a.is_empty()),
            grep: args.grep.clone().filter(|a| !a.is_empty()),
            since: date_arg("since", args.since.as_ref())?,
            until: date_arg("until", args.until.as_ref())?,
            spec: spec_of(args.paths.as_ref())?,
            first_parent: flag(args.first_parent),
            no_merges: flag(args.no_merges),
        };
        let result = log::run(odb, refs, &opts)?;
        let summary = format!(
            "{} commit(s){}",
            result.commits.len(),
            if result.truncated {
                ", more available"
            } else {
                ""
            }
        );
        Ok(ok(
            LOG,
            summary,
            json!({"rev": opts.rev, "commits": result.commits, "truncated": result.truncated, "stop_reason": result.reason}),
        ))
    })
}

/// Zeigt die Commit-Historie (`git log`).
#[harw_macros::tool(
    name = "git.log",
    description = "Lists commit history like git log, read-only and in pure Rust. Options: rev (default HEAD, or A..B), max_count (default 20, max 500), skip, author, grep, since/until (YYYY-MM-DD), paths (commits touching them), first_parent, no_merges. Returns JSON {commits[{commit,short,parents,author,committer,subject,body}], truncated}. Newest committer date first. Use for 'who changed what when'; no history simplification or rename following.",
    permission = "read_workspace",
    parallel_safe
)]
async fn git_log(context: &ToolExecutionContext, args: LogArgs) -> Result<ToolOutput, ToolsError> {
    let root = workspace_root(context);
    run_blocking(LOG, move || run_log(&root, &args)).await
}

// ---------------------------------------------------------------- show

/// Name von `git.show`.
pub const SHOW: &str = "git.show";

/// Argumente für `git.show`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct ShowArgs {
    /// What to show: a commit-ish (default HEAD), a tag, a tree, or <rev>:<path> for a file or directory at that revision.
    #[serde(default)]
    pub rev: Option<String>,
    /// Diff output for commits: "patch" (default), "stat", "name_only" or "name_status".
    #[serde(default)]
    pub format: Option<String>,
    /// Include the diff of a commit (default true).
    #[serde(default)]
    pub diff: Option<bool>,
    /// Lines of context (default 3, maximum 50).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub context: Option<usize>,
    /// Ignore all whitespace in the diff.
    #[serde(default)]
    pub ignore_whitespace: Option<bool>,
    /// Restrict the commit diff to these literal paths.
    #[serde(default)]
    pub paths: Option<Vec<String>>,
    /// Maximum number of files in the diff (default 200, maximum 2000).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub max_files: Option<usize>,
}

/// Führt `git.show` aus.
#[must_use]
pub fn run_show(root: &Path, args: &ShowArgs) -> ToolOutput {
    with_repo(SHOW, root, |repo, odb, refs| {
        let opts = ShowOpts {
            rev: args
                .rev
                .clone()
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| "HEAD".to_owned()),
            format: Format::from_name(choice(
                "format",
                args.format.as_deref(),
                Format::NAMES,
                "patch",
            )?)
            .ok_or("invalid format")?,
            diff: args.diff.unwrap_or(true),
            context: args.context.unwrap_or(3).min(MAX_CONTEXT),
            ignore_whitespace: flag(args.ignore_whitespace),
            spec: spec_of(args.paths.as_ref())?,
            max_files: limit_or(
                args.max_files,
                DEFAULT_MAX_FILES,
                crate::diffcore::HARD_MAX_FILES,
            ),
        };
        let data = show::run(odb, refs, &repo.scope, &opts)?;
        let summary = format!(
            "{} {}",
            data["kind"].as_str().unwrap_or("object"),
            opts.rev.chars().take(64).collect::<String>()
        );
        let truncated = data["truncated"].as_bool().unwrap_or(false)
            || data["patch_truncated"].as_bool().unwrap_or(false)
            || data["files_truncated"].as_bool().unwrap_or(false);
        let mut data = data;
        data["truncated"] = json!(truncated);
        Ok(ok(SHOW, summary, data))
    })
}

/// Zeigt Objekte (`git show`).
#[harw_macros::tool(
    name = "git.show",
    description = "Shows a git object like git show, read-only and in pure Rust: a commit (header, message and diff against its first parent), an annotated tag, a tree listing, or a file at a revision via rev=<rev>:<path> (e.g. HEAD~2:src/lib.rs). Options: format (patch|stat|name_only|name_status), diff, context, paths, max_files. Returns JSON with kind and content; blobs capped at 32 KiB, patches at 40 KiB (see truncated). Secret files (.env, keys) are never shown.",
    permission = "read_workspace",
    parallel_safe
)]
async fn git_show(
    context: &ToolExecutionContext,
    args: ShowArgs,
) -> Result<ToolOutput, ToolsError> {
    let root = workspace_root(context);
    run_blocking(SHOW, move || run_show(&root, &args)).await
}

// ---------------------------------------------------------------- branch

/// Name von `git.branch`.
pub const BRANCH: &str = "git.branch";

/// Argumente für `git.branch`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct BranchArgs {
    /// Also list remote-tracking branches (refs/remotes).
    #[serde(default)]
    pub remotes: Option<bool>,
    /// Also list tags.
    #[serde(default)]
    pub tags: Option<bool>,
    /// Only names matching this pattern (* and ? wildcards).
    #[serde(default)]
    pub pattern: Option<String>,
    /// Maximum number of entries (default 200, maximum 2000).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub limit: Option<usize>,
}

/// Führt `git.branch` aus.
#[must_use]
pub fn run_branch(root: &Path, args: &BranchArgs) -> ToolOutput {
    with_repo(BRANCH, root, |repo, odb, refs| {
        let opts = BranchOpts {
            remotes: flag(args.remotes),
            tags: flag(args.tags),
            pattern: args.pattern.clone().filter(|p| !p.is_empty()),
            limit: limit_or(args.limit, branch::DEFAULT_LIMIT, branch::HARD_LIMIT),
        };
        let data = branch::run(repo, odb, refs, &opts)?;
        let summary = format!("{} ref(s)", data["total"]);
        Ok(ok(BRANCH, summary, data))
    })
}

/// Listet Zweige (`git branch`).
#[harw_macros::tool(
    name = "git.branch",
    description = "Lists branches like git branch -vv, read-only and in pure Rust: local branches with tip, subject, date, current marker and upstream ahead/behind; optionally remote-tracking branches and tags. Options: remotes, tags, pattern (* and ?), limit. Returns JSON {head, branches[], total, truncated}. Cannot create, delete or switch branches. Remote URLs are never shown.",
    permission = "read_workspace",
    parallel_safe
)]
async fn git_branch(
    context: &ToolExecutionContext,
    args: BranchArgs,
) -> Result<ToolOutput, ToolsError> {
    let root = workspace_root(context);
    run_blocking(BRANCH, move || run_branch(&root, &args)).await
}

// ---------------------------------------------------------------- blame

/// Name von `git.blame`.
pub const BLAME: &str = "git.blame";

/// Argumente für `git.blame`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct BlameArgs {
    /// Workspace-relative path of the file to blame.
    pub path: String,
    /// Revision to blame (default HEAD); the committed file at that revision is used, not the working tree.
    #[serde(default)]
    pub rev: Option<String>,
    /// First line (1-based, default 1).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub start_line: Option<usize>,
    /// Last line (inclusive, default end of file).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub end_line: Option<usize>,
    /// Maximum number of lines returned (default 200, maximum 2000).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub max_lines: Option<usize>,
}

/// Führt `git.blame` aus.
#[must_use]
pub fn run_blame(root: &Path, args: &BlameArgs) -> ToolOutput {
    with_repo(BLAME, root, |_, odb, refs| {
        let opts = BlameOpts {
            path: args.path.clone(),
            rev: args
                .rev
                .clone()
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| "HEAD".to_owned()),
            start_line: args.start_line,
            end_line: args.end_line,
            max_lines: limit_or(args.max_lines, blame::DEFAULT_LINES, blame::HARD_LINES),
        };
        let data = blame::run(odb, refs, &opts)?;
        let summary = format!(
            "{} line(s) of {}",
            data["lines"].as_array().map_or(0, Vec::len),
            opts.path.chars().take(128).collect::<String>()
        );
        Ok(ok(BLAME, summary, data))
    })
}

/// Zeilenherkunft (`git blame`).
#[harw_macros::tool(
    name = "git.blame",
    description = "Shows which commit last changed each line of a file, like git blame, read-only and in pure Rust. Arguments: path (required), rev (default HEAD), start_line/end_line, max_lines (default 200, max 2000). Returns JSON {lines[{line,commit,text}], commits{short->{commit,author,email,date,summary}}, truncated, approximate}. Blames the committed file (not uncommitted edits); does not follow renames or moved code; secret files are refused.",
    permission = "read_workspace",
    parallel_safe
)]
async fn git_blame(
    context: &ToolExecutionContext,
    args: BlameArgs,
) -> Result<ToolOutput, ToolsError> {
    let root = workspace_root(context);
    run_blocking(BLAME, move || run_blame(&root, &args)).await
}

harw_tools::tool_provider! {
    /// Stellt die rein lesenden Git-Werkzeuge `git.*` bereit
    /// (`status`, `diff`, `log`, `show`, `branch`, `blame`).
    pub struct GitReadToolProvider {
        GitStatusTool,
        GitDiffTool,
        GitLogTool,
        GitShowTool,
        GitBranchTool,
        GitBlameTool,
    }
}

/// Namen aller Werkzeuge dieses Providers (für Profil-Listen).
pub const GIT_TOOL_NAMES: &[&str] = GitReadToolProvider::TOOL_NAMES;
