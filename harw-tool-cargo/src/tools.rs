//! Die acht Cargo-Werkzeuge: Argumente, Kommandoaufbau, Ausführung über den
//! [`ShellDelegate`] und Zusammenfassung.
//!
//! Ablauf je Aufruf: Argumente validieren ([`crate::plan`], [`crate::command`])
//! → festes `argv` → [`ShellDelegate::run`] (der `shell.exec`-Ausführer des
//! Harness) → Ausgabe auswerten ([`crate::report`]). Die Ausgabe wird im
//! Kurzformat (`--message-format=short`) angefordert: eine Zeile je Meldung
//! und keine Artefaktzeilen, sodass die 64-KiB-Grenze von `shell.exec` auch bei
//! großen Workspaces nicht die Meldungen verdrängt.

use crate::command;
use crate::guard::{self, Target, Workspace, parse_porcelain_z};
use crate::plan::{Plan, RawSelection, RawTargets};
use crate::report::{
    build_report, failed_report, fmt_report, metadata_report, test_report, tree_report,
};
use crate::shell::{ShellDelegate, ShellRun};
use harw_tool_fsread::budget::fail;
use harw_tool_fsread::scope::Scope;
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use rustix::fs::{AtFlags, unlinkat};
use serde::Deserialize;
use serde_json::Value;
use std::io::Read;
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// Größte Metadaten-Datei, die gelesen wird.
pub const MAX_METADATA_BYTES: u64 = 64 * 1024 * 1024;

/// Arbeitsverzeichnis für Zwischendateien (relativ zur Workspace-Wurzel).
pub const SCRATCH_DIR: &str = "target/harw-tool-cargo";

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Definiert eine Argumentstruktur mit den gemeinsamen Auswahl- und
/// Zieloptionen plus den werkzeugeigenen Feldern.
macro_rules! cargo_args {
    ($(#[$sm:meta])* struct $name:ident { $( $(#[$fm:meta])* $f:ident : [ $($t:tt)+ ] ),* $(,)? }) => {
        $(#[$sm])*
        #[derive(Debug, Deserialize, harw_macros::Tool)]
        #[serde(deny_unknown_fields)]
        pub struct $name {
            /// -p / --package: package(s) to select (names or name@version, at most 64).
            #[serde(default)]
            pub package: Option<Vec<String>>,
            /// --workspace: select every package of the workspace.
            #[serde(default)]
            pub workspace: Option<bool>,
            /// --exclude: packages to skip (needs workspace=true).
            #[serde(default)]
            pub exclude: Option<Vec<String>>,
            /// --features: features to enable (names, pkg/feature or dep:name).
            #[serde(default)]
            pub features: Option<Vec<String>>,
            /// --all-features: enable every feature.
            #[serde(default)]
            pub all_features: Option<bool>,
            /// --no-default-features: do not enable default features.
            #[serde(default)]
            pub no_default_features: Option<bool>,
            /// --all-targets: library, binaries, examples, tests and benches.
            #[serde(default)]
            pub all_targets: Option<bool>,
            /// --lib: only the library.
            #[serde(default)]
            pub lib: Option<bool>,
            /// --bins: all binaries.
            #[serde(default)]
            pub bins: Option<bool>,
            /// --tests: all integration tests.
            #[serde(default)]
            pub tests: Option<bool>,
            /// --examples: all examples.
            #[serde(default)]
            pub examples: Option<bool>,
            /// --benches: all benchmarks.
            #[serde(default)]
            pub benches: Option<bool>,
            /// --bin: binaries by name.
            #[serde(default)]
            pub bin: Option<Vec<String>>,
            /// --release: optimized build. Conflicts with profile.
            #[serde(default)]
            pub release: Option<bool>,
            /// --profile: named build profile. Conflicts with release.
            #[serde(default)]
            pub profile: Option<String>,
            /// --locked: fail if Cargo.lock would change.
            #[serde(default)]
            pub locked: Option<bool>,
            /// --offline: never access the network.
            #[serde(default)]
            pub offline: Option<bool>,
            /// -j / --jobs: parallel jobs (1-64).
            #[serde(default, deserialize_with = "harw_extension_api::lenient::lenient_opt_u64")]
            pub jobs: Option<u64>,
            /// Time limit in seconds (1-3600, default 600; the shell tool may cap it lower).
            #[serde(default, deserialize_with = "harw_extension_api::lenient::lenient_opt_u64")]
            pub timeout_secs: Option<u64>,
            /// Maximum number of diagnostics listed (default 50, hard 500); all are counted.
            #[serde(default, deserialize_with = "harw_extension_api::lenient::lenient_opt_usize")]
            pub max_diagnostics: Option<usize>,
            $( $(#[$fm])* #[serde(default)] pub $f: $($t)+, )*
        }

        impl $name {
            fn raw_selection(&self) -> RawSelection {
                RawSelection {
                    package: self.package.clone(),
                    workspace: self.workspace,
                    exclude: self.exclude.clone(),
                    features: self.features.clone(),
                    all_features: self.all_features,
                    no_default_features: self.no_default_features,
                    release: self.release,
                    profile: self.profile.clone(),
                    locked: self.locked,
                    offline: self.offline,
                    jobs: self.jobs,
                    timeout_secs: self.timeout_secs,
                    max_diagnostics: self.max_diagnostics,
                }
            }

            fn raw_targets(&self) -> RawTargets {
                RawTargets {
                    all_targets: self.all_targets,
                    lib: self.lib,
                    bins: self.bins,
                    tests: self.tests,
                    examples: self.examples,
                    benches: self.benches,
                    bin: self.bin.clone(),
                }
            }
        }
    };
}

cargo_args! {
    /// Argumente für `cargo.check`.
    struct CheckArgs {}
}

cargo_args! {
    /// Argumente für `cargo.build`.
    struct BuildArgs {}
}

cargo_args! {
    /// Argumente für `cargo.clippy`.
    struct ClippyArgs {
        /// --no-deps: lint only the selected packages, not their dependencies.
        no_deps: [Option<bool>],
        /// -D <lint>: lints to deny after '--' (for example 'warnings' or 'clippy::pedantic', at most 64).
        deny: [Option<Vec<String>>],
        /// -W <lint>: lints to warn about after '--'.
        warn: [Option<Vec<String>>],
        /// -A <lint>: lints to allow after '--'.
        allow: [Option<Vec<String>>],
    }
}

cargo_args! {
    /// Argumente für `cargo.test`.
    struct TestArgs {
        /// TESTNAME: run only tests whose name contains this text (letters, digits and _ : . / -).
        test_filter: [Option<String>],
        /// --no-run: compile the tests but do not run them.
        no_run: [Option<bool>],
        /// --doc: only documentation tests.
        doc: [Option<bool>],
        /// --no-fail-fast: keep running after the first failing test binary.
        no_fail_fast: [Option<bool>],
        /// Arguments after '--' for the test harness. Allowlist only: --nocapture, --show-output, --ignored, --include-ignored, --exact, --quiet, -q, --test-threads=N (1-256), --skip=NAME.
        harness_args: [Option<Vec<String>>],
    }
}

/// Argumente für `cargo.doc`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct DocArgs {
    /// -p / --package: package(s) to document (at most 64).
    #[serde(default)]
    pub package: Option<Vec<String>>,
    /// --workspace: document every package of the workspace.
    #[serde(default)]
    pub workspace: Option<bool>,
    /// --exclude: packages to skip (needs workspace=true).
    #[serde(default)]
    pub exclude: Option<Vec<String>>,
    /// --features: features to enable.
    #[serde(default)]
    pub features: Option<Vec<String>>,
    /// --all-features: enable every feature.
    #[serde(default)]
    pub all_features: Option<bool>,
    /// --no-default-features: do not enable default features.
    #[serde(default)]
    pub no_default_features: Option<bool>,
    /// --lib: only the library.
    #[serde(default)]
    pub lib: Option<bool>,
    /// --bins: all binaries.
    #[serde(default)]
    pub bins: Option<bool>,
    /// --no-deps: do not document dependencies.
    #[serde(default)]
    pub no_deps: Option<bool>,
    /// --document-private-items: include private items.
    #[serde(default)]
    pub document_private_items: Option<bool>,
    /// --release: optimized build. Conflicts with profile.
    #[serde(default)]
    pub release: Option<bool>,
    /// --profile: named build profile. Conflicts with release.
    #[serde(default)]
    pub profile: Option<String>,
    /// --locked: fail if Cargo.lock would change.
    #[serde(default)]
    pub locked: Option<bool>,
    /// --offline: never access the network.
    #[serde(default)]
    pub offline: Option<bool>,
    /// -j / --jobs: parallel jobs (1-64).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
    pub jobs: Option<u64>,
    /// Time limit in seconds (1-3600, default 600).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
    pub timeout_secs: Option<u64>,
    /// Maximum number of diagnostics listed (default 50, hard 500).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub max_diagnostics: Option<usize>,
}

/// Argumente für `cargo.fmt_check`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct FmtArgs {
    /// -p / --package: package(s) to check (at most 64).
    #[serde(default)]
    pub package: Option<Vec<String>>,
    /// --all: check every package of the workspace.
    #[serde(default)]
    pub all: Option<bool>,
    /// Time limit in seconds (1-3600, default 600).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
    pub timeout_secs: Option<u64>,
    /// Maximum number of listed hunks (default 50, hard 500).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub max_diagnostics: Option<usize>,
}

/// Argumente für `cargo.metadata`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct MetadataArgs {
    /// --no-deps: only workspace packages, no dependency resolution.
    #[serde(default)]
    pub no_deps: Option<bool>,
    /// --features: features to enable while resolving.
    #[serde(default)]
    pub features: Option<Vec<String>>,
    /// --all-features: enable every feature.
    #[serde(default)]
    pub all_features: Option<bool>,
    /// --no-default-features: do not enable default features.
    #[serde(default)]
    pub no_default_features: Option<bool>,
    /// --locked: fail if Cargo.lock would change.
    #[serde(default)]
    pub locked: Option<bool>,
    /// --offline: never access the network.
    #[serde(default)]
    pub offline: Option<bool>,
    /// Time limit in seconds (1-3600, default 600).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
    pub timeout_secs: Option<u64>,
    /// Maximum number of packages listed (default 50, hard 500).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub max_diagnostics: Option<usize>,
}

/// Argumente für `cargo.tree`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct TreeArgs {
    /// -p / --package: package(s) whose tree is shown (at most 64).
    #[serde(default)]
    pub package: Option<Vec<String>>,
    /// --workspace: all workspace packages.
    #[serde(default)]
    pub workspace: Option<bool>,
    /// --exclude: packages to skip (needs workspace=true).
    #[serde(default)]
    pub exclude: Option<Vec<String>>,
    /// -i / --invert: show what depends on this package (name or name@version).
    #[serde(default)]
    pub invert: Option<String>,
    /// --depth: maximum depth (0-32).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
    pub depth: Option<u64>,
    /// -d / --duplicates: only packages present in several versions.
    #[serde(default)]
    pub duplicates: Option<bool>,
    /// -e / --edges: edge kinds, any of normal, build, dev, features, all, no-normal, no-build, no-dev, no-proc-macro, proc-macro.
    #[serde(default)]
    pub edges: Option<Vec<String>>,
    /// --prefix: 'indent' (default), 'depth' or 'none'.
    #[serde(default)]
    pub prefix: Option<String>,
    /// --features: features to enable.
    #[serde(default)]
    pub features: Option<Vec<String>>,
    /// --all-features: enable every feature.
    #[serde(default)]
    pub all_features: Option<bool>,
    /// --no-default-features: do not enable default features.
    #[serde(default)]
    pub no_default_features: Option<bool>,
    /// --locked: fail if Cargo.lock would change.
    #[serde(default)]
    pub locked: Option<bool>,
    /// --offline: never access the network.
    #[serde(default)]
    pub offline: Option<bool>,
    /// Time limit in seconds (1-3600, default 600).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
    pub timeout_secs: Option<u64>,
}

