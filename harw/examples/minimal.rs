//! Minimal runnable example demonstrating how to compose a HARW setup using
//! the `harw` facade crate alone.
//!
//! Spec source: task "Write a runnable example (harw/examples/minimal.rs)"
//!
//! Run with:
//!   cargo run -p harw --example minimal
//!
//! This example serves as living documentation of the facade API:
//! - Build an `OperationRegistry` and populate it with built-in ops.
//! - Resolve aliases manually via the public `iter()` + `meta()` API.
//! - Assemble the default `ExtensionRegistry` (fs + shell tools, baseline instructions).
//! - Parse a minimal agent TOML definition from an inline string constant.

use harw::prelude::*;

const MINIMAL_AGENT_TOML: &str = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.minimal-example@1"
version = "1.0.0"
role = "worker"
specialization = "minimal-example"
"#;

fn main() {
    // ── 1. Build and populate the OperationRegistry ───────────────────────────
    let mut registry = OperationRegistry::new();
    harw::ops::builtins::register_all(&mut registry);
    let op_count = registry.len();

    // ── 2. Resolve a few aliases to prove the API shape ───────────────────────
    let alias_model = registry
        .iter()
        .find(|op| op.meta().aliases.contains(&"m"))
        .map(|op| op.meta().name)
        .unwrap_or("(not found)");

    let alias_provider = registry
        .iter()
        .find(|op| op.meta().aliases.contains(&"p"))
        .map(|op| op.meta().name)
        .unwrap_or("(not found)");

    let alias_effort = registry
        .iter()
        .find(|op| op.meta().aliases.contains(&"reasoning"))
        .map(|op| op.meta().name)
        .unwrap_or("(not found)");

    // ── 3. Assemble the default ExtensionRegistry ─────────────────────────────
    let assembled = harw::defaults::assemble_default_registry(std::env::temp_dir())
        .expect("project discovery must succeed from temp_dir");

    let total_tools: usize = assembled
        .registry
        .tool_providers()
        .iter()
        .map(|p| p.tools().len())
        .sum();

    // ── 4. Parse a minimal agent TOML definition ──────────────────────────────
    let raw = harw::agent::parse::parse_toml(MINIMAL_AGENT_TOML)
        .expect("minimal TOML fixture must parse without error");

    // ── 5. Report ─────────────────────────────────────────────────────────────
    println!(
        "harness assembled: ops={op_count} tools={total_tools} \
         alias[m]={alias_model} alias[p]={alias_provider} alias[reasoning]={alias_effort} \
         agent_id={agent_id}",
        agent_id = raw.id,
    );
}
