//! Live-Modellwechsel in der laufenden Montage.
//!
//! # Ursache des alten Fehlers („nach dem Modellwechsel muss harw neu
//! starten")
//! `/model switch` schrieb die Wahl nur in den Sitzungs-Controller (die
//! Wurzelsitzung übernimmt sie an der nächsten Turn-Grenze) und als
//! Vorgabe in die Profil-Config. Alles, was die Montage beim Start daraus
//! abgeleitet hatte, blieb aber beim alten Stand:
//! - die Kind-Fabrik des Wurzel-Baums (`crate::children`) gab ungepinnten
//!   Kindern — also u. a. dem Orchestrator, der die eigentliche Arbeit
//!   macht — das beim Start ermittelte Vorgabemodell als `active_model`
//!   und keinen Provider (Router-Vorgabe des Starts);
//! - die Rollenwahl (`/models set …`) wurde nur gespeichert;
//! - ein Provider, dessen Client beim Start nicht gebaut werden konnte
//!   (z. B. `secrets:`-Schlüssel, deren Speicher nur für den
//!   Vorgabe-Provider geöffnet wird), blieb bis zum Neustart ein
//!   Fehler-Backend.
//!
//! # Neue Regel
//! [`LiveModelRouting`] hält die Live-Wahl der Montage:
//! - [`LiveModelRouting::set_live_main`] — die generische Auswahl des
//!   Controllers, von der TUI an jeder Turn-Grenze gemeldet. Neu gestartete
//!   ungepinnte Kinder sprechen sie über einen [`PinnedModelProvider`] an;
//!   laufende behalten ihr Modell.
//! - [`LiveModelControl::set_internal_model`] — die Rollenwahl für neu
//!   gestartete Kinder (Orchestrator, Worker, Explorer, Recherche, …).
//! - [`LiveModelControl::ensure_provider_ready`] — baut einen nicht
//!   verfügbaren Provider-Client über den regulären Provider-Bau
//!   ([`harw_provider_http::build_named_backend`]) neu und setzt ihn in den
//!   Router ein; scheitert das, lehnt die Wechsel-Operation ab und das alte
//!   Modell bleibt aktiv.
//!
//! # Lebenszyklus der Provider-Angebote
//! - Eine **explizite** Wahl ([`LiveModelRouting::set_live_main`],
//!   [`LiveModelControl::set_internal_model`]) eines abgeschalteten
//!   (retired) Modells wird abgelehnt (fail-closed, Zustand unverändert,
//!   nie ein stiller Austausch); ein abgekündigtes (deprecated) Modell wird
//!   mit Warnung zugelassen.
//! - Die Auflösung einer Kind-Stelle
//!   ([`LiveModelRouting::resolved_internal_model`]) unterscheidet nach
//!   [`InternalModelSource`]: eine **explizite** Wahl gilt nach derselben
//!   Regel wie beim Setzen (Retirement-Fenster und abgekündigt → Warnung, die
//!   Route bleibt; erst abgeschaltet → Rückfall auf das Hauptmodell, mit
//!   Ersatzhinweis im Log), eine **implizit** aufgelöste Stelle fällt schon
//!   im Retirement-Fenster auf das Hauptmodell zurück.
//! - [`check_explicit_selection`] ist die gemeinsame Prüfung einer expliziten
//!   Wahl für Aufrufer **außerhalb** der Live-Wahl: die TUI prüft damit die
//!   Route des Controllers an der Turn-Grenze, **bevor** sie in die
//!   Wurzelsitzung geschrieben wird (die Wechsel-Operationen in `harw-ops`
//!   lehnen ein abgeschaltetes Modell schon vor der Controller-Mutation ab).
//!
//! # Nebenläufigkeit
//! Zustand hinter `RwLock`; ein Neubau ist durch eine eigene Sperre
//! serialisiert. Eine vergiftete Sperre gilt als „keine Live-Wahl"
//! (Rückfall auf den Stand des Starts), nie als Panik.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use harw_config::{
    InternalModelChoice, InternalModelPoint, InternalModelSource, InternalModelsToml,
    ResolvedConfig, ResolvedInternalModel, catalog_provider_of,
};
use harw_core::{ModelProvider, PinnedModelProvider};
use harw_model_catalog::lifecycle;
pub use harw_model_catalog::lifecycle::RetiredModelError;
use harw_ops::live_model::LiveModelControl;
use harw_provider_http::{RoutingBackends, SecretResolver};
use harw_types::{ModelId, ProviderId};

/// Geteilter `secrets:`-Resolver der Montage.
pub type SharedSecretResolver = Arc<dyn SecretResolver + Send + Sync>;

/// Öffnet bei Bedarf den versiegelten Speicher für einen Provider-Neubau
/// (von der Composition Root gesetzt, z. B. `harw-cli`).
pub type SecretResolverOpener =
    dyn Fn(&ResolvedConfig) -> Result<Option<SharedSecretResolver>, String> + Send + Sync;

/// Live-Auswahl des Hauptmodells (Provider optional, Modell gesetzt).
#[derive(Debug, Clone, PartialEq, Eq)]
struct LiveMainRoute {
    provider: Option<String>,
    model: String,
}

/// Veränderlicher Teil von [`LiveModelRouting`].
#[derive(Debug)]
struct LiveState {
    /// Zuletzt gemeldete Controller-Auswahl; `None` → Stand des Starts.
    main: Option<LiveMainRoute>,
    /// Aktuelle `[internal_models]`-Wahl (Start + Live-Änderungen).
    internal_models: InternalModelsToml,
    /// Daraus aufgelöste Kind-Modellstellen
    /// ([`crate::children::resolve_internal_models_for_children`]).
    resolved: HashMap<InternalModelPoint, ResolvedInternalModel>,
    /// Route (Provider oder leer, Modell), für die zuletzt eine
    /// Abkündigungswarnung gemeldet wurde; ändert sich die Route (oder
    /// entfällt die Abkündigung), wird der Merker zurückgesetzt. Hält die
    /// Warnung bei der Prüfung an jeder Turn-Grenze auf einmal je Route.
    warned_route: Option<(String, String)>,
}

/// Live-Modellwahl einer Montage (siehe Moduldoku).
pub struct LiveModelRouting {
    /// Konfiguration des Starts (Katalog, Provider, Vorgaben).
    config: Arc<ResolvedConfig>,
    state: RwLock<LiveState>,
    /// Backend-Tabelle des Vorgabe-Routers; `None` ohne HTTP-Router
    /// (Echo/Override) — dann gibt es nichts neu zu bauen.
    backends: Option<Arc<RoutingBackends>>,
    /// harw-Home für `file:`-Referenzen beim Neubau.
    home: Option<PathBuf>,
    /// Beim Start geöffneter `secrets:`-Resolver (falls vorhanden).
    resolver: Option<SharedSecretResolver>,
    /// Öffnet den Resolver nachträglich, wenn beim Start keiner nötig war.
    resolver_opener: Option<Arc<SecretResolverOpener>>,
    /// Serialisiert Neubauten desselben oder verschiedener Provider.
    rebuild: Mutex<()>,
}

impl std::fmt::Debug for LiveModelRouting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let main = self.state.read().ok().and_then(|state| state.main.clone());
        f.debug_struct("LiveModelRouting")
            .field("main", &main)
            .field("backends", &self.backends.is_some())
            .field("resolver", &self.resolver.is_some())
            .finish_non_exhaustive()
    }
}

impl LiveModelRouting {
    /// Baut die Live-Wahl aus der Konfiguration des Starts.
    #[must_use]
    pub fn new(config: Arc<ResolvedConfig>) -> Self {
        let internal_models = config.harness.internal_models.clone();
        let resolved = crate::children::resolve_internal_models_for_children(&config);
        Self {
            config,
            state: RwLock::new(LiveState {
                main: None,
                internal_models,
                resolved,
                warned_route: None,
            }),
            backends: None,
            home: None,
            resolver: None,
            resolver_opener: None,
            rebuild: Mutex::new(()),
        }
    }

    /// Ergänzt die Backend-Tabelle des Vorgabe-Routers und das harw-Home
    /// für einen Provider-Neubau.
    #[must_use]
    pub fn with_backends(mut self, backends: Option<Arc<RoutingBackends>>, home: PathBuf) -> Self {
        self.backends = backends;
        self.home = Some(home);
        self
    }

    /// Ergänzt den beim Start geöffneten Resolver und den nachträglichen
    /// Öffner.
    #[must_use]
    pub fn with_secret_resolvers(
        mut self,
        resolver: Option<SharedSecretResolver>,
        opener: Option<Arc<SecretResolverOpener>>,
    ) -> Self {
        self.resolver = resolver;
        self.resolver_opener = opener;
        self
    }

    /// Merkt sich die generische Controller-Auswahl.
    ///
    /// # Beschreibung
    /// Die TUI ruft das an jeder Turn-Grenze auf (ein laufender Turn behält
    /// sein Modell). Ohne Modell gilt wieder der Stand des Starts. Wirkt nur
    /// auf **neu** gestartete Kinder.
    ///
    /// # Lebenszyklus
    /// Fail-closed für abgeschaltete (retired) Modelle: Provider (genannt,
    /// sonst Katalog, sonst Vorgabe-Provider; jeweils auf den kanonischen
    /// Namen normalisiert) und Modell-ID werden gegen den Lebenszyklus
    /// geprüft, bevor die Wahl gespeichert wird; ein abgeschaltetes Modell
    /// wird abgelehnt und der bisherige Stand bleibt unverändert. Die Prüfung
    /// läuft an jeder Turn-Grenze auch für eine **unveränderte** Wahl: wurde
    /// ein vorher abgekündigtes Modell inzwischen abgeschaltet, wird die
    /// Wahl abgelehnt und die Live-Wahl entfernt (neue Kinder fallen auf den
    /// Stand des Starts zurück). Dasselbe gilt, wenn eine **andere** Wahl
    /// abgelehnt wird, während die installierte Wahl inzwischen selbst
    /// abgeschaltet ist; eine noch gültige installierte Wahl bleibt. Ein abgekündigtes (deprecated) Modell wird
    /// mit einer Warnung im Log zugelassen — einmal je Route, nicht bei jeder
    /// Turn-Grenze. Das Zurücksetzen (`model: None`) schlägt nie fehl.
    ///
    /// # Errors
    /// [`RetiredModelError`], wenn das gewählte Modell abgeschaltet ist; sie
    /// nennt die vom Provider empfohlenen Ersatzmodelle.
    pub fn set_live_main(
        &self,
        provider: Option<String>,
        model: Option<String>,
    ) -> Result<(), RetiredModelError> {
        let warning = self
            .select_live_main_at(provider, model, lifecycle::OffsetDateTime::now_utc())
            .inspect_err(|error| {
                tracing::warn!(%error, "runtime.live_model.main_refused");
            })?;
        if let Some(warning) = warning {
            tracing::warn!(%warning, "runtime.live_model.deprecated");
        }
        Ok(())
    }

