//! `/diff` — read-only git diff operation.
//!
//! # Responsibility
//! Implements `/diff` as a channel-parity command and read-only model tool.
//! `git` runs as a **read-only helper process** in its own Bubblewrap sandbox
//! ([`harw_sandbox::BwrapLauncher::plan_read_only`]), not through
//! `shell.exec`. The tool therefore needs only `ReadWorkspace` — the same
//! right `fs.read` uses — and works in the read-only `plan`/`explore` modes,
//! which never grant `ExecuteProcess` (Ladybird export: "shell.exec denied:
//! ExecuteProcess permission missing").
//!
//! # Security boundary
//! - **Fixed argv allowlist.** Only a plan built here is ever run, and
//!   [`ensure_allowlisted`] re-checks it right before the launch: the exact
//!   hardening prefix [`GIT_HARDENING_PREFIX`] followed by one of
//!   [`ALLOWED_GIT_SUBCOMMANDS`] (`diff`, `status`, `log`, `show`,
//!   `rev-parse`). Nothing the model sends becomes a git option: the only
//!   model input is an optional path, validated by
//!   [`validate_relative_pathspec`] and bound as `:(literal)<path>` after
//!   `--`.
//! - **Hardened git.** `-c core.fsmonitor= -c core.hooksPath=/dev/null
//!   -c diff.external= -c core.pager=cat --no-pager diff --no-textconv
//!   --no-ext-diff --no-color` neutralises the config- and attribute-driven
//!   code-execution paths a bare `git diff` would honour from repo-local
//!   state; `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null` and
//!   `GIT_ATTR_NOSYSTEM=1` keep host-wide config out; `GIT_OPTIONAL_LOCKS=0`
//!   stops git from refreshing `.git/index` (a read must not write);
//!   `GIT_TERMINAL_PROMPT=0` and `GIT_PAGER=cat` keep it non-interactive.
//! - **Read-only sandbox.** The Bubblewrap plan binds the workspace with
//!   `--ro-bind` regardless of `WriteWorkspace`, has no network, a cleared
//!   environment and an empty tmpfs `/tmp`. Anything a repository might still
//!   trigger (e.g. a clean filter from `.gitattributes`) runs without write
//!   access and without network. Only the workspace is visible: a repository
//!   above the workspace root is out of reach by design.
//! - **Bounded.** stdout is capped at [`MAX_OUTPUT_BYTES`] (the reader is
//!   dropped at the cap, git ends on `EPIPE`), stderr at
//!   [`MAX_STDERR_BYTES`], the whole run at [`GIT_TIMEOUT`]; the child is
//!   killed on drop.
//!
//! Because the process is sandboxed read-only, the operation stays
//! `approval = "always"` as before (F-022/F-191/G-027); only the permission
//! it needs changed.
//!
//! # Output and errors
//! - Workspace without `.git`: [`not_a_repo_message`] — "Kein Git-Repository
//!   in <root>; diff braucht ein Repo (Unterordner: …)", listing the direct
//!   subfolders that are repositories (holy export: a folder with several
//!   projects gave the opaque "git diff exited with status 129", which is
//!   git's usage error for an implicit `--no-index` outside a repository).
//!   The same message is used if git itself reports "not a git repository".
//! - Any other non-zero exit carries git's (bounded) stderr, never stdout.
//! - A truncated diff is a success with a trailing "Diff gekürzt bei 64 KiB"
//!   note.

use harw_authority::{Permission, SandboxSpec};
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_sandbox::{BwrapLauncher, SandboxError};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};

/// Upper bound of the rendered diff (stdout) in bytes.
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Upper bound of git's stderr kept for an error message.
const MAX_STDERR_BYTES: usize = 4 * 1024;

/// Wall-clock limit of one git run (including sandbox setup).
const GIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Fixed places where `git` is looked up (never `PATH`); all lie under the
/// directories the read-only sandbox binds (`/usr`, `/bin`).
const GIT_CANDIDATES: [&str; 3] = ["/usr/bin/git", "/usr/local/bin/git", "/bin/git"];

