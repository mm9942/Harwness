//! Gate `spawn`: Harw application code does not spawn workload processes.
//!
//! # The rule (docs/planning/93-unify-process-runtime)
//! On Linux every workload process is started by the job runtime
//! (`LinuxExecutor`): spawn, pidfd, cgroup, sandbox, kill, deadline and recovery
//! have one owner. Application code submits jobs.
//!
//! # Ratchet
//! The tree still has surfaces that start processes themselves (`shell.exec`,
//! the operator `!` path, `latex.*`, `job.start`). This gate freezes that set:
//!
//! - A non-test source file that starts a process (`Command::new`) and is neither
//!   in an infrastructure crate nor listed in `xtask/spawn-policy.toml` is a
//!   **violation** (a new spawn site).
//! - A listed file that no longer starts a process is a **violation** too: the
//!   list can only shrink, so a migration removes its entry in the same change.
//!
//! # What counts
//! `Command::new(` with no prefix or one of `Tokio`, `Process`, `Std`, `Async`
//! (`tokio::process::Command::new`, `TokioCommand::new`, ...), outside the
//! `#[cfg(test)] mod ...` block, comments, and files named `tests.rs`,
//! `test_support.rs` or `*_tests.rs`. A heuristic, deliberately conservative:
//! it reads text, not the type system.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use super::GateReport;

/// Where the policy lives.
pub const POLICY_PATH: &str = "xtask/spawn-policy.toml";

/// Directories never scanned.
const SKIPPED_DIRS: [&str; 8] = [
    "target",
    ".git",
    ".claude",
    "node_modules",
    "tests",
    "benches",
    "examples",
    "fixtures",
];

/// Prefixes under which `Command::new(` is a process `Command`.
const COMMAND_PREFIXES: [&str; 5] = ["", "Tokio", "Process", "Std", "Async"];

/// The parsed policy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    /// Infrastructure crates (path up to `/src/`) allowed to spawn, with the reason.
    pub crates: BTreeMap<String, String>,
    /// Individual files allowed to spawn, with the reason.
    pub files: BTreeMap<String, String>,
}

impl Policy {
    /// Parses the policy text.
    ///
    /// # Errors
    /// A malformed document, an unknown table, or a non-string reason.
    pub fn parse(text: &str) -> Result<Self, String> {
        let root: toml::Table =
            toml::from_str(text).map_err(|error| format!("{POLICY_PATH}: {error}"))?;
        let mut policy = Self::default();
        for (table_name, target) in [("crates", &mut policy.crates), ("files", &mut policy.files)] {
            let Some(value) = root.get(table_name) else {
                continue;
            };
            let table = value
                .as_table()
                .ok_or_else(|| format!("{POLICY_PATH}: `{table_name}` must be a table"))?;
            for (key, reason) in table {
                let reason = reason
                    .as_str()
                    .filter(|reason| !reason.trim().is_empty())
                    .ok_or_else(|| {
                        format!("{POLICY_PATH}: `{table_name}.{key}` needs a non-empty reason")
                    })?;
                target.insert(key.clone(), reason.to_owned());
            }
        }
        if let Some(unknown) = root
            .keys()
            .find(|key| !matches!(key.as_str(), "crates" | "files"))
        {
            return Err(format!("{POLICY_PATH}: unknown table `{unknown}`"));
        }
        Ok(policy)
    }
}

/// Whether `text` (already without comments and test modules) starts a process.
#[must_use]
pub fn starts_process(text: &str) -> bool {
    const NEEDLE: &str = "Command::new(";
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(found) = text[from..].find(NEEDLE) {
        let at = from + found;
        // The identifier run directly before `Command` is the prefix.
        let mut start = at;
        while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
            start -= 1;
        }
        if COMMAND_PREFIXES.contains(&&text[start..at]) {
            return true;
        }
        from = at + NEEDLE.len();
    }
    false
}

