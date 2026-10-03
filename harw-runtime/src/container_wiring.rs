//! Builds the `container.*` tool provider from `[tools.container]`.
//!
//! The section is global-only (`harw-config` rejects a project layer that
//! tries to set it), so the engine path and the image catalog are trusted
//! input here. The engine process gets a cleared environment built from an
//! allowlist (`harw_tool_container::engine_environment`): variables that
//! redirect an engine to another socket or credential file never reach it.

use std::sync::Arc;

use harw_config::ContainerToolsSection;
use harw_tool_container::engine_environment;
use harw_tool_container_run::{ContainerToolConfig, ContainerToolProvider, PodmanEngine};

/// Provider over the configured engine and images.
///
/// # Errors
/// A readable message when the engine path, the connection name or an image
/// entry is invalid (the tools are then withheld, never half-configured).
pub fn provider_from_config(
    section: &ContainerToolsSection,
) -> Result<ContainerToolProvider, String> {
    let env = engine_environment(&|key| std::env::var(key).ok(), section.connection.is_some());
    let engine = Arc::new(PodmanEngine::new(env));
    let config = ContainerToolConfig::from_entries(
        &section.engine,
        section.connection.as_deref(),
        &section.images,
        engine,
    )?;
    Ok(ContainerToolProvider::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn a_valid_section_builds_a_provider() {
        let section = ContainerToolsSection {
            enabled: true,
            images: vec![format!("rust=docker.io/library/rust@sha256:{HEX}")],
            ..ContainerToolsSection::default()
        };
        assert!(provider_from_config(&section).is_ok());
    }

    #[test]
    fn an_invalid_section_withholds_the_tools() {
        let tagged = ContainerToolsSection {
            enabled: true,
            images: vec!["rust=docker.io/library/rust:latest".to_owned()],
            ..ContainerToolsSection::default()
        };
        assert!(provider_from_config(&tagged).is_err(), "tags are refused");
        let wrong_engine = ContainerToolsSection {
            enabled: true,
            engine: "/usr/bin/docker".to_owned(),
            ..ContainerToolsSection::default()
        };
        assert!(provider_from_config(&wrong_engine).is_err());
    }
}
