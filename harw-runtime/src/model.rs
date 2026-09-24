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
use harw_provider_http::{ProviderLoadRegistry, SecretResolver};
use harw_types::{ModelId, ProviderId};

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
    build_root_model_with_registry_and_resolver(spec, config, source, resolver)
        .map(|(model, _registry)| model)
}

/// Wie [`build_root_model_with_resolver`], liefert zusätzlich die
/// [`ProviderLoadRegistry`] des gebauten Providers.
///
/// # Description
/// Composition-Root-Baustein für den Auslastungskanal
/// ([`harw_provider_http::ProviderLoadControl`]): ruft für
/// [`ModelSource::Configured`]
/// [`harw_provider_http::build_provider_with_load_registry_and_home`] statt
/// [`harw_provider_http::build_provider_with_home`] auf und reicht die
/// Registry unverändert weiter. `Echo`/`Override`/`NoneUsable` liefern eine
/// leere Registry — keiner dieser Zweige baut einen HTTP-Provider, es gibt
/// also nichts zu registrieren.
///
/// [`build_root_model_with_resolver`] delegiert hierher und verwirft die
/// Registry — bestehende Aufrufer sind von dieser Erweiterung nicht
/// betroffen.
///
/// # Errors
/// Wie [`build_root_model_with_resolver`].
pub fn build_root_model_with_registry_and_resolver(
    spec: &RuntimeSpec,
    config: &ResolvedConfig,
    source: ModelSource,
    resolver: Option<&dyn SecretResolver>,
) -> RuntimeResult<(Arc<dyn ModelProvider>, ProviderLoadRegistry)> {
    match source {
        ModelSource::Override(provider) => Ok((provider, ProviderLoadRegistry::new())),
        ModelSource::Echo(reply) => Ok((
            Arc::new(EchoModelProvider::new(reply)),
            ProviderLoadRegistry::new(),
        )),
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
                    return Ok((
                        Arc::new(UnusableModelProvider::new(
                            "no usable model/provider is configured; add an enabled provider \
                             under providers/ and reference it from \
                             default_provider/default_model (or a models/ entry) before sending \
                             a message"
                                .to_owned(),
                        )),
                        ProviderLoadRegistry::new(),
                    ));
                }
            };
            let (provider, registry) =
                harw_provider_http::build_provider_with_load_registry_and_home(
                    effective_config,
                    spec.home.as_path(),
                    resolver,
                )
                .map_err(|error| RuntimeError::Provider {
                    detail: error.to_string(),
                })?;
            Ok((Arc::from(provider), registry))
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

/// Die Modellkennung, die der Wurzel-Baum (`default_tree_model`)
/// tatsächlich anspricht (Teil C).
///
/// # Beschreibung
/// Dieselbe Auflösung wie [`build_root_model_with_registry_and_resolver`]:
/// das konfigurierte `default_model`, bei einem unbrauchbaren Vorgabe-Paar
/// das erste nutzbare Katalogmodell ([`resolve_default_model`]). Grundlage
/// für das Kontextfenster ungepinnter Kinder — mit dem rohen
/// `default_model` fiel ein solches Kind ohne gesetztes Vorgabemodell auf das
/// konservative Rückfallfenster, obwohl es ein bekanntes Modell ruft.
///
/// # Returns
/// Die Modellkennung oder `None`, wenn kein Modell konfiguriert ist.
#[must_use]
pub(crate) fn effective_default_model_id(config: &ResolvedConfig) -> Option<String> {
    match resolve_default_model(config) {
        DefaultModelResolution::Fallback { model, .. } => Some(model),
        DefaultModelResolution::AsConfigured | DefaultModelResolution::NoneUsable => {
            config.harness.default_model.clone()
        }
    }
}