/// The part of a source file that is not test code or comments.
#[must_use]
pub fn non_test_code(source: &str) -> String {
    let cut = source
        .match_indices("#[cfg(test)]")
        .find(|(index, _)| {
            let rest = source[index + "#[cfg(test)]".len()..].trim_start();
            let rest = rest
                .strip_prefix("pub(crate)")
                .map_or(rest, str::trim_start);
            let rest = rest.strip_prefix("pub").map_or(rest, str::trim_start);
            rest.starts_with("mod ")
        })
        .map_or(source.len(), |(index, _)| index);
    source[..cut]
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_test_file(name: &str) -> bool {
    name == "tests.rs" || name == "test_support.rs" || name.ends_with("_tests.rs")
}

/// The crate directory of a file: everything before `/src/`.
fn crate_of(path: &str) -> Option<&str> {
    path.find("/src/").map(|index| &path[..index])
}

/// Every non-test source file below `root` that starts a process.
///
/// # Errors
/// An unreadable directory or file.
pub fn scan(root: &Path) -> Result<(BTreeSet<String>, usize), String> {
    let mut found = BTreeSet::new();
    let mut scanned = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let entries = fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let kind = entry
                .file_type()
                .map_err(|e| format!("{}: {e}", path.display()))?;
            if kind.is_dir() {
                if !SKIPPED_DIRS.contains(&name.as_str()) {
                    pending.push(path);
                }
                continue;
            }
            if !name.ends_with(".rs") || is_test_file(&name) {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            if !format!("/{relative}").contains("/src/") {
                continue;
            }
            scanned += 1;
            let source =
                fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            if starts_process(&non_test_code(&source)) {
                found.insert(relative);
            }
        }
    }
    Ok((found, scanned))
}

/// Judges the scan against the policy.
#[must_use]
pub fn evaluate(found: &BTreeSet<String>, scanned: usize, policy: &Policy) -> GateReport {
    let mut violations = Vec::new();
    let mut crates_seen = BTreeSet::new();
    for file in found {
        let crate_dir = crate_of(file);
        let by_crate = crate_dir.is_some_and(|dir| policy.crates.contains_key(dir));
        if let Some(dir) = crate_dir {
            if by_crate {
                crates_seen.insert(dir.to_owned());
            }
        }
        if !by_crate && !policy.files.contains_key(file) {
            violations.push(format!(
                "{file}: starts a process but is neither an infrastructure crate nor on the \
                 migration list; submit a job through harw-job instead (PL-93)"
            ));
        }
    }
    for file in policy.files.keys() {
        if !found.contains(file) {
            violations.push(format!(
                "{file}: listed in {POLICY_PATH} but no longer starts a process; remove the \
                 entry (the list can only shrink)"
            ));
        }
    }
    for dir in policy.crates.keys() {
        if !crates_seen.contains(dir) {
            violations.push(format!(
                "{dir}: listed as infrastructure in {POLICY_PATH} but no file of it starts a \
                 process; remove the entry"
            ));
        }
    }
    GateReport {
        name: "spawn",
        checked: scanned,
        violations,
    }
}

