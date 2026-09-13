//! Local MCP credential resolution at the composition boundary.
//!
//! Config only carries a [`harw_config::SecretRef`]. This module is the first
//! runtime consumer: supported local references are resolved here and every
//! unsupported backend fails closed rather than turning a reference into an
//! accidental plaintext credential.

use std::fmt;
use std::path::Path;

use harw_config::SecretRef;
use harw_provider_http::SecretResolver;
use secrecy::ExposeSecret as _;

#[derive(Debug)]
pub enum McpCredentialError {
    MissingEnvironment {
        name: String,
    },
    EmptyCredential {
        reference: String,
    },
    FileRead {
        path: String,
        source: std::io::Error,
    },
    UnsupportedReference {
        reference: String,
    },
    InvalidKeyringReference {
        reference: String,
    },
    KeyringAccess {
        reference: String,
    },
    JsonField {
        path: String,
        pointer: String,
    },
    SecretResolution {
        reference: String,
        reason: String,
    },
}

impl fmt::Display for McpCredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEnvironment { name } => {
                write!(f, "MCP credential environment variable '{name}' is not set")
            }
            Self::EmptyCredential { reference } => {
                write!(f, "MCP credential '{reference}' resolved to an empty value")
            }
            Self::FileRead { path, source } => {
                write!(f, "could not read MCP credential file '{path}': {source}")
            }
            Self::UnsupportedReference { reference } => {
                write!(f, "MCP credential backend is not available: {reference}")
            }
            Self::InvalidKeyringReference { reference } => {
                write!(
                    f,
                    "MCP keyring credential reference must be service/account: {reference}"
                )
            }
            Self::KeyringAccess { reference } => {
                write!(f, "could not access MCP keyring credential '{reference}'")
            }
            Self::JsonField { path, pointer } => {
                write!(
                    f,
                    "MCP credential JSON pointer '{pointer}' not found or not a string in '{path}'"
                )
            }
            Self::SecretResolution { reference, reason } => {
                write!(
                    f,
                    "could not resolve MCP credential '{reference}': {reason}"
                )
            }
        }
    }
}
impl std::error::Error for McpCredentialError {}

/// Resolves a local MCP credential, optionally using a sealed-secret resolver.
///
/// `secrets:` references fail closed when no resolver is injected.
pub fn resolve_mcp_credential_with_resolver(
    reference: &SecretRef,
    resolver: Option<&dyn SecretResolver>,
) -> Result<Vec<u8>, McpCredentialError> {
    let reference_string = reference.as_ref_string();
    let value = match reference {
        SecretRef::Env(name) => std::env::var(name)
            .map_err(|_| McpCredentialError::MissingEnvironment { name: name.clone() })?,
        SecretRef::File(path) => std::fs::read_to_string(Path::new(path)).map_err(|source| {
            McpCredentialError::FileRead {
                path: path.clone(),
                source,
            }
        })?,
        SecretRef::FileJson { path, pointer } => {
            let raw = std::fs::read_to_string(Path::new(path)).map_err(|source| {
                McpCredentialError::FileRead {
                    path: path.clone(),
                    source,
                }
            })?;
            let doc: serde_json::Value =
                serde_json::from_str(&raw).map_err(|_| McpCredentialError::JsonField {
                    path: path.clone(),
                    pointer: pointer.clone(),
                })?;
            doc.pointer(pointer)
                .and_then(|value| value.as_str())
                .map(str::to_owned)
                .ok_or_else(|| McpCredentialError::JsonField {
                    path: path.clone(),
                    pointer: pointer.clone(),
                })?
        }
        SecretRef::Secrets(name) => resolver
            .ok_or_else(|| McpCredentialError::UnsupportedReference {
                reference: reference_string.clone(),
            })?
            .resolve(name)
            .map_err(|reason| McpCredentialError::SecretResolution {
                reference: reference_string.clone(),
                reason,
            })?
            .expose_secret()
            .to_owned(),
        SecretRef::Keyring(keyring_reference) => {
            let (service, account) =
                parse_keyring_reference(keyring_reference).ok_or_else(|| {
                    McpCredentialError::InvalidKeyringReference {
                        reference: reference_string.clone(),
                    }
                })?;
            let entry = keyring::Entry::new(service, account).map_err(|_| {
                McpCredentialError::KeyringAccess {
                    reference: reference_string.clone(),
                }
            })?;
            entry
                .get_password()
                .map_err(|_| McpCredentialError::KeyringAccess {
                    reference: reference_string.clone(),
                })?
        }
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(McpCredentialError::EmptyCredential {
            reference: reference_string,
        });
    }
    Ok(trimmed.as_bytes().to_vec())
}

