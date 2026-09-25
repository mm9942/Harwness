//! `--verify`, `--manifest` and `--version`: the runner's read-only,
//! no-session commands.
//!
//! # Description
//! All three commands only need the embedded artifact's bundle, never a
//! built [`harw_runtime::EmbeddedAgent`] or a session — they answer before
//! [`crate::run_embedded`]/[`crate::run_from_current_exe`] narrows rights or
//! opens a [`harwness_sdk::Harwness`]. Loading here is the same fail-closed
//! path the normal run takes: [`load_from_current_exe`]/[`load_from_bytes`]
//! verify the artifact's footer, hashes and bundle layout before anything
//! else is printed, so a tampered binary answers `--verify`/`--manifest`
//! with an error, never with a manifest read from unverified bytes.

use std::process::ExitCode;

use harw_agent_artifact::{Artifact, Bundle, EmbeddedArtifact};
use harw_agent_dsl::ir_v2::AgentIr;

use crate::error::RunnerError;

/// Loads and verifies the artifact embedded (by [`harw_agent_artifact::embed`])
/// in the currently running executable.
///
/// # Errors
/// [`RunnerError::Artifact`] without a footer or on a hash mismatch,
/// [`RunnerError::Bundle`] if the payloads are not a valid bundle.
pub fn load_from_current_exe() -> Result<(Artifact, Bundle), RunnerError> {
    let embedded = EmbeddedArtifact::from_current_exe()?;
    let artifact = embedded.into_artifact();
    let bundle = Bundle::from_artifact(&artifact)?;
    Ok((artifact, bundle))
}

/// Loads and verifies an artifact already extracted from its own bytes (a
/// native build's `include_bytes!`, or a footer-embedded binary's bytes a
/// caller already read); `crate::run_embedded` takes this path instead of
/// [`load_from_current_exe`].
///
/// # Errors
/// As [`load_from_current_exe`].
pub fn load_from_bytes(bytes: &[u8]) -> Result<(Artifact, Bundle), RunnerError> {
    let artifact = Artifact::from_bytes(bytes)?;
    let bundle = Bundle::from_artifact(&artifact)?;
    Ok((artifact, bundle))
}

/// The root agent's typed IR, decoded from the bundle header (the same
/// value as `bundle.root().map(|entry| &entry.ir)`, without the `Option`:
/// [`Bundle::from_artifact`] already checked the root is indexed).
///
/// # Errors
/// [`RunnerError::Json`] if the header's `ir` value does not decode as
/// [`AgentIr`] (only possible if the bundle was built against an older or
/// newer IR schema than this runner reads).
pub fn root_ir(bundle: &Bundle) -> Result<AgentIr, RunnerError> {
    Ok(serde_json::from_value(bundle.header.ir.clone())?)
}