/// Argumente für `cargo.test_one`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct TestOneArgs {
    /// Workspace member to test (exact package name, validated against cargo metadata).
    pub package: String,
    /// Test target: "lib" or "test:<integration test name>".
    pub target: String,
    /// Exact test path as printed by the harness, for example "parse::tests::empty_input" (letters, digits and _ : . / -).
    pub test: String,
    /// Time limit in seconds (1-600, default 300).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
    pub timeout_secs: Option<u64>,
}

const COLOR: [&str; 2] = ["--color", "never"];
const SHORT: &str = "--message-format=short";

fn default_flag(value: Option<bool>) -> bool {
    value.unwrap_or(false)
}

/// Baut das Kommando für `check`/`build`.
fn plan_simple(
    sub: &str,
    raw: &RawSelection,
    targets: &RawTargets,
) -> Result<(Plan, crate::plan::Selection), String> {
    let selection = raw.validate()?;
    let targets = targets.validate()?;
    let mut plan = Plan::cargo(sub);
    selection.package_args(&mut plan.argv);
    selection.feature_args(&mut plan.argv);
    selection.profile_args(&mut plan.argv);
    targets.args(&mut plan.argv);
    selection.net_args(&mut plan.argv);
    selection.jobs_args(&mut plan.argv);
    plan.extend([SHORT, COLOR[0], COLOR[1]]);
    Ok((plan, selection))
}

/// Baut das Kommando für `clippy`.
fn plan_clippy(args: &ClippyArgs) -> Result<(Plan, crate::plan::Selection), String> {
    let (mut plan, selection) = plan_simple("clippy", &args.raw_selection(), &args.raw_targets())?;
    if default_flag(args.no_deps) {
        plan.extend(["--no-deps"]);
    }
    let mut lints: Vec<String> = Vec::new();
    for (level, names) in [('D', &args.deny), ('W', &args.warn), ('A', &args.allow)] {
        let Some(names) = names else { continue };
        if names.len() > command::MAX_LIST {
            return Err(format!("too many lints (max {})", command::MAX_LIST));
        }
        for name in names {
            lints.extend(command::lint_flag(level, name)?);
        }
    }
    if !lints.is_empty() {
        plan.extend(["--".to_owned()]);
        plan.extend(lints);
    }
    Ok((plan, selection))
}

/// Baut das Kommando für `test`.
fn plan_test(args: &TestArgs) -> Result<(Plan, crate::plan::Selection), String> {
    let (mut plan, selection) = plan_simple("test", &args.raw_selection(), &args.raw_targets())?;
    for (set, flag) in [
        (args.no_run, "--no-run"),
        (args.doc, "--doc"),
        (args.no_fail_fast, "--no-fail-fast"),
    ] {
        if default_flag(set) {
            plan.extend([flag]);
        }
    }
    if default_flag(args.doc)
        && (default_flag(args.all_targets)
            || default_flag(args.lib)
            || default_flag(args.bins)
            || default_flag(args.tests)
            || default_flag(args.examples)
            || default_flag(args.benches)
            || args.bin.is_some())
    {
        return Err("doc cannot be combined with target selection flags".to_owned());
    }
    if default_flag(args.doc) && default_flag(args.no_run) {
        return Err("doc and no_run cannot be combined".to_owned());
    }
    if let Some(filter) = &args.test_filter {
        command::test_filter(filter)?;
        plan.extend([filter.clone()]);
    }
    if let Some(harness) = &args.harness_args {
        if harness.len() > 16 {
            return Err("too many harness_args (max 16)".to_owned());
        }
        for arg in harness {
            command::harness_arg(arg)?;
        }
        if !harness.is_empty() {
            plan.extend(["--".to_owned()]);
            plan.extend(harness.iter().cloned());
        }
    }
    Ok((plan, selection))
}

/// Baut das Kommando für `doc`.
fn plan_doc(args: &DocArgs) -> Result<(Plan, crate::plan::Selection), String> {
    let raw = RawSelection {
        package: args.package.clone(),
        workspace: args.workspace,
        exclude: args.exclude.clone(),
        features: args.features.clone(),
        all_features: args.all_features,
        no_default_features: args.no_default_features,
        release: args.release,
        profile: args.profile.clone(),
        locked: args.locked,
        offline: args.offline,
        jobs: args.jobs,
        timeout_secs: args.timeout_secs,
        max_diagnostics: args.max_diagnostics,
    };
    let selection = raw.validate()?;
    let mut plan = Plan::cargo("doc");
    selection.package_args(&mut plan.argv);
    selection.feature_args(&mut plan.argv);
    selection.profile_args(&mut plan.argv);
    for (set, flag) in [
        (args.lib, "--lib"),
        (args.bins, "--bins"),
        (args.no_deps, "--no-deps"),
        (args.document_private_items, "--document-private-items"),
    ] {
        if default_flag(set) {
            plan.extend([flag]);
        }
    }
    selection.net_args(&mut plan.argv);
    selection.jobs_args(&mut plan.argv);
    plan.extend([SHORT, COLOR[0], COLOR[1]]);
    Ok((plan, selection))
}

