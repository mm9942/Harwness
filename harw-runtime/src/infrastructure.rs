//! Infrastructure clients at the composition root (Crypto-Infrastructure
//! masterplan v2 §11, §34 H4).
//!
//! # Description
//! Converts `[infrastructure]` ([`harw_config::InfrastructureSection`]) into
//! [`harw_infra_client::InfraClientConfig`] and builds the
//! [`InfrastructureAvailability`] once per assembly. The result is handed to
//! [`crate::services::RuntimeServicesParts::infrastructure`]; operations
//! read it from their `OpContext` and never dial a socket themselves.
//!
//! `harw-config` does not depend on `harw-infra-client` (it must stay free
//! of the client stack), so the config type lives there and the conversion
//! lives here.
//!
//! # Failure policy
//! Infrastructure is optional. A missing section yields `None`; an invalid
//! one (relative socket path, `token_file` without `auth_socket`, unreadable
//! or world-accessible token file, …) is logged with `tracing::warn!` and
//! also yields `None` — never a failed assembly. Building the clients opens
//! no socket; an unreachable daemon shows up only when an operation calls
//! it (`infra.status` reports it as unavailable).
//!
//! # Secrets
//! The token file's contents are read by `harw-infra-client` into zeroizing
//! memory and never pass through this module. Warnings carry only the
//! configuration error (field name or file path), never the token.

use std::sync::Arc;

use harw_config::{InfrastructureSection, ResolvedConfig};
use harw_infra_client::{InfraClientConfig, InfrastructureAvailability};

/// Field-for-field conversion of the config section into the client config.
///
/// # Example
/// ```rust
/// use std::path::PathBuf;
/// use harw_config::InfrastructureSection;
///
/// let section = InfrastructureSection {
///     auth_socket: Some(PathBuf::from("/run/harw/infra/secure.sock")),
///     ..InfrastructureSection::default()
/// };
/// let config = harw_runtime::infrastructure::client_config(&section);
/// assert_eq!(config.auth_socket, section.auth_socket);
/// assert_eq!(config.network_socket, None);
/// ```
#[must_use]
pub fn client_config(section: &InfrastructureSection) -> InfraClientConfig {
    InfraClientConfig {
        auth_socket: section.auth_socket.clone(),
        network_socket: section.network_socket.clone(),
        security_socket: section.security_socket.clone(),
        token_file: section.token_file.clone(),
    }
}

/// Builds the infrastructure clients for `config`, if any are configured.
///
/// # Returns
/// - `None` without `[infrastructure]`, with a section that configures no
///   socket, or when the section is invalid (logged as a warning).
/// - `Some` with one client per configured socket otherwise.
///
/// # Concurrency
/// Synchronous; reads at most the token file. No socket is opened.
#[must_use]
pub fn build_infrastructure(config: &ResolvedConfig) -> Option<Arc<InfrastructureAvailability>> {
    let section = config.infrastructure.as_ref()?;
    match InfrastructureAvailability::from_config(&client_config(section)) {
        Ok(availability) if availability.is_empty() => {
            tracing::warn!("runtime.infrastructure_section_configures_no_socket");
            None
        }
        Ok(availability) => Some(Arc::new(availability)),
        Err(error) => {
            tracing::warn!(%error, "runtime.infrastructure_unavailable");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{build_infrastructure, client_config};
    use crate::test_support::{TestError, TestResult};
    use harw_config::{InfrastructureSection, ResolvedConfig};
    use std::path::{Path, PathBuf};

    fn config_with(section: Option<InfrastructureSection>) -> ResolvedConfig {
        ResolvedConfig {
            infrastructure: section,
            ..ResolvedConfig::default()
        }
    }

    #[test]
    fn absent_section_builds_nothing() {
        assert!(build_infrastructure(&config_with(None)).is_none());
    }

    #[test]
    fn empty_section_builds_nothing() {
        let config = config_with(Some(InfrastructureSection::default()));
        assert!(build_infrastructure(&config).is_none());
    }

    #[test]
    fn invalid_section_is_a_warning_not_an_error() {
        // Relative socket path and token file without auth socket: both are
        // rejected by `harw-infra-client`; the runtime just has no clients.
        let relative = config_with(Some(InfrastructureSection {
            network_socket: Some(PathBuf::from("run/network.sock")),
            ..InfrastructureSection::default()
        }));
        assert!(build_infrastructure(&relative).is_none());

        let token_only = config_with(Some(InfrastructureSection {
            token_file: Some(PathBuf::from("/nonexistent/auth.token")),
            ..InfrastructureSection::default()
        }));
        assert!(build_infrastructure(&token_only).is_none());
    }

    #[test]
    fn configured_sockets_become_clients_without_connecting() -> TestResult {
        // The socket does not exist: building must still succeed, because
        // nothing is dialled at assembly time.
        let config = config_with(Some(InfrastructureSection {
            auth_socket: Some(PathBuf::from("/nonexistent/harw-test/secure.sock")),
            security_socket: Some(PathBuf::from("/nonexistent/harw-test/security.sock")),
            ..InfrastructureSection::default()
        }));
        let availability =
            build_infrastructure(&config).ok_or(TestError::Missing("infrastructure"))?;
        let auth = availability
            .auth
            .as_ref()
            .ok_or(TestError::Missing("auth client"))?;
        assert_eq!(
            auth.socket_path(),
            Path::new("/nonexistent/harw-test/secure.sock")
        );
        assert!(availability.network.is_none());
        assert!(availability.security.is_some());
        Ok(())
    }

    #[test]
    fn client_config_copies_every_field() {
        let section = InfrastructureSection {
            auth_socket: Some(PathBuf::from("/a")),
            network_socket: Some(PathBuf::from("/n")),
            security_socket: Some(PathBuf::from("/s")),
            token_file: Some(PathBuf::from("/t")),
        };
        let config = client_config(&section);
        assert_eq!(config.auth_socket, section.auth_socket);
        assert_eq!(config.network_socket, section.network_socket);
        assert_eq!(config.security_socket, section.security_socket);
        assert_eq!(config.token_file, section.token_file);
    }
}