/// The only git subcommands this operation may ever run.
const ALLOWED_GIT_SUBCOMMANDS: [&str; 5] = ["diff", "status", "log", "show", "rev-parse"];

/// At most this many repository subfolders are named in the "no repository"
/// message.
const MAX_LISTED_SUBREPOS: usize = 20;

/// Global options every git invocation starts with, in exactly this order.
/// See the module doc for why each one is present; none are optional.
const GIT_HARDENING_PREFIX: [&str; 10] = [
    "git",
    "-c",
    "core.fsmonitor=",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "diff.external=",
    "-c",
    "core.pager=cat",
    "--no-pager",
];

/// Subcommand and flags of the hardened `git diff` after the prefix.
const GIT_DIFF_SUBCOMMAND: [&str; 4] = ["diff", "--no-textconv", "--no-ext-diff", "--no-color"];

/// Environment applied only inside the git sandbox (after `--clearenv`).
const GIT_DIFF_ENV: &[(&str, &str)] = &[
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_ATTR_NOSYSTEM", "1"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_OPTIONAL_LOCKS", "0"),
    ("GIT_PAGER", "cat"),
];

/// Argument container for the `/diff` operation.
#[derive(Debug, Default, serde::Deserialize, harw_macros::OpArgs)]
pub struct DiffArgs {
    /// Optional workspace-relative path filter.
    #[serde(default)]
    pub path: Option<String>,
    /// When true, request only `git diff --stat`.
    #[serde(default)]
    pub stat_only: bool,
}

impl harw_operations::FromRawArgs for DiffArgs {
    fn from_raw_args(tokens: &[String]) -> Result<Self, harw_operations::OpError> {
        let mut stat_only = false;
        let mut path = None;
        for token in tokens {
            match token.as_str() {
                "--stat" | "--stat-only" => stat_only = true,
                other if other.starts_with("--") => {
                    return Err(harw_operations::OpError::InvalidArguments(format!(
                        "unbekanntes Flag für /diff: {other}"
                    )));
                }
                other if path.is_none() => path = Some(other.to_owned()),
                other => {
                    return Err(harw_operations::OpError::InvalidArguments(format!(
                        "unerwartetes zusätzliches Argument für /diff: {other}"
                    )));
                }
            }
        }
        Ok(Self { stat_only, path })
    }
}

/// A pure, side-effect-free plan for one git run: argv (with the literal
/// `git` as `argv[0]`, replaced by the pinned binary at launch) and the
/// environment set inside the sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GitDiffPlan {
    /// argv in exec order; `argv[0] == "git"`.
    argv: Vec<String>,
    /// `(name, value)` pairs applied only to this invocation.
    env: Vec<(&'static str, &'static str)>,
}

/// Build the [`GitDiffPlan`] for one `/diff` call. Pure function: no I/O, no
/// sandbox access — everything needed comes from `args`.
fn build_git_diff_plan(args: &DiffArgs) -> Result<GitDiffPlan, OpError> {
    let mut argv: Vec<String> = GIT_HARDENING_PREFIX
        .iter()
        .chain(GIT_DIFF_SUBCOMMAND.iter())
        .map(|token| (*token).to_owned())
        .collect();
    if args.stat_only {
        argv.push("--stat".to_owned());
    }
    if let Some(path) = &args.path {
        let literal = validate_relative_pathspec(path)?;
        argv.push("--".to_owned());
        argv.push(format!(":(literal){literal}"));
    }
    Ok(GitDiffPlan {
        argv,
        env: GIT_DIFF_ENV.to_vec(),
    })
}

