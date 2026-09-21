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
use harw_core::{
    EchoModelProvider, ModelError, ModelFuture, ModelProvider, ModelRequest, PinnedModelProvider,
};
use harw_provider_http::SecretResolver;
use harw_types::ModelId;

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

/// Ergebnis der UIA-Provider/-Modell-Auflösung (siehe [`resolve_uia_model`]).
enum UiaModelResolution {
    /// `uia_provider`/`uia_model` sind nicht beide nutzbar gesetzt — die
    /// UIA-Sitzung nutzt das bereits gebaute Vorgabe-Modell des Laufs
    /// unverändert (kein zweiter HTTP-Client).
    UsesDefault,
    /// `uia_provider` bezeichnet einen vorhandenen, aktivierten Provider und
    /// `uia_model` ist gesetzt — ein eigenständiger HTTP-Client wird dafür
    /// gebaut.
    Explicit { provider: String, model: String },
}

/// Löst das effektive UIA-Provider/-Modell-Paar auf.
///
/// # Beschreibung
/// Spiegelt [`resolve_default_model`], allerdings ohne Fallback-Kette: ist
/// das konfigurierte `uia_provider`/`uia_model`-Paar nicht nutzbar, liefert
/// diese Funktion `UsesDefault` — die UIA-Sitzung fällt dann auf das bereits
/// gebaute Vorgabe-Modell des Laufs zurück (`default_provider`/
/// `default_model`, ggf. bereits selbst per [`resolve_default_model`]
/// ausgewichen), statt eine eigene Katalog-Suche zu betreiben.
///
/// `UsesDefault` gilt, wenn `uia_provider` fehlt, keinen vorhandenen,
/// aktivierten Provider bezeichnet, oder wenn `uia_model` fehlt.
///
/// # Arguments
/// - `config` (`&ResolvedConfig`): die bereits validierte Konfiguration
///   dieses Laufs.
///
/// # Returns
/// [`UiaModelResolution`] — siehe dort für die beiden Fälle.
fn resolve_uia_model(config: &ResolvedConfig) -> UiaModelResolution {
    match (
        config.harness.uia_provider.as_deref(),
        config.harness.uia_model.as_deref(),
    ) {
        (Some(provider_id), Some(model_id)) if provider_is_usable(config, provider_id) => {
            UiaModelResolution::Explicit {
                provider: provider_id.to_owned(),
                model: model_id.to_owned(),
            }
        }
        _ => UiaModelResolution::UsesDefault,
    }
}

/// Baut das Modell der interaktiven UIA-Sitzung.
///
/// # Description
/// Entspricht [`build_root_model`], nur für die UIA-Rolle: existiert, weil
/// `harw-config::HarnessConfig::uia_provider`/`uia_model` das Modell der
/// UIA-Sitzung unabhängig von `default_provider`/`default_model` pinnen
/// können (siehe `harw-config/src/harness_config.rs`). Löst `resolver` nicht
/// selbst auf — siehe [`build_uia_model_with_resolver`] für den Weg mit
/// injiziertem `secrets:`-Resolver.
///
/// # Arguments
/// - `spec` (`&RuntimeSpec`): siehe [`build_root_model`].
/// - `config` (`&ResolvedConfig`): siehe [`build_root_model`].
/// - `source_is_configured` (`bool`): vom Aufrufer **vor** dem Verbrauch des
///   [`ModelSource`] per `matches!(source, ModelSource::Configured)`
///   ermittelt (`ModelSource` wird by-value konsumiert). Bei `false`
///   (Echo/Override) berührt die UIA-Sitzung `harw-provider-http` nie und
///   erhält unverändert `default_tree_model` zurück.
/// - `default_tree_model` (`&Arc<dyn ModelProvider>`): das bereits gebaute
///   Vorgabe-Modell des Laufs ([`build_root_model`]); Rückgabewert, wenn
///   `source_is_configured` `false` ist oder [`resolve_uia_model`]
///   `UiaModelResolution::UsesDefault` liefert.
///
/// # Returns
/// `Arc<dyn ModelProvider>` — entweder `Arc::clone(default_tree_model)` oder
/// ein eigenständig gebauter HTTP-Client für `uia_provider`/`uia_model`.
///
/// # Errors
/// Wie [`build_root_model`]: [`RuntimeError::Provider`], wenn der
/// konfigurierte UIA-Provider nicht gebaut werden kann.
pub fn build_uia_model(
    spec: &RuntimeSpec,
    config: &ResolvedConfig,
    source_is_configured: bool,
    default_tree_model: &Arc<dyn ModelProvider>,
) -> RuntimeResult<Arc<dyn ModelProvider>> {
    build_uia_model_with_resolver(spec, config, source_is_configured, default_tree_model, None)
}

