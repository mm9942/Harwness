//! Pre-flight rebuild guard of `cargo.test_one` (pure, no I/O, no compilation).
//!
//! `cargo.test_one` runs exactly one test, and only when that run will not
//! recompile a large part of the workspace. This module decides that from
//! plain data: the workspace graph (parsed from `cargo metadata --no-deps`),
//! the list of changed paths (from `git status`) and a threshold. The tool in
//! [`crate::tools`] fetches the two inputs through the injected `shell.exec`
//! delegate and calls [`assess`]; nothing here starts a process.
//!
//! # Model
//! - The *closure* of the tested package is the package itself plus every
//!   workspace package reachable through normal and build dependencies, plus
//!   the dev-dependencies of the root only (cargo builds only the root's
//!   dev-dependencies for `cargo test -p root`).
//! - A closure member is *dirty* when a changed path lies inside its package
//!   directory (longest-prefix match).
//! - The *rebuild set* is every closure member that is dirty or depends
//!   (inside the closure) on a rebuild-set member. Cargo rebuilds exactly
//!   those, because a changed crate invalidates all of its dependents.
//! - The run is refused when the rebuild set is larger than
//!   [`MAX_REBUILD_PACKAGES`], or when a file changed that invalidates the
//!   whole build ([`is_global_file`]).
//!
//! Not modelled: the target directory being cold (no artifacts at all). That
//! cannot be detected reliably from here (`CARGO_TARGET_DIR`, shared target
//! dirs, sandbox mounts), so a cold cache is not a refusal reason.

use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Largest accepted rebuild set (the tested package itself counts).
pub const MAX_REBUILD_PACKAGES: usize = 3;

/// Default and largest time limit of `cargo.test_one`, in seconds.
pub const DEFAULT_TIMEOUT_SECS: u64 = 300;
/// Largest time limit of `cargo.test_one`, in seconds.
pub const MAX_TIMEOUT_SECS: u64 = 600;

/// How many names a refusal lists per set before it is cut.
const LIST_CAP: usize = 40;

/// Kind of a dependency edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepKind {
    /// `[dependencies]`.
    Normal,
    /// `[build-dependencies]`.
    Build,
    /// `[dev-dependencies]`.
    Dev,
}

/// One workspace package.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PackageInfo {
    /// Package directory relative to the workspace root (`""` for the root).
    pub dir: String,
    /// Workspace-internal dependency edges (package name, kind).
    pub deps: Vec<(String, DepKind)>,
    /// The package has a library target.
    pub has_lib: bool,
    /// Names of the integration test targets.
    pub test_targets: BTreeSet<String>,
}

/// The workspace graph.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Workspace {
    /// Workspace members by package name.
    pub packages: BTreeMap<String, PackageInfo>,
}

/// The test target of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// `--lib`.
    Lib,
    /// `--test <name>`.
    Test(String),
}

impl Target {
    /// Parses `"lib"` or `"test:<name>"`.
    ///
    /// # Errors
    /// Message naming the accepted forms.
    pub fn parse(value: &str) -> Result<Self, String> {
        if value == "lib" {
            return Ok(Self::Lib);
        }
        if let Some(name) = value.strip_prefix("test:") {
            crate::command::target_name(name)?;
            return Ok(Self::Test(name.to_owned()));
        }
        Err(format!(
            "target must be \"lib\" or \"test:<integration test name>\", got '{}'",
            value.chars().take(40).collect::<String>()
        ))
    }

    /// Spelling as accepted by [`Target::parse`].
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Lib => "lib".to_owned(),
            Self::Test(name) => format!("test:{name}"),
        }
    }
}

/// The fixed `argv` of one run: no `--workspace`, no free-form arguments.
///
/// `package` and `test` must be validated by the caller.
#[must_use]
pub fn test_one_argv(package: &str, target: &Target, test: &str) -> Vec<String> {
    let mut argv: Vec<String> = ["cargo", "test", "-p", package]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    match target {
        Target::Lib => argv.push("--lib".to_owned()),
        Target::Test(name) => argv.extend(["--test".to_owned(), name.clone()]),
    }
    argv.extend(
        [
            "--locked",
            "--message-format=short",
            "--color",
            "never",
            test,
            "--",
            "--exact",
        ]
        .iter()
        .map(|s| (*s).to_owned()),
    );
    argv
}