/// Baut das Kommando für `fmt --check`.
fn plan_fmt(args: &FmtArgs) -> Result<(Plan, crate::plan::Selection), String> {
    let raw = RawSelection {
        package: args.package.clone(),
        timeout_secs: args.timeout_secs,
        max_diagnostics: args.max_diagnostics,
        ..RawSelection::default()
    };
    let selection = raw.validate()?;
    let mut plan = Plan::cargo("fmt");
    plan.env.push(("NO_COLOR", "1"));
    plan.extend(["--check"]);
    if default_flag(args.all) {
        plan.extend(["--all"]);
    }
    selection.package_args(&mut plan.argv);
    Ok((plan, selection))
}

/// Baut das Kommando für `tree`.
fn plan_tree(args: &TreeArgs) -> Result<(Plan, crate::plan::Selection), String> {
    let raw = RawSelection {
        package: args.package.clone(),
        workspace: args.workspace,
        exclude: args.exclude.clone(),
        features: args.features.clone(),
        all_features: args.all_features,
        no_default_features: args.no_default_features,
        locked: args.locked,
        offline: args.offline,
        timeout_secs: args.timeout_secs,
        ..RawSelection::default()
    };
    let selection = raw.validate()?;
    let mut plan = Plan::cargo("tree");
    selection.package_args(&mut plan.argv);
    selection.feature_args(&mut plan.argv);
    selection.net_args(&mut plan.argv);
    if let Some(invert) = &args.invert {
        command::package(invert)?;
        plan.extend(["--invert".to_owned(), invert.clone()]);
    }
    if let Some(depth) = args.depth {
        if depth > 32 {
            return Err(format!("depth must be between 0 and 32, got {depth}"));
        }
        plan.extend(["--depth".to_owned(), depth.to_string()]);
    }
    if default_flag(args.duplicates) {
        plan.extend(["--duplicates"]);
    }
    if let Some(edges) = &args.edges {
        if edges.is_empty() || edges.len() > 10 {
            return Err("edges must contain 1-10 entries".to_owned());
        }
        for edge in edges {
            if !command::TREE_EDGES.contains(&edge.as_str()) {
                return Err(format!(
                    "invalid edge '{}': expected any of {}",
                    edge.chars().take(24).collect::<String>(),
                    command::TREE_EDGES.join(", ")
                ));
            }
        }
        plan.extend(["--edges".to_owned(), edges.join(",")]);
    }
    if let Some(prefix) = &args.prefix {
        if !["indent", "depth", "none"].contains(&prefix.as_str()) {
            return Err(format!(
                "invalid prefix '{}': expected indent, depth or none",
                prefix.chars().take(24).collect::<String>()
            ));
        }
        plan.extend(["--prefix".to_owned(), prefix.clone()]);
    }
    plan.extend([COLOR[0], COLOR[1]]);
    Ok((plan, selection))
}

/// Führt `plan` über den Ausführer aus.
async fn execute(
    shell: &ShellDelegate,
    context: &ToolExecutionContext,
    tool: &str,
    command: &str,
    timeout: u64,
) -> Result<ShellRun, ToolOutput> {
    shell
        .run(context, command, timeout)
        .await
        .map_err(|message| {
            let short: String = message.chars().take(4000).collect();
            fail(tool, short)
        })
}

/// Führt eine blockierende Dateiarbeit aus (Tokio-Pool, sonst direkt).
async fn blocking<T, F>(work: F) -> Option<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.spawn_blocking(work).await.ok(),
        Err(_) => Some(work()),
    }
}

fn workspace_root(context: &ToolExecutionContext) -> PathBuf {
    context.sandbox().workspace().canonical_root().to_path_buf()
}

// ---------------------------------------------------------------------------
// Werkzeuge
// ---------------------------------------------------------------------------

/// Prüft den Code wie `cargo check`.
#[harw_macros::tool(
    name = "cargo.check",
    description = "Type-checks Rust code like cargo check without producing binaries: select packages (-p, --workspace, --exclude), features (--features, --all-features, --no-default-features), targets (--all-targets, --lib, --bins, --tests, ...) and --release/--profile, --locked, --offline. Use when you want compile errors and warnings of a package or workspace instead of running cargo check by hand. Returns JSON {status, exit_code, duration_ms, errors, warnings, diagnostics:[{level, code, message, file, line, column}], log} with errors listed first; the raw log is only a clipped tail. Runs through the sandboxed shell tool and needs execute permission.",
    permission = "execute_process",
    state = ShellDelegate
)]
async fn cargo_check(
    shell: &ShellDelegate,
    context: &ToolExecutionContext,
    args: CheckArgs,
) -> Result<ToolOutput, ToolsError> {
    const TOOL: &str = "cargo.check";
    let (plan, selection) = match plan_simple("check", &args.raw_selection(), &args.raw_targets()) {
        Ok(planned) => planned,
        Err(message) => return Ok(fail(TOOL, message)),
    };
    let command = plan.command();
    match execute(shell, context, TOOL, &command, selection.timeout_secs).await {
        Ok(run) => Ok(build_report(
            TOOL,
            &command,
            &run,
            selection.max_diagnostics,
        )),
        Err(output) => Ok(output),
    }
}

/// Baut den Code wie `cargo build`.
#[harw_macros::tool(
    name = "cargo.build",
    description = "Compiles Rust code like cargo build: select packages (-p, --workspace, --exclude), features, targets (--lib, --bins, --all-targets, --bin NAME) and --release/--profile, --locked, --offline, -j. Use when you need to build artifacts or see build errors instead of running cargo build by hand. Returns JSON {status, exit_code, duration_ms, errors, warnings, diagnostics:[{level, code, message, file, line, column}], profile, cargo_took, log}; the raw log is only a clipped tail. Writes into target/ inside the sandbox only; needs execute permission and normally user approval.",
    permission = "execute_process",
    state = ShellDelegate
)]
async fn cargo_build(
    shell: &ShellDelegate,
    context: &ToolExecutionContext,
    args: BuildArgs,
) -> Result<ToolOutput, ToolsError> {
    const TOOL: &str = "cargo.build";
    let (plan, selection) = match plan_simple("build", &args.raw_selection(), &args.raw_targets()) {
        Ok(planned) => planned,
        Err(message) => return Ok(fail(TOOL, message)),
    };
    let command = plan.command();
    match execute(shell, context, TOOL, &command, selection.timeout_secs).await {
        Ok(run) => Ok(build_report(
            TOOL,
            &command,
            &run,
            selection.max_diagnostics,
        )),
        Err(output) => Ok(output),
    }
}

/// Lintet den Code wie `cargo clippy`.
#[harw_macros::tool(
    name = "cargo.clippy",
    description = "Lints Rust code like cargo clippy: package/feature/target selection as for cargo.check, no_deps (--no-deps) and typed lint levels deny (-D), warn (-W), allow (-A) after '--' (lint names only, for example warnings or clippy::pedantic). Use when you want clippy warnings with file:line instead of running cargo clippy by hand. Returns JSON {status, exit_code, errors, warnings, diagnostics:[{level, message, file, line, column}], log}; clippy --fix is not offered. Runs through the sandboxed shell tool and needs execute permission.",
    permission = "execute_process",
    state = ShellDelegate
)]
async fn cargo_clippy(
    shell: &ShellDelegate,
    context: &ToolExecutionContext,
    args: ClippyArgs,
) -> Result<ToolOutput, ToolsError> {
    const TOOL: &str = "cargo.clippy";
    let (plan, selection) = match plan_clippy(&args) {
        Ok(planned) => planned,
        Err(message) => return Ok(fail(TOOL, message)),
    };
    let command = plan.command();
    match execute(shell, context, TOOL, &command, selection.timeout_secs).await {
        Ok(run) => Ok(build_report(
            TOOL,
            &command,
            &run,
            selection.max_diagnostics,
        )),
        Err(output) => Ok(output),
    }
}

/// Führt die Tests aus wie `cargo test`.
#[harw_macros::tool(
    name = "cargo.test",
    description = "Runs Rust tests like cargo test: package/feature/target selection, a test_filter (TESTNAME), no_run, doc (--doc), no_fail_fast and harness_args after '--' restricted to an allowlist (--nocapture, --show-output, --ignored, --include-ignored, --exact, -q, --test-threads=N, --skip=NAME). Use when you need pass/fail counts and the failing tests with their panic location instead of running cargo test by hand. Returns JSON {status: ok|tests_failed|failed, tests:{passed, failed, ignored, filtered_out, failed_tests, failures:[{name, location, message}]}, diagnostics, duration_ms, log}; the raw log is only a clipped tail. Runs through the sandboxed shell tool and needs execute permission.",
    permission = "execute_process",
    state = ShellDelegate
)]
async fn cargo_test(
    shell: &ShellDelegate,
    context: &ToolExecutionContext,
    args: TestArgs,
) -> Result<ToolOutput, ToolsError> {
    const TOOL: &str = "cargo.test";
    let (plan, selection) = match plan_test(&args) {
        Ok(planned) => planned,
        Err(message) => return Ok(fail(TOOL, message)),
    };
    let command = plan.command();
    match execute(shell, context, TOOL, &command, selection.timeout_secs).await {
        Ok(run) => Ok(test_report(
            TOOL,
            &command,
            &run,
            selection.max_diagnostics,
            default_flag(args.no_run),
        )),
        Err(output) => Ok(output),
    }
}