    /// Kern von [`Self::set_live_main`] mit injizierter Uhr `now`.
    ///
    /// # Rückgabe
    /// `Ok(Some(warnung))` nur, wenn für diese Route noch keine
    /// Abkündigungswarnung gemeldet wurde (der Aufrufer protokolliert sie);
    /// sonst `Ok(None)`.
    ///
    /// # Errors
    /// [`RetiredModelError`], wenn das Angebot zu `now` abgeschaltet ist. Eine
    /// geänderte Wahl lässt eine noch gültige installierte Wahl unverändert;
    /// eine abgelehnte Wahl räumt auch eine installierte Wahl ab, die zu `now`
    /// selbst abgeschaltet ist (die abgelehnte Wahl selbst, falls sie schon
    /// installiert war, eingeschlossen).
    fn select_live_main_at(
        &self,
        provider: Option<String>,
        model: Option<String>,
        now: lifecycle::OffsetDateTime,
    ) -> Result<Option<String>, RetiredModelError> {
        let route = model
            .filter(|model| !model.trim().is_empty())
            .map(|model| LiveMainRoute {
                provider: provider.filter(|provider| !provider.trim().is_empty()),
                model,
            });
        let Some(route) = route else {
            if let Ok(mut state) = self.state.write() {
                state.main = None;
                state.warned_route = None;
            }
            return Ok(None);
        };
        let warning = match check_explicit_selection_at(
            &self.config,
            route.provider.as_deref(),
            &route.model,
            now,
        ) {
            Ok(warning) => warning,
            Err(error) => {
                // Fail-closed: eine installierte Wahl, die inzwischen
                // abgeschaltet wurde — die abgelehnte selbst (unverändert
                // gemeldet) oder eine frühere, die ein Wechsel nicht ersetzt —,
                // darf für neue Kinder nicht weitergelten. Eine noch gültige
                // installierte Wahl bleibt als letzter guter Stand.
                if let Ok(mut state) = self.state.write() {
                    let stale = state.main.as_ref().is_some_and(|installed| {
                        installed == &route
                            || check_explicit_selection_at(
                                &self.config,
                                installed.provider.as_deref(),
                                &installed.model,
                                now,
                            )
                            .is_err()
                    });
                    if stale {
                        state.main = None;
                        state.warned_route = None;
                    }
                }
                return Err(error);
            }
        };
        let key = (
            route.provider.clone().unwrap_or_default(),
            route.model.clone(),
        );
        let fresh = match self.state.write() {
            Ok(mut state) => {
                let fresh = warning.is_some() && state.warned_route.as_ref() != Some(&key);
                state.warned_route = warning.is_some().then_some(key);
                state.main = Some(route);
                fresh
            }
            // Vergiftete Sperre: nichts wird gespeichert, also auch nichts
            // dedupliziert.
            Err(_) => warning.is_some(),
        };
        Ok(warning.filter(|_| fresh))
    }

    /// Provider/Modell, das ein jetzt gestartetes ungepinntes Kind anspricht;
    /// `None` → Stand des Starts.
    #[must_use]
    pub fn live_main(&self) -> Option<(Option<String>, String)> {
        let live = self
            .state
            .read()
            .ok()
            .and_then(|state| state.main.clone())?;
        let provider = live
            .provider
            .or_else(|| catalog_provider_of(&self.config, &live.model));
        Some((provider, live.model))
    }

    /// Raw explicit image choice. Unlike child routing this never substitutes a
    /// retired selection with a different provider or model.
    pub fn image_model_choice(&self) -> Result<Option<InternalModelChoice>, String> {
        self.state
            .read()
            .map(|state| state.internal_models.image_understanding.clone())
            .map_err(|_| "image model routing lock unavailable".to_owned())
    }

    /// Das Modell für ein neu gestartetes Kind auf dem Hauptmodell.
    ///
    /// # Rückgabe
    /// Ohne Live-Wahl `base` unverändert, sonst ein [`PinnedModelProvider`]
    /// um `base`, der Provider und Modell der Live-Wahl fest anspricht.
    #[must_use]
    pub fn main_model_provider(&self, base: &Arc<dyn ModelProvider>) -> Arc<dyn ModelProvider> {
        match self.live_main() {
            Some((provider, model)) => Arc::new(PinnedModelProvider::new(
                Arc::clone(base),
                provider.map(ProviderId::from),
                Some(ModelId::from(model)),
            )),
            None => Arc::clone(base),
        }
    }

    /// Aktuelle Auflösung einer Kind-Modellstelle.
    ///
    /// # Lebenszyklus
    /// Die Entscheidung trifft `internal_route_decision` zur aktuellen Uhr
    /// und richtet sich nach [`ResolvedInternalModel::source`]: eine
    /// **explizite** Wahl folgt der Regel des Setzens
    /// ([`lifecycle::enforce_explicit`]) — abgekündigt oder im
    /// Retirement-Fenster bleibt sie (mit Warnung im Log), erst nach dem
    /// Retirement fällt sie zurück —, jede andere Herkunft nur dann, wenn sie
    /// implizit wählbar ist ([`lifecycle::implicitly_selectable_bundled`]).
    ///
    /// # Rückgabe
    /// `None` für eine unbekannte Stelle — und für eine Stelle, die auf das
    /// Hauptmodell zurückfällt: der Aufrufer nutzt dann das Hauptmodell,
    /// statt still ein Modell anzusprechen, das der Provider abschaltet.
    #[must_use]
    pub fn resolved_internal_model(
        &self,
        point: InternalModelPoint,
    ) -> Option<ResolvedInternalModel> {
        let resolved = self
            .state
            .read()
            .ok()
            .and_then(|state| state.resolved.get(&point).cloned())?;
        let now = lifecycle::OffsetDateTime::now_utc();
        match internal_route_decision(&self.config, &resolved, now) {
            InternalRouteDecision::Keep { warning } => {
                if let Some(warning) = warning {
                    tracing::warn!(
                        point = point.key(),
                        %warning,
                        "runtime.live_model.deprecated"
                    );
                }
                Some(resolved)
            }
            InternalRouteDecision::FallBack { reason } => {
                tracing::warn!(
                    point = point.key(),
                    %reason,
                    "runtime.live_model.internal_route_fallback"
                );
                None
            }
        }
    }

    /// Erzwingt den Lebenszyklus einer **expliziten** Wahl.
    ///
    /// # Beschreibung
    /// Prüft über [`check_explicit_selection`]; abgekündigt (deprecated) →
    /// Warnung im Log, die Wahl gilt; ohne bekannten Provider wird nichts
    /// geprüft.
    ///
    /// # Errors
    /// [`RetiredModelError`], wenn das Angebot abgeschaltet ist.
    fn enforce_lifecycle(
        &self,
        provider: Option<&str>,
        model: &str,
    ) -> Result<(), RetiredModelError> {
        if let Some(warning) = check_explicit_selection(&self.config, provider, model)? {
            tracing::warn!(%warning, model, "runtime.live_model.deprecated");
        }
        Ok(())
    }

    /// Konfiguration mit dem Default-Modell, das der Router tatsächlich
    /// nutzt (Rückfall wie beim Start).
    fn build_config(&self) -> std::borrow::Cow<'_, ResolvedConfig> {
        if self.config.harness.default_model.is_some() {
            return std::borrow::Cow::Borrowed(self.config.as_ref());
        }
        let mut config = (*self.config).clone();
        config.harness.default_model = crate::model::effective_default_model_id(&config);
        std::borrow::Cow::Owned(config)
    }

    /// Baut das Backend von `provider` über den regulären Provider-Bau.
    fn build_backend(
        &self,
        provider: &str,
        resolver: Option<&SharedSecretResolver>,
    ) -> Result<Box<dyn ModelProvider>, String> {
        let config = self.build_config();
        harw_provider_http::build_named_backend(
            &config,
            provider,
            self.home.as_deref(),
            resolver.map(|resolver| &**resolver as &dyn SecretResolver),
        )
        .map(|(backend, _load_control)| backend)
        .map_err(|error| error.to_string())
    }
}

impl LiveModelControl for LiveModelRouting {
    fn ensure_provider_ready(&self, provider: &str) -> Result<(), String> {
        let Some(backends) = &self.backends else {
            return Ok(());
        };
        if backends.unavailable_reason(provider).is_none() {
            return Ok(());
        }
        let _guard = self
            .rebuild
            .lock()
            .map_err(|_| "provider rebuild lock is poisoned".to_owned())?;
        // Ein paralleler Wechsel kann das Backend inzwischen gebaut haben.
        if backends.unavailable_reason(provider).is_none() {
            return Ok(());
        }
        let first = match self.build_backend(provider, self.resolver.as_ref()) {
            Ok(backend) => {
                return install(backends, provider, backend);
            }
            Err(error) => error,
        };
        // Beim Start war kein `secrets:`-Resolver nötig (er wird nur für den
        // Vorgabe-Provider geöffnet) — jetzt nachträglich öffnen und erneut
        // bauen.
        let opened = match (&self.resolver, &self.resolver_opener) {
            (None, Some(opener)) => opener(&self.config)?,
            _ => None,
        };
        let Some(resolver) = opened else {
            return Err(first);
        };
        let backend = self.build_backend(provider, Some(&resolver))?;
        install(backends, provider, backend)
    }