fn parse_keyring_reference(reference: &str) -> Option<(&str, &str)> {
    let (service, account) = reference.split_once('/')?;
    (!service.is_empty() && !account.is_empty() && !account.contains('/'))
        .then_some((service, account))
}

/// Resolves MCP credentials without a sealed-secret resolver.
///
/// This compatibility wrapper preserves the fail-closed behavior for
/// `secrets:` references.
// Vorgesehener Aufrufer: `harw doctor` (main.rs, fn `doctor`), sobald die
// Konfigurationsprüfung credential_ref-Auflösung ohne Secret-Store abdeckt.
#[allow(dead_code)]
pub fn resolve_mcp_credential(reference: &SecretRef) -> Result<Vec<u8>, McpCredentialError> {
    resolve_mcp_credential_with_resolver(reference, None)
}

#[cfg(test)]
mod tests {
    use harw_config::SecretRef;
    use harw_provider_http::SecretResolver;
    use secrecy::{ExposeSecret as _, SecretString};

    use super::{
        McpCredentialError, parse_keyring_reference, resolve_mcp_credential,
        resolve_mcp_credential_with_resolver,
    };

    struct FakeSecretResolver {
        result: Result<SecretString, String>,
    }

    impl SecretResolver for FakeSecretResolver {
        fn resolve(&self, _reference: &str) -> Result<SecretString, String> {
            match &self.result {
                Ok(secret) => Ok(SecretString::new(secret.expose_secret().to_owned().into())),
                Err(reason) => Err(reason.clone()),
            }
        }
    }

    #[test]
    fn injected_resolver_resolves_and_trims_sealed_secret() {
        let resolver = FakeSecretResolver {
            result: Ok(SecretString::new("  sealed-mcp-token\n".into())),
        };

        let credential = resolve_mcp_credential_with_resolver(
            &SecretRef::Secrets("tenant/mcp-token".to_owned()),
            Some(&resolver),
        )
        .expect("injected resolver should resolve a sealed MCP credential");

        assert_eq!(credential, b"sealed-mcp-token");
    }

    #[test]
    fn secrets_reference_without_resolver_is_rejected() {
        let error = resolve_mcp_credential(&SecretRef::Secrets("tenant/mcp-token".to_owned()))
            .expect_err("secrets references must fail closed without a resolver");

        assert!(matches!(
            error,
            McpCredentialError::UnsupportedReference { .. }
        ));
    }

    #[test]
    fn resolver_failure_keeps_safe_diagnostic_without_secret_value() {
        let secret = "must-not-appear-in-error";
        let resolver = FakeSecretResolver {
            result: Err("sealed secret resolver unavailable".to_owned()),
        };

        let error = resolve_mcp_credential_with_resolver(
            &SecretRef::Secrets("tenant/mcp-token".to_owned()),
            Some(&resolver),
        )
        .expect_err("resolver failures must reject the MCP credential");

        let diagnostic = error.to_string();
        assert!(matches!(error, McpCredentialError::SecretResolution { .. }));
        assert!(diagnostic.contains("sealed secret resolver unavailable"));
        assert!(!diagnostic.contains(secret));
    }

    #[test]
    fn empty_sealed_secret_is_rejected_after_trimming() {
        let resolver = FakeSecretResolver {
            result: Ok(SecretString::new(" \t\n ".into())),
        };

        let error = resolve_mcp_credential_with_resolver(
            &SecretRef::Secrets("tenant/mcp-token".to_owned()),
            Some(&resolver),
        )
        .expect_err("empty sealed credentials must be rejected");

        assert!(matches!(error, McpCredentialError::EmptyCredential { .. }));
    }

    #[test]
    fn keyring_reference_parses_exact_service_and_account() {
        assert_eq!(
            parse_keyring_reference("harwness/mcp-token"),
            Some(("harwness", "mcp-token"))
        );
    }

    #[test]
    fn keyring_reference_rejects_missing_or_ambiguous_parts() {
        for reference in [
            "",
            "service",
            "/account",
            "service/",
            "service/account/extra",
        ] {
            assert_eq!(
                parse_keyring_reference(reference),
                None,
                "reference {reference:?} should be rejected"
            );
        }
    }

    #[test]
    fn invalid_keyring_reference_fails_before_os_keyring_access() {
        let error = resolve_mcp_credential(&SecretRef::Keyring("service/account/extra".to_owned()))
            .expect_err("ambiguous keyring references must be rejected");

        assert!(matches!(
            error,
            McpCredentialError::InvalidKeyringReference { .. }
        ));
    }
}
