//! `--capabilities`: the runner's self-description
//! (`harw-agent-compiler/src/backend/runner.rs::RunnerCapabilities`).
//!
//! # Description
//! One JSON object on stdout, schema
//! [`harwness.agent-runner.capabilities/v1`](CAPABILITIES_SCHEMA), consumed
//! by `harw-agent-compiler` before it ships or links a runner: it decodes
//! this JSON with `RunnerCapabilities::parse` and refuses a runner whose
//! `artifact_formats`, `ir_schema`, `interfaces` or `features` do not cover
//! what a compiled agent needs. The field set here is exactly that struct's
//! shape; a field added on one side without the other breaks the contract
//! silently, so [`crate::capabilities::tests::test_matches_the_compiler_fixture`]
//! round-trips this module's JSON through the compiler's own parser.

use serde::Serialize;

use harw_agent_artifact::FORMAT_VERSION;
use harw_agent_dsl::AGENT_IR_SCHEMA;

/// Schema label of the capabilities answer (mirrors
/// `harw_agent_compiler::backend::runner::CAPABILITIES_SCHEMA`).
pub const CAPABILITIES_SCHEMA: &str = "harwness.agent-runner.capabilities/v1";

/// The child-process protocol this runner speaks (mirrors
/// `harw_agent_compiler::backend::runner::CHILD_PROTOCOL`).
pub const CHILD_PROTOCOL: &str = "harwness.agent-child/v1";

/// This crate's version (`CARGO_PKG_VERSION`), reported as `runner_version`.
pub const RUNNER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The interfaces this build was compiled with, in [`harw_agent_dsl::ir_v2::Interface::ALL`] order.
#[must_use]
pub fn compiled_interfaces() -> Vec<&'static str> {
    let mut interfaces = Vec::new();
    if cfg!(feature = "cli") {
        interfaces.push("cli");
    }
    if cfg!(feature = "repl") {
        interfaces.push("repl");
    }
    if cfg!(feature = "mcp") {
        interfaces.push("mcp");
    }
    if cfg!(feature = "http") {
        interfaces.push("http");
    }
    if cfg!(feature = "tui") {
        interfaces.push("tui");
    }
    interfaces
}

/// The provider/core cargo features this build was compiled with, sorted
/// (mirrors `harw_registry_defaults::capability_catalog::PROVIDER_FEATURES`).
#[must_use]
pub fn compiled_features() -> Vec<&'static str> {
    harw_registry_defaults::capability_catalog::PROVIDER_FEATURES
        .iter()
        .copied()
        .filter(|feature| enabled(feature))
        .collect()
}

/// `cfg!(feature = ..)` has no runtime form, so this matches each of the
/// (fixed, non-secret) provider feature names by hand.
fn enabled(feature: &str) -> bool {
    match feature {
        "authoring" => cfg!(feature = "authoring"),
        "core" => cfg!(feature = "core"),
        "knowledge" => cfg!(feature = "knowledge"),
        "matrix" => cfg!(feature = "matrix"),
        "tool-browser" => cfg!(feature = "tool-browser"),
        "tool-deps" => cfg!(feature = "tool-deps"),
        "tool-doc" => cfg!(feature = "tool-doc"),
        "tool-explorer" => cfg!(feature = "tool-explorer"),
        "tool-fs" => cfg!(feature = "tool-fs"),
        "tool-job" => cfg!(feature = "tool-job"),
        "tool-latex" => cfg!(feature = "tool-latex"),
        "tool-lens" => cfg!(feature = "tool-lens"),
        "tool-plan" => cfg!(feature = "tool-plan"),
        "tool-process" => cfg!(feature = "tool-process"),
        "tool-shell" => cfg!(feature = "tool-shell"),
        "tool-sudo" => cfg!(feature = "tool-sudo"),
        "tool-web" => cfg!(feature = "tool-web"),
        // A provider feature named after `PROVIDER_FEATURES` this crate
        // does not yet know: report it as absent rather than fail closed
        // (the `test_every_provider_feature_is_declared` test below keeps
        // the two lists in lockstep so this arm never actually triggers).
        _ => false,
    }
}

