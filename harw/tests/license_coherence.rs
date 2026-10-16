//! Workspace license coherence integration test.
//!
//! Harwness is dual-licensed `MIT OR Apache-2.0`. `deny.toml` checks the
//! licenses of *dependencies* only (`[licenses.private] ignore = true` skips
//! our own private crates), so nothing else notices when a member crate
//! drifts away from the workspace license. This test closes that gap.
//!
//! Asserts, by **static** TOML/text parsing (no `cargo` subprocess, same
//! approach as `version_coherence.rs`):
//!   1. `[workspace.package] license` is exactly `MIT OR Apache-2.0`.
//!   2. Every workspace member inherits it (`license.workspace = true`) and
//!      declares neither its own `license` nor a `license-file`.
//!   3. `LICENSE-MIT` and `LICENSE-APACHE` exist at the workspace root and
//!      carry the expected license titles.
//!   4. `README.md` links both license files.
//!   5. Packaging metadata that restates the license agrees with it:
//!      `packaging/harw.rb`, `webui/package.json` and the OCI label (and the
//!      license texts) in `packaging/podman/Containerfile`.
//!   6. `deny.toml` still allows both licenses of our own crates.
//!
//! # Limits — deliberately not closed
//! - `[workspace.members]` is read as a list of **literal** directory names,
//!   not globs (the root manifest uses literals only; see
//!   `version_coherence.rs`).
//! - The generated native crate template
//!   (`harw-agent-compiler/tests/golden/native/Cargo.toml.golden`) has no
//!   `license` by design (`publish = false`); it is not a workspace member and
//!   is not checked here.

mod common;

use common::{TestError, TestResult};
use std::fs;
use std::path::{Path, PathBuf};

const EXPECTED_LICENSE: &str = "MIT OR Apache-2.0";

fn read_text(path: &Path) -> TestResult<String> {
    fs::read_to_string(path).map_err(|e| TestError::Context {
        context: "Cannot read file",
        source: format!("{}: {e}", path.display()),
    })
}

fn read_toml(path: &Path) -> TestResult<toml::Value> {
    let table = read_text(path)?
        .parse::<toml::Table>()
        .map_err(|e| TestError::Context {
            context: "Cannot parse TOML file",
            source: format!("{}: {e}", path.display()),
        })?;
    Ok(toml::Value::Table(table))
}

/// Walk up from the crate directory to the directory whose `Cargo.toml`
/// declares `[workspace]`.
fn find_workspace_root(start: &Path) -> TestResult<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join("Cargo.toml");
        if candidate.exists() && read_text(&candidate)?.contains("[workspace]") {
            return Ok(dir);
        }
        if !dir.pop() {
            return Err(TestError::Unexpected(format!(
                "Could not find workspace root above {}",
                start.display()
            )));
        }
    }
}