    fn set_internal_model(
        &self,
        point: InternalModelPoint,
        choice: Option<InternalModelChoice>,
    ) -> bool {
        // Eine explizite Wahl eines abgeschalteten Modells wird abgelehnt
        // (Zustand unverändert); abgekündigt → nur Warnung. Geroutet wird mit
        // dem genannten, sonst dem Vorgabe-Provider (wie bei der Auflösung).
        if let Some(InternalModelChoice {
            provider,
            model: Some(model),
        }) = &choice
        {
            let provider = provider
                .as_deref()
                .or(self.config.harness.default_provider.as_deref());
            if let Err(error) = self.enforce_lifecycle(provider, model) {
                tracing::warn!(
                    point = point.key(),
                    %error,
                    "runtime.live_model.internal_model_refused"
                );
                return false;
            }
        }
        let Ok(mut state) = self.state.write() else {
            return false;
        };
        state.internal_models.set_choice(point, choice);
        let mut config = (*self.config).clone();
        config.harness.internal_models = state.internal_models.clone();
        state.resolved = crate::children::resolve_internal_models_for_children(&config);
        let live = state.resolved.contains_key(&point);
        tracing::info!(
            point = point.key(),
            live,
            "runtime.live_model.internal_model"
        );
        live
    }
}

/// Provider, gegen dessen Lebenszyklus eine Wahl geprüft wird: der genannte,
/// sonst der des Katalogs, sonst der Vorgabe-Provider der Konfiguration;
/// `None`, wenn keiner bekannt ist (dann gibt es nichts zu prüfen).
///
/// Jede Quelle kann ein Schlüssel der Provider-Tabelle statt des `name` sein
/// (`[providers.mistral-key] name = "mistral"`); die Lebenszyklus-Tabelle
/// vergleicht aber exakt mit dem Anbieternamen. Darum wird das Ergebnis auf
/// den kanonischen Namen normalisiert ([`canonical_provider_name`]) — sonst
/// umginge ein abweichender Schlüssel die Prüfung eines abgeschalteten
/// Modells.
fn lifecycle_provider_in(
    config: &ResolvedConfig,
    provider: Option<&str>,
    model: &str,
) -> Option<String> {
    provider
        .map(str::to_owned)
        .or_else(|| catalog_provider_of(config, model))
        .or_else(|| config.harness.default_provider.clone())
        .map(|name| canonical_provider_name(config, &name).to_owned())
}

/// Provider und Modell-ID des Angebots, gegen das der Lebenszyklus geprüft
/// wird; `None`, wenn kein Provider bekannt ist (nichts zu prüfen).
///
/// Mit **genanntem** Provider wird die Modell-ID providergenau aufgelöst
/// ([`resolve_model_id_for_provider`]); ohne genannten Provider bleibt es bei
/// der globalen Auflösung ([`resolve_model_id`]) — dort stammt der Provider
/// aus demselben Katalogeintrag.
fn lifecycle_offering(
    config: &ResolvedConfig,
    provider: Option<&str>,
    model: &str,
) -> Option<(String, String)> {
    let offering_provider = lifecycle_provider_in(config, provider, model)?;
    let model_id = match provider {
        Some(named) => resolve_model_id_for_provider(config, named, model),
        None => resolve_model_id(config, model),
    };
    Some((offering_provider, model_id))
}

/// Normalisiert einen Provider-Verweis (Schlüssel oder `name` eines
/// konfigurierten Providers) auf den kanonischen Provider-Namen; ein
/// unbekannter Verweis bleibt unverändert.
///
/// Spiegel von `canonical_provider_name` in `harw-ops` (dort nicht
/// öffentlich), damit Laufzeit und Operationen dieselben Providergrenzen
/// ziehen.
fn canonical_provider_name<'a>(config: &'a ResolvedConfig, name: &'a str) -> &'a str {
    config
        .providers
        .iter()
        .find(|(key, provider)| key.as_str() == name || provider.name == name)
        .map_or(name, |(_, provider)| provider.name.as_str())
}

/// Gehört der Katalogeintrag `entry` zum Provider `provider` (Schlüssel oder
/// `name`, beide Seiten kanonisiert)?
fn model_belongs_to(
    config: &ResolvedConfig,
    entry: &harw_config::ModelToml,
    provider: &str,
) -> bool {
    canonical_provider_name(config, &entry.provider) == canonical_provider_name(config, provider)
}

/// Löst eine Modellkennung (Katalog-Schlüssel, `id` oder Alias) **innerhalb
/// des Providers `provider`** zur Modell-ID des Katalogs auf.
///
/// # Beschreibung
/// Rangfolge, jeweils nur unter den Katalogmodellen von `provider`:
/// Katalog-Schlüssel, dann `id`, dann Alias. Die Auflösung hängt nicht von der
/// Iterationsreihenfolge der Konfiguration ab: derselbe Alias bei zwei
/// Providern liefert je Provider dessen eigene ID; benennt ein Alias bei
/// einem Provider mehrere Modelle (mehrdeutige Konfiguration, von `harw-ops`
/// beim Setzen abgelehnt), gilt die kleinste Modell-ID. Eine Kennung, die
/// bei `provider` weder Schlüssel noch `id` noch Alias ist, bleibt
/// unverändert (frei wählbare Modell-ID) — auch wenn sie Schlüssel, `id`
/// oder Alias eines Modells eines **anderen** Providers ist: sonst würde aus
/// `provider-a/<kennung>` still das Modell von Provider B und der
/// Lebenszyklus-Abgleich liefe gegen die falsche ID.
fn resolve_model_id_for_provider(config: &ResolvedConfig, provider: &str, model: &str) -> String {
    if let Some(entry) = config
        .models
        .get(model)
        .filter(|entry| model_belongs_to(config, entry, provider))
    {
        return entry.id.clone();
    }
    let own: Vec<&harw_config::ModelToml> = config
        .models
        .values()
        .filter(|entry| model_belongs_to(config, entry, provider))
        .collect();
    if own.iter().any(|entry| entry.id == model) {
        return model.to_owned();
    }
    own.iter()
        .filter(|entry| entry.aliases.iter().any(|alias| alias == model))
        .map(|entry| entry.id.as_str())
        .min()
        .map_or_else(|| model.to_owned(), str::to_owned)
}

/// Prüft eine **explizite** Provider-/Modellwahl gegen den Lebenszyklus der
/// Provider-Angebote (zur aktuellen Uhrzeit).
///
/// # Beschreibung
/// Löst Provider (genannt, sonst Katalog, sonst Vorgabe-Provider; stets als
/// kanonischer Name, nie als Schlüssel der Provider-Tabelle) und
/// Modell-ID (Katalog-Schlüssel, `id` oder Alias) wie die Live-Wahl auf
/// ([`LiveModelRouting::set_live_main`]) und prüft mit
/// [`lifecycle::enforce_explicit`]. Bei genanntem Provider wird
/// **providergenau** aufgelöst (nur dessen Katalogmodelle, siehe
/// `resolve_model_id_for_provider`), sodass ein Alias eines anderen Providers
/// die Wahl nie auf dessen Modell umschreibt. Reine Prüfung ohne Zustand: wer sie vor
/// einer Mutation aufruft, ändert bei einem Fehler nichts. Ein Modell wird
/// nie ersetzt.
///
/// # Rückgabe
/// `Ok(Some(warnung))` für ein abgekündigtes (deprecated) Modell — die Wahl
/// gilt, der Aufrufer meldet die Warnung —, sonst `Ok(None)`; auch dann, wenn
/// kein Provider bekannt ist (nichts zu prüfen).
///
/// # Errors
/// [`RetiredModelError`], wenn das Angebot abgeschaltet (retired) ist; sie
/// nennt die vom Provider empfohlenen Ersatzmodelle.
pub fn check_explicit_selection(
    config: &ResolvedConfig,
    provider: Option<&str>,
    model: &str,
) -> Result<Option<String>, RetiredModelError> {
    check_explicit_selection_at(
        config,
        provider,
        model,
        lifecycle::OffsetDateTime::now_utc(),
    )
}

/// Wie [`check_explicit_selection`], aber zum übergebenen Zeitpunkt `now`
/// (die Uhr ist injizierbar; kein Zugriff auf die Systemzeit).
///
/// # Errors
/// [`RetiredModelError`], wenn das Angebot zu `now` abgeschaltet ist.
fn check_explicit_selection_at(
    config: &ResolvedConfig,
    provider: Option<&str>,
    model: &str,
    now: lifecycle::OffsetDateTime,
) -> Result<Option<String>, RetiredModelError> {
    let Some((provider, model_id)) = lifecycle_offering(config, provider, model) else {
        return Ok(None);
    };
    lifecycle::enforce_explicit(&provider, &model_id, now)
}

/// Entscheidung über die Route einer aufgelösten Kind-Stelle.
#[derive(Debug, Clone, PartialEq, Eq)]
enum InternalRouteDecision {
    /// Die Route gilt; `warning` meldet ein abgekündigtes Angebot (samt
    /// Ersatzhinweis), das weiter nutzbar bleibt.
    Keep {
        /// Warnung für das Log, falls das Angebot abgekündigt ist.
        warning: Option<String>,
    },
    /// Rückfall auf das Hauptmodell; `reason` begründet das im Log.
    FallBack {
        /// Begründung samt Ersatzhinweis, soweit bekannt.
        reason: String,
    },
}