/// Prüft die Formatierung wie `cargo fmt --check`.
#[harw_macros::tool(
    name = "cargo.fmt_check",
    description = "Checks Rust formatting like cargo fmt --check without changing any file: -p/package, all (--all). Use when you want to know which files need formatting instead of running cargo fmt --check by hand. Returns JSON {status: ok|needs_formatting|failed, diffs, files_with_diffs, listed:[{file, line}], log}. Never writes files; runs through the sandboxed shell tool and needs execute permission.",
    permission = "execute_process",
    parallel_safe,
    state = ShellDelegate
)]
async fn cargo_fmt_check(
    shell: &ShellDelegate,
    context: &ToolExecutionContext,
    args: FmtArgs,
) -> Result<ToolOutput, ToolsError> {
    const TOOL: &str = "cargo.fmt_check";
    let (plan, selection) = match plan_fmt(&args) {
        Ok(planned) => planned,
        Err(message) => return Ok(fail(TOOL, message)),
    };
    let command = plan.command();
    let root = workspace_root(context);
    match execute(shell, context, TOOL, &command, selection.timeout_secs).await {
        Ok(run) => Ok(fmt_report(
            TOOL,
            &command,
            &run,
            &root.to_string_lossy(),
            selection.max_diagnostics,
        )),
        Err(output) => Ok(output),
    }
}

/// Zeigt den Abhängigkeitsbaum wie `cargo tree`.
#[harw_macros::tool(
    name = "cargo.tree",
    description = "Shows the dependency tree like cargo tree: -p/package, workspace, exclude, invert (-i), depth, duplicates (-d), edges (-e normal, build, dev, features, ...), prefix, features, locked, offline. Use when you need to know why a crate is pulled in or which versions are duplicated instead of running cargo tree by hand. Returns JSON {tree: text, lines, truncated, status}; the text is capped at 48 KiB. Runs through the sandboxed shell tool and needs execute permission.",
    permission = "execute_process",
    state = ShellDelegate
)]
async fn cargo_tree(
    shell: &ShellDelegate,
    context: &ToolExecutionContext,
    args: TreeArgs,
) -> Result<ToolOutput, ToolsError> {
    const TOOL: &str = "cargo.tree";
    let (plan, selection) = match plan_tree(&args) {
        Ok(planned) => planned,
        Err(message) => return Ok(fail(TOOL, message)),
    };
    let command = plan.command();
    match execute(shell, context, TOOL, &command, selection.timeout_secs).await {
        Ok(run) => Ok(tree_report(TOOL, &command, &run)),
        Err(output) => Ok(output),
    }
}

/// Erzeugt die Dokumentation wie `cargo doc`.
#[harw_macros::tool(
    name = "cargo.doc",
    description = "Builds Rust documentation like cargo doc: package/feature selection, lib, bins, no_deps (--no-deps), document_private_items, release/profile, locked, offline. Use when you need rustdoc warnings or the generated documentation path instead of running cargo doc by hand. Returns JSON {status, errors, warnings, diagnostics:[{level, message, file, line, column}], generated, log}; --open is not offered. Writes into target/doc inside the sandbox only; needs execute permission.",
    permission = "execute_process",
    state = ShellDelegate
)]
async fn cargo_doc(
    shell: &ShellDelegate,
    context: &ToolExecutionContext,
    args: DocArgs,
) -> Result<ToolOutput, ToolsError> {
    const TOOL: &str = "cargo.doc";
    let (plan, selection) = match plan_doc(&args) {
        Ok(planned) => planned,
        Err(message) => return Ok(fail(TOOL, message)),
    };
    let command = plan.command();
    match execute(shell, context, TOOL, &command, selection.timeout_secs).await {
        Ok(run) => Ok(build_report(
            TOOL,
            &command,
            &run,
            selection.max_diagnostics,
        )),
        Err(output) => Ok(output),
    }
}

/// Fasst `cargo metadata` zusammen.
#[harw_macros::tool(
    name = "cargo.metadata",
    description = "Summarizes the workspace like cargo metadata --format-version 1: workspace packages with version, manifest path, targets, features and dependency count, plus the number and names of external packages; no_deps (--no-deps), features, all_features, no_default_features, locked, offline. Use when you need the structure of a Cargo workspace instead of running cargo metadata and parsing megabytes of JSON. Returns JSON {workspace_members:[{name, version, manifest, targets, features, dependencies}], workspace_member_count, external_package_count, external_packages, truncated}; absolute paths are never returned. Runs through the sandboxed shell tool and needs execute permission.",
    permission = "execute_process",
    state = ShellDelegate
)]
async fn cargo_metadata(
    shell: &ShellDelegate,
    context: &ToolExecutionContext,
    args: MetadataArgs,
) -> Result<ToolOutput, ToolsError> {
    const TOOL: &str = "cargo.metadata";
    let raw = RawSelection {
        features: args.features.clone(),
        all_features: args.all_features,
        no_default_features: args.no_default_features,
        locked: args.locked,
        offline: args.offline,
        timeout_secs: args.timeout_secs,
        max_diagnostics: args.max_diagnostics,
        ..RawSelection::default()
    };
    let selection = match raw.validate() {
        Ok(selection) => selection,
        Err(message) => return Ok(fail(TOOL, message)),
    };
    let mut plan = Plan::cargo("metadata");
    plan.extend(["--format-version", "1"]);
    if default_flag(args.no_deps) {
        plan.extend(["--no-deps"]);
    }
    selection.feature_args(&mut plan.argv);
    selection.net_args(&mut plan.argv);
    plan.extend([COLOR[0], COLOR[1]]);
    // Die JSON-Ausgabe ist meist größer als das 64-KiB-Limit von shell.exec: sie
    // geht in eine Datei unter target/ und wird hier gelesen (symlinkfrei).
    let name = format!(
        "metadata-{}-{}.json",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let file = format!("{SCRATCH_DIR}/{name}");
    let command = format!(
        "{} && {} > {}",
        command::join(&["mkdir".to_owned(), "-p".to_owned(), SCRATCH_DIR.to_owned()]),
        command::join(&plan.argv),
        command::join(std::slice::from_ref(&file))
    );
    let run = match execute(shell, context, TOOL, &command, selection.timeout_secs).await {
        Ok(run) => run,
        Err(output) => return Ok(output),
    };
    let root = workspace_root(context);
    let file_for_read = file.clone();
    let name_for_cleanup = name.clone();
    let read = blocking(move || {
        read_and_remove(&root, &file_for_read, &name_for_cleanup, MAX_METADATA_BYTES)
    })
    .await;
    if run.exit_code != 0 {
        return Ok(failed_report(
            TOOL,
            &command,
            &run,
            selection.max_diagnostics,
        ));
    }
    match read {
        Some(Ok(metadata)) => Ok(metadata_report(
            TOOL,
            &command,
            &run,
            &metadata,
            selection.max_diagnostics,
        )),
        Some(Err(message)) => Ok(fail(TOOL, message)),
        None => Ok(fail(TOOL, "internal error while reading the metadata file")),
    }
}

/// Liest die Metadaten-Datei (symlinkfrei, begrenzt) und entfernt sie danach.
fn read_and_remove(
    root: &std::path::Path,
    relative: &str,
    name: &str,
    max_bytes: u64,
) -> Result<Value, String> {
    let scope = Scope::new(root).map_err(|e| {
        format!(
            "workspace root is not accessible: {}",
            harw_tool_fsread::scope::io_message(&e)
        )
    })?;
    let rel = scope.rel(relative).map_err(|e| e.to_string())?;
    let result = (|| {
        let mut file = scope
            .open_read(&rel)
            .map_err(|e| format!("cannot read the metadata output: {e}"))?;
        let mut bytes = Vec::new();
        file.by_ref()
            .take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
            return Err(format!("metadata output is larger than {max_bytes} bytes"));
        }
        serde_json::from_slice::<Value>(&bytes)
            .map_err(|e| format!("metadata output is not valid JSON: {e}"))
    })();
    // Aufräumen, egal wie das Lesen ausging: nur die eigene Datei, dirfd-relativ.
    if let Ok(dir_rel) = scope.rel(SCRATCH_DIR) {
        if let Ok(dir) = scope.open_dir(&dir_rel) {
            let _ = unlinkat(dir.as_fd(), name, AtFlags::empty());
        }
    }
    result
}