/// Wie [`build_uia_model`], zusätzlich mit injiziertem `secrets:`-Resolver.
///
/// # Description
/// Existiert aus demselben Grund wie
/// [`build_root_model_with_resolver`] (siehe Modul-Dokumentation, Abschnitt
/// „Geheimnisse"): dieses Crate kann den versiegelten Speicher nicht selbst
/// öffnen. Der Aufrufer baut den Resolver wie bisher und reicht ihn hier
/// herein.
///
/// Im `Explicit`-Zweig folgt diese Funktion exakt dem Fallback-Muster aus
/// [`build_root_model_with_resolver`]: eine Kopie der Konfiguration erhält
/// `default_provider`/`default_model` auf das UIA-Paar gesetzt und wird
/// unverändert an [`harw_provider_http::build_provider_with_home`]
/// weitergereicht — das baut einen zweiten, unabhängigen HTTP-Client, ohne
/// `harw-provider-http` selbst anzufassen.
///
/// # Arguments
/// Wie [`build_uia_model`], zusätzlich:
/// - `resolver` (`Option<&dyn SecretResolver>`): siehe
///   [`build_root_model_with_resolver`].
///
/// # Returns
/// Wie [`build_uia_model`].
///
/// # Errors
/// Wie [`build_uia_model`].
pub fn build_uia_model_with_resolver(
    spec: &RuntimeSpec,
    config: &ResolvedConfig,
    source_is_configured: bool,
    default_tree_model: &Arc<dyn ModelProvider>,
    resolver: Option<&dyn SecretResolver>,
) -> RuntimeResult<Arc<dyn ModelProvider>> {
    if !source_is_configured {
        // Echo/Override: kein konfigurierter Root-Provider, also auch kein
        // eigener UIA-Client — `harw-provider-http` bleibt unberührt.
        return Ok(Arc::clone(default_tree_model));
    }

    match resolve_uia_model(config) {
        UiaModelResolution::UsesDefault => Ok(Arc::clone(default_tree_model)),
        UiaModelResolution::Explicit { provider, model } => {
            let mut effective = config.clone();
            effective.harness.default_provider = Some(provider);
            effective.harness.default_model = Some(model);
            let provider = harw_provider_http::build_provider_with_home(
                &effective,
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

/// Löst die Modell-Kennung der uia-worker-Rollenfamilie auf.
///
/// # Beschreibung
/// `None`, wenn `uia_worker_model` nicht gesetzt ist, nicht im
/// Modell-Katalog (`config.models`) steht, oder wenn das Katalog-Modell
/// einen Provider trägt, der nicht mit dem effektiven UIA-Provider
/// übereinstimmt. Der effektive UIA-Provider ist dieselbe Or-Kette wie an
/// anderer Stelle im Harness (`harw-cli/src/models.rs`:
/// `harness.uia_provider.as_deref().or(default_provider)`):
/// `uia_provider`, falls gesetzt, sonst `default_provider`.
///
/// Diese Kopplung ist bewusst streng — `uia_worker_model` pinnt **nur** die
/// Modell-Kennung, nie den Provider (siehe [`build_uia_worker_model`]); ein
/// Katalog-Modell mit abweichendem Provider würde sonst an den falschen
/// HTTP-Client geschickt und dort am `selected_model()`-Provider-Abgleich
/// scheitern (`harw-provider-http/src/lib.rs` `selected_model`).
///
/// Bei Ablehnung wird eine `tracing::warn!` mit den beteiligten Katalog-IDs
/// (keine Geheimnisse) protokolliert.
///
/// # Arguments
/// - `config` (`&ResolvedConfig`): die bereits validierte Konfiguration
///   dieses Laufs.
///
/// # Returns
/// `Some(model_id)`, wenn die Kopplungsregel erfüllt ist, sonst `None`.
fn resolve_uia_worker_model(config: &ResolvedConfig) -> Option<String> {
    let worker_model_id = config.harness.uia_worker_model.as_deref()?;

    let Some(catalog_entry) = config.models.get(worker_model_id) else {
        tracing::warn!(
            uia_worker_model = worker_model_id,
            "configured uia_worker_model is not present in the model catalog — falling back to \
             the uia session model for the uia-worker role family"
        );
        return None;
    };

    let effective_uia_provider = config
        .harness
        .uia_provider
        .as_deref()
        .or(config.harness.default_provider.as_deref());

    if Some(catalog_entry.provider.as_str()) != effective_uia_provider {
        tracing::warn!(
            uia_worker_model = worker_model_id,
            catalog_provider = catalog_entry.provider.as_str(),
            effective_uia_provider = effective_uia_provider.unwrap_or("<none>"),
            "configured uia_worker_model's catalog provider does not match the effective uia \
             provider — falling back to the uia session model for the uia-worker role family"
        );
        return None;
    }

    Some(worker_model_id.to_owned())
}

/// Baut das Modell der uia-worker-Rollenfamilie (`uia-worker`,
/// `uia-explorer`, `uia-writer`, `uia-shell-worker`).
///
/// # Description
/// Kann nicht fehlschlagen — degradiert nur: liefert
/// [`resolve_uia_worker_model`] eine Modell-Kennung, wird sie über
/// [`PinnedModelProvider`] **nur** als `model_id` auf `uia_client` gepinnt.
/// Die `provider_id` wird nie umgebogen — ein Request mit falscher
/// `provider_id` würde von `selected_model()` im HTTP-Client abgelehnt
/// (siehe [`resolve_uia_worker_model`]). Liefert [`resolve_uia_worker_model`]
/// `None`, wird `uia_client` unverändert durchgereicht — die
/// uia-worker-Rollenfamilie nutzt dann das Modell, das `uia_client` bereits
/// trägt (`uia_model`).
///
/// # Arguments
/// - `config` (`&ResolvedConfig`): die bereits validierte Konfiguration
///   dieses Laufs.
/// - `uia_client` (`&Arc<dyn ModelProvider>`): der bereits gebaute
///   UIA-Client ([`build_uia_model`]), auf dem `uia_worker_model` ggf.
///   gepinnt wird.
///
/// # Returns
/// `Arc<dyn ModelProvider>` — entweder ein [`PinnedModelProvider`] um
/// `uia_client`, oder `Arc::clone(uia_client)` unverändert.
#[must_use]
pub fn build_uia_worker_model(
    config: &ResolvedConfig,
    uia_client: &Arc<dyn ModelProvider>,
) -> Arc<dyn ModelProvider> {
    match resolve_uia_worker_model(config) {
        Some(model_id) => Arc::new(PinnedModelProvider::new(
            Arc::clone(uia_client),
            None,
            Some(ModelId::from(model_id)),
        )),
        None => Arc::clone(uia_client),
    }
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
    use harw_types::{IngressSurface, PermissionTier, Principal, PrincipalKind, ProviderId};
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

    /// Fügt einen weiteren netzlosen Loopback-Provider unter `name` ein
    /// (gleiches Muster wie der `"local"`-Provider in [`loopback_config`]).
    fn insert_loopback_provider(config: &mut ResolvedConfig, name: &str) {
        config.providers.insert(
            name.to_owned(),
            ProviderToml {
                name: name.to_owned(),
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
    }

    /// Minimale Katalog-Modell-Fixtur für `config.models`.
    fn model_toml(id: &str, provider: &str) -> harw_config::ModelToml {
        harw_config::ModelToml {
            id: id.to_owned(),
            name: None,
            provider: provider.to_owned(),
            aliases: Vec::new(),
            context_window: None,
            max_tokens: None,
            prompt_caching: None,
            reasoning: false,
            input_types: Vec::new(),
            capabilities: harw_config::ModelCapabilitiesToml::default(),
        }
    }

    /// Konfiguration mit zwei baubaren, netzlosen Loopback-Providern
    /// (`"local-a"` als Vorgabe, `"local-b"` als abweichender UIA-Provider).
    fn two_provider_config() -> ResolvedConfig {
        let mut config = ResolvedConfig {
            harness: HarnessConfig {
                default_provider: Some("local-a".to_owned()),
                default_model: Some("local-a-model".to_owned()),
                ..HarnessConfig::default()
            },
            ..ResolvedConfig::default()
        };
        insert_loopback_provider(&mut config, "local-a");
        insert_loopback_provider(&mut config, "local-b");
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

    // ── resolve_uia_model / build_uia_model ─────────────────────────────

    #[test]
    fn resolve_uia_model_prefers_the_configured_pair_when_usable() {
        let mut config = loopback_config();
        config.harness.uia_provider = Some("local".to_owned());
        config.harness.uia_model = Some("uia-model".to_owned());

        match resolve_uia_model(&config) {
            UiaModelResolution::Explicit { provider, model } => {
                assert_eq!(provider, "local");
                assert_eq!(model, "uia-model");
            }
            UiaModelResolution::UsesDefault => panic!("expected an explicit uia pair"),
        }
    }

    #[test]
    fn resolve_uia_model_falls_back_to_default_when_uia_provider_is_disabled() {
        let mut config = loopback_config();
        insert_loopback_provider(&mut config, "uia-only");
        config
            .providers
            .get_mut("uia-only")
            .expect("provider")
            .enabled = false;
        config.harness.uia_provider = Some("uia-only".to_owned());
        config.harness.uia_model = Some("uia-model".to_owned());

        assert!(matches!(
            resolve_uia_model(&config),
            UiaModelResolution::UsesDefault
        ));
    }

    #[test]
    fn resolve_uia_model_falls_back_to_default_when_uia_model_is_unset() {
        let mut config = loopback_config();
        config.harness.uia_provider = Some("local".to_owned());

        assert!(matches!(
            resolve_uia_model(&config),
            UiaModelResolution::UsesDefault
        ));
    }

    #[test]
    fn build_uia_model_returns_the_default_tree_model_unchanged_for_echo_source() {
        let spec = spec_for(Path::new("/nonexistent-home"));
        let config = loopback_config();
        let default_tree_model: Arc<dyn ModelProvider> =
            Arc::new(EchoModelProvider::new("echo: hallo"));

        let uia_model = build_uia_model(&spec, &config, false, &default_tree_model)
            .expect("echo/override path never fails to build");

        assert!(Arc::ptr_eq(&default_tree_model, &uia_model));
    }

    /// Beweistest: `uia_provider` weicht von `default_provider` ab — der
    /// gebaute UIA-Client ist ein eigenständiger HTTP-Client, dessen
    /// `respond()` mit `provider_id = "local-b"` nicht am
    /// `selected_model()`-Provider-Abgleich scheitert (im Unterschied zum
    /// Verhalten, wenn man `local-b` fälschlich an den `local-a`-Client
    /// schicken würde).
    #[test]
    fn build_uia_model_builds_a_dedicated_client_when_uia_provider_differs_from_default() {
        let spec = spec_for(Path::new("/nonexistent-home"));
        let mut config = two_provider_config();
        config.harness.uia_provider = Some("local-b".to_owned());
        config.harness.uia_model = Some("local-b-model".to_owned());
        config
            .models
            .insert("local-b-model".to_owned(), model_toml("local-b-model", "local-b"));

        let default_tree_model = build_root_model(&spec, &config, ModelSource::Configured)
            .expect("default provider (local-a) must build");
        let uia_model = build_uia_model(&spec, &config, true, &default_tree_model)
            .expect("uia provider (local-b) must build as a dedicated client");

        assert!(
            !Arc::ptr_eq(&default_tree_model, &uia_model),
            "the uia client must be a second, independent provider instance"
        );

        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime");
        let request = empty_request().with_provider_id(Some(ProviderId::from("local-b")));
        let result = runtime.block_on(uia_model.respond(request));
        if let Err(error) = result {
            let message = error.to_string();
            assert!(
                !message.contains("this HTTP provider is configured for"),
                "a provider_id matching the dedicated uia client's own provider must not be \
                 rejected by selected_model(): {message}"
            );
        }
    }

    // ── resolve_uia_worker_model / build_uia_worker_model ───────────────

    #[test]
    fn resolve_uia_worker_model_uses_none_when_unset() {
        let config = loopback_config();
        assert_eq!(resolve_uia_worker_model(&config), None);
    }

    #[test]
    fn resolve_uia_worker_model_uses_none_when_model_is_dangling_in_catalog() {
        let mut config = loopback_config();
        config.harness.uia_worker_model = Some("nonexistent-model".to_owned());

        assert_eq!(resolve_uia_worker_model(&config), None);
    }

    #[test]
    fn resolve_uia_worker_model_rejects_mismatched_provider_and_falls_back_to_none() {
        let mut config = loopback_config();
        insert_loopback_provider(&mut config, "other");
        config.harness.uia_worker_model = Some("other-model".to_owned());
        config
            .models
            .insert("other-model".to_owned(), model_toml("other-model", "other"));
        // uia_provider unset -> effective uia provider falls back to default_provider ("local"),
        // but the catalog model belongs to "other" -> must be rejected.

        assert_eq!(resolve_uia_worker_model(&config), None);
    }

    #[test]
    fn build_uia_worker_model_pins_only_model_id_never_provider_id() {
        let mut config = loopback_config();
        config.harness.uia_worker_model = Some("local-model".to_owned());
        config
            .models
            .insert("local-model".to_owned(), model_toml("local-model", "local"));

        let recorder = harw_core::testing::RecordingModelProvider::new();
        let uia_client: Arc<dyn ModelProvider> = Arc::new(recorder.clone());

        let worker_model = build_uia_worker_model(&config, &uia_client);
        assert!(
            !Arc::ptr_eq(&uia_client, &worker_model),
            "an accepted choice must wrap uia_client, not pass it through unchanged"
        );

        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime");
        let caller_provider_id = ProviderId::from("whatever-the-caller-already-set");
        let request = empty_request().with_provider_id(Some(caller_provider_id.clone()));
        runtime
            .block_on(worker_model.respond(request))
            .expect("recording provider never fails");

        let recorded = recorder.last().expect("request must have been forwarded");
        assert_eq!(recorded.model_id, Some(ModelId::from("local-model")));
        assert_eq!(
            recorded.provider_id,
            Some(caller_provider_id),
            "provider_id must pass through untouched — only model_id may be pinned"
        );
    }

    #[test]
    fn build_uia_worker_model_falls_back_to_uia_client_when_choice_is_rejected() {
        let mut config = loopback_config();
        // "dangling-model" is not present in config.models -> rejected.
        config.harness.uia_worker_model = Some("dangling-model".to_owned());

        let uia_client: Arc<dyn ModelProvider> = Arc::new(EchoModelProvider::new("uia says hi"));
        let worker_model = build_uia_worker_model(&config, &uia_client);

        assert!(Arc::ptr_eq(&uia_client, &worker_model));
    }
}