/// Entscheidet zum Zeitpunkt `now`, ob die Route einer aufgelösten
/// Kind-Stelle gilt oder auf das Hauptmodell zurückfällt.
///
/// # Beschreibung
/// Rein und ohne Uhr: der Zeitpunkt wird übergeben (die Produktion reicht
/// `OffsetDateTime::now_utc()` durch, Tests einen festen Wert). Die Regel
/// hängt an [`ResolvedInternalModel::source`]:
/// - [`InternalModelSource::Explicit`]: dieselbe Prüfung wie beim Setzen der
///   Wahl ([`check_explicit_selection_at`]). Abgekündigt oder im
///   Retirement-Fenster → die Route bleibt, mit Warnung; erst abgeschaltet →
///   Rückfall, die Begründung nennt den empfohlenen Ersatz. Eine vom Setter
///   angenommene Wahl wird so nie vor dem Retirement still verworfen.
/// - Jede andere Herkunft: nur bei
///   [`lifecycle::implicitly_selectable_bundled`] (nicht im
///   Retirement-Fenster, nicht abgeschaltet), sonst Rückfall.
///
/// Eine Stelle ohne Modell (Hauptmodell) oder ohne bekannten Provider hat
/// nichts zu prüfen und bleibt.
fn internal_route_decision(
    config: &ResolvedConfig,
    resolved: &ResolvedInternalModel,
    now: lifecycle::OffsetDateTime,
) -> InternalRouteDecision {
    let keep = InternalRouteDecision::Keep { warning: None };
    let Some(model) = resolved.model.as_deref() else {
        return keep;
    };
    match resolved.source {
        InternalModelSource::Explicit => {
            match check_explicit_selection_at(config, resolved.provider.as_deref(), model, now) {
                Ok(warning) => InternalRouteDecision::Keep { warning },
                Err(retired) => InternalRouteDecision::FallBack {
                    reason: retired.to_string(),
                },
            }
        }
        InternalModelSource::OpenRouterDefault | InternalModelSource::MainModel => {
            let Some((provider, model_id)) =
                lifecycle_offering(config, resolved.provider.as_deref(), model)
            else {
                return keep;
            };
            if lifecycle::implicitly_selectable_bundled(&provider, &model_id, now) {
                keep
            } else {
                InternalRouteDecision::FallBack {
                    reason: format!(
                        "model {model_id} of provider {provider} is retired or in its retirement window"
                    ),
                }
            }
        }
    }
}

/// Löst eine Modellkennung (Katalog-Schlüssel, `id` oder Alias) zur
/// Modell-ID des Katalogs auf; eine unbekannte Kennung bleibt unverändert.
///
/// # Beschreibung
/// Grundlage der Modellanzeige (`<provider>/<modell>`): angezeigt wird die
/// Modell-ID, die tatsächlich angefragt wird, nie ein Alias. Die Auflösung ist
/// global (ohne Providerbindung); die Lebenszyklus-Prüfung einer Wahl mit
/// bekanntem Provider löst providergenau auf.
#[must_use]
pub fn resolve_model_id(config: &ResolvedConfig, model: &str) -> String {
    config
        .models
        .get(model)
        .or_else(|| {
            config
                .models
                .values()
                .find(|entry| entry.id == model || entry.aliases.iter().any(|alias| alias == model))
        })
        .map_or_else(|| model.to_owned(), |entry| entry.id.clone())
}

