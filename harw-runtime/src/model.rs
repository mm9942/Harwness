//! Root-Modell der Runtime-Montage.
//!
//! # Beschreibung
//! Vereinheitlicht die drei bisher getrennten Wege, auf denen die CLI ihren
//! Root-Provider baute:
//!
//! - [`ModelSource::Configured`] ersetzt `harw-cli/src/chat.rs:876`
//!   `build_model` (interaktiver Chat und One-Shot) **und**
//!   `harw-cli/src/main.rs:536` `build_serve_provider` (`harw serve`). Beide
//!   riefen bereits `harw_provider_http::build_provider_with_home`, nur mit
//!   unterschiedlicher Home-/Resolver-Behandlung.
//! - [`ModelSource::Echo`] ersetzt den Offline-Bootstrap aus
//!   `harw-cli/src/main.rs:683-760` `run_local_echo`
//!   (`EchoModelProvider::new(format!("echo: {input}"))`, `main.rs:704`).
//!
//! Zusätzlich nimmt [`ModelSource::Override`] einen bereits gebauten Provider
//! auf — der Fall, den Gateways (`harw-cli/src/gateway.rs:405-416`) und Tests
//! brauchen, die ihren Provider selbst zusammenstellen und ihn nur noch in die
//! Montage hineinreichen wollen.
//!
//! Alle Wege liefern denselben Typ: `Arc<dyn ModelProvider>`
//! (`harw-core/src/model.rs:399`). Der `Arc` ist nötig, weil derselbe Provider
//! im Root-Turn **und** im Kind-Spawner geteilt wird (`chat.rs` `one_shot`:
//! `let model: Arc<dyn ModelProvider> = Arc::from(model);`).
//!
//! # Geheimnisse
//! `secrets:`-Referenzen löst ein injizierter [`SecretResolver`] auf. Dieses
//! Crate kann ihn **nicht selbst** bauen: der einzige produktive Resolver ist
//! `harw-cli/src/secret_store.rs:66` `open_configured_secret_resolver`, der
//! `harw-secrets` (`SecretStore`, `load_kek_material`, `CryptoPolicy`)
//! braucht; `harw-secrets` steht nicht in `harw-runtime/Cargo.toml` und
//! Manifeste gehören nicht zu diesem Arbeitspaket. Deshalb gibt es zusätzlich
//! [`build_root_model_with_resolver`]: der Aufrufer (CLI/TUI) baut den
//! Resolver wie bisher und reicht ihn als `&dyn SecretResolver` herein.
//! [`build_root_model`] entspricht dem Aufruf mit `None` und deckt damit
//! `env:`-, `file:`- und `file-json:`-Referenzen ab.

use std::fmt;
use std::sync::Arc;

use harw_config::ResolvedConfig;
use harw_core::{EchoModelProvider, ModelProvider};
use harw_provider_http::SecretResolver;

use crate::error::{RuntimeError, RuntimeResult};
use crate::spec::RuntimeSpec;

/// Woher das Root-Modell eines Laufs stammt.
///
/// `Debug` ist von Hand geschrieben, weil [`ModelProvider`] kein `Debug`
/// verlangt (`harw-core/src/model.rs:399`) und ein `derive` an
/// `Arc<dyn ModelProvider>` scheitern würde.
pub enum ModelSource {
    /// Der aus der Konfiguration gebaute HTTP-Provider
    /// ([`harw_provider_http::build_provider_with_home`]).
    Configured,
    /// Der eingebaute Offline-Echo mit der angegebenen Antwort.
    ///
    /// Kein Netz, keine Credentials; `harw run` nutzt ihn, um den echten
    /// Turn-Loop ohne Provider-Anbindung zu fahren.
    Echo(String),
    /// Ein vom Aufrufer bereits gebauter Provider, der unverändert
    /// durchgereicht wird.
    Override(Arc<dyn ModelProvider>),
}

impl fmt::Debug for ModelSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configured => f.write_str("Configured"),
            // Der Echo-Text stammt aus der Nutzereingabe (`run_local_echo`)
            // und wird deshalb nicht mitgeschrieben, nur seine Länge.
            Self::Echo(reply) => f
                .debug_struct("Echo")
                .field("reply_len", &reply.len())
                .finish(),
            Self::Override(_) => f.write_str("Override(<dyn ModelProvider>)"),
        }
    }
}

impl ModelSource {
    /// Stabiler Name der Quelle für Diagnose und Protokoll.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::Echo(_) => "echo",
            Self::Override(_) => "override",
        }
    }
}

