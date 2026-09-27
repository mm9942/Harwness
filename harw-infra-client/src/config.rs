//! Configuration → available infrastructure clients (masterplan §11.1, §39).
//!
//! Harwness config only references the daemons' sockets; each daemon owns its
//! own configuration. Nothing configured means nothing available: the
//! [`Default`] config is empty, and the standard system paths are opt-in via
//! [`InfraClientConfig::system_defaults`]. There is no fallback to a working
//! or home directory, and relative socket paths are rejected.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::auth::{AuthHubClient, DEFAULT_AUTH_SOCKET};
use crate::error::InfraConfigError;
use crate::network::{DEFAULT_NETWORK_SOCKET, NetworkControlClient};
use crate::security::{DEFAULT_SECURITY_SOCKET, SecurityHubClient};
use crate::token::BearerToken;
use crate::transport::ClientOptions;

/// Connection references for the infrastructure daemons.
///
/// ```toml
/// [infrastructure]
/// auth_socket = "/run/harw/infra/secure.sock"
/// network_socket = "/run/harw/infra/network.sock"
/// security_socket = "/run/harw/infra/security.sock"
/// token_file = "/etc/harw/infra/auth.token"
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InfraClientConfig {
    /// AuthHub socket (`secure.sock`).
    pub auth_socket: Option<PathBuf>,
    /// NetSec control socket (`network.sock`).
    pub network_socket: Option<PathBuf>,
    /// SecurityHub socket (`security.sock`).
    pub security_socket: Option<PathBuf>,
    /// File holding the AuthHub bearer token (no "other" permission bits).
    /// Only valid together with `auth_socket`.
    pub token_file: Option<PathBuf>,
}

impl InfraClientConfig {
    /// All three sockets at their standard system paths, no token.
    pub fn system_defaults() -> Self {
        Self {
            auth_socket: Some(PathBuf::from(DEFAULT_AUTH_SOCKET)),
            network_socket: Some(PathBuf::from(DEFAULT_NETWORK_SOCKET)),
            security_socket: Some(PathBuf::from(DEFAULT_SECURITY_SOCKET)),
            token_file: None,
        }
    }
}

/// Which infrastructure clients this runtime has.
///
/// Built at the composition root and handed to `RuntimeServices`;
/// operations never dial sockets themselves (masterplan §11.2, H4 exit).
#[derive(Clone, Debug, Default)]
pub struct InfrastructureAvailability {
    /// AuthHub client, if configured.
    pub auth: Option<AuthHubClient>,
    /// NetSec control client, if configured.
    pub network: Option<NetworkControlClient>,
    /// SecurityHub client, if configured.
    pub security: Option<SecurityHubClient>,
}

impl InfrastructureAvailability {
    /// Build clients for every configured socket with default
    /// [`ClientOptions`]. Reads the token file, if any; does not connect.
    pub fn from_config(config: &InfraClientConfig) -> Result<Self, InfraConfigError> {
        Self::from_config_with_options(config, ClientOptions::default())
    }

    /// As [`Self::from_config`], with explicit call limits for all clients.
    pub fn from_config_with_options(
        config: &InfraClientConfig,
        options: ClientOptions,
    ) -> Result<Self, InfraConfigError> {
        let auth_socket = absolute(config.auth_socket.as_deref(), "auth_socket")?;
        let network_socket = absolute(config.network_socket.as_deref(), "network_socket")?;
        let security_socket = absolute(config.security_socket.as_deref(), "security_socket")?;

        let token = match (&config.token_file, auth_socket) {
            (Some(_), None) => return Err(InfraConfigError::TokenWithoutAuthSocket),
            (Some(path), Some(_)) => Some(BearerToken::read_from_file(path)?),
            (None, _) => None,
        };

        Ok(Self {
            auth: auth_socket.map(|socket| AuthHubClient::new(socket, token, options)),
            network: network_socket.map(|socket| NetworkControlClient::new(socket, options)),
            security: security_socket.map(|socket| SecurityHubClient::new(socket, options)),
        })
    }

    /// `true` if no client is configured.
    pub fn is_empty(&self) -> bool {
        self.auth.is_none() && self.network.is_none() && self.security.is_none()
    }
}

fn absolute<'a>(
    path: Option<&'a Path>,
    field: &'static str,
) -> Result<Option<&'a Path>, InfraConfigError> {
    match path {
        Some(path) if !path.is_absolute() => Err(InfraConfigError::RelativeSocketPath { field }),
        other => Ok(other),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_config_denies_unknown_fields_and_defaults_missing_ones() -> TestResult {
        let parsed: InfraClientConfig =
            serde_json::from_str(r#"{"auth_socket": "/run/harw/infra/secure.sock"}"#)
                .map_err(ctx("parse"))?;
        assert_eq!(parsed.auth_socket, Some(PathBuf::from(DEFAULT_AUTH_SOCKET)));
        assert_eq!(parsed.network_socket, None);

        let unknown = serde_json::from_str::<InfraClientConfig>(r#"{"auth_sock": "/x"}"#);
        assert!(unknown.is_err());
        Ok(())
    }

    #[test]
    fn test_empty_config_yields_no_clients() -> TestResult {
        let availability = InfrastructureAvailability::from_config(&InfraClientConfig::default())?;
        assert!(availability.is_empty());
        Ok(())
    }

    #[test]
    fn test_system_defaults_build_all_three_clients() -> TestResult {
        let availability =
            InfrastructureAvailability::from_config(&InfraClientConfig::system_defaults())?;
        let auth = availability.auth.ok_or(TestError::Missing("auth"))?;
        assert_eq!(auth.socket_path(), Path::new(DEFAULT_AUTH_SOCKET));
        let network = availability.network.ok_or(TestError::Missing("network"))?;
        assert_eq!(network.socket_path(), Path::new(DEFAULT_NETWORK_SOCKET));
        let security = availability
            .security
            .ok_or(TestError::Missing("security"))?;
        assert_eq!(security.socket_path(), Path::new(DEFAULT_SECURITY_SOCKET));
        Ok(())
    }

    #[test]
    fn test_relative_socket_path_is_rejected() {
        let config = InfraClientConfig {
            network_socket: Some(PathBuf::from("run/network.sock")),
            ..InfraClientConfig::default()
        };
        assert!(matches!(
            InfrastructureAvailability::from_config(&config),
            Err(InfraConfigError::RelativeSocketPath {
                field: "network_socket"
            })
        ));
    }

    #[test]
    fn test_token_file_requires_auth_socket_and_is_redacted() -> TestResult {
        let dir = tempfile::tempdir()?;
        let token_path = dir.path().join("auth.token");
        fs::write(&token_path, b"config-secret-token\n")?;
        fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600))?;

        let without_socket = InfraClientConfig {
            token_file: Some(token_path.clone()),
            ..InfraClientConfig::default()
        };
        assert!(matches!(
            InfrastructureAvailability::from_config(&without_socket),
            Err(InfraConfigError::TokenWithoutAuthSocket)
        ));

        let config = InfraClientConfig {
            auth_socket: Some(dir.path().join("secure.sock")),
            token_file: Some(token_path),
            ..InfraClientConfig::default()
        };
        let availability = InfrastructureAvailability::from_config(&config)?;
        let debug = format!("{availability:?}");
        assert!(!debug.contains("config-secret-token"), "{debug}");
        assert!(debug.contains("REDACTED"), "{debug}");
        assert!(availability.network.is_none());
        Ok(())
    }
}
