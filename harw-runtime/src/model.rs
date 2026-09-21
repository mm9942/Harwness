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
use harw_core::{EchoModelProvider, ModelError, ModelFuture, ModelProvider, ModelRequest};
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
            // Fallback-Speicherplatz: nur belegt, wenn `resolve_default_model`
            // tatsächlich einen abweichenden Vorgabe-Wert wählt (siehe dort).
            let fallback_holder;
            let effective_config = match resolve_default_model(config) {
                DefaultModelResolution::AsConfigured => config,
                DefaultModelResolution::Fallback { provider, model } => {
                    tracing::warn!(
                        configured_provider =
                            config.harness.default_provider.as_deref().unwrap_or("<none>"),
                        configured_model =
                            config.harness.default_model.as_deref().unwrap_or("<none>"),
                        fallback_provider = %provider,
                        fallback_model = %model,
                        "configured default provider/model is unusable — falling back to the \
                         first usable configured model instead of aborting startup"
                    );
                    let mut fallback = config.clone();
                    fallback.harness.default_provider = Some(provider);
                    fallback.harness.default_model = Some(model);
                    fallback_holder = fallback;
                    &fallback_holder
                }
                DefaultModelResolution::NoneUsable => {
                    tracing::warn!(
                        "no usable model/provider is configured — starting anyway; any model \
                         call will fail clearly until an enabled provider is referenced by \
                         default_provider/default_model or a catalog model"
                    );
                    return Ok(Arc::new(UnusableModelProvider::new(
                        "no usable model/provider is configured; add an enabled provider under \
                         providers/ and reference it from default_provider/default_model (or a \
                         models/ entry) before sending a message"
                            .to_owned(),
                    )));
                }
            };
            let provider = harw_provider_http::build_provider_with_home(
                effective_config,
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

/// Ergebnis der Vorgabe-Provider/-Modell-Auflösung für
/// [`ModelSource::Configured`] (siehe [`resolve_default_model`]).
enum DefaultModelResolution {
    /// `default_provider` zeigt auf einen vorhandenen, aktivierten Provider
    /// und `default_model` ist gesetzt — unverändert an
    /// `harw_provider_http::build_provider_with_home` weiterreichen. Das
    /// Modell muss dafür **nicht** im Katalog (`config.models`) stehen: der
    /// Provider erhält die Modell-Kennung unverändert (siehe
    /// `harw-provider-http`).
    AsConfigured,
    /// Das Vorgabe-Paar war hängend, fehlte oder der Provider war
    /// deaktiviert; dieses katalogisierte Modell mit vorhandenem,
    /// aktiviertem Provider dient stattdessen als Vorgabe.
    Fallback { provider: String, model: String },
    /// Kein einziges katalogisiertes Modell hat einen vorhandenen,
    /// aktivierten Provider — es gibt nichts, worauf ausgewichen werden
    /// könnte.
    NoneUsable,
}

/// `true`, wenn `provider_id` einen tatsächlich konfigurierten, aktivierten
/// Provider bezeichnet.
fn provider_is_usable(config: &ResolvedConfig, provider_id: &str) -> bool {
    config
        .providers
        .get(provider_id)
        .is_some_and(|provider| provider.enabled)
}

/// Löst das effektive Vorgabe-Provider/-Modell-Paar für
/// [`ModelSource::Configured`] auf.
///
/// # Beschreibung
/// Ein hängender `default_provider`/`default_model` (siehe
/// [`harw_config::discovery::ConfigDiagnostic`], `ResolvedConfig::validate`
/// bricht dafür nicht mehr ab) darf die gesamte Anwendung nicht unbenutzbar
/// machen: statt `harw_provider_http::build_provider_with_home` sofort mit
/// einem Konfigurationsfehler scheitern zu lassen, wählt diese Funktion —
/// deterministisch, sortiert nach Modell-Kennung — das erste katalogisierte
/// Modell (`config.models`), dessen Provider vorhanden und aktiviert ist.
///
/// Das aktuell konfigurierte Paar gilt bereits als nutzbar, sobald
/// `default_provider` einen vorhandenen, aktivierten Provider bezeichnet und
/// `default_model` überhaupt gesetzt ist — das Modell muss dafür **nicht**
/// im Katalog stehen (viele gültige Konfigurationen, u. a. die
/// Loopback-Testfixtur dieses Moduls, lassen `config.models` bewusst leer).
///
/// # Arguments
/// - `config` (`&ResolvedConfig`): die bereits validierte Konfiguration
///   dieses Laufs.
///
/// # Returns
/// [`DefaultModelResolution`] — siehe dort für die drei Fälle.
fn resolve_default_model(config: &ResolvedConfig) -> DefaultModelResolution {
    let provider_ok = config
        .harness
        .default_provider
        .as_deref()
        .is_some_and(|provider_id| provider_is_usable(config, provider_id));
    let model_ok = config.harness.default_model.is_some();
    if provider_ok && model_ok {
        return DefaultModelResolution::AsConfigured;
    }

    let mut model_ids: Vec<&String> = config.models.keys().collect();
    model_ids.sort();
    for model_id in model_ids {
        let provider = &config.models[model_id].provider;
        if provider_is_usable(config, provider) {
            return DefaultModelResolution::Fallback {
                provider: provider.clone(),
                model: model_id.clone(),
            };
        }
    }

    DefaultModelResolution::NoneUsable
}

/// Ein Modell-Provider ohne funktionierende Konfiguration.
///
/// # Beschreibung
/// Konstruierbar ohne Fehler — der Lauf startet — scheitert aber mit einer
/// klaren, statischen Meldung, sobald tatsächlich ein Modell-Aufruf versucht
/// wird ([`ModelProvider::respond`]). Deckt den Fall ab, in dem
/// [`resolve_default_model`] kein einziges nutzbares Modell/Provider-Paar
/// findet: "ein falsch konfigurierter Provider/ein falsch konfiguriertes
/// Modell darf nicht die gesamte Anwendung unbenutzbar machen" gilt auch,
/// wenn **gar kein** Provider konfiguriert ist — der Fehler erscheint dann
/// erst bei tatsächlicher Nutzung, nicht beim Start.
struct UnusableModelProvider {
    message: String,
}

impl UnusableModelProvider {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl ModelProvider for UnusableModelProvider {
    fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
        let message = self.message.clone();
        Box::pin(async move { Err(ModelError::RequestFailed(message)) })
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
                originator: None,
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

    /// F-046-style Verhalten: ohne jeden nutzbaren Provider/jedes nutzbare
    /// Modell startet der Lauf trotzdem — der Fehler erscheint erst, wenn
    /// tatsächlich ein Modell-Aufruf versucht wird, nicht beim Bau des
    /// Root-Modells.
    #[test]
    fn configured_source_without_any_usable_provider_starts_and_fails_only_on_a_model_call() {
        let spec = spec_for(Path::new("/nonexistent-home"));
        let model = build_root_model(&spec, &ResolvedConfig::default(), ModelSource::Configured)
            .expect("a config without any usable provider must still start");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime");
        let error = runtime
            .block_on(model.respond(empty_request()))
            .expect_err("a model call must fail clearly when nothing is configured");
        assert!(matches!(
            error,
            harw_core::ModelError::RequestFailed(ref message)
                if message.contains("no usable model/provider is configured")
        ));
    }

    /// F-046-Regression: ein hängender `default_provider` darf den Start
    /// nicht abbrechen, solange ein anderer, katalogisierter Provider
    /// tatsächlich nutzbar ist.
    #[test]
    fn configured_source_falls_back_to_first_usable_catalog_model_when_default_is_dangling() {
        let spec = spec_for(Path::new("/nonexistent-home"));
        let mut config = loopback_config();
        config.harness.default_provider = Some("gpt-5.6-terra-provider".to_owned());
        config.harness.default_model = Some("gpt-5.6-terra".to_owned());
        config.models.insert(
            "local-model".to_owned(),
            harw_config::ModelToml {
                id: "local-model".to_owned(),
                name: None,
                provider: "local".to_owned(),
                aliases: Vec::new(),
                context_window: None,
                max_tokens: None,
                prompt_caching: None,
                reasoning: false,
                input_types: Vec::new(),
                capabilities: harw_config::ModelCapabilitiesToml::default(),
            },
        );

        build_root_model(&spec, &config, ModelSource::Configured)
            .expect("a dangling default must fall back to the usable catalog model");
    }

    #[test]
    fn resolve_default_model_prefers_the_configured_pair_when_usable() {
        let config = loopback_config();
        assert!(matches!(
            resolve_default_model(&config),
            DefaultModelResolution::AsConfigured
        ));
    }

    #[test]
    fn resolve_default_model_ignores_a_disabled_default_provider() {
        let mut config = loopback_config();
        config
            .providers
            .get_mut("local")
            .expect("provider")
            .enabled = false;

        assert!(matches!(
            resolve_default_model(&config),
            DefaultModelResolution::NoneUsable
        ));
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