/// Baut das Root-Modell eines Laufs.
///
/// # Arguments
/// - `spec` (`&RuntimeSpec`): genutzt wird [`RuntimeSpec::home`] als
///   Root-Space; `file:`/`file-json:`-Credentials werden nur unterhalb von
///   `<home>/secrets/` gelesen (`harw-provider-http/src/lib.rs:175`).
/// - `config` (`&ResolvedConfig`): Ergebnis von
///   [`crate::config::load_config`].
/// - `source` (`ModelSource`): siehe [`ModelSource`].
///
/// # Errors
/// [`RuntimeError::Provider`], wenn der konfigurierte Provider nicht gebaut
/// werden kann (fehlender Default, unauflösbares Credential, ungültiger
/// Endpoint). Der Fehlertext ist der `Display` von
/// [`harw_provider_http::HttpProviderError`]; dieser nennt laut Ledger
/// `docs/remediation/ledger/W1/W1-06b.md` §2.3 nur redigierte Referenzen
/// (`file:<redacted>`) und statische Gründe, nie Credential-Werte.
pub fn build_root_model(
    spec: &RuntimeSpec,
    config: &ResolvedConfig,
    source: ModelSource,
) -> RuntimeResult<Arc<dyn ModelProvider>> {
    build_root_model_with_resolver(spec, config, source, None)
}