/// Ergebnis der UIA-Provider/-Modell-Auflösung (siehe [`resolve_uia_model`]).
enum UiaModelResolution {
    /// `uia_provider`/`uia_model` sind nicht beide nutzbar gesetzt — die
    /// UIA-Sitzung nutzt das bereits gebaute Vorgabe-Modell des Laufs
    /// unverändert (kein zweiter HTTP-Client).
    UsesDefault,
    /// `uia_provider` bezeichnet einen vorhandenen, aktivierten Provider und
    /// `uia_model` ist gesetzt — der bereits gebaute Vorgabe-Router
    /// (`default_tree_model`, selbst schon ein
    /// [`harw_provider_http::RoutingModelProvider`] über alle aktivierten
    /// Provider, siehe `harw-provider-http/src/lib.rs`
    /// `build_provider_with_optional_resolver`) wird lediglich in einen
    /// [`UiaDefaultRouteProvider`] gehüllt, der die UIA-Standardroute setzt.
    /// Es wird **kein** zweiter HTTP-Client gebaut.
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
/// Der Provider selbst muss dafür **nicht** neu gebaut werden: der bereits
/// gebaute Vorgabe-Router (`default_tree_model`) enthält bereits einen
/// Backend-Client für jeden aktivierten Provider
/// (`harw-provider-http/src/lib.rs` `build_provider_with_optional_resolver`,
/// ~321-368) und routet pro Anfrage nach `request.provider_id`
/// (`harw-provider-http/src/routing.rs` `RoutingModelProvider::select`,
/// ~70-89). `Explicit` bestimmt hier nur, welche Provider-/Modell-ID als
/// Standardroute gilt, wenn eine Anfrage selbst keine wählt.
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
/// derselbe Router, umhüllt von [`UiaDefaultRouteProvider`], die
/// `uia_provider`/`uia_model` als Standardroute setzt, wenn eine Anfrage
/// selbst keine `provider_id`/`model_id` mitbringt.
///
/// # Errors
/// Kann seit der Vereinheitlichung mit dem Vorgabe-Router nicht mehr
/// fehlschlagen — es wird kein zweiter HTTP-Client gebaut. Die
/// `RuntimeResult`-Signatur bleibt aus Kompatibilitätsgründen für
/// bestehende Aufrufer erhalten.
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
/// Der `Explicit`-Zweig baut seit der Vereinheitlichung mit dem
/// Vorgabe-Router **keinen** zweiten HTTP-Client mehr (siehe
/// [`UiaModelResolution::Explicit`], [`UiaDefaultRouteProvider`]) — `spec`
/// und `resolver` werden hier nicht mehr gebraucht, bleiben aber Teil der
/// Signatur, damit bestehende Aufrufer unverändert bleiben.
///
/// # Arguments
/// Wie [`build_uia_model`], zusätzlich:
/// - `resolver` (`Option<&dyn SecretResolver>`): unbenutzt seit der
///   Vereinheitlichung mit dem Vorgabe-Router; siehe oben.
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
    build_uia_model_with_registry_and_resolver(
        spec,
        config,
        source_is_configured,
        default_tree_model,
        resolver,
    )
    .map(|(model, _registry)| model)
}

/// Wie [`build_uia_model_with_resolver`], liefert zusätzlich die
/// [`ProviderLoadRegistry`] des UIA-Zweigs.
///
/// # Description
/// Seit der Vereinheitlichung mit dem Vorgabe-Router baut **keiner** der
/// drei Zweige (`!source_is_configured`, [`UiaModelResolution::UsesDefault`],
/// [`UiaModelResolution::Explicit`]) mehr einen eigenen HTTP-Client: alle
/// enthaltenen Provider stecken bereits im Vorgabe-Router
/// `default_tree_model` (`harw-provider-http/src/lib.rs`
/// `build_provider_with_optional_resolver`, ~321-368), der pro Anfrage nach
/// `request.provider_id` routet (`harw-provider-http/src/routing.rs`
/// `RoutingModelProvider::select`, ~70-89). `Explicit` hüllt diesen Router
/// nur in [`UiaDefaultRouteProvider`], damit Anfragen ohne eigene Wahl auf
/// `uia_provider`/`uia_model` fallen. Alle drei Zweige liefern deshalb eine
/// **leere** zusätzliche Registry — der bereits gebaute Root-Registry-Eintrag
/// deckt jeden Provider schon ab; ein zweiter, identischer Eintrag würde nur
/// dupliziert (siehe Merge-Logik in [`RuntimeAssemblyBuilder::build`]).
///
/// [`build_uia_model_with_resolver`] delegiert hierher und verwirft die
/// Registry — bestehende Aufrufer sind von dieser Erweiterung nicht
/// betroffen.
///
/// # Errors
/// Wie [`build_uia_model_with_resolver`]: kann seit der Vereinheitlichung
/// nicht mehr fehlschlagen (kein zweiter Client-Bau mehr), die
/// `RuntimeResult`-Signatur bleibt aus Kompatibilitätsgründen erhalten.
pub fn build_uia_model_with_registry_and_resolver(
    _spec: &RuntimeSpec,
    config: &ResolvedConfig,
    source_is_configured: bool,
    default_tree_model: &Arc<dyn ModelProvider>,
    _resolver: Option<&dyn SecretResolver>,
) -> RuntimeResult<(Arc<dyn ModelProvider>, ProviderLoadRegistry)> {
    if !source_is_configured {
        // Echo/Override: kein konfigurierter Root-Provider, also auch keine
        // eigene UIA-Standardroute — `harw-provider-http` bleibt unberührt.
        return Ok((Arc::clone(default_tree_model), ProviderLoadRegistry::new()));
    }

    match resolve_uia_model(config) {
        UiaModelResolution::UsesDefault => {
            Ok((Arc::clone(default_tree_model), ProviderLoadRegistry::new()))
        }
        UiaModelResolution::Explicit { provider, model } => {
            let uia_model: Arc<dyn ModelProvider> = Arc::new(UiaDefaultRouteProvider::new(
                Arc::clone(default_tree_model),
                ProviderId::from(provider),
                ModelId::from(model),
            ));
            Ok((uia_model, ProviderLoadRegistry::new()))
        }
    }
}

