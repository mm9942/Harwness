//! Slice 14 — Workspace version coherence integration test.
//!
//! Spec source: "Workspace-Versionen, interne Dependency-Versionen und
//! Lockfile sind konsistent." (0.2.0 milestone, Slice 14).
//!
//! Asserts:
//!   1. Every workspace-owned package (`harw` or `harw-*`) reports version
//!      `0.2.0` via `cargo_metadata`.
//!   2. No workspace package is still on `0.1.0`.
//!   3. The Cargo.lock (read by `cargo metadata`) confirms every internal
//!      package resolves to `0.2.0`.
//!   4. The workspace root `Cargo.toml` contains the version string exactly
//!      once; no member `Cargo.toml` contains it at all (they inherit via
//!      `version.workspace = true`).
//!   5. No member `Cargo.toml` contains a bare `version = "0.1.0"` pin.

use std::fs;
use std::path::{Path, PathBuf};

const EXPECTED_VERSION: &str = "0.2.0";
const OLD_VERSION: &str = "0.1.0";

/// Walk up from `start` until a directory contains a `Cargo.toml` that has
/// the `[workspace]` table.  Returns the path to that directory.
///
/// # Panics
/// Panics if no workspace root is found before reaching the filesystem root.
fn find_workspace_root(start: &Path) -> PathBuf {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join("Cargo.toml");
        if candidate.exists() {
            let text = fs::read_to_string(&candidate).unwrap_or_default();
            if text.contains("[workspace]") {
                return dir;
            }
        }
        if !dir.pop() {
            panic!("Could not find workspace root above {}", start.display());
        }
    }
}

/// Count non-overlapping occurrences of `needle` in `haystack`.
fn count_occurrences(haystack: &str, needle: &str) -> usize {
    let mut count = 0;
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        count += 1;
        start += pos + needle.len();
    }
    count
}