/// Time limit of the two pre-flight commands (`cargo metadata`, `git status`).
const PREFLIGHT_TIMEOUT_SECS: u64 = 60;

/// Runs `cargo metadata --no-deps` through the delegate and returns the JSON.
async fn preflight_metadata(
    shell: &ShellDelegate,
    context: &ToolExecutionContext,
    tool: &str,
) -> Result<Value, ToolOutput> {
    let argv: Vec<String> = [
        "cargo",
        "metadata",
        "--format-version",
        "1",
        "--no-deps",
        "--locked",
        COLOR[0],
        COLOR[1],
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    let name = format!(
        "metadata-{}-{}.json",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let file = format!("{SCRATCH_DIR}/{name}");
    let command = format!(
        "{} && {} > {}",
        command::join(&["mkdir".to_owned(), "-p".to_owned(), SCRATCH_DIR.to_owned()]),
        command::join(&argv),
        command::join(std::slice::from_ref(&file))
    );
    let run = execute(shell, context, tool, &command, PREFLIGHT_TIMEOUT_SECS).await?;
    let root = workspace_root(context);
    let read = blocking(move || read_and_remove(&root, &file, &name, MAX_METADATA_BYTES)).await;
    if run.exit_code != 0 {
        let tail: String = run.stderr.chars().take(1500).collect();
        return Err(fail(
            tool,
            format!(
                "pre-flight `cargo metadata --no-deps --locked` failed (exit {}); no test was run: {tail}",
                run.exit_code
            ),
        ));
    }
    match read {
        Some(Ok(value)) => Ok(value),
        Some(Err(message)) => Err(fail(tool, message)),
        None => Err(fail(tool, "internal error while reading the metadata file")),
    }
}

/// Lists changed paths (staged, unstaged, untracked) relative to the
/// workspace root through `git status`.
async fn preflight_changes(
    shell: &ShellDelegate,
    context: &ToolExecutionContext,
    tool: &str,
) -> Result<Vec<String>, ToolOutput> {
    let words = |items: &[&str]| -> Vec<String> { items.iter().map(|s| (*s).to_owned()).collect() };
    let prefix_cmd = command::join(&words(&["git", "rev-parse", "--show-prefix"]));
    let status_cmd = command::join(&words(&[
        "git",
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
        "--no-renames",
        ".",
    ]));
    let command = format!("{prefix_cmd} && {status_cmd}");
    let run = execute(shell, context, tool, &command, PREFLIGHT_TIMEOUT_SECS).await?;
    if run.exit_code != 0 {
        return Err(fail(
            tool,
            format!(
                "pre-flight `git status` failed (exit {}); the rebuild guard cannot tell what changed, so no test was run: {}",
                run.exit_code,
                run.stderr.chars().take(500).collect::<String>()
            ),
        ));
    }
    if run.truncated {
        return Err(fail(
            tool,
            "pre-flight `git status` output was truncated: far too many files changed for a focused test run; commit or stash the unrelated changes, or run the test as a background job (job.start). No test was run.",
        ));
    }
    let (prefix, rest) = run.stdout.split_once('\n').unwrap_or(("", ""));
    Ok(parse_porcelain_z(rest, prefix.trim()))
}

/// Runs exactly one test if the rebuild guard allows it.
#[harw_macros::tool(
    name = "cargo.test_one",
    description = "Runs exactly ONE Rust test (cargo test -p PACKAGE --lib|--test T --locked TEST -- --exact) and only when that will not recompile a large part of the workspace. Arguments: package (workspace member), target (\"lib\" or \"test:<integration test name>\"), test (exact test path), optional timeout_secs (default 300, max 600). A deterministic pre-flight check (no compilation) computes which workspace crates cargo would rebuild from the uncommitted changes and the dependency closure of the package; if more than 3 crates would rebuild, or Cargo.toml, Cargo.lock, rust-toolchain, .cargo/config.toml or a build.rs changed, nothing is run and the result is {status: \"refused\", reason: would_recompile|global_invalidation, rebuild_count, threshold, rebuild_set, changed_packages, changed_global_files, hint}. Otherwise returns the cargo.test result {status: ok|tests_failed|failed|no_test_matched, tests:{passed, failed, failures:[{name, location, message}]}, diagnostics, log} plus a guard summary. No --workspace, no free-form arguments. Runs through the sandboxed shell tool and needs execute permission.",
    permission = "execute_process",
    state = ShellDelegate
)]
async fn cargo_test_one(
    shell: &ShellDelegate,
    context: &ToolExecutionContext,
    args: TestOneArgs,
) -> Result<ToolOutput, ToolsError> {
    const TOOL: &str = "cargo.test_one";
    let timeout = match args.timeout_secs {
        None => guard::DEFAULT_TIMEOUT_SECS,
        Some(n) if (1..=guard::MAX_TIMEOUT_SECS).contains(&n) => n,
        Some(n) => {
            return Ok(fail(
                TOOL,
                format!(
                    "timeout_secs must be between 1 and {}, got {n}",
                    guard::MAX_TIMEOUT_SECS
                ),
            ));
        }
    };
    let target = match Target::parse(&args.target) {
        Ok(target) => target,
        Err(message) => return Ok(fail(TOOL, message)),
    };
    if let Err(message) = command::package(&args.package) {
        return Ok(fail(TOOL, message));
    }
    if args.package.contains('@') {
        return Ok(fail(
            TOOL,
            "package must be a plain package name without @version",
        ));
    }
    if let Err(message) = command::test_filter(&args.test) {
        return Ok(fail(TOOL, message.replace("test_filter", "test")));
    }
    let metadata = match preflight_metadata(shell, context, TOOL).await {
        Ok(metadata) => metadata,
        Err(output) => return Ok(output),
    };
    let workspace = match Workspace::from_metadata(&metadata) {
        Ok(workspace) => workspace,
        Err(message) => return Ok(fail(TOOL, message)),
    };
    let Some(info) = workspace.packages.get(&args.package) else {
        let similar = workspace.similar(&args.package);
        let hint = if similar.is_empty() {
            String::new()
        } else {
            format!("; similar: {}", similar.join(", "))
        };
        return Ok(fail(
            TOOL,
            format!("'{}' is not a workspace member{hint}", args.package),
        ));
    };
    match &target {
        Target::Lib if !info.has_lib => {
            return Ok(fail(
                TOOL,
                format!(
                    "{} has no library target; use target \"test:<name>\"",
                    args.package
                ),
            ));
        }
        Target::Test(name) if !info.test_targets.contains(name) => {
            let available: Vec<&str> = info.test_targets.iter().map(String::as_str).collect();
            return Ok(fail(
                TOOL,
                format!(
                    "{} has no integration test '{name}' (available: {})",
                    args.package,
                    if available.is_empty() {
                        "none".to_owned()
                    } else {
                        available.join(", ")
                    }
                ),
            ));
        }
        _ => {}
    }
    let changes = match preflight_changes(shell, context, TOOL).await {
        Ok(changes) => changes,
        Err(output) => return Ok(output),
    };
    let assessment = guard::assess(&workspace, &args.package, &changes);
    if let Some(reason) = assessment.refusal() {
        return Ok(ToolOutput::json(
            assessment.refusal_json(reason, &target, &args.test),
        ));
    }
    let argv = guard::test_one_argv(&args.package, &target, &args.test);
    let command = command::join(&argv);
    let run = match execute(shell, context, TOOL, &command, timeout).await {
        Ok(run) => run,
        Err(output) => return Ok(output),
    };
    let output = test_report(TOOL, &command, &run, 20, false);
    Ok(shape_test_one(output, &args, &target, &assessment))
}

/// Adds the guard summary and detects "no test matched".
fn shape_test_one(
    output: ToolOutput,
    args: &TestOneArgs,
    target: &Target,
    assessment: &guard::Assessment,
) -> ToolOutput {
    let ToolOutput::Json { mut content } = output else {
        return output;
    };
    content["package"] = serde_json::json!(args.package);
    content["target"] = serde_json::json!(target.label());
    content["test"] = serde_json::json!(args.test);
    content["guard"] = serde_json::json!({
        "rebuild_count": assessment.rebuild_set.len(),
        "threshold": guard::MAX_REBUILD_PACKAGES,
        "rebuild_set": assessment.rebuild_set,
    });
    let count = |key: &str| content["tests"][key].as_u64().unwrap_or(0);
    let ran = count("passed") + count("failed");
    if content["status"] == "ok" && ran == 0 {
        content["status"] = serde_json::json!(if count("ignored") > 0 {
            "ignored"
        } else {
            "no_test_matched"
        });
        content["hint"] = serde_json::json!(
            "No test ran: --exact needs the full path as printed by the harness (for example module::tests::name), and #[ignore]d tests are skipped."
        );
    }
    ToolOutput::Json { content }
}