/// Validate a workspace-relative git pathspec before it is bound with
/// `:(literal)`. Rejects anything that is not a plain relative path:
/// - a leading `/` (absolute) or a `..` component (parent traversal);
/// - a leading `:` — git's pathspec "magic" prefix (`:/`, `:(top)`,
///   `:(glob)`, `:(exclude)`, the short form `::`, …). `:(literal)` is
///   applied once by [`build_git_diff_plan`], over the validated tail; a
///   caller-supplied leading `:` is rejected outright rather than trusted to
///   be neutralised by nesting inside that wrapper;
/// - an embedded NUL.
fn validate_relative_pathspec(path: &str) -> Result<&str, OpError> {
    if path.is_empty()
        || path.contains('\0')
        || path.starts_with('/')
        || path.starts_with(':')
        || path.split('/').any(|component| component == "..")
    {
        return Err(OpError::InvalidArguments(
            "/diff path must be a non-empty workspace-relative path without `..`, a leading `/`, \
             or a leading `:` (git pathspec magic)"
                .to_owned(),
        ));
    }
    Ok(path)
}

/// Checks the fixed argv allowlist: exactly [`GIT_HARDENING_PREFIX`], then one
/// of [`ALLOWED_GIT_SUBCOMMANDS`]. Returns the subcommand.
///
/// # Errors
/// [`OpError::Execution`] for any other argv — defense in depth, since every
/// plan is built in this module.
fn ensure_allowlisted(argv: &[String]) -> Result<&str, OpError> {
    let prefix_ok = argv.len() > GIT_HARDENING_PREFIX.len()
        && argv
            .iter()
            .zip(GIT_HARDENING_PREFIX)
            .all(|(token, expected)| token == expected);
    match argv.get(GIT_HARDENING_PREFIX.len()).map(String::as_str) {
        Some(subcommand) if prefix_ok && ALLOWED_GIT_SUBCOMMANDS.contains(&subcommand) => {
            Ok(subcommand)
        }
        other => Err(OpError::Execution(format!(
            "diff: git-Aufruf außerhalb der festen Erlaubnisliste abgelehnt ({other:?}; erlaubt: \
             {})",
            ALLOWED_GIT_SUBCOMMANDS.join(", ")
        ))),
    }
}

/// First trusted `git` binary from [`GIT_CANDIDATES`].
fn find_git() -> Option<PathBuf> {
    GIT_CANDIDATES
        .iter()
        .map(PathBuf::from)
        .find(|candidate| candidate.is_file())
}

/// Whether `dir` itself carries a `.git` entry (directory, gitfile or link).
fn has_git_entry(dir: &Path) -> bool {
    dir.join(".git").symlink_metadata().is_ok()
}