impl Workspace {
    /// Builds the graph from `cargo metadata --format-version 1 --no-deps`.
    ///
    /// # Errors
    /// Message when the document does not have the expected shape.
    pub fn from_metadata(metadata: &Value) -> Result<Self, String> {
        let root = metadata
            .get("workspace_root")
            .and_then(Value::as_str)
            .ok_or("cargo metadata has no workspace_root")?;
        let packages = metadata
            .get("packages")
            .and_then(Value::as_array)
            .ok_or("cargo metadata has no packages")?;
        let names: BTreeSet<&str> = packages
            .iter()
            .filter_map(|p| p.get("name").and_then(Value::as_str))
            .collect();
        let mut out = BTreeMap::new();
        for package in packages {
            let name = package
                .get("name")
                .and_then(Value::as_str)
                .ok_or("package without name")?;
            let manifest = package
                .get("manifest_path")
                .and_then(Value::as_str)
                .ok_or("package without manifest_path")?;
            let dir = relative_dir(root, manifest)
                .ok_or_else(|| format!("manifest of {name} lies outside the workspace root"))?;
            let mut info = PackageInfo {
                dir,
                ..PackageInfo::default()
            };
            for dep in package
                .get("dependencies")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(dep_name) = dep.get("name").and_then(Value::as_str) else {
                    continue;
                };
                // Path/workspace dependencies only: registry crates are not
                // part of the workspace and never become dirty.
                if !names.contains(dep_name) || !dep.get("source").is_none_or(Value::is_null) {
                    continue;
                }
                let kind = match dep.get("kind").and_then(Value::as_str) {
                    Some("dev") => DepKind::Dev,
                    Some("build") => DepKind::Build,
                    _ => DepKind::Normal,
                };
                info.deps.push((dep_name.to_owned(), kind));
            }
            for target in package
                .get("targets")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let kinds: Vec<&str> = target
                    .get("kind")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                let target_name = target.get("name").and_then(Value::as_str).unwrap_or("");
                if kinds
                    .iter()
                    .any(|k| matches!(*k, "lib" | "rlib" | "proc-macro" | "dylib" | "cdylib"))
                {
                    info.has_lib = true;
                }
                if kinds.contains(&"test") {
                    info.test_targets.insert(target_name.to_owned());
                }
            }
            out.insert(name.to_owned(), info);
        }
        Ok(Self { packages: out })
    }

    /// Package names that contain `needle` (for "did you mean" hints).
    #[must_use]
    pub fn similar(&self, needle: &str) -> Vec<String> {
        self.packages
            .keys()
            .filter(|name| name.contains(needle) || needle.contains(name.as_str()))
            .take(8)
            .cloned()
            .collect()
    }

    /// Dependency closure of `root` (see the module docs).
    #[must_use]
    pub fn closure(&self, root: &str) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut queue = VecDeque::new();
        if self.packages.contains_key(root) {
            seen.insert(root.to_owned());
            queue.push_back(root.to_owned());
        }
        while let Some(name) = queue.pop_front() {
            for dep in self.closure_edges(root, &name) {
                if seen.insert(dep.to_owned()) {
                    queue.push_back(dep.to_owned());
                }
            }
        }
        seen
    }

    /// Edges that matter for the build of `root`: dev-dependencies count for
    /// the root only.
    fn closure_edges<'a>(&'a self, root: &str, name: &str) -> impl Iterator<Item = &'a str> {
        let is_root = name == root;
        self.packages
            .get(name)
            .into_iter()
            .flat_map(|p| p.deps.iter())
            .filter(move |(_, kind)| is_root || *kind != DepKind::Dev)
            .map(|(dep, _)| dep.as_str())
            .filter(|dep| self.packages.contains_key(*dep))
    }

    /// The package that owns `path` (longest directory prefix), if any.
    ///
    /// A package at the workspace root only owns `src/`, `tests/`, `benches/`
    /// and `examples/` so that docs and scripts do not make it dirty.
    #[must_use]
    pub fn owner(&self, path: &str) -> Option<&str> {
        let mut best: Option<(&str, usize)> = None;
        for (name, info) in &self.packages {
            let dir = info.dir.as_str();
            let owns = if dir.is_empty() {
                ["src/", "tests/", "benches/", "examples/"]
                    .iter()
                    .any(|p| path.starts_with(p))
            } else {
                path == dir
                    || path
                        .strip_prefix(dir)
                        .is_some_and(|rest| rest.starts_with('/'))
            };
            if owns && best.is_none_or(|(_, len)| dir.len() >= len) {
                best = Some((name.as_str(), dir.len()));
            }
        }
        best.map(|(name, _)| name)
    }
}