fn workspace_root() -> TestResult<PathBuf> {
    find_workspace_root(&PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

#[test]
fn workspace_license_is_dual_mit_apache() -> TestResult {
    let root = workspace_root()?;
    let root_value = read_toml(&root.join("Cargo.toml"))?;
    let license = root_value
        .get("workspace")
        .and_then(|w| w.get("package"))
        .and_then(|p| p.get("license"))
        .and_then(toml::Value::as_str)
        .ok_or(TestError::Missing(
            "[workspace.package.license] fehlt oder ist kein String",
        ))?;
    assert_eq!(
        license, EXPECTED_LICENSE,
        "[workspace.package] license weicht von {EXPECTED_LICENSE} ab"
    );
    Ok(())
}

#[test]
fn every_member_inherits_the_workspace_license() -> TestResult {
    let root = workspace_root()?;
    let root_value = read_toml(&root.join("Cargo.toml"))?;
    let members = root_value
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(toml::Value::as_array)
        .ok_or(TestError::Missing(
            "[workspace] members muss ein Array sein",
        ))?;
    assert!(
        !members.is_empty(),
        "[workspace] members ist leer — Lookup fehlgeschlagen"
    );

    let mut violations: Vec<String> = Vec::new();
    for member in members {
        let dir = member.as_str().ok_or_else(|| {
            TestError::Unexpected(format!(
                "[workspace.members]-Eintrag ist kein String: {member:?}"
            ))
        })?;
        let manifest = root.join(dir).join("Cargo.toml");
        let value = read_toml(&manifest)?;
        let package = value.get("package").ok_or_else(|| {
            TestError::Unexpected(format!(
                "{} hat keinen [package]-Abschnitt",
                manifest.display()
            ))
        })?;

        let inherits = match package.get("license") {
            Some(toml::Value::Table(t)) => {
                t.get("workspace").and_then(toml::Value::as_bool) == Some(true)
            }
            _ => false,
        };
        if !inherits {
            violations.push(format!(
                "{dir}: `license.workspace = true` fehlt (eigene oder keine Lizenzangabe)"
            ));
        }
        if package.get("license-file").is_some() {
            violations.push(format!("{dir}: `license-file` ist gesetzt"));
        }
    }

    assert!(
        violations.is_empty(),
        "Mitglieder ohne geerbte Workspace-Lizenz ({EXPECTED_LICENSE}):\n{}",
        violations.join("\n")
    );
    Ok(())
}

#[test]
fn license_texts_exist_and_readme_links_them() -> TestResult {
    let root = workspace_root()?;

    let mit = read_text(&root.join("LICENSE-MIT"))?;
    assert!(
        mit.trim_start().starts_with("MIT License"),
        "LICENSE-MIT beginnt nicht mit dem MIT-Lizenztitel"
    );
    let apache = read_text(&root.join("LICENSE-APACHE"))?;
    assert!(
        apache.contains("Apache License") && apache.contains("Version 2.0"),
        "LICENSE-APACHE enthält nicht den Apache-2.0-Lizenztext"
    );

    let readme = read_text(&root.join("README.md"))?;
    assert!(
        readme.contains("(LICENSE-MIT)") && readme.contains("(LICENSE-APACHE)"),
        "README.md verlinkt LICENSE-MIT und LICENSE-APACHE nicht beide"
    );
    Ok(())
}

#[test]
fn packaging_metadata_matches_the_workspace_license() -> TestResult {
    let root = workspace_root()?;

    // Homebrew: dual license is expressed as `any_of`, not as a single id.
    let formula = read_text(&root.join("packaging/harw.rb"))?;
    assert!(
        formula.contains(r#"license any_of: ["MIT", "Apache-2.0"]"#),
        "packaging/harw.rb: `license` muss `any_of: [\"MIT\", \"Apache-2.0\"]` sein"
    );

    // npm: SPDX expression, same as Cargo. Text check on purpose — `harw` has
    // no JSON dependency and the manifest is a flat, hand-written file.
    let package_json = read_text(&root.join("webui/package.json"))?;
    assert!(
        package_json.contains(&format!(r#""license": "{EXPECTED_LICENSE}""#)),
        "webui/package.json: `license` muss {EXPECTED_LICENSE} sein"
    );

    // OCI image: SPDX label plus both license texts shipped in the image.
    let containerfile = read_text(&root.join("packaging/podman/Containerfile"))?;
    assert!(
        containerfile.contains(&format!(
            r#"org.opencontainers.image.licenses="{EXPECTED_LICENSE}""#
        )),
        "Containerfile: OCI-Label org.opencontainers.image.licenses fehlt oder weicht ab"
    );
    assert!(
        containerfile.contains("COPY LICENSE-MIT LICENSE-APACHE"),
        "Containerfile: LICENSE-MIT und LICENSE-APACHE werden nicht ins Image kopiert"
    );
    Ok(())
}

#[test]
fn deny_toml_allows_the_workspace_licenses() -> TestResult {
    let root = workspace_root()?;
    let deny = read_toml(&root.join("deny.toml"))?;
    let allow: Vec<&str> = deny
        .get("licenses")
        .and_then(|l| l.get("allow"))
        .and_then(toml::Value::as_array)
        .ok_or(TestError::Missing("deny.toml [licenses] allow fehlt"))?
        .iter()
        .filter_map(toml::Value::as_str)
        .collect();
    for needed in ["MIT", "Apache-2.0"] {
        assert!(
            allow.contains(&needed),
            "deny.toml [licenses] allow enthält {needed} nicht"
        );
    }
    Ok(())
}