/// Direct subfolders of `root` (no symlinks) that are git repositories,
/// sorted, at most [`MAX_LISTED_SUBREPOS`].
fn repository_subfolders(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut repos: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| has_git_entry(&entry.path()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    repos.sort();
    repos.truncate(MAX_LISTED_SUBREPOS);
    repos
}

/// The clear "no repository" message for `root`.
fn not_a_repo_message(root: &Path) -> String {
    let repos = repository_subfolders(root);
    let subfolders = if repos.is_empty() {
        "Unterordner: keiner ist ein Git-Repository".to_owned()
    } else {
        format!("Unterordner: {}", repos.join(", "))
    };
    let mut message = format!(
        "Kein Git-Repository in {}; diff braucht ein Repo ({subfolders})",
        root.display()
    );
    if root.ancestors().skip(1).any(has_git_entry) {
        message.push_str(
            "; ein Repository oberhalb des Arbeitsbereichs ist in der Sandbox nicht sichtbar",
        );
    }
    message
}

/// Result of one bounded git run.
#[derive(Debug, Default)]
struct GitOutput {
    /// Exit code; `None` when git ended by a signal (e.g. `SIGPIPE` after
    /// the stdout cap).
    exit_code: Option<i32>,
    stdout: Vec<u8>,
    /// stdout hit [`MAX_OUTPUT_BYTES`]; the rest was dropped.
    stdout_truncated: bool,
    stderr: Vec<u8>,
}

/// Reads at most `cap` bytes; `true` when more was available.
async fn read_capped<R: AsyncRead + Unpin>(reader: R, cap: usize) -> (Vec<u8>, bool) {
    let mut buffer = Vec::new();
    let limit = u64::try_from(cap).unwrap_or(u64::MAX).saturating_add(1);
    // A read error just ends the capture; the exit status tells the rest.
    let _ = reader.take(limit).read_to_end(&mut buffer).await;
    let truncated = buffer.len() > cap;
    buffer.truncate(cap);
    (buffer, truncated)
}

/// Runs an allowlisted git plan read-only in its own sandbox.
///
/// # Errors
/// [`OpError::Execution`] when the argv is not allowlisted, `ReadWorkspace`
/// is missing, no git/bubblewrap is available, the start fails or the run
/// exceeds [`GIT_TIMEOUT`].
async fn run_read_only_git(
    sandbox: &SandboxSpec,
    plan: &GitDiffPlan,
) -> Result<GitOutput, OpError> {
    ensure_allowlisted(&plan.argv)?;
    let git = find_git().ok_or_else(|| {
        OpError::Execution(format!(
            "diff: kein git gefunden (gesucht: {})",
            GIT_CANDIDATES.join(", ")
        ))
    })?;
    let launcher = BwrapLauncher::discover().map_err(|error| {
        OpError::Execution(format!(
            "diff: Lese-Sandbox (bubblewrap) nicht verfügbar: {error}"
        ))
    })?;
    let mut command: Vec<OsString> = Vec::with_capacity(plan.argv.len());
    command.push(git.into_os_string());
    command.extend(plan.argv.iter().skip(1).map(OsString::from));
    let bwrap_plan = launcher
        .plan_read_only(sandbox, &plan.env, &command)
        .map_err(|error| match error {
            SandboxError::ProcessExecutionDenied => {
                OpError::Execution("diff: Berechtigung ReadWorkspace fehlt".to_owned())
            }
            other => OpError::Execution(format!("diff: Lese-Sandbox abgelehnt: {other}")),
        })?;
    let mut child = tokio::process::Command::new(launcher.executable())
        .args(bwrap_plan.args())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| OpError::Execution(format!("diff: git-Start fehlgeschlagen: {error}")))?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(OpError::Execution(
            "diff: git-Ausgabekanäle fehlen".to_owned(),
        ));
    };
    let run = async {
        let ((stdout, stdout_truncated), (stderr, _)) = tokio::join!(
            read_capped(stdout, MAX_OUTPUT_BYTES),
            read_capped(stderr, MAX_STDERR_BYTES)
        );
        let status = child.wait().await;
        (stdout, stdout_truncated, stderr, status)
    };
    match tokio::time::timeout(GIT_TIMEOUT, run).await {
        Ok((stdout, stdout_truncated, stderr, status)) => {
            let status = status
                .map_err(|error| OpError::Execution(format!("diff: Warten auf git: {error}")))?;
            Ok(GitOutput {
                exit_code: status.code(),
                stdout,
                stdout_truncated,
                stderr,
            })
        }
        // `child` fällt mit `kill_on_drop` und wird beendet.
        Err(_elapsed) => Err(OpError::Execution(format!(
            "diff: git antwortete nicht innerhalb von {} s",
            GIT_TIMEOUT.as_secs()
        ))),
    }
}

/// Renders a finished git run. A non-zero exit is an error carrying git's
/// stderr (never stdout); a truncated diff is a success with a note.
fn render_git_output(output: &GitOutput, root: &Path) -> Result<String, OpError> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if output.exit_code != Some(0) && !output.stdout_truncated {
        if stderr.contains("not a git repository") {
            return Err(OpError::Execution(not_a_repo_message(root)));
        }
        let status = output
            .exit_code
            .map_or_else(|| "Signal".to_owned(), |code| code.to_string());
        return Err(OpError::Execution(if stderr.is_empty() {
            format!("git diff exited with status {status}")
        } else {
            format!("git diff exited with status {status}: {stderr}")
        }));
    }
    let body = match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => "git diff produced no output.".to_owned(),
        (false, true) => stdout.into_owned(),
        (true, false) => format!("stderr:\n{stderr}"),
        (false, false) => format!("{stdout}\nstderr:\n{stderr}"),
    };
    if output.stdout_truncated {
        Ok(format!(
            "{body}\n[Diff gekürzt bei 64 KiB: Ausgabegrenze erreicht]"
        ))
    } else {
        Ok(body)
    }
}