/// Runs the gate in the current directory (the repository root).
///
/// # Errors
/// An unreadable tree or policy.
pub fn run() -> Result<GateReport, String> {
    let root = Path::new(".");
    let text = fs::read_to_string(root.join(POLICY_PATH))
        .map_err(|error| format!("{POLICY_PATH}: {error}"))?;
    let policy = Policy::parse(&text)?;
    let (found, scanned) = scan(root)?;
    Ok(evaluate(&found, scanned, &policy))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn only_process_commands_count() {
        assert!(starts_process("let c = tokio::process::Command::new(x);"));
        assert!(starts_process("let c = TokioCommand::new(x);"));
        assert!(starts_process("let c = ProcessCommand::new(\"git\");"));
        assert!(starts_process("Command::new(\"sh\")"));
        assert!(!starts_process("OperatorCommand::new(cmd, cwd)"));
        assert!(!starts_process("ShellCommand::new(x)"));
        assert!(!starts_process("no spawn here"));
    }

    #[test]
    fn comments_and_the_test_module_are_ignored() {
        let source = "// Command::new(\"x\")\nfn a() {}\n#[cfg(test)]\nmod tests {\n    fn t() { Command::new(\"x\"); }\n}\n";
        assert!(!starts_process(&non_test_code(source)));
        // A cfg(test) attribute on something else does not hide the rest.
        let source = "#[cfg(test)]\nuse x;\nfn a() { Command::new(\"y\"); }\n";
        assert!(starts_process(&non_test_code(source)));
    }

    fn policy() -> TestResult<Policy> {
        Policy::parse(
            "[crates]\n\"infra\" = \"the executor\"\n[files]\n\"app/src/old.rs\" = \"migrate: PL-93\"\n",
        )
        .map_err(crate::test_support::TestError::Unexpected)
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn a_new_spawn_site_is_a_violation_and_listed_ones_pass() -> TestResult {
        let ok = evaluate(
            &set(&["infra/src/exec.rs", "app/src/old.rs"]),
            10,
            &policy()?,
        );
        assert!(ok.is_green(), "{:?}", ok.violations);
        let bad = evaluate(
            &set(&["infra/src/exec.rs", "app/src/old.rs", "app/src/new.rs"]),
            10,
            &policy()?,
        );
        assert_eq!(bad.violations.len(), 1);
        assert!(bad.violations[0].starts_with("app/src/new.rs"));
        Ok(())
    }

    #[test]
    fn the_list_can_only_shrink() -> TestResult {
        // The listed file migrated: its entry must go.
        let stale = evaluate(&set(&["infra/src/exec.rs"]), 10, &policy()?);
        assert_eq!(stale.violations.len(), 1);
        assert!(stale.violations[0].contains("no longer starts a process"));
        // An infrastructure crate that stopped spawning is stale as well.
        let stale_crate = evaluate(&set(&["app/src/old.rs"]), 10, &policy()?);
        assert!(stale_crate.violations[0].contains("listed as infrastructure"));
        Ok(())
    }

    #[test]
    fn nothing_scanned_is_not_green() -> TestResult {
        let report = evaluate(&BTreeSet::new(), 0, &Policy::default());
        assert!(!report.is_green());
        Ok(())
    }

    #[test]
    fn a_policy_needs_reasons_and_known_tables() {
        assert!(Policy::parse("[files]\n\"a.rs\" = \"\"\n").is_err());
        assert!(Policy::parse("[other]\nx = 1\n").is_err());
        assert!(Policy::parse("").is_ok());
    }

    #[test]
    fn scanning_a_tree_finds_only_non_test_spawns() -> TestResult {
        let root =
            harw_test_support::unique_tmp_created("xtask", "spawn-scan").map_err(ctx("tempdir"))?;
        let src = root.join("app/src");
        fs::create_dir_all(&src).map_err(ctx("mkdir"))?;
        fs::write(src.join("a.rs"), "fn a() { Command::new(\"x\"); }").map_err(ctx("write"))?;
        fs::write(
            src.join("b.rs"),
            "fn b() {}\n#[cfg(test)]\nmod tests { fn t() { Command::new(\"x\"); } }",
        )
        .map_err(ctx("write"))?;
        fs::write(src.join("tests.rs"), "fn t() { Command::new(\"x\"); }").map_err(ctx("write"))?;
        let (found, scanned) = scan(&root).map_err(crate::test_support::TestError::Unexpected)?;
        assert_eq!(found, set(&["app/src/a.rs"]));
        let _ = fs::remove_dir_all(&root);
        assert_eq!(scanned, 2, "tests.rs is not scanned");
        Ok(())
    }
}