/// Wie [`build_root_model`], zusätzlich mit injiziertem
/// `secrets:`-Resolver.
///
/// # Description
/// Existiert, weil dieses Crate den versiegelten Speicher nicht selbst öffnen
/// kann (siehe Modul-Dokumentation, Abschnitt „Geheimnisse"). Der Aufrufer
/// baut den Resolver genau wie bisher
/// (`harw-cli/src/chat.rs:877`: `open_configured_secret_resolver(home, config)?`,
/// `harw-cli/src/main.rs:538`: `resolver.map(|r| r as &dyn SecretResolver)`)
/// und reicht ihn hier herein.
///
/// # Errors
/// Wie [`build_root_model`].
pub fn build_root_model_with_resolver(
    spec: &RuntimeSpec,
    config: &ResolvedConfig,
    source: ModelSource,
    resolver: Option<&dyn SecretResolver>,
) -> RuntimeResult<Arc<dyn ModelProvider>> {
    match source {
        ModelSource::Override(provider) => Ok(provider),
        ModelSource::Echo(reply) => Ok(Arc::new(EchoModelProvider::new(reply))),
        ModelSource::Configured => {
            let provider = harw_provider_http::build_provider_with_home(
                config,
                spec.home.as_path(),
                resolver,
            )
            .map_err(|error| RuntimeError::Provider {
                detail: error.to_string(),
            })?;
            Ok(Arc::from(provider))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::EntryKind;
    use harw_config::{HarnessConfig, OriginAllowlistToml, ProviderToml};
    use harw_core::{ModelRequest, ModelResponse};
    use harw_extension_api::types::LoadedInstructions;
    use harw_types::{IngressSurface, PermissionTier, Principal, PrincipalKind};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    fn spec_for(home: &Path) -> RuntimeSpec {
        RuntimeSpec {
            entry: EntryKind::OneShot,
            home: home.to_path_buf(),
            cwd: PathBuf::from("/nonexistent-cwd"),
            principal: Principal::trusted_ingress(
                PrincipalKind::Human,
                "test",
                IngressSurface::Tui,
                PermissionTier::Owner,
            ),
            mode_override: None,
            active_agent: None,
            reasoning_effort: None,
        }
    }

    /// Konfiguration mit genau einem baubaren, netzlosen Loopback-Provider.
    ///
    /// `auth_header = "none"` ist der einzige Weg ohne Credential
    /// (`harw-provider-http/src/lib.rs` `from_named_config`); `http` ist für
    /// Loopback-Hosts erlaubt (`validate_endpoint`).
    fn loopback_config() -> ResolvedConfig {
        let mut config = ResolvedConfig {
            harness: HarnessConfig {
                default_provider: Some("local".to_owned()),
                default_model: Some("local-model".to_owned()),
                ..HarnessConfig::default()
            },
            ..ResolvedConfig::default()
        };
        config.providers.insert(
            "local".to_owned(),
            ProviderToml {
                name: "local".to_owned(),
                api: "openai-chat".to_owned(),
                base_url: "http://127.0.0.1:11434/v1".to_owned(),
                auth: None,
                auth_header: Some("none".to_owned()),
                api_key: None,
                headers: HashMap::new(),
                models: Vec::new(),
                enabled: true,
                origin_allowlist: OriginAllowlistToml::default(),
                rate_limit: None,
                max_concurrency: None,
            },
        );
        config
    }

    fn empty_request() -> ModelRequest {
        ModelRequest::new(
            LoadedInstructions::default(),
            Vec::new(),
            harw_core::ConversationHistory::new(),
            Vec::new(),
        )
    }

    fn respond(provider: &dyn ModelProvider) -> ModelResponse {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime");
        runtime
            .block_on(provider.respond(empty_request()))
            .expect("echo response")
    }

    #[test]
    fn echo_source_answers_with_its_reply() {
        let spec = spec_for(Path::new("/nonexistent-home"));
        let model = build_root_model(
            &spec,
            &ResolvedConfig::default(),
            ModelSource::Echo("echo: hallo".to_owned()),
        )
        .expect("echo provider");

        let response = respond(model.as_ref());
        assert_eq!(response.message.as_deref(), Some("echo: hallo"));
        assert!(response.is_final());
    }

    #[test]
    fn override_source_is_passed_through_unchanged() {
        let spec = spec_for(Path::new("/nonexistent-home"));
        let provided: Arc<dyn ModelProvider> = Arc::new(EchoModelProvider::new("bereitgestellt"));
        let model = build_root_model(
            &spec,
            &ResolvedConfig::default(),
            ModelSource::Override(Arc::clone(&provided)),
        )
        .expect("override provider");

        assert!(Arc::ptr_eq(&provided, &model));
        assert_eq!(
            respond(model.as_ref()).message.as_deref(),
            Some("bereitgestellt")
        );
    }

    #[test]
    fn configured_source_builds_the_loopback_provider() {
        let spec = spec_for(Path::new("/nonexistent-home"));
        build_root_model(&spec, &loopback_config(), ModelSource::Configured)
            .expect("configured provider");
    }

    #[test]
    fn configured_source_without_default_provider_is_a_provider_error() {
        let spec = spec_for(Path::new("/nonexistent-home"));
        let Err(error) = build_root_model(
            &spec,
            &ResolvedConfig::default(),
            ModelSource::Configured,
        ) else {
            panic!("a config without default_provider must not build a provider");
        };
        assert!(matches!(error, RuntimeError::Provider { .. }), "{error}");
        assert!(error.to_string().starts_with("runtime provider error:"));
    }

    #[test]
    fn configured_source_rejects_a_non_loopback_plaintext_endpoint() {
        let spec = spec_for(Path::new("/nonexistent-home"));
        let mut config = loopback_config();
        config
            .providers
            .get_mut("local")
            .expect("provider")
            .base_url = "http://provider.example/v1".to_owned();

        let Err(error) = build_root_model(&spec, &config, ModelSource::Configured) else {
            panic!("plaintext http to a remote host must not build a provider");
        };
        assert!(matches!(error, RuntimeError::Provider { .. }), "{error}");
    }

    #[test]
    fn file_credentials_outside_home_secrets_fail_closed() {
        let spec = spec_for(Path::new("/nonexistent-home"));
        let mut config = loopback_config();
        let provider = config.providers.get_mut("local").expect("provider");
        provider.auth_header = Some("bearer".to_owned());
        provider.auth = Some(harw_config::SecretRef::File("/etc/hostname".to_owned()));

        let Err(error) = build_root_model(&spec, &config, ModelSource::Configured) else {
            panic!("a file: credential outside <home>/secrets must not resolve");
        };
        assert!(matches!(error, RuntimeError::Provider { .. }), "{error}");
        assert!(
            !error.to_string().contains("/etc/hostname"),
            "der Fehlertext darf die Referenz nicht im Klartext nennen: {error}"
        );
    }

    #[test]
    fn debug_hides_the_echo_text_and_labels_every_source() {
        let echo = ModelSource::Echo("geheime Eingabe".to_owned());
        let rendered = format!("{echo:?}");
        assert!(!rendered.contains("geheime Eingabe"), "{rendered}");
        assert!(rendered.contains("reply_len"), "{rendered}");
        assert_eq!(echo.label(), "echo");

        assert_eq!(format!("{:?}", ModelSource::Configured), "Configured");
        assert_eq!(ModelSource::Configured.label(), "configured");

        let over = ModelSource::Override(Arc::new(EchoModelProvider::new("x")));
        assert_eq!(format!("{over:?}"), "Override(<dyn ModelProvider>)");
        assert_eq!(over.label(), "override");
    }
}