/// Show the current workspace's git diff through a read-only, sandboxed git
/// process (see the module doc). Needs only `ReadWorkspace`.
#[operation(
    name = "diff",
    summary = "Zeigt den aktuellen git-Diff des Workspaces.",
    domain = "knowledge",
    permission = "observer",
    command(path = "/diff", visibility = "channel_parity", busy = "immediate"),
    // `diff` starts a real (read-only, sandboxed) git process. Repo-controlled
    // git config/attributes are neutralised above, and the sandbox has no
    // write access and no network; every model-initiated call still requires
    // explicit approval (F-022/F-191/G-027).
    model_tool(readonly, approval = "always"),
    // Web-Fläche übernimmt dieselbe Achse wie das ModelTool: `git diff` ist
    // ein reiner Lesevorgang auf dem Workspace, keine Mutation.
    web(path = "/api/diff", method = "get", approval = "none")
)]
async fn diff(ctx: &OpContext, args: DiffArgs) -> Result<OpOutput, OpError> {
    let plan = build_git_diff_plan(&args)?;
    let sandbox = ctx.sandbox();
    if !sandbox.permissions().contains(Permission::ReadWorkspace) {
        return Err(OpError::Execution(
            "diff: Berechtigung ReadWorkspace fehlt".to_owned(),
        ));
    }
    let root = sandbox.workspace().canonical_root();
    if !has_git_entry(root) {
        return Err(OpError::Execution(not_a_repo_message(root)));
    }
    let output = run_read_only_git(sandbox, &plan).await?;
    Ok(OpOutput::from(render_git_output(&output, root)?))
}