/// The target triple of the running host, in rustc's spelling.
///
/// Same construction as `harw_agent_compiler::env::host_target` (Linux is
/// assumed to be `gnu`); no build script sets `TARGET` here, so this reads
/// the host at run time from `std::env::consts` instead.
#[must_use]
pub fn host_target() -> String {
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "linux" => format!("{arch}-unknown-linux-gnu"),
        "macos" => format!("{arch}-apple-darwin"),
        "windows" => format!("{arch}-pc-windows-msvc"),
        other => format!("{arch}-unknown-{other}"),
    }
}

/// The `--capabilities` payload, field-for-field the shape
/// `RunnerCapabilities` (`harw-agent-compiler`) parses.
#[derive(Debug, Clone, Serialize)]
struct Capabilities {
    schema: &'static str,
    runner_version: &'static str,
    target: String,
    artifact_formats: Vec<u16>,
    ir_schema: &'static str,
    interfaces: Vec<&'static str>,
    features: Vec<&'static str>,
    child_protocol: Option<&'static str>,
}

/// Builds the answer for the running build.
#[must_use]
fn capabilities() -> Capabilities {
    Capabilities {
        schema: CAPABILITIES_SCHEMA,
        runner_version: RUNNER_VERSION,
        target: host_target(),
        artifact_formats: vec![FORMAT_VERSION],
        ir_schema: AGENT_IR_SCHEMA,
        interfaces: compiled_interfaces(),
        features: compiled_features(),
        child_protocol: Some(CHILD_PROTOCOL),
    }
}

/// Renders `--capabilities`' answer as a single JSON line.
///
/// # Errors
/// Never in practice (every field is a plain string or number); the
/// `Result` only carries `serde_json`'s fallible signature through.
pub fn render() -> Result<String, serde_json::Error> {
    serde_json::to_string(&capabilities())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn test_renders_valid_json_with_the_expected_schema() -> TestResult {
        let text = render()?;
        let value: serde_json::Value = serde_json::from_str(&text)?;
        assert_eq!(value["schema"], CAPABILITIES_SCHEMA);
        assert_eq!(value["child_protocol"], CHILD_PROTOCOL);
        assert_eq!(value["ir_schema"], AGENT_IR_SCHEMA);
        assert_eq!(value["artifact_formats"][0], FORMAT_VERSION);
        Ok(())
    }

    #[test]
    fn test_matches_the_compiler_fixture() -> TestResult {
        let text = render()?;
        let parsed = harw_agent_compiler::backend::runner::RunnerCapabilities::parse(&text)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        assert_eq!(
            parsed.schema,
            harw_agent_compiler::backend::runner::CAPABILITIES_SCHEMA
        );
        assert_eq!(
            parsed.child_protocol.as_deref(),
            Some(harw_agent_compiler::backend::runner::CHILD_PROTOCOL)
        );
        assert!(parsed.artifact_formats.contains(&FORMAT_VERSION));
        assert_eq!(parsed.ir_schema, AGENT_IR_SCHEMA);
        Ok(())
    }

    #[test]
    fn test_every_provider_feature_is_declared() {
        for feature in harw_registry_defaults::capability_catalog::PROVIDER_FEATURES {
            // `enabled` must recognize the name even when the crate feature
            // is off; a name that falls into the `_` arm would silently
            // never be reported even when compiled in.
            let _ = enabled(feature);
        }
        let unknown: Vec<&str> = harw_registry_defaults::capability_catalog::PROVIDER_FEATURES
            .iter()
            .filter(|feature| {
                !matches!(
                    **feature,
                    "authoring"
                        | "core"
                        | "knowledge"
                        | "matrix"
                        | "tool-browser"
                        | "tool-deps"
                        | "tool-doc"
                        | "tool-explorer"
                        | "tool-fs"
                        | "tool-job"
                        | "tool-latex"
                        | "tool-lens"
                        | "tool-plan"
                        | "tool-process"
                        | "tool-shell"
                        | "tool-sudo"
                        | "tool-web"
                )
            })
            .copied()
            .collect();
        assert!(
            unknown.is_empty(),
            "capabilities.rs is missing: {unknown:?}"
        );
    }
}