/// Umhüllt den Vorgabe-Router (`default_tree_model`) so, dass eine Anfrage
/// ohne eigene `provider_id`/`model_id` auf die UIA-Standardroute fällt,
/// während eine bereits vom Aufrufer gesetzte Wahl unverändert Vorrang
/// behält.
///
/// # Description
/// Existiert, weil [`PinnedModelProvider`] (`harw-core/src/pinned_model.rs`
/// `respond`) `provider_id`/`model_id` **unbedingt** überschreibt, sobald ein
/// Wert gepinnt ist — unabhängig davon, ob der Request bereits einen eigenen
/// Wert trägt. Für den `Explicit`-UIA-Zweig ist das falsch: eine Live-Auswahl
/// über `/uia-model switch`
/// (`harw-tui/src/session_controller.rs` `apply_to_session`, schreibt
/// `provider_id`/`model_id` direkt in den Request) muss weiterhin Vorrang
/// behalten. `UiaDefaultRouteProvider` setzt beide Felder deshalb **nur**,
/// wenn sie im Request noch `None` sind.
struct UiaDefaultRouteProvider {
    /// Der Vorgabe-Router, an den nach dem optionalen Auffüllen delegiert
    /// wird — enthält bereits einen Backend-Client für jeden aktivierten
    /// Provider (siehe [`build_uia_model_with_registry_and_resolver`]).
    inner: Arc<dyn ModelProvider>,
    /// Provider-ID der UIA-Standardroute (`uia_provider`).
    default_provider_id: ProviderId,
    /// Modell-ID der UIA-Standardroute (`uia_model`).
    default_model_id: ModelId,
}

impl UiaDefaultRouteProvider {
    /// Baut den Wrapper um `inner` mit der gegebenen UIA-Standardroute.
    ///
    /// # Arguments
    /// - `inner` (`Arc<dyn ModelProvider>`): der umhüllte Vorgabe-Router.
    /// - `default_provider_id` (`ProviderId`): Standard-Provider, falls der
    ///   Request keinen eigenen trägt.
    /// - `default_model_id` (`ModelId`): Standard-Modell, falls der Request
    ///   keines trägt.
    ///
    /// # Returns
    /// Einen fertig konfigurierten `UiaDefaultRouteProvider`.
    fn new(
        inner: Arc<dyn ModelProvider>,
        default_provider_id: ProviderId,
        default_model_id: ModelId,
    ) -> Self {
        Self {
            inner,
            default_provider_id,
            default_model_id,
        }
    }
}