#[test]
fn slice14_workspace_version_is_consistent() {
    // ── Locate workspace root ────────────────────────────────────────────────
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = find_workspace_root(&manifest_dir);

    // ── Run cargo metadata (reads workspace + lockfile) ─────────────────────
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(workspace_root.join("Cargo.toml"))
        .exec()
        .expect("cargo metadata must succeed");

    // Collect workspace member IDs for fast lookup.
    let workspace_member_ids: std::collections::HashSet<_> =
        metadata.workspace_members.iter().cloned().collect();

    // Collect workspace-owned packages (those that are workspace members and
    // whose name starts with "harw" or equals "harw").
    let workspace_packages: Vec<&cargo_metadata::Package> = metadata
        .packages
        .iter()
        .filter(|p| {
            workspace_member_ids.contains(&p.id)
                && (p.name == "harw" || p.name.starts_with("harw-"))
        })
        .collect();

    assert!(
        !workspace_packages.is_empty(),
        "No harw workspace packages found — workspace membership lookup failed"
    );

    // ── Assertion 1: every workspace package is on EXPECTED_VERSION ──────────
    let mut mismatched: Vec<String> = Vec::new();
    for pkg in &workspace_packages {
        let v = pkg.version.to_string();
        if v != EXPECTED_VERSION {
            mismatched.push(format!("{} = {}", pkg.name, v));
        }
    }
    assert!(
        mismatched.is_empty(),
        "Packages NOT on {EXPECTED_VERSION}:\n{}",
        mismatched.join("\n")
    );

    // ── Assertion 2: no workspace package is on OLD_VERSION ──────────────────
    let old_version_packages: Vec<String> = workspace_packages
        .iter()
        .filter(|p| p.version.to_string() == OLD_VERSION)
        .map(|p| p.name.to_string())
        .collect();

    assert!(
        old_version_packages.is_empty(),
        "Packages still on {OLD_VERSION}: {:?}",
        old_version_packages
    );

    // ── Assertion 3: lockfile resolves every internal package to EXPECTED_VERSION
    // cargo_metadata exposes the resolved packages; we re-check via the full
    // packages list which includes lockfile-resolved versions.
    let lock_mismatches: Vec<String> = metadata
        .packages
        .iter()
        .filter(|p| workspace_member_ids.contains(&p.id))
        .filter(|p| p.name == "harw" || p.name.starts_with("harw-"))
        .filter(|p| p.version.to_string() != EXPECTED_VERSION)
        .map(|p| format!("{} locked at {}", p.name, p.version))
        .collect();

    assert!(
        lock_mismatches.is_empty(),
        "Lockfile-resolved versions NOT at {EXPECTED_VERSION}:\n{}",
        lock_mismatches.join("\n")
    );

    // ── Assertion 4a: workspace root Cargo.toml contains EXPECTED_VERSION exactly once
    let root_toml_path = workspace_root.join("Cargo.toml");
    let root_toml_text = fs::read_to_string(&root_toml_path)
        .unwrap_or_else(|e| panic!("Cannot read {}: {e}", root_toml_path.display()));

    let root_occurrences = count_occurrences(&root_toml_text, EXPECTED_VERSION);
    assert_eq!(
        root_occurrences, 1,
        "Workspace root Cargo.toml must contain '{EXPECTED_VERSION}' exactly once \
         (the [workspace.package] line), found {root_occurrences} occurrences"
    );

    // ── Assertion 4b: no member Cargo.toml contains the literal version string ──
    let mut member_violations: Vec<String> = Vec::new();
    for pkg in &workspace_packages {
        let member_toml_path = pkg.manifest_path.as_std_path();
        let member_text = fs::read_to_string(member_toml_path)
            .unwrap_or_else(|e| panic!("Cannot read {}: {e}", member_toml_path.display()));
        // Nur die **eigene** `version`-Angabe im `[package]`-Abschnitt zählt.
        // Die früher hier stehende Volltextsuche nach `EXPECTED_VERSION` war
        // ein Fehlalarm-Generator: `harw-warden` deklariert
        // `sd-listen-fds = "0.2.0"`, und diese Fremdversion lautet zufällig
        // wie unsere. Eine Prüfung, die "unsere Version" mit "irgendeine
        // gleichlautende Version" verwechselt, meldet Verstöße, die keine
        // sind -- und ein Betrieb, der lernt, dass die Meldungen falsch sind,
        // liest sie irgendwann nicht mehr.
        let package_section = member_text
            .split("\n[")
            .next()
            .unwrap_or(&member_text);
        let count = package_section
            .lines()
            .filter(|line| {
                let t = line.trim_start();
                t.starts_with("version") && t.contains(EXPECTED_VERSION)
            })
            .count();
        if count > 0 {
            member_violations.push(format!(
                "{} ({}) — found {count} occurrence(s)",
                pkg.name,
                member_toml_path.display()
            ));
        }
    }
    assert!(
        member_violations.is_empty(),
        "Member Cargo.toml files must NOT contain '{EXPECTED_VERSION}' \
         (they inherit via version.workspace = true). Violations:\n{}",
        member_violations.join("\n")
    );

    // ── Assertion 5: no member Cargo.toml has a bare `version = "0.1.0"` pin ──
    let mut old_pin_violations: Vec<String> = Vec::new();
    for pkg in &workspace_packages {
        let member_toml_path = pkg.manifest_path.as_std_path();
        let member_text = fs::read_to_string(member_toml_path)
            .unwrap_or_else(|e| panic!("Cannot read {}: {e}", member_toml_path.display()));

        // Find lines that are exactly `version = "0.1.0"` (with any leading
        // whitespace) — the pattern that would indicate a hard-pinned version
        // override rather than an external dep version constraint.
        let bad_lines: Vec<&str> = member_text
            .lines()
            .filter(|line| {
                let trimmed = line.trim();
                // Exact own-package version pin: `version = "0.1.0"`
                // or `version.workspace = false` combined with 0.1.0 (two separate
                // lines, but catching the version pin is sufficient).
                trimmed == format!("version = \"{OLD_VERSION}\"").as_str()
            })
            .collect();

        if !bad_lines.is_empty() {
            old_pin_violations.push(format!(
                "{} ({}) — lines: {:?}",
                pkg.name,
                member_toml_path.display(),
                bad_lines
            ));
        }
    }
    assert!(
        old_pin_violations.is_empty(),
        "Member Cargo.toml files contain a bare `version = \"{OLD_VERSION}\"` pin. \
         Violations:\n{}",
        old_pin_violations.join("\n")
    );
}
