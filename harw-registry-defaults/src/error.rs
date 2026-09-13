//! Typed errors for default coding-agent registry assembly.
//!
//! This crate deliberately keeps the project-discovery failure typed until a
//! binary presentation boundary chooses how to render it. No `anyhow` or
//! `thiserror` is used here.

use std::fmt;
use harw_extension_api::registry::ContextProviderRegistrationError;

use harw_agent_dsl::error::DslError;
use harw_project_discovery::DiscoveryError;

/// Result type returned by default-registry construction.
pub type RegistryDefaultsResult<T> = Result<T, RegistryDefaultsError>;

/// Failure modes for default coding-agent registry assembly.
#[derive(Debug)]
pub enum RegistryDefaultsError {
    /// The supplied working directory could not be discovered as a project.
    ProjectDiscovery { source: DiscoveryError },
    /// Constructing the optional `browser`-feature `FirefoxHost` failed.
    /// This is a pure configuration failure (e.g. an invalid explicit
    /// Firefox binary path); it never indicates a missing running
    /// WebDriver, because host construction starts no process.
    BrowserHost(String),
    /// Eine eingebaute Agentendefinition liess sich nicht parsen, auflösen oder
    /// zu einer `ExecutableAgentIr` senken.
    ///
    /// Der Fehler benennt die betroffene Definition (`name`, z. B. `"explorer"`)
    /// und trägt den typisierten DSL-Fehler als Ursache — er ist immer ein
    /// Defekt der eingebetteten TOML-Datei, nie eine Nutzereingabe.
    AgentDefinition {
        /// Name der eingebauten Definition, z. B. `"explorer"`.
        name: String,
        /// Ursächlicher Fehler aus der Agent-Definition-DSL.
        source: DslError,
    },
    /// Ein Kontextanbieter liess sich nicht registrieren.
    ///
    /// Seit der `ContextProvider`-Trait `namespace()` und `max_trust()` trägt,
    /// prüft **jeder** Registrierungsweg beide Angaben — vorher gab es einen
    /// ungeprüften daneben. Ein Verstoss ist immer ein Defekt der eingebauten
    /// Zusammenstellung (ein doppelt beanspruchter Namensraum, ein leerer),
    /// nie eine Nutzereingabe.
    ContextProviderRegistration {
        /// Ursächlicher Fehler aus der Erweiterungs-Registry.
        source: ContextProviderRegistrationError,
    },
}

impl fmt::Display for RegistryDefaultsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProjectDiscovery { source } => {
                write!(f, "default registry project discovery failed: {source}")
            }
            Self::BrowserHost(reason) => {
                write!(f, "default registry browser host construction failed: {reason}")
            }
            Self::AgentDefinition { name, source } => {
                write!(
                    f,
                    "eingebaute Agentendefinition '{name}' konnte nicht aufgelöst werden: {source}"
                )
            }
            Self::ContextProviderRegistration { source } => {
                write!(f, "Kontextanbieter konnte nicht registriert werden: {source}")
            }
        }
    }
}

impl std::error::Error for RegistryDefaultsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ProjectDiscovery { source } => Some(source),
            Self::AgentDefinition { source, .. } => Some(source),
            Self::ContextProviderRegistration { source } => Some(source),
            Self::BrowserHost(_) => None,
        }
    }
}

impl From<DiscoveryError> for RegistryDefaultsError {
    fn from(source: DiscoveryError) -> Self {
        Self::ProjectDiscovery { source }
    }
}

impl From<ContextProviderRegistrationError> for RegistryDefaultsError {
    fn from(source: ContextProviderRegistrationError) -> Self {
        Self::ContextProviderRegistration { source }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use harw_project_discovery::DiscoveryError;

    use super::RegistryDefaultsError;

    #[test]
    fn project_discovery_error_retains_typed_source() {
        let error = RegistryDefaultsError::from(DiscoveryError::InvalidCwd(PathBuf::from(
            "missing-project",
        )));

        assert!(matches!(
            &error,
            RegistryDefaultsError::ProjectDiscovery {
                source: DiscoveryError::InvalidCwd(_)
            }
        ));
        assert!(std::error::Error::source(&error).is_some());
    }
}
