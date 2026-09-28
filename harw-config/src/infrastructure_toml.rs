//! `[infrastructure]` — socket references for the Harwness infrastructure
//! daemons (Crypto-Infrastructure masterplan v2 §11, §39; consumer:
//! `harw-runtime`, which converts it into
//! `harw_infra_client::InfraClientConfig`).
//!
//! # Scope
//! Harwness config only *references* the daemons (AuthHub `secure.sock`,
//! NetSec `network.sock`, SecurityHub `security.sock`) and the AuthHub
//! bearer-token file; every daemon owns its own configuration. This module
//! is a plain data type: no path is opened, no socket is dialled, no file is
//! read here.
//!
//! # Why the type lives here and not in `harw-infra-client`
//! `harw-config` must stay free of the client stack (hyper, the CryptGuard
//! wire codec). The field set mirrors `InfraClientConfig` one-to-one; the
//! conversion lives at the composition root (`harw-runtime`).
//!
//! # Defaults and trust
//! - Absent section ⇒ `ResolvedConfig::infrastructure == None` ⇒ no
//!   infrastructure clients and no `infra.*` operations. There is no implicit
//!   fallback to the standard system socket paths.
//! - Only **trusted** layers (home, active profile) may set it; a later
//!   trusted layer that sets the table replaces it wholesale. An untrusted
//!   repository layer is ignored completely — pointing Harwness at a foreign
//!   socket or token file would widen authority, not narrow it (same rule as
//!   `[web]`, see `harw_config::discovery`).
//! - Unknown keys are rejected (`deny_unknown_fields`).
//! - Path *validity* (absolute paths, `token_file` only with `auth_socket`,
//!   token-file permissions) is checked by `harw-infra-client` when the
//!   runtime builds the clients; a failure there is a warning, never fatal.
//!
//! # Example
//! ```rust
//! use harw_config::InfrastructureSection;
//!
//! # fn main() -> Result<(), toml::de::Error> {
//! let section: InfrastructureSection = toml::from_str(
//!     r#"auth_socket = "/run/harw/infra/secure.sock""#,
//! )?;
//! assert!(section.auth_socket.is_some());
//! assert!(section.network_socket.is_none());
//! # Ok(())
//! # }
//! ```

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// `[infrastructure]` — daemon sockets and the AuthHub token file.
///
/// ```toml
/// [infrastructure]
/// auth_socket = "/run/harw/infra/secure.sock"
/// network_socket = "/run/harw/infra/network.sock"
/// security_socket = "/run/harw/infra/security.sock"
/// token_file = "/etc/harw/infra/auth.token"
/// ```
///
/// Every field is optional; a present but empty table configures nothing.
/// The token file's *contents* never appear in this type — only its path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InfrastructureSection {
    /// AuthHub socket (`secure.sock`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_socket: Option<PathBuf>,
    /// NetSec control socket (`network.sock`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_socket: Option<PathBuf>,
    /// SecurityHub socket (`security.sock`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security_socket: Option<PathBuf>,
    /// File holding the AuthHub bearer token. Only valid together with
    /// `auth_socket` (enforced by `harw-infra-client`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_file: Option<PathBuf>,
}

impl InfrastructureSection {
    /// `true` if no socket and no token file is configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.auth_socket.is_none()
            && self.network_socket.is_none()
            && self.security_socket.is_none()
            && self.token_file.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_infrastructure_section_empty_table_configures_nothing() -> TestResult {
        let section: InfrastructureSection = toml::from_str("").map_err(ctx("parse toml"))?;
        assert!(section.is_empty());
        assert_eq!(section, InfrastructureSection::default());
        Ok(())
    }

    #[test]
    fn test_infrastructure_section_full_table_round_trip() -> TestResult {
        let src = r#"
            auth_socket = "/run/harw/infra/secure.sock"
            network_socket = "/run/harw/infra/network.sock"
            security_socket = "/run/harw/infra/security.sock"
            token_file = "/etc/harw/infra/auth.token"
        "#;
        let section: InfrastructureSection = toml::from_str(src).map_err(ctx("parse toml"))?;
        assert_eq!(
            section.auth_socket,
            Some(PathBuf::from("/run/harw/infra/secure.sock"))
        );
        assert_eq!(
            section.network_socket,
            Some(PathBuf::from("/run/harw/infra/network.sock"))
        );
        assert_eq!(
            section.security_socket,
            Some(PathBuf::from("/run/harw/infra/security.sock"))
        );
        assert_eq!(
            section.token_file,
            Some(PathBuf::from("/etc/harw/infra/auth.token"))
        );
        assert!(!section.is_empty());

        let encoded = toml::to_string(&section).map_err(ctx("encode toml"))?;
        let decoded: InfrastructureSection =
            toml::from_str(&encoded).map_err(ctx("parse encoded toml"))?;
        assert_eq!(decoded, section);
        Ok(())
    }

    #[test]
    fn test_infrastructure_section_rejects_unknown_field() {
        let parsed = toml::from_str::<InfrastructureSection>(r#"auth_sock = "/x""#);
        assert!(parsed.is_err());
    }
}