impl ModelProvider for UiaDefaultRouteProvider {
    /// Füllt `request.provider_id`/`request.model_id` nur, wenn sie noch
    /// `None` sind, und delegiert dann an `inner`.
    ///
    /// # Description
    /// Im Unterschied zu [`PinnedModelProvider`] wird ein bereits gesetzter
    /// Wert **nie** überschrieben — die Live-Auswahl des Aufrufers behält
    /// Vorrang (siehe Struct-Dokumentation).
    ///
    /// # Arguments
    /// - `request` (`ModelRequest`): der Request, dessen fehlende
    ///   `provider_id`/`model_id` mit der UIA-Standardroute aufgefüllt
    ///   werden, bevor er an `inner` weitergereicht wird.
    ///
    /// # Returns
    /// Das [`ModelFuture`] des inneren Routers, unverändert durchgereicht.
    ///
    /// # Concurrency
    /// Hält keine Locks; das zurückgegebene Future läuft vollständig im
    /// inneren Router.
    fn respond<'a>(&'a self, mut request: ModelRequest) -> ModelFuture<'a> {
        if request.provider_id.is_none() {
            request.provider_id = Some(self.default_provider_id.clone());
        }
        if request.model_id.is_none() {
            request.model_id = Some(self.default_model_id.clone());
        }
        self.inner.respond(request)
    }

    /// Reicht die gepinnte Modell-ID des inneren Routers durch.
    ///
    /// # Description
    /// `default_model_id` ist **kein** Pin: es füllt nur fehlende Werte auf,
    /// eine gesetzte `model_id` des Aufrufers hat Vorrang (siehe
    /// [`Self::respond`]). Deshalb wird nicht `default_model_id`, sondern der
    /// Wert des umhüllten Providers gemeldet.
    ///
    /// # Returns
    /// `self.inner.pinned_model_id()`.
    fn pinned_model_id(&self) -> Option<String> {
        self.inner.pinned_model_id()
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
/// `uia-explorer`, `uia-writer`, `uia-shell-worker`, `uia-latex-writer`).
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
/// - `uia_client` (`&Arc<dyn ModelProvider>`): das bereits gebaute
///   UIA-Modell ([`build_uia_model`]) — entweder `default_tree_model`
///   unverändert oder ein [`UiaDefaultRouteProvider`] darum —, auf dem
///   `uia_worker_model` ggf. gepinnt wird.
///
/// Da [`PinnedModelProvider`] hier nur `model_id` pinnt und `provider_id`
/// unangetastet lässt, füllt anschließend `uia_client` selbst — sofern es
/// ein [`UiaDefaultRouteProvider`] ist — eine fehlende `provider_id` mit der
/// UIA-Standardroute auf: eine uia-worker-Anfrage ohne eigene `provider_id`
/// landet damit trotzdem beim UIA-Provider, auch ohne dass diese Funktion
/// selbst die `provider_id` anfasst.
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
    use crate::test_support::{TestError, TestResult, ctx};
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
            approval_override: None,
            model_override: None,
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
                stream: None,
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
                default_reasoning_effort: None,
                gateway_identity_headers: false,
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
                stream: None,
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
                default_reasoning_effort: None,
                gateway_identity_headers: false,
            },
        );
    }

    /// Minimale Katalog-Modell-Fixtur für `config.models`.
    fn model_toml(id: &str, provider: &str) -> harw_config::ModelToml {
        harw_config::ModelToml {
            stream: None,
            rate_limit: None,
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
            default_reasoning_effort: None,
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

    fn respond(provider: &dyn ModelProvider) -> TestResult<ModelResponse> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(ctx("current-thread runtime"))?;
        runtime
            .block_on(provider.respond(empty_request()))
            .map_err(ctx("echo response"))
    }

    #[test]
    fn echo_source_answers_with_its_reply() -> TestResult {
        let spec = spec_for(Path::new("/nonexistent-home"));
        let model = build_root_model(
            &spec,
            &ResolvedConfig::default(),
            ModelSource::Echo("echo: hallo".to_owned()),
        )
        .map_err(ctx("echo provider"))?;

        let response = respond(model.as_ref())?;
        assert_eq!(response.message.as_deref(), Some("echo: hallo"));
        assert!(response.is_final());
        Ok(())
    }

    #[test]
    fn override_source_is_passed_through_unchanged() -> TestResult {
        let spec = spec_for(Path::new("/nonexistent-home"));
        let provided: Arc<dyn ModelProvider> = Arc::new(EchoModelProvider::new("bereitgestellt"));
        let model = build_root_model(
            &spec,
            &ResolvedConfig::default(),
            ModelSource::Override(Arc::clone(&provided)),
        )
        .map_err(ctx("override provider"))?;

        assert!(Arc::ptr_eq(&provided, &model));
        assert_eq!(
            respond(model.as_ref())?.message.as_deref(),
            Some("bereitgestellt")
        );
        Ok(())
    }

    #[test]
    fn configured_source_builds_the_loopback_provider() -> TestResult {
        let spec = spec_for(Path::new("/nonexistent-home"));
        build_root_model(&spec, &loopback_config(), ModelSource::Configured)
            .map_err(ctx("configured provider"))?;
        Ok(())
    }

    /// F-046-style Verhalten: ohne jeden nutzbaren Provider/jedes nutzbare
    /// Modell startet der Lauf trotzdem — der Fehler erscheint erst, wenn
    /// tatsächlich ein Modell-Aufruf versucht wird, nicht beim Bau des
    /// Root-Modells.
    #[test]
    fn configured_source_without_any_usable_provider_starts_and_fails_only_on_a_model_call()
    -> TestResult {
        let spec = spec_for(Path::new("/nonexistent-home"));
        let model = build_root_model(&spec, &ResolvedConfig::default(), ModelSource::Configured)
            .map_err(ctx("a config without any usable provider must still start"))?;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(ctx("current-thread runtime"))?;
        let Err(error) = runtime.block_on(model.respond(empty_request())) else {
            return Err(TestError::Unexpected(
                "a model call must fail clearly when nothing is configured".into(),
            ));
        };
        assert!(matches!(
            error,
            harw_core::ModelError::RequestFailed(ref message)
                if message.contains("no usable model/provider is configured")
        ));
        Ok(())
    }

    /// F-046-Regression: ein hängender `default_provider` darf den Start
    /// nicht abbrechen, solange ein anderer, katalogisierter Provider
    /// tatsächlich nutzbar ist.
    #[test]
    fn configured_source_falls_back_to_first_usable_catalog_model_when_default_is_dangling()
    -> TestResult {
        let spec = spec_for(Path::new("/nonexistent-home"));
        let mut config = loopback_config();
        config.harness.default_provider = Some("gpt-5.6-terra-provider".to_owned());
        config.harness.default_model = Some("gpt-5.6-terra".to_owned());
        config.models.insert(
            "local-model".to_owned(),
            harw_config::ModelToml {
                stream: None,
                rate_limit: None,
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
                default_reasoning_effort: None,
            },
        );

        build_root_model(&spec, &config, ModelSource::Configured).map_err(ctx(
            "a dangling default must fall back to the usable catalog model",
        ))?;
        Ok(())
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
    fn effective_default_model_id_follows_the_fallback_model() -> TestResult {
        let config = loopback_config();
        assert_eq!(
            effective_default_model_id(&config),
            config.harness.default_model.clone()
        );
        let mut dangling = loopback_config();
        dangling.harness.default_model = None;
        dangling.models.insert(
            "catalog-model".to_owned(),
            model_toml("catalog-model", "local"),
        );
        assert_eq!(
            effective_default_model_id(&dangling).as_deref(),
            Some("catalog-model"),
            "ohne Vorgabemodell ruft der Wurzel-Baum das erste nutzbare Katalogmodell"
        );
        Ok(())
    }

    #[test]
    fn resolve_default_model_ignores_a_disabled_default_provider() -> TestResult {
        let mut config = loopback_config();
        config
            .providers
            .get_mut("local")
            .ok_or(TestError::Missing("provider"))?
            .enabled = false;

        assert!(matches!(
            resolve_default_model(&config),
            DefaultModelResolution::NoneUsable
        ));
        Ok(())
    }

    #[test]
    fn configured_source_rejects_a_non_loopback_plaintext_endpoint() -> TestResult {
        let spec = spec_for(Path::new("/nonexistent-home"));
        let mut config = loopback_config();
        config
            .providers
            .get_mut("local")
            .ok_or(TestError::Missing("provider"))?
            .base_url = "http://provider.example/v1".to_owned();

        let Err(error) = build_root_model(&spec, &config, ModelSource::Configured) else {
            return Err(TestError::Unexpected(
                "plaintext http to a remote host must not build a provider".into(),
            ));
        };
        assert!(matches!(error, RuntimeError::Provider { .. }), "{error}");
        Ok(())
    }

    #[test]
    fn file_credentials_outside_home_secrets_fail_closed() -> TestResult {
        let spec = spec_for(Path::new("/nonexistent-home"));
        let mut config = loopback_config();
        let provider = config
            .providers
            .get_mut("local")
            .ok_or(TestError::Missing("provider"))?;
        provider.auth_header = Some("bearer".to_owned());
        provider.auth = Some(harw_config::SecretRef::File("/etc/hostname".to_owned()));

        let Err(error) = build_root_model(&spec, &config, ModelSource::Configured) else {
            return Err(TestError::Unexpected(
                "a file: credential outside <home>/secrets must not resolve".into(),
            ));
        };
        assert!(matches!(error, RuntimeError::Provider { .. }), "{error}");
        assert!(
            !error.to_string().contains("/etc/hostname"),
            "der Fehlertext darf die Referenz nicht im Klartext nennen: {error}"
        );
        Ok(())
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
    fn resolve_uia_model_prefers_the_configured_pair_when_usable() -> TestResult {
        let mut config = loopback_config();
        config.harness.uia_provider = Some("local".to_owned());
        config.harness.uia_model = Some("uia-model".to_owned());

        match resolve_uia_model(&config) {
            UiaModelResolution::Explicit { provider, model } => {
                assert_eq!(provider, "local");
                assert_eq!(model, "uia-model");
                Ok(())
            }
            UiaModelResolution::UsesDefault => Err(TestError::Unexpected(
                "expected an explicit uia pair".into(),
            )),
        }
    }

    #[test]
    fn resolve_uia_model_falls_back_to_default_when_uia_provider_is_disabled() -> TestResult {
        let mut config = loopback_config();
        insert_loopback_provider(&mut config, "uia-only");
        config
            .providers
            .get_mut("uia-only")
            .ok_or(TestError::Missing("provider"))?
            .enabled = false;
        config.harness.uia_provider = Some("uia-only".to_owned());
        config.harness.uia_model = Some("uia-model".to_owned());

        assert!(matches!(
            resolve_uia_model(&config),
            UiaModelResolution::UsesDefault
        ));
        Ok(())
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
    fn build_uia_model_returns_the_default_tree_model_unchanged_for_echo_source() -> TestResult {
        let spec = spec_for(Path::new("/nonexistent-home"));
        let config = loopback_config();
        let default_tree_model: Arc<dyn ModelProvider> =
            Arc::new(EchoModelProvider::new("echo: hallo"));

        let uia_model = build_uia_model(&spec, &config, false, &default_tree_model)
            .map_err(ctx("echo/override path never fails to build"))?;

        assert!(Arc::ptr_eq(&default_tree_model, &uia_model));
        Ok(())
    }

    /// Beweistest: `uia_provider` weicht von `default_provider` ab — der
    /// `Explicit`-Zweig baut **keinen** zweiten Router mehr, sondern hüllt
    /// den bereits gebauten Vorgabe-Router in [`UiaDefaultRouteProvider`].
    /// Beleg: `build_uia_model_with_registry_and_resolver` liefert eine
    /// leere zusätzliche Registry — es gibt nichts Neues zu registrieren,
    /// weil kein zweiter HTTP-Client entstanden ist.
    #[test]
    fn build_uia_model_explicit_branch_wraps_the_default_router_without_building_a_second_one()
    -> TestResult {
        let spec = spec_for(Path::new("/nonexistent-home"));
        let mut config = two_provider_config();
        config.harness.uia_provider = Some("local-b".to_owned());
        config.harness.uia_model = Some("local-b-model".to_owned());

        let default_tree_model = build_root_model(&spec, &config, ModelSource::Configured)
            .map_err(ctx("default provider (local-a) must build"))?;
        let (uia_model, uia_registry) = build_uia_model_with_registry_and_resolver(
            &spec,
            &config,
            true,
            &default_tree_model,
            None,
        )
        .map_err(ctx(
            "explicit uia branch must not fail — no client is built",
        ))?;

        assert!(
            !Arc::ptr_eq(&default_tree_model, &uia_model),
            "the uia model must be a distinct wrapper (UiaDefaultRouteProvider), not the same \
             Arc as the default tree model"
        );
        assert!(
            uia_registry.is_empty(),
            "the explicit uia branch must not build a second HTTP client/registry entry — the \
             default router already registers every enabled provider"
        );
        Ok(())
    }

    /// Beweistest: eine Anfrage ohne eigene `provider_id`/`model_id` über den
    /// UIA-Client landet beim UIA-Provider (`local-b`) — `select()` im
    /// zugrunde liegenden Router (`harw-provider-http/src/routing.rs`) würde
    /// sonst mit "requested model provider '' is not configured" oder
    /// ähnlich scheitern; ein `RequestFailed` mit einer Meldung, die auf
    /// einen fehlenden Provider hindeutet, wäre ein Fehlschlag dieses Tests.
    #[test]
    fn build_uia_model_explicit_branch_routes_default_requests_to_the_uia_provider() -> TestResult {
        let spec = spec_for(Path::new("/nonexistent-home"));
        let mut config = two_provider_config();
        config.harness.uia_provider = Some("local-b".to_owned());
        config.harness.uia_model = Some("local-b-model".to_owned());

        let default_tree_model = build_root_model(&spec, &config, ModelSource::Configured)
            .map_err(ctx("default provider (local-a) must build"))?;
        let uia_model = build_uia_model(&spec, &config, true, &default_tree_model)
            .map_err(ctx("explicit uia branch must not fail"))?;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(ctx("current-thread runtime"))?;
        // Kein eigener provider_id/model_id im Request — muss auf die
        // UIA-Standardroute (local-b) fallen, nicht auf local-a und nicht
        // auf einen unbekannten Provider.
        let result = runtime.block_on(uia_model.respond(empty_request()));
        if let Err(error) = result {
            let message = error.to_string();
            assert!(
                !message.contains("is not configured"),
                "a request without its own provider_id must route to the uia default provider \
                 'local-b', which is registered in the default router: {message}"
            );
        }
        Ok(())
    }

    /// Beweistest: eine Anfrage mit **eigener** `provider_id` wird respektiert
    /// — `UiaDefaultRouteProvider` darf eine bereits gesetzte Live-Auswahl
    /// (`/uia-model switch`, `SessionController::apply_to_session`) nicht
    /// überschreiben, im Unterschied zu [`PinnedModelProvider`].
    #[test]
    fn build_uia_model_explicit_branch_respects_an_already_set_provider_id() -> TestResult {
        let spec = spec_for(Path::new("/nonexistent-home"));
        let mut config = two_provider_config();
        config.harness.uia_provider = Some("local-b".to_owned());
        config.harness.uia_model = Some("local-b-model".to_owned());

        let default_tree_model = build_root_model(&spec, &config, ModelSource::Configured)
            .map_err(ctx("default provider (local-a) must build"))?;
        let uia_model = build_uia_model(&spec, &config, true, &default_tree_model)
            .map_err(ctx("explicit uia branch must not fail"))?;

        // `enable_all`: der unerreichbare Loopback-Provider liefert einen
        // Transportfehler, den der `RetryingProvider` mit Backoff wiederholt.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(ctx("current-thread runtime"))?;
        // Live-Auswahl auf "local-a" gesetzt — die UIA-Standardroute
        // ("local-b") darf das nicht überschreiben.
        let request = empty_request().with_provider_id(Some(ProviderId::from("local-a")));
        let result = runtime.block_on(uia_model.respond(request));
        if let Err(error) = result {
            let message = error.to_string();
            assert!(
                !message.contains("is not configured"),
                "an explicitly chosen provider_id ('local-a') must be respected and routed, not \
                 overwritten by the uia default route: {message}"
            );
        }
        Ok(())
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
    fn build_uia_worker_model_pins_only_model_id_never_provider_id() -> TestResult {
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
            .map_err(ctx("current-thread runtime"))?;
        let caller_provider_id = ProviderId::from("whatever-the-caller-already-set");
        let request = empty_request().with_provider_id(Some(caller_provider_id.clone()));
        runtime
            .block_on(worker_model.respond(request))
            .map_err(ctx("recording provider never fails"))?;

        let recorded = recorder
            .last()
            .ok_or(TestError::Missing("request must have been forwarded"))?;
        assert_eq!(recorded.model_id, Some(ModelId::from("local-model")));
        assert_eq!(
            recorded.provider_id,
            Some(caller_provider_id),
            "provider_id must pass through untouched — only model_id may be pinned"
        );
        Ok(())
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