/// Setzt ein neu gebautes Backend in den Router ein.
fn install(
    backends: &RoutingBackends,
    provider: &str,
    backend: Box<dyn ModelProvider>,
) -> Result<(), String> {
    backends
        .replace(provider, backend)
        .map_err(|error| error.to_string())?;
    tracing::info!(provider, "runtime.live_model.provider_rebuilt");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_config::{HarnessConfig, OriginAllowlistToml, ProviderToml, SecretRef};
    use harw_core::{ModelError, ModelFuture, ModelRequest};
    use harw_extension_api::types::LoadedInstructions;
    use harw_provider_http::RoutingModelProvider;
    use std::collections::BTreeMap;

    /// Merkt sich Provider/Modell jeder Anfrage und scheitert dann.
    #[derive(Default)]
    struct Recorder {
        seen: Mutex<Vec<(Option<String>, Option<String>)>>,
    }

    impl ModelProvider for Recorder {
        fn respond<'a>(&'a self, request: ModelRequest) -> ModelFuture<'a> {
            if let Ok(mut seen) = self.seen.lock() {
                seen.push((
                    request.provider_id.map(|id| id.as_str().to_owned()),
                    request.model_id.map(|id| id.as_str().to_owned()),
                ));
            }
            Box::pin(async { Err(ModelError::RequestFailed("recorder".to_owned())) })
        }
    }

    fn request() -> ModelRequest {
        ModelRequest::new(
            LoadedInstructions::default(),
            Vec::new(),
            harw_core::ConversationHistory::new(),
            Vec::new(),
        )
    }

    fn ask(provider: &dyn ModelProvider) -> TestResult<Result<(), ModelError>> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(|error| TestError::Unexpected(error.to_string()))?;
        Ok(runtime.block_on(provider.respond(request())).map(|_| ()))
    }

    fn last(recorder: &Recorder) -> Option<(Option<String>, Option<String>)> {
        recorder
            .seen
            .lock()
            .ok()
            .and_then(|seen| seen.last().cloned())
    }

    fn loopback_provider(name: &str, auth: Option<SecretRef>) -> ProviderToml {
        ProviderToml {
            stream: None,
            name: name.to_owned(),
            api: "openai-chat".to_owned(),
            base_url: "http://127.0.0.1:11434/v1".to_owned(),
            auth_header: if auth.is_some() {
                None
            } else {
                Some("none".to_owned())
            },
            auth,
            api_key: None,
            headers: std::collections::HashMap::new(),
            models: vec![format!("{name}-model")],
            enabled: true,
            origin_allowlist: OriginAllowlistToml::default(),
            rate_limit: None,
            max_concurrency: None,
            originator: None,
            default_reasoning_effort: None,
            gateway_identity_headers: false,
            request_timeout_secs: None,
            stream_idle_timeout_secs: None,
            retry_timeouts: None,
            max_tokens_field: None,
            send_reasoning_effort: None,
            strict_tools: None,
            parallel_tool_calls: None,
            allow_insecure_lan: false,
        }
    }

    /// Vorgabe `local` ohne Zugangsdaten, zweiter Provider `sealed` mit
    /// `secrets:`-Referenz (beim Start ohne Resolver nicht baubar).
    fn sealed_config() -> ResolvedConfig {
        let mut config = ResolvedConfig {
            harness: HarnessConfig {
                default_provider: Some("local".to_owned()),
                default_model: Some("local-model".to_owned()),
                ..HarnessConfig::default()
            },
            ..ResolvedConfig::default()
        };
        config
            .providers
            .insert("local".to_owned(), loopback_provider("local", None));
        config.providers.insert(
            "sealed".to_owned(),
            loopback_provider("sealed", Some(SecretRef::Secrets("sealed-key".to_owned()))),
        );
        config
    }

    struct FakeResolver;

    impl SecretResolver for FakeResolver {
        fn resolve(&self, _reference: &str) -> Result<secrecy::SecretString, String> {
            Ok(secrecy::SecretString::from("sealed-secret".to_owned()))
        }
    }

    /// Ohne Live-Wahl bleibt ein neues Kind beim Start-Client; nach einem
    /// Wechsel auf einen anderen Provider spricht ein **neues** Kind Provider
    /// und Modell der Wahl an, ein vorher gestartetes behält seins.
    #[test]
    fn new_children_follow_a_live_switch_running_ones_keep_their_model() -> TestResult {
        let recorder = Arc::new(Recorder::default());
        let base: Arc<dyn ModelProvider> = recorder.clone();
        let routing = LiveModelRouting::new(Arc::new(ResolvedConfig::default()));

        let before = routing.main_model_provider(&base);
        assert!(Arc::ptr_eq(&before, &base), "ohne Wahl kein Wrapper");

        routing
            .set_live_main(Some("openai".to_owned()), Some("gpt-5".to_owned()))
            .map_err(ctx("set live main"))?;
        let after = routing.main_model_provider(&base);
        let _ = ask(after.as_ref())?;
        assert_eq!(
            last(&recorder),
            Some((Some("openai".to_owned()), Some("gpt-5".to_owned())))
        );
        assert_eq!(after.pinned_model_id().as_deref(), Some("gpt-5"));

        routing
            .set_live_main(Some("anthropic".to_owned()), Some("claude-x".to_owned()))
            .map_err(ctx("set live main"))?;
        let _ = ask(after.as_ref())?;
        assert_eq!(
            last(&recorder),
            Some((Some("openai".to_owned()), Some("gpt-5".to_owned()))),
            "ein laufendes Kind behält sein Modell"
        );

        routing
            .set_live_main(None, None)
            .map_err(ctx("set live main"))?;
        assert!(routing.live_main().is_none());
        Ok(())
    }

    /// Eine Live-Wahl ohne Provider übernimmt den Provider aus dem Katalog.
    #[test]
    fn live_main_without_provider_uses_the_catalog_provider() -> TestResult {
        let mut config = ResolvedConfig::default();
        config.models.insert(
            "gpt-5".to_owned(),
            harw_config::ModelToml {
                stream: None,
                rate_limit: None,
                id: "gpt-5".to_owned(),
                name: None,
                provider: "openai".to_owned(),
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
        let routing = LiveModelRouting::new(Arc::new(config));
        routing
            .set_live_main(None, Some("gpt-5".to_owned()))
            .map_err(ctx("set live main"))?;
        assert_eq!(
            routing.live_main(),
            Some((Some("openai".to_owned()), "gpt-5".to_owned()))
        );
        Ok(())
    }

    /// `/models set orchestrator …` wirkt für neue Kinder sofort; eine Stelle
    /// außerhalb des Kind-Baums (Sitzungstitel) meldet `false`.
    #[test]
    fn role_choice_applies_live_for_child_points_only() {
        let routing = LiveModelRouting::new(Arc::new(ResolvedConfig::default()));
        let live = routing.set_internal_model(
            InternalModelPoint::RootOrchestrator,
            Some(InternalModelChoice {
                provider: Some("openai".to_owned()),
                model: Some("gpt-5".to_owned()),
            }),
        );
        assert!(live);
        let resolved = routing.resolved_internal_model(InternalModelPoint::RootOrchestrator);
        assert_eq!(
            resolved
                .as_ref()
                .and_then(|resolved| resolved.model.as_deref()),
            Some("gpt-5")
        );
        assert_eq!(
            resolved
                .as_ref()
                .and_then(|resolved| resolved.provider.as_deref()),
            Some("openai")
        );

        assert!(routing.set_internal_model(InternalModelPoint::RootOrchestrator, None));
        assert!(
            routing
                .resolved_internal_model(InternalModelPoint::RootOrchestrator)
                .is_some_and(|resolved| resolved.is_main_model())
        );
        assert!(!routing.set_internal_model(
            InternalModelPoint::SessionTitle,
            Some(InternalModelChoice {
                provider: None,
                model: Some("m".to_owned()),
            }),
        ));
    }

    /// Ein beim Start nicht baubarer Provider (kein `secrets:`-Resolver)
    /// wird beim Wechsel mit dem nachträglich geöffneten Resolver gebaut und
    /// ersetzt das Fehler-Backend; die nächste Anfrage erreicht ihn.
    #[test]
    fn switch_rebuilds_a_provider_that_was_unavailable_at_startup() -> TestResult {
        let config = sealed_config();
        let (router, _) = harw_provider_http::build_routing_provider_with_load_registry_and_home(
            &config,
            std::path::Path::new("/nonexistent-home"),
            None,
        )
        .map_err(|error| TestError::Unexpected(error.to_string()))?;
        let backends = router.backends();
        assert!(backends.unavailable_reason("sealed").is_some());
        assert!(backends.unavailable_reason("local").is_none());

        let opener: Arc<SecretResolverOpener> = Arc::new(
            |_config: &ResolvedConfig| -> Result<Option<SharedSecretResolver>, String> {
                let resolver: SharedSecretResolver = Arc::new(FakeResolver);
                Ok(Some(resolver))
            },
        );
        let routing = LiveModelRouting::new(Arc::new(config))
            .with_backends(Some(backends.clone()), PathBuf::from("/nonexistent-home"))
            .with_secret_resolvers(None, Some(opener));
        routing
            .ensure_provider_ready("sealed")
            .map_err(TestError::Unexpected)?;
        assert!(backends.unavailable_reason("sealed").is_none());
        // Bereits nutzbar: kein erneuter Bau, kein Fehler.
        routing
            .ensure_provider_ready("local")
            .map_err(TestError::Unexpected)?;
        Ok(())
    }

    /// Scheitert der Neubau, meldet der Dienst den Grund und das
    /// Fehler-Backend bleibt stehen (die Operation lehnt dann ab).
    #[test]
    fn failed_rebuild_reports_the_reason_and_keeps_the_error_backend() -> TestResult {
        let config = sealed_config();
        let (router, _) = harw_provider_http::build_routing_provider_with_load_registry_and_home(
            &config,
            std::path::Path::new("/nonexistent-home"),
            None,
        )
        .map_err(|error| TestError::Unexpected(error.to_string()))?;
        let backends = router.backends();
        let routing = LiveModelRouting::new(Arc::new(config))
            .with_backends(Some(backends.clone()), PathBuf::from("/nonexistent-home"));
        let Err(reason) = routing.ensure_provider_ready("sealed") else {
            return Err(TestError::Unexpected(
                "ohne Resolver darf der Neubau nicht gelingen".to_owned(),
            ));
        };
        assert!(!reason.is_empty());
        assert!(backends.unavailable_reason("sealed").is_some());
        assert!(routing.ensure_provider_ready("unknown").is_err());
        Ok(())
    }

    /// Router-Ebene: vor dem Einsetzen lehnt das Fehler-Backend mit seinem
    /// Grund ab, danach erreicht die nächste Anfrage das neue Backend.
    #[test]
    fn replaced_backend_serves_the_next_request() -> TestResult {
        let recorder = Arc::new(Recorder::default());
        let mut providers: BTreeMap<String, Box<dyn ModelProvider>> = BTreeMap::new();
        providers.insert(
            "a".to_owned(),
            Box::new(PinnedModelProvider::new(recorder.clone(), None, None)),
        );
        let mut unavailable = BTreeMap::new();
        unavailable.insert("b".to_owned(), "missing credential".to_owned());
        let router = Arc::new(
            RoutingModelProvider::with_unavailable(providers, unavailable, "a")
                .map_err(|error| TestError::Unexpected(error.to_string()))?,
        );
        let backends = router.backends();
        let router: Arc<dyn ModelProvider> = router;
        let to_b = PinnedModelProvider::new(
            router,
            Some(ProviderId::from("b")),
            Some(ModelId::from("b-model")),
        );
        let Err(ModelError::RequestFailed(reason)) = ask(&to_b)? else {
            return Err(TestError::Unexpected(
                "das Fehler-Backend muss ablehnen".to_owned(),
            ));
        };
        assert!(reason.contains("missing credential"));
        assert!(last(&recorder).is_none());

        backends
            .replace(
                "b",
                Box::new(PinnedModelProvider::new(recorder.clone(), None, None)),
            )
            .map_err(|error| TestError::Unexpected(error.to_string()))?;
        assert!(backends.unavailable_reason("b").is_none());
        let _ = ask(&to_b)?;
        assert_eq!(
            last(&recorder),
            Some((Some("b".to_owned()), Some("b-model".to_owned())))
        );
        Ok(())
    }

    /// Ein in der eingebetteten Lebenszyklus-Tabelle abgeschaltetes Angebot
    /// (Mistral OCR 4.0, retired 2026-09-30 — für die echte Uhr sicher in der
    /// Vergangenheit).
    const RETIRED_PROVIDER: &str = "mistral";
    const RETIRED_MODEL: &str = "mistral-ocr-4-0";

    fn retired_choice() -> InternalModelChoice {
        InternalModelChoice {
            provider: Some(RETIRED_PROVIDER.to_owned()),
            model: Some(RETIRED_MODEL.to_owned()),
        }
    }

    /// Eine explizite Wahl eines abgeschalteten Modells wird mit dem Ersatz
    /// abgelehnt und ändert nichts; das Zurücksetzen schlägt nie fehl.
    #[test]
    fn explicit_retired_model_is_refused_with_replacement() -> TestResult {
        let routing = LiveModelRouting::new(Arc::new(ResolvedConfig::default()));
        let Err(error) = routing.set_live_main(
            Some(RETIRED_PROVIDER.to_owned()),
            Some(RETIRED_MODEL.to_owned()),
        ) else {
            return Err(TestError::Unexpected(
                "ein abgeschaltetes Modell darf nicht gewählt werden".to_owned(),
            ));
        };
        assert_eq!(error.provider, RETIRED_PROVIDER);
        assert_eq!(error.model, RETIRED_MODEL);
        assert!(
            error.to_string().contains("mistral-ocr-4-1"),
            "die Meldung nennt den Ersatz: {error}"
        );
        assert!(routing.live_main().is_none());

        // Eine bestehende Wahl bleibt bei einer abgelehnten Wahl erhalten.
        routing
            .set_live_main(Some("openai".to_owned()), Some("gpt-5".to_owned()))
            .map_err(|error| TestError::Unexpected(error.to_string()))?;
        assert!(
            routing
                .set_live_main(
                    Some(RETIRED_PROVIDER.to_owned()),
                    Some(RETIRED_MODEL.to_owned())
                )
                .is_err()
        );
        assert_eq!(
            routing.live_main(),
            Some((Some("openai".to_owned()), "gpt-5".to_owned()))
        );

        routing
            .set_live_main(None, None)
            .map_err(|error| TestError::Unexpected(error.to_string()))?;
        assert!(routing.live_main().is_none());
        Ok(())
    }

    /// Ohne Provider und mit einem Alias wird über den Katalog auf Provider
    /// und Modell-ID aufgelöst, bevor der Lebenszyklus geprüft wird.
    #[test]
    fn retired_model_behind_a_catalog_alias_is_refused() -> TestResult {
        let mut config = ResolvedConfig::default();
        config.models.insert(
            "ocr".to_owned(),
            harw_config::ModelToml {
                stream: None,
                rate_limit: None,
                id: RETIRED_MODEL.to_owned(),
                name: None,
                provider: RETIRED_PROVIDER.to_owned(),
                aliases: vec!["legacy-ocr".to_owned()],
                context_window: None,
                max_tokens: None,
                prompt_caching: None,
                reasoning: false,
                input_types: Vec::new(),
                capabilities: harw_config::ModelCapabilitiesToml::default(),
                default_reasoning_effort: None,
            },
        );
        let routing = LiveModelRouting::new(Arc::new(config));
        let Err(error) = routing.set_live_main(None, Some("legacy-ocr".to_owned())) else {
            return Err(TestError::Unexpected(
                "der Alias eines abgeschalteten Modells muss abgelehnt werden".to_owned(),
            ));
        };
        assert_eq!(error.provider, RETIRED_PROVIDER);
        assert_eq!(error.model, RETIRED_MODEL);
        assert!(routing.live_main().is_none());
        Ok(())
    }

    /// Ein abgekündigtes, aber noch nicht abgeschaltetes Modell (Claude
    /// Mythos Preview, Retirement nicht angekündigt) wird zugelassen.
    #[test]
    fn deprecated_model_is_accepted() -> TestResult {
        let routing = LiveModelRouting::new(Arc::new(ResolvedConfig::default()));
        routing
            .set_live_main(
                Some("anthropic".to_owned()),
                Some("claude-mythos-preview".to_owned()),
            )
            .map_err(|error| TestError::Unexpected(error.to_string()))?;
        assert_eq!(
            routing.live_main(),
            Some((
                Some("anthropic".to_owned()),
                "claude-mythos-preview".to_owned()
            ))
        );
        Ok(())
    }

    /// Die gemeinsame Prüfung einer expliziten Wahl (für die TUI an der
    /// Turn-Grenze): abgeschaltet → Fehler mit Ersatz, abgekündigt → Warnung,
    /// aktiv oder unbekannt → `None`; ohne Provider wird nichts geprüft.
    #[test]
    fn check_explicit_selection_reports_retired_deprecated_and_active() -> TestResult {
        let config = ResolvedConfig::default();

        let Err(error) = check_explicit_selection(&config, Some(RETIRED_PROVIDER), RETIRED_MODEL)
        else {
            return Err(TestError::Unexpected(
                "ein abgeschaltetes Modell muss abgelehnt werden".to_owned(),
            ));
        };
        assert!(error.to_string().contains("mistral-ocr-4-1"), "{error}");

        let warning = check_explicit_selection(&config, Some("anthropic"), "claude-mythos-preview")
            .map_err(ctx("abgekündigt bleibt wählbar"))?
            .ok_or(TestError::Missing("Warnung für das abgekündigte Modell"))?;
        assert!(warning.contains("claude-mythos-5"), "{warning}");

        assert_eq!(
            check_explicit_selection(&config, Some("openai"), "gpt-5")
                .map_err(ctx("unbekanntes Angebot ist erlaubt"))?,
            None
        );
        // Ohne genannten Provider, ohne Katalog und ohne Vorgabe-Provider gibt
        // es nichts zu prüfen.
        assert_eq!(
            check_explicit_selection(&config, None, RETIRED_MODEL)
                .map_err(ctx("ohne Provider nichts zu prüfen"))?,
            None
        );
        Ok(())
    }

    /// Eine Rollenwahl auf ein abgeschaltetes Modell meldet `false` und lässt
    /// die Stelle unverändert (weiter auf dem Hauptmodell).
    #[test]
    fn explicit_retired_role_choice_is_refused_and_changes_nothing() -> TestResult {
        let routing = LiveModelRouting::new(Arc::new(ResolvedConfig::default()));
        assert!(
            !routing
                .set_internal_model(InternalModelPoint::RootOrchestrator, Some(retired_choice()))
        );
        let resolved = routing
            .resolved_internal_model(InternalModelPoint::RootOrchestrator)
            .ok_or(TestError::Missing("Stelle des Wurzel-Orchestrators"))?;
        assert!(resolved.is_main_model());
        Ok(())
    }

    /// Eine beim Start aufgelöste Stelle mit abgeschaltetem Modell liefert
    /// keine Auflösung (Rückfall auf das Hauptmodell); eine andere Stelle
    /// mit nutzbarem Modell bleibt unberührt. (Echte Uhr: Mistral OCR 4.0 ist
    /// seit 2026-09-30 abgeschaltet, die Wahl ist hier explizit.)
    #[test]
    fn implicit_route_skips_retired_model() -> TestResult {
        let mut config = ResolvedConfig::default();
        config
            .harness
            .internal_models
            .set_choice(InternalModelPoint::RootOrchestrator, Some(retired_choice()));
        config.harness.internal_models.set_choice(
            InternalModelPoint::Explorer,
            Some(InternalModelChoice {
                provider: Some("openai".to_owned()),
                model: Some("gpt-5".to_owned()),
            }),
        );
        let routing = LiveModelRouting::new(Arc::new(config));
        assert!(
            routing
                .resolved_internal_model(InternalModelPoint::RootOrchestrator)
                .is_none()
        );
        let explorer = routing
            .resolved_internal_model(InternalModelPoint::Explorer)
            .ok_or(TestError::Missing("Stelle des Explorers"))?;
        assert_eq!(explorer.model.as_deref(), Some("gpt-5"));
        Ok(())
    }

    /// Das gebündelte Mistral-Angebot `zai-glm-5-2`: Retirement zum
    /// Kalenderdatum 2026-10-31, also Fenster ab 2026-10-30 10:00 UTC und
    /// abgeschaltet ab 2026-10-31 12:00 UTC; Ersatz `zai-glm-5-3`.
    const WINDOW_PROVIDER: &str = "mistral";
    const WINDOW_MODEL: &str = "zai-glm-5-2";

    fn window_route(source: InternalModelSource) -> ResolvedInternalModel {
        ResolvedInternalModel {
            point: InternalModelPoint::RootOrchestrator,
            provider: Some(WINDOW_PROVIDER.to_owned()),
            model: Some(WINDOW_MODEL.to_owned()),
            source,
        }
    }

    /// Injizierte Uhr: voller Zeitpunkt in UTC (Monat 1–12).
    fn utc(year: i32, month: u8, day: u8, hour: u8) -> TestResult<lifecycle::OffsetDateTime> {
        let month = time::Month::try_from(month).map_err(ctx("gültiger Testmonat"))?;
        let date =
            time::Date::from_calendar_date(year, month, day).map_err(ctx("gültiges Testdatum"))?;
        let moment = date.with_hms(hour, 0, 0).map_err(ctx("gültige Testzeit"))?;
        Ok(moment.assume_utc())
    }

    fn fall_back_reason(decision: InternalRouteDecision) -> TestResult<String> {
        match decision {
            InternalRouteDecision::FallBack { reason } => Ok(reason),
            InternalRouteDecision::Keep { warning } => Err(TestError::Unexpected(format!(
                "Rückfall auf das Hauptmodell erwartet, die Route blieb (Warnung: {warning:?})"
            ))),
        }
    }

    /// Eine explizite Wahl bleibt im Retirement-Fenster (mit Warnung und
    /// Ersatzhinweis): der Setter hat sie angenommen, sie darf nicht still
    /// zum Hauptmodell werden. Geprüft an Fensteranfang, Mitte und Ende.
    #[test]
    fn explicit_route_is_kept_inside_the_retirement_window() -> TestResult {
        let config = ResolvedConfig::default();
        let route = window_route(InternalModelSource::Explicit);
        for now in [
            utc(2026, 10, 30, 10)?,
            utc(2026, 10, 30, 12)?,
            utc(2026, 10, 31, 11)?,
        ] {
            let InternalRouteDecision::Keep { warning } =
                internal_route_decision(&config, &route, now)
            else {
                return Err(TestError::Unexpected(format!(
                    "explizite Wahl muss im Fenster bleiben ({now})"
                )));
            };
            let warning = warning.ok_or(TestError::Missing("Warnung im Retirement-Fenster"))?;
            assert!(warning.contains("zai-glm-5-3"), "{warning}");
        }
        Ok(())
    }

    /// Dieselbe Stelle mit impliziter Herkunft wird im Retirement-Fenster
    /// übersprungen (Rückfall auf das Hauptmodell).
    #[test]
    fn implicit_route_is_skipped_inside_the_retirement_window() -> TestResult {
        let config = ResolvedConfig::default();
        for source in [
            InternalModelSource::OpenRouterDefault,
            InternalModelSource::MainModel,
        ] {
            let route = window_route(source);
            for now in [
                utc(2026, 10, 30, 10)?,
                utc(2026, 10, 30, 12)?,
                utc(2026, 10, 31, 11)?,
            ] {
                let reason = fall_back_reason(internal_route_decision(&config, &route, now))?;
                assert!(reason.contains(WINDOW_MODEL), "{reason}");
            }
        }
        Ok(())
    }

    /// Vor dem Fenster (2026-10-30 09:00 UTC) bleiben beide Herkünfte ohne
    /// Warnung.
    #[test]
    fn both_sources_are_kept_before_the_retirement_window() -> TestResult {
        let config = ResolvedConfig::default();
        let now = utc(2026, 10, 30, 9)?;
        for source in [
            InternalModelSource::Explicit,
            InternalModelSource::OpenRouterDefault,
        ] {
            assert_eq!(
                internal_route_decision(&config, &window_route(source), now),
                InternalRouteDecision::Keep { warning: None }
            );
        }
        Ok(())
    }

    /// Nach dem Retirement (ab 2026-10-31 12:00 UTC) fällt auch die explizite
    /// Wahl zurück; die Begründung nennt den Ersatz. Eine Stunde vorher gilt
    /// sie noch.
    #[test]
    fn explicit_route_falls_back_after_retirement() -> TestResult {
        let config = ResolvedConfig::default();
        let route = window_route(InternalModelSource::Explicit);

        assert!(matches!(
            internal_route_decision(&config, &route, utc(2026, 10, 31, 11)?),
            InternalRouteDecision::Keep { warning: Some(_) }
        ));
        for now in [utc(2026, 10, 31, 12)?, utc(2026, 11, 1, 0)?] {
            let reason = fall_back_reason(internal_route_decision(&config, &route, now))?;
            assert!(reason.contains(WINDOW_MODEL), "{reason}");
            assert!(reason.contains("zai-glm-5-3"), "{reason}");
        }
        Ok(())
    }

    /// Nach dem Retirement fällt auch die implizite Herkunft zurück.
    #[test]
    fn implicit_route_falls_back_after_retirement() -> TestResult {
        let config = ResolvedConfig::default();
        let route = window_route(InternalModelSource::OpenRouterDefault);
        let reason = fall_back_reason(internal_route_decision(
            &config,
            &route,
            utc(2026, 11, 1, 0)?,
        ))?;
        assert!(reason.contains(WINDOW_MODEL), "{reason}");
        Ok(())
    }

    /// Ein unbekanntes Angebot und eine Stelle ohne Modell bleiben zu jeder
    /// Zeit unberührt.
    #[test]
    fn unknown_offering_and_main_model_routes_are_always_kept() -> TestResult {
        let config = ResolvedConfig::default();
        let now = utc(2026, 11, 1, 0)?;
        let unknown = ResolvedInternalModel {
            point: InternalModelPoint::Explorer,
            provider: Some("openai".to_owned()),
            model: Some("gpt-5".to_owned()),
            source: InternalModelSource::Explicit,
        };
        let main = ResolvedInternalModel {
            point: InternalModelPoint::Explorer,
            provider: None,
            model: None,
            source: InternalModelSource::MainModel,
        };
        for route in [unknown, main] {
            assert_eq!(
                internal_route_decision(&config, &route, now),
                InternalRouteDecision::Keep { warning: None }
            );
        }
        Ok(())
    }

    /// Katalogeintrag für die providergenauen Auflösungstests.
    fn model_toml(id: &str, provider: &str, aliases: &[&str]) -> harw_config::ModelToml {
        harw_config::ModelToml {
            stream: None,
            rate_limit: None,
            id: id.to_owned(),
            name: None,
            provider: provider.to_owned(),
            aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
            context_window: None,
            max_tokens: None,
            prompt_caching: None,
            reasoning: false,
            input_types: Vec::new(),
            capabilities: harw_config::ModelCapabilitiesToml::default(),
            default_reasoning_effort: None,
        }
    }

    /// Derselbe Alias bei zwei Providern liefert je Provider dessen eigene
    /// ID, unabhängig von der Iterationsreihenfolge der Konfiguration; ein
    /// Provider-Verweis über Schlüssel oder `name` zieht dieselbe Grenze.
    #[test]
    fn same_alias_under_two_providers_resolves_per_provider() {
        let mut config = ResolvedConfig::default();
        config.models.insert(
            "a-key".to_owned(),
            model_toml("a-model", "prov-a", &["shared"]),
        );
        config.models.insert(
            "b-key".to_owned(),
            model_toml("b-model", "prov-b", &["shared"]),
        );
        assert_eq!(
            resolve_model_id_for_provider(&config, "prov-a", "shared"),
            "a-model"
        );
        assert_eq!(
            resolve_model_id_for_provider(&config, "prov-b", "shared"),
            "b-model"
        );
        // Katalog-Schlüssel und reine `id` des eigenen Providers.
        assert_eq!(
            resolve_model_id_for_provider(&config, "prov-a", "a-key"),
            "a-model"
        );
        assert_eq!(
            resolve_model_id_for_provider(&config, "prov-a", "a-model"),
            "a-model"
        );
    }

    /// Provider-Schlüssel und Provider-`name` gelten als derselbe Provider.
    #[test]
    fn provider_reference_by_key_or_name_is_one_provider() {
        let mut config = ResolvedConfig::default();
        config
            .providers
            .insert("mistral-key".to_owned(), loopback_provider("mistral", None));
        config.models.insert(
            "ocr".to_owned(),
            model_toml("mistral-ocr-4-0", "mistral-key", &["legacy-ocr"]),
        );
        for provider in ["mistral", "mistral-key"] {
            assert_eq!(
                resolve_model_id_for_provider(&config, provider, "legacy-ocr"),
                "mistral-ocr-4-0",
                "{provider}"
            );
        }
        assert_eq!(
            resolve_model_id_for_provider(&config, "other", "legacy-ocr"),
            "legacy-ocr"
        );
    }

    /// Eine Kennung, die nur bei einem anderen Provider Alias oder Schlüssel
    /// ist, bleibt unverändert (frei wählbare Modell-ID); die globale
    /// Auflösung der Anzeige bleibt davon unberührt.
    #[test]
    fn an_id_equal_to_another_providers_alias_is_not_remapped() {
        let mut config = ResolvedConfig::default();
        config.models.insert(
            "b-key".to_owned(),
            model_toml("b-model", "prov-b", &["shared"]),
        );
        assert_eq!(
            resolve_model_id_for_provider(&config, "prov-a", "shared"),
            "shared"
        );
        assert_eq!(
            resolve_model_id_for_provider(&config, "prov-a", "b-key"),
            "b-key"
        );
        assert_eq!(
            resolve_model_id_for_provider(&config, "prov-a", "b-model"),
            "b-model"
        );
        assert_eq!(resolve_model_id(&config, "shared"), "b-model");
    }

    /// Der Alias eines anderen Providers darf eine Wahl nicht auf dessen
    /// (abgeschaltetes) Modell umschreiben: `openai/my-gpt` bleibt eine freie
    /// ID und ist erlaubt, obwohl `my-gpt` bei einem Gateway auf
    /// `gpt-4o-2024-08-06` zeigt; gehört der Alias OpenAI selbst, wird er
    /// aufgelöst und die Wahl nach dem Retirement abgelehnt.
    #[test]
    fn check_explicit_selection_does_not_remap_another_providers_alias() -> TestResult {
        let after_retirement = utc(2027, 1, 15, 0)?;

        let mut foreign = ResolvedConfig::default();
        foreign.models.insert(
            "gw-gpt4o".to_owned(),
            model_toml("gpt-4o-2024-08-06", "gateway", &["my-gpt"]),
        );
        assert_eq!(
            check_explicit_selection_at(&foreign, Some("openai"), "my-gpt", after_retirement)
                .map_err(ctx("fremder Alias ist eine freie ID"))?,
            None
        );

        let mut own = ResolvedConfig::default();
        own.models.insert(
            "oa-gpt4o".to_owned(),
            model_toml("gpt-4o-2024-08-06", "openai", &["my-gpt"]),
        );
        let Err(error) =
            check_explicit_selection_at(&own, Some("openai"), "my-gpt", after_retirement)
        else {
            return Err(TestError::Unexpected(
                "der eigene Alias eines abgeschalteten Modells muss abgelehnt werden".to_owned(),
            ));
        };
        assert_eq!(error.provider, "openai");
        assert_eq!(error.model, "gpt-4o-2024-08-06");
        assert!(error.to_string().contains("gpt-6-luna"), "{error}");
        Ok(())
    }

    /// Gebündeltes, abgeschaltetes Mistral-Angebot ohne Ersatz (retired
    /// 2026-09-30) für die Tests der Provider-Normalisierung.
    const KEYED_MODEL: &str = "labs-leanstral-1-5";

    /// Provider-Tabelle mit abweichendem Schlüssel
    /// (`[providers.mistral-key] name = "mistral"`); optional mit einem
    /// Katalogeintrag für [`KEYED_MODEL`] und einem Vorgabe-Provider.
    fn keyed_config(with_catalog: bool, default_provider: Option<&str>) -> ResolvedConfig {
        let mut config = ResolvedConfig {
            harness: HarnessConfig {
                default_provider: default_provider.map(str::to_owned),
                ..HarnessConfig::default()
            },
            ..ResolvedConfig::default()
        };
        config
            .providers
            .insert("mistral-key".to_owned(), loopback_provider("mistral", None));
        if with_catalog {
            config.models.insert(
                "leanstral".to_owned(),
                model_toml(KEYED_MODEL, "mistral-key", &[]),
            );
        }
        config
    }

    /// Erwartet eine Ablehnung wegen Retirement und liefert den Fehler.
    fn refused_with(
        result: Result<Option<String>, RetiredModelError>,
    ) -> TestResult<RetiredModelError> {
        match result {
            Err(error) => Ok(error),
            Ok(warning) => Err(TestError::Unexpected(format!(
                "ein abgeschaltetes Modell wurde angenommen (Warnung: {warning:?})"
            ))),
        }
    }

    /// Der Lebenszyklus-Provider ist für jede Quelle (genannt, Katalog,
    /// Vorgabe) der kanonische Name, nie der Schlüssel der Provider-Tabelle;
    /// ein unbekannter Verweis bleibt unverändert.
    #[test]
    fn lifecycle_provider_is_the_canonical_name_for_every_source() {
        let config = keyed_config(true, Some("mistral-key"));
        for named in ["mistral-key", "mistral"] {
            assert_eq!(
                lifecycle_provider_in(&config, Some(named), KEYED_MODEL).as_deref(),
                Some("mistral"),
                "genannt: {named}"
            );
        }
        assert_eq!(
            lifecycle_provider_in(&config, None, KEYED_MODEL).as_deref(),
            Some("mistral"),
            "Katalog"
        );
        assert_eq!(
            lifecycle_provider_in(&config, None, "free-form").as_deref(),
            Some("mistral"),
            "Vorgabe-Provider"
        );
        assert_eq!(
            lifecycle_provider_in(&config, Some("other"), KEYED_MODEL).as_deref(),
            Some("other")
        );
        assert_eq!(
            lifecycle_provider_in(&ResolvedConfig::default(), None, "free-form"),
            None
        );
    }

    /// Ein abgeschaltetes Modell wird abgelehnt, egal ob der Provider als
    /// Schlüssel, als Name, über den Katalogeintrag oder über den
    /// Vorgabe-Provider (Schlüssel oder Name) benannt wird — in der
    /// Live-Wahl und in der gemeinsamen Prüfung der TUI-Turn-Grenze.
    #[test]
    fn retired_model_is_refused_however_the_provider_is_named() -> TestResult {
        let after = utc(2026, 10, 15, 0)?;
        let cases = [
            (keyed_config(true, None), Some("mistral-key")),
            (keyed_config(true, None), Some("mistral")),
            (keyed_config(true, None), None),
            (keyed_config(false, Some("mistral-key")), None),
            (keyed_config(false, Some("mistral")), None),
        ];
        for (config, named) in cases {
            let checked = check_explicit_selection_at(&config, named, KEYED_MODEL, after);
            let error = refused_with(checked)?;
            assert_eq!(error.provider, "mistral", "{named:?}");
            assert_eq!(error.model, KEYED_MODEL);

            let routing = LiveModelRouting::new(Arc::new(config));
            let selected = routing.select_live_main_at(
                named.map(str::to_owned),
                Some(KEYED_MODEL.to_owned()),
                after,
            );
            let error = refused_with(selected)?;
            assert_eq!(error.provider, "mistral", "{named:?}");
            assert!(routing.live_main().is_none(), "{named:?}");
        }
        Ok(())
    }

    /// Der öffentliche Pfad (echte Uhr; das Angebot ist seit 2026-09-30
    /// abgeschaltet) lehnt das Modell über Schlüssel und Namen gleich ab.
    #[test]
    fn set_live_main_refuses_a_retired_model_named_by_provider_key() -> TestResult {
        for named in ["mistral-key", "mistral"] {
            let routing = LiveModelRouting::new(Arc::new(keyed_config(true, None)));
            let Err(error) =
                routing.set_live_main(Some(named.to_owned()), Some(KEYED_MODEL.to_owned()))
            else {
                return Err(TestError::Unexpected(format!(
                    "{named}: ein abgeschaltetes Modell darf nicht gewählt werden"
                )));
            };
            assert_eq!(error.provider, "mistral", "{named}");
            assert_eq!(error.model, KEYED_MODEL);
            assert!(routing.live_main().is_none(), "{named}");
        }
        Ok(())
    }

    /// Die Rollenwahl eines abgeschalteten Modells scheitert mit Schlüssel,
    /// Namen und über den Vorgabe-Provider gleichermaßen und ändert nichts;
    /// ein aktives Modell desselben Providers wird angenommen.
    #[test]
    fn internal_model_refuses_a_retired_model_named_by_provider_key() -> TestResult {
        let point = InternalModelPoint::RootOrchestrator;
        let cases = [
            (keyed_config(true, None), Some("mistral-key")),
            (keyed_config(true, None), Some("mistral")),
            (keyed_config(false, Some("mistral-key")), None),
        ];
        for (config, named) in cases {
            let routing = LiveModelRouting::new(Arc::new(config));
            let before = routing.resolved_internal_model(point);
            let choice = |model: &str| InternalModelChoice {
                provider: named.map(str::to_owned),
                model: Some(model.to_owned()),
            };
            assert!(
                !routing.set_internal_model(point, Some(choice(KEYED_MODEL))),
                "{named:?}"
            );
            assert_eq!(routing.resolved_internal_model(point), before, "{named:?}");
            assert!(
                routing.set_internal_model(point, Some(choice("mistral-large-3"))),
                "{named:?}: ein aktives Modell wird angenommen"
            );
        }
        Ok(())
    }

    /// Die Auflösung einer Kind-Stelle (zur injizierten Uhr nach dem
    /// Retirement) fällt für jede Herkunft zurück, egal ob die Route den
    /// Schlüssel, den Namen oder keinen Provider (Vorgabe) trägt.
    #[test]
    fn internal_route_falls_back_for_a_retired_model_named_by_provider_key() -> TestResult {
        let after = utc(2026, 10, 15, 0)?;
        let config = keyed_config(true, Some("mistral-key"));
        for provider in [Some("mistral-key"), Some("mistral"), None] {
            for source in [
                InternalModelSource::Explicit,
                InternalModelSource::OpenRouterDefault,
                InternalModelSource::MainModel,
            ] {
                let route = ResolvedInternalModel {
                    point: InternalModelPoint::RootOrchestrator,
                    provider: provider.map(str::to_owned),
                    model: Some(KEYED_MODEL.to_owned()),
                    source,
                };
                let reason = fall_back_reason(internal_route_decision(&config, &route, after))?;
                assert!(reason.contains(KEYED_MODEL), "{provider:?}: {reason}");
            }
        }
        Ok(())
    }

    /// Eine im Retirement-Fenster gewählte Route wird an der nächsten
    /// Turn-Grenze nicht still weiterverwendet: unverändert gemeldet, aber
    /// inzwischen abgeschaltet → abgelehnt, und die Live-Wahl für neue Kinder
    /// ist entfernt (Stand des Starts).
    #[test]
    fn unchanged_deprecated_route_is_refused_once_it_is_retired() -> TestResult {
        let routing = LiveModelRouting::new(Arc::new(ResolvedConfig::default()));
        let provider = Some(WINDOW_PROVIDER.to_owned());
        let model = Some(WINDOW_MODEL.to_owned());
        let installed = Some((provider.clone(), WINDOW_MODEL.to_owned()));

        let warning = routing
            .select_live_main_at(provider.clone(), model.clone(), utc(2026, 10, 30, 12)?)
            .map_err(ctx("im Retirement-Fenster angenommen"))?
            .ok_or(TestError::Missing("Warnung im Retirement-Fenster"))?;
        assert!(warning.contains("zai-glm-5-3"), "{warning}");
        assert_eq!(routing.live_main(), installed);

        // Eine Stunde vor dem Retirement: unverändert angenommen, ohne
        // wiederholte Warnung.
        let again = routing
            .select_live_main_at(provider.clone(), model.clone(), utc(2026, 10, 31, 11)?)
            .map_err(ctx("vor dem Retirement weiter angenommen"))?;
        assert_eq!(again, None);
        assert_eq!(routing.live_main(), installed);

        for now in [utc(2026, 10, 31, 12)?, utc(2026, 11, 1, 0)?] {
            let error =
                refused_with(routing.select_live_main_at(provider.clone(), model.clone(), now))?;
            assert_eq!(error.model, WINDOW_MODEL);
            assert!(error.to_string().contains("zai-glm-5-3"), "{error}");
            assert!(
                routing.live_main().is_none(),
                "die abgeschaltete Route darf nicht mehr für neue Kinder gelten ({now})"
            );
        }
        Ok(())
    }

    /// Eine abgelehnte, geänderte Wahl lässt eine noch gültige installierte
    /// Route unberührt (fail-closed ohne Verlust des letzten guten Stands).
    #[test]
    fn refused_change_keeps_a_still_valid_installed_route() -> TestResult {
        let routing = LiveModelRouting::new(Arc::new(ResolvedConfig::default()));
        let now = utc(2026, 11, 1, 0)?;
        routing
            .select_live_main_at(Some("openai".to_owned()), Some("gpt-5".to_owned()), now)
            .map_err(ctx("aktives Modell angenommen"))?;
        let error = refused_with(routing.select_live_main_at(
            Some(WINDOW_PROVIDER.to_owned()),
            Some(WINDOW_MODEL.to_owned()),
            now,
        ))?;
        assert_eq!(error.model, WINDOW_MODEL);
        assert_eq!(
            routing.live_main(),
            Some((Some("openai".to_owned()), "gpt-5".to_owned()))
        );
        Ok(())
    }

    /// Wird nach dem Retirement eine **andere** abgeschaltete Wahl eingereicht,
    /// räumt die Ablehnung auch die installierte Route ab, die inzwischen
    /// selbst abgeschaltet ist: ein direkter Aufrufer behält kein
    /// abgeschaltetes Modell für neue Kinder.
    #[test]
    fn refused_change_also_clears_an_installed_route_that_is_itself_retired() -> TestResult {
        let routing = LiveModelRouting::new(Arc::new(keyed_config(true, None)));
        routing
            .select_live_main_at(
                Some(WINDOW_PROVIDER.to_owned()),
                Some(WINDOW_MODEL.to_owned()),
                utc(2026, 10, 30, 12)?,
            )
            .map_err(ctx("im Retirement-Fenster angenommen"))?;
        assert!(routing.live_main().is_some());

        let error = refused_with(routing.select_live_main_at(
            Some("mistral-key".to_owned()),
            Some(KEYED_MODEL.to_owned()),
            utc(2026, 11, 1, 0)?,
        ))?;
        assert_eq!(error.model, KEYED_MODEL);
        assert!(
            routing.live_main().is_none(),
            "die inzwischen abgeschaltete installierte Route darf nicht bleiben"
        );
        Ok(())
    }

    /// Die Abkündigungswarnung kommt einmal je Route, nicht bei jeder
    /// Turn-Grenze; ein Routenwechsel oder das Zurücksetzen setzt den Merker
    /// zurück. Der öffentliche Pfad nimmt dieselbe Route wiederholt an.
    #[test]
    fn repeated_deprecated_route_warns_only_once_per_route() -> TestResult {
        let routing = LiveModelRouting::new(Arc::new(ResolvedConfig::default()));
        let now = utc(2026, 10, 30, 12)?;
        let select = |provider: &str, model: &str| {
            routing
                .select_live_main_at(Some(provider.to_owned()), Some(model.to_owned()), now)
                .map_err(ctx("Wahl angenommen"))
        };

        assert!(select(WINDOW_PROVIDER, WINDOW_MODEL)?.is_some());
        for _ in 0..3 {
            assert_eq!(select(WINDOW_PROVIDER, WINDOW_MODEL)?, None);
        }
        assert!(routing.live_main().is_some());

        // Routenwechsel auf ein aktives Angebot setzt den Merker zurück: die
        // Rückkehr warnt erneut.
        assert_eq!(select("openai", "gpt-5")?, None);
        assert!(select(WINDOW_PROVIDER, WINDOW_MODEL)?.is_some());

        // Eine andere abgekündigte Route warnt für sich — ebenfalls einmal.
        assert!(select("anthropic", "claude-mythos-preview")?.is_some());
        assert_eq!(select("anthropic", "claude-mythos-preview")?, None);

        // Das Zurücksetzen löscht den Merker.
        routing
            .set_live_main(None, None)
            .map_err(ctx("Zurücksetzen"))?;
        assert!(routing.live_main().is_none());
        assert!(select("anthropic", "claude-mythos-preview")?.is_some());

        for _ in 0..2 {
            routing
                .set_live_main(
                    Some("anthropic".to_owned()),
                    Some("claude-mythos-preview".to_owned()),
                )
                .map_err(ctx("öffentlicher Pfad"))?;
        }
        assert_eq!(
            routing.live_main(),
            Some((
                Some("anthropic".to_owned()),
                "claude-mythos-preview".to_owned()
            ))
        );
        Ok(())
    }
}