#[cfg(test)]
mod tests {
    use super::{
        ALLOWED_GIT_SUBCOMMANDS, DiffArgs, DiffOperation, GIT_DIFF_ENV, GIT_DIFF_SUBCOMMAND,
        GIT_HARDENING_PREFIX, GitOutput, build_git_diff_plan, diff, ensure_allowlisted, find_git,
        not_a_repo_message, render_git_output, validate_relative_pathspec,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_operations::{
        ApprovalPolicy, FromRawArgs, OpContext, OpError, Operation, Surface, context::ServiceMap,
    };
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::{
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    /// Context with `permissions` over a fresh `<tmp>/…/workspace`; returns
    /// the context, the temp base (to remove) and the canonical workspace.
    fn test_context(permissions: &[Permission]) -> TestResult<(OpContext, PathBuf, PathBuf)> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("harw-diff-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("workspace")).map_err(ctx("create test workspace"))?;
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )
        .map_err(ctx("build workspace registry"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("workspace"),
            )
            .map_err(ctx("resolve workspace binding"))?;
        let workspace = binding.canonical_root().to_path_buf();
        Ok((
            OpContext::new(
                SessionId::new(),
                TurnId::new(),
                SandboxSpec::from_resolved(
                    binding,
                    PermissionSet::from_policy(permissions.iter().copied()),
                ),
                ServiceMap::new(),
            ),
            root,
            workspace,
        ))
    }

    fn argv(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|token| (*token).to_owned()).collect()
    }

    #[test]
    fn test_diff_args_from_raw_args_stat_flag_only() -> TestResult {
        let args = DiffArgs::from_raw_args(&toks(&["--stat"])).map_err(ctx("parse args"))?;
        assert!(args.stat_only);
        assert!(args.path.is_none());
        Ok(())
    }

    #[test]
    fn test_diff_args_from_raw_args_stat_flag_and_path() -> TestResult {
        let args = DiffArgs::from_raw_args(&toks(&["--stat", "src"])).map_err(ctx("parse args"))?;
        assert!(args.stat_only);
        assert_eq!(args.path.as_deref(), Some("src"));
        Ok(())
    }

    #[test]
    fn test_diff_args_from_raw_args_no_tokens_defaults() -> TestResult {
        let args = DiffArgs::from_raw_args(&toks(&[])).map_err(ctx("parse args"))?;
        assert!(!args.stat_only);
        assert!(args.path.is_none());
        Ok(())
    }

    #[test]
    fn test_diff_args_from_raw_args_unknown_flag_returns_err() {
        assert!(matches!(
            DiffArgs::from_raw_args(&toks(&["--bogus"])),
            Err(OpError::InvalidArguments(_))
        ));
    }

    #[test]
    fn test_diff_args_from_raw_args_extra_token_after_path_returns_err() {
        assert!(matches!(
            DiffArgs::from_raw_args(&toks(&["src", "extra"])),
            Err(OpError::InvalidArguments(_))
        ));
    }

    #[test]
    fn test_build_git_diff_plan_defaults_have_hardening_argv_and_env() -> TestResult {
        let plan = build_git_diff_plan(&DiffArgs::default()).map_err(ctx("build plan"))?;

        // Exact argv for the default case: pins every hardening flag from the
        // module doc so a future edit cannot silently drop one.
        let expected_argv: Vec<String> = GIT_HARDENING_PREFIX
            .iter()
            .chain(GIT_DIFF_SUBCOMMAND.iter())
            .map(|token| (*token).to_owned())
            .collect();
        assert_eq!(plan.argv, expected_argv);
        assert_eq!(
            plan.env,
            vec![
                ("GIT_CONFIG_NOSYSTEM", "1"),
                ("GIT_CONFIG_GLOBAL", "/dev/null"),
                ("GIT_ATTR_NOSYSTEM", "1"),
                ("GIT_TERMINAL_PROMPT", "0"),
                ("GIT_OPTIONAL_LOCKS", "0"),
                ("GIT_PAGER", "cat"),
            ]
        );
        assert_eq!(plan.env, GIT_DIFF_ENV.to_vec());
        assert_eq!(
            ensure_allowlisted(&plan.argv).map_err(ctx("allowlist"))?,
            "diff"
        );
        Ok(())
    }

    #[test]
    fn test_build_git_diff_plan_with_stat_and_path_appends_literal_pathspec() -> TestResult {
        let plan = build_git_diff_plan(&DiffArgs {
            path: Some("src".to_owned()),
            stat_only: true,
        })
        .map_err(ctx("build plan"))?;

        let tail = &plan.argv[plan.argv.len() - 3..];
        assert_eq!(tail, ["--stat", "--", ":(literal)src"]);
        Ok(())
    }

    #[test]
    fn test_build_git_diff_plan_rejects_pathspec_magic() -> TestResult {
        for magic_path in [":/", ":(top)src", "::src", ":src"] {
            let result = build_git_diff_plan(&DiffArgs {
                path: Some(magic_path.to_owned()),
                stat_only: false,
            });
            let Err(error) = result else {
                return Err(TestError::Unexpected(
                    "pathspec magic must be rejected".to_owned(),
                ));
            };
            assert!(matches!(error, OpError::InvalidArguments(_)));
        }
        Ok(())
    }

    #[test]
    fn test_validate_relative_pathspec_rejects_workspace_escape() {
        for bad in ["../secrets", "/etc/passwd", "a/../b", ""] {
            assert!(matches!(
                validate_relative_pathspec(bad),
                Err(OpError::InvalidArguments(_))
            ));
        }
    }

    #[test]
    fn test_allowlist_accepts_only_read_only_subcommands_after_the_exact_prefix() {
        for subcommand in ALLOWED_GIT_SUBCOMMANDS {
            let mut tokens: Vec<&str> = GIT_HARDENING_PREFIX.to_vec();
            tokens.push(subcommand);
            assert!(ensure_allowlisted(&argv(&tokens)).is_ok(), "{subcommand}");
        }
        let mut rejected: Vec<Vec<&str>> = ["push", "config", "commit", "-c", "checkout"]
            .into_iter()
            .map(|subcommand| {
                let mut tokens: Vec<&str> = GIT_HARDENING_PREFIX.to_vec();
                tokens.push(subcommand);
                tokens
            })
            .collect();
        // Ein zusätzliches `-c` vor dem Unterbefehl verschiebt das Präfix.
        rejected.push(vec![
            "git",
            "-c",
            "core.pager=less",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "diff.external=",
            "-c",
            "core.pager=cat",
            "--no-pager",
            "diff",
        ]);
        rejected.push(vec!["git", "diff"]);
        rejected.push(GIT_HARDENING_PREFIX.to_vec());
        for tokens in rejected {
            assert!(
                matches!(
                    ensure_allowlisted(&argv(&tokens)),
                    Err(OpError::Execution(_))
                ),
                "{tokens:?}"
            );
        }
    }

    #[test]
    fn test_nonzero_git_status_surfaces_stderr_but_never_stdout() -> TestResult {
        let output = GitOutput {
            exit_code: Some(128),
            stdout: b"sensitive diff content".to_vec(),
            stdout_truncated: false,
            stderr: b"fatal: bad revision 'HEAD'\n".to_vec(),
        };
        let Err(OpError::Execution(message)) = render_git_output(&output, Path::new("/ws")) else {
            return Err(TestError::Unexpected(
                "nonzero git status must fail".to_owned(),
            ));
        };
        assert_eq!(
            message,
            "git diff exited with status 128: fatal: bad revision 'HEAD'"
        );
        assert!(!message.contains("sensitive"));
        Ok(())
    }

    #[test]
    fn test_git_not_a_repository_stderr_maps_to_the_clear_message() -> TestResult {
        let output = GitOutput {
            exit_code: Some(128),
            stderr: b"fatal: not a git repository (or any of the parent directories): .git"
                .to_vec(),
            ..GitOutput::default()
        };
        let Err(OpError::Execution(message)) =
            render_git_output(&output, Path::new("/nonexistent/holy"))
        else {
            return Err(TestError::Unexpected("must fail".to_owned()));
        };
        assert!(
            message.starts_with("Kein Git-Repository in /nonexistent/holy; diff braucht ein Repo"),
            "{message}"
        );
        Ok(())
    }

    #[test]
    fn test_truncated_output_returns_ok_with_notice_even_after_sigpipe() -> TestResult {
        let text = render_git_output(
            &GitOutput {
                exit_code: None,
                stdout: b"diff --git a/x b/x\n+partial".to_vec(),
                stdout_truncated: true,
                stderr: Vec::new(),
            },
            Path::new("/ws"),
        )
        .map_err(ctx("a capped diff is a truncated success"))?;
        assert!(text.starts_with("diff --git a/x b/x\n+partial"));
        assert!(text.contains("Diff gekürzt bei 64 KiB"), "{text}");
        Ok(())
    }

    #[test]
    fn test_clean_run_without_output_says_so() -> TestResult {
        let text = render_git_output(
            &GitOutput {
                exit_code: Some(0),
                ..GitOutput::default()
            },
            Path::new("/ws"),
        )
        .map_err(ctx("clean run"))?;
        assert_eq!(text, "git diff produced no output.");
        Ok(())
    }

    #[test]
    fn test_diff_declares_always_approval_model_tool_surface() {
        let meta = DiffOperation.meta();
        assert_eq!(meta.name, "diff");
        assert!(meta.surfaces.iter().any(|surface| matches!(
            surface,
            Surface::ModelTool {
                readonly: true,
                approval: ApprovalPolicy::Always,
            }
        )));
    }

    /// Holy export: a folder with several projects, no repository at the root.
    #[tokio::test]
    async fn test_diff_in_a_non_repository_names_the_repository_subfolders() -> TestResult {
        let (ctx_, base, workspace) = test_context(&[Permission::ReadWorkspace])?;
        for (folder, git) in [("Aquarium", true), ("Holy-Cow-Alt", true), ("notes", false)] {
            std::fs::create_dir_all(workspace.join(folder)).map_err(ctx("create folder"))?;
            if git {
                std::fs::create_dir_all(workspace.join(folder).join(".git"))
                    .map_err(ctx("create .git"))?;
            }
        }
        let result = diff(
            &ctx_,
            DiffArgs {
                path: None,
                stat_only: true,
            },
        )
        .await;
        std::fs::remove_dir_all(&base).map_err(ctx("remove test workspace"))?;

        let Err(OpError::Execution(message)) = result else {
            return Err(TestError::Unexpected(format!(
                "expected an error, got {result:?}"
            )));
        };
        // Liegt oberhalb des Test-Temp-Verzeichnisses ein `.git` (etwa ein
        // verirrtes `/tmp/.git`), hängt `not_a_repo_message` zu Recht den
        // Hinweis auf das unsichtbare Eltern-Repository an.
        let mut expected = format!(
            "Kein Git-Repository in {}; diff braucht ein Repo (Unterordner: Aquarium, \
             Holy-Cow-Alt)",
            workspace.display()
        );
        if workspace.ancestors().skip(1).any(super::has_git_entry) {
            expected.push_str(
                "; ein Repository oberhalb des Arbeitsbereichs ist in der Sandbox nicht sichtbar",
            );
        }
        assert_eq!(message, expected);
        assert!(!message.contains("129"));
        Ok(())
    }

    #[tokio::test]
    async fn test_diff_without_read_workspace_is_refused() -> TestResult {
        let (ctx_, base, _) = test_context(&[])?;
        let result = diff(&ctx_, DiffArgs::default()).await;
        std::fs::remove_dir_all(&base).map_err(ctx("remove test workspace"))?;
        assert!(
            matches!(&result, Err(OpError::Execution(message)) if message.contains("ReadWorkspace")),
            "{result:?}"
        );
        Ok(())
    }

    #[test]
    fn test_not_a_repo_message_without_repository_subfolders() -> TestResult {
        let (_, base, workspace) = test_context(&[Permission::ReadWorkspace])?;
        let message = not_a_repo_message(&workspace);
        std::fs::remove_dir_all(&base).map_err(ctx("remove test workspace"))?;
        assert!(
            message.contains("(Unterordner: keiner ist ein Git-Repository)"),
            "{message}"
        );
        Ok(())
    }

    /// Ladybird export: in plan mode (`ReadWorkspace` only, no
    /// `ExecuteProcess`) `diff` failed with "ExecuteProcess permission
    /// missing". With the read-only git sandbox it must run. Needs a host with
    /// git and a working bubblewrap; elsewhere only the permission part is
    /// checked.
    #[tokio::test]
    async fn test_diff_works_with_plan_mode_permissions() -> TestResult {
        let (ctx_, base, workspace) = test_context(&[Permission::ReadWorkspace])?;
        let outcome = async {
            let Some(git) = find_git() else {
                return Ok(None);
            };
            let run = |args: &[&str]| {
                std::process::Command::new(&git)
                    .args(args)
                    .current_dir(&workspace)
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .output()
                    .map_err(ctx("run host git for the fixture"))
            };
            std::fs::write(workspace.join("a.txt"), "one\n").map_err(ctx("write a.txt"))?;
            run(&["init", "-q"])?;
            run(&["-c", "user.name=t", "-c", "user.email=t@t", "add", "a.txt"])?;
            run(&[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "init",
            ])?;
            std::fs::write(workspace.join("a.txt"), "one\ntwo\n").map_err(ctx("edit a.txt"))?;
            Ok::<_, TestError>(Some(
                diff(
                    &ctx_,
                    DiffArgs {
                        path: None,
                        stat_only: true,
                    },
                )
                .await,
            ))
        }
        .await;
        std::fs::remove_dir_all(&base).map_err(ctx("remove test workspace"))?;

        match outcome? {
            None => Ok(()),
            Some(Ok(output)) => {
                assert!(output.text.contains("a.txt"), "{}", output.text);
                Ok(())
            }
            Some(Err(OpError::Execution(message)))
                if message.contains("bubblewrap") || message.contains("bwrap") =>
            {
                // Kein nutzbares bwrap (Container ohne User-Namespaces).
                assert!(!message.contains("ExecuteProcess"), "{message}");
                Ok(())
            }
            Some(Err(error)) => Err(TestError::Unexpected(format!(
                "diff must work with ReadWorkspace only: {error:?}"
            ))),
        }
    }
}