fn relative_dir(root: &str, manifest: &str) -> Option<String> {
    let dir = manifest.rsplit_once('/').map_or("", |(dir, _)| dir);
    let rest = dir.strip_prefix(root)?;
    if rest.is_empty() {
        Some(String::new())
    } else {
        rest.strip_prefix('/').map(str::to_owned)
    }
}

/// Files whose change invalidates (almost) the whole build.
#[must_use]
pub fn is_global_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    if matches!(
        name,
        "Cargo.toml" | "Cargo.lock" | "rust-toolchain" | "rust-toolchain.toml" | "build.rs"
    ) {
        return true;
    }
    let mut parts = path.rsplit('/');
    let last = parts.next();
    let parent = parts.next();
    parent == Some(".cargo") && matches!(last, Some("config" | "config.toml"))
}

/// Parses `git status --porcelain=v1 -z --no-renames` into paths relative to
/// the workspace root. `prefix` is `git rev-parse --show-prefix` (path of the
/// workspace root inside the repository, with trailing `/`, or empty).
/// Entries outside the workspace root are dropped.
#[must_use]
pub fn parse_porcelain_z(output: &str, prefix: &str) -> Vec<String> {
    let mut paths = BTreeSet::new();
    for entry in output.split('\0') {
        if entry.len() < 4 || entry.as_bytes().get(2) != Some(&b' ') {
            continue;
        }
        let Some(path) = entry.get(3..) else { continue };
        if let Some(rel) = path.strip_prefix(prefix) {
            if !rel.is_empty() {
                paths.insert(rel.to_owned());
            }
        }
    }
    paths.into_iter().collect()
}

/// Why a run is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// A dependency of the tested package changed; too many crates rebuild.
    WouldRecompile,
    /// `Cargo.toml`, `Cargo.lock`, toolchain, cargo config or a `build.rs`
    /// changed.
    GlobalInvalidation,
}

impl Reason {
    /// Stable spelling used in the result JSON.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WouldRecompile => "would_recompile",
            Self::GlobalInvalidation => "global_invalidation",
        }
    }
}

/// The outcome of [`assess`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    /// Tested package.
    pub package: String,
    /// Size of the dependency closure.
    pub closure_size: usize,
    /// All dirty workspace packages (also those outside the closure).
    pub changed_packages: Vec<String>,
    /// Dirty packages inside the closure (the cause of a rebuild).
    pub dirty_in_closure: Vec<String>,
    /// Changed files that invalidate the whole build.
    pub changed_global_files: Vec<String>,
    /// Packages cargo would recompile, sorted.
    pub rebuild_set: Vec<String>,
}

impl Assessment {
    /// `None` when the run may start.
    #[must_use]
    pub fn refusal(&self) -> Option<Reason> {
        if !self.changed_global_files.is_empty() {
            Some(Reason::GlobalInvalidation)
        } else if self.rebuild_set.len() > MAX_REBUILD_PACKAGES {
            Some(Reason::WouldRecompile)
        } else {
            None
        }
    }