harw_tools::tool_provider! {
    /// Stellt die Cargo-Werkzeuge `cargo.check`, `cargo.build`, `cargo.clippy`,
    /// `cargo.test`, `cargo.fmt_check`, `cargo.tree`, `cargo.doc` und
    /// `cargo.metadata` bereit. Der Prozessstart läuft ausschließlich über den
    /// übergebenen [`ShellDelegate`].
    pub struct CargoToolProvider {
        state: ShellDelegate as shell;
        CargoCheckTool => CargoCheckTool::new(shell.clone()),
        CargoBuildTool => CargoBuildTool::new(shell.clone()),
        CargoClippyTool => CargoClippyTool::new(shell.clone()),
        CargoTestTool => CargoTestTool::new(shell.clone()),
        CargoFmtCheckTool => CargoFmtCheckTool::new(shell.clone()),
        CargoTreeTool => CargoTreeTool::new(shell.clone()),
        CargoDocTool => CargoDocTool::new(shell.clone()),
        CargoMetadataTool => CargoMetadataTool::new(shell.clone()),
        CargoTestOneTool => CargoTestOneTool::new(shell.clone()),
    }
}

/// Namen aller Werkzeuge dieses Providers (für Profil-Listen).
pub const CARGO_TOOL_NAMES: &[&str] = CargoToolProvider::TOOL_NAMES;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        Fixture, ScriptedShell, TestError, TestResult, error_of, json_of, run,
    };
    use harw_extension_api::contributors::ToolProvider;
    use harw_tools::{ToolName, ToolSpec};
    use serde_json::{Value, json};
    use std::sync::Arc;

    fn parse<T: for<'de> Deserialize<'de>>(value: Value) -> TestResult<T> {
        Ok(serde_json::from_value(value)?)
    }

    fn cmd(plan: Result<(Plan, crate::plan::Selection), String>) -> TestResult<String> {
        plan.map(|(plan, _)| plan.command())
            .map_err(TestError::Unexpected)
    }

    #[test]
    fn check_and_build_commands() -> TestResult {
        let args: CheckArgs = parse(json!({
            "package": ["harw-core"], "features": ["fancy"], "all_targets": true, "release": true, "locked": true, "offline": true, "jobs": 2
        }))?;
        assert_eq!(
            cmd(plan_simple(
                "check",
                &args.raw_selection(),
                &args.raw_targets()
            ))?,
            "cargo check -p harw-core --features fancy --release --all-targets --locked --offline -j 2 --message-format=short --color never"
        );
        let args: BuildArgs = parse(
            json!({"workspace": true, "exclude": ["harw-web"], "bin": ["harw"], "profile": "release-lto", "no_default_features": true}),
        )?;
        assert_eq!(
            cmd(plan_simple(
                "build",
                &args.raw_selection(),
                &args.raw_targets()
            ))?,
            "cargo build --workspace --exclude harw-web --no-default-features --profile release-lto --bin harw --message-format=short --color never"
        );
        let args: CheckArgs = parse(json!({}))?;
        assert_eq!(
            cmd(plan_simple(
                "check",
                &args.raw_selection(),
                &args.raw_targets()
            ))?,
            "cargo check --message-format=short --color never"
        );
        Ok(())
    }

    #[test]
    fn clippy_and_doc_and_fmt_and_tree_commands() -> TestResult {
        let args: ClippyArgs = parse(
            json!({"package": ["a"], "no_deps": true, "deny": ["warnings"], "warn": ["clippy::pedantic"], "allow": ["clippy::module_name_repetitions"]}),
        )?;
        assert_eq!(
            cmd(plan_clippy(&args))?,
            "cargo clippy -p a --message-format=short --color never --no-deps -- -D warnings -W clippy::pedantic -A clippy::module_name_repetitions"
        );
        let args: DocArgs = parse(
            json!({"workspace": true, "no_deps": true, "document_private_items": true, "lib": true, "locked": true}),
        )?;
        assert_eq!(
            cmd(plan_doc(&args))?,
            "cargo doc --workspace --lib --no-deps --document-private-items --locked --message-format=short --color never"
        );
        let args: FmtArgs = parse(json!({"all": true, "package": ["x"]}))?;
        assert_eq!(
            cmd(plan_fmt(&args))?,
            "NO_COLOR=1 cargo fmt --check --all -p x"
        );
        let args: TreeArgs = parse(
            json!({"package": ["a"], "invert": "serde@1.0.0", "depth": 3, "duplicates": true, "edges": ["normal", "build"], "prefix": "depth", "locked": true}),
        )?;
        assert_eq!(
            cmd(plan_tree(&args))?,
            "cargo tree -p a --locked --invert serde@1.0.0 --depth 3 --duplicates --edges normal,build --prefix depth --color never"
        );
        Ok(())
    }

    #[test]
    fn test_command_with_filter_and_allowlisted_harness_args() -> TestResult {
        let args: TestArgs = parse(json!({
            "package": ["demo"], "test_filter": "tests::passes", "no_fail_fast": true, "lib": true,
            "harness_args": ["--nocapture", "--test-threads=1", "--skip=slow"]
        }))?;
        assert_eq!(
            cmd(plan_test(&args))?,
            "cargo test -p demo --lib --message-format=short --color never --no-fail-fast tests::passes -- --nocapture --test-threads=1 --skip=slow"
        );
        let args: TestArgs = parse(json!({"no_run": true}))?;
        assert_eq!(
            cmd(plan_test(&args))?,
            "cargo test --message-format=short --color never --no-run"
        );
        let args: TestArgs = parse(json!({"doc": true, "package": ["a"]}))?;
        assert!(cmd(plan_test(&args))?.contains("--doc"));
        Ok(())
    }

    #[test]
    fn injection_attempts_are_rejected_before_any_command_exists() -> TestResult {
        let bad_tests = [
            json!({"package": ["a; rm -rf /"]}),
            json!({"package": ["--config=build.rustc='sh'"]}),
            json!({"package": ["$(touch pwned)"]}),
            json!({"features": ["a`id`"]}),
            json!({"test_filter": "--exact"}),
            json!({"test_filter": "a b"}),
            json!({"test_filter": "x'; sh -c 'y"}),
            json!({"harness_args": ["--format=json"]}),
            json!({"harness_args": ["--nocapture; ls"]}),
            json!({"harness_args": ["-Zunstable-options"]}),
            json!({"harness_args": ["--"]}),
            json!({"harness_args": (0..17).map(|_| "--exact").collect::<Vec<_>>()}),
            json!({"doc": true, "lib": true}),
            json!({"doc": true, "no_run": true}),
            json!({"profile": "x y"}),
            json!({"bin": ["a/b"]}),
            json!({"jobs": 100}),
            json!({"timeout_secs": 0}),
        ];
        for bad in bad_tests {
            let args: TestArgs = parse(bad.clone())?;
            assert!(plan_test(&args).is_err(), "{bad} must be rejected");
        }
        let bad_clippy = [
            json!({"deny": ["warnings; ls"]}),
            json!({"deny": ["-D"]}),
            json!({"warn": ["A"]}),
            json!({"allow": [""]}),
            json!({"deny": (0..65).map(|i| format!("l{i}")).collect::<Vec<_>>()}),
        ];
        for bad in bad_clippy {
            let args: ClippyArgs = parse(bad.clone())?;
            assert!(plan_clippy(&args).is_err(), "{bad} must be rejected");
        }
        let bad_tree = [
            json!({"edges": ["bogus"]}),
            json!({"edges": []}),
            json!({"prefix": "x"}),
            json!({"depth": 33}),
            json!({"invert": "a b"}),
            json!({"invert": "-x"}),
        ];
        for bad in bad_tree {
            let args: TreeArgs = parse(bad.clone())?;
            assert!(plan_tree(&args).is_err(), "{bad} must be rejected");
        }
        // Optionen, die ein Werkzeug nicht kennt, gibt es nicht als Feld: abgelehnt, nicht ignoriert.
        for (name, bad) in [
            ("check", json!({"fix": true})),
            ("clippy", json!({"fix": true})),
            ("doc", json!({"open": true})),
            ("test", json!({"target_dir": "/tmp"})),
            ("check", json!({"manifest_path": "../Cargo.toml"})),
            ("check", json!({"config": "x"})),
        ] {
            let rejected = match name {
                "check" => parse::<CheckArgs>(bad.clone()).is_err(),
                "clippy" => parse::<ClippyArgs>(bad.clone()).is_err(),
                "doc" => parse::<DocArgs>(bad.clone()).is_err(),
                _ => parse::<TestArgs>(bad.clone()).is_err(),
            };
            assert!(rejected, "{name} {bad}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn check_runs_the_planned_command_and_reports() -> TestResult {
        let fx = Fixture::new()?;
        let ctx = fx.exec_ctx()?;
        let shell = ScriptedShell::json(json!({
            "exit_code": 101,
            "stdout": "",
            "stderr": "src/lib.rs:7:5: error[E0308]: mismatched types\nerror: could not compile `demo` (lib) due to 1 previous error\n",
            "truncated": false
        }));
        let provider = CargoToolProvider::new(ShellDelegate::new(shell.clone()));
        let tool = provider
            .executor(&ToolName::new("cargo.check"))
            .ok_or(TestError::Missing("cargo.check"))?;
        let value = json_of(
            run(
                tool.as_ref(),
                &ctx,
                "cargo.check",
                json!({"package": ["demo"], "timeout_secs": 90}),
            )
            .await?,
        )?;
        assert_eq!(value["status"], "failed");
        assert_eq!(value["diagnostics"][0]["file"], "src/lib.rs");
        assert_eq!(
            shell.calls(),
            vec![(
                "cargo check -p demo --message-format=short --color never".to_owned(),
                90
            )]
        );
        Ok(())
    }

    #[tokio::test]
    async fn invalid_arguments_never_reach_the_shell() -> TestResult {
        let fx = Fixture::new()?;
        let ctx = fx.exec_ctx()?;
        let shell = ScriptedShell::json(json!({"exit_code": 0}));
        let provider = CargoToolProvider::new(ShellDelegate::new(shell.clone()));
        for (name, args) in [
            ("cargo.check", json!({"package": ["a;b"]})),
            ("cargo.test", json!({"harness_args": ["--evil"]})),
            ("cargo.clippy", json!({"deny": ["x y"]})),
            ("cargo.tree", json!({"edges": ["nope"]})),
            ("cargo.metadata", json!({"features": ["a b"]})),
            ("cargo.fmt_check", json!({"package": ["--all"]})),
            ("cargo.doc", json!({"profile": "a b"})),
            ("cargo.build", json!({"jobs": 0})),
        ] {
            let tool = provider
                .executor(&ToolName::new(name))
                .ok_or(TestError::Missing("executor"))?;
            let message = error_of(run(tool.as_ref(), &ctx, name, args.clone()).await?)
                .map_err(|e| TestError::Unexpected(format!("{name} {args}: {e}")))?;
            assert!(message.starts_with(name), "{message}");
        }
        assert!(shell.calls().is_empty(), "{:?}", shell.calls());
        Ok(())
    }

    #[tokio::test]
    async fn shell_failures_are_reported_as_tool_errors() -> TestResult {
        let fx = Fixture::new()?;
        let ctx = fx.exec_ctx()?;
        let provider = CargoToolProvider::new(ShellDelegate::new(ScriptedShell::error(
            "shell.exec timed out after 5s; process tree killed.",
        )));
        let tool = provider
            .executor(&ToolName::new("cargo.build"))
            .ok_or(TestError::Missing("cargo.build"))?;
        let message = error_of(run(tool.as_ref(), &ctx, "cargo.build", json!({})).await?)?;
        assert!(message.starts_with("cargo.build:") && message.contains("timed out"));
        Ok(())
    }

    #[tokio::test]
    async fn metadata_goes_through_a_scratch_file_and_cleans_up() -> TestResult {
        let fx = Fixture::new()?;
        let ctx = fx.exec_ctx()?;
        let ws = fx.ws.clone();
        let doc = json!({"workspace_root": ws.to_string_lossy(), "workspace_members": ["m"], "packages": [
            {"name": "demo", "version": "0.1.0", "id": "m", "manifest_path": format!("{}/Cargo.toml", ws.display()), "targets": [], "features": {}, "dependencies": []}]});
        let shell = {
            let ws = ws.clone();
            ScriptedShell::with(move |command| {
                // Der Ersatz „führt“ das Kommando aus, indem er die Zieldatei anlegt.
                let target = command
                    .rsplit(" > ")
                    .next()
                    .unwrap_or("")
                    .trim()
                    .trim_matches('\'')
                    .to_owned();
                let path = ws.join(&target);
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(path, doc.to_string());
                harw_tools::ToolOutput::json(
                    json!({"exit_code": 0, "stdout": "", "stderr": "", "truncated": false}),
                )
            })
        };
        let provider = CargoToolProvider::new(ShellDelegate::new(shell.clone()));
        let tool = provider
            .executor(&ToolName::new("cargo.metadata"))
            .ok_or(TestError::Missing("cargo.metadata"))?;
        let value = json_of(
            run(
                tool.as_ref(),
                &ctx,
                "cargo.metadata",
                json!({"no_deps": true}),
            )
            .await?,
        )?;
        assert_eq!(value["workspace_member_count"], 1);
        assert_eq!(value["workspace_members"][0]["manifest"], "Cargo.toml");
        let command = shell
            .calls()
            .first()
            .map(|(c, _)| c.clone())
            .unwrap_or_default();
        assert!(command.starts_with("mkdir -p target/harw-tool-cargo && cargo metadata --format-version 1 --no-deps --color never > "), "{command}");
        let leftovers: Vec<_> = std::fs::read_dir(ws.join(SCRATCH_DIR))?.collect();
        assert!(
            leftovers.is_empty(),
            "scratch file must be removed: {leftovers:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn metadata_rejects_symlinked_scratch_output_and_bad_json() -> TestResult {
        let fx = Fixture::new()?;
        let ctx = fx.exec_ctx()?;
        // Die Zieldatei wäre ein Symlink auf eine fremde Datei: nie gelesen.
        let outside = fx.ws.join("..").join("secret.json");
        std::fs::write(&outside, r#"{"packages": [{"name": "leak"}]}"#)?;
        let ws = fx.ws.clone();
        let shell = ScriptedShell::with(move |command| {
            let target = command
                .rsplit(" > ")
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches('\'')
                .to_owned();
            let path = ws.join(&target);
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::os::unix::fs::symlink(&outside, &path);
            harw_tools::ToolOutput::json(
                json!({"exit_code": 0, "stdout": "", "stderr": "", "truncated": false}),
            )
        });
        let provider = CargoToolProvider::new(ShellDelegate::new(shell));
        let tool = provider
            .executor(&ToolName::new("cargo.metadata"))
            .ok_or(TestError::Missing("tool"))?;
        let message = error_of(run(tool.as_ref(), &ctx, "cargo.metadata", json!({})).await?)?;
        assert!(
            message.contains("cannot read the metadata output"),
            "{message}"
        );
        let ws = fx.ws.clone();
        let bad = ScriptedShell::with(move |command| {
            let target = command
                .rsplit(" > ")
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches('\'')
                .to_owned();
            let path = ws.join(&target);
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(path, "not json");
            harw_tools::ToolOutput::json(
                json!({"exit_code": 0, "stdout": "", "stderr": "", "truncated": false}),
            )
        });
        let provider = CargoToolProvider::new(ShellDelegate::new(bad));
        let tool = provider
            .executor(&ToolName::new("cargo.metadata"))
            .ok_or(TestError::Missing("tool"))?;
        let message = error_of(run(tool.as_ref(), &ctx, "cargo.metadata", json!({})).await?)?;
        assert!(message.contains("not valid JSON"), "{message}");
        Ok(())
    }

    #[test]
    fn metadata_reader_enforces_the_size_cap_and_always_cleans_up() -> TestResult {
        let fx = Fixture::new()?;
        let dir = fx.ws.join(SCRATCH_DIR);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join("big.json"),
            format!("{{\"packages\": [], \"pad\": \"{}\"}}", "x".repeat(200)),
        )?;
        let relative = format!("{SCRATCH_DIR}/big.json");
        let error = read_and_remove(&fx.ws, &relative, "big.json", 64)
            .err()
            .ok_or(TestError::Missing("error"))?;
        assert!(error.contains("larger than 64 bytes"), "{error}");
        assert!(
            !dir.join("big.json").exists(),
            "scratch file must be removed even after an error"
        );
        std::fs::write(dir.join("ok.json"), r#"{"packages": []}"#)?;
        let value = read_and_remove(&fx.ws, &format!("{SCRATCH_DIR}/ok.json"), "ok.json", 64)
            .map_err(TestError::Unexpected)?;
        assert_eq!(value["packages"], json!([]));
        assert!(!dir.join("ok.json").exists());
        assert!(
            read_and_remove(
                &fx.ws,
                &format!("{SCRATCH_DIR}/missing.json"),
                "missing.json",
                64
            )
            .is_err()
        );
        Ok(())
    }

    /// Skriptet die drei Phasen von `cargo.test_one`: Metadata-Zwischendatei,
    /// `git status` und den eigentlichen `cargo test`-Lauf.
    fn test_one_shell(ws: &std::path::Path, dirty: &'static str) -> Arc<ScriptedShell> {
        let dep =
            |name: &str| json!({"name": name, "source": null, "kind": null, "path": ws.join(name)});
        let package = |name: &str, deps: Vec<Value>, targets: Value| {
            json!({
                "name": name,
                "manifest_path": format!("{}/{name}/Cargo.toml", ws.display()),
                "dependencies": deps,
                "targets": targets
            })
        };
        let lib = |name: &str| json!([{"name": name, "kind": ["lib"]}]);
        let doc = json!({
            "workspace_root": ws.to_string_lossy(),
            "packages": [
                package("low", vec![], lib("low")),
                package("mid1", vec![dep("low")], lib("mid1")),
                package("mid2", vec![dep("low")], lib("mid2")),
                package("mid3", vec![dep("low")], lib("mid3")),
                package(
                    "app",
                    vec![dep("mid1"), dep("mid2"), dep("mid3")],
                    json!([{"name": "app", "kind": ["lib"]}, {"name": "it", "kind": ["test"]}])
                ),
            ]
        });
        let ws = ws.to_path_buf();
        ScriptedShell::with(move |command| {
            let ok = |stdout: &str| {
                harw_tools::ToolOutput::json(
                    json!({"exit_code": 0, "stdout": stdout, "stderr": "", "truncated": false}),
                )
            };
            if command.starts_with("mkdir -p") {
                let target = command
                    .rsplit(" > ")
                    .next()
                    .unwrap_or("")
                    .trim()
                    .trim_matches('\'')
                    .to_owned();
                let path = ws.join(target);
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(path, doc.to_string());
                ok("")
            } else if command.starts_with("git rev-parse") {
                ok(dirty)
            } else {
                ok(
                    "running 1 test\ntest tests::x ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n",
                )
            }
        })
    }

    fn cargo_test_calls(shell: &ScriptedShell) -> Vec<String> {
        shell
            .calls()
            .into_iter()
            .map(|(command, _)| command)
            .filter(|command| command.starts_with("cargo test"))
            .collect()
    }

    #[tokio::test]
    async fn test_one_refuses_dirty_low_level_crate_without_issuing_cargo_test() -> TestResult {
        let fx = Fixture::new()?;
        let ctx = fx.exec_ctx()?;
        let shell = test_one_shell(&fx.ws, "\n M low/src/lib.rs\0");
        let provider = CargoToolProvider::new(ShellDelegate::new(shell.clone()));
        let tool = provider
            .executor(&ToolName::new("cargo.test_one"))
            .ok_or(TestError::Missing("cargo.test_one"))?;
        let value = json_of(
            run(
                tool.as_ref(),
                &ctx,
                "cargo.test_one",
                json!({"package": "app", "target": "lib", "test": "tests::x"}),
            )
            .await?,
        )?;
        assert_eq!(value["status"], "refused");
        assert_eq!(value["reason"], "would_recompile");
        assert_eq!(value["rebuild_count"], 5);
        assert_eq!(value["threshold"], guard::MAX_REBUILD_PACKAGES);
        assert!(cargo_test_calls(&shell).is_empty(), "{:?}", shell.calls());
        Ok(())
    }

    #[tokio::test]
    async fn test_one_clean_tree_issues_exactly_one_exact_test_call() -> TestResult {
        let fx = Fixture::new()?;
        let ctx = fx.exec_ctx()?;
        let shell = test_one_shell(&fx.ws, "\n");
        let provider = CargoToolProvider::new(ShellDelegate::new(shell.clone()));
        let tool = provider
            .executor(&ToolName::new("cargo.test_one"))
            .ok_or(TestError::Missing("cargo.test_one"))?;
        let value = json_of(
            run(
                tool.as_ref(),
                &ctx,
                "cargo.test_one",
                json!({"package": "app", "target": "test:it", "test": "tests::x"}),
            )
            .await?,
        )?;
        assert_eq!(value["status"], "ok", "{value}");
        assert_eq!(value["tests"]["passed"], 1);
        assert_eq!(value["guard"]["rebuild_count"], 0);
        let calls = cargo_test_calls(&shell);
        assert_eq!(calls.len(), 1, "{calls:?}");
        let command = &calls[0];
        assert!(
            command.starts_with("cargo test -p app --test it "),
            "{command}"
        );
        assert!(command.ends_with(" tests::x -- --exact"), "{command}");
        assert!(!command.contains("--workspace"), "{command}");
        Ok(())
    }

    #[tokio::test]
    async fn test_one_rejects_unknown_package_and_target_before_running() -> TestResult {
        let fx = Fixture::new()?;
        let ctx = fx.exec_ctx()?;
        let shell = test_one_shell(&fx.ws, "\n");
        let provider = CargoToolProvider::new(ShellDelegate::new(shell.clone()));
        let tool = provider
            .executor(&ToolName::new("cargo.test_one"))
            .ok_or(TestError::Missing("cargo.test_one"))?;
        let unknown_package = error_of(
            run(
                tool.as_ref(),
                &ctx,
                "cargo.test_one",
                json!({"package": "nope", "target": "lib", "test": "tests::x"}),
            )
            .await?,
        )?;
        assert!(
            unknown_package.contains("not a workspace member"),
            "{unknown_package}"
        );
        let unknown_test = error_of(
            run(
                tool.as_ref(),
                &ctx,
                "cargo.test_one",
                json!({"package": "app", "target": "test:missing", "test": "tests::x"}),
            )
            .await?,
        )?;
        assert!(
            unknown_test.contains("no integration test 'missing'"),
            "{unknown_test}"
        );
        assert!(cargo_test_calls(&shell).is_empty(), "{:?}", shell.calls());
        assert!(
            shell.calls().iter().all(|(c, _)| !c.starts_with("git ")),
            "no change detection before the target is validated: {:?}",
            shell.calls()
        );
        Ok(())
    }

    #[test]
    fn provider_exposes_nine_tools_with_documented_schemas() -> TestResult {
        let shell: Arc<ScriptedShell> = ScriptedShell::json(json!({"exit_code": 0}));
        let provider = CargoToolProvider::new(ShellDelegate::new(shell));
        let specs = provider.tools();
        assert_eq!(specs.len(), 9);
        for name in CARGO_TOOL_NAMES {
            assert!(name.starts_with("cargo."), "{name}");
            assert!(provider.executor(&ToolName::new(*name)).is_some());
        }
        assert!(provider.parallel_safe(&ToolName::new("cargo.fmt_check")));
        for name in [
            "cargo.check",
            "cargo.build",
            "cargo.test",
            "cargo.clippy",
            "cargo.doc",
            "cargo.tree",
            "cargo.metadata",
            "cargo.test_one",
        ] {
            assert!(!provider.parallel_safe(&ToolName::new(name)), "{name}");
        }
        for spec in specs {
            let ToolSpec::Function(function) = spec;
            let name = function.name.as_str().to_owned();
            assert!(
                function.description.len() > 60 && function.description.contains(". "),
                "{name}"
            );
            for (field, schema) in function.parameters.properties.clone().unwrap_or_default() {
                assert!(
                    schema.description.as_ref().is_some_and(|d| d.len() > 8),
                    "{name}.{field}"
                );
            }
            assert!(
                function
                    .parameters
                    .clone()
                    .into_strict()
                    .additional_properties
                    .is_some()
            );
        }
        assert!(
            CargoToolProvider::TOOL_PERMISSIONS
                .iter()
                .all(|p| *p == Some(harw_authority::Permission::ExecuteProcess))
        );
        Ok(())
    }

    #[test]
    fn sources_never_start_a_process_themselves() -> TestResult {
        let needles = [
            ["std::proc", "ess::Command"].concat(),
            ["tokio::proc", "ess"].concat(),
            ["Command::", "new("].concat(),
            [".spa", "wn("].concat(),
            ["libc::", "system"].concat(),
        ];
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path)?.replace("spawn_blocking(", "");
            for needle in &needles {
                assert!(
                    !text.contains(needle.as_str()),
                    "{} contains {needle}",
                    path.display()
                );
            }
        }
        Ok(())
    }
}