/// Runs `--verify`: loads and verifies via `load`, prints the digest (or,
/// with `json`, a `{"ok":.., "digest":..}` line), and maps success/failure
/// straight to exit `0`/`1` — the sysexits-style codes of
/// [`RunnerError::exit_code`] are for a run that was *going* to start an
/// agent, not for this pass/fail check.
pub fn run_verify(
    load: impl FnOnce() -> Result<(Artifact, Bundle), RunnerError>,
    json: bool,
) -> ExitCode {
    match load() {
        Ok((artifact, _bundle)) => {
            let digest = artifact.digest();
            if json {
                println!(r#"{{"ok":true,"digest":"{digest}"}}"#);
            } else {
                println!("ok: {digest}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            if json {
                let message = error.to_string().replace('"', "'");
                println!(r#"{{"ok":false,"error":"{message}"}}"#);
            } else {
                eprintln!("verify failed: {error}");
            }
            ExitCode::from(1)
        }
    }
}

/// Runs `--manifest`: prints the root agent's rights manifest
/// ([`AgentIr::permissions`]), as pretty JSON with `json` or as a short
/// human-readable listing otherwise.
///
/// # Errors
/// Whatever `load` and [`root_ir`] return; a decode failure here is a
/// runner/compiler schema mismatch, not a tampered artifact (the hash
/// already checked out by the time `load` returns `Ok`).
pub fn run_manifest(
    load: impl FnOnce() -> Result<(Artifact, Bundle), RunnerError>,
    json: bool,
) -> Result<ExitCode, RunnerError> {
    let (_artifact, bundle) = load()?;
    let ir = root_ir(&bundle)?;
    let permissions = &ir.permissions;
    if json {
        println!("{}", serde_json::to_string_pretty(permissions)?);
    } else {
        println!("agent: {} ({})", ir.id, ir.specialization);
        println!(
            "tools: {}",
            if permissions.tools.is_empty() {
                "(none)".to_owned()
            } else {
                permissions.tools.join(", ")
            }
        );
        if !permissions.forbidden_tools.is_empty() {
            println!("forbidden: {}", permissions.forbidden_tools.join(", "));
        }
        println!(
            "filesystem: read={} write={}",
            permissions.filesystem.read, permissions.filesystem.write
        );
        println!(
            "network: {} ({})",
            match permissions.network.mode {
                harw_agent_dsl::ir_v2::NetworkMode::Off => "off",
                harw_agent_dsl::ir_v2::NetworkMode::Allowlist => "allowlist",
            },
            if permissions.network.hosts.is_empty() {
                "no hosts".to_owned()
            } else {
                permissions.network.hosts.join(", ")
            }
        );
        println!(
            "shell: {} host: {}",
            permissions.shell, permissions.host
        );
        if let Some(budget) = &permissions.budget {
            println!(
                "budget: max_tokens={:?} max_tool_calls={:?} max_wall_secs={:?}",
                budget.max_tokens, budget.max_tool_calls, budget.max_wall_secs
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Runs `--version`: prints [`crate::capabilities::RUNNER_VERSION`].
#[must_use]
pub fn run_version() -> ExitCode {
    println!("{}", crate::capabilities::RUNNER_VERSION);
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    use harw_agent_artifact::{ArtifactBuilder, append_to_executable};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn fake_bundle_artifact() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let ir = serde_json::json!({
            "id": "demo",
            "snapshot": {"digest": "s-demo"},
        });
        let entry = serde_json::json!({
            "schema": harw_agent_artifact::bundle::ENTRY_SCHEMA,
            "id": "demo",
            "name": "demo",
            "ir": ir,
            "payload_refs": [],
            "children": [],
        });
        let entry_bytes = serde_json::to_vec(&entry)?;
        let header = serde_json::json!({
            "schema": harw_agent_artifact::bundle::BUNDLE_SCHEMA,
            "root": "demo",
            "ir": ir,
            "agents": {
                "demo": {"entry": "agents/demo.json", "snapshot": "s-demo"},
            },
        });
        let artifact = ArtifactBuilder::new(&header)
            .add_payload(
                harw_agent_artifact::PayloadKind::Other("agent".to_owned()),
                "agents/demo.json",
                entry_bytes,
            )
            .build()?;
        Ok(artifact.to_bytes())
    }

    #[test]
    fn test_load_from_bytes_round_trips_a_bundle() -> TestResult {
        let bytes = fake_bundle_artifact()?;
        let (artifact, bundle) = load_from_bytes(&bytes)?;
        assert_eq!(bundle.header.root, "demo");
        assert!(bundle.agents.contains_key("demo"));
        let _ = artifact.digest();
        Ok(())
    }

    #[test]
    fn test_load_from_bytes_rejects_tampered_bytes() -> TestResult {
        let mut bytes = fake_bundle_artifact()?;
        if let Some(last) = bytes.last_mut() {
            *last ^= 0xFF;
        }
        assert!(load_from_bytes(&bytes).is_err());
        Ok(())
    }

    #[test]
    fn test_verify_via_appended_executable() -> TestResult {
        let bytes = fake_bundle_artifact()?;
        let artifact = Artifact::from_bytes(&bytes)?;
        let binary = append_to_executable(b"fake runner bytes", &artifact);
        let embedded = EmbeddedArtifact::from_executable_bytes(&binary)?;
        let bundle = Bundle::from_artifact(embedded.artifact())?;
        assert_eq!(bundle.header.root, "demo");
        assert_eq!(embedded.artifact().digest(), artifact.digest());
        Ok(())
    }

    #[test]
    fn test_run_verify_reports_success_and_failure() -> TestResult {
        let bytes = fake_bundle_artifact()?;
        assert_eq!(
            run_verify(|| load_from_bytes(&bytes), true),
            ExitCode::SUCCESS
        );
        assert_eq!(
            run_verify(|| Err(RunnerError::Usage("no artifact".to_owned())), false),
            ExitCode::from(1)
        );
        Ok(())
    }
}