    /// The structured refusal for `reason`.
    #[must_use]
    pub fn refusal_json(&self, reason: Reason, target: &Target, test: &str) -> Value {
        json!({
            "status": "refused",
            "reason": reason.as_str(),
            "package": self.package,
            "target": target.label(),
            "test": test,
            "rebuild_count": self.rebuild_set.len(),
            "threshold": MAX_REBUILD_PACKAGES,
            "rebuild_set": capped(&self.rebuild_set),
            "changed_packages": capped(&self.changed_packages),
            "dirty_in_closure": capped(&self.dirty_in_closure),
            "changed_global_files": capped(&self.changed_global_files),
            "closure_size": self.closure_size,
            "hint": self.hint(reason),
        })
    }

    fn hint(&self, reason: Reason) -> String {
        match reason {
            Reason::GlobalInvalidation => format!(
                "No test was run. These files invalidate the whole build cache: {}. \
                 Commit or stash them (or revert the manifest/lockfile/toolchain/build.rs \
                 edit) before calling cargo.test_one, or run this test as a background \
                 job (job.start) where a long rebuild is acceptable.",
                self.changed_global_files
                    .iter()
                    .take(8)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Reason::WouldRecompile => {
                let others: Vec<&str> = self
                    .dirty_in_closure
                    .iter()
                    .map(String::as_str)
                    .filter(|p| *p != self.package)
                    .take(8)
                    .collect();
                let cause = if others.is_empty() {
                    format!("changes in {}", self.package)
                } else {
                    format!("uncommitted changes in {}", others.join(", "))
                };
                format!(
                    "No test was run. Cargo would recompile {} crates ({} allowed) because of \
                     {cause}. Commit or stash unrelated changes in those crates, move the \
                     test to a lower-level crate with fewer dependents, or run this test as \
                     a background job (job.start) where a long rebuild is acceptable.",
                    self.rebuild_set.len(),
                    MAX_REBUILD_PACKAGES
                )
            }
        }
    }
}

fn capped(items: &[String]) -> Value {
    let mut shown: Vec<Value> = items.iter().take(LIST_CAP).map(|s| json!(s)).collect();
    if items.len() > LIST_CAP {
        shown.push(json!(format!("... and {} more", items.len() - LIST_CAP)));
    }
    Value::Array(shown)
}

/// Computes the rebuild set of testing `package` given the changed `paths`
/// (relative to the workspace root).
#[must_use]
pub fn assess(ws: &Workspace, package: &str, paths: &[String]) -> Assessment {
    let closure = ws.closure(package);
    let mut changed: BTreeSet<&str> = BTreeSet::new();
    let mut global: BTreeSet<String> = BTreeSet::new();
    for path in paths {
        if is_global_file(path) {
            global.insert(path.clone());
        }
        if let Some(owner) = ws.owner(path) {
            changed.insert(owner);
        }
    }
    // Reverse edges inside the closure, then propagate from the dirty members.
    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for name in &closure {
        for dep in ws.closure_edges(package, name) {
            dependents.entry(dep).or_default().push(name.as_str());
        }
    }
    let dirty_in_closure: Vec<String> = closure
        .iter()
        .filter(|name| changed.contains(name.as_str()))
        .cloned()
        .collect();
    let mut rebuild: BTreeSet<&str> = BTreeSet::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    for name in &dirty_in_closure {
        if rebuild.insert(name.as_str()) {
            queue.push_back(name.as_str());
        }
    }
    while let Some(name) = queue.pop_front() {
        for dependent in dependents.get(name).into_iter().flatten() {
            if rebuild.insert(dependent) {
                queue.push_back(dependent);
            }
        }
    }
    let rebuild_set = rebuild.iter().map(|s| (*s).to_owned()).collect();
    Assessment {
        package: package.to_owned(),
        closure_size: closure.len(),
        changed_packages: changed.iter().map(|s| (*s).to_owned()).collect(),
        dirty_in_closure,
        changed_global_files: global.into_iter().collect(),
        rebuild_set,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    /// `app` (root, dev-dep `fixtures`) -> `mid` -> `low`; `app` -> `side`.
    /// `fixtures` -> `low`. `other` is outside the closure and depends on `low`.
    fn graph() -> Workspace {
        let mut packages = BTreeMap::new();
        let mut add = |name: &str, deps: &[(&str, DepKind)]| {
            packages.insert(
                name.to_owned(),
                PackageInfo {
                    dir: name.to_owned(),
                    deps: deps.iter().map(|(d, k)| ((*d).to_owned(), *k)).collect(),
                    has_lib: true,
                    test_targets: BTreeSet::new(),
                },
            );
        };
        add("low", &[]);
        add("mid", &[("low", DepKind::Normal)]);
        add("side", &[]);
        add("fixtures", &[("low", DepKind::Normal)]);
        add(
            "app",
            &[
                ("mid", DepKind::Normal),
                ("side", DepKind::Build),
                ("fixtures", DepKind::Dev),
            ],
        );
        add("other", &[("low", DepKind::Normal)]);
        Workspace { packages }
    }

    fn paths(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn clean_tree_is_allowed() {
        let a = assess(&graph(), "app", &[]);
        assert!(a.rebuild_set.is_empty());
        assert_eq!(a.refusal(), None);
        assert_eq!(a.closure_size, 5);
    }

    #[test]
    fn dirty_leaf_in_closure_is_allowed_when_small() {
        let a = assess(&graph(), "app", &paths(&["app/src/lib.rs"]));
        assert_eq!(a.rebuild_set, vec!["app"]);
        assert_eq!(a.refusal(), None);
        let a = assess(&graph(), "app", &paths(&["side/src/lib.rs"]));
        assert_eq!(a.rebuild_set, vec!["app", "side"]);
        assert_eq!(a.refusal(), None);
    }

    #[test]
    fn dirty_low_level_crate_with_many_dependents_is_refused() {
        let a = assess(&graph(), "app", &paths(&["low/src/lib.rs"]));
        // low, mid, fixtures (dev of the root) and app.
        assert_eq!(a.rebuild_set, vec!["app", "fixtures", "low", "mid"]);
        assert_eq!(a.refusal(), Some(Reason::WouldRecompile));
        let json = a.refusal_json(Reason::WouldRecompile, &Target::Lib, "t::x");
        assert_eq!(json["status"], "refused");
        assert_eq!(json["reason"], "would_recompile");
        assert_eq!(json["rebuild_count"], 4);
        assert_eq!(json["threshold"], MAX_REBUILD_PACKAGES);
        assert_eq!(json["target"], "lib");
        assert_eq!(json["rebuild_set"][0], "app");
        assert!(json["hint"].as_str().is_some_and(|h| h.contains("low")));
    }

    #[test]
    fn dev_dependencies_of_other_crates_do_not_count() {
        let mut ws = graph();
        if let Some(mid) = ws.packages.get_mut("mid") {
            mid.deps.push(("other".to_owned(), DepKind::Dev));
        }
        let a = assess(&ws, "app", &paths(&["other/src/lib.rs"]));
        assert!(a.rebuild_set.is_empty());
        assert_eq!(a.changed_packages, vec!["other"]);
    }

    #[test]
    fn dirty_crate_outside_the_closure_is_ignored() {
        let a = assess(&graph(), "mid", &paths(&["other/src/lib.rs", "docs/x.md"]));
        assert!(a.rebuild_set.is_empty());
        assert_eq!(a.changed_packages, vec!["other"]);
        assert_eq!(a.refusal(), None);
    }

    #[test]
    fn global_files_refuse() {
        for file in [
            "Cargo.lock",
            "Cargo.toml",
            "other/Cargo.toml",
            "rust-toolchain.toml",
            ".cargo/config.toml",
            "mid/build.rs",
        ] {
            let a = assess(&graph(), "side", &paths(&[file]));
            assert_eq!(a.refusal(), Some(Reason::GlobalInvalidation), "{file}");
            let json = a.refusal_json(Reason::GlobalInvalidation, &Target::Lib, "t");
            assert_eq!(json["reason"], "global_invalidation");
            assert_eq!(json["changed_global_files"][0], file);
        }
        assert!(!is_global_file("mid/src/cargo.toml.rs"));
        assert!(!is_global_file("mid/src/lib.rs"));
    }

    #[test]
    fn longest_prefix_owns_a_path() {
        let mut ws = graph();
        ws.packages.insert(
            "nested".to_owned(),
            PackageInfo {
                dir: "mid/nested".to_owned(),
                ..PackageInfo::default()
            },
        );
        assert_eq!(ws.owner("mid/nested/src/lib.rs"), Some("nested"));
        assert_eq!(ws.owner("mid/src/lib.rs"), Some("mid"));
        assert_eq!(ws.owner("midway/src/lib.rs"), None);
        assert_eq!(ws.owner("README.md"), None);
    }

    #[test]
    fn argv_is_fixed_and_exact() {
        for target in [Target::Lib, Target::Test("it".to_owned())] {
            let argv = test_one_argv("mid", &target, "a::b");
            assert!(!argv.iter().any(|a| a == "--workspace"));
            assert!(!argv.iter().any(|a| a == "--all-targets"));
            assert!(!argv.iter().any(|a| a == "--no-fail-fast"));
            assert!(!argv.iter().any(|a| a == "--release"));
            assert!(argv.iter().any(|a| a == "--exact"));
            assert!(argv.iter().any(|a| a == "--locked"));
            assert_eq!(argv.last().map(String::as_str), Some("--exact"));
        }
        assert_eq!(
            test_one_argv("mid", &Target::Test("it".to_owned()), "a::b").join(" "),
            "cargo test -p mid --test it --locked --message-format=short --color never a::b -- --exact"
        );
    }

    #[test]
    fn target_parsing() -> TestResult {
        assert_eq!(Target::parse("lib").ok(), Some(Target::Lib));
        assert_eq!(
            Target::parse("test:it_1").ok(),
            Some(Target::Test("it_1".to_owned()))
        );
        assert!(Target::parse("test:").is_err());
        assert!(Target::parse("test:--x").is_err());
        assert!(Target::parse("bin:x").is_err());
        assert!(Target::parse("all").is_err());
        Ok(())
    }

    #[test]
    fn porcelain_paths_are_made_workspace_relative() {
        let out = " M harw-a/src/lib.rs\0?? harw-b/new.rs\0D  other/x.rs\0";
        assert_eq!(
            parse_porcelain_z(out, ""),
            vec!["harw-a/src/lib.rs", "harw-b/new.rs", "other/x.rs"]
        );
        assert_eq!(
            parse_porcelain_z(out, "harw-a/"),
            vec!["src/lib.rs".to_owned()]
        );
        assert!(parse_porcelain_z("", "").is_empty());
    }

    #[test]
    fn metadata_is_parsed() -> TestResult {
        let metadata = json!({
            "workspace_root": "/w",
            "packages": [
                {"name": "a", "manifest_path": "/w/a/Cargo.toml", "dependencies": [
                    {"name": "b", "source": null, "kind": null, "path": "/w/b"},
                    {"name": "b", "source": null, "kind": "dev", "path": "/w/b"},
                    {"name": "serde", "source": "registry+x", "kind": null}
                ], "targets": [
                    {"name": "a", "kind": ["lib"]},
                    {"name": "it", "kind": ["test"]}
                ]},
                {"name": "b", "manifest_path": "/w/b/Cargo.toml", "dependencies": [], "targets": [
                    {"name": "b", "kind": ["bin"]}
                ]}
            ]
        });
        let ws = Workspace::from_metadata(&metadata)
            .map_err(crate::test_support::TestError::Unexpected)?;
        let a = &ws.packages["a"];
        assert_eq!(a.dir, "a");
        assert!(a.has_lib);
        assert!(a.test_targets.contains("it"));
        assert_eq!(
            a.deps,
            vec![
                ("b".to_owned(), DepKind::Normal),
                ("b".to_owned(), DepKind::Dev)
            ]
        );
        assert!(!ws.packages["b"].has_lib);
        assert!(Workspace::from_metadata(&json!({})).is_err());
        Ok(())
    }
}
